use ratatui::{
    Frame,
    layout::{Constraint, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span, Text},
    widgets::{
        Block, BorderType, Borders, Paragraph, Scrollbar, ScrollbarOrientation, ScrollbarState,
    },
};
use std::path::PathBuf;

use super::{ScreenState, SortMode};
use crate::widget::file_info;
use crate::widget::file_tree::FileTree;
use crate::widget::treemap::{FocusTarget, TreeMap};

pub struct MainScreenState {
    root_path: PathBuf,
    pub tree: FileTree,
    /// Treemap built from the current tree data (rebuilt when tree changes).
    treemap: TreeMap,
    /// Which view has focus: file tree or treemap.
    pub focus: FocusTarget,
    pub info_panel_hidden: bool,
    /// Info panel width, as a percentage of this panel. Carried on
    /// [`crate::panel::ViewPrefs`] and applied to every new screen.
    pub info_panel_pct: u16,
    /// Which editable field the info panel's cursor is on.
    ///
    /// Only drawn, and only meaningful, while the panel has focus.
    pub info_field: crate::widget::file_info::InfoField,
    /// Render hint: whether this panel's info panel holds the keyboard. Set by
    /// the app before drawing, like `active`, since focus is app-level state.
    pub info_active: bool,
    /// Rows added to (or taken from) the metadata's share of the info panel,
    /// set by `+` and `-`.
    ///
    /// A bias rather than an absolute height so it means the same thing at
    /// every terminal size: the split is derived from [`META_ROWS`], and this
    /// nudges it. Carried on [`crate::panel::ViewPrefs`] like the width.
    pub info_meta_bias: i16,
    /// The rect the info panel reserved for its preview sub-panel, or `None`
    /// when it did not fit.
    ///
    /// Recorded during render and filled in by the app afterwards: the preview
    /// content and the backend registry both live there, and `render_info` runs
    /// inside `terminal.draw`, where starting I/O would be wrong.
    pub info_preview_area: Option<Rect>,
    /// Render hint: whether this screen belongs to the active panel. In
    /// single-panel mode it is always the active one; in dual mode the app sets
    /// it before drawing each panel so the active panel's border stands out.
    pub active: bool,
    /// Cached info panel text — only recomputed when selection changes.
    cached_info_text: Text<'static>,
    /// What `cached_info_text` describes: the resolved path it was built from,
    /// and the view that was focused at the time.
    ///
    /// Keyed on the full path rather than the display name because two entries
    /// in different directories can share a basename — keying on the name alone
    /// let one view's panel show the other's stale text. The focus is part of
    /// the key so switching views always re-reads the newly focused cursor.
    cached_info_key: Option<(
        PathBuf,
        FocusTarget,
        Option<crate::widget::file_info::InfoField>,
    )>,
    /// The most recent search regex, so `n` / `p` can repeat it to the next /
    /// previous match without re-prompting.
    last_search: Option<regex::Regex>,
    /// In-progress transfer destinations that fall inside this panel's tree,
    /// drawn as "ghost" rows. Set by the app each frame before rendering (like
    /// `active`); empty when nothing is being transferred here.
    pub pending_ghosts: Vec<crate::transfer::PendingDest>,
    /// The rect the file tree last rendered into, and the scroll offset it used.
    ///
    /// Recorded during render because the offset is derived from the area's
    /// height, so it cannot be recovered afterwards from state alone. Mouse
    /// hit-testing needs both to turn a screen row into a tree line.
    pub tree_area: Option<Rect>,
    pub tree_scroll: usize,
    /// Content rows the tree last rendered into — `area.height - 3` (top border
    /// plus title bar, bottom border). Recorded during render for the same
    /// reason as `tree_scroll`: it depends on the area, so it cannot be recovered
    /// from state alone.
    ///
    /// Zero until the first frame; the page motions fall back to
    /// [`DEFAULT_VIEWPORT`] so a key pressed before the first draw still moves a
    /// sensible amount.
    pub tree_viewport: usize,
    /// Where the "Sort: …" indicator was drawn in the title bar, so a click on
    /// it can open the sort menu. Recorded during render like `tree_area`.
    pub sort_area: Option<Rect>,
    /// A chord prefix waiting for its second key, set by the app each frame like
    /// `active`.
    ///
    /// Chords no longer time out, so a pending `g` would otherwise be invisible
    /// state — the app would look like it had ignored the key and then behave
    /// oddly on the next one. Showing it makes the wait explainable.
    pub pending_chord: Option<char>,
    /// What this panel's footer should show, set by the app each frame like
    /// `active`.
    ///
    /// The footer is drawn inside the panel, but it describes whatever has
    /// focus — and Tab can move that to the transfer sidebar, which is not a
    /// panel and has its own keys. Without this the footer went on advertising
    /// `t:tag` and `f:filter` while none of them did anything, and the
    /// sidebar's own keys were undiscoverable.
    pub footer: FooterMode,
    /// Where to draw the footer, when that is not simply the bottom of this
    /// panel. Set by the app each frame like `active`.
    ///
    /// Exactly one pane's keys are ever on screen, so in a split only the
    /// focused panel draws a footer — and it draws it across the whole frame
    /// rather than its own half, since the keys describe the window. The
    /// transfer sidebar stops above this row to leave it clear.
    ///
    /// `None` means "the bottom row of my own area", which is what a lone panel
    /// with no sidebar wants.
    pub footer_rect: Option<Rect>,
}

/// Which keys a panel's footer describes.
///
/// Every panel has a footer of its own, but there is only one keyboard — so
/// when focus is somewhere that is not a panel, exactly one panel shows those
/// keys and the rest show nothing. Two panels each drawing a line is how both
/// halves of this went wrong in turn: first the inactive one kept advertising
/// the tree's keys next to the sidebar's, then both drew the sidebar's.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum FooterMode {
    /// This panel has (or would have) the keyboard: show its own keys.
    #[default]
    Own,
    /// The transfer sidebar has the keyboard; this panel speaks for it.
    Transfers,
    /// Something else is speaking, so this panel stays quiet.
    Hidden,
}

/// A user-facing explanation of why a regex would not compile.
///
/// `regex`'s own error is several lines of caret-annotated detail aimed at a
/// programmer; the first line carries the actual reason ("repetition operator
/// missing expression" and the like), which is what someone who typed `*p$`
/// needs to see.
fn bad_pattern_message(pattern: &str, err: &regex::Error) -> String {
    let reason = err
        .to_string()
        .lines()
        .map(str::trim)
        .rfind(|l| !l.is_empty() && !l.starts_with('^') && !l.contains("regex parse error"))
        .unwrap_or("invalid pattern")
        .to_string();
    format!("Invalid pattern '{}': {}", pattern, reason)
}

/// Lines a page motion moves before the first frame has been drawn and the real
/// viewport height is known. The value the page motions used unconditionally
/// before they were taught to measure the terminal.
const DEFAULT_VIEWPORT: usize = 20;

/// Rows the preview sub-panel needs before it is worth drawing: a separator and
/// enough content to be more than a tease.
const MIN_PREVIEW_ROWS: u16 = 6;

/// Columns below which a preview is unreadable — narrower than this and even a
/// single token of source is truncated.
const MIN_PREVIEW_COLS: u16 = 20;

/// Rows the metadata gets when the panel is tall enough to have a preview.
///
/// The dense layout is a fixed size — name, type, size, the three editable
/// fields, three timestamps and the path, plus a row for a directory's item
/// count — so this is all of it with one row to spare. Everything above it goes
/// to the preview, which is the part that can actually use more room.
///
/// `+` and `-` shift it by [`MainScreenState::info_meta_bias`], which is
/// bounded by [`crate::prefs::MAX_META_BIAS`].
const META_ROWS: u16 = 11;

/// Rows the metadata keeps for itself before any preview is offered at all.
///
/// Below this the fields that matter — name, type, size — are already going, so
/// the preview yields instead.
const MIN_META_ROWS: u16 = 10;

impl MainScreenState {
    pub fn new(root_path: PathBuf) -> Self {
        let tree = FileTree::new(root_path.clone(), SortMode::Largest, true, true);
        let treemap = TreeMap::from_file_tree(&tree);
        Self {
            root_path,
            tree,
            treemap,
            focus: FocusTarget::Tree,
            info_panel_hidden: true,
            info_panel_pct: crate::prefs::DEFAULT_INFO_PCT,
            info_field: crate::widget::file_info::InfoField::default(),
            info_active: false,
            info_meta_bias: 0,
            info_preview_area: None,
            active: true,
            cached_info_text: Text::default(),
            cached_info_key: None,
            last_search: None,
            pending_ghosts: Vec::new(),
            tree_area: None,
            tree_scroll: 0,
            tree_viewport: 0,
            sort_area: None,
            pending_chord: None,
            footer: FooterMode::Own,
            footer_rect: None,
        }
    }

    /// Create from a pre-built tree (used after loading completes).
    pub fn from_tree(root_path: PathBuf, tree: FileTree) -> Self {
        let treemap = TreeMap::from_file_tree(&tree);
        Self {
            root_path,
            tree,
            treemap,
            focus: FocusTarget::Tree,
            info_panel_hidden: true,
            info_panel_pct: crate::prefs::DEFAULT_INFO_PCT,
            info_field: crate::widget::file_info::InfoField::default(),
            info_active: false,
            info_meta_bias: 0,
            info_preview_area: None,
            active: true,
            cached_info_text: Text::default(),
            cached_info_key: None,
            last_search: None,
            pending_ghosts: Vec::new(),
            tree_area: None,
            tree_scroll: 0,
            tree_viewport: 0,
            sort_area: None,
            pending_chord: None,
            footer: FooterMode::Own,
            footer_rect: None,
        }
    }

    /// The directory this screen is rooted at — the copy destination when this
    /// screen is the *other* panel.
    pub fn root_path(&self) -> &PathBuf {
        &self.root_path
    }

    /// Adopt session-wide view preferences. Applied to freshly built screens so
    /// navigating into a directory preserves how the user has set the view up.
    ///
    /// Takes the whole struct rather than a list of bools, which would be easy to
    /// transpose silently.
    ///
    /// `sort_mode` is applied here too. It used to be left out on the reasoning
    /// that the order has to be known before the tree is built and so should be
    /// threaded through the loading screen — which is true of *how the tree is
    /// built*, but left the preference unenforced wherever a caller forgot to
    /// pass it, and the picker did. Applying it here as well costs an in-memory
    /// resort on the rare arrival that disagrees, and makes the preference hold
    /// by construction rather than by every call site remembering.
    pub fn apply_view_prefs(&mut self, prefs: crate::panel::ViewPrefs) {
        self.info_panel_hidden = prefs.info_panel_hidden;
        self.info_panel_pct = prefs.info_panel_pct;
        self.info_meta_bias = prefs.info_meta_bias;
        self.focus = prefs.focus;
        self.tree.show_perms = prefs.show_perms;
        self.tree.show_times = prefs.show_times;
        // The sort order is a view preference like the rest, and was the one
        // this forgot to apply. `ViewPrefs` carried it and every toggle wrote
        // it, but nothing read it back — so the order held only where a caller
        // happened to thread it through by hand (entering a directory did), and
        // reset to the default everywhere else: the picker, a changed root, a
        // shallow re-open.
        //
        // Resorting is pure reordering of nodes already in memory — no I/O — so
        // it is safe on every arrival. Skipped when it already matches to avoid
        // a needless reflatten on the common path.
        if self.tree.sort_mode != prefs.sort_mode {
            self.tree.set_sort_mode(prefs.sort_mode);
            self.rebuild_treemap();
        }
    }

    /// Visible content rows in the tree, from the last render.
    ///
    /// Falls back to [`DEFAULT_VIEWPORT`] before the first frame, so a page
    /// motion dispatched ahead of any draw still moves the distance it always
    /// used to.
    fn viewport(&self) -> usize {
        if self.tree_viewport == 0 {
            DEFAULT_VIEWPORT
        } else {
            self.tree_viewport
        }
    }

    /// Whether the info panel's text is currently cached. Test hook: sorting
    /// must not invalidate it, since rebuilding costs filesystem calls.
    pub fn info_cache_key_for_test(&self) -> bool {
        self.cached_info_key.is_some()
    }

    /// Rebuild the treemap from the current tree (call after tree structure changes).
    fn rebuild_treemap(&mut self) {
        self.treemap = TreeMap::from_file_tree(&self.tree);
    }

    /// Rebuild the treemap *and* drop the cached info-panel text.
    ///
    /// Only for changes that alter what the panel would say about the selected
    /// entry — a reload or refresh, where sizes and child counts may differ.
    /// Re-sorting is not such a change: it reorders rows without altering any
    /// of them, and rebuilding the panel means a stat, a canonicalize and a
    /// `read_dir` of the selected directory. On a network filesystem that is
    /// several round trips, paid on every press of `s`.
    fn rebuild_treemap_and_info(&mut self) {
        self.rebuild_treemap();
        self.cached_info_key = None;
    }

    /// Drop the cached info text so the next frame rebuilds it.
    ///
    /// For a change that alters what the panel *says* about an entry without
    /// altering the listing — a new mode or owner. `refresh()` would also do
    /// this, but it rebuilds the tree from disk and resets the cursor to the
    /// root, which leaves the panel describing a different file than the one
    /// just edited.
    pub fn invalidate_info_cache(&mut self) {
        self.cached_info_key = None;
    }

    /// Move the cursor to whatever was drawn at screen position `(x, y)`.
    ///
    /// Returns whether the click landed on something selectable. Uses the rect
    /// and scroll offset recorded during the last render, so a click always
    /// refers to what the user actually saw.
    pub fn click_at(&mut self, x: u16, y: u16) -> bool {
        // Treemap first: its cells already carry their rects.
        if self.focus == FocusTarget::Treemap {
            if let Some(i) = self.treemap.cell_at(x, y) {
                self.treemap.cursor = i;
                self.cached_info_key = None;
                return true;
            }
            return false;
        }

        let Some(area) = self.tree_area else {
            return false;
        };
        if x < area.x || x >= area.x + area.width || y < area.y || y >= area.y + area.height {
            return false;
        }
        // Row 0 of the box is the top border and title; content starts below it,
        // and the bottom border is not a row.
        if y <= area.y || y + 1 >= area.y + area.height {
            return false;
        }
        let row = (y - area.y - 1) as usize;
        let index = self.tree_scroll + row;
        if index < self.tree.lines.len() {
            // Assigned directly rather than through `set_cursor`: a click is a
            // fresh selection, not a motion, and should not extend an in-progress
            // visual-mode range.
            self.tree.cursor = index;
            self.cached_info_key = None;
            return true;
        }
        false
    }

    /// Whether `(x, y)` falls inside the area the tree last rendered into.
    pub fn contains(&self, x: u16, y: u16) -> bool {
        match self.tree_area {
            Some(a) => x >= a.x && x < a.x + a.width && y >= a.y && y < a.y + a.height,
            None => false,
        }
    }

    pub fn cursor_down(&mut self) -> bool {
        match self.focus {
            FocusTarget::Tree => self.tree.cursor_down(),
            FocusTarget::Treemap => self.treemap.cursor_down(),
        }
        true
    }

    pub fn cursor_up(&mut self) -> bool {
        match self.focus {
            FocusTarget::Tree => self.tree.cursor_up(),
            FocusTarget::Treemap => self.treemap.cursor_up(),
        }
        true
    }

    pub fn to_top(&mut self) -> bool {
        match self.focus {
            FocusTarget::Tree => self.tree.to_top(),
            FocusTarget::Treemap => self.treemap.cursor = 0,
        }
        true
    }

    pub fn to_bottom(&mut self) -> bool {
        match self.focus {
            FocusTarget::Tree => self.tree.to_bottom(),
            FocusTarget::Treemap => {
                self.treemap.cursor = self.treemap.cells.len().saturating_sub(1)
            }
        }
        true
    }

    /// Move the focused view's cursor by `delta` rows, clamped to its contents.
    ///
    /// Goes through [`FileTree::set_cursor`] rather than assigning `tree.cursor`,
    /// so a page jump tags the range it crossed while visual mode is active.
    fn move_cursor_by(&mut self, delta: isize) -> bool {
        match self.focus {
            FocusTarget::Tree => {
                let last = self.tree.lines.len().saturating_sub(1) as isize;
                let target = (self.tree.cursor as isize + delta).clamp(0, last.max(0));
                self.tree.set_cursor(target as usize);
            }
            FocusTarget::Treemap => {
                let last = self.treemap.cells.len().saturating_sub(1) as isize;
                let target = (self.treemap.cursor as isize + delta).clamp(0, last.max(0));
                self.treemap.cursor = target as usize;
            }
        }
        true
    }

    /// Rows a page motion covers in the focused view.
    ///
    /// The tree pages by the terminal's real content height — it used to page by
    /// a hardcoded 20, which is a third of a screen on a tall terminal and an
    /// overshoot on a short one. The treemap is a 2-D layout with no row-based
    /// viewport, so a fixed step is all that means anything there.
    fn page_step(&self) -> usize {
        match self.focus {
            FocusTarget::Tree => self.viewport(),
            FocusTarget::Treemap => 5,
        }
    }

    pub fn page_down(&mut self) -> bool {
        self.move_cursor_by(self.page_step() as isize)
    }

    pub fn page_up(&mut self) -> bool {
        self.move_cursor_by(-(self.page_step() as isize))
    }

    pub fn half_page_down(&mut self) -> bool {
        self.move_cursor_by((self.page_step() / 2).max(1) as isize)
    }

    pub fn half_page_up(&mut self) -> bool {
        self.move_cursor_by(-((self.page_step() / 2).max(1) as isize))
    }

    pub fn expand(&mut self) -> bool {
        match self.focus {
            FocusTarget::Tree => self.tree.expand_cursor(),
            FocusTarget::Treemap => self.treemap.cursor_right(),
        }
        true
    }

    pub fn collapse(&mut self) -> bool {
        match self.focus {
            FocusTarget::Tree => self.tree.collapse_cursor(),
            FocusTarget::Treemap => self.treemap.cursor_left(),
        }
        true
    }

    /// Navigate: expand in place if already expanded, collapse if expanded,
    /// or expand if not. Screen push for subdirectory navigation is handled
    /// by the app layer.
    pub fn navigate(&mut self) {
        self.tree.navigate();
    }

    pub fn go_parent(&mut self) -> bool {
        match self.focus {
            FocusTarget::Tree => self.tree.go_parent(),
            FocusTarget::Treemap => self.treemap.cursor_left(),
        }
        true
    }

    pub fn toggle_sort(&mut self) -> bool {
        // The cycle order lives on SortMode so `s`, the sort menu and the help
        // text cannot drift apart — this used to keep its own copy of the list.
        let modes = SortMode::ALL;
        let current_idx = modes.iter().position(|m| *m == self.tree.sort_mode).unwrap_or(0);
        let next_idx = (current_idx + 1) % modes.len();
        self.set_sort_mode(modes[next_idx])
    }

    /// Apply a sort order directly, as the sort menu does.
    pub fn set_sort_mode(&mut self, mode: SortMode) -> bool {
        // Timed in three parts so a slow sort can be attributed to reordering,
        // reflattening, or the treemap rebuild rather than guessed at.
        let trace = crate::app::trace_enabled();
        let t0 = trace.then(std::time::Instant::now);
        self.tree.set_sort_mode(mode);
        let after_sort = trace.then(std::time::Instant::now);
        self.rebuild_treemap();
        if let (Some(t0), Some(after_sort)) = (t0, after_sort) {
            crate::app::trace_note(format_args!(
                "  set_sort_mode({}): reorder+reflatten={:.1}ms treemap={:.1}ms lines={}",
                mode.label(),
                after_sort.duration_since(t0).as_secs_f64() * 1000.0,
                after_sort.elapsed().as_secs_f64() * 1000.0,
                self.tree.lines.len(),
            ));
        }
        true
    }

    pub fn toggle_hidden(&mut self) -> bool {
        self.tree.toggle_hidden();
        self.rebuild_treemap();
        true
    }

    pub fn toggle_bar(&mut self) -> bool {
        self.tree.show_size_bar = !self.tree.show_size_bar;
        true
    }

    /// Show or hide the permissions column. A pure display flag, so no treemap
    /// rebuild — the same reasoning as `toggle_bar`.
    pub fn toggle_perms(&mut self) -> bool {
        self.tree.show_perms = !self.tree.show_perms;
        true
    }

    /// Show or hide the modification-time column.
    pub fn toggle_times(&mut self) -> bool {
        self.tree.show_times = !self.tree.show_times;
        true
    }

    pub fn collapse_all(&mut self) -> bool {
        self.tree.collapse_all();
        self.rebuild_treemap();
        true
    }

    pub fn expand_all(&mut self) -> bool {
        self.tree.expand_all();
        self.rebuild_treemap();
        true
    }

    /// Manual rescan (`r`). Sizes are otherwise cached and reused across
    /// navigation, so this is the way to pick up on-disk changes. Clearing the
    /// shared cache invalidates it for every screen holding a handle to it, not
    /// just this one.
    pub fn refresh(&mut self) -> bool {
        self.tree.size_cache.clear();
        self.tree = FileTree::with_cache(
            self.root_path.clone(),
            self.tree.sort_mode,
            self.tree.show_hidden,
            self.tree.show_size_bar,
            self.tree.size_cache.clone(),
        );
        self.rebuild_treemap_and_info();
        true
    }

    /// Search through tree lines and move cursor to first match.
    /// Search line names with the regex engine and move the cursor to the first
    /// match. The pattern is treated as a case-insensitive regex; an invalid
    /// pattern is ignored (cursor stays put).
    /// Jump to the first name matching `pattern`.
    ///
    /// Returns an error message to surface, or `None` on success.
    pub fn search(&mut self, pattern: &str) -> Option<String> {
        let pattern = pattern.trim();
        if pattern.is_empty() {
            return None;
        }
        // Case-insensitive by default so search stays forgiving.
        let re = match regex::RegexBuilder::new(pattern)
            .case_insensitive(true)
            .build()
        {
            Ok(re) => re,
            Err(e) => return Some(bad_pattern_message(pattern, &e)),
        };
        // Remember the pattern so `n` / `p` can repeat it.
        self.last_search = Some(re.clone());
        // Jump to the first match at or after the current cursor, wrapping.
        self.jump_to_match(&re, true);
        None
    }

    /// Move the cursor to the next match of the last search (down the tree,
    /// wrapping to the top). No-op if nothing has been searched yet.
    pub fn search_next(&mut self) -> bool {
        if let Some(re) = self.last_search.clone() {
            self.step_to_match(&re, true);
        }
        true
    }

    /// Move the cursor to the previous match of the last search (up the tree,
    /// wrapping to the bottom). No-op if nothing has been searched yet.
    pub fn search_prev(&mut self) -> bool {
        if let Some(re) = self.last_search.clone() {
            self.step_to_match(&re, false);
        }
        true
    }

    /// Jump to the first match at or after (forward) / at or before the cursor,
    /// wrapping around the whole list. Used by the initial search.
    fn jump_to_match(&mut self, re: &regex::Regex, forward: bool) {
        let n = self.tree.lines.len();
        if n == 0 {
            return;
        }
        let start = self.tree.cursor;
        for offset in 0..n {
            let i = if forward {
                (start + offset) % n
            } else {
                (start + n - offset) % n
            };
            if re.is_match(&self.tree.lines[i].name) {
                self.tree.set_cursor(i);
                return;
            }
        }
    }

    /// Step to the next/previous match strictly past the current cursor,
    /// wrapping around. Used by `n` / `p`.
    fn step_to_match(&mut self, re: &regex::Regex, forward: bool) {
        let n = self.tree.lines.len();
        if n == 0 {
            return;
        }
        let start = self.tree.cursor;
        // offsets 1..=n so we skip the current line and can wrap back to it.
        for offset in 1..=n {
            let i = if forward {
                (start + offset) % n
            } else {
                (start + n - offset) % n
            };
            if re.is_match(&self.tree.lines[i].name) {
                self.tree.set_cursor(i);
                return;
            }
        }
    }

    /// The directory a "create here", "filter here", or "copy to here" action
    /// targets: the cursor line itself when it's a directory, otherwise its
    /// parent, falling back to the pane root.
    ///
    /// This is what the user means by "the current directory" — directories
    /// expand in place, so the cursor routinely sits several levels below the
    /// pane root while the root itself is no longer where anything is happening.
    /// Uses `path`, the entry as its own filesystem names it, rather than
    /// `resolved_path`. `resolved_path` is canonicalised *against the local
    /// disk*, which is right for a cache key and wrong for a destination: on
    /// macOS it rewrites `/tmp` to `/private/tmp`, and sending that to a Linux
    /// server asks it to write under a `/private` that does not exist. It also
    /// silently redirects a copy through a symlink's target instead of the
    /// directory the user is actually looking at.
    pub fn target_dir(&self) -> PathBuf {
        match self.tree.selected_line() {
            Some(line) if line.is_dir => line.path.clone(),
            Some(line) => line
                .path
                .parent()
                .map(|p| p.to_path_buf())
                .unwrap_or_else(|| self.root_path.clone()),
            None => self.root_path.clone(),
        }
    }

    /// Create a subdirectory named `name` in the cursor's current directory.
    /// A blank name is a no-op. Only the affected directory level is reloaded
    /// (not the whole tree), so this stays fast even in a large tree.
    /// Returns an error message for the caller to surface, or `None` on success.
    pub fn create_dir(&mut self, name: &str) -> Option<String> {
        let name = name.trim();
        if name.is_empty() {
            return None;
        }
        let parent = self.target_dir();
        let dir = parent.join(name);
        // Goes through the tree's own source, so a remote panel creates the
        // directory on the server rather than on this machine. A remote failure
        // (permissions, a name that already exists) has to reach the user —
        // silently doing nothing would look like the key press was dropped.
        if let Err(e) = self.tree.source.create_dir_all(&dir) {
            return Some(format!("Could not create '{}': {}", name, e));
        }
        // Reload just the parent level so the new directory appears, keeping the
        // size cache and the rest of the tree intact.
        self.tree.reload_dir(&parent);
        self.rebuild_treemap_and_info();
        None
    }

    /// Reload just one directory level (re-listing it, reusing the size cache),
    /// then rebuild the treemap. Used to reflect a completed transfer landing in
    /// this directory without a full rescan. A no-op if the directory isn't in
    /// the tree.
    pub fn reload_dir_public(&mut self, resolved_path: &std::path::Path) {
        self.tree.reload_dir(resolved_path);
        self.rebuild_treemap_and_info();
    }

    /// Toggle the tag on the focused view's selection.
    ///
    /// Works from the treemap too. It used to be tree-only, which made `t`
    /// silently do nothing after `v` — while `c`, `D` and `m` went on using the
    /// treemap's cursor. Tagging four tiles and copying therefore copied one
    /// file, the one under the cursor, with nothing on screen to say the tags
    /// had not registered. Tags live on the tree and are keyed by resolved path,
    /// which is exactly what a treemap cell carries, so the two views share one
    /// tag set rather than each keeping its own.
    pub fn toggle_tag(&mut self) -> bool {
        match self.focus {
            FocusTarget::Tree => self.tree.toggle_tag(),
            FocusTarget::Treemap => {
                if let Some(path) = self.treemap.selected_cell().map(|c| c.resolved_path.clone()) {
                    self.tree.toggle_tag_path(path);
                }
            }
        }
        true
    }

    /// Remove every tag.
    pub fn untag_all(&mut self) -> bool {
        self.tree.untag_all();
        true
    }

    /// Toggle visual (range-tag) mode.
    ///
    /// Genuinely tree-only, unlike `t`: a visual range is a span of consecutive
    /// rows, and the treemap's squarified tiles have no such order — "everything
    /// between these two tiles" names nothing. Returns whether it applied, so
    /// the caller can say so rather than leave `V` looking broken.
    pub fn toggle_visual(&mut self) -> bool {
        if self.focus == FocusTarget::Tree {
            self.tree.toggle_visual();
            return true;
        }
        false
    }

    /// Exit visual mode without clearing tags (called before non-motion actions).
    pub fn exit_visual(&mut self) {
        self.tree.exit_visual();
    }

    /// Whether visual mode is currently active.
    pub fn in_visual_mode(&self) -> bool {
        self.tree.in_visual_mode()
    }

    /// Snapshot of tagged paths for a copy.
    pub fn tagged_paths(&self) -> Vec<PathBuf> {
        self.tree.tagged_paths()
    }

    /// Whether a path currently shown in this tree is a directory, read from the
    /// already-loaded lines so it costs no I/O (a remote stat would be a round
    /// trip). `None` if the path isn't in the visible tree.
    pub fn is_dir_of(&self, resolved_path: &std::path::Path) -> Option<bool> {
        self.tree
            .lines
            .iter()
            .find(|l| l.resolved_path == resolved_path || l.path == resolved_path)
            .map(|l| l.is_dir)
    }

    /// Whether this tree already shows an entry at `path`.
    ///
    /// Read from the loaded lines for the same reason as [`Self::is_dir_of`]: a
    /// remote `stat` is a round trip, and the collision check runs once per file
    /// in a batch from a synchronous key handler. Only what the panel has listed
    /// counts, so a collapsed or not-yet-loaded subdirectory reports nothing —
    /// the destination directory itself is always listed, which is the case that
    /// matters for a copy landing in it.
    pub fn has_entry(&self, path: &std::path::Path) -> bool {
        self.tree
            .lines
            .iter()
            .any(|l| l.resolved_path == path || l.path == path)
    }

    /// Number of tagged files (for the footer indicator).
    pub fn tagged_count(&self) -> usize {
        self.tree.tagged.len()
    }

    /// Filter the cursor's current directory by regex. An empty pattern clears
    /// the filter; an invalid pattern is ignored. The "current directory" is the
    /// cursor line itself if it's a directory, otherwise its parent.
    /// Filter the cursor's directory by `pattern`.
    ///
    /// Returns an error message to surface, or `None` on success. A malformed
    /// pattern used to be discarded silently, which was indistinguishable from a
    /// pattern that simply matched everything — `*p$` is not valid regex (nothing
    /// to repeat) and looked like the filter was broken.
    ///
    /// Case-insensitive, matching `search`; having one of the two ignore case and
    /// the other not was a difference nobody could see.
    pub fn filter(&mut self, pattern: &str) -> Option<String> {
        let pattern = pattern.trim();
        if pattern.is_empty() {
            self.tree.clear_filter();
            return None;
        }
        let re = match regex::RegexBuilder::new(pattern)
            .case_insensitive(true)
            .build()
        {
            Ok(re) => re,
            Err(e) => return Some(bad_pattern_message(pattern, &e)),
        };
        self.tree.set_filter(re);
        None
    }

    /// The treemap's tiles, in layout order.
    pub fn treemap_cells(&self) -> &[crate::widget::treemap::TreemapCell] {
        &self.treemap.cells
    }

    /// In the treemap, whether the cursor can still move left. False on a
    /// left-edge tile — the point at which `h` should step up to the parent
    /// directory instead of sliding the cursor.
    pub fn treemap_can_move_left(&self) -> bool {
        self.treemap.can_move_left()
    }

    /// Move the treemap cursor to a specific tile.
    pub fn set_treemap_cursor(&mut self, index: usize) {
        if index < self.treemap.cells.len() {
            self.treemap.cursor = index;
        }
    }

    /// Get the currently selected path from whichever view is focused.
    pub fn selected_path(&self) -> Option<&PathBuf> {
        match self.focus {
            FocusTarget::Tree => self.tree.selected_line().map(|l| &l.path),
            FocusTarget::Treemap => self.treemap.selected_cell().map(|c| &c.path),
        }
    }

    /// Switch between the tree and the treemap, carrying the selection over.
    ///
    /// Without this the two views keep independent cursors, so toggling landed
    /// on whatever each was last pointing at — you would find a directory in the
    /// treemap, press `v`, and be somewhere unrelated in the tree.
    ///
    /// A path present in one view may be absent from the other (the treemap only
    /// shows the expanded level, the tree may have it collapsed), in which case
    /// the destination keeps its own cursor rather than jumping to an arbitrary
    /// row.
    pub fn toggle_view(&mut self) {
        let selected = self.selected_path().cloned();
        self.focus = match self.focus {
            FocusTarget::Tree => FocusTarget::Treemap,
            FocusTarget::Treemap => FocusTarget::Tree,
        };
        if let Some(path) = selected {
            self.select_path(&path);
        }
        // The info panel describes the focused view's selection.
        self.cached_info_key = None;
    }

    /// Move the focused view's cursor to `path`, if it is showing it.
    fn select_path(&mut self, path: &std::path::Path) {
        match self.focus {
            FocusTarget::Tree => {
                if let Some(i) = self.tree.lines.iter().position(|l| l.path == path) {
                    self.tree.set_cursor(i);
                }
            }
            FocusTarget::Treemap => {
                if let Some(i) = self.treemap.cells.iter().position(|c| c.path == path) {
                    self.treemap.cursor = i;
                }
            }
        }
    }

    /// Whether the focused view's selection is a directory.
    ///
    /// Focus-aware, like [`Self::selected_path`]. Reading `tree.selected_line()`
    /// directly answered for the *tree's* cursor even while the treemap was
    /// focused, so Enter on a treemap tile consulted an unrelated row.
    ///
    /// Uses the entry's own `is_dir` from the listing rather than
    /// `Path::is_dir()`, which is always false for a remote path.
    pub fn selected_is_dir(&self) -> bool {
        match self.focus {
            FocusTarget::Tree => self.tree.selected_line().map(|l| l.is_dir).unwrap_or(false),
            FocusTarget::Treemap => self
                .treemap
                .selected_cell()
                .map(|c| c.is_dir)
                .unwrap_or(false),
        }
    }

    /// Get the currently selected resolved (canonicalized) path.
    pub fn selected_resolved_path(&self) -> Option<&PathBuf> {
        match self.focus {
            FocusTarget::Tree => self.tree.selected_line().map(|l| &l.resolved_path),
            FocusTarget::Treemap => self.treemap.selected_cell().map(|c| &c.resolved_path),
        }
    }

    /// Get the depth of the currently selected line (tree) or cell (treemap).
    pub fn selected_line_depth(&self) -> Option<usize> {
        match self.focus {
            FocusTarget::Tree => self.tree.selected_line().map(|l| l.depth),
            FocusTarget::Treemap => self.treemap.selected_cell().map(|c| c.depth),
        }
    }

    /// Remove a path from the tree in-place (preserves expanded state).
    pub fn remove_path(&mut self, path: &std::path::Path) {
        self.tree.remove_path(path);
        self.rebuild_treemap_and_info();
    }
}

impl ScreenState for MainScreenState {
    fn render(&mut self, frame: &mut Frame, area: Rect) {
        // Split area into content and footer.
        let chunks =
            Layout::vertical([Constraint::Min(1), Constraint::Length(1)]).split(area);
        let content_area = chunks[0];
        // The footer describes the window, not this column, so the app may
        // place it across the whole frame — the transfer sidebar stops above
        // that row to leave it clear. Falls back to the bottom of this panel.
        let footer_area = self.footer_rect.unwrap_or(chunks[1]);

        // One split for both views: the width is a preference now, and two
        // copies of the constraint would let the tree and the treemap drift to
        // different widths for the same panel.
        let (main_area, info_area) = if self.info_panel_hidden {
            (content_area, None)
        } else {
            let pct = self
                .info_panel_pct
                .clamp(crate::prefs::MIN_INFO_PCT, crate::prefs::MAX_INFO_PCT);
            let inner = Layout::horizontal([
                Constraint::Percentage(100 - pct),
                Constraint::Percentage(pct),
            ])
            .split(content_area);
            (inner[0], Some(inner[1]))
        };

        match self.focus {
            FocusTarget::Tree => self.render_tree(frame, main_area),
            FocusTarget::Treemap => self.render_treemap(frame, main_area),
        }
        // Clear the sub-panel's rect whenever the info panel is not drawn.
        //
        // `render_info` is what records it, so hiding the panel leaves the rect
        // from the last frame it *was* visible — and the app draws the preview
        // into that rect afterwards, from outside this screen. The result was a
        // stripe of file content floating over the tree with no panel around
        // it, since the border had gone and the content had not.
        if info_area.is_none() {
            self.info_preview_area = None;
        }
        if let Some(area) = info_area {
            self.render_info(frame, area);
        }

        self.render_footer(frame, footer_area);
    }
}

impl MainScreenState {
    fn render_tree(&mut self, frame: &mut Frame, area: Rect) {
        // The block has borders (top/bottom) and a title bar, reducing usable
        // height. Borders and the title bar consume 3 rows; saturate so a very
        // short terminal underflows to 1 line instead of panicking.
        let visible_lines = area.height.saturating_sub(3).max(1) as usize;

        // Bring the cursor into view. This is the only place the viewport height
        // is known, so it is where the tree's persistent offset gets clamped —
        // the offset itself is never derived from the cursor, or the cursor would
        // be stuck to the window's bottom edge. Done before the text is built,
        // which borrows the tree.
        self.tree.clamp_scroll(visible_lines);
        let scroll = self.tree.scroll;

        let text = if self.pending_ghosts.is_empty() {
            self.tree.render_text()
        } else {
            self.tree.render_text_with_ghosts(&self.pending_ghosts)
        };

        // Build status bar subtitle — uses cached counts, no iteration.
        let total = self.tree.lines.len().saturating_sub(1); // subtract root
        let dirs = self.tree.dir_count();
        let files = self.tree.file_count();
        // While filtering, the counts describe what is *visible*, so say so —
        // otherwise "12 items" over a masked tree reads as the directory having
        // lost files.
        // Shallow browsing leads the title for the same reason FILTERED does: the
        // sizes on screen are dashes rather than measurements, and a title that
        // does not say so leaves the tree looking broken.
        let shallow_mark = if self.tree.is_shallow() {
            " SHALLOW |"
        } else {
            ""
        };
        // A remote pane says so, and says it first. "File Tree (/var/log)" is
        // identical whether that path is on this machine or a server, which on a
        // split with one of each is the difference between deleting your own
        // files and someone else's. Leading like FILTERED does, for the same
        // reason: the tail is what a narrow terminal loses.
        let remote = self.tree.source.is_remote();
        let kind = self.tree.source.display_kind();
        // Inside an archive the path is relative to its root, so `(/docs)` on
        // its own does not say *which* archive is open — and with two panes it
        // could be either of two. Name it.
        let where_ = match self.tree.source.container_name() {
            Some(name) => format!("{} {}", name, self.root_path.display()),
            None => self.root_path.display().to_string(),
        };
        // The filtered badge is drawn as its own span so it can carry a
        // background, while the rest of the title keeps whatever tint the source
        // gives it. Split out rather than formatted in, but the two halves still
        // add up to exactly the old string — `sort_area` below measures the
        // prefix in characters to place the click region, so changing its width
        // would move the "Sort:" hit box off the text.
        let filtered = self.tree.filter_pattern().is_some();
        let (badge, prefix) = if filtered {
            // "FILTERED" leads, because ratatui truncates a title at the right
            // border: on a narrow terminal the tail is the first thing lost, and
            // this is the part that must not be.
            (
                format!("{} FILTERED ", shallow_mark),
                format!(
                    "| {} ({}) | {} shown | {} dirs | {} files | ",
                    kind, where_, total, dirs, files,
                ),
            )
        } else {
            (
                String::new(),
                format!(
                    "{} {} ({}) | {} items | {} dirs | {} files | ",
                    shallow_mark, kind, where_, total, dirs, files,
                ),
            )
        };
        // Captured before the badge is moved into the title's spans below.
        let badge_len = badge.chars().count();
        let sort_text = format!("Sort: {} ▾ ", self.tree.sort_mode.label());
        // Coloured rather than lengthened: the title is the most contested row in
        // the app, and a colour costs no columns. The kind word above carries the
        // same thing for a monochrome terminal or a reader who cannot pick the
        // hue out — neither signal is load-bearing on its own.
        let tint = if self.tree.source.is_read_only() {
            // An archive and a server are both "not this directory tree", but
            // only one of them is read-only, and a split showing one of each
            // has to be tellable apart without reading the words.
            Some(crate::widget::file_tree::ARCHIVE_COLOR)
        } else if remote {
            Some(crate::widget::file_tree::REMOTE_COLOR)
        } else {
            None
        };
        let rest_style = match tint {
            Some(colour) => Style::default().fg(colour).add_modifier(Modifier::BOLD),
            None => Style::default(),
        };
        let title = if filtered {
            // Reversed out of the filter colour, the way the footer badge and
            // the archive marker are: a background is what reads as a label at a
            // glance, where coloured text reads as more title.
            Line::from(vec![
                Span::styled(
                    badge,
                    Style::default()
                        .fg(Color::Black)
                        .bg(crate::widget::file_tree::FILTER_COLOR)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(format!("{}{}", prefix, sort_text), rest_style),
            ])
        } else {
            Line::from(Span::styled(
                format!("{}{}", prefix, sort_text),
                rest_style,
            ))
        };

        // Remember where "Sort: …" landed so a click on it can open the sort
        // menu. The title is drawn inside the top border starting one column in,
        // and ratatui truncates it at the border — a hit region past the edge
        // would match clicks on nothing, so it is clipped to the box.
        self.sort_area = {
            // Both spans precede "Sort: …", so the badge counts toward the
            // offset — measuring the prefix alone put the hit box eleven columns
            // left of the text while a filter was on.
            let start =
                area.x + 1 + (badge_len + prefix.chars().count()) as u16;
            let end = (start + sort_text.chars().count() as u16)
                .min(area.x + area.width.saturating_sub(1));
            (start < end).then(|| Rect::new(start, area.y, end - start, 1))
        };

        // Keep what was drawn so a mouse click can be mapped back to a row. The
        // offset depends on the area's height, so it cannot be recomputed later
        // from state alone — it has to be recorded here, as the treemap already
        // does with its cell rects.
        self.tree_area = Some(area);
        self.tree_scroll = scroll;
        self.tree_viewport = visible_lines;

        let paragraph = Paragraph::new(text)
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_type(BorderType::Plain)
                    .border_style(Style::default().fg(self.border_color()))
                    .title(title),
            )
            .scroll((scroll as u16, 0));

        frame.render_widget(paragraph, area);

        if !self.tree.lines.is_empty() {
            // The thumb tracks the window, not the cursor. Built with
            // `default()` the content length stayed 0, which drew a thumb of no
            // meaningful size or position.
            let mut scrollbar_state = ScrollbarState::new(self.tree.lines.len())
                .viewport_content_length(visible_lines)
                .position(scroll);
            frame.render_stateful_widget(
                Scrollbar::new(ScrollbarOrientation::VerticalRight),
                area,
                &mut scrollbar_state,
            );
        }
    }

    fn render_treemap(&mut self, frame: &mut Frame, area: Rect) {
        // Get the tree's selected path to highlight in the treemap. Cloned so the
        // immutable borrow of `self.tree` ends before `self.treemap` is borrowed mutably.
        let highlighted_path = self.tree.selected_line().map(|l| l.path.clone());
        let cursor = self.treemap.cursor;
        // Cloned for the same borrow reason as `highlighted_path`: the tag set
        // lives on the tree and the treemap is about to be borrowed mutably.
        let tagged = self.tree.tagged.clone();

        // The same bordered box the tree and the info panel draw, in the same
        // focus colour. Without it the treemap was the one pane on screen with
        // no frame — in a split that left no edge between the two panels and no
        // way to tell which of them had the keyboard, since the cyan border is
        // how every other pane says so.
        let block = Block::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Plain)
            .border_style(Style::default().fg(self.border_color()))
            .title(self.treemap_title());
        // Cells are laid out inside the frame, not over it: `compute_layout`
        // fills whatever rect it is handed, so passing the outer area would draw
        // tiles on top of the border it had just been given.
        let inner = block.inner(area);
        frame.render_widget(block, area);

        self.treemap
            .render(frame, inner, cursor, highlighted_path.as_deref(), &tagged);
    }

    /// The treemap box's title: what is being shown, and how to leave it.
    ///
    /// Mirrors the tree's, which names the directory and the sort order — the
    /// treemap has no sort to report, so it names the view instead, which is
    /// also the answer to "why do my usual keys not work here".
    fn treemap_title(&self) -> String {
        let name = self
            .root_path
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| self.root_path.to_string_lossy().to_string());
        format!(" Treemap ({}) — v for the tree ", name)
    }

    fn render_info(&mut self, frame: &mut Frame, area: Rect) {
        // Use whichever view is focused to determine what to show info for.
        // `resolved` identifies the entry; `path` is what gets displayed.
        let (resolved, path) = match self.focus {
            FocusTarget::Tree => self
                .tree
                .selected_line()
                .map(|l| (l.resolved_path.clone(), l.path.clone()))
                .unwrap_or_else(|| (PathBuf::from("."), PathBuf::from("."))),
            FocusTarget::Treemap => self
                .treemap
                .selected_cell()
                .map(|c| (c.resolved_path.clone(), c.path.clone()))
                .unwrap_or_else(|| (PathBuf::from("."), PathBuf::from("."))),
        };

        // Recompute when the focused view changes or points somewhere new, or
        // when the field cursor moves — the cursor is drawn into the cached
        // text, so a stale entry would pin it to whichever row it was on when
        // the entry was built.
        let field = self.info_active.then_some(self.info_field);
        let key = (resolved, self.focus, field);
        if self.cached_info_key.as_ref() != Some(&key) {
            // A remote entry is described from the directory listing the tree
            // already holds. Inspecting it with `std::fs` would read the local
            // machine — showing an unrelated file's metadata whenever the path
            // happens to exist on both.
            let info_started = crate::app::trace_enabled().then(std::time::Instant::now);
            self.cached_info_text = if self.tree.source.is_remote() {
                let line = self.tree.selected_line();
                let size = line
                    .and_then(|l| self.tree.size_cache.get(&l.resolved_path))
                    .unwrap_or(0);
                file_info::render_remote_info_owned(
                    &path,
                    line.map(|l| l.is_dir).unwrap_or(false),
                    line.map(|l| l.is_symlink).unwrap_or(false),
                    size,
                    line.and_then(|l| l.mode),
                    line.and_then(|l| l.mtime),
                    line.and_then(|l| l.atime),
                    field,
                )
            } else {
                file_info::render_info_owned(&path, &self.tree.size_cache, field)
            };
            self.cached_info_key = Some(key);
            if let Some(started) = info_started {
                crate::app::trace_note(format_args!(
                    "  info_panel rebuild={:.1}ms",
                    started.elapsed().as_secs_f64() * 1000.0
                ));
            }
        }

        // The panel says so when it holds the keyboard, the way every other
        // focusable pane does — and names the keys that only work from here,
        // since nothing else on screen advertises them.
        let (border, title) = if self.info_active {
            (
                Color::Cyan,
                " Info — Enter edits, < > + - resize ".to_string(),
            )
        } else {
            (self.border_color(), " Info ".to_string())
        };
        let block = Block::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Plain)
            .border_style(Style::default().fg(border))
            .title(title);
        let inner = block.inner(area);
        frame.render_widget(block, area);

        // The metadata is this panel's job; the preview is a bonus, so it only
        // appears once the metadata has the room it needs. Below either
        // threshold the metadata takes the whole panel, exactly as it did
        // before the sub-panel existed — and `>` is the answer when it does not
        // fit.
        let (meta_area, preview_area) = if inner.width >= MIN_PREVIEW_COLS
            && inner.height >= MIN_META_ROWS + MIN_PREVIEW_ROWS
        {
            // The metadata is a fixed handful of rows — nine for a local file,
            // ten for a directory — so anything past that is space it cannot
            // use. Give it what it needs and hand the rest to the preview,
            // which can always show more.
            //
            // This used to cap the preview at a third of the panel, which meant
            // a tall terminal grew the *metadata*: at sixty rows the fields
            // took forty of them to say the same nine things, and the preview
            // was left with the smaller share of a much larger panel.
            let meta_rows = (META_ROWS as i16 + self.info_meta_bias).clamp(
                MIN_META_ROWS as i16,
                inner.height.saturating_sub(MIN_PREVIEW_ROWS).max(MIN_META_ROWS) as i16,
            ) as u16;
            let rows = Layout::vertical([
                Constraint::Length(meta_rows),
                Constraint::Min(MIN_PREVIEW_ROWS),
            ])
            .split(inner);
            (rows[0], Some(rows[1]))
        } else {
            (inner, None)
        };

        frame.render_widget(Paragraph::new(self.cached_info_text.clone()), meta_area);

        // Recorded, not drawn: the content lives on the app (which owns the one
        // loader and the backend registry), so the app fills this rect after the
        // panels have rendered. Recorded here for the same reason `sort_area`
        // is — it is derived from the area handed in, and cannot be recovered
        // from state alone.
        self.info_preview_area = preview_area;
    }

    /// Border color for this screen's panels — bright cyan when active, dim gray
    /// when it belongs to the inactive panel. In single-panel mode `active` is
    /// always true, so the appearance is unchanged.
    /// The pane's border colour.
    ///
    /// A filtered pane borders in the filter colour, because rows are missing
    /// and nothing else about the frame says so — the count in the title reads
    /// like any other count, and the footer badge is at the far end of the
    /// screen from where the eye is.
    ///
    /// Focus still has to be readable, so the filtered colour is only used on
    /// the active pane; an unfocused one stays dark gray. Two panes both
    /// bordered in green with no cyan would say "filtered" twice and "who has
    /// the keyboard" not at all, and focus is the more urgent of the two.
    fn border_color(&self) -> Color {
        if !self.active {
            return Color::DarkGray;
        }
        if self.tree.filter_pattern().is_some() {
            crate::widget::file_tree::FILTER_COLOR
        } else {
            Color::Cyan
        }
    }

    fn render_footer(&self, frame: &mut Frame, area: Rect) {
        let bg = Color::Rgb(20, 20, 30);

        // The sidebar is not a panel, so it has no footer of its own — this
        // one speaks for it while Tab has moved focus there. Checked before
        // `self.focus`, which tracks the tree/treemap/preview split within the
        // panel and says nothing about whether the panel has the keyboard.
        if self.footer == FooterMode::Hidden {
            // Another panel is speaking for the focused pane; two lines saying
            // it would be one too many.
            return;
        }
        // A pending chord takes the whole line. The keys listed to the right are
        // the ones that are live *now*, and while `g` waits none of them are —
        // leaving them up said `j` would move the cursor when it would in fact
        // ring the bell. So the line becomes the chord and what completes it.
        if let Some(c) = self.pending_chord {
            let (prefix, keys) = chord_footer(c, area.width as usize);
            frame.render_widget(
                Paragraph::new(Line::from(vec![
                    Span::styled(
                        prefix,
                        Style::default()
                            .fg(Color::Black)
                            .bg(Color::Rgb(120, 220, 255))
                            .add_modifier(Modifier::BOLD),
                    ),
                    Span::styled(
                        keys,
                        Style::default()
                            .fg(Color::Yellow)
                            .bg(bg)
                            .add_modifier(Modifier::BOLD),
                    ),
                ])),
                area,
            );
            return;
        }

        if self.footer == FooterMode::Transfers {
            // `Del/⌫` rather than spelling both out: the pair is two glyphs
            // wider than "Del" alone, and this line already runs close to the
            // width of an 80-column terminal.
            const KEYS: &str = " j/k:move  K/Del/⌫:cancel  C:clear done  Esc:back  ?:help  q:quit ";
            let prefix = if (area.width as usize) < 46 {
                " [XFER] "
            } else {
                " [TRANSFERS] "
            };
            frame.render_widget(
                Paragraph::new(Line::from(vec![
                    Span::styled(
                        prefix,
                        Style::default()
                            .fg(Color::Black)
                            .bg(Color::Cyan)
                            .add_modifier(Modifier::BOLD),
                    ),
                    Span::styled(
                        KEYS,
                        Style::default()
                            .fg(Color::Yellow)
                            .bg(bg)
                            .add_modifier(Modifier::BOLD),
                    ),
                ])),
                area,
            );
            return;
        }

        match self.focus {
            // In the treemap the tile colors need explaining, and the legend is
            // more useful there than a second copy of the keybindings.
            FocusTarget::Treemap => {
                // Short enough to survive on a narrow terminal; `?` opens help
                // for the full list. The label abbreviates when space is tight.
                const KEYS: &str = " hjkl:move  v:view  ?:help  q:quit ";
                let prefix = if (area.width as usize) < 46 {
                    " [TM] "
                } else {
                    " [TREEMAP] "
                };

                let mut spans = vec![Span::styled(
                    prefix,
                    Style::default()
                        .fg(Color::Yellow)
                        .bg(bg)
                        .add_modifier(Modifier::BOLD),
                )];

                // Keybindings are load-bearing, the legend is a hint — so the
                // legend gets only the width left over and drops entries that
                // don't fit rather than pushing the keys off the line.
                let mut budget = (area.width as usize)
                    .saturating_sub(prefix.len())
                    .saturating_sub(KEYS.len());

                // When the selected tile is too narrow to show its own name,
                // the footer is the only place the user can read it — so it
                // takes precedence over the legend swatches.
                if let Some(name) = self.treemap.truncated_selected_label() {
                    let text = format!(" {} ", name);
                    if text.len() <= budget {
                        budget -= text.len();
                        spans.push(Span::styled(
                            text,
                            Style::default()
                                .fg(Color::White)
                                .bg(bg)
                                .add_modifier(Modifier::BOLD),
                        ));
                    } else if budget > 4 {
                        // Even the name doesn't fit; show as much as it can.
                        let avail = budget - 4;
                        let clipped: String = name.chars().take(avail).collect();
                        let text = format!(" {}… ", clipped);
                        budget -= text.chars().count();
                        spans.push(Span::styled(
                            text,
                            Style::default()
                                .fg(Color::White)
                                .bg(bg)
                                .add_modifier(Modifier::BOLD),
                        ));
                    }
                }

                let mut used = 0usize;
                for cat in self.treemap.categories_present() {
                    let swatch = format!(" {} ", cat.label());
                    let cost = swatch.len() + 1; // swatch plus its separator
                    if used + cost > budget {
                        break;
                    }
                    used += cost;
                    spans.push(Span::styled(
                        swatch,
                        Style::default().fg(cat.fg_color()).bg(cat.bg_color()),
                    ));
                    spans.push(Span::styled(" ", Style::default().bg(bg)));
                }

                spans.push(Span::styled(
                    KEYS,
                    Style::default().fg(Color::Yellow).bg(bg),
                ));
                frame.render_widget(Paragraph::new(Line::from(spans)), area);
            }
            FocusTarget::Tree => {
                let mut spans = Vec::new();
                // Being somewhere read-only leads, because it is the thing that
                // changes what the keys below do: `D`, `R`, `N` and `m` all
                // refuse here, and finding that out by pressing one is a worse
                // way to learn it. The title's tint says the same thing for
                // anyone reading the top of the pane instead of the bottom.
                if self.tree.source.is_read_only() {
                    let width = area.width as usize;
                    spans.push(Span::styled(
                        if width < 60 {
                            " 📦 RO ".to_string()
                        } else {
                            " 📦 ARCHIVE (read-only) ".to_string()
                        },
                        Style::default()
                            .fg(Color::Black)
                            .bg(crate::widget::file_tree::ARCHIVE_COLOR)
                            .add_modifier(Modifier::BOLD),
                    ));
                }
                // A tagged/visual indicator takes precedence — it's transient
                // state the user needs to see. Prefix it before the keybindings.
                let tagged = self.tree.tagged.len();
                if self.tree.in_visual_mode() {
                    spans.push(Span::styled(
                        " [VISUAL] ",
                        Style::default()
                            .fg(Color::Black)
                            .bg(Color::Magenta)
                            .add_modifier(Modifier::BOLD),
                    ));
                }
                if tagged > 0 {
                    spans.push(Span::styled(
                        format!(" ▶ {} tagged ", tagged),
                        Style::default()
                            .fg(Color::Black)
                            .bg(crate::widget::file_tree::TAG_COLOR)
                            .add_modifier(Modifier::BOLD),
                    ));
                }
                // A filter hides rows with nothing else on screen to say so, which
                // leaves the tree looking wrong rather than filtered. The badge
                // carries the pattern and how to get rid of it — dropping the hint,
                // then truncating the pattern, as the terminal narrows, so the
                // badge never crowds out the keybindings entirely.
                if let Some(pattern) = self.tree.filter_pattern() {
                    let width = area.width as usize;
                    const HINT: &str = "  (f to change, empty to clear)";
                    // Keep at least this much room for the keybindings that follow.
                    let budget = width.saturating_sub(46);
                    let label = if budget >= pattern.chars().count() + HINT.chars().count() + 11 {
                        format!(" ⧉ filter: {}{} ", pattern, HINT)
                    } else if budget >= pattern.chars().count() + 11 {
                        format!(" ⧉ filter: {} ", pattern)
                    } else {
                        // Even the pattern will not fit; say that filtering is on,
                        // which is the part the user cannot infer from the rows.
                        " ⧉ FILTERED ".to_string()
                    };
                    spans.push(Span::styled(
                        label,
                        Style::default()
                            .fg(Color::Black)
                            .bg(crate::widget::file_tree::FILTER_COLOR)
                            .add_modifier(Modifier::BOLD),
                    ));
                }
                // A pending chord is handled by the early return above, which
                // takes the whole line — the keys here are not live while `g`
                // waits, so showing them alongside it was the confusing part.
                let footer = " [TREE]  j/k:move  l/h:expand/collapse  t:tag  V:visual  U:untag  f:filter  c:copy  ?:help  q:quit ";
                spans.push(Span::styled(
                    footer,
                    Style::default()
                        .fg(Color::Yellow)
                        .bg(bg)
                        .add_modifier(Modifier::BOLD),
                ));
                frame.render_widget(Paragraph::new(Line::from(spans)), area);
            }
        }
    }
}

/// The footer for a chord waiting on its second key: the prefix, and what can
/// complete it.
///
/// The pairs must match `KeyBindingHandler::resolve_chord`, which is the only
/// place that decides what a chord does — a hint listing a key that resolves to
/// nothing would send the user to the bell. `chord_hint_matches_bindings` in
/// the keybinding tests holds the two together.
///
/// Abbreviates on a narrow terminal rather than overflowing, the way the other
/// footers do: the labels go first, then the prefix.
fn chord_footer(c: char, width: usize) -> (String, String) {
    let prefix = format!(" {}… ", c);
    if c != 'g' {
        // No other chord prefix exists today; if one is added without a hint
        // here, say that a key is expected rather than claiming to know which.
        return (prefix, " waiting for the next key…  Esc:cancel ".to_string());
    }
    // Widest first: the full labels, then shorter ones, then just the letters —
    // which still says which keys are live, and at least the prefix badge
    // survives to show the app is mid-sequence.
    const FULL: &str =
        " g:top  u:parent  d:go to…  s:sort  r:rename tagged  t:untag all  z:archive  x:cancel transfers  Esc:cancel ";
    const MEDIUM: &str =
        " g:top  u:parent  d:go to…  s:sort  r:rename  t:untag  z:archive  x:cancel  Esc:back ";
    const SHORT: &str = " g:top  u:parent  d:go to  s:sort  r:rename  t:untag  z:archive  x:cancel ";
    const TERSE: &str = " g u d s r t z x ";
    let budget = width.saturating_sub(prefix.chars().count());
    let keys = if budget >= FULL.chars().count() {
        FULL
    } else if budget >= MEDIUM.chars().count() {
        MEDIUM
    } else if budget >= SHORT.chars().count() {
        SHORT
    } else {
        TERSE
    };
    (prefix, keys.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::utils::sizes::{CancelToken, SizeCache};
    use crate::vfs::BackendId;
    use crate::widget::file_tree::FileTree;
    use crate::widget::progress::OpProgress;
    use crate::widget::source::{RemoteSource, Source};
    use std::sync::Arc;

    /// Render a screen with a chord pending, at `width`, and return the footer.
    fn chord_footer_line(width: u16) -> String {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.txt"), b"x").unwrap();
        let tree = FileTree::with_source_cancellable_progress(
            Source::Local,
            dir.path().to_path_buf(),
            SortMode::default(),
            true,
            true,
            SizeCache::new(),
            &CancelToken::new(),
            &OpProgress::new(),
        )
        .expect("tree builds");
        let mut state = MainScreenState::from_tree(dir.path().to_path_buf(), tree);
        state.pending_chord = Some('g');

        let mut term =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(width, 20)).unwrap();
        term.draw(|f| state.render(f, f.area())).unwrap();
        let buf = term.backend().buffer().clone();
        let y = buf.area.height - 1;
        (0..buf.area.width)
            .map(|x| buf[(x, y)].symbol())
            .collect::<String>()
    }

    /// A pending `g` replaces the key list with what completes the chord.
    ///
    /// The footer used to add a `g…` badge and leave the normal bindings beside
    /// it, which read as though `j`, `t` and the rest were still live — they are
    /// not: while `g` waits, anything that is not a chord key rings the bell.
    #[test]
    fn a_pending_chord_lists_what_completes_it() {
        let line = chord_footer_line(120);
        assert!(line.contains("g…"), "the pending chord is not shown: {}", line);

        // Every key that completes the chord is offered.
        for c in crate::keybinding::KeyBindingHandler::G_CHORD_KEYS {
            assert!(
                line.contains(&format!("{}:", c)),
                "g{} is bound but not offered in the footer: {}",
                c,
                line
            );
        }

        // And the keys that are *not* live are gone, so the line cannot be read
        // as "these still work".
        for stale in ["j/k:move", "t:tag", "V:visual", "f:filter", "[TREE]"] {
            assert!(
                !line.contains(stale),
                "{} is still offered while a chord is pending: {}",
                stale,
                line
            );
        }
    }

    /// The chord footer abbreviates rather than overflowing a narrow terminal.
    #[test]
    fn a_pending_chord_footer_fits_the_terminal() {
        for width in [40u16, 60, 80, 120] {
            let line = chord_footer_line(width);
            assert_eq!(
                line.chars().count(),
                width as usize,
                "footer at width {} was not exactly one line",
                width
            );
            // The prefix survives at every width: it is the part that says the
            // app is mid-sequence rather than ignoring keys.
            assert!(
                line.contains("g…"),
                "the chord prefix was dropped at width {}: {}",
                width,
                line
            );
            // Nothing spilled past the edge into a wrapped second line.
            assert!(
                !line.trim_end().is_empty(),
                "footer was blank at width {}",
                width
            );
        }
    }

    /// Render a screen over `source` and return the whole buffer as text.
    fn rendered(source: Source, path: &std::path::Path) -> String {
        let tree = FileTree::with_source_cancellable_progress(
            source,
            path.to_path_buf(),
            SortMode::default(),
            true,
            true,
            SizeCache::new(),
            &CancelToken::new(),
            &OpProgress::new(),
        )
        .expect("tree builds");
        let mut state = MainScreenState::from_tree(path.to_path_buf(), tree);

        let mut term =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(120, 20)).unwrap();
        term.draw(|f| state.render(f, f.area())).unwrap();
        let buf = term.backend().buffer().clone();
        (0..buf.area.height)
            .map(|y| {
                (0..buf.area.width)
                    .map(|x| buf[(x, y)].symbol())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn archive_source() -> Source {
        let bytes = crate::vfs::archive::zip_reader::tests::fixture();
        let fs = crate::vfs::archive::ArchiveFs::open(
            bytes,
            crate::vfs::archive::ArchiveFormat::Zip,
            std::path::PathBuf::from("/tmp/photos.zip"),
        )
        .expect("fixture opens");
        Source::Remote(RemoteSource::new(BackendId(1), Arc::new(fs)).unwrap())
    }

    #[test]
    fn an_archive_pane_says_so_in_the_title_and_the_footer() {
        // Being read-only changes what D/R/N/m do, so it has to be visible
        // before one of them is pressed rather than discovered by pressing one.
        let screen = rendered(archive_source(), std::path::Path::new("/"));
        assert!(screen.contains("ARCHIVE"), "title should say ARCHIVE:\n{screen}");
        assert!(
            screen.contains("photos.zip"),
            "the title must name which archive is open — inside one, the path is \
             just '/' and says nothing:\n{screen}"
        );
        assert!(
            screen.contains("read-only"),
            "the footer should carry the read-only badge:\n{screen}"
        );
    }

    /// Build a filtered screen and return its buffer for inspection.
    fn filtered_buffer(active: bool) -> ratatui::buffer::Buffer {
        let dir = tempfile::tempdir().unwrap();
        for n in ["a.rs", "b.rs", "c.txt"] {
            std::fs::write(dir.path().join(n), b"x").unwrap();
        }
        let tree = FileTree::with_source_cancellable_progress(
            Source::Local,
            dir.path().to_path_buf(),
            SortMode::default(),
            true,
            true,
            SizeCache::new(),
            &CancelToken::new(),
            &OpProgress::new(),
        )
        .expect("tree builds");
        let mut state = MainScreenState::from_tree(dir.path().to_path_buf(), tree);
        state.active = active;
        state.tree.set_filter(regex::Regex::new("rs").unwrap());
        let mut term =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(100, 12)).unwrap();
        term.draw(|f| state.render(f, f.area())).unwrap();
        term.backend().buffer().clone()
    }

    /// A filtered pane is obvious from the frame, not just from the footer.
    ///
    /// Rows are missing and nothing about the box said so: "FILTERED" sat in the
    /// title as plain text that read like the rest of it, the border was the
    /// same cyan as an unfiltered pane, and the only coloured marker was at the
    /// far end of the screen from where the eye is.
    #[test]
    fn a_filtered_pane_colours_its_border_and_badges_its_title() {
        let buf = filtered_buffer(true);

        // The border carries the filter colour.
        assert_eq!(
            buf[(0u16, 0u16)].fg,
            crate::widget::file_tree::FILTER_COLOR,
            "a filtered pane should border in the filter colour"
        );

        // And FILTERED is reversed out of it rather than being plain text.
        let title: String = (0..buf.area.width)
            .map(|x| buf[(x, 0u16)].symbol().to_string())
            .collect();
        let at = title.find("FILTERED").expect("title should say FILTERED");
        assert_eq!(
            buf[(at as u16, 0u16)].bg,
            crate::widget::file_tree::FILTER_COLOR,
            "the FILTERED badge should have a filter-coloured background: {}",
            title
        );
    }

    /// Focus outranks the filter colour on an unfocused pane.
    ///
    /// Two panes both bordered in green would say "filtered" twice and "who has
    /// the keyboard" not at all, and in a split that is the more urgent signal.
    #[test]
    fn an_unfocused_filtered_pane_keeps_the_inactive_border() {
        let buf = filtered_buffer(false);
        assert_eq!(
            buf[(0u16, 0u16)].fg,
            Color::DarkGray,
            "an unfocused pane must stay dark gray even while filtered"
        );
    }

    /// The sort click region still lands on "Sort:" while filtering.
    ///
    /// The badge is a separate span now, and the hit box is placed by counting
    /// characters — measuring only the text after the badge put it eleven
    /// columns left of the words it is meant to cover.
    #[test]
    fn the_sort_hit_box_follows_the_filtered_title() {
        let dir = tempfile::tempdir().unwrap();
        for n in ["a.rs", "b.rs", "c.txt"] {
            std::fs::write(dir.path().join(n), b"x").unwrap();
        }
        let tree = FileTree::with_source_cancellable_progress(
            Source::Local,
            dir.path().to_path_buf(),
            SortMode::default(),
            true,
            true,
            SizeCache::new(),
            &CancelToken::new(),
            &OpProgress::new(),
        )
        .expect("tree builds");
        let mut state = MainScreenState::from_tree(dir.path().to_path_buf(), tree);
        state.active = true;
        state.tree.set_filter(regex::Regex::new("rs").unwrap());
        // Wide enough that the whole title fits, whatever the temporary
        // directory is called. macOS hands out `/private/var/folders/…` where
        // Linux gives `/tmp/…`, and at 100 columns the longer one pushed
        // "Sort: …" past the right border — where the hit box is correctly
        // clipped away to nothing, so the test failed on the platform rather
        // than on the behaviour it is about.
        let mut term =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(240, 12)).unwrap();
        term.draw(|f| state.render(f, f.area())).unwrap();
        let buf = term.backend().buffer().clone();
        let area = state.sort_area.expect("the sort region should be recorded");
        let under: String = (area.x..area.x + area.width)
            .map(|x| buf[(x, 0u16)].symbol().to_string())
            .collect();
        assert!(
            under.starts_with("Sort:"),
            "the sort hit box does not cover the sort text: {:?}",
            under
        );
    }

    #[test]
    fn an_ordinary_pane_carries_neither_marker() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.txt"), "hi").unwrap();
        let screen = rendered(Source::Local, dir.path());
        assert!(screen.contains("File Tree"));
        assert!(!screen.contains("ARCHIVE"));
        assert!(!screen.contains("read-only"));
    }
}
