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
    pub value: String,
    /// Cursor position in the input value.
    cursor: usize,
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
            value: String::new(),
            cursor: 0,
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
        self.value = default.into();
        self.cursor = self.value.chars().count();
        self
    }

    /// Byte offset of char index `n` in `value`, for slicing on a boundary.
    fn byte_at(&self, n: usize) -> usize {
        self.value
            .char_indices()
            .nth(n)
            .map(|(i, _)| i)
            .unwrap_or(self.value.len())
    }

    /// Handle key input. Returns `Some(value)` on submit, `None` if still editing.
    pub fn handle_key(&mut self, key: crossterm::event::KeyEvent) -> Option<String> {
        use crossterm::event::KeyCode;
        match key.code {
            KeyCode::Esc => Some(String::new()), // Cancel.
            KeyCode::Enter => Some(self.value.clone()),
            KeyCode::Char(c) => {
                self.insert_char(c);
                None
            }
            KeyCode::Backspace => {
                self.backspace();
                None
            }
            KeyCode::Left => {
                if self.cursor > 0 {
                    self.cursor -= 1;
                }
                None
            }
            KeyCode::Right => {
                if self.cursor < self.value.chars().count() {
                    self.cursor += 1;
                }
                None
            }
            KeyCode::Home => {
                self.cursor = 0;
                None
            }
            KeyCode::End => {
                self.cursor = self.value.chars().count();
                None
            }
            _ => None,
        }
    }

    fn insert_char(&mut self, c: char) {
        let at = self.byte_at(self.cursor);
        self.value.insert(at, c);
        self.cursor += 1;
    }

    fn backspace(&mut self) {
        if self.cursor > 0 {
            self.cursor -= 1;
            let at = self.byte_at(self.cursor);
            self.value.remove(at);
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
        let display = if self.value.is_empty() && !self.placeholder.is_empty() {
            format!("\x1b[4m{}\x1b[0m", self.placeholder) // Underlined placeholder (visual hint).
        } else if self.masked {
            "•".repeat(self.value.chars().count())
        } else {
            self.value.clone()
        };

        // The value can be longer than the box, so the input line is a window
        // onto it that scrolls to keep the cursor visible — otherwise a long
        // path just ran past the border and the user typed blind.
        //
        // The placeholder is decorated with escapes and is only shown when the
        // value is empty, so scrolling applies to real input only.
        let chars: Vec<char> = display.chars().collect();
        let showing_placeholder = self.value.is_empty() && !self.placeholder.is_empty();
        // One column is reserved for the cursor block.
        let window = inner_width.saturating_sub(1).max(1);
        let cursor = self.cursor.min(chars.len());
        let input_line = if showing_placeholder || chars.len() <= window {
            let cut: String = chars[..cursor.min(chars.len())].iter().collect();
            let rest: String = chars[cursor.min(chars.len())..].iter().collect();
            Line::from(vec![
                Span::styled(
                    format!("{}{}", cut, "█"),
                    Style::default().add_modifier(Modifier::BOLD),
                ),
                Span::styled(rest, Style::default().add_modifier(Modifier::BOLD)),
            ])
        } else {
            // Scroll so the cursor sits inside the window, keeping as much text
            // to its left as fits — typing at the tail of a path then shows the
            // tail, and moving left brings earlier text back into view.
            //
            // Reserve a column for each ellipsis marker so the assembled line
            // never overflows the box and wraps. Budgeting for both up front
            // costs at most one column when only one marker ends up drawn,
            // which is cheaper than a second pass to find a stable width.
            let budget = window.saturating_sub(2).max(1);
            let start = cursor
                .saturating_sub(budget)
                .min(chars.len().saturating_sub(budget));
            let end = (start + budget).min(chars.len());
            let visible = &chars[start..end];
            let rel = cursor - start;
            let before: String = visible[..rel.min(visible.len())].iter().collect();
            let after: String = visible[rel.min(visible.len())..].iter().collect();
            // Ellipsis marks text scrolled off each side.
            let mut spans = Vec::new();
            if start > 0 {
                spans.push(Span::styled("‹", Style::default().fg(Color::DarkGray)));
            }
            spans.push(Span::styled(
                format!("{}{}", before, "█"),
                Style::default().add_modifier(Modifier::BOLD),
            ));
            spans.push(Span::styled(
                after,
                Style::default().add_modifier(Modifier::BOLD),
            ));
            if end < chars.len() {
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
