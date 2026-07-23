use ratatui::{
    Frame,
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, BorderType, Borders, Clear, Paragraph},
};
use std::path::PathBuf;
use std::time::Instant;
use tokio::sync::oneshot;

use super::ScreenState;
use crate::widget::file_tree::FileTree;

/// State for a loading screen shown while a directory tree is being built.
pub struct LoadingState {
    pub path: PathBuf,
    /// Channel to receive the built tree.
    pub rx: oneshot::Receiver<FileTree>,
    /// When loading started (for elapsed time display).
    pub started: Instant,
    /// Spinner frame index (0-based, incremented each render).
    pub spinner: usize,
}

impl LoadingState {
    pub fn new(path: PathBuf, rx: oneshot::Receiver<FileTree>) -> Self {
        Self {
            path,
            rx,
            started: Instant::now(),
            spinner: 0,
        }
    }

    /// Check if the background task has completed.
    /// Returns `Some(tree)` if done, `None` if still loading.
    pub fn poll(&mut self) -> Option<FileTree> {
        if let Ok(tree) = self.rx.try_recv() {
            self.spinner = 0;
            Some(tree)
        } else {
            self.spinner += 1;
            None
        }
    }
}

impl ScreenState for LoadingState {
    fn render(&mut self, frame: &mut Frame, area: Rect) {
        // Dark overlay.
        frame.render_widget(
            Block::default().style(Style::default().bg(Color::Rgb(10, 10, 20))),
            area,
        );

        let spinners = ['┤', '┘', '┴', '┙', '┷', '┻', '┹', '┺', '┸', '├'];
        let spin_char = spinners[self.spinner % spinners.len()];
        let elapsed = self.started.elapsed();
        let time_str = if elapsed.as_secs() < 1 {
            format!("{}ms", elapsed.as_millis())
        } else {
            format!("{:.1}s", elapsed.as_secs_f64())
        };

        // Clamp the dialog to the available area — a terminal smaller than the
        // dialog would otherwise put it outside the buffer and panic on render.
        let dialog_w = 50.min(area.width);
        let dialog_h = 5.min(area.height);
        let center = Rect::new(
            area.x + area.width.saturating_sub(dialog_w) / 2,
            area.y + area.height.saturating_sub(dialog_h) / 2,
            dialog_w,
            dialog_h,
        );

        if center.width == 0 || center.height == 0 {
            return;
        }

        frame.render_widget(Clear, center);

        let lines = vec![
            Line::from(""),
            Line::from(vec![
                Span::styled(
                    format!(" {} Loading...", spin_char),
                    Style::default()
                        .fg(Color::Yellow)
                        .add_modifier(Modifier::BOLD),
                ),
            ]),
            Line::from(vec![Span::raw(format!(
                "    Scanning: {} ({})",
                self.path.display(),
                time_str
            ))]),
            Line::from(vec![Span::raw("    Press Esc to cancel")]),
            Line::from(""),
        ];

        let paragraph = Paragraph::new(lines).block(
            Block::default()
                .borders(Borders::ALL)
                .border_type(BorderType::Plain)
                .border_style(Style::default().fg(Color::Cyan)),
        );
        frame.render_widget(paragraph, center);
    }
}
