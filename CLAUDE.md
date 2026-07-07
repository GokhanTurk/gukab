# CLAUDE.md

This file documents the architecture and conventions for working in this repository.

## Project

**gukab** is a terminal-only (TUI) SSH + serial/console connection manager. Target
platforms: Arch Linux x86_64, Windows 10 1809+ (Windows Terminal), and macOS
(Intel x86_64 + Apple Silicon aarch64). When platforms are listed in any text (docs,
About, release notes), the order is **Linux, Windows, macOS** (user preference).
Config lives in `~/.config/gukab/` on Unix and
`%APPDATA%\gukab\` on Windows ([src/config/mod.rs](src/config/mod.rs) `config_dir`).
Platform-specific code is `#[cfg]`-gated; the in-session input reader is split
(Unix reads the raw stdin byte stream; Windows encodes crossterm key events to VT
bytes in [src/session/mod.rs](src/session/mod.rs) `encode_event`), and
`session::prepare_console()` enables VT output on Windows.

## Stack

- `ratatui` + `crossterm` — TUI
- `russh` `0.61.x` with `ring` feature — pure-Rust SSH client (no openssl-sys / libssh dependency)
- `ssh-key` — pinned to `=0.7.0-rc.10` (exact version russh depends on; `russh::client::AuthResult` is where russh re-exports the auth result type)
- `nucleo` — fuzzy finder
- `serde` + `toml` — config parsing
- `keyring` `3.x` — credential storage via OS keychain (keyring 4.x is a CLI tool, not a library)
- `serialport` `4.x` — pure-Rust serial/console I/O (Linux libudev, macOS IOKit — no
  openssl-sys); `libudev` is only used for port enumeration (`available_ports`)
- `tokio` — async runtime
- `thiserror` — centralized `Error` enum

## Commands

```bash
# Run (dev)
cargo run

# Build release for macOS targets
cargo build --release --target aarch64-apple-darwin
cargo build --release --target x86_64-apple-darwin

# Linux (run natively on Arch)
cargo build --release

# Lint
cargo clippy -- -D warnings

# Tests
cargo test
cargo test <test_name>   # single test
```

## Architecture

The planned module layout (being built incrementally):

```
src/
  main.rs          — entry point: load config → start TUI app → event loop
  config/          — load/save hosts.toml; Host struct with credential_ref field
  tui/
    app.rs         — App state (Elm-style Model); App::update() handles KeyEvents
    ui.rs          — render functions called by the event loop (View)
  ssh/
    client.rs      — connect(host, credential) → hands the terminal to the SSH session
```

**Data flow:**
1. `config::load_hosts()` → `Vec<Host>`
2. `tui::App::new(hosts)` → event loop: `KeyEvent → App::update() → ui::draw()`
3. `Enter` → `ssh::client::connect(host, credential)` → terminal handed to SSH session
4. `Shift+Enter` → `App` state = `EditMode(host)` → form render → Save → `config::save_hosts()`

**Credential flow:**
`Host.credential_ref` (String) → `keyring::Entry::new("gukab", ref)` → `get_password()`

Credentials (passwords, private keys) are **never** written to `hosts.toml`; they live exclusively in the OS keychain via the `keyring` crate.

**SSH key authentication:** a host may set `identity_file` (path to a private key,
e.g. `~/.ssh/id_ed25519`; `~`/`$HOME` is expanded by `config::expand_tilde`). The
key **file path** is the only thing stored — the key material is never copied into
`hosts.toml` or the keyring; the file stays on disk where the user keeps it.
`ssh::client::connect` tries auth in order: **(1)** public key (if `identity_file`
is set), **(2)** password, **(3)** keyboard-interactive, **(4)** none — each only if
the prior failed. An **encrypted** key's passphrase is read from the host's
`credential_ref` keyring entry (so the same field holds a password for password
hosts or a passphrase for key hosts; a passphrase-less key leaves `credential_ref`
empty). `load_identity` reads the key with `ssh_key::PrivateKey::read_openssh_file`,
decrypts if needed, and warns (OpenSSH-style, non-blocking) if the key file is
group/other-accessible (`warn_if_world_readable`). RSA keys are presented with
`HashAlg::Sha256` via `PrivateKeyWithHashAlg`.

**Keychain re-prompts after updates (macOS):** an unsigned binary is identified to
the keychain ACL by its content hash, which changes on every update — so "Always
Allow" is invalidated and macOS re-prompts for each host's credential. `gukab
--trust-keychain` ([src/macos_trust.rs](src/macos_trust.rs)) fixes this **without
Apple Developer ID**: it generates a per-machine **self-signed code-signing
certificate** (`gukab-local-signing`, via the system LibreSSL at `/usr/bin/openssl`
— Homebrew OpenSSL 3 p12 output fails `security import`) and `codesign`s the
binary with a fixed `--identifier gukab`. Because the signature now identifies the
binary by certificate (not hash), re-signing each new release with the same local
cert keeps "Always Allow" valid across restarts and updates. The private key is
created in a 0600 temp dir wiped right after `security import`, and only
`/usr/bin/codesign` is authorised to use it (no "allow all apps"). Host secrets are
untouched — still one keychain entry each. Existence is checked with `security
find-certificate -c` (a self-signed cert is not *trusted* for the code-signing
policy, so `find-identity -p codesigning` omits it even though `codesign --sign`
works). The command is macOS-only and idempotent (re-signs, never duplicates the
cert). Flow: run `gukab --trust-keychain` once after each update, then click
"Always Allow" once.

**Host-key verification:** `ClientHandler::check_server_key` ([src/ssh/client.rs](src/ssh/client.rs))
does trust-on-first-use — it records each server's SHA-256 fingerprint in
`~/.config/gukab/known_hosts` (`config::known_hosts_path()`, file mode `0600`) and
**refuses** to connect if a known host's key later changes (possible MITM). `hosts.toml`,
`known_hosts`, and session logs are all written owner-only.

## UI Behavior

- On launch: search box at top, host list below, live fuzzy filtering
- `Enter`: on a host row, connect via SSH; on a group header, collapse/expand the group
- `Ctrl+E`: open selected host in edit form (`Shift+Enter` also works only in terminals
  that report the modifier, e.g. kitty keyboard protocol — most terminals send plain Enter,
  so `Ctrl+E` is the reliable binding)
- `Ctrl+N`: add-host form (the form title reads **Add Host** for new, **Edit Host** when
  editing — both use the same `AppMode::Editing`, distinguished by `original_idx`)
- `Ctrl+D`: delete the selected host (a `ConfirmDelete` prompt; `y`/Enter confirms, `n`/Esc
  cancels) — removes it from `hosts.toml`
- `Ctrl+↑` / `Ctrl+↓` (or `Shift+↑` / `Shift+↓`): move the selected host up/down **within
  its group**, persisted to `hosts.toml` (disabled while a search filter is active, since the
  list is then ranked by relevance not stored order). `Shift+↑/↓` is the macOS-friendly
  alternative — there `Ctrl+↑/↓` is intercepted by Mission Control / App Exposé before
  reaching the terminal.
- `Ctrl+K`: add a standalone keyring credential (ref + password) — used for secrets that
  expect rules reference (e.g. an enable password), independent of any host
- `Ctrl+G`: open the **macro manager** — list / add / edit / delete the global macros in
  `automations.toml`, including each macro's nested expect rules (see Session Automation)
- `Ctrl+L` (or the always-present **＋ Console connection…** row at the top of the list):
  open the **console (serial) connection** form (see Serial / Console Connection)

The host list renders the name column at a fixed width (longest visible name, clamped to
12–26 cols, truncated with `…`) so the `user@host:port` column stays aligned regardless of
name length ([src/tui/ui.rs](src/tui/ui.rs) `fit_width`).

## Host Groups

Hosts can be organized into groups. A `[[groups]]` table in `hosts.toml` declares each group's
`name` and `icon` (any pasted glyph); a host joins a group via its `group = "..."` field
(editable in the host form's "Group" field). The list ([src/tui/app.rs](src/tui/app.rs)
`apply_filter` builds `Vec<Row>` of `Group`/`Host` rows; [src/tui/ui.rs](src/tui/ui.rs)
renders them) shows each group as a distinct bold header with a `▾`/`▸` collapse arrow and the
icon; member hosts are indented beneath and carry the group's icon. `Enter` on a header toggles
collapse (in-memory, default expanded). Fuzzy search matches host fields and the group name,
and force-expands groups while filtering. With no groups declared and all hosts ungrouped, the
list renders flat (legacy look). `[[groups]]` is preserved on save.

```toml
[[groups]]
name = "Core"
icon = ""

[[hosts]]
name = "sw1"
group = "Core"
# ...
```

## Session Automation

Inside an active SSH session ([src/ssh/client.rs](src/ssh/client.rs) `io_loop`), two
mechanisms run on top of the raw PTY passthrough:

- **Macros** — `Ctrl+A` is a local escape prefix (not forwarded). It opens a **fuzzy macro
  picker** popup ([src/ssh/macro_picker.rs](src/ssh/macro_picker.rs)): a centered search box +
  the macro list, filtered with the same fuzzy matcher as the host list. `↑↓` select, `Enter`
  runs, `Esc` cancels, `Ctrl+D` disconnects the session, `Ctrl+A Ctrl+A` sends a literal
  `Ctrl+A`. The picker briefly uses the alternate screen and restores the live session after.
  A macro's `send` may be **multi-line** (TOML triple-quoted string): each non-empty line is
  sent as its own command terminated by `\r` (Enter / `^M`).
- **Expect rules** — output is scanned against each rule's `pattern` (regex). On match, gukab
  auto-sends `send` (literal) or the keyring password named by `send_credential`, plus a
  newline. `once = true` fires only once per session. Credentials are read from the keyring at
  send time, never stored in config. **Expects belong to a macro** (or to a host) — there are
  no global always-on expects.
- **On-connect macros** — a host's `on_connect` field lists macro keys to auto-run right after
  connecting, e.g. `on_connect = ["en"]`. Running a macro on connect also **arms that macro's
  expects** for the session. So the "en" macro owns the `[Pp]assword:` → enable-secret rule,
  and it only applies to hosts that actually run "en"; a plain in-band-login switch (no
  `on_connect`) gets no expects and its login prompt is never auto-answered. Unknown keys
  print a notice and are skipped.

Scoping summary:
- **Macros** (global in `automations.toml`, or per-host) are always available for manual
  `Ctrl+A` use; they never auto-fire.
- **A macro's `expects` arm only when that macro runs via a host's `on_connect`.**
- A host's own `[[hosts.expects]]` always apply.

`automations.toml` (expects nested under the macro that owns them):
```toml
[[macros]]
key = "en"
send = "enable"

  [[macros.expects]]
  pattern = "[Pp]assword:"
  send_credential = "enable1"   # or: send = "literal"
  once = false

# Multi-line macro — each non-empty line is sent as a separate Enter-terminated command.
[[macros]]
key = "kd-m"
send = """
switchport access vlan 750
switchport mode access
spanning-tree portfast edge
spanning-tree bpduguard enable
"""
```
A Cisco host opts in with `on_connect = ["en"]`; a Planet-style switch leaves `on_connect`
empty and logs in untouched.

Store the `enable1` secret with `Ctrl+K` in the TUI. macOS maps
`keyring::Entry::new("gukab", "enable1")` to keychain **service=`gukab`, account=`enable1`**;
the manual equivalent is `security add-generic-password -s gukab -a enable1 -w '<pass>' -U`.

Engine internals: `connect()` merges global + per-host lists, `build_automations()` compiles
rules (rejecting invalid regex or rules that set both/neither of `send`/`send_credential`),
`scan_and_respond()` matches against a rolling 8 KB buffer, and the `Ctrl+A` prompt is
handled by `forward_stdin` / `run_macro_prompt`. The escape key is the `ESCAPE_PREFIX`
constant (currently `0x01`).

The **macro manager** (`Ctrl+G` from the host list, [src/tui/app.rs](src/tui/app.rs)
`update_macros`, [src/tui/ui.rs](src/tui/ui.rs) `draw_macros`) lists, adds, edits, and
deletes the **global** macros in `automations.toml`, including each macro's nested
`expects`. `send` is edited as a list of single-line command rows (each row = one
command; joined with `\n` on save, matching the runtime's line-per-command semantics).
Saves are validated to mirror the engine — unique non-empty `key`, at least one command
line; each expect needs a valid regex `pattern` and exactly one of `send` /
`send_credential` — then persisted via `config::save_automations` (owner-only; like
`save_hosts` it rewrites the file and drops hand-written comments). Per-host macros
(`[[hosts.macros]]`) are still edited in TOML directly; `Ctrl+K` only adds credentials.

## Session Logging

Every session's remote output is logged to
`~/.config/gukab/log/<host>/<YYYY-MM-DD_HH-MM-SS>.log` (folder per host, file per session;
host name sanitized to `[A-Za-z0-9._-]`, falling back to `hostname`). The log is a raw
transcript (like `script(1)`): commands echoed by the remote plus their output. Passwords
typed at prompts are not captured because the remote does not echo them.

Implemented in [src/ssh/session_log.rs](src/ssh/session_log.rs): `session_log::start(label)`
(the label is the host name/hostname for SSH, the device basename for serial) opens the file
and spawns a **dedicated writer thread** fed by an unbounded mpsc channel. The interactive
`run_session` loop only does an in-memory `tx.send(data.to_vec())` per output chunk — no disk
I/O on the hot path — so logging never adds typing latency. The thread batches writes
(drain-then-flush per burst) and flushes/closes when the session ends. If the log file can't
be created the session continues unlogged with a stderr warning.

## Serial / Console Connection

gukab also connects over a **serial console** (USB-to-serial into a device console port).
This is **ephemeral — never persisted**: `Ctrl+L` (or the always-present **＋ Console
connection…** row at the top of the host list) opens a form ([src/tui/app.rs](src/tui/app.rs)
`ConsoleForm`; [src/tui/ui.rs](src/tui/ui.rs) `draw_console_form`). Fields: **Device** (↑↓
cycles auto-detected ports from `serialport::available_ports`, `Ctrl+R` rescans, or type a
path) and **Baud** (↑↓ cycles presets `[9600,19200,38400,57600,115200]`, default 9600). A
collapsed **Advanced** row (Enter toggles) exposes data bits / parity / stop bits / flow
(defaults **8-N-1, no flow** — connect works without opening it). `Enter` builds a transient
`serial::SerialParams` (validated: non-empty device, baud > 0) and exits the event loop;
[src/tui/mod.rs](src/tui/mod.rs) then calls `serial::client::connect_serial`.

A console session has **full parity with SSH** — `Ctrl+A` macro picker, expect rules (armed
when a macro runs), session logging, output colorization. On serial the `Ctrl+A` picker pins a
**"⇅ baud rate…" entry at the top** (shown only on the empty picker); selecting it opens a
**baud chooser** ([src/ssh/baud_picker.rs](src/ssh/baud_picker.rs)) where `↑↓` picks a preset
directly or you type a custom rate, `Enter` applies it live. No dedicated key, so nothing
clashes with an outer multiplexer like tmux; serial consoles are a baud-guessing game. Only
global macros apply (there is no host).

The device field defaults to the first **auto-detected** port (USB adapters are ranked first
via `serialport::available_ports` port type), falling back to `/dev/ttyUSB0` on Linux. If the
port can't be opened for lack of permission (Linux serial nodes are group-owned), gukab prints
the exact one-time fix — resolving the device's **actual** owning group from `/etc/group`
(`dialout` on Debian/Ubuntu, `uucp` on Arch): `sudo usermod -aG <group> $USER`. gukab itself
is never run as root, so config stays under your user.

**One transport-agnostic engine.** The interactive loop is
[src/session/mod.rs](src/session/mod.rs) `run_session`, generic over a `Transport` trait
(`write` + `recv`); it owns the macro picker, expect engine (`build_automations`/
`scan_and_respond`), logging, colorization, and stdin passthrough. SSH provides `SshTransport`
(over a `russh::Channel`, [src/ssh/client.rs](src/ssh/client.rs)); serial provides
`SerialTransport` ([src/serial/client.rs](src/serial/client.rs)). The serial port is owned by
one blocking worker thread that reads (→ an mpsc the transport drains) and, between reads,
applies queued writes / `set_baud_rate` commands — no `try_clone`, so live baud changes never
race the reader. `Ctrl+B` is only intercepted when a `BaudControl` is present (serial); on SSH
it is forwarded normally.

## Constraints

- **No openssl-sys**: prefer `rustls`/`ring` for cross-compilation ease
- **No shell-out to ssh binary**: use `russh` directly
- **No GUI or web UI**
- `unwrap()` is banned in production code — use `thiserror` error propagation
- Before implementing any new feature: write a plan first, wait for approval, then code
