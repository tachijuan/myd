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

impl HelpCategory {
    /// Rows this category occupies: its heading, its items, and the blank line
    /// that separates it from the next one.
    fn height(&self) -> usize {
        self.items.len() + 2
    }
}

/// Scroll position of the help overlay.
///
/// The bindings run to ~180 lines, so even laid out in columns they can be
/// taller than the terminal and have to scroll. The bounds depend on the drawn
/// area — and now on how many columns fit across it — so they are recorded
/// during render, the same arrangement the file tree uses for its own offset.
#[derive(Debug, Default, Clone, Copy)]
pub struct HelpState {
    scroll: usize,
    /// Largest valid offset, from the last render. Zero until drawn once, so
    /// scrolling before the first frame is a harmless no-op.
    max_scroll: usize,
    /// Visible content rows, for page-sized jumps.
    viewport: usize,
    /// Rows in the tallest column, for the position indicator. All columns
    /// scroll together, so this — not the sum of the columns — is the extent
    /// the offset moves through.
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

/// Every keybinding the help screen documents, in reading order.
///
/// At module scope rather than inside `render_help` so the column splitter
/// can be tested against the real list instead of a stand-in.
static CATEGORIES: &[HelpCategory] = &[
    HelpCategory {
        title: "Navigation",
        items: &[
            ("j / Down", "Cursor down"),
            ("k / Up", "Cursor up"),
            ("gg", "Go to top"),
            ("  g…", "  g waits for the next key; Esc cancels"),
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
            ("t", "Tag / untag the selected tile"),
            ("", "V (visual range) needs the tree view"),
        ],
    },
    HelpCategory {
        title: "View",
        items: &[
            ("s", "Cycle sort order"),
            ("1-8", "Sort by that menu entry (5 = newest)"),
            ("gs", "Pick a sort order from a numbered menu"),
            ("  1-8", "  Choose that order outright (gs5 = newest)"),
            ("click Sort:", "Opens the same menu"),
            ("H", "Toggle hidden files"),
            ("B", "Toggle size bars"),
            ("S", "Browse without measuring directories"),
            ("P", "Toggle permissions column"),
            ("T", "Toggle modification-time column"),
            ("Ctrl+P", "Toggle info panel"),
            ("  Tab", "  Focus it; a preview sits under the metadata"),
            ("  j / k", "  Walk permissions, owner, group"),
            ("  Enter", "  Edit that field (tagged files, or the cursor)"),
            ("  < / >", "  Widen / narrow the panel; the width is kept"),
            ("  + / -", "  Grow / shrink the preview under the metadata"),
            ("Ctrl+L", "Redraw the screen"),
            ("f", "Filter the tree (regex, empty clears)"),
        ],
    },
    HelpCategory {
        title: "Archives",
        items: &[
            ("space", "List a zip/tar/7z/rar's contents"),
            ("Enter", "Browse an archive as a filesystem"),
            ("c", "Extract the tagged or selected entries"),
            ("h", "Leave the archive"),
            ("gz", "Create an archive from the tagged or selected"),
            ("  ", "  zip, 7z, tar or tgz; lands in this directory"),
        ],
    },
    HelpCategory {
        title: "Preview",
        items: &[
            ("space", "Show / hide the preview (archives: list)"),
            ("j / k", "Scroll; PDF: page; image: next file"),
            ("Ctrl+F/B", "Scroll a page (or turn one)"),
            ("Ctrl+D/U", "Scroll half a page"),
            ("g / G", "Start / end, or first / last page"),
            ("/", "Search within the file (regex)"),
            ("n / p", "Next / previous match"),
            ("N / P", "Previous / next match"),
            ("e", "Edit the previewed file in your editor"),
            ("q", "Close the preview"),
            ("Esc", "Return focus to the tree, keep the pane"),
            ("Tab", "Move focus in and out of the pane"),
        ],
    },
    HelpCategory {
        title: "Tagging & selection",
        items: &[
            ("t", "Tag / untag the selection (tree or treemap)"),
            ("V", "Visual mode: tag a range as you move (tree)"),
            ("U", "Untag all files"),
            ("gt", "Untag all files (the same, as a g chord)"),
            ("c", "Copy tagged files (or selection)"),
            ("m", "Move tagged files (or selection)"),
            ("y", "Yank: hold the selection for a later paste"),
            ("x", "Cut: hold it, and remove it when pasted"),
            ("p", "Paste what is held into this directory"),
            ("  ", "  Works in one pane: yank, navigate, paste"),
            ("  ", "  A held set shows as a badge in the footer"),
            ("Esc", "Clear the yank/cut buffer"),
            ("u", "Undo the last paste (asks first)"),
        ],
    },
    HelpCategory {
        title: "Actions",
        items: &[
            ("D", "Delete tagged / selected"),
            ("  y / n", "  Confirm or decline"),
            ("  a", "  Delete, and stop asking until you restart"),
            ("R", "Rename selected"),
            ("gr", "Rename every tagged file by regex"),
            ("  Tab", "  Switch the pattern and replacement fields"),
            ("  ", "  $1 in the replacement is the first capture group"),
            ("  ", "  Write ${1} when a letter or digit follows it"),
            ("  #", "  A counter: one # per digit, so ## gives 01, 02"),
            ("  ", "  Put it anywhere: trip-##-$1 numbers the batch"),
            ("  ", "  ##{7} starts at 7; ##{7+3} counts 07, 10, 13"),
            ("  ", "  A minus counts down: ###{10-1} gives 010, 009"),
            ("  ", "  Numbered in screen order; skips take no number"),
            ("  ", "  Write \\# for a literal # in the new name"),
            ("  ", "  Case-sensitive, unlike / and f"),
            ("  ", "  Rust regex: docs.rs/regex, man myd (PATTERNS)"),
            ("gn", "New directory here"),
            ("e", "Edit in your editor"),
            ("  ", "  $EDITOR, or editor= in prefs.toml; vim if unset"),
            ("  ", "  Works in the preview pane too"),
            ("o", "Open with the system default app"),
            ("O", "Open with a program you name"),
            ("  ", "  Runs it over the tagged files; myd waits for it"),
            ("  Tab", "  Cycle the command, the saved apps, the actions"),
            ("  ↑ / ↓", "  Walk the saved apps; Enter runs one"),
            ("  a-z", "  In the list, type to jump to an app"),
            ("  Actions", "  Save, update or forget the highlighted app"),
            ("  ", "  Or press the highlighted letter to run it"),
            ("  ", "  Saved in ~/.config/myd/apps.toml"),
            ("r", "Refresh"),
            ("/", "Search by name (regex)"),
            ("n / N", "Next / previous search match"),
            ("gd", "Go to — directories and saved hosts"),
            ("  Tab", "  Cycle the path field, the list, the actions"),
            ("  ↑ / ↓", "  Move through the list"),
            ("  Enter", "  Open a directory, or connect to a host"),
            ("  ", "  Type an sftp:// URL to connect to an address"),
            ("  a-z", "  In the list, type to search; Esc clears it"),
            ("  ", "  A lone match opens on Enter"),
            ("  Actions", "  Go, save, edit, forget, pin, move, shallow"),
            ("  j / k", "  Walk the actions; Enter runs one"),
            ("  ", "  Or press the highlighted letter to run it"),
            ("gz", "Create an archive of the tagged or selected"),
            ("  Tab", "  Cycle the name, the format, the buttons"),
            ("  ↑ / ↓", "  Choose the format; or press 1-4"),
            ("  Enter", "  Create it, in the current directory"),
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
        title: "Transfers",
        items: &[
            ("Ctrl+T", "Show / hide the transfer panel"),
            ("Tab", "Reached after the panels, then wraps around"),
            ("j / k", "Move between transfers"),
            ("K / Del / ⌫", "Cancel the selected transfer (asks first)"),
            ("C", "Clear finished ones; live transfers stay"),
            ("Esc / q", "Back to the file tree"),
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
            ("q / Esc", "Back out: scan, picker, filter, archive"),
            ("", "  then quit (asks if transfers are running)"),
            ("Ctrl+C", "Force quit immediately, anytime"),
        ],
    },
    HelpCategory {
        title: "Dialogs",
        items: &[
            ("Enter", "Confirm"),
            ("Esc", "Cancel"),
            ("Tab", "Move focus (never confirms)"),
            ("", "Text fields take the usual shell editing keys:"),
            ("  C-a / C-e", "  Start / end of line"),
            ("  C-b / C-f", "  Back / forward one character"),
            ("  M-b / M-f", "  Back / forward one word"),
            ("  C-w", "  Delete the word before the cursor"),
            ("  C-u", "  Delete the whole line"),
            ("  C-k", "  Delete to the end of the line"),
            ("  C-d", "  Delete the character under the cursor"),
            ("  C-h", "  Backspace"),
        ],
    },
];

/// Render a modal help overlay with a dimmed background and bordered box.
pub fn render_help(frame: &mut Frame, area: Rect, state: &mut HelpState) {
    // 1. Clear the area so our background can overlay everything.
    frame.render_widget(Clear, area);

    // 2. Draw a dimmed background over the full screen.
    let bg = Paragraph::new(Line::from("")).style(Style::default().bg(Color::Rgb(20, 20, 30)));
    frame.render_widget(bg, area);

    // Lay the categories out in columns when the terminal is wide enough. A
    // single 70-column box on a 200-column terminal wasted two thirds of the
    // width and made the list scroll for no reason.
    let columns = split_into_columns(CATEGORIES, column_count(area.width));

    // Render each column's categories to lines, so the tallest one sets the
    // box height and the scroll extent.
    let rendered: Vec<Vec<Line>> = columns.iter().map(|run| render_column(run)).collect();
    let total = rendered.iter().map(Vec::len).max().unwrap_or(0);

    // The box takes what height it can get and scrolls the rest. It used to be
    // clamped with `.min(...)`, which silently *clipped* — everything past the
    // fold, including how to cancel a transfer, was simply unreachable.
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

    // Centre the modal box vertically and horizontally. The box is as wide as
    // the columns it holds, never wider than the screen.
    let box_w = box_width(rendered.len() as u16).min(area.width);
    let outer = Layout::vertical([
        Constraint::Min(0),
        Constraint::Length(box_h),
        Constraint::Min(0),
    ])
    .split(area);
    let box_area = Layout::horizontal([
        Constraint::Min(0),
        Constraint::Length(box_w),
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

    // One column per run, split evenly across the inner width so a narrow
    // terminal that only fits part of a column still degrades gracefully.
    let slots = Layout::horizontal(vec![
        Constraint::Ratio(1, rendered.len() as u32);
        rendered.len()
    ])
    .split(inner);

    // Every column scrolls by the same offset: they are one list read
    // top-to-bottom then left-to-right, not independent panes.
    for (lines, slot) in rendered.into_iter().zip(slots.iter()) {
        let paragraph = Paragraph::new(lines).scroll((state.scroll as u16, 0));
        frame.render_widget(paragraph, *slot);
    }
}

/// Width of one column of bindings: the widest `key` + `desc` line the list
/// produces, plus the gutter that separates it from the next column.
const COLUMN_WIDTH: u16 = 70;

/// Total box width for `columns` columns, borders included.
fn box_width(columns: u16) -> u16 {
    COLUMN_WIDTH * columns + 2
}

/// How many columns of bindings fit in `width`, from one to three.
///
/// Three is the ceiling: past that the columns are short enough that the eye
/// has to hunt across the screen for a section, which is worse than a little
/// unused width.
fn column_count(width: u16) -> usize {
    if width >= box_width(3) {
        3
    } else if width >= box_width(2) {
        2
    } else {
        1
    }
}

/// Split the categories into `n` contiguous runs, balancing their heights.
///
/// Contiguous so the list still reads in its documented order — Navigation
/// first, Dialogs last — and category-aligned so a section is never torn in
/// half across a column boundary. Balanced by minimising the tallest run:
/// simply dividing the total by `n` puts the 48-row Actions section alone with
/// far too little beside it.
fn split_into_columns(cats: &'static [HelpCategory], n: usize) -> Vec<&'static [HelpCategory]> {
    let n = n.clamp(1, cats.len().max(1));
    if n == 1 || cats.is_empty() {
        return vec![cats];
    }

    let heights: Vec<usize> = cats.iter().map(HelpCategory::height).collect();

    // Binary search the smallest column height that still admits `n` runs, then
    // pack greedily at that height. The answer is between the tallest single
    // category (which cannot be split) and the whole list in one run.
    let mut lo = *heights.iter().max().unwrap();
    let mut hi = heights.iter().sum::<usize>();
    while lo < hi {
        let mid = lo + (hi - lo) / 2;
        if runs_needed(&heights, mid) <= n {
            hi = mid;
        } else {
            lo = mid + 1;
        }
    }

    // Pack at the height the search settled on. Greedy packing can finish in
    // fewer runs than asked for when the categories divide unevenly; that is
    // fine, the layout just draws the columns it was given.
    let mut runs = Vec::new();
    let mut start = 0;
    let mut used = 0;
    for (i, h) in heights.iter().enumerate() {
        // Leave a run for each category still to come, so the last columns are
        // never left empty by an over-eager earlier one.
        let remaining_runs = n - runs.len();
        if used + h > lo && remaining_runs > 1 {
            runs.push(&cats[start..i]);
            start = i;
            used = 0;
        }
        used += h;
    }
    runs.push(&cats[start..]);
    runs
}

/// How many runs greedy packing needs to keep every run at or under `limit`.
fn runs_needed(heights: &[usize], limit: usize) -> usize {
    let mut runs = 1;
    let mut used = 0;
    for h in heights {
        if used + h > limit && used > 0 {
            runs += 1;
            used = 0;
        }
        used += h;
    }
    runs
}

/// Render one column's categories into styled lines.
fn render_column(cats: &[HelpCategory]) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    for cat in cats {
        lines.push(Line::from(Span::styled(
            format!("  {}:", cat.title),
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        )));

        for (key, desc) in cat.items {
            lines.push(Line::from(vec![
                // 14 wide, not 13: `j / k / h / l` is exactly 13 characters
                // and ran straight into its description with no gap.
                Span::styled(
                    format!("    {:14}", key),
                    Style::default()
                        .fg(Color::Green)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(*desc, Style::default().fg(Color::White)),
            ]));
        }

        lines.push(Line::from(""));
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every category lands in exactly one column, in order, with none dropped
    /// and no column left empty — an empty run would draw a blank column and a
    /// dropped one would silently lose a section of the reference.
    #[test]
    fn splitting_preserves_every_category() {
        for n in 1..=4 {
            let runs = split_into_columns(CATEGORIES, n);
            assert!(
                runs.iter().all(|r| !r.is_empty()),
                "n={}: no column may be empty",
                n
            );
            assert_eq!(
                runs.iter().map(|r| r.len()).sum::<usize>(),
                CATEGORIES.len(),
                "n={}: every category must appear exactly once",
                n
            );
            // Concatenating the runs reproduces the original order.
            let flat: Vec<&str> = runs
                .iter()
                .flat_map(|r| r.iter().map(|c| c.title))
                .collect();
            let want: Vec<&str> = CATEGORIES.iter().map(|c| c.title).collect();
            assert_eq!(flat, want, "n={}: the reading order must be preserved", n);
        }
    }

    /// The split balances: the tallest column is close to the ideal share, not
    /// the 48-row Actions section sitting alone against everything else.
    #[test]
    fn splitting_balances_the_columns() {
        let total: usize = CATEGORIES.iter().map(HelpCategory::height).sum();
        for n in 2..=3 {
            let runs = split_into_columns(CATEGORIES, n);
            let tallest = runs
                .iter()
                .map(|r| r.iter().map(HelpCategory::height).sum::<usize>())
                .max()
                .unwrap();
            // Categories are indivisible, so perfect balance is impossible; the
            // tallest column must still beat "one category per column" badly.
            let ideal = total.div_ceil(n);
            assert!(
                tallest <= ideal + CATEGORIES.iter().map(HelpCategory::height).max().unwrap(),
                "n={}: tallest column {} is far past the ideal {}",
                n,
                tallest,
                ideal
            );
            // And it must actually be shorter than the undivided list.
            assert!(
                tallest < total,
                "n={}: splitting must reduce the scroll extent",
                n
            );
        }
    }

    /// The breakpoints: a column is only added once its full width is there.
    #[test]
    fn column_count_follows_the_width() {
        assert_eq!(column_count(0), 1);
        assert_eq!(column_count(80), 1);
        assert_eq!(column_count(box_width(2) - 1), 1);
        assert_eq!(column_count(box_width(2)), 2);
        assert_eq!(column_count(box_width(3) - 1), 2);
        assert_eq!(column_count(box_width(3)), 3);
        // Three is the ceiling, however wide the terminal gets.
        assert_eq!(column_count(1000), 3);
    }
}
