use ratatui::{
    style::{Color, Modifier, Style},
    text::{Line, Span, Text},
    widgets::{Block, BorderType, Borders, Clear, Paragraph},
    layout::Rect,
    Frame,
};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;

/// Live progress shared between a background worker and the render loop.
///
/// The worker bumps the counters as it goes; the UI reads them each frame to
/// draw a running total. All fields are atomics behind an `Arc` so the handle
/// can be cloned into the spawned task cheaply and read without locking.
#[derive(Clone, Debug, Default)]
pub struct OpProgress {
    inner: Arc<OpProgressInner>,
}

#[derive(Debug, Default)]
struct OpProgressInner {
    /// Items processed so far (files + directories, as the op defines them).
    done: AtomicU64,
    /// Total items to process, if known up front (0 = unknown / indeterminate).
    total: AtomicU64,
    /// Files seen (used by the scan counter).
    files: AtomicU64,
    /// Directories seen (used by the scan counter).
    dirs: AtomicU64,
    /// Combined bytes seen (used by the scan counter).
    bytes: AtomicU64,
    /// Set when the worker has finished (lets the UI stop early if it wants).
    finished: AtomicBool,
}

impl OpProgress {
    pub fn new() -> Self {
        Self::default()
    }

    /// Record the total number of items the operation will process.
    pub fn set_total(&self, total: u64) {
        self.inner.total.store(total, Ordering::Relaxed);
    }

    /// Mark one more item done.
    pub fn inc_done(&self) {
        self.inner.done.fetch_add(1, Ordering::Relaxed);
    }

    /// Add to the running file / directory / byte tallies (scan progress).
    pub fn add_file(&self, bytes: u64) {
        self.inner.files.fetch_add(1, Ordering::Relaxed);
        self.inner.bytes.fetch_add(bytes, Ordering::Relaxed);
    }

    pub fn add_dir(&self) {
        self.inner.dirs.fetch_add(1, Ordering::Relaxed);
    }

    /// Signal completion.
    pub fn finish(&self) {
        self.inner.finished.store(true, Ordering::Relaxed);
    }

    pub fn is_finished(&self) -> bool {
        self.inner.finished.load(Ordering::Relaxed)
    }

    pub fn done(&self) -> u64 {
        self.inner.done.load(Ordering::Relaxed)
    }
    pub fn total(&self) -> u64 {
        self.inner.total.load(Ordering::Relaxed)
    }
    pub fn files(&self) -> u64 {
        self.inner.files.load(Ordering::Relaxed)
    }
    pub fn dirs(&self) -> u64 {
        self.inner.dirs.load(Ordering::Relaxed)
    }
    pub fn bytes(&self) -> u64 {
        self.inner.bytes.load(Ordering::Relaxed)
    }
}

/// A full-screen progress overlay. Renders either a plain message or, when a
/// progress body is supplied, a titled box with live counts.
pub struct ProgressOverlay {
    title: String,
    body: Vec<String>,
}

impl Default for ProgressOverlay {
    fn default() -> Self {
        Self::new()
    }
}

impl ProgressOverlay {
    pub fn new() -> Self {
        Self {
            title: "Loading...".to_string(),
            body: Vec::new(),
        }
    }

    /// A single-line overlay (back-compat with the old static-message form).
    pub fn with_message(mut self, message: impl Into<String>) -> Self {
        self.title = message.into();
        self
    }

    /// Extra lines shown below the title (e.g. live counts).
    pub fn with_body(mut self, body: Vec<String>) -> Self {
        self.body = body;
        self
    }

    /// Build the copy/delete progress overlay from shared progress: an
    /// "N / M items" line plus a simple bar.
    pub fn for_operation(verb: &str, progress: &OpProgress) -> Self {
        let done = progress.done();
        let total = progress.total();
        let mut body = Vec::new();
        if total > 0 {
            body.push(format!("{} / {} items", done, total));
            body.push(progress_bar(done, total));
        } else {
            body.push(format!("{} items", done));
        }
        Self {
            title: format!("{}...", verb),
            body,
        }
    }

    /// Build the scan overlay: running files / dirs / combined size.
    pub fn for_scan(progress: &OpProgress) -> Self {
        let files = progress.files();
        let dirs = progress.dirs();
        let bytes = progress.bytes();
        Self {
            title: "Scanning...".to_string(),
            body: vec![
                format!("{} files   {} dirs", files, dirs),
                format!("{} used", crate::utils::sizes::format_size(bytes)),
            ],
        }
    }

    pub fn render(&self, frame: &mut Frame, area: Rect) {
        // Dark overlay (semi-transparent background via style).
        frame.render_widget(
            Block::default().style(Style::default().bg(Color::Black)),
            area,
        );

        // Size the box to the widest line and the number of body rows. Count
        // display characters, not bytes — the progress bar uses multibyte
        // block glyphs, so `str::len` would massively overestimate the width.
        let width = self
            .body
            .iter()
            .map(|l| l.chars().count())
            .chain(std::iter::once(self.title.chars().count()))
            .max()
            .unwrap_or(10)
            .clamp(20, 56) as u16
            + 4;
        let height = 2 + 1 + self.body.len() as u16 + if self.body.is_empty() { 0 } else { 1 };

        let center = centered(Rect::new(0, 0, width, height), area);
        frame.render_widget(Clear, center);

        let mut lines = vec![Line::from(Span::styled(
            self.title.clone(),
            Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD),
        ))];
        if !self.body.is_empty() {
            lines.push(Line::from(""));
            for b in &self.body {
                lines.push(Line::from(Span::styled(
                    b.clone(),
                    Style::default().fg(Color::White),
                )));
            }
        }

        let paragraph = Paragraph::new(Text::from(lines)).block(
            Block::default()
                .borders(Borders::ALL)
                .border_type(BorderType::Plain)
                .border_style(Style::default().fg(Color::Cyan)),
        );
        frame.render_widget(paragraph, center);
    }
}

/// A 20-cell text progress bar for a done/total ratio.
fn progress_bar(done: u64, total: u64) -> String {
    let width = 20usize;
    let ratio = if total > 0 {
        (done as f64 / total as f64).clamp(0.0, 1.0)
    } else {
        0.0
    };
    let filled = (ratio * width as f64) as usize;
    format!("[{}{}]", "█".repeat(filled), "░".repeat(width - filled))
}

/// Center `r` inside `area`, clamped to fit. A box larger than the terminal
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
