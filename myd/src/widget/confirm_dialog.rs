use ratatui::{
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, BorderType, Borders, Clear, Paragraph},
    layout::Rect,
    Frame,
};

pub struct ConfirmDialog {
    pub title: &'static str,
    pub message: String,
    cursor: usize,
}

impl ConfirmDialog {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            title: "Confirm",
            message: message.into(),
            cursor: 0,
        }
    }

    pub fn handle_key(&mut self, key: char) -> Option<bool> {
        match key {
            'y' => Some(true),
            'n' => Some(false),
            '\n' | ' ' => Some(self.cursor == 0),
            _ => None,
        }
    }

    pub fn render(&self, frame: &mut Frame, area: Rect) {
        let center = centered(Rect::new(0, 0, 50, 7), area);
        frame.render_widget(Clear, center);

        let msg_line = Line::from(self.message.split('\n').map(|s| Span::raw(s.to_string())).collect::<Vec<_>>());

        let buttons = Line::from(vec![
            if self.cursor == 0 {
                Span::styled(" [ Yes ] ", Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD))
            } else {
                Span::raw("  Yes  ")
            },
            Span::raw("  "),
            if self.cursor == 1 {
                Span::styled(" [  No  ] ", Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD))
            } else {
                Span::raw("   No   ")
            },
        ]);

        let content = ratatui::text::Text::from(vec![
            Line::from(Span::styled(self.title, Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD))),
            Line::from(""),
            msg_line,
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

fn centered(r: Rect, area: Rect) -> Rect {
    let x = area.width.saturating_sub(r.width) / 2;
    let y = area.height.saturating_sub(r.height) / 2;
    Rect::new(x, y, r.width, r.height)
}
