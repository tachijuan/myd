//! One editable text field, shared by every dialog that takes typed input.
//!
//! Two jobs, and they are separate problems:
//!
//! 1. **Editing semantics** come from [`tui_input`], so the fields answer to the
//!    readline bindings people already have in their fingers from a shell
//!    prompt — `C-a`, `C-e`, `C-w`, `C-u`, `C-k`, `alt-b`/`f`/`d`, word-wise
//!    `C-←`/`C-→`. Before this, `Ctrl+A` in a rename dialog inserted a literal
//!    `a` into the filename, because the handler matched `KeyCode::Char(c)`
//!    without looking at the modifiers.
//!
//! 2. **Rendering** is myd's own, because the cursor used to be drawn by
//!    *splicing a block character into the string*:
//!    `format!("{}█{}", before, after)`. That is one column wider than the text
//!    it describes, so every press of `←` shoved everything right of the cursor
//!    sideways — the reported "characters slide around". Here the cursor is a
//!    styled cell over the character it sits on, so the text never moves.
//!
//! Both halves are width-aware. A CJK name is two columns per character and an
//! emoji may be two as well, so a cursor counted in `chars` drifts away from the
//! glyph it is meant to mark; [`tui_input`] measures in display columns, and the
//! scroll offset below is in the same units.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::style::{Modifier, Style};
use ratatui::text::Span;
use tui_input::backend::crossterm::to_input_request;
use tui_input::{Input, InputRequest};

/// An editable line of text with a cursor.
#[derive(Debug, Default, Clone)]
pub struct TextField {
    input: Input,
}

impl TextField {
    /// An empty field.
    pub fn new() -> Self {
        Self::default()
    }

    /// A field pre-filled with `value`, cursor at the end.
    ///
    /// The end, not the start: a pre-filled name is there so an edit is a
    /// correction rather than a retype, and the common correction is to the
    /// tail — an extension, a trailing digit.
    pub fn with_value(value: impl Into<String>) -> Self {
        Self {
            input: Input::new(value.into()),
        }
    }

    /// The current text.
    pub fn value(&self) -> &str {
        self.input.value()
    }

    /// The cursor's position, counted in characters.
    pub fn cursor(&self) -> usize {
        self.input.cursor()
    }

    /// Replace the text, putting the cursor at the end.
    pub fn set_value(&mut self, value: impl Into<String>) {
        self.input = Input::new(value.into());
    }

    /// Whether the field is empty.
    pub fn is_empty(&self) -> bool {
        self.input.value().is_empty()
    }

    /// Feed a key to the editor, reporting whether it was consumed.
    ///
    /// `false` means the key belongs to the dialog: `Tab` moves focus, `Enter`
    /// accepts, `Esc` cancels. [`to_input_request`] already returns `None` for
    /// all three, so the dialogs keep their contract that only Enter, a click or
    /// a letter decides — Tab never accepts.
    pub fn handle_key(&mut self, key: KeyEvent) -> bool {
        // Ctrl-D deletes forward, as it does at a shell prompt. The crate stops
        // at `Delete` and never maps it, so it is added here rather than left
        // to insert a literal `d` — which is what every unmapped Ctrl chord did
        // before this type existed.
        //
        // Only when there is something to delete. At the end of a line C-d is
        // "end of input" in a shell, and a field that quietly did nothing is
        // better than one that borrows a meaning the dialog cannot honour.
        if key.code == KeyCode::Char('d') && key.modifiers.contains(KeyModifiers::CONTROL) {
            return self.input.handle(InputRequest::DeleteNextChar).is_some();
        }
        match to_input_request(&crossterm::event::Event::Key(key)) {
            Some(req) => {
                self.input.handle(req);
                true
            }
            None => false,
        }
    }

    /// The text scrolled to keep the cursor visible in `width` columns, and the
    /// cursor's offset within that window.
    ///
    /// Returns `(visible_text, cursor_column)`. A name longer than its box used
    /// to be drawn from the start and simply clipped, so typing past the edge
    /// moved the cursor somewhere the user could not see.
    pub fn visible(&self, width: usize) -> (String, usize) {
        if width == 0 {
            return (String::new(), 0);
        }
        let scroll = self.input.visual_scroll(width);
        let cursor = self.input.visual_cursor().saturating_sub(scroll);
        // Walk by display width, not by chars: a two-column glyph consumes two
        // of the columns being budgeted here.
        let mut out = String::new();
        let mut col = 0usize;
        let mut used = 0usize;
        for c in self.input.value().chars() {
            let w = unicode_width_of(c);
            if col + w <= scroll {
                col += w;
                continue;
            }
            if used + w > width {
                break;
            }
            out.push(c);
            used += w;
            col += w;
        }
        (out, cursor.min(width))
    }

    /// The field as spans, with the cursor drawn over the character it marks.
    ///
    /// The cursor is a reversed cell, never a character spliced into the text —
    /// see this module's header. At the end of the line there is no character to
    /// reverse, so a space stands in and gets the same treatment; that keeps the
    /// block the same width wherever it is.
    pub fn spans(&self, width: usize, style: Style, focused: bool) -> Vec<Span<'static>> {
        let (text, cursor) = self.visible(width);
        if !focused {
            return vec![Span::styled(pad(&text, width), style)];
        }
        let cursor_style = style.add_modifier(Modifier::REVERSED);
        let mut before = String::new();
        let mut at = String::new();
        let mut after = String::new();
        let mut col = 0usize;
        for c in text.chars() {
            let w = unicode_width_of(c);
            if col < cursor {
                before.push(c);
            } else if col == cursor && at.is_empty() {
                at.push(c);
            } else {
                after.push(c);
            }
            col += w;
        }
        if at.is_empty() {
            at.push(' ');
        }
        let tail_width = width
            .saturating_sub(before.chars().map(unicode_width_of).sum::<usize>())
            .saturating_sub(at.chars().map(unicode_width_of).sum::<usize>());
        vec![
            Span::styled(before, style),
            Span::styled(at, cursor_style),
            Span::styled(pad(&after, tail_width), style),
        ]
    }
}

/// Display columns for one character, treating an unknown width as one.
///
/// Public because the dialogs budget their own decorations — an ellipsis
/// marker, a leading indent — against the same measure the field uses, and two
/// definitions of "how wide is this" would disagree on a CJK name.
pub fn display_width(c: char) -> usize {
    unicode_width_of(c)
}

/// Display columns for one character, treating an unknown width as one.
fn unicode_width_of(c: char) -> usize {
    unicode_width::UnicodeWidthChar::width(c).unwrap_or(1)
}

/// Right-pad to `width` display columns, so a field's background is the same
/// length whatever it holds.
fn pad(s: &str, width: usize) -> String {
    let w: usize = s.chars().map(unicode_width_of).sum();
    let mut out = s.to_string();
    for _ in w..width {
        out.push(' ');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(c: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE)
    }

    fn ctrl(c: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL)
    }

    fn code(c: KeyCode) -> KeyEvent {
        KeyEvent::new(c, KeyModifiers::NONE)
    }

    fn typed(s: &str) -> TextField {
        let mut f = TextField::new();
        for c in s.chars() {
            assert!(f.handle_key(key(c)), "typing {c:?} should be consumed");
        }
        f
    }

    /// The reported bug: the cursor must not change the text's width.
    ///
    /// It used to be spliced in as a block character, so the rendered line grew
    /// by a column and everything right of the cursor shifted as it moved.
    #[test]
    fn moving_the_cursor_does_not_move_the_text() {
        let mut f = typed("report.txt");
        let width = 20;
        let render = |f: &TextField| -> String {
            f.spans(width, Style::default(), true)
                .iter()
                .map(|s| s.content.as_ref())
                .collect::<String>()
        };
        let at_end = render(&f);
        for _ in 0..5 {
            f.handle_key(code(KeyCode::Left));
            let now = render(&f);
            assert_eq!(
                now, at_end,
                "the text moved when the cursor did: {now:?} vs {at_end:?}"
            );
        }
    }

    /// Every rendered line is exactly the width it was given, cursor or not.
    #[test]
    fn the_rendered_width_is_constant() {
        let mut f = typed("abc");
        for _ in 0..4 {
            for focused in [true, false] {
                let w: usize = f
                    .spans(16, Style::default(), focused)
                    .iter()
                    .flat_map(|s| s.content.chars())
                    .map(unicode_width_of)
                    .sum();
                assert_eq!(w, 16, "a field must fill its box exactly");
            }
            f.handle_key(code(KeyCode::Left));
        }
    }

    #[test]
    fn readline_motions_move_the_cursor() {
        let mut f = typed("hello world");
        f.handle_key(ctrl('a'));
        assert_eq!(f.cursor(), 0, "C-a goes to the start");
        f.handle_key(ctrl('e'));
        assert_eq!(f.cursor(), 11, "C-e goes to the end");
        f.handle_key(ctrl('b'));
        assert_eq!(f.cursor(), 10, "C-b steps back");
        f.handle_key(ctrl('f'));
        assert_eq!(f.cursor(), 11, "C-f steps forward");
    }

    #[test]
    fn readline_deletions_cut_the_right_text() {
        let mut f = typed("hello world");
        f.handle_key(ctrl('w'));
        assert_eq!(f.value(), "hello ", "C-w kills the word behind the cursor");

        let mut f = typed("hello world");
        f.handle_key(ctrl('u'));
        assert_eq!(f.value(), "", "C-u kills the line");

        let mut f = typed("hello world");
        f.handle_key(ctrl('a'));
        f.handle_key(ctrl('k'));
        assert_eq!(f.value(), "", "C-k kills to the end");

        let mut f = typed("ab");
        f.handle_key(ctrl('h'));
        assert_eq!(f.value(), "a", "C-h is backspace");
    }

    /// Ctrl-D deletes forward. The crate has no binding for it, so this is
    /// myd's own and needs its own test.
    #[test]
    fn ctrl_d_deletes_forward() {
        let mut f = typed("abc");
        f.handle_key(ctrl('a'));
        assert!(f.handle_key(ctrl('d')), "C-d should be consumed");
        assert_eq!(f.value(), "bc", "C-d deletes the character under the cursor");
    }

    /// And at the end of the line it does nothing, rather than inserting a `d`
    /// — which is what every unmapped Ctrl chord used to do.
    #[test]
    fn ctrl_d_at_the_end_does_not_type_a_letter() {
        let mut f = typed("abc");
        f.handle_key(ctrl('d'));
        assert_eq!(f.value(), "abc", "nothing to delete, and nothing inserted");
    }

    /// The whole class of bug the old handler had: `KeyCode::Char(c)` matched
    /// without looking at the modifiers, so `Ctrl+A` typed an `a` into the name.
    #[test]
    fn control_chords_never_insert_their_letter() {
        for c in ['a', 'e', 'b', 'f', 'w', 'u', 'k', 'd', 'h'] {
            let mut f = typed("name.txt");
            f.handle_key(ctrl(c));
            assert!(
                !f.value().contains(&format!("{c}{c}")),
                "C-{c} inserted a literal {c}: {:?}",
                f.value()
            );
            assert!(
                f.value().len() <= "name.txt".len(),
                "C-{c} made the value longer: {:?}",
                f.value()
            );
        }
    }

    /// Tab, Enter and Esc belong to the dialog, not the editor.
    ///
    /// The house rule is that Tab moves focus and never accepts, so the field
    /// must decline it rather than swallow it.
    #[test]
    fn the_dialog_keys_are_left_alone() {
        let mut f = typed("x");
        for c in [KeyCode::Tab, KeyCode::Enter, KeyCode::Esc] {
            assert!(!f.handle_key(code(c)), "{c:?} must fall through to the dialog");
        }
        assert_eq!(f.value(), "x", "and none of them changed the text");
    }

    /// A name longer than its box scrolls to follow the cursor, rather than
    /// being clipped from the start with the cursor off-screen.
    #[test]
    fn a_long_value_scrolls_to_keep_the_cursor_visible() {
        let f = typed("a-very-long-file-name-indeed.txt");
        let (text, cursor) = f.visible(10);
        assert!(cursor <= 10, "the cursor must be inside the window");
        assert!(
            text.ends_with(".txt"),
            "the window should follow the cursor to the end: {text:?}"
        );
        let w: usize = text.chars().map(unicode_width_of).sum();
        assert!(w <= 10, "and must not overflow the width: {w}");
    }

    /// Wide characters are measured in columns, not chars.
    #[test]
    fn a_wide_name_is_measured_in_columns() {
        let f = typed("日本語のファイル");
        let (text, cursor) = f.visible(8);
        let w: usize = text.chars().map(unicode_width_of).sum();
        assert!(w <= 8, "four CJK glyphs fill eight columns, not eight glyphs: {w}");
        assert!(cursor <= 8);
    }

    /// Editing a multibyte value must not panic on a byte boundary.
    #[test]
    fn a_multibyte_value_is_editable() {
        let mut f = typed("résumé.txt");
        f.handle_key(ctrl('a'));
        f.handle_key(code(KeyCode::Right));
        f.handle_key(ctrl('d'));
        assert_eq!(f.value(), "rsumé.txt");
        f.handle_key(ctrl('e'));
        f.handle_key(code(KeyCode::Backspace));
        assert_eq!(f.value(), "rsumé.tx");
    }

    #[test]
    fn a_zero_width_box_renders_nothing_and_does_not_panic() {
        let f = typed("abc");
        let (text, cursor) = f.visible(0);
        assert!(text.is_empty());
        assert_eq!(cursor, 0);
        let spans = f.spans(0, Style::default(), true);
        let w: usize = spans.iter().flat_map(|s| s.content.chars()).map(unicode_width_of).sum();
        assert!(w <= 1, "a zero-width field draws at most the cursor cell");
    }
}
