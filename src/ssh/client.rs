use std::{borrow::Cow, sync::Arc};

use russh::{
    cipher, client, client::AuthResult, client::KeyboardInteractiveAuthResponse, kex,
    keys::PrivateKeyWithHashAlg, mac, ChannelMsg, MethodKind, MethodSet, Preferred,
};
use ssh_key::{Algorithm, EcdsaCurve, HashAlg, PrivateKey};

use crate::{
    config::{Automations, Host, Macro},
    session::{self, Incoming, Transport},
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
            // NIST curves: russh leaves these out of its own default list, but some
            // switch SSH stacks (e.g. Aruba's Mocana SSH 6.3) offer *only*
            // ecdh-sha2-nistp256 — without them KEX fails before the host-key check.
            kex::ECDH_SHA2_NISTP256,
            kex::ECDH_SHA2_NISTP384,
            kex::ECDH_SHA2_NISTP521,
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
    // The keyring secret is optional: for password hosts it is the login password,
    // for key hosts it is the (possibly absent) passphrase. A key-only host with no
    // passphrase has an empty `credential_ref` and no entry — that is not an error.
    let secret: Option<String> = if host.credential_ref.is_empty() {
        None
    } else {
        keyring::Entry::new("gukab", &host.credential_ref)
            .and_then(|e| e.get_password())
            .ok()
    };

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

    // Auth follows what the OpenSSH client does: probe with "none" first, then try
    // only the methods the device actually advertises. Sending a credential by a
    // method the device did not offer just burns an attempt against its lockout
    // counter, and retrying the same password by a second method burns another —
    // so each credential is offered exactly once, by one method.
    //
    // The "none" probe carries no credential. It doubles as the login for switches
    // that do no SSH-layer auth at all (Planet and friends present their own
    // Username/Password over the shell), and otherwise reports which methods remain.
    let mut authenticated = false;
    let mut methods = MethodSet::empty();
    match session.authenticate_none(&host.username).await? {
        AuthResult::Success => authenticated = true,
        AuthResult::Failure {
            remaining_methods, ..
        } => methods = remaining_methods,
    }

    // What we actually attempted, for the error message if everything fails.
    let mut tried: Vec<&str> = Vec::new();

    if !authenticated && !host.identity_file.is_empty() && methods.contains(&MethodKind::PublicKey)
    {
        let key = load_identity(&host.identity_file, secret.as_deref())?;
        let hash_alg = if key.algorithm().is_rsa() {
            Some(HashAlg::Sha256)
        } else {
            None
        };
        let key = PrivateKeyWithHashAlg::new(Arc::new(key), hash_alg);
        tried.push("publickey");
        match session.authenticate_publickey(&host.username, key).await? {
            AuthResult::Success => authenticated = true,
            AuthResult::Failure {
                remaining_methods, ..
            } => methods = remaining_methods,
        }
    }

    // One password attempt, by whichever of the two password-carrying methods the
    // device offers. `password` is preferred because its outcome is unambiguous;
    // keyboard-interactive is the fallback for devices that only advertise it.
    //
    // Key hosts are excluded: there the keyring secret is the key's *passphrase*, not
    // a login password, so offering it to the device would both waste an attempt and
    // hand the passphrase to the far end.
    if !authenticated
        && host.identity_file.is_empty()
        && let Some(password) = secret.as_deref()
    {
        if methods.contains(&MethodKind::Password) {
            tried.push("password");
            authenticated = session
                .authenticate_password(&host.username, password.to_string())
                .await?
                .success();
        } else if methods.contains(&MethodKind::KeyboardInteractive) {
            tried.push("keyboard-interactive");
            authenticated =
                keyboard_interactive_auth(&mut session, &host.username, password).await?;
        }
    }

    if !authenticated {
        return Err(SshError::AuthFailed(auth_failure_detail(
            &methods,
            &tried,
            secret.is_some(),
            &host.identity_file,
        )));
    }

    // Macros are always available (manual via Ctrl+A / on_connect; never auto-fire).
    let macros: Vec<Macro> = automations
        .macros
        .iter()
        .chain(host.macros.iter())
        .cloned()
        .collect();
    // Expects auto-fire on output — at most once per arming. A host's own expects
    // always apply; additionally, each macro listed in `on_connect` arms its own
    // expects tagged with the macro's key (e.g. the "en" macro owns the
    // enable-password rule), so a manual re-run can re-arm them without duplicates.
    // Nothing fires globally on a plain in-band-login switch.
    // Compile expect rules before touching the terminal so a bad regex fails cleanly.
    let mut compiled = session::build_automations(&host.expects, None)?;
    for key in &host.on_connect {
        if let Some(m) = macros.iter().find(|m| &m.key == key) {
            compiled.extend(session::build_automations(&m.expects, Some(key))?);
        }
    }

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
                let payload = session::macro_payload(&m.send);
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
    let log_label = if host.name.trim().is_empty() {
        host.hostname.as_str()
    } else {
        host.name.as_str()
    };
    let log_tx = crate::ssh::session_log::start(log_label);

    // ratatui hides the cursor while rendering, and leaving the alternate screen
    // does not reliably restore DECTCEM — so explicitly show the cursor before
    // handing the terminal to the remote shell. One-shot write, no perf impact.
    {
        use std::io::Write as _;
        let mut out = std::io::stdout();
        let _ = out.write_all(b"\x1b[?25h");
        let _ = out.flush();
    }

    session::prepare_console();
    crossterm::terminal::enable_raw_mode()?;
    let mut transport = SshTransport { channel: &mut channel, pty: (cols, rows) };
    let result =
        session::run_session(&mut transport, &macros, &mut compiled, log_tx.as_ref(), None).await;
    // Always restore the local terminal, even if the session errored, so the
    // user's shell is usable again after exiting.
    session::restore_terminal();

    result
}

/// [`Transport`] over an SSH channel: writes as channel data, reads channel
/// messages (mapping stderr `ExtendedData` and treating exit/close as EOF).
struct SshTransport<'a> {
    channel: &'a mut russh::Channel<russh::client::Msg>,
    /// Size the remote PTY currently has (requested at open, then each resize sent).
    pty: (u16, u16),
}

impl Transport for SshTransport<'_> {
    async fn write(&mut self, data: &[u8]) -> Result<(), SshError> {
        self.channel.data(data).await?;
        Ok(())
    }

    async fn recv(&mut self) -> Option<Incoming> {
        loop {
            match self.channel.wait().await {
                Some(ChannelMsg::Data { ref data }) => {
                    return Some(Incoming { bytes: data.to_vec(), is_stderr: false });
                }
                Some(ChannelMsg::ExtendedData { ref data, .. }) => {
                    return Some(Incoming { bytes: data.to_vec(), is_stderr: true });
                }
                Some(ChannelMsg::ExitStatus { .. }) | None => return None,
                Some(_) => continue,
            }
        }
    }

    async fn resize(&mut self, cols: u16, rows: u16) -> Result<(), SshError> {
        if (cols, rows) == self.pty {
            return Ok(());
        }
        self.channel.window_change(cols as u32, rows as u32, 0, 0).await?;
        self.pty = (cols, rows);
        Ok(())
    }
}

/// Load a private key from `path` for public-key auth, decrypting it with
/// `passphrase` if the file is encrypted. `~`/`$HOME` is expanded. The key bytes
/// stay in memory only for the duration of the connection — never written to
/// config or the keyring. A loosely-permissioned key file (readable/writable by
/// group or other) triggers an OpenSSH-style warning but does not block the
/// connection, since the key never leaves the user's own disk.
fn load_identity(path: &str, passphrase: Option<&str>) -> Result<PrivateKey, SshError> {
    let resolved = crate::config::expand_tilde(path);
    if !resolved.exists() {
        return Err(SshError::Key(format!(
            "identity file not found: {}",
            resolved.display()
        )));
    }
    warn_if_world_readable(&resolved);

    // `load_secret_key` accepts every format network engineers actually have on
    // disk: OpenSSH (`BEGIN OPENSSH PRIVATE KEY`), legacy PEM PKCS#1
    // (`BEGIN RSA PRIVATE KEY`), PKCS#8, PKCS#5-encrypted, and PuTTY .ppk. It also
    // decrypts in-place with the passphrase, so we don't pre-parse the format.
    let passphrase = passphrase.filter(|p| !p.is_empty());
    russh::keys::load_secret_key(&resolved, passphrase).map_err(|e| match e {
        russh::keys::Error::KeyIsEncrypted => SshError::Key(format!(
            "key {} is passphrase-protected — store the passphrase in the keyring (Ctrl+K) and set it as the host's credential ref",
            resolved.display()
        )),
        other => SshError::Key(format!("cannot load key {}: {other}", resolved.display())),
    })
}

/// Warn (stderr) if a key file is readable or writable by group/other, mirroring
/// OpenSSH's "UNPROTECTED PRIVATE KEY FILE" advisory. Best-effort; non-Unix and
/// stat failures are silently ignored.
#[cfg_attr(not(unix), allow(unused_variables))]
fn warn_if_world_readable(path: &std::path::Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        if let Ok(meta) = std::fs::metadata(path) {
            let mode = meta.permissions().mode();
            if mode & 0o077 != 0 {
                eprintln!(
                    "[gukab] WARNING: private key {} is accessible by group/other (mode {:o}); recommend chmod 600.",
                    path.display(),
                    mode & 0o777
                );
            }
        }
    }
}

/// Authenticate via keyboard-interactive, answering every prompt with `password`.
/// Network gear typically sends a single "Password:" prompt; the loop also handles
/// info-only requests (empty prompt list) and is bounded to avoid a stuck server.
/// Why an authentication sequence failed: what the device offered, what gukab
/// actually tried, and — when it tried nothing — why not. This text is what the user
/// sees in the TUI, so it has to be enough to act on without a packet capture.
fn auth_failure_detail(
    methods: &MethodSet,
    tried: &[&str],
    have_secret: bool,
    identity_file: &str,
) -> String {
    let offered: Vec<&str> = methods.iter().map(<&str>::from).collect();
    let offered = if offered.is_empty() {
        "none".to_string()
    } else {
        offered.join(", ")
    };
    if tried.is_empty() {
        let reason = if !identity_file.is_empty() {
            "nothing tried — the device does not offer publickey, and on a key host \
             credential_ref holds the key passphrase, not a login password"
        } else if !have_secret {
            "nothing to try — no credential stored for this host (set credential_ref or identity_file)"
        } else {
            "nothing tried — the device offers no method gukab can use with this host's credential"
        };
        return format!("device offers: {offered}; {reason}");
    }
    format!("device offers: {offered}; tried: {}", tried.join(", "))
}

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

#[cfg(test)]
mod auth_tests {
    use super::*;

    fn set(methods: &[MethodKind]) -> MethodSet {
        MethodSet::from(methods)
    }

    #[test]
    fn failure_detail_names_offered_and_tried_methods() {
        let offered = set(&[MethodKind::Password, MethodKind::KeyboardInteractive]);
        let msg = auth_failure_detail(&offered, &["password"], true, "");
        assert!(msg.contains("password, keyboard-interactive"), "{msg}");
        assert!(msg.contains("tried: password"), "{msg}");
    }

    #[test]
    fn failure_detail_explains_when_nothing_was_tried() {
        // No keyring secret and no key file: there was nothing to send.
        let offered = set(&[MethodKind::Password]);
        let msg = auth_failure_detail(&offered, &[], false, "");
        assert!(msg.contains("no credential stored"), "{msg}");

        // A password is stored but the device only accepts a key.
        let offered = set(&[MethodKind::PublicKey]);
        let msg = auth_failure_detail(&offered, &[], true, "");
        assert!(msg.contains("publickey"), "{msg}");
        assert!(msg.contains("no method gukab can use"), "{msg}");

        // Key host whose device offers only password: the keyring secret is the key
        // passphrase, so it is deliberately never offered as a login password.
        let offered = set(&[MethodKind::Password]);
        let msg = auth_failure_detail(&offered, &[], true, "~/.ssh/id_ed25519");
        assert!(msg.contains("key passphrase"), "{msg}");
    }

    #[test]
    fn failure_detail_handles_a_device_offering_nothing() {
        let msg = auth_failure_detail(&MethodSet::empty(), &[], true, "");
        assert!(msg.contains("device offers: none"), "{msg}");
    }
}
