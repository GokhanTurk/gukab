use ratatui::{
    layout::{Alignment, Constraint, Direction, Layout, Margin, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph},
    Frame,
};

use chrono::Local;

use crate::tui::app::{App, AppMode, EDIT_FIELD_COUNT, EDIT_FIELD_LABELS, PASSWORD_FIELD_IDX};

/// Width of the right-hand ASCII art panel.
const ART_W: u16 = 36;

pub fn draw(f: &mut Frame, app: &App) {
    let area = f.area();

    // Show the art as a right-hand panel only when there's room for both it and a
    // usable host list — it must never overlap the list text.
    let main_area = if area.width >= 90 && area.height >= 14 {
        let cols = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Min(50), Constraint::Length(ART_W)])
            .split(area);
        draw_banner(f, app.anim_frame(), cols[1]);
        cols[0]
    } else {
        area
    };

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Min(0),
            Constraint::Length(1),
        ])
        .split(main_area);

    draw_search(f, app, chunks[0]);
    draw_host_list(f, app, chunks[1]);
    draw_status_bar(f, app, chunks[2]);

    match app.mode {
        AppMode::Editing {
            ref draft,
            focused_field,
            cursor,
            original_idx,
        } => draw_edit_form(f, draft, focused_field, cursor, original_idx.is_none(), area),
        AppMode::Credential {
            ref reference,
            ref password,
            focused,
            cursor,
        } => draw_credential_form(f, reference, password, focused, cursor, area),
        AppMode::ConfirmDelete { ref name, .. } => draw_confirm_delete(f, name, area),
        AppMode::Normal => {}
    }
}

fn draw_search(f: &mut Frame, app: &App, area: Rect) {
    let block = Block::default().borders(Borders::ALL).title(" Search ");
    let widget = Paragraph::new(app.filter.as_str()).block(block);
    f.render_widget(widget, area);

    // Show the cursor at the end of the query (only relevant in Normal mode; a
    // popup, if open, sets the cursor afterwards and wins).
    if matches!(app.mode, AppMode::Normal) {
        let col = app.filter_cursor as u16;
        let max_x = area.x + area.width.saturating_sub(2);
        f.set_cursor_position((((area.x + 1) + col).min(max_x), area.y + 1));
    }
}

/// Min/max width of the host-name column (kept fixed per render for alignment).
const NAME_MIN: usize = 12;
const NAME_CAP: usize = 26;

/// Fit `s` into exactly `w` columns: truncate long values with `…`, pad short
/// ones with spaces, so every `user@host:port` lines up regardless of name length.
fn fit_width(s: &str, w: usize) -> String {
    let n = s.chars().count();
    if n > w {
        let mut out: String = s.chars().take(w.saturating_sub(1)).collect();
        out.push('…');
        out
    } else {
        let mut out = s.to_string();
        out.push_str(&" ".repeat(w - n));
        out
    }
}

fn draw_host_list(f: &mut Frame, app: &App, area: Rect) {
    use crate::tui::app::Row;

    // Size the name column to the longest visible host name (clamped), so the
    // address column is aligned even when some names are much longer than others.
    let name_w = app
        .rows
        .iter()
        .filter_map(|r| match r {
            Row::Host { idx, .. } => Some(app.hosts[*idx].name.chars().count()),
            Row::Group { .. } => None,
        })
        .max()
        .unwrap_or(NAME_MIN)
        .clamp(NAME_MIN, NAME_CAP);

    let items: Vec<ListItem> = app
        .rows
        .iter()
        .enumerate()
        .map(|(i, row)| {
            // The selected row drops its per-span colors so the high-contrast
            // highlight foreground (set below) shows through — readable on the
            // highlight background in every terminal, not just some palettes.
            let selected = i == app.selected;
            match row {
                Row::Group {
                    name,
                    icon,
                    collapsed,
                } => {
                    let arrow = if *collapsed { "▸" } else { "▾" };
                    let head = if icon.is_empty() {
                        format!("{arrow} {name}")
                    } else {
                        format!("{arrow} {icon}  {name}")
                    };
                    let style = if selected {
                        Style::default().add_modifier(Modifier::BOLD)
                    } else {
                        Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)
                    };
                    ListItem::new(Line::from(Span::styled(head, style)))
                }
                Row::Host { idx, icon } => {
                    let h = &app.hosts[*idx];
                    // Indent hosts under their group; show the group icon when present.
                    let prefix = if icon.is_empty() {
                        "   ".to_string()
                    } else {
                        format!("   {icon} ")
                    };
                    let (name_style, port_style) = if selected {
                        (Style::default(), Style::default())
                    } else {
                        (
                            Style::default().fg(Color::Cyan),
                            Style::default().fg(Color::DarkGray),
                        )
                    };
                    ListItem::new(Line::from(vec![
                        Span::raw(prefix),
                        Span::styled(fit_width(&h.name, name_w), name_style),
                        Span::raw(format!("  {}@{}", h.username, h.hostname)),
                        Span::styled(format!(":{}", h.port), port_style),
                    ]))
                }
            }
        })
        .collect();

    let mut list_state = ListState::default();
    list_state.select(if app.rows.is_empty() {
        None
    } else {
        Some(app.selected)
    });

    // Explicit RGB highlight (not a named color) so contrast is identical across
    // terminals: bright white text on a vivid blue bar. The selected row's spans
    // carry no fg of their own (see above), so this foreground always wins.
    let list = List::new(items)
        .block(Block::default().borders(Borders::ALL).title(" Hosts "))
        .highlight_style(
            Style::default()
                .fg(Color::Rgb(255, 255, 255))
                .bg(Color::Rgb(45, 95, 210))
                .add_modifier(Modifier::BOLD),
        )
        .highlight_symbol("▶ ");

    f.render_stateful_widget(list, area, &mut list_state);
}

fn draw_status_bar(f: &mut Frame, app: &App, area: Rect) {
    let text = match &app.status {
        Some(msg) => msg.as_str(),
        None => " ↑↓ nav  ^↑↓ move  Enter connect  ^N add  ^E edit  ^D delete  ^K cred  q quit",
    };
    let widget = Paragraph::new(text)
        .style(Style::default().bg(Color::DarkGray).fg(Color::White));
    f.render_widget(widget, area);
}

fn draw_edit_form(
    f: &mut Frame,
    draft: &crate::tui::app::EditDraft,
    focused_field: usize,
    cursor: usize,
    is_new: bool,
    area: Rect,
) {
    let popup_width = area.width.min(60);
    let popup_height = (EDIT_FIELD_COUNT as u16) * 3 + 4;
    let x = area.x + (area.width.saturating_sub(popup_width)) / 2;
    let y = area.y + (area.height.saturating_sub(popup_height)) / 2;
    let popup_area = Rect::new(x, y, popup_width, popup_height);

    f.render_widget(Clear, popup_area);

    let title = if is_new { " Add Host " } else { " Edit Host " };
    let block = Block::default()
        .borders(Borders::ALL)
        .title(title)
        .title_alignment(Alignment::Center);
    f.render_widget(block, popup_area);

    let inner = popup_area.inner(Margin {
        horizontal: 1,
        vertical: 1,
    });

    let field_constraints: Vec<Constraint> =
        (0..EDIT_FIELD_COUNT).map(|_| Constraint::Length(3)).collect();
    let footer = Constraint::Min(1);
    let mut all_constraints = field_constraints;
    all_constraints.push(footer);

    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints(all_constraints)
        .split(inner);

    for i in 0..EDIT_FIELD_COUNT {
        let is_focused = i == focused_field;
        let border_style = if is_focused {
            Style::default().fg(Color::Yellow)
        } else {
            Style::default()
        };
        let field_block = Block::default()
            .borders(Borders::ALL)
            .title(EDIT_FIELD_LABELS[i])
            .border_style(border_style);
        let display: String;
        let text = if i == PASSWORD_FIELD_IDX {
            display = "●".repeat(draft.field(i).chars().count());
            display.as_str()
        } else {
            draft.field(i)
        };
        let field_widget = Paragraph::new(text).block(field_block);
        f.render_widget(field_widget, rows[i]);
    }

    // Cursor at the caret position within the focused field.
    let frow = rows[focused_field];
    let col = cursor as u16;
    let max_x = frow.x + frow.width.saturating_sub(2);
    f.set_cursor_position((((frow.x + 1) + col).min(max_x), frow.y + 1));

    let hint = Paragraph::new("Tab: next field  Enter: save  Esc: cancel")
        .alignment(Alignment::Center)
        .style(Style::default().fg(Color::DarkGray));
    f.render_widget(hint, rows[EDIT_FIELD_COUNT]);
}

/// Small centered confirmation popup for deleting a host.
fn draw_confirm_delete(f: &mut Frame, name: &str, area: Rect) {
    let popup_width = area.width.min(54);
    let popup_height = 5u16;
    let x = area.x + (area.width.saturating_sub(popup_width)) / 2;
    let y = area.y + (area.height.saturating_sub(popup_height)) / 2;
    let popup_area = Rect::new(x, y, popup_width, popup_height);

    f.render_widget(Clear, popup_area);
    let block = Block::default()
        .borders(Borders::ALL)
        .title(" Delete host ")
        .title_alignment(Alignment::Center)
        .border_style(Style::default().fg(Color::Red));
    let inner = block.inner(popup_area);
    f.render_widget(block, popup_area);

    let lines = vec![
        Line::from(Span::styled(
            format!("Delete \"{name}\"?"),
            Style::default().add_modifier(Modifier::BOLD),
        )),
        Line::from(Span::styled(
            "y/Enter: delete    n/Esc: cancel",
            Style::default().fg(Color::DarkGray),
        )),
    ];
    let body = Paragraph::new(lines).alignment(Alignment::Center);
    f.render_widget(body, inner);
}

fn draw_credential_form(
    f: &mut Frame,
    reference: &str,
    password: &str,
    focused: usize,
    cursor: usize,
    area: Rect,
) {
    let popup_width = area.width.min(60);
    let popup_height = 2 * 3 + 4;
    let x = area.x + (area.width.saturating_sub(popup_width)) / 2;
    let y = area.y + (area.height.saturating_sub(popup_height)) / 2;
    let popup_area = Rect::new(x, y, popup_width, popup_height);

    f.render_widget(Clear, popup_area);

    let block = Block::default()
        .borders(Borders::ALL)
        .title(" Add Credential ")
        .title_alignment(Alignment::Center);
    f.render_widget(block, popup_area);

    let inner = popup_area.inner(Margin {
        horizontal: 1,
        vertical: 1,
    });

    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(3), Constraint::Length(3), Constraint::Min(1)])
        .split(inner);

    let masked = "●".repeat(password.chars().count());
    let fields = [
        ("Credential ref", reference),
        ("Password", masked.as_str()),
    ];
    for (i, (label, text)) in fields.iter().enumerate() {
        let border_style = if i == focused {
            Style::default().fg(Color::Yellow)
        } else {
            Style::default()
        };
        let field_block = Block::default()
            .borders(Borders::ALL)
            .title(*label)
            .border_style(border_style);
        f.render_widget(Paragraph::new(*text).block(field_block), rows[i]);
    }

    // Cursor at the caret position within the focused field.
    let col = cursor as u16;
    let frow = rows[focused];
    let max_x = frow.x + frow.width.saturating_sub(2);
    f.set_cursor_position((((frow.x + 1) + col).min(max_x), frow.y + 1));

    let hint = Paragraph::new("Tab: switch field  Enter: save  Esc: cancel")
        .alignment(Alignment::Center)
        .style(Style::default().fg(Color::DarkGray));
    f.render_widget(hint, rows[2]);
}

// ───────────────────────── ASCII art side panel ─────────────────────────

/// Ports per LED row (24 top + 24 bottom = 48-port switch).
const PORTS_PER_ROW: u64 = 24;
/// Inner box width: leading space + 24 LEDs + one space after each group of 6.
const SWITCH_INNER: usize = 1 + (PORTS_PER_ROW as usize) + (PORTS_PER_ROW as usize / 6);

/// Render the right-hand panel: GUKAB logo, an HH:MM seven-segment clock + date,
/// and a 48-port switch whose LEDs animate as a green wave. Only the LEDs change
/// per frame; everything else is static.
fn draw_banner(f: &mut Frame, frame: u64, area: Rect) {
    let cyan = Style::default()
        .fg(Color::Cyan)
        .add_modifier(Modifier::BOLD);
    let dim = Style::default().fg(Color::DarkGray);
    let white = Style::default().fg(Color::White);

    // 3-row block font: the middle row makes "B" unambiguous (2 rows read as "D").
    let mut lines: Vec<Line> = vec![
        Line::raw(""),
        Line::styled("█▀▀ █ █ █▄▀ ▄▀▄ █▀▄", cyan),
        Line::styled("█▄█ █ █ █▀▄ █▀█ █▀▄", cyan),
        Line::styled("▀▀▀ ▀▀▀ ▀ ▀ ▀ ▀ ▀▀▀", cyan),
        Line::raw(""),
    ];

    // Seven-segment HH:MM (no seconds) — re-read each redraw so it ticks.
    let now = Local::now();
    for row in seven_segment(&now.format("%H:%M").to_string()) {
        lines.push(Line::styled(row, white));
    }
    lines.push(Line::styled(now.format("%Y-%m-%d").to_string(), dim));
    lines.push(Line::raw(""));

    // Switch chassis: top border, two LED rows, bottom border.
    let border = format!("┌{}┐", "─".repeat(SWITCH_INNER));
    lines.push(Line::styled(border, dim));
    lines.push(led_row(frame, 1, dim));
    lines.push(led_row(frame, 2, dim));
    lines.push(Line::styled(format!("└{}┘", "─".repeat(SWITCH_INNER)), dim));

    let panel = Paragraph::new(lines).alignment(Alignment::Center);
    f.render_widget(panel, area);
}

/// Build one symmetric LED row framed by the chassis border. LEDs are grouped in
/// sixes; each LED blinks independently on a pseudo-random schedule (`row` salts
/// the hash so the two rows differ) like real switch activity.
fn led_row(frame: u64, row: u64, border_style: Style) -> Line<'static> {
    let mut spans: Vec<Span> = Vec::with_capacity(SWITCH_INNER + 2);
    spans.push(Span::styled("│", border_style));
    spans.push(Span::raw(" "));
    for i in 0..PORTS_PER_ROW {
        let color = blink_color(i, frame, row);
        spans.push(Span::styled("●", Style::default().fg(color)));
        if (i + 1) % 6 == 0 {
            spans.push(Span::raw(" "));
        }
    }
    spans.push(Span::styled("│", border_style));
    Line::from(spans)
}

/// Pseudo-random per-port LED color that changes a few times per second, so ports
/// blink at random rather than in a moving wave. Cheap integer hash — no RNG state.
fn blink_color(port: u64, frame: u64, salt: u64) -> Color {
    // Hold each state ~2 frames (~240ms) so it twinkles instead of flickering.
    let seed = frame / 2;
    let mut h = port
        .wrapping_mul(2654435761)
        .wrapping_add(seed.wrapping_mul(2246822519))
        .wrapping_add(salt.wrapping_mul(40503));
    h ^= h >> 13;
    h = h.wrapping_mul(3266489917);
    h ^= h >> 16;
    // ~50% of ports lit at any moment; the lit ones are mostly green with the
    // occasional amber port (~1/16), like real switch activity. Amber blinks too
    // since the color is recomputed every couple of frames.
    match h % 16 {
        0 => Color::Indexed(208), // amber / orange — rare ("tek tük")
        1..=3 => Color::LightGreen,
        4..=7 => Color::Green,
        _ => Color::DarkGray, // 8..=15 → off
    }
}

/// Render `text` (digits and `:`) as three rows of seven-segment ASCII art.
fn seven_segment(text: &str) -> [String; 3] {
    // 3 rows × 3 cols per glyph.
    let glyph = |c: char| -> [&'static str; 3] {
        match c {
            '0' => [" _ ", "| |", "|_|"],
            '1' => ["   ", "  |", "  |"],
            '2' => [" _ ", " _|", "|_ "],
            '3' => [" _ ", " _|", " _|"],
            '4' => ["   ", "|_|", "  |"],
            '5' => [" _ ", "|_ ", " _|"],
            '6' => [" _ ", "|_ ", "|_|"],
            '7' => [" _ ", "  |", "  |"],
            '8' => [" _ ", "|_|", "|_|"],
            '9' => [" _ ", "|_|", " _|"],
            ':' => [" ", ":", " "],
            _ => ["   ", "   ", "   "],
        }
    };
    let mut rows = [String::new(), String::new(), String::new()];
    for (i, c) in text.chars().enumerate() {
        if i > 0 {
            for row in &mut rows {
                row.push(' ');
            }
        }
        let g = glyph(c);
        for (r, row) in rows.iter_mut().enumerate() {
            row.push_str(g[r]);
        }
    }
    rows
}
