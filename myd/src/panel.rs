use std::path::{Path, PathBuf};
use tokio::task::JoinHandle;

use crate::screen::{LoadingOutcome, MainScreenState, Screen};
use crate::widget::treemap::FocusTarget;

/// View preferences that belong to a panel's session rather than to one
/// directory.
///
/// Entering a directory builds a fresh `MainScreenState`, so these are held on
/// the panel and applied to each new screen — otherwise hiding the info panel or
/// switching to the treemap would silently undo itself on every navigation.
#[derive(Debug, Clone, Copy)]
pub struct ViewPrefs {
    pub info_panel_hidden: bool,
    pub focus: FocusTarget,
    /// Whether the tree shows its `ls -l` permissions and modification-time
    /// columns.
    pub show_perms: bool,
    pub show_times: bool,
    /// Sort order, carried across screens so drilling into a directory keeps
    /// whatever order the user chose rather than snapping back to the default.
    pub sort_mode: crate::screen::SortMode,
}

impl Default for ViewPrefs {
    fn default() -> Self {
        Self {
            // The file listing is the reason to open the app; the info pane is
            // detail on demand. Start with it closed and let `Ctrl+p` bring it up.
            info_panel_hidden: true,
            focus: FocusTarget::default(),
            // Extra columns are opt-in: together with the size bar they cost
            // over 50 columns before the name even starts.
            show_perms: false,
            show_times: false,
            sort_mode: crate::screen::SortMode::default(),
        }
    }
}

/// One independent filesystem view: its own navigation stack, view preferences,
/// and background delete task. The app owns one panel in single-panel mode and
/// two side by side in dual mode.
pub struct Panel {
    pub screen_stack: Vec<Screen>,
    /// Carried onto every newly created main screen in this panel.
    pub view_prefs: ViewPrefs,
    /// Background delete task for this panel (for progress tracking). Each panel
    /// resolves its own so a delete in one doesn't block the other.
    pub delete_task: Option<JoinHandle<()>>,
    /// Paths being deleted, mirroring `delete_task`. All are removed from the
    /// tree once the task finishes (a delete may target many tagged files).
    pub deleting_paths: Vec<PathBuf>,
    /// Which backend this panel's paths live on. Local by default; set to a
    /// registered remote backend for an SFTP panel. Copies consult this to route
    /// a cross-backend transfer through the queue rather than a local copy.
    pub backend: crate::vfs::BackendId,
}

impl Panel {
    /// Build a panel rooted at `start`. A valid directory starts loading; when
    /// no path is given the panel opens on the current working directory (the
    /// directory picker is reserved for the `gd` chord). Only a path that turns
    /// out not to be a directory falls back to the picker.
    pub fn new(start: Option<PathBuf>) -> Self {
        let screen = match start {
            Some(path) => {
                let resolved = expand_user(&path).canonicalize().unwrap_or(path);
                if resolved.is_dir() {
                    Screen::loading(resolved)
                } else {
                    Screen::dir_picker()
                }
            }
            // Default to the current directory rather than the picker.
            None => match std::env::current_dir() {
                Ok(cwd) => Screen::loading(cwd),
                Err(_) => Screen::dir_picker(),
            },
        };

        Self {
            screen_stack: vec![screen],
            view_prefs: ViewPrefs::default(),
            delete_task: None,
            deleting_paths: Vec::new(),
            backend: crate::vfs::BackendId::LOCAL,
        }
    }

    /// The screen currently on top of this panel's stack.
    pub fn current_screen(&self) -> &Screen {
        self.screen_stack.last().expect("empty stack")
    }

    /// Mutable access to the top screen.
    pub fn current_screen_mut(&mut self) -> &mut Screen {
        self.screen_stack.last_mut().expect("empty stack")
    }

    /// Number of screens on this panel's navigation stack.
    pub fn depth(&self) -> usize {
        self.screen_stack.len()
    }

    /// Pop the top screen and re-apply this panel's view preferences to the
    /// screen it reveals, so the view stays consistent across a pop just as it
    /// is across a push.
    pub fn pop_screen(&mut self) {
        self.screen_stack.pop();
        let prefs = self.view_prefs;
        if let Some(Screen::Main(state)) = self.screen_stack.last_mut() {
            state.apply_view_prefs(prefs);
        }
    }

    /// Check for a completed loading task and replace the Loading screen with a
    /// Main screen. Returns `false` when a cancelled scan left this panel empty
    /// (nothing to fall back to).
    pub fn resolve_loading(&mut self) -> bool {
        let prefs = self.view_prefs;
        let outcome = match self.screen_stack.last_mut() {
            Some(last) => last.poll_loading(),
            None => return true,
        };
        match outcome {
            LoadingOutcome::Pending => true,
            LoadingOutcome::Done(path, tree) => {
                let mut state = MainScreenState::from_tree(path, tree);
                // Carry the panel's view preferences onto the new screen so
                // entering a directory doesn't silently reset them.
                state.apply_view_prefs(prefs);
                *self.screen_stack.last_mut().expect("empty stack") = Screen::Main(state);
                true
            }
            LoadingOutcome::Cancelled => {
                if self.screen_stack.len() > 1 {
                    self.pop_screen();
                    true
                } else {
                    false
                }
            }
        }
    }

    /// Check for a completed delete task and remove every deleted path from the
    /// tree in place.
    pub fn resolve_deleting(&mut self) {
        if let Some(ref task) = self.delete_task {
            if task.is_finished() {
                self.delete_task.take();
                for path in std::mem::take(&mut self.deleting_paths) {
                    self.current_screen_mut().remove_path(&path);
                }
            }
        }
    }

    /// Whether this panel has a delete in flight (drives the progress overlay).
    pub fn is_deleting(&self) -> bool {
        self.delete_task.is_some()
    }

    /// The directory the top Main screen is rooted at — the destination when
    /// this panel is the *other* panel in a cross-panel copy. `None` when the
    /// panel is showing a dir picker or still loading.
    pub fn current_dir(&self) -> Option<PathBuf> {
        match self.current_screen() {
            Screen::Main(state) => Some(state.root_path().clone()),
            _ => None,
        }
    }

    /// The resolved (canonicalized) path currently selected in this panel's top
    /// Main screen — the source of a cross-panel copy.
    pub fn selected_resolved_path(&self) -> Option<PathBuf> {
        match self.current_screen() {
            Screen::Main(state) => state.selected_resolved_path().cloned(),
            _ => None,
        }
    }

    /// This panel's shared size cache, if it is showing a directory. Cloning the
    /// cache shares the same underlying map (it is `Arc<DashMap>`), so handing it
    /// to a second panel opened on the same tree lets that panel reuse every
    /// already-measured size instead of re-walking the disk.
    pub fn size_cache(&self) -> Option<crate::utils::sizes::SizeCache> {
        match self.current_screen() {
            Screen::Main(state) => Some(state.tree.size_cache.clone()),
            _ => None,
        }
    }

    /// Open a panel on a remote backend, rooted at `path`, building its tree
    /// through `source`. Used after a successful SFTP connection.
    pub fn new_remote(source: crate::widget::source::Source, path: PathBuf) -> Self {
        let backend = source.backend();
        Self {
            screen_stack: vec![Screen::loading_remote(source, path, None)],
            view_prefs: ViewPrefs::default(),
            delete_task: None,
            deleting_paths: Vec::new(),
            backend,
        }
    }

    /// As [`new`], but seed the tree build with an existing size cache so a
    /// panel opened on an already-scanned directory is a cache hit rather than a
    /// full rescan.
    /// A panel showing an already-built tree.
    ///
    /// Used by the split, which copies the active panel's tree instead of
    /// re-listing the directory it is already displaying.
    pub fn from_tree(root: PathBuf, tree: crate::widget::file_tree::FileTree) -> Self {
        Self {
            screen_stack: vec![Screen::Main(
                crate::screen::MainScreenState::from_tree(root, tree),
            )],
            view_prefs: ViewPrefs::default(),
            delete_task: None,
            deleting_paths: Vec::new(),
            backend: crate::vfs::BackendId::LOCAL,
        }
    }

    pub fn new_with_cache(
        start: Option<PathBuf>,
        cache: Option<crate::utils::sizes::SizeCache>,
    ) -> Self {
        let screen = match start {
            Some(path) => {
                let resolved = expand_user(&path).canonicalize().unwrap_or(path);
                if resolved.is_dir() {
                    Screen::loading_with_cache(resolved, cache)
                } else {
                    Screen::dir_picker()
                }
            }
            None => Screen::dir_picker(),
        };

        Self {
            screen_stack: vec![screen],
            view_prefs: ViewPrefs::default(),
            delete_task: None,
            deleting_paths: Vec::new(),
            backend: crate::vfs::BackendId::LOCAL,
        }
    }
}

/// Expand a leading `~` to `$HOME`. Kept here (rather than on the app) so panel
/// construction is self-contained.
fn expand_user(path: &Path) -> PathBuf {
    if path.starts_with("~") {
        if let Some(home) = std::env::var_os("HOME") {
            let mut expanded = PathBuf::from(home);
            expanded.push(path.strip_prefix("~").unwrap_or(Path::new("")));
            return expanded;
        }
    }
    path.to_path_buf()
}
