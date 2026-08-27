use crate::widget::text_field::{display_width, TextField};
use ratatui::{
    style::{Color, Modifier, Style},
    text::{Line, Span, Text},
    widgets::{Block, BorderType, Borders, Clear, Paragraph},
    layout::Rect,
    Frame,
};

/// A modal text input dialog.
pub struct InputDialog {
    pub title: &'static str,
    pub message: String,
    pub placeholder: String,
    /// The edited text and its cursor. `TextField` carries the readline
    /// bindings; this dialog keeps its own rendering, which scrolls with
    /// ellipsis markers and can mask the value.
    field: TextField,
    /// When set, the entered text is rendered as dots — for passphrases and
    /// passwords, which must not appear on screen.
    masked: bool,
}

impl InputDialog {
    pub fn new(message: impl Into<String>, placeholder: impl Into<String>) -> Self {
        Self {
            title: "Input",
            message: message.into(),
            placeholder: placeholder.into(),
            field: TextField::new(),
            masked: false,
        }
    }

    pub fn with_title(mut self, title: &'static str) -> Self {
        self.title = title;
        self
    }

    /// Render the entered text as dots — for passphrases and passwords.
    pub fn masked(mut self) -> Self {
        self.masked = true;
        self
    }

    pub fn with_default(mut self, default: impl Into<String>) -> Self {
        self.field = TextField::with_value(default.into());
        self
    }

    /// The text entered so far.
    pub fn value(&self) -> &str {
        self.field.value()
    }

    /// Handle key input. Returns `Some(value)` on submit, `None` if still editing.
    pub fn handle_key(&mut self, key: crossterm::event::KeyEvent) -> Option<String> {
        use crossterm::event::KeyCode;
        match key.code {
            KeyCode::Esc => Some(String::new()), // Cancel.
            KeyCode::Enter => Some(self.field.value().to_string()),
            // Everything else is line editing. `TextField` declines Enter, Esc
            // and Tab, so the two arms above keep their meaning.
            _ => {
                self.field.handle_key(key);
                None
            }
        }
    }

    pub fn render(&self, frame: &mut Frame, area: Rect) {
        // The box grows with the terminal — 80% of the width, between 60 and 120
        // columns — so a long path gets room on a wide window instead of being
        // squeezed into a fixed 60. Width is still capped to the terminal; the
        // message then wraps to however many lines it needs and the box grows to
        // fit. A fixed height clipped the trailing lines (the Enter/Esc hint),
        // leaving the user with a prompt but no visible way to answer it.
        let width = (area.width as u32 * 8 / 10)
            .clamp(60, 120)
            .min(area.width.max(1) as u32) as u16;
        let inner_width = width.saturating_sub(2).max(1) as usize;
        let message_lines = wrap_text(&self.message, inner_width);
        // Content rows: title + blank + message(n) + blank + input + blank +
        // buttons = n + 6. Plus 2 border rows.
        let height = (message_lines.len() as u16 + 8).min(area.height.max(1));
        let center = centered(Rect::new(0, 0, width, height), area);
        if center.width == 0 || center.height == 0 {
            return;
        }
        frame.render_widget(Clear, center);

        // Input display with cursor indicator. A masked field shows dots so a
        // passphrase or password never appears on screen.
        //
        // The placeholder is styled through ratatui rather than with in-band
        // escapes. A literal "\x1b[4m…\x1b[0m" is not interpreted — the buffer
        // stores it as cells — so it drew as visible "[4m" text *and* ate eight
        // columns of the line's width, leaving that much of the row beyond it
        // unpainted and the tree underneath showing through.
        let showing_placeholder = self.field.is_empty() && !self.placeholder.is_empty();
        let display = if showing_placeholder {
            self.placeholder.clone()
        } else if self.masked {
            "•".repeat(self.field.value().chars().count())
        } else {
            self.field.value().to_string()
        };
        // Underlined and dim, so it still reads as a hint rather than as text
        // that has been typed.
        let text_style = if showing_placeholder {
            Style::default()
                .fg(Color::DarkGray)
                .add_modifier(Modifier::UNDERLINED)
        } else {
            Style::default().add_modifier(Modifier::BOLD)
        };

        // The value can be longer than the box, so the input line is a window
        // onto it that scrolls to keep the cursor visible — otherwise a long
        // path just ran past the border and the user typed blind.
        //
        // The cursor is a styled cell over the character it marks, never a block
        // spliced into the string: a spliced cursor is one column wider than the
        // text it describes, so every press of `←` shoved everything to its
        // right sideways. That was the reported "characters slide around", and
        // this dialog kept a copy of it after the other four were converted.
        //
        // Reserve a column for each ellipsis marker so the assembled line never
        // overflows the box and wraps. Budgeting for both up front costs at most
        // one column when only one marker is drawn, which is cheaper than a
        // second pass to find a stable width.
        let total: usize = display.chars().map(display_width).sum();
        let scrolls = !showing_placeholder && total > inner_width;
        let budget = if scrolls {
            inner_width.saturating_sub(2).max(1)
        } else {
            inner_width
        };

        let input_line = if showing_placeholder {
            // The placeholder stands in for an empty value, so the cursor sits
            // at the start of the field: nothing is typed yet, and a cursor
            // after the hint would read as text already entered.
            Line::from(vec![
                Span::styled(
                    " ".to_string(),
                    Style::default().add_modifier(Modifier::REVERSED),
                ),
                Span::styled(display.clone(), text_style),
            ])
        } else {
            let (visible, _) = self.field.visible(budget);
            let shown: usize = visible.chars().map(display_width).sum();
            let scrolled_left = visible.chars().count() < display.chars().count()
                && !display.starts_with(&visible);
            let scrolled_right = shown < total && display.starts_with(&visible);
            let mut spans = Vec::new();
            if scrolled_left {
                spans.push(Span::styled("‹", Style::default().fg(Color::DarkGray)));
            }
            spans.extend(self.field.spans(budget, text_style, true));
            if scrolled_right {
                spans.push(Span::styled("›", Style::default().fg(Color::DarkGray)));
            }
            Line::from(spans)
        };

        let buttons = Line::from(vec![
            Span::styled(" (Enter: OK, Esc: Cancel) ", Style::default().fg(Color::DarkGray)),
        ]);

        let mut lines = vec![
            Line::from(Span::styled(self.title, Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD))),
            Line::from(""),
        ];
        lines.extend(message_lines.into_iter().map(|l| Line::from(Span::raw(l))));
        lines.push(Line::from(""));
        lines.push(input_line);
        lines.push(Line::from(""));
        lines.push(buttons);
        let content = Text::from(lines);

        let paragraph = Paragraph::new(content).block(
            Block::default()
                .borders(Borders::ALL)
                .border_type(BorderType::Plain)
                .border_style(Style::default().fg(Color::Cyan)),
        );
        frame.render_widget(paragraph, center);
    }
}

/// Wrap `text` to `width` columns, breaking at spaces where possible.
///
/// Counts chars rather than bytes so multibyte hostnames and usernames wrap at
/// the right column instead of being cut short. A single word longer than the
/// line (a long user@host with no spaces) is hard-split so it still shows in
/// full rather than being truncated at the border.
fn wrap_text(text: &str, width: usize) -> Vec<String> {
    if width == 0 {
        return vec![String::new()];
    }
    let mut lines = Vec::new();
    let mut current = String::new();
    let mut current_len = 0usize;
    for word in text.split_whitespace() {
        let word_len = word.chars().count();
        // Long unbreakable word: flush, then split it across lines.
        if word_len > width {
            if current_len > 0 {
                lines.push(std::mem::take(&mut current));
                current_len = 0;
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
                current_len = current.chars().count();
            }
            continue;
        }
        let need = if current_len == 0 { word_len } else { current_len + 1 + word_len };
        if need > width {
            lines.push(std::mem::take(&mut current));
            current.push_str(word);
            current_len = word_len;
        } else {
            if current_len > 0 {
                current.push(' ');
            }
            current.push_str(word);
            current_len = need;
        }
    }
    if !current.is_empty() || lines.is_empty() {
        lines.push(current);
    }
    lines
}

/// Center `r` inside `area`, clamped to fit. A dialog larger than the terminal
/// would otherwise land outside the buffer and panic on render.
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

    fn key(c: KeyCode) -> KeyEvent {
        KeyEvent::new(c, KeyModifiers::NONE)
    }

    fn typed(d: &mut InputDialog, s: &str) {
        for c in s.chars() {
            d.handle_key(key(KeyCode::Char(c)));
        }
    }

    /// Render the dialog and return the row holding the edited value.
    fn value_row(d: &InputDialog, w: u16, h: u16) -> String {
        let mut term =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(w, h)).unwrap();
        term.draw(|f| d.render(f, f.area())).unwrap();
        let buf = term.backend().buffer().clone();
        // The value row is the one carrying the typed text.
        (0..h)
            .map(|y| {
                (0..w)
                    .map(|x| buf[(x, y)].symbol())
                    .collect::<String>()
            })
            .find(|line| line.contains("abcdef"))
            .unwrap_or_default()
    }

    /// The reported bug, in the dialog the report was about.
    ///
    /// The cursor used to be spliced into the string as a block character, so
    /// the rendered line was one column wider than the text and everything to
    /// the right of the cursor shifted every time it moved. `TextField` fixed
    /// the other four dialogs; this one kept its own render and kept the bug.
    #[test]
    fn moving_the_cursor_does_not_move_the_text() {
        let mut d = InputDialog::new("Filter:", "pattern");
        typed(&mut d, "abcdef");
        let at_end = value_row(&d, 80, 12);
        for step in 1..=5 {
            d.handle_key(key(KeyCode::Left));
            let now = value_row(&d, 80, 12);
            assert_eq!(
                now.trim_end(),
                at_end.trim_end(),
                "the text moved after {step} left presses:\n{now}\n{at_end}"
            );
        }
    }

    /// And the same for a value long enough to scroll, which is a second
    /// rendering path with its own copy of the splice.
    #[test]
    fn moving_the_cursor_in_a_scrolled_value_does_not_move_the_text() {
        let mut d = InputDialog::new("Filter:", "pattern");
        typed(&mut d, "abcdef-and-then-a-great-deal-more-text-than-fits-in-the-box");
        // A narrow box, so the value is longer than the window.
        let render = |d: &InputDialog| -> String {
            let mut term =
                ratatui::Terminal::new(ratatui::backend::TestBackend::new(64, 12)).unwrap();
            term.draw(|f| d.render(f, f.area())).unwrap();
            let buf = term.backend().buffer().clone();
            (0..12)
                .map(|y| (0..64).map(|x| buf[(x, y)].symbol()).collect::<String>())
                .find(|l| l.contains("text") || l.contains("more"))
                .unwrap_or_default()
        };
        let start = render(&d);
        for _ in 0..3 {
            d.handle_key(key(KeyCode::Left));
        }
        let moved = render(&d);
        assert_eq!(
            start.chars().filter(|c| !c.is_whitespace()).count(),
            moved.chars().filter(|c| !c.is_whitespace()).count(),
            "the visible width changed when the cursor moved:\n{start}\n{moved}"
        );
    }

    /// The placeholder is a hint, not text: the cursor belongs at the start of
    /// the field rather than after the hint.
    #[test]
    fn the_placeholder_keeps_the_cursor_at_the_start() {
        let d = InputDialog::new("Filter:", "pattern");
        let mut term =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(80, 12)).unwrap();
        term.draw(|f| d.render(f, f.area())).unwrap();
        let buf = term.backend().buffer().clone();
        let line = (0..12)
            .map(|y| (0..80).map(|x| buf[(x, y)].symbol()).collect::<String>())
            .find(|l| l.contains("pattern"))
            .expect("the placeholder should be shown");
        let hint = line.find("pattern").unwrap();
        // Whatever marks the cursor sits before the hint, never after it.
        assert!(
            line[..hint].contains(' '),
            "the cursor must not sit after the placeholder: {line:?}"
        );
    }

    /// Readline editing reaches this dialog too.
    #[test]
    fn the_shell_editing_keys_work_here() {
        let mut d = InputDialog::new("Filter:", "pattern");
        typed(&mut d, "hello world");
        d.handle_key(KeyEvent::new(KeyCode::Char('w'), KeyModifiers::CONTROL));
        assert_eq!(d.value(), "hello ");
        d.handle_key(KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL));
        assert_eq!(d.value(), "");
    }
}
