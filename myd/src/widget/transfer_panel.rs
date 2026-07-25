use ratatui::{
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span, Text},
    widgets::{Block, BorderType, Borders, Paragraph},
    Frame,
};

use crate::transfer::{format_eta, format_rate, TransferQueue, TransferState};
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

/// Render the transfer queue as a right-hand sidebar.
///
/// This is a pure view over the queue — it holds no state and takes no input, so
/// it cannot interfere with navigation.
pub fn render(frame: &mut Frame, area: Rect, queue: &TransferQueue) {
    if area.width == 0 || area.height == 0 {
        return;
    }

    // Report the work itself, not the worker-pool capacity: "0/4" read as if
    // four transfers existed and none had started. Show what is actually
    // running and what is still waiting.
    let active = queue.active_count();
    let queued = queue.queued_count();
    let title = if active == 0 && queued == 0 {
        " Transfers ".to_string()
    } else if queued == 0 {
        format!(" Transfers ({} active) ", active)
    } else {
        format!(" Transfers ({} active, {} queued) ", active, queued)
    };

    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Plain)
        .border_style(Style::default().fg(if queue.has_work() {
            Color::Cyan
        } else {
            Color::DarkGray
        }))
        .title(title);

    // Content width inside the borders; the bar is sized from this so it never
    // overflows a narrow panel.
    let inner_width = area.width.saturating_sub(2) as usize;
    let lines = build_lines(queue, inner_width);

    frame.render_widget(Paragraph::new(Text::from(lines)).block(block), area);
}

/// Build the panel body: an Active section, then Queued, then finished.
fn build_lines(queue: &TransferQueue, width: usize) -> Vec<Line<'static>> {
    let mut lines: Vec<Line<'static>> = Vec::new();

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
        return lines;
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
            lines.push(name_line(&t.name, width, Color::White));
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
            lines.push(name_line(&t.name, width, Color::Gray));
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
                    // The error is the whole value of a failed row, so show it
                    // even though it costs a line.
                    lines.push(Line::from(Span::styled(
                        format!("  {}", truncate(msg, width.saturating_sub(2))),
                        Style::default().fg(Color::Red),
                    )));
                }
                _ => {}
            }
        }
    }

    lines
}

fn section_header(text: String, color: Color) -> Line<'static> {
    Line::from(Span::styled(
        format!("▼ {}", text),
        Style::default().fg(color).add_modifier(Modifier::BOLD),
    ))
}

fn name_line(name: &str, width: usize, color: Color) -> Line<'static> {
    Line::from(Span::styled(
        truncate(name, width),
        Style::default().fg(color),
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
            render(f, Rect::new(0, 0, 0, 0), &q);
            render(f, Rect::new(0, 0, 1, 1), &q);
            render(f, Rect::new(0, 0, 30, 18), &q);
        })
        .unwrap();
    }
}
