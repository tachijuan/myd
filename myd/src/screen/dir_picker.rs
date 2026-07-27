use crossterm::event::{KeyCode, KeyEvent};
use ratatui::{
    Frame,
    layout::{Constraint, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span, Text},
    widgets::{Block, BorderType, Borders, Paragraph},
};
use std::path::PathBuf;

/// Which half of the picker has the keyboard.
///
/// The screen is a path field *and* a list, so a bare `j` is ambiguous. Rather
/// than guess, focus is explicit and `Tab` moves it, matching how `Tab` already
/// switches panels in the main view.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PickerFocus {
    /// Typing edits the path; `j`/`k` are ordinary characters.
    Field,
    /// `j`/`k` walk the list; typing a printable character jumps to the field so
    /// starting to type a path never silently does nothing.
    List,
}

/// State for the directory picker startup screen.
pub struct DirPickerState {
    options: Vec<(PathBuf, String)>,
    cursor: usize,
    /// Current input value (typed path).
    input: String,
    /// Input cursor position. Must always be a valid char boundary within
    /// `input` and never past its end — see `set_input`.
    input_cursor: usize,
    /// Whether `input` was filled from the highlighted option rather than typed.
    ///
    /// Browsing the list mirrors the option into the field so the user can see
    /// what Enter would open, but that text is a *suggestion*. The first typed
    /// character replaces it rather than extending it: appending produced a
    /// nonsense concatenation of the option and the typed path, which then
    /// resolved to whichever half happened to exist.
    input_is_suggestion: bool,
    /// Which half of the screen the keyboard drives.
    focus: PickerFocus,
}

impl Default for DirPickerState {
    fn default() -> Self {
        Self::new()
    }
}

impl DirPickerState {
    pub fn new() -> Self {
        let home = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default();
        let cwd = std::env::current_dir().unwrap_or(PathBuf::from("."));

        let common = [
            (home.clone(), "~ (Home)".into()),
            (cwd.clone(), format!(". (Current: {})", cwd.display())),
            (home.join("Desktop"), "Desktop".into()),
            (home.join("Documents"), "Documents".into()),
            (home.join("Downloads"), "Downloads".into()),
            (home.join("Pictures"), "Pictures".into()),
            (home.join("Music"), "Music".into()),
            (home.join("Videos"), "Videos".into()),
            (PathBuf::from("/"), "/ (Root)".into()),
            (PathBuf::from("/tmp"), "/tmp".into()),
        ];

        let options: Vec<(PathBuf, String)> = common
            .into_iter()
            .filter(|(p, _)| p.is_dir())
            .collect();

        Self {
            options,
            cursor: 0,
            input: String::new(),
            input_cursor: 0,
            input_is_suggestion: false,
            // The field starts focused: the picker exists to accept a path, and
            // the common directories are the shortcut, not the main event.
            focus: PickerFocus::Field,
        }
    }

    /// Show `value` in the path field as a *suggestion* from the option list.
    ///
    /// The text cursor goes to the end, and the content is marked so the next
    /// typed character replaces it. Assigning `input` directly left `input_cursor`
    /// at 0, so typing inserted in front of the suggestion and the field became
    /// `<typed><option>` — which resolved to whichever half existed, and looked
    /// exactly like a typed path being ignored.
    fn set_suggestion(&mut self, value: String) {
        self.input_cursor = value.chars().count();
        self.input = value;
        self.input_is_suggestion = true;
    }

    /// Clear a suggestion before the first real edit, so typing replaces the
    /// list's proposal instead of appending to it.
    fn take_over_suggestion(&mut self) {
        if self.input_is_suggestion {
            self.input.clear();
            self.input_cursor = 0;
            self.input_is_suggestion = false;
        }
    }

    /// Which half of the picker currently has the keyboard.
    pub fn focus(&self) -> PickerFocus {
        self.focus
    }

    /// Move the keyboard between the path field and the list.
    pub fn toggle_focus(&mut self) {
        self.focus = match self.focus {
            PickerFocus::Field => PickerFocus::List,
            PickerFocus::List => PickerFocus::Field,
        };
    }

    /// The current contents of the path field. Test hook.
    pub fn input_for_test(&self) -> &str {
        &self.input
    }

    /// The index of the highlighted option. Test hook.
    pub fn cursor_for_test(&self) -> usize {
        self.cursor
    }

    pub fn resolve_path(&self, path_str: &str) -> Option<PathBuf> {
        let path = expand_tilde(path_str);
        let path = path.canonicalize().unwrap_or(path);
        if path.is_dir() {
            Some(path)
        } else {
            None
        }
    }

    /// Byte offset of the text cursor.
    ///
    /// `input_cursor` counts *characters* (the renderer indexes it with
    /// `char_indices`), so every operation on the string has to convert. Mixing
    /// the two would panic on a path containing any non-ASCII character.
    fn cursor_byte(&self) -> usize {
        self.input
            .char_indices()
            .nth(self.input_cursor)
            .map(|(i, _)| i)
            .unwrap_or(self.input.len())
    }

    /// Number of characters in the field.
    fn input_len(&self) -> usize {
        self.input.chars().count()
    }

    /// Handle typed character in the input field.
    pub fn input_char(&mut self, c: char) {
        self.take_over_suggestion();
        let at = self.cursor_byte();
        self.input.insert(at, c);
        self.input_cursor += 1;
    }

    pub fn input_backspace(&mut self) {
        // Backspacing a suggestion clears the whole thing: it was never typed, so
        // erasing it one character at a time is busywork.
        if self.input_is_suggestion {
            self.take_over_suggestion();
            return;
        }
        if self.input_cursor > 0 {
            self.input_cursor -= 1;
            let at = self.cursor_byte();
            self.input.remove(at);
        }
    }

    /// Delete the character under the cursor.
    pub fn input_delete(&mut self) {
        if self.input_is_suggestion {
            self.take_over_suggestion();
            return;
        }
        if self.input_cursor < self.input_len() {
            let at = self.cursor_byte();
            self.input.remove(at);
        }
    }

    pub fn input_left(&mut self) {
        // Deliberately moving inside the text means the user intends to edit it,
        // so keep it rather than discarding it on the next keystroke.
        self.input_is_suggestion = false;
        if self.input_cursor > 0 {
            self.input_cursor -= 1;
        }
    }

    pub fn input_right(&mut self) {
        self.input_is_suggestion = false;
        if self.input_cursor < self.input_len() {
            self.input_cursor += 1;
        }
    }

    /// Highlight option `index` and mirror it into the path field, so the field
    /// always shows what Enter would open.
    fn select(&mut self, index: usize) {
        if self.options.is_empty() {
            return;
        }
        self.cursor = index.min(self.options.len() - 1);
        if let Some((path, _)) = self.options.get(self.cursor) {
            let shown = path.to_string_lossy().to_string();
            self.set_suggestion(shown);
        }
    }

    fn select_next(&mut self) {
        if self.options.is_empty() {
            return;
        }
        let next = (self.cursor + 1) % self.options.len();
        self.select(next);
    }

    fn select_prev(&mut self) {
        if self.options.is_empty() {
            return;
        }
        let prev = if self.cursor == 0 {
            self.options.len() - 1
        } else {
            self.cursor - 1
        };
        self.select(prev);
    }

    fn select_first(&mut self) {
        self.select(0);
    }

    fn select_last(&mut self) {
        self.select(self.options.len().saturating_sub(1));
    }

    /// Confirm the current selection and return the path.
    pub fn confirm(&self) -> Option<PathBuf> {
        if !self.input.is_empty() {
            if let Some(p) = self.resolve_path(&self.input) {
                return Some(p);
            }
        }
        if let Some((path, _)) = self.options.get(self.cursor) {
            return Some(path.clone());
        }
        None
    }

    /// Handle a raw key event for the dir picker's input field.
    /// Returns `Some(true)` to keep running, `Some(false)` to quit,
    /// or `None` if the key was not consumed (fall through to keybinding).
    pub fn handle_key(&mut self, key: KeyEvent) -> Option<bool> {
        use crossterm::event::KeyModifiers;

        // Tab moves the keyboard between the two halves. Checked first so it can
        // never be typed into the path.
        if key.code == KeyCode::Tab {
            self.toggle_focus();
            return Some(true);
        }

        // Ctrl combinations belong to the app (Ctrl+C to quit, and so on); never
        // absorb them into the field.
        if key.modifiers.contains(KeyModifiers::CONTROL) {
            return None;
        }

        // Arrows always drive the list, whichever half has focus: they are
        // unambiguous, and they were the only way to browse before Tab existed.
        match key.code {
            KeyCode::Up => {
                self.select_prev();
                return Some(true);
            }
            KeyCode::Down => {
                self.select_next();
                return Some(true);
            }
            _ => {}
        }

        if self.focus == PickerFocus::List {
            return match key.code {
                // vi motion over the list — what the screen has always claimed
                // these keys did, while the field was in fact swallowing them.
                KeyCode::Char('j') => {
                    self.select_next();
                    Some(true)
                }
                KeyCode::Char('k') => {
                    self.select_prev();
                    Some(true)
                }
                KeyCode::Char('g') => {
                    self.select_first();
                    Some(true)
                }
                KeyCode::Char('G') => {
                    self.select_last();
                    Some(true)
                }
                // Anything else printable is the start of a path, so move the
                // keyboard to the field and take the character with it. Typing a
                // path is the picker's whole purpose; making that a no-op until
                // the user finds Tab would be its own bug.
                KeyCode::Char(c) => {
                    self.focus = PickerFocus::Field;
                    self.input_char(c);
                    Some(true)
                }
                KeyCode::Backspace => {
                    self.focus = PickerFocus::Field;
                    self.input_backspace();
                    Some(true)
                }
                // Enter and Esc are the app's (confirm / go back).
                _ => None,
            };
        }

        match key.code {
            KeyCode::Char(c) => {
                self.input_char(c);
                Some(true)
            }
            KeyCode::Backspace => {
                self.input_backspace();
                Some(true)
            }
            KeyCode::Delete => {
                self.input_delete();
                Some(true)
            }
            KeyCode::Left => {
                self.input_left();
                Some(true)
            }
            KeyCode::Right => {
                self.input_right();
                Some(true)
            }
            KeyCode::Home => {
                self.input_is_suggestion = false;
                self.input_cursor = 0;
                Some(true)
            }
            KeyCode::End => {
                self.input_is_suggestion = false;
                self.input_cursor = self.input_len();
                Some(true)
            }
            _ => None,
        }
    }
}

impl super::ScreenState for DirPickerState {
    fn cursor_down(&mut self) -> bool {
        self.select_next();
        true
    }

    fn cursor_up(&mut self) -> bool {
        self.select_prev();
        true
    }

    fn render(&mut self, frame: &mut Frame, area: Rect) {
        let vertical = Layout::vertical([Constraint::Length(3), Constraint::Length(3), Constraint::Min(1)]).split(area);

        // Title.
        let title = Paragraph::new(Span::styled(
            "Select a directory (Tab switches field/list, Esc to go back)",
            Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD),
        ));
        frame.render_widget(title, vertical[0]);

        // Input field. The block text cursor is drawn only while the field has
        // focus — showing a caret in an unfocused box is what made it look like
        // typing would go there when j/k were in fact driving the list.
        let field_focused = self.focus == PickerFocus::Field;
        let input_line = if !field_focused {
            // Unfocused: show the value (or a hint) with no caret.
            if self.input.is_empty() {
                Line::from(vec![
                    Span::styled("  ", Style::default()),
                    Span::styled(
                        "Tab or type to enter a path...",
                        Style::default().fg(Color::DarkGray),
                    ),
                ])
            } else {
                Line::from(vec![
                    Span::styled("  ", Style::default()),
                    Span::styled(self.input.clone(), Style::default().fg(Color::Yellow)),
                ])
            }
        } else if self.input.is_empty() {
            Line::from(vec![
                Span::styled("> ", Style::default().fg(Color::Yellow)),
                Span::styled("█", Style::default().fg(Color::Yellow)),
                Span::styled(
                    "Enter a path...",
                    Style::default().fg(Color::DarkGray),
                ),
            ])
        } else {
            // Split the value at the cursor and render a block glyph there.
            let cut = self
                .input
                .char_indices()
                .nth(self.input_cursor)
                .map(|(i, _)| i)
                .unwrap_or(self.input.len());
            let (before, after) = self.input.split_at(cut);
            Line::from(vec![
                Span::styled("> ", Style::default().fg(Color::Yellow)),
                Span::styled(before.to_string(), Style::default().fg(Color::Yellow)),
                Span::styled("█", Style::default().fg(Color::Yellow)),
                Span::styled(after.to_string(), Style::default().fg(Color::Yellow)),
            ])
        };
        // Focused pane gets the bright cyan border the main view uses for the
        // active panel; the other goes dark grey. Same visual language throughout.
        let (focused_border, unfocused_border) = (Color::Cyan, Color::DarkGray);
        let input_para = Paragraph::new(input_line).block(
            Block::default()
                .borders(Borders::ALL)
                .border_type(BorderType::Plain)
                .border_style(Style::default().fg(if field_focused {
                    focused_border
                } else {
                    unfocused_border
                }))
                .title(if field_focused {
                    " Path (Enter to go) "
                } else {
                    " Path "
                }),
        );
        frame.render_widget(input_para, vertical[1]);

        // Option list.
        let lines: Text = self
            .options
            .iter()
            .enumerate()
            .map(|(i, (_path, label))| {
                if i == self.cursor {
                    // Reversed only while the list is driving, so the highlight
                    // marks "the keys move this" rather than just "last touched".
                    let style = if field_focused {
                        Style::default().fg(Color::Yellow)
                    } else {
                        Style::default()
                            .fg(Color::Yellow)
                            .add_modifier(Modifier::REVERSED)
                    };
                    Line::from(Span::styled(format!("> {}", label), style))
                } else {
                    Line::from(format!("  {}", label))
                }
            })
            .collect();

        let list = Paragraph::new(lines).block(
            Block::default()
                .borders(Borders::ALL)
                .border_type(BorderType::Plain)
                .border_style(Style::default().fg(if field_focused {
                    unfocused_border
                } else {
                    focused_border
                }))
                // Only claim j/k when they actually work. The title said "j/k to
                // navigate" unconditionally while the path field was swallowing
                // both keys.
                .title(if field_focused {
                    " Common Directories (↑/↓, or Tab for j/k) "
                } else {
                    " Common Directories (j/k to navigate) "
                }),
        );
        frame.render_widget(list, vertical[2]);
    }
}

fn expand_tilde(path: &str) -> PathBuf {
    if path.starts_with("~") {
        if let Some(home) = std::env::var_os("HOME") {
            let mut p = PathBuf::from(home);
            p.push(path.strip_prefix("~").unwrap_or(""));
            return p;
        }
    }
    PathBuf::from(path)
}
