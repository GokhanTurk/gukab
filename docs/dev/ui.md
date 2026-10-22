# TUI: keybindings, groups, notes panel, list rendering

Read this before touching [src/tui/app.rs](../../src/tui/app.rs),
[src/tui/ui.rs](../../src/tui/ui.rs), or [src/notes/mod.rs](../../src/notes/mod.rs).

## Keybindings (host list)

On launch: search box at top, host list below, live fuzzy filtering.

| Key | Action |
|---|---|
| `Enter` | on a host row, connect via SSH; on a group header, collapse/expand the group |
| `Ctrl+E` | open selected host in edit form (`Shift+Enter` also works, but only in terminals that report the modifier, e.g. kitty keyboard protocol — most terminals send plain Enter, so `Ctrl+E` is the reliable binding) |
| `Ctrl+N` | add-host form (the form title reads **Add Host** for new, **Edit Host** when editing — both use the same `AppMode::Editing`, distinguished by `original_idx`) |
| `Ctrl+D` | delete the selected host (a `ConfirmDelete` prompt; `y`/Enter confirms, `n`/Esc cancels) — removes it from `hosts.toml` |
| `Ctrl+↑` / `Ctrl+↓` (or `Shift+↑` / `Shift+↓`) | move the selected host up/down **within its group**, persisted to `hosts.toml` (disabled while a search filter is active, since the list is then ranked by relevance not stored order). `Shift+↑/↓` is the macOS-friendly alternative — there `Ctrl+↑/↓` is intercepted by Mission Control / App Exposé before reaching the terminal |
| `Ctrl+K` | add a standalone keyring credential (ref + password) — for secrets that expect rules reference (e.g. an enable password), independent of any host. The ref is also added to `[[credentials]]` in `automations.toml`, so the in-session picker can paste it |
| `Ctrl+G` | open the **macro manager** (see [automation.md](automation.md)) |
| `Ctrl+L` | open the **console (serial) connection** form (the always-present **＋ Console connection…** row at the top of the list does the same — see [serial.md](serial.md)) |
| `Ctrl+O` | focus the **notes panel** on the right (un-hiding it if hidden) |

The host list renders the name column at a fixed width (longest visible name, clamped to
12–26 cols, truncated with `…`) so the `user@host:port` column stays aligned regardless of
name length ([src/tui/ui.rs](../../src/tui/ui.rs) `fit_width`).

## A connection that never comes up returns to the list

`tui::run` owns an outer loop (TUI → connect → TUI); `App` is created there, not inside
`event_loop`, so the selection, search filter, collapsed groups and any host edits survive
the round trip. A failed connect (auth, host-key mismatch, serial permission, bad expect
regex) becomes `AppMode::Error(String)` — a centered popup over the list (`draw_error`),
sized to the message because some run several lines, dismissed with `Esc`. A session that
actually ran and then ended still **exits** gukab, leaving its scrollback on the shell.

## Host groups

Hosts can be organized into groups. A `[[groups]]` table in `hosts.toml` declares each
group's `name` and `icon` (any pasted glyph); a host joins a group via its `group = "..."`
field (editable in the host form's "Group" field). The list
([src/tui/app.rs](../../src/tui/app.rs) `apply_filter` builds `Vec<Row>` of `Group`/`Host`
rows; [src/tui/ui.rs](../../src/tui/ui.rs) renders them) shows each group as a distinct
bold header with a `▾`/`▸` collapse arrow and the icon; member hosts are indented beneath
and carry the group's icon. `Enter` on a header toggles collapse (in-memory, default
expanded). Fuzzy search matches host fields and the group name, and force-expands groups
while filtering. With no groups declared and all hosts ungrouped, the list renders flat
(legacy look). `[[groups]]` is preserved on save.

```toml
[[groups]]
name = "Core"
icon = ""

[[hosts]]
name = "sw1"
group = "Core"
# ...
```

## Notes panel

The right-hand panel (GUKAB logo + clock + animated switch,
[src/tui/ui.rs](../../src/tui/ui.rs) `draw_banner`) has a **notes section** beneath the
banner (`draw_side_panel` / `draw_notes_panel`; drawn only when the panel fits — terminal
≥ 90 cols and enough rows under the 17-line banner).

Each note is a plain **`.md` file** in `~/.config/gukab/notes/` (`config::notes_dir()`;
`%APPDATA%\gukab\notes\` on Windows) — the title is the file name (stem, unicode
preserved; forbidden path characters sanitized by
[src/notes/mod.rs](../../src/notes/mod.rs) `sanitize_title`), so notes can also be created
and edited outside gukab. Directory 0700, files 0600 on Unix. The list is sorted most
recently modified first and re-read on every `Ctrl+O` (so external edits show up).

The section header label is configurable (default **Notes**) and lives in
`~/.config/gukab/settings.toml` (`config::{Settings, load_settings, save_settings}`,
`[notes] label / hidden`) — the on-disk folder is always `notes` regardless of the label.
Unknown settings fields are ignored for forward compatibility.

`Ctrl+O` focuses the panel (`AppMode::Notes(NotesScreen)`,
[src/tui/app.rs](../../src/tui/app.rs) `update_notes` — same accumulator pattern as the
macro manager). Keys while focused (shown as hints **under the notes section**, not in the
status bar): `↑↓` select, `Enter` quick preview (centered scrollable popup), `a` add (title
prompt → file created → opens in the editor), `e` edit, `r` rename (renames the file), `d`
delete (confirmed), `l` change the section label, `h` hide the section (persisted; `Ctrl+O`
un-hides), `Esc` back.

**Editing uses an external editor**: on Linux/macOS `$VISUAL` → `$EDITOR` → first of
nano/vim/vi on `PATH`; on Windows always Notepad (guaranteed present). The event loop
([src/tui/mod.rs](../../src/tui/mod.rs)) sees `App.pending_note_edit`, releases the
terminal (`ratatui::restore`), runs the editor blocking, re-inits the TUI and reloads the
notes. (Windows 11's tabbed Notepad may detach immediately; the reload-on-focus covers
that.)
