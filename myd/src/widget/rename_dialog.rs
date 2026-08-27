//! Patterned rename: a regex and a replacement, applied to every tagged file.
//!
//! Renaming a batch by hand is the tedium this removes — `IMG_(\d+)` to
//! `holiday-$1` across forty files. The dialog is two fields and a live preview
//! of what the first tagged file would become, so the patterns can be corrected
//! before anything is renamed rather than after.
//!
//! The preview is the whole point: a regex that compiles is not the same as a
//! regex that does what you meant, and a rename batch is not something you want
//! to discover was wrong afterwards.

use crate::widget::text_field::TextField;
use ratatui::{
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span, Text},
    widgets::{Block, BorderType, Borders, Clear, Paragraph},
    Frame,
};

/// Which field has the keyboard.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Field {
    Pattern,
    Replacement,
}

/// What the dialog decided.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RenameDialogOutcome {
    /// Still editing.
    Continue,
    /// Dismissed; nothing is renamed.
    Cancelled,
    /// Apply this pattern and replacement to the tagged files.
    Apply { pattern: String, replacement: String },
}

/// How the current patterns look when applied to the sample name.
///
/// Held as a computed value rather than recalculated during render, so the
/// renderer stays free of the regex work and the same answer drives both the
/// preview line and whether Enter is allowed to apply.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Preview {
    /// No pattern typed yet — nothing to say.
    Empty,
    /// The pattern does not compile; carries the reason.
    BadPattern(String),
    /// Compiles, but does not match the sample.
    NoMatch,
    /// The sample would become this.
    Renamed(String),
}

/// An open patterned-rename dialog.
pub struct RenameDialog {
    pattern: TextField,
    replacement: TextField,
    focus: Field,
    /// The name previewed against — the first tagged file.
    sample: String,
    /// How many files the patterns would be applied to, for the header.
    count: usize,
    preview: Preview,
    /// Where each field was last drawn, so a click can focus it.
    field_areas: Vec<Rect>,
}

impl RenameDialog {
    /// Build a dialog previewing against `sample`, one of `count` tagged files.
    pub fn new(sample: impl Into<String>, count: usize) -> Self {
        let mut d = Self {
            pattern: TextField::new(),
            replacement: TextField::new(),
            focus: Field::Pattern,
            sample: sample.into(),
            count,
            preview: Preview::Empty,
            field_areas: Vec::new(),
        };
        d.recompute();
        d
    }

    pub fn pattern(&self) -> &str {
        self.pattern.value()
    }

    pub fn replacement(&self) -> &str {
        self.replacement.value()
    }

    pub fn focus(&self) -> Field {
        self.focus
    }

    pub fn preview(&self) -> &Preview {
        &self.preview
    }

    /// Whether the current patterns could be applied.
    ///
    /// A pattern that does not compile, or matches nothing, is not an
    /// application — Enter reports the problem instead, leaving the text in
    /// place to be corrected.
    pub fn is_applicable(&self) -> bool {
        matches!(self.preview, Preview::Renamed(_))
    }

    /// Recompute the preview from the current fields.
    fn recompute(&mut self) {
        if self.pattern.is_empty() {
            self.preview = Preview::Empty;
            return;
        }
        self.preview = match regex::Regex::new(self.pattern.value()) {
            // The message is reported as-is: regex's own errors name the
            // position and the construct, which is more use than "invalid
            // pattern" would be.
            Err(e) => Preview::BadPattern(first_line(&e.to_string())),
            Ok(re) => {
                if !re.is_match(&self.sample) {
                    Preview::NoMatch
                } else {
                    Preview::Renamed(re.replace_all(&self.sample, self.replacement.value()).into_owned())
                }
            }
        }
    }

    fn active_mut(&mut self) -> &mut TextField {
        match self.focus {
            Field::Pattern => &mut self.pattern,
            Field::Replacement => &mut self.replacement,
        }
    }

    /// Move the focus to `field`, putting the cursor at the end of its text.
    fn focus_field(&mut self, field: Field) {
        self.focus = field;
        // The field keeps its own cursor, so focus does not move it.
    }

    pub fn handle_key(&mut self, key: crossterm::event::KeyEvent) -> RenameDialogOutcome {
        use crossterm::event::KeyCode;

        match key.code {
            KeyCode::Esc => RenameDialogOutcome::Cancelled,
            // Tab moves between the fields and never applies, as in every other
            // dialog here: reaching for the other field must not be read as
            // consent to rename a batch.
            KeyCode::Tab | KeyCode::Down => {
                self.focus_field(match self.focus {
                    Field::Pattern => Field::Replacement,
                    Field::Replacement => Field::Pattern,
                });
                RenameDialogOutcome::Continue
            }
            KeyCode::BackTab | KeyCode::Up => {
                self.focus_field(match self.focus {
                    Field::Pattern => Field::Replacement,
                    Field::Replacement => Field::Pattern,
                });
                RenameDialogOutcome::Continue
            }
            KeyCode::Enter => {
                // Only a preview that resolved to a name can be applied. The
                // caller turns the other cases into a message and hands the
                // dialog back, so the patterns can be fixed rather than retyped.
                if self.is_applicable() {
                    RenameDialogOutcome::Apply {
                        pattern: self.pattern.value().to_string(),
                        replacement: self.replacement.value().to_string(),
                    }
                } else {
                    RenameDialogOutcome::Continue
                }
            }
            // Everything else is line editing: the field answers to the
            // readline bindings and reports whether it took the key. Enter,
            // Esc and Tab are handled above and `TextField` declines them, so
            // the dialog's own contract is unchanged.
            _ => {
                if self.active_mut().handle_key(key) {
                    self.recompute();
                }
                RenameDialogOutcome::Continue
            }
        }
    }

    /// Focus whichever field was clicked. Clicks elsewhere do nothing: this is
    /// a form, and a stray click must not apply it.
    pub fn click_at(&mut self, x: u16, y: u16) -> RenameDialogOutcome {
        for (i, r) in self.field_areas.iter().enumerate() {
            if x >= r.x && x < r.x + r.width && y >= r.y && y < r.y + r.height {
                let field = if i == 0 {
                    Field::Pattern
                } else {
                    Field::Replacement
                };
                self.focus_field(field);
                return RenameDialogOutcome::Continue;
            }
        }
        RenameDialogOutcome::Continue
    }

    pub fn render(&mut self, frame: &mut Frame, area: Rect) {
        let bg = Color::Rgb(24, 24, 34);
        let width = 72.min(area.width.max(1));
        let inner = width.saturating_sub(2).max(1) as usize;
        // title + blank + 2 labelled fields (2 rows each) + blank + sample +
        // arrow + result + blank + hint, plus borders.
        let height = 15.min(area.height.max(1));
        let center = centered(Rect::new(0, 0, width, height), area);
        if center.width == 0 || center.height == 0 {
            return;
        }
        frame.render_widget(Clear, center);

        let dim = Style::default().fg(Color::Rgb(150, 150, 170)).bg(bg);
        let normal = Style::default().fg(Color::Rgb(235, 235, 245)).bg(bg);

        let mut lines: Vec<Line> = Vec::new();
        lines.push(Line::from(Span::styled(
            format!(
                " Renaming {} tagged file{}",
                self.count,
                if self.count == 1 { "" } else { "s" }
            ),
            dim,
        )));
        lines.push(Line::from(Span::styled(String::new(), normal)));

        // Two fields. The focused one shows a block cursor; both are drawn the
        // same otherwise so the focus is the only difference the eye has to find.
        self.field_areas.clear();
        for (i, (label, value, field)) in [
            (" Match (regex)", &self.pattern, Field::Pattern),
            (" Replace with", &self.replacement, Field::Replacement),
        ]
        .into_iter()
        .enumerate()
        {
            let focused = self.focus == field;
            lines.push(Line::from(Span::styled(
                label.to_string(),
                if focused {
                    Style::default()
                        .fg(Color::Rgb(120, 220, 255))
                        .bg(bg)
                        .add_modifier(Modifier::BOLD)
                } else {
                    dim
                },
            )));
            // Row index within the box: border + title + blank + (2 rows per
            // field) + 1 for the value row.
            let row = center.y + 1 + 2 + (i as u16 * 2) + 1;
            self.field_areas
                .push(Rect::new(center.x + 1, row, width.saturating_sub(2), 1));
            // Two columns of indent, then the field. The cursor is a styled
            // cell rather than a character spliced into the text, so moving it
            // does not shift what is to its right — see `TextField`.
            let style = if focused { normal.add_modifier(Modifier::BOLD) } else { normal };
            let mut spans = vec![Span::styled("  ".to_string(), style)];
            spans.extend(value.spans(inner.saturating_sub(2), style, focused));
            lines.push(Line::from(spans));
        }

        lines.push(Line::from(Span::styled(String::new(), normal)));
        lines.push(Line::from(Span::styled(
            format!("  {}", truncate(&self.sample, inner.saturating_sub(2))),
            dim,
        )));

        // The preview line carries the whole state of the patterns: what the
        // name becomes, or why it cannot.
        let (arrow, style) = match &self.preview {
            Preview::Empty => (
                "  (type a pattern to see the result)".to_string(),
                dim,
            ),
            Preview::BadPattern(e) => (
                format!("  ✗ {}", truncate(e, inner.saturating_sub(4))),
                Style::default().fg(Color::Rgb(255, 120, 120)).bg(bg),
            ),
            Preview::NoMatch => (
                "  ✗ pattern does not match this name".to_string(),
                Style::default().fg(Color::Rgb(255, 120, 120)).bg(bg),
            ),
            Preview::Renamed(name) => (
                format!("→ {}", truncate(name, inner.saturating_sub(4))),
                Style::default()
                    .fg(Color::Rgb(140, 230, 140))
                    .bg(bg)
                    .add_modifier(Modifier::BOLD),
            ),
        };
        lines.push(Line::from(Span::styled(pad_to(&arrow, inner), style)));
        lines.push(Line::from(Span::styled(String::new(), normal)));
        lines.push(Line::from(Span::styled(
            " Tab: field   Enter: rename   Esc: cancel".to_string(),
            dim,
        )));

        let paragraph = Paragraph::new(Text::from(lines))
            .style(Style::default().bg(bg))
            .block(
                Block::default()
                    .title(Span::styled(
                        " Patterned rename ",
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

/// Apply `pattern` -> `replacement` to `name`.
///
/// Shared by the dialog's preview and the rename itself, so what was shown is
/// by construction what gets done — the preview cannot promise one name and the
/// batch produce another.
pub fn apply_pattern(
    pattern: &str,
    replacement: &str,
    name: &str,
) -> Result<Option<String>, String> {
    let re = regex::Regex::new(pattern).map_err(|e| first_line(&e.to_string()))?;
    if !re.is_match(name) {
        return Ok(None);
    }
    Ok(Some(re.replace_all(name, replacement).into_owned()))
}

/// The first line of a multi-line error, for a one-line display.
fn first_line(s: &str) -> String {
    s.lines()
        .find(|l| !l.trim().is_empty())
        .unwrap_or(s)
        .trim()
        .to_string()
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
    Rect::new(x, y, inner.width.min(outer.width), inner.height.min(outer.height))
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

    fn type_str(d: &mut RenameDialog, s: &str) {
        for c in s.chars() {
            d.handle_key(key(c));
        }
    }

    #[test]
    fn the_preview_follows_the_patterns() {
        let mut d = RenameDialog::new("IMG_0042.jpg", 3);
        assert_eq!(*d.preview(), Preview::Empty, "nothing typed yet");

        type_str(&mut d, r"IMG_(\d+)");
        // Matches, and with an empty replacement the matched part disappears.
        assert_eq!(*d.preview(), Preview::Renamed(".jpg".to_string()));

        d.handle_key(code(KeyCode::Tab));
        type_str(&mut d, "holiday-$1");
        assert_eq!(
            *d.preview(),
            Preview::Renamed("holiday-0042.jpg".to_string())
        );
        assert!(d.is_applicable());
    }

    #[test]
    fn a_pattern_that_does_not_compile_is_reported_not_applied() {
        let mut d = RenameDialog::new("a.txt", 1);
        type_str(&mut d, "(unclosed");
        assert!(
            matches!(d.preview(), Preview::BadPattern(_)),
            "got {:?}",
            d.preview()
        );
        assert!(!d.is_applicable(), "a broken pattern cannot be applied");
        // Enter is inert rather than destructive, leaving the text to be fixed.
        assert_eq!(
            d.handle_key(code(KeyCode::Enter)),
            RenameDialogOutcome::Continue
        );
        assert_eq!(d.pattern(), "(unclosed", "the text is still there to correct");
    }

    #[test]
    fn a_pattern_that_matches_nothing_is_not_applicable() {
        let mut d = RenameDialog::new("a.txt", 1);
        type_str(&mut d, "zzz");
        assert_eq!(*d.preview(), Preview::NoMatch);
        assert!(!d.is_applicable());
        assert_eq!(
            d.handle_key(code(KeyCode::Enter)),
            RenameDialogOutcome::Continue
        );
    }

    #[test]
    fn tab_moves_between_fields_and_never_applies() {
        // The dialog contract: only Enter, a click, or a letter decides. Tab
        // reaching for the other field must not rename a batch.
        let mut d = RenameDialog::new("a.txt", 1);
        type_str(&mut d, "a");
        assert_eq!(d.focus(), Field::Pattern);
        assert_eq!(
            d.handle_key(code(KeyCode::Tab)),
            RenameDialogOutcome::Continue,
            "Tab must not apply"
        );
        assert_eq!(d.focus(), Field::Replacement);
        type_str(&mut d, "b");
        assert_eq!(d.replacement(), "b", "typing lands in the focused field");
        assert_eq!(d.pattern(), "a", "and not in the other one");
        d.handle_key(code(KeyCode::Tab));
        assert_eq!(d.focus(), Field::Pattern, "Tab wraps back");
    }

    #[test]
    fn enter_applies_only_a_working_pattern() {
        let mut d = RenameDialog::new("IMG_1.jpg", 2);
        type_str(&mut d, "IMG");
        d.handle_key(code(KeyCode::Tab));
        type_str(&mut d, "PIC");
        assert_eq!(
            d.handle_key(code(KeyCode::Enter)),
            RenameDialogOutcome::Apply {
                pattern: "IMG".to_string(),
                replacement: "PIC".to_string(),
            }
        );
    }

    #[test]
    fn esc_cancels_from_either_field() {
        for tabs in [0, 1] {
            let mut d = RenameDialog::new("a.txt", 1);
            for _ in 0..tabs {
                d.handle_key(code(KeyCode::Tab));
            }
            assert_eq!(
                d.handle_key(code(KeyCode::Esc)),
                RenameDialogOutcome::Cancelled
            );
        }
    }

    #[test]
    fn editing_keys_work_on_the_focused_field() {
        let mut d = RenameDialog::new("a.txt", 1);
        type_str(&mut d, "abc");
        d.handle_key(code(KeyCode::Backspace));
        assert_eq!(d.pattern(), "ab");
        d.handle_key(code(KeyCode::Left));
        d.handle_key(key('X'));
        assert_eq!(d.pattern(), "aXb", "insert lands at the cursor");
        d.handle_key(code(KeyCode::Home));
        d.handle_key(code(KeyCode::Delete));
        assert_eq!(d.pattern(), "Xb", "Delete removes forwards");
    }

    #[test]
    fn a_multibyte_pattern_does_not_panic() {
        // These fields take pasted text, and slicing a regex by char index as
        // though it were bytes would split a codepoint.
        let mut d = RenameDialog::new("café.txt", 1);
        type_str(&mut d, "café");
        assert_eq!(d.pattern(), "café");
        d.handle_key(code(KeyCode::Left));
        d.handle_key(key('x'));
        assert_eq!(d.pattern(), "cafxé");
        d.handle_key(code(KeyCode::Backspace));
        assert_eq!(d.pattern(), "café");
    }

    #[test]
    fn apply_pattern_agrees_with_the_preview() {
        // The preview and the rename must not be able to disagree; they share
        // this function precisely so they cannot.
        let mut d = RenameDialog::new("IMG_7.jpg", 1);
        type_str(&mut d, r"IMG_(\d+)");
        d.handle_key(code(KeyCode::Tab));
        type_str(&mut d, "shot-$1");
        let Preview::Renamed(shown) = d.preview().clone() else {
            panic!("expected a rendered preview, got {:?}", d.preview());
        };
        let done = apply_pattern(d.pattern(), d.replacement(), "IMG_7.jpg").unwrap();
        assert_eq!(done, Some(shown));
    }

    #[test]
    fn clicking_a_field_focuses_it() {
        let mut d = RenameDialog::new("a.txt", 1);
        let backend = ratatui::backend::TestBackend::new(100, 30);
        let mut term = ratatui::Terminal::new(backend).unwrap();
        term.draw(|f| d.render(f, f.area())).unwrap();

        let second = d.field_areas[1];
        assert_eq!(
            d.click_at(second.x + 2, second.y),
            RenameDialogOutcome::Continue,
            "a click focuses, it does not apply"
        );
        assert_eq!(d.focus(), Field::Replacement);

        let first = d.field_areas[0];
        d.click_at(first.x + 2, first.y);
        assert_eq!(d.focus(), Field::Pattern);
    }
}
