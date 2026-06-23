# CLAUDE.md

This file documents the architecture and conventions for working in this repository.

## Project

**gukab** is a terminal-only (TUI) SSH connection manager. Target platforms: macOS (Intel x86_64 + Apple Silicon aarch64) and Arch Linux x86_64.

## Stack

- `ratatui` + `crossterm` — TUI
- `russh` `0.61.x` with `ring` feature — pure-Rust SSH client (no openssl-sys / libssh dependency)
- `ssh-key` — pinned to `=0.7.0-rc.10` (exact version russh depends on; `russh::client::AuthResult` is where russh re-exports the auth result type)
- `nucleo` — fuzzy finder
- `serde` + `toml` — config parsing
- `keyring` `3.x` — credential storage via OS keychain (keyring 4.x is a CLI tool, not a library)
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

TUI listing/editing of existing macros/expects is not implemented yet — edit
`automations.toml` directly. `Ctrl+K` only adds credentials.

## Session Logging

Every session's remote output is logged to
`~/.config/gukab/log/<host>/<YYYY-MM-DD_HH-MM-SS>.log` (folder per host, file per session;
host name sanitized to `[A-Za-z0-9._-]`, falling back to `hostname`). The log is a raw
transcript (like `script(1)`): commands echoed by the remote plus their output. Passwords
typed at prompts are not captured because the remote does not echo them.

Implemented in [src/ssh/session_log.rs](src/ssh/session_log.rs): `session_log::start(host)`
opens the file and spawns a **dedicated writer thread** fed by an unbounded mpsc channel. The
interactive `io_loop` only does an in-memory `tx.send(data.to_vec())` per output chunk — no
disk I/O on the hot path — so logging never adds typing latency. The thread batches writes
(drain-then-flush per burst) and flushes/closes when the session ends. If the log file can't
be created the session continues unlogged with a stderr warning.

## Constraints

- **No openssl-sys**: prefer `rustls`/`ring` for cross-compilation ease
- **No shell-out to ssh binary**: use `russh` directly
- **No GUI or web UI**
- `unwrap()` is banned in production code — use `thiserror` error propagation
- Before implementing any new feature: write a plan first, wait for approval, then code
