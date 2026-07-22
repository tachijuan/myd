use ratatui::{
    Frame,
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph},
};
use std::path::{Path, PathBuf};

use crate::widget::file_tree::FileTree;

/// A single cell in the treemap, representing a directory or file.
#[derive(Debug, Clone)]
pub struct TreemapCell {
    pub rect: Rect,
    pub path: PathBuf,
    pub resolved_path: PathBuf,
    pub size: u64,
    pub label: String,
    pub depth: usize,
}

/// Focus target for the main screen (tree or treemap).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum FocusTarget {
    #[default]
    Tree,
    Treemap,
}

/// The treemap widget — computes a squarified layout from file tree data.
#[derive(Debug, Clone)]
pub struct TreeMap {
    pub cells: Vec<TreemapCell>,
    pub cursor: usize,
    pub total_size: u64,
}

impl TreeMap {
    /// Build a treemap from the current file tree.
    ///
    /// Directories are shown as tiles. If a directory has no subdirectories
    /// but has files, the files become the leaf tiles instead.
    pub fn from_file_tree(tree: &FileTree) -> Self {
        let mut items = Vec::new();

        // Collect top-level children (skip root at depth 0)
        collect_items(&tree.root, tree, &mut items, 0);

        let total_size = items.iter().map(|(_, s)| s).sum();

        // Build cells with placeholder rects — squarified on render.
        let mut cells = items
            .into_iter()
            .map(|(info, size)| {
                let label = truncate_path(&info.path, info.depth);
                TreemapCell {
                    rect: Rect::default(),
                    path: info.path,
                    resolved_path: info.resolved_path,
                    size,
                    label,
                    depth: info.depth,
                }
            })
            .collect::<Vec<_>>();

        // Sort by size descending for squarification.
        cells.sort_by_key(|c| std::cmp::Reverse(c.size));

        // Ensure cursor is in bounds.
        let cursor = if cells.is_empty() {
            0
        } else {
            // Try to match the tree's current selection.
            if let Some(selected) = tree.selected_line() {
                cells.iter()
                    .position(|c| c.resolved_path == selected.resolved_path)
                    .unwrap_or(0)
            } else {
                0
            }
        };

        Self {
            cells,
            cursor,
            total_size,
        }
    }

    /// Compute the squarified layout for the given screen area.
    /// Call this before rendering when the area changes.
    pub fn compute_layout(&mut self, area: Rect) {
        if self.cells.is_empty() {
            return;
        }

        let items: Vec<(String, u64)> = self
            .cells
            .iter()
            .map(|c| (c.label.clone(), c.size))
            .collect();

        let rects = squarify(&items, area);

        for (i, rect) in rects.iter().enumerate() {
            if i < self.cells.len() {
                self.cells[i].rect = *rect;
            }
        }
    }

    /// Navigate to the cell whose center is most directly in the given direction.
    fn navigate_direction(&self, current: usize, direction: NavDirection) -> Option<usize> {
        if self.cells.is_empty() || current >= self.cells.len() {
            return None;
        }

        let current_cell = &self.cells[current];
        let center = current_cell.center();

        let mut best: Option<(usize, f64)> = None; // (index, score)

        for (i, cell) in self.cells.iter().enumerate() {
            if i == current {
                continue;
            }

            let other_center = cell.center();
            let score = match direction {
                NavDirection::Down => {
                    if other_center.1 <= center.1 {
                        continue;
                    }
                    // Prefer cells whose center is below and horizontally overlapping.
                    let horiz_overlap = horizontal_overlap(current_cell, cell);
                    let vert_dist = (other_center.1 - center.1) as f64;
                    (1.0 - horiz_overlap) * 100.0 + vert_dist
                }
                NavDirection::Up => {
                    if other_center.1 >= center.1 {
                        continue;
                    }
                    let horiz_overlap = horizontal_overlap(current_cell, cell);
                    let vert_dist = (center.1 - other_center.1) as f64;
                    (1.0 - horiz_overlap) * 100.0 + vert_dist
                }
                NavDirection::Left => {
                    if other_center.0 >= center.0 {
                        continue;
                    }
                    let vert_overlap = vertical_overlap(current_cell, cell);
                    let horiz_dist = (center.0 - other_center.0) as f64;
                    (1.0 - vert_overlap) * 100.0 + horiz_dist
                }
                NavDirection::Right => {
                    if other_center.0 <= center.0 {
                        continue;
                    }
                    let vert_overlap = vertical_overlap(current_cell, cell);
                    let horiz_dist = (other_center.0 - center.0) as f64;
                    (1.0 - vert_overlap) * 100.0 + horiz_dist
                }
            };

            match best {
                None => best = Some((i, score)),
                Some((_, best_score)) => {
                    if score < best_score {
                        best = Some((i, score));
                    }
                }
            }
        }

        best.map(|(i, _)| i)
    }

    /// Move cursor down (j).
    pub fn cursor_down(&mut self) {
        if let Some(next) = self.navigate_direction(self.cursor, NavDirection::Down) {
            self.cursor = next;
        }
    }

    /// Move cursor up (k).
    pub fn cursor_up(&mut self) {
        if let Some(next) = self.navigate_direction(self.cursor, NavDirection::Up) {
            self.cursor = next;
        }
    }

    /// Move cursor left (h).
    pub fn cursor_left(&mut self) {
        if let Some(next) = self.navigate_direction(self.cursor, NavDirection::Left) {
            self.cursor = next;
        }
    }

    /// Move cursor right (l).
    pub fn cursor_right(&mut self) {
        if let Some(next) = self.navigate_direction(self.cursor, NavDirection::Right) {
            self.cursor = next;
        }
    }

    /// Get the currently selected cell.
    pub fn selected_cell(&self) -> Option<&TreemapCell> {
        self.cells.get(self.cursor)
    }

    /// Render the treemap on the given area.
    ///
    /// - `selected`: the cursor index (reversed colors)
    /// - `highlighted_path`: path from the tree view's selection (brighter color)
    pub fn render(
        &self,
        frame: &mut Frame,
        area: Rect,
        selected: usize,
        highlighted_path: Option<&Path>,
    ) {
        // First compute layout for this area.
        if self.cells.is_empty() {
            // Render placeholder.
            let placeholder = Paragraph::new(Line::from(Span::styled(
                " No data for treemap ",
                Style::default().fg(Color::DarkGray),
            )));
            frame.render_widget(placeholder, area);
            return;
        }

        // Recompute layout for current area.
        let mut layout_copy = self.clone();
        layout_copy.compute_layout(area);

        for (i, cell) in layout_copy.cells.iter().enumerate() {
            let rect = cell.rect;
            if rect.width < 2 || rect.height < 1 {
                continue;
            }

            let is_selected = i == selected;
            let is_highlighted = highlighted_path
                .map(|p| cell.resolved_path == *p)
                .unwrap_or(false);

            if is_selected {
                // Selected: reversed colors with bold.
                let style = Style::default().add_modifier(Modifier::REVERSED | Modifier::BOLD);
                let block = Block::default()
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(Color::White));

                frame.render_widget(Clear, rect);
                frame.render_widget(
                    Paragraph::new("").style(style),
                    rect,
                );
                frame.render_widget(block, rect);

                // Draw label if it fits.
                let inner = Rect::new(
                    rect.x + 1,
                    rect.y + 1,
                    rect.width.saturating_sub(2),
                    rect.height.saturating_sub(2),
                );
                if inner.width >= 3 && inner.height >= 1 {
                    let label = Span::styled(
                        truncate_for_width(&cell.label, inner.width as usize),
                        Style::default().add_modifier(Modifier::REVERSED | Modifier::BOLD),
                    );
                    let line = Line::from(label);
                    let para = Paragraph::new(line);
                    frame.render_widget(para, inner);
                }
            } else if is_highlighted {
                // Highlighted (tree selection): bright border.
                let color = depth_color(cell.depth, true);
                let style = Style::default().fg(color).add_modifier(Modifier::BOLD);
                let block = Block::default()
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(Color::Yellow));

                frame.render_widget(Clear, rect);
                frame.render_widget(Paragraph::new("").style(style), rect);
                frame.render_widget(block, rect);

                // Draw label.
                let inner = Rect::new(
                    rect.x + 1,
                    rect.y + 1,
                    rect.width.saturating_sub(2),
                    rect.height.saturating_sub(2),
                );
                if inner.width >= 3 && inner.height >= 1 {
                    let label = Span::styled(
                        truncate_for_width(&cell.label, inner.width as usize),
                        Style::default().fg(color).add_modifier(Modifier::BOLD),
                    );
                    let para = Paragraph::new(Line::from(label));
                    frame.render_widget(para, inner);
                }
            } else {
                // Normal cell.
                let color = depth_color(cell.depth, false);
                let style = Style::default().fg(color);
                let block = Block::default()
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(Color::Rgb(60, 60, 80)));

                frame.render_widget(Clear, rect);
                frame.render_widget(Paragraph::new("").style(style), rect);
                frame.render_widget(block, rect);

                // Draw label if box is big enough.
                let inner = Rect::new(
                    rect.x + 1,
                    rect.y + 1,
                    rect.width.saturating_sub(2),
                    rect.height.saturating_sub(2),
                );
                if inner.width >= 5 && inner.height >= 1 {
                    let label = Span::styled(
                        truncate_for_width(&cell.label, inner.width as usize),
                        Style::default().fg(color),
                    );
                    let para = Paragraph::new(Line::from(label));
                    frame.render_widget(para, inner);
                }
            }
        }
    }
}

/// Navigation direction in the treemap.
#[derive(Debug, Clone, Copy)]
enum NavDirection {
    Up,
    Down,
    Left,
    Right,
}

/// Helper struct for collecting items from the tree.
#[derive(Debug, Clone)]
struct TreeItemInfo {
    path: PathBuf,
    resolved_path: PathBuf,
    depth: usize,
}

/// Recursively collect directories (and file leaves) from the tree.
fn collect_items(
    node: &crate::widget::file_tree::TreeNode,
    tree: &FileTree,
    items: &mut Vec<(TreeItemInfo, u64)>,
    depth: usize,
) {
    // Only process expanded nodes (and the root).
    if depth > 0 && !node.is_expanded {
        return;
    }

    if let Some(ref children) = node.children {
        let dir_children: Vec<_> = children.iter().filter(|c| c.is_dir).collect();
        let file_children: Vec<_> = children.iter().filter(|c| !c.is_dir).collect();

        if !dir_children.is_empty() {
            // Show subdirectories.
            for child in dir_children {
                let size = tree
                    .size_cache
                    .get(&child.resolved_path)
                    .unwrap_or_else(|| {
                        if child.is_dir {
                            crate::utils::sizes::get_dir_size(&child.path)
                        } else {
                            crate::utils::sizes::get_file_size(&child.path)
                        }
                    });
                items.push((
                    TreeItemInfo {
                        path: child.path.clone(),
                        resolved_path: child.resolved_path.clone(),
                        depth: depth + 1,
                    },
                    size,
                ));
                // Recurse into expanded directories.
                if child.is_expanded {
                    collect_items(child, tree, items, depth + 1);
                }
            }
        } else if !file_children.is_empty() {
            // No subdirs — show files as leaves.
            for child in file_children {
                let size = tree
                    .size_cache
                    .get(&child.resolved_path)
                    .unwrap_or_else(|| crate::utils::sizes::get_file_size(&child.path));
                items.push((
                    TreeItemInfo {
                        path: child.path.clone(),
                        resolved_path: child.resolved_path.clone(),
                        depth: depth + 1,
                    },
                    size,
                ));
            }
        }
    }
}

/// Get a color based on depth for visual distinction.
fn depth_color(depth: usize, bright: bool) -> Color {
    let colors_bright = [
        Color::Cyan,
        Color::Green,
        Color::Magenta,
        Color::Yellow,
        Color::Blue,
        Color::Red,
    ];
    let colors_dim = [
        Color::Rgb(30, 80, 120),   // dim cyan
        Color::Rgb(30, 100, 60),   // dim green
        Color::Rgb(100, 50, 100),  // dim magenta
        Color::Rgb(120, 100, 40),  // dim yellow
        Color::Rgb(40, 60, 120),   // dim blue
        Color::Rgb(120, 50, 50),   // dim red
    ];

    if bright {
        *colors_bright.get(depth % colors_bright.len()).unwrap_or(&Color::Gray)
    } else {
        *colors_dim.get(depth % colors_dim.len()).unwrap_or(&Color::DarkGray)
    }
}

/// Truncate a path for display: shorten middle components so the label fits.
/// Uses ~ for the home directory. Prefers showing the full filename.
/// e.g., "/home/juan/data/photos/vacation.jpg" -> "~/.../photos/vacation.jpg"
fn truncate_path(path: &Path, _depth: usize) -> String {
    // Check if path is under the user's home directory → use ~ shortcut.
    let home = std::env::var("HOME").ok().map(PathBuf::from);
    let (is_home, rel_path) = if let Some(ref h) = home {
        if let Ok(stripped) = path.strip_prefix(h) {
            (true, stripped)
        } else {
            (false, path)
        }
    } else {
        (false, path)
    };

    // Collect real components (skip "/" root).
    let components: Vec<&str> = rel_path
        .components()
        .filter_map(|c| c.as_os_str().to_str())
        .collect();

    if components.len() <= 3 {
        // Short path — show as-is (with ~ if applicable).
        let base = if is_home {
            format!("~/{}", rel_path.display())
        } else {
            path.display().to_string()
        };
        return base;
    }

    // Show the first component (or ~), the filename, and "..." in between.
    let first = components[0];
    let last = *components.last().unwrap_or(&"");
    let prefix = if is_home { "~" } else { "/" };

    // Show first dir + "..." + filename.
    format!("{}/.../{}/{}", prefix, first, last)
}

/// Truncate a label to fit `max_width` characters.
/// Prefers showing the filename (last path component).
/// Uses ~ for the home directory when truncating.
fn truncate_for_width(label: &str, max_width: usize) -> String {
    if label.len() <= max_width {
        return label.to_string();
    }

    // Check if label is a path with a "/" and try to preserve the filename.
    if let Some(slash_pos) = label.rfind('/') {
        let filename = &label[slash_pos + 1..];
        let dir_prefix = &label[..=slash_pos];

        // Check if the directory prefix starts with the user's home → use ~ shortcut.
        let use_home = std::env::var("HOME")
            .ok()
            .map_or(false, |h| dir_prefix.starts_with(&h) || dir_prefix.starts_with(&format!("{}/", h)));

        let shortened = if use_home {
            // Format: "~/..." + filename
            if filename.len() + 4 <= max_width {
                format!("~/...{}", filename)
            } else {
                // Too narrow for prefix — just truncate the filename itself.
                let avail = max_width.saturating_sub(3).max(1);
                let fn_trunc: String = filename.chars().take(avail).collect();
                format!("{}...", fn_trunc)
            }
        } else {
            // Show the beginning of the path + "..." + filename.
            let overhead = 3 + filename.len(); // "..." + filename
            let avail = max_width.saturating_sub(overhead).max(1);
            let start: String = dir_prefix.chars().take(avail).collect();
            format!("{}...{}", start, filename)
        };

        // Verify it actually fits (UTF-8 safety).
        if shortened.chars().count() <= max_width {
            return shortened;
        }

        // Fallback: show the tail (filename) only.
        let chars: Vec<char> = label.chars().collect();
        chars[chars.len().saturating_sub(max_width)..].iter().collect()
    } else {
        // Not a path — just show the tail.
        let chars: Vec<char> = label.chars().collect();
        chars[chars.len().saturating_sub(max_width)..].iter().collect()
    }
}

/// Compute horizontal overlap between two cells (0.0 to 1.0).
fn horizontal_overlap(a: &TreemapCell, b: &TreemapCell) -> f64 {
    let a_start = a.rect.x as f64;
    let a_end = (a.rect.x + a.rect.width) as f64;
    let b_start = b.rect.x as f64;
    let b_end = (b.rect.x + b.rect.width) as f64;

    let overlap = a_end.min(b_end) - a_start.max(b_start);
    let a_width = a.rect.width as f64;
    let b_width = b.rect.width as f64;

    if overlap <= 0.0 {
        return 0.0;
    }

    overlap / a_width.min(b_width)
}

/// Compute vertical overlap between two cells (0.0 to 1.0).
fn vertical_overlap(a: &TreemapCell, b: &TreemapCell) -> f64 {
    let a_start = a.rect.y as f64;
    let a_end = (a.rect.y + a.rect.height) as f64;
    let b_start = b.rect.y as f64;
    let b_end = (b.rect.y + b.rect.height) as f64;

    let overlap = a_end.min(b_end) - a_start.max(b_start);
    let a_height = a.rect.height as f64;
    let b_height = b.rect.height as f64;

    if overlap <= 0.0 {
        return 0.0;
    }

    overlap / a_height.min(b_height)
}

// ---------------------------------------------------------------------------
// Squarification algorithm (Jansen, Buja 2003)
// ---------------------------------------------------------------------------

impl TreemapCell {
    fn center(&self) -> (u16, u16) {
        (
            self.rect.x + self.rect.width / 2,
            self.rect.y + self.rect.height / 2,
        )
    }
}

/// Squarify a list of (label, value) pairs into rectangles within the given area.
/// Returns a list of Rects in the same order as the input items.
fn squarify(items: &[(String, u64)], area: Rect) -> Vec<Rect> {
    if items.is_empty() {
        return Vec::new();
    }

    // Filter out zero-size items but track their indices for assignment later.
    let total: u64 = items.iter().map(|(_, v)| v).sum();
    if total == 0 {
        // Give each item an equal slice.
        let n = items.len() as u16;
        let height = area.height.max(1) / n.min(area.height);
        items
            .iter()
            .enumerate()
            .map(|(i, _)| {
                let y = area.y + (i as u16).min(area.height) * height;
                let h = height.min(area.height.saturating_sub(y - area.y));
                Rect::new(area.x, y, area.width, h.max(1))
            })
            .collect()
    } else {
        let result = squarify_inner(items, area, total);
        // Ensure we return exactly items.len() rects.
        if result.len() == items.len() {
            result
        } else {
            // Fallback: equal division.
            items.iter().map(|_| area).collect()
        }
    }
}

fn squarify_inner(items: &[(String, u64)], area: Rect, _total: u64) -> Vec<Rect> {
    let mut result = Vec::new();

    // Normalize values to work with the area.
    let total_value: f64 = items.iter().map(|(_, v)| *v as f64).sum();
    if total_value == 0.0 {
        return items.iter().map(|_| area).collect();
    }

    // Scale factor: total area / total value.
    let scale = (area.width as f64 * area.height as f64) / total_value;

    // Squarify using normalized cumulative areas.
    let normalized: Vec<f64> = items
        .iter()
        .map(|(_, v)| (*v as f64 * scale).max(1.0))
        .collect();

    // Use cumulative approach.
    squarify_row(
        &normalized,
        0,
        items.len() - 1,
        area,
        false, // start with horizontal row.
        &mut result,
    );

    result
}

fn squarify_row(
    areas: &[f64],
    start: usize,
    end: usize,
    area: Rect,
    row_horizontal: bool, // true = row grows vertically (stack rows down)
    result: &mut Vec<Rect>,
) {
    if start > end {
        return;
    }

    if start == end {
        result.push(area);
        return;
    }

    if area.width == 0 || area.height == 0 {
        for _ in start..=end {
            result.push(Rect::default());
        }
        return;
    }

    // Build the row incrementally and find the best cut point.
    let mut row_areas: Vec<f64> = Vec::new();
    let mut row_sum: f64 = 0.0;

    // The "shortest side" of the current row.
    let row_length = if row_horizontal {
        area.width as f64
    } else {
        area.height as f64
    };

    let mut best_split = start;
    let mut best_worst_aspect = f64::MAX;

    for i in start..=end {
        let a = areas[i];
        row_areas.push(a);
        row_sum += a;

        // Compute the shortest side of the row.
        let row_short = row_sum / row_length;

        // Worst aspect ratio in the row.
        let worst = worst_aspect(&row_areas, row_short);

        if worst < best_worst_aspect {
            best_worst_aspect = worst;
            best_split = i;
        }
    }

    // Cut the row from start..=best_split, recurse on (best_split+1)..end.
    let row_count = best_split - start + 1;

    // Compute the row's short dimension.
    let row_short_u16 = if row_horizontal {
        ((row_sum / row_length) as u16).max(1).min(area.height)
    } else {
        ((row_sum / row_length) as u16).max(1).min(area.width)
    };

    // Assign rects to each item in the row.
    let remaining_len = if row_horizontal {
        area.height - row_short_u16
    } else {
        area.width - row_short_u16
    };

    for j in 0..row_count {
        let idx = start + j;
        let item_area = areas[idx];

        let (x, y, w, h) = if row_horizontal {
            // Row grows downward; items are placed side by side (columns).
            let width = (item_area / row_short_u16 as f64) as u16;
            let width = width.max(1).min(area.width);

            // Compute offset within the row.
            let offset: f64 = areas[start..idx].iter().copied().sum();
            let x_offset = (offset / row_short_u16 as f64) as u16;

            // Last item in row gets remaining width to avoid gaps.
            let w = if idx == best_split {
                area.width.saturating_sub(x_offset)
            } else {
                width
            };

            (area.x + x_offset, area.y, w.max(1), row_short_u16)
        } else {
            // Row grows rightward; items are stacked vertically (rows).
            let height = (item_area / row_short_u16 as f64) as u16;
            let height = height.max(1).min(area.height);

            let offset: f64 = areas[start..idx].iter().copied().sum();
            let y_offset = (offset / row_short_u16 as f64) as u16;

            let h = if idx == best_split {
                area.height.saturating_sub(y_offset)
            } else {
                height
            };

            (area.x, area.y + y_offset, row_short_u16, h.max(1))
        };

        if w > 0 && h > 0 {
            result.push(Rect::new(x, y, w, h));
        } else {
            result.push(Rect::default());
        }
    }

    // Recurse on remaining area.
    let next_area = if row_horizontal {
        Rect::new(
            area.x,
            area.y + row_short_u16,
            area.width,
            remaining_len,
        )
    } else {
        Rect::new(
            area.x + row_short_u16,
            area.y,
            remaining_len,
            area.height,
        )
    };

    squarify_row(
        areas,
        best_split + 1,
        end,
        next_area,
        !row_horizontal, // alternate direction.
        result,
    );
}

/// Compute the worst aspect ratio in a row given the item areas and row short side.
fn worst_aspect(areas: &[f64], short: f64) -> f64 {
    let mut worst = 0.0_f64;
    for &a in areas {
        let long = a / short;
        let aspect = long.max(short / a) / long.min(short / a).max(f64::EPSILON);
        worst = worst.max(aspect);
    }
    worst
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_squarify_returns_correct_count() {
        let items = vec![
            ("a".to_string(), 100),
            ("b".to_string(), 200),
            ("c".to_string(), 300),
        ];
        let area = Rect::new(0, 0, 50, 20);
        let rects = squarify(&items, area);
        assert_eq!(rects.len(), 3);
    }

    #[test]
    fn test_squarify_rects_fit_in_area() {
        let items = vec![
            ("a".to_string(), 100),
            ("b".to_string(), 200),
        ];
        let area = Rect::new(0, 0, 40, 15);
        let rects = squarify(&items, area);
        for rect in &rects {
            assert!(rect.x + rect.width <= area.x + area.width);
            assert!(rect.y + rect.height <= area.y + area.height);
        }
    }

    #[test]
    fn test_squarify_proportional_sizes() {
        // Items with very different sizes should produce proportional rects.
        let items = vec![
            ("large".to_string(), 600),
            ("medium".to_string(), 300),
            ("small".to_string(), 100),
        ];
        let area = Rect::new(0, 0, 100, 10); // total area = 1000
        let rects = squarify(&items, area);
        assert_eq!(rects.len(), 3);

        // Compute actual areas.
        let areas: Vec<u64> = rects.iter().map(|r| r.width as u64 * r.height as u64).collect();

        // Largest item should have the largest area.
        assert!(
            areas[0] >= areas[1] && areas[0] >= areas[2],
            "largest item should have largest area: {:?} vs {:?} vs {:?}",
            areas[0],
            areas[1],
            areas[2]
        );

        // Medium should be larger than small.
        assert!(
            areas[1] >= areas[2],
            "medium should be >= small: {} >= {}",
            areas[1],
            areas[2]
        );

        // Ratio between largest and smallest should be at least 2:1 (reflects 6:1 input).
        assert!(
            areas[0] >= 2 * areas[2],
            "largest should be at least 2x smallest: {} vs {}",
            areas[0],
            areas[2]
        );
    }

    #[test]
    fn test_truncate_path() {
        // Long path: should contain "..." and end with the last component.
        let path = Path::new("/var/log/syslog/archive/old.log");
        let label = truncate_path(path, 4);
        assert!(label.contains("..."), "label should contain ...: {}", label);
        assert!(label.ends_with("old.log"), "label should end with filename: {}", label);
    }

    #[test]
    fn test_truncate_path_short() {
        let path = Path::new("/tmp/test.txt");
        let label = truncate_path(path, 2);
        assert!(!label.contains("..."), "short path should not be truncated: {}", label);
    }

    #[test]
    fn test_truncate_for_width() {
        // No path separator — show tail of string.
        let label = "hello_world_test";
        assert_eq!(truncate_for_width(label, 20), "hello_world_test");
        // max_width=10: show last 10 chars ("world_test").
        assert_eq!(truncate_for_width(label, 10), "world_test");

        // Path — prefer showing filename.
        let home = std::env::var("HOME").ok();
        if let Some(h) = home {
            let path = format!("{}/docs/project/file.rs", h);
            let truncated = truncate_for_width(&path, 15);
            assert!(
                truncated.ends_with("file.rs"),
                "should end with filename: {}",
                truncated
            );
        }
    }

    #[test]
    fn test_empty_squarify() {
        let items: Vec<(String, u64)> = vec![];
        let area = Rect::new(0, 0, 50, 20);
        let rects = squarify(&items, area);
        assert!(rects.is_empty());
    }
}
