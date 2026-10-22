//! Mid-session fuzzy macro picker.
//!
//! Opened from the `Ctrl+A` escape prefix during an SSH session. It briefly
//! switches to the alternate screen, draws a small centered ratatui popup
//! (search box + fuzzy-filtered macro list), then restores the live session
//! screen. Input comes from the same stdin mpsc channel the io_loop uses.

use std::io::Write as _;

use ratatui::{
    backend::CrosstermBackend,
    layout::{Alignment, Constraint, Direction, Layout, Margin, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{
        Block, Borders, Clear, List, ListItem, ListState, Paragraph, Scrollbar,
        ScrollbarOrientation, ScrollbarState,
    },
    Terminal,
};
use tokio::sync::mpsc::Receiver;

use crate::config::Macro;
use crate::fuzzy::fuzzy_score;
use crate::session::PasteCredential;

/// Outcome of the picker.
#[derive(Debug, PartialEq)]
pub enum Pick {
    /// Run the macro with this key.
    Run(String),
    /// Paste the keyring secret with this reference (no Enter is sent).
    Paste(String),
    /// Open the baud chooser (serial sessions only — the pinned top entry).
    Baud,
    /// Close the picker, keep the session.
    Cancel,
    /// End the SSH session.
    Disconnect,
}

const ENTER_ALT: &[u8] = b"\x1b[?1049h";
const LEAVE_ALT: &[u8] = b"\x1b[?1049l";

/// One selectable line of the picker.
#[derive(Debug, Clone, Copy)]
enum Row<'a> {
    Baud(u32),
    Macro(&'a Macro),
    Credential(&'a PasteCredential),
}

impl Row<'_> {
    fn pick(&self) -> Pick {
        match self {
            Row::Baud(_) => Pick::Baud,
            Row::Macro(m) => Pick::Run(m.key.clone()),
            Row::Credential(c) => Pick::Paste(c.reference.clone()),
        }
    }
}

/// Show the picker and return the user's choice. `seed` pre-fills the query with
/// any bytes typed in the same chunk as the escape prefix. `baud_now` — when set
/// (serial sessions) — pins a "cycle baud" entry at the top of the empty picker,
/// showing the current baud. `credentials` are pasteable keyring entries.
pub async fn pick(
    macros: &[Macro],
    credentials: &[PasteCredential],
    rx: &mut Receiver<Vec<u8>>,
    seed: &[u8],
    baud_now: Option<u32>,
) -> Pick {
    // Switch to the alternate screen so we can draw over the session and restore
    // it untouched afterwards.
    {
        let mut out = std::io::stdout();
        let _ = out.write_all(ENTER_ALT);
        let _ = out.flush();
    }

    let result = run(macros, credentials, rx, seed, baud_now).await;

    {
        let mut out = std::io::stdout();
        let _ = out.write_all(LEAVE_ALT);
        let _ = out.flush();
    }
    result
}

async fn run(
    macros: &[Macro],
    credentials: &[PasteCredential],
    rx: &mut Receiver<Vec<u8>>,
    seed: &[u8],
    baud_now: Option<u32>,
) -> Pick {
    let backend = CrosstermBackend::new(std::io::stdout());
    let mut terminal = match Terminal::new(backend) {
        Ok(t) => t,
        Err(_) => return Pick::Cancel,
    };

    let mut query = String::from_utf8_lossy(seed)
        .chars()
        .filter(|c| !c.is_control())
        .collect::<String>();
    let mut selected = 0usize;

    loop {
        let list = rows(macros, credentials, &query, baud_now);
        if selected >= list.len() {
            selected = list.len().saturating_sub(1);
        }
        let _ = terminal.draw(|f| draw(f, &query, &list, selected, credentials.is_empty()));

        let Some(chunk) = rx.recv().await else {
            return Pick::Cancel;
        };

        // Whole-chunk control sequences first (arrows / lone Esc).
        match chunk.as_slice() {
            [0x1b] => return Pick::Cancel,
            [0x1b, b'[', b'A'] => {
                selected = selected.saturating_sub(1);
                continue;
            }
            [0x1b, b'[', b'B'] => {
                if selected + 1 < list.len() {
                    selected += 1;
                }
                continue;
            }
            _ => {}
        }

        for &b in &chunk {
            match b {
                0x04 => return Pick::Disconnect, // Ctrl+D
                b'\r' | b'\n' => {
                    let list = rows(macros, credentials, &query, baud_now);
                    return list.get(selected).map_or(Pick::Cancel, Row::pick);
                }
                0x7f | 0x08 => {
                    query.pop();
                    selected = 0;
                }
                0x20..=0x7e => {
                    query.push(b as char);
                    selected = 0;
                }
                _ => {}
            }
        }
    }
}

/// The picker rows for `query`.
///
/// Empty query: the pinned entries first — the serial "baud rate" row, then the
/// connected host's own login password — followed by every macro and the other
/// credentials. With a query: macros and credentials ranked together, best-first
/// (same relevance scoring as the host list); a macro's `key` outranks a match in
/// its `send` body.
fn rows<'a>(
    macros: &'a [Macro],
    credentials: &'a [PasteCredential],
    query: &str,
    baud_now: Option<u32>,
) -> Vec<Row<'a>> {
    if query.is_empty() {
        let mut out: Vec<Row> = baud_now.map(Row::Baud).into_iter().collect();
        out.extend(credentials.iter().filter(|c| c.pinned).map(Row::Credential));
        out.extend(macros.iter().map(Row::Macro));
        out.extend(credentials.iter().filter(|c| !c.pinned).map(Row::Credential));
        return out;
    }
    let q = query.to_lowercase();
    let macro_rows = macros.iter().filter_map(|m| {
        let key_s = fuzzy_score(&q, &m.key.to_lowercase());
        // Body matches are slightly discounted so a key hit always wins a tie.
        let send_s = fuzzy_score(&q, &m.send.to_lowercase()).map(|s| s - 1);
        key_s.into_iter().chain(send_s).max().map(|s| (s, Row::Macro(m)))
    });
    let credential_rows = credentials.iter().filter_map(|c| {
        fuzzy_score(&q, &c.reference.to_lowercase()).map(|s| (s, Row::Credential(c)))
    });
    let mut scored: Vec<(i32, Row)> = macro_rows.chain(credential_rows).collect();
    scored.sort_by_key(|(s, _)| std::cmp::Reverse(*s));
    scored.into_iter().map(|(_, r)| r).collect()
}

fn draw(f: &mut ratatui::Frame, query: &str, list: &[Row], selected: usize, no_credentials: bool) {
    let area = f.area();
    let width = area.width.min(54);
    let height = area.height.min(16);
    let x = area.x + (area.width.saturating_sub(width)) / 2;
    let y = area.y + (area.height.saturating_sub(height)) / 2;
    let popup = Rect::new(x, y, width, height);

    f.render_widget(Clear, popup);
    let block = Block::default()
        .borders(Borders::ALL)
        .title(" Macros ")
        .title_alignment(Alignment::Center);
    f.render_widget(block, popup);

    let inner = popup.inner(Margin {
        horizontal: 1,
        vertical: 1,
    });
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),
            Constraint::Min(0),
            Constraint::Length(1),
        ])
        .split(inner);

    // Search line.
    let search = Line::from(vec![
        Span::styled("/ ", Style::default().fg(Color::DarkGray)),
        Span::raw(query),
    ]);
    f.render_widget(Paragraph::new(search), rows[0]);
    // Caret after the query text.
    let cx = rows[0].x + 2 + query.chars().count() as u16;
    f.set_cursor_position((cx.min(rows[0].x + rows[0].width.saturating_sub(1)), rows[0].y));

    // Pinned rows (baud, this host's password) come first, then macros with a
    // one-line preview, then credentials — only their name is ever shown.
    let items: Vec<ListItem> = list
        .iter()
        .map(|row| match row {
            Row::Baud(n) => ListItem::new(Line::from(Span::styled(
                format!("⇅  baud rate…  (now {n})"),
                Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD),
            ))),
            Row::Macro(m) => {
                let preview = m.send.lines().next().unwrap_or("");
                ListItem::new(Line::from(vec![
                    Span::styled(
                        format!("{:<12}", m.key),
                        Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD),
                    ),
                    Span::styled(preview.to_string(), Style::default().fg(Color::DarkGray)),
                ]))
            }
            Row::Credential(c) => {
                let note = if c.pinned { "this host's password" } else { "paste password" };
                ListItem::new(Line::from(vec![
                    Span::styled(
                        format!("🔑 {:<10}", c.reference),
                        Style::default().fg(Color::Magenta).add_modifier(Modifier::BOLD),
                    ),
                    Span::styled(format!(" {note}"), Style::default().fg(Color::DarkGray)),
                ]))
            }
        })
        .collect();
    let mut state = ListState::default();
    if !items.is_empty() {
        state.select(Some(selected));
    }
    let total = items.len();
    let list = List::new(items)
        .highlight_style(Style::default().bg(Color::Blue).add_modifier(Modifier::BOLD))
        .highlight_symbol("▶ ");
    f.render_stateful_widget(list, rows[1], &mut state);

    // More rows than fit: a scrollbar on the right border and "↑ n more" / "↓ n more"
    // counters on the top / bottom border, so hidden entries (e.g. credentials listed
    // after many macros) are not missed.
    let (above, below) = overflow(total, state.offset(), rows[1].height as usize);
    if above > 0 || below > 0 {
        let mut bar = ScrollbarState::new(total).position(selected);
        let track = Rect::new(popup.x, rows[1].y, popup.width, rows[1].height);
        f.render_stateful_widget(
            Scrollbar::new(ScrollbarOrientation::VerticalRight)
                .begin_symbol(None)
                .end_symbol(None),
            track,
            &mut bar,
        );
        let counter = Style::default().fg(Color::Yellow);
        let edge = |y: u16| Rect::new(popup.x + 1, y, popup.width.saturating_sub(3), 1);
        if above > 0 {
            f.render_widget(
                Paragraph::new(format!(" ↑ {above} more ")).style(counter).alignment(Alignment::Right),
                edge(popup.y),
            );
        }
        if below > 0 {
            f.render_widget(
                Paragraph::new(format!(" ↓ {below} more ")).style(counter).alignment(Alignment::Right),
                edge(popup.y + popup.height.saturating_sub(1)),
            );
        }
    }

    // With nothing to paste, say where passwords come from instead of the key help
    // (Ctrl+K works in the host list, not inside a session).
    let hint_text = if no_credentials {
        "no passwords yet · Ctrl+K in the host list adds one"
    } else {
        "Enter run/paste · ↑↓ · Esc cancel · ^D disconnect"
    };
    let hint = Paragraph::new(hint_text)
        .alignment(Alignment::Center)
        .style(Style::default().fg(Color::DarkGray));
    f.render_widget(hint, rows[2]);
}

/// Rows hidden above and below a list of `total` rows scrolled to `offset` with
/// `visible` rows on screen.
fn overflow(total: usize, offset: usize, visible: usize) -> (usize, usize) {
    let above = offset.min(total);
    let below = total.saturating_sub(offset + visible);
    (above, below)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn overflow_counts_hidden_rows() {
        assert_eq!(overflow(5, 0, 12), (0, 0)); // everything fits
        assert_eq!(overflow(20, 0, 12), (0, 8)); // top of a long list
        assert_eq!(overflow(20, 8, 12), (8, 0)); // scrolled to the end
        assert_eq!(overflow(20, 3, 12), (3, 5));
    }

    fn mac(key: &str) -> Macro {
        Macro { key: key.into(), send: format!("{key} body"), expects: Vec::new() }
    }

    fn cred(reference: &str, pinned: bool) -> PasteCredential {
        PasteCredential { reference: reference.into(), pinned }
    }

    fn picks(rows: &[Row]) -> Vec<Pick> {
        rows.iter().map(Row::pick).collect()
    }

    #[test]
    fn empty_query_lists_pinned_rows_then_macros_then_credentials() {
        let macros = [mac("en"), mac("wr")];
        let creds = [cred("srv-pass", true), cred("enable1", false)];
        assert_eq!(
            picks(&rows(&macros, &creds, "", Some(9600))),
            [
                Pick::Baud,
                Pick::Paste("srv-pass".into()),
                Pick::Run("en".into()),
                Pick::Run("wr".into()),
                Pick::Paste("enable1".into()),
            ]
        );
    }

    #[test]
    fn query_searches_credentials_by_name() {
        let macros = [mac("en"), mac("wr")];
        let creds = [cred("srv-pass", true), cred("ubuntu24-sudo", false)];
        let found = picks(&rows(&macros, &creds, "sudo", None));
        assert_eq!(found.first(), Some(&Pick::Paste("ubuntu24-sudo".into())));
        assert!(!found.contains(&Pick::Paste("srv-pass".into())));
    }

    fn render(list: &[Row], selected: usize) -> String {
        let backend = ratatui::backend::TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).expect("terminal");
        terminal.draw(|f| draw(f, "", list, selected, false)).expect("draw");
        let buf = terminal.backend().buffer();
        (0..buf.area.height)
            .map(|y| (0..buf.area.width).map(|x| buf[(x, y)].symbol()).collect::<String>())
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn long_list_shows_how_many_rows_are_hidden() {
        let macros: Vec<Macro> = (0..14).map(|i| mac(&format!("m{i}"))).collect();
        let creds = [cred("schrod", false)];
        let list = rows(&macros, &creds, "", None);

        let top = render(&list, 0);
        assert!(top.contains("↓ 3 more"), "{top}");
        assert_eq!(top.matches(" more ").count(), 1, "only the ↓ counter: {top}");
        assert!(top.contains("^D disconnect"), "hint must fit: {top}");

        // Selecting the last row (the credential) scrolls; the counter flips.
        let bottom = render(&list, list.len() - 1);
        assert!(bottom.contains("↑ 3 more") && bottom.contains("schrod"), "{bottom}");
        assert_eq!(bottom.matches(" more ").count(), 1, "only the ↑ counter: {bottom}");
    }

    #[test]
    fn short_list_has_no_scroll_counters() {
        let macros = [mac("en")];
        let out = render(&rows(&macros, &[], "", None), 0);
        assert!(!out.contains("more"), "{out}");
    }
}
