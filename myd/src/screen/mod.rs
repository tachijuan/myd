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

/// Sort order for file tree entries.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SortMode {
    DirsFirst,
    FilesFirst,
    #[default]
    Largest,
    Smallest,
}

impl SortMode {
    pub fn label(&self) -> &'static str {
        match self {
            SortMode::DirsFirst => "dirs-first",
            SortMode::FilesFirst => "files-first",
            SortMode::Largest => "largest",
            SortMode::Smallest => "smallest",
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
        use crate::screen::SortMode;
        use crate::widget::file_tree::FileTree;
        use tokio::sync::oneshot;

        let (tx, rx) = oneshot::channel();
        tokio::task::spawn_blocking({
            let path = path.clone();
            move || {
                let cache = cache.unwrap_or_default();
                let tree = FileTree::with_cache(path, SortMode::Largest, true, true, cache);
                let _ = tx.send(tree);
            }
        });
        Screen::Loading(LoadingState::new(path, rx))
    }

    pub fn main(path: std::path::PathBuf) -> Self {
        Screen::Main(MainScreenState::new(path))
    }

    /// Poll a loading screen for completion. Returns `Some(tree)` if done.
    pub fn poll_loading(&mut self) -> Option<(std::path::PathBuf, FileTree)> {
        if let Screen::Loading(state) = self {
            if let Some(tree) = state.poll() {
                let path = state.path.clone();
                return Some((path, tree));
            }
        }
        None
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
