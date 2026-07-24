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
    /// A cross-panel copy is running in the background.
    Copying,
    Help,
}

/// Context for modal operations.
pub enum ModalTarget {
    Delete { path: PathBuf },
    Rename { old_path: PathBuf },
    ChangeRoot,
    Search,
    /// Overwrite confirmation for a cross-panel copy. `dest_panel` is the panel
    /// to refresh once the copy lands.
    Copy { src: PathBuf, dest: PathBuf, dest_panel: usize },
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
                    Modal::Progress(p) => p.render(f, area),
                    Modal::Copying => {
                        let overlay = ProgressOverlay::new().with_message("  Copying...");
                        overlay.render(f, area);
                    }
                    Modal::Help => render_help(f, area),
                    Modal::None => {
                        if deleting {
                            let overlay = ProgressOverlay::new().with_message("  Deleting...");
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
    /// deleted path from its tree).
    fn resolve_deleting(&mut self) {
        for panel in &mut self.panels {
            panel.resolve_deleting();
        }
    }

    /// Resolve a completed cross-panel copy: clear the overlay and refresh the
    /// destination panel so the copied entry appears.
    fn resolve_copying(&mut self) {
        if let Some(ref task) = self.copy_task {
            if task.is_finished() {
                self.copy_task.take();
                self.modal = Modal::None;
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
                self.modal = Modal::Input(InputDialog::new("Search files:", "/pattern/"));
                true
            }
            Action::Delete => {
                // Extract the target from the active panel before touching the
                // modal fields, so the panel borrow ends first.
                let target = match self.active_panel().current_screen() {
                    Screen::Main(state) => state.selected_resolved_path().cloned(),
                    _ => None,
                };
                if let Some(resolved) = target {
                    let name = resolved
                        .file_name()
                        .map(|s| s.to_string_lossy().to_string())
                        .unwrap_or_default();
                    self.modal_target = Some(ModalTarget::Delete { path: resolved });
                    self.modal = Modal::Confirm(ConfirmDialog::new(format!("Delete '{}'?", name)));
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

                    // Tree view: if on an expanded directory, collapse it in place.
                    let is_expanded = state.tree.is_cursor_expanded();
                    let is_dir = state.tree.selected_line().map(|l| l.is_dir).unwrap_or(false);
                    if is_dir && is_expanded {
                        return state.collapse();
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
                    | Action::Copy | Action::Delete | Action::Rename => unreachable!(),
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
                        Some(ModalTarget::Delete { path }) if result => {
                            // Run the delete on the active panel so its tree is
                            // the one refreshed when the task finishes.
                            let p = path.clone();
                            let task = tokio::spawn(async move {
                                let _ = delete_path(&p);
                            });
                            let panel = self.active_panel_mut();
                            panel.delete_task = Some(task);
                            panel.deleting_path = Some(path);
                        }
                        Some(ModalTarget::Copy { src, dest, dest_panel }) if result => {
                            self.spawn_copy(src, dest, dest_panel);
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
                            ModalTarget::Delete { .. } | ModalTarget::Copy { .. } => {}
                        }
                    }
                }
                true
            }
            Modal::Progress(_) => true,
            Modal::Copying => true,
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
            self.panels.push(Panel::new(start));
            // Focus the freshly opened panel.
            self.active = 1;
        }
    }

    /// Begin a cross-panel copy of the active panel's selection into the other
    /// panel's current directory. Pops an overwrite confirmation on collision;
    /// otherwise starts the copy immediately. No-op outside dual mode.
    fn start_copy(&mut self) {
        let Some(other) = self.other_index() else {
            return;
        };
        let Some(src) = self.active_panel().selected_resolved_path() else {
            return;
        };
        let Some(dest_dir) = self.panels[other].current_dir() else {
            return;
        };
        let Some(name) = src.file_name() else {
            return;
        };
        let dest = dest_dir.join(name);

        // Don't let a copy silently consume itself (same file both sides).
        if src == dest {
            return;
        }

        if dest.exists() {
            let display = name.to_string_lossy().to_string();
            self.modal_target = Some(ModalTarget::Copy {
                src,
                dest,
                dest_panel: other,
            });
            self.modal = Modal::Confirm(ConfirmDialog::new(format!(
                "'{}' exists. Overwrite?",
                display
            )));
        } else {
            self.spawn_copy(src, dest, other);
        }
    }

    /// Spawn the background copy task and show the copying overlay. When it
    /// finishes, `resolve_copying` refreshes `dest_panel`.
    fn spawn_copy(&mut self, src: PathBuf, dest: PathBuf, dest_panel: usize) {
        let task = tokio::spawn(async move {
            let _ = copy_path(&src, &dest);
        });
        self.copy_task = Some(task);
        self.copy_dest_panel = dest_panel;
        self.modal = Modal::Copying;
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

/// Copy `src` to `dest`, recursing into directories. Existing files at the
/// destination are overwritten (`std::fs::copy` semantics); the overwrite
/// decision is made by the caller before this runs. Directories are recreated
/// and their contents copied via `walkdir`, which is already a dependency.
fn copy_path(src: &Path, dest: &Path) -> std::io::Result<()> {
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
        }
        Ok(())
    } else {
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::copy(src, dest).map(|_| ())
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
