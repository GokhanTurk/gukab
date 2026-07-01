use std::collections::HashSet;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::config::{Automations, Expect, Group, Host, Macro};

pub const EDIT_FIELD_COUNT: usize = 9;
pub const EDIT_FIELD_LABELS: [&str; EDIT_FIELD_COUNT] = [
    "Name",
    "Hostname",
    "Port",
    "Username",
    "Group (blank = ungrouped)",
    "Credential ref (password / key passphrase)",
    "SSH key file (blank = password auth)",
    "On-connect macros (space-separated)",
    "Password (blank = keep existing)",
];
pub const PORT_FIELD_IDX: usize = 2;
pub const PASSWORD_FIELD_IDX: usize = 8;

/// Flat string representation of a Host used while editing.
/// `password` is never stored in TOML — it goes to the OS keychain on save.
pub struct EditDraft {
    pub name: String,
    pub hostname: String,
    pub port: String,
    pub username: String,
    pub group: String,
    pub credential_ref: String,
    pub identity_file: String,
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
            identity_file: h.identity_file.clone(),
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
            identity_file: String::new(),
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
            6 => &self.identity_file,
            7 => &self.on_connect,
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
            6 => &mut self.identity_file,
            7 => &mut self.on_connect,
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
    /// Confirmation prompt before deleting a host.
    ConfirmDelete {
        /// Index into `App.hosts` of the host to delete.
        idx: usize,
        /// Host name, shown in the prompt.
        name: String,
    },
    /// The macro manager (Ctrl+G): list / add / edit / delete global macros. All of
    /// its nested screens live in one `MacroState` so the whole subsystem is one mode.
    Macros(MacroState),
}

/// State of the macro manager. `macros` is a working copy of the global macros; it
/// is written back to `App.automations` and persisted only when a macro is saved or
/// deleted.
pub struct MacroState {
    pub macros: Vec<Macro>,
    pub list_selected: usize,
    pub screen: MacroScreen,
}

pub enum MacroScreen {
    /// The macro list.
    List,
    /// Add (`original_idx = None`) or edit an existing macro.
    Edit(Box<MacroEdit>),
    /// Confirm deletion of the macro at this index.
    ConfirmDeleteMacro(usize),
}

/// Which row of the macro edit form has focus.
#[derive(Clone, Copy, PartialEq)]
pub enum MacroFocus {
    Key,
    /// The `n`-th command line of `send`.
    Cmd(usize),
    /// The `n`-th expect rule (summary row).
    Expect(usize),
}

/// Draft of one macro being added/edited. `send` is edited as a list of single-line
/// command rows (each row = one command, matching the runtime's line-per-command
/// semantics); they are joined with `\n` on save.
pub struct MacroEdit {
    pub original_idx: Option<usize>,
    pub key: String,
    pub cmd_lines: Vec<String>,
    pub expects: Vec<Expect>,
    pub focus: MacroFocus,
    /// Char caret within the focused single-line field (Key or a command line).
    pub cursor: usize,
    /// `Some` while adding/editing one of this macro's expect rules.
    pub sub: Option<ExpectEdit>,
}

/// Draft of one expect rule being added/edited within a macro.
pub struct ExpectEdit {
    pub original_idx: Option<usize>,
    pub pattern: String,
    pub send: String,
    pub send_credential: String,
    pub once: bool,
    /// 0 = pattern, 1 = send, 2 = credential, 3 = once.
    pub focus: usize,
    pub cursor: usize,
}

impl MacroEdit {
    fn new_blank() -> Self {
        Self {
            original_idx: None,
            key: String::new(),
            cmd_lines: vec![String::new()],
            expects: Vec::new(),
            focus: MacroFocus::Key,
            cursor: 0,
            sub: None,
        }
    }

    fn from_macro(idx: usize, m: &Macro) -> Self {
        let cmd_lines = if m.send.is_empty() {
            vec![String::new()]
        } else {
            m.send.split('\n').map(|s| s.to_string()).collect()
        };
        Self {
            original_idx: Some(idx),
            key: m.key.clone(),
            cmd_lines,
            expects: m.expects.clone(),
            focus: MacroFocus::Key,
            cursor: m.key.chars().count(),
            sub: None,
        }
    }

    /// The ordered list of focusable rows: Key, each command line, each expect.
    pub fn focus_order(&self) -> Vec<MacroFocus> {
        let mut order = vec![MacroFocus::Key];
        order.extend((0..self.cmd_lines.len()).map(MacroFocus::Cmd));
        order.extend((0..self.expects.len()).map(MacroFocus::Expect));
        order
    }

    /// Text of the focused single-line field (`None` for an expect summary row).
    fn focus_field(&self) -> Option<&str> {
        match self.focus {
            MacroFocus::Key => Some(&self.key),
            MacroFocus::Cmd(i) => self.cmd_lines.get(i).map(String::as_str),
            MacroFocus::Expect(_) => None,
        }
    }

    /// Set focus and park the caret at the end of the newly focused field.
    fn set_focus(&mut self, f: MacroFocus) {
        self.focus = f;
        self.cursor = self.focus_field().map(|s| s.chars().count()).unwrap_or(0);
    }

    fn move_focus(&mut self, forward: bool) {
        let order = self.focus_order();
        let n = order.len();
        let cur = order.iter().position(|&f| f == self.focus).unwrap_or(0);
        let next = if forward { (cur + 1) % n } else { (cur + n - 1) % n };
        self.set_focus(order[next]);
    }
}

impl ExpectEdit {
    fn new_blank() -> Self {
        Self {
            original_idx: None,
            pattern: String::new(),
            send: String::new(),
            send_credential: String::new(),
            once: true,
            focus: 0,
            cursor: 0,
        }
    }

    fn from_expect(idx: usize, e: &Expect) -> Self {
        Self {
            original_idx: Some(idx),
            pattern: e.pattern.clone(),
            send: e.send.clone().unwrap_or_default(),
            send_credential: e.send_credential.clone().unwrap_or_default(),
            once: e.once,
            focus: 0,
            cursor: e.pattern.chars().count(),
        }
    }

    pub fn field(&self, idx: usize) -> &str {
        match idx {
            0 => &self.pattern,
            1 => &self.send,
            2 => &self.send_credential,
            _ => "",
        }
    }

    fn field_len(&self) -> usize {
        self.field(self.focus).chars().count()
    }
}

/// Validate a macro draft and build the `Macro`. `macros` is the current list (for
/// the duplicate-key check). Mirrors the runtime's requirements.
fn build_macro(edit: &MacroEdit, macros: &[Macro]) -> Result<Macro, String> {
    let key = edit.key.trim();
    if key.is_empty() {
        return Err("Key cannot be empty".into());
    }
    let dup = macros
        .iter()
        .enumerate()
        .any(|(i, m)| m.key == key && Some(i) != edit.original_idx);
    if dup {
        return Err(format!("A macro with key '{key}' already exists"));
    }
    // Drop trailing blank command lines, then require at least one real command.
    let mut lines = edit.cmd_lines.clone();
    while lines.len() > 1 && lines.last().is_some_and(|s| s.trim().is_empty()) {
        lines.pop();
    }
    if lines.iter().all(|l| l.trim().is_empty()) {
        return Err("Add at least one command line".into());
    }
    Ok(Macro {
        key: key.to_string(),
        send: lines.join("\n"),
        expects: edit.expects.clone(),
    })
}

/// Validate an expect draft and build the `Expect`. Mirrors
/// `ssh::client::build_single_automation`: valid regex and exactly one of
/// send / send_credential.
fn build_expect(edit: &ExpectEdit) -> Result<Expect, String> {
    if edit.pattern.trim().is_empty() {
        return Err("Pattern cannot be empty".into());
    }
    regex::Regex::new(&edit.pattern).map_err(|e| format!("Invalid regex: {e}"))?;
    let has_send = !edit.send.is_empty();
    let has_cred = !edit.send_credential.is_empty();
    if has_send == has_cred {
        return Err("Set exactly one of Send / Credential".into());
    }
    Ok(Expect {
        pattern: edit.pattern.clone(),
        send: has_send.then(|| edit.send.clone()),
        send_credential: has_cred.then(|| edit.send_credential.clone()),
        once: edit.once,
    })
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
    /// Global macros; the macro manager (Ctrl+G) edits these and persists them.
    pub automations: Automations,
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
    pub fn new(hosts: Vec<Host>, groups: Vec<Group>, automations: Automations) -> Self {
        let mut app = Self {
            hosts,
            groups,
            automations,
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
            AppMode::ConfirmDelete { .. } => self.update_confirm_delete(key),
            AppMode::Macros(_) => self.update_macros(key),
        }
    }

    fn update_normal(&mut self, key: KeyEvent) {
        // Status messages are transient: clear any standing message on the next
        // keypress so the keybinding hints reappear. A handler below may set a
        // fresh one (e.g. the "clear the search to reorder" warning), which then
        // shows until the following keystroke.
        self.status = None;
        match key.code {
            // Esc quits. `q` is NOT a quit key — the search box is always live, so
            // every printable char (incl. q/Q) must reach the filter.
            KeyCode::Esc => self.should_quit = true,

            // Ctrl+↑/↓ (or Shift+↑/↓) reorder the selected host within its group
            // (persisted). Shift is the macOS-friendly alternative: there Ctrl+↑/↓
            // is captured by Mission Control before reaching the terminal.
            KeyCode::Up
                if key
                    .modifiers
                    .intersects(KeyModifiers::CONTROL | KeyModifiers::SHIFT) =>
            {
                self.move_selected_host(true);
            }
            KeyCode::Down
                if key
                    .modifiers
                    .intersects(KeyModifiers::CONTROL | KeyModifiers::SHIFT) =>
            {
                self.move_selected_host(false);
            }

            KeyCode::Down => {
                if !self.rows.is_empty() {
                    self.selected = (self.selected + 1).min(self.rows.len() - 1);
                }
            }
            KeyCode::Up => {
                self.selected = self.selected.saturating_sub(1);
            }

            // Ctrl+D deletes the selected host (with confirmation).
            KeyCode::Char('d') if key.modifiers == KeyModifiers::CONTROL => {
                if let Some(idx) = self.selected_host_idx() {
                    self.mode = AppMode::ConfirmDelete {
                        idx,
                        name: self.hosts[idx].name.clone(),
                    };
                }
            }

            KeyCode::Char('k') if key.modifiers == KeyModifiers::CONTROL => {
                self.mode = AppMode::Credential {
                    reference: String::new(),
                    password: String::new(),
                    focused: 0,
                    cursor: 0,
                };
            }

            // Ctrl+G opens the macro manager (list / add / edit / delete global macros).
            KeyCode::Char('g') if key.modifiers == KeyModifiers::CONTROL => {
                self.mode = AppMode::Macros(MacroState {
                    macros: self.automations.macros.clone(),
                    list_selected: 0,
                    screen: MacroScreen::List,
                });
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

    fn update_confirm_delete(&mut self, key: KeyEvent) {
        let AppMode::ConfirmDelete { idx, .. } = self.mode else {
            return;
        };
        match key.code {
            KeyCode::Char('y') | KeyCode::Char('Y') | KeyCode::Enter => {
                self.delete_host(idx);
                self.mode = AppMode::Normal;
            }
            KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc => {
                self.mode = AppMode::Normal;
            }
            _ => {}
        }
    }

    /// Remove the host at `idx`, persist, and rebuild the list.
    fn delete_host(&mut self, idx: usize) {
        if idx >= self.hosts.len() {
            return;
        }
        let name = self.hosts.remove(idx).name;
        if let Err(e) = crate::config::save_hosts(&self.hosts, &self.groups) {
            self.set_status(format!("Save failed: {e}"));
        } else {
            self.set_status(format!("Deleted host {name}"));
        }
        self.apply_filter();
    }

    /// Move the selected host one slot up/down within its own group (and persist).
    /// Reordering is disabled while a search filter is active, since the displayed
    /// order is then by relevance rather than stored order.
    fn move_selected_host(&mut self, up: bool) {
        if !self.filter.is_empty() {
            self.set_status("Clear the search to reorder hosts".into());
            return;
        }
        let Some(cur) = self.selected_host_idx() else {
            return;
        };
        let group = self.hosts[cur].group.clone();
        // Nearest host above/below that shares the same group.
        let target = if up {
            (0..cur).rev().find(|&j| self.hosts[j].group == group)
        } else {
            ((cur + 1)..self.hosts.len()).find(|&j| self.hosts[j].group == group)
        };
        let Some(t) = target else {
            return;
        };
        self.hosts.swap(cur, t);
        if let Err(e) = crate::config::save_hosts(&self.hosts, &self.groups) {
            self.set_status(format!("Save failed: {e}"));
        }
        self.apply_filter();
        // Keep the selection on the moved host (now at index `t`).
        if let Some(pos) = self
            .rows
            .iter()
            .position(|r| matches!(r, Row::Host { idx, .. } if *idx == t))
        {
            self.selected = pos;
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
                    identity_file: draft.identity_file.trim().to_string(),
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

    /// Handle a key in the macro manager (list, macro edit form, expect sub-form,
    /// delete confirm). All state mutations happen while `state` is borrowed;
    /// whole-`self` side effects (persisting, closing) are deferred to after the
    /// borrow via the locals below.
    fn update_macros(&mut self, key: KeyEvent) {
        self.status = None;
        let ctrl = key.modifiers == KeyModifiers::CONTROL;
        // Deferred actions (applied after the `state` borrow is released).
        let mut close = false;
        let mut persist = false;
        let mut status_msg: Option<String> = None;

        {
            let AppMode::Macros(state) = &mut self.mode else {
                return;
            };
            // Screen transition to apply after the match (avoids reassigning the
            // matched place while it is borrowed).
            let mut next_screen: Option<MacroScreen> = None;

            match &mut state.screen {
                MacroScreen::List => match key.code {
                    KeyCode::Esc => close = true,
                    KeyCode::Up => {
                        state.list_selected = state.list_selected.saturating_sub(1);
                    }
                    KeyCode::Down => {
                        if !state.macros.is_empty() {
                            state.list_selected =
                                (state.list_selected + 1).min(state.macros.len() - 1);
                        }
                    }
                    KeyCode::Char('n') if ctrl => {
                        next_screen = Some(MacroScreen::Edit(Box::new(MacroEdit::new_blank())));
                    }
                    KeyCode::Char('d') if ctrl => {
                        if !state.macros.is_empty() {
                            next_screen =
                                Some(MacroScreen::ConfirmDeleteMacro(state.list_selected));
                        }
                    }
                    KeyCode::Enter | KeyCode::Char('e')
                        if key.code == KeyCode::Enter || ctrl =>
                    {
                        if let Some(m) = state.macros.get(state.list_selected) {
                            next_screen = Some(MacroScreen::Edit(Box::new(
                                MacroEdit::from_macro(state.list_selected, m),
                            )));
                        }
                    }
                    _ => {}
                },

                MacroScreen::ConfirmDeleteMacro(i) => {
                    let i = *i;
                    match key.code {
                        KeyCode::Char('y') | KeyCode::Char('Y') | KeyCode::Enter => {
                            if i < state.macros.len() {
                                state.macros.remove(i);
                            }
                            if state.list_selected >= state.macros.len() {
                                state.list_selected = state.macros.len().saturating_sub(1);
                            }
                            persist = true;
                            status_msg = Some("Macro deleted".into());
                            next_screen = Some(MacroScreen::List);
                        }
                        KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc => {
                            next_screen = Some(MacroScreen::List);
                        }
                        _ => {}
                    }
                }

                MacroScreen::Edit(edit) => {
                    if let Some(sub) = &mut edit.sub {
                        // ── Expect sub-form ──
                        match key.code {
                            KeyCode::Esc => edit.sub = None,
                            KeyCode::Tab => {
                                sub.focus = (sub.focus + 1) % 4;
                                sub.cursor = sub.field_len();
                            }
                            KeyCode::BackTab => {
                                sub.focus = (sub.focus + 3) % 4;
                                sub.cursor = sub.field_len();
                            }
                            KeyCode::Char(' ') if sub.focus == 3 => sub.once = !sub.once,
                            KeyCode::Enter | KeyCode::Char('s') if key.code == KeyCode::Enter || ctrl => {
                                match build_expect(sub) {
                                    Ok(exp) => {
                                        match sub.original_idx {
                                            Some(i) if i < edit.expects.len() => {
                                                edit.expects[i] = exp;
                                            }
                                            _ => edit.expects.push(exp),
                                        }
                                        edit.sub = None;
                                    }
                                    Err(e) => status_msg = Some(e),
                                }
                            }
                            _ => {
                                // Inlined (not `field_mut`) so the field and
                                // `cursor` are seen as disjoint borrows.
                                let field: Option<&mut String> = match sub.focus {
                                    0 => Some(&mut sub.pattern),
                                    1 => Some(&mut sub.send),
                                    2 => Some(&mut sub.send_credential),
                                    _ => None,
                                };
                                if let Some(field) = field {
                                    apply_edit_key(field, &mut sub.cursor, key);
                                }
                            }
                        }
                    } else {
                        // ── Macro edit form ──
                        match key.code {
                            KeyCode::Esc => next_screen = Some(MacroScreen::List),
                            KeyCode::Char('s') if ctrl => {
                                match build_macro(edit, &state.macros) {
                                    Ok(m) => {
                                        match edit.original_idx {
                                            Some(i) if i < state.macros.len() => {
                                                state.macros[i] = m;
                                            }
                                            _ => state.macros.push(m),
                                        }
                                        persist = true;
                                        status_msg = Some("Macro saved".into());
                                        next_screen = Some(MacroScreen::List);
                                    }
                                    Err(e) => status_msg = Some(e),
                                }
                            }
                            // Ctrl+X adds a new expect rule.
                            KeyCode::Char('x') if ctrl => {
                                edit.sub = Some(ExpectEdit::new_blank());
                            }
                            KeyCode::Tab | KeyCode::Down => edit.move_focus(true),
                            KeyCode::BackTab | KeyCode::Up => edit.move_focus(false),
                            // Ctrl+E / Enter on an expect row edits it.
                            KeyCode::Char('e') if ctrl => {
                                if let MacroFocus::Expect(i) = edit.focus
                                    && let Some(e) = edit.expects.get(i)
                                {
                                    edit.sub = Some(ExpectEdit::from_expect(i, e));
                                }
                            }
                            // Ctrl+D deletes the focused command line or expect.
                            KeyCode::Char('d') if ctrl => match edit.focus {
                                MacroFocus::Cmd(i) => {
                                    if edit.cmd_lines.len() > 1 {
                                        edit.cmd_lines.remove(i);
                                        edit.set_focus(MacroFocus::Cmd(
                                            i.min(edit.cmd_lines.len() - 1),
                                        ));
                                    } else {
                                        edit.cmd_lines[0].clear();
                                        edit.set_focus(MacroFocus::Cmd(0));
                                    }
                                }
                                MacroFocus::Expect(i) => {
                                    if i < edit.expects.len() {
                                        edit.expects.remove(i);
                                    }
                                    if edit.expects.is_empty() {
                                        edit.set_focus(MacroFocus::Key);
                                    } else {
                                        edit.set_focus(MacroFocus::Expect(
                                            i.min(edit.expects.len() - 1),
                                        ));
                                    }
                                }
                                MacroFocus::Key => {}
                            },
                            KeyCode::Enter => match edit.focus {
                                MacroFocus::Key => edit.set_focus(MacroFocus::Cmd(0)),
                                MacroFocus::Cmd(i) => {
                                    edit.cmd_lines.insert(i + 1, String::new());
                                    edit.set_focus(MacroFocus::Cmd(i + 1));
                                }
                                MacroFocus::Expect(i) => {
                                    if let Some(e) = edit.expects.get(i) {
                                        edit.sub = Some(ExpectEdit::from_expect(i, e));
                                    }
                                }
                            },
                            // Backspace on an empty command line removes that line.
                            KeyCode::Backspace
                                if matches!(edit.focus, MacroFocus::Cmd(_))
                                    && edit.cursor == 0 =>
                            {
                                if let MacroFocus::Cmd(i) = edit.focus
                                    && edit.cmd_lines.len() > 1
                                    && edit.cmd_lines[i].is_empty()
                                {
                                    edit.cmd_lines.remove(i);
                                    edit.set_focus(MacroFocus::Cmd(i.saturating_sub(1)));
                                }
                            }
                            _ => {
                                // Inlined (not `focus_field_mut`) so the field and
                                // `cursor` are seen as disjoint borrows.
                                let field: Option<&mut String> = match edit.focus {
                                    MacroFocus::Key => Some(&mut edit.key),
                                    MacroFocus::Cmd(i) => edit.cmd_lines.get_mut(i),
                                    MacroFocus::Expect(_) => None,
                                };
                                if let Some(field) = field {
                                    apply_edit_key(field, &mut edit.cursor, key);
                                }
                            }
                        }
                    }
                }
            }

            if let Some(s) = next_screen {
                state.screen = s;
            }
        }

        // ── Deferred whole-`self` side effects ──
        if persist {
            if let AppMode::Macros(state) = &self.mode {
                self.automations.macros = state.macros.clone();
            }
            if let Err(e) = crate::config::save_automations(&self.automations) {
                status_msg = Some(format!("Save failed: {e}"));
            }
        }
        if let Some(m) = status_msg {
            self.set_status(m);
        }
        if close {
            self.mode = AppMode::Normal;
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

#[cfg(test)]
mod macro_tests {
    use super::*;

    fn macro_edit(key: &str, lines: &[&str], expects: Vec<Expect>) -> MacroEdit {
        MacroEdit {
            original_idx: None,
            key: key.to_string(),
            cmd_lines: lines.iter().map(|s| s.to_string()).collect(),
            expects,
            focus: MacroFocus::Key,
            cursor: 0,
            sub: None,
        }
    }

    #[test]
    fn multiline_macro_with_expect_roundtrips_through_toml() {
        let edit = macro_edit(
            "kd",
            &["switchport mode access", "spanning-tree portfast", ""],
            vec![Expect {
                pattern: "[Pp]assword:".into(),
                send: None,
                send_credential: Some("en1".into()),
                once: false,
            }],
        );
        let m = build_macro(&edit, &[]).expect("valid macro");
        // Trailing blank command line is dropped; interior lines joined with `\n`.
        assert_eq!(m.send, "switchport mode access\nspanning-tree portfast");

        let autos = Automations { macros: vec![m] };
        let serialized = toml::to_string(&autos).expect("serialize");
        let back: Automations = toml::from_str(&serialized).expect("parse");
        assert_eq!(back.macros.len(), 1);
        let bm = &back.macros[0];
        assert_eq!(bm.key, "kd");
        assert_eq!(bm.send, "switchport mode access\nspanning-tree portfast");
        assert_eq!(bm.expects.len(), 1);
        assert_eq!(bm.expects[0].send_credential.as_deref(), Some("en1"));
        assert_eq!(bm.expects[0].send, None);
        assert!(!bm.expects[0].once);
    }

    #[test]
    fn build_macro_rejects_empty_key_blank_body_and_duplicates() {
        assert!(build_macro(&macro_edit("", &["x"], vec![]), &[]).is_err());
        assert!(build_macro(&macro_edit("k", &["", "  "], vec![]), &[]).is_err());
        let existing = vec![Macro {
            key: "en".into(),
            send: "enable".into(),
            expects: vec![],
        }];
        assert!(build_macro(&macro_edit("en", &["x"], vec![]), &existing).is_err());
        // Editing the same macro in place (matching original_idx) is not a dup.
        let mut edit = macro_edit("en", &["enable"], vec![]);
        edit.original_idx = Some(0);
        assert!(build_macro(&edit, &existing).is_ok());
    }

    #[test]
    fn build_expect_requires_exactly_one_target_and_valid_regex() {
        let mut e = ExpectEdit::new_blank();
        e.pattern = "[Pp]assword:".into();
        assert!(build_expect(&e).is_err()); // neither send nor credential
        e.send = "a".into();
        e.send_credential = "b".into();
        assert!(build_expect(&e).is_err()); // both
        e.send_credential.clear();
        assert!(build_expect(&e).is_ok()); // only send
        // Invalid regex is rejected.
        let mut bad = ExpectEdit::new_blank();
        bad.pattern = "[".into();
        bad.send = "x".into();
        assert!(build_expect(&bad).is_err());
    }
}
