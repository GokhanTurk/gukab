//! Opening and driving a serial console session.

use std::io::{Read, Write};
use std::sync::mpsc as std_mpsc;
use std::time::Duration;

use tokio::sync::mpsc;

use crate::config::Automations;
use crate::serial::{SerialParams, BAUD_PRESETS};
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

/// Live baud switcher (`Ctrl+B`): cycles the presets and applies each to the port.
struct BaudCycler {
    out: std_mpsc::Sender<Out>,
    idx: usize,
}

impl BaudControl for BaudCycler {
    fn cycle(&mut self) -> u32 {
        self.idx = (self.idx + 1) % BAUD_PRESETS.len();
        let n = BAUD_PRESETS[self.idx];
        let _ = self.out.send(Out::SetBaud(n));
        n
    }
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
        .map_err(|e| SshError::Serial(format!("cannot open {}: {e}", params.device)))?;

    let (tx_in, rx_in) = mpsc::unbounded_channel::<Incoming>();
    let (tx_out, rx_out) = std_mpsc::channel::<Out>();
    std::thread::spawn(move || serial_worker(port, tx_in, rx_out));

    let mut transport = SerialTransport { out: tx_out.clone(), incoming: rx_in };
    // Start the cycler at the current baud's preset index (or the last preset).
    let idx = BAUD_PRESETS
        .iter()
        .position(|&b| b == params.baud)
        .unwrap_or(BAUD_PRESETS.len() - 1);
    let mut baud = BaudCycler { out: tx_out, idx };

    // No host here, so only the global macros apply; a macro's expects are armed
    // when it is run from the Ctrl+A picker (as in an SSH session).
    let macros = automations.macros.clone();
    let mut compiled = session::build_automations(&[])?;

    let log_tx = crate::ssh::session_log::start(params.log_label());

    // Show the cursor (ratatui hid it) before handing the terminal to the device.
    {
        let mut out = std::io::stdout();
        let _ = out.write_all(b"\x1b[?25h");
        let _ = out.flush();
    }

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
