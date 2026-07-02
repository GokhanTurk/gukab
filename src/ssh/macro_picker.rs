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
    widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph},
    Terminal,
};
use tokio::sync::mpsc::Receiver;

use crate::config::Macro;
use crate::fuzzy::fuzzy_score;

/// Outcome of the picker.
pub enum Pick {
    /// Run the macro with this key.
    Run(String),
    /// Open the baud chooser (serial sessions only — the pinned top entry).
    Baud,
    /// Close the picker, keep the session.
    Cancel,
    /// End the SSH session.
    Disconnect,
}

const ENTER_ALT: &[u8] = b"\x1b[?1049h";
const LEAVE_ALT: &[u8] = b"\x1b[?1049l";

/// Show the picker and return the user's choice. `seed` pre-fills the query with
/// any bytes typed in the same chunk as the escape prefix. `baud_now` — when set
/// (serial sessions) — pins a "cycle baud" entry at the top of the empty picker,
/// showing the current baud.
pub async fn pick(
    macros: &[Macro],
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

    let result = run(macros, rx, seed, baud_now).await;

    {
        let mut out = std::io::stdout();
        let _ = out.write_all(LEAVE_ALT);
        let _ = out.flush();
    }
    result
}

async fn run(
    macros: &[Macro],
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

    // The "cycle baud" row is pinned at index 0, but only on the empty picker
    // (once you type to search macros it disappears).
    let baud_row = |query: &str| baud_now.filter(|_| query.is_empty());
    // Total selectable rows = optional baud row + matching macros.
    let total = |query: &str| filter(macros, query).len() + baud_row(query).is_some() as usize;

    loop {
        let matches = filter(macros, &query);
        let baud = baud_row(&query);
        let count = matches.len() + baud.is_some() as usize;
        if selected >= count {
            selected = count.saturating_sub(1);
        }
        let _ = terminal.draw(|f| draw(f, &query, &matches, selected, baud));

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
                let n = total(&query);
                if n > 0 && selected + 1 < n {
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
                    let baud_off = baud_row(&query).is_some() as usize;
                    if baud_off == 1 && selected == 0 {
                        return Pick::Baud;
                    }
                    let matches = filter(macros, &query);
                    return match matches.get(selected - baud_off) {
                        Some(m) => Pick::Run(m.key.clone()),
                        None => Pick::Cancel,
                    };
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

/// Macros matching the query, ranked best-first (same relevance scoring as the
/// host list). A match on the `key` outranks a match in the `send` body.
fn filter<'a>(macros: &'a [Macro], query: &str) -> Vec<&'a Macro> {
    if query.is_empty() {
        return macros.iter().collect();
    }
    let q = query.to_lowercase();
    let mut scored: Vec<(i32, &Macro)> = macros
        .iter()
        .filter_map(|m| {
            let key_s = fuzzy_score(&q, &m.key.to_lowercase());
            // Body matches are slightly discounted so a key hit always wins a tie.
            let send_s = fuzzy_score(&q, &m.send.to_lowercase()).map(|s| s - 1);
            let best = key_s.into_iter().chain(send_s).max();
            best.map(|s| (s, m))
        })
        .collect();
    scored.sort_by_key(|(s, _)| std::cmp::Reverse(*s));
    scored.into_iter().map(|(_, m)| m).collect()
}

fn draw(
    f: &mut ratatui::Frame,
    query: &str,
    matches: &[&Macro],
    selected: usize,
    baud: Option<u32>,
) {
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

    // Optional pinned "cycle baud" row, then the macro list with a one-line preview.
    let mut items: Vec<ListItem> = Vec::new();
    if let Some(n) = baud {
        items.push(ListItem::new(Line::from(Span::styled(
            format!("⇅  baud rate…  (now {n})"),
            Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD),
        ))));
    }
    items.extend(matches.iter().map(|m| {
        let preview = m.send.lines().next().unwrap_or("");
        ListItem::new(Line::from(vec![
            Span::styled(
                format!("{:<12}", m.key),
                Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD),
            ),
            Span::styled(preview.to_string(), Style::default().fg(Color::DarkGray)),
        ]))
    }));
    let mut state = ListState::default();
    if !items.is_empty() {
        state.select(Some(selected));
    }
    let list = List::new(items)
        .highlight_style(Style::default().bg(Color::Blue).add_modifier(Modifier::BOLD))
        .highlight_symbol("▶ ");
    f.render_stateful_widget(list, rows[1], &mut state);

    let hint = Paragraph::new("Enter run · ↑↓ select · Esc cancel · ^D disconnect")
        .alignment(Alignment::Center)
        .style(Style::default().fg(Color::DarkGray));
    f.render_widget(hint, rows[2]);
}
