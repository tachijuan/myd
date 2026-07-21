use ratatui::{
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, BorderType, Borders, Clear, Paragraph},
    layout::Rect,
    Frame,
};

/// A full-screen progress overlay shown during directory enumeration.
pub struct ProgressOverlay {
    pub message: &'static str,
}

impl Default for ProgressOverlay {
    fn default() -> Self {
        Self::new()
    }
}

impl ProgressOverlay {
    pub fn new() -> Self {
        Self { message: "Loading..." }
    }

    pub fn with_message(mut self, message: &'static str) -> Self {
        self.message = message;
        self
    }

    pub fn render(&self, frame: &mut Frame, area: Rect) {
        // Dark overlay (semi-transparent background via style).
        frame.render_widget(
            Block::default().style(Style::default().bg(Color::Black)),
            area,
        );

        let center = centered(Rect::new(0, 0, 30, 3), area);
        frame.render_widget(Clear, center);

        let text = ratatui::text::Text::from(Line::from(Span::styled(
            self.message,
            Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD),
        )));

        let paragraph = Paragraph::new(text).block(
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
