//! The preview pane: a scrollable, searchable view of one file's contents.
//!
//! Fills most of the screen, and takes focus so vi motions act on it rather than
//! on the tree behind it. Scroll bounds follow the arrangement used by the help
//! overlay and the file tree — they depend on the drawn area, so they are recorded
//! during render rather than kept in state.

use ratatui::{
    Frame,
    layout::{Alignment, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, BorderType, Borders, Clear, Paragraph},
};
use std::path::PathBuf;

use crate::preview::PreviewContent;
use crate::vfs::BackendId;

/// Highlight for the search match the cursor is on.
const CURRENT_MATCH: Color = Color::Rgb(255, 170, 40);
/// Highlight for the other matches.
const OTHER_MATCH: Color = Color::Rgb(90, 90, 130);

/// Rows a page motion moves before the first frame, when the real height of the
/// pane is not yet known. Matches the file tree's own fallback.
const DEFAULT_VIEWPORT: usize = 20;

/// What a loaded preview describes.
///
/// The pane re-reads when any of these change. Geometry is included because an
/// image has to be re-rendered at a new cell size; a resize that leaves the
/// content area the same size costs nothing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreviewKey {
    pub path: PathBuf,
    pub backend: BackendId,
    pub cols: u16,
    pub rows: u16,
    /// Zero-based page of a multi-page document. Part of the key because turning
    /// a page means re-rendering.
    pub page: usize,
}

/// The preview pane.
pub struct PreviewState {
    /// Loaded content, or `None` while the first load is in flight.
    content: Option<PreviewContent>,
    /// What `content` describes.
    key: Option<PreviewKey>,
    /// The file being previewed, for the title — known before the load finishes.
    title: String,
    scroll: usize,
    /// Largest valid offset, from the last render. Zero until drawn once, so
    /// scrolling before the first frame is a harmless no-op.
    max_scroll: usize,
    /// Visible content rows, for page-sized jumps.
    viewport: usize,
    /// Plain text per line, rebuilt with the content, so a search does not have
    /// to walk spans on every keystroke.
    search_text: Vec<String>,
    pattern: Option<String>,
    matches: Vec<usize>,
    current_match: Option<usize>,
    /// Where the content was last drawn, for mouse hit-testing.
    pub content_area: Option<Rect>,
    /// Where a graphics image should be drawn, in terminal coordinates.
    ///
    /// Recorded during render because only the layout knows it, and read after the
    /// frame is flushed by the code that writes the escape.
    pub graphics_area: Option<Rect>,
    /// Page being shown of a multi-page document, zero-based.
    ///
    /// Kept here rather than derived from the content so it survives the reload
    /// that turning a page causes — the content is dropped while the next page
    /// renders, and the pane still has to know which page it asked for.
    page: usize,
    /// Total pages, once a loaded document reports it.
    pages: Option<usize>,
}

impl Default for PreviewState {
    fn default() -> Self {
        Self::new()
    }
}

impl PreviewState {
    pub fn new() -> Self {
        Self {
            content: None,
            key: None,
            title: String::new(),
            scroll: 0,
            max_scroll: 0,
            viewport: 0,
            search_text: Vec::new(),
            pattern: None,
            matches: Vec::new(),
            current_match: None,
            content_area: None,
            graphics_area: None,
            page: 0,
            pages: None,
        }
    }

    /// What the pane is currently showing, if anything.
    pub fn key(&self) -> Option<&PreviewKey> {
        self.key.as_ref()
    }

    pub fn has_content(&self) -> bool {
        self.content.is_some()
    }

    /// Note that a load is starting: clears the old content so a stale file is
    /// never shown under a new title.
    pub fn begin_load(&mut self, title: String) {
        self.content = None;
        self.key = None;
        self.title = title;
        self.scroll = 0;
        self.max_scroll = 0;
        self.search_text.clear();
        self.matches.clear();
        self.current_match = None;
    }

    /// Install freshly loaded content.
    ///
    /// A search already in place is re-applied, so stepping through a file's
    /// matches survives the re-read that a resize causes.
    pub fn set_content(&mut self, key: PreviewKey, content: PreviewContent) {
        // Learn the page count, and which page actually came back, from the
        // document itself. Taking the page from the content rather than trusting
        // the field keeps the footer honest when a load resolves out of order or
        // renders a different page than the one last requested.
        match &content {
            PreviewContent::Image { pages, page, .. }
            | PreviewContent::Graphics { pages, page, .. } => {
                self.pages = *pages;
                self.page = *page;
            }
            _ => self.pages = None,
        }
        self.search_text = content.search_text();
        self.content = Some(content);
        self.key = Some(key);
        self.scroll = 0;
        if let Some(p) = self.pattern.clone() {
            self.apply_search(&p);
        }
    }

    /// The escape payload of a high-resolution image, if that is what is loaded.
    ///
    /// Returned for the event loop to write to the terminal after the frame; the
    /// pane itself cannot draw it.
    pub fn graphics(&self) -> Option<&str> {
        match &self.content {
            Some(PreviewContent::Graphics { payload, .. }) => Some(payload),
            _ => None,
        }
    }

    /// The document page currently being shown, zero-based.
    ///
    /// Named apart from [`Self::page`], which scrolls by a screenful — one is a
    /// position in a document, the other a movement through text.
    pub fn current_page(&self) -> usize {
        self.page
    }

    /// Whether the pane is showing (or loading) a different file than `path`.
    ///
    /// Used to decide when the page number stops meaning anything.
    pub fn showing_other_path(&self, path: &std::path::Path) -> bool {
        match &self.key {
            Some(k) => k.path != path,
            // Nothing loaded yet: the title is the only clue, and it is only a
            // file name. Treat that as "not a different file" so opening the pane
            // does not immediately reset a page the user just chose.
            None => false,
        }
    }

    pub fn reset_page(&mut self) {
        self.page = 0;
        self.pages = None;
    }

    /// Whether the pane is showing something with pages to turn.
    ///
    /// An ordinary image reports no page count at all and is not paged: `j`/`k`
    /// must scroll it, as they do everywhere else. A single-page PDF is likewise
    /// not paged, or the motions would appear to do nothing. `Some(0)` means a
    /// paged document of unknown length, which is pageable.
    pub fn is_paged(&self) -> bool {
        match self.pages {
            None => false,
            Some(0) => true,
            Some(n) => n > 1,
        }
    }

    /// Turn to the next or previous page, clamped.
    ///
    /// Returns whether the page changed, so the caller only re-reads when it did.
    /// Clamping matters: timg renders *something* for a page past the end rather
    /// than failing, so an unclamped next-page would silently show a blank.
    pub fn step_page(&mut self, forward: bool) -> bool {
        if !self.is_paged() {
            return false;
        }
        let next = if forward {
            match self.pages {
                // Some(0) is "length unknown", so there is nothing to clamp to.
                Some(n) if n > 0 && self.page + 1 >= n => return false,
                _ => self.page + 1,
            }
        } else if self.page == 0 {
            return false;
        } else {
            self.page - 1
        };
        self.page = next;
        true
    }

    /// Jump to the first or last page. Returns whether it moved.
    pub fn to_page_edge(&mut self, last: bool) -> bool {
        if !self.is_paged() {
            return false;
        }
        let target = if last {
            // Without a count there is no known last page, so this is a no-op
            // rather than a guess.
            match self.pages {
                Some(n) if n > 0 => n - 1,
                // No known last page, so this is a no-op rather than a guess.
                _ => return false,
            }
        } else {
            0
        };
        if target == self.page {
            return false;
        }
        self.page = target;
        true
    }

    /// Visible rows from the last render, falling back before the first frame.
    fn viewport(&self) -> usize {
        if self.viewport == 0 {
            DEFAULT_VIEWPORT
        } else {
            self.viewport
        }
    }

    pub fn scroll(&self) -> usize {
        self.scroll
    }

    /// Scroll by `delta` rows, clamped to the content.
    pub fn scroll_by(&mut self, delta: isize) {
        let next = self.scroll as isize + delta;
        self.scroll = next.clamp(0, self.max_scroll as isize) as usize;
    }

    /// A full screenful, keeping one line of overlap for continuity.
    pub fn page(&mut self, down: bool) {
        let step = self.viewport().saturating_sub(1).max(1) as isize;
        self.scroll_by(if down { step } else { -step });
    }

    /// Half a screenful.
    pub fn half_page(&mut self, down: bool) {
        let step = (self.viewport() / 2).max(1) as isize;
        self.scroll_by(if down { step } else { -step });
    }

    pub fn to_top(&mut self) {
        self.scroll = 0;
    }

    pub fn to_bottom(&mut self) {
        self.scroll = self.max_scroll;
    }

    /// Whether a search is active and matched anything.
    pub fn has_matches(&self) -> bool {
        !self.matches.is_empty()
    }

    pub fn match_count(&self) -> usize {
        self.matches.len()
    }

    /// Run a search. An empty pattern clears it; a bad regex is reported.
    ///
    /// Returns an error message for the caller to show, so a typo explains itself
    /// rather than silently matching nothing.
    pub fn search(&mut self, pattern: &str) -> Option<String> {
        if pattern.is_empty() {
            self.pattern = None;
            self.matches.clear();
            self.current_match = None;
            return None;
        }
        // Case-insensitive, like the tree's own search.
        match regex::RegexBuilder::new(pattern)
            .case_insensitive(true)
            .build()
        {
            Ok(_) => {
                self.pattern = Some(pattern.to_string());
                self.apply_search(pattern);
                if self.matches.is_empty() {
                    Some(format!("No matches for '{pattern}'"))
                } else {
                    None
                }
            }
            Err(_) => Some(format!("Invalid pattern '{pattern}'")),
        }
    }

    /// Recompute matches against the current content and jump to the first one
    /// at or after the current position.
    fn apply_search(&mut self, pattern: &str) {
        let Ok(re) = regex::RegexBuilder::new(pattern)
            .case_insensitive(true)
            .build()
        else {
            return;
        };
        self.matches = self
            .search_text
            .iter()
            .enumerate()
            .filter(|(_, l)| re.is_match(l))
            .map(|(i, _)| i)
            .collect();

        self.current_match = self
            .matches
            .iter()
            .position(|&l| l >= self.scroll)
            .or(if self.matches.is_empty() {
                None
            } else {
                Some(0)
            });
        self.reveal_current_match();
    }

    /// Step to the next or previous match, wrapping around.
    pub fn step_match(&mut self, forward: bool) {
        if self.matches.is_empty() {
            return;
        }
        let n = self.matches.len();
        self.current_match = Some(match self.current_match {
            Some(i) if forward => (i + 1) % n,
            Some(i) => (i + n - 1) % n,
            None => 0,
        });
        self.reveal_current_match();
    }

    /// Scroll so the current match is visible, centred where there is room.
    fn reveal_current_match(&mut self) {
        let Some(line) = self.current_match.and_then(|i| self.matches.get(i)).copied() else {
            return;
        };
        let half = self.viewport() / 2;
        let target = line.saturating_sub(half);
        self.scroll = target.min(self.max_scroll);
        // Before the first render `max_scroll` is 0, which would pin the view to
        // the top and lose the jump. Remember where we wanted to be; the next
        // render re-clamps.
        if self.max_scroll == 0 {
            self.scroll = target;
        }
    }

    /// The line number of the current match, for the footer.
    pub fn current_match_line(&self) -> Option<usize> {
        self.current_match.and_then(|i| self.matches.get(i)).copied()
    }

    /// One-based index of the current match, for the footer.
    pub fn current_match_index(&self) -> Option<usize> {
        self.current_match.map(|i| i + 1)
    }
}

/// Draw the preview pane over `area`.
pub fn render(frame: &mut Frame, area: Rect, state: &mut PreviewState, focused: bool) {
    frame.render_widget(Clear, area);

    let border = if focused {
        Color::Cyan
    } else {
        Color::DarkGray
    };

    // Content rows: the area less the top and bottom borders and the footer.
    let inner_h = area.height.saturating_sub(3) as usize;
    let inner_w = area.width.saturating_sub(2);

    let total = state.content.as_ref().map_or(0, |c| c.len());
    state.viewport = inner_h;
    state.max_scroll = total.saturating_sub(inner_h);
    if state.scroll > state.max_scroll {
        state.scroll = state.max_scroll;
    }

    let title = build_title(state, total, inner_h);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(border))
        .title(title)
        .title_alignment(Alignment::Left);
    let inner = block.inner(area);
    frame.render_widget(block, area);

    if inner.height == 0 {
        return;
    }

    // Content above, one-row footer below.
    let content_area = Rect {
        x: inner.x,
        y: inner.y,
        width: inner.width,
        height: inner.height.saturating_sub(1),
    };
    let footer_area = Rect {
        x: inner.x,
        y: inner.y + inner.height.saturating_sub(1),
        width: inner.width,
        height: 1,
    };
    state.content_area = Some(content_area);
    // Only set for graphics, and cleared otherwise, so a stale rect can never
    // leave an image painted over text.
    state.graphics_area = matches!(state.content, Some(PreviewContent::Graphics { .. }))
        .then_some(content_area);

    match &state.content {
        None => {
            frame.render_widget(
                Paragraph::new("Loading…").style(Style::default().fg(Color::DarkGray)),
                content_area,
            );
        }
        Some(PreviewContent::Note { message }) => {
            frame.render_widget(
                Paragraph::new(message.clone())
                    .style(Style::default().fg(Color::Gray))
                    .wrap(ratatui::widgets::Wrap { trim: false }),
                content_area,
            );
        }
        Some(PreviewContent::Text { lines, .. }) => {
            let drawn = decorate_matches(lines, state);
            frame.render_widget(
                Paragraph::new(drawn).scroll((state.scroll as u16, 0)),
                content_area,
            );
        }
        Some(PreviewContent::Graphics { .. }) => {
            // Deliberately nothing: the terminal draws the image itself, into the
            // gap this leaves. Writing cells here would be overwritten by the
            // image, or would overwrite it.
        }
        Some(PreviewContent::Image { lines, .. }) => {
            // Centre the block: the renderers preserve aspect ratio, so the
            // result is usually narrower than the pane.
            let w = crate::widget::ansi::block_width(lines) as u16;
            let pad = inner_w.saturating_sub(w.min(inner_w)) / 2;
            let centred = Rect {
                x: content_area.x + pad,
                y: content_area.y,
                width: content_area.width.saturating_sub(pad),
                height: content_area.height,
            };
            frame.render_widget(
                Paragraph::new(lines.clone()).scroll((state.scroll as u16, 0)),
                centred,
            );
        }
    }

    frame.render_widget(
        Paragraph::new(build_footer(state, focused)).style(Style::default().fg(Color::DarkGray)),
        footer_area,
    );
}

/// Title: the filename, plus the scroll position when there is more than fits.
///
/// The position indicator is there because a pane that silently clips looks
/// identical to one showing the whole file.
fn build_title(state: &PreviewState, total: usize, viewport: usize) -> String {
    if total > viewport && viewport > 0 {
        let first = state.scroll + 1;
        let last = (state.scroll + viewport).min(total);
        format!(" {} — {}-{} of {} ", state.title, first, last, total)
    } else {
        format!(" {} ", state.title)
    }
}

fn build_footer(state: &PreviewState, focused: bool) -> Line<'static> {
    let mut spans = Vec::new();

    let backend = match &state.content {
        Some(PreviewContent::Image { backend, .. })
        | Some(PreviewContent::Graphics { backend, .. }) => Some(*backend),
        _ => None,
    };
    if let Some(backend) = backend {
        spans.push(Span::styled(
            format!(" {backend} "),
            Style::default().fg(Color::Cyan),
        ));
        // When a capable-looking terminal still got blocks, say what would have
        // to change. Silently falling back is what made this hard to diagnose in
        // the first place.
        if let (Some(PreviewContent::Image { .. }), Some(why)) =
            (&state.content, crate::preview::graphics::explain())
        {
            spans.push(Span::styled(
                format!(" {why} "),
                Style::default().fg(OTHER_MATCH),
            ));
        }
        // Whether a cell size was measured decides how an image is sized, and it
        // is otherwise invisible — so a graphics preview says which it used.
        if matches!(state.content, Some(PreviewContent::Graphics { .. })) {
            let how = match crate::preview::graphics::cell_size() {
                Some((w, h)) => format!(" {w}x{h}px cells "),
                None => " cell size unknown ".to_string(),
            };
            spans.push(Span::styled(how, Style::default().fg(OTHER_MATCH)));
        }
        if state.is_paged() {
            let label = match state.pages {
                Some(n) if n > 0 => format!(" page {}/{} ", state.page + 1, n),
                // Length unknown, so only say where we are.
                _ => format!(" page {} ", state.page + 1),
            };
            spans.push(Span::styled(label, Style::default().fg(CURRENT_MATCH)));
        }
    }
    if let Some(PreviewContent::Text {
        truncated: true, ..
    }) = &state.content
    {
        spans.push(Span::styled(
            " truncated ",
            Style::default().fg(CURRENT_MATCH),
        ));
    }

    if let Some(pattern) = &state.pattern {
        let label = match (state.current_match_index(), state.match_count()) {
            (Some(i), n) if n > 0 => format!(" /{pattern}  {i}/{n} "),
            _ => format!(" /{pattern}  no matches "),
        };
        spans.push(Span::styled(label, Style::default().fg(CURRENT_MATCH)));
    }

    spans.push(Span::raw(if !focused {
        " Tab to focus  space to close "
    } else if state.is_paged() {
        " j/k page  space close  Esc unfocus "
    } else {
        " j/k scroll  / search  n/p match  space close  Esc unfocus "
    }));
    Line::from(spans)
}

/// Add match highlighting to the lines that contain one.
///
/// Applied at draw time rather than baked into the content so that clearing a
/// search does not mean re-reading the file.
fn decorate_matches(lines: &[Line<'static>], state: &PreviewState) -> Vec<Line<'static>> {
    if state.matches.is_empty() {
        return lines.to_vec();
    }
    let current = state.current_match_line();
    let mut out = lines.to_vec();
    for &i in &state.matches {
        let Some(line) = out.get_mut(i) else { continue };
        let colour = if Some(i) == current {
            CURRENT_MATCH
        } else {
            OTHER_MATCH
        };
        // Mark the whole row: highlighting the exact span would mean re-running
        // the regex over styled spans on every frame, and the row is what the
        // motions move between.
        for span in &mut line.spans {
            span.style = span.style.bg(colour);
            if Some(i) == current {
                span.style = span.style.fg(Color::Black).add_modifier(Modifier::BOLD);
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::{Terminal, backend::TestBackend};

    fn text(lines: usize) -> PreviewContent {
        PreviewContent::Text {
            lines: (0..lines)
                .map(|i| Line::from(format!("line {i} content")))
                .collect(),
            truncated: false,
        }
    }

    fn key() -> PreviewKey {
        PreviewKey {
            path: PathBuf::from("/tmp/a.txt"),
            backend: BackendId::LOCAL,
            cols: 80,
            rows: 24,
            page: 0,
        }
    }

    fn draw(state: &mut PreviewState, w: u16, h: u16) -> String {
        let mut t = Terminal::new(TestBackend::new(w, h)).unwrap();
        t.draw(|f| render(f, f.area(), state, true)).unwrap();
        let buf = t.backend().buffer().clone();
        (0..buf.area.height)
            .map(|y| {
                (0..buf.area.width)
                    .map(|x| buf[(x, y)].symbol())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn scrolling_clamps_at_both_ends() {
        let mut s = PreviewState::new();
        s.set_content(key(), text(100));
        draw(&mut s, 40, 14); // 14 - 3 = 11 content rows

        s.scroll_by(-5);
        assert_eq!(s.scroll(), 0, "scrolled above the top");

        s.to_bottom();
        let bottom = s.scroll();
        assert_eq!(bottom, 100 - 11);
        s.scroll_by(50);
        assert_eq!(s.scroll(), bottom, "scrolled past the end");
    }

    #[test]
    fn a_file_shorter_than_the_pane_does_not_scroll() {
        let mut s = PreviewState::new();
        s.set_content(key(), text(3));
        draw(&mut s, 40, 20);
        s.scroll_by(10);
        assert_eq!(s.scroll(), 0);
    }

    #[test]
    fn paging_moves_about_a_screen() {
        let mut s = PreviewState::new();
        s.set_content(key(), text(100));
        draw(&mut s, 40, 14); // 11 rows
        s.page(true);
        assert_eq!(s.scroll(), 10); // one row of overlap
        s.half_page(true);
        assert_eq!(s.scroll(), 15);
        s.page(false);
        assert_eq!(s.scroll(), 5);
    }

    #[test]
    fn search_finds_matching_lines_and_steps_with_wraparound() {
        let mut s = PreviewState::new();
        s.set_content(key(), text(50));
        draw(&mut s, 40, 14);

        assert_eq!(s.search("line 4"), None);
        // line 4, 40-49 => 11 matches
        assert_eq!(s.match_count(), 11);
        assert_eq!(s.current_match_line(), Some(4));

        s.step_match(true);
        assert_eq!(s.current_match_line(), Some(40));

        // Wrap forward off the end.
        for _ in 0..10 {
            s.step_match(true);
        }
        assert_eq!(s.current_match_line(), Some(4), "did not wrap forward");

        // And backward off the start.
        s.step_match(false);
        assert_eq!(s.current_match_line(), Some(49), "did not wrap backward");
    }

    #[test]
    fn an_empty_pattern_clears_the_search() {
        let mut s = PreviewState::new();
        s.set_content(key(), text(20));
        s.search("line 1");
        assert!(s.has_matches());
        assert_eq!(s.search(""), None);
        assert!(!s.has_matches());
    }

    #[test]
    fn a_bad_pattern_is_reported_rather_than_ignored() {
        let mut s = PreviewState::new();
        s.set_content(key(), text(10));
        let msg = s.search("*bad(").expect("should report");
        assert!(msg.contains("Invalid pattern"), "{msg}");
        assert!(!s.has_matches());
    }

    #[test]
    fn a_pattern_with_no_matches_says_so() {
        let mut s = PreviewState::new();
        s.set_content(key(), text(10));
        let msg = s.search("zzzznope").expect("should report");
        assert!(msg.contains("No matches"), "{msg}");
    }

    #[test]
    fn stepping_before_any_search_is_a_no_op() {
        let mut s = PreviewState::new();
        s.set_content(key(), text(10));
        s.step_match(true);
        assert_eq!(s.scroll(), 0);
    }

    #[test]
    fn a_search_scrolls_the_match_into_view() {
        let mut s = PreviewState::new();
        s.set_content(key(), text(200));
        draw(&mut s, 40, 14);
        s.search("line 150");
        assert!(s.scroll() > 100, "match not revealed: {}", s.scroll());
        assert!(s.scroll() <= 150);
    }

    /// A search survives the re-read that a resize triggers, so stepping through
    /// matches is not interrupted by the pane changing size.
    #[test]
    fn a_search_survives_reloaded_content() {
        let mut s = PreviewState::new();
        s.set_content(key(), text(50));
        s.search("line 4");
        let before = s.match_count();
        s.set_content(key(), text(50));
        assert_eq!(s.match_count(), before);
    }

    #[test]
    fn the_title_shows_the_scroll_position() {
        let mut s = PreviewState::new();
        s.begin_load("a.txt".to_string());
        s.set_content(key(), text(100));
        let out = draw(&mut s, 46, 14);
        assert!(out.contains("a.txt"), "{out}");
        assert!(out.contains("1-11 of 100"), "{out}");
    }

    /// A short file must not claim a range: that is the difference between "this
    /// is all of it" and "there is more you cannot see".
    #[test]
    fn a_short_file_shows_no_range() {
        let mut s = PreviewState::new();
        s.begin_load("a.txt".to_string());
        s.set_content(key(), text(2));
        let out = draw(&mut s, 46, 20);
        assert!(!out.contains(" of "), "{out}");
    }

    #[test]
    fn a_loading_pane_says_so() {
        let mut s = PreviewState::new();
        s.begin_load("big.log".to_string());
        let out = draw(&mut s, 40, 10);
        assert!(out.contains("Loading"), "{out}");
        assert!(out.contains("big.log"), "{out}");
    }

    #[test]
    fn a_note_is_shown_for_unpreviewable_files() {
        let mut s = PreviewState::new();
        s.begin_load("ls".to_string());
        s.set_content(
            key(),
            PreviewContent::Note {
                message: "Binary file (1.2 MB).".to_string(),
            },
        );
        let out = draw(&mut s, 50, 10);
        assert!(out.contains("Binary file"), "{out}");
    }

    /// The footer names the tool that drew an image, so it is clear which backend
    /// is in use when both are installed.
    #[test]
    fn the_footer_names_the_image_backend() {
        let mut s = PreviewState::new();
        s.begin_load("x.png".to_string());
        s.set_content(
            key(),
            PreviewContent::Image {
                lines: vec![Line::from("▀▀▀")],
                backend: "timg",
                page: 0,
                pages: None,
            },
        );
        let out = draw(&mut s, 60, 10);
        assert!(out.contains("timg"), "{out}");
    }

    fn pdf(page: usize, pages: Option<usize>) -> PreviewContent {
        PreviewContent::Image {
            lines: (0..6).map(|i| Line::from(format!("page row {i}"))).collect(),
            backend: "timg",
            page,
            pages,
        }
    }

    /// An ordinary image reports no page count and must scroll, not page —
    /// otherwise j/k on a tall photo would silently do nothing.
    #[test]
    fn a_plain_image_is_not_paged() {
        let mut s = PreviewState::new();
        s.set_content(key(), pdf(0, None));
        assert!(!s.is_paged());
        assert!(!s.step_page(true), "an image has no pages to turn");
    }

    /// A one-page PDF is not paged either, for the same reason.
    #[test]
    fn a_single_page_document_is_not_paged() {
        let mut s = PreviewState::new();
        s.set_content(key(), pdf(0, Some(1)));
        assert!(!s.is_paged());
    }

    #[test]
    fn pages_step_and_clamp_at_both_ends() {
        let mut s = PreviewState::new();
        s.set_content(key(), pdf(0, Some(3)));
        assert!(s.is_paged());

        assert!(!s.step_page(false), "already on the first page");
        assert_eq!(s.current_page(), 0);

        assert!(s.step_page(true));
        assert_eq!(s.current_page(), 1);
        assert!(s.step_page(true));
        assert_eq!(s.current_page(), 2);
        // timg renders *something* past the end rather than failing, so the
        // clamp is what stops a blank page being shown.
        assert!(!s.step_page(true), "must clamp at the last page");
        assert_eq!(s.current_page(), 2);

        assert!(s.step_page(false));
        assert_eq!(s.current_page(), 1);
    }

    #[test]
    fn g_and_shift_g_jump_between_first_and_last_page() {
        let mut s = PreviewState::new();
        s.set_content(key(), pdf(0, Some(19)));
        assert!(s.to_page_edge(true));
        assert_eq!(s.current_page(), 18);
        assert!(!s.to_page_edge(true), "already there");
        assert!(s.to_page_edge(false));
        assert_eq!(s.current_page(), 0);
    }

    /// Without pdfinfo the length is unknown: paging forward must still work, and
    /// jumping to the end must not invent a page number.
    #[test]
    fn an_unknown_length_document_pages_forward_but_has_no_end() {
        let mut s = PreviewState::new();
        s.set_content(key(), pdf(0, Some(0)));
        assert!(s.is_paged());
        assert!(s.step_page(true));
        assert_eq!(s.current_page(), 1);
        assert!(!s.to_page_edge(true), "no known last page to jump to");
    }

    /// The page number means nothing once a different file is shown.
    #[test]
    fn the_page_resets_for_another_file() {
        let mut s = PreviewState::new();
        s.set_content(key(), pdf(0, Some(5)));
        s.step_page(true);
        assert_eq!(s.current_page(), 1);

        assert!(s.showing_other_path(std::path::Path::new("/tmp/other.pdf")));
        s.reset_page();
        assert_eq!(s.current_page(), 0);
        assert!(!s.showing_other_path(std::path::Path::new("/tmp/a.txt")));
    }

    #[test]
    fn the_footer_shows_the_page_position() {
        let mut s = PreviewState::new();
        s.begin_load("spec.pdf".to_string());
        s.set_content(key(), pdf(4, Some(19)));
        let out = draw(&mut s, 70, 14);
        assert!(out.contains("page 5/19"), "{out}");
    }

    /// With no count, show where we are without claiming a total.
    #[test]
    fn the_footer_omits_an_unknown_page_total() {
        let mut s = PreviewState::new();
        s.begin_load("spec.pdf".to_string());
        s.set_content(key(), pdf(2, Some(0)));
        let out = draw(&mut s, 70, 14);
        assert!(out.contains("page 3"), "{out}");
        assert!(!out.contains("page 3/"), "should not claim a total: {out}");
    }

    #[test]
    fn a_truncated_file_is_flagged() {
        let mut s = PreviewState::new();
        s.begin_load("big.log".to_string());
        s.set_content(
            key(),
            PreviewContent::Text {
                lines: vec![Line::from("x")],
                truncated: true,
            },
        );
        let out = draw(&mut s, 70, 10);
        assert!(out.contains("truncated"), "{out}");
    }

    /// Starting a load must drop the old content, or the pane shows one file's
    /// text under another's name.
    #[test]
    fn beginning_a_load_clears_the_previous_file() {
        let mut s = PreviewState::new();
        s.set_content(key(), text(10));
        assert!(s.has_content());
        s.begin_load("other.txt".to_string());
        assert!(!s.has_content());
        assert!(s.key().is_none());
    }

    #[test]
    fn a_zero_height_area_does_not_panic() {
        let mut s = PreviewState::new();
        s.set_content(key(), text(10));
        let mut t = Terminal::new(TestBackend::new(20, 1)).unwrap();
        t.draw(|f| render(f, f.area(), &mut s, true)).unwrap();
    }

    #[test]
    fn the_content_area_is_recorded_for_hit_testing() {
        let mut s = PreviewState::new();
        s.set_content(key(), text(10));
        draw(&mut s, 40, 14);
        let area = s.content_area.expect("recorded");
        assert!(area.height > 0 && area.width > 0);
    }
}
