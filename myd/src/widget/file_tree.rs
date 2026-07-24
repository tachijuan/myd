use ratatui::{
    style::{Color, Modifier, Style},
    text::{Line, Span, Text},
};
use std::collections::HashSet;
use std::path::{Path, PathBuf};

use crate::screen::SortMode;
use crate::utils::sizes::{self, SizeCache};

/// A single node in the file tree.
#[derive(Debug, Clone)]
pub struct TreeNode {
    pub path: PathBuf,
    /// Resolved (canonicalized) path — computed once at creation, reused everywhere.
    pub resolved_path: PathBuf,
    /// Whether this node is a directory — computed once at creation.
    pub is_dir: bool,
    /// Children, loaded lazily. `None` means not yet loaded.
    pub children: Option<Vec<TreeNode>>,
    /// Whether this node is currently expanded.
    pub is_expanded: bool,
}

impl TreeNode {
    pub fn new(path: PathBuf) -> Self {
        let is_dir = path.is_dir();
        let resolved = path.canonicalize().unwrap_or(path.clone());
        Self {
            path,
            resolved_path: resolved,
            is_dir,
            children: None,
            is_expanded: false,
        }
    }

    /// Check if this node represents a directory (can be expanded).
    #[inline]
    pub fn is_directory(&self) -> bool {
        self.is_dir
    }

    /// Expand this node (loads children if not loaded).
    pub fn expand(&mut self, cache: &SizeCache, sort_mode: SortMode, show_hidden: bool) {
        self.expand_cancellable(cache, sort_mode, show_hidden, None);
    }

    /// As [`expand`], but a supplied cancel token can abort the size walk.
    pub fn expand_cancellable(
        &mut self,
        cache: &SizeCache,
        sort_mode: SortMode,
        show_hidden: bool,
        cancel: Option<&sizes::CancelToken>,
    ) {
        self.expand_cancellable_progress(cache, sort_mode, show_hidden, cancel, None);
    }

    /// As [`expand_cancellable`], but reports each scanned entry to `progress`.
    /// Used by the initial full scan so the loading overlay can show a live
    /// files / dirs / size count.
    pub fn expand_cancellable_progress(
        &mut self,
        cache: &SizeCache,
        sort_mode: SortMode,
        show_hidden: bool,
        cancel: Option<&sizes::CancelToken>,
        progress: Option<&crate::widget::progress::OpProgress>,
    ) {
        if !self.is_directory() {
            return;
        }
        if self.children.is_none() {
            self.children = Some(load_children(
                &self.path, cache, sort_mode, show_hidden, cancel, progress,
            ));
        }
        self.is_expanded = true;
    }

    /// Collapse this node (does NOT unload children).
    pub fn collapse(&mut self) {
        self.is_expanded = false;
    }

    /// Recursively expand all descendants.
    pub fn expand_all(&mut self, cache: &SizeCache, sort_mode: SortMode, show_hidden: bool) {
        self.expand(cache, sort_mode, show_hidden);
        if let Some(ref mut children) = self.children {
            for child in children.iter_mut() {
                child.expand_all(cache, sort_mode, show_hidden);
            }
        }
    }

    /// Recursively collapse all descendants.
    pub fn collapse_all(&mut self) {
        self.is_expanded = false;
        if let Some(ref mut children) = self.children {
            for child in children.iter_mut() {
                child.collapse_all();
            }
        }
    }
}

/// Load (and sort) the direct children of a directory path.
/// Computes recursive sizes for all children and caches them before sorting,
/// so both the sort and subsequent render use accurate total directory sizes.
fn load_children(
    dir: &Path,
    cache: &SizeCache,
    sort_mode: SortMode,
    show_hidden: bool,
    cancel: Option<&sizes::CancelToken>,
    progress: Option<&crate::widget::progress::OpProgress>,
) -> Vec<TreeNode> {
    let mut entries = Vec::new();

    if let Ok(rd) = std::fs::read_dir(dir) {
        for entry in rd.flatten() {
            let path = entry.path();
            if !show_hidden && is_hidden(&path) {
                continue;
            }
            entries.push(TreeNode::new(path));
        }
    }

    // Ensure every entry has a size before sorting. Directories get recursive
    // (du-like) size; files get metadata size. Entries already in the cache are
    // left alone — recomputing them is the expensive part of opening a
    // subdirectory whose parent was already scanned. Refresh clears the cache
    // when the on-disk state actually needs re-reading.
    for entry in &mut entries {
        // Bail out promptly if the user cancelled the scan.
        if cancel.map(|c| c.is_cancelled()).unwrap_or(false) {
            break;
        }
        if cache.get(&entry.resolved_path).is_some() {
            continue;
        }
        let size = if entry.is_dir {
            // Records every descendant's size too, so opening this directory
            // later is a cache hit instead of another walk.
            match cancel {
                Some(c) => sizes::get_dir_size_caching_cancellable_progress(
                    &entry.path, cache, c, progress,
                ),
                None => sizes::get_dir_size_caching(&entry.path, cache),
            }
        } else {
            let size = sizes::get_file_size(&entry.path);
            if let Some(p) = progress {
                p.add_file(size);
            }
            size
        };
        cache.insert(&entry.resolved_path, size);
    }

    // Sort using cached sizes.
    sort_entries(&mut entries, cache, sort_mode);
    entries
}

/// Check if a path is hidden (starts with .).
fn is_hidden(path: &Path) -> bool {
    path.file_name()
        .and_then(|n| n.to_str())
        .map(|n| n.starts_with('.'))
        .unwrap_or(false)
}

/// Sort entries in place according to sort mode.
/// Uses precomputed sizes from the cache (populated by load_children).
fn sort_entries(entries: &mut [TreeNode], cache: &SizeCache, sort_mode: SortMode) {
    entries.sort_by_key(|node| sort_key_fast(node, cache, sort_mode));
}

/// Re-sort children of a node and recursively process expanded children.
/// Reloads children from disk to ensure consistency after cache clear.
fn re_sort_node(node: &mut TreeNode, cache: &SizeCache, sort_mode: SortMode, show_hidden: bool) {
    if !node.is_dir {
        return;
    }
    // Reload children from disk (preserves expanded state).
    // Always use load_children to recompute and cache sizes — the cache may
    // have been cleared by set_sort_mode, and sort_key_fast falls back to
    // shallow metadata().len() for dirs when the cache is empty, which gives
    // wrong results for Largest/Smallest sort modes.
    node.children = Some(load_children(&node.path, cache, sort_mode, show_hidden, None, None));
    // Recurse into expanded children.
    if node.is_expanded {
        if let Some(ref mut children) = node.children {
            for child in children.iter_mut() {
                re_sort_node(child, cache, sort_mode, show_hidden);
            }
        }
    }
}

/// Reload children from disk at every expanded level (preserves expanded state).
fn reload_node(node: &mut TreeNode, cache: &SizeCache, sort_mode: SortMode, show_hidden: bool) {
    if !node.is_dir {
        return;
    }
    node.children = Some(load_children(&node.path, cache, sort_mode, show_hidden, None, None));
    if node.is_expanded {
        if let Some(ref mut children) = node.children {
            for child in children.iter_mut() {
                reload_node(child, cache, sort_mode, show_hidden);
            }
        }
    }
}

/// Generate a sort key for a node — uses cached sizes for directories,
/// falls back to metadata() for files if not yet cached.
fn sort_key_fast(node: &TreeNode, cache: &SizeCache, sort_mode: SortMode) -> (i32, i64, String) {
    let is_dir = if node.is_dir { 0 } else { 1 };
    let name = node.path.file_name().map(|s| s.to_string_lossy().to_lowercase()).unwrap_or_default();
    // Use cached size (recursive for dirs, metadata for files).
    let size = cache
        .get(&node.resolved_path)
        .unwrap_or_else(|| std::fs::metadata(&node.path).map(|m| m.len()).unwrap_or(0)) as i64;

    match sort_mode {
        SortMode::DirsFirst => (is_dir, 0, name),
        SortMode::FilesFirst => (if is_dir == 0 { 1 } else { 0 }, 0, name),
        SortMode::Largest => (0, -size, name),
        SortMode::Smallest => (0, size, name),
    }
}

/// A flattened line from the tree, representing one renderable row.
#[derive(Debug, Clone)]
pub struct TreeLine {
    /// Reference to the node's path.
    pub path: PathBuf,
    /// Resolved (canonicalized) path — computed once during flatten, reused everywhere.
    pub resolved_path: PathBuf,
    /// Is this a directory?
    pub is_dir: bool,
    /// Indentation depth (number of tree levels).
    pub depth: usize,
    /// Is this a hidden file? — computed once during flatten.
    pub hidden: bool,
    /// Display name — computed once during flatten.
    pub name: String,
}

/// The main FileTree widget state.
pub struct FileTree {
    /// Root node.
    pub root: TreeNode,
    /// Current cursor position (flat line index).
    pub cursor: usize,
    /// Sort mode.
    pub sort_mode: SortMode,
    /// Show hidden files.
    pub show_hidden: bool,
    /// Show size bars.
    pub show_size_bar: bool,
    /// Concurrent size cache.
    pub size_cache: SizeCache,
    /// Flattened lines (recomputed when tree structure changes).
    pub lines: Vec<TreeLine>,
    /// Cached sizes per line index — recomputed on reflatten. Eliminates render-time lookups.
    cached_sizes: Vec<u64>,
    /// Cached sibling totals per line index — recomputed on reflatten.
    cached_siblings: Vec<u64>,
    /// Cached expanded status per line index — recomputed on reflatten.
    cached_expanded: Vec<bool>,
    /// Cached dir/file counts (excluding root) — recomputed on reflatten.
    cached_dir_count: usize,
    cached_file_count: usize,
    /// Resolved paths the user has tagged for multi-file operations. Keyed on
    /// the canonical path so tags survive reflatten (sort, filter, expand).
    pub tagged: HashSet<PathBuf>,
    /// Visual-mode anchor: `Some(cursor_index)` while `V` is active. Motion keys
    /// then tag every line between the anchor and the cursor.
    visual_anchor: Option<usize>,
    /// Active regex filter as `(directory, pattern)`. Only entries whose parent
    /// is `directory` and whose name fails to match are hidden; other levels are
    /// untouched. Applied as a retain step in `reflatten`.
    filter: Option<(PathBuf, regex::Regex)>,
}

impl FileTree {
    pub fn new(path: PathBuf, sort_mode: SortMode, show_hidden: bool, show_size_bar: bool) -> Self {
        Self::with_cache(path, sort_mode, show_hidden, show_size_bar, SizeCache::new())
    }

    /// Build a tree that reuses an existing size cache.
    ///
    /// Used when opening a subdirectory of a tree that has already been
    /// scanned: the sizes are still valid, so there is no reason to walk the
    /// disk again. A manual refresh clears the cache to pick up on-disk changes.
    pub fn with_cache(
        path: PathBuf,
        sort_mode: SortMode,
        show_hidden: bool,
        show_size_bar: bool,
        cache: SizeCache,
    ) -> Self {
        // No cancel token: this path always produces a tree.
        Self::build(path, sort_mode, show_hidden, show_size_bar, cache, None, None)
            .expect("uncancelled build always yields a tree")
    }

    /// Build a tree, aborting the initial size scan if `cancel` is tripped.
    ///
    /// Returns `None` if the scan was cancelled before it finished, so the
    /// caller can drop a scan the user has abandoned rather than showing a
    /// half-measured tree.
    pub fn with_cache_cancellable(
        path: PathBuf,
        sort_mode: SortMode,
        show_hidden: bool,
        show_size_bar: bool,
        cache: SizeCache,
        cancel: &sizes::CancelToken,
    ) -> Option<Self> {
        Self::build(path, sort_mode, show_hidden, show_size_bar, cache, Some(cancel), None)
    }

    /// As [`with_cache_cancellable`], but reports scan progress (files / dirs /
    /// bytes) into `progress` so the loading overlay can show a live count.
    pub fn with_cache_cancellable_progress(
        path: PathBuf,
        sort_mode: SortMode,
        show_hidden: bool,
        show_size_bar: bool,
        cache: SizeCache,
        cancel: &sizes::CancelToken,
        progress: &crate::widget::progress::OpProgress,
    ) -> Option<Self> {
        Self::build(
            path,
            sort_mode,
            show_hidden,
            show_size_bar,
            cache,
            Some(cancel),
            Some(progress),
        )
    }

    fn build(
        path: PathBuf,
        sort_mode: SortMode,
        show_hidden: bool,
        show_size_bar: bool,
        cache: SizeCache,
        cancel: Option<&sizes::CancelToken>,
        progress: Option<&crate::widget::progress::OpProgress>,
    ) -> Option<Self> {
        let mut root = TreeNode::new(path.clone());
        root.expand_cancellable_progress(&cache, sort_mode, show_hidden, cancel, progress);

        // The scan was abandoned partway through — discard the partial tree.
        if cancel.map(|c| c.is_cancelled()).unwrap_or(false) {
            return None;
        }

        let mut tree = Self {
            root,
            cursor: 0,
            sort_mode,
            show_hidden,
            show_size_bar,
            size_cache: cache,
            lines: Vec::new(),
            cached_sizes: Vec::new(),
            cached_siblings: Vec::new(),
            cached_expanded: Vec::new(),
            cached_dir_count: 0,
            cached_file_count: 0,
            tagged: HashSet::new(),
            visual_anchor: None,
            filter: None,
        };
        tree.reflatten();
        Some(tree)
    }

    /// Recompute the flat line list and all cached render data.
    pub fn reflatten(&mut self) {
        self.lines.clear();
        flatten_node(&self.root, 0, &mut self.lines, self.show_hidden);
        self.apply_filter();
        if self.cursor >= self.lines.len() {
            self.cursor = self.lines.len().saturating_sub(1);
        }
        self.recompute_cache();
    }

    /// Drop lines masked by the active regex filter: entries whose parent is the
    /// filter directory and whose name doesn't match. Ancestors, the filter
    /// directory itself, and entries in other directories are always kept, so
    /// the mask stays scoped to the one level the user filtered.
    fn apply_filter(&mut self) {
        if let Some((ref dir, ref re)) = self.filter {
            self.lines.retain(|line| {
                if line.resolved_path.parent() == Some(dir.as_path()) {
                    re.is_match(&line.name)
                } else {
                    true
                }
            });
        }
    }

    /// Recompute cached render data (sizes, sibling totals, expanded status).
    fn recompute_cache(&mut self) {
        let n = self.lines.len();

        // Compute sizes — cache in size_cache and local vec.
        self.cached_sizes = self
            .lines
            .iter()
            .map(|l| self.get_or_compute_size(l))
            .collect();

        // Compute parent totals.
        let mut parent_totals: std::collections::HashMap<PathBuf, u64> =
            std::collections::HashMap::new();
        for (i, line) in self.lines.iter().enumerate() {
            if line.depth == 0 {
                continue;
            }
            if let Some(parent) = line.resolved_path.parent() {
                let parent = parent.to_path_buf();
                *parent_totals.entry(parent).or_insert(0) += self.cached_sizes[i];
            }
        }
        self.cached_siblings = self
            .lines
            .iter()
            .enumerate()
            .map(|(i, line)| {
                if line.depth == 0 {
                    self.cached_sizes[i]
                } else {
                    line.resolved_path
                        .parent()
                        .and_then(|p| parent_totals.get(p).copied())
                        .unwrap_or(self.cached_sizes[i])
                }
            })
            .collect();

        // Compute expanded status.
        let expanded_set = collect_expanded(&self.root);
        self.cached_expanded = (0..n)
            .map(|i| expanded_set.contains(&self.lines[i].resolved_path))
            .collect();

        // Compute dir/file counts (excluding root at depth 0).
        let mut dirs = 0usize;
        let mut files = 0usize;
        for line in &self.lines {
            if line.depth == 0 {
                continue;
            }
            if line.is_dir {
                dirs += 1;
            } else {
                files += 1;
            }
        }
        self.cached_dir_count = dirs;
        self.cached_file_count = files;
    }

    /// Get cached dir count (excluding root).
    pub fn dir_count(&self) -> usize {
        self.cached_dir_count
    }

    /// Get cached file count (excluding root).
    pub fn file_count(&self) -> usize {
        self.cached_file_count
    }

    /// Get the currently selected TreeLine.
    pub fn selected_line(&self) -> Option<&TreeLine> {
        self.lines.get(self.cursor)
    }

    /// Check if the node at the cursor is expanded.
    pub fn is_cursor_expanded(&self) -> bool {
        self.cached_expanded.get(self.cursor).copied().unwrap_or(false)
    }

    /// Expand the node at the cursor (loads children if needed).
    pub fn expand_cursor(&mut self) {
        let resolved = match self.selected_line() {
            Some(line) => line.resolved_path.clone(),
            None => return,
        };
        expand_node_by_path(
            &mut self.root,
            &resolved,
            &self.size_cache,
            self.sort_mode,
            self.show_hidden,
        );
        self.reflatten();
    }

    /// Collapse the node at the cursor.
    pub fn collapse_cursor(&mut self) {
        let resolved = match self.selected_line() {
            Some(line) => line.resolved_path.clone(),
            None => return,
        };
        collapse_node_by_path(&mut self.root, &resolved);
        self.reflatten();
    }

    /// Remove a node from the tree by resolved path (in-place, preserves expanded state).
    /// The path must be the canonicalized (resolved) path stored in TreeLine.resolved_path.
    /// No I/O is performed, so this works even after the file has been deleted.
    pub fn remove_path(&mut self, resolved_path: &Path) -> bool {
        let removed = remove_node_by_path(&mut self.root, &Some(resolved_path.to_path_buf()));
        if removed {
            self.reflatten();
        }
        removed
    }

    /// Navigate into the selected directory (in-place expand if already loaded,
    /// or push a new FileTree for the subdirectory — caller handles screen push).
    pub fn navigate(&mut self) {
        let line = match self.selected_line() {
            Some(l) => l.clone(),
            None => return,
        };
        if !line.is_dir {
            return;
        }
        // If already expanded, collapse instead (toggle) — use cached status.
        let is_expanded = self.cached_expanded.get(self.cursor).copied().unwrap_or(false);
        if is_expanded {
            self.collapse_cursor();
            return;
        }
        // Expand in place (loads children if not loaded).
        self.expand_cursor();
    }

    /// Move cursor to parent directory node.
    pub fn go_parent(&mut self) {
        let current_resolved = match self.selected_line() {
            Some(line) => line.resolved_path.clone(),
            None => return,
        };
        if let Some(parent) = current_resolved.parent() {
            // Find the line index for the parent (uses resolved_path — no I/O).
            for (i, line) in self.lines.iter().enumerate() {
                if line.resolved_path == parent {
                    self.cursor = i;
                    return;
                }
            }
        }
    }

    /// Move cursor to top.
    pub fn to_top(&mut self) {
        self.cursor = 0;
    }

    /// Move cursor to bottom.
    pub fn to_bottom(&mut self) {
        self.cursor = self.lines.len().saturating_sub(1);
    }

    /// Move cursor down.
    pub fn cursor_down(&mut self) {
        if self.cursor < self.lines.len().saturating_sub(1) {
            self.cursor += 1;
        }
        self.tag_visual_span();
    }

    /// Move cursor up.
    pub fn cursor_up(&mut self) {
        if self.cursor > 0 {
            self.cursor -= 1;
        }
        self.tag_visual_span();
    }

    /// Toggle the tag on the line under the cursor.
    pub fn toggle_tag(&mut self) {
        if let Some(line) = self.lines.get(self.cursor) {
            let path = line.resolved_path.clone();
            if !self.tagged.remove(&path) {
                self.tagged.insert(path);
            }
        }
    }

    /// Remove every tag.
    pub fn untag_all(&mut self) {
        self.tagged.clear();
    }

    /// Whether visual (range-tag) mode is active.
    pub fn in_visual_mode(&self) -> bool {
        self.visual_anchor.is_some()
    }

    /// Toggle visual mode. Entering anchors at the cursor and tags it; exiting
    /// leaves the accumulated tags in place.
    pub fn toggle_visual(&mut self) {
        if self.visual_anchor.is_some() {
            self.visual_anchor = None;
        } else {
            self.visual_anchor = Some(self.cursor);
            self.tag_visual_span();
        }
    }

    /// Exit visual mode without clearing tags (called when a non-motion action
    /// runs, so `V` then some command doesn't keep extending the range).
    pub fn exit_visual(&mut self) {
        self.visual_anchor = None;
    }

    /// While in visual mode, tag every line between the anchor and the cursor.
    fn tag_visual_span(&mut self) {
        if let Some(anchor) = self.visual_anchor {
            let (lo, hi) = if anchor <= self.cursor {
                (anchor, self.cursor)
            } else {
                (self.cursor, anchor)
            };
            for line in &self.lines[lo..=hi.min(self.lines.len().saturating_sub(1))] {
                self.tagged.insert(line.resolved_path.clone());
            }
        }
    }

    /// Snapshot of tagged paths for a copy operation.
    pub fn tagged_paths(&self) -> Vec<PathBuf> {
        self.tagged.iter().cloned().collect()
    }

    /// Apply a regex filter scoped to `dir`; only entries directly under `dir`
    /// whose names match survive. Replaces any previous filter.
    pub fn set_filter(&mut self, dir: PathBuf, re: regex::Regex) {
        self.filter = Some((dir, re));
        self.reflatten();
    }

    /// Clear the active filter, restoring the full view.
    pub fn clear_filter(&mut self) {
        if self.filter.is_some() {
            self.filter = None;
            self.reflatten();
        }
    }

    /// Expand all nodes.
    pub fn expand_all(&mut self) {
        self.root.expand_all(&self.size_cache, self.sort_mode, self.show_hidden);
        self.reflatten();
    }

    /// Collapse all nodes.
    pub fn collapse_all(&mut self) {
        self.root.collapse_all();
        self.reflatten();
    }

    /// Change sort mode, clear cache, and reload.
    pub fn set_sort_mode(&mut self, mode: SortMode) {
        self.sort_mode = mode;
        self.size_cache.clear();
        // Re-sort children at every level without collapsing.
        re_sort_node(&mut self.root, &self.size_cache, mode, self.show_hidden);
        self.reflatten();
    }

    /// Toggle hidden files visibility.
    pub fn toggle_hidden(&mut self) {
        self.show_hidden = !self.show_hidden;
        self.size_cache.clear();
        // Reload children at every expanded level (preserves expanded state).
        reload_node(&mut self.root, &self.size_cache, self.sort_mode, self.show_hidden);
        self.reflatten();
    }

    /// Render a single TreeLine as a ratatui Line.
    /// All heavy computation is precomputed — this function only does string formatting.
    fn render_line<'a>(
        &'a self,
        line: &'a TreeLine,
        is_selected: bool,
        is_expanded: bool,
        is_tagged: bool,
        my_size: u64,
        sibling_total: u64,
    ) -> Line<'a> {
        let mut spans = Vec::new();

        // Size column: first, right-justified in a fixed-width field.
        if self.show_size_bar {
            let (size_text, bar) = make_bar_spans(my_size, sibling_total);
            spans.push(Span::styled(size_text, Style::default().fg(Color::Gray)));
            spans.extend(bar);
        }

        // Indentation: two spaces per level.
        for _ in 0..line.depth {
            spans.push(Span::raw("  "));
        }

        // Icon.
        let icon = if line.is_dir {
            if is_expanded {
                "📂"
            } else {
                "📁"
            }
        } else {
            "📄"
        };
        spans.push(Span::raw(icon));
        spans.push(Span::raw(" "));

        let (color, modifier) = if is_selected {
            (None, Modifier::REVERSED)
        } else if line.is_dir {
            (Some(Color::Blue), Modifier::BOLD)
        } else if line.hidden {
            (None, Modifier::DIM)
        } else {
            (None, Modifier::empty())
        };

        let mut style = Style::default();
        if let Some(c) = color {
            style = style.fg(c);
        }
        style = style.add_modifier(modifier);
        // Tagged rows carry a distinct background so their staged state is
        // visible. The cursor's REVERSED highlight already stands out, so the
        // tag background is only applied when the row isn't the cursor.
        if is_tagged && !is_selected {
            style = style.bg(Color::Rgb(60, 60, 90));
        }
        spans.push(Span::styled(&line.name, style));

        Line::from(spans)
    }

    /// Get size from cache, or compute and cache it. Uses recursive size for dirs.
    fn get_or_compute_size(&self, line: &TreeLine) -> u64 {
        if let Some(size) = self.size_cache.get(&line.resolved_path) {
            return size;
        }
        let size = if line.is_dir {
            sizes::get_dir_size(&line.path)
        } else {
            sizes::get_file_size(&line.path)
        };
        self.size_cache.insert(&line.resolved_path, size);
        size
    }

    /// Render the full tree as ratatui Text.
    /// Uses precomputed cache — zero lookups, zero allocations during render.
    pub fn render_text(&self) -> Text<'_> {
        let lines: Vec<Line> = self
            .lines
            .iter()
            .enumerate()
            .map(|(i, line)| {
                self.render_line(
                    line,
                    i == self.cursor,
                    self.cached_expanded.get(i).copied().unwrap_or(false),
                    self.tagged.contains(&line.resolved_path),
                    if self.show_size_bar { self.cached_sizes.get(i).copied().unwrap_or(0) } else { 0 },
                    if self.show_size_bar { self.cached_siblings.get(i).copied().unwrap_or(0) } else { 0 },
                )
            })
            .collect();
        Text::from(lines)
    }
}

/// Collect all expanded node resolved paths for O(1) lookup during render.
fn collect_expanded(node: &TreeNode) -> HashSet<PathBuf> {
    let mut result = HashSet::new();
    if node.is_expanded {
        result.insert(node.resolved_path.clone());
    }
    if let Some(ref children) = node.children {
        for child in children {
            result.extend(collect_expanded(child));
        }
    }
    result
}

/// Generate size bar spans from precomputed values.
/// Returns: (right-justified size text, bar spans).
/// The size text column has fixed width for clean alignment.
fn make_bar_spans(my_size: u64, sibling_total: u64) -> (String, Vec<Span<'static>>) {
    let bar_width: usize = 10;
    let size_col_width: usize = 10;

    let total = if sibling_total > 0 { sibling_total } else { my_size };
    let ratio = if total > 0 { my_size as f64 / total as f64 } else { 0.0 };
    let ratio = ratio.min(1.0).max(0.0);

    let filled = (ratio * bar_width as f64) as usize;
    let empty = bar_width.saturating_sub(filled);

    let color = if ratio < 0.3 {
        Color::Green
    } else if ratio < 0.7 {
        Color::Yellow
    } else {
        Color::Red
    };

    let size_str = sizes::format_size(my_size);
    // Right-justify in fixed-width column.
    let padded = format!("{:>width$}", size_str, width = size_col_width);

    let bar = vec![
        Span::styled("[", Style::default().fg(Color::DarkGray)),
        Span::styled("█".repeat(filled), Style::default().fg(color)),
        Span::styled("░".repeat(empty), Style::default().fg(Color::DarkGray)),
        Span::styled("]", Style::default().fg(Color::DarkGray)),
        Span::raw(" "),
    ];

    (padded, bar)
}

/// Recursively flatten a node into lines.
fn flatten_node(node: &TreeNode, depth: usize, lines: &mut Vec<TreeLine>, show_hidden: bool) {
    // Never filter the root node (depth 0).
    if depth > 0 && !show_hidden && is_hidden(&node.path) {
        return;
    }

    let name = node
        .path
        .file_name()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| node.path.to_string_lossy().to_string());
    let hidden = depth > 0 && is_hidden(&node.path);

    lines.push(TreeLine {
        path: node.path.clone(),
        resolved_path: node.resolved_path.clone(),
        is_dir: node.is_dir,
        depth,
        hidden,
        name,
    });

    if node.is_expanded {
        if let Some(ref children) = node.children {
            for child in children {
                flatten_node(child, depth + 1, lines, show_hidden);
            }
        }
    }
}

/// Expand a node by its resolved path (mutating the tree), loading children if needed.
fn expand_node_by_path(
    node: &mut TreeNode,
    target: &Path,
    cache: &SizeCache,
    sort_mode: SortMode,
    show_hidden: bool,
) {
    if node.resolved_path == target {
        // Load children and mark expanded.
        node.expand(cache, sort_mode, show_hidden);
        return;
    }
    // Recurse into already-loaded children.
    if let Some(ref mut children) = node.children {
        for child in children {
            expand_node_by_path(child, target, cache, sort_mode, show_hidden);
        }
    }
}

/// Collapse a node by its resolved path.
fn collapse_node_by_path(node: &mut TreeNode, target: &Path) {
    if node.resolved_path == target {
        node.is_expanded = false;
        return;
    }
    if let Some(ref mut children) = node.children {
        for child in children {
            collapse_node_by_path(child, target);
        }
    }
}

/// Remove a node from the tree by path. Returns true if found and removed.
/// Removes from the parent's children list.
fn remove_node_by_path(node: &mut TreeNode, target: &Option<PathBuf>) -> bool {
    if let Some(ref mut children) = node.children {
        let target_ref = target.as_ref();
        // Find and remove the matching child (uses resolved_path — no I/O).
        let initial_len = children.len();
        children.retain(|child| {
            Some(&child.resolved_path) != target_ref
        });
        if children.len() < initial_len {
            return true;
        }
        // Recurse into children.
        for child in children.iter_mut() {
            if remove_node_by_path(child, target) {
                return true;
            }
        }
    }
    false
}


#[cfg(test)]
mod tests {
    use super::*;


    #[test]
    fn test_flatten_empty_dir() {
        let dir = tempfile::tempdir().unwrap();
        let tree = FileTree::new(dir.path().to_path_buf(), SortMode::DirsFirst, true, true);
        assert_eq!(tree.lines.len(), 1); // root only
        assert!(tree.lines[0].is_dir);
    }

    #[test]
    fn test_flatten_with_files() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.txt"), "hello").unwrap();
        std::fs::write(dir.path().join("b.txt"), "world").unwrap();
        std::fs::create_dir_all(dir.path().join("subdir")).unwrap();

        let tree = FileTree::new(dir.path().to_path_buf(), SortMode::DirsFirst, true, false);
        // root + 3 children = 4 lines
        assert_eq!(tree.lines.len(), 4);
        // dirs-first: subdir should come before files.
        assert!(tree.lines[1].is_dir);
    }

    #[test]
    fn test_flatten_files_first() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.txt"), "hello").unwrap();
        std::fs::create_dir_all(dir.path().join("subdir")).unwrap();

        let tree =
            FileTree::new(dir.path().to_path_buf(), SortMode::FilesFirst, true, false);
        // root + file + dir = 3
        assert_eq!(tree.lines.len(), 3);
        assert!(!tree.lines[1].is_dir); // file first
        assert!(tree.lines[2].is_dir);
    }

    #[test]
    fn test_filter_hidden() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("visible.txt"), "x").unwrap();
        std::fs::write(dir.path().join(".hidden"), "y").unwrap();

        let tree = FileTree::new(dir.path().to_path_buf(), SortMode::DirsFirst, false, false);
        // root + visible = 2 (hidden excluded)
        assert_eq!(tree.lines.len(), 2);
    }

    #[test]
    fn test_cursor_navigation() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.txt"), "x").unwrap();
        std::fs::write(dir.path().join("b.txt"), "y").unwrap();

        let mut tree =
            FileTree::new(dir.path().to_path_buf(), SortMode::DirsFirst, true, false);
        assert_eq!(tree.cursor, 0);
        tree.cursor_down();
        assert_eq!(tree.cursor, 1);
        tree.cursor_down();
        assert_eq!(tree.cursor, 2);
        tree.cursor_down(); // should not go past end
        assert_eq!(tree.cursor, 2);
        tree.to_top();
        assert_eq!(tree.cursor, 0);
        tree.to_bottom();
        assert_eq!(tree.cursor, 2);
    }

    #[test]
    fn test_sizes_computed() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.txt"), "hello").unwrap(); // 5 bytes
        std::fs::write(dir.path().join("b.txt"), "world!").unwrap(); // 6 bytes
        std::fs::create_dir_all(dir.path().join("subdir")).unwrap();
        std::fs::write(dir.path().join("subdir/inner.txt"), "xyz").unwrap();

        let tree = FileTree::new(dir.path().to_path_buf(), SortMode::DirsFirst, true, true);

        // Sizes are computed during reflatten (not lazily during render).
        // Verify cache is populated after construction.
        assert!(tree.size_cache.len() >= 3, "Cache should have sizes after construction");

        // Verify a.txt has correct size
        let line_a = tree.lines.iter().find(|l| l.path.file_name().map(|n| n == "a.txt").unwrap_or(false)).unwrap();
        assert_eq!(tree.size_cache.get(&line_a.resolved_path), Some(5));

        // Verify b.txt has correct size
        let line_b = tree.lines.iter().find(|l| l.path.file_name().map(|n| n == "b.txt").unwrap_or(false)).unwrap();
        assert_eq!(tree.size_cache.get(&line_b.resolved_path), Some(6));

        // Verify subdir has shallow size (3 bytes from inner.txt)
        let line_sub = tree.lines.iter().find(|l| l.path.file_name().map(|n| n == "subdir").unwrap_or(false)).unwrap();
        let sub_size = tree.size_cache.get(&line_sub.resolved_path).unwrap();
        assert!(sub_size >= 3, "subdir should have shallow size >= 3, got {}", sub_size);
    }

    #[test]
    fn test_toggle_tag_and_untag_all() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.txt"), "x").unwrap();
        std::fs::write(dir.path().join("b.txt"), "y").unwrap();
        let mut tree = FileTree::new(dir.path().to_path_buf(), SortMode::DirsFirst, true, false);

        tree.cursor_down(); // onto first child
        tree.toggle_tag();
        assert_eq!(tree.tagged.len(), 1);
        let tagged_path = tree.lines[tree.cursor].resolved_path.clone();
        assert!(tree.tagged.contains(&tagged_path));

        // Toggling again untags it.
        tree.toggle_tag();
        assert!(tree.tagged.is_empty());

        // Tag both, then untag_all clears everything.
        tree.cursor = 1;
        tree.toggle_tag();
        tree.cursor = 2;
        tree.toggle_tag();
        assert_eq!(tree.tagged.len(), 2);
        tree.untag_all();
        assert!(tree.tagged.is_empty());
    }

    #[test]
    fn test_visual_mode_tags_span() {
        let dir = tempfile::tempdir().unwrap();
        for n in ["a.txt", "b.txt", "c.txt"] {
            std::fs::write(dir.path().join(n), "x").unwrap();
        }
        let mut tree = FileTree::new(dir.path().to_path_buf(), SortMode::DirsFirst, true, false);

        // Anchor at line 1, sweep down to line 3 → all three tagged.
        tree.cursor = 1;
        tree.toggle_visual();
        assert!(tree.in_visual_mode());
        tree.cursor_down();
        tree.cursor_down();
        assert_eq!(tree.tagged.len(), 3);

        // Exiting visual keeps tags; a second sweep elsewhere would add more.
        tree.toggle_visual();
        assert!(!tree.in_visual_mode());
        assert_eq!(tree.tagged.len(), 3);
    }

    #[test]
    fn test_tags_survive_reflatten() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.txt"), "hello").unwrap();
        std::fs::write(dir.path().join("big.bin"), vec![0u8; 5000]).unwrap();
        let mut tree = FileTree::new(dir.path().to_path_buf(), SortMode::DirsFirst, true, true);

        tree.cursor = 1;
        tree.toggle_tag();
        let tagged = tree.tagged.iter().next().cloned().unwrap();

        // Changing sort mode reflattens; the tag (keyed on resolved_path) stays.
        tree.set_sort_mode(SortMode::Largest);
        assert!(tree.tagged.contains(&tagged));
    }

    #[test]
    fn test_filter_hides_nonmatching_siblings() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("keep.log"), "x").unwrap();
        std::fs::write(dir.path().join("keep2.log"), "x").unwrap();
        std::fs::write(dir.path().join("skip.txt"), "x").unwrap();
        let mut tree = FileTree::new(dir.path().to_path_buf(), SortMode::DirsFirst, true, false);
        let before = tree.lines.len();
        assert_eq!(before, 4); // root + 3

        let re = regex::Regex::new(r"\.log$").unwrap();
        tree.set_filter(tree.root.resolved_path.clone(), re);
        // root + 2 matching = 3
        assert_eq!(tree.lines.len(), 3);
        assert!(tree.lines.iter().all(|l| l.depth == 0 || l.name.ends_with(".log")));

        tree.clear_filter();
        assert_eq!(tree.lines.len(), before);
    }

    #[test]
    fn test_resolved_path_set() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("file.txt"), "x").unwrap();

        let tree = FileTree::new(dir.path().to_path_buf(), SortMode::DirsFirst, true, false);

        // All lines should have resolved_path set
        for line in &tree.lines {
            assert!(line.resolved_path.is_absolute(), "resolved_path should be absolute: {:?}", line.resolved_path);
        }
    }
}
