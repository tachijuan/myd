use ratatui::{
    Frame,
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph},
};
use std::path::{Path, PathBuf};

use crate::utils::filetype::FileCategory;
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
    /// Whether this tile is a directory.
    pub is_dir: bool,
    /// Content category driving the tile's color. For a file this comes from its
    /// extension; for a directory it is the category holding most of its bytes.
    pub category: FileCategory,
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
                // Decided in `collect_items` from the tree's own data — never by
                // touching the filesystem, since this runs on every sort.
                let category = info.category;
                TreemapCell {
                    rect: Rect::default(),
                    path: info.path,
                    resolved_path: info.resolved_path,
                    size,
                    label,
                    depth: info.depth,
                    is_dir: info.is_dir,
                    category,
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
        // Nothing to lay out in a degenerate area (terminal squeezed too small).
        // Zero the rects so stale ones can't be navigated to or drawn.
        if area.width == 0 || area.height == 0 {
            for cell in &mut self.cells {
                cell.rect = Rect::new(area.x, area.y, 0, 0);
            }
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

    /// Navigate to the cell most directly in the given direction.
    ///
    /// Filter is edge-based, not center-based: moving Right only considers cells
    /// starting at or past the current cell's right edge. Center-based filtering
    /// wrongly treats a small cell nested within the current cell's span as being
    /// "to the right" of it.
    ///
    /// Ranking is two-tier: any cell overlapping the current one along the
    /// orthogonal axis beats every non-overlapping cell, regardless of distance.
    /// Within a tier, the nearest edge wins, ties broken by orthogonal offset.
    /// A single blended score can't express this — when no candidate overlaps,
    /// the overlap term ties and the offset term picks an arbitrary winner.
    fn navigate_direction(&self, current: usize, direction: NavDirection) -> Option<usize> {
        if self.cells.is_empty() || current >= self.cells.len() {
            return None;
        }

        let cur = &self.cells[current];
        let cur_l = cur.rect.x as i32;
        let cur_r = (cur.rect.x + cur.rect.width) as i32;
        let cur_t = cur.rect.y as i32;
        let cur_b = (cur.rect.y + cur.rect.height) as i32;

        // Rank key: (not_overlapping, primary distance, orthogonal offset).
        // Lower is better; the bool orders false < true, so overlapping wins.
        let mut best: Option<(usize, (bool, i32, i32))> = None;

        for (i, cell) in self.cells.iter().enumerate() {
            if i == current {
                continue;
            }

            let o_l = cell.rect.x as i32;
            let o_r = (cell.rect.x + cell.rect.width) as i32;
            let o_t = cell.rect.y as i32;
            let o_b = (cell.rect.y + cell.rect.height) as i32;

            let key = match direction {
                NavDirection::Down => {
                    if o_t < cur_b {
                        continue;
                    }
                    let overlaps = horizontal_overlap(cur, cell) > 0.0;
                    (!overlaps, o_t - cur_b, (o_l - cur_l).abs())
                }
                NavDirection::Up => {
                    if o_b > cur_t {
                        continue;
                    }
                    let overlaps = horizontal_overlap(cur, cell) > 0.0;
                    (!overlaps, cur_t - o_b, (o_l - cur_l).abs())
                }
                NavDirection::Right => {
                    if o_l < cur_r {
                        continue;
                    }
                    let overlaps = vertical_overlap(cur, cell) > 0.0;
                    (!overlaps, o_l - cur_r, (o_t - cur_t).abs())
                }
                NavDirection::Left => {
                    if o_r > cur_l {
                        continue;
                    }
                    let overlaps = vertical_overlap(cur, cell) > 0.0;
                    (!overlaps, cur_l - o_r, (o_t - cur_t).abs())
                }
            };

            if best.is_none() || key < best.as_ref().unwrap().1 {
                best = Some((i, key));
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

    /// Whether there is a tile to the left of the cursor. False when the cursor
    /// is on a left-edge tile with nowhere further left to go.
    pub fn can_move_left(&self) -> bool {
        self.navigate_direction(self.cursor, NavDirection::Left)
            .is_some()
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

    /// The selected tile's label, but only when the tile is too narrow to show
    /// it in full. Returns `None` when the label already fits, so the caller can
    /// surface the name elsewhere exactly when the tile alone is not enough.
    ///
    /// Reads the rect stored by the last render, so it reflects what is
    /// actually on screen at the current terminal size.
    pub fn truncated_selected_label(&self) -> Option<&str> {
        let cell = self.selected_cell()?;
        let inner_width = cell.rect.width.saturating_sub(2) as usize;
        // A tile with no room at all draws no label; one whose label is clipped
        // draws a prefix. Both cases leave the full name unreadable.
        if inner_width == 0 || cell.label.chars().count() > inner_width {
            Some(&cell.label)
        } else {
            None
        }
    }

    /// Categories currently on screen, ordered by how many bytes each covers
    /// (largest first). Drives the legend, so it only names colors actually
    /// visible rather than the whole palette.
    pub fn categories_present(&self) -> Vec<FileCategory> {
        let mut totals: std::collections::HashMap<FileCategory, u64> =
            std::collections::HashMap::new();
        for cell in &self.cells {
            *totals.entry(cell.category).or_insert(0) += cell.size;
        }
        let mut cats: Vec<(FileCategory, u64)> = totals.into_iter().collect();
        // Sort by bytes descending, breaking ties on category order so the
        // legend stays stable between redraws.
        cats.sort_by_key(|(cat, bytes)| (std::cmp::Reverse(*bytes), *cat));
        cats.into_iter().map(|(cat, _)| cat).collect()
    }

    /// Render the treemap on the given area.
    ///
    /// - `selected`: the cursor index (reversed colors)
    /// - `highlighted_path`: path from the tree view's selection (brighter color)
    pub fn render(
        &mut self,
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

        // Recompute layout for the current area and keep it on `self` — cursor
        // navigation reads these rects, so they must reflect what was drawn.
        self.compute_layout(area);

        for (i, cell) in self.cells.iter().enumerate() {
            let rect = cell.rect;
            if rect.width < 2 || rect.height < 1 {
                continue;
            }

            let is_selected = i == selected;
            let is_highlighted = highlighted_path
                .map(|p| cell.resolved_path == *p)
                .unwrap_or(false);

            // The tile is filled with its category color, so related content
            // reads as one group. Selection and tree-highlight change only the
            // border and label weight — never the fill, which carries meaning.
            let bg = cell.category.bg_color();
            let (border_color, label_modifier) = if is_selected {
                (Color::White, Modifier::BOLD | Modifier::UNDERLINED)
            } else if is_highlighted {
                (Color::Yellow, Modifier::BOLD)
            } else {
                (Color::Rgb(90, 90, 100), Modifier::empty())
            };

            frame.render_widget(Clear, rect);
            frame.render_widget(
                Block::default()
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(border_color).bg(bg))
                    .style(Style::default().bg(bg)),
                rect,
            );

            // Label goes inside the border. Any inner room at all is worth using —
            // a narrow tile showing a few characters beats an anonymous box.
            let inner = Rect::new(
                rect.x + 1,
                rect.y + 1,
                rect.width.saturating_sub(2),
                rect.height.saturating_sub(2),
            );
            if inner.width >= 1 && inner.height >= 1 {
                let para = Paragraph::new(Line::from(Span::styled(
                    truncate_for_width(&cell.label, inner.width as usize),
                    Style::default()
                        .fg(cell.category.fg_color())
                        .bg(bg)
                        .add_modifier(label_modifier),
                )));
                frame.render_widget(para, inner);
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
    is_dir: bool,
    /// Content category, decided from the tree's own loaded children rather than
    /// by walking the disk (see `category_of_node`).
    category: crate::utils::filetype::FileCategory,
}

/// A directory's category, from the children the tree already holds.
///
/// The tree loaded these when the directory was opened, so classifying them is
/// pure computation. Walking the filesystem here instead cost a `readdir` plus a
/// `stat` per file, per directory tile, on every treemap rebuild — and the
/// treemap is rebuilt on every sort. On a CIFS mount that turned a 2ms sort into
/// a 10-16 second freeze.
///
/// An unloaded directory has nothing to classify, so it takes the neutral
/// colour until it is expanded.
fn category_of_node(node: &crate::widget::file_tree::TreeNode, tree: &FileTree) -> FileCategory {
    use crate::utils::filetype::dominant_category_of;

    // Preferred: the tally the size walk recorded, which covers the whole
    // subtree including directories the user has not expanded.
    if let Some(totals) = tree.size_cache.category_totals(&node.resolved_path) {
        if let Some(cat) = FileCategory::dominant_of_totals(&totals) {
            return cat;
        }
    }

    // Fallback for a tree whose sizes came from a listing rather than a walk
    // (a remote backend): classify the children that are loaded.
    match node.children.as_ref() {
        Some(children) => dominant_category_of(children.iter().filter(|c| !c.is_dir).map(|c| {
            (
                c.path.as_path(),
                tree.size_cache.get(&c.resolved_path).unwrap_or(0),
            )
        })),
        None => FileCategory::Other,
    }
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
                // Sizes come from the cache the tree load populated. Measuring
                // here would mean a recursive walk per uncached directory, and
                // the treemap is rebuilt on every sort — unaffordable on a slow
                // filesystem, whether that is SFTP or a CIFS/NFS mount that
                // merely looks local.
                let size = tree.size_cache.get(&child.resolved_path).unwrap_or(0);
                items.push((
                    TreeItemInfo {
                        path: child.path.clone(),
                        resolved_path: child.resolved_path.clone(),
                        depth: depth + 1,
                        is_dir: child.is_dir,
                        category: category_of_node(child, tree),
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
                let size = tree.size_cache.get(&child.resolved_path).unwrap_or(0);
                items.push((
                    TreeItemInfo {
                        path: child.path.clone(),
                        resolved_path: child.resolved_path.clone(),
                        depth: depth + 1,
                        is_dir: child.is_dir,
                        // A file is classified by its extension alone.
                        category: crate::utils::filetype::categorize(&child.path),
                    },
                    size,
                ));
            }
        }
    }
}

/// Label a path for display inside a treemap tile.
///
/// Tiles are small and their position already conveys where an entry sits in the
/// hierarchy, so the basename is what identifies it — a full path just gets
/// truncated into noise. Falls back to the whole path for entries without a
/// basename (e.g. `/`).
fn truncate_path(path: &Path, _depth: usize) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| path.display().to_string())
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

        // Fallback: show as much of the filename as fits, from the start —
        // leading characters are what identify the entry.
        filename.chars().take(max_width).collect()
    } else {
        // Not a path — keep the leading characters.
        label.chars().take(max_width).collect()
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

    let denom = a_width.min(b_width);
    if overlap <= 0.0 || denom <= 0.0 {
        return 0.0;
    }

    overlap / denom
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

    let denom = a_height.min(b_height);
    if overlap <= 0.0 || denom <= 0.0 {
        return 0.0;
    }

    overlap / denom
}

// ---------------------------------------------------------------------------
// Squarification algorithm (Jansen, Buja 2003)
// ---------------------------------------------------------------------------

/// Squarify a list of (label, value) pairs into rectangles within the given area.
/// Uses a binary-split strategy: at each step, try splitting the items into two
/// groups horizontally or vertically, pick the better orientation, and recurse.
fn squarify(items: &[(String, u64)], area: Rect) -> Vec<Rect> {
    if items.is_empty() {
        return Vec::new();
    }

    let total: u64 = items.iter().map(|(_, v)| v).sum();
    if total == 0 {
        equal_split(items, area)
    } else {
        squarify_split(items, area)
    }
}

/// Split items equally when they have no meaningful sizes.
fn equal_split(items: &[(String, u64)], area: Rect) -> Vec<Rect> {
    // A degenerate area has no room to divide; hand back empty rects so callers
    // skip these tiles rather than dividing by zero.
    if items.is_empty() || area.height == 0 || area.width == 0 {
        return vec![Rect::new(area.x, area.y, 0, 0); items.len()];
    }

    let n = items.len() as u16;
    // Rows are only as tall as the area allows; with more items than rows the
    // extras collapse onto the last row rather than running past the bottom.
    let rows = n.min(area.height);
    let height = area.height / rows;
    items
        .iter()
        .enumerate()
        .map(|(i, _)| {
            let row = (i as u16).min(rows - 1);
            let y = area.y + row * height;
            let h = height.min(area.height.saturating_sub(y - area.y));
            Rect::new(area.x, y, area.width, h.max(1))
        })
        .collect()
}

/// Recursive binary-split squarification.
fn squarify_split(items: &[(String, u64)], area: Rect) -> Vec<Rect> {
    if area.width < 2 || area.height < 2 {
        return equal_split(items, area);
    }

    if items.len() == 1 {
        return vec![area];
    }

    let total_value: f64 = items.iter().map(|(_, v)| *v as f64).sum();
    let total_pixel_area = area.width as f64 * area.height as f64;

    // Cumulative pixel areas (proportional to values).
    let cum: Vec<f64> = items
        .iter()
        .scan(0.0, |acc, (_, v)| {
            *acc += (*v as f64 / total_value) * total_pixel_area;
            *acc = acc.max(1.0);
            Some(*acc)
        })
        .collect();

    let mut best_split: Option<(usize, bool, f64)> = None; // (split_index, horizontal, cost)

    // Try splitting at each position.
    for split in 1..items.len() {
        // Horizontal split: groups are stacked top/bottom (share width).
        let left_h = (cum[split - 1] / area.width as f64).max(1.0);
        let right_h = area.height as f64 - left_h;
        if right_h >= 1.0 {
            // Left group: area.width × left_h. Right group: area.width × right_h.
            let cost = rect_aspect(area.width as f64, left_h)
                + rect_aspect(area.width as f64, right_h);
            if best_split.is_none() || cost < best_split.as_ref().unwrap().2 {
                best_split = Some((split, true, cost));
            }
        }

        // Vertical split: groups are placed left/right (share height).
        let left_w = (cum[split - 1] / area.height as f64).max(1.0);
        let right_w = area.width as f64 - left_w;
        if right_w >= 1.0 {
            // Left group: left_w × area.height. Right group: right_w × area.height.
            let cost = rect_aspect(left_w, area.height as f64)
                + rect_aspect(right_w, area.height as f64);
            if best_split.is_none() || cost < best_split.as_ref().unwrap().2 {
                best_split = Some((split, false, cost));
            }
        }
    }

    let (split, horizontal, _) = best_split.unwrap_or((items.len() / 2, true, f64::MAX));

    if horizontal {
        let left_h = ((cum[split - 1] / area.width as f64) as u16).max(1);
        let left_h = left_h.min(area.height - 1);
        let right_h = area.height - left_h;

        let left_area = Rect::new(area.x, area.y, area.width, left_h);
        let right_area = Rect::new(area.x, area.y + left_h, area.width, right_h);

        let mut result = squarify_split(&items[..split], left_area);
        let right = squarify_split(&items[split..], right_area);
        result.extend(right);
        result
    } else {
        let left_w = ((cum[split - 1] / area.height as f64) as u16).max(1);
        let left_w = left_w.min(area.width - 1);
        let right_w = area.width - left_w;

        let left_area = Rect::new(area.x, area.y, left_w, area.height);
        let right_area = Rect::new(area.x + left_w, area.y, right_w, area.height);

        let mut result = squarify_split(&items[..split], left_area);
        let right = squarify_split(&items[split..], right_area);
        result.extend(right);
        result
    }
}

/// Compute the aspect ratio of a rectangle as a cost (1.0 = perfect square, higher = worse).
fn rect_aspect(width: f64, height: f64) -> f64 {
    let w = width.max(f64::EPSILON);
    let h = height.max(f64::EPSILON);
    w.max(h) / w.min(h)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a TreeMap of `sizes` and lay it out in `area` the way render does.
    fn laid_out(sizes: &[u64], area: Rect) -> TreeMap {
        let cells: Vec<TreemapCell> = sizes
            .iter()
            .enumerate()
            .map(|(i, s)| TreemapCell {
                rect: Rect::default(),
                path: PathBuf::from(format!("/d{}", i)),
                resolved_path: PathBuf::from(format!("/d{}", i)),
                size: *s,
                label: format!("d{}", i),
                depth: 1,
                is_dir: true,
                category: FileCategory::Other,
            })
            .collect();
        let mut tm = TreeMap {
            cells,
            cursor: 0,
            total_size: sizes.iter().sum(),
        };
        tm.compute_layout(area);
        tm
    }

    /// A 3x3 grid of uniform tiles, indices laid out as 0 1 2 / 3 4 5 / 6 7 8.
    fn grid_3x3() -> TreeMap {
        let mut cells = Vec::new();
        for row in 0..3u16 {
            for col in 0..3u16 {
                let n = row * 3 + col;
                cells.push(TreemapCell {
                    rect: Rect::new(col * 20, row * 8, 20, 8),
                    path: PathBuf::from(format!("/c{}", n)),
                    resolved_path: PathBuf::from(format!("/c{}", n)),
                    size: 100,
                    label: format!("c{}", n),
                    depth: 1,
                    is_dir: true,
                    category: FileCategory::Other,
                });
            }
        }
        TreeMap {
            cells,
            cursor: 0,
            total_size: 900,
        }
    }

    #[test]
    fn test_render_persists_layout_for_navigation() {
        // Regression: render used to lay out a throwaway clone, leaving every rect
        // on `self` at its 0x0 default, so h/j/k/l could never find a neighbour.
        use ratatui::{Terminal, backend::TestBackend};
        let mut tm = laid_out(&[1000, 500, 300, 200], Rect::new(0, 0, 80, 24));
        // Wipe the layout so only render can restore it.
        for c in &mut tm.cells {
            c.rect = Rect::default();
        }

        let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
        terminal
            .draw(|f| {
                let a = f.area();
                tm.render(f, a, 0, None);
            })
            .unwrap();

        assert!(
            tm.cells.iter().all(|c| c.rect.width > 0 && c.rect.height > 0),
            "render must store the computed layout on self: {:?}",
            tm.cells.iter().map(|c| c.rect).collect::<Vec<_>>()
        );

        tm.cursor = 0;
        tm.cursor_right();
        assert_eq!(tm.cursor, 1, "cursor must move right after a real render");
    }

    #[test]
    fn test_navigation_on_uniform_grid() {
        let mut tm = grid_3x3();
        let cases: &[(usize, NavDirection, Option<usize>)] = &[
            (4, NavDirection::Left, Some(3)),
            (4, NavDirection::Right, Some(5)),
            (4, NavDirection::Up, Some(1)),
            (4, NavDirection::Down, Some(7)),
            (0, NavDirection::Right, Some(1)),
            (0, NavDirection::Down, Some(3)),
            (8, NavDirection::Left, Some(7)),
            (8, NavDirection::Up, Some(5)),
            // Edges of the map have nowhere to go.
            (0, NavDirection::Left, None),
            (0, NavDirection::Up, None),
            (8, NavDirection::Right, None),
            (8, NavDirection::Down, None),
        ];
        for (from, dir, want) in cases {
            tm.cursor = *from;
            assert_eq!(
                tm.navigate_direction(*from, *dir),
                *want,
                "from {} going {:?}",
                from,
                dir
            );
        }
    }

    #[test]
    fn test_navigation_is_reversible_on_squarified_layout() {
        // Regression: the old blended score added an axis-offset term that could
        // outrank overlap, so Right could land on a cell whose Left did not return.
        let tm = laid_out(&[1000, 500, 300, 200, 100, 50, 25, 10], Rect::new(0, 0, 80, 24));
        for i in 0..tm.cells.len() {
            for (dir, back) in [
                (NavDirection::Right, NavDirection::Left),
                (NavDirection::Down, NavDirection::Up),
            ] {
                if let Some(j) = tm.navigate_direction(i, dir) {
                    assert_eq!(
                        tm.navigate_direction(j, back),
                        Some(i),
                        "{} {:?} -> {}, but going {:?} did not come back",
                        i,
                        dir,
                        j,
                        back
                    );
                }
            }
        }
    }

    #[test]
    fn test_every_cell_reachable_on_squarified_layout() {
        let tm = laid_out(&[1000, 500, 300, 200, 100, 50, 25, 10], Rect::new(0, 0, 80, 24));
        let mut seen = std::collections::HashSet::new();
        let mut queue = vec![0usize];
        seen.insert(0usize);
        while let Some(cur) = queue.pop() {
            for dir in [
                NavDirection::Up,
                NavDirection::Down,
                NavDirection::Left,
                NavDirection::Right,
            ] {
                if let Some(n) = tm.navigate_direction(cur, dir) {
                    if seen.insert(n) {
                        queue.push(n);
                    }
                }
            }
        }
        assert_eq!(
            seen.len(),
            tm.cells.len(),
            "every tile must be reachable with h/j/k/l; reached {:?}",
            seen
        );
    }

    #[test]
    fn test_navigation_ignores_cells_within_own_span() {
        // A tall cell spanning the full height has nothing above or below it, even
        // though shorter cells elsewhere have centers higher/lower than its own.
        let tm = TreeMap {
            cells: vec![
                TreemapCell {
                    rect: Rect::new(0, 0, 40, 24),
                    path: PathBuf::from("/tall"),
                    resolved_path: PathBuf::from("/tall"),
                    size: 100,
                    label: "tall".into(),
                    depth: 1,
                    is_dir: true,
                    category: FileCategory::Other,
                },
                TreemapCell {
                    rect: Rect::new(40, 0, 40, 12),
                    path: PathBuf::from("/top"),
                    resolved_path: PathBuf::from("/top"),
                    size: 50,
                    label: "top".into(),
                    depth: 1,
                    is_dir: true,
                    category: FileCategory::Other,
                },
                TreemapCell {
                    rect: Rect::new(40, 12, 40, 12),
                    path: PathBuf::from("/bot"),
                    resolved_path: PathBuf::from("/bot"),
                    size: 50,
                    label: "bot".into(),
                    depth: 1,
                    is_dir: true,
                    category: FileCategory::Other,
                },
            ],
            cursor: 0,
            total_size: 200,
        };
        assert_eq!(tm.navigate_direction(0, NavDirection::Down), None);
        assert_eq!(tm.navigate_direction(0, NavDirection::Up), None);
        // Right is a real move; the nearer-aligned tile wins.
        assert_eq!(tm.navigate_direction(0, NavDirection::Right), Some(1));
        assert_eq!(tm.navigate_direction(1, NavDirection::Down), Some(2));
        assert_eq!(tm.navigate_direction(2, NavDirection::Up), Some(1));
    }

    #[test]
    fn test_navigate_direction_out_of_bounds_cursor() {
        let tm = laid_out(&[100, 50], Rect::new(0, 0, 40, 10));
        assert_eq!(tm.navigate_direction(99, NavDirection::Down), None);
        let empty = TreeMap {
            cells: Vec::new(),
            cursor: 0,
            total_size: 0,
        };
        assert_eq!(empty.navigate_direction(0, NavDirection::Right), None);
    }

    #[test]
    fn test_squarify_tiles_area_without_gaps_or_overlaps() {
        let area = Rect::new(0, 0, 80, 24);
        let sizes: Vec<u64> = vec![1000, 500, 300, 200, 100, 50, 25, 10];
        let items: Vec<(String, u64)> = sizes
            .iter()
            .enumerate()
            .map(|(i, s)| (format!("p{}", i), *s))
            .collect();
        let rects = squarify(&items, area);

        let covered: u64 = rects.iter().map(|r| r.width as u64 * r.height as u64).sum();
        assert_eq!(
            covered,
            area.width as u64 * area.height as u64,
            "tiles must cover the area exactly"
        );

        for (i, r) in rects.iter().enumerate() {
            assert!(
                r.x + r.width <= area.x + area.width && r.y + r.height <= area.y + area.height,
                "tile {} out of bounds: {:?}",
                i,
                r
            );
        }

        for i in 0..rects.len() {
            for j in (i + 1)..rects.len() {
                let (a, b) = (rects[i], rects[j]);
                let ox = (a.x + a.width).min(b.x + b.width) as i32 - a.x.max(b.x) as i32;
                let oy = (a.y + a.height).min(b.y + b.height) as i32 - a.y.max(b.y) as i32;
                assert!(ox <= 0 || oy <= 0, "tiles {} and {} overlap: {:?} {:?}", i, j, a, b);
            }
        }
    }

    #[test]
    fn test_render_labels_every_visible_tile() {
        // Regression: the label gate required 5 columns of inner width, so narrow
        // tiles rendered as anonymous empty boxes.
        use ratatui::{Terminal, backend::TestBackend};
        let mut tm = laid_out(&[1000, 500, 300, 200, 100, 50, 25, 10], Rect::new(0, 0, 80, 24));
        let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
        terminal
            .draw(|f| {
                let a = f.area();
                tm.render(f, a, 0, None);
            })
            .unwrap();
        let buf = terminal.backend().buffer().clone();

        for cell in &tm.cells {
            let r = cell.rect;
            if r.width < 3 || r.height < 3 {
                continue; // no room inside the border for any text
            }
            let mut text = String::new();
            for x in (r.x + 1)..(r.x + r.width - 1) {
                text.push_str(buf[(x, r.y + 1)].symbol());
            }
            assert!(
                !text.trim().is_empty(),
                "tile {:?} at {:?} rendered without a label",
                cell.label,
                r
            );
            assert!(
                cell.label.starts_with(text.trim()),
                "tile label {:?} should be truncated from the start, got {:?}",
                cell.label,
                text.trim()
            );
        }
    }


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
        // A tile is labelled with the basename, not the whole path — the tile's
        // position already conveys where the entry sits.
        let path = Path::new("/var/log/syslog/archive/old.log");
        assert_eq!(truncate_path(path, 4), "old.log");
    }

    #[test]
    fn test_truncate_path_short() {
        let path = Path::new("/tmp/test.txt");
        assert_eq!(truncate_path(path, 2), "test.txt");
    }

    #[test]
    fn test_truncate_path_has_no_stray_separators() {
        // Regression: the old middle-shortening built labels like "//...///aaa"
        // because the root component was not actually filtered out.
        for p in ["/aaa", "/tmp/x/aaa", "/a/b/c/d/e/aaa"] {
            let label = truncate_path(Path::new(p), 1);
            assert_eq!(label, "aaa", "{} produced {}", p, label);
            assert!(!label.contains('/'), "label must not contain separators: {}", label);
        }
    }

    #[test]
    fn test_truncate_for_width() {
        // No path separator — keep the leading characters, which identify the entry.
        let label = "hello_world_test";
        assert_eq!(truncate_for_width(label, 20), "hello_world_test");
        assert_eq!(truncate_for_width(label, 10), "hello_worl");

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

    #[test]
    fn test_treemap_navigation() {
        use std::path::PathBuf;

        // Build a TreeMap with 6 cells matching the debug layout.
        let mut tree_map = TreeMap {
            cells: vec![
                TreemapCell { rect: Rect::new(0, 0, 37, 28), path: PathBuf::from("/a"), resolved_path: PathBuf::from("/a"), size: 10_000_000, label: "a".to_string(), depth: 1, is_dir: true, category: FileCategory::Other },
                TreemapCell { rect: Rect::new(37, 0, 31, 28), path: PathBuf::from("/b"), resolved_path: PathBuf::from("/b"), size: 8_000_000, label: "b".to_string(), depth: 1, is_dir: true, category: FileCategory::Other },
                TreemapCell { rect: Rect::new(68, 0, 19, 28), path: PathBuf::from("/c"), resolved_path: PathBuf::from("/c"), size: 5_000_000, label: "c".to_string(), depth: 1, is_dir: true, category: FileCategory::Other },
                TreemapCell { rect: Rect::new(87, 0, 23, 14), path: PathBuf::from("/d"), resolved_path: PathBuf::from("/d"), size: 3_000_000, label: "d".to_string(), depth: 1, is_dir: true, category: FileCategory::Other },
                TreemapCell { rect: Rect::new(87, 14, 15, 14), path: PathBuf::from("/e"), resolved_path: PathBuf::from("/e"), size: 2_000_000, label: "e".to_string(), depth: 1, is_dir: true, category: FileCategory::Other },
                TreemapCell { rect: Rect::new(102, 14, 8, 14), path: PathBuf::from("/f"), resolved_path: PathBuf::from("/f"), size: 1_000_000, label: "f".to_string(), depth: 1, is_dir: true, category: FileCategory::Other },
            ],
            cursor: 0,
            total_size: 29_000_000,
        };

        // Right from 0 (a at 0,0,37,28) should go to 1 (b at 37,0,31,28).
        tree_map.cursor = 0;
        tree_map.cursor_right();
        assert_eq!(
            tree_map.cursor, 1,
            "Right from 'a' should go to 'b', got {}",
            tree_map.cursor
        );

        // Left from 1 (b) should go back to 0 (a).
        tree_map.cursor_left();
        assert_eq!(
            tree_map.cursor, 0,
            "Left from 'b' should go back to 'a', got {}",
            tree_map.cursor
        );

        // Right from 1 (b at 37,0) should go to 2 (c at 68,0).
        tree_map.cursor = 1;
        tree_map.cursor_right();
        assert_eq!(
            tree_map.cursor, 2,
            "Right from 'b' should go to 'c', got {}",
            tree_map.cursor
        );

        // Right from 2 (c at 68,0) should go to 3 (d at 87,0) — they share vertical overlap.
        tree_map.cursor = 2;
        tree_map.cursor_right();
        assert_eq!(
            tree_map.cursor, 3,
            "Right from 'c' should go to 'd', got {}",
            tree_map.cursor
        );

        // Down from 3 (d at 87,0,23,14) should go to 4 (e at 87,14,15,14) — directly below.
        tree_map.cursor = 3;
        tree_map.cursor_down();
        assert_eq!(
            tree_map.cursor, 4,
            "Down from 'd' should go to 'e', got {}",
            tree_map.cursor
        );

        // Right from 4 (e at 87,14,15,14) should go to 5 (f at 102,14) — f starts
        // exactly at e's right edge and the two share their full vertical span.
        tree_map.cursor = 4;
        tree_map.cursor_right();
        assert_eq!(
            tree_map.cursor, 5,
            "Right from 'e' should go to 'f', got {}",
            tree_map.cursor
        );

        // Down from 0 ('a' at 0,0,37,28) must not move: 'a' spans the full height,
        // so 'e' and 'f' (y 14..28) lie within its vertical span, not below it.
        // Jumping to a cell on the far right of the map would be a sideways teleport.
        tree_map.cursor = 0;
        tree_map.cursor_down();
        assert_eq!(
            tree_map.cursor, 0,
            "Down from full-height 'a' should stay put, got {}",
            tree_map.cursor
        );
    }
}
