//! Opening and driving a serial console session.

use std::io::{Read, Write};
use std::sync::mpsc as std_mpsc;
use std::time::Duration;

use tokio::sync::mpsc;

use crate::config::Automations;
use crate::serial::SerialParams;
use crate::session::{self, BaudControl, Incoming, Transport};
use crate::ssh::SshError;

/// Read poll timeout for the worker thread: short enough that queued outgoing
/// bytes (keystrokes) are written promptly, long enough to stay near-idle.
const READ_TIMEOUT: Duration = Duration::from_millis(20);

/// A message for the port-owning worker thread.
enum Out {
    Bytes(Vec<u8>),
    SetBaud(u32),
}

/// [`Transport`] over a serial port. The port itself lives in a dedicated blocking
/// worker thread (it owns the only handle), fed/read via channels so `set_baud_rate`
/// and reads never contend.
struct SerialTransport {
    out: std_mpsc::Sender<Out>,
    incoming: mpsc::UnboundedReceiver<Incoming>,
}

impl Transport for SerialTransport {
    async fn write(&mut self, data: &[u8]) -> Result<(), SshError> {
        self.out
            .send(Out::Bytes(data.to_vec()))
            .map_err(|_| SshError::Serial("serial port closed".into()))
    }

    async fn recv(&mut self) -> Option<Incoming> {
        self.incoming.recv().await
    }
}

/// Live baud control: applies a chosen rate to the open port (via the worker).
struct SerialBaud {
    out: std_mpsc::Sender<Out>,
    cur: u32,
}

impl BaudControl for SerialBaud {
    fn current(&self) -> u32 {
        self.cur
    }

    fn set_baud(&mut self, baud: u32) {
        self.cur = baud;
        let _ = self.out.send(Out::SetBaud(baud));
    }
}

/// Turn a port-open failure into a user-facing error. Permission-denied (the common
/// case on Linux, where serial nodes are group-owned) gets a tailored hint naming the
/// actual group to join — `dialout` on Debian/Ubuntu, `uucp` on Arch, etc.
fn open_error(device: &str, e: serialport::Error) -> SshError {
    let permission = matches!(
        e.kind(),
        serialport::ErrorKind::Io(std::io::ErrorKind::PermissionDenied)
    );
    if !permission {
        return SshError::Serial(format!("cannot open {device}: {e}"));
    }
    #[cfg(windows)]
    {
        SshError::Serial(format!(
            "access denied opening {device} — the port may be open in another program \
             (close the other terminal / PuTTY), or the name may be wrong."
        ))
    }
    #[cfg(unix)]
    {
        // Serial nodes are group-owned; name the actual owning group so the hint is
        // right across distros (`dialout` on Debian/Ubuntu, `uucp` on Arch, etc.).
        let group = device_group(device).unwrap_or_else(|| "dialout".to_string());
        SshError::Serial(format!(
            "permission denied opening {device}.\n\
             This device is owned by the '{group}' group. Add yourself to it (one-time):\n\
             \x20   sudo usermod -aG {group} $USER\n\
             Then LOG OUT and back in for the change to take effect.\n\
             To use it in the current shell right now (no logout): newgrp {group}"
        ))
    }
    #[cfg(not(any(unix, windows)))]
    {
        SshError::Serial(format!("permission denied opening {device}"))
    }
}

/// Best-effort owning group name of a device file (via `/etc/group`), so the hint
/// names the right group across distros. `None` if it can't be determined.
#[cfg(unix)]
fn device_group(device: &str) -> Option<String> {
    use std::os::unix::fs::MetadataExt as _;
    let gid = std::fs::metadata(device).ok()?.gid();
    let groups = std::fs::read_to_string("/etc/group").ok()?;
    for line in groups.lines() {
        // name:passwd:gid:members
        let mut f = line.split(':');
        let name = f.next()?;
        let _passwd = f.next();
        if f.next().and_then(|g| g.parse::<u32>().ok()) == Some(gid) {
            return Some(name.to_string());
        }
    }
    None
}

/// The blocking thread that owns the port: forwards reads to `tx_in`, and between
/// reads drains `rx_out` (writes / baud changes). Exits (closing the session) on a
/// port error or when the async side drops `rx_out`.
fn serial_worker(
    mut port: Box<dyn serialport::SerialPort>,
    tx_in: mpsc::UnboundedSender<Incoming>,
    rx_out: std_mpsc::Receiver<Out>,
) {
    let mut buf = [0u8; 4096];
    loop {
        // Apply anything queued before blocking on the next read.
        loop {
            match rx_out.try_recv() {
                Ok(Out::Bytes(v)) => {
                    let _ = port.write_all(&v);
                    let _ = port.flush();
                }
                Ok(Out::SetBaud(n)) => {
                    let _ = port.set_baud_rate(n);
                }
                Err(std_mpsc::TryRecvError::Empty) => break,
                Err(std_mpsc::TryRecvError::Disconnected) => return, // session ended
            }
        }
        match port.read(&mut buf) {
            Ok(0) => {}
            Ok(n) => {
                if tx_in
                    .send(Incoming { bytes: buf[..n].to_vec(), is_stderr: false })
                    .is_err()
                {
                    return;
                }
            }
            // A read timeout is normal (idle line); keep polling.
            Err(ref e) if e.kind() == std::io::ErrorKind::TimedOut => {}
            // Any other error (adapter unplugged, etc.) ends the session.
            Err(_) => return,
        }
    }
}

/// Open the serial port and run an interactive console session over it, reusing
/// the shared session engine (macros, expects, logging, colorization, and the
/// `Ctrl+B` live baud cycle). Global macros are available via `Ctrl+A`.
pub async fn connect_serial(
    params: &SerialParams,
    automations: &Automations,
) -> Result<(), SshError> {
    let port = serialport::new(&params.device, params.baud)
        .data_bits(params.data_bits_sp())
        .parity(params.parity_sp())
        .stop_bits(params.stop_bits_sp())
        .flow_control(params.flow_sp())
        .timeout(READ_TIMEOUT)
        .open()
        .map_err(|e| open_error(&params.device, e))?;

    let (tx_in, rx_in) = mpsc::unbounded_channel::<Incoming>();
    let (tx_out, rx_out) = std_mpsc::channel::<Out>();
    std::thread::spawn(move || serial_worker(port, tx_in, rx_out));

    let mut transport = SerialTransport { out: tx_out.clone(), incoming: rx_in };
    let mut baud = SerialBaud { out: tx_out, cur: params.baud };

    // No host here, so only the global macros apply; a macro's expects are armed
    // when it is run from the Ctrl+A picker (as in an SSH session).
    let macros = automations.macros.clone();
    let mut compiled = session::build_automations(&[], None)?;

    let log_tx = crate::ssh::session_log::start(params.log_label());

    // Show the cursor (ratatui hid it) before handing the terminal to the device.
    {
        let mut out = std::io::stdout();
        let _ = out.write_all(b"\x1b[?25h");
        let _ = out.flush();
    }

    session::prepare_console();
    crossterm::terminal::enable_raw_mode()?;
    let result = session::run_session(
        &mut transport,
        &macros,
        &mut compiled,
        log_tx.as_ref(),
        Some(&mut baud),
    )
    .await;
    session::restore_terminal();

    result
}
