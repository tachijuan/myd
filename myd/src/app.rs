use anyhow::Result;
use crossterm::event::{Event, KeyCode, KeyEvent, KeyEventKind};
use ratatui::layout::{Constraint, Layout};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use crate::keybinding::{Action, KeyBindingHandler};
use crate::panel::Panel;
use crate::screen::Screen;
use crate::widget::treemap::FocusTarget;
use crate::widget::confirm_dialog::ConfirmDialog;
use crate::widget::help::render_help;
use crate::widget::input_dialog::InputDialog;
use crate::widget::progress::{OpProgress, ProgressOverlay};

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

/// Modal overlay state.
pub enum Modal {
    None,
    Confirm(ConfirmDialog),
    Input(InputDialog),
    /// A background copy or delete is running. The verb ("Copying"/"Deleting")
    /// titles the overlay; live counts come from `FileBrowser::op_progress`.
    Operation { verb: &'static str },
    Help,
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
            pending_copies: Vec::new(),
            approved_copies: Vec::new(),
            copy_source_panel: 0,
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

            // Check for completed loading tasks and replace with Main screens.
            // A cancelled scan in the active panel with nothing to fall back to
            // tells us to quit.
            if !self.resolve_loading() {
                running = false;
            }

            // Check for completed delete and copy tasks.
            self.resolve_deleting();
            self.resolve_copying();

            let active = self.active;
            let panel_count = self.panels.len();
            let deleting = self.panels.iter().any(|p| p.is_deleting());
            terminal.draw(|f| {
                let area = f.area();
                if panel_count == 2 {
                    let cols = Layout::horizontal([
                        Constraint::Percentage(50),
                        Constraint::Percentage(50),
                    ])
                    .split(area);
                    for (i, panel) in self.panels.iter_mut().enumerate() {
                        // Tell the top Main screen whether it's the active panel
                        // so its border can stand out.
                        if let Screen::Main(state) = panel.current_screen_mut() {
                            state.active = i == active;
                        }
                        panel.current_screen_mut().render(f, cols[i]);
                    }
                } else {
                    if let Screen::Main(state) = self.panels[0].current_screen_mut() {
                        state.active = true;
                    }
                    self.panels[0].current_screen_mut().render(f, area);
                }

                match &self.modal {
                    Modal::Confirm(d) => d.render(f, area),
                    Modal::Input(d) => d.render(f, area),
                    Modal::Operation { verb } => {
                        let overlay = match &self.op_progress {
                            Some(p) => ProgressOverlay::for_operation(verb, p),
                            None => ProgressOverlay::new().with_message(*verb),
                        };
                        overlay.render(f, area);
                    }
                    Modal::Help => render_help(f, area),
                    Modal::None => {
                        // A delete that started from the confirm dialog also runs
                        // in the background; show its overlay when no modal is up.
                        if deleting {
                            let overlay = match &self.op_progress {
                                Some(p) => ProgressOverlay::for_operation("Deleting", p),
                                None => ProgressOverlay::new().with_message("Deleting"),
                            };
                            overlay.render(f, area);
                        }
                    }
                }
            })?;
        }

        // Guard's Drop will handle cleanup, but explicitly clear the flag so
        // the guard doesn't double-restore if called before drop.
        Ok(())
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
        for panel in &mut self.panels {
            panel.resolve_deleting();
        }
        // Once the last delete finished, drop its progress so the overlay clears.
        if was_deleting && !self.panels.iter().any(|p| p.is_deleting()) {
            self.op_progress = None;
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
        if let Some(result) = self.active_panel_mut().current_screen_mut().handle_raw_key(key) {
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
            Action::Copy => {
                self.start_copy();
                true
            }
            Action::ToggleTag => {
                self.active_panel_mut().current_screen_mut().toggle_tag()
            }
            Action::UntagAll => {
                self.active_panel_mut().current_screen_mut().untag_all()
            }
            Action::VisualMode => {
                self.active_panel_mut().current_screen_mut().toggle_visual()
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
                self.modal_target = Some(ModalTarget::CreateDir);
                self.modal = Modal::Input(InputDialog::new(
                    "New directory name:",
                    "name",
                ));
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
                // Extract the path first to avoid double borrow.
                let target = if let Screen::Main(state) = panel.current_screen() {
                    state
                        .selected_path()
                        .filter(|p| p.is_dir())
                        .cloned()
                        // Hand down the parent's size cache: the subtree was
                        // already measured, so reuse it instead of rescanning.
                        .map(|p| (p, state.tree.size_cache.clone()))
                } else {
                    None
                };
                if let Some((path, cache)) = target {
                    panel
                        .screen_stack
                        .push(Screen::loading_with_cache(path, Some(cache)));
                }
                true
            }
            Action::ChangeRoot => {
                self.modal_target = Some(ModalTarget::ChangeRoot);
                self.modal = Modal::Input(InputDialog::new("Change root directory:", "Enter path..."));
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
                    let at_root_line =
                        state.tree.selected_line().map(|l| l.depth == 0).unwrap_or(false);
                    let can_pop = stack_len > 1 && !dir_picker_below;
                    if !(at_root_line && can_pop) {
                        // Otherwise: on an expanded directory, collapse in place.
                        let is_expanded = state.tree.is_cursor_expanded();
                        let is_dir =
                            state.tree.selected_line().map(|l| l.is_dir).unwrap_or(false);
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
                    // No DirPicker: replace current screen with a fresh one.
                    *panel.current_screen_mut() = Screen::dir_picker();
                }
                true
            }
            _ => {
                let current = self.active_panel_mut().current_screen_mut();
                match action {
                    Action::CursorDown => current.cursor_down(),
                    Action::CursorUp => current.cursor_up(),
                    Action::Expand => current.expand(),
                    Action::ToTop => current.to_top(),
                    Action::ToBottom => current.to_bottom(),
                    Action::PageDown => current.page_down(),
                    Action::PageUp => current.page_up(),
                    Action::GoParent => current.go_parent(),
                    Action::Refresh => current.refresh(),
                    Action::ToggleSort => current.toggle_sort(),
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
                    Action::Quit | Action::PopScreen | Action::Help | Action::Confirm
                    | Action::ChangeRoot | Action::Search | Action::Collapse
                    | Action::GoDirPicker | Action::SwitchPanel | Action::ToggleSplit
                    | Action::Copy | Action::Delete | Action::Rename
                    | Action::ToggleTag | Action::UntagAll | Action::VisualMode
                    | Action::Filter | Action::CreateDir | Action::SearchNext
                    | Action::SearchPrev => unreachable!(),
                    Action::None => true,
                }
            }
        }
    }

    fn handle_modal_key(&mut self, key: KeyEvent) -> bool {
        match &mut self.modal {
            Modal::Confirm(dialog) => {
                if let Some(result) = dialog.handle_key(key_code_char(&key)) {
                    self.modal = Modal::None;
                    match self.modal_target.take() {
                        Some(ModalTarget::Delete { paths }) if result => {
                            self.spawn_delete_batch(paths);
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
                                    let new_path = old_path.parent().unwrap().join(&value);
                                    if let Err(e) = std::fs::rename(&old_path, &new_path) {
                                        eprintln!("Rename failed: {}", e);
                                    }
                                }
                                self.active_panel_mut().current_screen_mut().refresh();
                            }
                            ModalTarget::ChangeRoot => {
                                if !value.is_empty() {
                                    let path = PathBuf::from(&value).expand_user()
                                        .canonicalize().unwrap_or(PathBuf::from(&value));
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
                                self.active_panel_mut()
                                    .current_screen_mut()
                                    .create_dir(&value);
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
                            ModalTarget::Delete { .. }
                            | ModalTarget::CopyOverwrite { .. } => {}
                        }
                    }
                }
                true
            }
            Modal::Operation { .. } => true,
            Modal::Help => {
                // Dismiss help — the real key is handled by handle_key.
                self.modal = Modal::None;
                true
            }
            Modal::None => true,
        }
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
            let start = self.active_panel().current_dir();
            // The new pane opens on the active panel's current directory, which
            // has already been scanned — hand down its size cache so the split
            // is a cache hit instead of a full disk rescan.
            let cache = self.active_panel().size_cache();
            self.panels.push(Panel::new_with_cache(start, cache));
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
                self.begin_copy_batch(srcs, dest_dir, other, source);
            }
            None => {
                // Single-panel: prompt for a destination directory first.
                self.modal_target = Some(ModalTarget::CopyDest { srcs });
                self.modal = Modal::Input(InputDialog::new(
                    "Copy to directory:",
                    "Enter path...",
                ));
            }
        }
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
            let Some(name) = src.file_name() else { continue };
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
            self.modal = Modal::Confirm(ConfirmDialog::new(format!(
                "'{}' exists. Overwrite?",
                name
            )));
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

    /// Delete `paths` in the background, tracking progress, then remove them from
    /// the active panel's tree and clear its tags when the task completes.
    fn spawn_delete_batch(&mut self, paths: Vec<PathBuf>) {
        if paths.is_empty() {
            return;
        }
        let progress = OpProgress::new();
        let total: u64 = paths.iter().map(|p| count_entries(p)).sum();
        progress.set_total(total);
        self.op_progress = Some(progress.clone());

        let to_delete = paths.clone();
        let task = tokio::spawn(async move {
            for p in &to_delete {
                let _ = delete_path(p, Some(&progress));
            }
            progress.finish();
        });

        let panel = self.active_panel_mut();
        panel.delete_task = Some(task);
        panel.deleting_paths = paths;
        // Deleted files were the tags' whole point — clear them now so the UI
        // doesn't keep highlighting rows that are about to vanish.
        panel.current_screen_mut().clear_tags();
    }
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

/// Recursively delete a path, bumping `progress` once per entry removed so a
/// large tree reports live progress. Directories are walked deepest-first so
/// children are gone before their parent is removed.
fn delete_path(path: &Path, progress: Option<&OpProgress>) -> std::io::Result<()> {
    if path.is_dir() {
        // contents_first yields children before their parent directory.
        for entry in walkdir::WalkDir::new(path)
            .contents_first(true)
            .into_iter()
            .filter_map(|e| e.ok())
        {
            if entry.file_type().is_dir() {
                let _ = std::fs::remove_dir(entry.path());
            } else {
                let _ = std::fs::remove_file(entry.path());
            }
            if let Some(p) = progress {
                p.inc_done();
            }
        }
        // The root directory itself (walkdir with contents_first still yields it
        // last, so the loop above already removed it — but guard just in case).
        let _ = std::fs::remove_dir_all(path);
        Ok(())
    } else {
        let res = std::fs::remove_file(path);
        if let Some(p) = progress {
            p.inc_done();
        }
        res
    }
}

/// Copy `src` to `dest`, recursing into directories and bumping `progress` once
/// per entry copied. Existing files at the destination are overwritten
/// (`std::fs::copy` semantics); the overwrite decision is made by the caller
/// before this runs.
fn copy_path(src: &Path, dest: &Path, progress: Option<&OpProgress>) -> std::io::Result<()> {
    if src.is_dir() {
        for entry in walkdir::WalkDir::new(src).into_iter().filter_map(|e| e.ok()) {
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
