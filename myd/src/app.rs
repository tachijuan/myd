use anyhow::Result;
use crossterm::event::{Event, KeyCode, KeyEvent, KeyEventKind};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use crate::keybinding::{Action, KeyBindingHandler};
use crate::screen::{MainScreenState, Screen};
use crate::widget::confirm_dialog::ConfirmDialog;
use crate::widget::help::render_help;
use crate::widget::input_dialog::InputDialog;
use crate::widget::progress::ProgressOverlay;

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

/// Main application state machine.
pub struct FileBrowser {
    screen_stack: Vec<Screen>,
    key_handler: KeyBindingHandler,
    modal: Modal,
    modal_target: Option<ModalTarget>,
    /// Background delete task (for progress tracking).
    delete_task: Option<JoinHandle<()>>,
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
            self.resolve_loading();

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

    /// Check for completed loading tasks and replace Loading screens with Main.
    fn resolve_loading(&mut self) {
        if let Some(last) = self.screen_stack.last_mut() {
            if let Some((path, tree)) = last.poll_loading() {
                *last = Screen::Main(MainScreenState::from_tree(path, tree));
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
                // Pop back to the previous screen if there's a Main screen below us.
                if self.screen_stack.len() > 1
                    && !matches!(
                        self.screen_stack.get(self.screen_stack.len().saturating_sub(2)),
                        Some(Screen::DirPicker(_))
                    )
                {
                    self.screen_stack.pop();
                    return true;
                }
                // Otherwise quit the app.
                false
            }
            Action::PopScreen => {
                if self.screen_stack.len() > 1 {
                    self.screen_stack.pop();
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
                let path_to_push = if let Some(Screen::Main(state)) = self.screen_stack.last() {
                    state
                        .selected_path()
                        .filter(|p| p.is_dir())
                        .cloned()
                } else {
                    None
                };
                if let Some(path) = path_to_push {
                    self.screen_stack.push(Screen::loading(path));
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

                // If on an expanded directory, collapse it in place.
                if let Screen::Main(state) = current {
                    let is_expanded = state.tree.is_cursor_expanded();
                    let is_dir = state.tree.selected_line().map(|l| l.is_dir).unwrap_or(false);
                    if is_dir && is_expanded {
                        return state.collapse();
                    }
                }

                // Drop mutable borrow before accessing screen_stack again.
                let at_root = { current.selected_line_depth().unwrap_or(0) == 0 };

                if at_root && stack_len > 1 && !dir_picker_below {
                    self.screen_stack.pop();
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
                        }
                        true
                    }
                    Action::ToggleView => current.toggle_view(),
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
