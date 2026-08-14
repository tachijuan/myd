use ratatui::{
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span, Text},
    widgets::{Block, BorderType, Borders, Paragraph},
    Frame,
};

use crate::transfer::{format_eta, format_rate, TransferId, TransferQueue, TransferState};
use crate::widget::progress::ratio_bar;

/// Below this terminal width the panel hides itself: at narrower sizes it would
/// squeeze the file tree to the point of uselessness, and the tree is the reason
/// the app exists.
pub const MIN_TERMINAL_WIDTH: u16 = 90;

/// Panel width as a percentage of the terminal, then clamped.
const WIDTH_PERCENT: u16 = 30;
const MIN_WIDTH: u16 = 28;
const MAX_WIDTH: u16 = 40;

/// The width this panel wants inside `total_width`, or `None` when the terminal
/// is too narrow to justify showing it at all.
pub fn desired_width(total_width: u16) -> Option<u16> {
    if total_width < MIN_TERMINAL_WIDTH {
        return None;
    }
    let pct = total_width * WIDTH_PERCENT / 100;
    Some(pct.clamp(MIN_WIDTH, MAX_WIDTH))
}

/// Where each cancellable transfer was drawn, so a click or the cursor can be
/// mapped back to one.
///
/// Only active and queued transfers appear: a finished one has nothing to
/// cancel, so including it would give the cursor stops that do nothing.
#[derive(Debug, Default, Clone)]
pub struct PanelRows {
    /// `(first screen row of the entry, transfer id)`, in display order.
    pub rows: Vec<(u16, TransferId)>,
}

impl PanelRows {
    /// The transfer drawn at screen row `y`, if any.
    ///
    /// An entry spans several rows (name, bar, stats), so this takes the last
    /// entry that starts at or above `y` — a click anywhere in the block selects
    /// it, which is what a user aiming at a progress bar expects.
    pub fn at(&self, y: u16) -> Option<TransferId> {
        self.rows
            .iter()
            .rev()
            .find(|(row, _)| *row <= y)
            .map(|(_, id)| *id)
    }

    pub fn ids(&self) -> Vec<TransferId> {
        self.rows.iter().map(|(_, id)| *id).collect()
    }
}

/// Render the transfer queue as a right-hand sidebar.
///
/// `focused` draws the same bright border the browser panels use when active,
/// and `selected` highlights one transfer. Returns where each cancellable
/// transfer landed, for hit-testing.
pub fn render(
    frame: &mut Frame,
    area: Rect,
    queue: &TransferQueue,
    focused: bool,
    selected: Option<TransferId>,
) -> PanelRows {
    if area.width == 0 || area.height == 0 {
        return PanelRows::default();
    }

    // Report the work itself, not the worker-pool capacity: "0/4" read as if
    // four transfers existed and none had started. Show what is actually
    // running and what is still waiting.
    let active = queue.active_count();
    let queued = queue.queued_count();
    let base = if active == 0 && queued == 0 {
        " Transfers ".to_string()
    } else if queued == 0 {
        format!(" Transfers ({} active) ", active)
    } else {
        format!(" Transfers ({} active, {} queued) ", active, queued)
    };
    // Say how to cancel only while focused; otherwise it is noise on a panel
    // the user isn't driving. Dropped entirely when it wouldn't fit, since
    // ratatui clips a title at the border and half a hint is worse than none.
    let title = if focused {
        // Longest first: the fuller hint when there is room, then a shorter one,
        // then none. The old single hint read "j/k move, k cancels", which is
        // self-contradictory and wrong besides — `K` moves up, `k` cancels.
        let fitted = [
            " — j/K move, k cancels, C clears ",
            " — k cancels, C clears ",
            " — C clears ",
        ]
        .into_iter()
        // Two border corners plus a column of slack.
        .map(|hint| format!("{}{}", base.trim_end(), hint))
        .find(|with_hint| with_hint.chars().count() + 3 <= area.width as usize);
        fitted.unwrap_or(base)
    } else {
        base
    };

    // Matches the browser panels: bright cyan when this panel has focus, a light
    // grey otherwise, so "where am I" reads the same everywhere.
    let border = if focused {
        Color::Cyan
    } else {
        Color::Rgb(160, 160, 172)
    };

    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Plain)
        .border_style(Style::default().fg(border))
        .title(title);

    // Content width inside the borders; the bar is sized from this so it never
    // overflows a narrow panel.
    let inner_width = area.width.saturating_sub(2) as usize;
    let (lines, rows) = build_lines_tracked(queue, inner_width, selected, area.y + 1);

    frame.render_widget(Paragraph::new(Text::from(lines)).block(block), area);
    rows
}

/// Build the panel body: an Active section, then Queued, then finished.
#[cfg(test)]
fn build_lines(queue: &TransferQueue, width: usize) -> Vec<Line<'static>> {
    build_lines_tracked(queue, width, None, 0).0
}

/// As [`build_lines`], also reporting where each cancellable transfer landed.
///
/// `origin` is the screen row of the panel's first content line, so the recorded
/// positions are absolute and can be compared against a mouse event directly.
fn build_lines_tracked(
    queue: &TransferQueue,
    width: usize,
    selected: Option<TransferId>,
    origin: u16,
) -> (Vec<Line<'static>>, PanelRows) {
    let mut lines: Vec<Line<'static>> = Vec::new();
    let mut rows = PanelRows::default();

    if queue.is_empty() {
        lines.push(Line::from(Span::styled(
            "No transfers yet",
            Style::default().fg(Color::DarkGray),
        )));
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            "Copy with c to queue one.",
            Style::default().fg(Color::DarkGray),
        )));
        return (lines, rows);
    }

    let active: Vec<_> = queue
        .transfers()
        .iter()
        .filter(|t| t.state == TransferState::Active)
        .collect();
    let queued: Vec<_> = queue
        .transfers()
        .iter()
        .filter(|t| t.state == TransferState::Queued)
        .collect();
    let finished: Vec<_> = queue
        .transfers()
        .iter()
        .filter(|t| t.state.is_terminal())
        .collect();

    if !active.is_empty() {
        lines.push(section_header(
            format!("Active ({})", active.len()),
            Color::Cyan,
        ));
        for t in &active {
            rows.rows.push((origin + lines.len() as u16, t.id));
            let picked = selected == Some(t.id);
            lines.push(name_line_selected(
                &transfer_label(t),
                width,
                Color::White,
                picked,
            ));
            lines.push(bar_line(t.progress.fraction(), width));
            lines.push(stats_line(t, width));
        }
    }

    if !queued.is_empty() {
        if !lines.is_empty() {
            lines.push(Line::from(""));
        }
        lines.push(section_header(
            format!("Queued ({})", queued.len()),
            Color::Yellow,
        ));
        for t in &queued {
            rows.rows.push((origin + lines.len() as u16, t.id));
            let picked = selected == Some(t.id);
            lines.push(name_line_selected(
                &transfer_label(t),
                width,
                Color::Gray,
                picked,
            ));
        }
    }

    if !finished.is_empty() {
        if !lines.is_empty() {
            lines.push(Line::from(""));
        }
        let failed = finished
            .iter()
            .filter(|t| matches!(t.state, TransferState::Failed(_)))
            .count();
        let header = if failed > 0 {
            format!("Done ({}, {} failed)", finished.len(), failed)
        } else {
            format!("Done ({})", finished.len())
        };
        lines.push(section_header(header, Color::DarkGray));

        // Newest first: a long session's most recent results are what matter.
        for t in finished.iter().rev() {
            match &t.state {
                TransferState::Done => {
                    lines.push(status_line("✓", &t.name, width, Color::Green));
                }
                TransferState::Cancelled => {
                    lines.push(status_line("⨯", &t.name, width, Color::DarkGray));
                }
                TransferState::Failed(msg) => {
                    lines.push(status_line("!", &t.name, width, Color::Red));
                    // The error is the whole value of a failed row, so it is
                    // wrapped rather than truncated: the message now carries the
                    // root cause ("...: Permission denied") at its *end*, which
                    // is exactly the part a single truncated line would drop.
                    // Capped at three lines so one failure can't crowd out the
                    // rest of the queue.
                    for line in wrap(msg, width.saturating_sub(2)).into_iter().take(3) {
                        lines.push(Line::from(Span::styled(
                            format!("  {}", line),
                            Style::default().fg(Color::Red),
                        )));
                    }
                }
                _ => {}
            }
        }
    }

    (lines, rows)
}

fn section_header(text: String, color: Color) -> Line<'static> {
    Line::from(Span::styled(
        format!("▼ {}", text),
        Style::default().fg(color).add_modifier(Modifier::BOLD),
    ))
}

/// A transfer's display name, marked when it is the copy half of a move — the
/// source is about to be deleted, which is worth seeing before it happens.
fn transfer_label(t: &crate::transfer::Transfer) -> String {
    if t.remove_source {
        format!("{} (move)", t.name)
    } else {
        t.name.clone()
    }
}

/// A transfer's name row, highlighted when it is the panel's selection.
///
/// The highlight fills the whole width so the selection reads as a row rather
/// than as a differently-coloured word.
fn name_line_selected(name: &str, width: usize, color: Color, selected: bool) -> Line<'static> {
    if !selected {
        return Line::from(Span::styled(
            truncate(name, width),
            Style::default().fg(color),
        ));
    }
    let text = truncate(name, width);
    let pad = width.saturating_sub(text.chars().count());
    Line::from(Span::styled(
        format!("{}{}", text, " ".repeat(pad)),
        Style::default()
            .fg(Color::Black)
            .bg(Color::Rgb(80, 200, 235))
            .add_modifier(Modifier::BOLD),
    ))
}

fn status_line(mark: &str, name: &str, width: usize, color: Color) -> Line<'static> {
    // The mark plus a space eats 2 columns.
    let name = truncate(name, width.saturating_sub(2));
    Line::from(Span::styled(
        format!("{} {}", mark, name),
        Style::default().fg(color),
    ))
}

/// A bar plus its percentage, e.g. `███████░░ 71%`.
fn bar_line(fraction: f64, width: usize) -> Line<'static> {
    // Reserve 5 columns for " 100%".
    let bar_width = width.saturating_sub(5).max(1);
    Line::from(Span::styled(
        format!(
            "{} {:>3.0}%",
            ratio_bar(fraction, bar_width),
            fraction * 100.0
        ),
        Style::default().fg(Color::Cyan),
    ))
}

/// Rate and ETA, e.g. `8.4 MB/s  ETA 12s`.
fn stats_line(t: &crate::transfer::Transfer, width: usize) -> Line<'static> {
    let rate = t
        .progress
        .rate()
        .map(format_rate)
        .unwrap_or_else(|| "—".to_string());
    let eta = t
        .progress
        .eta()
        .map(|d| format!("ETA {}", format_eta(d)))
        .unwrap_or_default();

    let text = if eta.is_empty() {
        rate
    } else {
        format!("{}  {}", rate, eta)
    };
    Line::from(Span::styled(
        truncate(&text, width),
        Style::default().fg(Color::DarkGray),
    ))
}

/// Clip `s` to `width` display columns, marking elision with `…`.
///
/// Counts chars rather than bytes: a multibyte filename would otherwise be
/// mis-measured and overflow the panel.
/// Wrap `s` to `width` columns, breaking at spaces where possible.
///
/// Counts chars rather than bytes so a path with multibyte characters wraps at
/// the right column. A word longer than the line (a long path with no spaces) is
/// hard-split so it stays fully visible instead of being cut at the border.
fn wrap(s: &str, width: usize) -> Vec<String> {
    if width == 0 {
        return Vec::new();
    }
    let mut lines = Vec::new();
    let mut current = String::new();
    let mut len = 0usize;
    for word in s.split_whitespace() {
        let wlen = word.chars().count();
        if wlen > width {
            if len > 0 {
                lines.push(std::mem::take(&mut current));
                len = 0;
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
                len = current.chars().count();
            }
            continue;
        }
        let need = if len == 0 { wlen } else { len + 1 + wlen };
        if need > width {
            lines.push(std::mem::take(&mut current));
            current.push_str(word);
            len = wlen;
        } else {
            if len > 0 {
                current.push(' ');
            }
            current.push_str(word);
            len = need;
        }
    }
    if !current.is_empty() {
        lines.push(current);
    }
    lines
}

fn truncate(s: &str, width: usize) -> String {
    if width == 0 {
        return String::new();
    }
    let count = s.chars().count();
    if count <= width {
        return s.to_string();
    }
    if width == 1 {
        return "…".to_string();
    }
    let kept: String = s.chars().take(width - 1).collect();
    format!("{}…", kept)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transfer::{TransferConfig, TransferQueue};
    use crate::vfs::VPath;

    #[test]
    fn hides_on_narrow_terminals() {
        assert!(desired_width(80).is_none());
        assert!(desired_width(MIN_TERMINAL_WIDTH - 1).is_none());
        assert!(desired_width(MIN_TERMINAL_WIDTH).is_some());
    }

    #[test]
    fn width_is_clamped_to_sane_bounds() {
        assert_eq!(desired_width(90), Some(28)); // 27 -> clamped up
        assert_eq!(desired_width(200), Some(40)); // 60 -> clamped down
        let mid = desired_width(120).unwrap();
        assert!((MIN_WIDTH..=MAX_WIDTH).contains(&mid));
    }

    #[test]
    fn empty_queue_shows_a_hint() {
        let q = TransferQueue::default();
        let lines = build_lines(&q, 30);
        let text: String = lines
            .iter()
            .flat_map(|l| l.spans.iter().map(|s| s.content.to_string()))
            .collect();
        assert!(text.contains("No transfers yet"));
    }

    #[test]
    fn queued_transfers_appear_under_a_queued_header() {
        let mut q = TransferQueue::default();
        q.enqueue(VPath::local("/a/one.bin"), VPath::local("/b/one.bin"), 10);
        q.enqueue(VPath::local("/a/two.bin"), VPath::local("/b/two.bin"), 10);

        let text: String = build_lines(&q, 30)
            .iter()
            .flat_map(|l| l.spans.iter().map(|s| s.content.to_string()))
            .collect();

        assert!(text.contains("Queued (2)"));
        assert!(text.contains("one.bin") && text.contains("two.bin"));
    }

    #[test]
    fn failed_transfer_shows_its_error() {
        let mut q = TransferQueue::default();
        q.enqueue(VPath::local("/a/bad.bin"), VPath::local("/b/bad.bin"), 10);
        // Reach in the way the queue's reaper would.
        let text = {
            let t = &mut q.transfers_mut()[0];
            t.state = TransferState::Failed("permission denied".into());
            build_lines(&q, 34)
                .iter()
                .flat_map(|l| l.spans.iter().map(|s| s.content.to_string()))
                .collect::<String>()
        };

        assert!(text.contains("1 failed"));
        assert!(text.contains("permission denied"));
    }

    #[test]
    fn bar_line_fits_the_given_width() {
        for width in [10usize, 20, 28, 40] {
            let line = bar_line(0.71, width);
            let rendered: String = line.spans.iter().map(|s| s.content.to_string()).collect();
            assert!(
                rendered.chars().count() <= width,
                "width {} overflowed: {:?}",
                width,
                rendered
            );
            assert!(rendered.contains("71%"));
        }
    }

    #[test]
    fn truncate_counts_chars_not_bytes() {
        // Multibyte name: byte length would over-count and overflow the panel.
        let name = "日本語のファイル名.txt";
        let out = truncate(name, 8);
        assert_eq!(out.chars().count(), 8);
        assert!(out.ends_with('…'));
    }

    #[test]
    fn truncate_leaves_short_strings_alone() {
        assert_eq!(truncate("short", 20), "short");
        assert_eq!(truncate("", 5), "");
        assert_eq!(truncate("abc", 0), "");
    }

    #[test]
    fn renders_without_panicking_at_extreme_sizes() {
        let mut q = TransferQueue::new(TransferConfig::default());
        q.enqueue(VPath::local("/a/f.bin"), VPath::local("/b/f.bin"), 100);

        let backend = ratatui::backend::TestBackend::new(40, 20);
        let mut term = ratatui::Terminal::new(backend).unwrap();
        term.draw(|f| {
            // Degenerate and normal rects both must be safe.
            render(f, Rect::new(0, 0, 0, 0), &q, false, None);
            render(f, Rect::new(0, 0, 1, 1), &q, false, None);
            render(f, Rect::new(0, 0, 30, 18), &q, false, None);
        })
        .unwrap();
    }
}
