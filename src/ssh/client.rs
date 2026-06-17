use std::{borrow::Cow, sync::Arc};

use russh::{
    cipher, client, client::AuthResult, client::KeyboardInteractiveAuthResponse, kex, mac,
    ChannelMsg, Preferred,
};
use ssh_key::{Algorithm, EcdsaCurve, HashAlg};
use tokio::io::AsyncWriteExt;

use crate::{
    config::{Automations, Expect, Host, Macro},
    ssh::SshError,
};

/// SSH client handler that verifies the server's host key against a
/// trust-on-first-use store (`~/.config/gukab/known_hosts`).
struct ClientHandler {
    /// Identity used as the known_hosts key, e.g. `"192.0.2.1:22"`.
    host_id: String,
}

impl client::Handler for ClientHandler {
    type Error = russh::Error;

    async fn check_server_key(
        &mut self,
        server_public_key: &ssh_key::PublicKey,
    ) -> Result<bool, Self::Error> {
        let fingerprint = server_public_key.fingerprint(HashAlg::Sha256).to_string();
        let path = crate::config::known_hosts_path();

        match read_known_host(&path, &self.host_id) {
            // Seen before and matches → trusted.
            Some(stored) if stored == fingerprint => Ok(true),
            // Seen before but the key CHANGED → refuse (possible MITM).
            Some(_) => {
                eprintln!(
                    "[gukab] WARNING: host key for {} changed — refusing to connect (possible MITM).",
                    self.host_id
                );
                eprintln!(
                    "[gukab] If the device key legitimately changed, remove its line from {} and reconnect.",
                    path.display()
                );
                Ok(false)
            }
            // First contact → trust on first use and remember the fingerprint.
            None => {
                match append_known_host(&path, &self.host_id, &fingerprint) {
                    Ok(()) => eprintln!(
                        "[gukab] new host {} — fingerprint {} saved (trust-on-first-use).",
                        self.host_id, fingerprint
                    ),
                    Err(e) => eprintln!(
                        "[gukab] could not record host key for {} ({e}); continuing this once.",
                        self.host_id
                    ),
                }
                Ok(true)
            }
        }
    }
}

/// Look up the stored fingerprint for `host_id` in the known_hosts file.
/// Lines are `host_id<space>SHA256:...`; `#` comments and blanks are ignored.
fn read_known_host(path: &std::path::Path, host_id: &str) -> Option<String> {
    let content = std::fs::read_to_string(path).ok()?;
    for line in content.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some((host, fp)) = line.split_once(char::is_whitespace)
            && host == host_id
        {
            return Some(fp.trim().to_string());
        }
    }
    None
}

/// Append a `host_id fingerprint` line, creating the file `0600` if needed.
fn append_known_host(
    path: &std::path::Path,
    host_id: &str,
    fingerprint: &str,
) -> std::io::Result<()> {
    use std::io::Write as _;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut opts = std::fs::OpenOptions::new();
    opts.create(true).append(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        opts.mode(0o600);
    }
    let mut file = opts.open(path)?;
    writeln!(file, "{host_id} {fingerprint}")
}

/// Algorithm preferences that connect to anything from modern OpenSSH down to
/// legacy network gear. Modern algorithms are listed first so they win when the
/// server supports them; weak SHA-1 / CBC / 3DES / DSA fallbacks are appended so
/// old servers still negotiate successfully instead of erroring out.
fn legacy_compatible_preferred() -> Preferred {
    Preferred {
        kex: Cow::Borrowed(&[
            kex::MLKEM768X25519_SHA256,
            kex::CURVE25519,
            kex::CURVE25519_PRE_RFC_8731,
            kex::DH_GEX_SHA256,
            kex::DH_G18_SHA512,
            kex::DH_G17_SHA512,
            kex::DH_G16_SHA512,
            kex::DH_G15_SHA512,
            kex::DH_G14_SHA256,
            kex::DH_GEX_SHA1,
            kex::DH_G14_SHA1,
            kex::DH_G1_SHA1,
            kex::EXTENSION_SUPPORT_AS_CLIENT,
            kex::EXTENSION_SUPPORT_AS_SERVER,
            kex::EXTENSION_OPENSSH_STRICT_KEX_AS_CLIENT,
            kex::EXTENSION_OPENSSH_STRICT_KEX_AS_SERVER,
        ]),
        key: Cow::Borrowed(&[
            Algorithm::Ed25519,
            Algorithm::Ecdsa {
                curve: EcdsaCurve::NistP256,
            },
            Algorithm::Ecdsa {
                curve: EcdsaCurve::NistP384,
            },
            Algorithm::Ecdsa {
                curve: EcdsaCurve::NistP521,
            },
            Algorithm::Rsa {
                hash: Some(HashAlg::Sha512),
            },
            Algorithm::Rsa {
                hash: Some(HashAlg::Sha256),
            },
            Algorithm::Rsa { hash: None },
            Algorithm::Dsa,
        ]),
        cipher: Cow::Borrowed(&[
            cipher::CHACHA20_POLY1305,
            cipher::AES_256_GCM,
            cipher::AES_128_GCM,
            cipher::AES_256_CTR,
            cipher::AES_192_CTR,
            cipher::AES_128_CTR,
            cipher::AES_256_CBC,
            cipher::AES_192_CBC,
            cipher::AES_128_CBC,
            cipher::TRIPLE_DES_CBC,
        ]),
        mac: Cow::Borrowed(&[
            mac::HMAC_SHA256_ETM,
            mac::HMAC_SHA512_ETM,
            mac::HMAC_SHA256,
            mac::HMAC_SHA512,
            mac::HMAC_SHA1_ETM,
            mac::HMAC_SHA1,
        ]),
        ..Preferred::DEFAULT
    }
}

pub async fn connect(host: &Host, automations: &Automations) -> Result<(), SshError> {
    let password = keyring::Entry::new("gukab", &host.credential_ref)
        .and_then(|e| e.get_password())
        .map_err(|e| SshError::Keyring(e.to_string()))?;

    let config = Arc::new(client::Config {
        preferred: legacy_compatible_preferred(),
        // Disable Nagle's algorithm — without this, each keystroke is buffered up
        // to ~40ms (TCP small-packet coalescing), causing interactive typing lag.
        nodelay: true,
        ..client::Config::default()
    });
    let handler = ClientHandler {
        host_id: format!("{}:{}", host.hostname, host.port),
    };
    let mut session =
        client::connect(config, (host.hostname.as_str(), host.port), handler).await?;

    // Try plain password auth first; if the server doesn't offer it (common on
    // network switches like Planet, which only advertise keyboard-interactive),
    // fall back to keyboard-interactive answering each prompt with the password —
    // this is what the OpenSSH client does automatically.
    let authenticated = match session
        .authenticate_password(&host.username, password.clone())
        .await?
    {
        AuthResult::Success => true,
        _ if keyboard_interactive_auth(&mut session, &host.username, &password).await? => true,
        // Some switches do no real SSH-layer auth and present their own
        // Username/Password login over the shell. Accept "none" and let the
        // device prompt in-band — the user logs in there, like the ssh CLI does.
        _ => matches!(
            session.authenticate_none(&host.username).await?,
            AuthResult::Success
        ),
    };
    if !authenticated {
        return Err(SshError::AuthFailed);
    }

    // Macros are always available (manual via Ctrl+A / on_connect; never auto-fire).
    let macros: Vec<Macro> = automations
        .macros
        .iter()
        .chain(host.macros.iter())
        .cloned()
        .collect();
    // Expects auto-fire on output. A host's own expects always apply; additionally,
    // each macro listed in `on_connect` contributes its own expects (e.g. the "en"
    // macro owns the enable-password rule). So expects are opt-in per host via the
    // macros it runs — nothing fires globally on a plain in-band-login switch.
    let mut expects: Vec<Expect> = host.expects.clone();
    for key in &host.on_connect {
        if let Some(m) = macros.iter().find(|m| &m.key == key) {
            expects.extend(m.expects.iter().cloned());
        }
    }
    // Compile expect rules before touching the terminal so a bad regex fails cleanly.
    let mut compiled = build_automations(&expects)?;

    let mut channel = session.channel_open_session().await?;
    let (cols, rows) = crossterm::terminal::size().unwrap_or((80, 24));

    channel
        .request_pty(false, "xterm-256color", cols as u32, rows as u32, 0, 0, &[])
        .await?;
    channel.request_shell(false).await?;

    // Auto-run on-connect macros (e.g. ["en"] for Cisco enable mode). Cisco buffers
    // vty input, so sending right after shell-open is reliable; any password prompt
    // that follows is answered by the expect rules above.
    for key in &host.on_connect {
        match macros.iter().find(|m| &m.key == key) {
            Some(m) => {
                let payload = macro_payload(&m.send);
                if !payload.is_empty() {
                    channel.data(payload.as_bytes()).await?;
                }
            }
            None => {
                eprintln!("[gukab] unknown on_connect macro: {key}");
            }
        }
    }

    // Start session logging (best-effort; never blocks the interactive loop).
    let log_tx = crate::ssh::session_log::start(host);

    // ratatui hides the cursor while rendering, and leaving the alternate screen
    // does not reliably restore DECTCEM — so explicitly show the cursor before
    // handing the terminal to the remote shell. One-shot write, no perf impact.
    {
        use std::io::Write as _;
        let mut out = std::io::stdout();
        let _ = out.write_all(b"\x1b[?25h");
        let _ = out.flush();
    }

    crossterm::terminal::enable_raw_mode()?;
    let result = io_loop(&mut channel, &macros, &mut compiled, log_tx.as_ref()).await;
    // Always restore the local terminal, even if the session errored, so the
    // user's shell is usable again after exiting.
    restore_terminal();

    result
}

/// Authenticate via keyboard-interactive, answering every prompt with `password`.
/// Network gear typically sends a single "Password:" prompt; the loop also handles
/// info-only requests (empty prompt list) and is bounded to avoid a stuck server.
async fn keyboard_interactive_auth(
    session: &mut client::Handle<ClientHandler>,
    user: &str,
    password: &str,
) -> Result<bool, SshError> {
    let mut response = session
        .authenticate_keyboard_interactive_start(user.to_string(), None)
        .await?;
    for _ in 0..16 {
        match response {
            KeyboardInteractiveAuthResponse::Success => return Ok(true),
            KeyboardInteractiveAuthResponse::Failure { .. } => return Ok(false),
            KeyboardInteractiveAuthResponse::InfoRequest { prompts, .. } => {
                // Answer each prompt by its text: username/login prompts get the
                // username, everything else (password/passcode) gets the password.
                let answers = prompts
                    .iter()
                    .map(|p| {
                        let label = p.prompt.to_lowercase();
                        if label.contains("user") || label.contains("login") {
                            user.to_string()
                        } else {
                            password.to_string()
                        }
                    })
                    .collect();
                response = session
                    .authenticate_keyboard_interactive_respond(answers)
                    .await?;
            }
        }
    }
    Ok(false)
}

/// What an expect rule sends when its pattern matches.
enum Response {
    /// Literal text to transmit verbatim.
    Literal(String),
    /// Keyring reference; the password is read at send time so secrets never sit
    /// in memory longer than needed.
    Credential(String),
}

/// A compiled expect rule with its armed state.
struct Automation {
    re: regex::Regex,
    response: Response,
    once: bool,
    fired: bool,
}

/// Compile expect rules into runnable automations.
fn build_automations(expects: &[Expect]) -> Result<Vec<Automation>, SshError> {
    expects.iter().map(build_single_automation).collect()
}

/// Compile a single expect rule into a runnable automation.
fn build_single_automation(e: &Expect) -> Result<Automation, SshError> {
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

/// Undo the terminal state an interactive remote shell may have left behind.
/// Disabling raw mode alone is not enough: the remote shell can switch the
/// terminal into application-cursor-keys / keypad / bracketed-paste modes via
/// escape sequences that persist locally after the session ends, leaving the
/// user's shell echoing `^[[` for arrow keys.
///
/// We deliberately do NOT send `?1049l` (leave alternate screen): ratatui already
/// left the alt screen before the SSH session, so re-sending it makes terminals
/// that honor it (Konsole, Alacritty) run DECRC and jump the cursor back to the
/// position saved at alt-screen entry — corrupting the post-exit display.
fn restore_terminal() {
    use std::io::Write;

    let _ = crossterm::terminal::disable_raw_mode();

    // \x1b[?1l normal cursor keys   \x1b>     normal keypad
    // \x1b[?2004l no bracketed paste \x1b[?25h show cursor   \x1b[0m reset attrs
    let reset = "\x1b[?1l\x1b>\x1b[?2004l\x1b[?25h\x1b[0m";
    let mut stdout = std::io::stdout();
    let _ = stdout.write_all(reset.as_bytes());
    let _ = stdout.flush();
}

/// Local escape prefix (Ctrl+A) that opens the gukab macro prompt instead of
/// being forwarded to the remote. Pressing it twice sends a literal Ctrl+A.
const ESCAPE_PREFIX: u8 = 0x01;
/// Cap for the rolling output buffer scanned by expect rules.
const SCAN_BUFFER_CAP: usize = 8 * 1024;

async fn io_loop(
    channel: &mut russh::Channel<russh::client::Msg>,
    macros: &[Macro],
    automations: &mut Vec<Automation>,
    log_tx: Option<&tokio::sync::mpsc::UnboundedSender<Vec<u8>>>,
) -> Result<(), SshError> {
    let mut stdout = tokio::io::stdout();
    let mut scan_buf = String::new();
    let mut hl = crate::ssh::highlight::Highlighter::new();

    // Read raw stdin on a dedicated OS thread and forward bytes over an mpsc
    // channel. `tokio::io::stdin()` is documented as "best for non-interactive
    // use" and adds per-keystroke latency inside a `select!` loop; a plain
    // blocking read delivers each keystroke immediately.
    let mut rx = spawn_stdin_reader();

    loop {
        tokio::select! {
            msg = channel.wait() => {
                match msg {
                    Some(ChannelMsg::Data { ref data }) => {
                        // Display gets line-colorized output; logging and expect
                        // matching use the raw bytes (clean transcript, stable patterns).
                        let painted = hl.process(data);
                        stdout.write_all(&painted).await?;
                        stdout.flush().await?;
                        if let Some(tx) = log_tx {
                            let _ = tx.send(data.to_vec());
                        }
                        scan_and_respond(data, &mut scan_buf, automations, channel).await?;
                    }
                    Some(ChannelMsg::ExtendedData { ref data, .. }) => {
                        stdout.write_all(data).await?;
                        stdout.flush().await?;
                        if let Some(tx) = log_tx {
                            let _ = tx.send(data.to_vec());
                        }
                    }
                    Some(ChannelMsg::ExitStatus { .. }) | None => break,
                    _ => {}
                }
            }
            bytes = rx.recv() => {
                match bytes {
                    Some(bytes) => {
                        if !forward_stdin(&bytes, macros, channel, automations, &mut rx).await? {
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
fn spawn_stdin_reader() -> tokio::sync::mpsc::Receiver<Vec<u8>> {
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

/// Append remote output to the rolling scan buffer and fire any armed expect
/// rule whose pattern now matches. The buffer is cleared after a match so the
/// same prompt is not answered twice within one chunk.
async fn scan_and_respond(
    data: &[u8],
    scan_buf: &mut String,
    automations: &mut [Automation],
    channel: &mut russh::Channel<russh::client::Msg>,
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
        let payload = resolve_response(&auto.response)?;
        channel.data(payload.as_bytes()).await?;
        if auto.once {
            auto.fired = true;
        }
        scan_buf.clear();
        break;
    }
    Ok(())
}

/// Forward typed bytes to the remote, intercepting the Ctrl+A escape prefix to
/// open the local macro prompt. Returns `Ok(false)` if the session should end.
async fn forward_stdin(
    bytes: &[u8],
    macros: &[Macro],
    channel: &mut russh::Channel<russh::client::Msg>,
    automations: &mut Vec<Automation>,
    rx: &mut tokio::sync::mpsc::Receiver<Vec<u8>>,
) -> Result<bool, SshError> {
    // Pass straight through unless the escape prefix is present.
    let Some(pos) = bytes.iter().position(|&b| b == ESCAPE_PREFIX) else {
        channel.data(bytes).await?;
        return Ok(true);
    };

    // Send everything before the prefix verbatim.
    if pos > 0 {
        channel.data(&bytes[..pos]).await?;
    }

    // Ctrl+A Ctrl+A sends a literal Ctrl+A.
    let rest = &bytes[pos + 1..];
    if let Some((&ESCAPE_PREFIX, tail)) = rest.split_first() {
        channel.data(&[ESCAPE_PREFIX][..]).await?;
        if !tail.is_empty() {
            channel.data(tail).await?;
        }
        return Ok(true);
    }

    // Open the fuzzy macro picker; any bytes after the prefix seed the query.
    match crate::ssh::macro_picker::pick(macros, rx, rest).await {
        crate::ssh::macro_picker::Pick::Run(key) => {
            if let Some(m) = macros.iter().find(|m| m.key == key) {
                // Arm this macro's expects for the session (in addition to any
                // on_connect macros that were already armed at connection time).
                for expect in &m.expects {
                    if let Ok(auto) = build_single_automation(expect) {
                        automations.push(auto);
                    }
                }
                let payload = macro_payload(&m.send);
                if !payload.is_empty() {
                    channel.data(payload.as_bytes()).await?;
                }
            }
            Ok(true)
        }
        crate::ssh::macro_picker::Pick::Cancel => Ok(true),
        crate::ssh::macro_picker::Pick::Disconnect => Ok(false),
    }
}

/// Turn a macro's `send` (possibly multi-line, TOML triple-quoted) into the bytes
/// to transmit: each non-empty line terminated by `\r` (Enter / CR `^M`).
fn macro_payload(send: &str) -> String {
    send.split('\n')
        .map(|line| line.strip_suffix('\r').unwrap_or(line))
        .filter(|line| !line.is_empty())
        .map(|line| format!("{line}\r"))
        .collect()
}
