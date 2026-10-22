//! Transport-agnostic interactive session engine.
//!
//! The same loop drives an SSH channel and a serial port: raw stdin passthrough,
//! the `Ctrl+A` macro picker, regex expect rules, session logging, and output
//! colorization. A [`Transport`] abstracts the byte pipe; SSH and serial each
//! provide one. This is what lets the serial console reuse every SSH session
//! feature without duplicating the loop.

use tokio::io::AsyncWriteExt;
use tokio::sync::mpsc::{Receiver, UnboundedSender};

use crate::config::{CredentialName, Expect, Macro};
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
    /// The local terminal is now `cols`×`rows`. SSH forwards it as a `window-change`
    /// request; serial has no PTY, so the default does nothing.
    async fn resize(&mut self, _cols: u16, _rows: u16) -> Result<(), SshError> {
        Ok(())
    }
}

/// Optional live control surface for a serial session (baud switching). `None`
/// for SSH. Surfaced as a pinned entry in the `Ctrl+A` picker (no dedicated key,
/// so nothing clashes with an outer multiplexer like tmux); selecting it opens a
/// chooser (pick a preset or type a custom rate).
pub trait BaudControl {
    /// The current baud rate (shown in the picker entry / pre-filled in the chooser).
    fn current(&self) -> u32;
    /// Apply a new baud rate to the open port.
    fn set_baud(&mut self, baud: u32);
}

/// A keyring entry the `Ctrl+A` picker can paste. `pinned` marks the connected
/// host's own login password, listed at the top of the empty picker.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PasteCredential {
    pub reference: String,
    pub pinned: bool,
}

/// The picker's credentials: the host's own login password first (when it has one),
/// then the names registered in `automations.toml`, without duplicates.
pub fn paste_credentials(
    host_ref: Option<&str>,
    registered: &[CredentialName],
) -> Vec<PasteCredential> {
    let host_ref = host_ref.filter(|r| !r.is_empty());
    let mut out: Vec<PasteCredential> = host_ref
        .map(|r| PasteCredential { reference: r.to_string(), pinned: true })
        .into_iter()
        .collect();
    for c in registered {
        if !c.reference.is_empty() && !out.iter().any(|o| o.reference == c.reference) {
            out.push(PasteCredential { reference: c.reference.clone(), pinned: false });
        }
    }
    out
}

/// The current output line with escape sequences stripped — enough to tell whether
/// the remote is asking for a password right now (the cursor sits after the prompt).
#[derive(Default)]
struct PromptTail {
    line: Vec<u8>,
    esc: EscState,
}

#[derive(Default, Clone, Copy, PartialEq)]
enum EscState {
    #[default]
    Text,
    Esc,
    Csi,
    Osc,
}

/// Longest line kept; a prompt is short, so older bytes are dropped.
const PROMPT_TAIL_CAP: usize = 512;

impl PromptTail {
    fn push(&mut self, bytes: &[u8]) {
        for &b in bytes {
            self.esc = match (self.esc, b) {
                (EscState::Text, 0x1b) => EscState::Esc,
                (EscState::Text, b'\r' | b'\n') => {
                    self.line.clear();
                    EscState::Text
                }
                (EscState::Text, 0x08) => {
                    self.line.pop();
                    EscState::Text
                }
                (EscState::Text, b) if b >= 0x20 || b == b'\t' => {
                    if self.line.len() == PROMPT_TAIL_CAP {
                        self.line.remove(0);
                    }
                    self.line.push(b);
                    EscState::Text
                }
                (EscState::Text, _) => EscState::Text,
                (EscState::Esc, b'[') => EscState::Csi,
                (EscState::Esc, b']') => EscState::Osc,
                (EscState::Esc, _) => EscState::Text,
                // CSI ends with a final byte in 0x40..=0x7e.
                (EscState::Csi, 0x40..=0x7e) => EscState::Text,
                (EscState::Csi, _) => EscState::Csi,
                // OSC ends with BEL (or ST, whose ESC restarts the state machine).
                (EscState::Osc, 0x07) => EscState::Text,
                (EscState::Osc, 0x1b) => EscState::Esc,
                (EscState::Osc, _) => EscState::Osc,
            };
        }
    }

    /// Does the current line look like a password prompt (`Password:`,
    /// `[sudo] password for x:`, `[sudo] x için parola:`, `Enter passphrase:`)?
    fn looks_like_password_prompt(&self) -> bool {
        let text = String::from_utf8_lossy(&self.line).to_lowercase();
        let text = text.trim_end();
        text.ends_with(':')
            && ["password", "passphrase", "passcode", "parola", "şifre"]
                .iter()
                .any(|k| text.contains(k))
    }
}

/// Local escape prefix (Ctrl+A) that opens the gukab macro prompt instead of
/// being forwarded. Pressing it twice sends a literal Ctrl+A.
pub const ESCAPE_PREFIX: u8 = 0x01;
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

/// A compiled expect rule with its armed state. Every rule is one-shot per
/// arming: it fires at most once, then stays disarmed until the macro that owns
/// it is run again (which re-arms a fresh copy). This is what keeps a wrong
/// auto-sent password from being retried into an account lockout, and keeps a
/// long-disarmed rule from answering an unrelated later prompt.
pub struct Automation {
    re: regex::Regex,
    response: Response,
    /// Key of the macro this rule was armed by; `None` for host-level expects.
    owner: Option<String>,
    fired: bool,
}

/// Compile expect rules into runnable automations, all owned by `owner`.
pub fn build_automations(
    expects: &[Expect],
    owner: Option<&str>,
) -> Result<Vec<Automation>, SshError> {
    expects
        .iter()
        .map(|e| build_single_automation(e, owner))
        .collect()
}

/// Compile a single expect rule into a runnable automation.
pub fn build_single_automation(e: &Expect, owner: Option<&str>) -> Result<Automation, SshError> {
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
        owner: owner.map(str::to_owned),
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
    credentials: &[PasteCredential],
    automations: &mut Vec<Automation>,
    log_tx: Option<&UnboundedSender<Vec<u8>>>,
    mut baud: Option<&mut dyn BaudControl>,
) -> Result<(), SshError> {
    let mut stdout = tokio::io::stdout();
    let mut scan_buf = String::new();
    let mut hl = crate::ssh::highlight::Highlighter::new();
    let mut prompt = PromptTail::default();

    // Read raw stdin on a dedicated OS thread and forward bytes over an mpsc
    // channel; a plain blocking read delivers each keystroke immediately, unlike
    // `tokio::io::stdin()` inside a `select!` loop.
    let (mut rx, mut resize) = spawn_stdin_reader();

    // The window may have changed between the PTY request and now.
    if let Ok((cols, rows)) = crossterm::terminal::size() {
        transport.resize(cols, rows).await?;
    }

    loop {
        tokio::select! {
            (cols, rows) = resize.next() => {
                transport.resize(cols, rows).await?;
            }
            incoming = transport.recv() => {
                match incoming {
                    Some(Incoming { bytes, is_stderr }) => {
                        prompt.push(&bytes);
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
                        let ctx = PickerContext { macros, credentials, prompt: &prompt };
                        if !forward_stdin(&bytes, &ctx, transport, automations, &mut rx, baud_ref)
                            .await?
                        {
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
///
/// Unix: the terminal is in raw mode, so `stdin` already delivers the exact VT byte
/// stream (arrows, `Ctrl+…`, etc.) we forward to the remote.
/// Window size changes come from SIGWINCH.
#[cfg(unix)]
fn spawn_stdin_reader() -> (Receiver<Vec<u8>>, ResizeWatcher) {
    let (tx, rx) = tokio::sync::mpsc::channel::<Vec<u8>>(64);
    let resize = ResizeWatcher {
        signal: tokio::signal::unix::signal(tokio::signal::unix::SignalKind::window_change())
            .ok(),
    };
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
    (rx, resize)
}

/// Windows: the console doesn't hand us a raw VT byte stream, so read crossterm key
/// events and encode them to the same VT byte sequences the pickers/remote expect.
/// There is no SIGWINCH; `Event::Resize` from the same event stream is routed to the
/// resize watcher instead.
#[cfg(windows)]
fn spawn_stdin_reader() -> (Receiver<Vec<u8>>, ResizeWatcher) {
    let (tx, rx) = tokio::sync::mpsc::channel::<Vec<u8>>(64);
    // `watch` keeps only the latest size, so a burst while dragging the window
    // collapses to the final one.
    let (size_tx, size_rx) = tokio::sync::watch::channel((0u16, 0u16));
    std::thread::spawn(move || {
        while let Ok(ev) = crossterm::event::read() {
            if let crossterm::event::Event::Resize(cols, rows) = ev {
                let _ = size_tx.send((cols, rows));
                continue;
            }
            let bytes = encode_event(ev);
            if bytes.is_empty() {
                continue;
            }
            if tx.blocking_send(bytes).is_err() {
                break;
            }
        }
    });
    (rx, ResizeWatcher { size: size_rx })
}

/// Yields the local terminal size each time the window is resized. Never resolves if
/// the platform source is unavailable, so its `select!` arm simply stays idle.
struct ResizeWatcher {
    #[cfg(unix)]
    signal: Option<tokio::signal::unix::Signal>,
    #[cfg(windows)]
    size: tokio::sync::watch::Receiver<(u16, u16)>,
}

impl ResizeWatcher {
    #[cfg(unix)]
    async fn next(&mut self) -> (u16, u16) {
        if let Some(signal) = &mut self.signal {
            while signal.recv().await.is_some() {
                if let Ok(size) = crossterm::terminal::size() {
                    return size;
                }
            }
        }
        std::future::pending().await
    }

    #[cfg(windows)]
    async fn next(&mut self) -> (u16, u16) {
        if self.size.changed().await.is_ok() {
            return *self.size.borrow_and_update();
        }
        std::future::pending().await
    }
}

/// (Windows) Encode a crossterm event into the VT byte sequence to send to the
/// remote/device — the inverse of what the pickers parse.
#[cfg(windows)]
fn encode_event(ev: crossterm::event::Event) -> Vec<u8> {
    use crossterm::event::{Event, KeyEventKind};
    match ev {
        Event::Key(k) => {
            // Windows emits Press/Repeat/Release; only send presses & repeats.
            if k.kind == KeyEventKind::Release {
                Vec::new()
            } else {
                encode_key(k.code, k.modifiers)
            }
        }
        Event::Paste(s) => s.into_bytes(),
        _ => Vec::new(),
    }
}

/// (Windows) Map a key + modifiers to its terminal byte sequence.
#[cfg(windows)]
fn encode_key(code: crossterm::event::KeyCode, mods: crossterm::event::KeyModifiers) -> Vec<u8> {
    use crossterm::event::{KeyCode, KeyModifiers};
    let ctrl = mods.contains(KeyModifiers::CONTROL);
    let alt = mods.contains(KeyModifiers::ALT);
    let mut out = Vec::new();
    match code {
        KeyCode::Char(c) => {
            if ctrl && alt {
                // AltGr: Windows reports it as Ctrl+Alt with the layout-resolved
                // char (@ $ € on e.g. Turkish/German layouts) — send as plain text.
                let mut buf = [0u8; 4];
                out.extend_from_slice(c.encode_utf8(&mut buf).as_bytes());
            } else if ctrl {
                out.push(control_byte(c));
            } else {
                if alt {
                    out.push(0x1b); // Alt = ESC prefix
                }
                let mut buf = [0u8; 4];
                out.extend_from_slice(c.encode_utf8(&mut buf).as_bytes());
            }
        }
        KeyCode::Enter => out.push(b'\r'),
        KeyCode::Tab => out.push(b'\t'),
        KeyCode::BackTab => out.extend_from_slice(b"\x1b[Z"),
        KeyCode::Backspace => out.push(0x7f),
        KeyCode::Esc => out.push(0x1b),
        KeyCode::Left => out.extend_from_slice(b"\x1b[D"),
        KeyCode::Right => out.extend_from_slice(b"\x1b[C"),
        KeyCode::Up => out.extend_from_slice(b"\x1b[A"),
        KeyCode::Down => out.extend_from_slice(b"\x1b[B"),
        KeyCode::Home => out.extend_from_slice(b"\x1b[H"),
        KeyCode::End => out.extend_from_slice(b"\x1b[F"),
        KeyCode::PageUp => out.extend_from_slice(b"\x1b[5~"),
        KeyCode::PageDown => out.extend_from_slice(b"\x1b[6~"),
        KeyCode::Delete => out.extend_from_slice(b"\x1b[3~"),
        KeyCode::Insert => out.extend_from_slice(b"\x1b[2~"),
        KeyCode::F(n) => out.extend_from_slice(f_key(n)),
        _ => {}
    }
    out
}

/// (Windows) The control byte for `Ctrl+<c>` (e.g. `Ctrl+A` → 0x01, `Ctrl+C` → 0x03).
#[cfg(windows)]
fn control_byte(c: char) -> u8 {
    let u = c.to_ascii_uppercase() as u8;
    match u {
        // '@' A..Z '[' '\' ']' '^' '_'  →  0x00..=0x1f
        0x40..=0x5f => u & 0x1f,
        b' ' => 0x00,
        b'?' => 0x7f,
        _ => (c as u8) & 0x1f,
    }
}

/// (Windows) Standard xterm sequences for the function keys.
#[cfg(windows)]
fn f_key(n: u8) -> &'static [u8] {
    match n {
        1 => b"\x1bOP",
        2 => b"\x1bOQ",
        3 => b"\x1bOR",
        4 => b"\x1bOS",
        5 => b"\x1b[15~",
        6 => b"\x1b[17~",
        7 => b"\x1b[18~",
        8 => b"\x1b[19~",
        9 => b"\x1b[20~",
        10 => b"\x1b[21~",
        11 => b"\x1b[23~",
        12 => b"\x1b[24~",
        _ => b"",
    }
}

/// Prepare the console for the interactive session. No-op on Unix; on Windows it
/// enables `ENABLE_VIRTUAL_TERMINAL_PROCESSING` on stdout so the remote/device's ANSI
/// escapes (colors, cursor moves, alt-screen) render. Call before `enable_raw_mode`.
pub fn prepare_console() {
    #[cfg(windows)]
    {
        use windows_sys::Win32::System::Console::{
            GetConsoleMode, GetStdHandle, SetConsoleMode, ENABLE_VIRTUAL_TERMINAL_PROCESSING,
            STD_OUTPUT_HANDLE,
        };
        unsafe {
            let handle = GetStdHandle(STD_OUTPUT_HANDLE);
            let mut mode: u32 = 0;
            if GetConsoleMode(handle, &mut mode) != 0 {
                let _ = SetConsoleMode(handle, mode | ENABLE_VIRTUAL_TERMINAL_PROCESSING);
            }
        }
    }
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
        // One-shot per arming: never retry (a wrong password would be re-sent
        // until the device blocks the account) and never linger to answer an
        // unrelated later prompt. Re-running the owning macro re-arms the rule.
        auto.fired = true;
        scan_buf.clear();
        break;
    }
    Ok(())
}

/// Forward typed bytes to the transport, intercepting the Ctrl+A escape prefix
/// (macro prompt, which on serial also offers "cycle baud"). Returns `Ok(false)`
/// if the session should end.
/// What the `Ctrl+A` picker offers, plus the output line used to check for a
/// password prompt before pasting.
struct PickerContext<'a> {
    macros: &'a [Macro],
    credentials: &'a [PasteCredential],
    prompt: &'a PromptTail,
}

async fn forward_stdin<T: Transport>(
    bytes: &[u8],
    ctx: &PickerContext<'_>,
    transport: &mut T,
    automations: &mut Vec<Automation>,
    rx: &mut Receiver<Vec<u8>>,
    baud: Option<&mut dyn BaudControl>,
) -> Result<bool, SshError> {
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

    // Open the fuzzy macro picker; on serial it also pins a "baud rate" entry
    // showing the current baud. Any bytes after the prefix seed the query.
    let baud_now = baud.as_ref().map(|b| b.current());
    let macros = ctx.macros;
    match crate::ssh::macro_picker::pick(macros, ctx.credentials, rx, rest, baud_now).await {
        crate::ssh::macro_picker::Pick::Baud => {
            // Open the baud chooser (pick a preset or type a custom rate).
            if let Some(ctl) = baud
                && let Some(n) = crate::ssh::baud_picker::choose(rx, ctl.current()).await
            {
                ctl.set_baud(n);
                local_notice(&format!("baud → {n}"));
            }
            Ok(true)
        }
        crate::ssh::macro_picker::Pick::Run(key) => {
            if let Some(m) = macros.iter().find(|m| m.key == key) {
                // Re-arm this macro's expects for one firing each: drop any
                // copies from a previous run (or from on_connect) so rules
                // never accumulate, then arm fresh ones.
                automations.retain(|a| a.owner.as_deref() != Some(key.as_str()));
                for expect in &m.expects {
                    if let Ok(auto) = build_single_automation(expect, Some(&key)) {
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
        crate::ssh::macro_picker::Pick::Paste(reference) => {
            // Pasting where nothing asks for a password would echo the secret on
            // screen, into the session log and the remote shell history — so ask
            // first unless the cursor sits after a password prompt.
            if !ctx.prompt.looks_like_password_prompt() && !confirm_paste(&reference, rx).await {
                local_notice("paste cancelled");
                return Ok(true);
            }
            match read_secret(&reference) {
                // Typed as if from the keyboard; no Enter — the user confirms.
                Ok(secret) => transport.write(secret.as_bytes()).await?,
                Err(e) => local_notice(&format!("credential '{reference}' unavailable: {e}")),
            }
            Ok(true)
        }
        crate::ssh::macro_picker::Pick::Cancel => Ok(true),
        crate::ssh::macro_picker::Pick::Disconnect => Ok(false),
    }
}

/// Ask on the local terminal whether to paste although no password prompt is
/// visible. Only `y`/`Y` confirms; anything else (including Enter) cancels.
async fn confirm_paste(reference: &str, rx: &mut Receiver<Vec<u8>>) -> bool {
    use std::io::Write;
    let mut out = std::io::stdout();
    let _ = write!(
        out,
        "\r\n[gukab] no password prompt on screen — paste '{reference}' anyway? [y/N] "
    );
    let _ = out.flush();
    let yes = matches!(rx.recv().await.as_deref(), Some([b'y' | b'Y']));
    let _ = write!(out, "\r\n");
    let _ = out.flush();
    yes
}

/// Read a secret from the OS keychain at the moment it is pasted. Never cached; the
/// buffer is wiped when dropped.
fn read_secret(reference: &str) -> Result<zeroize::Zeroizing<String>, keyring::Error> {
    keyring::Entry::new("gukab", reference)
        .and_then(|e| e.get_password())
        .map(zeroize::Zeroizing::new)
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

    fn tail(chunks: &[&[u8]]) -> PromptTail {
        let mut t = PromptTail::default();
        for c in chunks {
            t.push(c);
        }
        t
    }

    #[test]
    fn password_prompts_are_recognized() {
        for prompt in [
            &b"[sudo] password for gokhan: "[..],
            "\x1b[0m[sudo] gokhan için parola: ".as_bytes(),
            b"Last login: Mon\r\nPassword:",
            b"Enter passphrase for key '/home/g/.ssh/id_ed25519': ",
            "Şifre: ".as_bytes(),
        ] {
            assert!(tail(&[prompt]).looks_like_password_prompt(), "{}", String::from_utf8_lossy(prompt));
        }
        // A prompt split across chunks still counts.
        assert!(tail(&[b"[sudo] pass", b"word for g: "]).looks_like_password_prompt());
    }

    #[test]
    fn shell_prompts_and_finished_lines_are_not_password_prompts() {
        for text in [
            &b"gokhan@ubuntu24:~$ "[..],
            b"\x1b]0;gokhan@ubuntu24: ~\x07\x1b[01;32mgokhan@ubuntu24\x1b[00m:~$ ",
            b"[sudo] password for gokhan: \r\n",          // answered, newline followed
            b"passwd: password updated successfully\r\n$ ",
            b"Switch#",
        ] {
            assert!(!tail(&[text]).looks_like_password_prompt(), "{}", String::from_utf8_lossy(text));
        }
    }

    #[test]
    fn paste_credentials_put_the_host_password_first_without_duplicates() {
        let registered = vec![
            CredentialName { reference: "enable1".into() },
            CredentialName { reference: "srv-pass".into() },
            CredentialName { reference: "enable1".into() },
        ];
        let list = paste_credentials(Some("srv-pass"), &registered);
        let refs: Vec<(&str, bool)> = list.iter().map(|c| (c.reference.as_str(), c.pinned)).collect();
        assert_eq!(refs, [("srv-pass", true), ("enable1", false)]);

        // No host password (key host or serial): only the registered names.
        let list = paste_credentials(Some(""), &registered);
        assert!(list.iter().all(|c| !c.pinned));
        assert_eq!(list.len(), 2);
    }

    #[tokio::test]
    async fn paste_without_a_prompt_needs_an_explicit_yes() {
        let (tx, mut rx) = tokio::sync::mpsc::channel::<Vec<u8>>(4);
        tx.send(b"y".to_vec()).await.unwrap();
        assert!(confirm_paste("srv", &mut rx).await);
        for answer in [&b"n"[..], b"\r", b"yes"] {
            tx.send(answer.to_vec()).await.unwrap();
            assert!(!confirm_paste("srv", &mut rx).await, "{answer:?} must cancel");
        }
    }

    #[tokio::test]
    async fn resize_defaults_to_a_no_op() {
        // Serial keeps the default: a window resize must not reach the device.
        let mut t = FakeTransport { written: Vec::new() };
        t.resize(120, 40).await.unwrap();
        assert!(t.written.is_empty());
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
        };
        assert!(build_single_automation(&both, None).is_err());
        let neither = Expect {
            pattern: "x".into(),
            send: None,
            send_credential: None,
        };
        assert!(build_single_automation(&neither, None).is_err());
        let bad_regex = Expect {
            pattern: "[".into(),
            send: Some("a".into()),
            send_credential: None,
        };
        assert!(build_single_automation(&bad_regex, None).is_err());
    }

    #[tokio::test]
    async fn expect_rule_auto_responds_over_any_transport() {
        // Regression guard for the transport generalization: a literal expect must
        // still fire and write its response (+newline) to whatever transport is used.
        let mut autos = build_automations(
            &[Expect {
                pattern: "[Pp]assword:".into(),
                send: Some("hunter2".into()),
                send_credential: None,
            }],
            Some("en"),
        )
        .unwrap();
        let mut t = FakeTransport { written: Vec::new() };
        let mut scan = String::new();
        scan_and_respond(b"Username: admin\r\nPassword:", &mut scan, &mut autos, &mut t)
            .await
            .unwrap();
        assert_eq!(t.written, b"hunter2\n");
        // Firing disarms the rule (one-shot per arming); a re-prompt — e.g. a
        // wrong password — is never answered again.
        t.written.clear();
        scan_and_respond(b"Password:", &mut scan, &mut autos, &mut t)
            .await
            .unwrap();
        assert!(t.written.is_empty());
    }

    #[tokio::test]
    async fn rerunning_a_macro_rearms_its_expects_without_duplicates() {
        let expect = Expect {
            pattern: "[Pp]assword:".into(),
            send: Some("hunter2".into()),
            send_credential: None,
        };
        // Armed at connect via on_connect, then fired once and disarmed.
        let mut autos = build_automations(std::slice::from_ref(&expect), Some("en")).unwrap();
        let mut t = FakeTransport { written: Vec::new() };
        let mut scan = String::new();
        scan_and_respond(b"Password:", &mut scan, &mut autos, &mut t)
            .await
            .unwrap();
        assert_eq!(t.written, b"hunter2\n");

        // Manual re-run of the same macro: drop its old copies, arm fresh ones
        // (mirrors the `Pick::Run` path in `forward_stdin`).
        autos.retain(|a| a.owner.as_deref() != Some("en"));
        autos.push(build_single_automation(&expect, Some("en")).unwrap());
        assert_eq!(autos.len(), 1);

        // The re-armed rule fires exactly once more.
        t.written.clear();
        scan_and_respond(b"Password:", &mut scan, &mut autos, &mut t)
            .await
            .unwrap();
        assert_eq!(t.written, b"hunter2\n");
        t.written.clear();
        scan_and_respond(b"Password:", &mut scan, &mut autos, &mut t)
            .await
            .unwrap();
        assert!(t.written.is_empty());
    }
}
