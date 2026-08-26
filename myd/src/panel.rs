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
    /// Info panel width, as a percentage of the panel it sits in.
    ///
    /// Lives here rather than on the screen for the same reason the rest do —
    /// a new screen per directory would otherwise snap it back on every `cd`.
    /// Seeded from [`crate::prefs::Prefs`] at startup and written back when
    /// changed, so it also survives a restart.
    pub info_panel_pct: u16,
    /// Rows added to the info panel's metadata share, set by `+` and `-`.
    ///
    /// Signed so it means "shift the split", which reads the same whatever the
    /// terminal height — an absolute row count would mean something different
    /// on every screen.
    pub info_meta_bias: i16,
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
            info_panel_pct: crate::prefs::startup().info_panel_pct,
            info_meta_bias: crate::prefs::startup().info_meta_bias,
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
    /// A long operation this panel is waiting on, if any.
    ///
    /// Held here rather than as an app-wide modal so the *other* panel stays
    /// usable: a connect to an unreachable host, or a move that is one round
    /// trip per entry, otherwise locked the whole interface behind an overlay
    /// that only `q`/`Esc` could dismiss. `delete_task` above has always worked
    /// this way; this brings the rest into line with it.
    pub busy: Option<PanelBusy>,
}

/// What a panel is waiting on, and how to describe and stop it.
pub struct PanelBusy {
    /// Shown in the panel's overlay — "Connecting", "Moving", …
    pub verb: &'static str,
    /// Live counts for the operations that report them. `None` leaves the
    /// overlay a plain spinner, which is all an unmeasurable wait can honestly
    /// show.
    pub progress: Option<crate::widget::progress::OpProgress>,
    /// Stops the work when the user backs out with `q`/`Esc`.
    ///
    /// A connect has nothing cooperative to cancel — dropping the receiver
    /// detaches the task — so it leaves this `None` and is cancelled through
    /// `cancel_connect` instead.
    pub cancel: Option<crate::utils::sizes::CancelToken>,
}

impl Panel {
    /// Build a panel rooted at `start`. A valid directory starts loading; when
    /// no path is given the panel opens on the current working directory (the
    /// directory picker is reserved for the `gd` chord). Only a path that turns
    /// out not to be a directory falls back to the picker.
    pub fn new(start: Option<PathBuf>) -> Self {
        Self::new_maybe_shallow(start, false)
    }

    /// As [`Self::new`], opening without measuring directory sizes when
    /// `shallow`. The `-s` flag, which is the `S` toggle applied from the start.
    pub fn new_maybe_shallow(start: Option<PathBuf>, shallow: bool) -> Self {
        // The only difference is which source the tree is built through, so the
        // two paths share everything else — a second copy of the is_dir /
        // fall-back-to-picker logic would be one more place to get it wrong.
        let load = |path: PathBuf| {
            if shallow {
                Screen::loading_with_source_sorted(
                    crate::widget::source::Source::LocalShallow,
                    path,
                    None,
                    crate::screen::SortMode::default(),
                )
            } else {
                Screen::loading(path)
            }
        };
        let screen = match start {
            Some(path) => {
                let resolved = expand_user(&path).canonicalize().unwrap_or(path);
                if resolved.is_dir() {
                    load(resolved)
                } else {
                    Screen::dir_picker()
                }
            }
            // Default to the current directory rather than the picker.
            None => match std::env::current_dir() {
                Ok(cwd) => load(cwd),
                Err(_) => Screen::dir_picker(),
            },
        };

        Self {
            screen_stack: vec![screen],
            view_prefs: ViewPrefs::default(),
            delete_task: None,
            busy: None,
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
            // The revealed tree is the authority on which backend this panel is
            // showing, exactly as it is when a load resolves (see
            // `resolve_loading_reporting`) and for the same reason. Only loads
            // used to set this, and a pop reveals a screen that already resolved
            // — so leaving an SFTP tree with `h` left the panel still tagged
            // remote, and the next copy addressed the destination as
            // `remote:/…`, which the server does not have. A pop to a `Loading`
            // screen is left alone deliberately: it sets the backend itself when
            // it resolves on the next tick.
            self.backend = state.tree.source.backend();
        }
    }

    /// Check for a completed loading task and replace the Loading screen with a
    /// Main screen. Returns `false` when a cancelled scan left this panel empty
    /// (nothing to fall back to).
    pub fn resolve_loading(&mut self) -> bool {
        self.resolve_loading_reporting(&mut None)
    }

    /// As [`Self::resolve_loading`], reporting the directory that just opened.
    ///
    /// Every way of arriving somewhere — typing a path, picking from the list,
    /// Enter in the tree, `gu`, a treemap tile — ends in a loading screen
    /// resolving here. Recording the visit at this one seam is why browsing to a
    /// directory now enters the history, which only the picker's own confirm
    /// used to do.
    pub fn resolve_loading_reporting(&mut self, opened: &mut Option<PathBuf>) -> bool {
        let prefs = self.view_prefs;
        let outcome = match self.screen_stack.last_mut() {
            Some(last) => last.poll_loading(),
            None => return true,
        };
        match outcome {
            LoadingOutcome::Pending => true,
            LoadingOutcome::Done(path, tree) => {
                // Only local directories are worth remembering: a remote path is
                // meaningless without the connection it belongs to, and the saved
                // host already records that.
                if !tree.source.is_remote() {
                    *opened = Some(path.clone());
                }
                // The tree that just loaded is the authority on which machine
                // this panel is showing, so adopt its backend. The field was only
                // set when a panel was created or connected: navigating a remote
                // panel to a local path (`gd` to a mounted volume, say) left it
                // still tagged remote, and the next copy addressed the
                // destination as `remote:/Volumes/…`, which the server does not
                // have. The reverse — a local panel that opens a remote tree —
                // has the same hazard.
                self.backend = tree.source.backend();
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

    /// Re-list `dir` in every screen on this panel's stack that is showing it.
    ///
    /// Every screen, not just the visible one, for the reason spelled out on
    /// [`Self::resolve_deleting`]: `Enter` pushes a screen and each keeps its
    /// own tree, so refreshing only the top one leaves the views beneath
    /// describing a directory as it was before the change. `h` back up then
    /// shows a name that has been renamed away, or misses one just created.
    ///
    /// A screen not showing `dir` is left alone, so this disturbs nothing that
    /// was not already displaying the directory that changed.
    pub fn reload_dir_everywhere(&mut self, dir: &std::path::Path) {
        for screen in &mut self.screen_stack {
            if let Screen::Main(state) = screen {
                let shows = state.root_path() == dir
                    || state.tree.lines.iter().any(|l| l.path == dir);
                if shows {
                    state.reload_dir_public(dir);
                }
            }
        }
    }

    /// Check for a completed delete task and remove every deleted path from the
    /// trees in place.
    ///
    /// Every screen on the stack, not just the visible one. `Enter` into a
    /// directory pushes a screen and each keeps the tree it was built with, so
    /// telling only the top one left the screens beneath still listing files
    /// that had been deleted -- and `h` back up showed those ghosts, with the
    /// header counting them. A row for a path that is not there is worse than a
    /// stale count: acting on one addresses a file that no longer exists.
    ///
    /// A screen that never held the path ignores it, so this costs a lookup per
    /// screen and the stack is a handful deep.
    pub fn resolve_deleting(&mut self) {
        if let Some(ref task) = self.delete_task {
            if task.is_finished() {
                self.delete_task.take();
                for path in std::mem::take(&mut self.deleting_paths) {
                    for screen in &mut self.screen_stack {
                        screen.remove_path(&path);
                    }
                }
            }
        }
    }

    /// Whether this panel has a delete in flight (drives the progress overlay).
    pub fn is_deleting(&self) -> bool {
        self.delete_task.is_some()
    }

    /// Mark this panel as waiting on `verb`.
    ///
    /// The panel's screen stack is left alone — the overlay draws over whatever
    /// it was showing, so clearing the state is all it takes to put the pane
    /// back, and a failed operation cannot leave it blank.
    pub fn set_busy(
        &mut self,
        verb: &'static str,
        progress: Option<crate::widget::progress::OpProgress>,
        cancel: Option<crate::utils::sizes::CancelToken>,
    ) {
        self.busy = Some(PanelBusy {
            verb,
            progress,
            cancel,
        });
    }

    /// Whether this panel is waiting on a long operation.
    pub fn is_busy(&self) -> bool {
        self.busy.is_some()
    }

    /// Stop whatever this panel is waiting on, revealing the screen underneath.
    ///
    /// Returns the verb that was cancelled, so the caller can tell which kind of
    /// work it just stopped (a connect needs its task detached as well).
    pub fn cancel_busy(&mut self) -> Option<&'static str> {
        let busy = self.busy.take()?;
        if let Some(cancel) = &busy.cancel {
            cancel.cancel();
        }
        Some(busy.verb)
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

    /// Where a copy or move into this panel should land: the directory the
    /// cursor is actually in, not the pane root.
    ///
    /// These differ whenever the user has expanded a subdirectory and moved into
    /// it, which is most of the time. Using the root sent copies to a directory
    /// the user was no longer looking at — and on a remote host that is often one
    /// they cannot write to, so it surfaced as a confusing permission error
    /// rather than as a misplaced file.
    pub fn dest_dir(&self) -> Option<PathBuf> {
        match self.current_screen() {
            Screen::Main(state) => Some(state.target_dir()),
            _ => None,
        }
    }

    /// The path currently selected in this panel's top Main screen, as displayed.
    ///
    /// Not the resolved form: canonicalising would hand a consumer the symlink's
    /// target rather than the entry the user is looking at.
    pub fn selected_path(&self) -> Option<PathBuf> {
        match self.current_screen() {
            Screen::Main(state) => state.selected_path().cloned(),
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

    /// Open a panel showing `screen`, with no directory scan started.
    ///
    /// [`new`] always begins loading something, so building a panel and then
    /// replacing its screen would leave a full walk of the current directory
    /// running for a tree nobody is going to see. Used for `--directory`, which
    /// opens on the picker instead.
    pub fn new_on_screen(screen: Screen) -> Self {
        Self {
            screen_stack: vec![screen],
            view_prefs: ViewPrefs::default(),
            delete_task: None,
            busy: None,
            deleting_paths: Vec::new(),
            backend: crate::vfs::BackendId::LOCAL,
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
            busy: None,
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
            busy: None,
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
            busy: None,
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::screen::SortMode;
    use crate::utils::sizes::{CancelToken, SizeCache};
    use crate::vfs::{BackendId, Vfs};
    use crate::widget::file_tree::FileTree;
    use crate::widget::progress::OpProgress;
    use crate::widget::source::{RemoteSource, Source};
    use std::sync::Arc;

    /// Build a `Screen::Main` over `path` from `source`.
    ///
    /// The tree is built synchronously here rather than through a loading
    /// screen: these tests are about what a *resolved* stack does, and driving
    /// the async load would only add a poll loop between the setup and the
    /// assertion.
    fn main_screen(source: Source, path: &std::path::Path) -> Screen {
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
        Screen::Main(MainScreenState::from_tree(path.to_path_buf(), tree))
    }

    /// A non-local `Source` that needs no network: `LocalFs` behind a
    /// `RemoteSource`, the same stand-in `source.rs`'s own tests use.
    fn nonlocal_source(id: BackendId) -> Source {
        let vfs: Arc<dyn Vfs> = Arc::new(crate::vfs::LocalFs::new());
        Source::Remote(RemoteSource::new(id, vfs).expect("driver thread starts"))
    }

    #[test]
    fn popping_back_to_a_local_screen_restores_the_panel_backend() {
        // Leaving a non-local tree with `h` used to leave the panel still
        // tagged with the non-local backend, because only a *load* set the
        // field and a pop reveals a screen that already resolved. The next copy
        // or delete then addressed the wrong filesystem.
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("sub")).unwrap();

        let mut panel = Panel::new_on_screen(main_screen(Source::Local, dir.path()));
        assert_eq!(panel.backend, BackendId::LOCAL);

        panel
            .screen_stack
            .push(main_screen(nonlocal_source(BackendId(1)), dir.path()));
        panel.backend = BackendId(1);

        panel.pop_screen();

        assert_eq!(
            panel.backend,
            BackendId::LOCAL,
            "popping back to a local screen must retag the panel local"
        );
    }

    #[test]
    fn popping_back_to_a_nonlocal_screen_restores_that_backend() {
        // The mirror case: descending from a non-local tree into a local one
        // and back must restore the *non-local* id, not merely reset to local.
        let dir = tempfile::tempdir().unwrap();

        let mut panel = Panel::new_on_screen(main_screen(nonlocal_source(BackendId(3)), dir.path()));
        panel.backend = BackendId(3);
        panel.screen_stack.push(main_screen(Source::Local, dir.path()));
        panel.backend = BackendId::LOCAL;

        panel.pop_screen();

        assert_eq!(panel.backend, BackendId(3));
    }

    #[test]
    fn popping_to_a_screen_with_no_tree_leaves_the_backend_alone() {
        // Only a tree can answer which backend a panel is showing. A pop onto a
        // picker (or onto a `Loading` screen, which sets the field itself when
        // it resolves) must not guess.
        let dir = tempfile::tempdir().unwrap();
        let mut panel = Panel::new_on_screen(Screen::dir_picker());
        panel.backend = BackendId(2);
        panel
            .screen_stack
            .push(main_screen(Source::Local, dir.path()));

        panel.pop_screen();

        assert_eq!(panel.backend, BackendId(2));
    }
}
