use crossterm::event::{KeyCode, KeyEvent};
use ratatui::{
    Frame,
    layout::{Constraint, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span, Text},
    widgets::{Block, BorderType, Borders, Paragraph},
};
use std::path::{Path, PathBuf};

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
    options: Vec<PickerOption>,
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
    /// A favourite the user asked to add or remove, awaiting the app.
    ///
    /// The picker cannot persist anything itself — the catalog and its file live
    /// on the app — so an edit is recorded here and drained on the next key
    /// dispatch, in the same spirit as the loading screens' pending results.
    pending_edit: Option<FavoriteEdit>,
}

/// A requested change to the saved directory list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FavoriteEdit {
    /// Ask the user which directory to save. `a` is "add a favourite", not
    /// "bookmark whatever the cursor happens to be on" — the point is to save a
    /// place you know about, which is usually not one already on the list.
    PromptAdd,
    /// Forget this path.
    Remove(PathBuf),
}

/// One row of the picker's shortcut list.
#[derive(Debug, Clone)]
pub struct PickerOption {
    pub path: PathBuf,
    pub label: String,
    /// Whether this row came from the saved favourites rather than the built-in
    /// locations. Only a favourite can be removed, and only a non-favourite can
    /// be added.
    pub is_favorite: bool,
    /// Times visited, for the trailing count. Zero for an unvisited built-in.
    pub uses: u64,
    /// RFC 3339 last visit, or `None`. Drives the ordering.
    pub last_used: Option<String>,
}

impl Default for DirPickerState {
    fn default() -> Self {
        Self::new()
    }
}

impl DirPickerState {
    pub fn new() -> Self {
        Self::with_favorites(&[])
    }

    /// Build the picker over `favorites` plus the built-in locations.
    ///
    /// The two are merged into one list ordered by recency, so a directory you
    /// actually use rises to the top whether or not it is one of the built-ins.
    /// A favourite that duplicates a built-in path replaces it rather than
    /// appearing twice.
    pub fn with_favorites(favorites: &[crate::hosts::SavedDir]) -> Self {
        let home = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default();
        let cwd = std::env::current_dir().unwrap_or(PathBuf::from("."));

        let common: [(PathBuf, String); 10] = [
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

        let mut options: Vec<PickerOption> = Vec::new();

        // Saved directories first, so a favourite wins the de-duplication
        // against a built-in naming the same place — it carries the visit
        // history, which the built-in does not.
        for f in favorites {
            options.push(PickerOption {
                path: PathBuf::from(&f.path),
                label: f.display().to_string(),
                is_favorite: true,
                uses: f.uses,
                last_used: f.last_used.clone(),
            });
        }
        for (path, label) in common {
            // A missing directory is not worth offering; a saved favourite that
            // has gone away is still listed, so it can be removed.
            if !path.is_dir() || options.iter().any(|o| o.path == path) {
                continue;
            }
            options.push(PickerOption {
                path,
                label,
                is_favorite: false,
                uses: 0,
                last_used: None,
            });
        }

        // One merged list, most recently visited first. Built-ins have no
        // timestamp and so settle below anything actually used, in their
        // original order rather than alphabetically — "Home" before "/tmp"
        // reads better than the reverse.
        let order: std::collections::HashMap<&Path, usize> = options
            .iter()
            .enumerate()
            .map(|(i, o)| (o.path.as_path(), i))
            .collect();
        let mut indexed: Vec<(usize, PickerOption)> = options
            .iter()
            .map(|o| (order[o.path.as_path()], o.clone()))
            .collect();
        indexed.sort_by(|(ai, a), (bi, b)| {
            let (ak, bk) = (
                a.last_used.as_deref().unwrap_or(""),
                b.last_used.as_deref().unwrap_or(""),
            );
            bk.cmp(ak).then_with(|| ai.cmp(bi))
        });
        let options: Vec<PickerOption> = indexed.into_iter().map(|(_, o)| o).collect();

        Self {
            options,
            cursor: 0,
            input: String::new(),
            input_cursor: 0,
            input_is_suggestion: false,
            // The field starts focused: the picker exists to accept a path, and
            // the common directories are the shortcut, not the main event.
            focus: PickerFocus::Field,
            pending_edit: None,
        }
    }

    /// Take the pending favourite edit, if the user asked for one.
    pub fn take_favorite_edit(&mut self) -> Option<FavoriteEdit> {
        self.pending_edit.take()
    }

    /// The highlighted option, if any.
    pub fn selected(&self) -> Option<&PickerOption> {
        self.options.get(self.cursor)
    }

    /// Carry the keyboard state across a rebuild, so adding or removing a
    /// favourite does not dump the user back into the path field.
    pub fn adopt_focus_from(&mut self, other: &Self) {
        self.focus = other.focus;
        self.input.clone_from(&other.input);
        self.input_cursor = other.input_cursor;
        self.input_is_suggestion = other.input_is_suggestion;
    }

    /// Highlight the row for `path`, if it is still listed. Leaves the cursor
    /// alone otherwise, which is what happens to the row just removed.
    pub fn select_path(&mut self, path: &Path) {
        if let Some(i) = self.options.iter().position(|o| o.path == path) {
            self.cursor = i;
        } else {
            self.cursor = self.cursor.min(self.options.len().saturating_sub(1));
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

    /// The shortcut list, in display order. Test hook.
    pub fn options_for_test(&self) -> &[PickerOption] {
        &self.options
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
        if let Some(opt) = self.options.get(self.cursor) {
            let shown = opt.path.to_string_lossy().to_string();
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
        if let Some(opt) = self.options.get(self.cursor) {
            return Some(opt.path.clone());
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
                // Save / forget, matching the dialing directory's a and d.
                // Only bound while the list has focus, so typing a path that
                // contains either letter is unaffected.
                KeyCode::Char('a') => {
                    self.pending_edit = Some(FavoriteEdit::PromptAdd);
                    Some(true)
                }
                KeyCode::Char('d') => {
                    if let Some(opt) = self.selected() {
                        if opt.is_favorite {
                            self.pending_edit = Some(FavoriteEdit::Remove(opt.path.clone()));
                        }
                    }
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
            .map(|(i, opt)| {
                // A star marks a saved favourite, so it is obvious which rows
                // `d` can remove and which `a` would add.
                let mark = if opt.is_favorite { "★ " } else { "  " };
                let count = if opt.uses > 0 {
                    format!("  ({})", opt.uses)
                } else {
                    String::new()
                };
                let text = format!("{}{}{}", mark, opt.label, count);
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
                    Line::from(Span::styled(format!("> {}", text), style))
                } else if opt.is_favorite {
                    Line::from(Span::styled(
                        format!("  {}", text),
                        Style::default().fg(Color::Green),
                    ))
                } else {
                    Line::from(format!("  {}", text))
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
                    " Directories (↑/↓, or Tab for j/k) "
                } else {
                    " Directories (j/k move · a save · d forget) "
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
