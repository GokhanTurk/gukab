//! Transport-agnostic interactive session engine.
//!
//! The same loop drives an SSH channel and a serial port: raw stdin passthrough,
//! the `Ctrl+A` macro picker, regex expect rules, session logging, and output
//! colorization. A [`Transport`] abstracts the byte pipe; SSH and serial each
//! provide one. This is what lets the serial console reuse every SSH session
//! feature without duplicating the loop.

use tokio::io::AsyncWriteExt;
use tokio::sync::mpsc::{Receiver, UnboundedSender};

use crate::config::{Expect, Macro};
use crate::ssh::SshError;

/// One chunk of bytes received from the remote/device. `is_stderr` marks SSH
/// `ExtendedData` (stderr), which is displayed raw and not scanned for expects.
pub struct Incoming {
    pub bytes: Vec<u8>,
    pub is_stderr: bool,
}

/// A bidirectional byte pipe (an SSH channel or a serial port).
pub trait Transport {
    /// Send bytes to the remote/device.
    async fn write(&mut self, data: &[u8]) -> Result<(), SshError>;
    /// Await the next chunk from the remote/device; `None` means the link closed.
    async fn recv(&mut self) -> Option<Incoming>;
}

/// Optional live control surface for a serial session (baud switching). `None`
/// for SSH, where the `Ctrl+B` cycle key is forwarded normally instead.
pub trait BaudControl {
    /// Advance to the next baud rate and return it (applied to the open port).
    fn cycle(&mut self) -> u32;
}

/// Local escape prefix (Ctrl+A) that opens the gukab macro prompt instead of
/// being forwarded. Pressing it twice sends a literal Ctrl+A.
pub const ESCAPE_PREFIX: u8 = 0x01;
/// Ctrl+B — cycles the serial baud rate (only intercepted when a [`BaudControl`]
/// is present; forwarded verbatim on SSH).
const BAUD_CYCLE_KEY: u8 = 0x02;
/// Cap for the rolling output buffer scanned by expect rules.
const SCAN_BUFFER_CAP: usize = 8 * 1024;

/// What an expect rule sends when its pattern matches.
enum Response {
    /// Literal text to transmit verbatim.
    Literal(String),
    /// Keyring reference; the password is read at send time so secrets never sit
    /// in memory longer than needed.
    Credential(String),
}

/// A compiled expect rule with its armed state.
pub struct Automation {
    re: regex::Regex,
    response: Response,
    once: bool,
    fired: bool,
}

/// Compile expect rules into runnable automations.
pub fn build_automations(expects: &[Expect]) -> Result<Vec<Automation>, SshError> {
    expects.iter().map(build_single_automation).collect()
}

/// Compile a single expect rule into a runnable automation.
pub fn build_single_automation(e: &Expect) -> Result<Automation, SshError> {
    let re = regex::Regex::new(&e.pattern)
        .map_err(|err| SshError::Automation(format!("invalid regex `{}`: {err}", e.pattern)))?;
    let response = match (&e.send, &e.send_credential) {
        (Some(text), None) => Response::Literal(text.clone()),
        (None, Some(reference)) => Response::Credential(reference.clone()),
        (Some(_), Some(_)) => {
            return Err(SshError::Automation(format!(
                "expect `{}` sets both `send` and `send_credential`",
                e.pattern
            )))
        }
        (None, None) => {
            return Err(SshError::Automation(format!(
                "expect `{}` has neither `send` nor `send_credential`",
                e.pattern
            )))
        }
    };
    Ok(Automation {
        re,
        response,
        once: e.once,
        fired: false,
    })
}

/// Resolve an expect response into the bytes to transmit (response + newline).
fn resolve_response(response: &Response) -> Result<String, SshError> {
    let text = match response {
        Response::Literal(text) => text.clone(),
        Response::Credential(reference) => keyring::Entry::new("gukab", reference)
            .and_then(|e| e.get_password())
            .map_err(|e| SshError::Keyring(e.to_string()))?,
    };
    Ok(format!("{text}\n"))
}

/// Turn a macro's `send` (possibly multi-line, TOML triple-quoted) into the bytes
/// to transmit: each non-empty line terminated by `\r` (Enter / CR `^M`).
pub fn macro_payload(send: &str) -> String {
    send.split('\n')
        .map(|line| line.strip_suffix('\r').unwrap_or(line))
        .filter(|line| !line.is_empty())
        .map(|line| format!("{line}\r"))
        .collect()
}

/// Undo the terminal state an interactive remote shell may have left behind.
/// Disabling raw mode alone is not enough: the remote shell can switch the
/// terminal into application-cursor-keys / keypad / bracketed-paste modes via
/// escape sequences that persist locally after the session ends.
///
/// We deliberately do NOT send `?1049l` (leave alternate screen): ratatui already
/// left the alt screen before the session, so re-sending it makes terminals that
/// honor it run DECRC and jump the cursor back — corrupting the post-exit display.
pub fn restore_terminal() {
    use std::io::Write;

    let _ = crossterm::terminal::disable_raw_mode();

    // \x1b[?1l normal cursor keys   \x1b>     normal keypad
    // \x1b[?2004l no bracketed paste \x1b[?25h show cursor   \x1b[0m reset attrs
    let reset = "\x1b[?1l\x1b>\x1b[?2004l\x1b[?25h\x1b[0m";
    let mut stdout = std::io::stdout();
    let _ = stdout.write_all(reset.as_bytes());
    let _ = stdout.flush();
}

/// Print a local status line (raw-mode safe) without disturbing the session.
fn local_notice(msg: &str) {
    use std::io::Write;
    let mut out = std::io::stdout();
    let _ = write!(out, "\r\n[gukab] {msg}\r\n");
    let _ = out.flush();
}

/// Drive an interactive session over `transport` until the link closes or the
/// user disconnects. Reuses the macro picker, expect engine, logging and
/// colorization for any transport. `baud` enables the serial `Ctrl+B` cycle.
pub async fn run_session<T: Transport>(
    transport: &mut T,
    macros: &[Macro],
    automations: &mut Vec<Automation>,
    log_tx: Option<&UnboundedSender<Vec<u8>>>,
    mut baud: Option<&mut dyn BaudControl>,
) -> Result<(), SshError> {
    let mut stdout = tokio::io::stdout();
    let mut scan_buf = String::new();
    let mut hl = crate::ssh::highlight::Highlighter::new();

    // Read raw stdin on a dedicated OS thread and forward bytes over an mpsc
    // channel; a plain blocking read delivers each keystroke immediately, unlike
    // `tokio::io::stdin()` inside a `select!` loop.
    let mut rx = spawn_stdin_reader();

    loop {
        tokio::select! {
            incoming = transport.recv() => {
                match incoming {
                    Some(Incoming { bytes, is_stderr }) => {
                        if is_stderr {
                            // stderr: display raw, log, but do not colorize or scan.
                            stdout.write_all(&bytes).await?;
                            stdout.flush().await?;
                            if let Some(tx) = log_tx {
                                let _ = tx.send(bytes);
                            }
                        } else {
                            // Display gets line-colorized output; logging and expect
                            // matching use the raw bytes (clean transcript, stable
                            // patterns).
                            let painted = hl.process(&bytes);
                            stdout.write_all(&painted).await?;
                            stdout.flush().await?;
                            if let Some(tx) = log_tx {
                                let _ = tx.send(bytes.clone());
                            }
                            scan_and_respond(&bytes, &mut scan_buf, automations, transport).await?;
                        }
                    }
                    None => break,
                }
            }
            bytes = rx.recv() => {
                match bytes {
                    Some(bytes) => {
                        // Fresh per-iteration reborrow (a trait object's lifetime makes
                        // `as_deref_mut` borrow for the whole call, conflicting across
                        // loop iterations).
                        let baud_ref: Option<&mut dyn BaudControl> = match &mut baud {
                            Some(b) => Some(&mut **b),
                            None => None,
                        };
                        if !forward_stdin(
                            &bytes, macros, transport, automations, &mut rx, baud_ref,
                        ).await? {
                            break;
                        }
                    }
                    None => break,
                }
            }
        }
    }

    Ok(())
}

/// Spawn a blocking thread that reads raw stdin and forwards each read promptly.
fn spawn_stdin_reader() -> Receiver<Vec<u8>> {
    let (tx, rx) = tokio::sync::mpsc::channel::<Vec<u8>>(64);
    std::thread::spawn(move || {
        use std::io::Read;
        let mut stdin = std::io::stdin().lock();
        let mut buf = [0u8; 4096];
        loop {
            match stdin.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => {
                    if tx.blocking_send(buf[..n].to_vec()).is_err() {
                        break;
                    }
                }
                Err(_) => break,
            }
        }
    });
    rx
}

/// Append output to the rolling scan buffer and fire any armed expect rule whose
/// pattern now matches. The buffer is cleared after a match so the same prompt is
/// not answered twice within one chunk.
async fn scan_and_respond<T: Transport>(
    data: &[u8],
    scan_buf: &mut String,
    automations: &mut [Automation],
    transport: &mut T,
) -> Result<(), SshError> {
    // Nothing armed: skip all buffer/regex work so heavy output stays responsive.
    if automations.iter().all(|a| a.fired) {
        return Ok(());
    }
    scan_buf.push_str(&String::from_utf8_lossy(data));
    if scan_buf.len() > SCAN_BUFFER_CAP {
        let cut = scan_buf.len() - SCAN_BUFFER_CAP;
        // Keep the tail; advance to a char boundary so the String stays valid.
        let boundary = (cut..scan_buf.len())
            .find(|&i| scan_buf.is_char_boundary(i))
            .unwrap_or(scan_buf.len());
        *scan_buf = scan_buf[boundary..].to_string();
    }

    for auto in automations.iter_mut() {
        if auto.fired || !auto.re.is_match(scan_buf) {
            continue;
        }
        let payload = match resolve_response(&auto.response) {
            Ok(payload) => payload,
            // A failed credential lookup (e.g. a missing/mistyped keyring ref) must
            // NOT tear down the live session — warn once, disarm this rule, and let
            // the user answer the prompt by hand.
            Err(e) => {
                local_notice(&format!("automation skipped ({e}); answer the prompt manually."));
                auto.fired = true;
                scan_buf.clear();
                break;
            }
        };
        transport.write(payload.as_bytes()).await?;
        if auto.once {
            auto.fired = true;
        }
        scan_buf.clear();
        break;
    }
    Ok(())
}

/// Forward typed bytes to the transport, intercepting the Ctrl+A escape prefix
/// (macro prompt) and, on serial, Ctrl+B (baud cycle). Returns `Ok(false)` if the
/// session should end.
async fn forward_stdin<T: Transport>(
    bytes: &[u8],
    macros: &[Macro],
    transport: &mut T,
    automations: &mut Vec<Automation>,
    rx: &mut Receiver<Vec<u8>>,
    baud: Option<&mut dyn BaudControl>,
) -> Result<bool, SshError> {
    // Serial-only: Ctrl+B cycles the baud rate. Bytes around it pass through.
    if let Some(pos) = bytes.iter().position(|&b| b == BAUD_CYCLE_KEY)
        && let Some(ctl) = baud
    {
        if pos > 0 {
            transport.write(&bytes[..pos]).await?;
        }
        let n = ctl.cycle();
        local_notice(&format!("baud → {n}"));
        let rest = &bytes[pos + 1..];
        if !rest.is_empty() {
            transport.write(rest).await?;
        }
        return Ok(true);
    }

    // Pass straight through unless the escape prefix is present.
    let Some(pos) = bytes.iter().position(|&b| b == ESCAPE_PREFIX) else {
        transport.write(bytes).await?;
        return Ok(true);
    };

    // Send everything before the prefix verbatim.
    if pos > 0 {
        transport.write(&bytes[..pos]).await?;
    }

    // Ctrl+A Ctrl+A sends a literal Ctrl+A.
    let rest = &bytes[pos + 1..];
    if let Some((&ESCAPE_PREFIX, tail)) = rest.split_first() {
        transport.write(&[ESCAPE_PREFIX][..]).await?;
        if !tail.is_empty() {
            transport.write(tail).await?;
        }
        return Ok(true);
    }

    // Open the fuzzy macro picker; any bytes after the prefix seed the query.
    match crate::ssh::macro_picker::pick(macros, rx, rest).await {
        crate::ssh::macro_picker::Pick::Run(key) => {
            if let Some(m) = macros.iter().find(|m| m.key == key) {
                // Arm this macro's expects for the session (in addition to any
                // on_connect macros already armed at connection time).
                for expect in &m.expects {
                    if let Ok(auto) = build_single_automation(expect) {
                        automations.push(auto);
                    }
                }
                let payload = macro_payload(&m.send);
                if !payload.is_empty() {
                    transport.write(payload.as_bytes()).await?;
                }
            }
            Ok(true)
        }
        crate::ssh::macro_picker::Pick::Cancel => Ok(true),
        crate::ssh::macro_picker::Pick::Disconnect => Ok(false),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Expect;

    /// A [`Transport`] that captures everything written and never yields input —
    /// enough to exercise the (generalized) expect engine off the SSH channel.
    struct FakeTransport {
        written: Vec<u8>,
    }

    impl Transport for FakeTransport {
        async fn write(&mut self, data: &[u8]) -> Result<(), SshError> {
            self.written.extend_from_slice(data);
            Ok(())
        }
        async fn recv(&mut self) -> Option<Incoming> {
            None
        }
    }

    #[test]
    fn macro_payload_sends_each_nonempty_line_with_cr() {
        assert_eq!(macro_payload("enable"), "enable\r");
        assert_eq!(macro_payload("a\n\nb\n"), "a\rb\r");
    }

    #[test]
    fn build_single_automation_enforces_exactly_one_target() {
        let both = Expect {
            pattern: "x".into(),
            send: Some("a".into()),
            send_credential: Some("b".into()),
            once: true,
        };
        assert!(build_single_automation(&both).is_err());
        let neither = Expect {
            pattern: "x".into(),
            send: None,
            send_credential: None,
            once: true,
        };
        assert!(build_single_automation(&neither).is_err());
        let bad_regex = Expect {
            pattern: "[".into(),
            send: Some("a".into()),
            send_credential: None,
            once: true,
        };
        assert!(build_single_automation(&bad_regex).is_err());
    }

    #[tokio::test]
    async fn expect_rule_auto_responds_over_any_transport() {
        // Regression guard for the transport generalization: a literal expect must
        // still fire and write its response (+newline) to whatever transport is used.
        let mut autos = build_automations(&[Expect {
            pattern: "[Pp]assword:".into(),
            send: Some("hunter2".into()),
            send_credential: None,
            once: true,
        }])
        .unwrap();
        let mut t = FakeTransport { written: Vec::new() };
        let mut scan = String::new();
        scan_and_respond(b"Username: admin\r\nPassword:", &mut scan, &mut autos, &mut t)
            .await
            .unwrap();
        assert_eq!(t.written, b"hunter2\n");
        // `once` disarms the rule; a second match does not fire again.
        t.written.clear();
        scan_and_respond(b"Password:", &mut scan, &mut autos, &mut t)
            .await
            .unwrap();
        assert!(t.written.is_empty());
    }
}
