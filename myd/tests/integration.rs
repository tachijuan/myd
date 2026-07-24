use std::fs::{self, File};
use std::io::Write;
use std::path::PathBuf;

/// Helper to create a temp directory with a known file structure.
fn create_test_structure() -> tempfile::TempDir {
    let dir = tempfile::TempDir::with_prefix("file-browser-test").unwrap();

    let f1 = dir.path().join("file_a.txt");
    write_string(&f1, "hello world");

    let f2 = dir.path().join("file_b.txt");
    write_string(&f2, "abc");

    let f3 = dir.path().join(".hidden_file");
    write_string(&f3, "secret");

    let subdir = dir.path().join("subdir");
    fs::create_dir(&subdir).unwrap();

    let f4 = subdir.join("nested.txt");
    write_string(&f4, "nested content here");

    let nested = subdir.join("deep");
    fs::create_dir(&nested).unwrap();

    let f5 = nested.join("deep_file.txt");
    write_string(&f5, "deep");

    dir
}

fn write_string(path: &PathBuf, content: &str) {
    let mut f = File::create(path).unwrap();
    f.write_all(content.as_bytes()).unwrap();
}

// ============================================================
// Size utilities tests
// ============================================================

#[test]
fn test_format_size_zero() {
    assert_eq!(myd::utils::sizes::format_size(0), "   0 B");
}

#[test]
fn test_format_size_bytes() {
    assert_eq!(myd::utils::sizes::format_size(42), "  42 B");
}

#[test]
fn test_format_size_kb() {
    assert_eq!(myd::utils::sizes::format_size(1024), " 1.0KB");
}

#[test]
fn test_format_size_mb() {
    assert_eq!(myd::utils::sizes::format_size(1_048_576), " 1.0MB");
}

#[test]
fn test_format_size_gb() {
    assert_eq!(myd::utils::sizes::format_size(1_073_741_824), " 1.0GB");
}

#[test]
fn test_get_file_size() {
    let dir = tempfile::tempdir().unwrap();
    let f = dir.path().join("test.bin");
    write_string(&f, "0123456789");
    assert_eq!(myd::utils::sizes::get_file_size(&f), 10);
}

#[test]
fn test_get_shallow_size() {
    let dir = create_test_structure();
    let size = myd::utils::sizes::get_shallow_size(dir.path());
    assert!(size >= 20);
}

#[test]
fn test_get_dir_size() {
    let dir = create_test_structure();
    let size = myd::utils::sizes::get_dir_size(dir.path());
    assert!(size >= 30);
}

#[test]
fn test_size_cache_concurrent() {
    let cache = myd::utils::sizes::SizeCache::new();
    let dir = tempfile::tempdir().unwrap();
    let f = dir.path().join("x.txt");
    write_string(&f, "abc");

    cache.insert(&f, 3);
    assert_eq!(cache.get(&f), Some(3));
    cache.clear();
    assert_eq!(cache.get(&f), None);
}

// ============================================================
// FileTree / SortMode tests
// ============================================================

#[test]
fn test_file_tree_root() {
    let dir = create_test_structure();
    let path = dir.path();
    assert!(path.is_dir());
    let entries: Vec<_> = fs::read_dir(path).unwrap().flatten().collect();
    assert!(entries.len() >= 3);
}

#[test]
fn test_sort_mode_labels() {
    assert_eq!(myd::screen::SortMode::DirsFirst.label(), "dirs-first");
    assert_eq!(myd::screen::SortMode::FilesFirst.label(), "files-first");
    assert_eq!(myd::screen::SortMode::Largest.label(), "largest");
    assert_eq!(myd::screen::SortMode::Smallest.label(), "smallest");
}

#[test]
fn test_sort_mode_unique_labels() {
    let labels = [
        myd::screen::SortMode::DirsFirst.label(),
        myd::screen::SortMode::FilesFirst.label(),
        myd::screen::SortMode::Largest.label(),
        myd::screen::SortMode::Smallest.label(),
    ];
    for i in 0..labels.len() {
        for j in (i + 1)..labels.len() {
            assert_ne!(labels[i], labels[j], "SortMode labels must be unique");
        }
    }
}

// ============================================================
// Dialog tests
// ============================================================

#[test]
fn test_confirm_dialog_y() {
    let mut dialog = myd::widget::confirm_dialog::ConfirmDialog::new("Delete?");
    assert_eq!(dialog.handle_key('y'), Some(true));
}

#[test]
fn test_confirm_dialog_n() {
    let mut dialog = myd::widget::confirm_dialog::ConfirmDialog::new("Delete?");
    assert_eq!(dialog.handle_key('n'), Some(false));
}

#[test]
fn test_confirm_dialog_enter() {
    let mut dialog = myd::widget::confirm_dialog::ConfirmDialog::new("Delete?");
    assert_eq!(dialog.handle_key('\n'), Some(true));
}

#[test]
fn test_confirm_dialog_unknown_key() {
    let mut dialog = myd::widget::confirm_dialog::ConfirmDialog::new("Delete?");
    assert_eq!(dialog.handle_key('x'), None);
}

// ============================================================
// File operations tests
// ============================================================

#[test]
fn test_delete_file() {
    let dir = tempfile::tempdir().unwrap();
    let f = dir.path().join("delete_me.txt");
    write_string(&f, "goodbye");
    assert!(f.exists());
    fs::remove_file(&f).unwrap();
    assert!(!f.exists());
}

#[test]
fn test_delete_directory() {
    let dir = tempfile::tempdir().unwrap();
    let subdir = dir.path().join("subdir");
    fs::create_dir(&subdir).unwrap();
    let f = subdir.join("nested.txt");
    write_string(&f, "data");
    fs::remove_dir_all(&subdir).unwrap();
    assert!(!subdir.exists());
}

#[test]
fn test_rename_file() {
    let dir = tempfile::tempdir().unwrap();
    let old = dir.path().join("old.txt");
    write_string(&old, "data");
    let new = dir.path().join("new.txt");
    fs::rename(&old, &new).unwrap();
    assert!(!old.exists());
    assert!(new.exists());
    assert_eq!(fs::read_to_string(new).unwrap(), "data");
}

#[test]
fn test_rename_existing_fails() {
    let dir = tempfile::tempdir().unwrap();
    let a = dir.path().join("a.txt");
    let b = dir.path().join("b.txt");
    write_string(&a, "data");
    write_string(&b, "other");
    let res = fs::rename(&a, &b);
    // On Unix, rename replaces the target; on Windows, it fails.
    // Just verify it either succeeds or gives a platform-specific error.
    if res.is_err() {
        assert_eq!(res.unwrap_err().kind(), std::io::ErrorKind::AlreadyExists);
    }
}

// ============================================================
// CLI tests
// ============================================================

#[test]
fn test_cli_path_arg() {
    use clap::Parser;
    // The starting directory is positional: `myd /tmp`.
    let cli = myd::cli::Cli::parse_from(["myd", "/tmp"]);
    assert_eq!(cli.path, Some(PathBuf::from("/tmp")));
}

#[test]
fn test_cli_no_path() {
    use clap::Parser;
    let cli = myd::cli::Cli::parse_from(["file-browser"]);
    assert_eq!(cli.path, None);
}

#[test]
fn test_cli_positional_path_with_tilde() {
    use clap::Parser;
    let cli = myd::cli::Cli::parse_from(["myd", "~/Documents"]);
    assert_eq!(cli.path, Some(PathBuf::from("~/Documents")));
}

// ---------------------------------------------------------------------------
// Sorting: directories must order by their recursive (du-like) size.
// ---------------------------------------------------------------------------

/// Build a fixture where a directory's own entry is tiny but its contents are
/// large, so shallow-size sorting produces a visibly different order.
fn sorting_fixture() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("smalldir")).unwrap();
    std::fs::write(dir.path().join("smalldir/big.bin"), vec![0u8; 50_000]).unwrap();
    std::fs::write(dir.path().join("mid.txt"), vec![0u8; 10_000]).unwrap();
    std::fs::write(dir.path().join("tiny.txt"), vec![0u8; 10]).unwrap();
    dir
}

fn names_at_depth(tree: &myd::widget::file_tree::FileTree, depth: usize) -> Vec<String> {
    tree.lines
        .iter()
        .filter(|l| l.depth == depth)
        .map(|l| l.name.clone())
        .collect()
}

#[test]
fn test_sort_largest_uses_recursive_dir_size() {
    use myd::screen::SortMode;
    use myd::widget::file_tree::FileTree;

    let dir = sorting_fixture();
    let tree = FileTree::new(dir.path().to_path_buf(), SortMode::Largest, true, true);
    // smalldir (50 KB of contents) must outrank mid.txt (10 KB), which only
    // happens when the directory is measured recursively.
    assert_eq!(
        names_at_depth(&tree, 1),
        vec!["smalldir", "mid.txt", "tiny.txt"]
    );
}

#[test]
fn test_sort_smallest_is_reverse_of_largest() {
    use myd::screen::SortMode;
    use myd::widget::file_tree::FileTree;

    let dir = sorting_fixture();
    let tree = FileTree::new(dir.path().to_path_buf(), SortMode::Smallest, true, true);
    assert_eq!(
        names_at_depth(&tree, 1),
        vec!["tiny.txt", "mid.txt", "smalldir"]
    );
}

#[test]
fn test_sort_correct_after_switching_mode() {
    use myd::screen::SortMode;
    use myd::widget::file_tree::FileTree;

    // set_sort_mode clears the size cache; sizes must be recomputed, not read
    // back as shallow metadata lengths.
    let dir = sorting_fixture();
    let mut tree = FileTree::new(dir.path().to_path_buf(), SortMode::DirsFirst, true, true);
    tree.set_sort_mode(SortMode::Largest);
    assert_eq!(
        names_at_depth(&tree, 1),
        vec!["smalldir", "mid.txt", "tiny.txt"]
    );

    tree.set_sort_mode(SortMode::Smallest);
    assert_eq!(
        names_at_depth(&tree, 1),
        vec!["tiny.txt", "mid.txt", "smalldir"]
    );
}

#[test]
fn test_sort_correct_after_toggle_hidden() {
    use myd::screen::SortMode;
    use myd::widget::file_tree::FileTree;

    // toggle_hidden also clears the cache and reloads every expanded level.
    let dir = sorting_fixture();
    let mut tree = FileTree::new(dir.path().to_path_buf(), SortMode::Largest, false, true);
    tree.toggle_hidden();
    assert_eq!(
        names_at_depth(&tree, 1),
        vec!["smalldir", "mid.txt", "tiny.txt"]
    );
}

#[test]
fn test_sort_applies_at_nested_levels() {
    use myd::screen::SortMode;
    use myd::widget::file_tree::FileTree;

    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("parent/a_small")).unwrap();
    std::fs::write(dir.path().join("parent/a_small/f.bin"), vec![0u8; 100]).unwrap();
    // b_huge's weight is nested two levels down, so only a recursive walk finds it.
    std::fs::create_dir_all(dir.path().join("parent/b_huge/deep")).unwrap();
    std::fs::write(dir.path().join("parent/b_huge/deep/f.bin"), vec![0u8; 90_000]).unwrap();
    std::fs::create_dir_all(dir.path().join("parent/c_mid")).unwrap();
    std::fs::write(dir.path().join("parent/c_mid/f.bin"), vec![0u8; 5_000]).unwrap();

    let mut tree = FileTree::new(dir.path().to_path_buf(), SortMode::Largest, true, true);
    tree.expand_all();
    assert_eq!(names_at_depth(&tree, 2), vec!["b_huge", "c_mid", "a_small"]);
}

#[test]
fn test_sort_applies_to_lazily_expanded_children() {
    use myd::screen::SortMode;
    use myd::widget::file_tree::FileTree;

    // Expanding via the cursor is the path the UI actually takes.
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("p/a_small")).unwrap();
    std::fs::write(dir.path().join("p/a_small/f.bin"), vec![0u8; 100]).unwrap();
    std::fs::create_dir_all(dir.path().join("p/b_huge")).unwrap();
    std::fs::write(dir.path().join("p/b_huge/f.bin"), vec![0u8; 90_000]).unwrap();

    let mut tree = FileTree::new(dir.path().to_path_buf(), SortMode::Largest, true, true);
    tree.cursor = 1; // "p"
    tree.expand_cursor();
    assert_eq!(names_at_depth(&tree, 2), vec!["b_huge", "a_small"]);
}

// ---------------------------------------------------------------------------
// Treemap: end-to-end from a real directory.
// ---------------------------------------------------------------------------

#[test]
fn test_treemap_from_real_directory() {
    use myd::screen::SortMode;
    use myd::widget::file_tree::FileTree;
    use myd::widget::treemap::TreeMap;
    use ratatui::{Terminal, backend::TestBackend};

    let dir = tempfile::tempdir().unwrap();
    for (name, sz) in [
        ("aaa", 90_000usize),
        ("bbb", 40_000),
        ("ccc", 12_000),
        ("ddd", 3_000),
    ] {
        std::fs::create_dir_all(dir.path().join(name)).unwrap();
        std::fs::write(dir.path().join(name).join("f.bin"), vec![0u8; sz]).unwrap();
    }

    let tree = FileTree::new(dir.path().to_path_buf(), SortMode::Largest, true, true);
    let mut tm = TreeMap::from_file_tree(&tree);
    assert_eq!(tm.cells.len(), 4);

    let mut terminal = Terminal::new(TestBackend::new(70, 18)).unwrap();
    terminal
        .draw(|f| {
            let a = f.area();
            let cursor = tm.cursor;
            tm.render(f, a, cursor, None);
        })
        .unwrap();

    // Tiles are labelled with the basename only — no path noise like "//...///aaa".
    let labels: Vec<&str> = tm.cells.iter().map(|c| c.label.as_str()).collect();
    assert_eq!(labels, vec!["aaa", "bbb", "ccc", "ddd"]);

    // Bigger directories get bigger tiles.
    let areas: Vec<u32> = tm
        .cells
        .iter()
        .map(|c| c.rect.width as u32 * c.rect.height as u32)
        .collect();
    assert!(
        areas.windows(2).all(|w| w[0] >= w[1]),
        "tile areas must be non-increasing with size: {:?}",
        areas
    );

    // Navigation works against the layout that was actually drawn.
    tm.cursor = 0;
    tm.cursor_right();
    assert_eq!(tm.cursor, 1, "cursor_right should reach the next tile");
    tm.cursor_left();
    assert_eq!(tm.cursor, 0, "cursor_left should return");
}

#[test]
fn test_treemap_survives_degenerate_area() {
    use myd::screen::SortMode;
    use myd::widget::file_tree::FileTree;
    use myd::widget::treemap::TreeMap;
    use ratatui::{Terminal, backend::TestBackend, layout::Rect};

    let dir = sorting_fixture();
    let tree = FileTree::new(dir.path().to_path_buf(), SortMode::Largest, true, true);
    let mut tm = TreeMap::from_file_tree(&tree);

    // A terminal squeezed to nothing must not panic (equal_split used to divide
    // by zero when the area had no height).
    let mut terminal = Terminal::new(TestBackend::new(4, 1)).unwrap();
    terminal
        .draw(|f| {
            let cursor = tm.cursor;
            tm.render(f, Rect::new(0, 0, 0, 0), cursor, None);
        })
        .unwrap();
    terminal
        .draw(|f| {
            let a = f.area();
            let cursor = tm.cursor;
            tm.render(f, a, cursor, None);
        })
        .unwrap();
}

#[test]
fn test_screens_render_at_tiny_terminal_sizes() {
    use myd::screen::{Screen, SortMode};
    use myd::widget::file_tree::FileTree;
    use ratatui::{Terminal, backend::TestBackend};

    // Fixed-size dialogs and the treemap must clamp to the available area rather
    // than indexing outside the buffer.
    let dir = sorting_fixture();
    for (w, h) in [(1u16, 1u16), (2, 3), (10, 4), (40, 2), (80, 24)] {
        let tree = FileTree::new(dir.path().to_path_buf(), SortMode::Largest, true, true);
        let mut screen = Screen::Main(myd::screen::MainScreenState::from_tree(
            dir.path().to_path_buf(),
            tree,
        ));
        let mut terminal = Terminal::new(TestBackend::new(w, h)).unwrap();

        terminal
            .draw(|f| screen.render(f, f.area()))
            .unwrap_or_else(|e| panic!("tree view panicked at {}x{}: {}", w, h, e));

        // Same again in the treemap view.
        screen.toggle_view();
        terminal
            .draw(|f| screen.render(f, f.area()))
            .unwrap_or_else(|e| panic!("treemap view panicked at {}x{}: {}", w, h, e));
    }
}

// ---------------------------------------------------------------------------
// Treemap tile coloring by file category.
// ---------------------------------------------------------------------------

#[test]
fn test_tiles_colored_by_file_category() {
    use myd::utils::filetype::FileCategory;
    use myd::screen::SortMode;
    use myd::widget::file_tree::FileTree;
    use myd::widget::treemap::TreeMap;

    let dir = tempfile::tempdir().unwrap();
    // A leaf directory of files: each tile is one file, colored by extension.
    for (name, sz) in [
        ("main.rs", 100_000usize),
        ("clip.mp4", 70_000),
        ("photo.png", 50_000),
        ("notes.md", 35_000),
        ("data.json", 25_000),
        ("lib.so", 15_000),
    ] {
        std::fs::write(dir.path().join(name), vec![0u8; sz]).unwrap();
    }

    let tree = FileTree::new(dir.path().to_path_buf(), SortMode::Largest, true, true);
    let tm = TreeMap::from_file_tree(&tree);

    let got: Vec<(String, FileCategory)> = tm
        .cells
        .iter()
        .map(|c| (c.label.clone(), c.category))
        .collect();
    assert_eq!(
        got,
        vec![
            ("main.rs".to_string(), FileCategory::Code),
            ("clip.mp4".to_string(), FileCategory::Video),
            ("photo.png".to_string(), FileCategory::Image),
            ("notes.md".to_string(), FileCategory::Document),
            ("data.json".to_string(), FileCategory::Data),
            ("lib.so".to_string(), FileCategory::Binary),
        ]
    );

    // Distinct categories must be visually distinguishable.
    let mut colors: Vec<String> = tm
        .cells
        .iter()
        .map(|c| format!("{:?}", c.category.bg_color()))
        .collect();
    colors.sort();
    colors.dedup();
    assert_eq!(colors.len(), 6, "each category needs its own fill color");
}

#[test]
fn test_directory_tiles_take_dominant_content_color() {
    use myd::utils::filetype::FileCategory;
    use myd::screen::SortMode;
    use myd::widget::file_tree::FileTree;
    use myd::widget::treemap::TreeMap;

    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("code")).unwrap();
    std::fs::write(dir.path().join("code/main.rs"), vec![0u8; 120_000]).unwrap();
    std::fs::create_dir_all(dir.path().join("media")).unwrap();
    std::fs::write(dir.path().join("media/clip.mp4"), vec![0u8; 60_000]).unwrap();
    // A mixed directory whose bytes are mostly images, despite more code files.
    std::fs::create_dir_all(dir.path().join("mixed")).unwrap();
    std::fs::write(dir.path().join("mixed/a.rs"), vec![0u8; 500]).unwrap();
    std::fs::write(dir.path().join("mixed/b.rs"), vec![0u8; 500]).unwrap();
    std::fs::write(dir.path().join("mixed/big.png"), vec![0u8; 40_000]).unwrap();

    let tree = FileTree::new(dir.path().to_path_buf(), SortMode::Largest, true, true);
    let tm = TreeMap::from_file_tree(&tree);

    let cat_of = |name: &str| {
        tm.cells
            .iter()
            .find(|c| c.label == name)
            .unwrap_or_else(|| panic!("no tile named {}", name))
            .category
    };
    assert_eq!(cat_of("code"), FileCategory::Code);
    assert_eq!(cat_of("media"), FileCategory::Video);
    // Bytes decide, not file count.
    assert_eq!(cat_of("mixed"), FileCategory::Image);
    assert!(tm.cells.iter().all(|c| c.is_dir));
}

#[test]
fn test_render_fills_tiles_with_category_color() {
    use myd::screen::SortMode;
    use myd::widget::file_tree::FileTree;
    use myd::widget::treemap::TreeMap;
    use ratatui::{Terminal, backend::TestBackend};

    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("main.rs"), vec![0u8; 100_000]).unwrap();
    std::fs::write(dir.path().join("clip.mp4"), vec![0u8; 40_000]).unwrap();

    let tree = FileTree::new(dir.path().to_path_buf(), SortMode::Largest, true, true);
    let mut tm = TreeMap::from_file_tree(&tree);

    let mut terminal = Terminal::new(TestBackend::new(60, 16)).unwrap();
    terminal
        .draw(|f| {
            let a = f.area();
            let cursor = tm.cursor;
            tm.render(f, a, cursor, None);
        })
        .unwrap();
    let buf = terminal.backend().buffer().clone();

    // Every interior cell of a tile carries that tile's category background.
    for cell in &tm.cells {
        let r = cell.rect;
        assert!(r.width >= 3 && r.height >= 3, "tile too small to check: {:?}", r);
        let want = cell.category.bg_color();
        for y in (r.y + 1)..(r.y + r.height - 1) {
            for x in (r.x + 1)..(r.x + r.width - 1) {
                assert_eq!(
                    buf[(x, y)].bg,
                    want,
                    "tile {:?} not filled at ({}, {})",
                    cell.label,
                    x,
                    y
                );
            }
        }
    }
}

#[test]
fn test_legend_lists_visible_categories_by_size() {
    use myd::utils::filetype::FileCategory;
    use myd::screen::SortMode;
    use myd::widget::file_tree::FileTree;
    use myd::widget::treemap::TreeMap;

    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("a.rs"), vec![0u8; 10_000]).unwrap();
    std::fs::write(dir.path().join("b.rs"), vec![0u8; 10_000]).unwrap();
    std::fs::write(dir.path().join("c.mp4"), vec![0u8; 50_000]).unwrap();

    let tree = FileTree::new(dir.path().to_path_buf(), SortMode::Largest, true, true);
    let tm = TreeMap::from_file_tree(&tree);

    // Video (50 KB) outweighs the combined code files (20 KB), and each
    // category is named once regardless of how many tiles it covers.
    assert_eq!(
        tm.categories_present(),
        vec![FileCategory::Video, FileCategory::Code]
    );
}

#[test]
fn test_footer_keeps_keybindings_when_legend_does_not_fit() {
    use myd::screen::{ScreenState, SortMode};
    use myd::widget::file_tree::FileTree;
    use ratatui::{Terminal, backend::TestBackend};

    let dir = tempfile::tempdir().unwrap();
    for (name, sz) in [
        ("a.rs", 60_000usize),
        ("b.mp4", 50_000),
        ("c.png", 40_000),
        ("d.md", 30_000),
        ("e.json", 20_000),
        ("f.so", 10_000),
    ] {
        std::fs::write(dir.path().join(name), vec![0u8; sz]).unwrap();
    }
    let tree = FileTree::new(dir.path().to_path_buf(), SortMode::Largest, true, true);
    let mut st = myd::screen::MainScreenState::from_tree(dir.path().to_path_buf(), tree);
    st.focus = myd::widget::treemap::FocusTarget::Treemap;
    st.info_panel_hidden = true;

    for width in [40u16, 60, 80, 120] {
        let mut terminal = Terminal::new(TestBackend::new(width, 12)).unwrap();
        terminal.draw(|f| st.render(f, f.area())).unwrap();
        let buf = terminal.backend().buffer().clone();

        let mut footer = String::new();
        for x in 0..width {
            footer.push_str(buf[(x, 11)].symbol());
        }
        assert!(
            footer.contains("q:quit"),
            "keybindings must survive at width {}: {:?}",
            width,
            footer
        );
    }
}

// ---------------------------------------------------------------------------
// Info panel: content must track the focused view's selection.
// ---------------------------------------------------------------------------

/// Read the info panel (right 30% of the screen) as text.
fn info_panel_text(st: &mut myd::screen::MainScreenState, w: u16, h: u16) -> String {
    use myd::screen::ScreenState;
    use ratatui::{Terminal, backend::TestBackend};

    let mut terminal = Terminal::new(TestBackend::new(w, h)).unwrap();
    terminal.draw(|f| st.render(f, f.area())).unwrap();
    let buf = terminal.backend().buffer().clone();

    let start = w - w * 30 / 100;
    let mut out = String::new();
    for y in 0..h {
        let mut row = String::new();
        for x in start..w {
            row.push_str(buf[(x, y)].symbol());
        }
        out.push_str(row.trim_end());
        out.push('\n');
    }
    out
}

#[test]
fn test_info_panel_follows_focused_view_for_same_named_entries() {
    use myd::screen::SortMode;
    use myd::widget::file_tree::FileTree;
    use myd::widget::treemap::FocusTarget;

    // Two entries that share a basename but differ wildly in size. Keying the
    // info cache on the display name alone made one view show the other's text.
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("big/data")).unwrap();
    std::fs::write(dir.path().join("big/data/f.bin"), vec![0u8; 90_000]).unwrap();
    std::fs::create_dir_all(dir.path().join("small/data")).unwrap();
    std::fs::write(dir.path().join("small/data/f.bin"), vec![0u8; 100]).unwrap();

    let mut tree = FileTree::new(dir.path().to_path_buf(), SortMode::Largest, true, true);
    tree.expand_all();
    let mut st = myd::screen::MainScreenState::from_tree(dir.path().to_path_buf(), tree);
    st.info_panel_hidden = false;

    // Tree cursor -> big/data.
    st.focus = FocusTarget::Tree;
    let idx = st
        .tree
        .lines
        .iter()
        .position(|l| l.name == "data" && l.path.to_string_lossy().contains("big"))
        .expect("big/data line");
    st.tree.cursor = idx;
    let tree_info = info_panel_text(&mut st, 100, 20);
    assert!(
        tree_info.contains("87.9KB"),
        "tree panel should describe big/data: {}",
        tree_info
    );

    // Treemap cursor -> small/data: same name, different entry.
    st.focus = FocusTarget::Treemap;
    let ti = st
        .treemap_cells()
        .iter()
        .position(|c| c.label == "data" && c.path.to_string_lossy().contains("small"))
        .expect("small/data tile");
    st.set_treemap_cursor(ti);
    let tm_info = info_panel_text(&mut st, 100, 20);
    assert!(
        tm_info.contains("100 B"),
        "treemap panel should describe small/data, got: {}",
        tm_info
    );
    assert_ne!(
        tree_info, tm_info,
        "info panel must not reuse the other view's text"
    );

    // Switching back re-reads the tree's cursor.
    st.focus = FocusTarget::Tree;
    assert_eq!(info_panel_text(&mut st, 100, 20), tree_info);
}

#[test]
fn test_info_panel_updates_after_refresh() {
    use myd::screen::SortMode;
    use myd::widget::file_tree::FileTree;
    use myd::widget::treemap::FocusTarget;

    // The selected path is unchanged across a refresh, but its size is not —
    // the cache must not survive a rebuild of the underlying data.
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("grow")).unwrap();
    std::fs::write(dir.path().join("grow/a.bin"), vec![0u8; 1000]).unwrap();

    let tree = FileTree::new(dir.path().to_path_buf(), SortMode::Largest, true, true);
    let mut st = myd::screen::MainScreenState::from_tree(dir.path().to_path_buf(), tree);
    st.info_panel_hidden = false;
    st.focus = FocusTarget::Tree;
    st.tree.cursor = 1;

    let before = info_panel_text(&mut st, 100, 20);
    assert!(before.contains("1000 B"), "expected initial size: {}", before);

    std::fs::write(dir.path().join("grow/b.bin"), vec![0u8; 500_000]).unwrap();
    st.refresh();
    st.tree.cursor = 1;

    let after = info_panel_text(&mut st, 100, 20);
    assert!(
        !after.contains("1000 B"),
        "info panel still shows the pre-refresh size: {}",
        after
    );
    assert!(
        after.contains("489.3KB"),
        "info panel should show the grown size: {}",
        after
    );
}

#[test]
fn test_info_panel_visibility_persists_across_view_switches() {
    use myd::screen::SortMode;
    use myd::widget::file_tree::FileTree;
    use myd::widget::treemap::FocusTarget;

    let dir = sorting_fixture();
    let tree = FileTree::new(dir.path().to_path_buf(), SortMode::Largest, true, true);
    let mut st = myd::screen::MainScreenState::from_tree(dir.path().to_path_buf(), tree);

    // Hiding the panel in one view keeps it hidden in the other, and vice versa.
    st.info_panel_hidden = true;
    for focus in [FocusTarget::Treemap, FocusTarget::Tree, FocusTarget::Treemap] {
        st.focus = focus;
        let text = info_panel_text(&mut st, 100, 20);
        assert!(
            !text.contains("Info"),
            "panel should stay hidden in {:?}: {}",
            focus,
            text
        );
    }

    st.info_panel_hidden = false;
    for focus in [FocusTarget::Tree, FocusTarget::Treemap] {
        st.focus = focus;
        let text = info_panel_text(&mut st, 100, 20);
        assert!(
            text.contains("Info"),
            "panel should be visible in {:?}: {}",
            focus,
            text
        );
    }
}

// ---------------------------------------------------------------------------
// View preferences persist across directory navigation.
// ---------------------------------------------------------------------------

/// Drive the app until any pending Loading screen has resolved to a Main screen.
async fn settle(app: &mut myd::app::FileBrowser) {
    use myd::screen::Screen;
    for _ in 0..400 {
        app.resolve_loading_for_test();
        if matches!(app.current_screen(), Screen::Main(_)) {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    }
    panic!("loading screen never resolved");
}

fn char_key(c: char) -> crossterm::event::KeyEvent {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE)
}

fn ctrl_key(c: char) -> crossterm::event::KeyEvent {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL)
}

/// (info_panel_hidden, focus) of the current main screen.
fn view_state(app: &myd::app::FileBrowser) -> (bool, myd::widget::treemap::FocusTarget) {
    use myd::screen::Screen;
    match app.current_screen() {
        Screen::Main(s) => (s.info_panel_hidden, s.focus),
        _ => panic!("expected a main screen"),
    }
}

/// A root with a subdirectory to descend into.
fn nav_fixture() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("sub")).unwrap();
    std::fs::write(dir.path().join("sub/f.bin"), vec![0u8; 5_000]).unwrap();
    std::fs::write(dir.path().join("top.txt"), vec![0u8; 10]).unwrap();
    dir
}

#[tokio::test]
async fn test_info_panel_state_survives_entering_a_directory() {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use myd::app::FileBrowser;
    use myd::widget::treemap::FocusTarget;

    let dir = nav_fixture();
    let mut app = FileBrowser::new(Some(dir.path().to_path_buf()), None, false);
    settle(&mut app).await;
    // The pane starts closed.
    assert_eq!(view_state(&app), (true, FocusTarget::Tree));

    // Open the info panel, then descend into "sub".
    app.handle_key_for_test(ctrl_key('b'));
    assert!(!view_state(&app).0, "Ctrl+b should show the panel");

    app.handle_key_for_test(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    settle(&mut app).await;
    assert!(
        !view_state(&app).0,
        "info panel must stay open after entering a directory"
    );
}

#[tokio::test]
async fn test_view_focus_survives_entering_a_directory() {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use myd::app::FileBrowser;
    use myd::widget::treemap::FocusTarget;

    let dir = nav_fixture();
    let mut app = FileBrowser::new(Some(dir.path().to_path_buf()), None, false);
    settle(&mut app).await;

    // Switch to the treemap, then descend. Entering from the treemap should not
    // dump the user back into the tree view.
    app.handle_key_for_test(char_key('v'));
    assert_eq!(view_state(&app).1, FocusTarget::Treemap);

    app.handle_key_for_test(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    settle(&mut app).await;
    assert_eq!(
        view_state(&app).1,
        FocusTarget::Treemap,
        "treemap view must persist after entering a directory"
    );
}

#[tokio::test]
async fn test_both_view_prefs_survive_navigation_round_trip() {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use myd::app::FileBrowser;
    use myd::widget::treemap::FocusTarget;

    let dir = nav_fixture();
    let mut app = FileBrowser::new(Some(dir.path().to_path_buf()), None, false);
    settle(&mut app).await;

    app.handle_key_for_test(ctrl_key('b'));
    app.handle_key_for_test(char_key('v'));
    let want = (false, FocusTarget::Treemap);
    assert_eq!(view_state(&app), want);

    // Descend...
    app.handle_key_for_test(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    settle(&mut app).await;
    assert_eq!(view_state(&app), want, "prefs lost on the way down");

    // ...and back up with Ctrl-o.
    app.handle_key_for_test(KeyEvent::new(KeyCode::Char('o'), KeyModifiers::CONTROL));
    settle(&mut app).await;
    assert_eq!(view_state(&app), want, "prefs lost on the way back up");
}

#[tokio::test]
async fn test_toggling_prefs_back_also_persists() {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use myd::app::FileBrowser;
    use myd::widget::treemap::FocusTarget;

    // The preference is whatever the user last chose — including the default.
    let dir = nav_fixture();
    let mut app = FileBrowser::new(Some(dir.path().to_path_buf()), None, false);
    settle(&mut app).await;

    app.handle_key_for_test(ctrl_key('b')); // show
    app.handle_key_for_test(ctrl_key('b')); // hide again
    app.handle_key_for_test(char_key('v')); // treemap
    app.handle_key_for_test(char_key('v')); // back to tree

    app.handle_key_for_test(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    settle(&mut app).await;
    assert_eq!(
        view_state(&app),
        (true, FocusTarget::Tree),
        "re-toggled prefs must persist too"
    );
}

// ---------------------------------------------------------------------------
// Size cache reuse: drilling in should not rescan.
// ---------------------------------------------------------------------------

#[test]
fn test_size_cache_clone_shares_storage() {
    use myd::utils::sizes::SizeCache;

    // Cloning must share, not copy — a subtree tree holds a clone of its
    // parent's cache and both must see the same entries.
    let a = SizeCache::new();
    a.insert(std::path::Path::new("/x"), 1);
    let b = a.clone();
    b.insert(std::path::Path::new("/y"), 2);
    assert_eq!(a.len(), 2, "clone must share the parent's storage");
    assert_eq!(a.get(std::path::Path::new("/y")), Some(2));

    // Clearing through one handle invalidates the other (what refresh relies on).
    b.clear();
    assert_eq!(a.len(), 0);
}

#[test]
fn test_dir_scan_caches_every_descendant() {
    use myd::utils::sizes::{get_dir_size, get_dir_size_caching, SizeCache};

    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("a/b/c")).unwrap();
    std::fs::create_dir_all(dir.path().join("a/d")).unwrap();
    std::fs::write(dir.path().join("a/a1.bin"), vec![0u8; 100]).unwrap();
    std::fs::write(dir.path().join("a/b/b1.bin"), vec![0u8; 200]).unwrap();
    std::fs::write(dir.path().join("a/b/c/c1.bin"), vec![0u8; 400]).unwrap();
    std::fs::write(dir.path().join("a/d/d1.bin"), vec![0u8; 800]).unwrap();

    let cache = SizeCache::new();
    let total = get_dir_size_caching(&dir.path().join("a"), &cache);
    assert_eq!(total, 1500, "caching walk must total the same as a plain walk");
    assert_eq!(total, get_dir_size(&dir.path().join("a")));

    // Every nested directory is recorded with its own recursive size, so
    // opening one later is a cache hit rather than a second walk.
    for (rel, want) in [("a", 1500u64), ("a/b", 600), ("a/b/c", 400), ("a/d", 800)] {
        let p = dir.path().join(rel);
        assert_eq!(cache.get(&p), Some(want), "wrong cached size for {}", rel);
        assert_eq!(cache.get(&p), Some(get_dir_size(&p)), "cache disagrees for {}", rel);
    }
}

#[test]
fn test_opening_subdirectory_reuses_cached_sizes() {
    use myd::screen::SortMode;
    use myd::widget::file_tree::FileTree;

    let dir = tempfile::tempdir().unwrap();
    for d in 0..4 {
        let sub = dir.path().join(format!("d{}", d));
        std::fs::create_dir_all(sub.join("inner")).unwrap();
        for f in 0..20 {
            std::fs::write(sub.join(format!("f{}.bin", f)), vec![0u8; 512]).unwrap();
            std::fs::write(sub.join("inner").join(format!("g{}.bin", f)), vec![0u8; 512]).unwrap();
        }
    }

    let root = FileTree::new(dir.path().to_path_buf(), SortMode::Largest, true, true);
    let d0 = dir.path().join("d0");

    // The root scan must have measured d0's children, not just d0 itself.
    let child = d0.join("inner");
    assert!(
        root.size_cache.get(&child).is_some(),
        "scanning the root should have cached nested directories"
    );

    // Reusing the cache produces exactly the same tree as a cold scan.
    let warm = FileTree::with_cache(
        d0.clone(),
        SortMode::Largest,
        true,
        true,
        root.size_cache.clone(),
    );
    let cold = FileTree::new(d0, SortMode::Largest, true, true);

    let names = |t: &FileTree| -> Vec<(String, Option<u64>)> {
        t.lines
            .iter()
            .map(|l| (l.name.clone(), t.size_cache.get(&l.resolved_path)))
            .collect()
    };
    assert_eq!(
        names(&warm),
        names(&cold),
        "cached open must match a fresh scan in order and sizes"
    );
}

#[tokio::test]
async fn test_refresh_forces_a_rescan() {
    use myd::screen::{ScreenState, SortMode};
    use myd::widget::file_tree::FileTree;
    use ratatui::{Terminal, backend::TestBackend};

    // Sizes are cached, so a file added behind the app's back stays invisible
    // until the user asks for a rescan with `r`.
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("grow")).unwrap();
    std::fs::write(dir.path().join("grow/a.bin"), vec![0u8; 1000]).unwrap();

    let tree = FileTree::new(dir.path().to_path_buf(), SortMode::Largest, true, true);
    let mut st = myd::screen::MainScreenState::from_tree(dir.path().to_path_buf(), tree);
    st.info_panel_hidden = false;
    st.tree.cursor = 1;

    let mut terminal = Terminal::new(TestBackend::new(100, 20)).unwrap();
    terminal.draw(|f| st.render(f, f.area())).unwrap();

    std::fs::write(dir.path().join("grow/b.bin"), vec![0u8; 500_000]).unwrap();

    st.refresh();
    st.tree.cursor = 1;
    let after = info_panel_text(&mut st, 100, 20);
    assert!(
        after.contains("489.3KB"),
        "refresh must re-read sizes from disk: {}",
        after
    );
}

// ---------------------------------------------------------------------------
// Treemap: full name in the footer when the tile truncates it.
// ---------------------------------------------------------------------------

/// Build a treemap screen whose tiles have deliberately long names.
fn long_name_screen() -> (tempfile::TempDir, myd::screen::MainScreenState) {
    let dir = tempfile::tempdir().unwrap();
    for (n, sz) in [
        ("a_very_long_directory_name_here", 90_000usize),
        ("another_extremely_long_name_xyz", 40_000),
        ("short", 20_000),
        ("medium_length_name", 10_000),
    ] {
        std::fs::create_dir_all(dir.path().join(n)).unwrap();
        std::fs::write(dir.path().join(n).join("f.bin"), vec![0u8; sz]).unwrap();
    }
    let tree = myd::widget::file_tree::FileTree::new(
        dir.path().to_path_buf(),
        myd::screen::SortMode::Largest,
        true,
        true,
    );
    let mut st = myd::screen::MainScreenState::from_tree(dir.path().to_path_buf(), tree);
    st.focus = myd::widget::treemap::FocusTarget::Treemap;
    (dir, st)
}

fn footer_text(st: &mut myd::screen::MainScreenState, w: u16, h: u16) -> String {
    use myd::screen::ScreenState;
    use ratatui::{Terminal, backend::TestBackend};

    let mut terminal = Terminal::new(TestBackend::new(w, h)).unwrap();
    terminal.draw(|f| st.render(f, f.area())).unwrap();
    let buf = terminal.backend().buffer().clone();
    let mut footer = String::new();
    for x in 0..w {
        footer.push_str(buf[(x, h - 1)].symbol());
    }
    footer
}

#[test]
fn test_footer_shows_name_only_when_tile_truncates_it() {
    let (_dir, mut st) = long_name_screen();

    // Find a tile whose label does not fit, and one that does.
    let footer_wide = footer_text(&mut st, 80, 14);
    assert!(
        footer_wide.contains("hjkl:move"),
        "keybindings must always be present: {}",
        footer_wide
    );

    let mut checked_truncated = false;
    let mut checked_fitting = false;
    for i in 0..st.treemap_cells().len() {
        st.set_treemap_cursor(i);
        let footer = footer_text(&mut st, 80, 14);
        let cell = &st.treemap_cells()[i];
        let label = cell.label.clone();
        let inner = cell.rect.width.saturating_sub(2) as usize;

        if inner == 0 || label.chars().count() > inner {
            assert!(
                footer.contains(&label),
                "truncated tile {:?} (inner width {}) should appear in footer: {}",
                label,
                inner,
                footer
            );
            checked_truncated = true;
        } else {
            assert!(
                !footer.contains(&label),
                "tile {:?} fits in its box, footer should not repeat it: {}",
                label,
                footer
            );
            checked_fitting = true;
        }
    }
    assert!(
        checked_truncated && checked_fitting,
        "fixture should exercise both a truncated and a fitting tile"
    );
}

#[test]
fn test_footer_name_never_crowds_out_keybindings() {
    let (_dir, mut st) = long_name_screen();

    // Select the tile with the longest name, then squeeze the terminal.
    let longest = st
        .treemap_cells()
        .iter()
        .enumerate()
        .max_by_key(|(_, c)| c.label.len())
        .map(|(i, _)| i)
        .unwrap();
    st.set_treemap_cursor(longest);

    for width in [40u16, 50, 60, 70, 100] {
        let footer = footer_text(&mut st, width, 14);
        assert!(
            footer.contains("q:quit"),
            "keybindings must survive at width {}: {:?}",
            width,
            footer
        );
        // Whatever is drawn must fit on the line.
        assert!(
            footer.chars().count() == width as usize,
            "footer overflowed at width {}",
            width
        );
    }
}

// ---------------------------------------------------------------------------
// q quits immediately; h in the treemap mirrors the tree.
// ---------------------------------------------------------------------------

/// A root with a subdirectory that itself holds several entries (so its
/// treemap has multiple tiles).
fn deep_nav_fixture() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let sub = dir.path().join("sub");
    for (n, sz) in [("aaa", 90_000usize), ("bbb", 40_000), ("ccc", 10_000)] {
        std::fs::create_dir_all(sub.join(n)).unwrap();
        std::fs::write(sub.join(n).join("f.bin"), vec![0u8; sz]).unwrap();
    }
    dir
}

#[tokio::test]
async fn test_q_quits_immediately_from_a_subdirectory() {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use myd::app::FileBrowser;

    // Enter a subdirectory, then press q. It should quit outright, not pop back
    // to the parent screen.
    let dir = nav_fixture();
    let mut app = FileBrowser::new(Some(dir.path().to_path_buf()), None, false);
    settle(&mut app).await;
    assert_eq!(app.screen_stack_depth(), 1);

    app.handle_key_for_test(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    settle(&mut app).await;
    assert_eq!(app.screen_stack_depth(), 2, "should have descended into sub");

    let keep_running = app.handle_key_for_test(char_key('q'));
    assert!(!keep_running, "q must quit the app, not pop a screen");
}

#[tokio::test]
async fn test_ctrl_o_still_pops_back_up() {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use myd::app::FileBrowser;

    // The going-back behavior q used to have now lives solely on Ctrl-o.
    let dir = nav_fixture();
    let mut app = FileBrowser::new(Some(dir.path().to_path_buf()), None, false);
    settle(&mut app).await;

    app.handle_key_for_test(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    settle(&mut app).await;
    assert_eq!(app.screen_stack_depth(), 2);

    let keep_running =
        app.handle_key_for_test(KeyEvent::new(KeyCode::Char('o'), KeyModifiers::CONTROL));
    assert!(keep_running, "Ctrl-o should not quit");
    assert_eq!(app.screen_stack_depth(), 1, "Ctrl-o should pop back up");
}

#[tokio::test]
async fn test_treemap_h_moves_left_then_steps_up_at_edge() {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use myd::app::FileBrowser;
    use myd::screen::Screen;
    use myd::widget::treemap::FocusTarget;

    let dir = deep_nav_fixture();
    let mut app = FileBrowser::new(Some(dir.path().to_path_buf()), None, false);
    settle(&mut app).await;

    // Move onto "sub" (the root line is selected initially), descend into it,
    // then switch to the treemap.
    app.handle_key_for_test(char_key('j'));
    app.handle_key_for_test(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    settle(&mut app).await;
    assert_eq!(app.screen_stack_depth(), 2);
    app.handle_key_for_test(char_key('v'));
    assert_eq!(view_state(&app).1, FocusTarget::Treemap);

    // A frame must render at least once so the treemap has laid out its tiles
    // (navigation reads the rects produced by rendering).
    render_once(&mut app);

    // Move to the rightmost tile so there is somewhere to move left from.
    let tile_count = match app.current_screen() {
        Screen::Main(s) => s.treemap_cells().len(),
        _ => unreachable!(),
    };
    assert!(tile_count >= 2, "fixture should produce multiple tiles");
    for _ in 0..tile_count {
        app.handle_key_for_test(char_key('l'));
    }
    let start = selected_name(&app);

    // h moves the cursor left while there is a tile to the left; the stack does
    // not change.
    let mut moved = false;
    for _ in 0..tile_count {
        if !treemap_can_move_left(&app) {
            break;
        }
        let before = selected_name(&app);
        app.handle_key_for_test(char_key('h'));
        assert_eq!(app.screen_stack_depth(), 2, "h should not pop while moving left");
        assert_ne!(selected_name(&app), before, "h should move the cursor left");
        moved = true;
    }
    assert!(moved, "should have moved left at least once from {}", start);

    // Now on the left-edge tile: h steps up to the parent directory.
    assert!(!treemap_can_move_left(&app), "cursor should be at the left edge");
    let keep_running = app.handle_key_for_test(char_key('h'));
    assert!(keep_running, "h should not quit");
    assert_eq!(
        app.screen_stack_depth(),
        1,
        "h on a left-edge tile should step up to the parent directory"
    );
}

fn render_once(app: &mut myd::app::FileBrowser) {
    use myd::screen::ScreenState;
    use ratatui::{Terminal, backend::TestBackend};
    let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
    // Render the current screen so the treemap lays out its tiles.
    if let myd::screen::Screen::Main(s) = app.current_screen_mut() {
        terminal.draw(|f| s.render(f, f.area())).unwrap();
    }
}

fn selected_name(app: &myd::app::FileBrowser) -> String {
    use myd::screen::Screen;
    match app.current_screen() {
        Screen::Main(s) => s
            .selected_path()
            .and_then(|p| p.file_name())
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default(),
        _ => String::new(),
    }
}

fn treemap_can_move_left(app: &myd::app::FileBrowser) -> bool {
    use myd::screen::Screen;
    match app.current_screen() {
        Screen::Main(s) => s.treemap_can_move_left(),
        _ => false,
    }
}

#[tokio::test]
async fn test_treemap_stays_in_treemap_when_stepping_up_with_h() {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use myd::app::FileBrowser;
    use myd::widget::treemap::FocusTarget;

    // Descend in TREE view, switch to the treemap only in the child, then step
    // up with h. The parent screen must be shown in treemap view, not reset to
    // the tree it was in when we descended.
    let dir = deep_nav_fixture();
    let mut app = FileBrowser::new(Some(dir.path().to_path_buf()), None, false);
    settle(&mut app).await;
    assert_eq!(view_state(&app).1, FocusTarget::Tree, "starts in tree view");

    // Onto "sub", descend (still tree view).
    app.handle_key_for_test(char_key('j'));
    app.handle_key_for_test(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    settle(&mut app).await;
    assert_eq!(app.screen_stack_depth(), 2);
    assert_eq!(view_state(&app).1, FocusTarget::Tree);

    // Switch to the treemap in the child only.
    app.handle_key_for_test(char_key('v'));
    assert_eq!(view_state(&app).1, FocusTarget::Treemap);
    render_once(&mut app);

    // Walk to the left edge, then h steps up to the parent.
    for _ in 0..10 {
        if !treemap_can_move_left(&app) {
            break;
        }
        app.handle_key_for_test(char_key('h'));
    }
    assert!(!treemap_can_move_left(&app));
    app.handle_key_for_test(char_key('h'));

    assert_eq!(app.screen_stack_depth(), 1, "h should step up to the parent");
    assert_eq!(
        view_state(&app).1,
        FocusTarget::Treemap,
        "stepping up must keep the treemap view, not revert to tree"
    );
}

#[tokio::test]
async fn test_ctrl_o_also_keeps_treemap_view() {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use myd::app::FileBrowser;
    use myd::widget::treemap::FocusTarget;

    let dir = deep_nav_fixture();
    let mut app = FileBrowser::new(Some(dir.path().to_path_buf()), None, false);
    settle(&mut app).await;

    app.handle_key_for_test(char_key('j'));
    app.handle_key_for_test(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    settle(&mut app).await;
    app.handle_key_for_test(char_key('v'));
    assert_eq!(view_state(&app).1, FocusTarget::Treemap);

    app.handle_key_for_test(KeyEvent::new(KeyCode::Char('o'), KeyModifiers::CONTROL));
    assert_eq!(app.screen_stack_depth(), 1);
    assert_eq!(
        view_state(&app).1,
        FocusTarget::Treemap,
        "Ctrl-o must also keep the treemap view"
    );
}

// ---------------------------------------------------------------------------
// Cancelling a directory scan with q / Esc.
// ---------------------------------------------------------------------------

/// A directory large enough that its initial scan takes long enough to be
/// cancelled mid-flight in a test.
fn big_scan_fixture() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    for d in 0..40 {
        let sub = dir.path().join(format!("d{:03}", d));
        std::fs::create_dir_all(sub.join("inner")).unwrap();
        for f in 0..200 {
            std::fs::write(sub.join(format!("f{}.bin", f)), vec![0u8; 256]).unwrap();
            std::fs::write(sub.join("inner").join(format!("g{}.bin", f)), vec![0u8; 256]).unwrap();
        }
    }
    dir
}

/// Drive the app until the loading screen resolves, returning whether the app
/// should keep running.
async fn drain_loading(app: &mut myd::app::FileBrowser) -> bool {
    for _ in 0..2000 {
        if !app.resolve_loading_for_test() {
            return false;
        }
        if !matches!(app.current_screen(), myd::screen::Screen::Loading(_)) {
            return true;
        }
        tokio::time::sleep(std::time::Duration::from_millis(2)).await;
    }
    panic!("loading never resolved");
}

#[tokio::test]
async fn test_q_cancels_scan_and_quits_when_first_screen() {
    use myd::app::FileBrowser;
    use myd::screen::Screen;

    let dir = big_scan_fixture();
    let mut app = FileBrowser::new(Some(dir.path().to_path_buf()), None, false);

    // The very first screen is the loading scan. Cancelling it quits the app,
    // since there is nothing behind it to return to.
    assert!(
        matches!(app.current_screen(), Screen::Loading(_)),
        "app should start in a loading screen"
    );

    // Still scanning (the fixture is large enough that it cannot have finished
    // in the moment since construction).
    assert!(
        matches!(app.current_screen(), Screen::Loading(_)),
        "scan should still be in progress"
    );

    // q while loading cancels rather than quitting outright.
    let keep_running = app.handle_key_for_test(char_key('q'));
    assert!(keep_running, "q during a scan should not quit instantly");

    // The cancelled scan resolves to a quit.
    let still_running = drain_loading(&mut app).await;
    assert!(
        !still_running,
        "cancelling the first-screen scan should quit the app"
    );
}

#[tokio::test]
async fn test_q_cancels_scan_and_returns_to_parent_when_drilled_in() {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use myd::app::FileBrowser;
    use myd::screen::Screen;

    // Root has one big subdirectory. Enter it, then cancel its scan: we should
    // land back on the parent directory, not quit.
    let dir = tempfile::tempdir().unwrap();
    let sub = dir.path().join("sub");
    for d in 0..40 {
        let s = sub.join(format!("d{:03}", d));
        std::fs::create_dir_all(&s).unwrap();
        for f in 0..200 {
            std::fs::write(s.join(format!("f{}.bin", f)), vec![0u8; 256]).unwrap();
        }
    }

    let mut app = FileBrowser::new(Some(dir.path().to_path_buf()), None, false);
    // Settle the (small) root scan.
    assert!(drain_loading(&mut app).await);
    assert_eq!(app.screen_stack_depth(), 1);

    // Onto "sub" and descend — pushes a loading screen.
    app.handle_key_for_test(char_key('j'));
    app.handle_key_for_test(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    assert!(
        matches!(app.current_screen(), Screen::Loading(_)),
        "descending should push a loading screen"
    );
    assert_eq!(app.screen_stack_depth(), 2);

    // Cancel the subdirectory scan.
    let keep_running = app.handle_key_for_test(char_key('q'));
    assert!(keep_running, "q during a scan should not quit");

    let still_running = drain_loading(&mut app).await;
    assert!(still_running, "cancelling a drilled-in scan should not quit");
    assert_eq!(
        app.screen_stack_depth(),
        1,
        "cancelling should return to the parent directory"
    );
    assert!(matches!(app.current_screen(), Screen::Main(_)));
}

#[tokio::test]
async fn test_scan_completes_normally_without_cancel() {
    use myd::app::FileBrowser;
    use myd::screen::Screen;

    // Sanity: with no cancel, a scan resolves into a Main screen as before.
    let dir = nav_fixture();
    let mut app = FileBrowser::new(Some(dir.path().to_path_buf()), None, false);
    assert!(drain_loading(&mut app).await);
    assert!(matches!(app.current_screen(), Screen::Main(_)));
    assert_eq!(app.screen_stack_depth(), 1);
}

// ---------------------------------------------------------------------------
// The help screen: dismissal keys close help without acting on the app.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_q_in_help_closes_help_without_quitting() {
    use myd::app::FileBrowser;

    let dir = nav_fixture();
    let mut app = FileBrowser::new(Some(dir.path().to_path_buf()), None, false);
    settle(&mut app).await;

    // Open help with '?'.
    app.handle_key_for_test(char_key('?'));
    assert!(app.is_help_open(), "'?' should open the help screen");

    // q closes help and must NOT quit the app.
    let keep_running = app.handle_key_for_test(char_key('q'));
    assert!(keep_running, "q in help must not quit the app");
    assert!(!app.is_help_open(), "q should close the help screen");
}

#[tokio::test]
async fn test_esc_and_toggles_close_help_without_acting() {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use myd::app::FileBrowser;

    // Esc, ?, and F1 each close help and are consumed (no side effect).
    for close_key in [
        KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE),
        KeyEvent::new(KeyCode::Char('?'), KeyModifiers::NONE),
        KeyEvent::new(KeyCode::F(1), KeyModifiers::NONE),
    ] {
        let dir = nav_fixture();
        let mut app = FileBrowser::new(Some(dir.path().to_path_buf()), None, false);
        settle(&mut app).await;

        app.handle_key_for_test(char_key('?'));
        assert!(app.is_help_open());

        let keep_running = app.handle_key_for_test(close_key);
        assert!(keep_running, "{:?} in help must not quit", close_key.code);
        assert!(!app.is_help_open(), "{:?} should close help", close_key.code);
    }
}

#[tokio::test]
async fn test_other_key_in_help_dismisses_and_acts() {
    use myd::app::FileBrowser;
    use myd::screen::Screen;

    // A non-dismissal key (j) both closes help and moves the cursor, so help
    // isn't a dead end for ordinary navigation.
    let dir = nav_fixture();
    let mut app = FileBrowser::new(Some(dir.path().to_path_buf()), None, false);
    settle(&mut app).await;

    let cursor_before = match app.current_screen() {
        Screen::Main(s) => s.tree.cursor,
        _ => panic!("expected main screen"),
    };

    app.handle_key_for_test(char_key('?'));
    assert!(app.is_help_open());

    let keep_running = app.handle_key_for_test(char_key('j'));
    assert!(keep_running);
    assert!(!app.is_help_open(), "j should dismiss help");
    let cursor_after = match app.current_screen() {
        Screen::Main(s) => s.tree.cursor,
        _ => panic!("expected main screen"),
    };
    assert_eq!(
        cursor_after,
        cursor_before + 1,
        "j should also move the cursor down after dismissing help"
    );
}

// ---------------------------------------------------------------------------
// Dual-panel mode: split, switch, and cross-panel copy.
// ---------------------------------------------------------------------------

/// Drive the app until every panel is rooted at a directory (a Main screen).
/// Only valid when all panels were given real directory paths.
async fn settle_all(app: &mut myd::app::FileBrowser) {
    for _ in 0..400 {
        app.resolve_loading_for_test();
        if (0..app.panel_count()).all(|i| app.panel_current_dir(i).is_some()) {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    }
    panic!("dual-panel loading never resolved");
}

fn key(code: crossterm::event::KeyCode) -> crossterm::event::KeyEvent {
    use crossterm::event::{KeyEvent, KeyModifiers};
    KeyEvent::new(code, KeyModifiers::NONE)
}

#[tokio::test]
async fn test_two_positional_paths_start_dual() {
    use myd::app::FileBrowser;

    let left = tempfile::tempdir().unwrap();
    let right = tempfile::tempdir().unwrap();
    let mut app = FileBrowser::new(
        Some(left.path().to_path_buf()),
        Some(right.path().to_path_buf()),
        false,
    );
    settle_all(&mut app).await;

    assert_eq!(app.panel_count(), 2, "two paths should open two panels");
    assert_eq!(app.active_panel_index(), 0, "left panel starts active");
    assert_eq!(
        app.panel_current_dir(0).unwrap().canonicalize().unwrap(),
        left.path().canonicalize().unwrap()
    );
    assert_eq!(
        app.panel_current_dir(1).unwrap().canonicalize().unwrap(),
        right.path().canonicalize().unwrap()
    );
}

#[tokio::test]
async fn test_dual_flag_starts_split_single_path() {
    use myd::app::FileBrowser;

    let left = tempfile::tempdir().unwrap();
    let mut app =
        FileBrowser::new(Some(left.path().to_path_buf()), None, true);
    // The left panel loads its directory; the right opens a dir picker (no
    // path), so only the active panel is expected to settle to a Main screen.
    settle(&mut app).await;
    assert_eq!(app.panel_count(), 2, "--dual should open two panels");
    assert!(
        app.panel_current_dir(0).is_some(),
        "left panel is rooted at the given directory"
    );
    assert!(
        app.panel_current_dir(1).is_none(),
        "right panel opens a directory picker when no path is given"
    );
}

#[tokio::test]
async fn test_tab_switches_active_panel() {
    use myd::app::FileBrowser;

    let left = tempfile::tempdir().unwrap();
    let right = tempfile::tempdir().unwrap();
    let mut app = FileBrowser::new(
        Some(left.path().to_path_buf()),
        Some(right.path().to_path_buf()),
        false,
    );
    settle_all(&mut app).await;

    assert_eq!(app.active_panel_index(), 0);
    app.handle_key_for_test(key(crossterm::event::KeyCode::Tab));
    assert_eq!(app.active_panel_index(), 1, "Tab moves to right panel");
    app.handle_key_for_test(key(crossterm::event::KeyCode::Tab));
    assert_eq!(app.active_panel_index(), 0, "Tab moves back to left");
}

#[tokio::test]
async fn test_tab_is_noop_in_single_panel() {
    use myd::app::FileBrowser;

    let dir = tempfile::tempdir().unwrap();
    let mut app = FileBrowser::new(Some(dir.path().to_path_buf()), None, false);
    settle(&mut app).await;
    app.handle_key_for_test(key(crossterm::event::KeyCode::Tab));
    assert_eq!(app.panel_count(), 1);
    assert_eq!(app.active_panel_index(), 0);
}

#[tokio::test]
async fn test_pipe_toggles_split_and_back() {
    use myd::app::FileBrowser;

    let dir = tempfile::tempdir().unwrap();
    let mut app = FileBrowser::new(Some(dir.path().to_path_buf()), None, false);
    settle(&mut app).await;
    assert_eq!(app.panel_count(), 1);

    // Split: opens a second panel rooted at the active panel's dir.
    app.handle_key_for_test(char_key('|'));
    assert_eq!(app.panel_count(), 2, "| should split into two panels");
    settle_all(&mut app).await;
    assert_eq!(
        app.active_panel_index(),
        1,
        "the new panel becomes active on split"
    );

    // Unsplit: drops the inactive panel, keeps the active one.
    app.handle_key_for_test(char_key('|'));
    assert_eq!(app.panel_count(), 1, "| should collapse back to one panel");
    assert_eq!(app.active_panel_index(), 0);
}

#[tokio::test]
async fn test_copy_places_file_in_other_panel_dir() {
    use myd::app::FileBrowser;

    let left = tempfile::tempdir().unwrap();
    let right = tempfile::tempdir().unwrap();
    std::fs::write(left.path().join("payload.bin"), vec![7u8; 128]).unwrap();
    let mut app = FileBrowser::new(
        Some(left.path().to_path_buf()),
        Some(right.path().to_path_buf()),
        false,
    );
    settle_all(&mut app).await;

    // Move the cursor onto the file in the active (left) panel. The root line
    // is index 0; the first child is the file.
    app.handle_key_for_test(char_key('j'));
    app.handle_key_for_test(char_key('c'));

    // Let the background copy finish and the destination panel refresh.
    for _ in 0..200 {
        app.resolve_loading_for_test();
        if right.path().join("payload.bin").exists() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    }
    assert!(
        right.path().join("payload.bin").exists(),
        "c should copy the selected file into the right panel's directory"
    );
}

#[tokio::test]
async fn test_copy_collision_prompts_confirm() {
    use myd::app::FileBrowser;
    use myd::screen::Screen;

    let left = tempfile::tempdir().unwrap();
    let right = tempfile::tempdir().unwrap();
    std::fs::write(left.path().join("dup.txt"), b"new").unwrap();
    std::fs::write(right.path().join("dup.txt"), b"old").unwrap();
    let mut app = FileBrowser::new(
        Some(left.path().to_path_buf()),
        Some(right.path().to_path_buf()),
        false,
    );
    settle_all(&mut app).await;

    app.handle_key_for_test(char_key('j')); // onto dup.txt
    app.handle_key_for_test(char_key('c')); // collision -> confirm modal

    // The destination file is unchanged until we confirm.
    assert_eq!(
        std::fs::read(right.path().join("dup.txt")).unwrap(),
        b"old",
        "copy must not overwrite before confirmation"
    );

    // Confirm overwrite with 'y', then let the copy complete.
    app.handle_key_for_test(char_key('y'));
    for _ in 0..200 {
        app.resolve_loading_for_test();
        if std::fs::read(right.path().join("dup.txt")).unwrap() == b"new" {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    }
    assert_eq!(
        std::fs::read(right.path().join("dup.txt")).unwrap(),
        b"new",
        "confirming overwrite should replace the destination file"
    );

    // Sanity: we're back to a normal main screen with no modal blocking.
    assert!(matches!(app.current_screen(), Screen::Main(_)));
}

#[tokio::test]
async fn test_copy_is_noop_in_single_panel() {
    use myd::app::FileBrowser;

    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("only.txt"), b"x").unwrap();
    let mut app = FileBrowser::new(Some(dir.path().to_path_buf()), None, false);
    settle(&mut app).await;
    app.handle_key_for_test(char_key('j'));
    // Should not panic or create anything; simply nothing to copy into.
    app.handle_key_for_test(char_key('c'));
    assert_eq!(app.panel_count(), 1);
}

// ---------------------------------------------------------------------------
// Multi-file operations: tagging, visual mode, filter, tagged copy.
// ---------------------------------------------------------------------------

/// Number of tagged paths on the active panel's screen.
fn tag_count(app: &myd::app::FileBrowser) -> usize {
    app.current_screen().tagged_paths().len()
}

/// A flat directory with several files to tag/filter.
fn flat_files_fixture() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("alpha.log"), b"a").unwrap();
    std::fs::write(dir.path().join("beta.log"), b"b").unwrap();
    std::fs::write(dir.path().join("gamma.txt"), b"g").unwrap();
    dir
}

#[tokio::test]
async fn test_t_tags_and_untags_file() {
    use myd::app::FileBrowser;

    let dir = flat_files_fixture();
    let mut app = FileBrowser::new(Some(dir.path().to_path_buf()), None, false);
    settle(&mut app).await;

    // Move onto the first child and tag it.
    app.handle_key_for_test(char_key('j'));
    app.handle_key_for_test(char_key('t'));
    assert_eq!(tag_count(&app), 1, "t should tag the file under the cursor");

    // t again on the same file untags it.
    app.handle_key_for_test(char_key('t'));
    assert_eq!(tag_count(&app), 0, "t again should untag it");
}

#[tokio::test]
async fn test_shift_u_untags_all() {
    use myd::app::FileBrowser;

    let dir = flat_files_fixture();
    let mut app = FileBrowser::new(Some(dir.path().to_path_buf()), None, false);
    settle(&mut app).await;

    app.handle_key_for_test(char_key('j'));
    app.handle_key_for_test(char_key('t'));
    app.handle_key_for_test(char_key('j'));
    app.handle_key_for_test(char_key('t'));
    assert_eq!(tag_count(&app), 2);

    app.handle_key_for_test(char_key('U'));
    assert_eq!(tag_count(&app), 0, "U should clear every tag");
}

#[tokio::test]
async fn test_visual_mode_sweep_tags_range() {
    use myd::app::FileBrowser;

    let dir = flat_files_fixture();
    let mut app = FileBrowser::new(Some(dir.path().to_path_buf()), None, false);
    settle(&mut app).await;

    // Anchor on the first child, sweep down over the other two.
    app.handle_key_for_test(char_key('j'));
    app.handle_key_for_test(char_key('V'));
    app.handle_key_for_test(char_key('j'));
    app.handle_key_for_test(char_key('j'));
    assert_eq!(tag_count(&app), 3, "visual sweep should tag the whole range");

    // A non-motion action exits visual mode but keeps the tags.
    app.handle_key_for_test(char_key('s')); // toggle sort — not a motion
    assert_eq!(tag_count(&app), 3, "tags persist after leaving visual mode");
}

#[tokio::test]
async fn test_filter_narrows_then_clears() {
    use myd::app::FileBrowser;
    use myd::screen::Screen;

    let dir = flat_files_fixture();
    let mut app = FileBrowser::new(Some(dir.path().to_path_buf()), None, false);
    settle(&mut app).await;

    let line_count = |app: &FileBrowser| match app.current_screen() {
        Screen::Main(s) => s.tree.lines.len(),
        _ => 0,
    };
    let before = line_count(&app);
    assert_eq!(before, 4, "root + 3 files");

    // f -> type a regex matching only the .log files -> Enter.
    app.handle_key_for_test(char_key('f'));
    for c in r"\.log$".chars() {
        app.handle_key_for_test(char_key(c));
    }
    app.handle_key_for_test(key(crossterm::event::KeyCode::Enter));
    assert_eq!(line_count(&app), 3, "root + 2 matching .log files");

    // f -> empty pattern -> Enter clears the filter.
    app.handle_key_for_test(char_key('f'));
    app.handle_key_for_test(key(crossterm::event::KeyCode::Enter));
    assert_eq!(line_count(&app), before, "empty pattern restores full view");
}

#[tokio::test]
async fn test_dual_panel_copies_tagged_files() {
    use myd::app::FileBrowser;

    let left = tempfile::tempdir().unwrap();
    let right = tempfile::tempdir().unwrap();
    // Distinct sizes so the default Largest sort gives a deterministic order:
    // one.bin, two.bin, skip.bin. The first two get tagged.
    std::fs::write(left.path().join("one.bin"), vec![1u8; 300]).unwrap();
    std::fs::write(left.path().join("two.bin"), vec![2u8; 200]).unwrap();
    std::fs::write(left.path().join("skip.bin"), vec![3u8; 100]).unwrap();
    let mut app = FileBrowser::new(
        Some(left.path().to_path_buf()),
        Some(right.path().to_path_buf()),
        false,
    );
    settle_all(&mut app).await;

    // Tag two of the three files in the active (left) panel.
    app.handle_key_for_test(char_key('j'));
    app.handle_key_for_test(char_key('t'));
    app.handle_key_for_test(char_key('j'));
    app.handle_key_for_test(char_key('t'));
    assert_eq!(tag_count(&app), 2);

    // Copy the tagged set into the right panel.
    app.handle_key_for_test(char_key('c'));
    for _ in 0..200 {
        app.resolve_loading_for_test();
        let both = right.path().join("one.bin").exists()
            && right.path().join("two.bin").exists();
        if both {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    }

    // Exactly the two tagged files landed; the untagged one did not.
    let names: std::collections::HashSet<String> = std::fs::read_dir(right.path())
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().to_string())
        .collect();
    assert!(names.contains("one.bin") && names.contains("two.bin"));
    assert!(!names.contains("skip.bin"), "untagged file must not copy");

    // Tags are cleared once the copy completes.
    assert_eq!(tag_count(&app), 0, "copy consumes the tags");
}

#[tokio::test]
async fn test_tagged_copy_collision_prompts_then_overwrites() {
    use myd::app::FileBrowser;

    let left = tempfile::tempdir().unwrap();
    let right = tempfile::tempdir().unwrap();
    std::fs::write(left.path().join("dup.bin"), b"new").unwrap();
    std::fs::write(right.path().join("dup.bin"), b"old").unwrap();
    let mut app = FileBrowser::new(
        Some(left.path().to_path_buf()),
        Some(right.path().to_path_buf()),
        false,
    );
    settle_all(&mut app).await;

    app.handle_key_for_test(char_key('j'));
    app.handle_key_for_test(char_key('t')); // tag dup.bin
    app.handle_key_for_test(char_key('c')); // collision -> confirm

    // Not overwritten until confirmed.
    assert_eq!(std::fs::read(right.path().join("dup.bin")).unwrap(), b"old");

    app.handle_key_for_test(char_key('y'));
    for _ in 0..200 {
        app.resolve_loading_for_test();
        if std::fs::read(right.path().join("dup.bin")).unwrap() == b"new" {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    }
    assert_eq!(
        std::fs::read(right.path().join("dup.bin")).unwrap(),
        b"new",
        "confirming overwrite replaces the destination"
    );
}

#[tokio::test]
async fn test_single_panel_copy_prompts_for_destination() {
    use myd::app::FileBrowser;

    let dir = tempfile::tempdir().unwrap();
    let dest = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("movable.bin"), vec![9u8; 16]).unwrap();
    let mut app = FileBrowser::new(Some(dir.path().to_path_buf()), None, false);
    settle(&mut app).await;

    app.handle_key_for_test(char_key('j'));
    app.handle_key_for_test(char_key('t')); // tag it
    app.handle_key_for_test(char_key('c')); // single-panel -> dest prompt

    // Type the destination directory and confirm.
    for c in dest.path().to_string_lossy().chars() {
        app.handle_key_for_test(char_key(c));
    }
    app.handle_key_for_test(key(crossterm::event::KeyCode::Enter));

    for _ in 0..200 {
        app.resolve_loading_for_test();
        if dest.path().join("movable.bin").exists() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    }
    assert!(
        dest.path().join("movable.bin").exists(),
        "single-panel copy should place the file in the prompted directory"
    );
}

#[tokio::test]
async fn test_copy_reproduces_deep_directory_structure() {
    use myd::app::FileBrowser;

    // Build a nested tree under "payload/":
    //   payload/top.txt
    //   payload/sub/inner.txt
    //   payload/sub/deep/leaf.bin
    //   payload/empty/            (an empty directory)
    let left = tempfile::tempdir().unwrap();
    let right = tempfile::tempdir().unwrap();
    let payload = left.path().join("payload");
    std::fs::create_dir_all(payload.join("sub/deep")).unwrap();
    std::fs::create_dir_all(payload.join("empty")).unwrap();
    std::fs::write(payload.join("top.txt"), b"top").unwrap();
    std::fs::write(payload.join("sub/inner.txt"), b"inner").unwrap();
    std::fs::write(payload.join("sub/deep/leaf.bin"), vec![7u8; 64]).unwrap();

    let mut app = FileBrowser::new(
        Some(left.path().to_path_buf()),
        Some(right.path().to_path_buf()),
        false,
    );
    settle_all(&mut app).await;

    // Cursor onto "payload" (the only child), tag it, copy to the right panel.
    app.handle_key_for_test(char_key('j'));
    app.handle_key_for_test(char_key('t'));
    app.handle_key_for_test(char_key('c'));

    let copied_root = right.path().join("payload");
    for _ in 0..300 {
        app.resolve_loading_for_test();
        if copied_root.join("sub/deep/leaf.bin").exists() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    }

    // Every file is reproduced with its content.
    assert_eq!(std::fs::read(copied_root.join("top.txt")).unwrap(), b"top");
    assert_eq!(
        std::fs::read(copied_root.join("sub/inner.txt")).unwrap(),
        b"inner"
    );
    assert_eq!(
        std::fs::read(copied_root.join("sub/deep/leaf.bin")).unwrap(),
        vec![7u8; 64]
    );
    // Directories (including the empty one) are reproduced.
    assert!(copied_root.join("sub/deep").is_dir());
    assert!(
        copied_root.join("empty").is_dir(),
        "an empty subdirectory should be recreated by the copy"
    );
}

#[tokio::test]
async fn test_split_reuses_cache_without_rescan() {
    use myd::app::FileBrowser;
    use myd::screen::Screen;

    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("a")).unwrap();
    std::fs::write(dir.path().join("a/f1.bin"), vec![0u8; 1000]).unwrap();
    std::fs::write(dir.path().join("top.txt"), b"hi").unwrap();

    let mut app = FileBrowser::new(Some(dir.path().to_path_buf()), None, false);
    settle(&mut app).await;

    // Snapshot the active panel's populated cache handle.
    let (cache_len_before, cache_handle) = match app.current_screen() {
        Screen::Main(s) => (s.tree.size_cache.len(), s.tree.size_cache.clone()),
        _ => panic!("expected main screen"),
    };
    assert!(cache_len_before > 0, "root scan should have cached sizes");

    // Split. The new pane opens on the same directory.
    app.handle_key_for_test(char_key('|'));
    assert_eq!(app.panel_count(), 2);
    settle_all(&mut app).await;

    // The new (now active) pane must share the SAME underlying cache map, so its
    // build was cache hits, not a fresh rescan. Cloned SizeCache handles compare
    // equal by pointer via Arc; assert the shared map has not shrunk and that
    // the new tree's cache observes entries inserted through the old handle.
    match app.current_screen() {
        Screen::Main(s) => {
            assert!(
                s.tree.size_cache.len() >= cache_len_before,
                "split pane should reuse the cache, not rebuild an empty one"
            );
            // Insert through the original handle; the new pane sees it → same map.
            let probe = std::path::PathBuf::from("/__cache_probe__");
            cache_handle.insert(&probe, 42);
            assert_eq!(
                s.tree.size_cache.get(&probe),
                Some(42),
                "both panes must share one size cache after a split"
            );
        }
        _ => panic!("expected main screen after split"),
    }
}

// ---------------------------------------------------------------------------
// Issue fixes: h-at-root pops immediately; delete operates on tags.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_h_at_root_pops_back_immediately() {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use myd::app::FileBrowser;

    // Enter a subdirectory, then a single `h` on the (auto-expanded) root line
    // should step back up — not merely collapse the root, which used to require
    // a second press.
    let dir = nav_fixture();
    let mut app = FileBrowser::new(Some(dir.path().to_path_buf()), None, false);
    settle(&mut app).await;

    // Move onto "sub" and descend.
    app.handle_key_for_test(char_key('j'));
    app.handle_key_for_test(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    settle(&mut app).await;
    assert_eq!(app.screen_stack_depth(), 2, "descended into sub");

    // A single h pops back to the parent.
    let keep_running = app.handle_key_for_test(char_key('h'));
    assert!(keep_running);
    assert_eq!(
        app.screen_stack_depth(),
        1,
        "the first h at the root should pop back up"
    );
}

#[tokio::test]
async fn test_delete_operates_on_tagged_files() {
    use myd::app::FileBrowser;

    let dir = tempfile::tempdir().unwrap();
    // Distinct sizes for a deterministic Largest sort: big, mid, small.
    std::fs::write(dir.path().join("big.bin"), vec![0u8; 3000]).unwrap();
    std::fs::write(dir.path().join("mid.bin"), vec![0u8; 2000]).unwrap();
    std::fs::write(dir.path().join("small.bin"), vec![0u8; 1000]).unwrap();
    let mut app = FileBrowser::new(Some(dir.path().to_path_buf()), None, false);
    settle(&mut app).await;

    // Tag the two largest (big, mid).
    app.handle_key_for_test(char_key('j'));
    app.handle_key_for_test(char_key('t'));
    app.handle_key_for_test(char_key('j'));
    app.handle_key_for_test(char_key('t'));

    // Delete → confirm with 'y'.
    app.handle_key_for_test(char_key('D'));
    app.handle_key_for_test(char_key('y'));

    for _ in 0..200 {
        app.resolve_loading_for_test();
        if !dir.path().join("big.bin").exists() && !dir.path().join("mid.bin").exists() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    }

    assert!(!dir.path().join("big.bin").exists(), "tagged file deleted");
    assert!(!dir.path().join("mid.bin").exists(), "tagged file deleted");
    assert!(
        dir.path().join("small.bin").exists(),
        "untagged file must survive the delete"
    );
    // Tags cleared after the delete.
    assert_eq!(tag_count(&app), 0);
}

#[tokio::test]
async fn test_delete_falls_back_to_cursor_when_no_tags() {
    use myd::app::FileBrowser;

    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("only.bin"), vec![0u8; 500]).unwrap();
    std::fs::write(dir.path().join("keep.bin"), vec![0u8; 400]).unwrap();
    let mut app = FileBrowser::new(Some(dir.path().to_path_buf()), None, false);
    settle(&mut app).await;

    // No tags; cursor onto the first child, delete it.
    app.handle_key_for_test(char_key('j'));
    app.handle_key_for_test(char_key('D'));
    app.handle_key_for_test(char_key('y'));

    for _ in 0..200 {
        app.resolve_loading_for_test();
        if !dir.path().join("only.bin").exists() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    }
    assert!(!dir.path().join("only.bin").exists(), "cursor file deleted");
    assert!(dir.path().join("keep.bin").exists(), "other file untouched");
}

// ---------------------------------------------------------------------------
// Progress overlays render live counts (issues 3 & 4).
// ---------------------------------------------------------------------------

/// Flatten a TestBackend buffer to a single string for substring assertions.
fn buffer_to_string(buf: &ratatui::buffer::Buffer) -> String {
    let mut s = String::new();
    for y in 0..buf.area.height {
        for x in 0..buf.area.width {
            s.push_str(buf[(x, y)].symbol());
        }
        s.push('\n');
    }
    s
}

#[test]
fn test_operation_overlay_shows_item_counts() {
    use myd::widget::progress::{OpProgress, ProgressOverlay};
    use ratatui::{backend::TestBackend, Terminal};

    let progress = OpProgress::new();
    progress.set_total(10);
    for _ in 0..3 {
        progress.inc_done();
    }

    let mut terminal = Terminal::new(TestBackend::new(60, 12)).unwrap();
    terminal
        .draw(|f| {
            let overlay = ProgressOverlay::for_operation("Copying", &progress);
            overlay.render(f, f.area());
        })
        .unwrap();

    let text = buffer_to_string(&terminal.backend().buffer().clone());
    assert!(text.contains("Copying"), "overlay shows the verb:\n{}", text);
    assert!(
        text.contains("3 / 10 items"),
        "overlay shows the running count:\n{}",
        text
    );
}

#[test]
fn test_scan_overlay_shows_files_dirs_and_size() {
    use myd::widget::progress::{OpProgress, ProgressOverlay};
    use ratatui::{backend::TestBackend, Terminal};

    let progress = OpProgress::new();
    progress.add_dir();
    progress.add_file(1024);
    progress.add_file(1024);

    let mut terminal = Terminal::new(TestBackend::new(60, 12)).unwrap();
    terminal
        .draw(|f| {
            let overlay = ProgressOverlay::for_scan(&progress);
            overlay.render(f, f.area());
        })
        .unwrap();

    let text = buffer_to_string(&terminal.backend().buffer().clone());
    assert!(text.contains("2 files"), "scan shows file count:\n{}", text);
    assert!(text.contains("1 dirs"), "scan shows dir count:\n{}", text);
    // 2048 bytes → "2.0 KB" (or similar) — assert the KB unit shows up.
    assert!(
        text.contains("KB") || text.contains("2048"),
        "scan shows combined size:\n{}",
        text
    );
}

// ---------------------------------------------------------------------------
// N creates a directory; search uses regex; tagged rows render a marker.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_n_creates_directory_in_current_pane() {
    use myd::app::FileBrowser;

    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("existing.txt"), b"x").unwrap();
    let mut app = FileBrowser::new(Some(dir.path().to_path_buf()), None, false);
    settle(&mut app).await;

    // N -> type a name -> Enter.
    app.handle_key_for_test(char_key('N'));
    for c in "newdir".chars() {
        app.handle_key_for_test(char_key(c));
    }
    app.handle_key_for_test(key(crossterm::event::KeyCode::Enter));

    assert!(
        dir.path().join("newdir").is_dir(),
        "N should create the directory in the pane's current dir"
    );
}

#[tokio::test]
async fn test_n_creates_inside_directory_under_cursor() {
    use myd::app::FileBrowser;

    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("parent")).unwrap();
    let mut app = FileBrowser::new(Some(dir.path().to_path_buf()), None, false);
    settle(&mut app).await;

    // Cursor onto "parent" (a directory), then create inside it.
    app.handle_key_for_test(char_key('j'));
    app.handle_key_for_test(char_key('N'));
    for c in "child".chars() {
        app.handle_key_for_test(char_key(c));
    }
    app.handle_key_for_test(key(crossterm::event::KeyCode::Enter));

    assert!(
        dir.path().join("parent/child").is_dir(),
        "N with the cursor on a directory should create inside it"
    );
}

#[tokio::test]
async fn test_search_uses_regex() {
    use myd::app::FileBrowser;
    use myd::screen::Screen;

    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("alpha.txt"), b"x").unwrap();
    std::fs::write(dir.path().join("report2024.log"), b"x").unwrap();
    std::fs::write(dir.path().join("notes.md"), b"x").unwrap();
    let mut app = FileBrowser::new(Some(dir.path().to_path_buf()), None, false);
    settle(&mut app).await;

    // A regex that only the .log file matches (digits before .log).
    app.handle_key_for_test(char_key('/'));
    for c in r"\d+\.log$".chars() {
        app.handle_key_for_test(char_key(c));
    }
    app.handle_key_for_test(key(crossterm::event::KeyCode::Enter));

    // The cursor should land on the matching file.
    let name = match app.current_screen() {
        Screen::Main(s) => s
            .tree
            .selected_line()
            .map(|l| l.name.clone())
            .unwrap_or_default(),
        _ => String::new(),
    };
    assert_eq!(name, "report2024.log", "regex search should find the .log file");
}

#[tokio::test]
async fn test_tagged_row_renders_marker() {
    use myd::app::FileBrowser;
    use myd::screen::Screen;
    use ratatui::{backend::TestBackend, Terminal};

    let dir = flat_files_fixture();
    let mut app = FileBrowser::new(Some(dir.path().to_path_buf()), None, false);
    settle(&mut app).await;

    // Tag the first child, then move the cursor off it so the tag marker isn't
    // hidden by the cursor row.
    app.handle_key_for_test(char_key('j'));
    app.handle_key_for_test(char_key('t'));
    app.handle_key_for_test(char_key('j'));
    assert_eq!(tag_count(&app), 1);

    let mut terminal = Terminal::new(TestBackend::new(80, 12)).unwrap();
    terminal
        .draw(|f| {
            if let Screen::Main(s) = app.current_screen_mut() {
                s.active = true;
                myd::screen::ScreenState::render(s, f, f.area());
            }
        })
        .unwrap();

    let text = buffer_to_string(&terminal.backend().buffer().clone());
    assert!(
        text.contains('▶'),
        "a tagged row should show the ▶ marker:\n{}",
        text
    );
}

// ---------------------------------------------------------------------------
// n / p step through search matches.
// ---------------------------------------------------------------------------

/// Name of the line under the cursor on the active panel.
fn cursor_name(app: &myd::app::FileBrowser) -> String {
    use myd::screen::Screen;
    match app.current_screen() {
        Screen::Main(s) => s
            .tree
            .selected_line()
            .map(|l| l.name.clone())
            .unwrap_or_default(),
        _ => String::new(),
    }
}

#[tokio::test]
async fn test_n_and_p_step_through_matches() {
    use myd::app::FileBrowser;

    // Three .log files of distinct sizes so the Largest sort is deterministic:
    // a.log (biggest), b.log, c.log, plus a non-matching file.
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("a.log"), vec![0u8; 3000]).unwrap();
    std::fs::write(dir.path().join("b.log"), vec![0u8; 2000]).unwrap();
    std::fs::write(dir.path().join("c.log"), vec![0u8; 1000]).unwrap();
    std::fs::write(dir.path().join("skip.txt"), vec![0u8; 500]).unwrap();
    let mut app = FileBrowser::new(Some(dir.path().to_path_buf()), None, false);
    settle(&mut app).await;

    // Search lands on the first match (a.log, the largest → first child).
    app.handle_key_for_test(char_key('/'));
    for c in r"\.log$".chars() {
        app.handle_key_for_test(char_key(c));
    }
    app.handle_key_for_test(key(crossterm::event::KeyCode::Enter));
    assert_eq!(cursor_name(&app), "a.log", "search lands on first match");

    // n advances down the matches.
    app.handle_key_for_test(char_key('n'));
    assert_eq!(cursor_name(&app), "b.log", "n -> next match");
    app.handle_key_for_test(char_key('n'));
    assert_eq!(cursor_name(&app), "c.log", "n -> next match");

    // n wraps around back to the first match.
    app.handle_key_for_test(char_key('n'));
    assert_eq!(cursor_name(&app), "a.log", "n wraps to the top");

    // p walks back up (wrapping to the bottom first).
    app.handle_key_for_test(char_key('p'));
    assert_eq!(cursor_name(&app), "c.log", "p wraps to the bottom");
    app.handle_key_for_test(char_key('p'));
    assert_eq!(cursor_name(&app), "b.log", "p -> previous match");
}

#[tokio::test]
async fn test_n_is_noop_without_prior_search() {
    use myd::app::FileBrowser;

    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("only.txt"), b"x").unwrap();
    let mut app = FileBrowser::new(Some(dir.path().to_path_buf()), None, false);
    settle(&mut app).await;

    // No search yet: n / p must not move the cursor or panic.
    let before = cursor_name(&app);
    app.handle_key_for_test(char_key('n'));
    app.handle_key_for_test(char_key('p'));
    assert_eq!(cursor_name(&app), before, "n/p do nothing before a search");
}

#[tokio::test]
async fn test_create_dir_preserves_size_cache() {
    use myd::app::FileBrowser;
    use myd::screen::Screen;

    // A tree with content so the initial scan populates the size cache.
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("a/deep")).unwrap();
    std::fs::write(dir.path().join("a/deep/f.bin"), vec![0u8; 4000]).unwrap();
    std::fs::write(dir.path().join("top.txt"), vec![0u8; 100]).unwrap();
    let mut app = FileBrowser::new(Some(dir.path().to_path_buf()), None, false);
    settle(&mut app).await;

    let cache_before = match app.current_screen() {
        Screen::Main(s) => s.tree.size_cache.len(),
        _ => 0,
    };
    assert!(cache_before > 0);

    // Create a directory. The old code called refresh(), which clears the cache
    // and rescans the whole tree; the fast path keeps the cache intact.
    app.handle_key_for_test(char_key('N'));
    for c in "fresh".chars() {
        app.handle_key_for_test(char_key(c));
    }
    app.handle_key_for_test(key(crossterm::event::KeyCode::Enter));

    assert!(dir.path().join("fresh").is_dir(), "directory created");
    let cache_after = match app.current_screen() {
        Screen::Main(s) => s.tree.size_cache.len(),
        _ => 0,
    };
    assert!(
        cache_after >= cache_before,
        "create-dir must not clear the size cache (was {}, now {})",
        cache_before,
        cache_after
    );
}
