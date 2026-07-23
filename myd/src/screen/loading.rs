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
use crate::utils::sizes::CancelToken;
use crate::widget::file_tree::FileTree;

/// State for a loading screen shown while a directory tree is being built.
pub struct LoadingState {
    pub path: PathBuf,
    /// Channel to receive the built tree. `None` arrives if the scan was
    /// cancelled before finishing.
    pub rx: oneshot::Receiver<Option<FileTree>>,
    /// Tripped to abort the background scan when the user cancels.
    cancel: CancelToken,
    /// When loading started (for elapsed time display).
    pub started: Instant,
    /// Spinner frame index (0-based, incremented each render).
    pub spinner: usize,
}

impl LoadingState {
    pub fn new(path: PathBuf, rx: oneshot::Receiver<Option<FileTree>>, cancel: CancelToken) -> Self {
        Self {
            path,
            rx,
            cancel,
            started: Instant::now(),
            spinner: 0,
        }
    }

    /// Signal the background scan to stop. The walk observes this cooperatively
    /// and abandons its work within a few directory entries.
    pub fn cancel(&self) {
        self.cancel.cancel();
    }

    /// Advance the loading state, reporting whether the scan is still running,
    /// finished with a tree, or was cancelled.
    pub fn poll(&mut self) -> LoadingPoll {
        match self.rx.try_recv() {
            Ok(Some(tree)) => {
                self.spinner = 0;
                LoadingPoll::Done(tree)
            }
            Ok(None) => {
                self.spinner = 0;
                LoadingPoll::Cancelled
            }
            Err(_) => {
                self.spinner += 1;
                LoadingPoll::Pending
            }
        }
    }
}

/// Outcome of polling a [`LoadingState`].
pub enum LoadingPoll {
    /// Still scanning.
    Pending,
    /// Scan finished; here is the tree.
    Done(FileTree),
    /// Scan was cancelled before finishing.
    Cancelled,
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
            Line::from(vec![Span::raw("    Press q or Esc to cancel")]),
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
