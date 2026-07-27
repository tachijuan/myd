use ratatui::{
    layout::{Constraint, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, BorderType, Borders, Clear, Paragraph},
    Frame,
};

/// Category of related keybindings for the help screen.
struct HelpCategory {
    title: &'static str,
    items: &'static [(&'static str, &'static str)],
}

/// Scroll position of the help overlay.
///
/// The list is ~90 lines against a typical 24-row terminal, so it has to
/// scroll. The bounds depend on the drawn area, so they are recorded during
/// render — the same arrangement the file tree uses for its own offset.
#[derive(Debug, Default, Clone, Copy)]
pub struct HelpState {
    scroll: usize,
    /// Largest valid offset, from the last render. Zero until drawn once, so
    /// scrolling before the first frame is a harmless no-op.
    max_scroll: usize,
    /// Visible content rows, for page-sized jumps.
    viewport: usize,
    /// Total content rows, for the position indicator.
    total: usize,
}

impl HelpState {
    pub fn new() -> Self {
        Self::default()
    }

    /// Whether the content is taller than the box — i.e. scrolling does anything.
    pub fn scrollable(&self) -> bool {
        self.max_scroll > 0
    }

    pub fn scroll(&self) -> usize {
        self.scroll
    }

    /// Scroll by `delta` rows, clamped to the content.
    pub fn scroll_by(&mut self, delta: isize) {
        let next = self.scroll as isize + delta;
        self.scroll = next.clamp(0, self.max_scroll as isize) as usize;
    }

    /// Scroll a screenful, keeping one line of overlap for continuity.
    pub fn page(&mut self, down: bool) {
        let step = self.viewport.saturating_sub(1).max(1) as isize;
        self.scroll_by(if down { step } else { -step });
    }

    pub fn to_top(&mut self) {
        self.scroll = 0;
    }

    pub fn to_bottom(&mut self) {
        self.scroll = self.max_scroll;
    }
}

/// Render a modal help overlay with a dimmed background and bordered box.
pub fn render_help(frame: &mut Frame, area: Rect, state: &mut HelpState) {
    // 1. Clear the area so our background can overlay everything.
    frame.render_widget(Clear, area);

    // 2. Draw a dimmed background over the full screen.
    let bg = Paragraph::new(Line::from("")).style(Style::default().bg(Color::Rgb(20, 20, 30)));
    frame.render_widget(bg, area);

    let categories: &[HelpCategory] = &[
        HelpCategory {
            title: "Navigation",
            items: &[
                ("j / Down", "Cursor down"),
                ("k / Up", "Cursor up"),
                ("gg", "Go to top"),
                ("G", "Go to bottom"),
                ("Ctrl+F/PgDn", "Page down (full screen)"),
                ("Ctrl+B/PgUp", "Page up (full screen)"),
                ("Ctrl+D", "Half page down"),
                ("Ctrl+U", "Half page up"),
                ("Enter", "Enter directory"),
                ("h / Left", "Collapse / Go back"),
                ("l / Right", "Expand directory"),
                ("Ctrl+O", "Go back (pop screen)"),
                ("v", "Toggle TREE/TREEMAP view"),
            ],
        },
        HelpCategory {
            title: "Tree",
            items: &[
                ("l / Right", "Expand selected directory"),
                ("h / Left", "Collapse / Go back"),
                ("*", "Expand all"),
                ("0", "Collapse all"),
            ],
        },
        HelpCategory {
            title: "Treemap",
            items: &[
                ("v", "Toggle TREE/TREEMAP view"),
                ("j / k / h / l", "Navigate treemap tiles"),
                ("G / gg", "First / Last tile"),
            ],
        },
        HelpCategory {
            title: "View",
            items: &[
                ("s", "Cycle sort order"),
                ("click Sort:", "Pick a sort order from a numbered menu"),
                ("H", "Toggle hidden files"),
                ("b", "Toggle size bars"),
                ("P", "Toggle permissions column"),
                ("T", "Toggle modification-time column"),
                ("Ctrl+P", "Toggle info panel"),
                ("f", "Filter the tree (regex, empty clears)"),
            ],
        },
        HelpCategory {
            title: "Tagging & selection",
            items: &[
                ("t", "Tag / untag file under cursor"),
                ("V", "Visual mode: tag a range as you move"),
                ("U", "Untag all files"),
                ("c", "Copy tagged files (or selection)"),
                ("m", "Move tagged files (or selection)"),
            ],
        },
        HelpCategory {
            title: "Actions",
            items: &[
                ("D", "Delete tagged / selected"),
                ("R", "Rename selected"),
                ("N", "New directory here"),
                ("o", "Open with the system default app"),
                ("r", "Refresh"),
                ("/", "Search by name (regex)"),
                ("n / p", "Next / previous search match"),
                ("gd", "Go to directory picker"),
                ("  Tab", "  Switch the path field and the list"),
                ("  a / d", "  Save a directory / forget the highlighted one"),
                ("  p / u", "  Pin to the top block / unpin"),
                ("  m", "  Move an entry into place (Enter, Esc)"),
            ],
        },
        HelpCategory {
            title: "Panels",
            items: &[
                ("|", "Toggle single / dual panels"),
                ("Tab", "Rotate focus through the panes"),
                ("c", "Copy tagged/selected to other panel"),
                ("m", "Move tagged/selected to other panel"),
            ],
        },
        HelpCategory {
            title: "Remote hosts",
            items: &[
                ("gr", "Connect — recent hosts, or type an address"),
                ("gs", "All saved hosts (the dialing directory)"),
                ("  /", "  Search the list; j/k/g/G navigate"),
                ("  a / e / d", "  Add / edit / delete a saved host"),
            ],
        },
        HelpCategory {
            title: "Transfers",
            items: &[
                ("Ctrl+T", "Show / hide the transfer panel"),
                ("Tab", "Reached after the panels, then wraps around"),
                ("j / K", "Move between transfers"),
                ("k / Del", "Cancel the selected transfer (asks first)"),
                ("dbl-click", "Cancel the transfer under the pointer"),
                ("gx", "Cancel every queued and running transfer"),
            ],
        },
        HelpCategory {
            title: "Mouse",
            items: &[
                ("Wheel", "Scroll the focused view"),
                ("Left click", "Focus a panel and select a row / tile"),
                ("Double click", "Open (same as Enter)"),
                ("Right click", "Select and open (enter a directory)"),
                ("Ctrl+N", "Release the mouse for terminal text selection"),
            ],
        },
        HelpCategory {
            title: "Exit",
            items: &[
                ("q / Esc", "Quit (asks first if transfers are running)"),
                ("Ctrl+C", "Force quit immediately, anytime"),
            ],
        },
        HelpCategory {
            title: "Dialogs",
            items: &[("Enter", "Confirm"), ("Esc", "Cancel")],
        },
    ];

    let mut lines = Vec::new();

    for cat in categories {
        lines.push(Line::from(Span::styled(
            format!("  {}:", cat.title),
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        )));

        for (key, desc) in cat.items {
            lines.push(Line::from(vec![
                Span::styled(
                    format!("    {:13}", key),
                    Style::default()
                        .fg(Color::Green)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(*desc, Style::default().fg(Color::White)),
            ]));
        }

        lines.push(Line::from(""));
    }

    let total = lines.len();

    // The content is far taller than a typical terminal, so the box takes what
    // height it can get and scrolls. It used to be clamped with `.min(...)`,
    // which silently *clipped* — everything past the fold, including how to
    // cancel a transfer, was simply unreachable.
    let box_h = ((total + 2) as u16).min(area.height);
    let viewport = box_h.saturating_sub(2) as usize;

    // Clamp before drawing so the last line can be scrolled to but not past.
    let max_scroll = total.saturating_sub(viewport);
    state.max_scroll = max_scroll;
    state.viewport = viewport;
    state.total = total;
    if state.scroll > max_scroll {
        state.scroll = max_scroll;
    }

    // Centre the modal box vertically and horizontally.
    let outer = Layout::vertical([
        Constraint::Min(0),
        Constraint::Length(box_h),
        Constraint::Min(0),
    ])
    .split(area);
    let box_area = Layout::horizontal([
        Constraint::Min(0),
        Constraint::Length(70.min(area.width)),
        Constraint::Min(0),
    ])
    .split(outer[1]);
    let box_area = box_area[1];

    // Say so when there is more below, and where you are — otherwise a clipped
    // list looks like the whole list.
    let title = if max_scroll > 0 {
        let shown = (state.scroll + viewport).min(total);
        format!(
            " Help — {}-{} of {}  (j/k or ↑↓ to scroll) ",
            state.scroll + 1,
            shown,
            total
        )
    } else {
        " Help (? / F1 to toggle) ".to_string()
    };

    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Double)
        .border_style(Style::default().fg(Color::Yellow))
        .title(Line::from(Span::styled(
            title,
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        )));

    frame.render_widget(block, box_area);
    let inner = Rect::new(
        box_area.x + 1,
        box_area.y + 1,
        box_area.width.saturating_sub(2),
        box_area.height.saturating_sub(2),
    );
    let paragraph = Paragraph::new(lines).scroll((state.scroll as u16, 0));
    frame.render_widget(paragraph, inner);
}
