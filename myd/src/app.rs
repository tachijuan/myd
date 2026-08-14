use anyhow::Result;
use crossterm::event::{Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::layout::{Constraint, Layout, Rect};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use crate::hosts::{HostCatalog, SavedHost};
use crate::keybinding::{Action, KeyBindingHandler};
use crate::panel::Panel;
use crate::screen::{FooterMode, Screen, SortMode};
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
        // Remove any image still on screen before handing the terminal back.
        //
        // A kitty image is an object the terminal re-composites and is not part of
        // the alternate screen's cells, so leaving that screen does not take it
        // with us — it needs its own delete. iTerm2 and sixel images belong to
        // cells, which *should* go with the alternate screen, but erasing them
        // first is cheap and does not depend on that being true of every build.
        let protocol = crate::preview::graphics::protocol();
        if let Some(seq) = crate::preview::graphics::clear_sequence(protocol) {
            let _ = stdout.write_all(seq.as_bytes());
        } else if crate::preview::graphics::needs_region_erase(protocol) {
            // Erase the whole alternate screen rather than tracking where the
            // image was: this runs once, on the way out.
            let _ = stdout.write_all(b"\x1b[2J");
        }
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
    /// Regex rename over the tagged files, opened by `gr`.
    Rename(crate::widget::rename_dialog::RenameDialog),
}

/// Context for modal operations.
pub enum ModalTarget {
    Delete { paths: Vec<PathBuf> },
    Rename { old_path: PathBuf },
    ChangeRoot,
    Search,
    /// Regex search within the previewed file, rather than across the tree.
    PreviewSearch,
    /// Regex filter prompt for the cursor's directory.
    Filter,
    /// New-directory-name prompt; created in the cursor's current directory.
    CreateDir,
    /// Single-panel copy: prompt for a destination directory, then copy `srcs`
    /// into it (with per-collision confirmation).
    CopyDest { srcs: Vec<PathBuf> },
    /// Per-file overwrite confirmation while draining `pending_copies`.
    CopyOverwrite { src: PathBuf, dest: PathBuf },
    /// Per-file overwrite confirmation for a queued cross-backend transfer.
    ///
    /// Carries `is_dir` rather than the destination path: the batch already
    /// knows the destination directory, and the worker needs the kind to decide
    /// whether to expand the entry.
    TransferOverwrite { src: PathBuf, is_dir: bool },
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
    /// A listing of a typed destination, in flight. Resolved on tick, like a
    /// connection attempt: the check cannot block the key handler.
    dest_probe: Option<DestProbeTask>,
    /// A queued transfer batch waiting on overwrite confirmations.
    ///
    /// The queued path needs the same per-file prompt the local one has: the
    /// worker replaces an existing destination on the stated assumption that
    /// "the overwrite decision was made by the caller before queueing", and the
    /// cross-backend caller was not making it — a remote-to-local copy silently
    /// overwrote. Held here because the answer arrives over several key events,
    /// exactly like `pending_copies`.
    pending_transfer: Option<PendingTransferBatch>,
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
    /// Archives opened this session, keyed by the container's path.
    ///
    /// The registry cannot unregister, so re-entering an archive has to reuse
    /// its backend: browsing in and out of one zip a dozen times would
    /// otherwise leave a dozen copies of its index resident, each with its own
    /// driver thread. Re-entry is also then instant, since the index is built.
    archive_backends: std::collections::HashMap<PathBuf, crate::widget::source::Source>,
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
    /// An archive being indexed off the event loop.
    archive_open_task: Option<ArchiveOpenTask>,
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
    /// Where the left button went down while the preview is open, and whether
    /// the pointer has moved since.
    ///
    /// The preview acts on *release*, not press, so that a click-drag can be
    /// told from a click: a terminal reports a drag as `Down`, then `Drag`
    /// events, then `Up`, and the `Down` alone is indistinguishable from a
    /// click that is about to end where it started.
    preview_press: Option<PreviewPress>,
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
    /// The file preview pane, shown over everything else when open.
    ///
    /// App-level rather than per-panel, like the transfer sidebar: it fills the
    /// screen and shows one file, so it has no meaningful per-panel identity. It
    /// follows the active panel's cursor.
    preview: crate::widget::preview::PreviewState,
    preview_open: bool,
    /// Whether the preview has keyboard focus instead of a panel or the sidebar.
    ///
    /// Kept exclusive with `transfer_focused` by [`Self::focus_preview`] and the
    /// Tab rotation — two focused panes would draw two active borders.
    preview_focused: bool,
    /// The in-flight preview load, and a flag for the task to observe when its
    /// result is no longer wanted.
    ///
    /// Moving the cursor supersedes a load rather than queueing behind it, or a
    /// held-down `j` would replay every file it passed over.
    preview_task: Option<PreviewTask>,
    /// What high-resolution image is currently on the terminal: where it was put,
    /// how big its payload was, and which page it was.
    ///
    /// Graphics escapes are written outside the frame, so nothing tracks them for
    /// us. Without this the idle 10Hz loop would re-send the whole payload every
    /// tick, which flickers and floods the terminal.
    preview_graphics_shown: Option<(ratatui::layout::Rect, usize, usize)>,
    /// Set when the screen has been written to behind ratatui's back and its
    /// buffer no longer describes what is displayed.
    ///
    /// Erasing an image writes over cells ratatui still believes it drew, so the
    /// next frame has to be a full repaint rather than a diff.
    force_repaint: bool,
    /// How to browse a directory that has no remembered traversal mode of its
    /// own. Seeded from `-s` and moved by the `S` toggle.
    ///
    /// `-s` is a statement about this session, not about the one directory it
    /// opened on: drilling into a subdirectory used to fall back to a full scan,
    /// which is exactly the recursive walk the flag was asking to avoid. A
    /// directory with a recorded preference still wins over this.
    shallow_default: bool,
    /// Whether the user has asked not to be prompted before deleting.
    ///
    /// Deliberately in memory only, and never written to the catalog: it lasts
    /// for this run of the app and is gone on the next one. Turning off the
    /// guard on an irreversible operation should be a thing you opt into while
    /// you are doing it, not a setting you can leave on and forget about.
    skip_delete_confirm: bool,
    /// How many times the terminal bell has been rung. Tests cannot hear it, so
    /// they count it instead.
    bells_rung: usize,
}

/// A preview load running in the background.
struct PreviewTask {
    key: crate::widget::preview::PreviewKey,
    rx: tokio::sync::oneshot::Receiver<crate::preview::PreviewContent>,
    /// Dropped when a newer load starts; the task checks it before doing work.
    cancel: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

/// A connection attempt running in the background, with a channel for its result.
struct ConnectTask {
    rx: tokio::sync::oneshot::Receiver<ConnectResult>,
    /// The panel that was active when the connect was issued — the remote opens
    /// here on success, so connecting replaces the pane the user was looking at.
    target_panel: usize,
}

/// An archive being indexed in the background, with a channel for its result.
///
/// Indexing is CPU-bound and proportional to the member count — a hundred
/// thousand members is a few hundred milliseconds even at its fastest — so it
/// cannot run in the key handler. It used to, which is what made `Enter` on a
/// large archive freeze the interface until it finished.
/// A left button held down over the preview pane.
///
/// Held from `Down` until `Up` so the release can tell a click from a drag —
/// see [`FileBrowser::preview_press`].
struct PreviewPress {
    /// Where the button went down, to compare against where it comes up.
    at: (u16, u16),
    /// Set by any `Drag` event, so a drag that returns to its starting cell is
    /// still a drag. Position alone would call that a click.
    dragged: bool,
}

struct ArchiveOpenTask {
    rx: tokio::sync::oneshot::Receiver<anyhow::Result<crate::vfs::archive::ArchiveFs>>,
    /// The container being opened, to register it under when it arrives.
    container: PathBuf,
    /// The panel that asked, so the archive opens where the user was looking.
    target_panel: usize,
}

/// A listing of a typed copy destination, in flight.
///
/// A destination typed into the single-panel prompt is not on screen, so there
/// is no loaded tree to check names against. One `read_dir` answers the whole
/// batch — cheaper than a stat per file, and the only option for a remote
/// destination, where the check cannot be synchronous at all.
struct DestProbeTask {
    rx: tokio::sync::oneshot::Receiver<Vec<String>>,
    /// The batch waiting on the answer, minus the collision split.
    srcs: Vec<PathBuf>,
    kinds: Vec<bool>,
    src_backend: crate::vfs::BackendId,
    dest_dir: PathBuf,
    dest_backend: crate::vfs::BackendId,
    source_panel: usize,
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

/// A cross-backend copy batch part-way through its overwrite confirmations.
///
/// Everything `enqueue_cross_backend_copy` needs, held while the colliding files
/// are put to the user one at a time. `approved` collects what survives; the
/// batch is enqueued only once `pending` is empty.
struct PendingTransferBatch {
    /// Colliding (src, is_dir) entries still to be asked about.
    pending: Vec<(PathBuf, bool)>,
    /// (src, is_dir) entries cleared to transfer.
    approved: Vec<(PathBuf, bool)>,
    src_backend: crate::vfs::BackendId,
    dest_dir: PathBuf,
    dest_backend: crate::vfs::BackendId,
    source_panel: usize,
}

impl FileBrowser {
    /// Build the app. `left`/`right` are the two panels' starting directories;
    /// dual mode is enabled by the `--dual` flag *or* by supplying a right path.
    pub fn new(left: Option<PathBuf>, right: Option<PathBuf>, dual: bool) -> Self {
        Self::new_shallow(left, right, dual, false)
    }

    /// As [`Self::new`], opening every panel without measuring directory sizes
    /// when `shallow` — the `-s` flag.
    ///
    /// Applies to both panes: the flag says how you want to browse, and a split
    /// where one side measured and the other did not would be arbitrary. A
    /// remote pane ignores it because remote trees are never measured anyway.
    pub fn new_shallow(
        left: Option<PathBuf>,
        right: Option<PathBuf>,
        dual: bool,
        shallow: bool,
    ) -> Self {
        let mut panels = vec![Panel::new_maybe_shallow(left, shallow)];
        if dual || right.is_some() {
            panels.push(Panel::new_maybe_shallow(right, shallow));
        }
        let mut app = Self::with_panels(panels);
        // The flag describes the session, so every directory opened later
        // without a preference of its own starts out this way too.
        app.shallow_default = shallow;
        app
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
            pending_transfer: None,
            dest_probe: None,
            copy_source_panel: 0,
            backends: BackendRegistry::new(),
            archive_backends: std::collections::HashMap::new(),
            transfers: TransferQueue::default(),
            transfer_panel_override: None,
            connect_task: None,
            archive_open_task: None,
            pending_connect: None,
            hosts: HostCatalog::load(),
            connecting_label: None,
            mouse_captured: false,
            panel_areas: Vec::new(),
            last_frame: ratatui::layout::Rect::new(0, 0, 0, 0),
            last_click: None,
            preview_press: None,
            transfer_focused: false,
            transfer_cursor: None,
            transfer_rows: Default::default(),
            transfer_area: None,
            quit_requested: false,
            preview: crate::widget::preview::PreviewState::new(),
            preview_open: false,
            preview_focused: false,
            preview_task: None,
            preview_graphics_shown: None,
            force_repaint: false,
            shallow_default: false,
            skip_delete_confirm: false,
            bells_rung: 0,
        }
    }

    /// As [`Self::new_on_picker`], but over a supplied catalog rather than the
    /// one on disk — the picker is built during construction, so a test cannot
    /// swap the catalog in afterwards.
    pub fn new_on_picker_with_hosts_for_test(hosts: HostCatalog) -> Self {
        let mut browser = Self::with_panels(Vec::new());
        browser.hosts = hosts;
        let mut picker = crate::screen::DirPickerState::with_catalog(&browser.hosts);
        picker.set_traversal_context(
            browser.shallow_default,
            Self::dir_shallow_prefs(&browser.hosts),
        );
        browser.panels = vec![Panel::new_on_screen(Screen::DirPicker(picker))];
        browser
    }

    /// Start on the picker rather than a directory, for `myd --directory`.
    ///
    /// Built here rather than in `Panel::new` because the picker has to list the
    /// saved directories and hosts, and the catalog is not loaded until this
    /// constructor has run — a panel-built picker would come up empty, which is
    /// the one thing the flag exists to avoid.
    pub fn new_on_picker() -> Self {
        Self::new_on_picker_shallow(false)
    }

    /// As [`Self::new_on_picker`], carrying the `-s` flag into the session.
    ///
    /// `-d` opens nothing, so there is no panel for the flag to apply to yet —
    /// but it still describes how the directory the picker eventually chooses
    /// should be opened. Dropping it here made `myd -s -d` measure everything,
    /// which is exactly what `-s` was asked to prevent.
    pub fn new_on_picker_shallow(shallow: bool) -> Self {
        // No panels yet: the picker has to list the saved directories and hosts,
        // and the catalog is not read until the app is built. Starting a panel on
        // the current directory first would spawn a full walk for a tree that is
        // about to be replaced.
        let mut browser = Self::with_panels(Vec::new());
        browser.shallow_default = shallow;
        let mut picker = crate::screen::DirPickerState::with_catalog(&browser.hosts);
        picker.set_traversal_context(shallow, Self::dir_shallow_prefs(&browser.hosts));
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
        // Ask the terminal what image protocols it supports before taking over
        // the screen. The query is answered on the same channel as keystrokes, so
        // running it later would deliver the reply into the event loop as if the
        // user had typed it. Cached, so this is the only time it costs anything.
        let _ = crate::preview::graphics::protocol();

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
                            // A plain move carries no meaning, and acting on
                            // each would flood the loop. A drag does carry one
                            // thing worth knowing — that this gesture is a
                            // drag and not a click — so it goes through, and
                            // `route_mouse` records it without doing work.
                            MouseEventKind::Moved => {}
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
                } else if self.preview_open {
                    // The preview covers the panels, so the wheel belongs to it
                    // whenever it is up — scrolling a tree the user cannot see
                    // would be the wrong answer.
                    self.preview.scroll_by(pending_scroll as isize);
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
            // And any archive being indexed off the event loop.
            self.resolve_archive_open();
            // And any listing of a typed copy destination.
            self.resolve_dest_probe();
            // Install a finished preview, and start one if the cursor has moved
            // onto a different file.
            self.resolve_preview();
            self.request_preview();

            let after_resolve = trace_started_now(tick_started);
            // Erasing an image writes over cells ratatui still believes it drew,
            // so its buffer no longer describes the screen and a diff would leave
            // the erased region blank. `clear` discards that belief and forces the
            // next draw to emit everything.
            if std::mem::take(&mut self.force_repaint) {
                terminal.clear()?;
            }
            terminal.draw(|f| self.draw(f))?;
            // A high-resolution image is escape data the terminal draws itself, so
            // it can only go out after the frame has been flushed — ratatui would
            // otherwise overwrite the cells it lands on. The pane leaves a blank
            // hole for it during draw.
            self.flush_preview_graphics();

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
        // what's left, so it is independent of single/dual mode. It yields
        // entirely on a narrow terminal.
        //
        // Only over the rows above the footer. Each panel reserves its own
        // bottom row for the footer *inside* the area it is handed, so a sidebar
        // spanning the full height sat beside that row and clipped it — the
        // keybindings lost their tail ("?:help q:quit") to the sidebar's width
        // whenever the queue was on screen. The footer is one line describing
        // the whole window, so it gets the whole width.
        let (area, transfer_area) = match show_transfers
            .then(|| transfer_panel::desired_width(full.width))
            .flatten()
        {
            Some(w) => {
                let cols =
                    Layout::horizontal([Constraint::Min(1), Constraint::Length(w)]).split(full);
                // The sidebar stops one row short of the bottom, leaving that
                // row clear right across the frame for the footer. The panels
                // still get the left column — tree and all — and are told
                // separately (via `footer_width`) that their bottom row may run
                // the full width.
                let sidebar = Rect {
                    height: cols[1].height.saturating_sub(1),
                    ..cols[1]
                };
                (cols[0], Some(sidebar))
            }
            None => (full, None),
        };
        // How wide the footer row may be drawn, which is the whole terminal
        // whenever the sidebar has stepped out of that row. `None` leaves each
        // panel's footer at its own width, which is what a split wants.
        let footer_width = transfer_area.map(|_| full.width);

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
        // A half-typed chord, shown in the status bar. Read once here: the key
        // handler is borrowed immutably while the panels are borrowed mutably
        // below.
        let pending_chord = self.key_handler.pending_chord();

        // Focus lives in one place: a browser panel, or the transfer sidebar.
        // `state.active` used to mean "is the active panel index", which stopped
        // being true once the sidebar became focusable — both it and the last
        // panel drew a cyan border at once.
        let panel_has_focus = !self.transfer_focused;
        // Read once here for the same reason `pending_chord` is: the panels are
        // borrowed mutably below, so `self` cannot be consulted inside the loop.
        let transfer_focused = self.transfer_focused;

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
                    state.pending_chord = pending_chord;
                    // There is one keyboard, so exactly one set of keys may be
                    // on screen. Each panel owns a footer, so with the sidebar
                    // focused the active panel draws its keys and the others
                    // draw nothing at all — gating on `i == active` alone left
                    // the other panel still advertising the tree's keys beside
                    // the sidebar's line, and dropping the gate had both panels
                    // draw the sidebar's line twice.
                    state.footer = if transfer_focused {
                        if i == active {
                            FooterMode::Transfers
                        } else {
                            FooterMode::Hidden
                        }
                    } else {
                        FooterMode::Own
                    };
                    // Only the right-hand panel abuts the sidebar, so only its
                    // footer has the reclaimed columns to grow into. Widening
                    // both would draw the left one straight over the right.
                    state.footer_width = if i + 1 == panel_count {
                        footer_width.map(|w| w.saturating_sub(cols[i].x))
                    } else {
                        None
                    };
                }
                panel.current_screen_mut().render(f, cols[i]);
            }
        } else {
            self.panel_areas = vec![area];
            let backend = self.panels[0].backend;
            if let Screen::Main(state) = self.panels[0].current_screen_mut() {
                state.active = panel_has_focus;
                state.pending_ghosts = ghosts_for_panel(&pending, backend, state.root_path());
                state.pending_chord = pending_chord;
                // The sole panel, so it is the one that speaks for the sidebar.
                state.footer = if transfer_focused {
                    FooterMode::Transfers
                } else {
                    FooterMode::Own
                };
                state.footer_width = footer_width;
            }
            self.panels[0].current_screen_mut().render(f, area);
        }

        // The preview covers the panels, so it is drawn after them and before any
        // modal. Drawn over the whole frame rather than the panel column: it is a
        // view of one file, not of one panel.
        if self.preview_open {
            let focused = self.preview_focused;
            crate::widget::preview::render(f, preview_area(full), &mut self.preview, focused);
        } else {
            // Focus must never rest on something that is not drawn — the same
            // rule the transfer sidebar follows above.
            self.preview_focused = false;
            self.preview.content_area = None;
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
            Modal::Rename(d) => d.render(f, full),
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
            //
            // Only an explicitly recorded preference forces a re-open;
            // `shallow_default` is deliberately not consulted. Arrivals already
            // apply it when they build the loading screen, so a measured tree
            // reaching this point in a shallow session is one the user just
            // switched to with `S` — re-opening it would undo the toggle.
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


    /// Act on a confirm dialog's answer, however it was given.
    ///
    /// Shared by the key handler and the mouse handler: a click on a button has
    /// to mean exactly what pressing that button's key means, and two copies of
    /// this match would drift.
    fn apply_confirm_answer(&mut self, answer: crate::widget::confirm_dialog::Answer) {
        use crate::widget::confirm_dialog::Answer;
        let result = answer == Answer::Yes;
        self.modal = Modal::None;
        match self.modal_target.take() {
            // The delete prompt answers with letters rather than yes/no, so that
            // "always" can sit alongside them as a third button. 'a' both
            // consents to this delete and stops the asking; anything else
            // (including Esc, which the caller maps to a cancelling choice)
            // leaves the files alone.
            Some(ModalTarget::Delete { paths }) => match answer {
                Answer::Choice('a') => {
                    self.skip_delete_confirm = true;
                    self.spawn_delete_batch(paths);
                }
                Answer::Choice('y') | Answer::Yes => self.spawn_delete_batch(paths),
                _ => {}
            },
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
                        // The error is now the visible dialog; do not
                        // reopen the picker over it.
                        return;
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
            Some(ModalTarget::TransferOverwrite { src, is_dir }) => {
                // Same rule for a queued transfer: confirmed files
                // join the batch, declined ones are dropped.
                if result {
                    if let Some(batch) = self.pending_transfer.as_mut() {
                        batch.approved.push((src, is_dir));
                    }
                }
                self.prompt_next_transfer();
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
        self.resolve_archive_open();
        self.resolve_dest_probe();
        self.resolve_loading();
        self.resolve_deleting();
        self.resolve_copying();
        self.advance_transfers();
        self.resolve_preview();
        self.request_preview();
    }

    /// Whether a copy destination is still being listed in the background.
    ///
    /// Pressing `c` does not decide anything immediately: the destination is
    /// listed off the event loop and the collision prompt (or the queueing) only
    /// happens once `resolve_dest_probe` sees the result. A test that asserts
    /// straight after the keypress races that listing, so it needs to be able to
    /// wait for it.
    pub fn dest_probe_pending_for_test(&self) -> bool {
        self.dest_probe.is_some()
    }

    /// Whether a full repaint has been requested and not yet consumed.
    ///
    /// The event loop takes this flag and calls `Terminal::clear`, which the test
    /// harness does not run — so a test observes the request rather than its
    /// effect.
    pub fn force_repaint_pending_for_test(&self) -> bool {
        self.force_repaint
    }

    /// Whether the preview pane is open, and whether it has focus (for tests).
    pub fn preview_open_for_test(&self) -> bool {
        self.preview_open
    }

    pub fn preview_focused_for_test(&self) -> bool {
        self.preview_focused
    }

    /// The preview's scroll offset, for asserting that motions move it.
    pub fn preview_scroll_for_test(&self) -> usize {
        self.preview.scroll()
    }

    /// How many lines the current search matched.
    pub fn preview_match_count_for_test(&self) -> usize {
        self.preview.match_count()
    }

    /// The line the current search match sits on.
    pub fn preview_match_line_for_test(&self) -> Option<usize> {
        self.preview.current_match_line()
    }

    /// The document page the preview is showing, zero-based (for tests).
    pub fn preview_page_for_test(&self) -> usize {
        self.preview.current_page()
    }

    /// Whether the preview is showing a single image (for tests).
    pub fn preview_is_single_image_for_test(&self) -> bool {
        self.preview.is_single_image()
    }

    /// Whether the preview is treating its content as a paged document.
    pub fn preview_is_paged_for_test(&self) -> bool {
        self.preview.is_paged()
    }

    /// Whether a preview has finished loading and has content to draw.
    pub fn preview_has_content_for_test(&self) -> bool {
        self.preview.has_content()
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

    /// The text of whichever modal is up, for tests that assert a refusal says
    /// the right thing — the wording *is* the feature for a guard.
    pub fn modal_message_for_test(&self) -> Option<String> {
        match &self.modal {
            Modal::Confirm(d) => Some(d.message.clone()),
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
            Modal::Rename(_) => "rename",
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

    /// A panel's current screen (for tests). `panel_current_dir` answers where a
    /// panel is; this answers what it is showing.
    pub fn panel_screen_for_test(&self, index: usize) -> Option<&Screen> {
        self.panels.get(index).map(|p| p.current_screen())
    }

    /// How many screens deep a panel's stack is (for tests).
    /// Whether an archive is still being indexed (for tests, which have to wait
    /// for it rather than assert against the panel it has not replaced yet).
    pub fn archive_opening_for_test(&self) -> bool {
        self.archive_open_task.is_some()
    }

    pub fn panel_depth_for_test(&self, index: usize) -> usize {
        self.panels.get(index).map(|p| p.depth()).unwrap_or(0)
    }

    /// The active panel's remembered sort order (for tests) — the one carried
    /// onto screens opened later, as distinct from the current tree's.
    pub fn view_prefs_sort_mode_for_test(&self) -> crate::screen::SortMode {
        self.active_panel().view_prefs.sort_mode
    }

    /// The active panel's filter pattern, if one is masking the tree (for tests).
    pub fn filter_pattern_for_test(&self) -> Option<String> {
        match self.active_panel().current_screen() {
            Screen::Main(state) => state.tree.filter_pattern().map(str::to_string),
            _ => None,
        }
    }

    /// Which backend a panel's paths are addressed on (for tests).
    pub fn panel_backend_for_test(&self, index: usize) -> crate::vfs::BackendId {
        self.panels
            .get(index)
            .map(|p| p.backend)
            .unwrap_or(crate::vfs::BackendId::LOCAL)
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

        // A click inside the rename form focuses the field it landed on; one
        // outside is swallowed rather than dismissing, since a stray click must
        // not discard half-typed patterns.
        if matches!(self.modal, Modal::Rename(_)) {
            if matches!(ev.kind, MouseEventKind::Down(MouseButton::Left)) {
                let Modal::Rename(dialog) = &mut self.modal else {
                    return true;
                };
                let outcome = dialog.click_at(x, y);
                return self.apply_rename_dialog_outcome(outcome);
            }
            return true;
        }

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
        // A click on a confirm dialog's button is that button's answer. Clicks
        // anywhere else in (or outside) the dialog are swallowed: it is a
        // question, and a stray click must not answer it.
        if matches!(self.modal, Modal::Confirm(_)) {
            if matches!(ev.kind, MouseEventKind::Down(MouseButton::Left)) {
                let Modal::Confirm(dialog) = &mut self.modal else {
                    return true;
                };
                if let Some(answer) = dialog.click_at(x, y) {
                    self.apply_confirm_answer(answer);
                }
            }
            return true;
        }
        if !matches!(self.modal, Modal::None) {
            return true;
        }

        // The preview covers the panels, so a click belongs to it whenever it is
        // up — the same rule the scroll wheel already follows. Without this the
        // click fell through to the tree hidden underneath, moving a cursor the
        // user could not see and loading a different file into the pane they
        // were reading.
        //
        // A left click advances like `j`, which is what the pane's own binding
        // does: turn the page on a paged document, scroll a line on a long one,
        // and step to the next file on a single image. Routing it through the
        // key handler rather than reimplementing it means the click cannot drift
        // from what `j` does.
        if self.preview_open {
            match ev.kind {
                // Press only arms the gesture. Acting here would advance on the
                // first event of a click-*drag* too, since a terminal opens
                // both the same way — which is the whole difference being drawn.
                MouseEventKind::Down(MouseButton::Left) => {
                    self.preview_press = Some(PreviewPress {
                        at: (x, y),
                        dragged: false,
                    });
                }
                // Any drag disqualifies the gesture, including one that wanders
                // back to where it started.
                MouseEventKind::Drag(MouseButton::Left) => {
                    if let Some(p) = self.preview_press.as_mut() {
                        p.dragged = true;
                    }
                }
                MouseEventKind::Up(MouseButton::Left) => {
                    let Some(press) = self.preview_press.take() else {
                        return true;
                    };
                    // A release somewhere else is a drag whose intermediate
                    // events never arrived — some terminals report only the
                    // ends — so position is checked as well as the flag.
                    if press.dragged || press.at != (x, y) {
                        return true;
                    }
                    // Clicking is also a way of saying "I am reading this", so
                    // it takes focus first — otherwise the first click would
                    // only move focus and the second would advance, which is
                    // not what a click on a page looks like.
                    self.focus_preview();
                    self.handle_preview_key(KeyEvent::new(
                        KeyCode::Char('j'),
                        crossterm::event::KeyModifiers::NONE,
                    ));
                }
                _ => {}
            }
            return true;
        }
        // Not over the preview any more, so an armed gesture is stale.
        self.preview_press = None;

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
        // Two focused panes would draw two active borders and both would answer
        // to `j`.
        self.preview_focused = false;
        if self.transfer_cursor.is_none() {
            self.transfer_cursor = self.transfer_rows.ids().first().copied();
        }
    }

    /// Give the preview pane keyboard focus.
    fn focus_preview(&mut self) {
        if !self.preview_open {
            return;
        }
        self.preview_focused = true;
        self.transfer_focused = false;
        self.transfer_cursor = None;
    }

    /// Open or close the preview pane (space).
    ///
    /// Opening focuses it: it covers the screen, so leaving focus on the tree
    /// behind it would mean `j` scrolling something the user cannot see.
    fn toggle_preview(&mut self) {
        if self.preview_open {
            self.close_preview();
        } else {
            self.preview_open = true;
            self.focus_preview();
            self.request_preview();
        }
    }

    /// Repaint everything from scratch (Ctrl+L).
    ///
    /// The escape hatch for a screen that has been corrupted by something
    /// outside our control — a stray write from another process, a resize the
    /// terminal reported oddly, a graphics escape a terminal did not fully
    /// understand. ratatui normally emits only the cells that changed, so a
    /// screen that no longer matches its buffer stays wrong until something
    /// happens to overwrite each damaged cell; this discards that assumption.
    ///
    /// Any image on screen is forgotten as well as cleared, so the next frame
    /// draws it again rather than assuming it survived.
    fn redraw(&mut self) {
        if let Some((area, _, _)) = self.preview_graphics_shown.take() {
            self.erase_graphics(area);
        }
        self.force_repaint = true;
    }

    fn close_preview(&mut self) {
        self.preview_open = false;
        self.preview_focused = false;
        // Abandon any load in flight — its result is for a pane that is gone.
        self.cancel_preview_task();
    }

    fn cancel_preview_task(&mut self) {
        if let Some(t) = self.preview_task.take() {
            t.cancel.store(true, std::sync::atomic::Ordering::Relaxed);
        }
    }

    /// The geometry the preview will be drawn at, matching [`Self::preview_area`].
    ///
    /// Needed before rendering because an image is rendered by an external tool at
    /// a fixed cell size, so the size has to be known when the load starts.
    fn preview_cells(&self) -> (u16, u16) {
        let area = preview_area(self.last_frame);
        // Less the border on each side, and the footer row.
        (
            area.width.saturating_sub(2),
            area.height.saturating_sub(3).max(1),
        )
    }

    /// Start loading a preview of the active panel's selection, if that is not
    /// already what the pane is showing.
    fn request_preview(&mut self) {
        if !self.preview_open {
            return;
        }
        let Some(path) = self.panels[self.active].selected_resolved_path() else {
            return;
        };
        let backend = self.panels[self.active].backend;
        let (cols, rows) = self.preview_cells();
        // Moving to a different file starts at its first page, rather than
        // carrying over a page number that meant something in another document.
        if self.preview.showing_other_path(&path) {
            self.preview.reset_page();
        }
        let key = crate::widget::preview::PreviewKey {
            path: path.clone(),
            backend,
            cols,
            rows,
            page: self.preview.current_page(),
        };

        // Already showing it, or already fetching it.
        if self.preview.key() == Some(&key) {
            return;
        }
        if self.preview_task.as_ref().is_some_and(|t| t.key == key) {
            return;
        }

        // A newer request supersedes the one in flight.
        self.cancel_preview_task();

        let label = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| path.display().to_string());
        self.preview.begin_load(label);

        let fs = self.backends.get(backend);
        let cancel = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let (tx, rx) = tokio::sync::oneshot::channel();
        let req = crate::preview::PreviewRequest {
            path: crate::vfs::VPath::new(backend, path.clone()),
            label: path,
            cols,
            rows,
            page: key.page,
        };
        let flag = cancel.clone();
        tokio::spawn(async move {
            // Nothing here may run on the event loop: reading a remote file is
            // round trips, and an image renderer is a process.
            if flag.load(std::sync::atomic::Ordering::Relaxed) {
                return;
            }
            let content = crate::preview::load(fs, req).await;
            if !flag.load(std::sync::atomic::Ordering::Relaxed) {
                let _ = tx.send(content);
            }
        });

        self.preview_task = Some(PreviewTask { key, rx, cancel });
    }

    /// Write a high-resolution preview image to the terminal.
    ///
    /// Must run *after* `terminal.draw` has flushed: kitty and iTerm2 graphics are
    /// escape payloads rather than cells, so ratatui knows nothing about them and
    /// would paint over the area on the next frame. The pane leaves that area blank
    /// on purpose (see [`crate::widget::preview`]).
    ///
    /// Only emitted when something changed. The event loop runs at 10Hz even when
    /// idle, and re-sending a few hundred kilobytes of base64 every tick would
    /// flood the terminal and make the image flicker.
    fn flush_preview_graphics(&mut self) {
        // Taken by value: erasing the old image needs `&mut self`, which cannot
        // coexist with a payload borrowed out of `self.preview`.
        let wanted = match (&self.preview.graphics_area, self.preview.graphics()) {
            (Some(area), Some(payload)) if self.preview_open => {
                Some((*area, payload.to_string()))
            }
            _ => None,
        };

        let Some((area, payload)) = wanted else {
            // Nothing to show any more, so remove whatever is still on screen.
            if let Some((old, _, _)) = self.preview_graphics_shown.take() {
                self.erase_graphics(old);
            }
            return;
        };

        // Identify what is on screen by where it is and what it is, so a resize,
        // a page turn or a different file all re-emit, and an idle tick does not.
        let stamp = (area, payload.len(), self.preview.current_page());
        if self.preview_graphics_shown == Some(stamp) {
            return;
        }

        // Remove whatever was there first, or a smaller image drawn over a larger
        // one leaves the old edges showing.
        if let Some((old, _, _)) = self.preview_graphics_shown {
            self.erase_graphics(old);
        }

        // Place the cursor at the top-left of the hole: every protocol draws from
        // the cursor position.
        let placed = format!("\x1b[{};{}H", area.y + 1, area.x + 1);
        if self.write_graphics(&placed) && self.write_graphics(&payload) {
            self.preview_graphics_shown = Some(stamp);
        }
    }

    /// Remove an image previously drawn at `area`.
    ///
    /// How depends on the protocol. kitty images are objects with a delete
    /// operation, so one escape removes every placement. iTerm2 and sixel have no
    /// such operation — the image belongs to the cells it was drawn into — so the
    /// only way to get rid of one is to erase those cells.
    ///
    /// Redrawing the frame does not do it. ratatui writes only the cells whose
    /// content changed, and the cells under an image are ones it believes are
    /// already blank, so it emits nothing for them and the picture survives. That
    /// is the ghost left behind in a native iTerm2 pane.
    ///
    /// After erasing, the region has to be repainted or the pane's own border and
    /// text would be missing too, so this asks for a full redraw on the next tick.
    fn erase_graphics(&mut self, area: ratatui::layout::Rect) {
        use crate::preview::graphics;

        let protocol = graphics::protocol();
        if let Some(seq) = graphics::clear_sequence(protocol) {
            self.write_graphics(seq);
            return;
        }
        if !graphics::needs_region_erase(protocol) {
            return;
        }

        self.write_graphics(&region_erase_sequence(area));
        // The cells just erased include whatever the pane had drawn there, so the
        // next frame must be a full repaint rather than a diff against a buffer
        // that no longer matches the screen.
        self.force_repaint = true;
    }

    /// Write an escape sequence straight to the terminal, wrapping it for tmux
    /// when that is what it takes to get through.
    ///
    /// tmux swallows an escape it does not understand unless it is wrapped in a
    /// passthrough envelope — but it understands sixel natively and parses it
    /// itself, so wrapping *that* would hide the image from the very thing meant
    /// to draw it. Only the protocols tmux does not know get wrapped.
    fn write_graphics(&self, payload: &str) -> bool {
        use crate::preview::graphics::{self, Protocol};
        use std::io::Write;

        // Cursor positioning is ordinary CSI that tmux handles itself; only the
        // image-carrying sequences need the envelope.
        let needs_wrap = payload.contains("\x1b_") || payload.contains("\x1b]");
        let body = match (
            needs_wrap && std::env::var_os("TMUX").is_some(),
            graphics::protocol(),
        ) {
            (true, Protocol::Kitty) | (true, Protocol::Iterm2) => {
                std::borrow::Cow::Owned(graphics::wrap_for_tmux(payload))
            }
            _ => std::borrow::Cow::Borrowed(payload),
        };

        let mut out = std::io::stdout();
        out.write_all(body.as_bytes()).is_ok() && out.flush().is_ok()
    }

    /// Install a finished preview load. Called once per tick.
    fn resolve_preview(&mut self) {
        let Some(task) = self.preview_task.as_mut() else {
            return;
        };
        match task.rx.try_recv() {
            Ok(content) => {
                let key = task.key.clone();
                self.preview_task = None;
                self.preview.set_content(key, content);
            }
            Err(tokio::sync::oneshot::error::TryRecvError::Empty) => {}
            Err(tokio::sync::oneshot::error::TryRecvError::Closed) => {
                // The task died or was superseded; stop waiting on it.
                self.preview_task = None;
            }
        }
    }

    /// Keys the preview pane handles while focused.
    ///
    /// Returns `None` when the key is not one of them, so global keys like `?`
    /// and Ctrl+C still work — the same contract as the transfer sidebar.
    ///
    /// `Esc` *must* be handled here: globally it resolves to [`Action::Quit`], so
    /// without this the obvious way to leave the pane would exit the app.
    fn handle_preview_key(&mut self, key: KeyEvent) -> Option<bool> {
        if !self.preview_open {
            return None;
        }

        // `q` closes the pane whether or not it has focus. Unfocused it would
        // otherwise fall through to the global binding and quit the app, which is
        // a surprising amount to lose when a preview is on screen and `q` is the
        // obvious way to dismiss it.
        if matches!(key.code, KeyCode::Char('q')) && key.modifiers.is_empty() {
            self.close_preview();
            return Some(true);
        }

        if !self.preview_focused {
            return None;
        }

        // The pane's own paging keys shadow the tree's, which is the point of
        // focus: Ctrl+F pages whatever the user is looking at. On a multi-page
        // document that means a document page, not a screenful.
        if key.modifiers.contains(KeyModifiers::CONTROL) {
            let paged = self.preview.is_paged();
            // A single image has neither pages nor anything to scroll, so the
            // paging keys move the tree, as j/k do below.
            let image = self.preview.is_single_image();
            match key.code {
                KeyCode::Char('f') => {
                    if image {
                        self.dispatch_action(Action::PageDown);
                    } else if paged {
                        self.preview.step_page(true);
                    } else {
                        self.preview.page(true);
                    }
                    return Some(true);
                }
                KeyCode::Char('b') => {
                    if image {
                        self.dispatch_action(Action::PageUp);
                    } else if paged {
                        self.preview.step_page(false);
                    } else {
                        self.preview.page(false);
                    }
                    return Some(true);
                }
                KeyCode::Char('d') => {
                    self.preview.half_page(true);
                    return Some(true);
                }
                KeyCode::Char('u') => {
                    self.preview.half_page(false);
                    return Some(true);
                }
                // Anything else Ctrl- falls through to the global bindings.
                _ => return None,
            }
        }

        // A rendered page has nothing to scroll — it is drawn to fit — so on a
        // multi-page document the motions turn pages instead. Requesting the new
        // page is left to the tick, which already reloads when the key changes.
        // A single image is drawn to fit and has nothing to scroll, so the
        // motions move the *tree's* cursor and the preview follows to whatever is
        // selected next. That makes j/k a way to flick through a directory of
        // pictures, which is what they are reaching for on an image.
        if self.preview.is_single_image() {
            match key.code {
                KeyCode::Char('j') | KeyCode::Down => {
                    self.dispatch_action(Action::CursorDown);
                    return Some(true);
                }
                KeyCode::Char('k') | KeyCode::Up => {
                    self.dispatch_action(Action::CursorUp);
                    return Some(true);
                }
                _ => {}
            }
        }

        if self.preview.is_paged() {
            match key.code {
                KeyCode::Char('j') | KeyCode::Down | KeyCode::PageDown => {
                    self.preview.step_page(true);
                    return Some(true);
                }
                KeyCode::Char('k') | KeyCode::Up | KeyCode::PageUp => {
                    self.preview.step_page(false);
                    return Some(true);
                }
                KeyCode::Char('g') | KeyCode::Home => {
                    self.preview.to_page_edge(false);
                    return Some(true);
                }
                KeyCode::Char('G') | KeyCode::End => {
                    self.preview.to_page_edge(true);
                    return Some(true);
                }
                _ => {}
            }
        }

        match key.code {
            KeyCode::Char('j') | KeyCode::Down => {
                self.preview.scroll_by(1);
                Some(true)
            }
            KeyCode::Char('k') | KeyCode::Up => {
                self.preview.scroll_by(-1);
                Some(true)
            }
            KeyCode::PageDown => {
                self.preview.page(true);
                Some(true)
            }
            KeyCode::PageUp => {
                self.preview.page(false);
                Some(true)
            }
            KeyCode::Char('G') | KeyCode::End => {
                self.preview.to_bottom();
                Some(true)
            }
            KeyCode::Home => {
                self.preview.to_top();
                Some(true)
            }
            // `gg` goes to the top. Handled here rather than through the chord
            // machinery so the pane's motions do not depend on the tree's state.
            KeyCode::Char('g') => {
                self.preview.to_top();
                Some(true)
            }
            KeyCode::Char('/') => {
                self.modal_target = Some(ModalTarget::PreviewSearch);
                self.modal = Modal::Input(InputDialog::new("Search in file (regex):", ""));
                Some(true)
            }
            // n/p step forward/back; N/P reverse each, as in the tree.
            KeyCode::Char('n') => {
                self.preview.step_match(true);
                Some(true)
            }
            KeyCode::Char('p') => {
                self.preview.step_match(false);
                Some(true)
            }
            KeyCode::Char('N') => {
                self.preview.step_match(false);
                Some(true)
            }
            KeyCode::Char('P') => {
                self.preview.step_match(true);
                Some(true)
            }
            // Both close the pane. `q` closing rather than quitting matters:
            // globally it exits the app, and reaching for it to dismiss a preview
            // should not end the session.
            KeyCode::Char(' ') | KeyCode::Char('q') => {
                self.close_preview();
                Some(true)
            }
            // Esc hands focus back without closing, so the tree can be moved
            // while the pane stays up and follows the cursor.
            KeyCode::Esc => {
                self.preview_focused = false;
                Some(true)
            }
            _ => None,
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
            // Plain vi motion, as everywhere else in the app. `k` used to
            // cancel and `K` to move, which read as a trap: the one key a vi
            // user presses without thinking was the destructive one.
            KeyCode::Char('j') | KeyCode::Down => {
                self.transfer_cursor_step(true);
                Some(true)
            }
            KeyCode::Char('k') | KeyCode::Up => {
                self.transfer_cursor_step(false);
                Some(true)
            }
            // Cancelling is shifted, keeping the destructive keys (`K`, `C`)
            // apart from the ones you hold down to navigate.
            //
            // Backspace alongside Delete: which of the two a terminal sends for
            // the key labelled "delete" depends on the terminal and its
            // configuration, and on a Mac keyboard the obvious key sends
            // Backspace. Both mean "get rid of this" here, and neither does
            // anything else in this pane.
            KeyCode::Char('K') | KeyCode::Delete | KeyCode::Backspace => {
                self.prompt_cancel_selected_transfer();
                Some(true)
            }
            // Drop the finished entries, keeping anything still queued or
            // running. Shifted like `K`, so neither key that destroys something
            // can be hit while navigating with `j`/`k`.
            //
            // No confirmation, unlike cancelling — clearing discards a record of
            // work that has already happened, not the work itself. There is
            // nothing to lose beyond the list, so a prompt would be ceremony.
            KeyCode::Char('C') => {
                self.transfers.clear_finished();
                // The cursor may have been sitting on a row that just went away.
                // Leaving it dangling would point at whatever slid up into that
                // slot, so `K` would then cancel a transfer the user never
                // selected.
                self.reconcile_transfer_cursor();
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

    /// Drop a transfer cursor that no longer names a live row.
    ///
    /// The panel only gives cursor stops to cancellable (queued or active)
    /// transfers, so anything else the cursor points at is stale.
    fn reconcile_transfer_cursor(&mut self) {
        let Some(id) = self.transfer_cursor else {
            return;
        };
        let still_there = self
            .transfers
            .transfers()
            .iter()
            .any(|t| t.id == id && !t.state.is_terminal());
        if !still_there {
            self.transfer_cursor = None;
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
        let mut picker = crate::screen::DirPickerState::with_catalog(&self.hosts);
        picker.set_traversal_context(self.shallow_default, Self::dir_shallow_prefs(&self.hosts));
        self.active_panel_mut()
            .screen_stack
            .push(Screen::DirPicker(picker));
    }

    /// Every directory's remembered traversal mode, keyed as the catalog stores
    /// it, for the picker's mode indicator.
    fn dir_shallow_prefs(
        catalog: &HostCatalog,
    ) -> std::collections::HashMap<String, bool> {
        catalog
            .favorites()
            .iter()
            .map(|f| (f.path.clone(), f.shallow))
            .collect()
    }

    /// Rebuild the open directory picker over the current catalog.
    ///
    /// Keeps the keyboard focus and the cursor's path where they still exist:
    /// adding or removing an entry should not fling the cursor to the top of the
    /// list or dump the user back into the path field.
    fn rebuild_dir_picker(&mut self) {
        let catalog = self.hosts.clone();
        let shallow_default = self.shallow_default;
        let prefs = Self::dir_shallow_prefs(&catalog);
        if let Screen::DirPicker(state) = self.active_panel_mut().current_screen_mut() {
            let keep = state.selected().map(|o| o.path.clone());
            let mut rebuilt = crate::screen::DirPickerState::with_catalog(&catalog);
            rebuilt.set_traversal_context(shallow_default, prefs);
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
        // An archive is also not local, but for the opposite reason: its sizes
        // are already exact, so there is nothing a walk would discover.
        if self.backends.get(panel.backend).is_read_only() {
            self.modal = Modal::Confirm(ConfirmDialog::notice(
                "An archive's directory sizes are already exact — every member's \
                 size is in its index, so there is nothing to measure.",
            ));
            return;
        }
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
        // Replaced rather than pushed. The stack is the navigation history, so
        // only entering a directory belongs on it: pushing here meant `S` left a
        // screen behind showing the *same* directory in the other mode, and the
        // next `q` looked like it had done nothing — it popped back to the
        // measured view of where you already were instead of returning to the
        // parent. Toggling twice buried the real parent two screens down.
        *self.active_panel_mut().current_screen_mut() = Screen::loading_with_source_sorted(
            source,
            root.clone(),
            cache,
            sort_mode,
        );

        // The `S` toggle is the explicit change of mode the startup flag defers
        // to, so it redirects the session as well as this one directory —
        // otherwise turning measuring off here and walking on would start
        // measuring again at the next subdirectory.
        self.shallow_default = shallow;

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
        // Both cases are "there is no path on this machine to hand over", but
        // the fix differs enough to be worth saying which one applies.
        if self.backends.get(panel.backend).is_read_only() {
            self.modal = Modal::Confirm(ConfirmDialog::notice(
                "Cannot open a file inside an archive with a local application. \
                 Copy it out first (c), then open the copy.",
            ));
            return;
        }
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
        // The preview pane takes its motions while focused, before the screen or
        // the global table sees them — that is what focus means. It covers the
        // screen, so it comes first.
        if let Some(result) = self.handle_preview_key(key) {
            return result;
        }

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
                // A filter is a mask over the tree, and `q`/`Esc` takes it off.
                // Innermost first: with a filter active inside an archive, one
                // press shows the archive whole and the next leaves it, so each
                // keystroke undoes exactly one thing the user turned on.
                if matches!(screen, Screen::Main(state) if state.tree.filter_pattern().is_some()) {
                    // The same route the dialog takes for an empty pattern, so
                    // clearing by key and clearing by prompt cannot diverge.
                    self.active_panel_mut().current_screen_mut().filter("");
                    return true;
                }
                // Inside an archive, back out to the directory it sits in rather
                // than quitting. Entering one is somewhere you went, so leaving
                // is what backing out should mean — the same reasoning the
                // picker above follows, and what `h` at the root already does.
                if self.active_backend_is_read_only() && self.active_panel().depth() > 1 {
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
                // Position in the rotation: 0..panels are the browser panels,
                // then the sidebar when it is on screen, then the preview when it
                // is open.
                let sidebar_stop = panels;
                let preview_stop = panels + usize::from(sidebar);
                let current = if self.preview_focused {
                    preview_stop
                } else if self.transfer_focused {
                    sidebar_stop
                } else {
                    self.active.min(panels.saturating_sub(1))
                };
                let stops = preview_stop + usize::from(self.preview_open);
                if stops > 1 {
                    let next = (current + 1) % stops;
                    if sidebar && next == sidebar_stop {
                        self.focus_transfers();
                    } else if self.preview_open && next == preview_stop {
                        self.focus_preview();
                    } else {
                        self.transfer_focused = false;
                        self.preview_focused = false;
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
            Action::VisualMode => {
                // A range needs an order to range over, and the treemap has
                // none. Say so rather than swallow the key.
                if !self.active_panel_mut().current_screen_mut().toggle_visual() {
                    self.modal = Modal::Confirm(ConfirmDialog::notice(
                        "Visual range-tagging needs the tree view — press v to switch \
                         back. Individual tiles can still be tagged with t.",
                    ));
                }
                true
            }
            Action::Filter => {
                self.modal_target = Some(ModalTarget::Filter);
                self.modal = Modal::Input(InputDialog::new(
                    "Filter (regex, empty to clear):",
                    "/pattern/",
                ));
                true
            }
            Action::CreateDir => {
                if self.refuse_if_read_only("create directories") {
                    return true;
                }
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
                            // remembered traversal mode too, falling back to the
                            // session's when it has none.
                            let shallow = self
                                .hosts
                                .dir_shallow_pref(&path.to_string_lossy())
                                .unwrap_or(self.shallow_default);
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
                            // Under `--directory` the picker is the panel's only
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
                // Enter on an archive browses its contents rather than doing
                // nothing. Checked before the directory case, since an archive
                // is a file and would otherwise fall through it.
                if let Screen::Main(state) = panel.current_screen() {
                    if !state.selected_is_dir() {
                        // `resolved_format`, not `archive_format`: the name is
                        // the guess and the container's own first bytes
                        // overrule it. A `.cbr` that is really a zip is
                        // ordinary rather than corrupt.
                        let archive = state.selected_path().and_then(|p| {
                            crate::vfs::archive::resolved_format(p).map(|f| (p.clone(), f))
                        });
                        if let Some((path, format)) = archive {
                            self.open_archive(path, format);
                            return true;
                        }
                    }
                }

                let panel = self.active_panel_mut();
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
                        // instead of measuring again on every arrival. With
                        // nothing recorded, the session's mode carries in — a
                        // subdirectory entered under `-s` is not a reason to
                        // start measuring.
                        let shallow = self
                            .hosts
                            .dir_shallow_pref(&path.to_string_lossy())
                            .unwrap_or(self.shallow_default);
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
            Action::OpenSortMenu => {
                self.open_sort_menu();
                true
            }
            Action::SetSort(i) => {
                if let Some(mode) = SortMode::ALL.get(i) {
                    self.set_sort_mode(*mode);
                }
                true
            }
            Action::PatternRename => {
                self.open_pattern_rename();
                true
            }
            Action::Bell => {
                self.ring_bell();
                true
            }
            Action::TogglePreview => {
                self.toggle_preview();
                true
            }
            Action::Redraw => {
                self.redraw();
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
                // Refused up front rather than left to the backend: the delete
                // task discards its error and the rows leave the tree either
                // way, so a backend-level failure would look like it worked.
                if self.refuse_if_read_only("delete files") {
                    return true;
                }
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
                    // Asked once not to be asked again, so go straight to it.
                    if self.skip_delete_confirm {
                        self.spawn_delete_batch(targets);
                        return true;
                    }
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
                    // The letters are spelled out in the message, as the move
                    // collision prompt does, since a bare `[a]` button says
                    // nothing about what it means. 'a' rather than 'd' for
                    // "always": 'd' next to a delete prompt reads as "delete".
                    self.modal = Modal::Confirm(
                        ConfirmDialog::new(format!(
                            "{} [y]es, [n]o, or [a]lways (no more prompts this session)?",
                            prompt
                        ))
                        .with_choices(&['y', 'n', 'a']),
                    );
                }
                true
            }
            Action::Rename => {
                if self.refuse_if_read_only("rename files") {
                    return true;
                }
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
                // `l` on an archive enters it, matching Enter. An archive is a
                // file, so the expand below would otherwise do nothing at all.
                if let Screen::Main(state) = self.active_panel().current_screen() {
                    let is_dir = state
                        .tree
                        .selected_line()
                        .map(|l| l.is_dir)
                        .unwrap_or(false);
                    if !is_dir {
                        // `resolved_format`, not `archive_format`: the name is
                        // the guess and the container's own first bytes
                        // overrule it. A `.cbr` that is really a zip is
                        // ordinary rather than corrupt.
                        let archive = state.selected_path().and_then(|p| {
                            crate::vfs::archive::resolved_format(p).map(|f| (p.clone(), f))
                        });
                        if let Some((path, format)) = archive {
                            self.open_archive(path, format);
                            return true;
                        }
                    }
                }
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
                    | Action::TogglePreview
                    | Action::Redraw
                    | Action::ToggleShallow
                    | Action::OpenSortMenu
                    | Action::SetSort(_)
                    | Action::PatternRename
                    | Action::Bell => unreachable!(),
                    Action::None => true,
                }
            }
        }
    }

    fn handle_modal_key(&mut self, key: KeyEvent) -> bool {
        // The sort menu owns its keys outright: it binds the digits and j/k
        // itself, and going through the chord detector would put its 500 ms
        // timeout in front of every keystroke.
        // The rename dialog owns its keys too: it is a form, and its fields have
        // to take every printable character rather than have `q` quit or `j`
        // move a cursor underneath.
        if let Modal::Rename(dialog) = &mut self.modal {
            let outcome = dialog.handle_key(key);
            return self.apply_rename_dialog_outcome(outcome);
        }

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
                    self.apply_confirm_answer(answer);
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
                            // An empty value clears the search, as in the filter
                            // prompt — the dialog cannot tell Esc from an empty
                            // Enter, and clearing is the harmless reading.
                            ModalTarget::PreviewSearch => {
                                if let Some(msg) = self.preview.search(&value) {
                                    self.modal =
                                        Modal::Confirm(ConfirmDialog::notice(msg));
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
                                let src_backend = self.active_panel().backend;
                                let src_read_only =
                                    self.backends.get(src_backend).is_read_only();
                                let dest_backend = resolve_copy_dest_backend(
                                    src_backend,
                                    src_read_only,
                                    dir.is_dir(),
                                );
                                // A destination that can be checked here should
                                // be, because "that is not a directory" beats a
                                // transfer that joins the queue and fails. Only
                                // a server's own paths cannot be checked from
                                // this machine.
                                let can_check_locally = src_backend.is_local() || src_read_only;
                                if !can_check_locally || dir.is_dir() {
                                    let active = self.active;
                                    if copy_needs_transfer_queue(src_backend, dest_backend) {
                                        // Either endpoint is remote, so this goes
                                        // on the queue. `begin_copy_batch` spawns
                                        // `copy_path`, which is plain `std::fs`
                                        // and would have operated on the local
                                        // disk under remote paths.
                                        let kinds: Vec<bool> = if let Screen::Main(state) =
                                            self.panels[active].current_screen()
                                        {
                                            srcs.iter()
                                                .map(|p| state.is_dir_of(p).unwrap_or(false))
                                                .collect()
                                        } else {
                                            vec![false; srcs.len()]
                                        };
                                        // No panel shows the typed destination,
                                        // so it is listed in the background and
                                        // the overwrite prompts follow from that.
                                        self.begin_transfer_batch_probing(
                                            srcs, kinds, src_backend, dir, dest_backend, active,
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
                            | ModalTarget::TransferOverwrite { .. }
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
            Modal::SortMenu(_) | Modal::Rename(_) => true,
            Modal::None => true,
        }
    }

    /// Kick off a connection at startup from a CLI `sftp://` argument. The
    /// connect runs in the background once the event loop starts.
    pub fn connect_on_start(&mut self, target: &str) {
        self.start_connect(target);
    }

    /// As [`Self::connect_on_start`], but opening the remote in a named panel.
    ///
    /// `start_connect` dials into whichever panel is active, which is panel 0 at
    /// start-up. `myd <local> sftp://host` needs the remote on the right, so the
    /// destination is passed explicitly rather than by making the panel active
    /// first — that would also move the initial focus.
    pub fn connect_on_start_in_panel(&mut self, target: &str, panel: usize) {
        match crate::vfs::sftp::SftpTarget::parse(target) {
            Ok(t) => self.spawn_connect(t, crate::vfs::sftp::Credentials::default(), panel),
            Err(e) => {
                self.modal =
                    Modal::Confirm(ConfirmDialog::new(format!("Invalid remote target: {}", e)));
            }
        }
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

    /// Act on the rename dialog's decision.
    ///
    /// Applying replaces the dialog with a notice only when something went
    /// wrong; a clean rename just closes it. A pattern that does not match is
    /// refused by the dialog itself and never reaches here, so the failure
    /// reported here is the one the preview could not have predicted — a name
    /// collision, or a backend that said no.
    fn apply_rename_dialog_outcome(
        &mut self,
        outcome: crate::widget::rename_dialog::RenameDialogOutcome,
    ) -> bool {
        use crate::widget::rename_dialog::RenameDialogOutcome as Outcome;
        match outcome {
            Outcome::Continue => {}
            Outcome::Cancelled => self.modal = Modal::None,
            Outcome::Apply {
                pattern,
                replacement,
            } => {
                self.modal = Modal::None;
                if let Some(msg) = self.apply_pattern_rename(&pattern, &replacement) {
                    self.modal = Modal::Confirm(ConfirmDialog::notice(msg));
                }
            }
        }
        true
    }

    /// Act on the sort menu's decision.
    fn apply_sort_menu_outcome(&mut self, outcome: SortMenuOutcome) -> bool {
        match outcome {
            SortMenuOutcome::Continue => {}
            SortMenuOutcome::Cancelled => self.modal = Modal::None,
            SortMenuOutcome::Chosen(mode) => {
                self.modal = Modal::None;
                self.set_sort_mode(mode);
            }
        }
        true
    }

    /// Sort the active panel by `mode`, from the menu or from a digit key.
    fn set_sort_mode(&mut self, mode: SortMode) {
        let panel = self.active_panel_mut();
        if let Screen::Main(st) = panel.current_screen_mut() {
            st.set_sort_mode(mode);
        }
        // Remembered for screens opened later, exactly as the `s` key does it.
        // Without this the choice lasted only until the next directory, which
        // `s` would not have done — a difference between two ways of setting the
        // same thing.
        if let Screen::Main(st) = panel.current_screen() {
            panel.view_prefs.sort_mode = st.tree.sort_mode;
        }
    }

    /// Open the sort menu for the active panel.
    fn open_sort_menu(&mut self) {
        let current = match self.panels[self.active].current_screen() {
            Screen::Main(s) => s.tree.sort_mode,
            _ => return,
        };
        self.modal = Modal::SortMenu(SortMenu::new(current));
    }

    /// Ring the terminal bell.
    ///
    /// Written straight to stdout rather than drawn, since it is a sound and not
    /// a thing on screen. Whether it is audible, a flash, or nothing at all is
    /// the terminal's business — many are configured for a visual bell, and some
    /// for neither, which is why this is not the only feedback: the status bar
    /// shows the pending chord while it is waiting.
    ///
    /// Counted for tests, which cannot hear it.
    fn ring_bell(&mut self) {
        use std::io::Write;
        self.bells_rung += 1;
        let mut out = std::io::stdout();
        let _ = out.write_all(b"\x07");
        let _ = out.flush();
    }

    /// How many times the bell has rung (for tests).
    pub fn bells_rung_for_test(&self) -> usize {
        self.bells_rung
    }

    /// The chord prefix waiting for its second key, if any (for tests and the
    /// status bar).
    pub fn pending_chord_for_test(&self) -> Option<char> {
        self.key_handler.pending_chord()
    }

    /// The files a patterned rename would act on: the tagged set, or the
    /// cursor's file when nothing is tagged.
    ///
    /// Mirrors how `D` chooses its targets, so "what does this act on" has one
    /// answer across the app rather than one per operation.
    fn rename_targets(&self) -> Vec<PathBuf> {
        let mut targets = self.active_panel().current_screen().tagged_paths();
        if targets.is_empty() {
            if let Screen::Main(state) = self.active_panel().current_screen() {
                if let Some(p) = state.selected_resolved_path() {
                    targets.push(p.clone());
                }
            }
            return targets;
        }
        // Tags live in a HashSet, so `tagged_paths` hands them back in an
        // arbitrary order that changes between runs. Put them back into the
        // order they appear on screen: "the first tagged file" is what the
        // dialog previews, and it has to mean the first one the user can see
        // rather than whichever the hash happened to yield.
        if let Screen::Main(state) = self.active_panel().current_screen() {
            let mut ordered: Vec<PathBuf> = Vec::with_capacity(targets.len());
            for line in state.tree.lines.iter() {
                let tagged = targets
                    .iter()
                    .any(|t| *t == line.resolved_path || *t == line.path);
                if tagged && !ordered.contains(&line.resolved_path) {
                    ordered.push(line.resolved_path.clone());
                }
            }
            // Anything tagged but not currently visible (a collapsed branch)
            // keeps its place at the end rather than being dropped.
            for t in targets.iter() {
                if !ordered.contains(t) {
                    ordered.push(t.clone());
                }
            }
            if !ordered.is_empty() {
                targets = ordered;
            }
        }
        targets
    }

    /// Open the patterned-rename dialog over the tagged files.
    fn open_pattern_rename(&mut self) {
        if self.refuse_if_read_only("rename files") {
            return;
        }
        let targets = self.rename_targets();
        if targets.is_empty() {
            return;
        }
        // Preview against the first target, which is what the dialog shows being
        // transformed. Named files only: the pattern applies to the name, not
        // the directory it sits in.
        let sample = targets[0]
            .file_name()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_default();
        self.modal = Modal::Rename(crate::widget::rename_dialog::RenameDialog::new(
            sample,
            targets.len(),
        ));
    }

    /// Apply `pattern` -> `replacement` to every target, renaming as it goes.
    ///
    /// Returns a message to show, or `None` when everything was renamed. Files
    /// the pattern does not match are skipped rather than treated as failures:
    /// tagging a mixed set and renaming only the ones that match is the ordinary
    /// case, not a mistake.
    fn apply_pattern_rename(&mut self, pattern: &str, replacement: &str) -> Option<String> {
        use crate::widget::rename_dialog::apply_pattern;

        let targets = self.rename_targets();
        let mut renamed = 0usize;
        let mut skipped = 0usize;
        let mut failures: Vec<String> = Vec::new();

        for path in targets {
            let Some(name) = path.file_name().map(|s| s.to_string_lossy().to_string()) else {
                continue;
            };
            match apply_pattern(pattern, replacement, &name) {
                // A pattern that stopped compiling between the preview and here
                // is not something to report per file.
                Err(e) => return Some(format!("Invalid pattern: {}", e)),
                Ok(None) => skipped += 1,
                Ok(Some(new_name)) if new_name == name => skipped += 1,
                Ok(Some(new_name)) => {
                    // An empty result would ask the backend to rename a file to
                    // nothing; refuse before it reaches the wire.
                    if new_name.is_empty() {
                        failures.push(format!("{}: would leave an empty name", name));
                        continue;
                    }
                    if let Some(msg) = self.rename_path(&path, &new_name) {
                        failures.push(format!("{}: {}", name, msg));
                    } else {
                        renamed += 1;
                    }
                }
            }
        }

        // Tags name paths that no longer exist once renamed, so clear them
        // rather than leave the panel tagging files that are gone.
        if renamed > 0 {
            self.active_panel_mut().current_screen_mut().clear_tags();
        }

        if !failures.is_empty() {
            // Report at most a few: a long list in a modal is unreadable, and
            // the count carries the rest.
            let shown: Vec<String> = failures.iter().take(3).cloned().collect();
            let more = failures.len().saturating_sub(shown.len());
            let mut msg = format!("Renamed {}, {} failed:\n{}", renamed, failures.len(), shown.join("\n"));
            if more > 0 {
                msg.push_str(&format!("\n(and {} more)", more));
            }
            return Some(msg);
        }
        if renamed == 0 {
            return Some(format!(
                "Nothing renamed: the pattern matched none of the {} file{}.",
                skipped,
                if skipped == 1 { "" } else { "s" }
            ));
        }
        None
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

    /// Whether the active panel's backend refuses mutations.
    ///
    /// Distinct from "not local": a server is remote and writable, an archive
    /// is local and is not.
    fn active_backend_is_read_only(&self) -> bool {
        self.backends
            .get(self.active_panel().backend)
            .is_read_only()
    }

    /// Refuse a mutation on a read-only backend, saying why and what to do.
    ///
    /// Returns `true` when the caller should stop. Follows the shape
    /// `open_selection_externally` established for "this operation does not
    /// apply to this backend": a notice, and no state touched.
    fn refuse_if_read_only(&mut self, verb: &str) -> bool {
        if !self.active_backend_is_read_only() {
            return false;
        }
        self.modal = Modal::Confirm(ConfirmDialog::notice(format!(
            "Cannot {verb} inside an archive — archives are read-only here. \
             Copy what you need out first (c), then work on the copy."
        )));
        true
    }

    /// Open the selected archive as a panel, browsing its contents as files.
    ///
    /// Reading and indexing the container is blocking work, so it happens
    /// inside the loading screen's `spawn_blocking` — where the spinner is
    /// already drawn and the cancel token already works — rather than on the
    /// event loop. That is the same reason a remote directory loads there.
    fn open_archive(&mut self, container: PathBuf, format: crate::vfs::archive::ArchiveFormat) {
        let panel = self.active_panel();

        // An archive inside an archive would compose — the inner container's
        // bytes come from the outer one — but the cost multiplies through a
        // stream format and the nesting depth is the archive's to choose, not
        // ours. Extracting first is one keystroke.
        if self.backends.get(panel.backend).is_read_only() {
            let name = container
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_default();
            self.modal = Modal::Confirm(ConfirmDialog::notice(format!(
                "Cannot open an archive inside another archive. \
                 Copy '{name}' out first (c), then open it."
            )));
            return;
        }

        // The container has to be read whole to be indexed, which over a
        // network is a download the user did not ask for. Copying it across
        // first makes that transfer explicit, and shows its progress.
        if !panel.backend.is_local() {
            self.modal = Modal::Confirm(ConfirmDialog::notice(
                "Cannot browse an archive on a remote panel — it would have to be \
                 downloaded whole first. Copy it here (c), then open it.",
            ));
            return;
        }

        // Already open: reuse it, so the index is built once however many times
        // the user steps in and out.
        if let Some(source) = self.archive_backends.get(&container).cloned() {
            self.active_panel_mut()
                .screen_stack
                .push(Screen::loading_remote(source, PathBuf::from("/"), None));
            return;
        }

        // A format read through `bsdtar` is never held in memory: the tool
        // seeks in the file itself, so a DVD-sized `.iso` costs nothing to open
        // and the size cap below would refuse it for no reason.
        if format.needs_bsdtar() {
            if !crate::vfs::archive::libarchive_reader::available() {
                self.modal = Modal::Confirm(ConfirmDialog::notice(
                    crate::vfs::archive::libarchive_reader::explain_missing(format),
                ));
                return;
            }
            self.finish_opening_archive(Vec::new(), container, format);
            return;
        }

        // Nothing is read here: a local container is memory-mapped when it is
        // opened, so the file's size costs address space rather than memory and
        // an archive of any size opens. Indexing touches a zip's tail or walks a
        // tar's headers; neither faults in the bulk of the file. That is what
        // removed the size ceiling this used to refuse above.
        self.finish_opening_archive(Vec::new(), container, format);
    }

    /// Index a container, register it as a backend, and open a panel on it.
    ///
    /// `bytes` is the container's contents for the readers that parse in
    /// process, and empty for the ones that hand the path to `bsdtar`.
    /// Start indexing off the event loop; [`Self::resolve_archive_open`] finishes
    /// the job when it lands.
    ///
    /// Indexing walks every member, so its cost is the archive's member count.
    /// Running it here would block the key handler for as long as that takes —
    /// half a second for a hundred thousand members, and far worse before the
    /// index's own counting was made incremental — during which nothing
    /// redraws and no key is read.
    fn finish_opening_archive(
        &mut self,
        bytes: Vec<u8>,
        container: PathBuf,
        format: crate::vfs::archive::ArchiveFormat,
    ) {
        // One at a time: a second Enter while the first is still indexing would
        // otherwise leave a task whose result nothing is waiting for.
        if self.archive_open_task.is_some() {
            return;
        }

        let (tx, rx) = tokio::sync::oneshot::channel();
        let path = container.clone();
        // `spawn_blocking`, not `spawn`: this is CPU-bound and would otherwise
        // stall every other task sharing that worker thread.
        tokio::task::spawn_blocking(move || {
            let _ = tx.send(crate::vfs::archive::ArchiveFs::open(bytes, format, path));
        });

        self.archive_open_task = Some(ArchiveOpenTask {
            rx,
            container,
            target_panel: self.active,
        });
        // Says what is happening while it happens, rather than letting the
        // interface look wedged.
        self.modal = Modal::Operation { verb: "Reading archive" };
    }

    /// Register a freshly indexed archive and open a panel on it.
    fn resolve_archive_open(&mut self) {
        use crate::widget::source::{RemoteSource, Source};

        let Some(task) = self.archive_open_task.as_mut() else {
            return;
        };
        let result = match task.rx.try_recv() {
            Ok(r) => r,
            // Still indexing.
            Err(tokio::sync::oneshot::error::TryRecvError::Empty) => return,
            Err(tokio::sync::oneshot::error::TryRecvError::Closed) => {
                self.archive_open_task = None;
                self.modal = Modal::None;
                return;
            }
        };
        let Some(task) = self.archive_open_task.take() else {
            return;
        };
        // The overlay goes now, whatever the answer: an error replaces it with
        // its own dialog, and success has a panel to show.
        self.modal = Modal::None;

        let fs = match result {
            Ok(fs) => std::sync::Arc::new(fs),
            Err(e) => {
                self.modal = Modal::Confirm(ConfirmDialog::notice(explain_error(&e)));
                return;
            }
        };

        let backend = self.backends.register(fs.clone());
        let source = match RemoteSource::new(backend, fs) {
            Ok(s) => Source::Remote(s),
            Err(e) => {
                self.modal = Modal::Confirm(ConfirmDialog::notice(format!(
                    "Could not open this archive: {e}"
                )));
                return;
            }
        };
        self.archive_backends.insert(task.container, source.clone());

        // Into the panel that asked, guarded in case the layout changed while
        // indexing — the same care `resolve_connect` takes for the same reason.
        //
        // A fresh size cache, not the panel's: cache keys are bare paths, and
        // every archive has a `/README.md`. Sharing one would have two
        // different archives reporting each other's sizes.
        let panel = task.target_panel.min(self.panels.len().saturating_sub(1));
        self.panels[panel]
            .screen_stack
            .push(Screen::loading_remote(source, PathBuf::from("/"), None));
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

        // The archive root has no name to give the copy — `file_name()` on "/"
        // is `None` — so the queue would skip it and nothing would happen at
        // all. Copying the archive whole is a thing you do from the directory
        // it sits in, where it is an ordinary file.
        if self.active_backend_is_read_only()
            && srcs.iter().any(|p| p.parent().is_none())
        {
            self.modal = Modal::Confirm(ConfirmDialog::notice(
                "Cannot copy the archive root from inside it. Leave the archive (h) \
                 and copy the file itself, or copy the entries within it.",
            ));
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
                    self.begin_transfer_batch(
                        srcs,
                        kinds,
                        src_backend,
                        dest_dir,
                        dest_backend,
                        source,
                        Some(other),
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

    /// Confirm any overwrites, then queue a cross-backend copy.
    ///
    /// The local copy path has always asked before replacing a file; the queued
    /// one went straight to the worker, which replaces the destination because
    /// "the overwrite decision was made by the caller before queueing" — a
    /// caller that was not making it. A remote-to-local copy therefore destroyed
    /// an existing file without a word.
    ///
    /// `dest_panel` is the pane showing the destination, whose loaded listing
    /// answers the collision question without a round trip. `None` means nothing
    /// on screen shows that directory — a destination typed into the
    /// single-panel prompt — and the caller has already listed it instead; see
    /// [`Self::begin_transfer_batch_probing`].
    #[allow(clippy::too_many_arguments)]
    fn begin_transfer_batch(
        &mut self,
        srcs: Vec<PathBuf>,
        kinds: Vec<bool>,
        src_backend: crate::vfs::BackendId,
        dest_dir: PathBuf,
        dest_backend: crate::vfs::BackendId,
        source_panel: usize,
        dest_panel: Option<usize>,
    ) {
        // Names already at the destination, from whichever panel is showing it.
        let existing: Option<Vec<String>> = dest_panel.and_then(|p| {
            match self.panels[p].current_screen() {
                Screen::Main(state) => Some(
                    srcs.iter()
                        .filter_map(|s| s.file_name())
                        .filter(|name| state.has_entry(&dest_dir.join(name)))
                        .map(|n| n.to_string_lossy().to_string())
                        .collect(),
                ),
                _ => None,
            }
        });
        self.split_and_prompt_transfer(
            srcs,
            kinds,
            src_backend,
            dest_dir,
            dest_backend,
            source_panel,
            existing.unwrap_or_default(),
        );
    }

    /// Split `srcs` into colliding and clear entries against `existing`, then
    /// start the confirmation flow.
    #[allow(clippy::too_many_arguments)]
    fn split_and_prompt_transfer(
        &mut self,
        srcs: Vec<PathBuf>,
        kinds: Vec<bool>,
        src_backend: crate::vfs::BackendId,
        dest_dir: PathBuf,
        dest_backend: crate::vfs::BackendId,
        source_panel: usize,
        existing: Vec<String>,
    ) {
        let mut pending: Vec<(PathBuf, bool)> = Vec::new();
        let mut approved: Vec<(PathBuf, bool)> = Vec::new();

        for (i, src) in srcs.into_iter().enumerate() {
            let is_dir = kinds.get(i).copied().unwrap_or(false);
            let collides = src
                .file_name()
                .map(|n| existing.iter().any(|e| e.as_str() == n))
                .unwrap_or(false);
            if collides {
                pending.push((src, is_dir));
            } else {
                approved.push((src, is_dir));
            }
        }

        self.pending_transfer = Some(PendingTransferBatch {
            pending,
            approved,
            src_backend,
            dest_dir,
            dest_backend,
            source_panel,
        });
        self.prompt_next_transfer();
    }

    /// As [`Self::begin_transfer_batch`], for a destination no panel is showing.
    ///
    /// Lists the destination directory in the background and finishes the split
    /// when the answer arrives (see [`Self::resolve_dest_probe`]). One listing
    /// covers the whole batch: a stat per file would be a round trip per file on
    /// a remote destination, and the check cannot block the key handler at all.
    ///
    /// A listing that fails — the directory does not exist yet, or cannot be
    /// read — yields no names, so nothing collides and the batch proceeds. The
    /// transfer itself reports a genuinely missing destination.
    #[allow(clippy::too_many_arguments)]
    fn begin_transfer_batch_probing(
        &mut self,
        srcs: Vec<PathBuf>,
        kinds: Vec<bool>,
        src_backend: crate::vfs::BackendId,
        dest_dir: PathBuf,
        dest_backend: crate::vfs::BackendId,
        source_panel: usize,
    ) {
        use crate::vfs::VPath;

        let (tx, rx) = tokio::sync::oneshot::channel();
        let fs = self.backends.get(dest_backend);
        let probe_path = VPath::new(dest_backend, dest_dir.clone());
        tokio::spawn(async move {
            let names = fs
                .read_dir(&probe_path)
                .await
                .map(|entries| entries.into_iter().map(|e| e.name).collect())
                .unwrap_or_default();
            let _ = tx.send(names);
        });

        self.dest_probe = Some(DestProbeTask {
            rx,
            srcs,
            kinds,
            src_backend,
            dest_dir,
            dest_backend,
            source_panel,
        });
    }

    /// Finish a typed-destination copy once its listing arrives.
    fn resolve_dest_probe(&mut self) {
        let Some(probe) = self.dest_probe.as_mut() else {
            return;
        };
        let existing = match probe.rx.try_recv() {
            Ok(names) => names,
            // Still listing.
            Err(tokio::sync::oneshot::error::TryRecvError::Empty) => return,
            // The task vanished; treat it as "nothing known to collide" rather
            // than dropping the copy the user asked for.
            Err(tokio::sync::oneshot::error::TryRecvError::Closed) => Vec::new(),
        };
        let Some(probe) = self.dest_probe.take() else {
            return;
        };
        self.split_and_prompt_transfer(
            probe.srcs,
            probe.kinds,
            probe.src_backend,
            probe.dest_dir,
            probe.dest_backend,
            probe.source_panel,
            existing,
        );
    }

    /// Ask about the next colliding transfer, or enqueue once none are left.
    fn prompt_next_transfer(&mut self) {
        let Some(batch) = self.pending_transfer.as_mut() else {
            return;
        };
        if let Some((src, is_dir)) = batch.pending.pop() {
            let name = src
                .file_name()
                .map(|s| s.to_string_lossy().to_string())
                .unwrap_or_default();
            self.modal_target = Some(ModalTarget::TransferOverwrite { src, is_dir });
            self.modal =
                Modal::Confirm(ConfirmDialog::new(format!("'{}' exists. Overwrite?", name)));
            return;
        }

        // Every collision answered: send what survived.
        let Some(batch) = self.pending_transfer.take() else {
            return;
        };
        if batch.approved.is_empty() {
            // Nothing left to do, but the tags were still the operation's input.
            self.panels[batch.source_panel]
                .current_screen_mut()
                .clear_tags();
            return;
        }
        let (srcs, kinds): (Vec<PathBuf>, Vec<bool>) = batch.approved.into_iter().unzip();
        self.enqueue_cross_backend_copy(
            srcs,
            kinds,
            batch.src_backend,
            batch.dest_dir,
            batch.dest_backend,
            batch.source_panel,
        );
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
        // A move out of an archive is a copy followed by deleting the source,
        // and the delete cannot happen — so it is a copy wearing the wrong
        // name. Say so rather than half-doing it.
        if self.refuse_if_read_only("move files out of") {
            return;
        }

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
        if self.backends.get(self.panels[other].backend).is_read_only() {
            self.modal = Modal::Confirm(ConfirmDialog::notice(
                "Cannot move files into an archive — archives are read-only here.",
            ));
            return;
        }
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

/// An error with everything that caused it, for a dialog.
///
/// `format!("{e}")` on an `anyhow::Error` prints only the outermost context, so
/// a failure wrapped in "could not read geeu.rar" showed exactly that and threw
/// away the part that said *why* — which was the only useful half. The chain is
/// joined rather than nested because the dialog is prose, not a stack trace.
pub fn explain_error(e: &anyhow::Error) -> String {
    let mut seen: Vec<String> = Vec::new();
    for cause in e.chain() {
        let text = cause.to_string();
        // A context line that merely repeats its cause adds nothing but width.
        if !text.is_empty() && !seen.iter().any(|s| s == &text) {
            seen.push(text);
        }
    }
    seen.join(": ")
}

/// Which backend a *typed* copy destination names.
///
/// The dialog gives a bare path with no way to say which machine it is on, so
/// it has to be inferred. Three cases:
///
/// - From a **read-only** source there is no "over there" to copy to — an
///   archive cannot be written — so every destination is on this machine.
///   Without this, typing a directory that does not exist yet resolved to the
///   archive's own backend and the extract was routed into the zip: it failed,
///   and surfaced as a red transfer row rather than "that directory does not
///   exist".
/// - From a **remote** source an existing local directory is taken at its word;
///   anything else is a path on the server, which is the only reading that
///   keeps a server-side copy into a not-yet-created directory working.
/// - From a **local** source it is local, there being nowhere else.
pub fn resolve_copy_dest_backend(
    src: crate::vfs::BackendId,
    src_read_only: bool,
    dest_exists_locally: bool,
) -> crate::vfs::BackendId {
    if src_read_only || (!src.is_local() && dest_exists_locally) {
        crate::vfs::BackendId::LOCAL
    } else {
        src
    }
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

/// Where the preview pane is drawn: most of the frame, inset a little.
///
/// Not the whole frame, so it reads as a pane over the browser rather than a
/// different program, and the panel borders stay visible at the edges. Falls back
/// to the full area on a terminal too small to inset.
///
/// A free function because the geometry is needed both when drawing and when
/// starting a load — an image is rendered by an external tool at a fixed cell
/// size, so the size has to be known in advance.
pub fn preview_area(full: ratatui::layout::Rect) -> ratatui::layout::Rect {
    /// Frame cells left visible around the pane, when there is room.
    const INSET_X: u16 = 4;
    const INSET_Y: u16 = 2;

    if full.width <= INSET_X * 2 + 10 || full.height <= INSET_Y * 2 + 4 {
        return full;
    }
    ratatui::layout::Rect {
        x: full.x + INSET_X,
        y: full.y + INSET_Y,
        width: full.width - INSET_X * 2,
        height: full.height - INSET_Y * 2,
    }
}

/// The escape that erases the cells an image was drawn into.
///
/// Used for the protocols with no delete operation of their own. `ESC[K` clears
/// from the cursor to the end of the line, so the cursor is placed at the image's
/// left edge on each row it covered in turn — erasing exactly those rows and
/// leaving the rest of the frame alone.
///
/// Rows and columns in the escape are 1-based, where a [`Rect`] is 0-based.
///
/// [`Rect`]: ratatui::layout::Rect
pub fn region_erase_sequence(area: ratatui::layout::Rect) -> String {
    let mut seq = String::with_capacity(area.height as usize * 12);
    for row in area.y..area.y.saturating_add(area.height) {
        use std::fmt::Write as _;
        let _ = write!(seq, "\x1b[{};{}H\x1b[K", row + 1, area.x + 1);
    }
    seq
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

/// Flatten a key event into the single char the dialogs answer to.
///
/// Everything unrecognised used to collapse onto `' '`, which the dialogs read
/// as "accept" — so Tab, Backspace, a function key or a stray arrow all counted
/// as pressing Yes. Only keys that genuinely mean something get a char now, and
/// anything else becomes `\0`, which every dialog ignores.
///
/// `\t` and the arrows are passed through so a dialog can move its own focus.
fn key_code_char(key: &KeyEvent) -> char {
    use crossterm::event::KeyCode;
    match key.code {
        KeyCode::Char(c) => c,
        KeyCode::Enter => '\n',
        KeyCode::Tab | KeyCode::Right | KeyCode::Down => '\t',
        // Shift-Tab and the reverse arrows step the other way.
        KeyCode::BackTab | KeyCode::Left | KeyCode::Up => '\u{1}',
        _ => '\0',
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
