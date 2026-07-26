//! The dialing directory's user interface.
//!
//! Opens compact on `gr` — the three most-used hosts and nothing else, because
//! that is usually the whole answer — and expands to the full list on demand.
//!
//! Implemented as a modal rather than a screen so it receives raw key events
//! before the global keybinding handler. That matters for two reasons: vi
//! navigation and `/` search need `j`, `k` and `/` to mean something local, and
//! the chord detector's 500 ms timeout would otherwise sit between the user and
//! every keystroke. It is also why this picker has real vi navigation while the
//! older directory picker does not — that one consumes every character into an
//! always-on text field, so `j` types a "j" rather than moving.

use ratatui::{
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, BorderType, Borders, Clear, Paragraph},
    Frame,
};

use crate::hosts::{HostCatalog, SavedHost};

/// How many hosts the compact view offers.
pub const QUICK_COUNT: usize = 3;

/// What the picker is doing with keystrokes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// Navigating. `j`/`k` move, `/` starts a search, letters are commands.
    Normal,
    /// Typing a filter. Characters go into the query.
    Search,
}

/// Which list the picker is showing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum View {
    /// The top few by usage, plus a way into everything else.
    Quick,
    /// Every saved host, filterable.
    Full,
}

/// What the picker decided, handed back to the app.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PickerOutcome {
    /// Still open.
    Continue,
    /// Dismissed without connecting.
    Cancelled,
    /// Connect to this URL.
    Connect(String),
    /// Prompt for a target to type by hand.
    PromptManual,
    /// Add a new host.
    AddHost,
    /// Edit this saved host.
    EditHost(String),
    /// Ask before deleting this saved host.
    DeleteHost(String),
}

/// State for one open picker.
pub struct HostPicker {
    hosts: Vec<SavedHost>,
    /// Indices into `hosts` currently on screen, in display order. Filtering
    /// rewrites this rather than the list, so the cursor can always be mapped
    /// back to the right host — selecting the wrong one after a search would be
    /// the obvious bug here.
    visible: Vec<usize>,
    cursor: usize,
    pub mode: Mode,
    pub view: View,
    query: String,
    /// Top row of the visible window, for lists longer than the box.
    scroll: usize,
}

impl HostPicker {
    /// Open on the most-used few.
    pub fn quick(catalog: &HostCatalog) -> Self {
        let hosts: Vec<SavedHost> = catalog.recent(QUICK_COUNT).into_iter().cloned().collect();
        let visible = (0..hosts.len()).collect();
        Self {
            hosts,
            visible,
            cursor: 0,
            mode: Mode::Normal,
            view: View::Quick,
            query: String::new(),
            scroll: 0,
        }
    }

    /// Open on the whole catalog.
    pub fn full(catalog: &HostCatalog) -> Self {
        let mut hosts: Vec<SavedHost> = catalog.hosts().to_vec();
        hosts.sort_by(|a, b| a.label.cmp(&b.label));
        let visible = (0..hosts.len()).collect();
        Self {
            hosts,
            visible,
            cursor: 0,
            mode: Mode::Normal,
            view: View::Full,
            query: String::new(),
            scroll: 0,
        }
    }

    pub fn is_empty(&self) -> bool {
        self.hosts.is_empty()
    }

    pub fn query(&self) -> &str {
        &self.query
    }

    /// The highlighted host, if any.
    pub fn selected(&self) -> Option<&SavedHost> {
        self.visible.get(self.cursor).map(|&i| &self.hosts[i])
    }

    fn visible_len(&self) -> usize {
        self.visible.len()
    }

    /// How many hosts are currently listed, after any filter.
    pub fn visible_count(&self) -> usize {
        self.visible.len()
    }

    /// Recompute the visible set from the query.
    ///
    /// Plain case-insensitive substring matching across every field the user can
    /// see, rather than a regex: this filters as you type, where a half-finished
    /// regex is usually invalid and would make the list flicker empty.
    fn refilter(&mut self) {
        self.visible = if self.query.is_empty() {
            (0..self.hosts.len()).collect()
        } else {
            (0..self.hosts.len())
                .filter(|&i| self.hosts[i].matches(&self.query))
                .collect()
        };
        self.cursor = self.cursor.min(self.visible_len().saturating_sub(1));
        self.scroll = 0;
    }

    pub fn cursor_down(&mut self) {
        let n = self.visible_len();
        if n > 0 {
            self.cursor = (self.cursor + 1) % n;
        }
    }

    pub fn cursor_up(&mut self) {
        let n = self.visible_len();
        if n > 0 {
            self.cursor = if self.cursor == 0 { n - 1 } else { self.cursor - 1 };
        }
    }

    /// Move the cursor to the row at `y` within `area`, if it lands on one.
    ///
    /// Used for mouse clicks; returns whether a row was hit.
    pub fn click_row(&mut self, area: Rect, y: u16) -> bool {
        let first = self.list_origin(area);
        if y < first {
            return false;
        }
        let idx = self.scroll + (y - first) as usize;
        if idx < self.visible_len() {
            self.cursor = idx;
            return true;
        }
        false
    }

    /// The first screen row a list entry can occupy inside `area`.
    fn list_origin(&self, area: Rect) -> u16 {
        // Border, then title, then the search line when one is shown.
        area.y + 2 + if self.shows_search_line() { 1 } else { 0 }
    }

    fn shows_search_line(&self) -> bool {
        self.view == View::Full && (self.mode == Mode::Search || !self.query.is_empty())
    }

    /// Handle a key. The app acts on the returned outcome.
    pub fn handle_key(&mut self, key: crossterm::event::KeyEvent) -> PickerOutcome {
        use crossterm::event::{KeyCode, KeyModifiers};

        // Search mode owns almost every key, so the query can contain letters
        // that are commands in normal mode.
        if self.mode == Mode::Search {
            match key.code {
                KeyCode::Esc => {
                    // Abandon the filter entirely, showing the full list again.
                    self.mode = Mode::Normal;
                    self.query.clear();
                    self.refilter();
                }
                KeyCode::Enter => {
                    // Keep the filter, return to navigating it.
                    self.mode = Mode::Normal;
                }
                KeyCode::Backspace => {
                    self.query.pop();
                    self.refilter();
                }
                KeyCode::Down => self.cursor_down(),
                KeyCode::Up => self.cursor_up(),
                KeyCode::Char(c) => {
                    self.query.push(c);
                    self.refilter();
                }
                _ => {}
            }
            return PickerOutcome::Continue;
        }

        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => PickerOutcome::Cancelled,
            KeyCode::Enter | KeyCode::Char('l') => match self.selected() {
                Some(h) => PickerOutcome::Connect(h.to_url()),
                // An empty catalog should still let you get somewhere.
                None => PickerOutcome::PromptManual,
            },
            KeyCode::Char('j') | KeyCode::Down => {
                self.cursor_down();
                PickerOutcome::Continue
            }
            KeyCode::Char('k') | KeyCode::Up => {
                self.cursor_up();
                PickerOutcome::Continue
            }
            KeyCode::Char('g') | KeyCode::Home => {
                self.cursor = 0;
                PickerOutcome::Continue
            }
            KeyCode::Char('G') | KeyCode::End => {
                self.cursor = self.visible_len().saturating_sub(1);
                PickerOutcome::Continue
            }
            KeyCode::Char('d') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                for _ in 0..10 {
                    self.cursor_down();
                }
                PickerOutcome::Continue
            }
            KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                for _ in 0..10 {
                    self.cursor_up();
                }
                PickerOutcome::Continue
            }
            KeyCode::Char('/') => {
                // Searching a three-item list is pointless; open the full list
                // and search that instead of silently doing nothing.
                self.view = View::Full;
                self.mode = Mode::Search;
                self.query.clear();
                PickerOutcome::Continue
            }
            KeyCode::Char('L') => {
                self.view = View::Full;
                PickerOutcome::Continue
            }
            KeyCode::Char('a') => PickerOutcome::AddHost,
            KeyCode::Char('e') => match self.selected() {
                Some(h) => PickerOutcome::EditHost(h.label.clone()),
                None => PickerOutcome::Continue,
            },
            KeyCode::Char('d') => match self.selected() {
                Some(h) => PickerOutcome::DeleteHost(h.label.clone()),
                None => PickerOutcome::Continue,
            },
            KeyCode::Char('t') => PickerOutcome::PromptManual,
            _ => PickerOutcome::Continue,
        }
    }

    /// Rebuild from the catalog after an add, edit, or delete, keeping the view
    /// and filter the user had set up.
    pub fn reload(&mut self, catalog: &HostCatalog) {
        let keep_view = self.view;
        let keep_query = std::mem::take(&mut self.query);
        let keep_cursor = self.cursor;
        *self = match keep_view {
            View::Quick => Self::quick(catalog),
            View::Full => Self::full(catalog),
        };
        self.query = keep_query;
        self.refilter();
        self.cursor = keep_cursor.min(self.visible_len().saturating_sub(1));
    }

    pub fn render(&mut self, frame: &mut Frame, area: Rect) {
        let title = match self.view {
            View::Quick => " Connect to ".to_string(),
            View::Full => format!(" Saved hosts ({}) ", self.hosts.len()),
        };

        // Wide enough for a realistic user@host:port plus a path.
        let width = 66u16.min(area.width.saturating_sub(2)).max(24);
        let rows = self.visible_len().clamp(1, 12) as u16;
        let hint_lines = 2u16;
        let search_line = if self.shows_search_line() { 1 } else { 0 };
        let height = (rows + hint_lines + search_line + 3).min(area.height);
        let center = centered(Rect::new(0, 0, width, height), area);

        frame.render_widget(Clear, center);

        let mut lines: Vec<Line> = Vec::new();

        if self.shows_search_line() {
            let cursor = if self.mode == Mode::Search { "█" } else { "" };
            lines.push(Line::from(vec![
                Span::styled("/", Style::default().fg(Color::Yellow)),
                Span::styled(
                    format!("{}{}", self.query, cursor),
                    Style::default().fg(Color::White),
                ),
                Span::styled(
                    format!("   {} match(es)", self.visible_len()),
                    Style::default().fg(Color::DarkGray),
                ),
            ]));
        }

        if self.visible.is_empty() {
            let msg = if self.hosts.is_empty() {
                "No saved hosts yet — press 'a' to add one, or 't' to type an address"
            } else {
                "Nothing matches that filter"
            };
            lines.push(Line::from(Span::styled(
                msg,
                Style::default().fg(Color::DarkGray),
            )));
        } else {
            // Keep the cursor inside the window.
            let visible_rows = rows as usize;
            if self.cursor < self.scroll {
                self.scroll = self.cursor;
            } else if self.cursor >= self.scroll + visible_rows {
                self.scroll = self.cursor + 1 - visible_rows;
            }

            let inner = width.saturating_sub(2) as usize;
            for (row, &host_idx) in self
                .visible
                .iter()
                .enumerate()
                .skip(self.scroll)
                .take(visible_rows)
                .collect::<Vec<_>>()
            {
                let h = &self.hosts[host_idx];
                let selected = row == self.cursor;

                // In the compact view the rows double as shortcuts, so number
                // them; in the full list a number column would just be noise.
                let lead = match self.view {
                    View::Quick => format!("{}. ", row + 1),
                    View::Full => if selected { "> ".into() } else { "  ".to_string() },
                };

                let label = h.label.clone();
                let target = h.target_display();
                let uses = if h.uses > 0 {
                    format!("  ({}x)", h.uses)
                } else {
                    String::new()
                };

                let mut text = format!("{}{:<14} {}{}", lead, label, target, uses);
                if text.chars().count() > inner {
                    text = text.chars().take(inner).collect();
                }

                let style = if selected {
                    Style::default()
                        .fg(Color::Black)
                        .bg(Color::Cyan)
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(Color::White)
                };
                lines.push(Line::from(Span::styled(text, style)));
            }
        }

        lines.push(Line::from(""));
        let hint = match (self.view, self.mode) {
            (_, Mode::Search) => "Type to filter  Enter: keep  Esc: clear",
            (View::Quick, _) => "Enter: connect  L: all hosts  /: search  a: add  t: type  Esc",
            (View::Full, _) => "Enter: connect  /: search  a: add  e: edit  d: delete  Esc",
        };
        lines.push(Line::from(Span::styled(
            hint,
            Style::default().fg(Color::DarkGray),
        )));

        let paragraph = Paragraph::new(lines).block(
            Block::default()
                .title(title)
                .borders(Borders::ALL)
                .border_type(BorderType::Plain)
                .border_style(Style::default().fg(Color::Cyan)),
        );
        frame.render_widget(paragraph, center);
    }
}

/// Center `r` inside `area`, clamped so an oversized box cannot render outside
/// the buffer.
fn centered(r: Rect, area: Rect) -> Rect {
    let w = r.width.min(area.width);
    let h = r.height.min(area.height);
    Rect::new(
        area.x + area.width.saturating_sub(w) / 2,
        area.y + area.height.saturating_sub(h) / 2,
        w,
        h,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    fn key(c: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE)
    }
    fn code(c: KeyCode) -> KeyEvent {
        KeyEvent::new(c, KeyModifiers::NONE)
    }

    fn catalog() -> HostCatalog {
        let mut hosts = Vec::new();
        for (label, host, uses) in [
            ("prod", "prod.example.com", 30u64),
            ("backup", "10.0.0.5", 20),
            ("scratch", "dev.local", 10),
            ("france", "fr.example.com", 5),
            ("archive", "old.example.com", 1),
        ] {
            let mut h = SavedHost::new(label, host);
            h.uses = uses;
            h.user = Some("juan".into());
            hosts.push(h);
        }
        HostCatalog::in_memory(hosts)
    }

    #[test]
    fn quick_view_shows_only_the_top_three() {
        let p = HostPicker::quick(&catalog());
        assert_eq!(p.visible_len(), QUICK_COUNT);
        assert_eq!(p.selected().unwrap().label, "prod");
    }

    #[test]
    fn vi_keys_navigate_rather_than_typing() {
        let mut p = HostPicker::full(&catalog());
        let first = p.selected().unwrap().label.clone();
        assert_eq!(p.handle_key(key('j')), PickerOutcome::Continue);
        assert_ne!(p.selected().unwrap().label, first, "j did not move the cursor");
        p.handle_key(key('k'));
        assert_eq!(p.selected().unwrap().label, first);
        // And the query stays empty — the keys were commands, not text.
        assert!(p.query().is_empty());
    }

    #[test]
    fn g_and_shift_g_jump_to_the_ends() {
        let mut p = HostPicker::full(&catalog());
        p.handle_key(key('G'));
        assert_eq!(p.cursor, p.visible_len() - 1);
        p.handle_key(key('g'));
        assert_eq!(p.cursor, 0);
    }

    #[test]
    fn slash_filters_incrementally_and_maps_back_to_the_right_host() {
        let mut p = HostPicker::full(&catalog());
        p.handle_key(key('/'));
        assert_eq!(p.mode, Mode::Search);

        for c in "fra".chars() {
            p.handle_key(key(c));
        }
        assert_eq!(p.visible_len(), 1, "filter should narrow to one host");

        // The cursor must resolve through the filtered indices, not the raw list.
        let sel = p.selected().unwrap();
        assert_eq!(sel.label, "france");

        // Enter keeps the filter and returns to navigation.
        p.handle_key(code(KeyCode::Enter));
        assert_eq!(p.mode, Mode::Normal);
        assert_eq!(p.visible_len(), 1);

        match p.handle_key(code(KeyCode::Enter)) {
            PickerOutcome::Connect(url) => assert!(url.contains("fr.example.com"), "{url}"),
            other => panic!("expected a connect, got {other:?}"),
        }
    }

    #[test]
    fn esc_in_search_clears_the_filter_but_keeps_the_picker() {
        let mut p = HostPicker::full(&catalog());
        p.handle_key(key('/'));
        p.handle_key(key('z'));
        assert_eq!(p.visible_len(), 0);

        assert_eq!(p.handle_key(code(KeyCode::Esc)), PickerOutcome::Continue);
        assert_eq!(p.mode, Mode::Normal);
        assert!(p.query().is_empty());
        assert_eq!(p.visible_len(), 5, "the full list should be back");
    }

    #[test]
    fn esc_in_normal_mode_dismisses() {
        let mut p = HostPicker::full(&catalog());
        assert_eq!(p.handle_key(code(KeyCode::Esc)), PickerOutcome::Cancelled);
    }

    #[test]
    fn search_letters_are_text_not_commands() {
        let mut p = HostPicker::full(&catalog());
        p.handle_key(key('/'));
        // 'a' would be "add host" in normal mode.
        assert_eq!(p.handle_key(key('a')), PickerOutcome::Continue);
        assert_eq!(p.query(), "a");
    }

    #[test]
    fn management_keys_report_the_selected_host() {
        let mut p = HostPicker::full(&catalog());
        p.handle_key(key('G'));
        let label = p.selected().unwrap().label.clone();
        assert_eq!(p.handle_key(key('e')), PickerOutcome::EditHost(label.clone()));
        assert_eq!(p.handle_key(key('d')), PickerOutcome::DeleteHost(label));
        assert_eq!(p.handle_key(key('a')), PickerOutcome::AddHost);
    }

    #[test]
    fn slash_from_the_quick_view_opens_the_full_list() {
        // Filtering three entries is useless; searching should widen the view.
        let mut p = HostPicker::quick(&catalog());
        p.handle_key(key('/'));
        assert_eq!(p.view, View::Full);
        assert_eq!(p.mode, Mode::Search);
    }

    #[test]
    fn an_empty_catalog_offers_a_manual_target() {
        let mut p = HostPicker::quick(&HostCatalog::in_memory(vec![]));
        assert!(p.is_empty());
        assert_eq!(p.handle_key(code(KeyCode::Enter)), PickerOutcome::PromptManual);
    }

    #[test]
    fn cursor_wraps_in_both_directions() {
        let mut p = HostPicker::full(&catalog());
        let n = p.visible_len();
        p.handle_key(key('k'));
        assert_eq!(p.cursor, n - 1, "k at the top should wrap to the bottom");
        p.handle_key(key('j'));
        assert_eq!(p.cursor, 0);
    }

    #[test]
    fn reload_keeps_the_filter_after_an_edit() {
        let mut p = HostPicker::full(&catalog());
        p.handle_key(key('/'));
        for c in "fra".chars() {
            p.handle_key(key(c));
        }
        p.handle_key(code(KeyCode::Enter));

        p.reload(&catalog());
        assert_eq!(p.visible_len(), 1, "filter was lost across a reload");
        assert_eq!(p.selected().unwrap().label, "france");
    }

    #[test]
    fn renders_without_panicking_in_a_tiny_terminal() {
        // A box larger than the terminal must be clamped, not drawn out of bounds.
        let mut p = HostPicker::full(&catalog());
        for (w, h) in [(80u16, 24u16), (20, 6), (8, 3)] {
            let backend = ratatui::backend::TestBackend::new(w, h);
            let mut term = ratatui::Terminal::new(backend).unwrap();
            term.draw(|f| p.render(f, f.area())).unwrap();
        }
    }
}
