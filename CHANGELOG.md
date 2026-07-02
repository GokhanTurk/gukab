# Changelog

All notable changes to **gukab** are documented here.
This project follows [Semantic Versioning](https://semver.org).

## [Unreleased]

### Added
- **Serial / console connections.** Connect to a device's console port over a
  USB-to-serial adapter — no reachable IP required. `Ctrl+L` (or the new
  **＋ Console connection…** row at the top of the list) opens an ephemeral form
  (nothing is saved): pick the **device** (auto-detected ports are cycled with ↑↓,
  `Ctrl+R` rescans, or type a path) and **baud** (↑↓ cycles 9600/19200/38400/57600/
  115200; default 9600). The device defaults to the first auto-detected port (USB
  adapters first), or `/dev/ttyUSB0` on Linux. An **Advanced** section (collapsed by
  default) exposes data bits / parity / stop bits / flow control — defaults are 8-N-1
  with no flow, which fits virtually every network console cable, so you can just
  connect. A console session has the same features as SSH — `Ctrl+A` macro picker,
  expect rules, session logging and colorized output — and the `Ctrl+A` picker also
  offers a **baud chooser** (pick a preset with ↑↓ or type a custom rate) to change
  speed live. If the port can't be opened for lack of permission, gukab names the
  exact group to join (e.g. `uucp` on Arch, `dialout` on Debian) and how to apply it
  (log out/in, or `newgrp`) — it never runs as root.

## [1.5.0] - 2026-07-01

### Added
- **In-app macro manager (`Ctrl+G`).** List, add, edit, and delete the global macros
  in `automations.toml` from inside gukab — no more hand-editing TOML. Each macro's
  `send` is edited as a list of command lines (Enter adds a line, `Ctrl+D` removes
  one), and its nested **expect** rules are managed too (`Ctrl+X` add, `Ctrl+E`/Enter
  edit, `Ctrl+D` delete). Saves are validated to match the automation engine (unique
  non-empty key, ≥1 command; each expect needs a valid regex and exactly one of
  `send` / `send_credential`) and written owner-only. A macro edited this session is
  live on the next connect. Per-host macros are still edited in TOML directly.

## [1.4.0] - 2026-06-24

### Added
- **`gukab --trust-keychain` (macOS)** — stops the keychain from re-prompting for
  credentials after every update. An unsigned binary is identified to the keychain
  by its content hash, which changes on each update and invalidates "Always Allow".
  This command gives the binary a stable, **per-machine self-signed code-signing
  identity** (`gukab-local-signing`) and signs it, so the keychain identifies it by
  certificate instead of hash — "Always Allow" then persists across restarts and
  updates. Free (no Apple Developer ID). The signing key is generated locally,
  authorised for `codesign` only, and its temp material is wiped right after import;
  host secrets are untouched. Run it once after each update. You then click
  "Always Allow" once per distinct credential (not per host).

### Fixed
- `gukab -h` now shows that hosts can be reordered with `Shift+↑/↓` as well as
  `Ctrl+↑/↓`.

## [1.3.0] - 2026-06-23

### Added
- **SSH key authentication.** A host can set an `identity_file` (private key path,
  e.g. `~/.ssh/id_ed25519`) — set it in the host form's new **SSH key file** field.
  Only the path is stored; the key material is never copied into `hosts.toml` or the
  keyring. Auth is tried in order: public key → password → keyboard-interactive →
  none. An encrypted key's passphrase is read from the host's keyring `credential_ref`
  (store it with `Ctrl+K`); a passphrase-less key needs no credential at all.
  Keys load via OpenSSH, legacy PEM (PKCS#1 `BEGIN RSA PRIVATE KEY`), PKCS#8, and
  PuTTY formats; a group/other-readable key file triggers a non-blocking warning.
- **Reorder hosts with `Shift+↑` / `Shift+↓`** in addition to `Ctrl+↑` / `Ctrl+↓` —
  on macOS `Ctrl+↑/↓` is intercepted by Mission Control before reaching the terminal.

### Changed
- `credential_ref` is now optional in `hosts.toml` — a key-only host (or an in-band
  login switch) no longer needs a placeholder credential.

### Fixed
- A failed expect-rule credential lookup (a missing or mistyped keyring ref) no
  longer tears down the live SSH session. It now warns once, disarms that rule, and
  lets you answer the prompt by hand.
- Status-bar messages are transient again: a warning (e.g. "clear the search to
  reorder") is cleared on the next keypress, so the keybinding hints reappear.

## [1.2.0] - 2026-06-18

### Added
- `gukab --version` / `-V` and `gukab --help` / `-h`.
- Example configs: [`examples/hosts.toml`](examples/hosts.toml) and
  [`examples/automations.toml`](examples/automations.toml), fully commented.
- The running version is shown in the side panel (under the switch).

### Changed
- The macro picker (`Ctrl+A`) now ranks results by relevance — same fuzzy scoring
  as the host list — instead of an unordered match list.
- The installer now installs to `~/.local/bin` (was `~/.cargo/bin`); gukab is a
  standalone binary, not a cargo-installed dev tool.

### Fixed
- `q` no longer quits the host list — it now types into the search box like any
  other character (the search is always live). Quit with `Esc`.

## [1.1.1] - 2026-06-18

### Fixed
- Selected host row is now readable in **every** terminal. The highlight used a
  named blue background while the row kept its own cyan/gray text, so it was
  low-contrast on wezterm/alacritty (only some palettes looked fine). It now
  uses an explicit RGB bar — bright white bold text on vivid blue — identical
  across terminals.

## [1.1.0] - 2026-06-18

### Added
- **Delete a host** with `Ctrl+D` (confirmation prompt; `y`/Enter to delete,
  `n`/Esc to cancel).
- **Reorder hosts** with `Ctrl+↑` / `Ctrl+↓` — moves the selected host up/down
  within its group and persists to `hosts.toml`.
- Animated demo (VHS) in the README.

### Changed
- The host form title reflects the action: **Add Host** for `Ctrl+N`,
  **Edit Host** for `Ctrl+E`.
- The address column is now aligned: the name column is a fixed width and long
  names are truncated with `…`, so `user@host:port` lines up regardless of name
  length.

## [1.0.0] - 2026-06-17

### Added
- First public release. A terminal-only (TUI) SSH connection manager for network
  devices. Targets Arch Linux x86_64 and macOS (Apple Silicon + Intel).
- **Fuzzy host search** with relevance ranking — the closest match floats to the top.
- **Host groups** — collapsible, per-group icons, indented members.
- **Session automation** — keyboard macros (`Ctrl+A` fuzzy picker) and regex
  `expect` rules that auto-answer prompts (e.g. enable passwords).
- **Credentials in the OS keychain** — never written to config files.
- **Per-session logging** — every session transcript saved per host.

### Security
- Trust-on-first-use SSH **host-key verification** (`~/.config/gukab/known_hosts`);
  connections are refused if a known host's key changes (possible MITM).
- `hosts.toml`, `known_hosts`, and session logs are written owner-only (`0600`/`0700`).

### Distribution
- Prebuilt binaries, a `curl | sh` installer, and a self-updater (`gukab-update`)
  via cargo-dist; published on tagged releases.

[1.2.0]: https://github.com/GokhanTurk/gukab/releases/tag/v1.2.0
[1.1.1]: https://github.com/GokhanTurk/gukab/releases/tag/v1.1.1
[1.1.0]: https://github.com/GokhanTurk/gukab/releases/tag/v1.1.0
[1.0.0]: https://github.com/GokhanTurk/gukab/releases/tag/v1.0.0
