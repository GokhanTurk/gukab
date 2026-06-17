//! Line-based colorization of the SSH session output.
//!
//! Re-coloring an interactive PTY mid-stream is fragile, so the rule is: only
//! *complete* lines that arrive before any of their bytes have been shown get
//! colored; the trailing partial line (prompts, what you're typing, `--More--`)
//! is emitted raw immediately and never recolored. Colors are named ANSI codes,
//! so they render in the user's terminal theme (e.g. Catppuccin Mocha).

use regex::Regex;

const RESET: &str = "\x1b[0m";
const RED: &str = "\x1b[31m";
const GREEN: &str = "\x1b[32m";
const CYAN: &str = "\x1b[36m";

/// Flush the line buffer raw if it grows past this without a newline (e.g. a
/// device streaming a huge line) so we never accumulate unbounded memory.
const MAX_PENDING: usize = 64 * 1024;

pub struct Highlighter {
    /// Bytes not yet completed by a newline.
    pending: Vec<u8>,
    /// How many leading bytes of `pending` have already been written to stdout.
    emitted: usize,
    ipv4: Regex,
    iface: Regex,
}

impl Highlighter {
    pub fn new() -> Self {
        // Unwrap is fine: these are constant, tested patterns compiled at startup.
        let ipv4 = Regex::new(r"\b\d{1,3}(?:\.\d{1,3}){3}\b").expect("valid ipv4 regex");
        let iface = Regex::new(r"(?i)\b(?:Gi|Fa|Te|Tw|Fo|Hu|Eth|Gig\w*|Ten\w*|Po|Vl|Vlan|Lo|Se|Tu)\d[\d/.:]*\b")
            .expect("valid iface regex");
        Self {
            pending: Vec::new(),
            emitted: 0,
            ipv4,
            iface,
        }
    }

    /// Feed a chunk of remote output; returns the bytes to write to stdout
    /// (complete lines colorized, partial tail raw).
    pub fn process(&mut self, data: &[u8]) -> Vec<u8> {
        self.pending.extend_from_slice(data);
        let mut out = Vec::with_capacity(data.len() + 16);

        while let Some(nl) = self.pending[..].iter().position(|&b| b == b'\n') {
            let end = nl + 1; // include the '\n'
            if self.emitted == 0 {
                // Nothing of this line shown yet → safe to colorize the whole line.
                out.extend_from_slice(&colorize_line(&self.pending[..end], &self.ipv4, &self.iface));
            } else {
                // Start already shown raw → emit only the unshown remainder, raw.
                let from = self.emitted.min(end);
                out.extend_from_slice(&self.pending[from..end]);
            }
            self.pending.drain(..end);
            self.emitted = self.emitted.saturating_sub(end);
        }

        // Guard: a runaway line with no newline — flush raw and reset.
        if self.pending.len() > MAX_PENDING {
            out.extend_from_slice(&self.pending[self.emitted..]);
            self.pending.clear();
            self.emitted = 0;
            return out;
        }

        // Emit any new tail bytes raw so prompts/typing appear immediately.
        if self.pending.len() > self.emitted {
            out.extend_from_slice(&self.pending[self.emitted..]);
            self.emitted = self.pending.len();
        }
        out
    }
}

/// Words that color a whole line red / green when present as standalone tokens.
const RED_WORDS: &[&str] = &[
    "down", "err", "err-disabled", "fail", "failed", "error", "denied", "invalid", "notconnect",
];
const GREEN_WORDS: &[&str] = &["up", "connected", "forwarding", "active", "permit"];

/// Colorize one complete line (`line` ends with '\n', possibly preceded by '\r').
/// Lines carrying their own escape/control bytes are returned untouched.
fn colorize_line(line: &[u8], ipv4: &Regex, iface: &Regex) -> Vec<u8> {
    // Never touch lines that contain control bytes (besides \r \n \t) — they may
    // carry the device's own escape sequences.
    if line
        .iter()
        .any(|&b| b < 0x20 && !matches!(b, b'\r' | b'\n' | b'\t'))
    {
        return line.to_vec();
    }

    // Split trailing line terminator to re-attach verbatim.
    let mut content_len = line.len();
    while content_len > 0 && matches!(line[content_len - 1], b'\n' | b'\r') {
        content_len -= 1;
    }
    let content = &line[..content_len];
    let terminator = &line[content_len..];

    let text = match std::str::from_utf8(content) {
        Ok(t) => t,
        Err(_) => return line.to_vec(), // non-UTF-8: leave alone
    };
    let lower = text.to_ascii_lowercase();

    let mut out = Vec::with_capacity(line.len() + 12);
    if text.starts_with('%') || RED_WORDS.iter().any(|w| has_word(&lower, w)) {
        out.extend_from_slice(RED.as_bytes());
        out.extend_from_slice(content);
        out.extend_from_slice(RESET.as_bytes());
    } else if GREEN_WORDS.iter().any(|w| has_word(&lower, w)) {
        out.extend_from_slice(GREEN.as_bytes());
        out.extend_from_slice(content);
        out.extend_from_slice(RESET.as_bytes());
    } else {
        out.extend_from_slice(highlight_tokens(text, ipv4, iface).as_bytes());
    }
    out.extend_from_slice(terminator);
    out
}

/// Wrap IPv4 addresses and interface names in cyan, leaving the rest default.
fn highlight_tokens(text: &str, ipv4: &Regex, iface: &Regex) -> String {
    // Collect non-overlapping match ranges from both patterns.
    let mut ranges: Vec<(usize, usize)> = ipv4
        .find_iter(text)
        .chain(iface.find_iter(text))
        .map(|m| (m.start(), m.end()))
        .collect();
    if ranges.is_empty() {
        return text.to_string();
    }
    ranges.sort_by_key(|r| r.0);

    let mut out = String::with_capacity(text.len() + ranges.len() * 9);
    let mut pos = 0;
    for (start, end) in ranges {
        if start < pos {
            continue; // overlapping (e.g. iface inside another match) — skip
        }
        out.push_str(&text[pos..start]);
        out.push_str(CYAN);
        out.push_str(&text[start..end]);
        out.push_str(RESET);
        pos = end;
    }
    out.push_str(&text[pos..]);
    out
}

/// True if `word` appears in `haystack` bounded by non-alphanumeric chars, so
/// "up" matches "is up," but not "backup".
fn has_word(haystack: &str, word: &str) -> bool {
    let bytes = haystack.as_bytes();
    let w = word.as_bytes();
    let mut i = 0;
    while let Some(off) = haystack[i..].find(word) {
        let start = i + off;
        let end = start + w.len();
        let before_ok = start == 0 || !is_word_byte(bytes[start - 1]);
        let after_ok = end == bytes.len() || !is_word_byte(bytes[end]);
        if before_ok && after_ok {
            return true;
        }
        i = start + 1;
    }
    false
}

fn is_word_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

impl Default for Highlighter {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(bytes: Vec<u8>) -> String {
        String::from_utf8(bytes).unwrap()
    }

    #[test]
    fn up_line_is_green() {
        let mut hl = Highlighter::new();
        let out = s(hl.process(b"GigabitEthernet0/1 is up\n"));
        assert!(out.starts_with(GREEN) && out.contains(RESET) && out.ends_with('\n'));
    }

    #[test]
    fn down_line_is_red() {
        let mut hl = Highlighter::new();
        let out = s(hl.process(b"Vlan10 is administratively down\n"));
        assert!(out.starts_with(RED));
    }

    #[test]
    fn percent_error_is_red() {
        let mut hl = Highlighter::new();
        let out = s(hl.process(b"% Invalid input detected\n"));
        assert!(out.starts_with(RED));
    }

    #[test]
    fn ip_token_is_cyan() {
        let mut hl = Highlighter::new();
        let out = s(hl.process(b"  Internet address is 10.0.1.1\n"));
        assert!(out.contains(&format!("{CYAN}10.0.1.1{RESET}")));
    }

    #[test]
    fn word_boundary_avoids_false_positive() {
        // "backup" must not trigger the "up" rule.
        let mut hl = Highlighter::new();
        let out = s(hl.process(b"running backup config\n"));
        assert!(!out.starts_with(GREEN));
    }

    #[test]
    fn partial_then_complete_stays_raw() {
        let mut hl = Highlighter::new();
        // Prompt + typed command arrive as a partial line (no newline) → raw.
        assert_eq!(s(hl.process(b"Switch# ")), "Switch# ");
        // Completing it must not recolor the already-shown start.
        assert_eq!(s(hl.process(b"show ver\n")), "show ver\n");
    }
}
