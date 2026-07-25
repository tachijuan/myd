use ratatui::{
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, BorderType, Borders, Clear, Paragraph},
    Frame,
};

/// What a choice dialog answered with.
///
/// A plain yes/no confirmation reports `Yes`/`No`; a multi-choice dialog reports
/// the letter that was pressed, so the caller can tell "skip" from "cancel the
/// whole batch" — two answers that both mean "don't do this one".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Answer {
    Yes,
    No,
    /// One of the dialog's declared choice letters.
    Choice(char),
}

pub struct ConfirmDialog {
    pub title: &'static str,
    pub message: String,
    cursor: usize,
    /// Letters this dialog accepts instead of y/n. Empty for a yes/no dialog.
    choices: Vec<char>,
}

impl ConfirmDialog {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            title: "Confirm",
            message: message.into(),
            cursor: 0,
            choices: Vec::new(),
        }
    }

    /// Accept these letters rather than y/n.
    ///
    /// Used where "no" isn't a single thing — a move collision can be skipped or
    /// can abandon the rest of the batch, and silently picking one of those
    /// would be a poor guess.
    pub fn with_choices(mut self, choices: &[char]) -> Self {
        self.choices = choices.to_vec();
        self
    }

    /// Handle a key; `None` while the dialog is still waiting for a valid one.
    pub fn handle_key_answer(&mut self, key: char) -> Option<Answer> {
        if !self.choices.is_empty() {
            let lowered = key.to_ascii_lowercase();
            if self.choices.contains(&lowered) {
                return Some(Answer::Choice(lowered));
            }
            // Enter picks the highlighted (first) choice; Esc is handled by the
            // caller, which maps it to the safest option.
            if key == '\n' {
                return self.choices.first().copied().map(Answer::Choice);
            }
            return None;
        }
        match key {
            'y' => Some(Answer::Yes),
            'n' => Some(Answer::No),
            '\n' | ' ' => Some(if self.cursor == 0 { Answer::Yes } else { Answer::No }),
            _ => None,
        }
    }

    /// Yes/no view of [`handle_key_answer`], for the many plain confirmations.
    pub fn handle_key(&mut self, key: char) -> Option<bool> {
        match self.handle_key_answer(key)? {
            Answer::Yes => Some(true),
            Answer::No => Some(false),
            // A multi-choice dialog has no meaningful boolean reading.
            Answer::Choice(_) => None,
        }
    }

    pub fn render(&self, frame: &mut Frame, area: Rect) {
        // Sized to its content: a fixed box clipped long messages (a full path
        // in a collision prompt is easily wider than 50 columns) and left the
        // user answering a question they could only half read.
        let width = 60.min(area.width.max(1));
        let inner_width = width.saturating_sub(2).max(1) as usize;
        let message_lines = wrap_text(&self.message, inner_width);
        // title + blank + message(n) + blank + buttons, plus 2 border rows.
        let height = (message_lines.len() as u16 + 6).min(area.height.max(1));
        let center = centered(Rect::new(0, 0, width, height), area);
        if center.width == 0 || center.height == 0 {
            return;
        }
        frame.render_widget(Clear, center);

        let buttons = if self.choices.is_empty() {
            Line::from(vec![
                if self.cursor == 0 {
                    Span::styled(
                        " [ Yes ] ",
                        Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD),
                    )
                } else {
                    Span::raw("  Yes  ")
                },
                Span::raw("  "),
                if self.cursor == 1 {
                    Span::styled(
                        " [  No  ] ",
                        Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD),
                    )
                } else {
                    Span::raw("   No   ")
                },
            ])
        } else {
            Line::from(Span::styled(
                self.choices
                    .iter()
                    .map(|c| format!("[{}]", c))
                    .collect::<Vec<_>>()
                    .join("  "),
                Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD),
            ))
        };

        let mut lines = vec![
            Line::from(Span::styled(
                self.title,
                Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD),
            )),
            Line::from(""),
        ];
        lines.extend(message_lines.into_iter().map(|l| Line::from(Span::raw(l))));
        lines.push(Line::from(""));
        lines.push(buttons);

        let paragraph = Paragraph::new(ratatui::text::Text::from(lines)).block(
            Block::default()
                .borders(Borders::ALL)
                .border_type(BorderType::Plain)
                .border_style(Style::default().fg(Color::Cyan)),
        );
        frame.render_widget(paragraph, center);
    }
}

/// Wrap `text` to `width` columns, counting chars so multibyte paths wrap at the
/// right column. A word longer than the line is hard-split rather than truncated.
fn wrap_text(text: &str, width: usize) -> Vec<String> {
    if width == 0 {
        return vec![String::new()];
    }
    let mut lines = Vec::new();
    let mut current = String::new();
    let mut current_len = 0usize;
    for word in text.split_whitespace() {
        let word_len = word.chars().count();
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
        let need = if current_len == 0 {
            word_len
        } else {
            current_len + 1 + word_len
        };
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
