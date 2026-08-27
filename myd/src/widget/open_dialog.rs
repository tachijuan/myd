//! Running a program of the user's choosing over the selection.
//!
//! A text field for the command line and three buttons. The field mechanics
//! follow [`crate::widget::rename_dialog`] and the buttons follow
//! [`crate::widget::confirm_dialog`] — this dialog is the two halves together,
//! and copying both keeps `Tab`, `Enter` and clicks meaning what they already
//! mean everywhere else here.

use crate::widget::text_field::TextField;
use ratatui::{
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span, Text},
    widgets::{Block, BorderType, Borders, Clear, Paragraph},
    Frame,
};
use std::path::PathBuf;

/// What the dialog decided this keystroke.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OpenDialogOutcome {
    /// Still editing. The dialog stays up.
    Continue,
    Cancelled,
    /// Run this command line over the captured targets.
    Run { command: String },
}

/// Which half of the dialog has the keyboard.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OpenFocus {
    Field,
    Buttons,
}

/// The buttons, in the order they are drawn.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OpenButton {
    Ok,
    Cancel,
}

pub struct OpenDialog {
    command: TextField,
    focus: OpenFocus,
    /// Index into [`Self::buttons`].
    button: usize,
    /// The paths this will act on, captured when the dialog opened. Held so the
    /// summary line can say what is about to happen; the app re-reads the
    /// selection when it actually runs.
    targets: Vec<PathBuf>,
    /// Rebuilt on every render, so the drawn buttons and their click targets
    /// cannot describe different boxes.
    button_areas: Vec<Rect>,
}

impl OpenDialog {
    pub fn new(targets: Vec<PathBuf>) -> Self {
        Self {
            command: TextField::new(),
            focus: OpenFocus::Field,
            button: 0,
            targets,
            button_areas: Vec::new(),
        }
    }

    /// Start with the field pre-filled, for a remembered command.
    pub fn with_command(mut self, command: impl Into<String>) -> Self {
        self.command = TextField::with_value(command);
        // The field keeps its own cursor.
        self
    }

    pub fn command(&self) -> &str {
        self.command.value()
    }

    pub fn targets(&self) -> &[PathBuf] {
        &self.targets
    }

    /// The buttons this dialog has, in draw order.
    ///
    /// One source for the labels, the click rectangles and the Tab cycle, so
    /// the three can never describe different boxes.
    pub fn buttons(&self) -> Vec<OpenButton> {
        vec![OpenButton::Ok, OpenButton::Cancel]
    }

    fn label(button: OpenButton) -> &'static str {
        match button {
            OpenButton::Ok => " [ OK ] ",
            OpenButton::Cancel => " [ Cancel ] ",
        }
    }

    /// The button with the keyboard, if the buttons have it at all.
    pub fn focused_button(&self) -> Option<OpenButton> {
        if self.focus != OpenFocus::Buttons {
            return None;
        }
        self.buttons().get(self.button).copied()
    }

    /// A command line with something in it. An empty field has nothing to run,
    /// and Enter on it should keep the dialog rather than close it having done
    /// nothing visible.
    fn is_runnable(&self) -> bool {
        !self.command.value().trim().is_empty()
    }

    /// What pressing `button` decides.
    fn press(&self, button: OpenButton) -> OpenDialogOutcome {
        match button {
            OpenButton::Ok => self.run_outcome(),
            OpenButton::Cancel => OpenDialogOutcome::Cancelled,
        }
    }

    fn run_outcome(&self) -> OpenDialogOutcome {
        if self.is_runnable() {
            OpenDialogOutcome::Run {
                command: self.command.value().trim().to_string(),
            }
        } else {
            OpenDialogOutcome::Continue
        }
    }

    pub fn handle_key(&mut self, key: crossterm::event::KeyEvent) -> OpenDialogOutcome {
        use crossterm::event::KeyCode;

        match key.code {
            KeyCode::Esc => OpenDialogOutcome::Cancelled,

            // Tab moves the focus and never runs anything — the same contract as
            // every other dialog here. Reaching for the buttons must not be read
            // as consent to execute a program.
            KeyCode::Tab | KeyCode::BackTab => {
                self.cycle(matches!(key.code, KeyCode::BackTab));
                OpenDialogOutcome::Continue
            }

            KeyCode::Enter => match self.focus {
                // Enter in the field runs it. This is a command prompt, and
                // making the user Tab to OK before it would take would be a
                // nuisance — the Tab rule still holds, because Tab itself
                // remains inert.
                OpenFocus::Field => self.run_outcome(),
                OpenFocus::Buttons => match self.buttons().get(self.button).copied() {
                    Some(button) => self.press(button),
                    None => OpenDialogOutcome::Continue,
                },
            },

            // The buttons take the arrows they are laid out along; the field
            // needs left and right for its own cursor.
            KeyCode::Left if self.focus == OpenFocus::Buttons => {
                self.cycle(true);
                OpenDialogOutcome::Continue
            }
            KeyCode::Right if self.focus == OpenFocus::Buttons => {
                self.cycle(false);
                OpenDialogOutcome::Continue
            }
            KeyCode::Up | KeyCode::Down => {
                self.focus = match self.focus {
                    OpenFocus::Field => OpenFocus::Buttons,
                    OpenFocus::Buttons => OpenFocus::Field,
                };
                OpenDialogOutcome::Continue
            }

            // Everything below edits the field. Typing while the buttons have
            // focus returns to the field and inserts, so a user who tabbed too
            // far and kept typing does not lose the characters.
            // Everything below edits the field. A key that types or deletes
            // returns focus to the field first, so a user who tabbed too far
            // and kept typing does not lose the characters; a bare motion does
            // not steal focus back.
            _ => {
                let edits = !matches!(
                    key.code,
                    KeyCode::Left | KeyCode::Right | KeyCode::Home | KeyCode::End
                );
                if self.command.handle_key(key) && edits {
                    self.focus = OpenFocus::Field;
                }
                OpenDialogOutcome::Continue
            }
        }
    }

    /// Move the focus one stop, wrapping. The field is the stop before the
    /// first button, so Tab walks field → OK → Cancel → field.
    fn cycle(&mut self, backwards: bool) {
        let count = self.buttons().len();
        // Stop 0 is the field; 1..=count are the buttons.
        let current = match self.focus {
            OpenFocus::Field => 0,
            OpenFocus::Buttons => self.button.min(count.saturating_sub(1)) + 1,
        };
        let stops = count + 1;
        let next = if backwards {
            (current + stops - 1) % stops
        } else {
            (current + 1) % stops
        };
        if next == 0 {
            self.focus = OpenFocus::Field;
        } else {
            self.focus = OpenFocus::Buttons;
            self.button = next - 1;
        }
    }

    /// The cursor's byte offset in the command line.
    ///
    /// Paths get pasted in here and they carry multibyte characters; indexing by
    /// the char cursor directly would panic mid-codepoint.
    /// Answer a click at `(x, y)`.
    ///
    /// A click on a button is that button's decision, the same one Enter makes
    /// on the focused one. Clicks elsewhere only move the focus to the field:
    /// this dialog launches a program, and a stray click must not do that.
    pub fn click_at(&mut self, x: u16, y: u16) -> OpenDialogOutcome {
        let hit = self
            .button_areas
            .iter()
            .position(|r| x >= r.x && x < r.x + r.width && y >= r.y && y < r.y + r.height);
        if let Some(i) = hit {
            if let Some(button) = self.buttons().get(i).copied() {
                self.focus = OpenFocus::Buttons;
                self.button = i;
                return self.press(button);
            }
        }
        OpenDialogOutcome::Continue
    }

    /// The line describing what this will act on.
    fn summary(&self) -> String {
        match self.targets.len() {
            0 => " Nothing selected".to_string(),
            1 => format!(
                " Opening {}",
                self.targets[0]
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_else(|| self.targets[0].display().to_string())
            ),
            n => format!(" Opening {} tagged files", n),
        }
    }

    pub fn render(&mut self, frame: &mut Frame, area: Rect) {
        let bg = Color::Rgb(24, 24, 34);
        let width = 72.min(area.width.max(1));
        let inner = width.saturating_sub(2).max(1) as usize;
        // title + blank + label + field + blank + hint + blank + buttons,
        // plus the two border rows.
        let height = 12.min(area.height.max(1));
        let center = centered(Rect::new(0, 0, width, height), area);
        if center.width == 0 || center.height == 0 {
            // Nothing survives the clamp on a terminal this small. Drawing the
            // buttons anyway would leave click rectangles pointing at cells no
            // one can see.
            self.button_areas.clear();
            return;
        }
        frame.render_widget(Clear, center);

        let dim = Style::default().fg(Color::Rgb(150, 150, 170)).bg(bg);
        let normal = Style::default().fg(Color::Rgb(235, 235, 245)).bg(bg);
        let accent = Color::Rgb(120, 220, 255);

        let mut lines: Vec<Line> = Vec::new();
        lines.push(Line::from(Span::styled(
            truncate(&self.summary(), inner),
            dim,
        )));
        lines.push(Line::from(Span::styled(String::new(), normal)));

        let field_focused = self.focus == OpenFocus::Field;
        lines.push(Line::from(Span::styled(
            " Command".to_string(),
            if field_focused {
                Style::default().fg(accent).bg(bg).add_modifier(Modifier::BOLD)
            } else {
                dim
            },
        )));
        // Two columns of indent, then the field. The cursor is a styled cell,
        // not a character spliced into the text — see `TextField`.
        let field_style = if field_focused {
            normal.add_modifier(Modifier::BOLD)
        } else {
            normal
        };
        let mut field_spans = vec![Span::styled("  ".to_string(), field_style)];
        field_spans.extend(self.command.spans(inner.saturating_sub(2), field_style, field_focused));
        lines.push(Line::from(field_spans));

        lines.push(Line::from(Span::styled(String::new(), normal)));
        lines.push(Line::from(Span::styled(
            truncate(
                "  The selected files are appended, e.g.  vim -v  runs  vim -v <files>",
                inner,
            ),
            dim,
        )));
        lines.push(Line::from(Span::styled(String::new(), normal)));

        // Buttons and their click targets are built together, in one pass, for
        // the same reason the confirm dialog does it: two passes can disagree.
        let buttons = self.buttons();
        let sep = "  ";
        let mut x = center.x + 1;
        let button_y = center.y + center.height.saturating_sub(2);
        let mut spans: Vec<Span> = Vec::new();
        self.button_areas.clear();
        for (i, button) in buttons.iter().enumerate() {
            if i > 0 {
                spans.push(Span::styled(sep, normal));
                x += sep.chars().count() as u16;
            }
            let label = Self::label(*button);
            let w = label.chars().count() as u16;
            let focused = self.focus == OpenFocus::Buttons && i == self.button;
            let style = if focused {
                Style::default()
                    .fg(Color::Yellow)
                    .bg(bg)
                    .add_modifier(Modifier::BOLD | Modifier::REVERSED)
            } else {
                Style::default().fg(Color::Yellow).bg(bg)
            };
            spans.push(Span::styled(label, style));
            self.button_areas.push(Rect::new(x, button_y, w, 1));
            x += w;
        }

        // The buttons sit on the last content row. Pad the lines before them so
        // the box is filled to that row whatever the terminal height allowed.
        let content_rows = center.height.saturating_sub(2) as usize;
        while lines.len() + 1 < content_rows {
            lines.push(Line::from(Span::styled(String::new(), normal)));
        }
        lines.truncate(content_rows.saturating_sub(1));
        lines.push(Line::from(spans));

        let paragraph = Paragraph::new(Text::from(lines))
            .style(Style::default().bg(bg))
            .block(
                Block::default()
                    .title(Span::styled(
                        " Open with ",
                        Style::default().fg(accent).add_modifier(Modifier::BOLD),
                    ))
                    .borders(Borders::ALL)
                    .border_type(BorderType::Rounded)
                    .border_style(Style::default().fg(accent))
                    .style(Style::default().bg(bg)),
            );
        frame.render_widget(paragraph, center);
    }
}

fn truncate(s: &str, width: usize) -> String {
    if s.chars().count() <= width {
        return s.to_string();
    }
    let keep = width.saturating_sub(1);
    let mut out: String = s.chars().take(keep).collect();
    out.push('…');
    out
}


fn centered(inner: Rect, outer: Rect) -> Rect {
    let x = outer.x + outer.width.saturating_sub(inner.width) / 2;
    let y = outer.y + outer.height.saturating_sub(inner.height) / 2;
    Rect::new(
        x,
        y,
        inner.width.min(outer.width),
        inner.height.min(outer.height),
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

    fn type_str(d: &mut OpenDialog, s: &str) {
        for c in s.chars() {
            d.handle_key(key(c));
        }
    }

    fn one_file() -> Vec<PathBuf> {
        vec![PathBuf::from("/tmp/a.txt")]
    }

    fn two_files() -> Vec<PathBuf> {
        vec![PathBuf::from("/tmp/a.txt"), PathBuf::from("/tmp/b.txt")]
    }

    #[test]
    fn enter_in_the_field_runs_the_typed_command() {
        let mut d = OpenDialog::new(one_file());
        type_str(&mut d, "vim -v");
        assert_eq!(
            d.handle_key(code(KeyCode::Enter)),
            OpenDialogOutcome::Run {
                command: "vim -v".to_string()
            }
        );
    }

    #[test]
    fn tab_moves_the_focus_and_never_runs_anything() {
        // The house rule: Tab is for reaching the buttons, not for pressing
        // them. A dialog that launches a program is the last place to bend it.
        let mut d = OpenDialog::new(one_file());
        type_str(&mut d, "rm -rf");
        for _ in 0..8 {
            assert_eq!(
                d.handle_key(code(KeyCode::Tab)),
                OpenDialogOutcome::Continue,
                "Tab must never decide anything"
            );
        }
        for _ in 0..8 {
            assert_eq!(
                d.handle_key(code(KeyCode::BackTab)),
                OpenDialogOutcome::Continue,
                "Shift-Tab must never decide anything either"
            );
        }
    }

    #[test]
    fn tab_walks_the_field_then_every_button_and_wraps() {
        let mut d = OpenDialog::new(one_file());
        assert_eq!(d.focused_button(), None, "the field starts with focus");
        d.handle_key(code(KeyCode::Tab));
        assert_eq!(d.focused_button(), Some(OpenButton::Ok));
        d.handle_key(code(KeyCode::Tab));
        assert_eq!(d.focused_button(), Some(OpenButton::Cancel));
        d.handle_key(code(KeyCode::Tab));
        assert_eq!(d.focused_button(), None, "and back to the field");
    }

    #[test]
    fn the_same_two_buttons_whatever_is_selected() {
        // The selection changes what the command runs over, not what the dialog
        // offers to do with it.
        assert_eq!(
            OpenDialog::new(one_file()).buttons(),
            vec![OpenButton::Ok, OpenButton::Cancel]
        );
        assert_eq!(
            OpenDialog::new(two_files()).buttons(),
            vec![OpenButton::Ok, OpenButton::Cancel]
        );
    }

    #[test]
    fn esc_cancels_distinctly_from_an_empty_enter() {
        // The plain input dialog cannot tell these apart — it returns an empty
        // string for both — and several call sites work around it. This one
        // reports them separately.
        let mut d = OpenDialog::new(one_file());
        assert_eq!(
            d.handle_key(code(KeyCode::Enter)),
            OpenDialogOutcome::Continue,
            "an empty command has nothing to run"
        );
        assert_eq!(d.handle_key(code(KeyCode::Esc)), OpenDialogOutcome::Cancelled);
    }

    #[test]
    fn a_blank_command_is_not_runnable() {
        let mut d = OpenDialog::new(one_file());
        type_str(&mut d, "   ");
        assert_eq!(d.handle_key(code(KeyCode::Enter)), OpenDialogOutcome::Continue);
    }

    #[test]
    fn the_command_is_trimmed_before_it_runs() {
        let mut d = OpenDialog::new(one_file());
        type_str(&mut d, "  vim  ");
        assert_eq!(
            d.handle_key(code(KeyCode::Enter)),
            OpenDialogOutcome::Run {
                command: "vim".to_string()
            }
        );
    }

    #[test]
    fn each_button_decides_its_own_thing() {
        let mut d = OpenDialog::new(one_file());
        type_str(&mut d, "vim");

        d.handle_key(code(KeyCode::Tab)); // OK
        assert_eq!(
            d.handle_key(code(KeyCode::Enter)),
            OpenDialogOutcome::Run {
                command: "vim".to_string()
            }
        );

        d.handle_key(code(KeyCode::Tab)); // Cancel
        assert_eq!(d.handle_key(code(KeyCode::Enter)), OpenDialogOutcome::Cancelled);
    }

    #[test]
    fn typing_after_tabbing_returns_to_the_field() {
        // Otherwise the characters vanish: the user tabbed one stop too far,
        // kept typing, and watched nothing appear.
        let mut d = OpenDialog::new(one_file());
        d.handle_key(code(KeyCode::Tab));
        assert_eq!(d.focused_button(), Some(OpenButton::Ok));
        type_str(&mut d, "less");
        assert_eq!(d.command(), "less");
        assert_eq!(d.focused_button(), None);
    }

    #[test]
    fn editing_keys_move_within_the_command() {
        let mut d = OpenDialog::new(one_file());
        type_str(&mut d, "vim");
        d.handle_key(code(KeyCode::Home));
        type_str(&mut d, "g");
        assert_eq!(d.command(), "gvim");
        d.handle_key(code(KeyCode::End));
        type_str(&mut d, "!");
        assert_eq!(d.command(), "gvim!");
        d.handle_key(code(KeyCode::Backspace));
        assert_eq!(d.command(), "gvim");
        d.handle_key(code(KeyCode::Home));
        d.handle_key(code(KeyCode::Delete));
        assert_eq!(d.command(), "vim");
    }

    #[test]
    fn a_multibyte_command_does_not_panic_on_the_cursor() {
        // The cursor counts characters and the string indexes bytes; mixing the
        // two panics mid-codepoint.
        let mut d = OpenDialog::new(one_file());
        type_str(&mut d, "é→ç");
        d.handle_key(code(KeyCode::Home));
        d.handle_key(code(KeyCode::Right));
        type_str(&mut d, "ü");
        assert_eq!(d.command(), "éü→ç");
        d.handle_key(code(KeyCode::Backspace));
        assert_eq!(d.command(), "é→ç");
    }

    #[test]
    fn clicking_a_button_presses_it() {
        use ratatui::{backend::TestBackend, Terminal};
        let mut d = OpenDialog::new(one_file());
        type_str(&mut d, "vim");
        let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
        terminal.draw(|f| d.render(f, f.area())).unwrap();

        let areas = d.button_areas.clone();
        assert_eq!(areas.len(), 2, "two buttons were drawn");
        assert_eq!(
            d.click_at(areas[1].x, areas[1].y),
            OpenDialogOutcome::Cancelled,
            "clicking Cancel backs out"
        );
        assert_eq!(
            d.click_at(areas[0].x, areas[0].y),
            OpenDialogOutcome::Run {
                command: "vim".to_string()
            },
            "clicking OK runs the command"
        );
    }

    #[test]
    fn clicking_away_from_the_buttons_does_nothing() {
        use ratatui::{backend::TestBackend, Terminal};
        let mut d = OpenDialog::new(one_file());
        type_str(&mut d, "vim");
        let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
        terminal.draw(|f| d.render(f, f.area())).unwrap();
        assert_eq!(d.click_at(0, 0), OpenDialogOutcome::Continue);
    }

    #[test]
    fn the_summary_says_what_is_being_opened() {
        assert!(OpenDialog::new(one_file()).summary().contains("a.txt"));
        assert!(OpenDialog::new(two_files()).summary().contains("2 tagged"));
    }

    #[test]
    fn it_renders_at_tiny_terminal_sizes_without_panicking() {
        use ratatui::{backend::TestBackend, Terminal};
        for (w, h) in [(1u16, 1u16), (2, 3), (10, 4), (40, 2), (80, 24)] {
            let mut d = OpenDialog::new(one_file());
            type_str(&mut d, "vim -v");
            let mut terminal = Terminal::new(TestBackend::new(w, h)).unwrap();
            terminal
                .draw(|f| d.render(f, f.area()))
                .unwrap_or_else(|e| panic!("open dialog panicked at {}x{}: {}", w, h, e));
        }
    }
}
