use anyhow::Result;
use crossterm::event::{Event, KeyCode, KeyEvent, KeyEventKind};
use ratatui::layout::{Constraint, Layout};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use crate::hosts::{HostCatalog, SavedHost};
use crate::keybinding::{Action, KeyBindingHandler};
use crate::panel::Panel;
use crate::screen::Screen;
use crate::transfer::TransferQueue;
use crate::utils::sizes::CancelToken;
use crate::vfs::{BackendRegistry, VPath};
use crate::widget::confirm_dialog::ConfirmDialog;
use crate::widget::help::render_help;
use crate::widget::host_picker::{HostPicker, PickerOutcome};
use crate::widget::input_dialog::InputDialog;
use crate::widget::progress::{OpProgress, ProgressOverlay};
use crate::widget::transfer_panel;
use crate::widget::treemap::FocusTarget;

/// Drop guard that restores terminal state even if the app panics or is interrupted.
/// Disables raw mode, leaves alternate screen, shows cursor, and flushes output.
struct TerminalGuard;

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        let mut stdout = std::io::stdout();
        // Flush any pending output before cleanup.
        let _ = stdout.flush();
        // Disable raw mode (restores cooked mode for normal terminal I/O).
        let _ = crossterm::terminal::disable_raw_mode();
        // Leave alternate screen, disable mouse capture, show cursor.
        let _ = crossterm::execute!(
            stdout,
            crossterm::terminal::LeaveAlternateScreen,
            crossterm::event::DisableMouseCapture,
            crossterm::cursor::Show,
        );
        // Flush again to ensure all control sequences are sent.
        let _ = stdout.flush();
    }
}

/// Prompt for the one-line saved-host form.
const HOST_FORM_PROMPT: &str =
    "Saved host  —  label = sftp://[user@]host[:port][/path]\n(no password is stored; keys and prompts work as usual)";

/// Modal overlay state.
pub enum Modal {
    None,
    Confirm(ConfirmDialog),
    Input(InputDialog),
    /// A background copy or delete is running. The verb ("Copying"/"Deleting")
    /// titles the overlay; live counts come from `FileBrowser::op_progress`.
    Operation { verb: &'static str },
    Help,
    /// The dialing directory. Owns its own key handling (vi navigation and `/`
    /// search), which is why it is a modal rather than a screen.
    HostPicker(HostPicker),
}

/// Context for modal operations.
pub enum ModalTarget {
    Delete { paths: Vec<PathBuf> },
    Rename { old_path: PathBuf },
    ChangeRoot,
    Search,
    /// Regex filter prompt for the cursor's directory.
    Filter,
    /// New-directory-name prompt; created in the cursor's current directory.
    CreateDir,
    /// Single-panel copy: prompt for a destination directory, then copy `srcs`
    /// into it (with per-collision confirmation).
    CopyDest { srcs: Vec<PathBuf> },
    /// Per-file overwrite confirmation while draining `pending_copies`.
    CopyOverwrite { src: PathBuf, dest: PathBuf },
    /// Per-file overwrite/skip/cancel choice while draining `pending_moves`.
    MoveOverwrite {
        src: PathBuf,
        dest: PathBuf,
        is_dir: bool,
    },
    /// Remote host prompt, e.g. `sftp://user@host/path` or an ssh config alias.
    Connect,
    /// Masked prompt for a key passphrase or account password, feeding a
    /// connection retry.
    Password,
    /// Add or edit a saved host. `editing` is the label being replaced, so a
    /// rename updates the right entry instead of adding a second one.
    HostForm { editing: Option<String> },
    /// Confirm removing a saved host.
    HostDelete { label: String },
}

/// Main application state machine.
pub struct FileBrowser {
    /// One panel in single-panel mode, two side by side in dual mode.
    panels: Vec<Panel>,
    /// Index (0 or 1) of the panel that receives navigation keys.
    active: usize,
    key_handler: KeyBindingHandler,
    modal: Modal,
    modal_target: Option<ModalTarget>,
    /// Background copy task shared across panels (only one copy at a time).
    copy_task: Option<tokio::task::JoinHandle<()>>,
    /// Panel to refresh when `copy_task` finishes.
    copy_dest_panel: usize,
    /// Colliding (src, dest) pairs awaiting a per-file overwrite confirmation.
    pending_copies: Vec<(PathBuf, PathBuf)>,
    /// (src, dest) pairs cleared to copy — spawned as one batch once every
    /// collision has been resolved.
    approved_copies: Vec<(PathBuf, PathBuf)>,
    /// Panel whose tags to clear when the current copy batch completes.
    copy_source_panel: usize,
    /// Live progress for the in-flight copy or delete batch, shared with its
    /// background task and read by the render loop.
    op_progress: Option<OpProgress>,
    /// Cancels the in-flight copy/delete batch. A remote delete is one round
    /// trip per entry, so a large tree must be abandonable from the overlay.
    op_cancel: Option<CancelToken>,
    /// Outcome of the in-flight move: which sources actually left, and any
    /// failures to report. Only the ones that truly moved are dropped from the
    /// source tree.
    #[allow(clippy::type_complexity)]
    move_results: Option<(
        Arc<std::sync::Mutex<Vec<PathBuf>>>,
        Arc<std::sync::Mutex<Vec<String>>>,
    )>,
    /// Colliding moves awaiting an overwrite/skip/cancel answer, and the ones
    /// cleared to run. `(src, dest, is_dir)` — the kind is carried so a queued
    /// cross-backend move draws the right ghost icon.
    #[allow(clippy::type_complexity)]
    pending_moves: Vec<(PathBuf, PathBuf, bool)>,
    #[allow(clippy::type_complexity)]
    approved_moves: Vec<(PathBuf, PathBuf, bool)>,
    /// Panels and destination directory for the move batch being assembled.
    move_source_panel: usize,
    move_dest_panel: usize,
    move_dest_dir: PathBuf,
    /// Live filesystem backends. Index 0 is always the local filesystem.
    backends: BackendRegistry,
    /// Queued and running transfers, drained by the event loop each tick.
    transfers: TransferQueue,
    /// Manual override for the transfer sidebar's visibility.
    ///
    /// `None` is the default: the panel shows itself only once the queue has a
    /// transfer and stays up while any remain — it isn't clutter before you've
    /// started a copy. `Some(true)`/`Some(false)` is a user toggle (`Ctrl+t`)
    /// that pins it open or closed regardless. App-global rather than per-panel
    /// `ViewPrefs`, since the queue itself is app-global.
    transfer_panel_override: Option<bool>,
    /// In-flight SFTP connection attempt, if any. Polled each tick; on success
    /// it registers a backend and opens a remote panel.
    connect_task: Option<ConnectTask>,
    /// Connection details being retried after a credential prompt.
    pending_connect: Option<PendingConnect>,
    /// Saved remote locations, loaded once at startup and written back on every
    /// change so a crash cannot lose an entry that was already confirmed.
    hosts: HostCatalog,
    /// Label of the host currently being connected to, promoted in the ranking
    /// only once the connection actually succeeds — a host that never connects
    /// should not climb the list.
    connecting_label: Option<String>,
}

/// A connection attempt running in the background, with a channel for its result.
struct ConnectTask {
    rx: tokio::sync::oneshot::Receiver<ConnectResult>,
    /// The panel that was active when the connect was issued — the remote opens
    /// here on success, so `gr` replaces the pane the user was looking at.
    target_panel: usize,
}

/// What a connection attempt produced.
enum ConnectResult {
    /// Connected: the backend and the directory to open it on.
    Connected(std::sync::Arc<dyn crate::vfs::Vfs>, PathBuf),
    /// A credential is needed; carries the retry context and what to ask for.
    NeedsCredential(
        crate::vfs::sftp::SftpTarget,
        crate::vfs::sftp::Credentials,
        crate::vfs::sftp::AuthNeed,
    ),
    /// The attempt failed.
    Failed(String),
}

/// Connection details preserved across a credential prompt, so the retry keeps
/// the target and any credentials already gathered.
struct PendingConnect {
    target: crate::vfs::sftp::SftpTarget,
    creds: crate::vfs::sftp::Credentials,
    need: crate::vfs::sftp::AuthNeed,
    /// The panel the remote should open in — carried across credential prompts
    /// so a retry still lands in the pane the user started from.
    target_panel: usize,
}

impl FileBrowser {
    /// Build the app. `left`/`right` are the two panels' starting directories;
    /// dual mode is enabled by the `--dual` flag *or* by supplying a right path.
    pub fn new(left: Option<PathBuf>, right: Option<PathBuf>, dual: bool) -> Self {
        let mut panels = vec![Panel::new(left)];
        if dual || right.is_some() {
            panels.push(Panel::new(right));
        }

        Self {
            panels,
            active: 0,
            key_handler: KeyBindingHandler::new(),
            modal: Modal::None,
            modal_target: None,
            copy_task: None,
            copy_dest_panel: 0,
            op_progress: None,
            op_cancel: None,
            move_results: None,
            pending_moves: Vec::new(),
            approved_moves: Vec::new(),
            move_source_panel: 0,
            move_dest_panel: 0,
            move_dest_dir: PathBuf::new(),
            pending_copies: Vec::new(),
            approved_copies: Vec::new(),
            copy_source_panel: 0,
            backends: BackendRegistry::new(),
            transfers: TransferQueue::default(),
            transfer_panel_override: None,
            connect_task: None,
            pending_connect: None,
            hosts: HostCatalog::load(),
            connecting_label: None,
        }
    }

    /// The panel currently receiving navigation keys.
    fn active_panel(&self) -> &Panel {
        &self.panels[self.active]
    }

    fn active_panel_mut(&mut self) -> &mut Panel {
        &mut self.panels[self.active]
    }

    /// Index of the other panel in dual mode, or `None` in single-panel mode.
    fn other_index(&self) -> Option<usize> {
        if self.panels.len() == 2 {
            Some(1 - self.active)
        } else {
            None
        }
    }

    pub async fn run(&mut self) -> Result<()> {
        crossterm::terminal::enable_raw_mode()?;
        crossterm::execute!(
            std::io::stdout(),
            crossterm::terminal::EnterAlternateScreen,
            crossterm::event::DisableMouseCapture,
            crossterm::terminal::Clear(crossterm::terminal::ClearType::All),
            crossterm::cursor::Hide,
        )?;

        // Insert guard FIRST so any early return still cleans up.
        let _guard = TerminalGuard;

        let backend = ratatui::backend::CrosstermBackend::new(std::io::stdout());
        let mut terminal = ratatui::Terminal::new(backend)?;

        // Shared flag for Ctrl-C detection.
        let interrupted = Arc::new(AtomicBool::new(false));
        let ctrl_c_flag = interrupted.clone();

        // Spawn a task to listen for Ctrl-C and set the flag.
        tokio::spawn(async move {
            let _ = tokio::signal::ctrl_c().await;
            ctrl_c_flag.store(true, Ordering::SeqCst);
        });

        let mut running = true;

        while running {
            // Check for Ctrl-C.
            if interrupted.load(Ordering::SeqCst) {
                running = false;
            }

            if let Ok(true) = crossterm::event::poll(std::time::Duration::from_millis(100)) {
                if let Ok(Event::Key(key)) = crossterm::event::read() {
                    if key.kind == KeyEventKind::Press {
                        running = self.route_key(key);
                    }
                }
            }

            // Per-phase timing so a stall between keypresses can be attributed
            // to the resolve pass or the draw, not just observed. Off unless
            // MYD_TRACE is set.
            let tick_started = trace_enabled().then(std::time::Instant::now);

            // Check for completed loading tasks and replace with Main screens.
            // A cancelled scan in the active panel with nothing to fall back to
            // tells us to quit.
            if !self.resolve_loading() {
                running = false;
            }

            // Check for completed delete and copy tasks.
            self.resolve_deleting();
            self.resolve_copying();
            // Promote queued transfers, reap finished ones, and refresh the
            // destination of any that just completed. Non-blocking, so the UI
            // stays responsive while transfers run.
            self.advance_transfers();
            // Advance any in-flight connection attempt.
            self.resolve_connect();

            let after_resolve = trace_started_now(tick_started);
            terminal.draw(|f| self.draw(f))?;

            // Only log slow ticks: an idle loop runs at 10Hz and would otherwise
            // bury the interesting entries.
            if let (Some(start), Some(mid)) = (tick_started, after_resolve) {
                let total = start.elapsed();
                if total > std::time::Duration::from_millis(150) {
                    trace_note(format_args!(
                        "  tick: resolve={:.1}ms draw={:.1}ms",
                        mid.duration_since(start).as_secs_f64() * 1000.0,
                        mid.elapsed().as_secs_f64() * 1000.0,
                    ));
                }
            }
        }

        // Guard's Drop will handle cleanup, but explicitly clear the flag so
        // the guard doesn't double-restore if called before drop.
        Ok(())
    }

    /// Draw one frame: the transfer sidebar, the panel(s), then any modal.
    ///
    /// Shared by the event loop and `render_for_test`, so tests exercise the
    /// real layout instead of a copy that could drift from it.
    fn draw(&mut self, f: &mut ratatui::Frame) {
        let active = self.active;
        let panel_count = self.panels.len();
        let deleting = self.panels.iter().any(|p| p.is_deleting());
        let show_transfers = self.transfer_panel_visible();
        let full = f.area();

        // Carve the transfer sidebar off the right edge before the panels divide
        // what's left, so it spans full height and is independent of
        // single/dual mode. It yields entirely on a narrow terminal.
        let (area, transfer_area) = match show_transfers
            .then(|| transfer_panel::desired_width(full.width))
            .flatten()
        {
            Some(w) => {
                let cols =
                    Layout::horizontal([Constraint::Min(1), Constraint::Length(w)]).split(full);
                (cols[0], Some(cols[1]))
            }
            None => (full, None),
        };

        // Draw the sidebar first: it borrows the queue, while the panel loop
        // below needs `self.panels` mutably.
        if let Some(ta) = transfer_area {
            transfer_panel::render(f, ta, &self.transfers);
        }

        // In-progress transfer destinations, to overlay as ghost rows on the
        // panel(s) they land in.
        let pending = self.transfers.pending_destinations();

        if panel_count == 2 {
            let cols = Layout::horizontal([Constraint::Percentage(50), Constraint::Percentage(50)])
                .split(area);
            for (i, panel) in self.panels.iter_mut().enumerate() {
                let backend = panel.backend;
                // Tell the top Main screen whether it's the active panel so its
                // border can stand out.
                if let Screen::Main(state) = panel.current_screen_mut() {
                    state.active = i == active;
                    state.pending_ghosts = ghosts_for_panel(&pending, backend, state.root_path());
                }
                panel.current_screen_mut().render(f, cols[i]);
            }
        } else {
            let backend = self.panels[0].backend;
            if let Screen::Main(state) = self.panels[0].current_screen_mut() {
                state.active = true;
                state.pending_ghosts = ghosts_for_panel(&pending, backend, state.root_path());
            }
            self.panels[0].current_screen_mut().render(f, area);
        }

        // Modals center on the whole terminal, not the tree column, so toggling
        // the sidebar doesn't shift a dialog under the cursor.
        // Taken by value so the picker can render with `&mut self` (it keeps a
        // scroll offset), then put back.
        let op_progress = self.op_progress.clone();
        match &mut self.modal {
            Modal::Confirm(d) => d.render(f, full),
            Modal::Input(d) => d.render(f, full),
            Modal::HostPicker(p) => p.render(f, full),
            Modal::Operation { verb } => {
                let overlay = match &op_progress {
                    Some(p) => ProgressOverlay::for_operation(verb, p),
                    None => ProgressOverlay::new().with_message(*verb),
                };
                overlay.render(f, full);
            }
            Modal::Help => render_help(f, full),
            Modal::None => {
                // A delete that started from the confirm dialog also runs in the
                // background; show its overlay when no modal is up.
                if deleting {
                    let overlay = match &self.op_progress {
                        Some(p) => ProgressOverlay::for_operation("Deleting", p),
                        None => ProgressOverlay::new().with_message("Deleting"),
                    };
                    overlay.render(f, full);
                }
            }
        }
    }

    /// Pop the top screen of the active panel.
    fn pop_screen(&mut self) {
        self.active_panel_mut().pop_screen();
    }

    /// Resolve completed loading tasks across all panels. A cancelled scan that
    /// empties the *active* panel with no dual-panel sibling to fall back to
    /// returns `false`, signalling the app to quit. When the inactive panel
    /// empties in dual mode, drop the split rather than quitting.
    fn resolve_loading(&mut self) -> bool {
        let mut keep_running = true;
        for i in 0..self.panels.len() {
            if !self.panels[i].resolve_loading() {
                // This panel has nothing left to show.
                if self.panels.len() == 2 {
                    // Collapse to the surviving panel rather than quitting.
                    self.panels.remove(i);
                    self.active = 0;
                    return true;
                }
                if i == self.active {
                    keep_running = false;
                }
            }
        }
        keep_running
    }

    /// Resolve completed delete tasks across all panels (each removes its own
    /// deleted paths from its tree).
    fn resolve_deleting(&mut self) {
        let was_deleting = self.panels.iter().any(|p| p.is_deleting());

        // A finished move reports which sources actually left; hand those to the
        // panel so only they disappear from the tree.
        if let Some((moved, _)) = &self.move_results {
            let finished = self
                .panels
                .iter()
                .any(|p| p.delete_task.as_ref().is_some_and(|t| t.is_finished()));
            if finished {
                let gone = moved.lock().unwrap().clone();
                for panel in &mut self.panels {
                    if panel.delete_task.as_ref().is_some_and(|t| t.is_finished()) {
                        panel.deleting_paths = gone.clone();
                    }
                }
            }
        }

        for panel in &mut self.panels {
            panel.resolve_deleting();
        }
        // Once the last delete finished, drop its progress so the overlay clears.
        if was_deleting && !self.panels.iter().any(|p| p.is_deleting()) {
            self.op_progress = None;
            self.op_cancel = None;
            // A move runs on this same path, and it lands entries in the *other*
            // panel — re-list that directory so they appear. Harmless for a
            // plain delete, where the destination panel is the active one.
            let dest = self.copy_dest_panel;
            if let Some(panel) = self.panels.get_mut(dest) {
                if let Some(dir) = panel.current_dir() {
                    if let Screen::Main(state) = panel.current_screen_mut() {
                        state.reload_dir_public(&dir);
                    }
                }
            }
            // Dismiss the "Deleting"/"Moving" overlay.
            if matches!(self.modal, Modal::Operation { .. }) {
                self.modal = Modal::None;
            }
            // Report anything the move couldn't do — a silently skipped file
            // would look like it moved.
            if let Some((_, failures)) = self.move_results.take() {
                let failures = failures.lock().unwrap().clone();
                if !failures.is_empty() {
                    let detail = failures.join("; ");
                    self.modal = Modal::Confirm(ConfirmDialog::new(format!(
                        "Move failed: {}",
                        detail
                    )));
                }
            }
        }
    }

    /// Resolve a completed copy batch: clear the overlay, clear the source
    /// panel's tags (the copy consumed them), and refresh the destination panel
    /// so the copied entries appear.
    fn resolve_copying(&mut self) {
        if let Some(ref task) = self.copy_task {
            if task.is_finished() {
                self.copy_task.take();
                self.op_progress = None;
                self.op_cancel = None;
                self.modal = Modal::None;
                // Tags are staged input to the copy — clear them once it lands.
                if let Some(panel) = self.panels.get_mut(self.copy_source_panel) {
                    panel.current_screen_mut().clear_tags();
                }
                if let Some(panel) = self.panels.get_mut(self.copy_dest_panel) {
                    panel.current_screen_mut().refresh();
                }
            }
        }
    }

    /// Advance the transfer queue and apply targeted updates for any transfer
    /// that just completed.
    ///
    /// Rather than a full rescan, each completed destination triggers a
    /// single-level reload of its *parent* directory in whichever panel shows
    /// it — so a copied file (or directory) appears immediately and its ghost
    /// clears, touching only the affected directory level.
    fn advance_transfers(&mut self) {
        self.transfers.tick(&self.backends);
        let completed = self.transfers.take_completed_destinations();
        for dest in completed {
            self.apply_completed_transfer(&dest);
        }
    }

    /// Refresh the one directory level a completed transfer landed in.
    ///
    /// Reloads the destination's parent directory in every panel on the same
    /// backend whose tree already has that directory loaded — a `reload_dir`,
    /// which re-lists just that level and reuses the size cache, not a full
    /// `refresh`. If the directory isn't currently shown, there's nothing to
    /// update and this is a no-op.
    fn apply_completed_transfer(&mut self, dest: &crate::vfs::VPath) {
        let Some(parent) = dest.path.parent() else {
            return;
        };
        for panel in &mut self.panels {
            if panel.backend != dest.backend {
                continue;
            }
            if let Screen::Main(state) = panel.current_screen_mut() {
                // Only reload if this panel's tree actually contains the parent
                // directory (cheap check against the loaded lines), so unrelated
                // panels aren't disturbed.
                let has_parent = state.root_path() == parent
                    || state.tree.lines.iter().any(|l| l.path == parent);
                if has_parent {
                    state.reload_dir_public(parent);
                }
            }
        }
    }

    /// Drive one key through the app, exactly as the event loop does —
    /// including modal-aware routing (e.g. dismissing the help screen).
    /// Exposed so tests can exercise real key sequences end to end.
    pub fn handle_key_for_test(&mut self, key: KeyEvent) -> bool {
        self.route_key(key)
    }

    /// Whether the help screen is currently showing (for tests).
    pub fn is_help_open(&self) -> bool {
        matches!(self.modal, Modal::Help)
    }

    /// Whether the transfer sidebar should be drawn.
    ///
    /// With no explicit toggle, it appears only once the queue holds a transfer
    /// (and stays while any remain) — so it's absent at startup and until the
    /// first copy. A `Ctrl+t` toggle pins it open or closed regardless.
    fn transfer_panel_visible(&self) -> bool {
        self.transfer_panel_override
            .unwrap_or_else(|| !self.transfers.is_empty())
    }

    /// Whether the transfer sidebar is showing (for tests).
    pub fn is_transfer_panel_visible(&self) -> bool {
        self.transfer_panel_visible()
    }

    /// Read-only view of the transfer queue (for tests).
    pub fn transfer_queue(&self) -> &TransferQueue {
        &self.transfers
    }

    /// Queue a transfer through the app, as a copy action would (for tests).
    /// Mirrors the real copy path, which clears any hidden override so the panel
    /// surfaces the queue.
    pub fn enqueue_transfer_for_test(
        &mut self,
        src: crate::vfs::VPath,
        dest: crate::vfs::VPath,
    ) -> crate::transfer::TransferId {
        let id = self.transfers.enqueue(src, dest, 0);
        self.transfer_panel_override = None;
        id
    }

    /// Advance the transfer queue one scheduling step, applying targeted
    /// destination refreshes just like the event loop (for tests).
    pub fn tick_transfers_for_test(&mut self) {
        self.advance_transfers();
    }

    /// Render the whole app into `frame`, exactly as the event loop does — so
    /// tests exercise the real three-column layout rather than a reconstruction.
    pub fn render_for_test(&mut self, frame: &mut ratatui::Frame) {
        self.draw(frame);
    }

    /// Advance the background-task machinery one tick, as the event loop does
    /// (connection attempts, loading, transfers). For tests driving the remote
    /// connect + browse flow headlessly.
    pub fn tick_for_test(&mut self) {
        self.resolve_connect();
        self.resolve_loading();
        self.resolve_deleting();
        self.resolve_copying();
        self.advance_transfers();
    }

    /// Which modal is up, as a stable name (for tests).
    pub fn modal_kind_for_test(&self) -> &'static str {
        match &self.modal {
            Modal::None => "none",
            Modal::Confirm(_) => "confirm",
            Modal::Input(_) => "input",
            Modal::Operation { .. } => "operation",
            Modal::Help => "help",
            Modal::HostPicker(_) => "host_picker",
        }
    }

    /// The label of the host the picker has highlighted (for tests).
    pub fn picker_selection_for_test(&self) -> Option<String> {
        match &self.modal {
            Modal::HostPicker(p) => p.selected().map(|h| h.label.clone()),
            _ => None,
        }
    }

    /// How many hosts the picker is currently showing (for tests).
    pub fn picker_visible_count_for_test(&self) -> usize {
        match &self.modal {
            Modal::HostPicker(p) => p.visible_count(),
            _ => 0,
        }
    }

    /// Replace the host catalog, so tests don't touch the user's real one.
    pub fn set_hosts_for_test(&mut self, hosts: HostCatalog) {
        self.hosts = hosts;
    }

    /// The saved-host list (for tests).
    pub fn hosts_for_test(&self) -> &HostCatalog {
        &self.hosts
    }

    /// Whether a connection attempt is in flight (for tests).
    /// Whether a background copy/delete/move batch is still running.
    pub fn is_operation_running_for_test(&self) -> bool {
        self.panels.iter().any(|p| p.is_deleting()) || self.copy_task.is_some()
    }

    pub fn is_connecting_for_test(&self) -> bool {
        self.is_connecting()
    }

    /// The screen currently on top of the active panel's stack.
    pub fn current_screen(&self) -> &Screen {
        self.active_panel().current_screen()
    }

    /// Number of screens on the active panel's navigation stack (for tests).
    pub fn screen_stack_depth(&self) -> usize {
        self.active_panel().depth()
    }

    /// Mutable access to the active panel's top screen (for tests that render).
    pub fn current_screen_mut(&mut self) -> &mut Screen {
        self.active_panel_mut().current_screen_mut()
    }

    /// Number of panels (1 = single, 2 = dual). For tests.
    pub fn panel_count(&self) -> usize {
        self.panels.len()
    }

    /// Index of the active panel. For tests.
    pub fn active_panel_index(&self) -> usize {
        self.active
    }

    /// The directory a given panel is rooted at. For tests.
    pub fn panel_current_dir(&self, index: usize) -> Option<PathBuf> {
        self.panels.get(index).and_then(|p| p.current_dir())
    }

    /// Resolve any pending Loading screen into a Main screen (normally driven
    /// by the event loop each tick). Returns false if the app should now quit
    /// (a cancelled first-screen scan).
    pub fn resolve_loading_for_test(&mut self) -> bool {
        // Mirror the event loop's per-tick resolution so tests observe copy and
        // delete completion (tag clearing, dest refresh) the same way the app
        // does, not just the loading transitions.
        self.resolve_deleting();
        self.resolve_copying();
        self.resolve_loading()
    }

    /// Route a key press according to the current modal state. This is the top
    /// of the input pipeline — the event loop and tests both go through it, so
    /// modal-specific handling (like dismissing help) is exercised the same way.
    /// Returns whether the app should keep running.
    fn route_key(&mut self, key: KeyEvent) -> bool {
        // Ctrl-C is an unconditional exit, checked before any modal or screen
        // routing can swallow it — the guaranteed way out, whatever the app is
        // doing (a remote connect, a modal, a load). Background tasks are
        // abandoned; the terminal guard restores the screen on the way out.
        if key.code == KeyCode::Char('c')
            && key
                .modifiers
                .contains(crossterm::event::KeyModifiers::CONTROL)
        {
            self.abort_remote_work();
            return false;
        }

        match self.modal {
            Modal::None => self.handle_key(key),
            Modal::Help => {
                // Dismiss help. Keys whose only role here is to close the help
                // screen (quit/back and the help toggles) are consumed so they
                // don't also act on the screen behind it — e.g. q must not quit
                // the app. Any other key both dismisses help and acts (so j
                // moves the cursor).
                self.modal = Modal::None;
                let dismiss_only = matches!(
                    key.code,
                    KeyCode::Esc | KeyCode::Char('q') | KeyCode::Char('?') | KeyCode::F(1)
                );
                if dismiss_only {
                    true
                } else {
                    self.handle_key(key)
                }
            }
            _ => self.handle_modal_key(key),
        }
    }

    fn handle_key(&mut self, key: KeyEvent) -> bool {
        // Let the current screen handle raw keys first (e.g., dir picker input).
        if let Some(result) = self
            .active_panel_mut()
            .current_screen_mut()
            .handle_raw_key(key)
        {
            return result;
        }

        // Fall back to the global keybinding handler.
        let action = self.key_handler.handle(key);

        if let Some(action) = action {
            self.dispatch_action(action)
        } else {
            true
        }
    }

    fn dispatch_action(&mut self, action: Action) -> bool {
        // Opt-in timing for diagnosing slow keys on network filesystems. Set
        // MYD_TRACE=1 and the app appends one line per action to
        // ~/.cache/myd-trace.log, so a stall can be attributed to a specific
        // step rather than guessed at.
        let trace_started = trace_enabled().then(std::time::Instant::now);
        let result = self.dispatch_action_inner(action);
        if let Some(started) = trace_started {
            trace_action(action, started.elapsed());
        }
        result
    }

    fn dispatch_action_inner(&mut self, action: Action) -> bool {
        // Visual mode only survives motion and its own toggle; any other command
        // ends the range-tag gesture (tags already made are kept).
        if !matches!(
            action,
            Action::CursorUp | Action::CursorDown | Action::VisualMode
        ) {
            self.active_panel_mut().current_screen_mut().exit_visual();
        }

        match action {
            Action::Quit => {
                // While a directory is being scanned, q/Esc cancels that scan
                // rather than quitting outright. The background walk stops
                // promptly, and resolve_loading then returns to the previous
                // directory (or quits if the scan was the very first screen).
                let screen = self.active_panel().current_screen();
                if screen.is_loading() {
                    screen.cancel_loading();
                    return true;
                }
                // The directory picker is a prompt, not a place: `q` backs out
                // of it and returns to whatever was underneath, the same way Esc
                // dismisses any other prompt. Quitting the app from it was a
                // surprise, since `gd` was a question the user could decline.
                // With nothing underneath there is nothing to return to, so it
                // quits as before.
                if matches!(screen, Screen::DirPicker(_)) && self.active_panel().depth() > 1 {
                    self.pop_screen();
                    return true;
                }
                // Otherwise quit the app immediately, regardless of history.
                // Use Ctrl-o (PopScreen) to step back up a directory.
                false
            }
            Action::SwitchPanel => {
                // Tab moves focus to the other panel (no-op in single-panel mode).
                if let Some(other) = self.other_index() {
                    self.active = other;
                }
                true
            }
            Action::ToggleSplit => {
                self.toggle_split();
                true
            }
            Action::ToggleTransferPanel => {
                // Pin the panel to the opposite of what's currently shown. This
                // works whether the current visibility came from the auto rule
                // (queue non-empty) or a previous toggle.
                self.transfer_panel_override = Some(!self.transfer_panel_visible());
                true
            }
            Action::CancelTransfers => {
                self.transfers.cancel_all();
                true
            }
            Action::Copy => {
                self.start_copy();
                true
            }
            Action::Move => {
                self.start_move();
                true
            }
            Action::ToggleTag => self.active_panel_mut().current_screen_mut().toggle_tag(),
            Action::UntagAll => self.active_panel_mut().current_screen_mut().untag_all(),
            Action::VisualMode => self.active_panel_mut().current_screen_mut().toggle_visual(),
            Action::Filter => {
                self.modal_target = Some(ModalTarget::Filter);
                self.modal = Modal::Input(InputDialog::new(
                    "Filter (regex, empty to clear):",
                    "/pattern/",
                ));
                true
            }
            Action::CreateDir => {
                self.modal_target = Some(ModalTarget::CreateDir);
                self.modal = Modal::Input(InputDialog::new("New directory name:", "name"));
                true
            }
            Action::SearchNext => self.active_panel_mut().current_screen_mut().search_next(),
            Action::SearchPrev => self.active_panel_mut().current_screen_mut().search_prev(),
            Action::PopScreen => {
                if self.active_panel().depth() > 1 {
                    self.pop_screen();
                }
                true
            }
            Action::Help => {
                if matches!(self.modal, Modal::Help) {
                    self.modal = Modal::None;
                } else {
                    self.modal = Modal::Help;
                }
                true
            }
            Action::Confirm => {
                let panel = self.active_panel_mut();
                // Confirm dir picker selection → push loading screen.
                if let Screen::DirPicker(state) = panel.current_screen_mut() {
                    if let Some(path) = state.confirm() {
                        panel.screen_stack.push(Screen::loading(path));
                    }
                    return true;
                }
                // Enter on a directory in main screen → navigate into it.
                // Extract the path first to avoid double borrow. Use the tree
                // line's own `is_dir` (from the listing) rather than a local
                // `Path::is_dir()`, which is always false for a remote path.
                let target = if let Screen::Main(state) = panel.current_screen() {
                    let is_dir = state
                        .tree
                        .selected_line()
                        .map(|l| l.is_dir)
                        .unwrap_or(false);
                    if is_dir {
                        state.selected_path().cloned().map(|p| {
                            (
                                p,
                                state.tree.size_cache.clone(),
                                state.tree.source.clone(),
                                // Carry the order the user is looking at into the
                                // directory being opened.
                                state.tree.sort_mode,
                            )
                        })
                    } else {
                        None
                    }
                } else {
                    None
                };
                if let Some((path, cache, source, sort_mode)) = target {
                    // A remote directory loads through its backend on the
                    // blocking pool; expanding it in place would run the network
                    // round trips on the event-loop thread and freeze the UI.
                    if source.is_remote() {
                        panel.screen_stack.push(Screen::loading_remote_sorted(
                            source,
                            path,
                            Some(cache),
                            sort_mode,
                        ));
                    } else {
                        panel.screen_stack.push(Screen::loading_sorted(
                            path,
                            Some(cache),
                            sort_mode,
                        ));
                    }
                }
                true
            }
            Action::Connect => {
                // The dialing directory replaces what used to be a bare text
                // prompt. With nothing saved yet it goes straight to that
                // prompt, so a first run costs no extra keystroke.
                if self.hosts.is_empty() {
                    self.prompt_manual_connect();
                } else {
                    self.modal = Modal::HostPicker(HostPicker::quick(&self.hosts));
                }
                true
            }
            Action::HostDirectory => {
                self.modal = Modal::HostPicker(HostPicker::full(&self.hosts));
                true
            }
            Action::ChangeRoot => {
                self.modal_target = Some(ModalTarget::ChangeRoot);
                self.modal =
                    Modal::Input(InputDialog::new("Change root directory:", "Enter path..."));
                true
            }
            Action::Search => {
                self.modal_target = Some(ModalTarget::Search);
                self.modal = Modal::Input(InputDialog::new("Search (regex):", "pattern"));
                true
            }
            Action::Delete => {
                // Tagged files are the operation set; fall back to the cursor
                // selection when nothing is tagged. Extract before touching the
                // modal fields so the panel borrow ends first.
                let mut targets = self.active_panel().current_screen().tagged_paths();
                if targets.is_empty() {
                    if let Screen::Main(state) = self.active_panel().current_screen() {
                        if let Some(p) = state.selected_resolved_path() {
                            targets.push(p.clone());
                        }
                    }
                }
                if !targets.is_empty() {
                    let prompt = if targets.len() == 1 {
                        let name = targets[0]
                            .file_name()
                            .map(|s| s.to_string_lossy().to_string())
                            .unwrap_or_default();
                        format!("Delete '{}'?", name)
                    } else {
                        format!("Delete {} tagged items?", targets.len())
                    };
                    self.modal_target = Some(ModalTarget::Delete { paths: targets });
                    self.modal = Modal::Confirm(ConfirmDialog::new(prompt));
                }
                true
            }
            Action::Rename => {
                let target = match self.active_panel().current_screen() {
                    Screen::Main(state) => state.selected_path().cloned(),
                    _ => None,
                };
                if let Some(line) = target {
                    let name = line
                        .file_name()
                        .map(|s| s.to_string_lossy().to_string())
                        .unwrap_or_default();
                    self.modal_target = Some(ModalTarget::Rename { old_path: line });
                    self.modal = Modal::Input(
                        InputDialog::new(format!("Rename '{}':", name), "").with_default(name),
                    );
                }
                true
            }
            Action::Collapse => {
                // Capture stack state before mutable borrow.
                let panel = self.active_panel_mut();
                let stack_len = panel.screen_stack.len();
                let dir_picker_below = matches!(
                    panel.screen_stack.get(stack_len.saturating_sub(2)),
                    Some(Screen::DirPicker(_))
                );
                let current = panel.current_screen_mut();

                if let Screen::Main(state) = current {
                    // In the treemap, `h` slides the cursor left; on a left-edge
                    // tile (nowhere further left) it steps up to the parent
                    // directory instead, mirroring how tree `h` moves toward the
                    // parent and pops up when it can go no further.
                    if state.focus == FocusTarget::Treemap {
                        if state.treemap_can_move_left() {
                            return state.collapse();
                        }
                        if stack_len > 1 && !dir_picker_below {
                            self.pop_screen();
                        }
                        return true;
                    }

                    // Tree view. The root line is auto-expanded, so a plain
                    // in-place collapse here would eat the first `h` after
                    // entering a directory — the user expects `h` at the root to
                    // step back up. So when the cursor is on the root (depth 0)
                    // and there's a screen to return to, fall through to the pop
                    // below instead of collapsing the root.
                    let at_root_line = state
                        .tree
                        .selected_line()
                        .map(|l| l.depth == 0)
                        .unwrap_or(false);
                    let can_pop = stack_len > 1 && !dir_picker_below;
                    if !(at_root_line && can_pop) {
                        // Otherwise: on an expanded directory, collapse in place.
                        let is_expanded = state.tree.is_cursor_expanded();
                        let is_dir = state
                            .tree
                            .selected_line()
                            .map(|l| l.is_dir)
                            .unwrap_or(false);
                        if is_dir && is_expanded {
                            return state.collapse();
                        }
                    }
                }

                // Drop mutable borrow before accessing the stack again.
                let at_root = { current.selected_line_depth().unwrap_or(0) == 0 };

                if at_root && stack_len > 1 && !dir_picker_below {
                    self.pop_screen();
                    return true;
                }

                // Move cursor to parent line.
                self.active_panel_mut().current_screen_mut().go_parent()
            }
            Action::GoDirPicker => {
                let panel = self.active_panel_mut();
                // Check if DirPicker already exists in the stack.
                let has_dir_picker = panel
                    .screen_stack
                    .iter()
                    .any(|s| matches!(s, Screen::DirPicker(_)));
                if has_dir_picker {
                    // Pop screens until the DirPicker is on top.
                    while panel.screen_stack.len() > 1
                        && !matches!(panel.screen_stack.last(), Some(Screen::DirPicker(_)))
                    {
                        panel.screen_stack.pop();
                    }
                } else {
                    // Push rather than replace, so the picker sits *over* the
                    // current view. Replacing discarded the tree underneath,
                    // which left `q` nothing to return to — declining `gd` then
                    // had to quit the app.
                    panel.screen_stack.push(Screen::dir_picker());
                }
                true
            }
            Action::Expand => {
                // On a remote panel, expanding a directory in place would run
                // the SFTP round trips on the event-loop thread and lock the UI.
                // Route it through the async loading screen instead, exactly as
                // Enter does. Local panels keep the cheap in-place expand.
                let remote_dir = if let Screen::Main(state) = self.active_panel().current_screen() {
                    if state.tree.source.is_remote() {
                        let is_dir = state
                            .tree
                            .selected_line()
                            .map(|l| l.is_dir)
                            .unwrap_or(false);
                        if is_dir {
                            state.selected_path().cloned().map(|p| {
                                (p, state.tree.size_cache.clone(), state.tree.source.clone())
                            })
                        } else {
                            None
                        }
                    } else {
                        None
                    }
                } else {
                    None
                };
                match remote_dir {
                    Some((path, cache, source)) => {
                        self.active_panel_mut()
                            .screen_stack
                            .push(Screen::loading_remote(source, path, Some(cache)));
                    }
                    None => {
                        self.active_panel_mut().current_screen_mut().expand();
                    }
                }
                true
            }
            // Refresh and expand-all re-list directories from scratch. On a
            // remote panel that means SFTP round trips that would block the event
            // loop in place (and trap the user); refresh would also wrongly
            // rebuild the remote tree as a *local* one. So for remote panels these
            // rebuild through the async loading screen.
            //
            // Sort and hidden toggles do NOT re-list: sort reorders the nodes
            // already in memory, and a remote tree already has every entry loaded
            // so hiding is a pure reflatten. Both are handled below in place.
            Action::Refresh | Action::ExpandAll if self.active_panel_is_remote() => {
                self.remote_rebuild(matches!(action, Action::Refresh));
                true
            }
            _ => {
                let current = self.active_panel_mut().current_screen_mut();
                match action {
                    Action::CursorDown => current.cursor_down(),
                    Action::CursorUp => current.cursor_up(),
                    Action::Expand => unreachable!("handled above"),
                    Action::ToTop => current.to_top(),
                    Action::ToBottom => current.to_bottom(),
                    Action::PageDown => current.page_down(),
                    Action::PageUp => current.page_up(),
                    Action::GoParent => current.go_parent(),
                    Action::Refresh => current.refresh(),
                    Action::ToggleSort => {
                        let result = current.toggle_sort();
                        // Remember the order for screens opened later, so it
                        // survives navigation the same way the view and info
                        // panel do.
                        let panel = self.active_panel_mut();
                        if let Screen::Main(state) = panel.current_screen() {
                            panel.view_prefs.sort_mode = state.tree.sort_mode;
                        }
                        result
                    }
                    Action::ToggleHidden => current.toggle_hidden(),
                    Action::ToggleBar => current.toggle_bar(),
                    Action::CollapseAll => current.collapse_all(),
                    Action::ExpandAll => current.expand_all(),
                    Action::ToggleInfoPanel => {
                        let panel = self.active_panel_mut();
                        if let Screen::Main(state) = panel.current_screen_mut() {
                            state.info_panel_hidden = !state.info_panel_hidden;
                            // Remember it for screens opened later this session.
                            panel.view_prefs.info_panel_hidden = state.info_panel_hidden;
                        }
                        true
                    }
                    Action::ToggleView => {
                        let panel = self.active_panel_mut();
                        let result = panel.current_screen_mut().toggle_view();
                        if let Screen::Main(state) = panel.current_screen() {
                            panel.view_prefs.focus = state.focus;
                        }
                        result
                    }
                    Action::Quit
                    | Action::PopScreen
                    | Action::Help
                    | Action::Confirm
                    | Action::ChangeRoot
                    | Action::Search
                    | Action::Collapse
                    | Action::GoDirPicker
                    | Action::SwitchPanel
                    | Action::ToggleSplit
                    | Action::Copy
                    | Action::Move
                    | Action::Delete
                    | Action::Rename
                    | Action::ToggleTag
                    | Action::UntagAll
                    | Action::VisualMode
                    | Action::Filter
                    | Action::CreateDir
                    | Action::SearchNext
                    | Action::SearchPrev
                    | Action::ToggleTransferPanel
                    | Action::CancelTransfers
                    | Action::Connect
                    | Action::HostDirectory => unreachable!(),
                    Action::None => true,
                }
            }
        }
    }

    fn handle_modal_key(&mut self, key: KeyEvent) -> bool {
        // The picker owns its keys outright — vi navigation and `/` search need
        // j/k// to be local commands, and going through the chord detector would
        // put its 500 ms timeout in front of every keystroke.
        if let Modal::HostPicker(picker) = &mut self.modal {
            let outcome = picker.handle_key(key);
            return self.apply_picker_outcome(outcome);
        }

        match &mut self.modal {
            Modal::Confirm(dialog) => {
                use crate::widget::confirm_dialog::Answer;
                // Esc backs out of a multi-choice prompt as "cancel the batch",
                // the only reading that can't lose data.
                let answer = if key.code == KeyCode::Esc {
                    Some(Answer::Choice('c'))
                } else {
                    dialog.handle_key_answer(key_code_char(&key))
                };
                if let Some(answer) = answer {
                    let result = answer == Answer::Yes;
                    self.modal = Modal::None;
                    match self.modal_target.take() {
                        Some(ModalTarget::Delete { paths }) if result => {
                            self.spawn_delete_batch(paths);
                        }
                        Some(ModalTarget::HostDelete { label }) => {
                            if result {
                                self.hosts.remove(&label);
                                if let Err(e) = self.hosts.save() {
                                    self.modal = Modal::Confirm(ConfirmDialog::new(format!(
                                        "Could not save the host list: {}",
                                        e
                                    )));
                                    return true;
                                }
                            }
                            // Back to the list either way, so a mis-keyed delete
                            // doesn't also close the picker.
                            self.reopen_picker();
                        }
                        Some(ModalTarget::CopyOverwrite { src, dest }) => {
                            // Confirmed collisions join the approved batch; a
                            // declined one is simply skipped. Either way we move
                            // on to the next pending collision (or spawn).
                            if result {
                                self.approved_copies.push((src, dest));
                            }
                            self.prompt_next_copy();
                        }
                        Some(ModalTarget::MoveOverwrite { src, dest, is_dir }) => {
                            match answer {
                                // Overwrite: the batch clears the destination
                                // just before renaming into it.
                                Answer::Choice('o') => {
                                    self.approved_moves.push((src, dest, is_dir));
                                    self.prompt_next_move();
                                }
                                // Skip this one, keep going.
                                Answer::Choice('s') => self.prompt_next_move(),
                                // Cancel: abandon everything still pending *and*
                                // everything already approved — the user asked to
                                // stop the move, not just this file.
                                _ => {
                                    self.pending_moves.clear();
                                    self.approved_moves.clear();
                                    self.modal = Modal::None;
                                }
                            }
                        }
                        _ => {}
                    }
                }
                true
            }
            Modal::Input(dialog) => {
                let result = dialog.handle_key(key);
                if let Some(value) = result {
                    self.modal = Modal::None;
                    if let Some(target) = self.modal_target.take() {
                        match target {
                            ModalTarget::Rename { old_path } => {
                                if !value.is_empty() {
                                    if let Some(msg) = self.rename_path(&old_path, &value) {
                                        self.modal = Modal::Confirm(ConfirmDialog::new(msg));
                                    }
                                }
                            }
                            ModalTarget::ChangeRoot => {
                                if !value.is_empty() {
                                    let path = PathBuf::from(&value)
                                        .expand_user()
                                        .canonicalize()
                                        .unwrap_or(PathBuf::from(&value));
                                    if path.is_dir() {
                                        self.active_panel_mut()
                                            .screen_stack
                                            .push(Screen::loading(path));
                                    }
                                }
                            }
                            ModalTarget::Search => {
                                if !value.is_empty() {
                                    self.active_panel_mut().current_screen_mut().search(&value);
                                }
                            }
                            ModalTarget::Filter => {
                                // Empty pattern clears the filter (handled downstream).
                                self.active_panel_mut().current_screen_mut().filter(&value);
                            }
                            ModalTarget::CreateDir => {
                                let failure = self
                                    .active_panel_mut()
                                    .current_screen_mut()
                                    .create_dir(&value);
                                if let Some(msg) = failure {
                                    self.modal = Modal::Confirm(ConfirmDialog::new(msg));
                                }
                            }
                            ModalTarget::CopyDest { srcs } => {
                                let dir = PathBuf::from(&value)
                                    .expand_user()
                                    .canonicalize()
                                    .unwrap_or_else(|_| PathBuf::from(&value));
                                if dir.is_dir() {
                                    // Copy into the chosen directory, refreshing
                                    // the active panel (single-panel mode).
                                    let active = self.active;
                                    self.begin_copy_batch(srcs, dir, active, active);
                                }
                            }
                            ModalTarget::Connect => {
                                if !value.is_empty() {
                                    // A typed target isn't a saved one, so
                                    // nothing gets promoted in the ranking.
                                    self.connecting_label = None;
                                    self.start_connect(&value);
                                }
                            }
                            ModalTarget::HostForm { editing } => {
                                self.submit_host_form(&value, editing);
                            }
                            ModalTarget::HostDelete { .. } => {}
                            ModalTarget::Password => {
                                if value.is_empty() {
                                    // An empty entry cancels the whole attempt.
                                    self.pending_connect = None;
                                } else {
                                    self.retry_connect_with_secret(value);
                                }
                            }
                            // Confirm-modal targets; unreachable from an input.
                            ModalTarget::Delete { .. }
                            | ModalTarget::CopyOverwrite { .. }
                            | ModalTarget::MoveOverwrite { .. } => {}
                        }
                    }
                }
                true
            }
            Modal::Operation { .. } => {
                // q/Esc abandons whatever this overlay is covering, so a slow
                // host can never trap the user behind it: a hanging connection
                // attempt, or a delete/copy that is one round trip per entry
                // against a distant server.
                if matches!(key.code, KeyCode::Char('q') | KeyCode::Esc) {
                    if self.is_connecting() {
                        self.cancel_connect();
                    } else if let Some(cancel) = self.op_cancel.take() {
                        cancel.cancel();
                    }
                }
                true
            }
            Modal::Help => {
                // Dismiss help — the real key is handled by handle_key.
                self.modal = Modal::None;
                true
            }
            // Handled by the early return at the top of this function, which
            // needs `&mut self` for the whole call and so can't sit in this match.
            Modal::HostPicker(_) => true,
            Modal::None => true,
        }
    }

    /// Kick off a connection at startup from a CLI `sftp://` argument. The
    /// connect runs in the background once the event loop starts.
    pub fn connect_on_start(&mut self, target: &str) {
        self.start_connect(target);
    }

    /// The free-text connect prompt, for a target that isn't saved.
    fn prompt_manual_connect(&mut self) {
        self.modal_target = Some(ModalTarget::Connect);
        self.modal = Modal::Input(InputDialog::new(
            "Connect to (sftp://[user@]host[:port][/path]):",
            "sftp://host/path",
        ));
    }

    /// Act on what the dialing directory decided.
    fn apply_picker_outcome(&mut self, outcome: PickerOutcome) -> bool {
        match outcome {
            PickerOutcome::Continue => {}
            PickerOutcome::Cancelled => self.modal = Modal::None,
            PickerOutcome::Connect(url) => {
                // Remember which entry this was so the ranking is updated only
                // if the connection actually succeeds.
                self.connecting_label = self
                    .hosts
                    .hosts()
                    .iter()
                    .find(|h| h.to_url() == url)
                    .map(|h| h.label.clone());
                self.modal = Modal::None;
                self.start_connect(&url);
            }
            PickerOutcome::PromptManual => self.prompt_manual_connect(),
            PickerOutcome::AddHost => {
                self.modal_target = Some(ModalTarget::HostForm { editing: None });
                self.modal = Modal::Input(
                    InputDialog::new(HOST_FORM_PROMPT, "prod = sftp://juan@host:22/srv")
                        .with_title("Add a saved host"),
                );
            }
            PickerOutcome::EditHost(label) => {
                let existing = self.hosts.find(&label).cloned();
                let prefill = existing
                    .as_ref()
                    .map(|h| format!("{} = {}", h.label, h.to_url()))
                    .unwrap_or_default();
                self.modal_target = Some(ModalTarget::HostForm {
                    editing: Some(label),
                });
                self.modal = Modal::Input(
                    InputDialog::new(HOST_FORM_PROMPT, "")
                        .with_title("Edit saved host")
                        .with_default(prefill),
                );
            }
            PickerOutcome::DeleteHost(label) => {
                self.modal_target = Some(ModalTarget::HostDelete {
                    label: label.clone(),
                });
                self.modal = Modal::Confirm(ConfirmDialog::new(format!(
                    "Remove saved host '{}'? (the remote itself is untouched)",
                    label
                )));
            }
        }
        true
    }

    /// Apply a submitted add/edit form.
    ///
    /// The form is one line — `label = url` — rather than a multi-field wizard:
    /// `InputDialog` handles a single field, and the entry is short enough that
    /// splitting it across prompts would be more ceremony than it saves.
    fn submit_host_form(&mut self, value: &str, editing: Option<String>) {
        let value = value.trim();
        if value.is_empty() {
            self.reopen_picker();
            return;
        }

        let (label, url) = match value.split_once('=') {
            Some((l, u)) => (l.trim().to_string(), u.trim().to_string()),
            // No label given: name it after the host.
            None => (String::new(), value.to_string()),
        };

        match SavedHost::from_url(&label, &url) {
            Ok(mut host) => {
                if host.label.is_empty() {
                    host.label = host.host.clone();
                }
                // A rename replaces the old entry rather than leaving both.
                if let Some(old) = editing {
                    if old != host.label {
                        self.hosts.remove(&old);
                    }
                }
                self.hosts.upsert(host);
                if let Err(e) = self.hosts.save() {
                    self.modal_target = None;
                    self.modal = Modal::Confirm(ConfirmDialog::new(format!(
                        "Could not save the host list: {}",
                        e
                    )));
                    return;
                }
                self.reopen_picker();
            }
            Err(e) => {
                self.modal_target = None;
                self.modal = Modal::Confirm(ConfirmDialog::new(format!(
                    "Could not parse '{}': {}\n\nExpected: label = sftp://[user@]host[:port][/path]",
                    url, e
                )));
            }
        }
    }

    /// Return to the dialing directory after a management action.
    fn reopen_picker(&mut self) {
        self.modal_target = None;
        self.modal = if self.hosts.is_empty() {
            Modal::HostPicker(HostPicker::quick(&self.hosts))
        } else {
            Modal::HostPicker(HostPicker::full(&self.hosts))
        };
    }

    /// Begin connecting to a remote host named by `target` (e.g.
    /// `sftp://user@host/path` or an ssh config alias).
    ///
    /// Parses the target and kicks off the async connection; `resolve_connect`
    /// finishes the job when the attempt completes.
    fn start_connect(&mut self, target: &str) {
        // Remember which panel is active now — the remote opens here on success,
        // so `gr` takes over the pane the user was looking at.
        let target_panel = self.active;
        match crate::vfs::sftp::SftpTarget::parse(target) {
            Ok(t) => {
                self.spawn_connect(t, crate::vfs::sftp::Credentials::default(), target_panel)
            }
            Err(e) => {
                self.modal = Modal::Confirm(ConfirmDialog::new(format!(
                    "Invalid remote target: {}",
                    e
                )));
            }
        }
    }

    /// Spawn the connection attempt in the background so the UI stays responsive
    /// while the handshake and authentication happen. `target_panel` is the pane
    /// the remote will open in once connected.
    fn spawn_connect(
        &mut self,
        target: crate::vfs::sftp::SftpTarget,
        creds: crate::vfs::sftp::Credentials,
        target_panel: usize,
    ) {
        use crate::vfs::sftp::{ConnectOutcome, SftpFs};
        use std::sync::Arc;

        let (tx, rx) = tokio::sync::oneshot::channel();
        tokio::spawn(async move {
            // A host key that isn't recorded is accepted here; a proper prompt
            // is a later refinement. A *changed* key is still refused inside
            // connect().
            let result = match SftpFs::connect(&target, &creds, true).await {
                Ok(ConnectOutcome::Connected(fs)) => {
                    let home = fs.home().to_path_buf();
                    ConnectResult::Connected(Arc::new(fs), home)
                }
                Ok(ConnectOutcome::NeedsCredential(need)) => {
                    ConnectResult::NeedsCredential(target, creds, need)
                }
                Err(e) => ConnectResult::Failed(e.to_string()),
            };
            let _ = tx.send(result);
        });

        self.connect_task = Some(ConnectTask { rx, target_panel });
        self.modal = Modal::Operation { verb: "Connecting" };
    }

    /// Whether the active panel is showing a remote (non-local) tree.
    fn active_panel_is_remote(&self) -> bool {
        matches!(
            self.active_panel().current_screen(),
            Screen::Main(state) if state.tree.source.is_remote()
        )
    }

    /// Rebuild the active remote panel's current directory through the async
    /// loading screen, so the SFTP round trips run off the event-loop thread.
    /// `clear_cache` forces a full re-list (refresh); otherwise cached sizes are
    /// reused. A no-op if the panel isn't a remote Main screen.
    fn remote_rebuild(&mut self, clear_cache: bool) {
        let panel = self.active_panel_mut();
        let (path, cache, source) = match panel.current_screen() {
            Screen::Main(state) => (
                state.root_path().clone(),
                state.tree.size_cache.clone(),
                state.tree.source.clone(),
            ),
            _ => return,
        };
        if clear_cache {
            cache.clear();
        }
        // Replace the current screen with a fresh async load of the same
        // directory rather than pushing, so refresh doesn't deepen the stack.
        *panel.current_screen_mut() = Screen::loading_remote(source, path, Some(cache));
    }

    /// Whether a connection attempt is in progress (its overlay is up).
    fn is_connecting(&self) -> bool {
        self.connect_task.is_some()
    }

    /// Abandon an in-flight connection attempt and return to browsing.
    ///
    /// Dropping the task's receiver detaches it; the background connect task
    /// runs to completion on its own and its result is discarded. Cheaper and
    /// simpler than trying to interrupt a russh handshake mid-flight.
    fn cancel_connect(&mut self) {
        self.connect_task = None;
        self.pending_connect = None;
        self.modal = Modal::None;
    }

    /// Everything needed to bail out of remote work immediately, for Ctrl-C:
    /// drop any connection attempt and signal every in-flight scan to stop.
    fn abort_remote_work(&mut self) {
        self.connect_task = None;
        self.pending_connect = None;
        for panel in &self.panels {
            let screen = panel.current_screen();
            if screen.is_loading() {
                screen.cancel_loading();
            }
        }
        self.transfers.cancel_all();
    }

    /// Poll the in-flight connection. On success, register the backend and open
    /// a remote panel; on a credential request, prompt; on failure, report it.
    fn resolve_connect(&mut self) {
        let Some(task) = self.connect_task.as_mut() else {
            return;
        };
        let (result, target_panel) = match task.rx.try_recv() {
            Ok(r) => (r, task.target_panel),
            // Still connecting, or the task vanished.
            Err(tokio::sync::oneshot::error::TryRecvError::Empty) => return,
            Err(tokio::sync::oneshot::error::TryRecvError::Closed) => {
                self.connect_task = None;
                self.modal = Modal::None;
                return;
            }
        };
        self.connect_task = None;

        match result {
            ConnectResult::Connected(vfs, home) => {
                self.modal = Modal::None;
                self.pending_connect = None;
                let backend = self.backends.register(vfs.clone());
                let source = match crate::widget::source::RemoteSource::new(backend, vfs) {
                    Ok(s) => crate::widget::source::Source::Remote(s),
                    Err(e) => {
                        self.modal = Modal::Confirm(ConfirmDialog::new(format!(
                            "Could not start remote browser: {}",
                            e
                        )));
                        return;
                    }
                };
                // Open the remote in the panel that was active when `gr` was
                // issued, replacing whatever it was showing. Guard the index in
                // case the panel layout changed while connecting.
                let panel = target_panel.min(self.panels.len().saturating_sub(1));
                self.panels[panel] = Panel::new_remote(source, home);
                self.active = panel;

                // Promote the saved host now that the connection actually
                // worked. Doing it when the picker was dismissed would let a
                // host that never connects climb the recent list.
                if let Some(label) = self.connecting_label.take() {
                    self.hosts.record_use(&label);
                    if let Err(e) = self.hosts.save() {
                        tracing::warn!(error = %e, "could not persist host usage");
                    }
                }
            }
            ConnectResult::NeedsCredential(target, creds, need) => {
                self.prompt_for_credential(target, creds, need, target_panel);
            }
            ConnectResult::Failed(msg) => {
                self.modal = Modal::Confirm(ConfirmDialog::new(format!(
                    "Connection failed: {}",
                    msg
                )));
                self.pending_connect = None;
                // A failed attempt must not promote the host in the ranking.
                self.connecting_label = None;
            }
        }
    }

    /// Put up a masked prompt for whatever credential the connection needs, and
    /// remember the retry context.
    fn prompt_for_credential(
        &mut self,
        target: crate::vfs::sftp::SftpTarget,
        creds: crate::vfs::sftp::Credentials,
        need: crate::vfs::sftp::AuthNeed,
        target_panel: usize,
    ) {
        let prompt = credential_prompt(&need);
        self.pending_connect = Some(PendingConnect {
            target,
            creds,
            need,
            target_panel,
        });
        self.modal_target = Some(ModalTarget::Password);
        self.modal = Modal::Input(InputDialog::new(prompt, "").masked());
    }

    /// Retry a connection with a freshly entered credential.
    fn retry_connect_with_secret(&mut self, secret: String) {
        let Some(pending) = self.pending_connect.take() else {
            return;
        };
        use crate::vfs::sftp::AuthNeed;
        let mut creds = pending.creds;
        match pending.need {
            AuthNeed::Passphrase { .. } => creds.passphrase = Some(secret),
            AuthNeed::Password { .. } => creds.password = Some(secret),
        }
        self.spawn_connect(pending.target, creds, pending.target_panel);
    }

    /// Toggle between single and dual panel layouts.
    ///
    /// Splitting opens a second panel at the active panel's current directory so
    /// it is immediately useful (falling back to a dir picker if the active
    /// panel isn't rooted at a directory yet). Unsplitting drops the inactive
    /// panel and keeps the active one.
    fn toggle_split(&mut self) {
        if self.panels.len() == 2 {
            // Collapse to the active panel.
            let keep = self.panels.remove(self.active);
            self.panels.clear();
            self.panels.push(keep);
            self.active = 0;
        } else {
            // The active panel already holds a fully built tree for this
            // directory. Clone it rather than re-listing: a rescan showed a
            // loading screen for something already in memory, and on a remote or
            // network-mounted directory that meant a real wait. Cloning also
            // keeps a remote split remote — rebuilding went through the local
            // filesystem regardless of the backend.
            let cloned = match self.active_panel().current_screen() {
                Screen::Main(state) => Some((
                    state.root_path().clone(),
                    state.tree.clone(),
                    self.active_panel().backend,
                )),
                _ => None,
            };
            match cloned {
                Some((path, tree, backend)) => {
                    let mut panel = Panel::from_tree(path, tree);
                    panel.backend = backend;
                    self.panels.push(panel);
                }
                // Not showing a tree yet (a picker or still loading) — fall back
                // to opening the directory the normal way.
                None => {
                    let start = self.active_panel().current_dir();
                    let cache = self.active_panel().size_cache();
                    self.panels.push(Panel::new_with_cache(start, cache));
                }
            }
            // Focus the freshly opened panel.
            self.active = 1;
        }
    }

    /// Copy the active panel's tagged files (or the single cursor selection when
    /// nothing is tagged). In dual mode the destination is the other panel's
    /// directory; in single-panel mode the user is prompted for one.
    fn start_copy(&mut self) {
        // The tag set is the source of truth; fall back to the cursor selection.
        let mut srcs = self.active_panel().current_screen().tagged_paths();
        if srcs.is_empty() {
            if let Some(p) = self.active_panel().selected_resolved_path() {
                srcs.push(p);
            }
        }
        if srcs.is_empty() {
            return;
        }

        match self.other_index() {
            Some(other) => {
                // Dual mode: copy into the other panel's current directory.
                let Some(dest_dir) = self.panels[other].current_dir() else {
                    return;
                };
                let source = self.active;

                // If either endpoint is remote, this is a transfer: it goes
                // through the non-blocking queue and the transfer panel, not the
                // modal copy overlay.
                let src_backend = self.panels[source].backend;
                let dest_backend = self.panels[other].backend;
                if !src_backend.is_local() || !dest_backend.is_local() {
                    // Look up each source's directory-ness from the source panel's
                    // tree (no I/O), so the destination ghost draws the right icon.
                    let kinds: Vec<bool> = if let Screen::Main(state) =
                        self.panels[source].current_screen()
                    {
                        srcs.iter()
                            .map(|p| state.is_dir_of(p).unwrap_or(false))
                            .collect()
                    } else {
                        vec![false; srcs.len()]
                    };
                    self.enqueue_cross_backend_copy(
                        srcs,
                        kinds,
                        src_backend,
                        dest_dir,
                        dest_backend,
                        source,
                    );
                    return;
                }

                self.begin_copy_batch(srcs, dest_dir, other, source);
            }
            None => {
                // Single-panel: prompt for a destination directory first.
                self.modal_target = Some(ModalTarget::CopyDest { srcs });
                self.modal = Modal::Input(InputDialog::new("Copy to directory:", "Enter path..."));
            }
        }
    }

    /// Queue a copy where at least one side is remote. Each source becomes one
    /// transfer on the queue; directories are enqueued as a whole and expanded
    /// by the worker. The source panel's tags are cleared since they were the
    /// operation's input.
    fn enqueue_cross_backend_copy(
        &mut self,
        srcs: Vec<PathBuf>,
        kinds: Vec<bool>,
        src_backend: crate::vfs::BackendId,
        dest_dir: PathBuf,
        dest_backend: crate::vfs::BackendId,
        source_panel: usize,
    ) {
        use crate::vfs::VPath;
        for (i, src) in srcs.into_iter().enumerate() {
            let Some(name) = src.file_name().map(|n| n.to_owned()) else {
                continue;
            };
            let is_dir = kinds.get(i).copied().unwrap_or(false);
            let src_vpath = VPath::new(src_backend, src);
            let dest_vpath = VPath::new(dest_backend, dest_dir.join(name));
            // Total is unknown here; the worker re-stats the source and fills it
            // in, so the panel shows a real percentage once it starts.
            self.transfers.enqueue_kind(src_vpath, dest_vpath, 0, is_dir);
        }
        // Tags were the operation's input; clear them. The panel now reveals
        // itself automatically (the queue is non-empty) — drop any prior "hidden"
        // override so a copy always surfaces the queue the user just created.
        self.panels[source_panel].current_screen_mut().clear_tags();
        self.transfer_panel_override = None;
    }

    /// Plan a copy of `srcs` into `dest_dir`: non-colliding files are approved
    /// immediately, colliding ones are queued for a per-file overwrite prompt.
    /// `dest_panel` is refreshed and `source_panel`'s tags cleared on completion.
    fn begin_copy_batch(
        &mut self,
        srcs: Vec<PathBuf>,
        dest_dir: PathBuf,
        dest_panel: usize,
        source_panel: usize,
    ) {
        self.copy_dest_panel = dest_panel;
        self.copy_source_panel = source_panel;
        self.approved_copies.clear();
        self.pending_copies.clear();

        for src in srcs {
            let Some(name) = src.file_name() else {
                continue;
            };
            let dest = dest_dir.join(name);
            // Never copy a file onto itself.
            if src == dest {
                continue;
            }
            if dest.exists() {
                self.pending_copies.push((src, dest));
            } else {
                self.approved_copies.push((src, dest));
            }
        }

        self.prompt_next_copy();
    }

    /// Drain the next colliding file into an overwrite prompt; once none remain,
    /// spawn the approved batch (or clear state if nothing was approved).
    fn prompt_next_copy(&mut self) {
        if let Some((src, dest)) = self.pending_copies.pop() {
            let name = dest
                .file_name()
                .map(|s| s.to_string_lossy().to_string())
                .unwrap_or_default();
            self.modal_target = Some(ModalTarget::CopyOverwrite { src, dest });
            self.modal =
                Modal::Confirm(ConfirmDialog::new(format!("'{}' exists. Overwrite?", name)));
        } else {
            self.spawn_copy_batch();
        }
    }

    /// Spawn one background task that copies every approved (src, dest) pair and
    /// show the copying overlay. `resolve_copying` finishes the bookkeeping.
    fn spawn_copy_batch(&mut self) {
        let batch = std::mem::take(&mut self.approved_copies);
        if batch.is_empty() {
            // Nothing survived the collision prompts; just close the overlay.
            self.modal = Modal::None;
            return;
        }

        // Count the total entries (files + dirs) across every source so the
        // overlay can show N / M. This is a shallow metadata walk, far cheaper
        // than the copy itself.
        let progress = OpProgress::new();
        let total: u64 = batch.iter().map(|(src, _)| count_entries(src)).sum();
        progress.set_total(total);
        self.op_progress = Some(progress.clone());

        let task = tokio::spawn(async move {
            for (src, dest) in &batch {
                let _ = copy_path(src, dest, Some(&progress));
            }
            progress.finish();
        });
        self.copy_task = Some(task);
        self.modal = Modal::Operation { verb: "Copying" };
    }

    /// Move the active panel's tagged files (or the cursor selection) into the
    /// other panel's directory.
    ///
    /// Like `mv`: within one backend this is a rename, so it completes instantly
    /// no matter how large the files are; across backends the bytes are copied
    /// and the sources removed only once their copies have landed.
    fn start_move(&mut self) {
        let mut srcs = self.active_panel().current_screen().tagged_paths();
        if srcs.is_empty() {
            if let Some(p) = self.active_panel().selected_resolved_path() {
                srcs.push(p);
            }
        }
        if srcs.is_empty() {
            return;
        }

        // A move needs somewhere to move *to*. With one panel there is no
        // destination, so say that rather than silently doing nothing.
        let Some(other) = self.other_index() else {
            self.modal = Modal::Confirm(ConfirmDialog::new(
                "Move needs two panels — split with | first.".to_string(),
            ));
            return;
        };
        let Some(dest_dir) = self.panels[other].current_dir() else {
            return;
        };

        let src_panel = self.active;
        let src_backend = self.panels[src_panel].backend;
        let dest_backend = self.panels[other].backend;

        self.move_source_panel = src_panel;
        self.move_dest_panel = other;
        self.move_dest_dir = dest_dir.clone();
        self.approved_moves.clear();
        self.pending_moves.clear();

        // Split the sources into ones that can go straight through and ones
        // whose destination name is taken. Directory-ness comes from the tree
        // (no I/O), so the queued ghost draws the right icon.
        for src in srcs {
            let Some(name) = src.file_name().map(|n| n.to_owned()) else {
                continue;
            };
            let dest = dest_dir.join(&name);
            // Moving something onto itself is a no-op, not a collision.
            if src == dest && src_backend == dest_backend {
                continue;
            }
            let is_dir = if let Screen::Main(state) = self.panels[src_panel].current_screen() {
                state.is_dir_of(&src).unwrap_or(false)
            } else {
                false
            };
            let dest_vpath = VPath::new(dest_backend, dest.clone());
            let dest_fs = self.backends.get(dest_backend);
            let taken =
                futures::executor::block_on(async { dest_fs.symlink_stat(&dest_vpath).await })
                    .is_ok();
            if taken {
                self.pending_moves.push((src, dest, is_dir));
            } else {
                self.approved_moves.push((src, dest, is_dir));
            }
        }

        self.prompt_next_move();
    }

    /// Ask what to do about the next colliding move, then run the batch.
    ///
    /// Three answers, matching what a conflict actually calls for: overwrite
    /// this one, skip it and carry on, or abandon the whole move. Copy only
    /// offers overwrite/skip, but a move destroys the source too, so being able
    /// to stop the entire sequence matters more here.
    fn prompt_next_move(&mut self) {
        if let Some((src, dest, is_dir)) = self.pending_moves.pop() {
            let name = dest
                .file_name()
                .map(|s| s.to_string_lossy().to_string())
                .unwrap_or_default();
            self.modal_target = Some(ModalTarget::MoveOverwrite { src, dest, is_dir });
            self.modal = Modal::Confirm(
                ConfirmDialog::new(format!(
                    "'{}' exists. [o]verwrite, [s]kip, or [c]ancel the move?",
                    name
                ))
                .with_choices(&['o', 's', 'c']),
            );
        } else {
            self.spawn_move_batch();
        }
    }

    /// Dispatch the approved moves.
    ///
    /// Same-backend moves are renames — instant, so they run inline. Moves that
    /// cross backends have to copy the bytes, so they go through the transfer
    /// queue exactly as a cross-backend copy does: bounded parallelism, a live
    /// progress bar and rate in the transfer panel, and the interface stays
    /// interactive instead of sitting behind a modal overlay.
    fn spawn_move_batch(&mut self) {
        let batch = std::mem::take(&mut self.approved_moves);
        if batch.is_empty() {
            self.modal = Modal::None;
            return;
        }

        let src_panel = self.move_source_panel;
        let dest_panel = self.move_dest_panel;
        let src_backend = self.panels[src_panel].backend;
        let dest_backend = self.panels[dest_panel].backend;

        if src_backend != dest_backend {
            // Cross-backend: hand every item to the transfer queue, flagged so
            // its source is removed once its copy has fully landed.
            for (src, dest, is_dir) in batch {
                self.transfers.enqueue_move(
                    VPath::new(src_backend, src),
                    VPath::new(dest_backend, dest),
                    0,
                    is_dir,
                );
            }
            self.panels[src_panel].current_screen_mut().clear_tags();
            // Surface the queue even if the panel was previously pinned hidden —
            // the user just started work they should be able to watch.
            self.transfer_panel_override = None;
            self.modal = Modal::None;
            return;
        }

        // Same backend: renames only, so this finishes almost immediately.
        let fs = self.backends.get(src_backend);
        let progress = OpProgress::new();
        progress.set_total(batch.len() as u64);
        self.op_progress = Some(progress.clone());
        let cancel = CancelToken::new();
        self.op_cancel = Some(cancel.clone());

        let jobs: Vec<(VPath, VPath)> = batch
            .iter()
            .map(|(src, dest, _)| {
                (
                    VPath::new(src_backend, src.clone()),
                    VPath::new(dest_backend, dest.clone()),
                )
            })
            .collect();

        // Records which sources actually left, so a move that failed doesn't
        // have its entry removed from the tree as though it had succeeded.
        let moved = Arc::new(std::sync::Mutex::new(Vec::<PathBuf>::new()));
        let failures = Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
        let task = tokio::spawn({
            let moved = moved.clone();
            let failures = failures.clone();
            let fs = fs.clone();
            async move {
                for (from, to) in &jobs {
                    if cancel.is_cancelled() {
                        break;
                    }
                    // An approved overwrite means the destination is expected to
                    // be there; clear it so the rename can land.
                    if fs.symlink_stat(to).await.is_ok() {
                        let _ = crate::vfs::ops::delete_recursive(&fs, to, None, &cancel).await;
                    }
                    match crate::vfs::ops::move_path(
                        &fs,
                        &fs,
                        from,
                        to,
                        Some(&progress),
                        &cancel,
                    )
                    .await
                    {
                        Ok(_) => moved.lock().unwrap().push(from.path.clone()),
                        Err(e) => failures.lock().unwrap().push(e.to_string()),
                    }
                }
                progress.finish();
            }
        });
        self.move_results = Some((moved, failures));

        // A move empties the sources, so it resolves through the same path as a
        // delete: the moved entries leave the source panel's tree.
        self.panels[src_panel].delete_task = Some(task);
        // Filled in from `move_results` when the task finishes; a move that
        // failed must leave its source visible.
        self.panels[src_panel].deleting_paths = Vec::new();
        self.panels[src_panel].current_screen_mut().clear_tags();
        self.copy_dest_panel = dest_panel;
        self.modal = Modal::Operation { verb: "Moving" };
    }

    /// Rename `old_path` to `new_name` within its own directory.
    ///
    /// Goes through the active panel's backend, so a remote panel renames on the
    /// server. Returns an error message to surface, or `None` on success.
    ///
    /// A rename is one round trip, so it runs to completion here rather than as
    /// a background task — unlike delete, which is one round trip per entry.
    fn rename_path(&mut self, old_path: &Path, new_name: &str) -> Option<String> {
        let Some(parent) = old_path.parent() else {
            return Some("Cannot rename the filesystem root".to_string());
        };
        let new_path = parent.join(new_name);
        if new_path == old_path {
            return None;
        }

        let backend = self.active_panel().backend;
        let fs = self.backends.get(backend);
        let from = VPath::new(backend, old_path.to_path_buf());
        let to = VPath::new(backend, new_path);

        let result = futures::executor::block_on(async {
            // Refuse to clobber an existing entry: rename silently replaces the
            // destination on both backends, and losing a file to a mistyped name
            // is not recoverable.
            if fs.stat(&to).await.is_ok() {
                return Err(anyhow::anyhow!("'{}' already exists", new_name));
            }
            fs.rename(&from, &to).await
        });

        if let Err(e) = result {
            return Some(format!("Rename failed: {}", e));
        }

        // Re-list just the containing directory so the new name appears, keeping
        // the rest of the tree and the size cache intact. A full refresh would
        // re-scan everything — and on a remote panel would rebuild it as local.
        if let Screen::Main(state) = self.active_panel_mut().current_screen_mut() {
            state.reload_dir_public(parent);
        }
        None
    }

    /// Delete `paths` in the background, tracking progress, then remove them from
    /// the active panel's tree and clear its tags when the task completes.
    fn spawn_delete_batch(&mut self, paths: Vec<PathBuf>) {
        if paths.is_empty() {
            return;
        }
        // Route through the panel's backend so a remote panel deletes on the
        // server. `delete_recursive` empties directories depth-first, which both
        // backends require, and never follows a symlink out of the tree.
        let backend = self.active_panel().backend;
        let fs = self.backends.get(backend);
        let progress = OpProgress::new();
        self.op_progress = Some(progress.clone());
        let cancel = CancelToken::new();
        self.op_cancel = Some(cancel.clone());

        let to_delete: Vec<VPath> = paths
            .iter()
            .map(|p| VPath::new(backend, p.clone()))
            .collect();
        let task = tokio::spawn(async move {
            // Size the job first so the overlay shows a real fraction. On a
            // remote tree this is itself round trips, so it runs inside the task
            // rather than blocking the key press that started it.
            let mut total = 0u64;
            for p in &to_delete {
                total += crate::vfs::ops::count_entries(&fs, p, &cancel).await;
            }
            progress.set_total(total);

            for p in &to_delete {
                if cancel.is_cancelled() {
                    break;
                }
                let _ = crate::vfs::ops::delete_recursive(&fs, p, Some(&progress), &cancel).await;
            }
            progress.finish();
        });

        // Nothing lands anywhere else, so point the post-op refresh at this panel
        // rather than leaving it aimed at a previous copy's destination.
        self.copy_dest_panel = self.active;
        let panel = self.active_panel_mut();
        panel.delete_task = Some(task);
        panel.deleting_paths = paths;
        // Deleted files were the tags' whole point — clear them now so the UI
        // doesn't keep highlighting rows that are about to vanish.
        panel.current_screen_mut().clear_tags();
    }
}

/// `Instant::now()` when tracing is on, mirroring an existing `Option` marker.
fn trace_started_now(marker: Option<std::time::Instant>) -> Option<std::time::Instant> {
    marker.map(|_| std::time::Instant::now())
}

/// Whether `MYD_TRACE` asked for key-timing diagnostics. Read once.
pub fn trace_enabled() -> bool {
    use std::sync::OnceLock;
    static ENABLED: OnceLock<bool> = OnceLock::new();
    *ENABLED.get_or_init(|| std::env::var("MYD_TRACE").is_ok_and(|v| v != "0"))
}

/// Append one timing line for a dispatched action.
///
/// Writing to a file rather than stderr because the alternate screen is active;
/// anything printed to the terminal would corrupt the display.
pub fn trace_note(args: std::fmt::Arguments<'_>) {
    use std::io::Write as _;
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(trace_path())
    {
        let _ = writeln!(f, "{}", args);
    }
}

/// Where trace output goes.
fn trace_path() -> String {
    std::env::var("MYD_TRACE_FILE").unwrap_or_else(|_| {
        let base = std::env::var("HOME").unwrap_or_else(|_| "/tmp".into());
        format!("{}/.cache/myd-trace.log", base)
    })
}

fn trace_action(action: Action, elapsed: std::time::Duration) {
    use std::io::Write as _;
    let path = trace_path();
    if let Some(parent) = std::path::Path::new(&path).parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(&path) {
        let _ = writeln!(f, "{:?}\t{:.1}ms", action, elapsed.as_secs_f64() * 1000.0);
    }
}

/// The prompt text for a credential the connection is waiting on.
///
/// A rejected password reads differently from the first ask: it states that
/// authentication failed and that Esc cancels, so a repeated prompt can't be
/// mistaken for a dropped keystroke.
fn credential_prompt(need: &crate::vfs::sftp::AuthNeed) -> String {
    use crate::vfs::sftp::AuthNeed;
    match need {
        AuthNeed::Passphrase { key_path } => {
            let name = key_path
                .file_name()
                .map(|s| s.to_string_lossy().to_string())
                .unwrap_or_else(|| "key".to_string());
            format!("Passphrase for {}:", name)
        }
        AuthNeed::Password { user, host, retry } => {
            if *retry {
                format!(
                    "Authentication failed for {}@{}. Enter password to try again, or press Esc to cancel:",
                    user, host
                )
            } else {
                format!("Password for {}@{}:", user, host)
            }
        }
    }
}

/// Test hook for [`credential_prompt`], so the retry wording is covered.
pub fn credential_prompt_for_test(need: &crate::vfs::sftp::AuthNeed) -> String {
    credential_prompt(need)
}

/// The subset of pending transfer destinations that belong in one panel's tree:
/// those on the panel's backend whose target sits under the panel's root. So a
/// ghost only appears in the panel that is actually receiving the file.
fn ghosts_for_panel(
    pending: &[crate::transfer::PendingDest],
    backend: crate::vfs::BackendId,
    root: &Path,
) -> Vec<crate::transfer::PendingDest> {
    pending
        .iter()
        .filter(|d| d.path.backend == backend && d.path.path.starts_with(root))
        .cloned()
        .collect()
}

/// Count the entries (files + directories) rooted at `path`, for progress
/// totals. A single file counts as 1.
fn count_entries(path: &Path) -> u64 {
    if path.is_dir() {
        walkdir::WalkDir::new(path)
            .into_iter()
            .filter_map(|e| e.ok())
            .count() as u64
    } else {
        1
    }
}

/// Copy `src` to `dest`, recursing into directories and bumping `progress` once
/// per entry copied. Existing files at the destination are overwritten
/// (`std::fs::copy` semantics); the overwrite decision is made by the caller
/// before this runs.
fn copy_path(src: &Path, dest: &Path, progress: Option<&OpProgress>) -> std::io::Result<()> {
    if src.is_dir() {
        for entry in walkdir::WalkDir::new(src)
            .into_iter()
            .filter_map(|e| e.ok())
        {
            // Re-root each entry from `src` onto `dest`.
            let rel = match entry.path().strip_prefix(src) {
                Ok(rel) => rel,
                Err(_) => continue,
            };
            let target = dest.join(rel);
            if entry.file_type().is_dir() {
                std::fs::create_dir_all(&target)?;
            } else {
                if let Some(parent) = target.parent() {
                    std::fs::create_dir_all(parent)?;
                }
                std::fs::copy(entry.path(), &target)?;
            }
            if let Some(p) = progress {
                p.inc_done();
            }
        }
        Ok(())
    } else {
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let res = std::fs::copy(src, dest).map(|_| ());
        if let Some(p) = progress {
            p.inc_done();
        }
        res
    }
}

fn key_code_char(key: &KeyEvent) -> char {
    use crossterm::event::KeyCode;
    match key.code {
        KeyCode::Char(c) => c,
        KeyCode::Enter => '\n',
        _ => ' ',
    }
}

trait PathExt {
    fn expand_user(&self) -> PathBuf;
}

impl PathExt for PathBuf {
    fn expand_user(&self) -> PathBuf {
        if self.starts_with("~") {
            if let Some(home) = std::env::var_os("HOME") {
                let mut path = PathBuf::from(home);
                path.push(self.strip_prefix("~").unwrap_or(Path::new("")));
                return path;
            }
        }
        self.clone()
    }
}
