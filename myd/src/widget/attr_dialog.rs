//! Editing one attribute of the selection: permissions, owner, or group.
//!
//! A text field plus buttons, following [`crate::widget::open_dialog`] — which
//! is itself the rename dialog's field and the confirm dialog's buttons put
//! together. Copying it keeps `Tab`, `Enter`, `Esc` and clicks meaning what
//! they already mean everywhere else here.
//!
//! The one addition is a recursive checkbox, offered only when the single
//! target is a directory. It is a third focus stop rather than a separate key,
//! so nothing about it has to be remembered.

use ratatui::{
    Frame,
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span, Text},
    widgets::{Block, BorderType, Borders, Clear, Paragraph},
};
use std::path::PathBuf;

pub use crate::widget::file_info::InfoField as AttrField;

/// What the dialog decided this keystroke.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AttrDialogOutcome {
    /// Still editing. The dialog stays up.
    Continue,
    Cancelled,
    /// Apply this value to the captured targets.
    Apply { value: String, recursive: bool },
}

/// Which part of the dialog has the keyboard.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AttrFocus {
    Field,
    Recursive,
    Buttons,
}

/// The buttons, in the order they are drawn.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AttrButton {
    Apply,
    Cancel,
}

pub struct AttrDialog {
    field: AttrField,
    value: String,
    /// Char index within `value`, converted to bytes by [`Self::byte_index`].
    cursor: usize,
    focus: AttrFocus,
    /// Index into [`Self::buttons`].
    button: usize,
    recursive: bool,
    /// Whether the recursive checkbox exists at all. When false it is not drawn
    /// and not a focus stop, so it can be neither tabbed to nor clicked.
    allow_recursive: bool,
    /// The paths this will act on, captured when the dialog opened.
    targets: Vec<PathBuf>,
    /// Rebuilt on every render, so the drawn boxes and their click targets
    /// cannot describe different rectangles.
    button_areas: Vec<Rect>,
    /// Where the checkbox was drawn, for the same reason.
    recursive_area: Option<Rect>,
}

impl AttrDialog {
    /// A dialog for `field` over `targets`.
    ///
    /// `allow_recursive` is the caller's answer to "is this one directory?" —
    /// the dialog cannot tell, since a path that is a directory now may not be
    /// by the time it is applied, and asking the filesystem from a widget would
    /// put I/O in the render path.
    pub fn new(field: AttrField, targets: Vec<PathBuf>, allow_recursive: bool) -> Self {
        Self {
            field,
            value: String::new(),
            cursor: 0,
            focus: AttrFocus::Field,
            button: 0,
            recursive: false,
            allow_recursive,
            targets,
            button_areas: Vec::new(),
            recursive_area: None,
        }
    }

    /// Start with the field pre-filled with the current value, so an edit is a
    /// correction rather than a retype.
    pub fn with_value(mut self, value: impl Into<String>) -> Self {
        self.value = value.into();
        self.cursor = self.value.chars().count();
        self
    }

    pub fn field(&self) -> AttrField {
        self.field
    }

    pub fn value(&self) -> &str {
        &self.value
    }

    pub fn targets(&self) -> &[PathBuf] {
        &self.targets
    }

    pub fn recursive(&self) -> bool {
        self.recursive
    }

    pub fn allows_recursive(&self) -> bool {
        self.allow_recursive
    }

    /// The buttons this dialog has, in draw order.
    ///
    /// One source for the labels, the click rectangles and the Tab cycle.
    pub fn buttons(&self) -> Vec<AttrButton> {
        vec![AttrButton::Apply, AttrButton::Cancel]
    }

    fn label(button: AttrButton) -> &'static str {
        match button {
            AttrButton::Apply => " [ Apply ] ",
            AttrButton::Cancel => " [ Cancel ] ",
        }
    }

    /// The button with the keyboard, if the buttons have it at all.
    pub fn focused_button(&self) -> Option<AttrButton> {
        if self.focus != AttrFocus::Buttons {
            return None;
        }
        self.buttons().get(self.button).copied()
    }

    /// A value with something in it. An empty field has nothing to apply, and
    /// Enter on it should keep the dialog rather than close it having silently
    /// done nothing.
    fn is_applicable(&self) -> bool {
        !self.value.trim().is_empty()
    }

    fn apply_outcome(&self) -> AttrDialogOutcome {
        if self.is_applicable() {
            AttrDialogOutcome::Apply {
                value: self.value.trim().to_string(),
                recursive: self.recursive && self.allow_recursive,
            }
        } else {
            AttrDialogOutcome::Continue
        }
    }

    /// What pressing `button` decides.
    fn press(&self, button: AttrButton) -> AttrDialogOutcome {
        match button {
            AttrButton::Apply => self.apply_outcome(),
            AttrButton::Cancel => AttrDialogOutcome::Cancelled,
        }
    }

    pub fn handle_key(&mut self, key: crossterm::event::KeyEvent) -> AttrDialogOutcome {
        use crossterm::event::KeyCode;

        match key.code {
            // Distinct from an empty Enter, which keeps the dialog: Esc means
            // "I have changed my mind", and that must not be ambiguous when the
            // thing being changed is a permission bit.
            KeyCode::Esc => AttrDialogOutcome::Cancelled,

            // Tab moves the focus and never applies anything — the house rule.
            // Reaching for a button must not be read as consent to change a
            // file's mode.
            KeyCode::Tab | KeyCode::BackTab => {
                self.cycle(matches!(key.code, KeyCode::BackTab));
                AttrDialogOutcome::Continue
            }

            KeyCode::Enter => match self.focus {
                // Enter in the field applies, as in the open dialog: this is a
                // one-value form, and requiring a Tab first would be ceremony.
                // The Tab rule still holds, because Tab itself stays inert.
                AttrFocus::Field => self.apply_outcome(),
                // On the checkbox Enter toggles rather than applies. A key that
                // means "yes, this one" on a control whose whole purpose is a
                // yes/no should answer the control, not the form.
                AttrFocus::Recursive => {
                    self.recursive = !self.recursive;
                    AttrDialogOutcome::Continue
                }
                AttrFocus::Buttons => match self.buttons().get(self.button).copied() {
                    Some(button) => self.press(button),
                    None => AttrDialogOutcome::Continue,
                },
            },

            // Space toggles the checkbox, which is what a checkbox answers to.
            // Everywhere else it is an ordinary character in the field.
            KeyCode::Char(' ') if self.focus == AttrFocus::Recursive => {
                self.recursive = !self.recursive;
                AttrDialogOutcome::Continue
            }

            // The buttons take the arrows they are laid out along; the field
            // needs left and right for its own cursor.
            KeyCode::Left if self.focus == AttrFocus::Buttons => {
                self.cycle(true);
                AttrDialogOutcome::Continue
            }
            KeyCode::Right if self.focus == AttrFocus::Buttons => {
                self.cycle(false);
                AttrDialogOutcome::Continue
            }
            KeyCode::Up => {
                self.cycle(true);
                AttrDialogOutcome::Continue
            }
            KeyCode::Down => {
                self.cycle(false);
                AttrDialogOutcome::Continue
            }

            // Everything below edits the field. Typing while something else has
            // focus returns to the field and inserts, so a user who tabbed too
            // far and kept typing does not lose the characters.
            KeyCode::Char(c) => {
                self.focus = AttrFocus::Field;
                let at = self.byte_index();
                self.value.insert(at, c);
                self.cursor += 1;
                AttrDialogOutcome::Continue
            }
            KeyCode::Backspace => {
                self.focus = AttrFocus::Field;
                if self.cursor > 0 {
                    self.cursor -= 1;
                    let at = self.byte_index();
                    self.value.remove(at);
                }
                AttrDialogOutcome::Continue
            }
            KeyCode::Delete => {
                self.focus = AttrFocus::Field;
                let at = self.byte_index();
                if at < self.value.len() {
                    self.value.remove(at);
                }
                AttrDialogOutcome::Continue
            }
            KeyCode::Left => {
                self.cursor = self.cursor.saturating_sub(1);
                AttrDialogOutcome::Continue
            }
            KeyCode::Right => {
                if self.cursor < self.value.chars().count() {
                    self.cursor += 1;
                }
                AttrDialogOutcome::Continue
            }
            KeyCode::Home => {
                self.cursor = 0;
                AttrDialogOutcome::Continue
            }
            KeyCode::End => {
                self.cursor = self.value.chars().count();
                AttrDialogOutcome::Continue
            }
            _ => AttrDialogOutcome::Continue,
        }
    }

    /// The focus stops, in order: the field, the checkbox when it exists, then
    /// each button.
    ///
    /// Built from `allow_recursive` so a checkbox that is not drawn is not a
    /// stop either — the same single-source rule the buttons follow.
    fn stops(&self) -> usize {
        1 + usize::from(self.allow_recursive) + self.buttons().len()
    }

    fn current_stop(&self) -> usize {
        let recursive_stop = usize::from(self.allow_recursive);
        match self.focus {
            AttrFocus::Field => 0,
            AttrFocus::Recursive => 1,
            AttrFocus::Buttons => {
                1 + recursive_stop + self.button.min(self.buttons().len().saturating_sub(1))
            }
        }
    }

    /// Move the focus one stop, wrapping.
    fn cycle(&mut self, backwards: bool) {
        let stops = self.stops();
        let current = self.current_stop();
        let next = if backwards {
            (current + stops - 1) % stops
        } else {
            (current + 1) % stops
        };
        let recursive_stop = usize::from(self.allow_recursive);
        if next == 0 {
            self.focus = AttrFocus::Field;
        } else if self.allow_recursive && next == 1 {
            self.focus = AttrFocus::Recursive;
        } else {
            self.focus = AttrFocus::Buttons;
            self.button = next - 1 - recursive_stop;
        }
    }

    /// The cursor's byte offset in the value.
    ///
    /// A user name can carry multibyte characters; indexing by the char cursor
    /// directly would panic mid-codepoint.
    fn byte_index(&self) -> usize {
        self.value
            .char_indices()
            .nth(self.cursor)
            .map(|(i, _)| i)
            .unwrap_or(self.value.len())
    }

    /// Answer a click at `(x, y)`.
    ///
    /// A click on a button is that button's decision. A click on the checkbox
    /// toggles it. Clicks elsewhere do nothing: this dialog changes file
    /// attributes, and a stray click must not.
    pub fn click_at(&mut self, x: u16, y: u16) -> AttrDialogOutcome {
        let inside = |r: &Rect| x >= r.x && x < r.x + r.width && y >= r.y && y < r.y + r.height;

        if self.allow_recursive {
            if let Some(area) = self.recursive_area.filter(inside) {
                let _ = area;
                self.focus = AttrFocus::Recursive;
                self.recursive = !self.recursive;
                return AttrDialogOutcome::Continue;
            }
        }

        if let Some(i) = self.button_areas.iter().position(inside) {
            if let Some(button) = self.buttons().get(i).copied() {
                self.focus = AttrFocus::Buttons;
                self.button = i;
                return self.press(button);
            }
        }
        AttrDialogOutcome::Continue
    }

    /// The line describing what this will act on.
    fn summary(&self) -> String {
        let what = self.field.label();
        match self.targets.len() {
            0 => " Nothing selected".to_string(),
            1 => format!(
                " {} of {}",
                what,
                self.targets[0]
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_else(|| self.targets[0].display().to_string())
            ),
            n => format!(" {} of {} tagged files", what, n),
        }
    }

    /// The hint under the field, which differs per attribute because what
    /// counts as a valid entry does.
    fn hint(&self) -> &'static str {
        match self.field {
            AttrField::Perms => "  Octal (644) or symbolic (rw-r--r--)",
            AttrField::Owner => "  A user name, or a numeric id",
            AttrField::Group => "  A group name, or a numeric id",
        }
    }

    pub fn render(&mut self, frame: &mut Frame, area: Rect) {
        let bg = Color::Rgb(24, 24, 34);
        let width = 60.min(area.width.max(1));
        let inner = width.saturating_sub(2).max(1) as usize;
        // title + blank + label + field + blank + hint + [checkbox] + blank +
        // buttons, plus the two border rows.
        let height = if self.allow_recursive { 13 } else { 12 }.min(area.height.max(1));
        let center = centered(Rect::new(0, 0, width, height), area);
        if center.width == 0 || center.height == 0 {
            // Nothing survives the clamp on a terminal this small. Drawing the
            // controls anyway would leave click rectangles pointing at cells no
            // one can see.
            self.button_areas.clear();
            self.recursive_area = None;
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

        let field_focused = self.focus == AttrFocus::Field;
        lines.push(Line::from(Span::styled(
            format!(" {}", self.field.label()),
            if field_focused {
                Style::default()
                    .fg(accent)
                    .bg(bg)
                    .add_modifier(Modifier::BOLD)
            } else {
                dim
            },
        )));
        let shown = if field_focused {
            let cut = self.byte_index();
            format!("  {}█{}", &self.value[..cut], &self.value[cut..])
        } else {
            format!("  {}", self.value)
        };
        lines.push(Line::from(Span::styled(
            pad_to(&shown, inner),
            if field_focused {
                normal.add_modifier(Modifier::BOLD)
            } else {
                normal
            },
        )));

        lines.push(Line::from(Span::styled(String::new(), normal)));
        lines.push(Line::from(Span::styled(
            truncate(self.hint(), inner),
            dim,
        )));

        // The checkbox row, when there is one. Its rect is recorded here so the
        // drawn box and the clickable one are the same by construction.
        self.recursive_area = None;
        if self.allow_recursive {
            let mark = if self.recursive { "x" } else { " " };
            let text = format!("  [{}] Apply to everything inside", mark);
            let focused = self.focus == AttrFocus::Recursive;
            lines.push(Line::from(Span::styled(
                pad_to(&text, inner),
                if focused {
                    Style::default()
                        .fg(accent)
                        .bg(bg)
                        .add_modifier(Modifier::BOLD | Modifier::REVERSED)
                } else {
                    normal
                },
            )));
            // Content starts one row below the top border; the checkbox is the
            // seventh content line built above (index 6).
            let row = center.y + 1 + 6;
            if row < center.y + center.height.saturating_sub(1) {
                self.recursive_area = Some(Rect::new(
                    center.x + 1,
                    row,
                    center.width.saturating_sub(2),
                    1,
                ));
            }
        }

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
            let focused = self.focus == AttrFocus::Buttons && i == self.button;
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
                        format!(" Change {} ", self.field.label().to_lowercase()),
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

fn pad_to(s: &str, width: usize) -> String {
    let len = s.chars().count();
    if len >= width {
        return s.chars().take(width).collect();
    }
    let mut out = s.to_string();
    out.extend(std::iter::repeat_n(' ', width - len));
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

    fn type_str(d: &mut AttrDialog, s: &str) {
        for c in s.chars() {
            d.handle_key(key(c));
        }
    }

    fn one_file() -> Vec<PathBuf> {
        vec![PathBuf::from("/tmp/a.txt")]
    }

    fn file_dialog() -> AttrDialog {
        AttrDialog::new(AttrField::Perms, one_file(), false)
    }

    fn dir_dialog() -> AttrDialog {
        AttrDialog::new(AttrField::Perms, vec![PathBuf::from("/tmp/d")], true)
    }

    #[test]
    fn enter_in_the_field_applies_the_typed_value() {
        let mut d = file_dialog();
        type_str(&mut d, "644");
        assert_eq!(
            d.handle_key(code(KeyCode::Enter)),
            AttrDialogOutcome::Apply {
                value: "644".to_string(),
                recursive: false,
            }
        );
    }

    /// The house rule: Tab moves focus and never decides. Changing a file's
    /// mode must not be something Tab can trigger.
    #[test]
    fn tab_never_applies() {
        let mut d = dir_dialog();
        type_str(&mut d, "644");
        // All the way round the cycle, twice, including over Apply.
        for _ in 0..(d.stops() * 2) {
            assert_eq!(
                d.handle_key(code(KeyCode::Tab)),
                AttrDialogOutcome::Continue,
                "Tab decided something"
            );
        }
        for _ in 0..(d.stops() * 2) {
            assert_eq!(
                d.handle_key(code(KeyCode::BackTab)),
                AttrDialogOutcome::Continue,
                "BackTab decided something"
            );
        }
    }

    /// Esc must be distinguishable from an empty Enter, which keeps the dialog.
    #[test]
    fn esc_cancels_and_an_empty_enter_does_not() {
        let mut d = file_dialog();
        assert_eq!(
            d.handle_key(code(KeyCode::Enter)),
            AttrDialogOutcome::Continue,
            "an empty field should not apply"
        );
        assert_eq!(
            d.handle_key(code(KeyCode::Esc)),
            AttrDialogOutcome::Cancelled
        );
    }

    /// A checkbox that is not drawn must not be reachable, or Tab lands on a
    /// control the user cannot see.
    #[test]
    fn a_file_has_no_recursive_stop() {
        let mut d = file_dialog();
        assert!(!d.allows_recursive());
        // field → Apply → Cancel → field: three stops, no checkbox.
        assert_eq!(d.stops(), 3);
        let mut seen_recursive = false;
        for _ in 0..6 {
            d.handle_key(code(KeyCode::Tab));
            if d.focus == AttrFocus::Recursive {
                seen_recursive = true;
            }
        }
        assert!(!seen_recursive, "Tab reached a checkbox that is not drawn");
    }

    #[test]
    fn a_directory_offers_a_recursive_stop() {
        let mut d = dir_dialog();
        assert!(d.allows_recursive());
        assert_eq!(d.stops(), 4);
        d.handle_key(code(KeyCode::Tab));
        assert_eq!(d.focus, AttrFocus::Recursive);
    }

    #[test]
    fn space_toggles_the_checkbox_only_when_it_has_focus() {
        let mut d = dir_dialog();
        type_str(&mut d, "755");
        assert!(!d.recursive());

        // In the field, space is an ordinary character.
        d.handle_key(key(' '));
        assert!(!d.recursive(), "space toggled from the field");
        assert!(d.value().contains(' '), "space was not typed into the field");

        d.handle_key(code(KeyCode::Tab));
        assert_eq!(d.focus, AttrFocus::Recursive);
        d.handle_key(key(' '));
        assert!(d.recursive(), "space did not toggle the checkbox");
        d.handle_key(key(' '));
        assert!(!d.recursive(), "space did not toggle back");
    }

    /// Enter on the checkbox answers the checkbox, not the form — otherwise the
    /// key that means "yes, this one" would submit instead.
    #[test]
    fn enter_on_the_checkbox_toggles_rather_than_applying() {
        let mut d = dir_dialog();
        type_str(&mut d, "755");
        d.handle_key(code(KeyCode::Tab));
        assert_eq!(
            d.handle_key(code(KeyCode::Enter)),
            AttrDialogOutcome::Continue,
            "Enter on the checkbox applied the change"
        );
        assert!(d.recursive());
    }

    #[test]
    fn the_recursive_flag_reaches_the_outcome() {
        let mut d = dir_dialog();
        type_str(&mut d, "700");
        d.handle_key(code(KeyCode::Tab));
        d.handle_key(key(' '));
        // Back to the field to submit.
        d.handle_key(code(KeyCode::BackTab));
        assert_eq!(
            d.handle_key(code(KeyCode::Enter)),
            AttrDialogOutcome::Apply {
                value: "700".to_string(),
                recursive: true,
            }
        );
    }

    /// A file can never produce a recursive apply, even if the flag were set
    /// some other way — `allow_recursive` is the authority.
    #[test]
    fn a_file_never_applies_recursively() {
        let mut d = file_dialog();
        d.recursive = true;
        type_str(&mut d, "644");
        assert_eq!(
            d.handle_key(code(KeyCode::Enter)),
            AttrDialogOutcome::Apply {
                value: "644".to_string(),
                recursive: false,
            }
        );
    }

    #[test]
    fn typing_while_the_buttons_have_focus_returns_to_the_field() {
        let mut d = file_dialog();
        d.handle_key(code(KeyCode::Tab));
        assert_eq!(d.focus, AttrFocus::Buttons);
        d.handle_key(key('7'));
        assert_eq!(d.focus, AttrFocus::Field);
        assert_eq!(d.value(), "7", "the keystroke was lost");
    }

    #[test]
    fn cancel_cancels_and_apply_applies() {
        let mut d = file_dialog();
        type_str(&mut d, "600");
        // Tab to Apply.
        d.handle_key(code(KeyCode::Tab));
        assert_eq!(d.focused_button(), Some(AttrButton::Apply));
        assert_eq!(
            d.handle_key(code(KeyCode::Enter)),
            AttrDialogOutcome::Apply {
                value: "600".to_string(),
                recursive: false,
            }
        );

        let mut d = file_dialog();
        type_str(&mut d, "600");
        d.handle_key(code(KeyCode::Tab));
        d.handle_key(code(KeyCode::Tab));
        assert_eq!(d.focused_button(), Some(AttrButton::Cancel));
        assert_eq!(
            d.handle_key(code(KeyCode::Enter)),
            AttrDialogOutcome::Cancelled
        );
    }

    /// A multibyte value must not panic the cursor arithmetic — indexing by the
    /// char cursor as if it were a byte offset would split a codepoint.
    #[test]
    fn a_multibyte_value_is_editable() {
        let mut d = AttrDialog::new(AttrField::Owner, one_file(), false);
        type_str(&mut d, "José");

        // Backspace at the end removes the multibyte character itself.
        d.handle_key(code(KeyCode::Backspace));
        assert_eq!(d.value(), "Jos");

        // And with the cursor moved past one, it removes the character before
        // it rather than a byte of it.
        let mut d = AttrDialog::new(AttrField::Owner, one_file(), false);
        type_str(&mut d, "José");
        d.handle_key(code(KeyCode::Left));
        d.handle_key(code(KeyCode::Backspace));
        assert_eq!(d.value(), "Joé");

        // Typing after the multibyte character inserts at the right place.
        d.handle_key(code(KeyCode::End));
        d.handle_key(key('!'));
        assert_eq!(d.value(), "Joé!");
    }

    #[test]
    fn the_field_starts_from_the_current_value() {
        let d = AttrDialog::new(AttrField::Perms, one_file(), false).with_value("rw-r--r--");
        assert_eq!(d.value(), "rw-r--r--");
        // The cursor sits at the end, so typing appends rather than prepends.
        assert_eq!(d.cursor, 9);
    }

    fn render_to(d: &mut AttrDialog, w: u16, h: u16) -> String {
        let mut term =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(w, h)).unwrap();
        term.draw(|f| {
            let area = f.area();
            d.render(f, area);
        })
        .unwrap();
        let buf = term.backend().buffer().clone();
        (0..buf.area.height)
            .map(|y| {
                (0..buf.area.width)
                    .map(|x| buf[(x, y)].symbol())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn the_dialog_renders_its_field_and_buttons() {
        let mut d = file_dialog();
        type_str(&mut d, "644");
        let screen = render_to(&mut d, 80, 24);
        assert!(screen.contains("644"), "no value: {screen}");
        assert!(screen.contains("[ Apply ]"), "no Apply button: {screen}");
        assert!(screen.contains("[ Cancel ]"), "no Cancel button: {screen}");
    }

    /// The checkbox is drawn only when it is offered, so what is on screen and
    /// what Tab can reach agree.
    #[test]
    fn the_checkbox_is_drawn_only_for_a_directory() {
        let mut d = dir_dialog();
        assert!(render_to(&mut d, 80, 24).contains("Apply to everything inside"));

        let mut d = file_dialog();
        assert!(!render_to(&mut d, 80, 24).contains("Apply to everything inside"));
    }

    /// A tiny terminal must clamp rather than index outside the buffer, and
    /// must not leave click rectangles pointing at cells nobody can see.
    #[test]
    fn the_dialog_survives_a_tiny_terminal() {
        for (w, h) in [(1, 1), (2, 3), (10, 4), (20, 2), (40, 8)] {
            let mut d = dir_dialog();
            let _ = render_to(&mut d, w, h);
        }
    }

    /// A click on the checkbox toggles it; a click on nothing decides nothing.
    #[test]
    fn clicking_the_checkbox_toggles_it() {
        let mut d = dir_dialog();
        let _ = render_to(&mut d, 80, 24);
        let area = d.recursive_area.expect("the checkbox was not drawn");
        assert_eq!(
            d.click_at(area.x + 3, area.y),
            AttrDialogOutcome::Continue
        );
        assert!(d.recursive(), "the click did not toggle the checkbox");
    }

    #[test]
    fn clicking_apply_applies_and_clicking_nothing_does_nothing() {
        let mut d = file_dialog();
        type_str(&mut d, "644");
        let _ = render_to(&mut d, 80, 24);
        let apply = d.button_areas[0];

        // A click well away from every control must not decide anything — this
        // dialog changes file attributes.
        assert_eq!(d.click_at(0, 0), AttrDialogOutcome::Continue);

        assert_eq!(
            d.click_at(apply.x + 1, apply.y),
            AttrDialogOutcome::Apply {
                value: "644".to_string(),
                recursive: false,
            }
        );
    }
}
