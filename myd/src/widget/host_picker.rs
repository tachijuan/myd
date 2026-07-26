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

/// Screen rows each host entry occupies: its label, then its target.
///
/// The target gets its own line so a long `user@host:port/path` has room to be
/// read; beside the label it was cut off at the border. Exactly two rows — the
/// box height is reserved from this number, so an entry that rendered a third
/// row would push the hints outside the border.
const ROWS_PER_HOST: u16 = 2;

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

    /// Move the cursor to the entry drawn at screen row `y`, if any.
    ///
    /// Used for mouse clicks; returns whether an entry was hit. Each host
    /// occupies two rows (label, then its wrapped target), so a click on either
    /// selects that host.
    pub fn click_row(&mut self, area: Rect, y: u16) -> bool {
        let first = self.list_origin(area);
        if y < first {
            return false;
        }
        let idx = self.scroll + ((y - first) / ROWS_PER_HOST) as usize;
        if idx < self.visible_len() {
            self.cursor = idx;
            return true;
        }
        false
    }

    /// The first screen row a list entry can occupy inside `area`.
    fn list_origin(&self, area: Rect) -> u16 {
        // Border, then the search line when one is shown. The title shares the
        // border row, so it costs no extra line.
        area.y + 1 + if self.shows_search_line() { 1 } else { 0 }
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

        // Wide enough for a realistic user@host:port plus a path, but never
        // wider than the terminal.
        let width = 66u16.min(area.width.saturating_sub(2)).max(20);
        let inner = width.saturating_sub(2) as usize;

        // The hint is the longest fixed string here and wraps on a narrow
        // terminal, so its height has to be known before the box is sized —
        // otherwise it either overflows the border or gets clipped.
        // Hints in priority order, most important first. A narrow panel drops
        // the tail rather than wrapping to a third line or splitting a
        // "key: action" pair across lines — but "Esc: close" is how you get out,
        // so it is pinned and never dropped.
        let (hints, pinned): (&[&str], &str) = match (self.view, self.mode) {
            (_, Mode::Search) => (&["Type to filter", "Enter: keep"], "Esc: clear"),
            (View::Quick, _) => (
                &["Enter: connect", "L: all hosts", "/: search", "a: add", "t: type"],
                "Esc: close",
            ),
            (View::Full, _) => (
                &["Enter: connect", "/: search", "a: add", "e: edit", "d: delete"],
                "Esc: close",
            ),
        };
        // Wrapped to the *text* width, not the panel width: every rendered line
        // is prefixed with a space, so wrapping to `inner` would leave the last
        // column to be cut off by the pad.
        let hint_width = inner.saturating_sub(1).max(1);
        let hint_lines = layout_hints(hints, pinned, hint_width, 2);


        // Fixed cost: two border rows, the blank separator, the hint, and the
        // search line when shown. Whatever is left is what the list can use — so
        // on a short terminal the list shrinks and scrolls instead of the box
        // being clipped and losing its bottom border and hint.
        let search_line = if self.shows_search_line() { 1 } else { 0 };
        let chrome = 2 + 1 + hint_lines.len() as u16 + search_line;
        let rows_available = area.height.saturating_sub(chrome) / ROWS_PER_HOST;
        let rows = (self.visible_len().clamp(1, 10) as u16)
            .min(rows_available.max(1)) as usize;

        let height = ((rows as u16) * ROWS_PER_HOST + chrome).min(area.height);
        let center = centered(Rect::new(0, 0, width, height), area);

        frame.render_widget(Clear, center);

        // A solid dark fill behind everything. Without it the panel inherits
        // whatever the tree drew underneath, which is what made the text hard to
        // read — foreground colours were being chosen against an unknown
        // background.
        let bg = Color::Rgb(24, 24, 34);
        let mut lines: Vec<Line> = Vec::new();

        if self.shows_search_line() {
            let cursor = if self.mode == Mode::Search { "█" } else { "" };
            lines.push(Line::from(vec![
                Span::styled(
                    " / ",
                    Style::default()
                        .fg(Color::Black)
                        .bg(Color::Yellow)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(
                    format!(" {}{}", self.query, cursor),
                    Style::default().fg(Color::White).bg(bg).add_modifier(Modifier::BOLD),
                ),
                Span::styled(
                    format!("   {} match(es)", self.visible_len()),
                    Style::default().fg(Color::Rgb(150, 150, 170)).bg(bg),
                ),
            ]));
        }

        if self.visible.is_empty() {
            let msg = if self.hosts.is_empty() {
                "No saved hosts yet. Press 'a' to add one, or 't' to type an address."
            } else {
                "Nothing matches that filter."
            };
            for l in wrap_text(msg, inner) {
                lines.push(Line::from(Span::styled(
                    l,
                    Style::default().fg(Color::Rgb(190, 190, 205)).bg(bg),
                )));
            }
        } else {
            let visible_rows = rows;
            if self.cursor < self.scroll {
                self.scroll = self.cursor;
            } else if self.cursor >= self.scroll + visible_rows {
                self.scroll = self.cursor + 1 - visible_rows;
            }

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
                    View::Quick => format!(" {}. ", row + 1),
                    View::Full => if selected { " ▸ ".into() } else { "   ".to_string() },
                };

                // High contrast either way: the cursor row is black on bright
                // cyan, the rest near-white on the panel fill. Both comfortably
                // exceed the old DarkGray-on-unknown-background.
                let (row_style, dim_style) = if selected {
                    let s = Style::default()
                        .fg(Color::Black)
                        .bg(Color::Rgb(80, 200, 235))
                        .add_modifier(Modifier::BOLD);
                    (s, s)
                } else {
                    (
                        Style::default()
                            .fg(Color::Rgb(235, 235, 245))
                            .bg(bg)
                            .add_modifier(Modifier::BOLD),
                        Style::default().fg(Color::Rgb(155, 155, 175)).bg(bg),
                    )
                };

                let uses = if h.uses > 0 {
                    format!(" ({}x)", h.uses)
                } else {
                    String::new()
                };

                // Line one: the label, padded so the list reads as a column.
                let head = format!("{}{}{}", lead, h.label, uses);
                lines.push(Line::from(Span::styled(
                    pad_to(&head, inner),
                    row_style,
                )));

                // Line two: the target on its own line, which is what gives a
                // long `user@host:port/path` room to be read at all — beside the
                // label it was truncated at the border.
                //
                // Exactly one line, because the height reserved above is
                // `ROWS_PER_HOST` per entry; letting a long target wrap onto a
                // third row overflowed the box and pushed the hints out of it.
                // A target too long even for a full line is elided in the
                // middle, keeping the user and the leaf directory — the two ends
                // that identify it — rather than losing the tail.
                let target = h.target_display();
                let target_width = inner.saturating_sub(5).max(8);
                lines.push(Line::from(Span::styled(
                    pad_to(&format!("     {}", elide_middle(&target, target_width)), inner),
                    dim_style,
                )));
            }
        }

        lines.push(Line::from(Span::styled(pad_to("", inner), Style::default().bg(bg))));
        for l in hint_lines {
            lines.push(Line::from(Span::styled(
                pad_to(&format!(" {}", l), inner),
                Style::default().fg(Color::Rgb(150, 150, 170)).bg(bg),
            )));
        }

        let paragraph = Paragraph::new(lines)
            // The fill also covers the border row and any slack below the
            // content, so the panel reads as one solid surface rather than
            // letting the tree show through around the edges.
            .style(Style::default().bg(bg))
            .block(
                Block::default()
                    .title(Span::styled(
                        title,
                        Style::default()
                            .fg(Color::Rgb(120, 220, 255))
                            .add_modifier(Modifier::BOLD),
                    ))
                    .borders(Borders::ALL)
                    .border_type(BorderType::Rounded)
                    .border_style(Style::default().fg(Color::Rgb(120, 220, 255)))
                    .style(Style::default().bg(bg)),
            );
        frame.render_widget(paragraph, center);
    }
}

/// Pad `s` with spaces to `width` columns so a row's background fill spans the
/// whole panel. Without this the highlight stops at the end of the text and the
/// selected row reads as a ragged fragment.
fn pad_to(s: &str, width: usize) -> String {
    let len = s.chars().count();
    if len >= width {
        return s.chars().take(width).collect();
    }
    let mut out = s.to_string();
    out.extend(std::iter::repeat_n(' ', width - len));
    out
}

/// Shorten `s` to `width` by removing the middle.
///
/// A connection target is identified by its two ends — the user and host at the
/// front, the leaf directory at the back — so trimming the tail (as a plain
/// truncation does) throws away half of what distinguishes one entry from
/// another.
fn elide_middle(s: &str, width: usize) -> String {
    let chars: Vec<char> = s.chars().collect();
    if chars.len() <= width {
        return s.to_string();
    }
    if width <= 1 {
        return "…".to_string();
    }
    // Favour the front, which carries user@host.
    let keep = width - 1;
    let head = keep.div_ceil(2);
    let tail = keep - head;
    let mut out: String = chars[..head].iter().collect();
    out.push('…');
    out.extend(chars[chars.len() - tail..].iter());
    out
}

/// Wrap `text` to `width` columns, breaking at spaces where possible.
///
/// Counts chars rather than bytes so multibyte hostnames wrap at the right
/// column. A single word longer than the line — a long `user@host:port/path`
/// with no spaces, which is the common case here — is hard-split so it stays
/// fully readable instead of being truncated at the border.
fn wrap_text(text: &str, width: usize) -> Vec<String> {
    wrap_on(text, width, char::is_whitespace)
}

/// Lay out key hints across at most `max_lines` lines of `width` columns.
///
/// Each hint is a "key: action" pair and is kept whole — breaking one leaves
/// "Esc:" on one line and "close" on the next, reading as two unrelated things.
/// (A non-breaking space cannot achieve this: Rust's `split_whitespace` treats
/// U+00A0 as whitespace like any other, which is why a first attempt at it had
/// no effect.)
///
/// `pinned` is always placed, even if every other hint has to be dropped: it is
/// the one that says how to get out of the panel, and a user who cannot see the
/// rest still needs it.
fn layout_hints(hints: &[&str], pinned: &str, width: usize, max_lines: usize) -> Vec<String> {
    // Reserve room for the pinned hint up front. Trying to append it last and
    // falling back on failure went wrong in the obvious way: the optional hints
    // filled every line first, leaving nowhere to put it.
    let pinned_len = pinned.chars().count();
    let mut lines: Vec<String> = Vec::new();
    let mut current = String::new();

    for h in hints {
        let hlen = h.chars().count();
        let need = if current.is_empty() {
            hlen
        } else {
            current.chars().count() + 2 + hlen
        };

        // On the final line, keep enough space for the pinned hint.
        let on_last_line = lines.len() + 1 == max_lines;
        let budget = if on_last_line {
            width.saturating_sub(pinned_len + 2)
        } else {
            width
        };

        if need <= budget {
            if !current.is_empty() {
                current.push_str("  ");
            }
            current.push_str(h);
        } else if !on_last_line && hlen <= width {
            lines.push(std::mem::take(&mut current));
            current.push_str(h);
        } else {
            // Out of room. Stop rather than skipping ahead: the list is in
            // priority order, so a gap would show a less useful hint in place of
            // a more useful one.
            break;
        }
    }

    if current.is_empty() {
        current = pinned.to_string();
    } else if current.chars().count() + 2 + pinned_len <= width {
        current.push_str("  ");
        current.push_str(pinned);
    } else if lines.len() + 1 < max_lines {
        lines.push(std::mem::take(&mut current));
        current = pinned.to_string();
    } else {
        // Truncate the optional hints to make room; the way out always shows.
        let room = width.saturating_sub(pinned_len + 2);
        let kept: String = current.chars().take(room).collect();
        current = format!("{}  {}", kept.trim_end(), pinned);
    }
    lines.push(current);
    lines
}

/// Wrap `text` to `width`, breaking only where `is_break` allows a split.
fn wrap_on(text: &str, width: usize, is_break: impl Fn(char) -> bool) -> Vec<String> {
    if width == 0 {
        return vec![String::new()];
    }
    let words: Vec<&str> = text.split(&is_break).filter(|s| !s.is_empty()).collect();
    let mut lines = Vec::new();
    let mut current = String::new();
    let mut len = 0usize;
    for word in words {
        let wlen = word.chars().count();
        if wlen > width {
            if len > 0 {
                lines.push(std::mem::take(&mut current));
                len = 0;
            }
            let mut chunk = String::new();
            for ch in word.chars() {
                if chunk.chars().count() == width {
                    lines.push(std::mem::take(&mut chunk));
                }
                chunk.push(ch);
            }
            if !chunk.is_empty() {
                current = chunk;
                len = current.chars().count();
            }
            continue;
        }
        let need = if len == 0 { wlen } else { len + 1 + wlen };
        if need > width {
            lines.push(std::mem::take(&mut current));
            current.push_str(word);
            len = wlen;
        } else {
            if len > 0 {
                current.push(' ');
            }
            current.push_str(word);
            len = need;
        }
    }
    if !current.is_empty() || lines.is_empty() {
        lines.push(current);
    }
    lines
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
        // One entry whose target cannot fit on a line at any realistic width.
        // Layout bugs only show up on the entries that overflow, so the fixture
        // has to contain one.
        let mut long = SavedHost::new("long", "a-very-long-hostname-indeed.frankfurt.example.com");
        long.user = Some("deployment-service".into());
        long.port = Some(2222);
        long.path = Some("/srv/applications/releases/current".into());
        long.uses = 2;
        hosts.push(long);
        HostCatalog::in_memory(hosts)
    }

    #[test]
    fn hints_always_keep_the_way_out() {
        let hints = ["Enter: connect", "/: search", "a: add", "e: edit", "d: delete"];
        for w in [20usize, 30, 37, 43, 57, 80] {
            let out = layout_hints(&hints, "Esc: close", w, 2);
            assert!(out.len() <= 2, "w={w}: {out:?}");
            for l in &out {
                assert!(l.chars().count() <= w, "w={w} overflowed: {l:?}");
            }
            assert!(
                out.iter().any(|l| l.contains("Esc: close")),
                "w={w}: the way out was dropped: {out:?}"
            );
        }
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

        for c in "fr.exam".chars() {
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
        assert_eq!(
            p.visible_len(),
            catalog().len(),
            "the full list should be back"
        );
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
        for c in "fr.exam".chars() {
            p.handle_key(key(c));
        }
        p.handle_key(code(KeyCode::Enter));

        p.reload(&catalog());
        assert_eq!(p.visible_len(), 1, "filter was lost across a reload");
        assert_eq!(p.selected().unwrap().label, "france");
    }

    /// The panel must stay inside its border at any size.
    ///
    /// Both failures here were real: a long target wrapped onto a third row and
    /// pushed the hints past the bottom border, and the hint line was cut at the
    /// border instead of wrapping.
    #[test]
    fn the_box_always_closes_and_keeps_its_hints() {
        let c = catalog();
        for full in [false, true] {
            for (w, h) in [(90u16, 24u16), (60, 16), (46, 20), (40, 12), (30, 10)] {
                let mut p = if full {
                    HostPicker::full(&c)
                } else {
                    HostPicker::quick(&c)
                };
                let backend = ratatui::backend::TestBackend::new(w, h);
                let mut term = ratatui::Terminal::new(backend).unwrap();
                term.draw(|f| p.render(f, f.area())).unwrap();

                let buf = term.backend().buffer().clone();
                let rows: Vec<String> = (0..buf.area.height)
                    .map(|y| {
                        (0..buf.area.width)
                            .map(|x| buf[(x, y)].symbol().to_string())
                            .collect()
                    })
                    .collect();

                // The bottom border must be drawn: if the content overflowed, it
                // is missing and the panel bleeds into the tree behind it.
                let has_bottom = rows.iter().any(|r| r.contains('╰'));
                assert!(
                    has_bottom,
                    "{}x{} full={}: the box lost its bottom border\n{}",
                    w, h, full, rows.join("\n")
                );

                // And the way out must be visible somewhere in the box.
                let text = rows.join(" ");
                assert!(
                    text.contains("Esc"),
                    "{}x{} full={}: the Esc hint was pushed out\n{}",
                    w, h, full, rows.join("\n")
                );
            }
        }
    }

    #[test]
    fn a_long_target_is_elided_not_wrapped() {
        // Wrapping a target onto a second line overflowed the reserved height.
        let long = "someuser@a-very-long-hostname-indeed.example.com:2222/very/deep/path";
        let out = elide_middle(long, 30);
        assert_eq!(out.chars().count(), 30);
        assert!(out.contains('…'));
        // Both identifying ends survive.
        assert!(out.starts_with("someuser@"), "{out}");
        assert!(out.ends_with("path"), "{out}");
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
