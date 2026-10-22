# Serial / console connection and the transport-agnostic engine

Read this before touching [src/serial/](../../src/serial/),
[src/session/mod.rs](../../src/session/mod.rs), or the console form in
[src/tui/app.rs](../../src/tui/app.rs) (`ConsoleForm`).

## Console connections are ephemeral — never persisted

`Ctrl+L` (or the always-present **＋ Console connection…** row at the top of the host list)
opens a form ([src/tui/app.rs](../../src/tui/app.rs) `ConsoleForm`;
[src/tui/ui.rs](../../src/tui/ui.rs) `draw_console_form`). Fields:

- **Device** — `↑↓` cycles auto-detected ports from `serialport::available_ports`,
  `Ctrl+R` rescans, or type a path
- **Baud** — `↑↓` cycles presets `[9600, 19200, 38400, 57600, 115200]`, default 9600
- a collapsed **Advanced** row (Enter toggles) exposes data bits / parity / stop bits /
  flow (defaults **8-N-1, no flow** — connect works without opening it)

`Enter` builds a transient `serial::SerialParams` (validated: non-empty device, baud > 0)
and exits the event loop; [src/tui/mod.rs](../../src/tui/mod.rs) then calls
`serial::client::connect_serial`.

## Full parity with SSH

A console session has the `Ctrl+A` macro picker, expect rules (armed when a macro runs),
session logging, and output colorization — see [automation.md](automation.md). Only global
macros apply (there is no host).

On serial the `Ctrl+A` picker pins a **"⇅ baud rate…" entry at the top** (shown only on
the empty picker); selecting it opens a **baud chooser**
([src/ssh/baud_picker.rs](../../src/ssh/baud_picker.rs)) where `↑↓` picks a preset directly
or you type a custom rate, `Enter` applies it live. No dedicated key, so nothing clashes
with an outer multiplexer like tmux; serial consoles are a baud-guessing game.

## Device detection and permissions

The device field defaults to the first **auto-detected** port (USB adapters are ranked
first via `serialport::available_ports` port type), falling back to `/dev/ttyUSB0` on
Linux. If the port can't be opened for lack of permission (Linux serial nodes are
group-owned), gukab prints the exact one-time fix — resolving the device's **actual**
owning group from `/etc/group` (`dialout` on Debian/Ubuntu, `uucp` on Arch):
`sudo usermod -aG <group> $USER`. gukab itself is never run as root, so config stays under
your user.

## One transport-agnostic engine

The interactive loop is [src/session/mod.rs](../../src/session/mod.rs) `run_session`,
generic over a `Transport` trait (`write` + `recv`, plus `resize`, which serial leaves as
the default no-op — see "Terminal resize" in [ssh.md](ssh.md)); it owns the macro picker, expect
engine (`build_automations` / `scan_and_respond`), logging, colorization, and stdin
passthrough.

- SSH provides `SshTransport` (over a `russh::Channel`,
  [src/ssh/client.rs](../../src/ssh/client.rs))
- serial provides `SerialTransport` ([src/serial/client.rs](../../src/serial/client.rs))

The serial port is owned by one blocking worker thread that reads (→ an mpsc the transport
drains) and, between reads, applies queued writes / `set_baud_rate` commands — no
`try_clone`, so live baud changes never race the reader. `Ctrl+B` is only intercepted when
a `BaudControl` is present (serial); on SSH it is forwarded normally.
