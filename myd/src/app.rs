use anyhow::Result;
use crossterm::event::{Event, KeyCode, KeyEvent, KeyEventKind};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use crate::keybinding::{Action, KeyBindingHandler};
use crate::screen::{LoadingOutcome, MainScreenState, Screen};
use crate::widget::confirm_dialog::ConfirmDialog;
use crate::widget::help::render_help;
use crate::widget::input_dialog::InputDialog;
use crate::widget::progress::ProgressOverlay;
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

/// Modal overlay state.
pub enum Modal {
    None,
    Confirm(ConfirmDialog),
    Input(InputDialog),
    Progress(ProgressOverlay),
    Deleting { path: PathBuf },
    Help,
}

/// Context for modal operations.
pub enum ModalTarget {
    Delete { path: PathBuf },
    Rename { old_path: PathBuf },
    ChangeRoot,
    Search,
}

use tokio::task::JoinHandle;

/// View preferences that belong to the session rather than to one directory.
///
/// Entering a directory builds a fresh `MainScreenState`, so these are held on
/// the app and applied to each new screen — otherwise hiding the info panel or
/// switching to the treemap would silently undo itself on every navigation.
#[derive(Debug, Clone, Copy)]
pub struct ViewPrefs {
    pub info_panel_hidden: bool,
    pub focus: FocusTarget,
}

impl Default for ViewPrefs {
    fn default() -> Self {
        Self {
            // The file listing is the reason to open the app; the info pane is
            // detail on demand. Start with it closed and let `t` bring it up.
            info_panel_hidden: true,
            focus: FocusTarget::default(),
        }
    }
}

/// Main application state machine.
pub struct FileBrowser {
    screen_stack: Vec<Screen>,
    key_handler: KeyBindingHandler,
    modal: Modal,
    modal_target: Option<ModalTarget>,
    /// Background delete task (for progress tracking).
    delete_task: Option<JoinHandle<()>>,
    /// Carried onto every newly created main screen.
    view_prefs: ViewPrefs,
}

impl FileBrowser {
    pub fn new(start_path: Option<PathBuf>) -> Self {
        let mut screen_stack = Vec::new();

        match start_path {
            Some(path) => {
                let resolved = path.expand_user().canonicalize().unwrap_or(path);
                if resolved.is_dir() {
                    screen_stack.push(Screen::loading(resolved));
                } else {
                    screen_stack.push(Screen::dir_picker());
                }
            }
            None => {
                screen_stack.push(Screen::dir_picker());
            }
        }

        Self {
            screen_stack,
            key_handler: KeyBindingHandler::new(),
            modal: Modal::None,
            modal_target: None,
            delete_task: None,
            view_prefs: ViewPrefs::default(),
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
                        if matches!(self.modal, Modal::None) {
                            running = self.handle_key(key);
                        } else if matches!(self.modal, Modal::Help) {
                            // Dismiss help. Don't re-process Esc to avoid exiting.
                            self.modal = Modal::None;
                            if !matches!(key.code, KeyCode::Esc) {
                                running = self.handle_key(key);
                            }
                        } else {
                            running = self.handle_modal_key(key);
                        }
                    }
                }
            }

            // Check for completed loading tasks and replace with Main screens.
            // A cancelled first-screen scan tells us to quit.
            if !self.resolve_loading() {
                running = false;
            }

            // Check for completed delete tasks.
            self.resolve_deleting();

            terminal.draw(|f| {
                let current = self.screen_stack.last_mut().expect("empty stack");
                current.render(f, f.area());

                match &self.modal {
                    Modal::Confirm(d) => d.render(f, f.area()),
                    Modal::Input(d) => d.render(f, f.area()),
                    Modal::Progress(p) => p.render(f, f.area()),
                    Modal::Help => render_help(f, f.area()),
                    Modal::Deleting { .. } => {
                        let overlay = ProgressOverlay::new().with_message("  Deleting...");
                        overlay.render(f, f.area());
                    }
                    Modal::None => {}
                }
            })?;
        }

        // Guard's Drop will handle cleanup, but explicitly clear the flag so
        // the guard doesn't double-restore if called before drop.
        Ok(())
    }

    /// Pop the top screen and apply the session's view preferences to the
    /// screen it reveals.
    ///
    /// A parent screen still on the stack keeps whatever focus it had when we
    /// descended, which may be stale — e.g. we entered in tree view and later
    /// switched the child to the treemap. Re-applying the current prefs keeps
    /// the view consistent across a pop, just as it is across a push.
    fn pop_screen(&mut self) {
        self.screen_stack.pop();
        let prefs = self.view_prefs;
        if let Some(Screen::Main(state)) = self.screen_stack.last_mut() {
            state.apply_view_prefs(prefs.info_panel_hidden, prefs.focus);
        }
    }

    /// Check for completed loading tasks and replace Loading screens with Main.
    /// Returns `false` when a cancelled scan was the only screen (nothing to go
    /// back to), signalling the app to quit.
    fn resolve_loading(&mut self) -> bool {
        let prefs = self.view_prefs;
        let outcome = match self.screen_stack.last_mut() {
            Some(last) => last.poll_loading(),
            None => return true,
        };
        match outcome {
            LoadingOutcome::Pending => true,
            LoadingOutcome::Done(path, tree) => {
                let mut state = MainScreenState::from_tree(path, tree);
                // Carry the session's view preferences onto the new screen so
                // entering a directory doesn't silently reset them.
                state.apply_view_prefs(prefs.info_panel_hidden, prefs.focus);
                *self.screen_stack.last_mut().expect("empty stack") = Screen::Main(state);
                true
            }
            LoadingOutcome::Cancelled => {
                // Drop the loading screen. If a screen remains beneath it we
                // return to that directory; otherwise there is nothing to show
                // and the app exits.
                if self.screen_stack.len() > 1 {
                    self.pop_screen();
                    true
                } else {
                    false
                }
            }
        }
    }

    /// Check for completed delete tasks and remove from tree.
    fn resolve_deleting(&mut self) {
        if let Some(ref task) = self.delete_task {
            if task.is_finished() {
                if let Modal::Deleting { path } = &self.modal {
                    let p = path.clone();
                    self.delete_task.take();
                    self.modal = Modal::None;
                    let current = self.screen_stack.last_mut().expect("empty stack");
                    current.remove_path(&p);
                }
            }
        }
    }

    /// Drive one key through the app, exactly as the event loop does.
    /// Exposed so tests can exercise real key sequences end to end.
    pub fn handle_key_for_test(&mut self, key: KeyEvent) -> bool {
        self.handle_key(key)
    }

    /// The screen currently on top of the stack.
    pub fn current_screen(&self) -> &Screen {
        self.screen_stack.last().expect("empty stack")
    }

    /// Number of screens on the navigation stack (for tests).
    pub fn screen_stack_depth(&self) -> usize {
        self.screen_stack.len()
    }

    /// Mutable access to the top screen (for tests that need to render it).
    pub fn current_screen_mut(&mut self) -> &mut Screen {
        self.screen_stack.last_mut().expect("empty stack")
    }

    /// Resolve any pending Loading screen into a Main screen (normally driven
    /// by the event loop each tick). Returns false if the app should now quit
    /// (a cancelled first-screen scan).
    pub fn resolve_loading_for_test(&mut self) -> bool {
        self.resolve_loading()
    }

    fn handle_key(&mut self, key: KeyEvent) -> bool {
        // Let the current screen handle raw keys first (e.g., dir picker input).
        if let Some(result) = self.screen_stack.last_mut().expect("empty stack").handle_raw_key(key) {
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
        match action {
            Action::Quit => {
                // While a directory is being scanned, q/Esc cancels that scan
                // rather than quitting outright. The background walk stops
                // promptly, and resolve_loading then returns to the previous
                // directory (or quits if the scan was the very first screen).
                if let Some(screen) = self.screen_stack.last() {
                    if screen.is_loading() {
                        screen.cancel_loading();
                        return true;
                    }
                }
                // Otherwise quit the app immediately, regardless of history.
                // Use Ctrl-o (PopScreen) to step back up a directory.
                false
            }
            Action::PopScreen => {
                if self.screen_stack.len() > 1 {
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
                // Confirm dir picker selection → push loading screen.
                if let Some(Screen::DirPicker(state)) = self.screen_stack.last_mut() {
                    if let Some(path) = state.confirm() {
                        self.screen_stack.push(Screen::loading(path));
                    }
                    return true;
                }
                // Enter on a directory in main screen → navigate into it.
                // Extract the path first to avoid double borrow.
                let target = if let Some(Screen::Main(state)) = self.screen_stack.last() {
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
                    self.screen_stack
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
                self.modal = Modal::Input(InputDialog::new("Search files:", "/pattern/"));
                true
            }
            Action::Collapse => {
                // Capture stack state before mutable borrow.
                let stack_len = self.screen_stack.len();
                let dir_picker_below = matches!(
                    self.screen_stack.get(self.screen_stack.len().saturating_sub(2)),
                    Some(Screen::DirPicker(_))
                );
                let current = self.screen_stack.last_mut().expect("empty stack");

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

                    // Tree view: if on an expanded directory, collapse it in place.
                    let is_expanded = state.tree.is_cursor_expanded();
                    let is_dir = state.tree.selected_line().map(|l| l.is_dir).unwrap_or(false);
                    if is_dir && is_expanded {
                        return state.collapse();
                    }
                }

                // Drop mutable borrow before accessing screen_stack again.
                let at_root = { current.selected_line_depth().unwrap_or(0) == 0 };

                if at_root && stack_len > 1 && !dir_picker_below {
                    self.pop_screen();
                    return true;
                }

                // Move cursor to parent line.
                let current = self.screen_stack.last_mut().expect("empty stack");
                current.go_parent()
            }
            Action::GoDirPicker => {
                // Check if DirPicker already exists in the stack.
                let has_dir_picker = self
                    .screen_stack
                    .iter()
                    .any(|s| matches!(s, Screen::DirPicker(_)));
                if has_dir_picker {
                    // Pop screens until the DirPicker is on top.
                    while self.screen_stack.len() > 1
                        && !matches!(self.screen_stack.last(), Some(Screen::DirPicker(_)))
                    {
                        self.screen_stack.pop();
                    }
                } else {
                    // No DirPicker: replace current screen with a fresh one.
                    *self.screen_stack.last_mut().expect("empty stack") = Screen::dir_picker();
                }
                true
            }
            _ => {
                let current = self.screen_stack.last_mut().expect("empty stack");
                match action {
                    Action::CursorDown => current.cursor_down(),
                    Action::CursorUp => current.cursor_up(),
                    Action::Expand => current.expand(),
                    Action::ToTop => current.to_top(),
                    Action::ToBottom => current.to_bottom(),
                    Action::PageDown => current.page_down(),
                    Action::PageUp => current.page_up(),
                    Action::GoParent => current.go_parent(),
                    Action::Delete => {
                        if let Screen::Main(state) = current {
                            if let Some(resolved) = state.selected_resolved_path() {
                                let name = resolved.file_name().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
                                self.modal_target = Some(ModalTarget::Delete { path: resolved.clone() });
                                self.modal = Modal::Confirm(ConfirmDialog::new(format!("Delete '{}'?", name)));
                            }
                        }
                        true
                    }
                    Action::Refresh => current.refresh(),
                    Action::Rename => {
                        if let Screen::Main(state) = current {
                            if let Some(line) = state.selected_path() {
                                let name = line.file_name().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
                                self.modal_target = Some(ModalTarget::Rename { old_path: line.clone() });
                                self.modal = Modal::Input(
                                    InputDialog::new(format!("Rename '{}':", name), "")
                                        .with_default(name),
                                );
                            }
                        }
                        true
                    }
                    Action::ToggleSort => current.toggle_sort(),
                    Action::ToggleHidden => current.toggle_hidden(),
                    Action::ToggleBar => current.toggle_bar(),
                    Action::CollapseAll => current.collapse_all(),
                    Action::ExpandAll => current.expand_all(),
                    Action::ToggleInfoPanel => {
                        if let Screen::Main(state) = current {
                            state.info_panel_hidden = !state.info_panel_hidden;
                            // Remember it for screens opened later this session.
                            self.view_prefs.info_panel_hidden = state.info_panel_hidden;
                        }
                        true
                    }
                    Action::ToggleView => {
                        let result = current.toggle_view();
                        if let Some(Screen::Main(state)) = self.screen_stack.last() {
                            self.view_prefs.focus = state.focus;
                        }
                        result
                    }
                    Action::Quit | Action::PopScreen | Action::Help | Action::Confirm
                    | Action::ChangeRoot | Action::Search | Action::Collapse
                    | Action::GoDirPicker => unreachable!(),
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
                    if let Some(ModalTarget::Delete { path }) = self.modal_target.take() {
                        if result {
                            let p = path.clone();
                            let task = tokio::spawn(async move {
                                let _ = delete_path(&p);
                            });
                            self.delete_task = Some(task);
                            self.modal = Modal::Deleting { path };
                        }
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
                                let current = self.screen_stack.last_mut().expect("empty stack");
                                current.refresh();
                            }
                            ModalTarget::ChangeRoot => {
                                if !value.is_empty() {
                                    let path = PathBuf::from(&value).expand_user()
                                        .canonicalize().unwrap_or(PathBuf::from(&value));
                                    if path.is_dir() {
                                        self.screen_stack.push(Screen::loading(path));
                                    }
                                }
                            }
                            ModalTarget::Search => {
                                if !value.is_empty() {
                                    let current = self.screen_stack.last_mut().expect("empty stack");
                                    current.search(&value);
                                }
                            }
                            ModalTarget::Delete { .. } => {}
                        }
                    }
                }
                true
            }
            Modal::Progress(_) => true,
            Modal::Deleting { .. } => true,
            Modal::Help => {
                // Dismiss help — the real key is handled by handle_key.
                self.modal = Modal::None;
                true
            }
            Modal::None => true,
        }
    }
}

/// Recursively delete a path.
fn delete_path(path: &Path) -> std::io::Result<()> {
    if path.is_dir() {
        std::fs::remove_dir_all(path)
    } else {
        std::fs::remove_file(path)
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
