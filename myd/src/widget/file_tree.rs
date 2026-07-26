use ratatui::{
    style::{Color, Modifier, Style},
    text::{Line, Span, Text},
};
use std::collections::HashSet;
use std::path::{Path, PathBuf};

use crate::screen::SortMode;
use crate::utils::sizes::{self, SizeCache};
use crate::widget::source::Source;

/// Highlight color for tagged rows — a vivid amber that reads clearly against
/// the default background and stands apart from the blue directories and the
/// reversed cursor. Shared with the footer's "N tagged" badge.
pub const TAG_COLOR: Color = Color::Rgb(255, 170, 40);

/// Color for "ghost" rows — a transfer in progress toward that location. A muted
/// teal, distinct from the amber tags and blue directories, so an in-flight copy
/// reads as provisional rather than real.
pub const GHOST_COLOR: Color = Color::Rgb(120, 180, 190);

/// Color for symlinks — a bright cyan, the long-standing convention for links in
/// `ls` and most file managers. Distinct from the blue used for real
/// directories, since a link may point at either a file or a directory.
pub const SYMLINK_COLOR: Color = Color::Rgb(80, 220, 220);

/// A single node in the file tree.
#[derive(Debug, Clone)]
pub struct TreeNode {
    pub path: PathBuf,
    /// Resolved (canonicalized) path — computed once at creation, reused everywhere.
    pub resolved_path: PathBuf,
    /// Whether this node is a directory — computed once at creation.
    pub is_dir: bool,
    /// Modification / access times from the directory listing, kept so the
    /// time-based sort orders are pure in-memory comparisons (no per-node stat,
    /// which would be a network round trip on a remote tree). `None` when the
    /// listing didn't report them.
    pub mtime: Option<std::time::SystemTime>,
    pub atime: Option<std::time::SystemTime>,
    /// Whether this entry is a symlink. `is_dir` already reflects the *target*
    /// (so symlinked directories expand), and this flag is what distinguishes
    /// the link itself for display.
    pub is_symlink: bool,
    /// Children, loaded lazily. `None` means not yet loaded.
    pub children: Option<Vec<TreeNode>>,
    /// Whether this node is currently expanded.
    pub is_expanded: bool,
}

impl TreeNode {
    /// Build a node for a local path, determining directory-ness via `std::fs`.
    /// Kept for the many local call sites (and tests) that predate remote
    /// support; remote trees use [`TreeNode::with_kind`].
    pub fn new(path: PathBuf) -> Self {
        let is_dir = path.is_dir();
        Self::with_kind(path, is_dir)
    }

    /// Build a node with directory-ness already known — from a `readdir` result
    /// or a [`Source`], avoiding a per-node stat (unacceptable over SFTP).
    ///
    /// Remote paths are not canonicalized: resolving a symlink would cost a
    /// round trip, and the remote server already hands back absolute paths.
    pub fn with_kind(path: PathBuf, is_dir: bool) -> Self {
        Self::with_meta(path, is_dir, None, None)
    }

    /// Build a node with its listing timestamps, so the tree can sort by time
    /// without any further I/O.
    pub fn with_meta(
        path: PathBuf,
        is_dir: bool,
        mtime: Option<std::time::SystemTime>,
        atime: Option<std::time::SystemTime>,
    ) -> Self {
        Self::with_meta_link(path, is_dir, mtime, atime, false)
    }

    /// As [`TreeNode::with_meta`], plus whether the entry is a symlink.
    pub fn with_meta_link(
        path: PathBuf,
        is_dir: bool,
        mtime: Option<std::time::SystemTime>,
        atime: Option<std::time::SystemTime>,
        is_symlink: bool,
    ) -> Self {
        Self::with_meta_link_resolved(path, is_dir, mtime, atime, is_symlink, true)
    }

    /// As [`TreeNode::with_meta_link`], with control over canonicalization.
    ///
    /// `canonicalize` must be false for remote entries: the path lives on the
    /// server, so resolving it against the *local* filesystem is a syscall per
    /// entry that can only fail — pure overhead on a large remote listing.
    pub fn with_meta_link_resolved(
        path: PathBuf,
        is_dir: bool,
        mtime: Option<std::time::SystemTime>,
        atime: Option<std::time::SystemTime>,
        is_symlink: bool,
        canonicalize: bool,
    ) -> Self {
        let resolved = if canonicalize {
            path.canonicalize().unwrap_or_else(|_| path.clone())
        } else {
            path.clone()
        };
        Self {
            path,
            resolved_path: resolved,
            is_dir,
            mtime,
            atime,
            is_symlink,
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
    pub fn expand(
        &mut self,
        source: &Source,
        cache: &SizeCache,
        sort_mode: SortMode,
        show_hidden: bool,
    ) {
        self.expand_cancellable(source, cache, sort_mode, show_hidden, None);
    }

    /// As [`expand`], but a supplied cancel token can abort the size walk.
    pub fn expand_cancellable(
        &mut self,
        source: &Source,
        cache: &SizeCache,
        sort_mode: SortMode,
        show_hidden: bool,
        cancel: Option<&sizes::CancelToken>,
    ) {
        self.expand_cancellable_progress(source, cache, sort_mode, show_hidden, cancel, None);
    }

    /// As [`expand_cancellable`], but reports each scanned entry to `progress`.
    /// Used by the initial full scan so the loading overlay can show a live
    /// files / dirs / size count.
    pub fn expand_cancellable_progress(
        &mut self,
        source: &Source,
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
                source, &self.path, cache, sort_mode, show_hidden, cancel, progress,
            ));
        }
        self.is_expanded = true;
    }

    /// Collapse this node (does NOT unload children).
    pub fn collapse(&mut self) {
        self.is_expanded = false;
    }

    /// Recursively expand all descendants.
    pub fn expand_all(
        &mut self,
        source: &Source,
        cache: &SizeCache,
        sort_mode: SortMode,
        show_hidden: bool,
    ) {
        self.expand(source, cache, sort_mode, show_hidden);
        if let Some(ref mut children) = self.children {
            for child in children.iter_mut() {
                child.expand_all(source, cache, sort_mode, show_hidden);
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
    source: &Source,
    dir: &Path,
    cache: &SizeCache,
    sort_mode: SortMode,
    show_hidden: bool,
    cancel: Option<&sizes::CancelToken>,
    progress: Option<&crate::widget::progress::OpProgress>,
) -> Vec<TreeNode> {
    let mut entries = Vec::new();
    // Keep each entry's listing-supplied size (if any) alongside its node, so the
    // size pass below can use it instead of issuing a fresh stat.
    let mut listing_sizes: Vec<Option<u64>> = Vec::new();

    for entry in source.read_dir(dir) {
        if !show_hidden && is_hidden(&entry.path) {
            continue;
        }
        listing_sizes.push(entry.len);
        entries.push(TreeNode::with_meta_link_resolved(
            entry.path,
            entry.is_dir,
            entry.mtime,
            entry.atime,
            entry.is_symlink,
            // Remote paths are server-side; canonicalizing them locally is a
            // failed stat per entry.
            !source.is_remote(),
        ));
    }

    // Ensure every entry has a size before sorting. When the listing already
    // gave a size (files everywhere; every entry on a remote backend), use it —
    // a per-entry stat over SFTP is one network round trip each, which is what
    // locked the UI up when digging into remote trees. Only a local directory,
    // whose real size needs a recursive walk, falls through to `dir_size`.
    //
    // For a remote source, `dir_size` returns the shallow size anyway (a `du`
    // over SFTP would be thousands of round trips), so remote directories show
    // their listing size and fill in as they are expanded.
    for (entry, listing_len) in entries.iter_mut().zip(listing_sizes) {
        // Bail out promptly if the user cancelled the scan.
        if cancel.map(|c| c.is_cancelled()).unwrap_or(false) {
            break;
        }
        if cache.get(&entry.resolved_path).is_some() {
            continue;
        }
        let size = match listing_len {
            // Size already known from the listing — no extra round trip.
            Some(len) => {
                if !entry.is_dir {
                    if let Some(p) = progress {
                        p.add_file(len);
                    }
                }
                len
            }
            // No listing size (a local directory): compute it.
            None if entry.is_dir => source.dir_size(&entry.path, cache, cancel, progress),
            None => {
                let size = source.file_size(&entry.path);
                if let Some(p) = progress {
                    p.add_file(size);
                }
                size
            }
        };
        cache.insert(&entry.resolved_path, size);
    }

    // Sort using cached sizes.
    sort_entries(&mut entries, cache, source, sort_mode);
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
fn sort_entries(entries: &mut [TreeNode], cache: &SizeCache, source: &Source, sort_mode: SortMode) {
    // `sort_by_key` recomputes the key on *every comparison* — and each key
    // allocates a lowercased String and hits the size cache, so an n-entry
    // directory paid O(n log n) allocations and map lookups. `sort_by_cached_key`
    // builds each key once.
    let _ = source;
    entries.sort_by_cached_key(|node| sort_key_fast(node, cache, sort_mode));
}

/// Re-order a node's already-loaded children in place, recursing into every
/// loaded subtree. Does no I/O: it reuses the sizes already in the cache, so it
/// is safe to run synchronously even on a remote tree.
fn resort_node(node: &mut TreeNode, source: &Source, cache: &SizeCache, sort_mode: SortMode) {
    let Some(children) = node.children.as_mut() else {
        return;
    };
    sort_entries(children, cache, source, sort_mode);
    // Re-order every loaded subtree, whether or not it's currently expanded, so
    // the order is already correct if the user expands it later.
    for child in children.iter_mut() {
        resort_node(child, source, cache, sort_mode);
    }
}

/// Reload children from disk at every expanded level (preserves expanded state).
fn reload_node(
    node: &mut TreeNode,
    source: &Source,
    cache: &SizeCache,
    sort_mode: SortMode,
    show_hidden: bool,
) {
    if !node.is_dir {
        return;
    }
    node.children = Some(load_children(
        source, &node.path, cache, sort_mode, show_hidden, None, None,
    ));
    if node.is_expanded {
        if let Some(ref mut children) = node.children {
            for child in children.iter_mut() {
                reload_node(child, source, cache, sort_mode, show_hidden);
            }
        }
    }
}

/// Generate a sort key for a node — uses cached sizes for directories,
/// falls back to a shallow size lookup for files if not yet cached.
fn sort_key_fast(node: &TreeNode, cache: &SizeCache, sort_mode: SortMode) -> (i32, i64, String) {
    let is_dir = if node.is_dir { 0 } else { 1 };
    let name = node.path.file_name().map(|s| s.to_string_lossy().to_lowercase()).unwrap_or_default();
    // Sorting is pure reordering: it reads the sizes gathered when the tree was
    // built and never touches the filesystem.
    //
    // This runs on the event-loop thread, so any I/O here stalls the whole UI.
    // "It's only a local stat" is not a safe assumption — a CIFS or NFS mount is
    // an ordinary local path as far as this code can see, and a stat on one is a
    // network round trip. A large directory on such a mount made changing the
    // sort order unresponsive. A cache miss therefore sorts as unknown rather
    // than going to find out.
    let size = cache.get(&node.resolved_path).unwrap_or(0) as i64;

    match sort_mode {
        SortMode::DirsFirst => (is_dir, 0, name),
        SortMode::FilesFirst => (if is_dir == 0 { 1 } else { 0 }, 0, name),
        SortMode::Largest => (0, -size, name),
        SortMode::Smallest => (0, size, name),
        // Time sorts use the listing timestamps already on the node — no I/O.
        // Newest / most-recently-accessed negate so the largest time sorts
        // first; an entry with no timestamp sorts last (treated as the oldest).
        SortMode::Newest => (0, -epoch_secs(node.mtime), name),
        SortMode::Oldest => (0, epoch_secs_oldest_last(node.mtime), name),
        SortMode::RecentlyAccessed => (0, -epoch_secs(node.atime), name),
    }
}

/// Seconds since the Unix epoch for a timestamp, or 0 when unknown. Used for the
/// "newest / recently accessed" sorts, where a missing time sorts as the oldest.
fn epoch_secs(t: Option<std::time::SystemTime>) -> i64 {
    t.and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// As [`epoch_secs`], but a missing timestamp sorts *last* in ascending (oldest
/// first) order — so unknown times don't masquerade as the oldest entry.
fn epoch_secs_oldest_last(t: Option<std::time::SystemTime>) -> i64 {
    t.and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs() as i64)
        .unwrap_or(i64::MAX)
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
    /// Is this entry a symlink? Rendered distinctly (arrow marker + colour).
    pub is_symlink: bool,
    /// Listing timestamps, carried so the info panel can show them for a remote
    /// entry without a per-selection stat (which would be a network round trip).
    pub mtime: Option<std::time::SystemTime>,
    pub atime: Option<std::time::SystemTime>,
}

/// The main FileTree widget state.
///
/// `Clone` is cheap relative to rebuilding: the nodes and flattened lines are
/// copied, but the size cache is shared (it is `Arc`-backed). Splitting the view
/// clones the tree rather than re-listing the directory.
#[derive(Clone)]
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
    /// Where this tree's data comes from: the local filesystem, or a remote
    /// backend. Local trees behave exactly as before; a remote tree routes every
    /// listing and size query through its backend.
    pub source: Source,
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
        Self::build_with_source(
            Source::Local,
            path,
            sort_mode,
            show_hidden,
            show_size_bar,
            cache,
            cancel,
            progress,
        )
    }

    /// As [`build`], but from an explicit [`Source`] — the entry point for
    /// remote trees. The root node's directory-ness comes from the source
    /// rather than a local stat.
    #[allow(clippy::too_many_arguments)]
    fn build_with_source(
        source: Source,
        path: PathBuf,
        sort_mode: SortMode,
        show_hidden: bool,
        show_size_bar: bool,
        cache: SizeCache,
        cancel: Option<&sizes::CancelToken>,
        progress: Option<&crate::widget::progress::OpProgress>,
    ) -> Option<Self> {
        // The root is always a directory; determine it via the source so a
        // remote root doesn't trigger a local stat.
        let root_is_dir = source.is_dir(&path);
        // A remote root must not be canonicalized against the *local* filesystem:
        // a path like /var/log or /home/<user> often exists locally too, so
        // `canonicalize` silently resolves the wrong one. That rewrites the
        // node's cache key and every later lookup misses.
        let mut root = TreeNode::with_meta_link_resolved(
            path.clone(),
            root_is_dir,
            None,
            None,
            false,
            !source.is_remote(),
        );
        root.expand_cancellable_progress(&source, &cache, sort_mode, show_hidden, cancel, progress);

        // The scan was abandoned partway through — discard the partial tree.
        if cancel.map(|c| c.is_cancelled()).unwrap_or(false) {
            return None;
        }

        // Size the root from the children just loaded. `load_children` measures
        // the entries *inside* a directory but not the directory itself, and
        // nothing else may measure it later: rendering and sorting read the
        // cache without ever falling back to the filesystem. Summing what is
        // already cached costs nothing and keeps the root's bar and info panel
        // correct.
        if cache.get(&root.resolved_path).is_none() {
            if let Some(children) = root.children.as_ref() {
                let total: u64 = children
                    .iter()
                    .filter_map(|c| cache.get(&c.resolved_path))
                    .sum();
                cache.insert(&root.resolved_path, total);
            }
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
            source,
        };
        tree.reflatten();
        Some(tree)
    }

    /// Build a remote tree from a [`Source`], reusing (or seeding) a size cache.
    #[allow(clippy::too_many_arguments)]
    pub fn with_source_cancellable_progress(
        source: Source,
        path: PathBuf,
        sort_mode: SortMode,
        show_hidden: bool,
        show_size_bar: bool,
        cache: SizeCache,
        cancel: &sizes::CancelToken,
        progress: &crate::widget::progress::OpProgress,
    ) -> Option<Self> {
        Self::build_with_source(
            source,
            path,
            sort_mode,
            show_hidden,
            show_size_bar,
            cache,
            Some(cancel),
            Some(progress),
        )
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

        // Compute parent totals. Keyed on borrowed paths: a `to_path_buf` per
        // line allocated once for every entry in the tree on every reflatten.
        let mut parent_totals: std::collections::HashMap<&Path, u64> =
            std::collections::HashMap::new();
        for (i, line) in self.lines.iter().enumerate() {
            if line.depth == 0 {
                continue;
            }
            if let Some(parent) = line.resolved_path.parent() {
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
            .map(|i| expanded_set.contains(self.lines[i].resolved_path.as_path()))
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
        // Clone the source (cheap: Arc handles for remote, unit for local) so it
        // isn't borrowed while `self.root` is borrowed mutably.
        let source = self.source.clone();
        expand_node_by_path(
            &mut self.root,
            &source,
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

    /// Reload just the direct children of the directory at `resolved_path`,
    /// re-reading that one directory from disk while keeping the size cache and
    /// the rest of the tree untouched. Used after creating a directory: far
    /// cheaper than a full `refresh()` (which clears the cache and rescans the
    /// whole tree). If the directory isn't loaded/expanded in the tree yet,
    /// nothing changes on screen and this is a no-op.
    pub fn reload_dir(&mut self, resolved_path: &Path) {
        let source = self.source.clone();
        if reload_children_by_path(
            &mut self.root,
            &source,
            resolved_path,
            &self.size_cache,
            self.sort_mode,
            self.show_hidden,
        ) {
            self.reflatten();
        }
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
        let source = self.source.clone();
        self.root
            .expand_all(&source, &self.size_cache, self.sort_mode, self.show_hidden);
        self.reflatten();
    }

    /// Collapse all nodes.
    pub fn collapse_all(&mut self) {
        self.root.collapse_all();
        self.reflatten();
    }

    /// Change sort mode and re-order the already-loaded tree.
    ///
    /// Sorting is pure reordering: the nodes and their cached sizes are already
    /// in memory, so this never re-lists a directory or touches the cache. That
    /// matters on a remote panel, where re-listing would fire an SFTP round trip
    /// per directory on the event-loop thread and lock the UI up.
    pub fn set_sort_mode(&mut self, mode: SortMode) {
        self.sort_mode = mode;
        let source = self.source.clone();
        resort_node(&mut self.root, &source, &self.size_cache, mode);
        self.reflatten();
    }

    /// Toggle hidden files visibility.
    pub fn toggle_hidden(&mut self) {
        self.show_hidden = !self.show_hidden;

        // A remote tree is always loaded with every entry (hidden included), and
        // `flatten_node` does the actual hiding — so toggling is a pure reflatten
        // with no I/O. Re-listing here would fire an SFTP round trip per
        // directory on the event-loop thread and lock the UI up.
        if self.source.is_remote() {
            self.reflatten();
            return;
        }

        // Local: hidden entries may have been skipped by `load_children` while
        // hidden was off, so re-list expanded levels to pick them up. Cheap on a
        // local disk.
        self.size_cache.clear();
        let source = self.source.clone();
        reload_node(
            &mut self.root,
            &source,
            &self.size_cache,
            self.sort_mode,
            self.show_hidden,
        );
        self.reflatten();
    }

    /// Render a single TreeLine as a ratatui Line.
    /// All heavy computation is precomputed — this function only does string formatting.
    #[allow(clippy::too_many_arguments)]
    fn render_line<'a>(
        &'a self,
        line: &'a TreeLine,
        is_selected: bool,
        is_expanded: bool,
        is_tagged: bool,
        is_ghost: bool,
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

        // Tag marker column: a bright chevron on tagged rows, a space otherwise,
        // so tagged files are obvious even under the cursor and keep alignment
        // with untagged rows.
        if is_tagged {
            // Mirrors the name styling below: inverted on the cursor row so the
            // marker column also distinguishes "tagged and here" from "tagged".
            let marker_style = if is_selected {
                Style::default()
                    .fg(TAG_COLOR)
                    .bg(Color::Black)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default()
                    .fg(Color::Black)
                    .bg(TAG_COLOR)
                    .add_modifier(Modifier::BOLD)
            };
            spans.push(Span::styled("▶ ", marker_style));
        } else if is_selected {
            // An untagged cursor row still needs a marker; without one the only
            // cue is REVERSED, which some terminals render weakly.
            spans.push(Span::styled(
                "› ",
                Style::default().add_modifier(Modifier::REVERSED | Modifier::BOLD),
            ));
        } else {
            spans.push(Span::raw("  "));
        }

        // Indentation: two spaces per level.
        for _ in 0..line.depth {
            spans.push(Span::raw("  "));
        }

        // Icon. Symlinks get their own glyph so they're distinguishable at a
        // glance from the real file or directory they point at.
        let icon = if line.is_symlink {
            "🔗"
        } else if line.is_dir {
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
        } else if line.is_symlink {
            // Distinct from both the blue of a real directory and the default of
            // a file, since a link can be either.
            (Some(SYMLINK_COLOR), Modifier::ITALIC)
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
        // Tagged rows get a bright, high-contrast fill so their staged state is
        // unmistakable.
        //
        // The cursor has to remain visible on a tagged row. Simply dropping
        // REVERSED there made the cursor row identical to every other tagged row,
        // so moving through a directory of tagged files lost the cursor entirely.
        // Instead the two states use different fills: the cursor row inverts to
        // amber-on-black, tagged-but-unselected rows stay black-on-amber. Both
        // read as "tagged" (same colour, same ▶ marker) while only one reads as
        // "here".
        if is_tagged {
            style = if is_selected {
                Style::default()
                    .fg(TAG_COLOR)
                    .bg(Color::Black)
                    .add_modifier(Modifier::BOLD)
            } else {
                style
                    .remove_modifier(Modifier::REVERSED)
                    .fg(Color::Black)
                    .bg(TAG_COLOR)
                    .add_modifier(Modifier::BOLD)
            };
        }
        // A ghost — a transfer in progress toward this location — is drawn in a
        // muted, italic style so it reads clearly as "not there yet". It wins
        // over the normal/tag styling; a ghost is never the cursor selection.
        if is_ghost {
            style = Style::default()
                .fg(GHOST_COLOR)
                .add_modifier(Modifier::ITALIC | Modifier::DIM);
        }
        let mut name_span = Span::styled(line.name.clone(), style);
        // Mark ghosts with a trailing hint so the state is unmistakable even in
        // a monochrome terminal.
        if is_ghost {
            name_span = Span::styled(format!("{} (copying…)", line.name), style);
        } else if line.is_symlink {
            // A trailing arrow (with a slash for link-to-directory) keeps the
            // distinction readable without colour — and on a monochrome or
            // colour-blind-unfriendly terminal it is the only cue that survives.
            let suffix = if line.is_dir { "@/" } else { "@" };
            name_span = Span::styled(format!("{}{}", line.name, suffix), style);
        }
        spans.push(name_span);

        Line::from(spans)
    }

    /// Get size from cache, or compute and cache it. Uses recursive size for dirs.
    fn get_or_compute_size(&self, line: &TreeLine) -> u64 {
        if let Some(size) = self.size_cache.get(&line.resolved_path) {
            return size;
        }
        // A miss reports "unknown" rather than measuring the entry here.
        //
        // This runs from `recompute_cache` — on every reflatten, so on every
        // sort, filter and hidden toggle — on the event-loop thread. Measuring a
        // directory means a full recursive walk, and even a file costs a stat.
        // That is not affordable on a slow filesystem, and "local" does not
        // imply "fast": a CIFS or NFS mount is an ordinary path here, and a
        // recursive walk over one made changing the sort order unresponsive.
        //
        // Sizes are gathered when the tree is loaded (see `load_children`),
        // which is where the scanning belongs — it runs off the event loop with
        // a progress overlay and can be cancelled.
        0
    }

    /// Render the full tree as ratatui Text.
    /// Uses precomputed cache — zero lookups, zero allocations during render.
    pub fn render_text(&self) -> Text<'_> {
        self.render_text_with_ghosts(&[])
    }

    /// Render the tree, overlaying "ghost" rows for in-progress transfer
    /// destinations. A ghost whose path already exists in the tree recolors that
    /// row; a ghost for a not-yet-present entry is injected as a synthetic row
    /// under its parent directory (when that directory is visible).
    pub fn render_text_with_ghosts(&self, ghosts: &[crate::transfer::PendingDest]) -> Text<'static> {
        use std::collections::HashSet;

        // Paths (by their on-disk form) that a transfer is writing to.
        let ghost_paths: HashSet<&std::path::Path> =
            ghosts.iter().map(|g| g.path.path.as_path()).collect();

        // Ghosts that don't correspond to an existing line become injected rows,
        // grouped by the parent directory they belong under.
        let existing: HashSet<&std::path::Path> =
            self.lines.iter().map(|l| l.path.as_path()).collect();
        let mut injected: std::collections::HashMap<PathBuf, Vec<&crate::transfer::PendingDest>> =
            std::collections::HashMap::new();
        for g in ghosts {
            if existing.contains(g.path.path.as_path()) {
                continue; // handled by recoloring below
            }
            if let Some(parent) = g.path.path.parent() {
                injected.entry(parent.to_path_buf()).or_default().push(g);
            }
        }

        let mut out: Vec<Line<'static>> = Vec::with_capacity(self.lines.len() + ghosts.len());
        for (i, line) in self.lines.iter().enumerate() {
            let is_ghost = ghost_paths.contains(line.path.as_path());
            out.push(owned_line(self.render_line(
                line,
                i == self.cursor,
                self.cached_expanded.get(i).copied().unwrap_or(false),
                self.tagged.contains(&line.resolved_path),
                is_ghost,
                if self.show_size_bar { self.cached_sizes.get(i).copied().unwrap_or(0) } else { 0 },
                if self.show_size_bar { self.cached_siblings.get(i).copied().unwrap_or(0) } else { 0 },
            )));

            // If this line is a directory that has pending arrivals, inject their
            // ghost rows right after it (indented one level deeper).
            if line.is_dir {
                if let Some(pending) = injected.get(line.path.as_path()) {
                    for g in pending {
                        out.push(self.render_ghost_row(g, line.depth + 1));
                    }
                }
            }
        }

        // Ghosts destined for the tree's own root directory (the parent is the
        // root path, which has no line of its own in most layouts) attach at the
        // top level.
        if let Some(pending) = injected.get(self.root.path.as_path()) {
            // Only if the root itself isn't already a rendered directory line
            // (avoid double-injection).
            let root_has_line = self.lines.iter().any(|l| l.path == self.root.path);
            if !root_has_line {
                for g in pending {
                    out.push(self.render_ghost_row(g, 0));
                }
            }
        }

        Text::from(out)
    }

    /// Build a synthetic ghost row for a destination that isn't in the tree yet.
    fn render_ghost_row(
        &self,
        ghost: &crate::transfer::PendingDest,
        depth: usize,
    ) -> Line<'static> {
        let name = ghost
            .path
            .file_name()
            .map(|s| s.to_string())
            .unwrap_or_default();
        let synthetic = TreeLine {
            path: ghost.path.path.clone(),
            resolved_path: ghost.path.path.clone(),
            is_dir: ghost.is_dir,
            depth,
            hidden: false,
            name,
            is_symlink: false,
            mtime: None,
            atime: None,
        };
        owned_line(self.render_line(&synthetic, false, false, false, true, 0, 0))
    }
}

/// Convert a borrowed rendered line into an owned one, so the ghost-aware render
/// can mix real lines (which borrow `self.lines`) with synthetic ones.
fn owned_line(line: Line<'_>) -> Line<'static> {
    Line::from(
        line.spans
            .into_iter()
            .map(|s| Span::styled(s.content.into_owned(), s.style))
            .collect::<Vec<_>>(),
    )
}

/// Collect all expanded node resolved paths for O(1) lookup during render.
fn collect_expanded(node: &TreeNode) -> HashSet<&Path> {
    let mut result = HashSet::new();
    collect_expanded_into(node, &mut result);
    result
}

/// Fill `out` with the expanded nodes' paths, borrowing rather than cloning.
///
/// This runs on every reflatten — so on every sort, filter and hidden toggle.
/// Building one set of borrowed paths, instead of a fresh `HashSet<PathBuf>` per
/// node that the parent then `extend`s, avoids a clone and a map merge per entry.
fn collect_expanded_into<'a>(node: &'a TreeNode, out: &mut HashSet<&'a Path>) {
    if node.is_expanded {
        out.insert(node.resolved_path.as_path());
    }
    if let Some(ref children) = node.children {
        for child in children {
            collect_expanded_into(child, out);
        }
    }
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
        is_symlink: node.is_symlink,
        mtime: node.mtime,
        atime: node.atime,
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
    source: &Source,
    target: &Path,
    cache: &SizeCache,
    sort_mode: SortMode,
    show_hidden: bool,
) {
    if node.resolved_path == target {
        // Load children and mark expanded.
        node.expand(source, cache, sort_mode, show_hidden);
        return;
    }
    // Recurse into already-loaded children.
    if let Some(ref mut children) = node.children {
        for child in children {
            expand_node_by_path(child, source, target, cache, sort_mode, show_hidden);
        }
    }
}

/// Find the directory node at `target` and reload only its direct children from
/// disk (preserving deeper expansion state). Returns whether the node was found
/// and reloaded. Only reloads a node whose children are already loaded, so a
/// collapsed/unloaded directory is left alone.
fn reload_children_by_path(
    node: &mut TreeNode,
    source: &Source,
    target: &Path,
    cache: &SizeCache,
    sort_mode: SortMode,
    show_hidden: bool,
) -> bool {
    if node.resolved_path == target {
        if node.is_dir && node.children.is_some() {
            // Reload this level; children already in the cache are skipped, so a
            // newly created (uncached) entry is the only real work.
            reload_node(node, source, cache, sort_mode, show_hidden);
        }
        return true;
    }
    if let Some(ref mut children) = node.children {
        for child in children {
            if reload_children_by_path(child, source, target, cache, sort_mode, show_hidden) {
                return true;
            }
        }
    }
    false
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
