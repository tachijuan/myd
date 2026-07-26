mod dir_picker;
mod loading;
mod main_screen;

use ratatui::{Frame, layout::Rect};
use crossterm::event::KeyEvent;

pub use dir_picker::DirPickerState;
pub use loading::LoadingState;
pub use main_screen::MainScreenState;

use crate::widget::file_tree::FileTree;
use crate::widget::treemap::FocusTarget;

/// Result of polling a loading screen.
pub enum LoadingOutcome {
    /// Still scanning.
    Pending,
    /// Scan finished; carries the scanned path and its tree.
    Done(std::path::PathBuf, FileTree),
    /// The user cancelled the scan.
    Cancelled,
}

/// Sort order for file tree entries.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SortMode {
    DirsFirst,
    FilesFirst,
    #[default]
    Largest,
    Smallest,
    /// Newest modification time first.
    Newest,
    /// Oldest modification time first.
    Oldest,
    /// Most recently accessed first.
    RecentlyAccessed,
}

impl SortMode {
    pub fn label(&self) -> &'static str {
        match self {
            SortMode::DirsFirst => "dirs-first",
            SortMode::FilesFirst => "files-first",
            SortMode::Largest => "largest",
            SortMode::Smallest => "smallest",
            SortMode::Newest => "newest",
            SortMode::Oldest => "oldest",
            SortMode::RecentlyAccessed => "recently-accessed",
        }
    }

    /// Every mode, in the order `s` cycles through them.
    ///
    /// Single source of truth for the cycle, the sort menu and the help text, so
    /// adding a mode cannot leave one of them behind.
    pub const ALL: [SortMode; 7] = [
        SortMode::Largest,
        SortMode::Smallest,
        SortMode::DirsFirst,
        SortMode::FilesFirst,
        SortMode::Newest,
        SortMode::Oldest,
        SortMode::RecentlyAccessed,
    ];

    /// A one-line explanation, for the sort menu.
    pub fn description(&self) -> &'static str {
        match self {
            SortMode::DirsFirst => "Directories first, then files, each A-Z",
            SortMode::FilesFirst => "Files first, then directories, each A-Z",
            SortMode::Largest => "Biggest first",
            SortMode::Smallest => "Smallest first",
            SortMode::Newest => "Most recently modified first",
            SortMode::Oldest => "Least recently modified first",
            SortMode::RecentlyAccessed => "Most recently opened first",
        }
    }
}

/// Top-level screen enum.
pub enum Screen {
    DirPicker(DirPickerState),
    Main(MainScreenState),
    Loading(LoadingState),
}

impl Screen {
    pub fn dir_picker() -> Self {
        Screen::DirPicker(DirPickerState::new())
    }

    /// Start loading a directory tree asynchronously.
    /// Returns a Loading screen that spawns a background task to build the tree.
    pub fn loading(path: std::path::PathBuf) -> Self {
        Self::loading_with_cache(path, None)
    }

    /// Start loading a directory tree, optionally reusing an existing size
    /// cache. Passing the parent tree's cache makes drilling into a
    /// subdirectory reuse sizes that were already computed instead of
    /// rescanning the disk.
    pub fn loading_with_cache(
        path: std::path::PathBuf,
        cache: Option<crate::utils::sizes::SizeCache>,
    ) -> Self {
        Self::loading_sorted(path, cache, SortMode::default())
    }

    /// As [`loading_with_cache`], built in a given sort order.
    ///
    /// The order has to be decided here rather than applied afterwards: the tree
    /// is sorted as it is built, so handing it down keeps a drilled-into
    /// directory in whatever order the user had chosen.
    pub fn loading_sorted(
        path: std::path::PathBuf,
        cache: Option<crate::utils::sizes::SizeCache>,
        sort_mode: SortMode,
    ) -> Self {
        use crate::utils::sizes::CancelToken;
        use crate::widget::file_tree::FileTree;
        use crate::widget::progress::OpProgress;
        use tokio::sync::oneshot;

        let (tx, rx) = oneshot::channel();
        let cancel = CancelToken::new();
        let progress = OpProgress::new();
        tokio::task::spawn_blocking({
            let path = path.clone();
            let cancel = cancel.clone();
            let progress = progress.clone();
            move || {
                let cache = cache.unwrap_or_default();
                let tree = FileTree::with_cache_cancellable_progress(
                    path,
                    sort_mode,
                    true,
                    true,
                    cache,
                    &cancel,
                    &progress,
                );
                let _ = tx.send(tree);
            }
        });
        Screen::Loading(LoadingState::new(path, rx, cancel, progress))
    }

    /// Start loading a *remote* directory tree from a [`Source`].
    ///
    /// Mirrors [`loading_with_cache`] but builds the tree through the source's
    /// backend. The source carries its own runtime, so building on the blocking
    /// pool turns the async `Vfs` calls into synchronous ones without making the
    /// tree async.
    pub fn loading_remote(
        source: crate::widget::source::Source,
        path: std::path::PathBuf,
        cache: Option<crate::utils::sizes::SizeCache>,
    ) -> Self {
        Self::loading_remote_sorted(source, path, cache, SortMode::default())
    }

    /// As [`loading_remote`], built in a given sort order.
    pub fn loading_remote_sorted(
        source: crate::widget::source::Source,
        path: std::path::PathBuf,
        cache: Option<crate::utils::sizes::SizeCache>,
        sort_mode: SortMode,
    ) -> Self {
        use crate::utils::sizes::CancelToken;
        use crate::widget::file_tree::FileTree;
        use crate::widget::progress::OpProgress;
        use tokio::sync::oneshot;

        let (tx, rx) = oneshot::channel();
        let cancel = CancelToken::new();
        let progress = OpProgress::new();
        tokio::task::spawn_blocking({
            let path = path.clone();
            let cancel = cancel.clone();
            let progress = progress.clone();
            move || {
                let cache = cache.unwrap_or_default();
                let tree = FileTree::with_source_cancellable_progress(
                    source,
                    path,
                    sort_mode,
                    true,
                    true,
                    cache,
                    &cancel,
                    &progress,
                );
                let _ = tx.send(tree);
            }
        });
        Screen::Loading(LoadingState::new(path, rx, cancel, progress))
    }

    pub fn main(path: std::path::PathBuf) -> Self {
        Screen::Main(MainScreenState::new(path))
    }

    /// Poll a loading screen for completion.
    pub fn poll_loading(&mut self) -> LoadingOutcome {
        use loading::LoadingPoll;
        if let Screen::Loading(state) = self {
            return match state.poll() {
                LoadingPoll::Done(tree) => LoadingOutcome::Done(state.path.clone(), tree),
                LoadingPoll::Cancelled => LoadingOutcome::Cancelled,
                LoadingPoll::Pending => LoadingOutcome::Pending,
            };
        }
        LoadingOutcome::Pending
    }

    /// Signal a loading screen to stop scanning, if that's what this screen is.
    pub fn cancel_loading(&self) {
        if let Screen::Loading(state) = self {
            state.cancel();
        }
    }

    /// Check if this screen is a Loading screen.
    pub fn is_loading(&self) -> bool {
        matches!(self, Screen::Loading(_))
    }
}

/// Trait-like interface dispatched from the app loop.
pub trait ScreenState {
    fn cursor_down(&mut self) -> bool { true }
    fn cursor_up(&mut self) -> bool { true }
    fn collapse(&mut self) -> bool { true }
    fn expand(&mut self) -> bool { true }
    fn to_top(&mut self) -> bool { true }
    fn to_bottom(&mut self) -> bool { true }
    fn page_down(&mut self) -> bool { true }
    fn page_up(&mut self) -> bool { true }
    fn go_parent(&mut self) -> bool { true }
    fn change_root(&mut self) -> bool { true }
    fn delete(&mut self) -> bool { true }
    fn refresh(&mut self) -> bool { true }
    fn rename(&mut self) -> bool { true }
    fn toggle_sort(&mut self) -> bool { true }
    fn toggle_hidden(&mut self) -> bool { true }
    fn toggle_bar(&mut self) -> bool { true }
    fn collapse_all(&mut self) -> bool { true }
    fn expand_all(&mut self) -> bool { true }
    fn search(&mut self, _pattern: &str) -> bool { true }
    fn toggle_view(&mut self) -> bool { true }
    fn render(&mut self, frame: &mut Frame, area: Rect);
}

// Delegate Screen -> state.
impl Screen {
    pub fn cursor_down(&mut self) -> bool {
        match self {
            Screen::DirPicker(s) => s.cursor_down(),
            Screen::Main(s) => s.cursor_down(),
            Screen::Loading(_) => true,
        }
    }
    pub fn cursor_up(&mut self) -> bool {
        match self {
            Screen::DirPicker(s) => s.cursor_up(),
            Screen::Main(s) => s.cursor_up(),
            Screen::Loading(_) => true,
        }
    }
    pub fn collapse(&mut self) -> bool {
        match self {
            Screen::DirPicker(s) => s.collapse(),
            Screen::Main(s) => s.collapse(),
            Screen::Loading(_) => true,
        }
    }
    pub fn expand(&mut self) -> bool {
        match self {
            Screen::DirPicker(s) => s.expand(),
            Screen::Main(s) => s.expand(),
            Screen::Loading(_) => true,
        }
    }
    pub fn to_top(&mut self) -> bool {
        match self {
            Screen::DirPicker(s) => s.to_top(),
            Screen::Main(s) => s.to_top(),
            Screen::Loading(_) => true,
        }
    }
    pub fn to_bottom(&mut self) -> bool {
        match self {
            Screen::DirPicker(s) => s.to_bottom(),
            Screen::Main(s) => s.to_bottom(),
            Screen::Loading(_) => true,
        }
    }
    pub fn page_down(&mut self) -> bool {
        match self {
            Screen::DirPicker(s) => s.page_down(),
            Screen::Main(s) => s.page_down(),
            Screen::Loading(_) => true,
        }
    }
    pub fn page_up(&mut self) -> bool {
        match self {
            Screen::DirPicker(s) => s.page_up(),
            Screen::Main(s) => s.page_up(),
            Screen::Loading(_) => true,
        }
    }
    pub fn go_parent(&mut self) -> bool {
        match self {
            Screen::DirPicker(s) => s.go_parent(),
            Screen::Main(s) => s.go_parent(),
            Screen::Loading(_) => true,
        }
    }
    pub fn change_root(&mut self) -> bool {
        match self {
            Screen::DirPicker(s) => s.change_root(),
            Screen::Main(s) => s.change_root(),
            Screen::Loading(_) => true,
        }
    }
    pub fn delete(&mut self) -> bool {
        match self {
            Screen::DirPicker(s) => s.delete(),
            Screen::Main(s) => s.delete(),
            Screen::Loading(_) => true,
        }
    }
    pub fn refresh(&mut self) -> bool {
        match self {
            Screen::DirPicker(s) => s.refresh(),
            Screen::Main(s) => s.refresh(),
            Screen::Loading(_) => true,
        }
    }
    pub fn rename(&mut self) -> bool {
        match self {
            Screen::DirPicker(s) => s.rename(),
            Screen::Main(s) => s.rename(),
            Screen::Loading(_) => true,
        }
    }
    pub fn toggle_sort(&mut self) -> bool {
        match self {
            Screen::DirPicker(s) => s.toggle_sort(),
            Screen::Main(s) => s.toggle_sort(),
            Screen::Loading(_) => true,
        }
    }
    pub fn toggle_hidden(&mut self) -> bool {
        match self {
            Screen::DirPicker(s) => s.toggle_hidden(),
            Screen::Main(s) => s.toggle_hidden(),
            Screen::Loading(_) => true,
        }
    }
    pub fn toggle_bar(&mut self) -> bool {
        match self {
            Screen::DirPicker(s) => s.toggle_bar(),
            Screen::Main(s) => s.toggle_bar(),
            Screen::Loading(_) => true,
        }
    }
    pub fn collapse_all(&mut self) -> bool {
        match self {
            Screen::DirPicker(s) => s.collapse_all(),
            Screen::Main(s) => s.collapse_all(),
            Screen::Loading(_) => true,
        }
    }
    pub fn expand_all(&mut self) -> bool {
        match self {
            Screen::DirPicker(s) => s.expand_all(),
            Screen::Main(s) => s.expand_all(),
            Screen::Loading(_) => true,
        }
    }
    pub fn search(&mut self, pattern: &str) -> bool {
        match self {
            Screen::DirPicker(s) => s.search(pattern),
            Screen::Main(s) => s.search(pattern),
            Screen::Loading(_) => true,
        }
    }
    pub fn filter(&mut self, pattern: &str) -> bool {
        match self {
            Screen::Main(s) => s.filter(pattern),
            _ => true,
        }
    }
    /// Create a directory; returns an error message to surface, or `None`.
    pub fn create_dir(&mut self, name: &str) -> Option<String> {
        match self {
            Screen::Main(s) => s.create_dir(name),
            _ => None,
        }
    }
    pub fn search_next(&mut self) -> bool {
        match self {
            Screen::Main(s) => s.search_next(),
            _ => true,
        }
    }
    pub fn search_prev(&mut self) -> bool {
        match self {
            Screen::Main(s) => s.search_prev(),
            _ => true,
        }
    }
    pub fn toggle_tag(&mut self) -> bool {
        match self {
            Screen::Main(s) => s.toggle_tag(),
            _ => true,
        }
    }
    pub fn untag_all(&mut self) -> bool {
        match self {
            Screen::Main(s) => s.untag_all(),
            _ => true,
        }
    }
    pub fn toggle_visual(&mut self) -> bool {
        match self {
            Screen::Main(s) => s.toggle_visual(),
            _ => true,
        }
    }
    pub fn exit_visual(&mut self) {
        if let Screen::Main(s) = self {
            s.exit_visual();
        }
    }
    /// Snapshot of tagged paths from the top Main screen (empty otherwise).
    pub fn tagged_paths(&self) -> Vec<std::path::PathBuf> {
        match self {
            Screen::Main(s) => s.tagged_paths(),
            _ => Vec::new(),
        }
    }
    /// Clear all tags on the top Main screen (no-op otherwise).
    pub fn clear_tags(&mut self) {
        if let Screen::Main(s) = self {
            s.untag_all();
        }
    }
    pub fn toggle_view(&mut self) -> bool {
        match self {
            Screen::DirPicker(_) => true,
            Screen::Main(s) => {
                s.focus = match s.focus {
                    FocusTarget::Tree => FocusTarget::Treemap,
                    FocusTarget::Treemap => FocusTarget::Tree,
                };
                true
            }
            Screen::Loading(_) => true,
        }
    }
    pub fn navigate(&mut self) {
        match self {
            Screen::DirPicker(_) => {}
            Screen::Main(s) => s.navigate(),
            Screen::Loading(_) => {}
        }
    }
    /// Get the depth of the currently selected line (for navigation logic).
    pub fn selected_line_depth(&self) -> Option<usize> {
        match self {
            Screen::DirPicker(_) => None,
            Screen::Main(s) => s.selected_line_depth(),
            Screen::Loading(_) => None,
        }
    }
    /// Remove a path from the tree in-place (preserves expanded state).
    pub fn remove_path(&mut self, path: &std::path::Path) {
        match self {
            Screen::DirPicker(_) => {}
            Screen::Main(s) => s.remove_path(path),
            Screen::Loading(_) => {}
        }
    }
    pub fn render(&mut self, frame: &mut Frame, area: Rect) {
        match self {
            Screen::DirPicker(s) => s.render(frame, area),
            Screen::Main(s) => s.render(frame, area),
            Screen::Loading(s) => s.render(frame, area),
        }
    }

    /// Handle a raw key event for screens that need direct input (e.g., dir picker).
    /// Returns `true` to keep the app running, `false` to quit.
    /// If the screen consumed the key, returns `Some(true/false)`.
    /// If the screen did not consume it, returns `None` (fall through to keybinding).
    pub fn handle_raw_key(&mut self, key: KeyEvent) -> Option<bool> {
        match self {
            Screen::DirPicker(s) => s.handle_key(key),
            Screen::Main(_) => None,
            Screen::Loading(_) => None,
        }
    }
}
