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
    cached_info_key: Option<(PathBuf, FocusTarget)>,
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
            active: true,
            cached_info_text: Text::default(),
            cached_info_key: None,
            last_search: None,
            pending_ghosts: Vec::new(),
            tree_area: None,
            tree_scroll: 0,
            tree_viewport: 0,
            sort_area: None,
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
            active: true,
            cached_info_text: Text::default(),
            cached_info_key: None,
            last_search: None,
            pending_ghosts: Vec::new(),
            tree_area: None,
            tree_scroll: 0,
            tree_viewport: 0,
            sort_area: None,
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
    /// transpose silently. `sort_mode` is deliberately not applied here: the order
    /// has to be known before the tree is built, so it is threaded through the
    /// loading screen instead of being set after the fact.
    pub fn apply_view_prefs(&mut self, prefs: crate::panel::ViewPrefs) {
        self.info_panel_hidden = prefs.info_panel_hidden;
        self.focus = prefs.focus;
        self.tree.show_perms = prefs.show_perms;
        self.tree.show_times = prefs.show_times;
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

    /// Toggle the tag on the tree cursor's file. Tree-only (no-op in treemap).
    pub fn toggle_tag(&mut self) -> bool {
        if self.focus == FocusTarget::Tree {
            self.tree.toggle_tag();
        }
        true
    }

    /// Remove every tag.
    pub fn untag_all(&mut self) -> bool {
        self.tree.untag_all();
        true
    }

    /// Toggle visual (range-tag) mode. Tree-only.
    pub fn toggle_visual(&mut self) -> bool {
        if self.focus == FocusTarget::Tree {
            self.tree.toggle_visual();
        }
        true
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
        let footer_area = chunks[1];

        match self.focus {
            FocusTarget::Tree => {
                if self.info_panel_hidden {
                    self.render_tree(frame, content_area);
                } else {
                    let inner =
                        Layout::horizontal([Constraint::Percentage(70), Constraint::Percentage(30)])
                            .split(content_area);
                    self.render_tree(frame, inner[0]);
                    self.render_info(frame, inner[1]);
                }
            }
            FocusTarget::Treemap => {
                if self.info_panel_hidden {
                    self.render_treemap(frame, content_area);
                } else {
                    let inner =
                        Layout::horizontal([Constraint::Percentage(70), Constraint::Percentage(30)])
                            .split(content_area);
                    self.render_treemap(frame, inner[0]);
                    self.render_info(frame, inner[1]);
                }
            }
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
        let prefix = format!(
            " File Tree ({}) | {} items | {} dirs | {} files | ",
            self.root_path.display(),
            total,
            dirs,
            files,
        );
        let sort_text = format!("Sort: {} ▾ ", self.tree.sort_mode.label());
        let title = format!("{}{}", prefix, sort_text);

        // Remember where "Sort: …" landed so a click on it can open the sort
        // menu. The title is drawn inside the top border starting one column in,
        // and ratatui truncates it at the border — a hit region past the edge
        // would match clicks on nothing, so it is clipped to the box.
        self.sort_area = {
            let start = area.x + 1 + prefix.chars().count() as u16;
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

        self.treemap
            .render(frame, area, cursor, highlighted_path.as_deref());
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

        // Recompute when the focused view changes or points somewhere new.
        let key = (resolved, self.focus);
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
                    line.and_then(|l| l.mtime),
                    line.and_then(|l| l.atime),
                )
            } else {
                file_info::render_info_owned(&path, &self.tree.size_cache)
            };
            self.cached_info_key = Some(key);
            if let Some(started) = info_started {
                crate::app::trace_note(format_args!(
                    "  info_panel rebuild={:.1}ms",
                    started.elapsed().as_secs_f64() * 1000.0
                ));
            }
        }

        let paragraph = Paragraph::new(self.cached_info_text.clone()).block(
            Block::default()
                .borders(Borders::ALL)
                .border_type(BorderType::Plain)
                .border_style(Style::default().fg(self.border_color()))
                .title(" Info "),
        );
        frame.render_widget(paragraph, area);
    }

    /// Border color for this screen's panels — bright cyan when active, dim gray
    /// when it belongs to the inactive panel. In single-panel mode `active` is
    /// always true, so the appearance is unchanged.
    fn border_color(&self) -> Color {
        if self.active {
            Color::Cyan
        } else {
            Color::DarkGray
        }
    }

    fn render_footer(&self, frame: &mut Frame, area: Rect) {
        let bg = Color::Rgb(20, 20, 30);
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
