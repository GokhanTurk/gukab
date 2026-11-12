//! Turn a raw terminal byte stream into a readable plain-text transcript.
//!
//! A session log used to be the remote side's raw output: colors, window-title and
//! systemd OSC 3008 markers, bracketed-paste toggles and — worst — readline's
//! line-editing redraws (`\b`, `ESC[K`, `ESC[1@` …). Stripping the escape codes alone
//! leaves *wrong* commands behind (`scp a@10monitoring…` after a Backspace fix), so
//! [`LogCleaner`] emulates a single terminal line instead: it keeps the current line
//! and the cursor column, applies the cursor/erase/insert/delete sequences, and emits
//! the line's final form when a newline commits it.
//!
//! Extras:
//! - Full-screen programs (nano, vim, less, htop — anything on the alternate screen)
//!   are not drawn; they become one `[full-screen program …]` marker line.
//! - systemd's OSC 3008 context markers (Ubuntu 26.04+) become a
//!   `── <time>  <cwd> ──` header above each command and an `[exit N]` line after a
//!   failed one.
//! - A line that was written and then erased to blank (Cisco's `--More--`) is dropped.
//!
//! Known limit: readline editing a command longer than the terminal width moves the
//! cursor up a row (`ESC[A`), which a one-line model cannot follow.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

/// Suffix of the first log line in a file the cleaner wrote; the converter skips those.
pub const CLEAN_MARK: &str = "(clean)";

/// Cap on buffered escape-sequence bytes, so a malformed stream can't grow them forever.
const MAX_SEQ: usize = 4096;

#[derive(Default)]
enum State {
    #[default]
    Ground,
    Esc,
    /// CSI parameter/intermediate bytes collected so far.
    Csi(Vec<u8>),
    /// OSC payload collected so far.
    Osc(Vec<u8>),
    /// OSC payload, an ESC seen (expecting `\` to terminate).
    OscEsc(Vec<u8>),
    /// DCS/SOS/PM/APC string — ignored until its terminator.
    Str,
    StrEsc,
    /// `ESC (`-style sequences: swallow exactly one more byte.
    OneMore,
}

/// Streaming cleaner: [`feed`](Self::feed) raw chunks in, get finished lines out.
#[derive(Default)]
pub struct LogCleaner {
    state: State,
    line: Vec<char>,
    col: usize,
    /// The current line has shown a non-space character at some point.
    had_text: bool,
    /// Bytes of an incomplete UTF-8 character.
    utf8: Vec<u8>,
    /// `Some(start time)` while a full-screen program owns the alternate screen.
    alt: Option<Option<String>>,
    /// OSC 3008: a shell prompt began; lines committed now are prompt + command.
    awaiting_cmd: bool,
    /// Prompt/command lines held back until we know whether a command started.
    held: Vec<String>,
    /// OSC 3008 ids of commands still running (nested shells push more).
    running: Vec<String>,
    /// Time of the chunk being processed (`None` for offline conversion).
    time: Option<String>,
    out: String,
}

impl LogCleaner {
    pub fn new() -> Self {
        Self::default()
    }

    /// Process one raw chunk; returns the text that became final (whole lines, each
    /// ending in `\n`). `time` (e.g. `"14:32:05"`) stamps headers and markers produced
    /// by this chunk.
    pub fn feed(&mut self, bytes: &[u8], time: Option<&str>) -> String {
        self.time = time.map(str::to_owned);
        for &b in bytes {
            self.byte(b);
        }
        std::mem::take(&mut self.out)
    }

    /// End of stream: flush the unfinished last line (usually the final prompt).
    pub fn finish(&mut self, time: Option<&str>) -> String {
        self.time = time.map(str::to_owned);
        self.flush_utf8();
        if let Some(start) = self.alt.take() {
            let marker = self.alt_marker(start.as_deref(), None);
            self.emit(&marker);
        }
        let rest = self.take_line();
        if !rest.is_empty() {
            self.held.push(rest);
        }
        self.flush_held();
        std::mem::take(&mut self.out)
    }

    fn byte(&mut self, b: u8) {
        match std::mem::take(&mut self.state) {
            State::Ground => self.ground(b),
            State::Esc => self.esc(b),
            State::Csi(mut buf) => match b {
                0x40..=0x7e => self.csi(&buf, b),
                0x20..=0x3f if buf.len() < MAX_SEQ => {
                    buf.push(b);
                    self.state = State::Csi(buf);
                }
                0x1b => self.state = State::Esc,
                // Any other byte aborts the sequence (CAN/SUB or garbage).
                _ => {}
            },
            State::Osc(mut buf) => match b {
                0x07 => self.osc(&buf),
                0x1b => self.state = State::OscEsc(buf),
                0x18 | 0x1a => {}
                _ => {
                    if buf.len() < MAX_SEQ {
                        buf.push(b);
                    }
                    self.state = State::Osc(buf);
                }
            },
            State::OscEsc(buf) => {
                // ESC \ is the proper terminator; any other ESC x also ends the OSC.
                self.osc(&buf);
                if b != b'\\' {
                    self.esc(b);
                }
            }
            State::Str => match b {
                0x07 | 0x18 | 0x1a => {}
                0x1b => self.state = State::StrEsc,
                _ => self.state = State::Str,
            },
            State::StrEsc => {
                if b != b'\\' {
                    self.state = State::Str;
                }
            }
            State::OneMore => {}
        }
    }

    fn ground(&mut self, b: u8) {
        if b >= 0x80 {
            self.utf8_byte(b);
            return;
        }
        self.flush_utf8();
        match b {
            0x1b => self.state = State::Esc,
            b'\n' => self.commit(),
            b'\r' => self.col = 0,
            0x08 => self.col = self.col.saturating_sub(1),
            b'\t' => self.col = (self.col / 8 + 1) * 8,
            0x20..=0x7e => self.put(b as char),
            _ => {} // BEL, DEL and other C0 controls
        }
    }

    fn utf8_byte(&mut self, b: u8) {
        if (0x80..0xc0).contains(&b) && self.utf8.is_empty() {
            self.put(char::REPLACEMENT_CHARACTER); // stray continuation byte
            return;
        }
        if b >= 0xc0 && !self.utf8.is_empty() {
            self.flush_utf8(); // new lead byte before the previous char completed
        }
        self.utf8.push(b);
        let need = match self.utf8[0] {
            0xc0..=0xdf => 2,
            0xe0..=0xef => 3,
            _ => 4,
        };
        if self.utf8.len() >= need {
            let bytes = std::mem::take(&mut self.utf8);
            match std::str::from_utf8(&bytes) {
                Ok(s) => s.chars().for_each(|c| self.put(c)),
                Err(_) => self.put(char::REPLACEMENT_CHARACTER),
            }
        }
    }

    /// An incomplete UTF-8 sequence interrupted by something else.
    fn flush_utf8(&mut self) {
        if !self.utf8.is_empty() {
            self.utf8.clear();
            self.put(char::REPLACEMENT_CHARACTER);
        }
    }

    fn esc(&mut self, b: u8) {
        self.state = match b {
            b'[' => State::Csi(Vec::new()),
            b']' => State::Osc(Vec::new()),
            b'P' | b'X' | b'^' | b'_' => State::Str,
            b'(' | b')' | b'*' | b'+' | b'-' | b'.' | b'/' | b'#' | b'%' | b' ' => State::OneMore,
            _ => State::Ground, // ESC 7/8/=/>/M/c … — no effect on the text
        };
    }

    fn csi(&mut self, params: &[u8], fin: u8) {
        let private = params.first() == Some(&b'?');
        let body = if private { &params[1..] } else { params };
        // Other private markers / intermediates (`ESC[>4;1m`, `ESC[2 q`) never move text.
        if body.iter().any(|c| !(c.is_ascii_digit() || *c == b';')) {
            return;
        }
        let nums: Vec<usize> = std::str::from_utf8(body)
            .unwrap_or_default()
            .split(';')
            .map(|p| p.parse().unwrap_or(0))
            .collect();
        let first = nums.first().copied().unwrap_or(0);
        let n = first.max(1);

        if private {
            if matches!(fin, b'h' | b'l') && nums.iter().any(|m| matches!(m, 47 | 1047 | 1049)) {
                self.alt_screen(fin == b'h');
            }
            return;
        }
        if self.alt.is_some() {
            return;
        }
        match fin {
            b'C' | b'a' => self.col += n,
            b'D' => self.col = self.col.saturating_sub(n),
            b'G' | b'`' => self.col = n - 1,
            // Cursor position: rows don't exist here, keep the column.
            b'H' | b'f' => self.col = nums.get(1).copied().unwrap_or(1).max(1) - 1,
            b'K' => match first {
                0 => {
                    if self.col < self.line.len() {
                        self.line.truncate(self.col);
                    }
                }
                1 => {
                    let end = (self.col + 1).min(self.line.len());
                    self.line[..end].fill(' ');
                }
                _ => self.line.clear(),
            },
            // Clear screen (`clear`, Ctrl+L): the line on screen is gone; the shell
            // redraws its prompt afterwards.
            b'J' if first >= 2 => {
                self.line.clear();
                self.col = 0;
            }
            b'@' => {
                if self.col < self.line.len() {
                    for _ in 0..n {
                        self.line.insert(self.col, ' ');
                    }
                }
            }
            b'P' => {
                if self.col < self.line.len() {
                    let end = (self.col + n).min(self.line.len());
                    self.line.drain(self.col..end);
                }
            }
            b'X' => {
                let end = (self.col + n).min(self.line.len());
                if self.col < end {
                    self.line[self.col..end].fill(' ');
                }
            }
            _ => {} // SGR colors, modes, scroll regions, window ops …
        }
    }

    fn osc(&mut self, payload: &[u8]) {
        let Ok(text) = std::str::from_utf8(payload) else {
            return;
        };
        if let Some(rest) = text.strip_prefix("3008;") {
            self.osc3008(rest);
        }
        // OSC 0/2 (window title) and everything else: dropped.
    }

    /// systemd context markers: `start=<id>;…;type=shell|command;cwd=<dir>` and
    /// `end=<id>;exit=success|failure|crash|interrupt;status=N;signal=SIG`.
    fn osc3008(&mut self, rest: &str) {
        let field = |key: &str| {
            rest.split(';')
                .find_map(|kv| kv.strip_prefix(key).and_then(|v| v.strip_prefix('=')))
        };
        if let Some(id) = field("start") {
            match field("type") {
                Some("shell") => {
                    // A new prompt without a command (empty Enter, Ctrl+C): the held
                    // prompt lines go out unannotated.
                    self.flush_held();
                    self.awaiting_cmd = true;
                }
                Some("command") => {
                    self.awaiting_cmd = false;
                    let header = match (self.time.as_deref(), field("cwd")) {
                        (Some(t), Some(cwd)) => Some(format!("── {t}  {cwd} ──")),
                        (Some(t), None) => Some(format!("── {t} ──")),
                        (None, Some(cwd)) => Some(format!("── {cwd} ──")),
                        (None, None) => None,
                    };
                    if let Some(h) = header {
                        self.emit(&h);
                    }
                    self.flush_held();
                    self.running.push(id.to_owned());
                }
                _ => {}
            }
        } else if let Some(id) = field("end") {
            let Some(pos) = self.running.iter().position(|r| r == id) else {
                return; // end of a shell/session context, not a command
            };
            self.running.remove(pos);
            let status = match (field("exit"), field("status"), field("signal")) {
                (_, _, Some(sig)) => Some(format!("[signal {sig}]")),
                (Some("success") | None, _, _) => None,
                (_, Some(code), _) => Some(format!("[exit {code}]")),
                (Some(kind), None, _) => Some(format!("[exit {kind}]")),
            };
            if let Some(s) = status {
                // Output without a trailing newline: finish that line first.
                if self.line.iter().any(|c| *c != ' ') {
                    self.commit();
                }
                self.emit(&s);
            }
        }
    }

    fn alt_screen(&mut self, enter: bool) {
        if enter {
            if self.alt.is_none() {
                if self.line.iter().any(|c| *c != ' ') {
                    self.commit();
                }
                self.line.clear();
                self.col = 0;
                self.alt = Some(self.time.clone());
            }
        } else if let Some(start) = self.alt.take() {
            let end = self.time.clone();
            let marker = self.alt_marker(start.as_deref(), end.as_deref());
            self.emit(&marker);
            self.line.clear();
            self.col = 0;
            self.had_text = false;
        }
    }

    fn alt_marker(&self, start: Option<&str>, end: Option<&str>) -> String {
        match (start, end) {
            (Some(s), Some(e)) => format!("[full-screen program {s} – {e}]"),
            (Some(s), None) => format!("[full-screen program from {s}]"),
            _ => "[full-screen program]".to_owned(),
        }
    }

    fn put(&mut self, c: char) {
        if self.alt.is_some() {
            return;
        }
        if self.col >= self.line.len() {
            self.line.resize(self.col, ' ');
            self.line.push(c);
        } else {
            self.line[self.col] = c;
        }
        self.col += 1;
        if c != ' ' {
            self.had_text = true;
        }
    }

    /// The current line, trailing blanks trimmed; resets line state.
    fn take_line(&mut self) -> String {
        let s: String = self.line.iter().collect();
        self.line.clear();
        self.col = 0;
        s.trim_end().to_owned()
    }

    fn commit(&mut self) {
        if self.alt.is_some() {
            self.line.clear();
            self.col = 0;
            return;
        }
        let had_text = std::mem::take(&mut self.had_text);
        let s = self.take_line();
        if s.is_empty() && had_text {
            return; // written, then erased (pager prompt): not real output
        }
        if self.awaiting_cmd {
            self.held.push(s);
        } else {
            self.emit(&s);
        }
    }

    fn flush_held(&mut self) {
        for l in std::mem::take(&mut self.held) {
            self.emit(&l);
        }
    }

    fn emit(&mut self, s: &str) {
        self.out.push_str(s);
        self.out.push('\n');
    }
}

/// Clean a whole raw log in one go (offline conversion; no real times available).
pub fn clean_bytes(data: &[u8]) -> String {
    let mut c = LogCleaner::new();
    let mut s = c.feed(data, None);
    s.push_str(&c.finish(None));
    s
}

/// `gukab --clean-log <path>`: convert one raw log, or every raw `*.log` under a
/// directory, into a `<name>.clean.log` next to it. Originals are never modified.
/// Returns the files written.
pub fn convert_path(path: &Path) -> io::Result<Vec<PathBuf>> {
    let mut written = Vec::new();
    let meta = fs::symlink_metadata(path)?;
    if meta.is_dir() {
        convert_dir(path, &mut written)?;
    } else if meta.is_file()
        && let Some(out) = convert_file(path)?
    {
        written.push(out);
    }
    Ok(written)
}

fn convert_dir(dir: &Path, written: &mut Vec<PathBuf>) -> io::Result<()> {
    let mut entries: Vec<_> = fs::read_dir(dir)?.filter_map(Result::ok).collect();
    entries.sort_by_key(|e| e.path());
    for e in entries {
        let p = e.path();
        let Ok(ft) = e.file_type() else { continue };
        if ft.is_dir() {
            convert_dir(&p, written)?;
        } else if ft.is_file()
            && p.extension().is_some_and(|x| x == "log")
            && !p.to_string_lossy().ends_with(".clean.log")
        {
            match convert_file(&p) {
                Ok(Some(out)) => written.push(out),
                Ok(None) => {}
                Err(err) => eprintln!("gukab: skipped {}: {err}", p.display()),
            }
        }
    }
    Ok(())
}

/// Returns `None` when the file is already clean (written by the live cleaner).
fn convert_file(path: &Path) -> io::Result<Option<PathBuf>> {
    let data = fs::read(path)?;
    let first = data.split(|b| *b == b'\n').next().unwrap_or_default();
    if String::from_utf8_lossy(first).trim_end().ends_with(CLEAN_MARK) {
        return Ok(None);
    }
    let out = path.with_extension("clean.log");
    let mut opts = fs::OpenOptions::new();
    opts.create(true).write(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        opts.mode(0o600);
    }
    io::Write::write_all(&mut opts.open(&out)?, clean_bytes(&data).as_bytes())?;
    Ok(Some(out))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn clean(data: &[u8]) -> String {
        clean_bytes(data)
    }

    #[test]
    fn strips_colors_titles_and_paste_mode() {
        let raw = b"\x1b[?2004h\x1b]0;u@h: ~\x07\x1b[01;32mu@h\x1b[00m:\x1b[01;34m~\x1b[00m$ ls\r\n\
                    \x1b[?2004l\r\x1b[0m\x1b[01;34mdata\x1b[0m  notes.txt\r\n";
        assert_eq!(clean(raw), "u@h:~$ ls\ndata  notes.txt\n");
    }

    #[test]
    fn backspace_erase_fixes_typo() {
        let raw = b"$ scp a@10\x08\x1b[K\x08\x1b[Kmon:/x .\r\n";
        assert_eq!(clean(raw), "$ scp a@mon:/x .\n");
    }

    #[test]
    fn history_recall_and_insert_at_start() {
        // ↑ recalls `mv a b`, Home, then `sudo ` typed in front (readline inserts).
        let raw = b"$ mv a b\x08\x08\x08\x08\x08\x08\x1b[1@s\x1b[1@u\x1b[1@d\x1b[1@o\x1b[1@ \r\n";
        assert_eq!(clean(raw), "$ sudo mv a b\n");
    }

    #[test]
    fn history_replacement_and_delete_chars() {
        // Previous history entry shown, cursor to start of command, rest erased, new text.
        let raw = b"$ sudo docker compose up\r\x1b[C\x1b[C\x1b[Kls\r\n";
        assert_eq!(clean(raw), "$ ls\n");
        let raw = b"$ rm xx -r\x08\x08\x08\x08\x08\x1b[1P\r\n";
        assert_eq!(clean(raw), "$ rm x -r\n");
    }

    #[test]
    fn clear_screen_redraw() {
        let raw = b"$ s\x1b[H\x1b[2J\x1b]0;t\x07$ s\x08\x1b[Kls\r\n";
        assert_eq!(clean(raw), "$ ls\n");
    }

    #[test]
    fn crcrlf_and_tabs() {
        assert_eq!(clean(b"Last login: x\r\r\na\tb\r\n"), "Last login: x\na       b\n");
    }

    #[test]
    fn pager_more_prompt_dropped() {
        let raw = b"line1\r\n --More-- \x08\x08\x08\x08\x08\x08\x08\x08\x08        \
                    \x08\x08\x08\x08\x08\x08\x08\x08\x08\r\nline2\r\n";
        assert_eq!(clean(raw), "line1\nline2\n");
    }

    #[test]
    fn password_feedback_stars_erased() {
        let raw = b"[sudo] Password: *\x08 \x08**\r\n";
        assert_eq!(clean(raw), "[sudo] Password: **\n");
    }

    #[test]
    fn full_screen_program_becomes_marker() {
        let raw = b"$ nano f.yaml\r\n\x1b[?1049h\x1b[22;0;0t\x1b[1;33r\x1b[30;5Himage: x\r\n\
                    \x1b[?1049l\x1b[23;0;0t$ ";
        let mut c = LogCleaner::new();
        let mut s = c.feed(&raw[..raw.len() - 20], Some("14:32:05"));
        s.push_str(&c.feed(&raw[raw.len() - 20..], Some("14:35:40")));
        s.push_str(&c.finish(None));
        assert_eq!(s, "$ nano f.yaml\n[full-screen program 14:32:05 – 14:35:40]\n$\n");
    }

    #[test]
    fn osc3008_headers_and_exit_status() {
        let shell = b"\x1b]3008;start=S1;machineid=m;user=u;type=shell;cwd=/opt\x1b\\";
        let mut raw = Vec::new();
        raw.extend_from_slice(shell);
        raw.extend_from_slice(b"\x1b[?2004hu@h:/opt$ rm x\r\n\x1b[?2004l\r");
        raw.extend_from_slice(b"\x1b]3008;start=C1;user=u;type=command;cwd=/opt\x1b\\");
        raw.extend_from_slice(b"rm: cannot remove 'x': Permission denied\r\n");
        raw.extend_from_slice(b"\x1b]3008;end=C1;exit=failure;status=1\x1b\\");
        raw.extend_from_slice(shell);
        raw.extend_from_slice(b"u@h:/opt$ \r\n"); // empty Enter: no command context
        raw.extend_from_slice(shell);
        raw.extend_from_slice(b"u@h:/opt$ true\r\n\r");
        raw.extend_from_slice(b"\x1b]3008;start=C2;type=command;cwd=/opt\x1b\\");
        raw.extend_from_slice(b"\x1b]3008;end=C2;exit=success\x1b\\");
        raw.extend_from_slice(shell);
        raw.extend_from_slice(b"u@h:/opt$ ");

        let mut c = LogCleaner::new();
        let mut s = c.feed(&raw, Some("14:51:10"));
        s.push_str(&c.finish(None));
        assert_eq!(
            s,
            "── 14:51:10  /opt ──\nu@h:/opt$ rm x\nrm: cannot remove 'x': Permission denied\n\
             [exit 1]\nu@h:/opt$\n── 14:51:10  /opt ──\nu@h:/opt$ true\nu@h:/opt$\n"
        );
    }

    #[test]
    fn osc3008_signal_and_unterminated_output() {
        let raw = b"\x1b]3008;start=C;type=command;cwd=/\x07partial\
                    \x1b]3008;end=C;exit=interrupt;signal=SIGINT\x07";
        assert_eq!(clean(raw), "── / ──\npartial\n[signal SIGINT]\n");
    }

    #[test]
    fn utf8_split_across_chunks() {
        let mut c = LogCleaner::new();
        let mut s = c.feed(b"de\xc4", None);
        s.push_str(&c.feed(b"\x9fi\xc5\x9f\r\n", None));
        assert_eq!(s, "deği\u{15f}\n");
        assert_eq!(clean(b"a\xffb\r\n"), "a\u{fffd}b\n");
    }

    #[test]
    fn dcs_and_charset_sequences_ignored() {
        assert_eq!(clean(b"\x1bP1$r0m\x1b\\\x1b(Bok\x1b[>4;1m\x1b[2 q\r\n"), "ok\n");
    }

    #[test]
    fn finish_flushes_last_prompt() {
        let mut c = LogCleaner::new();
        assert_eq!(c.feed(b"logout", None), "");
        assert_eq!(c.finish(None), "logout\n");
    }

    #[test]
    fn converter_writes_clean_copy_and_skips_clean_logs() {
        let dir = std::env::temp_dir().join(format!("gukab-logclean-{}", std::process::id()));
        let sub = dir.join("host");
        fs::create_dir_all(&sub).unwrap();
        fs::write(sub.join("a.log"), b"==== gukab session x ====\n\x1b[31mred\x1b[0m\r\n").unwrap();
        fs::write(sub.join("b.log"), format!("==== gukab session y ==== {CLEAN_MARK}\nok\n")).unwrap();
        let written = convert_path(&dir).unwrap();
        assert_eq!(written, vec![sub.join("a.clean.log")]);
        assert_eq!(
            fs::read_to_string(sub.join("a.clean.log")).unwrap(),
            "==== gukab session x ====\nred\n"
        );
        // Re-running never treats the .clean.log outputs as inputs.
        assert_eq!(convert_path(&dir).unwrap(), vec![sub.join("a.clean.log")]);
        fs::remove_dir_all(&dir).unwrap();
    }
}
