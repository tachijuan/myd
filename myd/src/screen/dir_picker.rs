use crossterm::event::{KeyCode, KeyEvent};
use ratatui::{
    Frame,
    layout::{Constraint, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span, Text},
    widgets::{Block, BorderType, Borders, Paragraph},
};
use std::path::PathBuf;

/// State for the directory picker startup screen.
pub struct DirPickerState {
    options: Vec<(PathBuf, String)>,
    cursor: usize,
    /// Current input value (typed path).
    input: String,
    /// Input cursor position.
    input_cursor: usize,
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
        }
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

    /// Handle typed character in the input field.
    pub fn input_char(&mut self, c: char) {
        if self.input_cursor < self.input.len() {
            self.input.insert(self.input_cursor, c);
        } else {
            self.input.push(c);
        }
        self.input_cursor += 1;
    }

    pub fn input_backspace(&mut self) {
        if self.input_cursor > 0 {
            self.input_cursor -= 1;
            self.input.remove(self.input_cursor);
        }
    }

    pub fn input_left(&mut self) {
        if self.input_cursor > 0 {
            self.input_cursor -= 1;
        }
    }

    pub fn input_right(&mut self) {
        if self.input_cursor < self.input.len() {
            self.input_cursor += 1;
        }
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
                // Delete char at cursor.
                if self.input_cursor < self.input.len() {
                    self.input.remove(self.input_cursor);
                }
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
                self.input_cursor = 0;
                Some(true)
            }
            KeyCode::End => {
                self.input_cursor = self.input.len();
                Some(true)
            }
            _ => None,
        }
    }
}

impl super::ScreenState for DirPickerState {
    fn cursor_down(&mut self) -> bool {
        if !self.options.is_empty() {
            self.cursor = (self.cursor + 1) % self.options.len();
            if let Some((path, _)) = self.options.get(self.cursor) {
                self.input = path.to_string_lossy().to_string();
            }
        }
        true
    }

    fn cursor_up(&mut self) -> bool {
        if !self.options.is_empty() {
            self.cursor = if self.cursor == 0 {
                self.options.len() - 1
            } else {
                self.cursor - 1
            };
            if let Some((path, _)) = self.options.get(self.cursor) {
                self.input = path.to_string_lossy().to_string();
            }
        }
        true
    }

    fn render(&mut self, frame: &mut Frame, area: Rect) {
        let vertical = Layout::vertical([Constraint::Length(3), Constraint::Length(3), Constraint::Min(1)]).split(area);

        // Title.
        let title = Paragraph::new(Span::styled(
            "Select a directory (Esc to quit)",
            Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD),
        ));
        frame.render_widget(title, vertical[0]);

        // Input field. A block cursor is drawn at the input position — an empty
        // field shows the cursor over a dimmed placeholder so it's clearly the
        // focused, editable box.
        let input_line = if self.input.is_empty() {
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
        let input_para = Paragraph::new(input_line).block(
            Block::default()
                .borders(Borders::ALL)
                .border_type(BorderType::Plain)
                .border_style(Style::default().fg(Color::Green))
                .title(" Path (Enter to go) "),
        );
        frame.render_widget(input_para, vertical[1]);

        // Option list.
        let lines: Text = self
            .options
            .iter()
            .enumerate()
            .map(|(i, (_path, label))| {
                if i == self.cursor {
                    Line::from(Span::styled(
                        format!("> {}", label),
                        Style::default().fg(Color::Yellow).add_modifier(Modifier::REVERSED),
                    ))
                } else {
                    Line::from(format!("  {}", label))
                }
            })
            .collect();

        let list = Paragraph::new(lines).block(
            Block::default()
                .borders(Borders::ALL)
                .border_type(BorderType::Plain)
                .border_style(Style::default().fg(Color::Cyan))
                .title(" Common Directories (j/k to navigate) "),
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
