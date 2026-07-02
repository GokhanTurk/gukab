//! Mid-session baud-rate chooser for serial consoles.
//!
//! Opened from the `Ctrl+A` picker's "baud rate…" entry. A single editable value
//! (pre-filled with the current baud) plus a preset list: `↑↓` picks a preset
//! straight into the field, or type any custom rate. `Enter` applies, `Esc` cancels.
//! Like the macro picker it briefly uses the alternate screen and reads from the
//! same stdin mpsc channel.

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

use crate::serial::BAUD_PRESETS;

const ENTER_ALT: &[u8] = b"\x1b[?1049h";
const LEAVE_ALT: &[u8] = b"\x1b[?1049l";

/// Show the chooser and return the selected baud, or `None` if cancelled.
pub async fn choose(rx: &mut Receiver<Vec<u8>>, current: u32) -> Option<u32> {
    {
        let mut out = std::io::stdout();
        let _ = out.write_all(ENTER_ALT);
        let _ = out.flush();
    }
    let result = run(rx, current).await;
    {
        let mut out = std::io::stdout();
        let _ = out.write_all(LEAVE_ALT);
        let _ = out.flush();
    }
    result
}

async fn run(rx: &mut Receiver<Vec<u8>>, current: u32) -> Option<u32> {
    let backend = CrosstermBackend::new(std::io::stdout());
    let mut terminal = Terminal::new(backend).ok()?;

    let mut field = current.to_string();
    // Highlighted preset (also mirrored into `field`); `None` once the user types.
    let mut sel: Option<usize> = BAUD_PRESETS.iter().position(|&b| b == current);

    loop {
        let _ = terminal.draw(|f| draw(f, &field, sel));

        let chunk = rx.recv().await?;

        match chunk.as_slice() {
            [0x1b] => return None, // Esc
            [0x1b, b'[', b'A'] => {
                // Up: move to the previous preset (or the first).
                let i = sel.map(|i| i.saturating_sub(1)).unwrap_or(0);
                sel = Some(i);
                field = BAUD_PRESETS[i].to_string();
                continue;
            }
            [0x1b, b'[', b'B'] => {
                // Down: move to the next preset.
                let i = sel.map(|i| (i + 1).min(BAUD_PRESETS.len() - 1)).unwrap_or(0);
                sel = Some(i);
                field = BAUD_PRESETS[i].to_string();
                continue;
            }
            _ => {}
        }

        for &b in &chunk {
            match b {
                0x04 => return None, // Ctrl+D
                b'\r' | b'\n' => {
                    if let Ok(n) = field.trim().parse::<u32>()
                        && n > 0
                    {
                        return Some(n);
                    }
                    // Invalid/empty custom value: ignore and keep the chooser open.
                }
                0x7f | 0x08 => {
                    field.pop();
                    sel = None;
                }
                b'0'..=b'9' => {
                    field.push(b as char);
                    sel = None;
                }
                _ => {}
            }
        }
    }
}

fn draw(f: &mut ratatui::Frame, field: &str, sel: Option<usize>) {
    let area = f.area();
    let width = area.width.min(40);
    let height = area.height.min((BAUD_PRESETS.len() as u16) + 6);
    let x = area.x + (area.width.saturating_sub(width)) / 2;
    let y = area.y + (area.height.saturating_sub(height)) / 2;
    let popup = Rect::new(x, y, width, height);

    f.render_widget(Clear, popup);
    let block = Block::default()
        .borders(Borders::ALL)
        .title(" Baud rate ")
        .title_alignment(Alignment::Center);
    f.render_widget(block, popup);

    let inner = popup.inner(Margin { horizontal: 1, vertical: 1 });
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),
            Constraint::Min(0),
            Constraint::Length(1),
        ])
        .split(inner);

    // Editable value line.
    let value = Line::from(vec![
        Span::styled("baud: ", Style::default().fg(Color::DarkGray)),
        Span::styled(
            field.to_string(),
            Style::default().fg(Color::White).add_modifier(Modifier::BOLD),
        ),
    ]);
    f.render_widget(Paragraph::new(value), rows[0]);
    let cx = rows[0].x + 6 + field.chars().count() as u16;
    f.set_cursor_position((cx.min(rows[0].x + rows[0].width.saturating_sub(1)), rows[0].y));

    // Preset list.
    let items: Vec<ListItem> = BAUD_PRESETS
        .iter()
        .map(|b| ListItem::new(Line::from(Span::raw(b.to_string()))))
        .collect();
    let mut state = ListState::default();
    state.select(sel);
    let list = List::new(items)
        .highlight_style(Style::default().bg(Color::Blue).add_modifier(Modifier::BOLD))
        .highlight_symbol("▶ ");
    f.render_stateful_widget(list, rows[1], &mut state);

    let hint = Paragraph::new("↑↓ preset · type custom · Enter apply · Esc")
        .alignment(Alignment::Center)
        .style(Style::default().fg(Color::DarkGray));
    f.render_widget(hint, rows[2]);
}
