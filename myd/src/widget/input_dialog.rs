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
}

impl InputDialog {
    pub fn new(message: impl Into<String>, placeholder: impl Into<String>) -> Self {
        Self {
            title: "Input",
            message: message.into(),
            placeholder: placeholder.into(),
            value: String::new(),
            cursor: 0,
        }
    }

    pub fn with_title(mut self, title: &'static str) -> Self {
        self.title = title;
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
        let center = centered(Rect::new(0, 0, 55, 7), area);
        if center.width == 0 || center.height == 0 {
            return;
        }
        frame.render_widget(Clear, center);

        // Input display with cursor indicator.
        let display = if self.value.is_empty() && !self.placeholder.is_empty() {
            format!("\x1b[4m{}\x1b[0m", self.placeholder) // Underlined placeholder (visual hint).
        } else {
            self.value.clone()
        };

        let input_line = Line::from(Span::styled(
            format!("{}{}", &display[..self.cursor.min(display.len())], "█"),
            Style::default().add_modifier(Modifier::BOLD),
        ));

        let buttons = Line::from(vec![
            Span::styled(" (Enter: OK, Esc: Cancel) ", Style::default().fg(Color::DarkGray)),
        ]);

        let content = Text::from(vec![
            Line::from(Span::styled(self.title, Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD))),
            Line::from(""),
            Line::from(Span::raw(self.message.clone())),
            Line::from(""),
            input_line,
            Line::from(""),
            buttons,
        ]);

        let paragraph = Paragraph::new(content).block(
            Block::default()
                .borders(Borders::ALL)
                .border_type(BorderType::Plain)
                .border_style(Style::default().fg(Color::Cyan)),
        );
        frame.render_widget(paragraph, center);
    }
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
