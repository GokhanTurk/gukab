use std::collections::HashSet;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::config::{Group, Host};

pub const EDIT_FIELD_COUNT: usize = 8;
pub const EDIT_FIELD_LABELS: [&str; EDIT_FIELD_COUNT] = [
    "Name",
    "Hostname",
    "Port",
    "Username",
    "Group (blank = ungrouped)",
    "Credential ref",
    "On-connect macros (space-separated)",
    "Password (blank = keep existing)",
];
pub const PORT_FIELD_IDX: usize = 2;
pub const PASSWORD_FIELD_IDX: usize = 7;

/// Flat string representation of a Host used while editing.
/// `password` is never stored in TOML — it goes to the OS keychain on save.
pub struct EditDraft {
    pub name: String,
    pub hostname: String,
    pub port: String,
    pub username: String,
    pub group: String,
    pub credential_ref: String,
    pub on_connect: String,
    pub password: String,
}

impl From<&Host> for EditDraft {
    fn from(h: &Host) -> Self {
        Self {
            name: h.name.clone(),
            hostname: h.hostname.clone(),
            port: h.port.to_string(),
            username: h.username.clone(),
            group: h.group.clone().unwrap_or_default(),
            credential_ref: h.credential_ref.clone(),
            on_connect: h.on_connect.join(" "),
            password: String::new(), // never pre-filled
        }
    }
}

impl EditDraft {
    pub fn blank() -> Self {
        Self {
            name: String::new(),
            hostname: String::new(),
            port: "22".to_string(),
            username: String::new(),
            group: String::new(),
            credential_ref: String::new(),
            on_connect: String::new(),
            password: String::new(),
        }
    }

    pub fn field(&self, idx: usize) -> &str {
        match idx {
            0 => &self.name,
            1 => &self.hostname,
            2 => &self.port,
            3 => &self.username,
            4 => &self.group,
            5 => &self.credential_ref,
            6 => &self.on_connect,
            _ => &self.password,
        }
    }

    fn field_mut(&mut self, idx: usize) -> &mut String {
        match idx {
            0 => &mut self.name,
            1 => &mut self.hostname,
            2 => &mut self.port,
            3 => &mut self.username,
            4 => &mut self.group,
            5 => &mut self.credential_ref,
            6 => &mut self.on_connect,
            _ => &mut self.password,
        }
    }
}

pub enum AppMode {
    Normal,
    Editing {
        /// None = new host
        original_idx: Option<usize>,
        draft: EditDraft,
        focused_field: usize,
        /// Caret position (char index) within the focused field.
        cursor: usize,
    },
    /// Standalone keyring credential entry (Ctrl+K), independent of any host.
    /// Used to store secrets referenced by expect rules (e.g. an enable password).
    Credential {
        reference: String,
        password: String,
        /// 0 = ref field, 1 = password field
        focused: usize,
        /// Caret position (char index) within the focused field.
        cursor: usize,
    },
}

/// A visible line in the host list: either a group header or a host beneath it.
pub enum Row {
    Group {
        name: String,
        icon: String,
        collapsed: bool,
    },
    Host {
        idx: usize,
        icon: String,
    },
}

pub struct App {
    pub hosts: Vec<Host>,
    pub groups: Vec<Group>,
    pub filter: String,
    /// Visible rows (group headers + hosts), rebuilt by `apply_filter`.
    pub rows: Vec<Row>,
    /// Names of currently collapsed groups.
    pub collapsed: HashSet<String>,
    pub selected: usize,
    pub mode: AppMode,
    pub should_quit: bool,
    pub pending_connect: Option<Host>,
    pub status: Option<String>,
    /// Caret position (char index) within the search query.
    pub filter_cursor: usize,
    /// When the app started — drives the wall-clock-based banner animation.
    started: std::time::Instant,
}

impl App {
    pub fn new(hosts: Vec<Host>, groups: Vec<Group>) -> Self {
        let mut app = Self {
            hosts,
            groups,
            filter: String::new(),
            rows: Vec::new(),
            collapsed: HashSet::new(),
            selected: 0,
            mode: AppMode::Normal,
            should_quit: false,
            pending_connect: None,
            status: None,
            filter_cursor: 0,
            started: std::time::Instant::now(),
        };
        app.apply_filter();
        app
    }

    /// Current animation frame (~8 fps), derived from elapsed wall-clock time so
    /// the animation is smooth regardless of how often the UI is redrawn.
    pub fn anim_frame(&self) -> u64 {
        (self.started.elapsed().as_millis() / 120) as u64
    }

    pub fn set_status(&mut self, msg: String) {
        self.status = Some(msg);
    }

    /// The host index of the currently selected row, if it is a host row.
    fn selected_host_idx(&self) -> Option<usize> {
        match self.rows.get(self.selected) {
            Some(Row::Host { idx, .. }) => Some(*idx),
            _ => None,
        }
    }

    /// Open the edit form for the currently selected host, if any.
    fn open_edit_selected(&mut self) {
        if let Some(idx) = self.selected_host_idx() {
            let host = &self.hosts[idx];
            let original_idx = Some(idx);
            let draft = EditDraft::from(host);
            let cursor = draft.field(0).chars().count();
            self.mode = AppMode::Editing {
                original_idx,
                draft,
                focused_field: 0,
                cursor,
            };
        }
    }

    /// Relevance of a host to `filter`: the best fuzzy score across its fields,
    /// or `None` if nothing matches. With an empty filter every host scores 0
    /// (kept in declared order). The name is weighted highest since that is what
    /// the user usually types; hostname/username/group are slightly discounted so
    /// a name hit always wins a tie.
    fn host_score(&self, filter: &str, h: &Host) -> Option<i32> {
        if filter.is_empty() {
            return Some(0);
        }
        let mut best: Option<i32> = None;
        let mut consider = |score: Option<i32>| {
            if let Some(s) = score {
                best = Some(best.map_or(s, |b| b.max(s)));
            }
        };
        consider(crate::fuzzy::fuzzy_score(filter, &h.name.to_lowercase()));
        consider(crate::fuzzy::fuzzy_score(filter, &h.hostname.to_lowercase()).map(|s| s - 1));
        consider(crate::fuzzy::fuzzy_score(filter, &h.username.to_lowercase()).map(|s| s - 1));
        if let Some(g) = h.group.as_deref() {
            consider(crate::fuzzy::fuzzy_score(filter, &g.to_lowercase()).map(|s| s - 1));
        }
        best
    }

    /// Icon declared for a group name (empty if undeclared).
    fn group_icon(&self, name: &str) -> String {
        self.groups
            .iter()
            .find(|g| g.name == name)
            .map(|g| g.icon.clone())
            .unwrap_or_default()
    }

    /// Rebuild `rows`: group headers (in declared, then first-seen order, then
    /// ungrouped last) each followed by their visible hosts unless collapsed.
    /// While a filter is active, collapse is ignored so matches always show.
    fn apply_filter(&mut self) {
        let filter = self.filter.to_lowercase();
        let filtering = !filter.is_empty();

        // Score every host; keep only those that match (with their score).
        let scores: Vec<Option<i32>> = (0..self.hosts.len())
            .map(|i| self.host_score(&filter, &self.hosts[i]))
            .collect();
        let visible: Vec<usize> = (0..self.hosts.len())
            .filter(|&i| scores[i].is_some())
            .collect();

        // The single best-scoring host (highest score, ties keep first-seen) — the
        // row we want selected so "fen3" lands on Feneryolu-3, not the first row.
        let best_host: Option<usize> = if filtering {
            visible
                .iter()
                .copied()
                .max_by_key(|&i| scores[i].unwrap_or(i32::MIN))
        } else {
            None
        };

        // If no grouping is in play at all, render a flat list (legacy look).
        let any_grouping =
            !self.groups.is_empty() || self.hosts.iter().any(|h| h.group.is_some());
        if !any_grouping {
            // While filtering, rank by score (best first); otherwise keep order.
            let mut order = visible.clone();
            if filtering {
                order.sort_by(|&a, &b| scores[b].cmp(&scores[a]));
            }
            self.rows = order
                .into_iter()
                .map(|idx| Row::Host { idx, icon: String::new() })
                .collect();
            self.select_best_or_first_host(best_host);
            return;
        }

        // Ordered group names: declared first, then any other group used by a host
        // (first-seen). Ungrouped hosts are handled separately, last.
        let mut order: Vec<String> = self.groups.iter().map(|g| g.name.clone()).collect();
        for &i in &visible {
            if let Some(g) = &self.hosts[i].group
                && !order.contains(g)
            {
                order.push(g.clone());
            }
        }

        let mut rows = Vec::new();
        for name in &order {
            let mut members: Vec<usize> = visible
                .iter()
                .copied()
                .filter(|&i| self.hosts[i].group.as_deref() == Some(name.as_str()))
                .collect();
            if members.is_empty() {
                continue;
            }
            // Rank members within the group by score while filtering.
            if filtering {
                members.sort_by(|&a, &b| scores[b].cmp(&scores[a]));
            }
            let collapsed = self.collapsed.contains(name);
            let icon = self.group_icon(name);
            rows.push(Row::Group {
                name: name.clone(),
                icon: icon.clone(),
                collapsed,
            });
            if !collapsed || filtering {
                for idx in members {
                    rows.push(Row::Host { idx, icon: icon.clone() });
                }
            }
        }
        // Ungrouped hosts, under a trailing header.
        let mut ungrouped: Vec<usize> = visible
            .iter()
            .copied()
            .filter(|&i| self.hosts[i].group.is_none())
            .collect();
        if !ungrouped.is_empty() {
            if filtering {
                ungrouped.sort_by(|&a, &b| scores[b].cmp(&scores[a]));
            }
            let name = "Ungrouped".to_string();
            let collapsed = self.collapsed.contains(&name);
            rows.push(Row::Group {
                name,
                icon: String::new(),
                collapsed,
            });
            if !collapsed || filtering {
                for idx in ungrouped {
                    rows.push(Row::Host { idx, icon: String::new() });
                }
            }
        }

        self.rows = rows;
        self.select_best_or_first_host(best_host);
    }

    /// Move the selection to `best_host`'s row if given (the top-ranked match),
    /// otherwise to the first host row, skipping any leading group headers.
    fn select_best_or_first_host(&mut self, best_host: Option<usize>) {
        self.clamp_selected();
        if let Some(best) = best_host
            && let Some(pos) = self
                .rows
                .iter()
                .position(|r| matches!(r, Row::Host { idx, .. } if *idx == best))
        {
            self.selected = pos;
            return;
        }
        // Skip initial group headers; select first host row instead.
        while self.selected < self.rows.len() {
            match &self.rows[self.selected] {
                Row::Host { .. } => break,
                Row::Group { .. } => self.selected += 1,
            }
        }
    }

    fn clamp_selected(&mut self) {
        if self.selected >= self.rows.len() {
            self.selected = self.rows.len().saturating_sub(1);
        }
    }

    pub fn update(&mut self, key: KeyEvent) {
        match self.mode {
            AppMode::Normal => self.update_normal(key),
            AppMode::Editing { .. } => self.update_editing(key),
            AppMode::Credential { .. } => self.update_credential(key),
        }
    }

    fn update_normal(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Char('q') if key.modifiers.is_empty() => self.should_quit = true,
            KeyCode::Esc => self.should_quit = true,

            KeyCode::Down => {
                if !self.rows.is_empty() {
                    self.selected = (self.selected + 1).min(self.rows.len() - 1);
                }
            }
            KeyCode::Up => {
                self.selected = self.selected.saturating_sub(1);
            }

            KeyCode::Char('k') if key.modifiers == KeyModifiers::CONTROL => {
                self.mode = AppMode::Credential {
                    reference: String::new(),
                    password: String::new(),
                    focused: 0,
                    cursor: 0,
                };
            }

            KeyCode::Char('n') if key.modifiers == KeyModifiers::CONTROL => {
                self.mode = AppMode::Editing {
                    original_idx: None,
                    draft: EditDraft::blank(),
                    focused_field: 0,
                    cursor: 0,
                };
            }

            // Ctrl+E is the reliable edit trigger: most terminals (KDE Konsole,
            // macOS Terminal, …) send Shift+Enter as a plain Enter, so the modifier
            // check below never matches there. Shift+Enter is kept for terminals that
            // do report it (e.g. kitty keyboard protocol).
            KeyCode::Char('e') if key.modifiers == KeyModifiers::CONTROL => {
                self.open_edit_selected();
            }
            KeyCode::Enter if key.modifiers.contains(KeyModifiers::SHIFT) => {
                self.open_edit_selected();
            }
            KeyCode::Enter => match self.rows.get(self.selected) {
                // On a group header, toggle collapse; on a host, connect.
                Some(Row::Group { name, .. }) => {
                    let name = name.clone();
                    if !self.collapsed.remove(&name) {
                        self.collapsed.insert(name);
                    }
                    self.apply_filter();
                }
                Some(Row::Host { idx, .. }) => {
                    let host = self.hosts[*idx].clone();
                    self.pending_connect = Some(host);
                    self.should_quit = true;
                }
                None => {}
            },

            // Text editing (insert / Backspace / Delete / Left / Right / Home / End).
            _ => {
                let mut cursor = self.filter_cursor;
                let outcome = apply_edit_key(&mut self.filter, &mut cursor, key);
                if outcome.consumed {
                    self.filter_cursor = cursor.min(self.filter.chars().count());
                    if outcome.changed {
                        self.apply_filter();
                    }
                }
            }
        }
    }

    fn update_editing(&mut self, key: KeyEvent) {
        let AppMode::Editing {
            ref original_idx,
            ref mut draft,
            ref mut focused_field,
            ref mut cursor,
        } = self.mode
        else {
            return;
        };

        match key.code {
            KeyCode::Esc => {
                self.mode = AppMode::Normal;
            }

            KeyCode::Tab => {
                *focused_field = (*focused_field + 1) % EDIT_FIELD_COUNT;
                *cursor = draft.field(*focused_field).chars().count();
            }
            KeyCode::BackTab => {
                *focused_field =
                    focused_field.checked_sub(1).unwrap_or(EDIT_FIELD_COUNT - 1);
                *cursor = draft.field(*focused_field).chars().count();
            }

            KeyCode::Enter => {
                let idx = *original_idx;
                // Preserve automations (macros/expects) — the flat edit form does
                // not expose them, so keep what an edited host already had.
                let (macros, expects) = match idx {
                    Some(i) => (
                        self.hosts[i].macros.clone(),
                        self.hosts[i].expects.clone(),
                    ),
                    None => (Vec::new(), Vec::new()),
                };
                let group = {
                    let g = draft.group.trim();
                    if g.is_empty() {
                        None
                    } else {
                        Some(g.to_string())
                    }
                };
                let on_connect = draft
                    .on_connect
                    .split_whitespace()
                    .map(|s| s.to_string())
                    .collect();
                let host = Host {
                    name: draft.name.clone(),
                    hostname: draft.hostname.clone(),
                    port: draft.port.parse().unwrap_or(22),
                    username: draft.username.clone(),
                    credential_ref: draft.credential_ref.clone(),
                    group,
                    macros,
                    expects,
                    on_connect,
                };
                let credential_ref = draft.credential_ref.clone();
                let password = draft.password.clone();

                // Persist password to keyring when provided
                if !password.is_empty() {
                    if credential_ref.is_empty() {
                        self.set_status(
                            "Credential ref cannot be empty when setting a password".into(),
                        );
                        return;
                    }
                    if let Err(e) = write_credential(&credential_ref, &password) {
                        self.set_status(e);
                        return;
                    }
                    self.set_status(format!("Credential saved (ref: {credential_ref})"));
                }

                if let Some(i) = idx {
                    self.hosts[i] = host;
                } else {
                    self.hosts.push(host);
                }
                if let Err(e) = crate::config::save_hosts(&self.hosts, &self.groups) {
                    self.set_status(format!("Save failed: {e}"));
                }
                self.apply_filter();
                self.mode = AppMode::Normal;
            }

            // Port field accepts digits only.
            KeyCode::Char(c)
                if *focused_field == PORT_FIELD_IDX
                    && !c.is_ascii_digit()
                    && !key
                        .modifiers
                        .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) => {}

            // Text editing (insert / Backspace / Delete / Left / Right / Home / End).
            _ => {
                apply_edit_key(draft.field_mut(*focused_field), cursor, key);
            }
        }
    }

    fn update_credential(&mut self, key: KeyEvent) {
        let AppMode::Credential {
            ref mut reference,
            ref mut password,
            ref mut focused,
            ref mut cursor,
        } = self.mode
        else {
            return;
        };

        match key.code {
            KeyCode::Esc => self.mode = AppMode::Normal,
            KeyCode::Tab | KeyCode::BackTab => {
                *focused ^= 1;
                let field = if *focused == 0 { &*reference } else { &*password };
                *cursor = field.chars().count();
            }

            KeyCode::Enter => {
                let reference = reference.clone();
                let password = password.clone();
                if reference.is_empty() || password.is_empty() {
                    self.set_status("Both ref and password are required".into());
                    return;
                }
                match write_credential(&reference, &password) {
                    Ok(()) => {
                        self.set_status(format!("Credential saved (ref: {reference})"));
                        self.mode = AppMode::Normal;
                    }
                    Err(e) => self.set_status(e),
                }
            }

            // Text editing (insert / Backspace / Delete / Left / Right / Home / End).
            _ => {
                let field = if *focused == 0 { reference } else { password };
                apply_edit_key(field, cursor, key);
            }
        }
    }
}

/// Result of feeding a key to the shared line editor.
pub struct EditOutcome {
    /// The key was an editing key (consumed here).
    pub consumed: bool,
    /// The text content (not just the caret) changed.
    pub changed: bool,
}

/// Apply one text-editing key to `(text, cursor)` where `cursor` is a char index.
/// Handles Left/Right/Home/End, Backspace/Delete and character insertion at the
/// caret. Non-editing keys are left for the caller (`consumed = false`).
fn apply_edit_key(text: &mut String, cursor: &mut usize, key: KeyEvent) -> EditOutcome {
    let char_count = text.chars().count();
    let consumed;
    let mut changed = false;
    match key.code {
        KeyCode::Left => {
            *cursor = cursor.saturating_sub(1);
            consumed = true;
        }
        KeyCode::Right => {
            if *cursor < char_count {
                *cursor += 1;
            }
            consumed = true;
        }
        KeyCode::Home => {
            *cursor = 0;
            consumed = true;
        }
        KeyCode::End => {
            *cursor = char_count;
            consumed = true;
        }
        KeyCode::Backspace => {
            if *cursor > 0 {
                let byte = char_byte_index(text, *cursor - 1);
                text.remove(byte);
                *cursor -= 1;
                changed = true;
            }
            consumed = true;
        }
        KeyCode::Delete => {
            if *cursor < char_count {
                let byte = char_byte_index(text, *cursor);
                text.remove(byte);
                changed = true;
            }
            consumed = true;
        }
        // Only plain (or Shift) chars are text; Ctrl/Alt combos are left for callers.
        KeyCode::Char(c)
            if !key
                .modifiers
                .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT | KeyModifiers::SUPER) =>
        {
            let byte = char_byte_index(text, *cursor);
            text.insert(byte, c);
            *cursor += 1;
            changed = true;
            consumed = true;
        }
        _ => consumed = false,
    }
    EditOutcome { consumed, changed }
}

/// Byte offset of the `char_idx`-th character (or `s.len()` at/after the end).
fn char_byte_index(s: &str, char_idx: usize) -> usize {
    s.char_indices()
        .nth(char_idx)
        .map(|(b, _)| b)
        .unwrap_or(s.len())
}

/// Write a secret to the OS keychain under `keyring::Entry::new("gukab", reference)`
/// and read it back to confirm the backend persisted it. Returns a user-facing
/// error message on failure.
fn write_credential(reference: &str, password: &str) -> Result<(), String> {
    let entry =
        keyring::Entry::new("gukab", reference).map_err(|e| format!("Keyring error: {e}"))?;
    entry
        .set_password(password)
        .map_err(|e| format!("Keyring write error: {e}"))?;
    entry
        .get_password()
        .map_err(|e| format!("Keyring verify failed: {e}"))?;
    Ok(())
}
