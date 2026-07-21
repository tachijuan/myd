use ratatui::{
    Frame,
    layout::{Constraint, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span, Text},
    widgets::{
        Block, BorderType, Borders, Paragraph, Scrollbar, ScrollbarOrientation, ScrollbarState,
    },
};
use std::path::PathBuf;

use super::{ScreenState, SortMode};
use crate::widget::file_info;
use crate::widget::file_tree::FileTree;

pub struct MainScreenState {
    root_path: PathBuf,
    pub tree: FileTree,
    pub info_panel_hidden: bool,
    /// Cached info panel text — only recomputed when selection changes.
    cached_info_text: Text<'static>,
    /// Last selected name (to detect when selection changes, cheaper than PathBuf comparison).
    last_selected_name: String,
}

impl MainScreenState {
    pub fn new(root_path: PathBuf) -> Self {
        let tree = FileTree::new(root_path.clone(), SortMode::Largest, true, true);
        Self {
            root_path,
            tree,
            info_panel_hidden: false,
            cached_info_text: Text::default(),
            last_selected_name: String::new(),
        }
    }

    /// Create from a pre-built tree (used after loading completes).
    pub fn from_tree(root_path: PathBuf, tree: FileTree) -> Self {
        Self {
            root_path,
            tree,
            info_panel_hidden: false,
            cached_info_text: Text::default(),
            last_selected_name: String::new(),
        }
    }

    pub fn cursor_down(&mut self) -> bool {
        self.tree.cursor_down();
        true
    }

    pub fn cursor_up(&mut self) -> bool {
        self.tree.cursor_up();
        true
    }

    pub fn to_top(&mut self) -> bool {
        self.tree.to_top();
        true
    }

    pub fn to_bottom(&mut self) -> bool {
        self.tree.to_bottom();
        true
    }

    pub fn page_down(&mut self) -> bool {
        let page = self.tree.lines.len().min(20);
        self.tree.cursor = (self.tree.cursor + page).min(self.tree.lines.len().saturating_sub(1));
        true
    }

    pub fn page_up(&mut self) -> bool {
        self.tree.cursor = self.tree.cursor.saturating_sub(20);
        true
    }

    pub fn expand(&mut self) -> bool {
        self.tree.expand_cursor();
        true
    }

    pub fn collapse(&mut self) -> bool {
        self.tree.collapse_cursor();
        true
    }

    /// Navigate: expand in place if already expanded, collapse if expanded,
    /// or expand if not. Screen push for subdirectory navigation is handled
    /// by the app layer.
    pub fn navigate(&mut self) {
        self.tree.navigate();
    }

    pub fn go_parent(&mut self) -> bool {
        self.tree.go_parent();
        true
    }

    pub fn toggle_sort(&mut self) -> bool {
        let modes = [
            SortMode::Largest,
            SortMode::Smallest,
            SortMode::DirsFirst,
            SortMode::FilesFirst,
        ];
        let current_idx = modes.iter().position(|m| *m == self.tree.sort_mode).unwrap_or(0);
        let next_idx = (current_idx + 1) % modes.len();
        self.tree.set_sort_mode(modes[next_idx]);
        true
    }

    pub fn toggle_hidden(&mut self) -> bool {
        self.tree.toggle_hidden();
        true
    }

    pub fn toggle_bar(&mut self) -> bool {
        self.tree.show_size_bar = !self.tree.show_size_bar;
        true
    }

    pub fn collapse_all(&mut self) -> bool {
        self.tree.collapse_all();
        true
    }

    pub fn expand_all(&mut self) -> bool {
        self.tree.expand_all();
        true
    }

    pub fn refresh(&mut self) -> bool {
        self.tree = FileTree::new(
            self.root_path.clone(),
            self.tree.sort_mode,
            self.tree.show_hidden,
            self.tree.show_size_bar,
        );
        true
    }

    /// Search through tree lines and move cursor to first match.
    pub fn search(&mut self, pattern: &str) -> bool {
        let pattern = pattern.trim().to_lowercase();
        if pattern.is_empty() {
            return true;
        }
        for (i, line) in self.tree.lines.iter().enumerate() {
            let name = line
                .path
                .file_name()
                .map(|s| s.to_string_lossy().to_lowercase())
                .unwrap_or_default();
            if name.contains(&pattern) {
                self.tree.cursor = i;
                return true;
            }
        }
        true
    }

    /// Get the currently selected path.
    pub fn selected_path(&self) -> Option<&PathBuf> {
        self.tree.selected_line().map(|l| &l.path)
    }

    /// Get the currently selected resolved (canonicalized) path.
    pub fn selected_resolved_path(&self) -> Option<&PathBuf> {
        self.tree.selected_line().map(|l| &l.resolved_path)
    }

    /// Get the depth of the currently selected line.
    pub fn selected_line_depth(&self) -> Option<usize> {
        self.tree.selected_line().map(|l| l.depth)
    }

    /// Remove a path from the tree in-place (preserves expanded state).
    pub fn remove_path(&mut self, path: &std::path::Path) {
        self.tree.remove_path(path);
    }
}

impl ScreenState for MainScreenState {
    fn render(&mut self, frame: &mut Frame, area: Rect) {
        // Split area into content and footer.
        let chunks =
            Layout::vertical([Constraint::Min(1), Constraint::Length(1)]).split(area);
        let content_area = chunks[0];
        let footer_area = chunks[1];

        if self.info_panel_hidden {
            self.render_tree(frame, content_area);
        } else {
            let inner =
                Layout::horizontal([Constraint::Percentage(70), Constraint::Percentage(30)])
                    .split(content_area);
            self.render_tree(frame, inner[0]);
            self.render_info(frame, inner[1]);
        }

        self.render_footer(frame, footer_area);
    }
}

impl MainScreenState {
    fn render_tree(&mut self, frame: &mut Frame, area: Rect) {
        let text = self.tree.render_text();

        // Build status bar subtitle — uses cached counts, no iteration.
        let total = self.tree.lines.len().saturating_sub(1); // subtract root
        let dirs = self.tree.dir_count();
        let files = self.tree.file_count();
        let title = format!(
            " File Tree ({}) | {} items | {} dirs | {} files | Sort: {} ",
            self.root_path.display(),
            total,
            dirs,
            files,
            self.tree.sort_mode.label()
        );

        // Calculate scroll offset so the cursor line stays visible.
        // The block has borders (top/bottom) and a title bar, reducing usable height.
        let visible_lines = (area.height - 3).max(1) as usize;
        let scroll = if self.tree.cursor >= visible_lines {
            self.tree.cursor - visible_lines + 1
        } else {
            0
        };

        let paragraph = Paragraph::new(text)
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_type(BorderType::Plain)
                    .border_style(Style::default().fg(Color::Cyan))
                    .title(title),
            )
            .scroll((scroll as u16, 0));

        frame.render_widget(paragraph, area);

        if !self.tree.lines.is_empty() {
            let mut scrollbar_state = ScrollbarState::default().position(self.tree.cursor);
            frame.render_stateful_widget(
                Scrollbar::new(ScrollbarOrientation::VerticalRight),
                area,
                &mut scrollbar_state,
            );
        }
    }

    fn render_info(&mut self, frame: &mut Frame, area: Rect) {
        let current_name = self
            .tree
            .selected_line()
            .map(|l| l.name.clone());

        // Only recompute info text when selection changes.
        if current_name.as_ref().map(|s| s.as_str()) != Some(self.last_selected_name.as_str()) {
            self.last_selected_name = current_name.clone().unwrap_or_default();

            if let Some(line) = self.tree.selected_line() {
                self.cached_info_text = file_info::render_info_owned(&line.path, &self.tree.size_cache);
            } else {
                self.cached_info_text = Text::from(Line::raw("No selection"));
            }
        }

        let paragraph = Paragraph::new(self.cached_info_text.clone()).block(
            Block::default()
                .borders(Borders::ALL)
                .border_type(BorderType::Plain)
                .border_style(Style::default().fg(Color::Cyan))
                .title(" Info "),
        );
        frame.render_widget(paragraph, area);
    }

    fn render_footer(&self, frame: &mut Frame, area: Rect) {
        let footer = format!(
            " j/k:navigate  l:expand  h:collapse/back  Enter:enter  s:sort  ?/F1:help  q:quit/back "
        );
        let line = Line::from(Span::styled(
            footer,
            Style::default()
                .fg(Color::Yellow)
                .bg(Color::Rgb(20, 20, 30))
                .add_modifier(Modifier::BOLD),
        ));
        let paragraph = Paragraph::new(line);
        frame.render_widget(paragraph, area);
    }
}
