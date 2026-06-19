# Changelog

All notable changes to **gukab** are documented here.
This project follows [Semantic Versioning](https://semver.org).

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
