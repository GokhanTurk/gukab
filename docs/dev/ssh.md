# SSH: credentials, auth, crypto, host keys

Read this before touching [src/ssh/client.rs](../../src/ssh/client.rs),
[src/config/mod.rs](../../src/config/mod.rs) credential handling, or
[src/macos_trust.rs](../../src/macos_trust.rs).

## Credential flow

`Host.credential_ref` (String) → `keyring::Entry::new("gukab", ref)` → `get_password()`

Credentials (passwords, private keys) are **never** written to `hosts.toml`; they live
exclusively in the OS keychain via the `keyring` crate (4.x, default `v1` feature: Secret
Service over pure-Rust zbus on Linux, Windows Credential Manager, macOS legacy keychain).
`keychain_tests` in `src/tui/app.rs` round-trips a throwaway entry through the real
keychain; it is `#[ignore]`d, so run `cargo test -- --ignored keychain` on each platform
after a keyring upgrade.

## SSH key authentication

A host may set `identity_file` (path to a private key, e.g. `~/.ssh/id_ed25519`;
`~`/`$HOME` is expanded by `config::expand_tilde`). The key **file path** is the only
thing stored — the key material is never copied into `hosts.toml` or the keyring; the
file stays on disk where the user keeps it.

`ssh::client::connect` authenticates the way the OpenSSH client does — **it only uses
methods the device advertises, and offers each credential exactly once**:

1. a `none` probe first — it carries no credential; it reports the device's remaining
   methods via `AuthResult::Failure { remaining_methods }`, and doubles as the login for
   switches that do no SSH-layer auth and present their own `Username:/Password:` over
   the shell;
2. then public key if `identity_file` is set *and* the device offers `publickey`;
3. then a **single** password attempt — by `password` if offered, else
   `keyboard-interactive` (devices like Planet advertise only the latter).

The same password is never retried by a second method, and on a key host the keyring
secret is **not** offered as a login password (there it is the key passphrase), so a
wrong credential costs the device's lockout counter one attempt, not three. When
everything fails, `SshError::AuthFailed(String)` carries `auth_failure_detail`'s summary
— which methods the device offered and which one gukab tried — so the popup is enough to
diagnose from.

An **encrypted** key's passphrase is read from the host's `credential_ref` keyring entry
(so the same field holds a password for password hosts or a passphrase for key hosts; a
passphrase-less key leaves `credential_ref` empty). `load_identity` reads the key with
`ssh_key::PrivateKey::read_openssh_file`, decrypts if needed, and warns (OpenSSH-style,
non-blocking) if the key file is group/other-accessible (`warn_if_world_readable`). RSA
keys are presented with `HashAlg::Sha256` via `PrivateKeyWithHashAlg`.

## Algorithm negotiation

`legacy_compatible_preferred` offers *every* kex / cipher / MAC / host-key algorithm
russh implements, modern ones first and the weak SHA-1 / CBC / 3DES / DSA fallbacks last,
so old gear still negotiates. **This list must be maintained by hand**: russh's own
`Preferred::DEFAULT` deliberately omits the NIST curves, and Aruba's Mocana SSH 6.3 stack
offers **only** `ecdh-sha2-nistp256` — with those three curves missing, KEX failed before
the host-key check ("Key exchange init failed"). Anything russh lacks entirely
(`hmac-md5`, `hmac-sha1-96`, arcfour, blowfish, SSH-1) is out of reach here; OpenSSH
refuses those by default too, so the working rule is that a device reachable with a plain
`ssh host` will negotiate with gukab.

`algorithm_tests` (in `client.rs`) compares the list against russh's `ALL_*` tables, so a
russh upgrade that adds or drops an algorithm fails `cargo test` until the list is updated.
Only the null algorithms (`none`/`clear`) and the FIDO `sk-*` types (user keys, never host
keys) are excluded.

## Host-key verification

`ClientHandler::check_server_key` does trust-on-first-use — it records each server's
SHA-256 fingerprint in `~/.config/gukab/known_hosts` (`config::known_hosts_path()`, file
mode `0600`) and **refuses** to connect if a known host's key later changes (possible
MITM). `hosts.toml`, `known_hosts`, and session logs are all written owner-only. Since
russh 0.64 the handler receives a `PublicKeyOrCertificate`; `host_key_fingerprint` pins a
host certificate by the key it certifies (the CA is not checked), which keeps existing
`known_hosts` entries valid.

## macOS: keychain re-prompts after updates

An unsigned binary is identified to the keychain ACL by its content hash, which changes
on every update — so "Always Allow" is invalidated and macOS re-prompts for each host's
credential. `gukab --trust-keychain` ([src/macos_trust.rs](../../src/macos_trust.rs))
fixes this **without an Apple Developer ID**: it generates a per-machine **self-signed
code-signing certificate** (`gukab-local-signing`, via the system LibreSSL at
`/usr/bin/openssl` — Homebrew OpenSSL 3 p12 output fails `security import`) and
`codesign`s the binary with a fixed `--identifier gukab`. Because the signature now
identifies the binary by certificate (not hash), re-signing each new release with the
same local cert keeps "Always Allow" valid across restarts and updates.

The private key is created in a 0600 temp dir wiped right after `security import`, and
only `/usr/bin/codesign` is authorised to use it (no "allow all apps"). Host secrets are
untouched — still one keychain entry each. Existence is checked with `security
find-certificate -c` (a self-signed cert is not *trusted* for the code-signing policy, so
`find-identity -p codesigning` omits it even though `codesign --sign` works). The command
is macOS-only and idempotent (re-signs, never duplicates the cert). Flow: run `gukab
--trust-keychain` once after each update, then click "Always Allow" once.

## Terminal resize → `window-change`

`connect()` reads the local size once for `request_pty` (falling back to 80×24 if the
read fails). After that, `run_session` ([src/session/mod.rs](../../src/session/mod.rs))
forwards every resize through `Transport::resize`; `SshTransport` sends it as an SSH
`window-change` request (what OpenSSH does on SIGWINCH), so remote full-screen programs
and `$COLUMNS`/`TIOCGWINSZ` follow the local window.

- **Sources** (`ResizeWatcher`): Linux and macOS listen for SIGWINCH
  (`tokio::signal::unix`) and read `crossterm::terminal::size()`; Windows has no
  SIGWINCH, so the stdin reader thread routes crossterm `Event::Resize` into a `watch`
  channel instead of `encode_event`. Both coalesce a burst (dragging the window) to the
  final size.
- **De-duplication:** `SshTransport` remembers the size the remote PTY has and sends
  only when it changes. `run_session` also sends the current size once at start, which
  covers a resize between the PTY request and the shell coming up (and a failed initial
  size read).
- **Serial** keeps the trait's default no-op — a console port has no PTY.
- Manual check (Linux, Windows, macOS): connect in a small window, run
  `tput cols; tput lines`, resize, run it again.
