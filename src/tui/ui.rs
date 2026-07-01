use ratatui::{
    layout::{Alignment, Constraint, Direction, Layout, Margin, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph},
    Frame,
};

use chrono::Local;

use crate::tui::app::{
    App, AppMode, CField, ConsoleForm, ExpectEdit, MacroEdit, MacroFocus, MacroScreen, MacroState,
    EDIT_FIELD_COUNT, EDIT_FIELD_LABELS, PASSWORD_FIELD_IDX,
};
use crate::config::Macro;
use crate::serial::{Flow, Parity};

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
        AppMode::Macros(ref state) => draw_macros(f, state, area),
        AppMode::Console(ref form) => draw_console_form(f, form, area),
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
            Row::Group { .. } | Row::ConsoleAction => None,
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
                Row::ConsoleAction => {
                    // Selected row drops its fg so the highlight shows through.
                    let style = if selected {
                        Style::default()
                    } else {
                        Style::default().fg(Color::Green).add_modifier(Modifier::BOLD)
                    };
                    ListItem::new(Line::from(Span::styled("＋  Console connection…", style)))
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
        None => " ↑↓ nav  Enter connect  ^N add  ^E edit  ^D del  ^K cred  ^G macros  ^L console  Esc quit",
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

// ───────────────────────── Macro manager (Ctrl+G) ─────────────────────────

/// Selection highlight shared with the host list: bright white text on a vivid blue
/// bar (explicit RGB so contrast is identical across terminals).
fn selection_highlight() -> Style {
    Style::default()
        .fg(Color::Rgb(255, 255, 255))
        .bg(Color::Rgb(45, 95, 210))
        .add_modifier(Modifier::BOLD)
}

/// Width (in cells) of the list highlight symbol, reserved on every row.
const HL_SYMBOL: &str = "▶ ";
const HL_SYMBOL_W: u16 = 2;

/// Draw a bordered single-line field (yellow border when focused), like the host
/// edit form's rows. Returns the inner text area so the caller can park the cursor.
fn draw_field(f: &mut Frame, area: Rect, label: &str, text: &str, focused: bool) {
    let border_style = if focused {
        Style::default().fg(Color::Yellow)
    } else {
        Style::default()
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .title(label.to_string())
        .border_style(border_style);
    f.render_widget(Paragraph::new(text).block(block), area);
}

/// Place the text cursor at char index `cursor` within a bordered field `area`.
fn cursor_in_field(f: &mut Frame, area: Rect, cursor: usize) {
    let max_x = area.x + area.width.saturating_sub(2);
    f.set_cursor_position((((area.x + 1) + cursor as u16).min(max_x), area.y + 1));
}

fn draw_macros(f: &mut Frame, state: &MacroState, area: Rect) {
    // The list is the base layer; edit / confirm popups overlay it for context.
    draw_macro_list(f, &state.macros, state.list_selected, area);
    match &state.screen {
        MacroScreen::List => {}
        MacroScreen::ConfirmDeleteMacro(i) => {
            let key = state.macros.get(*i).map(|m| m.key.as_str()).unwrap_or("");
            draw_confirm_delete_macro(f, key, area);
        }
        MacroScreen::Edit(edit) => {
            draw_macro_edit(f, edit, area);
            if let Some(sub) = &edit.sub {
                draw_expect_edit(f, sub, area);
            }
        }
    }
}

fn draw_macro_list(f: &mut Frame, macros: &[Macro], selected: usize, area: Rect) {
    let popup_width = area.width.min(74);
    let popup_height = area.height.saturating_sub(4).clamp(6, 22);
    let x = area.x + (area.width.saturating_sub(popup_width)) / 2;
    let y = area.y + (area.height.saturating_sub(popup_height)) / 2;
    let popup_area = Rect::new(x, y, popup_width, popup_height);

    f.render_widget(Clear, popup_area);
    let block = Block::default()
        .borders(Borders::ALL)
        .title(" Macros ")
        .title_alignment(Alignment::Center);
    let inner = block.inner(popup_area);
    f.render_widget(block, popup_area);

    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(1), Constraint::Length(1)])
        .split(inner);

    if macros.is_empty() {
        let empty = Paragraph::new("No macros yet — Ctrl+N to add one.")
            .alignment(Alignment::Center)
            .style(Style::default().fg(Color::DarkGray));
        f.render_widget(empty, rows[0]);
    } else {
        let items: Vec<ListItem> = macros
            .iter()
            .map(|m| {
                let preview = m
                    .send
                    .lines()
                    .find(|l| !l.trim().is_empty())
                    .unwrap_or("");
                let mut spans = vec![Span::styled(
                    format!("{:<12}", m.key),
                    Style::default().add_modifier(Modifier::BOLD),
                )];
                spans.push(Span::styled(
                    preview.to_string(),
                    Style::default().fg(Color::Gray),
                ));
                if !m.expects.is_empty() {
                    spans.push(Span::styled(
                        format!("  [{} expect]", m.expects.len()),
                        Style::default().fg(Color::DarkGray),
                    ));
                }
                ListItem::new(Line::from(spans))
            })
            .collect();
        let list = List::new(items)
            .highlight_style(selection_highlight())
            .highlight_symbol(HL_SYMBOL);
        let mut lstate = ListState::default();
        lstate.select(Some(selected.min(macros.len().saturating_sub(1))));
        f.render_stateful_widget(list, rows[0], &mut lstate);
    }

    let hint = Paragraph::new("Enter/Ctrl+E edit  Ctrl+N add  Ctrl+D delete  Esc close")
        .alignment(Alignment::Center)
        .style(Style::default().fg(Color::DarkGray));
    f.render_widget(hint, rows[1]);
}

fn draw_macro_edit(f: &mut Frame, edit: &MacroEdit, area: Rect) {
    // All rows are single lines in one scrolling List (so many-line macros can't
    // overflow), using the same highlight colors as the host list.
    let popup_width = area.width.min(66);
    // Content = Key + "Commands" label + N cmd lines + "Expects" label + M expects.
    let content_rows =
        (1 + 1 + edit.cmd_lines.len() + 1 + edit.expects.len()) as u16;
    // +2 for the border, +1 for the hint line inside.
    let popup_height = (content_rows + 3).min(area.height);
    let x = area.x + (area.width.saturating_sub(popup_width)) / 2;
    let y = area.y + (area.height.saturating_sub(popup_height)) / 2;
    let popup_area = Rect::new(x, y, popup_width, popup_height);

    f.render_widget(Clear, popup_area);
    let title = if edit.original_idx.is_none() {
        " Add Macro "
    } else {
        " Edit Macro "
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .title(title)
        .title_alignment(Alignment::Center);
    let inner = block.inner(popup_area);
    f.render_widget(block, popup_area);

    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(1), Constraint::Length(1)])
        .split(inner);
    let list_area = rows[0];

    // Section headers reuse the host list's group-header color (yellow bold).
    let label_style = Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD);
    let mut items: Vec<ListItem> = Vec::new();
    // Focus target per row (None = a non-selectable label row).
    let mut focus_at: Vec<Option<MacroFocus>> = Vec::new();
    // (flat row index, prefix width) of the focused text field, for caret placement.
    let mut caret: Option<(usize, u16)> = None;

    // Selected rows carry no per-span fg so the white highlight fg wins (as in the
    // host list); unselected rows use the host list's cyan/gray accents.
    let key_sel = edit.focus == MacroFocus::Key;
    let key_prefix = "Key  ";
    items.push(ListItem::new(Line::from(if key_sel {
        vec![Span::raw(key_prefix), Span::raw(edit.key.clone())]
    } else {
        vec![
            Span::styled(key_prefix, Style::default().fg(Color::DarkGray)),
            Span::styled(edit.key.clone(), Style::default().fg(Color::Cyan)),
        ]
    })));
    focus_at.push(Some(MacroFocus::Key));
    if key_sel {
        caret = Some((0, key_prefix.chars().count() as u16));
    }

    items.push(ListItem::new(Line::from(Span::styled("Commands", label_style))));
    focus_at.push(None);
    for (i, cmd) in edit.cmd_lines.iter().enumerate() {
        let sel = edit.focus == MacroFocus::Cmd(i);
        let prefix = format!("{:>3}  ", i + 1);
        let pw = prefix.chars().count() as u16;
        items.push(ListItem::new(Line::from(if sel {
            vec![Span::raw(prefix), Span::raw(cmd.clone())]
        } else {
            vec![
                Span::styled(prefix, Style::default().fg(Color::DarkGray)),
                Span::raw(cmd.clone()),
            ]
        })));
        focus_at.push(Some(MacroFocus::Cmd(i)));
        if sel {
            caret = Some((items.len() - 1, pw));
        }
    }

    items.push(ListItem::new(Line::from(Span::styled("Expects", label_style))));
    focus_at.push(None);
    for (i, e) in edit.expects.iter().enumerate() {
        let action = if let Some(s) = &e.send {
            format!("send {s:?}")
        } else if let Some(c) = &e.send_credential {
            format!("cred {c}")
        } else {
            "?".to_string()
        };
        let once = if e.once { " (once)" } else { "" };
        let text = format!("  • {}  →  {}{}", e.pattern, action, once);
        let sel = edit.focus == MacroFocus::Expect(i);
        items.push(ListItem::new(Line::from(if sel {
            Span::raw(text)
        } else {
            Span::styled(text, Style::default().fg(Color::Gray))
        })));
        focus_at.push(Some(MacroFocus::Expect(i)));
    }

    let selected_flat = focus_at.iter().position(|f| *f == Some(edit.focus));
    let list = List::new(items)
        .highlight_style(selection_highlight())
        .highlight_symbol(HL_SYMBOL);
    let mut lstate = ListState::default();
    lstate.select(selected_flat);
    f.render_stateful_widget(list, list_area, &mut lstate);

    let hint = Paragraph::new("Tab/↑↓ move  ^X add expect  ^E edit  ^D remove  ^S save  Esc")
        .alignment(Alignment::Center)
        .style(Style::default().fg(Color::DarkGray));
    f.render_widget(hint, rows[1]);

    // Caret for the focused text field (Key / command line). The focused row is the
    // selected one, so its content is shifted right by the highlight symbol width.
    if let Some((flat, prefix)) = caret {
        let offset = lstate.offset();
        if flat >= offset {
            let vis = (flat - offset) as u16;
            if vis < list_area.height {
                let base_x = list_area.x + HL_SYMBOL_W + prefix + edit.cursor as u16;
                let max_x = list_area.x + list_area.width.saturating_sub(1);
                f.set_cursor_position((base_x.min(max_x), list_area.y + vis));
            }
        }
    }
}

fn draw_expect_edit(f: &mut Frame, sub: &ExpectEdit, area: Rect) {
    let popup_width = area.width.min(60);
    let popup_height = 4 * 3 + 4;
    let x = area.x + (area.width.saturating_sub(popup_width)) / 2;
    let y = area.y + (area.height.saturating_sub(popup_height)) / 2;
    let popup_area = Rect::new(x, y, popup_width, popup_height);

    f.render_widget(Clear, popup_area);
    let title = if sub.original_idx.is_none() {
        " Add Expect "
    } else {
        " Edit Expect "
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .title(title)
        .title_alignment(Alignment::Center);
    let inner = block.inner(popup_area);
    f.render_widget(block, popup_area);

    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Length(3),
            Constraint::Length(3),
            Constraint::Length(3),
            Constraint::Min(1),
        ])
        .split(inner);

    draw_field(f, rows[0], "Pattern (regex)", sub.field(0), sub.focus == 0);
    draw_field(f, rows[1], "Send (literal)", sub.field(1), sub.focus == 1);
    draw_field(f, rows[2], "Credential ref", sub.field(2), sub.focus == 2);
    let once_text = if sub.once {
        "[x] fire once per session"
    } else {
        "[ ] fire on every match"
    };
    draw_field(f, rows[3], "Once (Space toggles)", once_text, sub.focus == 3);

    if sub.focus < 3 {
        cursor_in_field(f, rows[sub.focus], sub.cursor);
    }

    let hint = Paragraph::new("Tab: field  Space: toggle  Ctrl+S/Enter: save  Esc: cancel")
        .alignment(Alignment::Center)
        .style(Style::default().fg(Color::DarkGray));
    f.render_widget(hint, rows[4]);
}

/// Confirm popup for deleting a macro (mirrors `draw_confirm_delete` for hosts).
fn draw_confirm_delete_macro(f: &mut Frame, key: &str, area: Rect) {
    let popup_width = area.width.min(54);
    let popup_height = 5u16;
    let x = area.x + (area.width.saturating_sub(popup_width)) / 2;
    let y = area.y + (area.height.saturating_sub(popup_height)) / 2;
    let popup_area = Rect::new(x, y, popup_width, popup_height);

    f.render_widget(Clear, popup_area);
    let block = Block::default()
        .borders(Borders::ALL)
        .title(" Delete macro ")
        .title_alignment(Alignment::Center)
        .border_style(Style::default().fg(Color::Red));
    let inner = block.inner(popup_area);
    f.render_widget(block, popup_area);

    let lines = vec![
        Line::from(Span::styled(
            format!("Delete macro \"{key}\"?"),
            Style::default().add_modifier(Modifier::BOLD),
        )),
        Line::from(Span::styled(
            "y/Enter: delete    n/Esc: cancel",
            Style::default().fg(Color::DarkGray),
        )),
    ];
    f.render_widget(
        Paragraph::new(lines).alignment(Alignment::Center),
        inner,
    );
}

// ───────────────────────── Console (serial) form ─────────────────────────

fn parity_short(p: Parity) -> &'static str {
    match p {
        Parity::None => "N",
        Parity::Even => "E",
        Parity::Odd => "O",
    }
}

fn parity_label(p: Parity) -> &'static str {
    match p {
        Parity::None => "None",
        Parity::Even => "Even",
        Parity::Odd => "Odd",
    }
}

fn flow_label(fl: Flow) -> &'static str {
    match fl {
        Flow::None => "None",
        Flow::Software => "Software (XON/XOFF)",
        Flow::Hardware => "Hardware (RTS/CTS)",
    }
}

fn draw_console_form(f: &mut Frame, form: &ConsoleForm, area: Rect) {
    let fields = form.fields();
    let n = fields.len();

    let popup_width = area.width.min(64);
    let popup_height = ((n as u16) * 3 + 3).min(area.height);
    let x = area.x + (area.width.saturating_sub(popup_width)) / 2;
    let y = area.y + (area.height.saturating_sub(popup_height)) / 2;
    let popup_area = Rect::new(x, y, popup_width, popup_height);

    f.render_widget(Clear, popup_area);
    let block = Block::default()
        .borders(Borders::ALL)
        .title(" Console connection ")
        .title_alignment(Alignment::Center);
    let inner = block.inner(popup_area);
    f.render_widget(block, popup_area);

    let mut cons: Vec<Constraint> = (0..n).map(|_| Constraint::Length(3)).collect();
    cons.push(Constraint::Min(1));
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints(cons)
        .split(inner);

    let mut focus_rect: Option<Rect> = None;
    for (i, field) in fields.iter().enumerate() {
        let focused = i == form.focus;
        let (label, value) = match field {
            CField::Device => (
                "Device  (↑↓ detected · ^R rescan)".to_string(),
                form.device.clone(),
            ),
            CField::Baud => ("Baud  (↑↓ presets)".to_string(), form.baud.clone()),
            CField::Advanced => {
                let arrow = if form.advanced_open { "▾" } else { "▸" };
                let summary = format!(
                    "{}-{}-{}  ·  flow {}",
                    form.data_bits,
                    parity_short(form.parity),
                    form.stop_bits,
                    flow_label(form.flow),
                );
                (format!("{arrow} Advanced  (Enter toggles)"), summary)
            }
            CField::DataBits => ("Data bits  (←→)".to_string(), form.data_bits.to_string()),
            CField::Parity => ("Parity  (←→)".to_string(), parity_label(form.parity).to_string()),
            CField::StopBits => ("Stop bits  (←→)".to_string(), form.stop_bits.to_string()),
            CField::Flow => ("Flow control  (←→)".to_string(), flow_label(form.flow).to_string()),
        };
        draw_field(f, rows[i], &label, &value, focused);
        if focused && matches!(field, CField::Device | CField::Baud) {
            focus_rect = Some(rows[i]);
        }
    }

    let hint = Paragraph::new("Tab: field  ↑↓/←→: value  Enter: connect  Esc: cancel")
        .alignment(Alignment::Center)
        .style(Style::default().fg(Color::DarkGray));
    f.render_widget(hint, rows[n]);

    // Caret only for the focused text field (Device / Baud).
    if let Some(r) = focus_rect {
        cursor_in_field(f, r, form.cursor);
    }
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
    // Readable even on semi-transparent terminals (DarkGray washes out there).
    let label = Style::default().fg(Color::Gray);

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
    lines.push(Line::raw("")); // breathing room between the clock and the date
    lines.push(Line::styled(now.format("%Y-%m-%d").to_string(), label));
    lines.push(Line::raw(""));

    // Switch chassis: top border, two LED rows, bottom border.
    let border = format!("┌{}┐", "─".repeat(SWITCH_INNER));
    lines.push(Line::styled(border, dim));
    lines.push(led_row(frame, 1, dim));
    lines.push(led_row(frame, 2, dim));
    lines.push(Line::styled(format!("└{}┘", "─".repeat(SWITCH_INNER)), dim));

    // Version (compiled in), under the switch.
    lines.push(Line::raw(""));
    lines.push(Line::styled(format!("v{}", env!("CARGO_PKG_VERSION")), cyan));

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

#[cfg(test)]
mod render_tests {
    use super::*;
    use crate::tui::app::MacroEdit;
    use ratatui::{backend::TestBackend, Terminal};

    #[test]
    fn many_line_macro_renders_focused_row_visible() {
        // A kdm-style macro with many command lines in a short terminal.
        let cmds: Vec<String> = (1..=16).map(|i| format!("command-line-{i}")).collect();
        let mut edit = MacroEdit {
            original_idx: Some(0),
            key: "kdm".into(),
            cmd_lines: cmds,
            expects: Vec::new(),
            focus: crate::tui::app::MacroFocus::Cmd(14),
            cursor: 3,
            sub: None,
        };
        let backend = TestBackend::new(80, 20);
        let mut term = Terminal::new(backend).unwrap();
        term.draw(|f| draw_macro_edit(f, &edit, f.area())).unwrap();
        let buf = term.backend().buffer().clone();
        let text: String = buf.content().iter().map(|c| c.symbol()).collect();
        // The focused (scrolled-to) command line must be visible.
        assert!(text.contains("command-line-15"), "focused row not visible");

        // Focusing the first row scrolls back to the top / Key row.
        edit.focus = crate::tui::app::MacroFocus::Key;
        term.draw(|f| draw_macro_edit(f, &edit, f.area())).unwrap();
        let text: String = term
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|c| c.symbol())
            .collect();
        assert!(text.contains("kdm"), "Key row not visible");
    }

    #[test]
    fn console_form_renders_and_advanced_toggles() {
        use crate::tui::app::ConsoleForm;
        let mut form = ConsoleForm::new();
        form.device = "/dev/ttyUSB0".into();
        form.baud = "115200".into();

        let backend = TestBackend::new(80, 24);
        let mut term = Terminal::new(backend).unwrap();
        let render = |term: &mut Terminal<TestBackend>, form: &ConsoleForm| -> String {
            term.draw(|f| draw_console_form(f, form, f.area())).unwrap();
            term.backend()
                .buffer()
                .content()
                .iter()
                .map(|c| c.symbol())
                .collect()
        };

        let text = render(&mut term, &form);
        assert!(text.contains("/dev/ttyUSB0"));
        assert!(text.contains("115200"));
        assert!(text.contains("Advanced"));
        assert!(!text.contains("Flow control"), "advanced should be collapsed");

        form.advanced_open = true;
        let text = render(&mut term, &form);
        assert!(text.contains("Flow control"), "advanced rows should show");
    }
}
