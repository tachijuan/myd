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
        self.cursor = self.value.len();
        self
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
                if self.cursor < self.value.len() {
                    self.cursor += 1;
                }
                None
            }
            _ => None,
        }
    }

    fn insert_char(&mut self, c: char) {
        self.value.insert(self.cursor, c);
        if self.cursor < self.value.len() {
            self.cursor += 1;
        }
    }

    fn backspace(&mut self) {
        if self.cursor > 0 {
            self.cursor -= 1;
            self.value.remove(self.cursor);
        }
    }

    pub fn render(&self, frame: &mut Frame, area: Rect) {
        // Width is capped to the terminal; the message then wraps to however many
        // lines it needs and the box grows to fit. A fixed height clipped the
        // trailing lines (the Enter/Esc hint), leaving the user with a prompt but
        // no visible way to answer it.
        let width = 60.min(area.width.max(1));
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

        // Byte-safe cursor clamp: the mask uses a multibyte glyph, so slice on a
        // char boundary rather than a raw byte index.
        let cut = display
            .char_indices()
            .nth(self.cursor)
            .map(|(i, _)| i)
            .unwrap_or(display.len());
        let input_line = Line::from(Span::styled(
            format!("{}{}", &display[..cut], "█"),
            Style::default().add_modifier(Modifier::BOLD),
        ));

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
