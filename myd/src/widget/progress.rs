use ratatui::{
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span, Text},
    widgets::{Block, BorderType, Borders, Clear, Paragraph},
    Frame,
};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

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
    /// Bytes copied so far, across every file in the operation.
    ///
    /// Item counts alone cannot describe a large copy: one 8 GB file is a
    /// single item, so a `done / total` bar sits at 0/1 for the whole transfer
    /// and then jumps to 1/1. Bytes move continuously, so this is what the bar
    /// is actually drawn from whenever a total is known.
    bytes_done: AtomicU64,
    /// Total bytes the operation expects to move (0 = unknown).
    bytes_total: AtomicU64,
    /// Bytes copied of the file currently in flight, and its size. Together
    /// these give the per-file bar, which is the one that moves when a single
    /// enormous file is the entire operation.
    file_done: AtomicU64,
    file_total: AtomicU64,
    /// The name of the file currently being copied. A `Mutex<String>` rather
    /// than an atomic because it is written once per file — orders of magnitude
    /// less often than the byte counters — and read once per frame.
    current: Mutex<String>,
    /// When the byte counters started moving, for the rate and ETA. Set on the
    /// first `add_bytes` rather than at construction so the walk that computes
    /// the total is not counted as transfer time.
    started: Mutex<Option<Instant>>,
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

    /// Record the total number of bytes the operation will move.
    pub fn set_bytes_total(&self, total: u64) {
        self.inner.bytes_total.store(total, Ordering::Relaxed);
    }

    /// Begin a new file: name it and state its size, resetting the per-file
    /// counter. Called once per file, before any `add_bytes` for it.
    pub fn begin_file(&self, name: impl Into<String>, size: u64) {
        if let Ok(mut cur) = self.inner.current.lock() {
            *cur = name.into();
        }
        self.inner.file_total.store(size, Ordering::Relaxed);
        self.inner.file_done.store(0, Ordering::Relaxed);
    }

    /// Add `n` bytes to both the per-file and whole-operation tallies.
    ///
    /// Called once per chunk, so this is the hot path — hence `Relaxed` on
    /// every counter. The UI reads them for display only; a frame that sees a
    /// slightly stale value simply redraws 100 ms later.
    pub fn add_bytes(&self, n: u64) {
        // The clock starts with the first byte, not at construction: the total
        // is computed by a directory walk that can itself take a while on a
        // cold cache, and counting it would understate the rate for the whole
        // rest of the copy.
        if let Ok(mut started) = self.inner.started.lock() {
            if started.is_none() {
                *started = Some(Instant::now());
            }
        }
        self.inner.file_done.fetch_add(n, Ordering::Relaxed);
        self.inner.bytes_done.fetch_add(n, Ordering::Relaxed);
    }

    pub fn bytes_done(&self) -> u64 {
        self.inner.bytes_done.load(Ordering::Relaxed)
    }
    pub fn bytes_total(&self) -> u64 {
        self.inner.bytes_total.load(Ordering::Relaxed)
    }
    pub fn file_done(&self) -> u64 {
        self.inner.file_done.load(Ordering::Relaxed)
    }
    pub fn file_total(&self) -> u64 {
        self.inner.file_total.load(Ordering::Relaxed)
    }

    /// The name of the file currently in flight, if one has been announced.
    pub fn current_file(&self) -> Option<String> {
        let cur = self.inner.current.lock().ok()?;
        if cur.is_empty() {
            None
        } else {
            Some(cur.clone())
        }
    }

    /// Bytes per second since the first byte moved, or `None` before there is
    /// enough to divide by. The guards matter: a rate computed over the first
    /// few milliseconds swings wildly, and displaying it makes the overlay
    /// look broken rather than fast.
    pub fn rate(&self) -> Option<f64> {
        let started = (*self.inner.started.lock().ok()?)?;
        let elapsed = started.elapsed().as_secs_f64();
        if elapsed < 0.5 {
            return None;
        }
        let done = self.bytes_done();
        if done == 0 {
            return None;
        }
        Some(done as f64 / elapsed)
    }

    /// Seconds remaining at the current average rate, if both a total and a
    /// rate are known.
    pub fn eta_secs(&self) -> Option<u64> {
        let total = self.bytes_total();
        let done = self.bytes_done();
        if total == 0 || done >= total {
            return None;
        }
        let rate = self.rate()?;
        if rate <= 0.0 {
            return None;
        }
        Some(((total - done) as f64 / rate).round() as u64)
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

    /// Build the copy/delete progress overlay from shared progress.
    ///
    /// The item count alone is not enough for a large copy. One 8 GB file is a
    /// single item, so a `done / total` bar shows 0/1 for minutes and then
    /// jumps straight to 1/1 — the operation looks hung. Whenever byte totals
    /// are available the overall bar is drawn from *bytes*, which move
    /// continuously, and a second bar tracks the file currently in flight so
    /// that even a single-file copy has something advancing.
    ///
    /// Operations that report no bytes (delete, chmod) fall back to the item
    /// bar unchanged.
    pub fn for_operation(verb: &str, progress: &OpProgress) -> Self {
        let done = progress.done();
        let total = progress.total();
        let bytes_done = progress.bytes_done();
        let bytes_total = progress.bytes_total();
        let mut body = Vec::new();

        // The name first: on a long copy this is the one line that says what
        // is actually happening right now.
        if let Some(name) = progress.current_file() {
            body.push(truncate_middle(&name, BAR_WIDTH + 2));
        }

        if bytes_total > 0 {
            // Overall: bytes, because that is what is really being moved.
            body.push(format!(
                "{} / {}   ({} / {} items)",
                format_size_trim(bytes_done),
                format_size_trim(bytes_total),
                done,
                total,
            ));
            body.push(progress_bar_ratio(bytes_done as f64 / bytes_total as f64));

            // Rate and ETA share a line, and each is omitted until it can be
            // stated honestly — see `OpProgress::rate`.
            //
            // Placed above the per-file bar deliberately. A short pane drops
            // trailing lines (see `render`), and "how long is left" is worth
            // more than a second bar breaking down the first.
            let mut stats = Vec::new();
            if let Some(rate) = progress.rate() {
                stats.push(format!("{}/s", format_size_trim(rate as u64)));
            }
            if let Some(eta) = progress.eta_secs() {
                stats.push(format!("{} left", format_duration(eta)));
            }
            if !stats.is_empty() {
                body.push(stats.join("   "));
            }

            // Per-file, but only when it adds something. With one file in the
            // batch the two bars would be identical, and a duplicated bar reads
            // as a rendering bug rather than as extra detail.
            let file_total = progress.file_total();
            if total > 1 && file_total > 0 {
                let file_done = progress.file_done().min(file_total);
                body.push(format!(
                    "this file: {} / {}",
                    format_size_trim(file_done),
                    format_size_trim(file_total),
                ));
                body.push(progress_bar_ratio(file_done as f64 / file_total as f64));
            }
        } else if total > 0 {
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
        //
        // The cap is the *area*, not a constant: in a narrow dual-panel column
        // a fixed-width box gets clipped by `centered`, and clipping lands
        // mid-line, cutting the percentage off the end of a bar. Fitting the
        // content to the column instead means the numbers always survive.
        let budget = (area.width.saturating_sub(4)).clamp(8, 56) as usize;
        let mut body: Vec<String> = self.body.iter().map(|l| fit(l, budget)).collect();

        // Drop body lines that would not fit the pane's height rather than
        // letting `Paragraph` clip them. Clipping takes them off the *bottom*,
        // which is where the rate and ETA live — the overlay would lose its
        // most useful lines and give no sign it had. Trimming here at least
        // keeps what remains complete, and the lines are ordered so that the
        // most important survive.
        //
        // Chrome is 2 border rows, the title, and the blank line under it.
        let max_body = (area.height as usize).saturating_sub(4);
        if body.len() > max_body {
            body.truncate(max_body);
        }

        let width = body
            .iter()
            .map(|l| l.chars().count())
            .chain(std::iter::once(self.title.chars().count().min(budget)))
            .max()
            .unwrap_or(budget)
            // `budget` is both floor and ceiling: it is already clamped to a
            // sane range, and using a larger constant as the floor would invert
            // the range in a pane narrower than that constant and panic.
            .min(budget)
            .max(budget.min(10)) as u16
            + 4;
        let height = 2 + 1 + body.len() as u16 + if body.is_empty() { 0 } else { 1 };

        let center = centered(Rect::new(0, 0, width, height), area);
        frame.render_widget(Clear, center);

        let mut lines = vec![Line::from(Span::styled(
            fit(&self.title, budget),
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        ))];
        if !body.is_empty() {
            lines.push(Line::from(""));
            for b in &body {
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

/// Width in cells of the overlay's bars. Also the budget `truncate_middle`
/// sizes the filename against, so the box does not change width from one file
/// to the next.
const BAR_WIDTH: usize = 28;

/// A text progress bar for a done/total ratio.
fn progress_bar(done: u64, total: u64) -> String {
    let ratio = if total > 0 {
        (done as f64 / total as f64).clamp(0.0, 1.0)
    } else {
        0.0
    };
    progress_bar_ratio(ratio)
}

/// A text progress bar for an already-computed ratio, with the percentage
/// beside it. On a multi-gigabyte copy the bar can look motionless for a while;
/// the number is what shows it is still moving.
fn progress_bar_ratio(ratio: f64) -> String {
    let ratio = if ratio.is_finite() {
        ratio.clamp(0.0, 1.0)
    } else {
        0.0
    };
    render_bar(ratio, BAR_WIDTH)
}

/// `[bar] NN%` at an explicit bar width. Separated from `progress_bar_ratio` so
/// `fit` can redraw a bar narrower instead of truncating one.
fn render_bar(ratio: f64, width: usize) -> String {
    format!("[{}] {:>3.0}%", ratio_bar(ratio, width), ratio * 100.0)
}

/// `format_size` without its column padding. The shared helper right-aligns to
/// a fixed width for the size column; inline in a sentence that leaves gaps.
fn format_size_trim(bytes: u64) -> String {
    crate::utils::sizes::format_size(bytes).trim().to_string()
}

/// A duration as `1h 4m`, `4m 12s` or `12s` — coarse on purpose, since an ETA
/// derived from an average rate is not precise enough to justify more.
fn format_duration(secs: u64) -> String {
    if secs >= 3600 {
        format!("{}h {}m", secs / 3600, (secs % 3600) / 60)
    } else if secs >= 60 {
        format!("{}m {}s", secs / 60, secs % 60)
    } else {
        format!("{}s", secs)
    }
}

/// Fit one body line into `max` display columns.
///
/// A bar line is rebuilt narrower rather than cut, because the informative part
/// — the percentage — sits at the *end*, and truncating a bar throws away
/// exactly the number the user is looking at. Other lines are elided in the
/// middle by the same reasoning applied to filenames.
fn fit(line: &str, max: usize) -> String {
    if line.chars().count() <= max {
        return line.to_string();
    }
    // `[<bar>] <pct>%` — re-render at whatever width is left after the
    // brackets, the space and the four columns the percentage needs.
    if let Some(ratio) = bar_ratio(line) {
        let overhead = 3 + 5;
        if max > overhead + 3 {
            return render_bar(ratio, max - overhead);
        }
        // Too narrow even for a stub bar: the number alone still informs.
        return format!("{:.0}%", ratio * 100.0);
    }
    // A trailing parenthetical is supplementary by construction — the byte
    // line's "(N / M items)". Dropping it whole beats eliding the middle of a
    // line whose two halves are unrelated, which strands an orphan ")".
    if let Some(head) = line.split(" (").next() {
        if head.len() < line.len() && head.chars().count() <= max {
            return head.to_string();
        }
    }
    truncate_middle(line, max)
}

/// Recover the ratio a bar line was drawn from, by counting its filled cells.
/// Cheaper and less fragile than threading the numbers through the body as a
/// parallel structure, and the glyphs are ours so the parse cannot drift.
fn bar_ratio(line: &str) -> Option<f64> {
    let inner = line.strip_prefix('[')?;
    let end = inner.find(']')?;
    let bar = &inner[..end];
    let filled = bar.chars().filter(|c| *c == '\u{2588}').count();
    let total = bar
        .chars()
        .filter(|c| *c == '\u{2588}' || *c == '\u{2591}')
        .count();
    if total == 0 {
        return None;
    }
    Some(filled as f64 / total as f64)
}

/// Shorten `s` to `max` display columns, eliding the middle.
///
/// The middle rather than the tail because the two informative parts of a long
/// filename are its start and its extension; cutting the end throws away the
/// half that says what kind of file it is. Counts `char`s, matching how the
/// overlay sizes its box.
fn truncate_middle(s: &str, max: usize) -> String {
    let count = s.chars().count();
    if count <= max || max < 4 {
        return s.to_string();
    }
    let keep = max - 1;
    let head = keep.div_ceil(2);
    let tail = keep - head;
    let chars: Vec<char> = s.chars().collect();
    let head_s: String = chars[..head].iter().collect();
    let tail_s: String = chars[count - tail..].iter().collect();
    format!("{}…{}", head_s, tail_s)
}

/// Render `ratio` (0.0..=1.0) as a `width`-cell bar of block glyphs, without
/// surrounding brackets. Shared with the transfer panel, which sizes its own
/// bars to the available column width.
pub fn ratio_bar(ratio: f64, width: usize) -> String {
    let ratio = ratio.clamp(0.0, 1.0);
    let filled = ((ratio * width as f64).round() as usize).min(width);
    format!("{}{}", "█".repeat(filled), "░".repeat(width - filled))
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
