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
use crate::widget::help::{render_help, HelpState};
use crate::widget::input_dialog::InputDialog;
use crate::widget::sort_menu::{SortMenu, SortMenuOutcome};
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
    Help(HelpState),
    /// Numbered sort-order menu, opened by clicking the "Sort:" indicator.
    SortMenu(SortMenu),
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
    /// Masked prompt for a key passphrase or account password, feeding a
    /// connection retry.
    Password,
    /// Add or edit a saved host. `editing` is the label being replaced, so a
    /// rename updates the right entry instead of adding a second one.
    HostForm { editing: Option<String> },
    /// Confirm removing a saved host.
    HostDelete { label: String },
    /// Prompt for a directory to save as a favourite.
    FavoriteAdd,
    /// Confirm walking the tree to measure directory sizes.
    MeasureDirs,
    /// Edit a saved directory's path in place, keeping its history and position.
    FavoriteEdit { original: PathBuf },
    /// Confirm cancelling a running or queued transfer.
    CancelTransfer { id: crate::transfer::TransferId },
    /// Confirm quitting while transfers are still in flight.
    QuitConfirm,
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
    /// The directory the most recent copy batch was addressed to. Kept so a test
    /// can assert the *address* used, not just that bytes appeared somewhere.
    copy_dest_dir: Option<PathBuf>,
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
    /// Whether mouse capture is on.
    ///
    /// Capture takes over the terminal's own click-drag selection, so it can be
    /// released (Ctrl+O) when the user wants to copy text out. Most terminals
    /// also honour Shift+drag as an override while captured.
    mouse_captured: bool,
    /// Column ranges of each panel from the last frame, for click-to-focus.
    /// Recomputed every draw, like every other rect in the layout.
    panel_areas: Vec<ratatui::layout::Rect>,
    /// The whole terminal area from the last frame. Modals centre on this, so a
    /// click has to be tested against the same rect they were drawn into.
    last_frame: ratatui::layout::Rect,
    /// Cell and time of the last left click, for double-click detection.
    /// Terminals report presses individually and never a double-click.
    last_click: Option<(u16, u16, std::time::Instant)>,
    /// Whether the transfer sidebar has keyboard focus instead of a panel.
    transfer_focused: bool,
    /// The transfer the sidebar has highlighted, if it is focused.
    transfer_cursor: Option<crate::transfer::TransferId>,
    /// Where each cancellable transfer was drawn last frame, for hit-testing.
    transfer_rows: crate::widget::transfer_panel::PanelRows,
    /// The sidebar's rect from the last frame, so a click can be attributed to
    /// it rather than to the panel beside it.
    transfer_area: Option<ratatui::layout::Rect>,
    /// Set when the user confirms quitting past in-flight transfers.
    ///
    /// A confirm dialog cannot end the app on its own: `handle_modal_key`
    /// returns "keep running" and the dialog arm has no way to say otherwise.
    /// `route_key` reads this flag on the way out instead.
    quit_requested: bool,
}

/// A connection attempt running in the background, with a channel for its result.
struct ConnectTask {
    rx: tokio::sync::oneshot::Receiver<ConnectResult>,
    /// The panel that was active when the connect was issued — the remote opens
    /// here on success, so connecting replaces the pane the user was looking at.
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
        Self::with_panels(panels)
    }

    /// Build the app around already-constructed panels.
    ///
    /// The constructors differ only in which panels they open, so they share
    /// this rather than repeating three dozen fields — one that drifts out of
    /// step is a bug nobody would spot in review. Taking the panels as an
    /// argument (rather than filling them in afterwards) keeps the catalog read
    /// to exactly one, and means no panel is ever built and discarded.
    fn with_panels(panels: Vec<Panel>) -> Self {
        Self {
            panels,
            active: 0,
            key_handler: KeyBindingHandler::new(),
            modal: Modal::None,
            modal_target: None,
            copy_task: None,
            copy_dest_panel: 0,
            copy_dest_dir: None,
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
            mouse_captured: false,
            panel_areas: Vec::new(),
            last_frame: ratatui::layout::Rect::new(0, 0, 0, 0),
            last_click: None,
            transfer_focused: false,
            transfer_cursor: None,
            transfer_rows: Default::default(),
            transfer_area: None,
            quit_requested: false,
        }
    }

    /// As [`Self::new_on_picker`], but over a supplied catalog rather than the
    /// one on disk — the picker is built during construction, so a test cannot
    /// swap the catalog in afterwards.
    pub fn new_on_picker_with_hosts_for_test(hosts: HostCatalog) -> Self {
        let mut browser = Self::with_panels(Vec::new());
        browser.hosts = hosts;
        let picker = crate::screen::DirPickerState::with_catalog(&browser.hosts);
        browser.panels = vec![Panel::new_on_screen(Screen::DirPicker(picker))];
        browser
    }

    /// Start on the "go to" picker rather than a directory, for `myd --goto`.
    ///
    /// Built here rather than in `Panel::new` because the picker has to list the
    /// saved directories and hosts, and the catalog is not loaded until this
    /// constructor has run — a panel-built picker would come up empty, which is
    /// the one thing the flag exists to avoid.
    pub fn new_on_picker() -> Self {
        // No panels yet: the picker has to list the saved directories and hosts,
        // and the catalog is not read until the app is built. Starting a panel on
        // the current directory first would spawn a full walk for a tree that is
        // about to be replaced.
        let mut browser = Self::with_panels(Vec::new());
        let picker = crate::screen::DirPickerState::with_catalog(&browser.hosts);
        // The picker is the panel's only screen rather than one stacked on a
        // directory: there is nothing underneath to go back to, and `q` on it
        // quits, which is what someone who asked to be shown the picker expects.
        browser.panels = vec![Panel::new_on_screen(Screen::DirPicker(picker))];
        browser
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
            crossterm::event::EnableMouseCapture,
            crossterm::terminal::Clear(crossterm::terminal::ClearType::All),
            crossterm::cursor::Hide,
        )?;
        self.mouse_captured = true;

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

            // Drain everything queued rather than one event per 100 ms poll.
            // Mouse movement arrives in bursts, and taking a single event per
            // tick would leave the UI lagging seconds behind the pointer.
            let mut pending_scroll: i32 = 0;
            let mut first = true;
            while running {
                let ready = if first {
                    // Block briefly on the first read so an idle app doesn't spin.
                    matches!(
                        crossterm::event::poll(std::time::Duration::from_millis(100)),
                        Ok(true)
                    )
                } else {
                    matches!(
                        crossterm::event::poll(std::time::Duration::ZERO),
                        Ok(true)
                    )
                };
                first = false;
                if !ready {
                    break;
                }
                match crossterm::event::read() {
                    Ok(Event::Key(key)) => {
                        if key.kind == KeyEventKind::Press {
                            running = self.route_key(key);
                        }
                    }
                    Ok(Event::Mouse(m)) => {
                        use crossterm::event::MouseEventKind;
                        // Coalesce wheel ticks: a fast scroll delivers many
                        // events, and handling each with a full redraw between
                        // them is what makes scrolling feel sticky.
                        match m.kind {
                            MouseEventKind::ScrollDown => pending_scroll += 1,
                            MouseEventKind::ScrollUp => pending_scroll -= 1,
                            // Drags and plain moves carry no meaning here, and
                            // acting on each would flood the loop.
                            MouseEventKind::Moved | MouseEventKind::Drag(_) => {}
                            _ => running = self.route_mouse(m),
                        }
                    }
                    // Resize is handled by the next draw, which re-reads the
                    // frame size; previously every non-Key event was discarded.
                    Ok(_) => {}
                    Err(_) => break,
                }
            }
            if pending_scroll != 0 {
                // The help overlay scrolls itself; otherwise the wheel drives
                // whatever pane is focused underneath.
                if let Modal::Help(state) = &mut self.modal {
                    state.scroll_by(pending_scroll as isize);
                } else {
                    self.scroll_by(pending_scroll);
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
        self.last_frame = full;

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
            self.transfer_rows = transfer_panel::render(
                f,
                ta,
                &self.transfers,
                self.transfer_focused,
                self.transfer_cursor,
            );
            self.transfer_area = Some(ta);
        } else {
            // The sidebar hides itself on a narrow terminal and when the queue
            // is empty. Clear the stale geometry, or a click in that column
            // would still be attributed to a panel that is no longer drawn — and
            // focus would be stuck on something invisible.
            self.transfer_area = None;
            self.transfer_rows = Default::default();
            self.transfer_focused = false;
        }

        // In-progress transfer destinations, to overlay as ghost rows on the
        // panel(s) they land in.
        let pending = self.transfers.pending_destinations();

        // Focus lives in one place: a browser panel, or the transfer sidebar.
        // `state.active` used to mean "is the active panel index", which stopped
        // being true once the sidebar became focusable — both it and the last
        // panel drew a cyan border at once.
        let panel_has_focus = !self.transfer_focused;

        if panel_count == 2 {
            let cols = Layout::horizontal([Constraint::Percentage(50), Constraint::Percentage(50)])
                .split(area);
            // Kept for click-to-focus; every rect here is a frame-local that
            // would otherwise be gone before the next mouse event arrives.
            self.panel_areas = cols.to_vec();
            for (i, panel) in self.panels.iter_mut().enumerate() {
                let backend = panel.backend;
                // Tell the top Main screen whether it's the active panel so its
                // border can stand out.
                if let Screen::Main(state) = panel.current_screen_mut() {
                    state.active = panel_has_focus && i == active;
                    state.pending_ghosts = ghosts_for_panel(&pending, backend, state.root_path());
                }
                panel.current_screen_mut().render(f, cols[i]);
            }
        } else {
            self.panel_areas = vec![area];
            let backend = self.panels[0].backend;
            if let Screen::Main(state) = self.panels[0].current_screen_mut() {
                state.active = panel_has_focus;
                state.pending_ghosts = ghosts_for_panel(&pending, backend, state.root_path());
            }
            self.panels[0].current_screen_mut().render(f, area);
        }

        // Where the active panel drew its "Sort:" indicator, so the sort menu can
        // open directly beneath it. Read after the panels render, since that is
        // when the rect is recorded.
        let sort_anchor = match self.panels[self.active].current_screen() {
            Screen::Main(s) => s.sort_area,
            _ => None,
        };

        // Modals center on the whole terminal, not the tree column, so toggling
        // the sidebar doesn't shift a dialog under the cursor.
        // Taken by value so the picker can render with `&mut self` (it keeps a
        // scroll offset), then put back.
        let op_progress = self.op_progress.clone();
        match &mut self.modal {
            Modal::Confirm(d) => d.render(f, full),
            Modal::Input(d) => d.render(f, full),
            Modal::SortMenu(m) => m.render(f, full, sort_anchor),
            Modal::Operation { verb } => {
                let overlay = match &op_progress {
                    Some(p) => ProgressOverlay::for_operation(verb, p),
                    None => ProgressOverlay::new().with_message(*verb),
                };
                overlay.render(f, full);
            }
            Modal::Help(state) => render_help(f, full, state),
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
        // Directories that finished loading this tick, recorded below. Collected
        // first because the catalog cannot be borrowed while a panel is.
        let mut opened: Vec<PathBuf> = Vec::new();
        for i in 0..self.panels.len() {
            let mut just_opened = None;
            if !self.panels[i].resolve_loading_reporting(&mut just_opened) {
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
            if let Some(path) = just_opened {
                opened.push(path);
            }
        }

        // Directories opened this tick still need the shallow preference applied
        // — that must hold however the user arrived, including at startup before
        // the catalog was reachable.
        //
        // History is deliberately *not* recorded here. It was, briefly, which
        // swept in every directory drilled into while browsing and buried the
        // handful of places the user had actually chosen. The picker records
        // what the picker opens; see `Action::Confirm`.
        if !opened.is_empty() {
            // A directory the user marked shallow may have been loaded the
            // ordinary way — at startup, before the catalog was reachable.
            // Re-open it in shallow mode rather than leaving a measured tree the
            // preference says they did not want.
            let needs_shallow: Vec<PathBuf> = opened
                .iter()
                .filter(|p| self.hosts.dir_is_shallow(&p.to_string_lossy()))
                .cloned()
                .collect();
            for path in needs_shallow {
                for panel in self.panels.iter_mut() {
                    let already_shallow = match panel.current_screen() {
                        Screen::Main(s) => {
                            s.root_path() == &path && !s.tree.is_shallow()
                        }
                        _ => false,
                    };
                    if already_shallow {
                        *panel.current_screen_mut() = Screen::loading_with_source_sorted(
                            crate::widget::source::Source::LocalShallow,
                            path.clone(),
                            None,
                            crate::screen::SortMode::default(),
                        );
                    }
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
                // Reload where the entries actually landed, which is the cursor's
                // directory — the same place `dest_dir` sent them.
                if let Some(dir) = panel.dest_dir() {
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
        matches!(self.modal, Modal::Help(_))
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

    /// The directory the most recent copy batch was addressed to (for tests).
    pub fn copy_dest_for_test(&self) -> Option<PathBuf> {
        self.copy_dest_dir.clone()
    }

    /// Replace the active panel with one showing `tree` on its source's backend.
    ///
    /// Test hook. Making a panel remote otherwise needs a live connection, which
    /// puts anything that merely depends on "this panel is remote" behind the
    /// gated SFTP harness.
    pub fn replace_panel_with_remote_for_test(&mut self, tree: crate::widget::file_tree::FileTree) {
        let backend = tree.source.backend();
        let root = tree.root.path.clone();
        let panel = self.active_panel_mut();
        *panel = crate::panel::Panel::from_tree(root, tree);
        panel.backend = backend;
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

    /// The active panel's cursor position in the flattened tree (for tests).
    pub fn selected_line_index_for_test(&self) -> Option<usize> {
        match self.panels[self.active].current_screen() {
            Screen::Main(state) => Some(state.tree.cursor),
            _ => None,
        }
    }

    /// Scroll as the wheel does (for tests).
    pub fn scroll_by_for_test(&mut self, delta: i32) {
        self.scroll_by(delta);
    }

    /// How many browser panels are drawing themselves as focused (for tests).
    ///
    /// Must always be 0 (sidebar focused) or 1 — never two at once.
    pub fn focused_panel_count_for_test(&self) -> usize {
        self.panels
            .iter()
            .filter(|p| matches!(p.current_screen(), Screen::Main(s) if s.active))
            .count()
    }

    /// Whether the transfer sidebar has focus (for tests).
    pub fn transfer_focused_for_test(&self) -> bool {
        self.transfer_focused
    }

    /// The sidebar's selected transfer (for tests).
    pub fn transfer_cursor_for_test(&self) -> Option<crate::transfer::TransferId> {
        self.transfer_cursor
    }

    /// The screen row and id of the nth cancellable transfer (for tests).
    pub fn transfer_row_for_test(
        &self,
        n: usize,
    ) -> Option<(u16, crate::transfer::TransferId)> {
        self.transfer_rows.rows.get(n).copied()
    }

    /// The name of the entry under the cursor (for tests).
    pub fn selected_name_for_test(&self) -> Option<String> {
        match self.panels[self.active].current_screen() {
            Screen::Main(state) => state.tree.selected_line().map(|l| l.name.clone()),
            _ => None,
        }
    }

    /// Which modal is up, as a stable name (for tests).
    pub fn modal_kind_for_test(&self) -> &'static str {
        match &self.modal {
            Modal::None => "none",
            Modal::Confirm(_) => "confirm",
            Modal::Input(_) => "input",
            Modal::Operation { .. } => "operation",
            Modal::Help(_) => "help",
            Modal::SortMenu(_) => "sort_menu",
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

    /// How many screens deep a panel's stack is (for tests).
    pub fn panel_depth_for_test(&self, index: usize) -> usize {
        self.panels.get(index).map(|p| p.depth()).unwrap_or(0)
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

        let keep_running = self.route_key_inner(key);
        // A confirmed quit-past-transfers cannot be reported by the dialog
        // itself, so it is picked up here on the way out.
        keep_running && !self.quit_requested
    }

    fn route_key_inner(&mut self, key: KeyEvent) -> bool {
        match self.modal {
            Modal::None => self.handle_key(key),
            Modal::Help(_) => {
                // Scrolling comes first: the list is far taller than a terminal,
                // so j/k have to move within it rather than dismissing it and
                // moving the file cursor underneath.
                if let Modal::Help(state) = &mut self.modal {
                    let ctrl = key
                        .modifiers
                        .contains(crossterm::event::KeyModifiers::CONTROL);
                    let handled = match key.code {
                        KeyCode::Char('j') | KeyCode::Down => {
                            state.scroll_by(1);
                            true
                        }
                        KeyCode::Char('k') | KeyCode::Up => {
                            state.scroll_by(-1);
                            true
                        }
                        KeyCode::PageDown | KeyCode::Char(' ') => {
                            state.page(true);
                            true
                        }
                        KeyCode::PageUp => {
                            state.page(false);
                            true
                        }
                        KeyCode::Char('d') if ctrl => {
                            state.page(true);
                            true
                        }
                        KeyCode::Char('u') if ctrl => {
                            state.page(false);
                            true
                        }
                        KeyCode::Char('g') | KeyCode::Home => {
                            state.to_top();
                            true
                        }
                        KeyCode::Char('G') | KeyCode::End => {
                            state.to_bottom();
                            true
                        }
                        _ => false,
                    };
                    if handled {
                        return true;
                    }
                }

                // Otherwise dismiss. Keys whose only role here is to close the
                // help screen (quit/back and the help toggles) are consumed so
                // they don't also act on the screen behind it — e.g. q must not
                // quit the app. Any other key both dismisses help and acts.
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

    /// Route a mouse event. Returns whether the app should keep running.
    ///
    /// Mirrors [`route_key`](Self::route_key)'s modal-first ordering: a modal is
    /// on top of everything, so it must get the click before the panels do.
    pub fn route_mouse(&mut self, ev: crossterm::event::MouseEvent) -> bool {
        use crossterm::event::{MouseButton, MouseEventKind};

        let (x, y) = (ev.column, ev.row);

        if matches!(self.modal, Modal::SortMenu(_)) {
            if matches!(ev.kind, MouseEventKind::Down(MouseButton::Left)) {
                let Modal::SortMenu(menu) = &mut self.modal else {
                    return true;
                };
                // A single click picks: this is a menu of commands, and needing
                // a double-click to run one is not what a menu does.
                let outcome = menu.click_at(x, y);
                return self.apply_sort_menu_outcome(outcome);
            }
            return true;
        }
        if !matches!(self.modal, Modal::None) {
            return true;
        }

        // A click in the sidebar focuses it and selects a transfer; a
        // double-click on one asks to cancel it.
        if let Some(ta) = self.transfer_area {
            let in_sidebar =
                x >= ta.x && x < ta.x + ta.width && y >= ta.y && y < ta.y + ta.height;
            if in_sidebar {
                if matches!(ev.kind, MouseEventKind::Down(MouseButton::Left)) {
                    let double = self.note_click(x, y);
                    self.transfer_focused = true;
                    if let Some(id) = self.transfer_rows.at(y) {
                        self.transfer_cursor = Some(id);
                        if double {
                            self.prompt_cancel_selected_transfer();
                        }
                    }
                }
                return true;
            }
        }

        match ev.kind {
            MouseEventKind::Down(MouseButton::Left) => {
                // A click back in a browser panel takes focus off the sidebar.
                self.transfer_focused = false;

                // The sort indicator in the title bar opens the sort menu.
                // Checked before the panel hit-test, since it sits on the border
                // row that the tree would otherwise ignore.
                let on_sort = self.panels.iter().position(|p| {
                    matches!(p.current_screen(), Screen::Main(s)
                        if s.sort_area.is_some_and(|a| {
                            x >= a.x && x < a.x + a.width && y >= a.y && y < a.y + a.height
                        }))
                });
                if let Some(i) = on_sort {
                    self.active = i;
                    self.open_sort_menu();
                    return true;
                }

                // Clicking a panel focuses it, so a click in the inactive pane
                // doesn't move the other pane's cursor.
                if let Some(i) = self
                    .panel_areas
                    .iter()
                    .position(|a| x >= a.x && x < a.x + a.width && y >= a.y && y < a.y + a.height)
                {
                    if i < self.panels.len() {
                        self.active = i;
                    }
                }
                let double = self.note_click(x, y);
                if let Screen::Main(state) = self.active_panel_mut().current_screen_mut() {
                    if state.click_at(x, y) && double {
                        // A double-click does exactly what Enter does — open a
                        // directory, or act on the selection.
                        return self.dispatch_action(Action::Confirm);
                    }
                }
                true
            }
            // Right-click also opens, for anyone who prefers it to a double-click.
            MouseEventKind::Down(MouseButton::Right) => {
                if let Screen::Main(state) = self.active_panel_mut().current_screen_mut() {
                    if state.click_at(x, y) {
                        return self.dispatch_action(Action::Confirm);
                    }
                }
                true
            }
            _ => true,
        }
    }

    /// Give the transfer sidebar keyboard focus, selecting its first entry.
    fn focus_transfers(&mut self) {
        if self.transfer_area.is_none() {
            return;
        }
        self.transfer_focused = true;
        if self.transfer_cursor.is_none() {
            self.transfer_cursor = self.transfer_rows.ids().first().copied();
        }
    }

    /// Move the sidebar's selection by one entry.
    fn transfer_cursor_step(&mut self, forward: bool) {
        let ids = self.transfer_rows.ids();
        if ids.is_empty() {
            self.transfer_cursor = None;
            return;
        }
        let at = self
            .transfer_cursor
            .and_then(|c| ids.iter().position(|i| *i == c));
        let next = match at {
            Some(i) if forward => (i + 1) % ids.len(),
            Some(i) => {
                if i == 0 {
                    ids.len() - 1
                } else {
                    i - 1
                }
            }
            None => 0,
        };
        self.transfer_cursor = Some(ids[next]);
    }

    /// Ask before cancelling the selected transfer.
    ///
    /// Confirmed because cancelling discards work that may have taken minutes on
    /// a slow link, and `k` is one key away from the `j` used to get there.
    fn prompt_cancel_selected_transfer(&mut self) {
        let Some(id) = self.transfer_cursor else {
            return;
        };
        let Some(t) = self.transfers.transfers().iter().find(|t| t.id == id) else {
            return;
        };
        let name = t.name.clone();
        self.modal_target = Some(ModalTarget::CancelTransfer { id });
        self.modal = Modal::Confirm(ConfirmDialog::new(format!(
            "Cancel the transfer of '{}'? Any partial copy is discarded.",
            name
        )));
    }

    /// Keys the transfer sidebar handles while focused.
    ///
    /// Returns `None` when the key isn't one of them, so it falls through to the
    /// usual handling and the sidebar can't swallow global keys like `?` or
    /// Ctrl+C.
    fn handle_transfer_key(&mut self, key: KeyEvent) -> Option<bool> {
        if !self.transfer_focused {
            return None;
        }
        match key.code {
            KeyCode::Char('j') | KeyCode::Down => {
                self.transfer_cursor_step(true);
                Some(true)
            }
            KeyCode::Char('K') | KeyCode::Up => {
                self.transfer_cursor_step(false);
                Some(true)
            }
            // Lowercase k cancels; K moves up. Vi users expect k to move, so this
            // is a deliberate departure — it is what was asked for, and the
            // confirmation dialog makes a mistaken press recoverable.
            KeyCode::Char('k') | KeyCode::Delete => {
                self.prompt_cancel_selected_transfer();
                Some(true)
            }
            KeyCode::Esc | KeyCode::Char('q') => {
                self.transfer_focused = false;
                self.transfer_cursor = None;
                Some(true)
            }
            _ => None,
        }
    }

    /// Record a click and report whether it completes a double-click.
    ///
    /// Terminals report individual presses; there is no double-click event, so it
    /// has to be inferred from timing and position. The cell must match exactly —
    /// two quick clicks on different rows are two selections, not an open.
    fn note_click(&mut self, x: u16, y: u16) -> bool {
        /// Matches the common desktop default. Long enough to be comfortable,
        /// short enough that two deliberate selections aren't merged.
        const DOUBLE_CLICK: std::time::Duration = std::time::Duration::from_millis(400);

        let now = std::time::Instant::now();
        let is_double = match self.last_click {
            Some((lx, ly, at)) => {
                lx == x && ly == y && now.duration_since(at) <= DOUBLE_CLICK
            }
            None => false,
        };
        // Clear on a match so a third click starts a fresh pair rather than
        // counting as another double.
        self.last_click = if is_double { None } else { Some((x, y, now)) };
        is_double
    }

    /// Apply a favourite add/remove the directory picker asked for, persist it,
    /// and rebuild the picker so the change is visible immediately.
    fn apply_favorite_edit(&mut self) {
        use crate::screen::FavoriteEdit;

        let edit = match self.active_panel_mut().current_screen_mut() {
            Screen::DirPicker(state) => state.take_favorite_edit(),
            _ => None,
        };
        let Some(edit) = edit else {
            return;
        };

        let changed = match &edit {
            FavoriteEdit::PromptAdd => {
                // Ask which directory to save, seeded with whatever the path
                // field holds so a path being typed can be saved as-is.
                let seed = match self.active_panel().current_screen() {
                    Screen::DirPicker(state) => state.input_for_test().to_string(),
                    _ => String::new(),
                };
                self.modal_target = Some(ModalTarget::FavoriteAdd);
                self.modal = Modal::Input(
                    InputDialog::new("Save directory as a favourite:", "/path/to/directory")
                        .with_default(seed),
                );
                return;
            }
            FavoriteEdit::Remove(path) => {
                self.hosts.remove_favorite(&path.to_string_lossy())
            }
            FavoriteEdit::EditDir(path) => {
                // A popup pre-filled with the current path, so correcting a typo
                // or a moved directory is an edit rather than a delete and a
                // retype.
                self.modal_target = Some(ModalTarget::FavoriteEdit {
                    original: path.clone(),
                });
                self.modal = Modal::Input(
                    InputDialog::new("Edit directory path:", "/path/to/directory")
                        .with_title("Edit saved directory")
                        .with_default(path.to_string_lossy().to_string()),
                );
                return;
            }
            FavoriteEdit::DeleteHost(label) => {
                self.modal_target = Some(ModalTarget::HostDelete {
                    label: label.clone(),
                });
                self.modal = Modal::Confirm(ConfirmDialog::new(format!(
                    "Remove saved host '{}'? (the remote itself is untouched)",
                    label
                )));
                return;
            }
            FavoriteEdit::EditHost(label) => {
                // Reuses the dialing directory's form, so a host is edited the
                // same way whichever list it was reached from.
                let existing = self.hosts.find(label).cloned();
                let prefill = existing
                    .as_ref()
                    .map(|h| format!("{} = {}", h.label, h.to_url()))
                    .unwrap_or_default();
                self.modal_target = Some(ModalTarget::HostForm {
                    editing: Some(label.clone()),
                });
                self.modal = Modal::Input(
                    InputDialog::new(HOST_FORM_PROMPT, "")
                        .with_title("Edit saved host")
                        .with_default(prefill),
                );
                return;
            }
            FavoriteEdit::ToggleShallow(path) => {
                let key = path.to_string_lossy().to_string();
                let now = !self.hosts.dir_is_shallow(&key);
                self.hosts.set_dir_shallow(&key, now);
                true
            }
            FavoriteEdit::Pin(path) => self.hosts.pin_dir(&path.to_string_lossy()),
            FavoriteEdit::PinAndMove(path) => {
                // `m` on an entry outside the pinned block: pin it, then start
                // the move on the rebuilt picker so the entry is where the move
                // logic expects to find it.
                let pinned = self.hosts.pin_dir(&path.to_string_lossy());
                if pinned {
                    if let Err(e) = self.hosts.save() {
                        tracing::warn!(error = %e, "could not persist the pin");
                    }
                    self.rebuild_dir_picker();
                    let path = path.clone();
                    if let Screen::DirPicker(state) =
                        self.active_panel_mut().current_screen_mut()
                    {
                        state.select_path(&path);
                        state.start_move(&path);
                    }
                }
                // Already saved and rebuilt above; nothing further to do.
                return;
            }
            FavoriteEdit::Unpin(path) => self.hosts.unpin_dir(&path.to_string_lossy()),
            FavoriteEdit::Reorder { order, unpin } => {
                // Applied as one step: the order first, then any entry the user
                // slid out of the block. Doing it the other way round would
                // renumber around a path that is about to leave.
                self.hosts.apply_pin_order(order);
                if let Some(path) = unpin {
                    self.hosts.unpin_dir(&path.to_string_lossy());
                }
                true
            }
        };
        if !changed {
            return;
        }
        if let Err(e) = self.hosts.save() {
            self.modal = Modal::Confirm(ConfirmDialog::notice(format!(
                "Could not save the favourites list: {}",
                e
            )));
            return;
        }

        self.rebuild_dir_picker();
    }

    /// Push the combined picker onto the active panel.
    ///
    /// Pushed rather than replacing the current screen, so dismissing it returns
    /// to what was underneath instead of leaving the panel with nothing to show.
    fn open_dir_picker(&mut self) {
        let picker = crate::screen::DirPickerState::with_catalog(&self.hosts);
        self.active_panel_mut()
            .screen_stack
            .push(Screen::DirPicker(picker));
    }

    /// Rebuild the open directory picker over the current catalog.
    ///
    /// Keeps the keyboard focus and the cursor's path where they still exist:
    /// adding or removing an entry should not fling the cursor to the top of the
    /// list or dump the user back into the path field.
    fn rebuild_dir_picker(&mut self) {
        let catalog = self.hosts.clone();
        if let Screen::DirPicker(state) = self.active_panel_mut().current_screen_mut() {
            let keep = state.selected().map(|o| o.path.clone());
            let mut rebuilt = crate::screen::DirPickerState::with_catalog(&catalog);
            rebuilt.adopt_focus_from(state);
            if let Some(path) = keep {
                rebuilt.select_path(&path);
            }
            *state = rebuilt;
        }
    }

    /// Turn directory measuring off, or ask before turning it back on.
    ///
    /// Going shallow is instant — there is nothing to compute. Going back means
    /// walking the tree, which is the slowest thing this app does and the reason
    /// the user turned it off, so it asks first rather than freezing on a
    /// keystroke.
    fn toggle_shallow(&mut self) {
        let panel = self.active_panel();
        if !panel.backend.is_local() {
            self.modal = Modal::Confirm(ConfirmDialog::notice(
                "Remote directories are never measured — a recursive walk over \
                 SFTP is one round trip per directory.",
            ));
            return;
        }
        let Screen::Main(state) = panel.current_screen() else {
            return;
        };
        if state.tree.is_shallow() {
            let root = state.root_path().clone();
            self.modal_target = Some(ModalTarget::MeasureDirs);
            self.modal = Modal::Confirm(ConfirmDialog::new(format!(
                "Measure every directory under {}? This walks the whole tree.",
                root.display()
            )));
            return;
        }
        self.set_shallow(true);
    }

    /// Rebuild the active panel's tree with measuring on or off, and remember
    /// the choice for this directory.
    fn set_shallow(&mut self, shallow: bool) {
        let Screen::Main(state) = self.active_panel().current_screen() else {
            return;
        };
        let root = state.root_path().clone();
        let source = state.tree.source.with_shallow(shallow);
        let sort_mode = state.tree.sort_mode;

        // A fresh scan rather than an in-place edit: the sizes are what changed,
        // and they are computed during the walk. Going shallow discards the cache
        // too, since its entries are the totals now being disowned.
        let cache = if shallow {
            None
        } else {
            Some(state.tree.size_cache.clone())
        };
        self.active_panel_mut()
            .screen_stack
            .push(Screen::loading_with_source_sorted(
                source,
                root.clone(),
                cache,
                sort_mode,
            ));

        // Remembered per directory, so somewhere you have decided not to measure
        // stays that way the next time you open it. Keyed on the root captured
        // above: reading it back now would ask the *loading* screen, which has no
        // directory yet, and the preference would silently go nowhere.
        self.hosts
            .set_dir_shallow(&root.to_string_lossy(), shallow);
        if let Err(e) = self.hosts.save() {
            tracing::warn!(error = %e, "could not persist the traversal mode");
        }
    }

    /// Hand the focused selection to the desktop's default application.
    ///
    /// Refuses on a remote panel: `open` and `xdg-open` only understand local
    /// paths, so a remote one would either fail with the launcher's own message
    /// or — worse — open an unrelated local file that happens to share the path,
    /// which is the same trap that made remote copies land in the wrong place.
    fn open_selection_externally(&mut self) {
        let panel = self.active_panel();
        if !panel.backend.is_local() {
            self.modal = Modal::Confirm(ConfirmDialog::notice(
                "Cannot open remote files with a local application. Copy it across first (c).",
            ));
            return;
        }
        let Some(path) = panel.selected_path() else {
            return;
        };
        if let Err(e) = crate::utils::opener::open_path(&path) {
            tracing::warn!(path = %path.display(), error = %e, "could not open externally");
            self.modal = Modal::Confirm(ConfirmDialog::notice(format!("{}", e)));
        }
    }

    /// Scroll the view under the pointer by `delta` rows.
    ///
    /// The wheel moves the *cursor*, not the viewport, so it stays consistent
    /// with `j`/`k`: within the window the cursor travels and the content holds
    /// still, and the view scrolls once the cursor reaches an edge. Moving the
    /// viewport independently would let the cursor drift off-screen, and the
    /// render-time clamp would immediately drag the view back — the two would
    /// fight each other.
    fn scroll_by(&mut self, delta: i32) {
        let screen = self.active_panel_mut().current_screen_mut();
        if delta > 0 {
            for _ in 0..delta {
                screen.cursor_down();
            }
        } else {
            for _ in 0..(-delta) {
                screen.cursor_up();
            }
        }
    }

    /// Release or re-grab the mouse, so terminal text selection can be used.
    fn toggle_mouse_capture(&mut self) {
        let mut out = std::io::stdout();
        if self.mouse_captured {
            let _ = crossterm::execute!(out, crossterm::event::DisableMouseCapture);
        } else {
            let _ = crossterm::execute!(out, crossterm::event::EnableMouseCapture);
        }
        self.mouse_captured = !self.mouse_captured;
    }

    /// Whether mouse capture is currently on (for the footer and tests).
    pub fn mouse_captured(&self) -> bool {
        self.mouse_captured
    }

    fn handle_key(&mut self, key: KeyEvent) -> bool {
        // The transfer sidebar takes its own keys while focused, but only the
        // ones it owns — anything else falls through so global keys still work.
        if let Some(result) = self.handle_transfer_key(key) {
            return result;
        }

        // Let the current screen handle raw keys first (e.g., dir picker input).
        if let Some(result) = self
            .active_panel_mut()
            .current_screen_mut()
            .handle_raw_key(key)
        {
            // The picker cannot reach the catalog, so it records a requested
            // favourite change and the app applies and persists it here.
            self.apply_favorite_edit();
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
        //
        // Every motion belongs here, not just j/k: a page jump or `G` inside a
        // visual selection has to extend it, and if the action ends visual mode
        // first then the tagging inside the motion has no anchor to work from.
        if !matches!(
            action,
            Action::CursorUp
                | Action::CursorDown
                | Action::PageDown
                | Action::PageUp
                | Action::HalfPageDown
                | Action::HalfPageUp
                | Action::ToTop
                | Action::ToBottom
                | Action::VisualMode
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
                // Abandoning transfers is not recoverable — a partial copy is
                // discarded — so it is worth one keystroke to confirm. Queued
                // transfers count too: losing them loses the intent, not just
                // the progress. Ctrl-C still force-quits without asking.
                if self.transfers.has_work() {
                    let n = self.transfers.active_count() + self.transfers.queued_count();
                    self.modal_target = Some(ModalTarget::QuitConfirm);
                    self.modal = Modal::Confirm(ConfirmDialog::new(format!(
                        "{} transfer{} still running. Quit and abandon {}?",
                        n,
                        if n == 1 { "" } else { "s" },
                        if n == 1 { "it" } else { "them" },
                    )));
                    return true;
                }

                // Otherwise quit the app immediately, regardless of history.
                // Use Ctrl-o (PopScreen) to step back up a directory.
                false
            }
            Action::SwitchPanel => {
                // Tab rotates through every focusable pane in layout order — each
                // browser panel left to right, then the transfer sidebar — and
                // wraps around. Everything focusable is reachable from one key
                // rather than the sidebar needing its own.
                //
                // This has to be a rotation over the whole set, not a pair of
                // special cases: leaving the sidebar used to clear its focus
                // without moving `active`, so focus fell back to whichever panel
                // it came from. With two panels open that alternated between the
                // second panel and the sidebar forever, and the first panel was
                // unreachable by Tab.
                let panels = self.panels.len();
                let sidebar = self.transfer_area.is_some();
                // Position in the rotation: 0..panels are the browser panels, and
                // `panels` is the sidebar when it is on screen.
                let current = if self.transfer_focused {
                    panels
                } else {
                    self.active.min(panels.saturating_sub(1))
                };
                let stops = panels + usize::from(sidebar);
                if stops > 1 {
                    let next = (current + 1) % stops;
                    if next == panels {
                        self.focus_transfers();
                    } else {
                        self.transfer_focused = false;
                        self.transfer_cursor = None;
                        self.active = next;
                    }
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
                if matches!(self.modal, Modal::Help(_)) {
                    self.modal = Modal::None;
                } else {
                    self.modal = Modal::Help(HelpState::new());
                }
                true
            }
            Action::Confirm => {
                let panel = self.active_panel_mut();
                // Confirm dir picker selection → push loading screen.
                if let Screen::DirPicker(state) = panel.current_screen_mut() {
                    match state.confirm() {
                        crate::screen::PickerChoice::Open(path) => {
                            // Recorded here rather than where loads resolve, so
                            // only what the user *chose* in this dialog enters
                            // the list. Recording every resolved load swept in
                            // each directory drilled into while browsing, which
                            // buried the handful of places actually picked.
                            self.hosts.record_visit(&path.to_string_lossy());
                            if let Err(e) = self.hosts.save() {
                                tracing::warn!(
                                    error = %e,
                                    "could not persist directory history"
                                );
                            }
                            // A directory opened from the picker honours the
                            // remembered traversal mode too.
                            let shallow =
                                self.hosts.dir_is_shallow(&path.to_string_lossy());
                            let panel = self.active_panel_mut();
                            if shallow {
                                panel.screen_stack.push(Screen::loading_with_source_sorted(
                                    crate::widget::source::Source::LocalShallow,
                                    path.clone(),
                                    None,
                                    crate::screen::SortMode::default(),
                                ));
                            } else {
                                panel.screen_stack.push(Screen::loading(path.clone()));
                            }
                        }
                        crate::screen::PickerChoice::NotADirectory(path) => {
                            // Say so and hand the field back, with what was typed
                            // intact so a typo is a correction rather than a
                            // retype. Falling through to the highlighted entry
                            // would have opened somewhere the user never asked for.
                            state.focus_field();
                            self.modal = Modal::Confirm(ConfirmDialog::notice(format!(
                                "'{}' is not a directory.",
                                path.display()
                            )));
                        }
                        crate::screen::PickerChoice::Connect(url) => {
                            // A saved host: leave the picker and dial it, the
                            // same path a typed URL takes.
                            //
                            // Only when there is something to leave it *for*.
                            // Under `--goto` the picker is the panel's only
                            // screen, and popping it emptied the stack — the
                            // next redraw then panicked on `current_screen`.
                            // A successful connect replaces the whole panel
                            // anyway, so keeping the picker costs nothing and
                            // means a failed one returns to the list it was
                            // picked from rather than to a blank panel.
                            if self.active_panel().depth() > 1 {
                                self.pop_screen();
                            }
                            self.connecting_label = label_for_url(&self.hosts, &url);
                            self.start_connect(&url);
                        }
                        crate::screen::PickerChoice::Nothing => {}
                    }
                    return true;
                }
                // Enter on a directory in main screen → navigate into it.
                // Extract the path first to avoid double borrow. `selected_is_dir`
                // answers for whichever view has focus — asking the tree directly
                // meant Enter on a treemap tile consulted the tree's cursor, which
                // is somewhere else entirely.
                let target = if let Screen::Main(state) = panel.current_screen() {
                    if state.selected_is_dir() {
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
                        panel.screen_stack.push(Screen::loading_with_source_sorted(
                            source,
                            path,
                            Some(cache),
                            sort_mode,
                        ));
                    } else {
                        // Honour whatever was decided for this directory last
                        // time: somewhere not worth walking stays that way
                        // instead of measuring again on every arrival.
                        let shallow =
                            self.hosts.dir_is_shallow(&path.to_string_lossy());
                        let panel = self.active_panel_mut();
                        if shallow {
                            panel.screen_stack.push(Screen::loading_with_source_sorted(
                                source.with_shallow(true),
                                path,
                                None,
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
                }
                true
            }
            Action::ToggleMouse => {
                self.toggle_mouse_capture();
                true
            }
            Action::OpenWithDefaultApp => {
                self.open_selection_externally();
                true
            }
            Action::ToggleShallow => {
                self.toggle_shallow();
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
                    // That picker was built from the catalog as it stood when it
                    // was first opened. Directories visited since are recorded in
                    // the catalog but absent from its list, so rebuild it —
                    // otherwise the history feature silently does nothing for
                    // anyone who reaches the picker this way.
                    self.rebuild_dir_picker();
                } else {
                    // Push rather than replace, so the picker sits *over* the
                    // current view. Replacing discarded the tree underneath,
                    // which left `q` nothing to return to — declining `gd` then
                    // had to quit the app.
                    self.open_dir_picker();
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
                    Action::HalfPageDown => current.half_page_down(),
                    Action::HalfPageUp => current.half_page_up(),
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
                    // Both columns are remembered for screens opened later, so
                    // entering a directory doesn't silently drop them.
                    Action::TogglePerms => {
                        let panel = self.active_panel_mut();
                        let result = panel.current_screen_mut().toggle_perms();
                        if let Screen::Main(state) = panel.current_screen() {
                            panel.view_prefs.show_perms = state.tree.show_perms;
                        }
                        result
                    }
                    Action::ToggleTimes => {
                        let panel = self.active_panel_mut();
                        let result = panel.current_screen_mut().toggle_times();
                        if let Screen::Main(state) = panel.current_screen() {
                            panel.view_prefs.show_times = state.tree.show_times;
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
                    | Action::ToggleMouse
                    | Action::OpenWithDefaultApp
                    | Action::ToggleShallow => unreachable!(),
                    Action::None => true,
                }
            }
        }
    }

    fn handle_modal_key(&mut self, key: KeyEvent) -> bool {
        // The sort menu owns its keys outright: it binds the digits and j/k
        // itself, and going through the chord detector would put its 500 ms
        // timeout in front of every keystroke.
        if let Modal::SortMenu(menu) = &mut self.modal {
            let outcome = menu.handle_key(key);
            return self.apply_sort_menu_outcome(outcome);
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
                        Some(ModalTarget::QuitConfirm) => {
                            // Declining simply closes the dialog; the flag stays
                            // false so a later `q` asks again.
                            self.quit_requested = result;
                        }
                        Some(ModalTarget::CancelTransfer { id }) if result => {
                            self.transfers.cancel(id);
                        }
                        Some(ModalTarget::MeasureDirs) if result => {
                            self.set_shallow(false);
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
                                    let failure = self
                                        .active_panel_mut()
                                        .current_screen_mut()
                                        .search(&value);
                                    if let Some(msg) = failure {
                                        self.modal =
                                            Modal::Confirm(ConfirmDialog::new(msg));
                                    }
                                }
                            }
                            ModalTarget::FavoriteEdit { original } => {
                                let typed = value.trim();
                                let dir = PathBuf::from(typed).expand_user();
                                if typed.is_empty() {
                                    // Nothing typed: treat as a cancel.
                                } else if !dir.is_dir() {
                                    self.modal = Modal::Confirm(ConfirmDialog::notice(format!(
                                        "'{}' is not a directory.",
                                        dir.display()
                                    )));
                                } else if let Some(msg) = self
                                    .hosts
                                    .rename_favorite(&original.to_string_lossy(), &dir.to_string_lossy())
                                {
                                    self.modal = Modal::Confirm(ConfirmDialog::notice(msg));
                                } else {
                                    if let Err(e) = self.hosts.save() {
                                        self.modal = Modal::Confirm(ConfirmDialog::notice(
                                            format!("Could not save the list: {}", e),
                                        ));
                                    }
                                    self.rebuild_dir_picker();
                                }
                            }
                            ModalTarget::FavoriteAdd => {
                                let typed = value.trim();
                                // One prompt for both kinds of destination: a
                                // `label = sftp://…` line saves a host, anything
                                // else is a directory path. Two prompts would
                                // have meant knowing which you wanted before you
                                // could say what it was.
                                if typed.contains("sftp://") {
                                    // Reuses the dialing directory's own parser
                                    // and validation, so a host saved from here
                                    // is identical to one saved from the form.
                                    self.submit_host_form(typed, None);
                                    self.rebuild_dir_picker();
                                    return true;
                                }
                                // `~` is expanded; the path is otherwise saved as
                                // typed, so it stays meaningful on the machine
                                // that will open it.
                                let dir = PathBuf::from(typed).expand_user();
                                if typed.is_empty() {
                                    // Nothing typed: treat as a cancel.
                                } else if !dir.is_dir() {
                                    self.modal = Modal::Confirm(ConfirmDialog::notice(format!(
                                        "'{}' is not a directory.",
                                        dir.display()
                                    )));
                                } else {
                                    let key = dir.to_string_lossy().to_string();
                                    let added = self
                                        .hosts
                                        .add_favorite(crate::hosts::SavedDir::saved(key));
                                    if added {
                                        if let Err(e) = self.hosts.save() {
                                            self.modal =
                                                Modal::Confirm(ConfirmDialog::notice(format!(
                                                    "Could not save the favourites list: {}",
                                                    e
                                                )));
                                        }
                                    } else {
                                        self.modal = Modal::Confirm(ConfirmDialog::notice(
                                            format!("'{}' is already saved.", dir.display()),
                                        ));
                                    }
                                    self.rebuild_dir_picker();
                                }
                            }
                            ModalTarget::Filter => {
                                // Empty pattern clears the filter (handled downstream);
                                // a malformed one says so rather than doing nothing.
                                let failure = self
                                    .active_panel_mut()
                                    .current_screen_mut()
                                    .filter(&value);
                                if let Some(msg) = failure {
                                    self.modal = Modal::Confirm(ConfirmDialog::new(msg));
                                }
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
                                // `~` is expanded, but the path is otherwise used
                                // exactly as typed. Canonicalising it here resolved
                                // it against the *local* disk: on macOS a typed
                                // `/tmp` became `/private/tmp`, which was then sent
                                // to a remote server that has no `/private`.
                                let dir = PathBuf::from(&value).expand_user();
                                // Only a local destination can be checked from
                                // here; a remote one is validated by the transfer
                                // itself, which now reports a missing destination
                                // directory as exactly that.
                                let backend = self.active_panel().backend;
                                let is_local = backend.is_local();
                                if !is_local || dir.is_dir() {
                                    let active = self.active;
                                    if copy_needs_transfer_queue(backend, backend) {
                                        // Both endpoints are the same remote
                                        // panel, so this is a server-side copy and
                                        // belongs on the queue. `begin_copy_batch`
                                        // spawns `copy_path`, which is plain
                                        // `std::fs` and would have operated on the
                                        // local disk under remote paths.
                                        let kinds: Vec<bool> = if let Screen::Main(state) =
                                            self.panels[active].current_screen()
                                        {
                                            srcs.iter()
                                                .map(|p| state.is_dir_of(p).unwrap_or(false))
                                                .collect()
                                        } else {
                                            vec![false; srcs.len()]
                                        };
                                        self.enqueue_cross_backend_copy(
                                            srcs, kinds, backend, dir, backend, active,
                                        );
                                    } else {
                                        // Copy into the chosen directory, refreshing
                                        // the active panel (single-panel mode).
                                        self.begin_copy_batch(srcs, dir, active, active);
                                    }
                                } else {
                                    self.modal = Modal::Confirm(ConfirmDialog::new(format!(
                                        "'{}' is not a directory.",
                                        dir.display()
                                    )));
                                }
                            }
                            ModalTarget::HostForm { editing } => {
                                self.submit_host_form(&value, editing);
                            }
                            ModalTarget::HostDelete { .. }
                            | ModalTarget::CancelTransfer { .. }
                            | ModalTarget::MeasureDirs
                            | ModalTarget::QuitConfirm => {}
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
            Modal::Help(_) => {
                // Dismiss help — the real key is handled by handle_key.
                self.modal = Modal::None;
                true
            }
            // Handled by the early returns at the top of this function, which
            // need `&mut self` for the whole call and so can't sit in this match.
            Modal::SortMenu(_) => true,
            Modal::None => true,
        }
    }

    /// Kick off a connection at startup from a CLI `sftp://` argument. The
    /// connect runs in the background once the event loop starts.
    pub fn connect_on_start(&mut self, target: &str) {
        self.start_connect(target);
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

    /// Act on the sort menu's decision.
    fn apply_sort_menu_outcome(&mut self, outcome: SortMenuOutcome) -> bool {
        match outcome {
            SortMenuOutcome::Continue => {}
            SortMenuOutcome::Cancelled => self.modal = Modal::None,
            SortMenuOutcome::Chosen(mode) => {
                self.modal = Modal::None;
                if let Screen::Main(st) = self.active_panel_mut().current_screen_mut() {
                    st.set_sort_mode(mode);
                }
            }
        }
        true
    }

    /// Open the sort menu for the active panel.
    fn open_sort_menu(&mut self) {
        let current = match self.panels[self.active].current_screen() {
            Screen::Main(s) => s.tree.sort_mode,
            _ => return,
        };
        self.modal = Modal::SortMenu(SortMenu::new(current));
    }

    /// Return to the directory picker after a management action.
    ///
    /// Every add, edit and delete now comes from the combined picker, so this
    /// refreshes it in place. There used to be a fallback that opened the old
    /// quick-connect modal for edits reached from `gr`; with that chord gone,
    /// nothing can arrive here without the picker underneath.
    fn reopen_picker(&mut self) {
        self.modal_target = None;
        self.modal = Modal::None;
        self.rebuild_dir_picker();
    }

    /// Begin connecting to a remote host named by `target` (e.g.
    /// `sftp://user@host/path` or an ssh config alias).
    ///
    /// Parses the target and kicks off the async connection; `resolve_connect`
    /// finishes the job when the attempt completes.
    fn start_connect(&mut self, target: &str) {
        // Remember which panel is active now — the remote opens here on success,
        // so connecting takes over the pane the user was looking at.
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
                // Open the remote in the panel that was active when the connect was
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
                // Dual mode: copy into the directory the other panel's cursor is
                // in. Not its root — a pane whose cursor is inside an expanded
                // subdirectory would otherwise receive the copy several levels
                // above where the user is looking.
                let Some(dest_dir) = self.panels[other].dest_dir() else {
                    return;
                };
                let source = self.active;

                // If either endpoint is remote, this is a transfer: it goes
                // through the non-blocking queue and the transfer panel, not the
                // modal copy overlay.
                let src_backend = self.panels[source].backend;
                let dest_backend = self.panels[other].backend;
                if copy_needs_transfer_queue(src_backend, dest_backend) {
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
        // Recorded for the same reason `begin_copy_batch` records it: so the
        // address a copy was sent to can be asserted, not just its side effects.
        self.copy_dest_dir = Some(dest_dir.clone());
        for (i, src) in srcs.into_iter().enumerate() {
            let Some(name) = src.file_name().map(|n| n.to_owned()) else {
                continue;
            };
            let is_dir = kinds.get(i).copied().unwrap_or(false);
            let src_vpath = VPath::new(src_backend, src);
            let dest_vpath = VPath::new(dest_backend, dest_dir.join(name));
            // The resolved endpoints, logged before anything is queued: a transfer
            // that writes somewhere unexpected is indistinguishable from one that
            // fails to write, and this is the line that tells them apart.
            tracing::debug!(
                src = %src_vpath,
                dest = %dest_vpath,
                is_dir,
                src_backend = src_backend.0,
                dest_backend = dest_backend.0,
                "queueing cross-backend transfer"
            );
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
        self.copy_dest_dir = Some(dest_dir.clone());
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
        // The cursor's directory, matching copy — see `Panel::dest_dir`.
        let Some(dest_dir) = self.panels[other].dest_dir() else {
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
/// Whether a copy between these backends has to go through the transfer queue.
///
/// True whenever either endpoint is remote. A plain `std::fs` copy would read and
/// write the *local* disk under paths that name files on a server — silently
/// touching the wrong machine. Shared by the dual-panel and single-panel copy
/// paths, which previously disagreed: only the former checked.
pub fn copy_needs_transfer_queue(
    src: crate::vfs::BackendId,
    dest: crate::vfs::BackendId,
) -> bool {
    !src.is_local() || !dest.is_local()
}

/// The saved label for `url`, so a successful connect promotes the right entry.
fn label_for_url(catalog: &HostCatalog, url: &str) -> Option<String> {
    catalog
        .hosts()
        .iter()
        .find(|h| h.to_url() == url)
        .map(|h| h.label.clone())
}

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
