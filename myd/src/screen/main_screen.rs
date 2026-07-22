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
use crate::widget::treemap::{FocusTarget, TreeMap};

pub struct MainScreenState {
    root_path: PathBuf,
    pub tree: FileTree,
    /// Treemap built from the current tree data (rebuilt when tree changes).
    treemap: TreeMap,
    /// Which view has focus: file tree or treemap.
    pub focus: FocusTarget,
    pub info_panel_hidden: bool,
    /// Cached info panel text — only recomputed when selection changes.
    cached_info_text: Text<'static>,
    /// Last selected name (to detect when selection changes, cheaper than PathBuf comparison).
    last_selected_name: String,
}

impl MainScreenState {
    pub fn new(root_path: PathBuf) -> Self {
        let tree = FileTree::new(root_path.clone(), SortMode::Largest, true, true);
        let treemap = TreeMap::from_file_tree(&tree);
        Self {
            root_path,
            tree,
            treemap,
            focus: FocusTarget::Tree,
            info_panel_hidden: false,
            cached_info_text: Text::default(),
            last_selected_name: String::new(),
        }
    }

    /// Create from a pre-built tree (used after loading completes).
    pub fn from_tree(root_path: PathBuf, tree: FileTree) -> Self {
        let treemap = TreeMap::from_file_tree(&tree);
        Self {
            root_path,
            tree,
            treemap,
            focus: FocusTarget::Tree,
            info_panel_hidden: false,
            cached_info_text: Text::default(),
            last_selected_name: String::new(),
        }
    }

    /// Rebuild the treemap from the current tree (call after tree structure changes).
    fn rebuild_treemap(&mut self) {
        self.treemap = TreeMap::from_file_tree(&self.tree);
    }

    pub fn cursor_down(&mut self) -> bool {
        match self.focus {
            FocusTarget::Tree => self.tree.cursor_down(),
            FocusTarget::Treemap => self.treemap.cursor_down(),
        }
        true
    }

    pub fn cursor_up(&mut self) -> bool {
        match self.focus {
            FocusTarget::Tree => self.tree.cursor_up(),
            FocusTarget::Treemap => self.treemap.cursor_up(),
        }
        true
    }

    pub fn to_top(&mut self) -> bool {
        match self.focus {
            FocusTarget::Tree => self.tree.to_top(),
            FocusTarget::Treemap => self.treemap.cursor = 0,
        }
        true
    }

    pub fn to_bottom(&mut self) -> bool {
        match self.focus {
            FocusTarget::Tree => self.tree.to_bottom(),
            FocusTarget::Treemap => {
                self.treemap.cursor = self.treemap.cells.len().saturating_sub(1)
            }
        }
        true
    }

    pub fn page_down(&mut self) -> bool {
        match self.focus {
            FocusTarget::Tree => {
                let page = self.tree.lines.len().min(20);
                self.tree.cursor = (self.tree.cursor + page).min(self.tree.lines.len().saturating_sub(1));
            }
            FocusTarget::Treemap => {
                self.treemap.cursor = (self.treemap.cursor + 5).min(self.treemap.cells.len().saturating_sub(1));
            }
        }
        true
    }

    pub fn page_up(&mut self) -> bool {
        match self.focus {
            FocusTarget::Tree => {
                self.tree.cursor = self.tree.cursor.saturating_sub(20);
            }
            FocusTarget::Treemap => {
                self.treemap.cursor = self.treemap.cursor.saturating_sub(5);
            }
        }
        true
    }

    pub fn expand(&mut self) -> bool {
        match self.focus {
            FocusTarget::Tree => self.tree.expand_cursor(),
            FocusTarget::Treemap => self.treemap.cursor_right(),
        }
        true
    }

    pub fn collapse(&mut self) -> bool {
        match self.focus {
            FocusTarget::Tree => self.tree.collapse_cursor(),
            FocusTarget::Treemap => self.treemap.cursor_left(),
        }
        true
    }

    /// Navigate: expand in place if already expanded, collapse if expanded,
    /// or expand if not. Screen push for subdirectory navigation is handled
    /// by the app layer.
    pub fn navigate(&mut self) {
        self.tree.navigate();
    }

    pub fn go_parent(&mut self) -> bool {
        match self.focus {
            FocusTarget::Tree => self.tree.go_parent(),
            FocusTarget::Treemap => self.treemap.cursor_left(),
        }
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
        self.rebuild_treemap();
        true
    }

    pub fn toggle_hidden(&mut self) -> bool {
        self.tree.toggle_hidden();
        self.rebuild_treemap();
        true
    }

    pub fn toggle_bar(&mut self) -> bool {
        self.tree.show_size_bar = !self.tree.show_size_bar;
        true
    }

    pub fn collapse_all(&mut self) -> bool {
        self.tree.collapse_all();
        self.rebuild_treemap();
        true
    }

    pub fn expand_all(&mut self) -> bool {
        self.tree.expand_all();
        self.rebuild_treemap();
        true
    }

    pub fn refresh(&mut self) -> bool {
        self.tree = FileTree::new(
            self.root_path.clone(),
            self.tree.sort_mode,
            self.tree.show_hidden,
            self.tree.show_size_bar,
        );
        self.rebuild_treemap();
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

    /// Get the currently selected path from whichever view is focused.
    pub fn selected_path(&self) -> Option<&PathBuf> {
        match self.focus {
            FocusTarget::Tree => self.tree.selected_line().map(|l| &l.path),
            FocusTarget::Treemap => self.treemap.selected_cell().map(|c| &c.path),
        }
    }

    /// Get the currently selected resolved (canonicalized) path.
    pub fn selected_resolved_path(&self) -> Option<&PathBuf> {
        match self.focus {
            FocusTarget::Tree => self.tree.selected_line().map(|l| &l.resolved_path),
            FocusTarget::Treemap => self.treemap.selected_cell().map(|c| &c.resolved_path),
        }
    }

    /// Get the depth of the currently selected line (tree) or cell (treemap).
    pub fn selected_line_depth(&self) -> Option<usize> {
        match self.focus {
            FocusTarget::Tree => self.tree.selected_line().map(|l| l.depth),
            FocusTarget::Treemap => self.treemap.selected_cell().map(|c| c.depth),
        }
    }

    /// Remove a path from the tree in-place (preserves expanded state).
    pub fn remove_path(&mut self, path: &std::path::Path) {
        self.tree.remove_path(path);
        self.rebuild_treemap();
    }
}

impl ScreenState for MainScreenState {
    fn render(&mut self, frame: &mut Frame, area: Rect) {
        // Split area into content and footer.
        let chunks =
            Layout::vertical([Constraint::Min(1), Constraint::Length(1)]).split(area);
        let content_area = chunks[0];
        let footer_area = chunks[1];

        match self.focus {
            FocusTarget::Tree => {
                if self.info_panel_hidden {
                    self.render_tree(frame, content_area);
                } else {
                    let inner =
                        Layout::horizontal([Constraint::Percentage(70), Constraint::Percentage(30)])
                            .split(content_area);
                    self.render_tree(frame, inner[0]);
                    self.render_info(frame, inner[1]);
                }
            }
            FocusTarget::Treemap => {
                if self.info_panel_hidden {
                    self.render_treemap(frame, content_area);
                } else {
                    let inner =
                        Layout::horizontal([Constraint::Percentage(70), Constraint::Percentage(30)])
                            .split(content_area);
                    self.render_treemap(frame, inner[0]);
                    self.render_info(frame, inner[1]);
                }
            }
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

    fn render_treemap(&mut self, frame: &mut Frame, area: Rect) {
        // Get the tree's selected path to highlight in the treemap.
        let highlighted_path = self.tree.selected_line().map(|l| &l.path as &std::path::Path);

        self.treemap.render(frame, area, self.treemap.cursor, highlighted_path);
    }

    fn render_info(&mut self, frame: &mut Frame, area: Rect) {
        // Use whichever view is focused to determine what to show info for.
        let (current_name, path) = match self.focus {
            FocusTarget::Tree => {
                self.tree.selected_line()
                    .map(|l| (l.name.clone(), l.path.clone()))
                    .unwrap_or((String::new(), PathBuf::from(".")))
            }
            FocusTarget::Treemap => {
                self.treemap.selected_cell()
                    .map(|c| (c.label.clone(), c.path.clone()))
                    .unwrap_or((String::new(), PathBuf::from(".")))
            }
        };

        // Only recompute info text when selection changes.
        if current_name != self.last_selected_name {
            self.last_selected_name = current_name.clone();

            self.cached_info_text = file_info::render_info_owned(&path, &self.tree.size_cache);
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
        let view_label = match self.focus {
            FocusTarget::Tree => "TREE",
            FocusTarget::Treemap => "TREEMAP",
        };
        let footer = format!(
            " [{}]  j/k:navigate  l:expand  h:collapse/back  Enter:enter  v:toggle view  t:toggle info  ?/F1:help  q:quit ",
            view_label
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
