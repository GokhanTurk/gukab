# Session automation: macros, expect rules, logging

Read this before touching [src/session/mod.rs](../../src/session/mod.rs),
[src/ssh/macro_picker.rs](../../src/ssh/macro_picker.rs), the macro manager in
[src/tui/app.rs](../../src/tui/app.rs) (`update_macros`), or
[src/ssh/session_log.rs](../../src/ssh/session_log.rs).

## Two mechanisms on top of raw PTY passthrough

- **Macros** — `Ctrl+A` is a local escape prefix (not forwarded). It opens a **fuzzy
  macro picker** popup ([src/ssh/macro_picker.rs](../../src/ssh/macro_picker.rs)): a
  centered search box + the macro list, filtered with the same fuzzy matcher as the host
  list. `↑↓` select, `Enter` runs, `Esc` cancels, `Ctrl+D` disconnects the session,
  `Ctrl+A Ctrl+A` sends a literal `Ctrl+A`. The picker briefly uses the alternate screen
  and restores the live session after. A macro's `send` may be **multi-line** (TOML
  triple-quoted string): each non-empty line is sent as its own command terminated by
  `\r` (Enter / `^M`).

- **Expect rules** — output is scanned against each rule's `pattern` (regex). On match,
  gukab auto-sends `send` (literal) or the keyring password named by `send_credential`,
  plus a newline. **Every rule is one-shot per arming**: it fires at most once, then stays
  disarmed until the macro that owns it runs again (each run — `on_connect` or manual
  `Ctrl+A` — re-arms it for exactly one more firing, replacing any earlier copies). So a
  wrong auto-sent password is never retried into an account lockout, and a rule that
  already fired never answers an unrelated later prompt (e.g. the `Password:` asked while
  creating a user). A legacy `once` field in config files is ignored. Credentials are read
  from the keyring at send time, never stored in config. **Expects belong to a macro** (or
  to a host) — there are no global always-on expects.

- **On-connect macros** — a host's `on_connect` field lists macro keys to auto-run right
  after connecting, e.g. `on_connect = ["en"]`. Running a macro on connect also **arms
  that macro's expects** (one firing each). So the "en" macro owns the `[Pp]assword:` →
  enable-secret rule, and it only applies to hosts that actually run "en"; a plain
  in-band-login switch (no `on_connect`) gets no expects and its login prompt is never
  auto-answered. Unknown keys print a notice and are skipped.

- **Pasteable credentials** — the picker also lists `🔑` rows: the connected host's own
  login password (pinned at the top; only for password hosts — a key host's
  `credential_ref` is its key passphrase and is never offered) and every name in
  `[[credentials]]` (`automations.toml`, names only; `Ctrl+K` adds to it). Picking one
  reads the secret from the keychain at that moment (`read_secret`, wrapped in
  `zeroize::Zeroizing`), and writes it to the transport **without** a newline. Nothing
  automatic is involved — expects never touch these rows. Before sending, a
  `PromptTail` (the current output line, escape sequences stripped) is checked: if it
  doesn't end in a password prompt (`password` / `passphrase` / `passcode` / `parola` /
  `şifre` … `:`), `confirm_paste` asks `[y/N]` on the local terminal — pasting into a
  shell would echo the secret onto the screen, the session log and the remote history.
  The secret is never shown or logged by gukab itself; once handed to the transport
  (russh's buffers, the serial worker) it is outside gukab's control.

### Scoping summary

- **Macros** (global in `automations.toml`, or per-host) are always available for manual
  `Ctrl+A` use; they never auto-fire.
- **A macro's `expects` arm each time that macro runs** (via `on_connect` or `Ctrl+A`),
  for at most one firing per rule per run.
- A host's own `[[hosts.expects]]` are armed at connect (also one firing each).

## automations.toml

Expects are nested under the macro that owns them; `[[credentials]]` lists the
keychain names the picker may paste:

```toml
[[macros]]
key = "en"
send = "enable"

  [[macros.expects]]
  pattern = "[Pp]assword:"
  send_credential = "enable1"   # or: send = "literal"

[[credentials]]
ref = "ubuntu24-sudo"         # secret saved with Ctrl+K; only the name lives here

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
`keyring::Entry::new("gukab", "enable1")` to keychain **service=`gukab`,
account=`enable1`**; the manual equivalent is
`security add-generic-password -s gukab -a enable1 -w '<pass>' -U`.

## Engine internals

`connect()` merges global + per-host lists, `build_automations()` compiles rules
(rejecting invalid regex or rules that set both/neither of `send`/`send_credential`),
`scan_and_respond()` matches against a rolling 8 KB buffer, and the `Ctrl+A` prompt is
handled by `forward_stdin` / `run_macro_prompt`. The escape key is the `ESCAPE_PREFIX`
constant (currently `0x01`).

## Macro manager

`Ctrl+G` from the host list ([src/tui/app.rs](../../src/tui/app.rs) `update_macros`,
[src/tui/ui.rs](../../src/tui/ui.rs) `draw_macros`) lists, adds, edits, and deletes the
**global** macros in `automations.toml`, including each macro's nested `expects`. `send`
is edited as a list of single-line command rows (each row = one command; joined with `\n`
on save, matching the runtime's line-per-command semantics). Saves are validated to mirror
the engine — unique non-empty `key`, at least one command line; each expect needs a valid
regex `pattern` and exactly one of `send` / `send_credential` — then persisted via
`config::save_automations` (owner-only; like `save_hosts` it rewrites the file and drops
hand-written comments). Per-host macros (`[[hosts.macros]]`) are still edited in TOML
directly; `Ctrl+K` only adds credentials.

## Session logging

Every session's remote output is logged to
`~/.config/gukab/log/<host>/<YYYY-MM-DD_HH-MM-SS>.log` (folder per host, file per session;
host name sanitized to `[A-Za-z0-9._-]`, falling back to `hostname`). The log is a raw
transcript (like `script(1)`): commands echoed by the remote plus their output. Passwords
typed at prompts are not captured because the remote does not echo them.

Implemented in [src/ssh/session_log.rs](../../src/ssh/session_log.rs):
`session_log::start(label)` (the label is the host name/hostname for SSH, the device
basename for serial) opens the file and spawns a **dedicated writer thread** fed by an
unbounded mpsc channel. The interactive `run_session` loop only does an in-memory
`tx.send(data.to_vec())` per output chunk — no disk I/O on the hot path — so logging never
adds typing latency. The thread batches writes (drain-then-flush per burst) and
flushes/closes when the session ends. If the log file can't be created the session
continues unlogged with a stderr warning.
