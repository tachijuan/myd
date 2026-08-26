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
use crate::widget::open_dialog::{OpenDialog, OpenDialogOutcome};
use crate::widget::sort_menu::{SortMenu, SortMenuOutcome};
use crate::widget::progress::{OpProgress, ProgressOverlay};
use crate::widget::transfer_panel;
use crate::widget::treemap::FocusTarget;

/// Take the terminal: raw mode, alternate screen, mouse capture, hidden cursor.
///
/// Paired with [`leave_tui`]. Both exist as free functions because there are now
/// three callers between them — the guard, [`FileBrowser::run`], and the suspend
/// around an external program — and a second copy of either would eventually
/// enable something the other did not undo.
fn enter_tui() -> std::io::Result<()> {
    crossterm::terminal::enable_raw_mode()?;
    crossterm::execute!(
        std::io::stdout(),
        crossterm::terminal::EnterAlternateScreen,
        crossterm::event::EnableMouseCapture,
        crossterm::terminal::Clear(crossterm::terminal::ClearType::All),
        crossterm::cursor::Hide,
    )
}

/// Give the terminal back: cooked mode, main screen, visible cursor.
///
/// Every step is best-effort. This runs on the way out — including from a panic,
/// via [`TerminalGuard`] — and stopping at the first error would leave the
/// terminal in a worse state than finishing the rest.
fn leave_tui() {
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

/// Drop guard that restores terminal state even if the app panics or is interrupted.
/// Disables raw mode, leaves alternate screen, shows cursor, and flushes output.
struct TerminalGuard;

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        leave_tui();
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
    Help(HelpState),
    /// Numbered sort-order menu, opened by clicking the "Sort:" indicator.
    SortMenu(SortMenu),
    /// Regex rename over the tagged files, opened by `gr`.
    Rename(crate::widget::rename_dialog::RenameDialog),
    /// Run a program of the user's choosing over the selection, opened by `O`.
    Open(crate::widget::open_dialog::OpenDialog),
    /// Change one attribute of the selection, opened by Enter in the info panel.
    Attr(crate::widget::attr_dialog::AttrDialog),
    /// Name and format a new archive of the selection, opened by `gz`.
    CreateArchive(crate::widget::archive_dialog::ArchiveDialog),
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
    /// Confirm replacing an archive that is already there.
    ArchiveOverwrite {
        req: crate::vfs::archive::WriteRequest,
    },
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
    /// An archive being written off the event loop.
    archive_create_task: Option<ArchiveCreateTask>,
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
    /// Whether the info panel has keyboard focus instead of a panel, the
    /// sidebar or the preview.
    ///
    /// Kept exclusive with the other two by [`Self::focus_info`] and the Tab
    /// rotation, for the same reason they are exclusive with each other.
    info_focused: bool,
    /// Whether a preference has changed and not yet been written to disk.
    ///
    /// Set by the resize keys, cleared by [`Self::save_prefs_if_dirty`] on the
    /// way out — so a session that never touched a preference never writes the
    /// file at all.
    prefs_dirty: bool,
    /// What the most recent inline-preview request asked for, so a test can
    /// assert on the request rather than on the predicate that shapes it.
    info_preview_last_cells_only: Option<bool>,
    /// The compact preview at the foot of each panel's info panel, one slot per
    /// panel and indexed the same way [`Self::panels`] is.
    ///
    /// Per panel rather than one shared slot because a panel's preview belongs
    /// to *its* cursor: with a split showing two info panels, following only
    /// the focused one meant the other went blank as soon as focus moved, and
    /// filling both from one loader captioned one panel's file with the other's
    /// metadata. Each panel keeps its own, so switching focus changes nothing
    /// about what is already on screen.
    ///
    /// Separate from `self.preview` (the full pane) for different reasons: the
    /// two are shown at once, they load at different geometries — geometry is
    /// part of `PreviewKey` — and these render into cells while the full pane
    /// may use a graphics protocol.
    info_previews: Vec<InfoPreview>,
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
    /// The last command `O` ran, offered back as the next dialog's default.
    ///
    /// In memory only, like `skip_delete_confirm` above but for a different
    /// reason: this one is a convenience rather than a guard, and a command line
    /// remembered across runs would be a surprising thing to press Enter on
    /// weeks later without reading.
    last_open_command: Option<String>,
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

/// One panel's inline preview: its content, its in-flight load, and the
/// geometry that load was sized for.
///
/// Grouped in a struct rather than as parallel `Vec`s so a panel added or
/// removed by the split can never leave the three out of step.
#[derive(Default)]
struct InfoPreview {
    state: crate::widget::preview::PreviewState,
    task: Option<PreviewTask>,
    /// Cell size the sub-panel was last drawn at, read back after that panel
    /// rendered. `None` when it did not fit, which is also what stops the load.
    cells: Option<(u16, u16)>,
    /// When this panel's target last changed, so an expensive load waits for
    /// the cursor to settle.
    settle: Option<(crate::widget::preview::PreviewKey, std::time::Instant)>,
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

/// An archive being written off the event loop.
struct ArchiveCreateTask {
    rx: tokio::sync::oneshot::Receiver<anyhow::Result<()>>,
    /// Where it is being written, so the panel showing that directory can be
    /// reloaded once it lands.
    dest: PathBuf,
    /// The panel that asked, so the overlay is cleared where it was raised.
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
            archive_create_task: None,
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
            info_focused: false,
            prefs_dirty: false,
            info_preview_last_cells_only: None,
            info_previews: Vec::new(),
            preview_graphics_shown: None,
            force_repaint: false,
            shallow_default: false,
            skip_delete_confirm: false,
            last_open_command: None,
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

        enter_tui()?;
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
            // And any archive being written off it.
            self.resolve_create_archive();
            // And any listing of a typed copy destination.
            self.resolve_dest_probe();
            // Install a finished preview, and start one if the cursor has moved
            // onto a different file.
            self.resolve_preview();
            self.request_preview();
            // The info panel's inline preview follows the cursor the same way,
            // and declines the loads too expensive to run per keystroke.
            self.resolve_info_previews();
            self.request_info_previews();

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

        // Preferences are written once, here, rather than on every keystroke
        // that changes one — see `save_prefs_if_dirty`.
        self.save_prefs_if_dirty();

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
                // separately (via `footer_rect`) where their footer row goes.
                let sidebar = Rect {
                    height: cols[1].height.saturating_sub(1),
                    ..cols[1]
                };
                (cols[0], Some(sidebar))
            }
            None => (full, None),
        };
        // The footer row: the frame's last line, spanning its full width. Only
        // the focused pane's keys go here, so whichever panel is speaking gets
        // this rect and the rest draw nothing. `None` when there is no sidebar
        // and one panel, where the panel's own bottom row already is this row.
        let footer_rect = Rect {
            x: full.x,
            y: full.y + full.height.saturating_sub(1),
            width: full.width,
            height: 1,
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
        // A half-typed chord, shown in the status bar. Read once here: the key
        // handler is borrowed immutably while the panels are borrowed mutably
        // below.
        let pending_chord = self.key_handler.pending_chord();

        // Focus lives in one place: a browser panel, or the transfer sidebar.
        // `state.active` used to mean "is the active panel index", which stopped
        // being true once the sidebar became focusable — both it and the last
        // panel drew a cyan border at once.
        let panel_has_focus = !self.transfer_focused;
        // Focus must never rest on something that is not drawn. The info panel
        // can be hidden by `Ctrl+P` from anywhere, including while it holds the
        // keyboard, so the flag is reconciled here rather than at every place
        // that could hide it.
        if !self.info_panel_visible() {
            self.info_focused = false;
        }
        // Read once here for the same reason `pending_chord` is: the panels are
        // borrowed mutably below, so `self` cannot be consulted inside the loop.
        let transfer_focused = self.transfer_focused;
        let info_focused = self.info_focused;

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
                    // Only the active panel's info panel can hold the keyboard,
                    // so the inactive one draws no field cursor.
                    state.info_active = info_focused && i == active;
                    state.pending_ghosts = ghosts_for_panel(&pending, backend, state.root_path());
                    state.pending_chord = pending_chord;
                    // There is one keyboard, so exactly one set of keys is on
                    // screen: the focused pane's. Every other panel stays
                    // quiet.
                    //
                    // The footer says what the keys *do*, and the keys do one
                    // thing. Two panels each describing themselves put
                    // "[TREE] … [TREEMAP] …" side by side, which reads as two
                    // live keymaps when only one of them is — and the two
                    // disagree, since j/k and hjkl mean different things in the
                    // tree and the treemap.
                    //
                    // The active panel speaks for the sidebar when focus is
                    // there, since the sidebar has no footer of its own.
                    state.footer = if i == active {
                        if transfer_focused {
                            FooterMode::Transfers
                        } else {
                            FooterMode::Own
                        }
                    } else {
                        FooterMode::Hidden
                    };
                    // The one panel that draws a footer draws it across the
                    // whole frame, starting at the left edge rather than at
                    // this panel's — the row is nobody else's now, and a
                    // keymap for the whole window reads oddly indented to
                    // wherever the active column happens to begin.
                    state.footer_rect = (i == active).then_some(footer_rect);
                }
                panel.current_screen_mut().render(f, cols[i]);
            }
        } else {
            self.panel_areas = vec![area];
            let backend = self.panels[0].backend;
            if let Screen::Main(state) = self.panels[0].current_screen_mut() {
                state.active = panel_has_focus;
                state.info_active = info_focused;
                state.pending_ghosts = ghosts_for_panel(&pending, backend, state.root_path());
                state.pending_chord = pending_chord;
                // The sole panel, so it is the one that speaks for the sidebar.
                state.footer = if transfer_focused {
                    FooterMode::Transfers
                } else {
                    FooterMode::Own
                };
                state.footer_rect = Some(footer_rect);
            }
            self.panels[0].current_screen_mut().render(f, area);
        }

        // A pane waiting on a long operation draws its own overlay, inside its
        // own column. That containment is the point: the other pane stays
        // visible and usable, which an app-wide modal made impossible.
        let busy_areas: Vec<(Rect, &'static str, Option<crate::widget::progress::OpProgress>)> =
            self.panels
                .iter()
                .enumerate()
                .filter_map(|(i, panel)| {
                    let busy = panel.busy.as_ref()?;
                    let area = self.panel_areas.get(i).copied()?;
                    Some((area, busy.verb, busy.progress.clone()))
                })
                .collect();
        for (area, verb, progress) in busy_areas {
            let overlay = match &progress {
                Some(p) => ProgressOverlay::for_operation(verb, p),
                None => ProgressOverlay::new().with_message(verb),
            };
            overlay.render(f, area);
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

        // The info panel's preview sub-panel: each panel reserved a rect and
        // left it blank, because the content and the loaders live here. Read
        // after the panels render, since that is when the rects are recorded.
        //
        // Every panel draws its own slot, so what is on screen depends on that
        // panel's cursor and not on which panel has focus — moving focus
        // changes nothing about a preview already drawn.
        self.info_previews
            .resize_with(self.panels.len(), Default::default);
        let rects: Vec<Option<Rect>> = self
            .panels
            .iter()
            .map(|p| match p.current_screen() {
                Screen::Main(s) => s.info_preview_area,
                _ => None,
            })
            .collect();
        // Whether each panel's info panel is on screen at all — which is not the
        // same question as whether it has a rect. A visible panel too short to
        // split off a preview row records `None` too, and treating that as
        // "hidden" would throw away the preview of a panel the user is looking at.
        let info_shown: Vec<bool> = self
            .panels
            .iter()
            .map(|p| matches!(p.current_screen(), Screen::Main(s) if !s.info_panel_hidden))
            .collect();
        for (i, rect) in rects.iter().enumerate() {
            self.info_previews[i].cells = rect.map(|a| {
                (
                    a.width,
                    // One row goes to the separator the compact renderer draws.
                    a.height.saturating_sub(1).max(1),
                )
            });
            // A hidden info panel is showing nothing, so discard what it held.
            //
            // `graphics_area` is recorded by `render_compact`, which does not run
            // for a hidden panel — so without this the rect and the payload both
            // survive from the last visible frame, and `flush_preview_graphics`
            // goes on believing an image is still wanted. Nothing re-emits it, so
            // it is never redrawn; it simply stays on the terminal, because a
            // kitty or iTerm2 image is not made of cells and ratatui cannot paint
            // over one it knows nothing about. The image outlived its panel.
            //
            // Reset rather than merely ignored: the payload is the whole picture,
            // which for a photograph is megabytes worth holding on to only while
            // something is showing it. Reopening the panel reloads from the
            // cursor anyway, so nothing useful is thrown away.
            if !info_shown[i]
                && (self.info_previews[i].state.has_content()
                    || self.info_previews[i].task.is_some())
            {
                self.cancel_info_preview_task(i);
                self.info_previews[i].state = crate::widget::preview::PreviewState::new();
                self.info_previews[i].settle = None;
            }
        }
        if !self.preview_open {
            for (i, rect) in rects.iter().enumerate() {
                if let Some(rect) = rect {
                    crate::widget::preview::render_compact(
                        f,
                        *rect,
                        &mut self.info_previews[i].state,
                    );
                }
            }
        }

        // Modals center on the whole terminal, not the tree column, so toggling
        // the sidebar doesn't shift a dialog under the cursor.
        match &mut self.modal {
            Modal::Confirm(d) => d.render(f, full),
            Modal::Input(d) => d.render(f, full),
            Modal::SortMenu(m) => m.render(f, full, sort_anchor),
            Modal::Rename(d) => d.render(f, full),
            Modal::Open(d) => d.render(f, full),
            Modal::Attr(d) => d.render(f, full),
            Modal::CreateArchive(d) => d.render(f, full),
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
            // Dismiss the "Moving" overlay on whichever pane carried it. A
            // plain delete never sets one — it has always drawn from
            // `is_deleting` instead — so this is a no-op for that path.
            for i in 0..self.panels.len() {
                self.clear_busy_verb(i, "Moving");
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
                self.clear_busy_verb(self.copy_dest_panel, "Copying");
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
            // Declining simply drops it. The dialog is not reopened: `gz` is
            // one keystroke, and coming back from a declined confirm to a form
            // the user has already left is a state machine nothing else here
            // has.
            Some(ModalTarget::ArchiveOverwrite { req }) if result => {
                self.spawn_create_archive(req);
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

    /// Whether `panel` is waiting on a long operation.
    pub fn panel_is_busy_for_test(&self, panel: usize) -> bool {
        self.panels.get(panel).is_some_and(|p| p.is_busy())
    }

    /// Advance the background-task machinery one tick, as the event loop does
    /// (connection attempts, loading, transfers). For tests driving the remote
    /// connect + browse flow headlessly.
    pub fn tick_for_test(&mut self) {
        self.resolve_connect();
        self.resolve_archive_open();
        self.resolve_create_archive();
        self.resolve_dest_probe();
        self.resolve_loading();
        self.resolve_deleting();
        self.resolve_copying();
        self.advance_transfers();
        self.resolve_preview();
        self.request_preview();
        self.resolve_info_previews();
        self.request_info_previews();
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

    /// Each panel's `info_panel_hidden`, for tests.
    pub fn info_hidden_flags_for_test(&self) -> Vec<bool> {
        self.panels
            .iter()
            .map(|p| match p.current_screen() {
                Screen::Main(s) => s.info_panel_hidden,
                _ => true,
            })
            .collect()
    }

    /// Whether an inline preview may use a graphics protocol, for tests.
    pub fn info_preview_may_use_graphics_for_test(&self) -> bool {
        self.info_preview_may_use_graphics()
    }

    /// Whether the active panel's inline preview asked for a graphics payload,
    /// taken from the request actually issued rather than from the predicate.
    pub fn info_preview_wants_graphics_for_test(&self) -> Option<bool> {
        self.info_preview_last_cells_only.map(|cells| !cells)
    }

    /// Seed panel `i`'s inline preview with a real graphics payload, as a
    /// finished load would.
    ///
    /// A test backend reports no graphics protocol, so nothing here can produce
    /// one for real. Installing the content directly is what lets a test reach
    /// the paths that only run once an image is genuinely on the terminal.
    pub fn seed_info_preview_graphics_for_test(&mut self, i: usize, payload: &str) {
        use crate::preview::PreviewContent;
        use crate::widget::preview::PreviewKey;

        let key = PreviewKey {
            path: std::path::PathBuf::from("seeded.png"),
            backend: self.panels[i.min(self.panels.len() - 1)].backend,
            cols: 20,
            rows: 10,
            page: 0,
        };

        self.info_previews
            .resize_with(self.panels.len().max(i + 1), Default::default);
        self.info_previews[i].state.set_content(
            key,
            PreviewContent::Graphics {
                payload: payload.to_string(),
                rows: 10,
                backend: "test",
                page: 0,
                pages: None,
            },
        );
    }

    /// Whether panel `i`'s inline preview holds any loaded content.
    pub fn info_preview_has_content_for_test(&self, i: usize) -> bool {
        self.info_previews
            .get(i)
            .is_some_and(|s| s.state.has_content())
    }

    /// Whether panel `i`'s inline preview still holds a graphics payload.
    pub fn info_preview_has_graphics_for_test(&self, i: usize) -> bool {
        self.info_previews
            .get(i)
            .is_some_and(|s| s.state.graphics().is_some())
    }

    /// Index of the active panel, for tests.
    pub fn active_panel_for_test(&self) -> usize {
        self.active
    }

    /// Whether the info panel holds the keyboard (for tests).
    pub fn info_focused_for_test(&self) -> bool {
        self.info_focused
    }

    /// The info panel's field cursor, or `None` when it has no focus.
    pub fn info_field_for_test(&self) -> Option<crate::widget::file_info::InfoField> {
        match self.active_panel().current_screen() {
            Screen::Main(s) => self.info_focused.then_some(s.info_field),
            _ => None,
        }
    }

    /// The active panel's info panel width, as a percentage.
    pub fn info_panel_pct_for_test(&self) -> u16 {
        self.active_panel().view_prefs.info_panel_pct
    }

    /// The info panel's metadata/preview split bias, for tests.
    pub fn info_meta_bias_for_test(&self) -> i16 {
        self.active_panel().view_prefs.info_meta_bias
    }

    /// Whether a preference is waiting to be written on exit.
    pub fn prefs_dirty_for_test(&self) -> bool {
        self.prefs_dirty
    }

    /// Run the exit-time preference flush, which `run` does on the way out.
    ///
    /// Refuses unless `$MYD_PREFS` points somewhere, so a test can never write
    /// the config of whoever is running the suite. Tests share one process and
    /// its environment, so a guard held by one of them is not protection for
    /// the rest — and the flush is the one call here that writes to disk.
    pub fn save_prefs_for_test(&mut self) {
        assert!(
            std::env::var_os("MYD_PREFS").is_some(),
            "set $MYD_PREFS (see PrefsGuard) before flushing preferences in a test"
        );
        self.save_prefs_if_dirty();
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
            Modal::Help(_) => "help",
            Modal::SortMenu(_) => "sort_menu",
            Modal::Rename(_) => "rename",
            Modal::Open(_) => "open",
            Modal::Attr(_) => "attr",
            Modal::CreateArchive(_) => "create_archive",
        }
    }

    /// The open dialog, for tests that check what it is offering.
    pub fn open_dialog_for_test(&self) -> Option<&OpenDialog> {
        match &self.modal {
            Modal::Open(d) => Some(d),
            _ => None,
        }
    }

    /// The command `O` would offer back next time, for tests.
    pub fn last_open_command_for_test(&self) -> Option<&str> {
        self.last_open_command.as_deref()
    }


    /// Replace the host catalog, so tests don't touch the user's real one.
    pub fn set_hosts_for_test(&mut self, hosts: HostCatalog) {
        self.hosts = hosts;
    }

    /// The saved-host list (for tests).
    pub fn hosts_for_test(&self) -> &HostCatalog {
        &self.hosts
    }

    /// Whether a background copy/delete/move batch is still running.
    pub fn is_operation_running_for_test(&self) -> bool {
        self.panels.iter().any(|p| p.is_deleting()) || self.copy_task.is_some()
    }

    /// Whether a connection attempt is in flight (for tests).
    pub fn is_connecting_for_test(&self) -> bool {
        self.is_connecting()
    }

    /// The verb a panel is waiting on, if any — "Connecting", "Moving", … .
    ///
    /// The state that used to be an app-wide modal, so a test can assert *which
    /// pane* is occupied rather than only that something is.
    pub fn panel_busy_verb_for_test(&self, panel: usize) -> Option<&'static str> {
        self.panels.get(panel)?.busy.as_ref().map(|b| b.verb)
    }

    /// The saved-directory catalog, for tests that need a preference recorded
    /// the way an earlier session would have left it.
    pub fn hosts_mut_for_test(&mut self) -> &mut crate::hosts::HostCatalog {
        &mut self.hosts
    }

    /// What the graphics flush believes is on screen (for tests).
    ///
    /// `(area, payload length, page)`. The length is what makes the flush cheap:
    /// comparing it needs no copy of the image.
    pub fn preview_graphics_stamp_for_test(
        &self,
    ) -> Option<(ratatui::layout::Rect, usize, usize)> {
        self.preview_graphics_shown
    }

    /// Run one graphics flush, as the event loop does after each frame.
    pub fn flush_preview_graphics_for_test(&mut self) {
        self.flush_preview_graphics();
    }

    /// How deep the active panel's screen stack is (for tests).
    ///
    /// Distinguishes navigation that pushes history from navigation that
    /// replaces the current view, which look identical from the root path alone.
    pub fn screen_stack_len_for_test(&self) -> usize {
        self.active_panel().screen_stack.len()
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

        // Same contract for the open dialog: a click on a button presses it, and
        // anything else is swallowed rather than dismissing — this one launches
        // a program, so a stray click is the last thing that should decide it.
        if matches!(self.modal, Modal::Open(_)) {
            if matches!(ev.kind, MouseEventKind::Down(MouseButton::Left)) {
                let Modal::Open(dialog) = &mut self.modal else {
                    return true;
                };
                let outcome = dialog.click_at(x, y);
                return self.apply_open_dialog_outcome(outcome);
            }
            return true;
        }

        // Same again for the attribute dialog, which has a clickable checkbox
        // as well as its buttons.
        if matches!(self.modal, Modal::Attr(_)) {
            if matches!(ev.kind, MouseEventKind::Down(MouseButton::Left)) {
                let Modal::Attr(dialog) = &mut self.modal else {
                    return true;
                };
                let outcome = dialog.click_at(x, y);
                return self.apply_attr_dialog_outcome(outcome);
            }
            return true;
        }

        // And the archive dialog, whose format rows are clickable as well as
        // its buttons.
        if matches!(self.modal, Modal::CreateArchive(_)) {
            if matches!(ev.kind, MouseEventKind::Down(MouseButton::Left)) {
                let Modal::CreateArchive(dialog) = &mut self.modal else {
                    return true;
                };
                let outcome = dialog.click_at(x, y);
                return self.apply_create_archive_outcome(outcome);
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
        self.info_focused = false;
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
        self.info_focused = false;
        self.transfer_cursor = None;
    }

    /// Give the active panel's info panel keyboard focus.
    ///
    /// A no-op when the panel is not on screen, so focus can never rest on
    /// something that is not drawn — the same guard the other two setters use.
    fn focus_info(&mut self) {
        if !self.info_panel_visible() {
            return;
        }
        self.info_focused = true;
        self.preview_focused = false;
        self.transfer_focused = false;
        self.transfer_cursor = None;
    }

    /// Whether the active panel is currently showing its info panel.
    ///
    /// Read from the panel's own preference rather than from last frame's
    /// geometry: unlike the transfer sidebar, the info panel has no width
    /// threshold below which it yields, so the preference is the whole answer.
    fn info_panel_visible(&self) -> bool {
        matches!(
            self.active_panel().current_screen(),
            Screen::Main(state) if !state.info_panel_hidden
        )
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
            // The full pane is the one graphics surface, and it is opened
            // deliberately on one file, so it takes the terminal's best
            // protocol and the full read budget.
            cells_only: false,
            compact_listing: false,
            max_text_bytes: None,
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
    /// idle, and re-sending megabytes of base64 every tick would flood the
    /// terminal and make the image flicker. Deciding that costs no copy of the
    /// payload either — see the stamp below.
    fn flush_preview_graphics(&mut self) {
        // Which surface owns the terminal's one image, located without copying
        // it. The full pane comes first: it covers the panels, so while it is
        // open it is the only thing that can own the image. The inline preview
        // takes over when the pane is closed — it only ever produces a payload
        // when it is the single surface (see `info_preview_may_use_graphics`),
        // so the two can share this slot.
        //
        // The payload is the whole picture — for a photograph handed to iTerm2 in
        // its own format, megabytes of it — and this runs after every frame. The
        // copy used to be made up front, so an idle loop spent it ten times a
        // second only to reach the stamp check below and find nothing to redraw.
        //
        // `None` = the full pane, `Some(i)` = that inline slot.
        let source: Option<usize> = if self.preview_open {
            match (&self.preview.graphics_area, self.preview.graphics()) {
                (Some(_), Some(_)) => None,
                _ => {
                    self.clear_shown_graphics();
                    return;
                }
            }
        } else {
            match self.info_previews.iter().position(|slot| {
                slot.state.graphics_area.is_some() && slot.state.graphics().is_some()
            }) {
                Some(i) => Some(i),
                None => {
                    self.clear_shown_graphics();
                    return;
                }
            }
        };

        // Borrowed, not cloned: only the geometry and length are needed to decide
        // whether anything changed.
        let (area, len) = {
            let state = match source {
                None => &self.preview,
                Some(i) => &self.info_previews[i].state,
            };
            match (state.graphics_area, state.graphics()) {
                (Some(area), Some(payload)) => (area, payload.len()),
                _ => {
                    self.clear_shown_graphics();
                    return;
                }
            }
        };

        // Identify what is on screen by where it is and what it is, so a resize,
        // a page turn or a different file all re-emit, and an idle tick does not.
        let stamp = (area, len, self.preview.current_page());
        if self.preview_graphics_shown == Some(stamp) {
            return;
        }

        // Only now is the copy worth making — this frame really is going to write
        // the image. Taken by value because erasing the old one needs `&mut
        // self`, which cannot coexist with a payload borrowed out of `self`.
        let payload = match source {
            None => self.preview.graphics().unwrap_or_default().to_string(),
            Some(i) => self.info_previews[i]
                .state
                .graphics()
                .unwrap_or_default()
                .to_string(),
        };

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

    /// Erase whatever image is still on screen, if any.
    ///
    /// The "nothing to show any more" arm of [`Self::flush_preview_graphics`],
    /// which several checks there reach independently.
    fn clear_shown_graphics(&mut self) {
        if let Some((old, _, _)) = self.preview_graphics_shown.take() {
            self.erase_graphics(old);
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

    /// Whether a file is worth loading an inline preview for.
    ///
    /// The full pane is opened deliberately, on one file, so it may take its
    /// time on anything. The sub-panel follows the cursor, so a run of `j` down
    /// a directory of archives would start and cancel a load per keystroke —
    /// each of which may be a process, a round trip, or a decompression.
    ///
    /// Judged from what the tree already holds, so the gate itself costs no I/O.
    fn worth_inline_preview(path: &std::path::Path, is_dir: bool, over_network: bool) -> bool {
        if is_dir {
            // The loader would stat it only to return "This is a directory."
            return false;
        }
        if over_network
            && (crate::utils::filetype::is_image_like(path) || crate::utils::filetype::is_pdf(path))
        {
            // A remote image is downloaded to a temp file before it can be
            // rendered. Per cursor move, that is a transfer per keystroke.
            //
            // Only over a *network*: an archive is a non-local backend too, but
            // its members are read from a file on this machine, so an image
            // inside one costs no round trips and previews like any other.
            return false;
        }
        // An archive previews as its table of contents, exactly as the full
        // pane does. Indexing the container is real work, but it is the one
        // thing anyone wants to know about an archive, and the settle delay
        // below keeps a fast scroll past a shelf of them from paying for it.
        true
    }

    /// Whether this backend is reached over a network.
    ///
    /// Defers to [`crate::vfs::Vfs::is_remote`], which is where this question is
    /// answered for the whole app — see its documentation for why it is not the
    /// same as `!BackendId::is_local`.
    fn backend_is_remote(&self, backend: crate::vfs::BackendId) -> bool {
        self.backends.get(backend).is_remote()
    }

    /// Start each panel's inline preview, for any that is on screen and is not
    /// already showing (or fetching) what its cursor points at.
    ///
    /// Mirrors [`Self::request_preview`], with three differences: it is gated on
    /// that panel's sub-panel having been drawn rather than on the pane being
    /// open, it asks for cells rather than a graphics protocol, and it declines
    /// the loads that are too expensive to run per cursor move.
    ///
    /// Runs for every panel, not just the focused one — a panel's preview
    /// belongs to its own cursor, and following only the focused panel is what
    /// made the other half of a split go blank the moment focus moved.
    fn request_info_previews(&mut self) {
        // The full pane covers the panels, so no sub-panel is visible while it
        // is open — and loading for something nobody can see is waste. This also
        // removes any chance of two graphics surfaces at once.
        if self.preview_open {
            return;
        }
        self.info_previews.resize_with(self.panels.len(), Default::default);
        for i in 0..self.panels.len() {
            self.request_info_preview_for(i);
        }
    }

    /// Whether an inline preview may render through a graphics protocol.
    ///
    /// Only when it would be the single image on the terminal. Two sub-panels
    /// drawn at once — a split with both info panels shown — would share the
    /// one slot the app tracks, and on kitty erasing either removes both, so
    /// those fall back to block characters rather than flickering against each
    /// other.
    fn info_preview_may_use_graphics(&self) -> bool {
        self.panels
            .iter()
            .filter(|p| {
                matches!(
                    p.current_screen(),
                    Screen::Main(state) if !state.info_panel_hidden
                )
            })
            .count()
            == 1
    }

    /// The body of [`Self::request_info_previews`] for one panel.
    fn request_info_preview_for(&mut self, i: usize) {
        let Some((cols, rows)) = self.info_previews[i].cells else {
            return;
        };
        let Some(path) = self.panels[i].selected_resolved_path() else {
            return;
        };
        let backend = self.panels[i].backend;

        let is_dir = match self.panels[i].current_screen() {
            Screen::Main(state) => state.tree.selected_line().map(|l| l.is_dir).unwrap_or(false),
            _ => false,
        };
        let over_network = self.backend_is_remote(backend);
        if !Self::worth_inline_preview(&path, is_dir, over_network) {
            // Clear whatever the last file left behind rather than leaving it
            // under the new selection, which would read as this entry's content.
            // A fresh state has no title, so the box draws empty rather than
            // claiming to be loading something it will never fetch.
            self.cancel_info_preview_task(i);
            self.info_previews[i].state = crate::widget::preview::PreviewState::new();
            self.info_previews[i].settle = None;
            return;
        }

        let key = crate::widget::preview::PreviewKey {
            path: path.clone(),
            backend,
            cols,
            rows,
            // The sub-panel has no paging keys, so it always shows the first.
            page: 0,
        };

        // Already showing it, or already fetching it.
        if self.info_previews[i].state.key() == Some(&key) {
            return;
        }
        if self.info_previews[i].task.as_ref().is_some_and(|t| t.key == key) {
            return;
        }

        // An image render is an external process, and the cancel flag cannot
        // kill one that has already started — so those wait for the cursor to
        // settle. Text on a local disk loads in microseconds and must stay
        // instant, which is most of what anyone scrolls past.
        // Indexing an archive belongs here too: it is the most expensive thing
        // the sub-panel will now attempt, and scrolling past a directory of
        // them must not index every one on the way through.
        let slow = crate::utils::filetype::is_image_like(&path)
            || crate::utils::filetype::is_pdf(&path)
            || crate::vfs::archive::archive_format(&path).is_some()
            || over_network;
        if slow {
            const SETTLE: std::time::Duration = std::time::Duration::from_millis(120);
            match &self.info_previews[i].settle {
                Some((k, since)) if *k == key => {
                    if since.elapsed() < SETTLE {
                        return;
                    }
                }
                _ => {
                    self.info_previews[i].settle =
                        Some((key.clone(), std::time::Instant::now()));
                    return;
                }
            }
        }
        self.info_previews[i].settle = None;

        self.cancel_info_preview_task(i);

        let label = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| path.display().to_string());
        self.info_previews[i].state.begin_load(label);

        // A graphics payload where this is the only surface that would carry
        // one, and block characters otherwise — see the field's own comment.
        let cells_only = !self.info_preview_may_use_graphics();
        self.info_preview_last_cells_only = Some(cells_only);

        let fs = self.backends.get(backend);
        let cancel = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let (tx, rx) = tokio::sync::oneshot::channel();
        let req = crate::preview::PreviewRequest {
            path: crate::vfs::VPath::new(backend, path.clone()),
            label: path,
            cols,
            rows,
            page: 0,
            // A graphics payload where this is the only surface that would
            // carry one, and block characters otherwise.
            //
            // The app tracks exactly one image on the terminal, and on kitty
            // the delete escape removes *every* placement at once — so two
            // surfaces cannot be told apart. But the full pane covers the
            // panels (nothing is loaded here while it is open), and a split
            // only has two sub-panels when both info panels are shown. Outside
            // that one case there is no second surface to conflict with, and
            // forcing cells cost real resolution: the same image at the same
            // size looked far worse here than in the full pane.
            cells_only,
            // Names only: this box is a fraction of a panel wide.
            compact_listing: true,
            // A handful of rows has no use for a megabyte, and on a remote panel
            // that megabyte would cross the wire on every cursor move.
            max_text_bytes: Some(16 * 1024),
        };
        let flag = cancel.clone();
        tokio::spawn(async move {
            if flag.load(std::sync::atomic::Ordering::Relaxed) {
                return;
            }
            let content = crate::preview::load(fs, req).await;
            if !flag.load(std::sync::atomic::Ordering::Relaxed) {
                let _ = tx.send(content);
            }
        });
        self.info_previews[i].task = Some(PreviewTask { key, rx, cancel });
    }

    /// Supersede panel `i`'s in-flight sub-panel load, if there is one.
    fn cancel_info_preview_task(&mut self, i: usize) {
        if let Some(task) = self.info_previews[i].task.take() {
            task.cancel
                .store(true, std::sync::atomic::Ordering::Relaxed);
        }
    }

    /// Install any finished sub-panel loads. Called once per tick.
    fn resolve_info_previews(&mut self) {
        for slot in self.info_previews.iter_mut() {
            let Some(task) = slot.task.as_mut() else {
                continue;
            };
            match task.rx.try_recv() {
                Ok(content) => {
                    let key = task.key.clone();
                    slot.task = None;
                    slot.state.set_content(key, content);
                }
                Err(tokio::sync::oneshot::error::TryRecvError::Empty) => {}
                Err(tokio::sync::oneshot::error::TryRecvError::Closed) => {
                    slot.task = None;
                }
            }
        }
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

    /// The info panel's own keys, while it has focus.
    ///
    /// Returns `None` for anything it does not own, so the global table still
    /// sees `q`, `?` and the rest — the same contract the preview pane and the
    /// transfer sidebar follow.
    ///
    /// `Tab` is deliberately absent: it belongs to the global rotation, and a
    /// pane that swallowed it would be one you could enter but not leave.
    fn handle_info_key(&mut self, key: KeyEvent) -> Option<bool> {
        if !self.info_focused {
            return None;
        }
        match key.code {
            KeyCode::Char('j') | KeyCode::Down => {
                self.step_info_field(true);
                Some(true)
            }
            KeyCode::Char('k') | KeyCode::Up => {
                self.step_info_field(false);
                Some(true)
            }
            // Width is adjusted from here rather than globally because this is
            // the one place the panel is the thing you are looking at, and `<`
            // and `>` are ordinary characters everywhere else.
            //
            // The keys move the *divider*, not the panel's size: the info panel
            // is the right-hand column, so `<` pushes the split left and makes
            // it wider. Reading them as "smaller" and "bigger" instead means
            // the arrow points away from the edge that actually moves.
            KeyCode::Char('<') | KeyCode::Char(',') => {
                self.resize_info_panel(5);
                Some(true)
            }
            KeyCode::Char('>') | KeyCode::Char('.') => {
                self.resize_info_panel(-5);
                Some(true)
            }
            // The vertical counterpart: `+` grows the preview beneath the
            // metadata, `-` gives the rows back. Shifted and unshifted spellings
            // both, since `+` needs Shift on most layouts and `=` is the key it
            // shares.
            KeyCode::Char('+') | KeyCode::Char('=') => {
                self.resize_info_preview(1);
                Some(true)
            }
            KeyCode::Char('-') | KeyCode::Char('_') => {
                self.resize_info_preview(-1);
                Some(true)
            }
            KeyCode::Enter => {
                self.open_attr_dialog();
                Some(true)
            }
            // Esc hands focus back without hiding the panel, mirroring the
            // preview pane — the panel stays up and keeps following the cursor.
            KeyCode::Esc => {
                self.info_focused = false;
                Some(true)
            }
            _ => None,
        }
    }

    /// Open the edit dialog for the field the info panel's cursor is on.
    ///
    /// Refused inside an archive, which is read-only. Remote panels are allowed:
    /// SFTP can set both a mode and an owner, so the guard is
    /// `refuse_if_read_only` — the one `D`, `R` and `m` use — rather than the
    /// stricter local-only guard `O` needs for running a local program.
    fn open_attr_dialog(&mut self) {
        use crate::widget::attr_dialog::AttrDialog;

        let field = match self.active_panel().current_screen() {
            Screen::Main(state) => state.info_field,
            _ => return,
        };
        if self.refuse_if_read_only(match field {
            crate::widget::file_info::InfoField::Perms => "change permissions",
            _ => "change ownership",
        }) {
            return;
        }

        let targets = self.selection_targets();
        if targets.is_empty() {
            return;
        }

        // Recursion is only offered for a single directory: "apply to
        // everything inside" has no obvious meaning over a mixed selection, and
        // guessing at one would be a lot of files changed by accident.
        let allow_recursive = targets.len() == 1
            && matches!(
                self.active_panel().current_screen(),
                Screen::Main(state)
                    if state.tree.selected_line().map(|l| l.is_dir).unwrap_or(false)
            );

        // Pre-fill with what the panel is showing, so an edit is a correction
        // rather than a retype — and so the accepted spellings are discoverable
        // from the value already in the box.
        let current = self.current_attr_value(field, targets.first());
        let mut dialog = AttrDialog::new(field, targets, allow_recursive);
        if let Some(value) = current {
            dialog = dialog.with_value(value);
        }
        self.modal = Modal::Attr(dialog);
    }

    /// What `field` currently reads as for `path`, for pre-filling the dialog.
    ///
    /// `None` when it cannot be determined — a remote entry has no local
    /// metadata to stat, and an empty field is more honest than a local file's
    /// mode presented as the server's.
    fn current_attr_value(
        &self,
        field: crate::widget::file_info::InfoField,
        path: Option<&PathBuf>,
    ) -> Option<String> {
        use crate::widget::file_info::InfoField;

        let path = path?;
        if !self.active_panel().backend.is_local() {
            // The listing carries the mode, so that one can still be offered.
            if field == InfoField::Perms {
                if let Screen::Main(state) = self.active_panel().current_screen() {
                    let line = state.tree.selected_line()?;
                    return line.mode.map(|m| format!("{:o}", m & 0o7777));
                }
            }
            return None;
        }

        let meta = std::fs::symlink_metadata(path).ok()?;
        match field {
            InfoField::Perms => {
                use std::os::unix::fs::PermissionsExt;
                Some(format!("{:o}", meta.permissions().mode() & 0o7777))
            }
            InfoField::Owner => {
                use std::os::unix::fs::MetadataExt;
                Some(
                    crate::widget::file_info::uid_to_name(meta.uid())
                        .unwrap_or_else(|| meta.uid().to_string()),
                )
            }
            InfoField::Group => {
                use std::os::unix::fs::MetadataExt;
                Some(
                    crate::widget::file_info::gid_to_name(meta.gid())
                        .unwrap_or_else(|| meta.gid().to_string()),
                )
            }
        }
    }

    /// Act on what the attribute dialog decided. Returns false only to quit,
    /// which it never does — the signature matches the other modal handlers.
    fn apply_attr_dialog_outcome(
        &mut self,
        outcome: crate::widget::attr_dialog::AttrDialogOutcome,
    ) -> bool {
        use crate::widget::attr_dialog::AttrDialogOutcome;

        match outcome {
            AttrDialogOutcome::Continue => true,
            AttrDialogOutcome::Cancelled => {
                self.modal = Modal::None;
                true
            }
            AttrDialogOutcome::Apply { value, recursive } => {
                // Take the targets and the field from the dialog rather than
                // re-reading the selection: the dialog said what it was going
                // to act on, and that promise holds even if the cursor moved.
                let (field, targets) = match &self.modal {
                    Modal::Attr(d) => (d.field(), d.targets().to_vec()),
                    _ => return true,
                };
                self.modal = Modal::None;
                self.apply_attr_change(field, &value, &targets, recursive);
                true
            }
        }
    }

    /// Parse `value` and apply it to every target, then report what happened.
    fn apply_attr_change(
        &mut self,
        field: crate::widget::file_info::InfoField,
        value: &str,
        targets: &[PathBuf],
        recursive: bool,
    ) {
        use crate::vfs::ops::AttrChange;
        use crate::widget::file_info::{self, InfoField};

        // Parse before touching anything. A half-applied batch because the
        // value turned out to be nonsense partway through is the one outcome
        // worth ruling out entirely.
        let change = match field {
            InfoField::Perms => match file_info::parse_mode(value) {
                Some(mode) => AttrChange::Mode(mode),
                None => {
                    self.modal = Modal::Confirm(ConfirmDialog::notice(format!(
                        "'{value}' is not a permission. Use octal (644) or \
                         symbolic (rw-r--r--)."
                    )));
                    return;
                }
            },
            InfoField::Owner => match file_info::name_to_uid(value) {
                Some(uid) => AttrChange::Owner {
                    uid: Some(uid),
                    gid: None,
                },
                None => {
                    self.modal = Modal::Confirm(ConfirmDialog::notice(format!(
                        "There is no user '{value}' on this system."
                    )));
                    return;
                }
            },
            InfoField::Group => match file_info::name_to_gid(value) {
                Some(gid) => AttrChange::Owner {
                    uid: None,
                    gid: Some(gid),
                },
                None => {
                    self.modal = Modal::Confirm(ConfirmDialog::notice(format!(
                        "There is no group '{value}' on this system."
                    )));
                    return;
                }
            },
        };

        let backend = self.active_panel().backend;
        let fs = self.backends.get(backend);
        let paths: Vec<crate::vfs::VPath> = targets
            .iter()
            .map(|p| crate::vfs::VPath::new(backend, p.clone()))
            .collect();
        let cancel = crate::utils::sizes::CancelToken::new();

        // Blocking on the event loop, deliberately: a chmod is one syscall per
        // entry and returns immediately, unlike a delete or a transfer. The
        // recursive case over a deep tree is the exception — see below.
        let failures: Vec<String> = futures::executor::block_on(async {
            let mut failures = Vec::new();
            for path in &paths {
                if recursive {
                    failures.extend(
                        crate::vfs::ops::set_attr_recursive(&fs, path, change, None, &cancel).await,
                    );
                } else if let Err(e) = match change {
                    AttrChange::Mode(mode) => fs.set_mode(path, mode).await,
                    AttrChange::Owner { uid, gid } => fs.set_owner(path, uid, gid).await,
                } {
                    failures.push(format!("{}: {}", path.path.display(), e));
                }
            }
            failures
        });

        let changed = targets.len() - failures.len().min(targets.len());
        if !failures.is_empty() {
            // The first few, then a count: the whole list of a failed recursive
            // change could be thousands of lines, and the dialog would be
            // unreadable long before it was complete.
            let shown: Vec<&String> = failures.iter().take(3).collect();
            let mut detail = shown
                .iter()
                .map(|s| s.as_str())
                .collect::<Vec<_>>()
                .join("; ");
            if failures.len() > shown.len() {
                detail.push_str(&format!(" (and {} more)", failures.len() - shown.len()));
            }
            self.modal = Modal::Confirm(ConfirmDialog::notice(format!(
                "Changed {changed}, failed {}: {detail}",
                failures.len()
            )));
        }

        // The panel caches its text, so without this it would go on showing the
        // old mode until the cursor moved off the row and back.
        //
        // Deliberately not `refresh()`, which rebuilds the tree from disk and
        // resets the cursor to the root — leaving the panel describing the
        // wrong entry immediately after an edit. Nothing about a mode or an
        // owner changes the listing, so invalidating the cached text is the
        // whole of what is needed.
        if let Screen::Main(state) = self.active_panel_mut().current_screen_mut() {
            state.invalidate_info_cache();
        }
        // And draw that rebuilt text now. The event loop redraws when something
        // has happened, and applying a change from a dialog that has just
        // closed is not something it counts — so without this the panel keeps
        // showing the old mode until the next keystroke, which reads as the
        // change not having worked. Found in a real terminal; a test that draws
        // unconditionally cannot see it.
        self.force_repaint = true;
    }

    /// Move the info panel's field cursor.
    fn step_info_field(&mut self, down: bool) {
        if let Screen::Main(state) = self.active_panel_mut().current_screen_mut() {
            state.info_field = if down {
                state.info_field.next()
            } else {
                state.info_field.prev()
            };
        }
    }

    /// Widen or narrow the info panel, and remember the new width.
    ///
    /// Written to the panel's own prefs so it holds across navigation, and to
    /// disk so it holds across restarts. A failed write costs the persistence,
    /// not the resize — the panel still moves.
    fn resize_info_panel(&mut self, delta: i16) {
        let current = self.active_panel().view_prefs.info_panel_pct;
        let next = (current as i16 + delta).clamp(
            crate::prefs::MIN_INFO_PCT as i16,
            crate::prefs::MAX_INFO_PCT as i16,
        ) as u16;
        if next == current {
            return;
        }
        self.active_panel_mut().view_prefs.info_panel_pct = next;
        if let Screen::Main(state) = self.active_panel_mut().current_screen_mut() {
            state.info_panel_pct = next;
        }
        // Marked dirty rather than written: `<` and `>` are held down, and a
        // file write per keystroke would put a create-write-rename cycle on the
        // event loop for every repeat. Flushed once on the way out.
        self.prefs_dirty = true;
    }

    /// Grow or shrink the preview beneath the metadata.
    ///
    /// `delta` is rows given to the preview, so a positive value takes them
    /// from the metadata — the sign follows the key (`+` means "more preview"),
    /// not the field it is stored in.
    fn resize_info_preview(&mut self, delta: i16) {
        let current = self.active_panel().view_prefs.info_meta_bias;
        let next = (current - delta).clamp(
            -crate::prefs::MAX_META_BIAS,
            crate::prefs::MAX_META_BIAS,
        );
        if next == current {
            return;
        }
        self.active_panel_mut().view_prefs.info_meta_bias = next;
        if let Screen::Main(state) = self.active_panel_mut().current_screen_mut() {
            state.info_meta_bias = next;
        }
        self.prefs_dirty = true;
    }

    /// Write any changed preferences to disk.
    ///
    /// Called on the way out. A failure is logged and nothing more — losing a
    /// panel width is not worth failing an exit over, and by this point there
    /// is no screen left to report it on.
    fn save_prefs_if_dirty(&mut self) {
        if !self.prefs_dirty {
            return;
        }
        self.prefs_dirty = false;
        // Start from what is on disk and overwrite only what this session owns.
        // Building the struct from the live panel alone would write a default
        // over every field the panel does not carry — so resizing the info
        // panel would silently reset a hand-set `default_archive_format`.
        // Re-read rather than `prefs::startup()`, which is a snapshot from
        // launch: an edit made to the file since then is the user's, and this
        // save must not undo it either.
        let prefs = crate::prefs::Prefs {
            info_panel_pct: self.active_panel().view_prefs.info_panel_pct,
            info_meta_bias: self.active_panel().view_prefs.info_meta_bias,
            ..crate::prefs::Prefs::load()
        };
        if let Err(e) = prefs.save() {
            tracing::warn!(error = %e, "could not save preferences");
        }
    }
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

    /// Re-root the active panel one directory up, keeping its backend.
    ///
    /// Returns `false` when there is nowhere to go — already at `/`, or on a
    /// screen that has no root — so the caller can fall back to moving the
    /// cursor.
    ///
    /// The screen is *replaced* rather than pushed, as in [`set_shallow`]: going
    /// up is not descending into somewhere new, and pushing would make the next
    /// `q` return to the child that was just left rather than to wherever the
    /// panel was opened from.
    fn reroot_to_parent(&mut self) -> bool {
        let Screen::Main(state) = self.active_panel().current_screen() else {
            return false;
        };
        let root = state.root_path().clone();
        let Some(parent) = root.parent().map(|p| p.to_path_buf()) else {
            // Already at the filesystem root (or the server's).
            return false;
        };
        // `Path::parent` of "/" is None, but of "/x" it is "" on some inputs —
        // treat an empty parent as the root so the last step up still lands
        // somewhere listable rather than on a path the server will reject.
        let parent = if parent.as_os_str().is_empty() {
            PathBuf::from("/")
        } else {
            parent
        };

        let source = state.tree.source.clone();
        let sort_mode = state.tree.sort_mode;
        // The size cache is keyed by path and stays valid across a re-root: the
        // child we came from is one of the parent's entries, so its measured
        // total is exactly what the new listing needs.
        let cache = if source.is_shallow() {
            None
        } else {
            Some(state.tree.size_cache.clone())
        };

        *self.active_panel_mut().current_screen_mut() =
            Screen::loading_with_source_sorted(source, parent, cache, sort_mode);
        true
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
        // and they are computed during the walk.
        //
        // The cache is dropped in *both* directions, because it is wrong in both.
        // Going shallow, its entries are the totals being disowned. Coming out of
        // shallow, they are the zeros shallow mode wrote: `dir_size` returns 0
        // there and the loader caches that like any other answer, so carrying
        // them over meant the measured scan's "already known, skip it" check saw
        // a zero and never walked. A directory holding gigabytes then reported a
        // few bytes, while opening myd in it afresh reported it correctly —
        // there was no cache to inherit that way.
        let cache = None;
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

    /// Refuse `O` when the selection is not a set of paths on this machine.
    ///
    /// Returns true when it refused, having put the reason on screen. The two
    /// cases share "there is nothing here to hand to a local program", but the
    /// way out differs enough to be worth saying which one applies — the same
    /// split [`Self::open_selection_externally`] makes.
    fn refuse_open_with_if_not_local(&mut self) -> bool {
        let panel = self.active_panel();
        if self.backends.get(panel.backend).is_read_only() {
            self.modal = Modal::Confirm(ConfirmDialog::notice(
                "Cannot run a program on a file inside an archive. \
                 Copy it out first (c), then open the copy.",
            ));
            return true;
        }
        if !panel.backend.is_local() {
            self.modal = Modal::Confirm(ConfirmDialog::notice(
                "Cannot run a local program on remote files. Copy them across first (c).",
            ));
            return true;
        }
        false
    }

    /// Open the "run a program over the selection" dialog.
    fn open_with_program(&mut self) {
        if self.refuse_open_with_if_not_local() {
            return;
        }
        let targets = self.selection_targets();
        if targets.is_empty() {
            return;
        }
        let mut dialog = OpenDialog::new(targets);
        // Offer the last command back. Re-running one program over a series of
        // files is the common case, and retyping it every time is the friction
        // this is meant to remove.
        if let Some(last) = &self.last_open_command {
            dialog = dialog.with_command(last.clone());
        }
        self.modal = Modal::Open(dialog);
    }

    /// Act on what the open dialog decided. Returns false only to quit, which it
    /// never does — the signature matches the other modal handlers.
    fn apply_open_dialog_outcome(&mut self, outcome: OpenDialogOutcome) -> bool {
        match outcome {
            OpenDialogOutcome::Continue => true,
            OpenDialogOutcome::Cancelled => {
                self.modal = Modal::None;
                true
            }
            OpenDialogOutcome::Run { command } => {
                // Take the targets from the dialog rather than re-reading the
                // selection: the dialog said what it was going to act on, and
                // that promise should hold even if something moved underneath.
                let targets = match &self.modal {
                    Modal::Open(d) => d.targets().to_vec(),
                    _ => Vec::new(),
                };
                self.modal = Modal::None;
                self.last_open_command = Some(command.clone());
                self.run_open_command(&command, &targets);
                true
            }
        }
    }

    /// Parse `command`, resolve it, and run it over `targets`.
    fn run_open_command(&mut self, command: &str, targets: &[PathBuf]) {
        let Some((program, args)) = crate::utils::opener::split_command(command) else {
            return;
        };
        let program = match crate::utils::opener::resolve_program(&program) {
            Ok(p) => p,
            Err(e) => {
                tracing::warn!(command = %command, error = %e, "could not resolve program");
                self.modal = Modal::Confirm(ConfirmDialog::notice(format!("{}", e)));
                return;
            }
        };
        match self.run_program_suspended(&program, &args, targets) {
            Ok(status) if !status.success() => {
                // A non-zero exit is the program's own business, not an error in
                // myd — but it is invisible once the screen comes back, so say
                // it happened rather than let the user wonder.
                let code = status
                    .code()
                    .map(|c| format!("exit status {}", c))
                    .unwrap_or_else(|| "killed by a signal".to_string());
                self.modal = Modal::Confirm(ConfirmDialog::notice(format!(
                    "{} finished with {}.",
                    program.display(),
                    code
                )));
            }
            Ok(_) => {}
            Err(e) => {
                tracing::warn!(program = %program.display(), error = %e, "could not run program");
                self.modal = Modal::Confirm(ConfirmDialog::notice(format!("{}", e)));
            }
        }
        // The directory may look different now — the program may well have
        // written to it, which is often the entire point of running it.
        self.active_panel_mut().current_screen_mut().refresh();
    }

    /// Hand the terminal to `program`, wait for it to finish, and take it back.
    ///
    /// The child inherits this process's streams so that an editor or a pager
    /// gets the real terminal and behaves exactly as it would from a shell.
    ///
    /// The restore is unconditional: if the spawn fails, myd would otherwise
    /// carry on drawing an alternate screen it had already left, which looks
    /// like a hang and is not recoverable from inside the app.
    fn run_program_suspended(
        &mut self,
        program: &Path,
        args: &[String],
        files: &[PathBuf],
    ) -> Result<std::process::ExitStatus> {
        leave_tui();
        let result = crate::utils::opener::run_foreground(program, args, files);
        let restored = enter_tui();
        // `enter_tui` re-enables mouse capture, so the flag has to agree with it
        // — otherwise `toggle_mouse_capture` inverts the wrong way afterwards.
        self.mouse_captured = true;
        // The child owned the screen and ratatui's buffer no longer describes
        // what is on it. `force_repaint` clears before the next draw.
        self.force_repaint = true;
        if let Err(e) = restored {
            tracing::error!(error = %e, "could not restore the terminal");
        }
        result
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

        // Same contract for the info panel: the keys it owns while focused, and
        // nothing else, so `q`, `?` and the rest still work from inside it.
        if let Some(result) = self.handle_info_key(key) {
            return result;
        }

        // Let the current screen handle raw keys first (e.g., dir picker input)
        // — unless this pane is waiting on something, in which case its screen
        // is behind an overlay and must not eat the keystroke.
        //
        // The picker is what made this necessary: connecting from `gd` leaves
        // the picker on the stack (so a failed connect returns to the list), and
        // as a raw-key consumer it swallowed everything — including the `Tab`
        // that was supposed to move focus to the pane that is *not* busy.
        if !self.active_panel().is_busy() {
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
                // A pane waiting on something backs out of that first, so a slow
                // host or a long move can always be abandoned — the same "undo
                // one thing at a time" rule the rest of this chain follows.
                //
                // A connect additionally needs its task dropped, which is what
                // detaches the handshake; the other operations carry a cancel
                // token that `cancel_busy` trips.
                if self.active_panel().is_busy() {
                    if self.connecting_panel() == Some(self.active) {
                        self.cancel_connect();
                    } else {
                        self.active_panel_mut().cancel_busy();
                        // The app-level token the operation actually watches;
                        // the panel's is a clone of it, so tripping either does
                        // the job, but this is the one the resolver clears.
                        if let Some(cancel) = self.op_cancel.take() {
                            cancel.cancel();
                        }
                    }
                    return true;
                }
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
                // Tab rotates through every focusable pane in layout order and
                // wraps around, so everything focusable is reachable from one
                // key rather than each pane needing its own.
                //
                // Built as an explicit list rather than arithmetic over counts.
                // A panel's info panel is a stop *immediately after that panel*,
                // which is the order they sit in on screen — and with the stops
                // computed as offsets there was only ever one info-panel stop at
                // the end of the rotation, always targeting the active panel. In
                // a split with both info panels open, the left one could not be
                // reached at all, so its permissions and ownership could not be
                // edited.
                //
                // Only what is actually drawn gets a stop, which is what makes
                // an invisible pane unreachable rather than a dead stop.
                #[derive(PartialEq)]
                enum Stop {
                    Panel(usize),
                    Info(usize),
                    Sidebar,
                    Preview,
                }

                let mut stops: Vec<Stop> = Vec::new();
                for (i, panel) in self.panels.iter().enumerate() {
                    // A busy pane keeps its stop: the user can Tab onto it to
                    // watch what it is doing, and back out of it with q there.
                    stops.push(Stop::Panel(i));
                    let shows_info = matches!(
                        panel.current_screen(),
                        Screen::Main(state) if !state.info_panel_hidden
                    );
                    // Its info panel is another matter — it sits under the
                    // overlay and has nothing to edit until the wait is over, so
                    // it would be a dead stop in the rotation.
                    if shows_info && !panel.is_busy() {
                        stops.push(Stop::Info(i));
                    }
                }
                if self.transfer_area.is_some() {
                    stops.push(Stop::Sidebar);
                }
                if self.preview_open {
                    stops.push(Stop::Preview);
                }

                let current = if self.info_focused {
                    Stop::Info(self.active)
                } else if self.preview_focused {
                    Stop::Preview
                } else if self.transfer_focused {
                    Stop::Sidebar
                } else {
                    Stop::Panel(self.active)
                };

                if stops.len() > 1 {
                    let at = stops.iter().position(|s| *s == current).unwrap_or(0);
                    match &stops[(at + 1) % stops.len()] {
                        Stop::Sidebar => self.focus_transfers(),
                        Stop::Preview => self.focus_preview(),
                        Stop::Info(i) => {
                            // The info panel belongs to its panel, so focusing
                            // it makes that panel active — the fields it edits
                            // are that panel's selection.
                            self.active = *i;
                            self.focus_info();
                        }
                        Stop::Panel(i) => {
                            self.transfer_focused = false;
                            self.preview_focused = false;
                            self.info_focused = false;
                            self.transfer_cursor = None;
                            self.active = *i;
                        }
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
                        // A pane that is *currently* shallow stays shallow when
                        // you walk into a subdirectory. What is on screen wins
                        // over what was recorded for the destination: climbing
                        // out of a shallow tree with `h` and stepping back into
                        // a directory recorded as measured would otherwise
                        // launch the full walk the pane was visibly avoiding,
                        // with the footer still reading "shallow".
                        //
                        // Otherwise honour whatever was decided for this
                        // directory last time — somewhere not worth walking
                        // stays that way instead of measuring again on every
                        // arrival — and failing that the session's mode, since a
                        // subdirectory entered under `-s` is not a reason to
                        // start measuring.
                        let shallow = source.is_shallow()
                            || self
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
            Action::OpenWith => {
                self.open_with_program();
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
            Action::CreateArchive => {
                self.open_create_archive_dialog();
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
                    // and there is any way up at all, fall through to the pop or
                    // re-root below instead of collapsing the root.
                    let at_root_line = state
                        .tree
                        .selected_line()
                        .map(|l| l.depth == 0)
                        .unwrap_or(false);
                    let can_pop = stack_len > 1 && !dir_picker_below;
                    // Re-rooting is a way up too, so it must suppress the
                    // collapse exactly as a poppable stack does — otherwise the
                    // first `h` of every level is spent collapsing the root and
                    // going up costs two presses instead of one.
                    let can_reroot = state.root_path().parent().is_some();
                    if !(at_root_line && (can_pop || can_reroot)) {
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

                // At the root with no screen to pop: re-root one level up rather
                // than doing nothing. The panel's starting directory was
                // otherwise a hard ceiling, which is most obvious on a remote
                // opened at a path — `sftp://host:c` landed in ~/c with an empty
                // stack, so there was no way up short of retyping a URL in `g d`.
                if at_root && self.reroot_to_parent() {
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
                    | Action::OpenWith
                    | Action::TogglePreview
                    | Action::Redraw
                    | Action::ToggleShallow
                    | Action::OpenSortMenu
                    | Action::SetSort(_)
                    | Action::PatternRename
                    | Action::CreateArchive
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

        // The open dialog owns its keys for the same reason the rename dialog
        // does: it is a form, and its field has to take every printable
        // character rather than have `q` quit underneath it.
        if let Modal::Open(dialog) = &mut self.modal {
            let outcome = dialog.handle_key(key);
            return self.apply_open_dialog_outcome(outcome);
        }

        // And the attribute dialog, for the same reason: its field has to take
        // every printable character, including the `r`, `w` and `x` of a
        // symbolic mode.
        if let Modal::Attr(dialog) = &mut self.modal {
            let outcome = dialog.handle_key(key);
            return self.apply_attr_dialog_outcome(outcome);
        }

        // And the archive dialog, whose name field has to take every printable
        // character — including the digits that pick a format when the radio
        // group has the focus instead.
        if let Modal::CreateArchive(dialog) = &mut self.modal {
            let outcome = dialog.handle_key(key);
            return self.apply_create_archive_outcome(outcome);
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
                            | ModalTarget::QuitConfirm
                            | ModalTarget::ArchiveOverwrite { .. } => {}
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
            Modal::Help(_) => {
                // Dismiss help — the real key is handled by handle_key.
                self.modal = Modal::None;
                true
            }
            // Handled by the early returns at the top of this function, which
            // need `&mut self` for the whole call and so can't sit in this match.
            Modal::SortMenu(_)
            | Modal::Rename(_)
            | Modal::Open(_)
            | Modal::Attr(_)
            | Modal::CreateArchive(_) => true,
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
                    "Could not parse '{}': {}\n\nExpected: label = sftp://[user@]host[:port][:path|/path]",
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

    /// The files an operation acts on: the tagged set, or the cursor's file when
    /// nothing is tagged, in the order they appear on screen.
    ///
    /// Mirrors how `D` chooses its targets, so "what does this act on" has one
    /// answer across the app rather than one per operation. Shared by the
    /// patterned rename and by `O`.
    fn selection_targets(&self) -> Vec<PathBuf> {
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
        let targets = self.selection_targets();
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

        let targets = self.selection_targets();
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
        // The wait belongs to the pane being dialled, not to the app. An
        // app-wide modal here meant a connect to an unreachable host froze both
        // panes until it timed out — the handshake was already in the
        // background, but every key was being swallowed by the overlay.
        //
        // No cancel token: there is nothing cooperative to trip inside a russh
        // handshake, so `q`/`Esc` detaches the task through `cancel_connect`.
        if let Some(panel) = self.panels.get_mut(target_panel) {
            panel.set_busy("Connecting", None, None);
        }
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
        // interface look wedged — in the pane doing the reading, so the other
        // pane stays usable while a large archive is indexed.
        //
        // No cancel token: `ArchiveFs::open` is a synchronous parse with nothing
        // cooperative to trip. `q` takes the overlay down and lets the task
        // finish unobserved, which `resolve_archive_open` handles by finding no
        // panel still waiting.
        let target = self.active;
        if let Some(panel) = self.panels.get_mut(target) {
            panel.set_busy("Reading archive", None, None);
        }
    }

    /// Register a freshly indexed archive and open a panel on it.
    fn resolve_archive_open(&mut self) {
        use crate::widget::source::{RemoteSource, Source};

        let Some(task) = self.archive_open_task.as_mut() else {
            return;
        };
        let waiting_panel = task.target_panel;
        let result = match task.rx.try_recv() {
            Ok(r) => r,
            // Still indexing.
            Err(tokio::sync::oneshot::error::TryRecvError::Empty) => return,
            Err(tokio::sync::oneshot::error::TryRecvError::Closed) => {
                self.archive_open_task = None;
                self.clear_busy_verb(waiting_panel, "Reading archive");
                return;
            }
        };
        let Some(task) = self.archive_open_task.take() else {
            return;
        };
        // The overlay goes now, whatever the answer: an error replaces it with
        // its own dialog, and success has a panel to show.
        self.clear_busy_verb(waiting_panel, "Reading archive");

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

    /// Open the dialog that names a new archive of the selection.
    ///
    /// Two refusals, and they are different questions. A read-only backend is
    /// an archive, which cannot be written into at all; a remote panel is
    /// writable but its files are not on this disk, and the writer reads
    /// through `std::fs`. Without the second check `gz` on an SFTP panel would
    /// quietly archive whatever *local* paths happened to share those names —
    /// so the guard `refuse_if_read_only` provides is not enough on its own,
    /// which its own doc says outright.
    fn open_create_archive_dialog(&mut self) {
        if self.active_backend_is_read_only() {
            self.modal = Modal::Confirm(ConfirmDialog::notice(
                "Cannot create an archive inside another archive — archives are \
                 read-only here. Copy what you need out first (c), then archive \
                 the copy.",
            ));
            return;
        }
        if !self.active_panel().backend.is_local() {
            self.modal = Modal::Confirm(ConfirmDialog::notice(
                "Cannot create an archive on a remote panel — the files would have \
                 to be downloaded whole first. Copy them here (c), then archive them.",
            ));
            return;
        }
        // One at a time, for a stronger reason than the indexing path has: two
        // writes addressed to the same name would race for the same file.
        if self.archive_create_task.is_some() {
            self.modal = Modal::Confirm(ConfirmDialog::notice(
                "An archive is already being created. Wait for it to finish, or \
                 press q to stop it.",
            ));
            return;
        }

        let sources = self.selection_targets();
        if sources.is_empty() {
            self.modal = Modal::Confirm(ConfirmDialog::notice(
                "Nothing to archive. Tag files with t, or put the cursor on one.",
            ));
            return;
        }
        // The pane root, not `dest_dir`. Those two answer different questions
        // and only one of them applies here: `dest_dir` is where a copy *into*
        // this panel should land, so it follows the cursor — which is right for
        // `c`, where the cursor marks the destination. Here the cursor marks the
        // *source*. Following it put the archive of `code/booker2` inside
        // `code/booker2`, which is both surprising and the one directory the
        // archive should not be in.
        let Some(dest_dir) = self.active_panel().current_dir() else {
            return;
        };

        self.modal = Modal::CreateArchive(crate::widget::archive_dialog::ArchiveDialog::new(
            sources,
            dest_dir,
            crate::prefs::startup().default_archive_format,
        ));
    }

    /// Apply what the archive dialog decided.
    fn apply_create_archive_outcome(
        &mut self,
        outcome: crate::widget::archive_dialog::ArchiveDialogOutcome,
    ) -> bool {
        use crate::widget::archive_dialog::ArchiveDialogOutcome;

        match outcome {
            ArchiveDialogOutcome::Continue => true,
            ArchiveDialogOutcome::Cancelled => {
                self.modal = Modal::None;
                true
            }
            ArchiveDialogOutcome::Create { name, format } => {
                let Modal::CreateArchive(dialog) = &self.modal else {
                    return true;
                };
                let sources = dialog.sources().to_vec();
                let dest_dir = dialog.dest_dir().clone();
                self.modal = Modal::None;

                // A path is a different feature. The archive goes in the
                // directory the panel is scoped to, and silently creating
                // `a/b.zip` where `a` may not exist is a worse answer than
                // saying no to it.
                if name.contains('/') || name.contains('\\') {
                    self.modal = Modal::Confirm(ConfirmDialog::notice(format!(
                        "'{name}' is a path, not a name. The archive is created in \
                         the current directory."
                    )));
                    return true;
                }
                // The radio is the declared intent, so it wins over a typed
                // extension — but by appending rather than replacing, since
                // `notes.txt` is a stem worth keeping in `notes.txt.zip`.
                let name = if crate::vfs::archive::writer::strip_known_extension(&name)
                    == name.as_str()
                {
                    format!("{name}.{}", format.extension())
                } else {
                    crate::vfs::archive::writer::with_extension_for(&name, format)
                };

                let req = crate::vfs::archive::WriteRequest {
                    dest: dest_dir.join(&name),
                    format,
                    sources,
                };
                if req.dest.exists() {
                    // The same question, and the same words, the copy path
                    // already asks when a destination is taken.
                    self.modal = Modal::Confirm(ConfirmDialog::new(format!(
                        "'{name}' exists. Overwrite?"
                    )));
                    self.modal_target = Some(ModalTarget::ArchiveOverwrite { req });
                    return true;
                }
                self.spawn_create_archive(req);
                true
            }
        }
    }

    /// Start writing an archive off the event loop.
    ///
    /// Unlike indexing, this passes a cancel token: the write checks it once
    /// per entry, so `q` on the busy panel stops a large archive part way and
    /// takes the partial file with it.
    fn spawn_create_archive(&mut self, req: crate::vfs::archive::WriteRequest) {
        let progress = crate::widget::progress::OpProgress::new();
        let cancel = CancelToken::new();
        let (tx, rx) = tokio::sync::oneshot::channel();

        let dest = req.dest.clone();
        let worker_progress = progress.clone();
        let worker_cancel = cancel.clone();
        // `spawn_blocking`, not `spawn`: compressing is CPU-bound and would
        // otherwise stall every other task sharing that worker thread.
        tokio::task::spawn_blocking(move || {
            let result =
                crate::vfs::archive::create_archive(&req, Some(&worker_progress), &worker_cancel);
            let _ = tx.send(result);
        });

        let target = self.active;
        self.archive_create_task = Some(ArchiveCreateTask {
            rx,
            dest,
            target_panel: target,
        });
        if let Some(panel) = self.panels.get_mut(target) {
            panel.set_busy("Creating archive", Some(progress), Some(cancel));
        }
    }

    /// Land a finished archive: clear the overlay, report a failure, and show
    /// the new file.
    fn resolve_create_archive(&mut self) {
        let Some(task) = self.archive_create_task.as_mut() else {
            return;
        };
        let waiting_panel = task.target_panel;
        let result = match task.rx.try_recv() {
            Ok(r) => r,
            // Still writing.
            Err(tokio::sync::oneshot::error::TryRecvError::Empty) => return,
            Err(tokio::sync::oneshot::error::TryRecvError::Closed) => {
                self.archive_create_task = None;
                self.clear_busy_verb(waiting_panel, "Creating archive");
                return;
            }
        };
        let Some(task) = self.archive_create_task.take() else {
            return;
        };
        self.clear_busy_verb(waiting_panel, "Creating archive");

        if let Err(e) = result {
            // A cancel is the user's own doing and already visible — the
            // overlay went with it — so it is not reported as a failure.
            if !e.to_string().contains("cancelled") {
                self.modal = Modal::Confirm(ConfirmDialog::notice(format!(
                    "Could not create the archive: {}",
                    explain_error(&e)
                )));
            }
            return;
        }

        // Tags are staged input to the archive, as they are to a copy — clear
        // them once it lands.
        if let Some(panel) = self.panels.get_mut(waiting_panel) {
            panel.current_screen_mut().clear_tags();
        }
        // Show the new file. A targeted reload of the one directory level it
        // landed in, not a full refresh of the tree.
        if let Some(parent) = task.dest.parent() {
            let parent = parent.to_path_buf();
            for panel in &mut self.panels {
                if !panel.backend.is_local() {
                    continue;
                }
                if let Screen::Main(state) = panel.current_screen_mut() {
                    state.reload_dir_public(&parent);
                }
            }
        }
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

    /// Take a panel out of the given busy state, revealing its screen again.
    ///
    /// Matched on the verb so a stale resolver cannot clear an overlay that now
    /// belongs to a *different* operation — the user can start a move in a pane
    /// whose earlier connect is only just being reaped.
    fn clear_busy_verb(&mut self, panel: usize, verb: &str) {
        if let Some(panel) = self.panels.get_mut(panel) {
            if panel.busy.as_ref().is_some_and(|b| b.verb == verb) {
                panel.cancel_busy();
            }
        }
    }

    /// Take a panel out of its "Connecting" state. A no-op when that panel is
    /// not waiting on a connect, so it is safe to call on every outcome.
    fn clear_connect_busy(&mut self, panel: usize) {
        self.clear_busy_verb(panel, "Connecting");
    }

    /// The panel a connection attempt is opening into, if one is in flight.
    fn connecting_panel(&self) -> Option<usize> {
        self.connect_task.as_ref().map(|t| t.target_panel)
    }

    /// Abandon an in-flight connection attempt and return to browsing.
    ///
    /// Dropping the task's receiver detaches it; the background connect task
    /// runs to completion on its own and its result is discarded. Cheaper and
    /// simpler than trying to interrupt a russh handshake mid-flight.
    fn cancel_connect(&mut self) {
        if let Some(panel) = self.connecting_panel() {
            self.clear_connect_busy(panel);
        }
        self.connect_task = None;
        self.pending_connect = None;
        // A cancelled attempt must not promote the host in the picker's
        // ranking: it never connected.
        self.connecting_label = None;
        self.modal = Modal::None;
    }

    /// Everything needed to bail out of remote work immediately, for Ctrl-C:
    /// drop any connection attempt and signal every in-flight scan to stop.
    fn abort_remote_work(&mut self) {
        self.connect_task = None;
        self.pending_connect = None;
        for panel in &mut self.panels {
            let screen = panel.current_screen();
            if screen.is_loading() {
                screen.cancel_loading();
            }
            // Stop anything a pane is waiting on too, so Ctrl-C leaves nothing
            // spinning on the way out.
            panel.cancel_busy();
        }
        self.transfers.cancel_all();
    }

    /// Poll the in-flight connection. On success, register the backend and open
    /// a remote panel; on a credential request, prompt; on failure, report it.
    fn resolve_connect(&mut self) {
        let Some(task) = self.connect_task.as_mut() else {
            return;
        };
        // Read before the receiver is borrowed: every arm below needs to know
        // which pane was waiting, including the one where the task vanished.
        let target_panel = task.target_panel;
        let result = match task.rx.try_recv() {
            Ok(r) => r,
            // Still connecting, or the task vanished.
            Err(tokio::sync::oneshot::error::TryRecvError::Empty) => return,
            Err(tokio::sync::oneshot::error::TryRecvError::Closed) => {
                self.connect_task = None;
                self.clear_connect_busy(target_panel);
                return;
            }
        };
        self.connect_task = None;

        match result {
            ConnectResult::Connected(vfs, home) => {
                // Read before the state is cleared below: whether this pane was
                // the one waiting decides if focus should follow the connection.
                let was_waiting = self
                    .panels
                    .get(target_panel)
                    .and_then(|p| p.busy.as_ref())
                    .is_some_and(|b| b.verb == "Connecting");
                self.clear_connect_busy(target_panel);
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

                // Focus follows the connection only if the user is still on the
                // pane that asked for it. Connecting used to block everything,
                // so focus could not have moved and taking it back was free;
                // now that the other pane stays usable, the user can be part way
                // through moving around in it when the host finally answers.
                // Pulling the keyboard away at that moment means the next
                // keystroke lands on a different tree than the one being looked
                // at — which is how the wrong file gets acted on.
                if self.active == panel || !was_waiting {
                    self.active = panel;
                }

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
                // Put the pane back to what it was showing before reporting: a
                // failed connect must not leave it blank.
                self.clear_connect_busy(target_panel);
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
        // Stop the pane's spinner while the prompt is up: nothing is in flight
        // until the answer comes back, and a "Connecting" overlay behind a
        // password box would claim otherwise. The prompt stays modal on
        // purpose — a secret typed into the wrong pane is worse than a wait.
        self.clear_connect_busy(target_panel);
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
            // Nothing survived the collision prompts; close the last one.
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
        // Shown in the pane the files are landing in, since that is where the
        // result will appear. Only same-backend copies reach here at all —
        // anything cross-backend goes through the transfer queue, which has
        // never blocked — so this is the local case, fast on a local disk and
        // worth not freezing the other pane on a slow mount.
        //
        // No cancel token: this path has never had one (`op_cancel` is set by
        // move and delete only), and adding cancellation to it is a change of
        // behaviour rather than of layout.
        let dest = self.copy_dest_panel;
        let progress_handle = self.op_progress.clone();
        if let Some(panel) = self.panels.get_mut(dest) {
            panel.set_busy("Copying", progress_handle, None);
        }
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
        // On the pane the files are leaving, which is the one whose tree
        // changes as they go. On a remote panel this is one round trip per
        // entry, so it is the slowest of the four and the one that most needed
        // to stop holding the other pane hostage.
        let progress_handle = self.op_progress.clone();
        let cancel_handle = self.op_cancel.clone();
        self.panels[src_panel].set_busy("Moving", progress_handle, cancel_handle);
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
