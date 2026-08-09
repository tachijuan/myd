//! A small numbered menu for picking the sort order.
//!
//! Reached by clicking the "Sort:" indicator in the tree's title bar. `s` still
//! cycles through the modes, which is faster once you know the order — this is
//! for choosing directly, and for discovering what the orders are at all.
//!
//! Every entry is numbered, so it can be picked with the mouse or by typing the
//! number without moving the hand from the keyboard. The tree binds those same
//! digits directly, so the menu is where the numbering is learned rather than
//! the only place it works.

use ratatui::{
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, BorderType, Borders, Clear, Paragraph},
    Frame,
};

use crate::screen::SortMode;

/// What the menu decided.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SortMenuOutcome {
    /// Still open.
    Continue,
    /// Dismissed without changing anything.
    Cancelled,
    /// Apply this mode.
    Chosen(SortMode),
}

/// An open sort menu.
pub struct SortMenu {
    /// Which entry is highlighted, for keyboard navigation.
    cursor: usize,
    /// The mode active when the menu opened, marked so the current setting is
    /// obvious.
    current: SortMode,
    /// Where the menu was last drawn, for click hit-testing.
    area: Option<Rect>,
}

impl SortMenu {
    pub fn new(current: SortMode) -> Self {
        // Open on the active mode rather than the top, so Enter is a no-op and
        // the arrow keys start from where the user already is.
        let cursor = SortMode::ALL.iter().position(|m| *m == current).unwrap_or(0);
        Self {
            cursor,
            current,
            area: None,
        }
    }

    pub fn selected(&self) -> SortMode {
        SortMode::ALL[self.cursor.min(SortMode::ALL.len() - 1)]
    }

    pub fn handle_key(&mut self, key: crossterm::event::KeyEvent) -> SortMenuOutcome {
        use crossterm::event::KeyCode;

        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => SortMenuOutcome::Cancelled,
            KeyCode::Enter | KeyCode::Char('l') => SortMenuOutcome::Chosen(self.selected()),
            KeyCode::Char('j') | KeyCode::Down => {
                self.cursor = (self.cursor + 1) % SortMode::ALL.len();
                SortMenuOutcome::Continue
            }
            KeyCode::Char('k') | KeyCode::Up => {
                self.cursor = if self.cursor == 0 {
                    SortMode::ALL.len() - 1
                } else {
                    self.cursor - 1
                };
                SortMenuOutcome::Continue
            }
            // Typing the number picks that entry outright — the whole point of
            // numbering them.
            KeyCode::Char(c) if c.is_ascii_digit() => {
                let n = c.to_digit(10).unwrap_or(0) as usize;
                match n.checked_sub(1).and_then(|i| SortMode::ALL.get(i)) {
                    Some(m) => SortMenuOutcome::Chosen(*m),
                    None => SortMenuOutcome::Continue,
                }
            }
            _ => SortMenuOutcome::Continue,
        }
    }

    /// Handle a click at `(x, y)`.
    ///
    /// A click on an entry chooses it immediately: the menu is a list of
    /// commands, and requiring a double-click to run one would be a step users
    /// don't expect from a menu. A click outside dismisses, as menus do.
    pub fn click_at(&mut self, x: u16, y: u16) -> SortMenuOutcome {
        let Some(area) = self.area else {
            return SortMenuOutcome::Continue;
        };
        let inside = x >= area.x && x < area.x + area.width && y >= area.y && y < area.y + area.height;
        if !inside {
            return SortMenuOutcome::Cancelled;
        }
        // Row 0 is the border and title; entries start below it.
        if y <= area.y {
            return SortMenuOutcome::Continue;
        }
        let idx = (y - area.y - 1) as usize;
        match SortMode::ALL.get(idx) {
            Some(m) => SortMenuOutcome::Chosen(*m),
            None => SortMenuOutcome::Continue,
        }
    }

    /// Draw the menu near `anchor` — the indicator that opened it — falling back
    /// to a centred box when there isn't room beside it.
    pub fn render(&mut self, frame: &mut Frame, full: Rect, anchor: Option<Rect>) {
        let bg = Color::Rgb(24, 24, 34);

        let width = SortMode::ALL
            .iter()
            .map(|m| m.label().chars().count() + m.description().chars().count() + 8)
            .max()
            .unwrap_or(40)
            .min(full.width.saturating_sub(2) as usize)
            .max(20) as u16;
        let height = (SortMode::ALL.len() as u16 + 3).min(full.height);

        // Prefer to open directly under whatever was clicked, like a real menu,
        // but never off-screen.
        let area = match anchor {
            Some(a) => {
                let x = a.x.min(full.x + full.width.saturating_sub(width));
                let y = if a.y + 1 + height <= full.y + full.height {
                    a.y + 1
                } else {
                    // No room below; sit above the indicator instead.
                    a.y.saturating_sub(height)
                };
                Rect::new(x, y, width, height)
            }
            None => Rect::new(
                full.x + full.width.saturating_sub(width) / 2,
                full.y + full.height.saturating_sub(height) / 2,
                width,
                height,
            ),
        };
        self.area = Some(area);

        frame.render_widget(Clear, area);

        let inner = width.saturating_sub(2) as usize;
        let mut lines: Vec<Line> = Vec::new();

        for (i, mode) in SortMode::ALL.iter().enumerate() {
            let selected = i == self.cursor;
            // A dot marks the mode already in effect, so the menu shows the
            // current state rather than only offering changes.
            let mark = if *mode == self.current { "●" } else { " " };
            let text = format!(
                " {} {}. {:<18} {}",
                mark,
                i + 1,
                mode.label(),
                mode.description()
            );
            let style = if selected {
                Style::default()
                    .fg(Color::Black)
                    .bg(Color::Rgb(80, 200, 235))
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(Color::Rgb(235, 235, 245)).bg(bg)
            };
            lines.push(Line::from(Span::styled(pad_to(&text, inner), style)));
        }

        lines.push(Line::from(Span::styled(
            // Derived from ALL rather than written out: this said "1-7" and was
            // silently wrong the moment an eighth mode was added.
            pad_to(
                &format!(
                    " 1-{} / click to pick   Esc: cancel",
                    SortMode::ALL.len()
                ),
                inner,
            ),
            Style::default().fg(Color::Rgb(150, 150, 170)).bg(bg),
        )));

        let paragraph = Paragraph::new(lines).style(Style::default().bg(bg)).block(
            Block::default()
                .title(Span::styled(
                    " Sort by ",
                    Style::default()
                        .fg(Color::Rgb(120, 220, 255))
                        .add_modifier(Modifier::BOLD),
                ))
                .borders(Borders::ALL)
                .border_type(BorderType::Rounded)
                .border_style(Style::default().fg(Color::Rgb(120, 220, 255)))
                .style(Style::default().bg(bg)),
        );
        frame.render_widget(paragraph, area);
    }
}

/// Pad with spaces so a row's highlight spans the whole menu width.
fn pad_to(s: &str, width: usize) -> String {
    let len = s.chars().count();
    if len >= width {
        return s.chars().take(width).collect();
    }
    let mut out = s.to_string();
    out.extend(std::iter::repeat_n(' ', width - len));
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    fn key(c: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE)
    }

    #[test]
    fn opens_on_the_current_mode() {
        let m = SortMenu::new(SortMode::Newest);
        assert_eq!(m.selected(), SortMode::Newest);
    }

    #[test]
    fn a_number_picks_that_entry() {
        let mut m = SortMenu::new(SortMode::Largest);
        assert_eq!(
            m.handle_key(key('3')),
            SortMenuOutcome::Chosen(SortMode::ALL[2])
        );
    }

    #[test]
    fn out_of_range_numbers_are_ignored() {
        let mut m = SortMenu::new(SortMode::Largest);
        assert_eq!(m.handle_key(key('9')), SortMenuOutcome::Continue);
        // Zero has no entry either, and must not underflow.
        assert_eq!(m.handle_key(key('0')), SortMenuOutcome::Continue);
    }

    #[test]
    fn vi_keys_navigate_and_enter_chooses() {
        let mut m = SortMenu::new(SortMode::ALL[0]);
        m.handle_key(key('j'));
        assert_eq!(m.selected(), SortMode::ALL[1]);
        m.handle_key(key('k'));
        assert_eq!(m.selected(), SortMode::ALL[0]);
        assert_eq!(
            m.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
            SortMenuOutcome::Chosen(SortMode::ALL[0])
        );
    }

    #[test]
    fn esc_cancels() {
        let mut m = SortMenu::new(SortMode::Largest);
        assert_eq!(
            m.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)),
            SortMenuOutcome::Cancelled
        );
    }

    #[test]
    fn navigation_wraps_at_both_ends() {
        let mut m = SortMenu::new(SortMode::ALL[0]);
        m.handle_key(key('k'));
        assert_eq!(m.selected(), *SortMode::ALL.last().unwrap());
        m.handle_key(key('j'));
        assert_eq!(m.selected(), SortMode::ALL[0]);
    }

    #[test]
    fn clicking_an_entry_chooses_it_and_outside_dismisses() {
        let mut m = SortMenu::new(SortMode::Largest);
        let backend = ratatui::backend::TestBackend::new(100, 30);
        let mut term = ratatui::Terminal::new(backend).unwrap();
        term.draw(|f| m.render(f, f.area(), None)).unwrap();

        let area = m.area.expect("render should record its area");
        // First entry sits one row below the top border.
        assert_eq!(
            m.click_at(area.x + 3, area.y + 1),
            SortMenuOutcome::Chosen(SortMode::ALL[0])
        );
        assert_eq!(
            m.click_at(area.x + 3, area.y + 3),
            SortMenuOutcome::Chosen(SortMode::ALL[2])
        );
        // Well outside the box.
        assert_eq!(m.click_at(0, 29), SortMenuOutcome::Cancelled);
    }

    #[test]
    fn renders_beside_its_anchor_and_stays_on_screen() {
        let mut m = SortMenu::new(SortMode::Largest);
        let backend = ratatui::backend::TestBackend::new(100, 30);
        let mut term = ratatui::Terminal::new(backend).unwrap();

        // An anchor near the right edge must not push the menu off-screen.
        let anchor = Rect::new(95, 0, 10, 1);
        term.draw(|f| m.render(f, f.area(), Some(anchor))).unwrap();
        let area = m.area.unwrap();
        assert!(area.x + area.width <= 100, "menu ran off the right edge");

        // An anchor near the bottom flips the menu above it.
        let anchor = Rect::new(2, 29, 10, 1);
        term.draw(|f| m.render(f, f.area(), Some(anchor))).unwrap();
        let area = m.area.unwrap();
        assert!(area.y + area.height <= 30, "menu ran off the bottom");
    }

    #[test]
    fn every_mode_is_offered() {
        // The menu is built from SortMode::ALL, so a newly added mode appears
        // here automatically rather than being silently unreachable.
        assert_eq!(SortMode::ALL.len(), 8);
        for m in SortMode::ALL {
            assert!(!m.label().is_empty());
            assert!(!m.description().is_empty());
        }
    }

    #[test]
    fn every_mode_can_be_typed_as_a_single_digit() {
        // Entries are picked by typing their number, and the handler reads one
        // digit. A tenth mode would be listed but unreachable from the keyboard,
        // and the help text's "1-9" would be a lie — so this is the point to
        // stop and rethink the shortcut, not to quietly add another row.
        assert!(
            SortMode::ALL.len() <= 9,
            "a tenth sort mode needs a different shortcut scheme"
        );
        // And each digit really does select the mode at that position.
        for (i, mode) in SortMode::ALL.iter().enumerate() {
            let mut m = SortMenu::new(SortMode::ALL[0]);
            let digit = char::from_digit(i as u32 + 1, 10).expect("1-9");
            assert_eq!(
                m.handle_key(key(digit)),
                SortMenuOutcome::Chosen(*mode),
                "typing {} should choose {}",
                digit,
                mode.label()
            );
        }
    }
}
