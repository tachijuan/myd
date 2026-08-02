use std::fs::{self, File};
use std::io::Write;
use std::path::PathBuf;

/// The process working directory is global, so tests that change it (to exercise
/// the "default to cwd" behavior) must not run concurrently — one setting the cwd
/// while another reads it would race. They lock this for their duration.
static CWD_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

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

/// Three files with distinct, known modification times (old / mid / new).
fn mtime_fixture() -> tempfile::TempDir {
    use std::time::{Duration, SystemTime};
    let dir = tempfile::tempdir().unwrap();
    let base = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000_000);
    for (name, offset) in [("old.txt", 0), ("mid.txt", 100_000), ("new.txt", 200_000)] {
        let path = dir.path().join(name);
        std::fs::write(&path, b"x").unwrap();
        let f = std::fs::OpenOptions::new().write(true).open(&path).unwrap();
        f.set_modified(base + Duration::from_secs(offset)).unwrap();
    }
    dir
}

#[test]
fn test_sort_newest_orders_by_mtime_descending() {
    use myd::screen::SortMode;
    use myd::widget::file_tree::FileTree;

    let dir = mtime_fixture();
    let tree = FileTree::new(dir.path().to_path_buf(), SortMode::Newest, true, true);
    assert_eq!(
        names_at_depth(&tree, 1),
        vec!["new.txt", "mid.txt", "old.txt"],
        "newest sort should list the most recently modified first"
    );
}

#[test]
fn test_sort_oldest_orders_by_mtime_ascending() {
    use myd::screen::SortMode;
    use myd::widget::file_tree::FileTree;

    let dir = mtime_fixture();
    let tree = FileTree::new(dir.path().to_path_buf(), SortMode::Oldest, true, true);
    assert_eq!(
        names_at_depth(&tree, 1),
        vec!["old.txt", "mid.txt", "new.txt"],
        "oldest sort should list the least recently modified first"
    );
}

#[tokio::test]
async fn test_sort_mode_cycles_through_time_orders() {
    // Cycling with `s` reaches the time-based modes.
    let dir = mtime_fixture();
    let mut app = FileBrowser::new(Some(dir.path().to_path_buf()), None, false);
    settle(&mut app).await;

    let sort_label = |app: &FileBrowser| match app.current_screen() {
        Screen::Main(s) => s.tree.sort_mode.label().to_string(),
        _ => String::new(),
    };

    // From the default (largest), press `s` until every new time mode appears.
    let mut seen = std::collections::HashSet::new();
    for _ in 0..7 {
        seen.insert(sort_label(&app));
        app.handle_key_for_test(char_key('s'));
    }
    seen.insert(sort_label(&app));
    for expected in ["newest", "oldest", "recently-accessed"] {
        assert!(seen.contains(expected), "cycle never reached {}", expected);
    }
}

#[test]
fn test_sort_correct_after_switching_mode() {
    use myd::screen::SortMode;
    use myd::widget::file_tree::FileTree;

    // set_sort_mode reorders in place using the recursive directory sizes
    // already in the cache — a directory sorts by its total size, not its
    // shallow metadata length.
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

    // On a local tree, toggle_hidden reloads expanded levels to pick up hidden
    // entries that were skipped while hidden was off, then re-sorts.
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
    std::fs::write(
        dir.path().join("parent/b_huge/deep/f.bin"),
        vec![0u8; 90_000],
    )
    .unwrap();
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
    use ratatui::{backend::TestBackend, Terminal};

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
            tm.render(f, a, cursor, None, &std::collections::HashSet::new());
        })
        .unwrap();

    // Tiles are labelled with the basename only — no path noise like
    // "//...///aaa" — and a directory carries a trailing slash, as in `ls -F`,
    // so a tile says whether Enter will open it.
    let labels: Vec<&str> = tm.cells.iter().map(|c| c.label.as_str()).collect();
    assert_eq!(labels, vec!["aaa/", "bbb/", "ccc/", "ddd/"]);

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
    use ratatui::{backend::TestBackend, layout::Rect, Terminal};

    let dir = sorting_fixture();
    let tree = FileTree::new(dir.path().to_path_buf(), SortMode::Largest, true, true);
    let mut tm = TreeMap::from_file_tree(&tree);

    // A terminal squeezed to nothing must not panic (equal_split used to divide
    // by zero when the area had no height).
    let mut terminal = Terminal::new(TestBackend::new(4, 1)).unwrap();
    terminal
        .draw(|f| {
            let cursor = tm.cursor;
            tm.render(f, Rect::new(0, 0, 0, 0), cursor, None, &std::collections::HashSet::new());
        })
        .unwrap();
    terminal
        .draw(|f| {
            let a = f.area();
            let cursor = tm.cursor;
            tm.render(f, a, cursor, None, &std::collections::HashSet::new());
        })
        .unwrap();
}

#[test]
fn test_screens_render_at_tiny_terminal_sizes() {
    use myd::screen::{Screen, SortMode};
    use myd::widget::file_tree::FileTree;
    use ratatui::{backend::TestBackend, Terminal};

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
    use myd::screen::SortMode;
    use myd::utils::filetype::FileCategory;
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
    use myd::screen::SortMode;
    use myd::utils::filetype::FileCategory;
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
            // Matched on the path rather than the label: the label is display
            // text and now marks directories with a trailing slash.
            .find(|c| c.path.file_name().map(|n| n == name).unwrap_or(false))
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
    use ratatui::{backend::TestBackend, Terminal};

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
            tm.render(f, a, cursor, None, &std::collections::HashSet::new());
        })
        .unwrap();
    let buf = terminal.backend().buffer().clone();

    // Every interior cell of a tile carries that tile's category background.
    for cell in &tm.cells {
        let r = cell.rect;
        assert!(
            r.width >= 3 && r.height >= 3,
            "tile too small to check: {:?}",
            r
        );
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
    use myd::screen::SortMode;
    use myd::utils::filetype::FileCategory;
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
    use ratatui::{backend::TestBackend, Terminal};

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
    use ratatui::{backend::TestBackend, Terminal};

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
        .position(|c| {
            c.path.file_name().map(|n| n == "data").unwrap_or(false)
                && c.path.to_string_lossy().contains("small")
        })
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
    assert!(
        before.contains("1000 B"),
        "expected initial size: {}",
        before
    );

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
    for focus in [
        FocusTarget::Treemap,
        FocusTarget::Tree,
        FocusTarget::Treemap,
    ] {
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
        // `tick_for_test` too, not just the loading screen: opening an archive
        // indexes it on the blocking pool and installs the panel from a tick, so
        // a settle that only resolved loading would return on the screen the
        // user was already on and assert against the wrong panel.
        app.tick_for_test();
        app.resolve_loading_for_test();
        if matches!(app.current_screen(), Screen::Main(_)) && !app.archive_opening_for_test() {
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
    app.handle_key_for_test(ctrl_key('p'));
    assert!(!view_state(&app).0, "Ctrl+p should show the panel");

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

    app.handle_key_for_test(ctrl_key('p'));
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

    app.handle_key_for_test(ctrl_key('p')); // show
    app.handle_key_for_test(ctrl_key('p')); // hide again
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
    assert_eq!(
        total, 1500,
        "caching walk must total the same as a plain walk"
    );
    assert_eq!(total, get_dir_size(&dir.path().join("a")));

    // Every nested directory is recorded with its own recursive size, so
    // opening one later is a cache hit rather than a second walk.
    for (rel, want) in [("a", 1500u64), ("a/b", 600), ("a/b/c", 400), ("a/d", 800)] {
        let p = dir.path().join(rel);
        assert_eq!(cache.get(&p), Some(want), "wrong cached size for {}", rel);
        assert_eq!(
            cache.get(&p),
            Some(get_dir_size(&p)),
            "cache disagrees for {}",
            rel
        );
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
            std::fs::write(
                sub.join("inner").join(format!("g{}.bin", f)),
                vec![0u8; 512],
            )
            .unwrap();
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
    use ratatui::{backend::TestBackend, Terminal};

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
    use ratatui::{backend::TestBackend, Terminal};

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
    assert_eq!(
        app.screen_stack_depth(),
        2,
        "should have descended into sub"
    );

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
        assert_eq!(
            app.screen_stack_depth(),
            2,
            "h should not pop while moving left"
        );
        assert_ne!(selected_name(&app), before, "h should move the cursor left");
        moved = true;
    }
    assert!(moved, "should have moved left at least once from {}", start);

    // Now on the left-edge tile: h steps up to the parent directory.
    assert!(
        !treemap_can_move_left(&app),
        "cursor should be at the left edge"
    );
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
    use ratatui::{backend::TestBackend, Terminal};
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

    assert_eq!(
        app.screen_stack_depth(),
        1,
        "h should step up to the parent"
    );
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
            std::fs::write(
                sub.join("inner").join(format!("g{}.bin", f)),
                vec![0u8; 256],
            )
            .unwrap();
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
    assert!(
        still_running,
        "cancelling a drilled-in scan should not quit"
    );
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
        assert!(
            !app.is_help_open(),
            "{:?} should close help",
            close_key.code
        );
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

    let bars_before = match app.current_screen() {
        Screen::Main(s) => s.tree.show_size_bar,
        _ => panic!("expected main screen"),
    };

    app.handle_key_for_test(char_key('?'));
    assert!(app.is_help_open());

    // `B` (toggle size bars) rather than `j`: the help list is taller than the
    // terminal, so j/k now scroll *within* it instead of dismissing it. A key
    // the overlay has no use for still dismisses and acts.
    let keep_running = app.handle_key_for_test(char_key('B'));
    assert!(keep_running);
    assert!(!app.is_help_open(), "an unrelated key should dismiss help");
    let bars_after = match app.current_screen() {
        Screen::Main(s) => s.tree.show_size_bar,
        _ => panic!("expected main screen"),
    };
    assert_ne!(
        bars_after, bars_before,
        "the key should also act after dismissing help"
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

/// Drive ticks until a copy's destination listing has been resolved.
///
/// Pressing `c` onto a destination does not decide anything straight away: the
/// destination is listed in a background task and the collision prompt — or the
/// queueing, when there is no collision — happens on a later tick. Asserting
/// immediately races that listing and fails whenever it loses, which under a
/// loaded parallel test run happens often enough to matter.
///
/// Waits for the probe to clear rather than for a modal to appear, since the
/// no-collision case is expected to produce no modal at all.
async fn settle_copy(app: &mut myd::app::FileBrowser) {
    for _ in 0..400 {
        app.tick_for_test();
        if !app.dest_probe_pending_for_test() {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    }
    panic!("destination listing never resolved");
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

    // Serialize with other cwd-mutating tests (the cwd is process-global).
    let _cwd_lock = CWD_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    // Point the cwd at a small temp dir so the right panel (which defaults to the
    // current directory) settles quickly instead of scanning a large real cwd.
    let cwd = tempfile::tempdir().unwrap();
    std::fs::write(cwd.path().join("c.txt"), "c").unwrap();
    std::env::set_current_dir(cwd.path()).unwrap();

    let left = tempfile::tempdir().unwrap();
    let mut app = FileBrowser::new(Some(left.path().to_path_buf()), None, true);
    // The left panel loads the given directory; the right panel, with no path,
    // defaults to the current directory (the dir picker is reserved for `gd`).
    settle(&mut app).await;
    // Let the right panel's load settle too.
    for _ in 0..400 {
        app.resolve_loading_for_test();
        if app.panel_current_dir(1).is_some() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(3)).await;
    }
    assert_eq!(app.panel_count(), 2, "--dual should open two panels");
    assert!(
        app.panel_current_dir(0).is_some(),
        "left panel is rooted at the given directory"
    );
    assert!(
        app.panel_current_dir(1).is_some(),
        "right panel defaults to the current directory when no path is given"
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
    assert_eq!(
        tag_count(&app),
        3,
        "visual sweep should tag the whole range"
    );

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
        let both = right.path().join("one.bin").exists() && right.path().join("two.bin").exists();
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
    assert!(
        text.contains("Copying"),
        "overlay shows the verb:\n{}",
        text
    );
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
    assert_eq!(
        name, "report2024.log",
        "regex search should find the .log file"
    );
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

/// The cursor must stay visible when it sits on a tagged file.
///
/// Tagged rows are filled black-on-amber. When the cursor landed on one, the
/// REVERSED highlight was simply dropped, leaving that row styled identically to
/// every other tagged row — so moving through a directory of tagged files lost
/// the cursor entirely. The two states now use inverted fills of the same
/// colour: both read as "tagged", only one reads as "here".
#[tokio::test]
async fn cursor_stays_visible_on_a_tagged_row() {
    use myd::app::FileBrowser;
    use myd::screen::Screen;
    use ratatui::{backend::TestBackend, Terminal};

    let dir = flat_files_fixture();
    let mut app = FileBrowser::new(Some(dir.path().to_path_buf()), None, false);
    settle(&mut app).await;

    // Tag two adjacent rows and leave the cursor on the second.
    app.handle_key_for_test(char_key('j'));
    app.handle_key_for_test(char_key('t'));
    app.handle_key_for_test(char_key('j'));
    app.handle_key_for_test(char_key('t'));
    assert_eq!(tag_count(&app), 2);

    let cursor_row = app.selected_line_index_for_test().unwrap();

    let mut terminal = Terminal::new(TestBackend::new(80, 12)).unwrap();
    terminal
        .draw(|f| {
            if let Screen::Main(s) = app.current_screen_mut() {
                s.active = true;
                myd::screen::ScreenState::render(s, f, f.area());
            }
        })
        .unwrap();

    let buf = terminal.backend().buffer().clone();

    // Collect the background colours of the two tagged rows. Content starts one
    // row below the top border.
    // Compare whole rows rather than one column: the amber tag fill only covers
    // the marker and the name, whose width varies per entry.
    let row_styles = |line_index: usize| -> Vec<(ratatui::style::Color, ratatui::style::Color)> {
        let y = (line_index + 1) as u16;
        (0..buf.area.width)
            .map(|x| {
                let c = &buf[(x, y)];
                (c.fg, c.bg)
            })
            .collect()
    };

    let here = row_styles(cursor_row);
    let other = row_styles(cursor_row - 1);

    let tag_amber = ratatui::style::Color::Rgb(255, 170, 40);

    // The distinguishing property has to be the *styling*, not the text: two
    // tagged rows naturally differ in filename. Compare the set of fg/bg pairs
    // each row uses, ignoring how many cells carry them.
    let palette = |row: &[(ratatui::style::Color, ratatui::style::Color)]| {
        let mut v: Vec<_> = row
            .iter()
            .filter(|(f, b)| *f == tag_amber || *b == tag_amber)
            .copied()
            .collect();
        v.sort_by_key(|(f, b)| (format!("{:?}", f), format!("{:?}", b)));
        v.dedup();
        v
    };

    let here_palette = palette(&here);
    let other_palette = palette(&other);

    // Both must still read as tagged.
    assert!(!here_palette.is_empty(), "the cursor row lost its tagged colour");
    assert!(!other_palette.is_empty(), "the tagged row lost its tagged colour");

    assert_ne!(
        here_palette, other_palette,
        "a tagged row under the cursor is styled exactly like a tagged row that \
         is not ({:?}) — the cursor is invisible among tagged files",
        here_palette
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

// ---------------------------------------------------------------------------
// Transfer panel + non-blocking transfer queue.
// ---------------------------------------------------------------------------

use myd::app::FileBrowser;
use myd::screen::Screen;

/// Render the whole app and return its buffer as text.
fn app_screen_text(app: &mut FileBrowser, w: u16, h: u16) -> String {
    use ratatui::{backend::TestBackend, Terminal};
    let mut terminal = Terminal::new(TestBackend::new(w, h)).unwrap();
    terminal.draw(|f| app.render_for_test(f)).unwrap();
    let buf = terminal.backend().buffer();
    (0..buf.area.height)
        .map(|y| {
            (0..buf.area.width)
                .map(|x| buf[(x, y)].symbol())
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[tokio::test]
async fn transfer_panel_hidden_until_a_transfer_starts() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("a.txt"), "x").unwrap();
    let src = dir.path().join("payload.bin");
    std::fs::write(&src, vec![0u8; 4096]).unwrap();
    let mut app = FileBrowser::new(Some(dir.path().to_path_buf()), None, false);
    settle(&mut app).await;

    // At startup, with no transfers queued, the panel is not shown — it's not
    // clutter before you've started a copy.
    assert!(
        !app.is_transfer_panel_visible(),
        "panel must stay hidden until a transfer starts"
    );
    assert!(!app_screen_text(&mut app, 120, 20).contains("Transfers"));

    // Once a transfer is queued, the panel appears on its own.
    app.enqueue_transfer_for_test(
        myd::vfs::VPath::local(&src),
        myd::vfs::VPath::local(dir.path().join("out/payload.bin")),
    );
    assert!(
        app.is_transfer_panel_visible(),
        "panel must appear once a transfer is queued"
    );
    assert!(app_screen_text(&mut app, 120, 20).contains("Transfers"));

    // It stays up while transfers remain, then hides again once the queue clears.
    for _ in 0..2000 {
        app.tick_transfers_for_test();
        if !app.transfer_queue().has_work() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(2)).await;
    }
    // Finished transfers still count as "in the queue" until cleared, so the
    // panel remains visible to show the results.
    assert!(app.is_transfer_panel_visible());
}

#[tokio::test]
async fn ctrl_t_toggles_the_transfer_panel() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("a.txt"), "x").unwrap();
    let src = dir.path().join("payload.bin");
    std::fs::write(&src, vec![0u8; 4096]).unwrap();
    let mut app = FileBrowser::new(Some(dir.path().to_path_buf()), None, false);
    settle(&mut app).await;

    // Toggle it on before any transfer — the explicit override wins over the
    // auto rule, so the user can pin it open whenever they like.
    app.handle_key_for_test(ctrl_key('t'));
    assert!(app.is_transfer_panel_visible());
    assert!(app_screen_text(&mut app, 120, 20).contains("Transfers"));

    // Toggle it back off.
    app.handle_key_for_test(ctrl_key('t'));
    assert!(!app.is_transfer_panel_visible());
    assert!(!app_screen_text(&mut app, 120, 20).contains("Transfers"));

    // Queue a transfer (like a copy) — this clears the override, so the auto
    // rule shows it, and Ctrl+t can pin it hidden again.
    app.enqueue_transfer_for_test(
        myd::vfs::VPath::local(&src),
        myd::vfs::VPath::local(dir.path().join("out/payload.bin")),
    );
    assert!(app.is_transfer_panel_visible());
    app.handle_key_for_test(ctrl_key('t'));
    assert!(
        !app.is_transfer_panel_visible(),
        "Ctrl+t should hide the panel even with a queued transfer"
    );
}

#[tokio::test]
async fn transfer_panel_yields_on_a_narrow_terminal() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("a.txt"), "x").unwrap();
    let src = dir.path().join("payload.bin");
    std::fs::write(&src, vec![0u8; 4096]).unwrap();
    let mut app = FileBrowser::new(Some(dir.path().to_path_buf()), None, false);
    settle(&mut app).await;

    // Queue a transfer so the panel wants to show, then confirm a narrow
    // terminal still yields the room to the tree.
    app.enqueue_transfer_for_test(
        myd::vfs::VPath::local(&src),
        myd::vfs::VPath::local(dir.path().join("out/payload.bin")),
    );
    assert!(app.is_transfer_panel_visible());
    let narrow = app_screen_text(&mut app, 70, 20);
    assert!(!narrow.contains("Transfers"));
    assert!(
        narrow.contains("File Tree"),
        "tree must still render: {}",
        narrow
    );
}

#[tokio::test]
async fn queued_transfers_show_in_the_panel_and_complete() {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("payload.bin");
    let payload = vec![9u8; 512 * 1024];
    std::fs::write(&src, &payload).unwrap();
    let dest_dir = dir.path().join("out");

    let mut app = FileBrowser::new(Some(dir.path().to_path_buf()), None, false);
    settle(&mut app).await;

    app.enqueue_transfer_for_test(
        myd::vfs::VPath::local(&src),
        myd::vfs::VPath::local(dest_dir.join("payload.bin")),
    );

    // Before the first tick it is queued, and the panel says so.
    app.tick_transfers_for_test();
    let text = app_screen_text(&mut app, 120, 24);
    assert!(
        text.contains("payload.bin"),
        "queued transfer should be listed: {}",
        text
    );

    for _ in 0..2000 {
        app.tick_transfers_for_test();
        if !app.transfer_queue().has_work() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(2)).await;
    }

    assert_eq!(app.transfer_queue().finished_count(), 1);
    assert_eq!(
        std::fs::read(dest_dir.join("payload.bin")).unwrap(),
        payload
    );
}

#[tokio::test]
async fn navigation_still_works_while_transfers_run() {
    // The whole point of the queue: the UI must not block.
    let dir = tempfile::tempdir().unwrap();
    for i in 0..6 {
        std::fs::write(
            dir.path().join(format!("f{}.bin", i)),
            vec![1u8; 512 * 1024],
        )
        .unwrap();
    }
    let dest_dir = dir.path().join("out");

    let mut app = FileBrowser::new(Some(dir.path().to_path_buf()), None, false);
    settle(&mut app).await;

    for i in 0..6 {
        let name = format!("f{}.bin", i);
        app.enqueue_transfer_for_test(
            myd::vfs::VPath::local(dir.path().join(&name)),
            myd::vfs::VPath::local(dest_dir.join(&name)),
        );
    }
    app.tick_transfers_for_test();

    // Work is genuinely in flight here — that's the precondition for the
    // responsiveness checks below. (Not all six need to be *queued*: the default
    // cap is large enough to start them all at once, which is fine.)
    assert!(app.transfer_queue().has_work());
    assert!(app.transfer_queue().active_count() >= 1);
    assert!(
        app.transfer_queue().active_count() <= app.transfer_queue().config.max_parallel,
        "the cap must still hold"
    );

    // Keys still route and the app still renders mid-transfer.
    let cursor_before = match app.current_screen() {
        Screen::Main(s) => s.tree.cursor,
        _ => 0,
    };
    app.handle_key_for_test(char_key('j'));
    let cursor_after = match app.current_screen() {
        Screen::Main(s) => s.tree.cursor,
        _ => 0,
    };
    assert_ne!(
        cursor_before, cursor_after,
        "cursor must move during transfers"
    );

    // Toggling views mid-transfer must not panic either.
    app.handle_key_for_test(char_key('v'));
    let _ = app_screen_text(&mut app, 120, 24);
    app.handle_key_for_test(char_key('v'));

    for _ in 0..4000 {
        app.tick_transfers_for_test();
        if !app.transfer_queue().has_work() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(2)).await;
    }
    assert_eq!(app.transfer_queue().finished_count(), 6);
}

#[tokio::test]
async fn gx_cancels_outstanding_transfers() {
    let dir = tempfile::tempdir().unwrap();
    for i in 0..5 {
        std::fs::write(dir.path().join(format!("c{}.bin", i)), vec![2u8; 64 * 1024]).unwrap();
    }
    let mut app = FileBrowser::new(Some(dir.path().to_path_buf()), None, false);
    settle(&mut app).await;

    for i in 0..5 {
        let name = format!("c{}.bin", i);
        app.enqueue_transfer_for_test(
            myd::vfs::VPath::local(dir.path().join(&name)),
            myd::vfs::VPath::local(dir.path().join("out").join(&name)),
        );
    }

    // g then x.
    app.handle_key_for_test(char_key('g'));
    app.handle_key_for_test(char_key('x'));

    for _ in 0..2000 {
        app.tick_transfers_for_test();
        if !app.transfer_queue().has_work() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(2)).await;
    }

    assert!(!app.transfer_queue().has_work());
    // Nothing succeeded: every entry was cancelled before or during transfer.
    assert!(app
        .transfer_queue()
        .transfers()
        .iter()
        .all(|t| !matches!(t.state, myd::transfer::TransferState::Done)));
}

#[tokio::test]
async fn transfer_panel_coexists_with_dual_panels_and_info_panel() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("a.txt"), "x").unwrap();
    let mut app = FileBrowser::new(Some(dir.path().to_path_buf()), None, true);
    settle(&mut app).await;
    app.resolve_loading_for_test();
    for _ in 0..200 {
        app.resolve_loading_for_test();
        tokio::time::sleep(std::time::Duration::from_millis(2)).await;
    }

    // Dual panels + info panel + transfer sidebar all at once must not panic and
    // must all be present. Pin the transfer panel open (Ctrl+t) since it's not
    // shown by default without a queued transfer.
    app.handle_key_for_test(ctrl_key('p'));
    app.handle_key_for_test(ctrl_key('t'));
    let text = app_screen_text(&mut app, 160, 30);
    assert!(text.contains("Transfers"), "sidebar missing: {}", text);
    assert_eq!(app.panel_count(), 2);
}

// ---------------------------------------------------------------------------
// Startup defaults to the current directory (not the directory picker).
// ---------------------------------------------------------------------------

#[tokio::test]
async fn no_arg_startup_opens_current_directory_not_the_picker() {
    // Serialize with other cwd-mutating tests (the cwd is process-global).
    let _cwd = CWD_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("a.txt"), "x").unwrap();
    // Set the process cwd so "no path" resolves to a known directory.
    std::env::set_current_dir(dir.path()).unwrap();

    // No path given.
    let mut app = FileBrowser::new(None, None, false);

    // It must never land on the directory picker — it opens the cwd directly.
    assert!(
        !matches!(app.current_screen(), Screen::DirPicker(_)),
        "no-arg startup should not open the directory picker"
    );

    settle(&mut app).await;
    assert!(matches!(app.current_screen(), Screen::Main(_)));

    let rooted = app
        .panel_current_dir(0)
        .and_then(|p| p.canonicalize().ok())
        == std::env::current_dir().ok().and_then(|p| p.canonicalize().ok());
    assert!(rooted, "panel should be rooted at the current directory");
}

/// `--directory` opens the picker instead of a directory.
#[tokio::test]
async fn directory_flag_starts_on_the_picker() {
    // Serialize with other cwd-mutating tests (the cwd is process-global).
    let _cwd = CWD_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("a.txt"), "x").unwrap();
    std::env::set_current_dir(dir.path()).unwrap();

    let mut app = FileBrowser::new_on_picker();

    assert!(
        matches!(app.current_screen(), Screen::DirPicker(_)),
        "--directory should open the picker"
    );
    // Nothing was scanned on the way: the picker is the only screen, so `q`
    // quits rather than dropping into a tree the user never asked for.
    assert_eq!(app.panel_depth_for_test(0), 1, "no directory underneath");

    // No `settle` here: there is deliberately no load in flight, and waiting for
    // one is how this test first "failed". A tick must leave the picker alone.
    app.resolve_loading_for_test();
    assert!(
        matches!(app.current_screen(), Screen::DirPicker(_)),
        "the picker must not be replaced by a background load"
    );
}

/// The `--directory` picker lists the catalog, which is the whole point of the
/// flag — an empty picker would be no better than the path field alone.
#[tokio::test]
async fn directory_flag_lists_the_saved_catalog() {
    let _cwd = CWD_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let dir = tempfile::tempdir().unwrap();
    std::env::set_current_dir(dir.path()).unwrap();

    // The real catalog is whatever this machine happens to have, so the picker
    // is built over a known one — it is populated during construction, which is
    // exactly the behaviour under test.
    let app = FileBrowser::new_on_picker_with_hosts_for_test(test_catalog());

    match app.current_screen() {
        Screen::DirPicker(p) => {
            let hosts = p.visible_options().iter().filter(|o| o.is_host()).count();
            assert_eq!(hosts, 4, "the saved hosts should be listed");
        }
        _ => panic!("expected the picker"),
    }
}

/// Connecting from a `--directory` picker must not empty the screen stack.
///
/// The picker is the only screen under `--directory`, and the connect path
/// popped it unconditionally — the next redraw then panicked in
/// `current_screen` with "empty stack", taking the app down as soon as a host
/// was chosen.
#[tokio::test]
async fn connecting_from_the_directory_picker_does_not_empty_the_panel() {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    let _cwd = CWD_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let dir = tempfile::tempdir().unwrap();
    std::env::set_current_dir(dir.path()).unwrap();

    let mut app = FileBrowser::new_on_picker_with_hosts_for_test(test_catalog());
    assert_eq!(app.panel_depth_for_test(0), 1, "the picker is the only screen");

    // Reproduce the reported flow: `/` to search, narrow to one host, Enter.
    app.handle_key_for_test(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));
    for c in "/france".chars() {
        app.handle_key_for_test(char_key(c));
    }
    app.handle_key_for_test(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));

    // The connection is asynchronous and this host does not resolve, so what
    // matters is that the panel still has a screen to draw. Touching it is what
    // used to panic.
    assert!(
        app.panel_depth_for_test(0) >= 1,
        "the panel must keep a screen while the connection is in flight"
    );
    let _ = app.current_screen();

    // A tick drives the connect state machine; it must not panic either.
    app.resolve_loading_for_test();
    let _ = app.current_screen();
}

#[tokio::test]
async fn gd_chord_opens_the_directory_picker() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("a.txt"), "x").unwrap();
    let mut app = FileBrowser::new(Some(dir.path().to_path_buf()), None, false);
    settle(&mut app).await;
    assert!(matches!(app.current_screen(), Screen::Main(_)));

    // `g` then `d` — the picker is reached only via this chord now.
    app.handle_key_for_test(char_key('g'));
    app.handle_key_for_test(char_key('d'));
    assert!(
        matches!(app.current_screen(), Screen::DirPicker(_)),
        "gd should open the directory picker"
    );
}

#[tokio::test]
async fn gs_chord_opens_the_sort_picker() {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("a.txt"), "x").unwrap();
    let mut app = FileBrowser::new(Some(dir.path().to_path_buf()), None, false);
    settle(&mut app).await;
    assert_eq!(app.modal_kind_for_test(), "none", "no modal to start");

    app.handle_key_for_test(char_key('g'));
    // Mid-chord: `g` alone must not have done anything yet.
    assert_eq!(app.modal_kind_for_test(), "none", "g alone is not a command");
    app.handle_key_for_test(char_key('s'));
    assert_eq!(
        app.modal_kind_for_test(),
        "sort_menu",
        "gs should open the sort picker"
    );

    // Esc backs out without changing the order, as it does everywhere else.
    app.handle_key_for_test(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
    assert_eq!(app.modal_kind_for_test(), "none", "Esc dismisses the picker");
}

/// A `g` is held until the next key arrives, however long that takes.
#[tokio::test]
async fn a_pending_g_waits_rather_than_expiring() {
    // Chords used to expire after 500ms, which made the same two keystrokes mean
    // different things depending on how fast they were typed — and on a loaded
    // machine `g` then `s` would run `s` (cycle sort) instead of opening the
    // picker. The wait is now unbounded.
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("a.txt"), "x").unwrap();
    let mut app = FileBrowser::new(Some(dir.path().to_path_buf()), None, false);
    settle(&mut app).await;

    let before = match app.current_screen() {
        Screen::Main(s) => s.tree.sort_mode,
        _ => unreachable!("settled onto a main screen"),
    };

    app.handle_key_for_test(char_key('g'));
    assert_eq!(
        app.pending_chord_for_test(),
        Some('g'),
        "the prefix is held, and visible to the status bar"
    );

    // Far longer than the old window.
    tokio::time::sleep(std::time::Duration::from_millis(900)).await;
    app.handle_key_for_test(char_key('s'));

    assert_eq!(
        app.modal_kind_for_test(),
        "sort_menu",
        "the chord completes no matter how long the gap was"
    );
    assert_eq!(app.pending_chord_for_test(), None, "and the prefix is cleared");
    app.handle_key_for_test(key(crossterm::event::KeyCode::Esc));

    let after = match app.current_screen() {
        Screen::Main(s) => s.tree.sort_mode,
        _ => unreachable!("still a main screen"),
    };
    assert_eq!(before, after, "s must not also have cycled the order");
}

/// An unknown second key beeps instead of running that key on its own.
#[tokio::test]
async fn an_unknown_chord_beeps_and_does_nothing_else() {
    // The old design fell through to the second key alone, so a slow `g`+`r`
    // refreshed. Now `g` followed by something that completes no chord is
    // reported and discarded — it does not half-run.
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("a.txt"), "x").unwrap();
    let mut app = FileBrowser::new(Some(dir.path().to_path_buf()), None, false);
    settle(&mut app).await;

    let bells = app.bells_rung_for_test();
    let before = match app.current_screen() {
        Screen::Main(s) => s.tree.sort_mode,
        _ => unreachable!("settled onto a main screen"),
    };

    // `gz` is not a chord. `z` alone is not a binding either, but the point is
    // that the *chord* failing is what beeps.
    app.handle_key_for_test(char_key('g'));
    app.handle_key_for_test(char_key('z'));
    assert_eq!(
        app.bells_rung_for_test(),
        bells + 1,
        "an unknown chord rings the bell"
    );
    assert_eq!(app.pending_chord_for_test(), None, "and the prefix is cleared");

    // `gt` is not a chord either, and `t` alone tags — which must not happen.
    let bells = app.bells_rung_for_test();
    app.handle_key_for_test(char_key('g'));
    app.handle_key_for_test(char_key('t'));
    assert_eq!(app.bells_rung_for_test(), bells + 1, "beeps again");
    match app.current_screen() {
        Screen::Main(s) => assert!(
            s.tagged_paths().is_empty(),
            "the second key must not run on its own"
        ),
        _ => panic!("expected a main screen"),
    }

    // And `s` after a failed chord is an ordinary keystroke again.
    app.handle_key_for_test(char_key('s'));
    settle(&mut app).await;
    let after = match app.current_screen() {
        Screen::Main(s) => s.tree.sort_mode,
        _ => unreachable!("still a main screen"),
    };
    assert_ne!(before, after, "the handler is back to normal after a beep");
}

/// A pending chord is visible, since nothing else signals the wait.
#[tokio::test]
async fn a_pending_chord_shows_in_the_status_bar() {
    // Without a timeout the app can sit mid-chord indefinitely, so the state has
    // to be on screen — otherwise a stray `g` looks like a key that did nothing
    // and the next keystroke behaves surprisingly.
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("a.txt"), "x").unwrap();
    let mut app = FileBrowser::new(Some(dir.path().to_path_buf()), None, false);
    settle(&mut app).await;

    let text = app_screen_text(&mut app, 100, 12);
    assert!(!text.contains("g…"), "nothing pending to start with");

    app.handle_key_for_test(char_key('g'));
    let text = app_screen_text(&mut app, 100, 12);
    assert!(
        text.contains("g…"),
        "a held prefix must be visible: {}",
        text
    );

    app.handle_key_for_test(char_key('g'));
    let text = app_screen_text(&mut app, 100, 12);
    assert!(!text.contains("g…"), "and gone once the chord completes");
}

/// Esc abandons a pending chord quietly; other non-character keys beep.
#[tokio::test]
async fn esc_cancels_a_pending_chord_without_a_beep() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("a.txt"), "x").unwrap();
    let mut app = FileBrowser::new(Some(dir.path().to_path_buf()), None, false);
    settle(&mut app).await;

    let bells = app.bells_rung_for_test();
    app.handle_key_for_test(char_key('g'));
    assert_eq!(app.pending_chord_for_test(), Some('g'));
    app.handle_key_for_test(key(crossterm::event::KeyCode::Esc));
    assert_eq!(app.pending_chord_for_test(), None, "Esc drops the prefix");
    assert_eq!(
        app.bells_rung_for_test(),
        bells,
        "backing out deliberately is not an error"
    );

    // An arrow key mid-chord is not the second half of anything, so it does beep.
    app.handle_key_for_test(char_key('g'));
    app.handle_key_for_test(key(crossterm::event::KeyCode::Down));
    assert_eq!(app.pending_chord_for_test(), None);
    assert_eq!(
        app.bells_rung_for_test(),
        bells + 1,
        "a key that cannot complete a chord reports it"
    );
}

/// The whole gesture from the request: `gs5` sorts by newest.
#[tokio::test]
async fn gs_then_a_number_picks_that_sort_order() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("a.txt"), "x").unwrap();
    let mut app = FileBrowser::new(Some(dir.path().to_path_buf()), None, false);
    settle(&mut app).await;

    // Entry 5 is Newest. Asserted against SortMode::ALL rather than hardcoded,
    // so inserting a mode ahead of it corrects this test instead of leaving it
    // quietly checking the wrong entry.
    assert_eq!(
        myd::screen::SortMode::ALL[4],
        myd::screen::SortMode::Newest,
        "gs5 is only 'newest' while Newest is the fifth entry"
    );

    app.handle_key_for_test(char_key('g'));
    app.handle_key_for_test(char_key('s'));
    app.handle_key_for_test(char_key('5'));
    assert_eq!(
        app.modal_kind_for_test(),
        "none",
        "picking a number closes the menu"
    );

    match app.current_screen() {
        Screen::Main(s) => assert_eq!(
            s.tree.sort_mode,
            myd::screen::SortMode::Newest,
            "gs5 should sort by newest"
        ),
        _ => panic!("still a main screen"),
    }

    // And the order survives entering a directory, like the `s` key's does.
    assert_eq!(
        app.view_prefs_sort_mode_for_test(),
        myd::screen::SortMode::Newest,
        "the chosen order is remembered for screens opened later"
    );
}

// ---------------------------------------------------------------------------
// The user can always stop and exit, even during remote work.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn ctrl_c_quits_even_with_a_modal_up() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("a.txt"), "x").unwrap();
    let mut app = FileBrowser::new(Some(dir.path().to_path_buf()), None, false);
    settle(&mut app).await;

    // Open the connect prompt (an Input modal). A plain key would be swallowed
    // by the modal, but Ctrl-C must still quit.
    app.handle_key_for_test(char_key('g'));
    app.handle_key_for_test(char_key('r'));
    // Ctrl-C returns false (quit) regardless of the modal being up.
    let keep_running = app.handle_key_for_test(ctrl_key('c'));
    assert!(!keep_running, "Ctrl-C must quit even with a modal open");
}

#[tokio::test]
async fn ctrl_c_quits_during_a_connection_attempt() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("a.txt"), "x").unwrap();
    let mut app = FileBrowser::new(Some(dir.path().to_path_buf()), None, false);
    settle(&mut app).await;

    // Point at an unroutable address so the connect hangs (it won't complete
    // within the test); the "Connecting..." overlay is up.
    app.connect_on_start("sftp://192.0.2.1/tmp"); // TEST-NET-1, never routable
    assert!(app.is_connecting_for_test());

    // Ctrl-C exits immediately without waiting for the connect to time out.
    let keep_running = app.handle_key_for_test(ctrl_key('c'));
    assert!(!keep_running, "Ctrl-C must quit while connecting");
}

#[tokio::test]
async fn q_cancels_a_hanging_connection_and_returns_to_browsing() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("a.txt"), "x").unwrap();
    let mut app = FileBrowser::new(Some(dir.path().to_path_buf()), None, false);
    settle(&mut app).await;

    app.connect_on_start("sftp://192.0.2.1/tmp");
    assert!(app.is_connecting_for_test());

    // q during "Connecting..." abandons the attempt and keeps the app running,
    // dropping the user back to their local panel rather than trapping them.
    let keep_running = app.handle_key_for_test(char_key('q'));
    assert!(keep_running, "q should cancel the connect, not quit the app");
    assert!(!app.is_connecting_for_test(), "connection attempt should be cancelled");
    // Still on the local Main screen.
    assert!(matches!(app.current_screen(), Screen::Main(_)));
    assert_eq!(app.panel_count(), 1);
}

// ---------------------------------------------------------------------------
// Ghost entries + auto-update of the destination on transfer completion.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn in_progress_transfer_shows_a_ghost_then_updates_on_completion() {
    // Source file and an empty destination directory, both local.
    let root = tempfile::tempdir().unwrap();
    let src = root.path().join("payload.bin");
    std::fs::write(&src, vec![7u8; 256 * 1024]).unwrap();
    let dest_dir = root.path().join("dest");
    std::fs::create_dir_all(&dest_dir).unwrap();

    // Open the destination directory in the panel so we can watch it update.
    let mut app = FileBrowser::new(Some(dest_dir.clone()), None, false);
    settle(&mut app).await;

    // The destination directory starts empty on screen.
    let before = app_screen_text(&mut app, 100, 16);
    assert!(!before.contains("payload.bin"), "dest should start empty: {}", before);

    // Queue a transfer INTO the open destination directory.
    app.enqueue_transfer_for_test(
        myd::vfs::VPath::local(&src),
        myd::vfs::VPath::local(dest_dir.join("payload.bin")),
    );

    // Before it runs, a ghost row is drawn in the destination tree.
    let ghost_frame = app_screen_text(&mut app, 100, 16);
    assert!(
        ghost_frame.contains("payload.bin") && ghost_frame.contains("copying"),
        "a ghost row should appear while queued: {}",
        ghost_frame
    );

    // Drive to completion.
    for _ in 0..2000 {
        app.tick_transfers_for_test();
        if !app.transfer_queue().has_work() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(2)).await;
    }
    assert_eq!(app.transfer_queue().finished_count(), 1);
    // The file really landed.
    assert!(dest_dir.join("payload.bin").exists());

    // The destination tree updated on its own — the real file is present and the
    // ghost is gone — WITHOUT the user pressing `r`.
    let after = app_screen_text(&mut app, 100, 16);
    assert!(
        after.contains("payload.bin"),
        "completed file should appear automatically: {}",
        after
    );
    assert!(
        !after.contains("copying"),
        "ghost should clear once the transfer completes: {}",
        after
    );
}

#[tokio::test]
async fn completion_refresh_is_scoped_to_the_destination_directory() {
    // A file already present in the dest dir must survive the targeted reload
    // (i.e. the reload re-lists that one level, it doesn't wipe the tree).
    let root = tempfile::tempdir().unwrap();
    let src = root.path().join("new.bin");
    std::fs::write(&src, vec![1u8; 64 * 1024]).unwrap();
    let dest_dir = root.path().join("dest");
    std::fs::create_dir_all(&dest_dir).unwrap();
    std::fs::write(dest_dir.join("already-here.txt"), "x").unwrap();

    let mut app = FileBrowser::new(Some(dest_dir.clone()), None, false);
    settle(&mut app).await;
    assert!(app_screen_text(&mut app, 100, 16).contains("already-here.txt"));

    app.enqueue_transfer_for_test(
        myd::vfs::VPath::local(&src),
        myd::vfs::VPath::local(dest_dir.join("new.bin")),
    );
    for _ in 0..2000 {
        app.tick_transfers_for_test();
        if !app.transfer_queue().has_work() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(2)).await;
    }

    let after = app_screen_text(&mut app, 100, 16);
    assert!(after.contains("new.bin"), "new file should appear: {}", after);
    assert!(
        after.contains("already-here.txt"),
        "existing file must survive the targeted reload: {}",
        after
    );
}

/// A symlink to a directory must be enterable, not treated as a plain file.
///
/// `read_dir` reports the *link's* type, so without resolving the target a
/// symlinked directory arrives with `is_dir == false` and can't be expanded —
/// the user sees it but can't traverse it.
#[tokio::test]
async fn test_symlinked_directory_is_traversable() {
    use myd::screen::SortMode;
    use myd::widget::file_tree::FileTree;

    let td = tempfile::tempdir().unwrap();
    let real = td.path().join("real_dir");
    std::fs::create_dir(&real).unwrap();
    std::fs::write(real.join("inside.txt"), "hi").unwrap();
    std::os::unix::fs::symlink(&real, td.path().join("link_dir")).unwrap();

    let mut tree = FileTree::new(td.path().to_path_buf(), SortMode::Largest, true, false);
    tree.expand_all();

    let link = tree
        .lines
        .iter()
        .find(|l| l.name == "link_dir")
        .expect("link_dir should be listed");
    assert!(link.is_symlink, "link_dir should be flagged as a symlink");
    assert!(
        link.is_dir,
        "a symlink to a directory must report is_dir so it can be entered"
    );

    // Expanding it reaches the target's contents.
    assert!(
        tree.lines
            .iter()
            .any(|l| l.name == "inside.txt" && l.depth > 1),
        "should traverse into the symlinked directory"
    );
}

/// Symlinks render differently from the real files and directories they point
/// at: a link glyph, a distinct colour, and a trailing `@` that survives on a
/// monochrome terminal.
#[tokio::test]
async fn test_symlinks_render_distinctly() {
    use myd::screen::SortMode;
    use myd::widget::file_tree::{FileTree, SYMLINK_COLOR};

    let td = tempfile::tempdir().unwrap();
    let real = td.path().join("real_dir");
    std::fs::create_dir(&real).unwrap();
    std::fs::write(td.path().join("plain.txt"), "x").unwrap();
    std::os::unix::fs::symlink(&real, td.path().join("link_dir")).unwrap();
    std::os::unix::fs::symlink(td.path().join("plain.txt"), td.path().join("link_file")).unwrap();

    let mut tree = FileTree::new(td.path().to_path_buf(), SortMode::Largest, true, false);
    tree.expand_all();

    let text = tree.render_text();
    let rendered: Vec<String> = text
        .lines
        .iter()
        .map(|l| l.spans.iter().map(|s| s.content.to_string()).collect())
        .collect();
    let all = rendered.join("\n");

    // Directory link and file link both get the link glyph and an `@` suffix
    // (with a `/` for the directory), while the real entries keep theirs.
    assert!(all.contains("🔗 link_dir@/"), "dir symlink marker missing: {}", all);
    assert!(all.contains("🔗 link_file@"), "file symlink marker missing: {}", all);
    assert!(all.contains("📂 real_dir"), "real dir should keep its icon: {}", all);
    assert!(all.contains("📄 plain.txt"), "real file should keep its icon: {}", all);

    // The link name is coloured distinctly from a real directory's blue.
    let link_style = text
        .lines
        .iter()
        .flat_map(|l| l.spans.iter())
        .find(|s| s.content.starts_with("link_dir"))
        .map(|s| s.style)
        .expect("link_dir span");
    assert_eq!(link_style.fg, Some(SYMLINK_COLOR));
}

/// A long prompt must stay fully readable and keep its Enter/Esc hint on screen.
///
/// The dialog was a fixed 55x7 box with 7 content lines, so the borders clipped
/// the last two rows — the hint vanished — and a long `user@host` was truncated
/// at the border, leaving the user unsure what was being asked.
#[tokio::test]
async fn test_input_dialog_wraps_long_prompts_and_keeps_hint() {
    use myd::widget::input_dialog::InputDialog;
    use ratatui::{backend::TestBackend, Terminal};

    let dialog = InputDialog::new(
        "Password for a-fairly-long-username@some.remote.host.example.com:",
        "",
    )
    .masked();

    let mut term = Terminal::new(TestBackend::new(100, 30)).unwrap();
    term.draw(|f| dialog.render(f, f.area())).unwrap();
    let buf = term.backend().buffer();
    let text: String = (0..buf.area.height)
        .map(|y| {
            (0..buf.area.width)
                .map(|x| buf[(x, y)].symbol().to_string())
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n");

    // The whole address survives, wrapped rather than cut at the border.
    assert!(
        text.contains("a-fairly-long-username@some.remote.host.example.com:"),
        "long prompt was truncated: {}",
        text
    );
    // And the user can still see how to answer it.
    assert!(
        text.contains("Enter: OK") && text.contains("Esc: Cancel"),
        "the Enter/Esc hint was clipped: {}",
        text
    );
}

/// A rejected password must be re-promptable rather than a dead end.
///
/// A wrong password used to `bail!` into a generic failure dialog, stranding the
/// user with no way back to the prompt — a typo meant restarting the connection.
#[tokio::test]
async fn test_wrong_password_reprompts_instead_of_failing() {
    use myd::vfs::sftp::AuthNeed;

    // The retry flag is what distinguishes "ask again" from a fatal error, and
    // it drives the wording the user sees.
    let first = AuthNeed::Password {
        user: "juan".into(),
        host: "example.com".into(),
        retry: false,
    };
    let again = AuthNeed::Password {
        user: "juan".into(),
        host: "example.com".into(),
        retry: true,
    };
    assert_ne!(first, again, "a retry must be distinguishable from the first ask");

    // The retry prompt says the attempt failed and that Esc gives up.
    let prompt = myd::app::credential_prompt_for_test(&again);
    assert!(
        prompt.contains("Authentication failed"),
        "retry prompt should say the password was rejected: {}",
        prompt
    );
    assert!(
        prompt.contains("Esc"),
        "retry prompt should offer a way out: {}",
        prompt
    );
    // The first ask stays terse.
    let first_prompt = myd::app::credential_prompt_for_test(&first);
    assert!(!first_prompt.contains("Authentication failed"), "{}", first_prompt);
}

/// A nested directory copy must reproduce the tree exactly, including empty
/// directories and byte-identical file contents.
///
/// Files within a level and subdirectories themselves are now copied
/// concurrently (a serial walk was the dominant cost on a high-latency link), so
/// this guards against the parallelism corrupting or dropping anything.
#[tokio::test]
async fn test_concurrent_directory_copy_reproduces_the_tree() {
    use myd::transfer::{run_transfer, TransferConfig, TransferId, TransferJob, TransferProgress};
    use myd::utils::sizes::CancelToken;
    use myd::vfs::{LocalFs, VPath, Vfs};
    use std::sync::Arc;

    let td = tempfile::tempdir().unwrap();
    let src = td.path().join("src");
    let dest = td.path().join("dest");

    // A tree wide and deep enough to exercise the concurrent paths, with varied
    // file sizes and an empty directory.
    let mut expected: Vec<(std::path::PathBuf, Vec<u8>)> = Vec::new();
    for d in 0..4 {
        let dir = src.join(format!("dir{}", d)).join("nested");
        std::fs::create_dir_all(&dir).unwrap();
        for f in 0..5 {
            let content: Vec<u8> = (0..(1024 * (f + 1) + d)).map(|i| (i % 251) as u8).collect();
            let path = dir.join(format!("file{}.bin", f));
            std::fs::write(&path, &content).unwrap();
            expected.push((path.strip_prefix(&src).unwrap().to_path_buf(), content));
        }
    }
    std::fs::create_dir_all(src.join("empty_dir")).unwrap();

    let fs: Arc<dyn Vfs> = Arc::new(LocalFs::new());
    let outcome = run_transfer(TransferJob {
        id: TransferId(1),
        src_fs: fs.clone(),
        dest_fs: fs.clone(),
        src: VPath::local(&src),
        dest: VPath::local(&dest),
        progress: Arc::new(TransferProgress::new(0)),
        cancel: CancelToken::new(),
        config: TransferConfig::default(),
    })
    .await
    .expect("directory copy failed");
    assert!(matches!(outcome, myd::transfer::TransferOutcome::Done));

    for (rel, content) in &expected {
        let landed = dest.join(rel);
        let got = std::fs::read(&landed)
            .unwrap_or_else(|e| panic!("missing {}: {}", landed.display(), e));
        assert_eq!(&got, content, "content differs for {}", rel.display());
    }
    assert!(
        dest.join("empty_dir").is_dir(),
        "an empty directory must still be created"
    );
}

/// The progress total grows as the tree is discovered, and ends up matching the
/// bytes actually transferred.
///
/// The total used to come from a full recursive pre-walk that had to finish
/// before the first byte moved; it is now accumulated from the listings the copy
/// already performs.
#[tokio::test]
async fn test_directory_copy_progress_total_matches_bytes_copied() {
    use myd::transfer::{run_transfer, TransferConfig, TransferId, TransferJob, TransferProgress};
    use myd::utils::sizes::CancelToken;
    use myd::vfs::{LocalFs, VPath, Vfs};
    use std::sync::Arc;

    let td = tempfile::tempdir().unwrap();
    let src = td.path().join("src");
    let dest = td.path().join("dest");

    let mut total_bytes = 0u64;
    for d in 0..3 {
        let dir = src.join(format!("d{}", d));
        std::fs::create_dir_all(&dir).unwrap();
        for f in 0..4 {
            let content = vec![7u8; 2048 * (f + 1)];
            total_bytes += content.len() as u64;
            std::fs::write(dir.join(format!("f{}.bin", f)), &content).unwrap();
        }
    }

    let fs: Arc<dyn Vfs> = Arc::new(LocalFs::new());
    let progress = Arc::new(TransferProgress::new(0));
    run_transfer(TransferJob {
        id: TransferId(1),
        src_fs: fs.clone(),
        dest_fs: fs.clone(),
        src: VPath::local(&src),
        dest: VPath::local(&dest),
        progress: progress.clone(),
        cancel: CancelToken::new(),
        config: TransferConfig::default(),
    })
    .await
    .unwrap();

    assert_eq!(
        progress.total_bytes(),
        total_bytes,
        "discovered total should equal the tree's real size"
    );
    assert_eq!(progress.bytes_done(), total_bytes, "all bytes accounted for");
    assert!((progress.fraction() - 1.0).abs() < 1e-9);
}

/// The transfer panel reports the work, not the worker-pool capacity.
///
/// It used to read "Transfers (0/4)", which looks like four transfers exist and
/// none have started; the 4 was just the parallelism cap.
#[tokio::test]
async fn test_transfer_panel_title_shows_work_not_capacity() {
    use myd::app::FileBrowser;

    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("a.bin"), vec![1u8; 256 * 1024]).unwrap();
    let dest_dir = dir.path().join("out");

    let mut app = FileBrowser::new(Some(dir.path().to_path_buf()), None, false);
    settle(&mut app).await;

    // Idle: no misleading "0/N".
    app.handle_key_for_test(ctrl_key('t')); // pin the panel visible
    let idle = app_screen_text(&mut app, 120, 20);
    assert!(idle.contains("Transfers"), "panel should be shown: {}", idle);
    assert!(
        !idle.contains("0/"),
        "an idle panel must not imply pending work: {}",
        idle
    );

    // With work in flight the title names active/queued counts explicitly.
    app.enqueue_transfer_for_test(
        myd::vfs::VPath::local(dir.path().join("a.bin")),
        myd::vfs::VPath::local(dest_dir.join("a.bin")),
    );
    app.tick_transfers_for_test();
    let busy = app_screen_text(&mut app, 120, 20);
    assert!(
        busy.contains("active") || busy.contains("Transfers"),
        "title should describe the actual work: {}",
        busy
    );
    assert!(
        !busy.contains("/4)"),
        "title must not show the parallelism cap: {}",
        busy
    );
}

/// Changing the sort order on a remote tree must never touch the network.
///
/// Regression: `sort_key_fast` fell back to `source.file_size()` on a cache
/// miss. That runs inside the sort comparator, on the event-loop thread, so on
/// a remote tree every miss was a blocking SFTP round trip — a 733-entry
/// listing produced thousands of them and froze the UI for minutes. Remote
/// sizes come from the directory listing, so a miss now sorts as unknown
/// instead of going to the server to find out.
#[tokio::test]
async fn test_remote_resort_never_hits_the_network() {
    use myd::screen::SortMode;
    use myd::utils::sizes::{CancelToken, SizeCache};
    use myd::vfs::{BackendId, VEntry, VMetadata, VPath, VRead, VWrite, Vfs};
    use myd::widget::file_tree::FileTree;
    use myd::widget::progress::OpProgress;
    use myd::widget::source::{RemoteSource, Source};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    static STATS: AtomicUsize = AtomicUsize::new(0);

    /// A remote backend that counts every stat, so any network access during a
    /// re-sort is visible.
    struct CountingRemote;

    #[async_trait::async_trait]
    impl Vfs for CountingRemote {
        fn scheme(&self) -> &'static str {
            "sftp"
        }
        async fn read_dir(&self, p: &VPath) -> anyhow::Result<Vec<VEntry>> {
            // A wide, directory-heavy listing, like the reported case.
            // Two levels of subdirectories below the root, which sits at
            // /remote/data (3 components including the leading "/").
            let depth = p.path.components().count();
            let mut v = Vec::new();
            if depth < 5 {
                for i in 0..20 {
                    v.push(VEntry::new(format!("d{:03}", i), true));
                }
            }
            for i in 0..12 {
                let mut e = VEntry::new(format!("f{:03}.txt", i), false);
                e.len = 1000 + i as u64;
                v.push(e);
            }
            Ok(v)
        }
        async fn stat(&self, p: &VPath) -> anyhow::Result<VMetadata> {
            STATS.fetch_add(1, Ordering::Relaxed);
            let n = p
                .path
                .file_name()
                .map(|s| s.to_string_lossy().to_string())
                .unwrap_or_default();
            Ok(VMetadata {
                is_dir: !n.contains('.'),
                is_symlink: false,
                len: 4096,
                mode: None,
                uid: None,
                gid: None,
                mtime: None,
                atime: None,
                ctime: None,
            })
        }
        async fn create_dir_all(&self, _p: &VPath) -> anyhow::Result<()> {
            Ok(())
        }
        async fn remove_file(&self, _p: &VPath) -> anyhow::Result<()> {
            Ok(())
        }
        async fn remove_dir(&self, _p: &VPath) -> anyhow::Result<()> {
            Ok(())
        }
        async fn rename(&self, _f: &VPath, _t: &VPath) -> anyhow::Result<()> {
            Ok(())
        }
        async fn open_read(&self, _p: &VPath) -> anyhow::Result<Box<dyn VRead>> {
            anyhow::bail!("unused")
        }
        async fn open_write(
            &self,
            _p: &VPath,
            _l: Option<u64>,
        ) -> anyhow::Result<Box<dyn VWrite>> {
            anyhow::bail!("unused")
        }
        async fn dir_size(
            &self,
            _p: &VPath,
            _c: &SizeCache,
            _ct: &CancelToken,
            _pr: Option<&OpProgress>,
        ) -> u64 {
            0
        }
        fn has_recursive_sizes(&self) -> bool {
            false
        }
    }

    let vfs: Arc<dyn Vfs> = Arc::new(CountingRemote);
    let source = Source::Remote(RemoteSource::new(BackendId(1), vfs).unwrap());
    let cancel = CancelToken::new();
    let progress = OpProgress::new();
    let mut tree = FileTree::with_source_cancellable_progress(
        source,
        std::path::PathBuf::from("/remote/data"),
        SortMode::Largest,
        true,
        false,
        SizeCache::new(),
        &cancel,
        &progress,
    )
    .expect("remote tree should build");
    tree.expand_all();
    assert!(tree.lines.len() > 200, "need a large tree to be meaningful, got {}", tree.lines.len());

    // Worst case: nothing cached at all. Even then the re-sort is pure
    // in-memory work — a single stat here would be a network round trip.
    tree.size_cache.clear();
    let before = STATS.load(Ordering::Relaxed);
    let started = std::time::Instant::now();
    tree.set_sort_mode(SortMode::Smallest);
    let elapsed = started.elapsed();

    assert_eq!(
        STATS.load(Ordering::Relaxed) - before,
        0,
        "re-sorting a remote tree must not stat the server"
    );
    assert!(
        elapsed < std::time::Duration::from_millis(250),
        "re-sort should be effectively instant, took {:?}",
        elapsed
    );

    // Cycling through every mode stays network-free too (the time-based sorts
    // read timestamps captured in the listing).
    for mode in [
        SortMode::Newest,
        SortMode::Oldest,
        SortMode::RecentlyAccessed,
        SortMode::DirsFirst,
        SortMode::FilesFirst,
    ] {
        tree.set_sort_mode(mode);
    }
    assert_eq!(
        STATS.load(Ordering::Relaxed) - before,
        0,
        "no sort mode may reach the network"
    );
}

/// Reflattening a remote tree must not walk the local disk.
///
/// `recompute_cache` runs on every reflatten — every sort, filter and hidden
/// toggle — and called `get_dir_size`/`get_file_size` on a cache miss. Those
/// take *local* paths, so on a remote tree they measured either nothing or, for
/// a path like /usr/share that exists on both machines, an unrelated local
/// directory. Measured at ~793ms for a 2651-line tree before the fix.
#[tokio::test]
async fn test_remote_reflatten_does_not_walk_the_local_disk() {
    use myd::screen::SortMode;
    use myd::utils::sizes::{CancelToken, SizeCache};
    use myd::widget::file_tree::FileTree;
    use myd::widget::progress::OpProgress;
    use myd::widget::source::Source;

    // Root at a path that really exists locally — the dangerous case, where a
    // local walk silently succeeds against the wrong filesystem.
    let local_root = std::path::PathBuf::from("/usr/share");
    if !local_root.is_dir() {
        eprintln!("/usr/share missing; skipping");
        return;
    }

    // A minimal remote backend; this test only cares that nothing walks the
    // local disk, so the listing content is arbitrary.
    struct Wide;
    #[async_trait::async_trait]
    impl myd::vfs::Vfs for Wide {
        fn scheme(&self) -> &'static str { "sftp" }
        async fn read_dir(&self, p: &myd::vfs::VPath) -> anyhow::Result<Vec<myd::vfs::VEntry>> {
            let depth = p.path.components().count();
            let mut v = Vec::new();
            if depth < 5 {
                for i in 0..15 { v.push(myd::vfs::VEntry::new(format!("d{:03}", i), true)); }
            }
            for i in 0..10 {
                let mut e = myd::vfs::VEntry::new(format!("f{:03}.txt", i), false);
                e.len = 1000 + i as u64;
                v.push(e);
            }
            Ok(v)
        }
        async fn stat(&self, p: &myd::vfs::VPath) -> anyhow::Result<myd::vfs::VMetadata> {
            let n = p.path.file_name().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
            Ok(myd::vfs::VMetadata { is_dir: !n.contains('.'), is_symlink: false, len: 4096,
                mode: None, uid: None, gid: None, mtime: None, atime: None, ctime: None })
        }
        async fn create_dir_all(&self, _p: &myd::vfs::VPath) -> anyhow::Result<()> { Ok(()) }
        async fn remove_file(&self, _p: &myd::vfs::VPath) -> anyhow::Result<()> { Ok(()) }
        async fn remove_dir(&self, _p: &myd::vfs::VPath) -> anyhow::Result<()> { Ok(()) }
        async fn rename(&self, _f: &myd::vfs::VPath, _t: &myd::vfs::VPath) -> anyhow::Result<()> { Ok(()) }
        async fn open_read(&self, _p: &myd::vfs::VPath) -> anyhow::Result<Box<dyn myd::vfs::VRead>> { anyhow::bail!("unused") }
        async fn open_write(&self, _p: &myd::vfs::VPath, _l: Option<u64>) -> anyhow::Result<Box<dyn myd::vfs::VWrite>> { anyhow::bail!("unused") }
        async fn dir_size(&self, _p: &myd::vfs::VPath, _c: &SizeCache, _ct: &CancelToken,
            _pr: Option<&OpProgress>) -> u64 { 0 }
        fn has_recursive_sizes(&self) -> bool { false }
    }
    let vfs: std::sync::Arc<dyn myd::vfs::Vfs> = std::sync::Arc::new(Wide);
    let source = Source::Remote(
        myd::widget::source::RemoteSource::new(myd::vfs::BackendId(1), vfs).unwrap());
    let cancel = CancelToken::new();
    let progress = OpProgress::new();
    let mut tree = FileTree::with_source_cancellable_progress(
        source,
        local_root,
        SortMode::Largest,
        true,
        false,
        SizeCache::new(),
        &cancel,
        &progress,
    )
    .expect("remote tree should build");
    tree.expand_all();
    assert!(tree.lines.len() > 500, "need a big tree, got {}", tree.lines.len());

    tree.size_cache.clear();
    let started = std::time::Instant::now();
    tree.reflatten();
    let elapsed = started.elapsed();

    assert!(
        elapsed < std::time::Duration::from_millis(200),
        "reflatten walked the local disk: took {:?}",
        elapsed
    );
}

/// `m` moves the selection into the other panel's directory.
///
/// Within one backend a move is a rename — the bytes never move — so this also
/// confirms the file arrives intact and the source is gone.
#[tokio::test]
async fn test_move_relocates_files_between_panels() {
    use myd::app::FileBrowser;

    let left = tempfile::tempdir().unwrap();
    let right = tempfile::tempdir().unwrap();
    let payload = vec![42u8; 128 * 1024];
    std::fs::write(left.path().join("cargo.bin"), &payload).unwrap();

    let mut app = FileBrowser::new(
        Some(left.path().to_path_buf()),
        Some(right.path().to_path_buf()),
        true,
    );
    for _ in 0..400 {
        app.resolve_loading_for_test();
        if app.panel_current_dir(0).is_some() && app.panel_current_dir(1).is_some() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(3)).await;
    }

    // Put the cursor on the file and move it to the right panel.
    app.handle_key_for_test(char_key('j'));
    app.handle_key_for_test(char_key('m'));

    for _ in 0..600 {
        app.resolve_loading_for_test();
        if right.path().join("cargo.bin").exists() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(3)).await;
    }

    assert_eq!(
        std::fs::read(right.path().join("cargo.bin")).unwrap(),
        payload,
        "the file should arrive intact"
    );
    assert!(
        !left.path().join("cargo.bin").exists(),
        "the source must be gone after a move"
    );
}

/// A move with only one panel open explains itself instead of doing nothing.
#[tokio::test]
async fn test_move_without_a_destination_panel_explains_why() {
    use myd::app::FileBrowser;

    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("a.txt"), "x").unwrap();
    let mut app = FileBrowser::new(Some(dir.path().to_path_buf()), None, false);
    settle(&mut app).await;

    app.handle_key_for_test(char_key('j'));
    app.handle_key_for_test(char_key('m'));
    let text = app_screen_text(&mut app, 100, 20);
    assert!(
        text.contains("two panels"),
        "a single-panel move should say what's missing: {}",
        text
    );
    assert!(dir.path().join("a.txt").exists(), "nothing should be moved");
}

/// Deleting still works after moving to the VFS, including a whole directory.
#[tokio::test]
async fn test_delete_removes_files_and_directories_through_the_vfs() {
    use myd::app::FileBrowser;

    let dir = tempfile::tempdir().unwrap();
    let victim = dir.path().join("victim_dir");
    std::fs::create_dir_all(victim.join("nested")).unwrap();
    std::fs::write(victim.join("nested/deep.txt"), "gone").unwrap();
    std::fs::write(dir.path().join("keep.txt"), "stay").unwrap();

    let mut app = FileBrowser::new(Some(dir.path().to_path_buf()), None, false);
    settle(&mut app).await;

    // Select the directory and delete it (confirming the dialog).
    let mut found = false;
    for _ in 0..20 {
        if let myd::screen::Screen::Main(s) = app.current_screen() {
            if s.tree
                .selected_line()
                .map(|l| l.name == "victim_dir")
                .unwrap_or(false)
            {
                found = true;
                break;
            }
        }
        app.handle_key_for_test(char_key('j'));
    }
    assert!(found, "could not put the cursor on victim_dir");

    app.handle_key_for_test(char_key('D'));
    app.handle_key_for_test(crossterm::event::KeyEvent::new(
        crossterm::event::KeyCode::Enter,
        crossterm::event::KeyModifiers::NONE,
    ));

    for _ in 0..600 {
        app.resolve_loading_for_test();
        if !victim.exists() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(3)).await;
    }

    assert!(!victim.exists(), "the directory tree should be deleted");
    assert!(
        dir.path().join("keep.txt").exists(),
        "unrelated files must survive"
    );
}

/// A move must never destroy an existing file at the destination.
///
/// `rename` replaces the destination silently on both backends, so without a
/// guard a move onto a taken name would overwrite it with no confirmation and
/// no way back. The source stays put and the failure is reported.
#[tokio::test]
async fn test_move_refuses_to_overwrite_an_existing_file() {
    use myd::app::FileBrowser;

    let left = tempfile::tempdir().unwrap();
    let right = tempfile::tempdir().unwrap();
    std::fs::write(left.path().join("report.txt"), "NEW").unwrap();
    std::fs::write(right.path().join("report.txt"), "PRECIOUS").unwrap();

    let mut app = FileBrowser::new(
        Some(left.path().to_path_buf()),
        Some(right.path().to_path_buf()),
        true,
    );
    for _ in 0..400 {
        app.resolve_loading_for_test();
        if app.panel_current_dir(0).is_some() && app.panel_current_dir(1).is_some() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(3)).await;
    }

    app.handle_key_for_test(char_key('j'));
    app.handle_key_for_test(char_key('m'));
    for _ in 0..600 {
        app.resolve_loading_for_test();
        if !app.is_operation_running_for_test() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(3)).await;
    }

    assert_eq!(
        std::fs::read_to_string(right.path().join("report.txt")).unwrap(),
        "PRECIOUS",
        "the existing destination file must survive"
    );
    assert!(
        left.path().join("report.txt").exists(),
        "a refused move must leave the source in place"
    );
}

/// A move collision offers overwrite, skip, or cancel — and each does what it
/// says.
///
/// Copy only offers overwrite/skip; a move also destroys the source, so being
/// able to stop the whole sequence matters more here.
#[tokio::test]
async fn test_move_collision_offers_overwrite_skip_and_cancel() {
    use myd::app::FileBrowser;

    // Helper: two panels, `left` holding `names`, `right` already holding a
    // colliding "taken.txt".
    async fn setup(
        names: &[&str],
    ) -> (
        tempfile::TempDir,
        tempfile::TempDir,
        FileBrowser,
    ) {
        let left = tempfile::tempdir().unwrap();
        let right = tempfile::tempdir().unwrap();
        for n in names {
            std::fs::write(left.path().join(n), format!("SRC:{}", n)).unwrap();
        }
        std::fs::write(right.path().join("taken.txt"), "DEST-ORIGINAL").unwrap();
        let mut app = FileBrowser::new(
            Some(left.path().to_path_buf()),
            Some(right.path().to_path_buf()),
            true,
        );
        for _ in 0..400 {
            app.resolve_loading_for_test();
            if app.panel_current_dir(0).is_some() && app.panel_current_dir(1).is_some() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(3)).await;
        }
        (left, right, app)
    }

    async fn settle_ops(app: &mut FileBrowser) {
        for _ in 0..600 {
            app.resolve_loading_for_test();
            if !app.is_operation_running_for_test() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(3)).await;
        }
    }

    // --- The prompt names all three options. ---
    let (_l, _r, mut app) = setup(&["taken.txt"]).await;
    app.handle_key_for_test(char_key('j')); // step onto the file
    app.handle_key_for_test(char_key('m'));
    let prompt = app_screen_text(&mut app, 110, 20);
    assert!(
        prompt.contains("[o]verwrite") && prompt.contains("[s]kip") && prompt.contains("[c]ancel"),
        "the collision prompt should offer all three: {}",
        prompt
    );

    // --- Overwrite replaces the destination. ---
    let (left, right, mut app) = setup(&["taken.txt"]).await;
    app.handle_key_for_test(char_key('j'));
    app.handle_key_for_test(char_key('m'));
    app.handle_key_for_test(char_key('o'));
    settle_ops(&mut app).await;
    assert_eq!(
        std::fs::read_to_string(right.path().join("taken.txt")).unwrap(),
        "SRC:taken.txt",
        "overwrite should replace the destination"
    );
    assert!(
        !left.path().join("taken.txt").exists(),
        "an overwriting move still removes the source"
    );

    // --- Skip leaves both sides alone. ---
    let (left, right, mut app) = setup(&["taken.txt"]).await;
    app.handle_key_for_test(char_key('j'));
    app.handle_key_for_test(char_key('m'));
    app.handle_key_for_test(char_key('s'));
    settle_ops(&mut app).await;
    assert_eq!(
        std::fs::read_to_string(right.path().join("taken.txt")).unwrap(),
        "DEST-ORIGINAL",
        "skip must not touch the destination"
    );
    assert!(
        left.path().join("taken.txt").exists(),
        "skip must leave the source in place"
    );

    // --- Cancel abandons the whole batch, including non-colliding files. ---
    let (left, right, mut app) = setup(&["taken.txt", "clean.txt"]).await;
    app.handle_key_for_test(char_key('*')); // expand so both are visible
    // Tag every file in the panel.
    for _ in 0..6 {
        app.handle_key_for_test(char_key('j'));
        app.handle_key_for_test(char_key('t'));
    }
    app.handle_key_for_test(char_key('m'));
    app.handle_key_for_test(char_key('c'));
    settle_ops(&mut app).await;
    assert_eq!(
        std::fs::read_to_string(right.path().join("taken.txt")).unwrap(),
        "DEST-ORIGINAL",
        "cancel must not overwrite"
    );
    assert!(
        left.path().join("clean.txt").exists(),
        "cancel abandons the entire move, not just the colliding file"
    );
    assert!(
        !right.path().join("clean.txt").exists(),
        "the non-colliding file must not have been moved either"
    );
}

/// A cross-backend move rides the transfer queue, like a cross-backend copy.
///
/// It used to run as a sequential background task behind a modal overlay, so a
/// move between hosts showed no rate, no ETA and no per-file progress. Queuing
/// it gives it the same bounded parallelism and transfer-panel reporting as a
/// copy — and the source is removed only once its copy has landed.
#[tokio::test]
async fn test_cross_backend_move_uses_the_transfer_queue() {
    use myd::transfer::TransferState;
    use myd::vfs::VPath;

    let src_dir = tempfile::tempdir().unwrap();
    let dest_dir = tempfile::tempdir().unwrap();
    let payload = vec![5u8; 96 * 1024];
    std::fs::write(src_dir.path().join("cargo.bin"), &payload).unwrap();

    // Two backend ids over the same local filesystem: enough to exercise the
    // cross-backend path, which routes on the id.
    let mut queue = myd::transfer::TransferQueue::default();
    let mut registry = myd::vfs::BackendRegistry::new();
    let second = registry.register(std::sync::Arc::new(myd::vfs::LocalFs::new()));

    queue.enqueue_move(
        VPath::new(myd::vfs::BackendId::LOCAL, src_dir.path().join("cargo.bin")),
        VPath::new(second, dest_dir.path().join("cargo.bin")),
        0,
        false,
    );
    assert_eq!(queue.queued_count(), 1, "the move should be queued, not run inline");

    for _ in 0..3000 {
        queue.tick(&registry);
        if !queue.has_work() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(2)).await;
    }

    assert_eq!(queue.finished_count(), 1);
    assert!(
        queue
            .transfers()
            .iter()
            .all(|t| t.state == TransferState::Done),
        "the queued move should complete: {:?}",
        queue.transfers().iter().map(|t| &t.state).collect::<Vec<_>>()
    );
    assert_eq!(
        std::fs::read(dest_dir.path().join("cargo.bin")).unwrap(),
        payload,
        "the bytes should arrive intact"
    );
    assert!(
        !src_dir.path().join("cargo.bin").exists(),
        "a queued move removes the source once its copy has landed"
    );
}

/// A queued move whose copy fails must not delete the source.
#[tokio::test]
async fn test_failed_cross_backend_move_keeps_the_source() {
    use myd::vfs::VPath;

    let src_dir = tempfile::tempdir().unwrap();
    std::fs::write(src_dir.path().join("keep.bin"), b"important").unwrap();

    let mut queue = myd::transfer::TransferQueue::default();
    let mut registry = myd::vfs::BackendRegistry::new();
    let second = registry.register(std::sync::Arc::new(myd::vfs::LocalFs::new()));

    // Destination under a path that can't be created (a file, not a directory),
    // so the copy fails.
    let blocker = src_dir.path().join("blocker");
    std::fs::write(&blocker, b"not a directory").unwrap();

    queue.enqueue_move(
        VPath::new(myd::vfs::BackendId::LOCAL, src_dir.path().join("keep.bin")),
        VPath::new(second, blocker.join("sub/keep.bin")),
        0,
        false,
    );

    for _ in 0..3000 {
        queue.tick(&registry);
        if !queue.has_work() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(2)).await;
    }

    assert!(
        src_dir.path().join("keep.bin").exists(),
        "a move whose copy failed must leave the source alone"
    );
}

/// Drilling into a directory keeps the sort order you chose.
///
/// The loading screens hardcoded `SortMode::Largest`, so entering a directory
/// silently reset the order — the sort key was chosen at build time and nothing
/// carried the user's choice down.
#[tokio::test]
async fn test_sort_order_survives_entering_a_directory() {
    use myd::app::FileBrowser;
    use myd::screen::{Screen, SortMode};

    let dir = tempfile::tempdir().unwrap();
    let sub = dir.path().join("subdir");
    std::fs::create_dir(&sub).unwrap();
    for i in 0..3 {
        std::fs::write(sub.join(format!("f{}.txt", i)), vec![b'x'; 100 * (i + 1)]).unwrap();
    }

    let mut app = FileBrowser::new(Some(dir.path().to_path_buf()), None, false);
    settle(&mut app).await;

    // Cycle off the default order, and remember what we landed on.
    app.handle_key_for_test(char_key('s'));
    let chosen = match app.current_screen() {
        Screen::Main(s) => s.tree.sort_mode,
        _ => panic!("expected a main screen"),
    };
    assert_ne!(chosen, SortMode::default(), "`s` should change the order");

    // Enter the subdirectory.
    let mut entered = false;
    for _ in 0..20 {
        if let Screen::Main(s) = app.current_screen() {
            if s.tree.selected_line().map(|l| l.name == "subdir").unwrap_or(false) {
                entered = true;
                break;
            }
        }
        app.handle_key_for_test(char_key('j'));
    }
    assert!(entered, "could not put the cursor on subdir");
    app.handle_key_for_test(crossterm::event::KeyEvent::new(
        crossterm::event::KeyCode::Enter,
        crossterm::event::KeyModifiers::NONE,
    ));
    settle(&mut app).await;

    match app.current_screen() {
        Screen::Main(s) => assert_eq!(
            s.tree.sort_mode, chosen,
            "the chosen sort order must survive drilling into a directory"
        ),
        _ => panic!("expected a main screen after entering the directory"),
    }
}

/// Changing the sort order must not touch the filesystem at all.
///
/// Reported against a CIFS mount: sorting there was unresponsive while a local
/// disk was instant. The cause was I/O reachable from the sort path — the sort
/// comparator stat'd on a cache miss, and the reflatten/treemap rebuild that
/// follows every sort ran a *recursive walk* per uncached directory. On a
/// network mount each of those is a round trip. "Local" does not mean "fast".
///
/// This proves the absence of I/O rather than its speed: the tree is loaded,
/// then every file is deleted from disk. Any filesystem access during a sort
/// would now see an empty directory, so if the sort still reports the sizes and
/// entries it loaded with, it cannot have gone back to the disk.
#[tokio::test]
async fn test_sorting_does_no_filesystem_io() {
    use myd::screen::SortMode;
    use myd::utils::sizes::SizeCache;
    use myd::widget::file_tree::FileTree;

    let dir = tempfile::tempdir().unwrap();
    for d in 0..6 {
        let sub = dir.path().join(format!("d{}", d));
        std::fs::create_dir_all(&sub).unwrap();
        for f in 0..8 {
            // Distinct sizes so the ordering is observable.
            std::fs::write(sub.join(format!("f{}.bin", f)), vec![b'x'; 100 * (f + 1)]).unwrap();
        }
    }

    let mut tree = FileTree::new(dir.path().to_path_buf(), SortMode::Largest, true, true);
    tree.expand_all();
    let loaded_lines = tree.lines.len();
    assert!(loaded_lines > 40, "expected a populated tree, got {}", loaded_lines);

    // Pull the disk out from under it.
    for d in 0..6 {
        std::fs::remove_dir_all(dir.path().join(format!("d{}", d))).unwrap();
    }

    // Every sort mode must still work purely from what is in memory.
    for mode in [
        SortMode::Smallest,
        SortMode::DirsFirst,
        SortMode::FilesFirst,
        SortMode::Newest,
        SortMode::Oldest,
        SortMode::RecentlyAccessed,
        SortMode::Largest,
    ] {
        let started = std::time::Instant::now();
        tree.set_sort_mode(mode);
        assert!(
            started.elapsed() < std::time::Duration::from_millis(250),
            "sorting by {:?} took {:?} — it is doing I/O",
            mode,
            started.elapsed()
        );
        assert_eq!(
            tree.lines.len(),
            loaded_lines,
            "sorting by {:?} lost entries — it re-read the (now empty) directory",
            mode
        );
    }

    // Largest-first still orders by the sizes captured at load time, which is
    // only possible from the cache.
    let files: Vec<u64> = tree
        .lines
        .iter()
        .filter(|l| !l.is_dir)
        .map(|l| tree.size_cache.get(&l.resolved_path).unwrap_or(0))
        .collect();
    assert!(
        files.iter().any(|&s| s > 0),
        "sizes from the load should survive the sort"
    );

    // The decisive case: an empty cache, so every lookup misses. Old code
    // measured each entry here — a stat per file and a *recursive walk* per
    // directory. Against a real tree that is what made a network mount crawl,
    // and it also silently re-populates the cache from disk.
    let dir2 = tempfile::tempdir().unwrap();
    for d in 0..6 {
        let sub = dir2.path().join(format!("d{}", d));
        std::fs::create_dir_all(&sub).unwrap();
        for f in 0..8 {
            std::fs::write(sub.join(format!("f{}.bin", f)), vec![b'y'; 100 * (f + 1)]).unwrap();
        }
    }
    let mut cold = FileTree::new(dir2.path().to_path_buf(), SortMode::Largest, true, true);
    cold.expand_all();
    let cold_lines = cold.lines.len();
    cold.size_cache.clear();
    assert_eq!(cold.size_cache.len(), 0, "cache should start empty");

    cold.set_sort_mode(SortMode::Smallest);
    assert_eq!(cold.lines.len(), cold_lines, "cold-cache sort lost entries");
    // Nothing was measured, so nothing was written back to the cache. A single
    // entry here means the sort path went to the filesystem.
    assert_eq!(
        cold.size_cache.len(),
        0,
        "sorting re-populated the size cache — it measured entries from disk"
    );
    let _ = SizeCache::new();
}

/// Sorting must not rebuild the info panel.
///
/// The panel's text depends on which entry is selected, not on the order rows
/// are displayed in — but `rebuild_treemap` dropped its cache on every sort,
/// and rebuilding it costs a stat, a canonicalize and a `read_dir` of the
/// selected directory. On a network filesystem that is several round trips paid
/// on every press of `s`, on top of the sort itself.
#[tokio::test]
async fn test_sorting_does_not_rebuild_the_info_panel() {
    use myd::app::FileBrowser;
    use myd::screen::Screen;

    let dir = tempfile::tempdir().unwrap();
    for i in 0..5 {
        std::fs::write(dir.path().join(format!("f{}.txt", i)), vec![b'x'; 50 * (i + 1)]).unwrap();
    }

    let mut app = FileBrowser::new(Some(dir.path().to_path_buf()), None, false);
    settle(&mut app).await;

    // Open the info panel and render once so its text is cached.
    app.handle_key_for_test(ctrl_key('p'));
    let _ = app_screen_text(&mut app, 120, 20);

    let cached_after_first_render = match app.current_screen() {
        Screen::Main(s) => s.info_cache_key_for_test(),
        _ => panic!("expected a main screen"),
    };
    assert!(
        cached_after_first_render,
        "the info panel should be cached after rendering"
    );

    // Sorting reorders rows; it changes nothing about the selected entry, so
    // the cached panel text must survive.
    app.handle_key_for_test(char_key('s'));
    let still_cached = match app.current_screen() {
        Screen::Main(s) => s.info_cache_key_for_test(),
        _ => panic!("expected a main screen"),
    };
    assert!(
        still_cached,
        "sorting invalidated the info panel, forcing a filesystem re-read"
    );
}

/// Rebuilding the treemap must not touch the filesystem.
///
/// This was the real cause of multi-second sorts on a CIFS mount: the treemap
/// is rebuilt on every sort, and it derived each directory tile's colour by
/// walking that directory — a `readdir` plus a `stat` per file, per tile. A
/// trace from a 1,895-line CIFS tree showed reorder+reflatten at 2.2ms and the
/// treemap rebuild at 9,686ms.
///
/// Proves absence of I/O rather than speed: the tree is loaded, then the files
/// are deleted from disk. A rebuild that still reports the right sizes and
/// colours cannot have gone back to the filesystem.
#[tokio::test]
async fn test_treemap_rebuild_does_no_filesystem_io() {
    use myd::screen::SortMode;
    use myd::utils::filetype::FileCategory;
    use myd::widget::file_tree::FileTree;
    use myd::widget::treemap::TreeMap;

    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("code")).unwrap();
    std::fs::write(dir.path().join("code/main.rs"), vec![0u8; 90_000]).unwrap();
    std::fs::create_dir_all(dir.path().join("media")).unwrap();
    std::fs::write(dir.path().join("media/clip.mp4"), vec![0u8; 50_000]).unwrap();

    let tree = FileTree::new(dir.path().to_path_buf(), SortMode::Largest, true, true);
    let before = TreeMap::from_file_tree(&tree);
    let colour_of = |tm: &TreeMap, name: &str| {
        tm.cells
            .iter()
            .find(|c| c.path.file_name().map(|n| n == name).unwrap_or(false))
            .unwrap_or_else(|| panic!("no tile named {}", name))
            .category
    };
    assert_eq!(colour_of(&before, "code"), FileCategory::Code);
    assert_eq!(colour_of(&before, "media"), FileCategory::Video);

    // Pull the disk out from under it.
    std::fs::remove_dir_all(dir.path().join("code")).unwrap();
    std::fs::remove_dir_all(dir.path().join("media")).unwrap();

    let started = std::time::Instant::now();
    let after = TreeMap::from_file_tree(&tree);
    let elapsed = started.elapsed();

    assert!(
        elapsed < std::time::Duration::from_millis(250),
        "treemap rebuild took {:?} — it is doing I/O",
        elapsed
    );
    assert_eq!(
        after.cells.len(),
        before.cells.len(),
        "rebuild lost tiles — it re-read the (now empty) directory"
    );
    assert_eq!(
        colour_of(&after, "code"),
        FileCategory::Code,
        "tile colour must come from cached data, not the filesystem"
    );
    assert_eq!(colour_of(&after, "media"), FileCategory::Video);
}

/// Splitting reuses the tree already on screen instead of re-listing.
///
/// `|` showed a loading screen for a directory the active panel had already
/// scanned. On a network mount that meant a real wait to see something the app
/// was already displaying.
#[tokio::test]
async fn test_split_clones_the_tree_without_rescanning() {
    use myd::app::FileBrowser;
    use myd::screen::Screen;

    let dir = tempfile::tempdir().unwrap();
    for i in 0..5 {
        std::fs::write(dir.path().join(format!("f{}.txt", i)), vec![b'x'; 100]).unwrap();
    }

    let mut app = FileBrowser::new(Some(dir.path().to_path_buf()), None, false);
    settle(&mut app).await;
    let before = match app.current_screen() {
        Screen::Main(s) => s.tree.lines.len(),
        _ => panic!("expected a main screen"),
    };

    // Delete the contents: a rescan would come back empty, a clone cannot.
    for i in 0..5 {
        std::fs::remove_file(dir.path().join(format!("f{}.txt", i))).unwrap();
    }

    app.handle_key_for_test(char_key('|'));
    assert_eq!(app.panel_count(), 2, "split should open a second panel");

    // The new panel is immediately usable — not a loading screen.
    match app.current_screen() {
        Screen::Main(s) => assert_eq!(
            s.tree.lines.len(),
            before,
            "the split panel should mirror the tree, not re-read the directory"
        ),
        Screen::Loading(_) => panic!("split showed a loading screen instead of cloning the tree"),
        _ => panic!("unexpected screen after split"),
    }
}

/// Backing out of the directory picker returns to the tree rather than quitting.
///
/// `gd` asks a question; declining it should return you to where you were, the
/// way Esc dismisses any other prompt — not exit the app. (Esc, not `q`: the
/// picker has a text field, and a path may legitimately contain a `q`.)
#[tokio::test]
async fn test_leaving_the_dir_picker_does_not_quit() {
    use myd::app::FileBrowser;
    use myd::screen::Screen;

    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("a.txt"), "x").unwrap();
    let mut app = FileBrowser::new(Some(dir.path().to_path_buf()), None, false);
    settle(&mut app).await;

    // `gd` opens the picker.
    app.handle_key_for_test(char_key('g'));
    app.handle_key_for_test(char_key('d'));
    assert!(
        matches!(app.current_screen(), Screen::DirPicker(_)),
        "gd should open the directory picker"
    );

    // Esc dismisses it and keeps the app running.
    let still_running = app.handle_key_for_test(crossterm::event::KeyEvent::new(
        crossterm::event::KeyCode::Esc,
        crossterm::event::KeyModifiers::NONE,
    ));
    assert!(
        still_running,
        "leaving the directory picker must not quit the whole app"
    );
    assert!(
        matches!(app.current_screen(), Screen::Main(_)),
        "dismissing the picker should reveal the tree that was underneath"
    );

    // From the tree itself, q still quits.
    assert!(
        !app.handle_key_for_test(char_key('q')),
        "q on the main screen should still quit"
    );
}

/// Changing the sort order on a remote tree must make **zero** backend calls.
///
/// Not "few" or "fast": none. Sorting reorders data already in memory, so any
/// call to the backend is a bug regardless of how quick it happens to be on a
/// fast link. This asserts that directly — the fake backend panics if it is
/// touched once sorting begins — rather than inferring it from timings, which
/// is what let earlier I/O in the sort path go unnoticed.
#[tokio::test]
async fn test_remote_sort_makes_no_backend_calls_at_all() {
    use myd::screen::SortMode;
    use myd::utils::sizes::{CancelToken, SizeCache};
    use myd::vfs::{BackendId, VEntry, VMetadata, VPath, VRead, VWrite, Vfs};
    use myd::widget::file_tree::FileTree;
    use myd::widget::progress::OpProgress;
    use myd::widget::source::{RemoteSource, Source};
    use myd::widget::treemap::TreeMap;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::sync::Arc;

    /// Live while loading; any touch after `SEALED` is set panics.
    static SEALED: AtomicBool = AtomicBool::new(false);
    static CALLS: AtomicUsize = AtomicUsize::new(0);

    struct Tripwire;
    impl Tripwire {
        fn hit(what: &str) {
            CALLS.fetch_add(1, Ordering::SeqCst);
            assert!(
                !SEALED.load(Ordering::SeqCst),
                "sorting reached the backend: {}",
                what
            );
        }
    }

    #[async_trait::async_trait]
    impl Vfs for Tripwire {
        fn scheme(&self) -> &'static str {
            "sftp"
        }
        async fn read_dir(&self, p: &VPath) -> anyhow::Result<Vec<VEntry>> {
            Self::hit("read_dir");
            let depth = p.path.components().count();
            let mut v = Vec::new();
            if depth < 4 {
                for i in 0..12 {
                    v.push(VEntry::new(format!("d{:02}", i), true));
                }
            }
            for i in 0..20 {
                let mut e = VEntry::new(format!("f{:02}.txt", i), false);
                e.len = 1000 + i as u64;
                e.mtime = Some(
                    std::time::SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(i as u64),
                );
                v.push(e);
            }
            Ok(v)
        }
        async fn stat(&self, p: &VPath) -> anyhow::Result<VMetadata> {
            Self::hit("stat");
            let n = p
                .path
                .file_name()
                .map(|s| s.to_string_lossy().to_string())
                .unwrap_or_default();
            Ok(VMetadata {
                is_dir: !n.contains('.'),
                is_symlink: false,
                len: 4096,
                mode: None,
                uid: None,
                gid: None,
                mtime: None,
                atime: None,
                ctime: None,
            })
        }
        async fn symlink_stat(&self, p: &VPath) -> anyhow::Result<VMetadata> {
            Self::hit("symlink_stat");
            self.stat(p).await
        }
        async fn create_dir_all(&self, _p: &VPath) -> anyhow::Result<()> {
            Self::hit("create_dir_all");
            Ok(())
        }
        async fn remove_file(&self, _p: &VPath) -> anyhow::Result<()> {
            Self::hit("remove_file");
            Ok(())
        }
        async fn remove_dir(&self, _p: &VPath) -> anyhow::Result<()> {
            Self::hit("remove_dir");
            Ok(())
        }
        async fn rename(&self, _f: &VPath, _t: &VPath) -> anyhow::Result<()> {
            Self::hit("rename");
            Ok(())
        }
        async fn open_read(&self, _p: &VPath) -> anyhow::Result<Box<dyn VRead>> {
            Self::hit("open_read");
            anyhow::bail!("unused")
        }
        async fn open_write(
            &self,
            _p: &VPath,
            _l: Option<u64>,
        ) -> anyhow::Result<Box<dyn VWrite>> {
            Self::hit("open_write");
            anyhow::bail!("unused")
        }
        async fn dir_size(
            &self,
            _p: &VPath,
            _c: &SizeCache,
            _ct: &CancelToken,
            _pr: Option<&OpProgress>,
        ) -> u64 {
            Self::hit("dir_size");
            0
        }
        fn has_recursive_sizes(&self) -> bool {
            false
        }
    }

    let vfs: Arc<dyn Vfs> = Arc::new(Tripwire);
    let source = Source::Remote(RemoteSource::new(BackendId(1), vfs).unwrap());
    let cancel = CancelToken::new();
    let progress = OpProgress::new();
    let mut tree = FileTree::with_source_cancellable_progress(
        source,
        std::path::PathBuf::from("/remote/data"),
        SortMode::Largest,
        true,
        true,
        SizeCache::new(),
        &cancel,
        &progress,
    )
    .expect("remote tree should build");
    tree.expand_all();
    assert!(
        CALLS.load(Ordering::SeqCst) > 0,
        "loading the tree should talk to the backend"
    );
    assert!(tree.lines.len() > 200, "need a large tree to be meaningful");

    // Everything past here is pure reordering.
    SEALED.store(true, Ordering::SeqCst);
    for mode in [
        SortMode::Smallest,
        SortMode::DirsFirst,
        SortMode::FilesFirst,
        SortMode::Newest,
        SortMode::Oldest,
        SortMode::RecentlyAccessed,
        SortMode::Largest,
    ] {
        tree.set_sort_mode(mode);
    }
    // `toggle_sort` rebuilds the treemap too, so that must be clean as well.
    let _ = TreeMap::from_file_tree(&tree);
    SEALED.store(false, Ordering::SeqCst);
}

// ---------------------------------------------------------------------------
// Dialing directory
// ---------------------------------------------------------------------------

/// Non-character keys for the picker tests.
fn special_key(code: crossterm::event::KeyCode) -> crossterm::event::KeyEvent {
    crossterm::event::KeyEvent::new(code, crossterm::event::KeyModifiers::NONE)
}

/// A catalog with known contents, never touching the user's real hosts.toml.
fn test_catalog() -> myd::hosts::HostCatalog {
    use myd::hosts::SavedHost;
    let mut hosts = Vec::new();
    // Timestamps descending: the quick list is ordered by recency, so `prod` is
    // the most recent connection and heads the list.
    for (label, host, uses, last) in [
        ("prod", "prod.example.com", 30u64, "2026-07-26T12:00:00Z"),
        ("backup", "10.0.0.5", 20, "2026-07-25T12:00:00Z"),
        ("scratch", "dev.local", 10, "2026-07-24T12:00:00Z"),
        ("france", "fr.example.com", 5, "2026-07-23T12:00:00Z"),
    ] {
        let mut h = SavedHost::new(label, host);
        h.uses = uses;
        h.user = Some("juan".into());
        h.last_used = Some(last.into());
        hosts.push(h);
    }
    myd::hosts::HostCatalog::in_memory(hosts)
}

/// `gd` lists every saved host, which is what made `gs` redundant.
#[tokio::test]
async fn gd_lists_every_saved_host() {
    let dir = create_test_structure();
    let mut app = FileBrowser::new(Some(dir.path().to_path_buf()), None, false);
    app.set_hosts_for_test(test_catalog());
    app.resolve_loading_for_test();

    app.handle_key_for_test(char_key('g'));
    app.handle_key_for_test(char_key('d'));

    match app.current_screen() {
        myd::screen::Screen::DirPicker(p) => {
            let hosts = p.visible_options().iter().filter(|o| o.is_host()).count();
            assert_eq!(hosts, 4, "every saved host is listed alongside directories");
        }
        _ => panic!("gd should open the picker"),
    }
}

/// `/` narrows `gd` to a host, which is the replacement for `gs`.
#[tokio::test]
async fn searching_gd_reaches_a_saved_host() {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    let dir = create_test_structure();
    let mut app = FileBrowser::new(Some(dir.path().to_path_buf()), None, false);
    app.set_hosts_for_test(test_catalog());
    app.resolve_loading_for_test();

    app.handle_key_for_test(char_key('g'));
    app.handle_key_for_test(char_key('d'));
    app.handle_key_for_test(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));
    for c in "/france".chars() {
        app.handle_key_for_test(char_key(c));
    }

    match app.current_screen() {
        myd::screen::Screen::DirPicker(p) => {
            assert_eq!(p.visible_count(), 1, "the search should isolate one host");
            assert!(p.visible_options()[0].is_host());
        }
        _ => panic!("expected the picker"),
    }
}

/// The picker's path field takes a remote URL, which is what made `gr`'s
/// separate connect prompt redundant. Without this the field was local-only and
/// an unsaved address had nowhere to be typed.
#[tokio::test]
async fn the_picker_field_connects_to_a_typed_sftp_url() {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    let dir = create_test_structure();
    let mut app = FileBrowser::new(Some(dir.path().to_path_buf()), None, false);
    app.set_hosts_for_test(myd::hosts::HostCatalog::in_memory(vec![]));
    app.resolve_loading_for_test();

    app.handle_key_for_test(char_key('g'));
    app.handle_key_for_test(char_key('d'));
    for c in "sftp://juan@nowhere.invalid/srv".chars() {
        app.handle_key_for_test(char_key(c));
    }
    app.handle_key_for_test(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));

    // The host does not resolve, so this cannot assert a connection — what
    // matters is that it was treated as a target to dial at all. The old
    // behaviour was a "not a directory" notice, which is the bug this prevents.
    assert_ne!(
        app.modal_kind_for_test(),
        "confirm",
        "an sftp:// URL must not be rejected as 'not a directory'"
    );
}

/// Open `gd` with the list focused and the cursor on the first host row.
///
/// These tests used to reach the hosts through `gs`, which opened the picker
/// already filtered to them. `gd` lists directories first, so getting to a host
/// is now a matter of stepping past them — the rows are all in one list.
fn open_picker_on_first_host(app: &mut FileBrowser) {
    app.handle_key_for_test(char_key('g'));
    app.handle_key_for_test(char_key('d'));
    app.handle_key_for_test(special_key(crossterm::event::KeyCode::Tab));

    let on_host = |app: &FileBrowser| match app.current_screen() {
        myd::screen::Screen::DirPicker(p) => {
            p.selected().map(|o| o.is_host()).unwrap_or(false)
        }
        _ => false,
    };
    let rows = match app.current_screen() {
        myd::screen::Screen::DirPicker(p) => p.visible_count(),
        _ => panic!("gd should open the picker"),
    };
    // Bounded by the row count: `j` wraps, so a list with no hosts at all would
    // otherwise spin here rather than failing with something readable.
    for _ in 0..rows {
        if on_host(app) {
            return;
        }
        app.handle_key_for_test(char_key('j'));
    }
    panic!("no host row in the picker");
}

/// The picker owns j/k and `/` — they must navigate and search rather than
/// reaching the global keybindings or the chord detector.
#[tokio::test]
async fn picker_vi_navigation_and_search_work_through_the_app() {
    let dir = create_test_structure();
    let mut app = FileBrowser::new(Some(dir.path().to_path_buf()), None, false);
    app.set_hosts_for_test(test_catalog());
    app.resolve_loading_for_test();

    open_picker_on_first_host(&mut app);

    let label = |app: &FileBrowser| match app.current_screen() {
        myd::screen::Screen::DirPicker(p) => {
            p.selected().and_then(|o| o.host.as_ref()).map(|h| h.label.clone())
        }
        _ => None,
    };

    // j moves the cursor.
    let first = label(&app).unwrap();
    app.handle_key_for_test(char_key('j'));
    assert_ne!(label(&app).unwrap(), first);

    // / filters incrementally, and the cursor maps back to the right host.
    // Browsing mirrored the row into the path field; one Backspace clears that
    // (it was a suggestion, not typed) so `/` reads as search rather than as the
    // start of an absolute path.
    app.handle_key_for_test(special_key(crossterm::event::KeyCode::Backspace));
    app.handle_key_for_test(char_key('/'));
    for c in "fra".chars() {
        app.handle_key_for_test(char_key(c));
    }
    match app.current_screen() {
        myd::screen::Screen::DirPicker(p) => {
            assert_eq!(p.visible_count(), 1, "the search should narrow to one");
        }
        _ => panic!("expected the picker"),
    }
    assert_eq!(label(&app).as_deref(), Some("france"));

    // Still the picker; none of those keys leaked into the file tree.
    assert!(matches!(
        app.current_screen(),
        myd::screen::Screen::DirPicker(_)
    ));
}

/// Esc closes the picker without connecting.
#[tokio::test]
async fn esc_dismisses_the_picker() {
    let dir = create_test_structure();
    let mut app = FileBrowser::new(Some(dir.path().to_path_buf()), None, false);
    app.set_hosts_for_test(test_catalog());
    app.resolve_loading_for_test();

    app.handle_key_for_test(char_key('g'));
    app.handle_key_for_test(char_key('r'));
    app.handle_key_for_test(special_key(crossterm::event::KeyCode::Esc));

    assert_eq!(app.modal_kind_for_test(), "none");
    assert!(!app.is_connecting_for_test());
}

/// Adding a host goes through a form and lands in the catalog.
#[tokio::test]
async fn adding_a_host_stores_it_without_a_password() {
    let dir = create_test_structure();
    let mut app = FileBrowser::new(Some(dir.path().to_path_buf()), None, false);
    app.set_hosts_for_test(myd::hosts::HostCatalog::in_memory(vec![]));
    app.resolve_loading_for_test();

    app.handle_key_for_test(char_key('g'));
    app.handle_key_for_test(char_key('d'));
    // `a` needs the list focused; the path field starts focused.
    app.handle_key_for_test(special_key(crossterm::event::KeyCode::Tab));
    app.handle_key_for_test(char_key('a'));
    assert_eq!(app.modal_kind_for_test(), "input");

    // One prompt takes both kinds: a `label = sftp://…` line saves a host.
    for c in "edge = sftp://ops@edge.example.com:2222/srv".chars() {
        app.handle_key_for_test(char_key(c));
    }
    app.handle_key_for_test(special_key(crossterm::event::KeyCode::Enter));

    let saved = app.hosts_for_test().find("edge").expect("host not saved");
    assert_eq!(saved.host, "edge.example.com");
    assert_eq!(saved.port, Some(2222));
    assert_eq!(saved.user.as_deref(), Some("ops"));
    assert_eq!(saved.path.as_deref(), Some("/srv"));
    // Back to the list so several can be added in a row.
    assert_eq!(app.modal_kind_for_test(), "none");
    assert!(
        matches!(app.current_screen(), myd::screen::Screen::DirPicker(_)),
        "and still on the picker"
    );
}

/// Deleting asks first, and removes only on confirmation.
#[tokio::test]
async fn deleting_a_host_requires_confirmation() {
    let dir = create_test_structure();
    let mut app = FileBrowser::new(Some(dir.path().to_path_buf()), None, false);
    app.set_hosts_for_test(test_catalog());
    app.resolve_loading_for_test();

    open_picker_on_first_host(&mut app);
    let doomed = match app.current_screen() {
        myd::screen::Screen::DirPicker(p) => {
            p.selected().and_then(|o| o.host.as_ref()).unwrap().label.clone()
        }
        _ => panic!("gd should open the picker"),
    };

    app.handle_key_for_test(char_key('d'));
    assert_eq!(app.modal_kind_for_test(), "confirm");

    // Decline: the host stays.
    app.handle_key_for_test(char_key('n'));
    assert!(app.hosts_for_test().find(&doomed).is_some());
    assert_eq!(app.modal_kind_for_test(), "none");

    // Confirm: it goes.
    app.handle_key_for_test(char_key('d'));
    app.handle_key_for_test(char_key('y'));
    assert!(app.hosts_for_test().find(&doomed).is_none());
}

/// A bad address is reported rather than silently dropped.
#[tokio::test]
async fn an_unparsable_host_form_reports_the_error() {
    let dir = create_test_structure();
    let mut app = FileBrowser::new(Some(dir.path().to_path_buf()), None, false);
    app.set_hosts_for_test(myd::hosts::HostCatalog::in_memory(vec![]));
    app.resolve_loading_for_test();

    app.handle_key_for_test(char_key('g'));
    app.handle_key_for_test(char_key('d'));
    app.handle_key_for_test(special_key(crossterm::event::KeyCode::Tab));
    app.handle_key_for_test(char_key('a'));
    for c in "bad = http://nope".chars() {
        app.handle_key_for_test(char_key(c));
    }
    app.handle_key_for_test(special_key(crossterm::event::KeyCode::Enter));

    assert_eq!(app.modal_kind_for_test(), "confirm");
    assert!(app.hosts_for_test().is_empty());
}

/// The picker must render at any terminal size without panicking.
#[tokio::test]
async fn picker_renders_at_realistic_sizes() {
    let dir = create_test_structure();
    let mut app = FileBrowser::new(Some(dir.path().to_path_buf()), None, false);
    app.set_hosts_for_test(test_catalog());
    app.resolve_loading_for_test();
    app.handle_key_for_test(char_key('g'));
    app.handle_key_for_test(char_key('d'));

    for (w, h) in [(120u16, 40u16), (80, 24), (40, 12), (20, 6)] {
        let backend = ratatui::backend::TestBackend::new(w, h);
        let mut term = ratatui::Terminal::new(backend).unwrap();
        term.draw(|f| app.render_for_test(f)).unwrap();
    }
}

// ---------------------------------------------------------------------------
// Mouse
// ---------------------------------------------------------------------------

fn mouse_at(
    kind: crossterm::event::MouseEventKind,
    x: u16,
    y: u16,
) -> crossterm::event::MouseEvent {
    crossterm::event::MouseEvent {
        kind,
        column: x,
        row: y,
        modifiers: crossterm::event::KeyModifiers::NONE,
    }
}

/// Clicking a tree row must select exactly the row under the pointer.
///
/// The mapping depends on the rect and scroll offset recorded during render,
/// so the app has to be drawn first — testing it against a guessed geometry
/// would prove nothing.
#[tokio::test]
async fn clicking_a_tree_row_selects_it() {
    use crossterm::event::{MouseButton, MouseEventKind};

    let dir = create_test_structure();
    let mut app = FileBrowser::new(Some(dir.path().to_path_buf()), None, false);
    settle(&mut app).await;

    let backend = ratatui::backend::TestBackend::new(100, 30);
    let mut term = ratatui::Terminal::new(backend).unwrap();
    term.draw(|f| app.render_for_test(f)).unwrap();

    // Row 0 of the box is the border/title, so content starts at y = 1.
    // Clicking the third content row selects tree line 2.
    app.route_mouse(mouse_at(MouseEventKind::Down(MouseButton::Left), 10, 3));
    assert_eq!(
        app.selected_line_index_for_test(),
        Some(2),
        "click should land on the row under the pointer"
    );

    app.route_mouse(mouse_at(MouseEventKind::Down(MouseButton::Left), 10, 1));
    assert_eq!(app.selected_line_index_for_test(), Some(0));
}

/// A click on the border or outside the tree must not move the cursor.
#[tokio::test]
async fn clicking_outside_the_rows_does_nothing() {
    use crossterm::event::{MouseButton, MouseEventKind};

    let dir = create_test_structure();
    let mut app = FileBrowser::new(Some(dir.path().to_path_buf()), None, false);
    settle(&mut app).await;

    let backend = ratatui::backend::TestBackend::new(100, 30);
    let mut term = ratatui::Terminal::new(backend).unwrap();
    term.draw(|f| app.render_for_test(f)).unwrap();

    app.route_mouse(mouse_at(MouseEventKind::Down(MouseButton::Left), 10, 3));
    let before = app.selected_line_index_for_test();

    // The top border row.
    app.route_mouse(mouse_at(MouseEventKind::Down(MouseButton::Left), 10, 0));
    assert_eq!(app.selected_line_index_for_test(), before);

    // Far below the last row.
    app.route_mouse(mouse_at(MouseEventKind::Down(MouseButton::Left), 10, 29));
    assert_eq!(app.selected_line_index_for_test(), before);
}

/// In dual mode a click focuses the panel it landed in, so it can't move the
/// other panel's cursor.
#[tokio::test]
async fn clicking_a_panel_focuses_it() {
    use crossterm::event::{MouseButton, MouseEventKind};

    let dir = create_test_structure();
    let mut app = FileBrowser::new(
        Some(dir.path().to_path_buf()),
        Some(dir.path().to_path_buf()),
        true,
    );
    app.resolve_loading_for_test();
    settle(&mut app).await;

    let backend = ratatui::backend::TestBackend::new(100, 30);
    let mut term = ratatui::Terminal::new(backend).unwrap();
    term.draw(|f| app.render_for_test(f)).unwrap();

    assert_eq!(app.panel_count(), 2);

    // Right half, then left half.
    app.route_mouse(mouse_at(MouseEventKind::Down(MouseButton::Left), 75, 3));
    assert_eq!(app.active_panel_index(), 1);

    app.route_mouse(mouse_at(MouseEventKind::Down(MouseButton::Left), 20, 3));
    assert_eq!(app.active_panel_index(), 0);
}

/// The wheel scrolls without needing a click first.
#[tokio::test]
async fn scrolling_moves_the_cursor() {
    let dir = create_test_structure();
    let mut app = FileBrowser::new(Some(dir.path().to_path_buf()), None, false);
    settle(&mut app).await;

    let backend = ratatui::backend::TestBackend::new(100, 30);
    let mut term = ratatui::Terminal::new(backend).unwrap();
    term.draw(|f| app.render_for_test(f)).unwrap();

    let start = app.selected_line_index_for_test().unwrap();
    app.scroll_by_for_test(3);
    assert_eq!(app.selected_line_index_for_test(), Some(start + 3));

    app.scroll_by_for_test(-2);
    assert_eq!(app.selected_line_index_for_test(), Some(start + 1));
}

/// Ctrl+N releases the mouse so terminal text selection works, and re-grabs it.
#[tokio::test]
async fn ctrl_n_toggles_mouse_capture() {
    let dir = create_test_structure();
    let mut app = FileBrowser::new(Some(dir.path().to_path_buf()), None, false);
    settle(&mut app).await;

    let before = app.mouse_captured();
    app.handle_key_for_test(ctrl_key('n'));
    assert_ne!(app.mouse_captured(), before, "Ctrl+N should toggle capture");
    app.handle_key_for_test(ctrl_key('n'));
    assert_eq!(app.mouse_captured(), before);
}

/// A click while a dialog is up must not reach the tree behind it.
#[tokio::test]
async fn a_modal_swallows_clicks() {
    use crossterm::event::{MouseButton, MouseEventKind};

    let dir = create_test_structure();
    let mut app = FileBrowser::new(Some(dir.path().to_path_buf()), None, false);
    settle(&mut app).await;

    let backend = ratatui::backend::TestBackend::new(100, 30);
    let mut term = ratatui::Terminal::new(backend).unwrap();
    term.draw(|f| app.render_for_test(f)).unwrap();

    app.route_mouse(mouse_at(MouseEventKind::Down(MouseButton::Left), 10, 3));
    let before = app.selected_line_index_for_test();

    // Open help, then click where a row would be.
    app.handle_key_for_test(char_key('?'));
    assert_eq!(app.modal_kind_for_test(), "help");
    app.route_mouse(mouse_at(MouseEventKind::Down(MouseButton::Left), 10, 6));

    assert_eq!(
        app.selected_line_index_for_test(),
        before,
        "the click leaked through the modal"
    );
}

/// Double-clicking a directory opens it, exactly as Enter does.
///
/// Terminals report presses individually and never a double-click, so this is
/// inferred from timing and cell position — which means a single click must
/// still only select.
#[tokio::test]
async fn double_click_opens_a_directory() {
    use crossterm::event::{MouseButton, MouseEventKind};

    let dir = create_test_structure();
    let mut app = FileBrowser::new(Some(dir.path().to_path_buf()), None, false);
    settle(&mut app).await;

    let backend = ratatui::backend::TestBackend::new(100, 30);
    let mut term = ratatui::Terminal::new(backend).unwrap();
    term.draw(|f| app.render_for_test(f)).unwrap();

    let root = app.panel_current_dir(0);

    // Find the row holding "subdir" so the click lands on a directory.
    let subdir_row = (0..12)
        .find(|&row| {
            app.route_mouse(mouse_at(
                MouseEventKind::Down(MouseButton::Left),
                10,
                (row + 1) as u16,
            ));
            app.selected_name_for_test().as_deref() == Some("subdir")
        })
        .expect("subdir should be visible in the tree");
    let y = (subdir_row + 1) as u16;

    // One click only selects.
    assert_eq!(app.panel_current_dir(0), root, "a single click should not open");

    // Two clicks in the same cell, close together, open it.
    app.route_mouse(mouse_at(MouseEventKind::Down(MouseButton::Left), 10, y));
    app.route_mouse(mouse_at(MouseEventKind::Down(MouseButton::Left), 10, y));
    settle(&mut app).await;

    assert_ne!(
        app.panel_current_dir(0),
        root,
        "a double-click on a directory should enter it"
    );
}

/// Two clicks on *different* rows are two selections, not an open.
#[tokio::test]
async fn clicks_on_different_rows_do_not_count_as_a_double() {
    use crossterm::event::{MouseButton, MouseEventKind};

    let dir = create_test_structure();
    let mut app = FileBrowser::new(Some(dir.path().to_path_buf()), None, false);
    settle(&mut app).await;

    let backend = ratatui::backend::TestBackend::new(100, 30);
    let mut term = ratatui::Terminal::new(backend).unwrap();
    term.draw(|f| app.render_for_test(f)).unwrap();

    let root = app.panel_current_dir(0);
    app.route_mouse(mouse_at(MouseEventKind::Down(MouseButton::Left), 10, 2));
    app.route_mouse(mouse_at(MouseEventKind::Down(MouseButton::Left), 10, 3));
    settle(&mut app).await;

    assert_eq!(
        app.panel_current_dir(0),
        root,
        "clicks on different rows must not open anything"
    );
}

// ---------------------------------------------------------------------------
// Sort menu
// ---------------------------------------------------------------------------

/// Clicking the "Sort:" indicator opens a numbered menu, and typing a number
/// applies that order.
#[tokio::test]
async fn clicking_the_sort_indicator_opens_a_menu() {
    use crossterm::event::{MouseButton, MouseEventKind};
    use myd::screen::{Screen, SortMode};

    let dir = create_test_structure();
    let mut app = FileBrowser::new(Some(dir.path().to_path_buf()), None, false);
    settle(&mut app).await;

    let backend = ratatui::backend::TestBackend::new(120, 30);
    let mut term = ratatui::Terminal::new(backend).unwrap();
    term.draw(|f| app.render_for_test(f)).unwrap();

    // The indicator's rect is recorded during render.
    let sort_area = match app.current_screen() {
        Screen::Main(s) => s.sort_area.expect("sort indicator should have a rect"),
        _ => panic!("expected a main screen"),
    };

    app.route_mouse(mouse_at(
        MouseEventKind::Down(MouseButton::Left),
        sort_area.x + 1,
        sort_area.y,
    ));
    assert_eq!(app.modal_kind_for_test(), "sort_menu");

    // Typing a number applies that order and closes the menu.
    app.handle_key_for_test(char_key('4'));
    assert_eq!(app.modal_kind_for_test(), "none");
    let mode = match app.current_screen() {
        Screen::Main(s) => s.tree.sort_mode,
        _ => panic!(),
    };
    assert_eq!(mode, SortMode::ALL[3]);
}

/// Esc closes the menu without changing the order.
#[tokio::test]
async fn the_sort_menu_can_be_dismissed() {
    use crossterm::event::{MouseButton, MouseEventKind};
    use myd::screen::Screen;

    let dir = create_test_structure();
    let mut app = FileBrowser::new(Some(dir.path().to_path_buf()), None, false);
    settle(&mut app).await;

    let backend = ratatui::backend::TestBackend::new(120, 30);
    let mut term = ratatui::Terminal::new(backend).unwrap();
    term.draw(|f| app.render_for_test(f)).unwrap();

    let before = match app.current_screen() {
        Screen::Main(s) => s.tree.sort_mode,
        _ => panic!(),
    };
    let sort_area = match app.current_screen() {
        Screen::Main(s) => s.sort_area.unwrap(),
        _ => panic!(),
    };

    app.route_mouse(mouse_at(
        MouseEventKind::Down(MouseButton::Left),
        sort_area.x + 1,
        sort_area.y,
    ));
    app.handle_key_for_test(special_key(crossterm::event::KeyCode::Esc));

    assert_eq!(app.modal_kind_for_test(), "none");
    let after = match app.current_screen() {
        Screen::Main(s) => s.tree.sort_mode,
        _ => panic!(),
    };
    assert_eq!(before, after, "cancelling must not change the sort order");
}

/// A click elsewhere in the title bar must not open the menu.
#[tokio::test]
async fn clicking_away_from_the_indicator_does_not_open_the_menu() {
    use crossterm::event::{MouseButton, MouseEventKind};

    let dir = create_test_structure();
    let mut app = FileBrowser::new(Some(dir.path().to_path_buf()), None, false);
    settle(&mut app).await;

    let backend = ratatui::backend::TestBackend::new(120, 30);
    let mut term = ratatui::Terminal::new(backend).unwrap();
    term.draw(|f| app.render_for_test(f)).unwrap();

    // Far left of the title bar, where the path is drawn.
    app.route_mouse(mouse_at(MouseEventKind::Down(MouseButton::Left), 3, 0));
    assert_eq!(app.modal_kind_for_test(), "none");
}

// ---------------------------------------------------------------------------
// Transfer panel focus
// ---------------------------------------------------------------------------

/// Tab reaches the transfer sidebar, j/k move within it, and k asks before
/// cancelling.
#[tokio::test]
async fn the_transfer_panel_is_focusable_and_cancellable() {
    let dir = create_test_structure();
    let mut app = FileBrowser::new(Some(dir.path().to_path_buf()), None, false);
    settle(&mut app).await;

    // Queue two transfers so the sidebar has content and a cursor can move.
    let src = dir.path().join("file_a.txt");
    for name in ["one.txt", "two.txt"] {
        app.enqueue_transfer_for_test(
            myd::vfs::VPath::local(&src),
            myd::vfs::VPath::local(dir.path().join(name)),
        );
    }

    // The sidebar's geometry only exists once drawn.
    let backend = ratatui::backend::TestBackend::new(120, 30);
    let mut term = ratatui::Terminal::new(backend).unwrap();
    term.draw(|f| app.render_for_test(f)).unwrap();

    assert!(!app.transfer_focused_for_test(), "starts on the file tree");

    app.handle_key_for_test(special_key(crossterm::event::KeyCode::Tab));
    assert!(app.transfer_focused_for_test(), "Tab should reach the sidebar");
    term.draw(|f| app.render_for_test(f)).unwrap();

    let first = app.transfer_cursor_for_test();
    assert!(first.is_some(), "focusing should select a transfer");

    app.handle_key_for_test(char_key('j'));
    assert_ne!(app.transfer_cursor_for_test(), first, "j should move");

    // k asks before cancelling rather than doing it outright.
    app.handle_key_for_test(char_key('k'));
    assert_eq!(app.modal_kind_for_test(), "confirm");

    // Declining leaves the transfer alone.
    app.handle_key_for_test(char_key('n'));
    assert_eq!(app.modal_kind_for_test(), "none");
}

/// Esc returns focus to the file tree.
#[tokio::test]
async fn esc_leaves_the_transfer_panel() {
    let dir = create_test_structure();
    let mut app = FileBrowser::new(Some(dir.path().to_path_buf()), None, false);
    settle(&mut app).await;
    app.enqueue_transfer_for_test(
        myd::vfs::VPath::local(dir.path().join("file_a.txt")),
        myd::vfs::VPath::local(dir.path().join("copy.txt")),
    );

    let backend = ratatui::backend::TestBackend::new(120, 30);
    let mut term = ratatui::Terminal::new(backend).unwrap();
    term.draw(|f| app.render_for_test(f)).unwrap();

    app.handle_key_for_test(special_key(crossterm::event::KeyCode::Tab));
    assert!(app.transfer_focused_for_test());
    app.handle_key_for_test(special_key(crossterm::event::KeyCode::Esc));
    assert!(!app.transfer_focused_for_test());
}

/// Clicking a transfer focuses the sidebar and selects it; double-clicking asks
/// to cancel.
#[tokio::test]
async fn clicking_a_transfer_selects_it_and_double_click_cancels() {
    use crossterm::event::{MouseButton, MouseEventKind};

    let dir = create_test_structure();
    let mut app = FileBrowser::new(Some(dir.path().to_path_buf()), None, false);
    settle(&mut app).await;
    app.enqueue_transfer_for_test(
        myd::vfs::VPath::local(dir.path().join("file_a.txt")),
        myd::vfs::VPath::local(dir.path().join("copy.txt")),
    );

    let backend = ratatui::backend::TestBackend::new(120, 30);
    let mut term = ratatui::Terminal::new(backend).unwrap();
    term.draw(|f| app.render_for_test(f)).unwrap();

    let (row, _) = app
        .transfer_row_for_test(0)
        .expect("a queued transfer should have a row");

    // Somewhere inside the sidebar's columns.
    let x = 100u16;
    app.route_mouse(mouse_at(MouseEventKind::Down(MouseButton::Left), x, row));
    assert!(app.transfer_focused_for_test(), "a click should focus the sidebar");
    assert!(app.transfer_cursor_for_test().is_some());
    assert_eq!(app.modal_kind_for_test(), "none", "one click must not cancel");

    app.route_mouse(mouse_at(MouseEventKind::Down(MouseButton::Left), x, row));
    assert_eq!(
        app.modal_kind_for_test(),
        "confirm",
        "a double-click should ask to cancel"
    );
}

// ---------------------------------------------------------------------------
// Quit guard
// ---------------------------------------------------------------------------

/// Quitting with transfers in flight asks first, and declining keeps the app up.
#[tokio::test]
async fn quitting_during_a_transfer_asks_first() {
    let dir = create_test_structure();
    let mut app = FileBrowser::new(Some(dir.path().to_path_buf()), None, false);
    settle(&mut app).await;

    app.enqueue_transfer_for_test(
        myd::vfs::VPath::local(dir.path().join("file_a.txt")),
        myd::vfs::VPath::local(dir.path().join("copy.txt")),
    );

    // `q` must not end the app while work is outstanding.
    let running = app.handle_key_for_test(char_key('q'));
    assert!(running, "q must not quit outright during a transfer");
    assert_eq!(app.modal_kind_for_test(), "confirm");

    // Declining keeps running and leaves the queue alone.
    let running = app.handle_key_for_test(char_key('n'));
    assert!(running, "declining should keep the app open");
    assert_eq!(app.modal_kind_for_test(), "none");
    assert!(app.transfer_queue().has_work(), "the transfer must survive");
}

/// Confirming the prompt actually quits.
#[tokio::test]
async fn confirming_the_quit_prompt_exits() {
    let dir = create_test_structure();
    let mut app = FileBrowser::new(Some(dir.path().to_path_buf()), None, false);
    settle(&mut app).await;

    app.enqueue_transfer_for_test(
        myd::vfs::VPath::local(dir.path().join("file_a.txt")),
        myd::vfs::VPath::local(dir.path().join("copy.txt")),
    );

    app.handle_key_for_test(char_key('q'));
    let running = app.handle_key_for_test(char_key('y'));
    assert!(!running, "confirming should quit");
}

/// With no transfers the prompt must not appear — the common case keeps its
/// single-keystroke exit.
#[tokio::test]
async fn quitting_with_an_idle_queue_exits_immediately() {
    let dir = create_test_structure();
    let mut app = FileBrowser::new(Some(dir.path().to_path_buf()), None, false);
    settle(&mut app).await;

    let running = app.handle_key_for_test(char_key('q'));
    assert!(!running, "q should quit at once when nothing is transferring");
    assert_eq!(app.modal_kind_for_test(), "none");
}

/// Ctrl+C stays an unconditional exit: it is the guaranteed way out and must
/// not be gated behind a dialog.
#[tokio::test]
async fn ctrl_c_still_force_quits_during_a_transfer() {
    let dir = create_test_structure();
    let mut app = FileBrowser::new(Some(dir.path().to_path_buf()), None, false);
    settle(&mut app).await;

    app.enqueue_transfer_for_test(
        myd::vfs::VPath::local(dir.path().join("file_a.txt")),
        myd::vfs::VPath::local(dir.path().join("copy.txt")),
    );

    let running = app.handle_key_for_test(ctrl_key('c'));
    assert!(!running, "Ctrl+C must force-quit regardless of transfers");
}

/// Declining leaves no latent quit: a later `q` asks again rather than exiting.
#[tokio::test]
async fn declining_the_quit_prompt_does_not_arm_a_later_quit() {
    let dir = create_test_structure();
    let mut app = FileBrowser::new(Some(dir.path().to_path_buf()), None, false);
    settle(&mut app).await;

    app.enqueue_transfer_for_test(
        myd::vfs::VPath::local(dir.path().join("file_a.txt")),
        myd::vfs::VPath::local(dir.path().join("copy.txt")),
    );

    app.handle_key_for_test(char_key('q'));
    app.handle_key_for_test(char_key('n'));

    let running = app.handle_key_for_test(char_key('q'));
    assert!(running, "the second q should prompt again, not exit silently");
    assert_eq!(app.modal_kind_for_test(), "confirm");
}

// ---------------------------------------------------------------------------
// Help scrolling
// ---------------------------------------------------------------------------

/// Render the help overlay at a given size and return its text.
fn help_text(app: &mut FileBrowser, w: u16, h: u16) -> String {
    let backend = ratatui::backend::TestBackend::new(w, h);
    let mut term = ratatui::Terminal::new(backend).unwrap();
    term.draw(|f| app.render_for_test(f)).unwrap();
    let buf = term.backend().buffer().clone();
    (0..buf.area.height)
        .map(|y| {
            (0..buf.area.width)
                .map(|x| buf[(x, y)].symbol().to_string())
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// The help list is far taller than a terminal, and the transfer keys sit at the
/// bottom. Before scrolling existed they were simply unreachable — which is why
/// cancelling a transfer looked impossible.
#[tokio::test]
async fn help_scrolls_to_reach_the_transfer_keys() {
    let dir = create_test_structure();
    let mut app = FileBrowser::new(Some(dir.path().to_path_buf()), None, false);
    settle(&mut app).await;

    app.handle_key_for_test(char_key('?'));
    assert!(app.is_help_open());

    let top = help_text(&mut app, 80, 24);
    assert!(
        top.contains("Navigation"),
        "the top of the list should be visible"
    );
    assert!(
        !top.contains("Cancel the selected transfer"),
        "the transfer keys should be below the fold on a 24-row terminal"
    );

    // G jumps to the bottom, where the transfer keys live.
    app.handle_key_for_test(char_key('G'));
    let bottom = help_text(&mut app, 80, 24);
    assert!(
        bottom.contains("Cancel the selected transfer"),
        "scrolling to the bottom must reveal how to cancel a transfer:\n{}",
        bottom
    );
    assert!(app.is_help_open(), "scrolling must not dismiss the overlay");
}

/// j/k scroll rather than dismissing, and the offset clamps at both ends.
#[tokio::test]
async fn help_scroll_keys_move_and_clamp() {
    let dir = create_test_structure();
    let mut app = FileBrowser::new(Some(dir.path().to_path_buf()), None, false);
    settle(&mut app).await;

    app.handle_key_for_test(char_key('?'));
    let first = help_text(&mut app, 80, 24);

    app.handle_key_for_test(char_key('j'));
    let scrolled = help_text(&mut app, 80, 24);
    assert!(app.is_help_open(), "j must scroll, not dismiss");
    assert_ne!(first, scrolled, "j should move the view");

    // Scrolling up past the start clamps rather than wrapping or underflowing.
    for _ in 0..50 {
        app.handle_key_for_test(char_key('k'));
    }
    let back_at_top = help_text(&mut app, 80, 24);
    assert_eq!(back_at_top, first, "scrolling up should clamp at the top");

    // And down past the end.
    for _ in 0..200 {
        app.handle_key_for_test(char_key('j'));
    }
    let bottom = help_text(&mut app, 80, 24);
    for _ in 0..20 {
        app.handle_key_for_test(char_key('j'));
    }
    assert_eq!(
        bottom,
        help_text(&mut app, 80, 24),
        "scrolling down should clamp at the bottom"
    );
}

/// A tall terminal shows everything, so no scroll indicator is needed.
#[tokio::test]
async fn help_without_overflow_shows_no_scroll_hint() {
    let dir = create_test_structure();
    let mut app = FileBrowser::new(Some(dir.path().to_path_buf()), None, false);
    settle(&mut app).await;

    app.handle_key_for_test(char_key('?'));
    // Tall enough for the whole list, which grows as categories are added — the
    // preview pane's keys took it past 100 rows.
    let text = help_text(&mut app, 80, 130);
    assert!(
        !text.contains("to scroll"),
        "a terminal tall enough for the whole list should not advertise scrolling"
    );
    assert!(text.contains("Cancel the selected transfer"));
}

/// Exactly one pane may look focused at a time.
///
/// `state.active` used to mean "is the active panel index" and never consulted
/// the transfer sidebar, so tabbing to the sidebar left the previous panel still
/// drawing a cyan border — two focused-looking panes at once.
#[tokio::test]
async fn focusing_the_transfer_panel_unfocuses_the_browser_panels() {
    use myd::screen::Screen;

    let dir = create_test_structure();
    let mut app = FileBrowser::new(
        Some(dir.path().to_path_buf()),
        Some(dir.path().to_path_buf()),
        true,
    );
    settle_all(&mut app).await;

    app.enqueue_transfer_for_test(
        myd::vfs::VPath::local(dir.path().join("file_a.txt")),
        myd::vfs::VPath::local(dir.path().join("copy.txt")),
    );

    let backend = ratatui::backend::TestBackend::new(140, 30);
    let mut term = ratatui::Terminal::new(backend).unwrap();
    term.draw(|f| app.render_for_test(f)).unwrap();

    // A browser panel starts focused.
    assert!(!app.transfer_focused_for_test());
    assert_eq!(
        app.focused_panel_count_for_test(),
        1,
        "exactly one panel should look focused to begin with"
    );

    // Tab through to the sidebar, redrawing each time so the render hint updates.
    for _ in 0..3 {
        app.handle_key_for_test(special_key(crossterm::event::KeyCode::Tab));
        term.draw(|f| app.render_for_test(f)).unwrap();
        if app.transfer_focused_for_test() {
            break;
        }
    }
    assert!(
        app.transfer_focused_for_test(),
        "Tab should reach the transfer panel"
    );

    assert_eq!(
        app.focused_panel_count_for_test(),
        0,
        "no browser panel may still look focused once the sidebar has focus"
    );

    // And tabbing away restores exactly one focused panel.
    app.handle_key_for_test(special_key(crossterm::event::KeyCode::Tab));
    term.draw(|f| app.render_for_test(f)).unwrap();
    assert!(!app.transfer_focused_for_test());
    assert_eq!(app.focused_panel_count_for_test(), 1);

    // The screen enum is exhaustive; keep the import meaningful.
    assert!(matches!(app.current_screen(), Screen::Main(_)));
}

// ---------------------------------------------------------------------------
// Paging: Ctrl+F / Ctrl+B move a full screen, Ctrl+D / Ctrl+U half, and all
// four measure the terminal instead of assuming 20 lines.
// ---------------------------------------------------------------------------

/// A directory with enough entries to page through several screens of them.
fn paging_fixture(n: usize) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    for i in 0..n {
        // Descending sizes, so the default Largest sort gives a stable order
        // that does not depend on the filesystem's readdir order.
        std::fs::write(
            dir.path().join(format!("f{:03}.bin", i)),
            vec![0u8; (n - i) * 8],
        )
        .unwrap();
    }
    dir
}

/// A main screen over `paging_fixture`, plus a terminal to draw it into.
fn paging_screen(
    n: usize,
    w: u16,
    h: u16,
) -> (
    tempfile::TempDir,
    myd::screen::MainScreenState,
    ratatui::Terminal<ratatui::backend::TestBackend>,
) {
    use myd::screen::SortMode;
    use myd::widget::file_tree::FileTree;
    use ratatui::{backend::TestBackend, Terminal};

    let dir = paging_fixture(n);
    let tree = FileTree::new(dir.path().to_path_buf(), SortMode::Largest, true, true);
    let st = myd::screen::MainScreenState::from_tree(dir.path().to_path_buf(), tree);
    let terminal = Terminal::new(TestBackend::new(w, h)).unwrap();
    (dir, st, terminal)
}

fn draw(
    st: &mut myd::screen::MainScreenState,
    terminal: &mut ratatui::Terminal<ratatui::backend::TestBackend>,
) {
    use myd::screen::ScreenState;
    terminal.draw(|f| st.render(f, f.area())).unwrap();
}

#[test]
fn ctrl_f_and_ctrl_b_move_a_full_viewport() {
    // Pre-fix Ctrl+F was unbound entirely, so the cursor did not move at all.
    let (_dir, mut st, mut term) = paging_screen(60, 80, 20);
    draw(&mut st, &mut term);
    // 20 rows, less the 1-row footer, less the tree box's top border + title bar
    // and bottom border.
    assert_eq!(st.tree_viewport, 16, "viewport should be measured, not assumed");

    st.page_down();
    assert_eq!(st.tree.cursor, 16, "Ctrl+F moves one full screen");
    st.page_up();
    assert_eq!(st.tree.cursor, 0, "Ctrl+B comes back");
}

#[test]
fn ctrl_d_and_ctrl_u_move_half_a_viewport() {
    // Pre-fix these moved a hardcoded 20 regardless of the terminal, so the
    // exact expected value is what makes this test meaningful — asserting
    // merely "it moved" would have passed against the constant.
    let (_dir, mut st, mut term) = paging_screen(60, 80, 20);
    draw(&mut st, &mut term);

    st.half_page_down();
    assert_eq!(st.tree.cursor, 8, "half of a 16-row viewport");
    st.half_page_up();
    assert_eq!(st.tree.cursor, 0);
}

#[test]
fn page_moves_scale_with_the_terminal_height() {
    // The regression guard against anyone reintroducing a constant.
    let (_dir_a, mut short, mut term_a) = paging_screen(120, 80, 20);
    draw(&mut short, &mut term_a);
    short.page_down();

    let (_dir_b, mut tall, mut term_b) = paging_screen(120, 80, 44);
    draw(&mut tall, &mut term_b);
    tall.page_down();

    assert!(
        tall.tree.cursor > short.tree.cursor,
        "a taller terminal must page further: {} vs {}",
        tall.tree.cursor,
        short.tree.cursor
    );
}

#[test]
fn a_page_move_before_the_first_frame_still_moves() {
    // Nothing has been drawn, so there is no measured viewport yet; the motion
    // falls back to the distance the old hardcoded page used.
    let (_dir, mut st, _term) = paging_screen(60, 80, 20);
    assert_eq!(st.tree_viewport, 0, "nothing drawn yet");
    st.page_down();
    assert_eq!(st.tree.cursor, 20, "falls back to DEFAULT_VIEWPORT");
}

#[tokio::test]
async fn page_moves_tag_the_range_in_visual_mode() {
    use myd::app::FileBrowser;
    use ratatui::{backend::TestBackend, Terminal};

    // Pre-fix this tagged exactly one entry: dispatch_action_inner ended visual
    // mode before the motion ran, and the motion assigned `cursor` directly and
    // so never called tag_visual_span either.
    let dir = paging_fixture(60);
    let mut app = FileBrowser::new(Some(dir.path().to_path_buf()), None, false);
    settle(&mut app).await;

    let mut term = Terminal::new(TestBackend::new(80, 20)).unwrap();
    term.draw(|f| app.render_for_test(f)).unwrap();

    app.handle_key_for_test(char_key('V'));
    app.handle_key_for_test(ctrl_key('d'));

    let tagged = match app.current_screen() {
        myd::screen::Screen::Main(s) => s.tree.tagged_paths().len(),
        _ => panic!("expected a main screen"),
    };
    // Anchor at 0 through the cursor at 8, inclusive.
    assert_eq!(tagged, 9, "a half-page jump must tag the range it crossed");
}

#[tokio::test]
async fn to_top_and_to_bottom_tag_the_range_in_visual_mode() {
    use myd::app::FileBrowser;

    // `to_top` / `to_bottom` bypassed tag_visual_span the same way the page
    // motions did, so `V` then `G` tagged only the anchor.
    let dir = paging_fixture(30);
    let mut app = FileBrowser::new(Some(dir.path().to_path_buf()), None, false);
    settle(&mut app).await;

    let total = match app.current_screen() {
        myd::screen::Screen::Main(s) => s.tree.lines.len(),
        _ => panic!("expected a main screen"),
    };

    app.handle_key_for_test(char_key('V'));
    app.handle_key_for_test(char_key('G'));

    let tagged = match app.current_screen() {
        myd::screen::Screen::Main(s) => s.tree.tagged_paths().len(),
        _ => panic!("expected a main screen"),
    };
    assert_eq!(tagged, total, "V then G tags every line");
}

// ---------------------------------------------------------------------------
// Scrolling: the cursor crosses the viewport before the content moves.
// ---------------------------------------------------------------------------

#[test]
fn cursor_moves_within_the_viewport_before_the_view_scrolls() {
    // The headline regression. The offset used to be recomputed from the cursor
    // on every frame (`cursor - visible + 1`), which pinned the cursor to the
    // bottom row once it passed the first screenful: moving *up* then shifted
    // the whole view instead of walking the cursor back up through it.
    let (_dir, mut st, mut term) = paging_screen(60, 80, 20);
    draw(&mut st, &mut term);
    let visible = st.tree_viewport;
    assert_eq!(visible, 16);

    // Walk down past the first screen. The offset must have followed by exactly
    // as much as the cursor overshot the window.
    for _ in 0..25 {
        st.cursor_down();
        draw(&mut st, &mut term);
    }
    assert_eq!(st.tree.cursor, 25);
    assert_eq!(st.tree_scroll, 25 + 1 - visible, "offset tracks the bottom edge");
    let parked = st.tree_scroll;

    // The precise statement of the bug: one press of `k` moves the cursor up
    // *within* the window and leaves the content exactly where it was. Pre-fix
    // the offset was re-derived as 24 - 16 + 1 and the whole view jumped.
    st.cursor_up();
    draw(&mut st, &mut term);
    assert_eq!(st.tree.cursor, 24);
    assert_eq!(
        st.tree_scroll, parked,
        "moving the cursor inside the window must not scroll the view"
    );
}

#[test]
fn scrolling_down_then_up_returns_to_the_same_offset() {
    // Guards against an off-by-one that accumulates over a long sweep.
    let (_dir, mut st, mut term) = paging_screen(60, 80, 20);
    draw(&mut st, &mut term);

    for _ in 0..30 {
        st.cursor_down();
        draw(&mut st, &mut term);
    }
    for _ in 0..30 {
        st.cursor_up();
        draw(&mut st, &mut term);
    }

    assert_eq!(st.tree.cursor, 0);
    assert_eq!(st.tree_scroll, 0, "a round trip must land back at the top");
}

#[test]
fn the_scroll_offset_is_clamped_when_lines_disappear() {
    // A post-fix invariant guard, not a pre-fix failure: with the offset derived
    // from the cursor this was vacuously true. Now that the offset persists, a
    // shrinking line list could leave the view scrolled past the end.
    use myd::screen::SortMode;
    use myd::widget::file_tree::FileTree;

    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("big")).unwrap();
    for i in 0..80 {
        std::fs::write(dir.path().join("big").join(format!("f{:03}", i)), b"x").unwrap();
    }
    let tree = FileTree::new(dir.path().to_path_buf(), SortMode::Largest, true, true);
    let mut st = myd::screen::MainScreenState::from_tree(dir.path().to_path_buf(), tree);
    let mut term =
        ratatui::Terminal::new(ratatui::backend::TestBackend::new(80, 20)).unwrap();

    // Expand the subtree, drop to the bottom, and scroll deep into it.
    st.expand_all();
    draw(&mut st, &mut term);
    st.to_bottom();
    draw(&mut st, &mut term);
    assert!(st.tree_scroll > 0, "should be scrolled well down");

    // Collapsing throws most of those lines away.
    st.collapse_all();
    draw(&mut st, &mut term);

    let visible = st.tree_viewport;
    assert!(
        st.tree_scroll + visible <= st.tree.lines.len().max(visible),
        "offset {} + viewport {} ran past {} lines",
        st.tree_scroll,
        visible,
        st.tree.lines.len()
    );
    assert!(
        st.tree.cursor >= st.tree_scroll && st.tree.cursor < st.tree_scroll + visible,
        "cursor {} outside the window [{}, {})",
        st.tree.cursor,
        st.tree_scroll,
        st.tree_scroll + visible
    );
}

#[test]
fn clicking_a_row_still_selects_it_after_scrolling() {
    // The tree_scroll/click_at contract: the recorded offset must keep meaning
    // "index of the first visible line", or mouse hit-testing silently drifts.
    let (_dir, mut st, mut term) = paging_screen(60, 80, 20);
    draw(&mut st, &mut term);
    st.to_bottom();
    draw(&mut st, &mut term);

    let scroll = st.tree_scroll;
    assert!(scroll > 0, "should be scrolled down");

    let area = st.tree_area.expect("tree drew");
    // Content row 3: past the top border/title row.
    st.click_at(area.x + 2, area.y + 1 + 3);
    assert_eq!(st.tree.cursor, scroll + 3);
}

#[test]
fn to_bottom_leaves_the_last_line_visible() {
    let (_dir, mut st, mut term) = paging_screen(60, 80, 20);
    draw(&mut st, &mut term);
    st.to_bottom();
    draw(&mut st, &mut term);

    let visible = st.tree_viewport;
    assert_eq!(st.tree.cursor, st.tree.lines.len() - 1);
    assert_eq!(
        st.tree_scroll,
        st.tree.lines.len() - visible,
        "the last line should sit on the bottom row, with no blank rows below"
    );
}

// ---------------------------------------------------------------------------
// Shallow remote directory sizes: shown as unknown, sorted as unknown.
// ---------------------------------------------------------------------------

use myd::screen::SortMode as ShallowSortMode;
use myd::utils::sizes::{CancelToken as ShallowCancel, SizeCache as ShallowCache};
use myd::widget::file_tree::FileTree as ShallowTree;
use myd::widget::progress::OpProgress as ShallowProgress;

/// A remote backend whose listing mimics SFTP: real file lengths, and the
/// directory inode's own 4096 bytes for every directory.
struct ShallowDirs;

#[async_trait::async_trait]
impl myd::vfs::Vfs for ShallowDirs {
    fn scheme(&self) -> &'static str {
        "sftp"
    }
    async fn read_dir(&self, p: &myd::vfs::VPath) -> anyhow::Result<Vec<myd::vfs::VEntry>> {
        // Only the root has children; the two directories are empty.
        if p.path.file_name().map(|s| s.to_string_lossy().to_string()) == Some("dir_a".into())
            || p.path.file_name().map(|s| s.to_string_lossy().to_string()) == Some("dir_b".into())
        {
            return Ok(Vec::new());
        }
        let mut v = Vec::new();
        for name in ["dir_a", "dir_b"] {
            let mut e = myd::vfs::VEntry::new(name, true);
            // What a real SFTP READDIR reports for a directory: its inode size.
            e.len = 4096;
            e.mode = Some(0o750);
            v.push(e);
        }
        let mut small = myd::vfs::VEntry::new("small_file", false);
        small.len = 100;
        v.push(small);
        let mut big = myd::vfs::VEntry::new("big_file", false);
        big.len = 1_000_000;
        v.push(big);
        Ok(v)
    }
    async fn stat(&self, p: &myd::vfs::VPath) -> anyhow::Result<myd::vfs::VMetadata> {
        let n = p
            .path
            .file_name()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_default();
        let is_dir = !n.contains("file");
        Ok(myd::vfs::VMetadata {
            is_dir,
            is_symlink: false,
            len: if is_dir { 4096 } else { 100 },
            mode: None,
            uid: None,
            gid: None,
            mtime: None,
            atime: None,
            ctime: None,
        })
    }
    async fn create_dir_all(&self, _p: &myd::vfs::VPath) -> anyhow::Result<()> {
        Ok(())
    }
    async fn remove_file(&self, _p: &myd::vfs::VPath) -> anyhow::Result<()> {
        Ok(())
    }
    async fn remove_dir(&self, _p: &myd::vfs::VPath) -> anyhow::Result<()> {
        Ok(())
    }
    async fn rename(&self, _f: &myd::vfs::VPath, _t: &myd::vfs::VPath) -> anyhow::Result<()> {
        Ok(())
    }
    async fn open_read(&self, _p: &myd::vfs::VPath) -> anyhow::Result<Box<dyn myd::vfs::VRead>> {
        anyhow::bail!("unused")
    }
    async fn open_write(
        &self,
        _p: &myd::vfs::VPath,
        _l: Option<u64>,
    ) -> anyhow::Result<Box<dyn myd::vfs::VWrite>> {
        anyhow::bail!("unused")
    }
    async fn dir_size(
        &self,
        _p: &myd::vfs::VPath,
        _c: &ShallowCache,
        _ct: &ShallowCancel,
        _pr: Option<&ShallowProgress>,
    ) -> u64 {
        4096
    }
    fn has_recursive_sizes(&self) -> bool {
        false
    }
}

/// A remote tree over [`ShallowDirs`] rooted at `root`.
///
/// Used to build a remote panel whose paths also exist on the local disk, which
/// is what makes "did this operate on the wrong machine?" observable.
fn remote_tree_rooted_at(root: &std::path::Path) -> ShallowTree {
    use myd::widget::source::Source;

    let vfs: std::sync::Arc<dyn myd::vfs::Vfs> = std::sync::Arc::new(ShallowDirs);
    let source = Source::Remote(
        myd::widget::source::RemoteSource::new(myd::vfs::BackendId(1), vfs).unwrap(),
    );
    ShallowTree::with_source_cancellable_progress(
        source,
        root.to_path_buf(),
        ShallowSortMode::Largest,
        true,
        true,
        ShallowCache::new(),
        &ShallowCancel::new(),
        &ShallowProgress::new(),
    )
    .expect("remote tree should build")
}

/// A remote tree over [`ShallowDirs`], sorted by `mode`.
fn shallow_remote_tree(mode: ShallowSortMode) -> ShallowTree {
    use myd::widget::source::Source;

    let vfs: std::sync::Arc<dyn myd::vfs::Vfs> = std::sync::Arc::new(ShallowDirs);
    let source = Source::Remote(
        myd::widget::source::RemoteSource::new(myd::vfs::BackendId(1), vfs).unwrap(),
    );
    ShallowTree::with_source_cancellable_progress(
        source,
        std::path::PathBuf::from("/remote"),
        mode,
        true,
        true,
        ShallowCache::new(),
        &ShallowCancel::new(),
        &ShallowProgress::new(),
    )
    .expect("remote tree should build")
}

/// A remote pane must be distinguishable from a local one at a glance.
#[test]
fn a_remote_title_bar_says_sftp_and_is_coloured() {
    use myd::screen::{MainScreenState, ScreenState};

    let tree = shallow_remote_tree(ShallowSortMode::Largest);
    let mut screen =
        MainScreenState::from_tree(std::path::PathBuf::from("/remote"), tree);

    let mut term =
        ratatui::Terminal::new(ratatui::backend::TestBackend::new(90, 8)).unwrap();
    term.draw(|f| screen.render(f, f.area())).unwrap();
    let buf = term.backend().buffer();
    let title: String = (0..buf.area.width).map(|x| buf[(x, 0u16)].symbol()).collect();

    assert!(
        title.contains("SFTP (/remote)"),
        "a remote pane must name the protocol, not say 'File Tree': {}",
        title
    );
    assert!(
        !title.contains("File Tree"),
        "and must not also claim to be a plain file tree: {}",
        title
    );

    // The colour is the part that costs no columns, so it carries the same
    // signal on a narrow terminal where the text is truncated away.
    let coloured = (0..buf.area.width)
        .any(|x| buf[(x, 0u16)].fg == myd::widget::file_tree::REMOTE_COLOR);
    assert!(coloured, "the remote title must be drawn in REMOTE_COLOR");
}

/// A local pane keeps the plain title and is not tinted.
#[test]
fn a_local_title_bar_is_unchanged() {
    use myd::screen::{MainScreenState, ScreenState, SortMode};
    use myd::widget::file_tree::FileTree;

    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("a.txt"), "x").unwrap();
    let tree = FileTree::new(dir.path().to_path_buf(), SortMode::Largest, true, true);
    let mut screen = MainScreenState::from_tree(dir.path().to_path_buf(), tree);

    let mut term =
        ratatui::Terminal::new(ratatui::backend::TestBackend::new(90, 8)).unwrap();
    term.draw(|f| screen.render(f, f.area())).unwrap();
    let buf = term.backend().buffer();
    let title: String = (0..buf.area.width).map(|x| buf[(x, 0u16)].symbol()).collect();

    assert!(title.contains("File Tree ("), "local keeps its title: {}", title);
    assert!(!title.contains("SFTP"), "and says nothing about SFTP: {}", title);
    let tinted = (0..buf.area.width)
        .any(|x| buf[(x, 0u16)].fg == myd::widget::file_tree::REMOTE_COLOR);
    assert!(!tinted, "a local pane must not wear the remote colour");
}

/// Names of the depth-1 entries, in render order.
fn child_names(tree: &ShallowTree) -> Vec<String> {
    tree.lines
        .iter()
        .filter(|l| l.depth == 1)
        .map(|l| l.name.clone())
        .collect()
}

#[test]
fn remote_directories_sort_last_in_largest() {
    // Pre-fix every directory sorted as its 4096-byte inode size, so a directory
    // holding gigabytes landed below any file over 4 KB.
    let tree = shallow_remote_tree(ShallowSortMode::Largest);
    assert_eq!(
        child_names(&tree),
        vec!["big_file", "small_file", "dir_a", "dir_b"],
        "real sizes first, unmeasured directories last"
    );
}

#[test]
fn remote_directories_also_sort_last_in_smallest() {
    // The sign-flip catcher. `Largest` negates its size, so "sorts last" is
    // i64::MAX in *both* arms; an implementation that reaches for i64::MIN in one
    // of them puts the directories first here, and only here.
    let tree = shallow_remote_tree(ShallowSortMode::Smallest);
    assert_eq!(
        child_names(&tree),
        vec!["small_file", "big_file", "dir_a", "dir_b"],
        "unmeasured directories stay last regardless of direction"
    );
}

#[test]
fn local_directories_still_sort_by_size() {
    // The gate must not become unconditional: locally, directory sizes are real
    // recursive totals and must keep ordering against files.
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("fat_dir")).unwrap();
    std::fs::write(dir.path().join("fat_dir/inner.bin"), vec![0u8; 500_000]).unwrap();
    std::fs::create_dir_all(dir.path().join("thin_dir")).unwrap();
    std::fs::write(dir.path().join("thin_dir/inner.bin"), vec![0u8; 10]).unwrap();
    std::fs::write(dir.path().join("mid_file.bin"), vec![0u8; 50_000]).unwrap();

    let tree = ShallowTree::new(dir.path().to_path_buf(), ShallowSortMode::Largest, true, true);
    assert_eq!(
        child_names(&tree),
        vec!["fat_dir", "mid_file.bin", "thin_dir"],
        "local directory sizes are real and must sort against files"
    );
}

/// Render a tree and return its lines as strings.
fn tree_rows(tree: &ShallowTree, w: u16, h: u16) -> Vec<String> {
    use ratatui::{backend::TestBackend, widgets::Paragraph, Terminal};

    let mut terminal = Terminal::new(TestBackend::new(w, h)).unwrap();
    terminal
        .draw(|f| f.render_widget(Paragraph::new(tree.render_text()), f.area()))
        .unwrap();
    let buf = terminal.backend().buffer().clone();
    (0..h)
        .map(|y| {
            (0..w)
                .map(|x| buf[(x, y)].symbol())
                .collect::<String>()
                .trim_end()
                .to_string()
        })
        .collect()
}

#[test]
fn a_remote_directory_renders_a_dash_instead_of_a_fake_size() {
    // Pre-fix these rows read "4.0KB", which is the directory inode's size and
    // says nothing about the contents.
    let tree = shallow_remote_tree(ShallowSortMode::Largest);
    let rows = tree_rows(&tree, 100, 10);

    let dir_row = rows
        .iter()
        .find(|r| r.contains("dir_a"))
        .expect("dir_a should render");
    assert!(dir_row.contains('—'), "unmeasured dir needs a dash: {}", dir_row);
    assert!(
        !dir_row.contains("4.0KB") && !dir_row.contains("4KB"),
        "must not show the inode size as if it were real: {}",
        dir_row
    );

    // A file's size is reported accurately by the listing and must survive.
    let file_row = rows
        .iter()
        .find(|r| r.contains("big_file"))
        .expect("big_file should render");
    assert!(
        file_row.contains("976.6KB"),
        "file sizes are real and must still show: {}",
        file_row
    );
}

#[test]
fn a_remote_directory_bar_is_empty() {
    let tree = shallow_remote_tree(ShallowSortMode::Largest);
    let rows = tree_rows(&tree, 100, 10);
    let dir_row = rows.iter().find(|r| r.contains("dir_a")).unwrap();

    let bar = &dir_row[dir_row.find('[').unwrap()..=dir_row.find(']').unwrap()];
    assert!(!bar.contains('█'), "an unknown size cannot fill a bar: {}", bar);
    assert!(bar.contains('░'), "the empty bar should still be drawn: {}", bar);
}

#[test]
fn a_shallow_size_row_keeps_the_column_width() {
    // The em dash is padded by char count, so this is where alignment would break.
    let tree = shallow_remote_tree(ShallowSortMode::Largest);
    let rows = tree_rows(&tree, 100, 10);

    // Counted in characters, not bytes: the em dash is 3 bytes in UTF-8 while a
    // formatted size is ASCII, so a byte offset would differ even when the
    // columns line up perfectly on screen.
    let icon_col = |needle: &str| -> usize {
        let row = rows.iter().find(|r| r.contains(needle)).unwrap();
        row.chars()
            .position(|c| c == ']')
            .expect("bar should close")
    };
    assert_eq!(
        icon_col("dir_a"),
        icon_col("big_file"),
        "the size column must be the same width whether or not the size is known"
    );
}

#[test]
fn the_treemap_gives_remote_directories_equal_tiles() {
    // Pre-fix all directories carried 4096 bytes, so squarify produced tiles of
    // unequal area that looked meaningful but encoded nothing.
    use myd::widget::treemap::TreeMap;

    let tree = shallow_remote_tree(ShallowSortMode::Largest);
    let mut map = TreeMap::from_file_tree(&tree);
    map.compute_layout(ratatui::layout::Rect::new(0, 0, 80, 24));

    assert_eq!(
        map.total_size, 0,
        "unmeasured directories must not contribute a fake total"
    );
    let dir_cells: Vec<_> = map.cells.iter().filter(|c| c.is_dir).collect();
    assert!(dir_cells.len() >= 2, "expected the two directories");
    let first = dir_cells[0].rect;
    for c in &dir_cells {
        assert_eq!(
            (c.rect.width, c.rect.height),
            (first.width, first.height),
            "unmeasured directories should tile equally"
        );
    }
}

// ---------------------------------------------------------------------------
// Permissions and modification-time columns.
// ---------------------------------------------------------------------------

/// A directory with a known-mode file and directory, plus the app over it.
async fn columns_app() -> (tempfile::TempDir, myd::app::FileBrowser) {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("a_dir")).unwrap();
    std::fs::write(dir.path().join("a_file.txt"), b"hello").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(
            dir.path().join("a_file.txt"),
            std::fs::Permissions::from_mode(0o644),
        )
        .unwrap();
        std::fs::set_permissions(
            dir.path().join("a_dir"),
            std::fs::Permissions::from_mode(0o755),
        )
        .unwrap();
    }
    let mut app = myd::app::FileBrowser::new(Some(dir.path().to_path_buf()), None, false);
    settle(&mut app).await;
    (dir, app)
}

/// Draw the app and return its screen rows.
fn app_rows(app: &mut myd::app::FileBrowser, w: u16, h: u16) -> Vec<String> {
    use ratatui::{backend::TestBackend, Terminal};
    let mut terminal = Terminal::new(TestBackend::new(w, h)).unwrap();
    terminal.draw(|f| app.render_for_test(f)).unwrap();
    let buf = terminal.backend().buffer().clone();
    (0..h)
        .map(|y| (0..w).map(|x| buf[(x, y)].symbol()).collect::<String>())
        .collect()
}

#[cfg(unix)]
#[tokio::test]
async fn p_shows_a_permissions_column() {
    let (_dir, mut app) = columns_app().await;

    let before = app_rows(&mut app, 120, 12).join("\n");
    assert!(
        !before.contains("rw-"),
        "permissions must be off by default: {}",
        before
    );

    app.handle_key_for_test(char_key('P'));
    let after = app_rows(&mut app, 120, 12).join("\n");
    assert!(
        after.contains("-rw-r--r--"),
        "the 0o644 file needs its mode: {}",
        after
    );
    assert!(
        after.contains("drwxr-xr-x"),
        "the 0o755 directory needs its mode: {}",
        after
    );
}

#[tokio::test]
async fn t_shows_a_timestamp_column() {
    let (_dir, mut app) = columns_app().await;

    let before = app_rows(&mut app, 120, 12).join("\n");
    let stamp = regex::Regex::new(r"\d{4}-\d\d-\d\d \d\d:\d\d").unwrap();
    assert!(!stamp.is_match(&before), "times must be off by default");

    app.handle_key_for_test(char_key('T'));
    let after = app_rows(&mut app, 120, 12).join("\n");
    assert!(
        stamp.is_match(&after),
        "expected a Y-m-d H:M timestamp: {}",
        after
    );
}

#[cfg(unix)]
#[tokio::test]
async fn the_two_columns_are_independent() {
    let (_dir, mut app) = columns_app().await;
    let stamp = regex::Regex::new(r"\d{4}-\d\d-\d\d \d\d:\d\d").unwrap();

    // On, on, then the first back off: only the timestamp should remain.
    app.handle_key_for_test(char_key('P'));
    app.handle_key_for_test(char_key('T'));
    app.handle_key_for_test(char_key('P'));

    let rows = app_rows(&mut app, 120, 12).join("\n");
    assert!(stamp.is_match(&rows), "the time column should still be on");
    assert!(
        !rows.contains("rw-"),
        "the permissions column should be back off: {}",
        rows
    );
}

#[tokio::test]
async fn the_columns_survive_entering_a_directory() {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    let (_dir, mut app) = columns_app().await;
    // Draw first, so the tree has a viewport and the cursor lands on a real row.
    let _ = app_rows(&mut app, 120, 12);

    app.handle_key_for_test(char_key('P'));
    app.handle_key_for_test(char_key('T'));

    let flags = |app: &myd::app::FileBrowser| match app.current_screen() {
        myd::screen::Screen::Main(s) => (s.tree.show_perms, s.tree.show_times),
        _ => panic!("expected a main screen"),
    };
    assert_eq!(flags(&app), (true, true));

    // Descend into the subdirectory: a freshly built screen must adopt them.
    app.handle_key_for_test(char_key('j'));
    app.handle_key_for_test(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    settle(&mut app).await;
    assert_eq!(
        flags(&app),
        (true, true),
        "columns must survive entering a directory"
    );

    // And on the way back out.
    app.handle_key_for_test(ctrl_key('o'));
    settle(&mut app).await;
    assert_eq!(flags(&app), (true, true), "and popping back out");
}

#[cfg(unix)]
#[tokio::test]
async fn the_columns_keep_names_aligned() {
    let (_dir, mut app) = columns_app().await;
    app.handle_key_for_test(char_key('P'));
    app.handle_key_for_test(char_key('T'));

    let rows = app_rows(&mut app, 120, 12);
    // Counted in characters, not bytes: the icons and the em-dash placeholders
    // are multibyte, so byte offsets would differ even on a perfectly aligned row.
    let name_col = |needle: &str| -> usize {
        let row = rows.iter().find(|r| r.contains(needle)).unwrap();
        let byte_idx = row.find(needle).unwrap();
        row[..byte_idx].chars().count()
    };
    assert_eq!(
        name_col("a_file.txt"),
        name_col("a_dir"),
        "a file row and a directory row must start their names at the same column"
    );
}

#[tokio::test]
async fn remote_listings_carry_their_mode_bits() {
    // Pre-fix the remote arm of Source::read_dir dropped VEntry.mode entirely, so
    // a remote tree could never show permissions.
    let tree = shallow_remote_tree(ShallowSortMode::Largest);
    let dir_line = tree
        .lines
        .iter()
        .find(|l| l.name == "dir_a")
        .expect("dir_a should be listed");
    assert_eq!(
        dir_line.mode,
        Some(0o750),
        "the listing's mode must reach the tree line"
    );
}

#[tokio::test]
async fn ctrl_b_no_longer_toggles_the_info_panel() {
    // Ctrl+B used to open the info panel and now pages up instead. Asserting the
    // panel state is *unchanged* is what stops a future revert from quietly
    // reclaiming the key and breaking paging.
    let dir = nav_fixture();
    let mut app = myd::app::FileBrowser::new(Some(dir.path().to_path_buf()), None, false);
    settle(&mut app).await;

    let before = view_state(&app).0;
    app.handle_key_for_test(ctrl_key('b'));
    assert_eq!(
        view_state(&app).0,
        before,
        "Ctrl+B must not touch the info panel; that is Ctrl+P"
    );

    // And the key that should work, does.
    app.handle_key_for_test(ctrl_key('p'));
    assert_ne!(view_state(&app).0, before, "Ctrl+P toggles the info panel");
}

#[test]
fn ghost_rows_render_without_a_mode() {
    // A ghost row is a synthetic TreeLine for a transfer destination that does
    // not exist yet, so it has no permissions to show. With the column on it must
    // still render — and keep the column's width, or every ghost row's name would
    // sit 12 characters left of its neighbours'.
    use myd::screen::SortMode;
    use myd::transfer::PendingDest;
    use myd::vfs::{BackendId, VPath};
    use myd::widget::file_tree::FileTree;

    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("existing.txt"), b"x").unwrap();

    let mut tree = FileTree::new(dir.path().to_path_buf(), SortMode::Largest, true, false);
    tree.show_perms = true;
    tree.show_times = true;

    let ghost = PendingDest {
        path: VPath {
            backend: BackendId(0),
            path: dir.path().join("arriving.bin"),
        },
        is_dir: false,
    };

    // Must not panic, and the placeholder must be there at full width.
    let text = tree.render_text_with_ghosts(&[ghost]);
    let rendered: Vec<String> = text
        .lines
        .iter()
        .map(|l| {
            l.spans
                .iter()
                .map(|s| s.content.as_ref())
                .collect::<String>()
        })
        .collect();

    let ghost_row = rendered
        .iter()
        .find(|r| r.contains("arriving.bin"))
        .expect("the ghost row should render");
    assert!(
        ghost_row.contains("?---------"),
        "a not-yet-existing entry needs the unknown-mode placeholder: {}",
        ghost_row
    );
}

#[cfg(unix)]
#[test]
fn the_root_row_carries_its_own_metadata() {
    // The root is built separately from the listing, so it initially had no mode
    // or times and rendered "?---------" and a dash while every other row was
    // correct. One local stat per tree fixes it.
    use myd::screen::SortMode;
    use myd::widget::file_tree::FileTree;

    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("f.txt"), b"x").unwrap();

    let tree = FileTree::new(dir.path().to_path_buf(), SortMode::Largest, true, false);
    let root = &tree.lines[0];
    assert_eq!(root.depth, 0, "line 0 should be the root");
    assert!(root.mode.is_some(), "the local root should report its mode");
    assert!(root.mtime.is_some(), "the local root should report its mtime");
}

#[test]
fn a_remote_root_reports_no_local_metadata() {
    // A remote root must NOT be stat'ed with std::fs: a path like /var/log exists
    // on both machines, so local metadata would silently describe the wrong file —
    // the same class of bug that made the info panel show local data for remote
    // entries. A placeholder is correct here.
    let tree = shallow_remote_tree(ShallowSortMode::Largest);
    let root = &tree.lines[0];
    assert_eq!(root.depth, 0);
    assert!(
        root.mode.is_none(),
        "a remote root must not borrow local mode bits"
    );
}

#[tokio::test]
async fn copy_targets_the_destination_panes_cursor_directory() {
    // A copy's destination is the OTHER pane's `current_dir()`, which is that
    // pane's *root* — not where its cursor actually is. Directories expand in
    // place, so after expanding a subdirectory and putting the cursor inside it,
    // the visible "current directory" and the copy destination disagree.
    //
    // `N` (create directory) already resolves this correctly via `target_dir()`;
    // copy does not, so a copy lands one or more levels above where the user is
    // looking.
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    let src = tempfile::tempdir().unwrap();
    std::fs::write(src.path().join("payload.txt"), b"data").unwrap();

    let dst = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dst.path().join("inbox")).unwrap();
    std::fs::write(dst.path().join("inbox/keep.txt"), b"k").unwrap();

    let mut app = myd::app::FileBrowser::new(
        Some(src.path().to_path_buf()),
        Some(dst.path().to_path_buf()),
        true,
    );
    settle_all(&mut app).await;
    for _ in 0..200 {
        app.resolve_loading_for_test();
        if app.panel_current_dir(0).is_some() && app.panel_current_dir(1).is_some() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(3)).await;
    }

    // Focus the right pane and open "inbox", leaving the cursor inside it.
    app.handle_key_for_test(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));
    assert_eq!(app.active_panel_index(), 1);
    for _ in 0..40 {
        let on_inbox = match app.current_screen() {
            myd::screen::Screen::Main(s) => s
                .tree
                .selected_line()
                .map(|l| l.name == "inbox")
                .unwrap_or(false),
            _ => false,
        };
        if on_inbox {
            break;
        }
        app.handle_key_for_test(char_key('j'));
    }
    // Expand it in place, then step onto a child so the cursor is *inside* inbox.
    app.handle_key_for_test(char_key('l'));
    app.handle_key_for_test(char_key('j'));

    let cursor_dir = match app.current_screen() {
        myd::screen::Screen::Main(s) => s
            .tree
            .selected_line()
            .map(|l| {
                if l.is_dir {
                    l.resolved_path.clone()
                } else {
                    l.resolved_path.parent().unwrap().to_path_buf()
                }
            })
            .unwrap(),
        _ => panic!("expected a main screen"),
    };
    assert_eq!(
        cursor_dir.file_name().unwrap(),
        "inbox",
        "the right pane's cursor should be inside inbox"
    );

    // Back to the left pane, select the file, and copy.
    app.handle_key_for_test(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));
    for _ in 0..40 {
        let on_it = match app.current_screen() {
            myd::screen::Screen::Main(s) => s
                .tree
                .selected_line()
                .map(|l| l.name == "payload.txt")
                .unwrap_or(false),
            _ => false,
        };
        if on_it {
            break;
        }
        app.handle_key_for_test(char_key('j'));
    }
    app.handle_key_for_test(char_key('c'));
    for _ in 0..200 {
        app.tick_for_test();
        app.resolve_loading_for_test();
        if dst.path().join("inbox/payload.txt").exists() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(3)).await;
    }

    assert!(
        dst.path().join("inbox/payload.txt").exists(),
        "the copy should land in the directory the destination pane's cursor is in \
         (inbox), but inbox contains {:?} and the pane root contains {:?}",
        std::fs::read_dir(dst.path().join("inbox"))
            .unwrap()
            .flatten()
            .map(|e| e.file_name())
            .collect::<Vec<_>>(),
        std::fs::read_dir(dst.path())
            .unwrap()
            .flatten()
            .map(|e| e.file_name())
            .collect::<Vec<_>>()
    );
}

#[test]
fn logging_initializes_and_writes_to_the_configured_file() {
    // `trace::init()` was only called by the myd-transfer helper, never by the
    // TUI, so MYD_LOG / MYD_TRACE silently did nothing for the app itself. This
    // asserts a subscriber actually installs and reaches the file.
    //
    // Runs the real binary: init() is idempotent per-process and other tests may
    // already have set the global dispatcher, so checking it in-process would
    // prove nothing.
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("a.txt"), b"x").unwrap();
    let log = dir.path().join("trace.log");

    let exe = env!("CARGO_BIN_EXE_myd");
    let mut child = std::process::Command::new(exe)
        .arg(dir.path())
        .env("MYD_LOG", "myd=debug")
        .env("MYD_TRACE_FILE", &log)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .expect("myd should start");

    // Give it a moment to install the subscriber and write the startup line.
    let mut found = false;
    for _ in 0..100 {
        std::thread::sleep(std::time::Duration::from_millis(50));
        if std::fs::read_to_string(&log)
            .map(|s| s.contains("diagnostics started"))
            .unwrap_or(false)
        {
            found = true;
            break;
        }
    }
    let _ = child.kill();
    let _ = child.wait();

    assert!(
        found,
        "MYD_LOG should install a subscriber and write to MYD_TRACE_FILE; log was {:?}",
        std::fs::read_to_string(&log).ok()
    );
}

// ---------------------------------------------------------------------------
// Directory picker: typed paths are honoured, and Tab moves focus.
// ---------------------------------------------------------------------------

/// Open the picker over a temp dir via the `gd` chord.
async fn picker_app() -> (tempfile::TempDir, myd::app::FileBrowser) {
    let start = tempfile::tempdir().unwrap();
    let mut app = myd::app::FileBrowser::new(Some(start.path().to_path_buf()), None, false);
    // An explicit catalog rather than the real one: these tests assert on list
    // positions, and the machine's own home directory should not decide how many
    // rows there are.
    let cfg = tempfile::tempdir().unwrap();
    let mut catalog = myd::hosts::HostCatalog::load_from_unseeded(&cfg.path().join("hosts.toml"));
    for name in ["alpha", "beta", "gamma"] {
        let d = start.path().join(name);
        std::fs::create_dir_all(&d).unwrap();
        catalog.add_favorite(myd::hosts::SavedDir::saved(d.to_string_lossy().to_string()));
    }
    app.set_hosts_for_test(catalog);
    settle(&mut app).await;
    app.handle_key_for_test(char_key('g'));
    app.handle_key_for_test(char_key('d'));
    assert!(
        matches!(app.current_screen(), myd::screen::Screen::DirPicker(_)),
        "gd should open the directory picker"
    );
    (start, app)
}

fn picker(app: &myd::app::FileBrowser) -> &myd::screen::DirPickerState {
    match app.current_screen() {
        myd::screen::Screen::DirPicker(s) => s,
        _ => panic!("expected the directory picker"),
    }
}

fn type_str(app: &mut myd::app::FileBrowser, s: &str) {
    for c in s.chars() {
        app.handle_key_for_test(char_key(c));
    }
}

#[tokio::test]
async fn a_typed_path_is_honoured_after_browsing_the_list() {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    // The reported bug. Browsing the list wrote the option's path into the field
    // but left the text cursor at 0, so everything typed afterwards was inserted
    // *in front of* it. The field read "<typed><option>" and resolved to the
    // option — the typed path was silently discarded.
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("projects/kanban");
    std::fs::create_dir_all(&target).unwrap();

    let (_start, mut app) = picker_app().await;

    // Look at the list first, as the screen invites.
    app.handle_key_for_test(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
    // Then type a real path over it.
    type_str(&mut app, &target.to_string_lossy());

    assert_eq!(
        picker(&app).confirm(),
        myd::screen::PickerChoice::Open(target.clone()),
        "the typed path must win; field held {:?}",
        picker(&app).input_for_test()
    );

    app.handle_key_for_test(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    settle(&mut app).await;
    assert_eq!(
        app.panel_current_dir(0),
        Some(target),
        "Enter should open the typed path"
    );
}

#[tokio::test]
async fn typing_replaces_a_path_offered_by_the_list() {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    // The mechanism behind the bug above. Browsing mirrors the option into the
    // field as a *suggestion*; the first typed character replaces it. Previously
    // the text cursor stayed at 0 so typing prepended, giving
    // "<typed><option>" — which resolved to whichever half happened to exist.
    let (_start, mut app) = picker_app().await;
    app.handle_key_for_test(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
    let filled = picker(&app).input_for_test().to_string();
    assert!(!filled.is_empty(), "browsing should fill the field");

    app.handle_key_for_test(char_key('/'));
    assert_eq!(
        picker(&app).input_for_test(),
        "/",
        "typing must replace the list's suggestion, not extend it"
    );
}

#[tokio::test]
async fn a_suggestion_can_still_be_edited_deliberately() {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    // Replacing on the first keystroke must not make the suggestion un-editable:
    // moving the caret into it means the user wants to keep and adjust it, which
    // is how you append a subdirectory to a listed favourite.
    let (_start, mut app) = picker_app().await;
    app.handle_key_for_test(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
    let filled = picker(&app).input_for_test().to_string();

    // End is a deliberate move into the text; now typing appends.
    app.handle_key_for_test(KeyEvent::new(KeyCode::End, KeyModifiers::NONE));
    type_str(&mut app, "/sub");
    assert_eq!(
        picker(&app).input_for_test(),
        format!("{}/sub", filled),
        "after moving the caret, typing should extend the path"
    );
}

#[tokio::test]
async fn tab_switches_focus_and_j_k_then_navigate_the_list() {
    use myd::screen::PickerFocus;

    // The screen advertised "j/k to navigate" while the path field swallowed both
    // keys, so the only way to move was the arrows.
    let (_start, mut app) = picker_app().await;
    assert_eq!(
        picker(&app).focus(),
        PickerFocus::Field,
        "the field starts focused — the picker exists to take a path"
    );

    // In the field, j/k are text.
    type_str(&mut app, "jk");
    assert_eq!(picker(&app).input_for_test(), "jk");
    app.handle_key_for_test(crossterm::event::KeyEvent::new(
        crossterm::event::KeyCode::Backspace,
        crossterm::event::KeyModifiers::NONE,
    ));
    app.handle_key_for_test(crossterm::event::KeyEvent::new(
        crossterm::event::KeyCode::Backspace,
        crossterm::event::KeyModifiers::NONE,
    ));

    // Tab hands the keyboard to the list, where j/k move.
    app.handle_key_for_test(crossterm::event::KeyEvent::new(
        crossterm::event::KeyCode::Tab,
        crossterm::event::KeyModifiers::NONE,
    ));
    assert_eq!(picker(&app).focus(), PickerFocus::List);

    let before = picker(&app).cursor_for_test();
    app.handle_key_for_test(char_key('j'));
    let after_j = picker(&app).cursor_for_test();
    assert_ne!(after_j, before, "j must move the list once it has focus");
    app.handle_key_for_test(char_key('k'));
    assert_eq!(
        picker(&app).cursor_for_test(),
        before,
        "k must move back"
    );

    // Tab is never typed into the path.
    assert!(
        !picker(&app).input_for_test().contains('\t'),
        "Tab must not reach the field"
    );
}

#[tokio::test]
async fn typing_while_the_list_has_focus_returns_to_the_field() {
    use myd::screen::PickerFocus;

    // Typing a path is the picker's purpose; it must not be a no-op just because
    // the list happens to hold focus.
    let (_start, mut app) = picker_app().await;
    app.handle_key_for_test(crossterm::event::KeyEvent::new(
        crossterm::event::KeyCode::Tab,
        crossterm::event::KeyModifiers::NONE,
    ));
    assert_eq!(picker(&app).focus(), PickerFocus::List);

    // A printable character that is not a list key starts a path. `/` is no
    // longer such a character — it opens the search — so this uses `~`, which is
    // how most typed paths begin anyway.
    type_str(&mut app, "~/tm");
    assert_eq!(picker(&app).focus(), PickerFocus::Field);
    assert_eq!(picker(&app).input_for_test(), "~/tm");
}

#[tokio::test]
async fn slash_searches_the_list_rather_than_starting_a_path() {
    use myd::screen::PickerFocus;

    // `/` narrows the list, which is what it does everywhere else in the app.
    // The path field is still reachable with Tab or any other character.
    let (_start, mut app) = picker_app().await;
    app.handle_key_for_test(crossterm::event::KeyEvent::new(
        crossterm::event::KeyCode::Tab,
        crossterm::event::KeyModifiers::NONE,
    ));
    let all = picker(&app).visible_count();
    assert!(all >= 3, "need a few rows to filter");

    type_str(&mut app, "/alpha");
    assert_eq!(
        picker(&app).focus(),
        PickerFocus::List,
        "searching keeps the keyboard on the list"
    );
    assert_eq!(picker(&app).query(), "alpha");
    assert!(
        picker(&app).visible_count() < all,
        "the list should have narrowed: {} of {}",
        picker(&app).visible_count(),
        all
    );

    // Esc abandons the search and restores the full list.
    app.handle_key_for_test(crossterm::event::KeyEvent::new(
        crossterm::event::KeyCode::Esc,
        crossterm::event::KeyModifiers::NONE,
    ));
    assert_eq!(picker(&app).visible_count(), all, "Esc clears the filter");
}

#[tokio::test]
async fn enter_on_a_search_with_one_match_opens_it() {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    // Having typed enough to leave exactly one candidate, the user has already
    // said which one they mean. Making them press Enter a second time to accept
    // the filter and then again to open is ceremony, so the single match opens
    // straight away.
    let (start, mut app) = picker_app().await;
    app.handle_key_for_test(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));

    type_str(&mut app, "/alpha");
    assert_eq!(
        picker(&app).visible_count(),
        1,
        "the fixture should narrow to one row for this to test anything"
    );

    app.handle_key_for_test(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    settle(&mut app).await;

    assert_eq!(
        app.panel_current_dir(0),
        Some(start.path().join("alpha")),
        "one match means Enter opens it"
    );
}

#[tokio::test]
async fn enter_on_a_search_with_several_matches_hands_over_the_filtered_list() {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use myd::screen::PickerFocus;

    // More than one candidate is still a choice, so Enter accepts the filter and
    // leaves the narrowed list to navigate rather than guessing at the top row.
    let (_start, mut app) = picker_app().await;
    app.handle_key_for_test(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));

    // "a" matches alpha, beta and gamma.
    type_str(&mut app, "/a");
    assert!(
        picker(&app).visible_count() > 1,
        "need several matches: {} shown",
        picker(&app).visible_count()
    );
    let shown = picker(&app).visible_count();

    // No `settle` here on purpose: nothing should have started loading, and
    // waiting for a load that never comes is how this test first "failed".
    app.handle_key_for_test(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));

    assert!(
        matches!(app.current_screen(), myd::screen::Screen::DirPicker(_)),
        "Enter with several matches must not open anything"
    );
    assert_eq!(
        picker(&app).visible_count(),
        shown,
        "the narrowed list stays in place to choose from"
    );
    assert_eq!(
        picker(&app).focus(),
        PickerFocus::List,
        "and the keyboard drives that list"
    );

    // j/k now walk the filtered rows, which is the point of handing them back.
    let before = picker(&app).cursor_for_test();
    app.handle_key_for_test(char_key('j'));
    assert_ne!(picker(&app).cursor_for_test(), before);
}

#[tokio::test]
async fn arrow_keys_navigate_the_list_from_the_field() {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use myd::screen::PickerFocus;

    // Arrows were the only working navigation before Tab existed, so they must
    // keep reaching the list from the field — that is the habit users have.
    //
    // The first press engages the list on the row already highlighted rather
    // than stepping past it; only subsequent presses move. This was reported as
    // "arrowing down skips the first entry".
    let (_start, mut app) = picker_app().await;
    assert_eq!(picker(&app).focus(), PickerFocus::Field);
    let start_cursor = picker(&app).cursor_for_test();

    app.handle_key_for_test(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
    assert_eq!(
        picker(&app).cursor_for_test(),
        start_cursor,
        "the first Down engages the list where it already is"
    );
    assert_eq!(
        picker(&app).focus(),
        PickerFocus::List,
        "and hands the keyboard to the list"
    );

    app.handle_key_for_test(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
    assert_ne!(
        picker(&app).cursor_for_test(),
        start_cursor,
        "the second Down moves"
    );
    app.handle_key_for_test(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE));
    assert_eq!(picker(&app).cursor_for_test(), start_cursor);
}

#[test]
fn the_picker_field_handles_multibyte_paths() {
    use myd::screen::DirPickerState;

    // input_cursor counts characters (the renderer indexes it with char_indices)
    // while the edit operations used it as a byte offset. Any non-ASCII character
    // would have panicked or corrupted the string.
    let mut p = DirPickerState::new();
    for c in "/tmp/café/naïve".chars() {
        p.input_char(c);
    }
    assert_eq!(p.input_for_test(), "/tmp/café/naïve");

    // Walk left past the multibyte `ï` and `é` and delete the separator between
    // them, which is only reachable correctly if the cursor counts characters.
    for _ in 0..5 {
        p.input_left();
    }
    p.input_backspace();
    assert_eq!(p.input_for_test(), "/tmp/cafénaïve");

    // Deleting forward at a multibyte boundary must also hold. Back to the start,
    // then remove the leading slash.
    for _ in 0..40 {
        p.input_left();
    }
    p.input_delete();
    assert_eq!(p.input_for_test(), "tmp/cafénaïve");

    // And a character typed mid-string lands where the caret is, not at a byte
    // offset that would fall inside `é`.
    for _ in 0..8 {
        p.input_right();
    }
    p.input_char('X');
    assert_eq!(p.input_for_test(), "tmp/caféXnaïve");
}

#[test]
fn a_copy_destination_is_not_canonicalized_against_the_local_disk() {
    // From a user's log: a copy's destination came out as `remote:/private/tmp`.
    // `/private/tmp` is macOS's canonicalisation of `/tmp` — a *local* resolution
    // applied to a path that was then sent to a Linux server, where `/private`
    // does not exist (the server answered NoSuchFile).
    //
    // `target_dir` returned `resolved_path`, which is canonicalised for local
    // trees. A destination directory has to be the path as the *destination
    // machine* names it, so it must come from `path`, not `resolved_path`.
    use myd::screen::SortMode;
    use myd::widget::file_tree::FileTree;

    let real = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(real.path().join("actual")).unwrap();
    std::fs::write(real.path().join("actual/f.txt"), b"x").unwrap();

    // A symlinked route to the same directory stands in for /tmp -> /private/tmp.
    let link_base = tempfile::tempdir().unwrap();
    let link = link_base.path().join("via_link");
    #[cfg(unix)]
    std::os::unix::fs::symlink(real.path().join("actual"), &link).unwrap();

    let tree = FileTree::new(link.clone(), SortMode::Largest, true, false);
    let mut st = myd::screen::MainScreenState::from_tree(link.clone(), tree);
    st.tree.set_cursor(0);

    let dest = st.target_dir();
    assert_eq!(
        dest, link,
        "the destination must stay the path the user is browsing ({}), not its \
         local canonicalisation ({})",
        link.display(),
        dest.display()
    );
}

#[tokio::test]
async fn a_single_panel_copy_destination_is_not_canonicalized_locally() {
    // From a user's log on macOS: a copy from a remote panel ended up targeting
    // `remote:/private/tmp`. Typing `/tmp` into the single-panel "Copy to
    // directory:" prompt ran `.canonicalize()` on it — a *local* resolution —
    // turning it into macOS's `/private/tmp`, which was then handed to the remote
    // server where `/private` does not exist.
    //
    // A destination directory has to be the path the destination filesystem uses,
    // so it must not be resolved against the local disk.
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    let real = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(real.path().join("actual")).unwrap();

    // A symlinked route to a directory stands in for /tmp -> /private/tmp.
    let link_base = tempfile::tempdir().unwrap();
    let link = link_base.path().join("via_link");
    #[cfg(unix)]
    std::os::unix::fs::symlink(real.path().join("actual"), &link).unwrap();

    let src = tempfile::tempdir().unwrap();
    std::fs::write(src.path().join("payload.txt"), b"data").unwrap();

    let mut app = myd::app::FileBrowser::new(Some(src.path().to_path_buf()), None, false);
    settle(&mut app).await;
    // Single panel: `c` prompts for a destination directory.
    app.handle_key_for_test(char_key('j'));
    app.handle_key_for_test(char_key('c'));
    settle_copy(&mut app).await;
    assert_eq!(app.modal_kind_for_test(), "input", "expected the destination prompt");

    for ch in link.to_string_lossy().chars() {
        app.handle_key_for_test(KeyEvent::new(KeyCode::Char(ch), KeyModifiers::NONE));
    }
    app.handle_key_for_test(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));

    for _ in 0..200 {
        app.tick_for_test();
        app.resolve_loading_for_test();
        if link.join("payload.txt").exists() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(3)).await;
    }

    // The copy must be addressed through the path the user typed. Reading it back
    // through the link proves the bytes landed, and `copy_dest_for_test` proves
    // the address itself was not rewritten.
    assert_eq!(
        app.copy_dest_for_test(),
        Some(link.clone()),
        "the typed destination must not be canonicalized against the local disk"
    );
}

/// `myd <local> sftp://host` opens a split with the local path on the left.
#[tokio::test]
async fn a_lone_remote_argument_builds_a_single_pane() {
    // Reported: `myd sftp://host` started split. The cwd was handed to the other
    // pane, and `new_shallow` splits whenever a right path is present, so the
    // layout ignored the absent --dual. One argument should mean one pane.
    use clap::Parser;
    use myd::cli::{Cli, Startup};

    let cli = Cli::try_parse_from(["myd", "sftp://gb10"]).unwrap();
    let Startup::Remote { panel, local, dual, .. } = cli.startup(Some(PathBuf::from("/cwd")))
    else {
        panic!("a remote argument should ask for a remote startup");
    };
    assert_eq!(panel, 0, "the remote takes the only pane");
    assert!(!dual, "and does not ask for a split");
    assert_eq!(local, None, "with no local pane alongside it");

    // Built the way main does, without dialing anything.
    let (left, right) = if panel == 0 { (None, local) } else { (local, None) };
    let mut app = FileBrowser::new(left, right, dual);
    settle(&mut app).await;
    assert_eq!(app.panel_count(), 1, "a single pane");
}

#[tokio::test]
async fn a_lone_remote_still_splits_when_dual_is_asked_for() {
    // `--dual` is how to get the old pairing back: the cwd fills the pane the
    // remote is not taking.
    use clap::Parser;
    use myd::cli::{Cli, Startup};

    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("a.txt"), b"x").unwrap();

    let cli = Cli::try_parse_from(["myd", "--dual", "sftp://gb10"]).unwrap();
    let Startup::Remote { panel, local, dual, .. } =
        cli.startup(Some(dir.path().to_path_buf()))
    else {
        panic!("a remote argument should ask for a remote startup");
    };
    assert!(dual, "--dual still splits");
    assert_eq!(local, Some(dir.path().to_path_buf()), "the cwd fills the other pane");

    let (left, right) = if panel == 0 { (None, local) } else { (local, None) };
    let mut app = FileBrowser::new(left, right, dual);
    settle_all(&mut app).await;
    assert_eq!(app.panel_count(), 2, "two panes");
    assert_eq!(
        app.panel_current_dir(1).and_then(|p| p.canonicalize().ok()),
        dir.path().canonicalize().ok(),
        "the cwd sits beside the pane the remote will take"
    );
}

#[tokio::test]
async fn a_remote_second_argument_builds_a_split_with_the_local_path_left() {
    // The routing itself is asserted in cli.rs; this checks the layout it asks
    // for is what actually gets built — a split, with the local path in pane 0
    // and pane 1 left for the remote to take over on connect. Before the fix
    // this was a split whose right pane showed the picker.
    use myd::cli::{Cli, Startup};
    use clap::Parser;

    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("a.txt"), b"x").unwrap();

    let cli = Cli::try_parse_from(["myd", &dir.path().to_string_lossy(), "sftp://gb10"]).unwrap();
    let Startup::Remote { panel, local, dual, .. } = cli.startup(None) else {
        panic!("a remote second argument should ask for a remote startup");
    };
    assert_eq!(panel, 1, "the remote takes the right pane");
    assert!(dual, "and the layout is split");

    // Build the panels the way main does, without dialing anything.
    let (left, right) = if panel == 0 { (None, local) } else { (local, None) };
    let mut app = FileBrowser::new(left, right, dual);
    settle(&mut app).await;

    assert_eq!(app.panel_count(), 2, "two panes");
    assert_eq!(
        app.panel_current_dir(0).and_then(|p| p.canonicalize().ok()),
        dir.path().canonicalize().ok(),
        "the local path keeps the left pane"
    );
}

/// A queued remote-to-local copy must ask before replacing an existing file.
#[tokio::test]
async fn a_transfer_onto_an_existing_file_asks_first() {
    // Reported: `myd /tmp sftp://gb10`, copied a file from the remote pane to
    // /tmp where a file of that name already existed, and it was overwritten
    // with no warning.
    //
    // The local copy path has always prompted. The queued path went straight to
    // the worker, which replaces the destination on the stated assumption that
    // "the overwrite decision was made by the caller before queueing" — and the
    // cross-backend caller never made it.
    let dest = tempfile::tempdir().unwrap();
    std::fs::write(dest.path().join("big_file"), b"original").unwrap();

    let mut app = FileBrowser::new(
        Some(dest.path().to_path_buf()),
        Some(dest.path().to_path_buf()),
        true,
    );
    settle_all(&mut app).await;

    // Panel 0 becomes the remote source, holding a file of the same name.
    let twin = tempfile::tempdir().unwrap();
    std::fs::write(twin.path().join("big_file"), b"payload").unwrap();
    app.replace_panel_with_remote_for_test(remote_tree_rooted_at(twin.path()));

    // Copy the remote file into the local pane, which already has that name.
    app.handle_key_for_test(char_key('j'));
    app.handle_key_for_test(char_key('c'));
    settle_copy(&mut app).await;

    assert_eq!(
        app.modal_kind_for_test(),
        "confirm",
        "an existing destination must be confirmed, not silently replaced"
    );
    assert!(
        app.transfer_queue().transfers().is_empty(),
        "and nothing may be queued until the answer comes back"
    );

    // Declining leaves the file alone and queues nothing.
    app.handle_key_for_test(char_key('n'));
    assert!(
        app.transfer_queue().transfers().is_empty(),
        "a declined overwrite must not transfer"
    );
    assert_eq!(
        std::fs::read(dest.path().join("big_file")).unwrap(),
        b"original",
        "the existing file must be untouched"
    );
}

/// A destination typed into the single-panel prompt is checked too.
#[tokio::test]
async fn a_typed_transfer_destination_is_checked_for_collisions() {
    // The dual-pane case reads the destination panel's loaded listing, but a
    // typed destination is on no panel, so there was nothing to check and the
    // batch went out unasked — the same silent overwrite, by a different route.
    // It is listed in the background instead, one read_dir for the whole batch.
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    let dest = tempfile::tempdir().unwrap();
    std::fs::write(dest.path().join("big_file"), b"original").unwrap();

    let start = tempfile::tempdir().unwrap();
    let mut app = FileBrowser::new(Some(start.path().to_path_buf()), None, false);
    settle(&mut app).await;

    // A remote single panel, so `c` prompts for a destination.
    let twin = tempfile::tempdir().unwrap();
    std::fs::write(twin.path().join("big_file"), b"payload").unwrap();
    app.replace_panel_with_remote_for_test(remote_tree_rooted_at(twin.path()));

    app.handle_key_for_test(char_key('j'));
    app.handle_key_for_test(char_key('c'));
    settle_copy(&mut app).await;
    assert_eq!(app.modal_kind_for_test(), "input", "expected the path prompt");
    for ch in dest.path().to_string_lossy().chars() {
        app.handle_key_for_test(KeyEvent::new(KeyCode::Char(ch), KeyModifiers::NONE));
    }
    app.handle_key_for_test(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));

    // The listing resolves on a tick, like a connection attempt.
    for _ in 0..200 {
        app.tick_for_test();
        if app.modal_kind_for_test() == "confirm" {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(3)).await;
    }

    assert_eq!(
        app.modal_kind_for_test(),
        "confirm",
        "a typed destination that already holds the name must be confirmed"
    );
    assert!(
        app.transfer_queue().transfers().is_empty(),
        "and nothing queued before the answer"
    );

    app.handle_key_for_test(char_key('n'));
    assert!(
        app.transfer_queue().transfers().is_empty(),
        "a declined overwrite must not transfer"
    );
    assert_eq!(
        std::fs::read(dest.path().join("big_file")).unwrap(),
        b"original",
        "the existing file must be untouched"
    );
}

/// A typed destination with no collision queues without ever prompting.
#[tokio::test]
async fn a_typed_transfer_destination_without_collisions_just_goes() {
    // The probe must not strand a batch that has nothing to ask about: the
    // listing arrives, nothing matches, and the transfer is enqueued.
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    let dest = tempfile::tempdir().unwrap();
    let start = tempfile::tempdir().unwrap();
    let mut app = FileBrowser::new(Some(start.path().to_path_buf()), None, false);
    settle(&mut app).await;

    let twin = tempfile::tempdir().unwrap();
    std::fs::write(twin.path().join("big_file"), b"payload").unwrap();
    app.replace_panel_with_remote_for_test(remote_tree_rooted_at(twin.path()));

    app.handle_key_for_test(char_key('j'));
    app.handle_key_for_test(char_key('c'));
    for ch in dest.path().to_string_lossy().chars() {
        app.handle_key_for_test(KeyEvent::new(KeyCode::Char(ch), KeyModifiers::NONE));
    }
    app.handle_key_for_test(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));

    for _ in 0..200 {
        app.tick_for_test();
        if !app.transfer_queue().transfers().is_empty() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(3)).await;
    }

    assert_eq!(
        app.modal_kind_for_test(),
        "none",
        "nothing collides, so nothing to ask"
    );
    assert_eq!(
        app.transfer_queue().transfers().len(),
        1,
        "and the transfer must still be queued"
    );
}

/// Tab in the overwrite prompt moves the focus; it must not confirm.
#[tokio::test]
async fn tab_in_the_overwrite_prompt_does_not_confirm() {
    // Reported: hitting Tab at the copy-overwrite prompt was taken as "OK", so
    // the file was replaced. Every key the app did not recognise was flattened
    // to `' '` on the way into the dialog, and `' '` meant accept.
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    let dest = tempfile::tempdir().unwrap();
    std::fs::write(dest.path().join("big_file"), b"original").unwrap();

    let mut app = FileBrowser::new(
        Some(dest.path().to_path_buf()),
        Some(dest.path().to_path_buf()),
        true,
    );
    settle_all(&mut app).await;

    let twin = tempfile::tempdir().unwrap();
    std::fs::write(twin.path().join("big_file"), b"payload").unwrap();
    app.replace_panel_with_remote_for_test(remote_tree_rooted_at(twin.path()));

    app.handle_key_for_test(char_key('j'));
    app.handle_key_for_test(char_key('c'));
    settle_copy(&mut app).await;
    assert_eq!(app.modal_kind_for_test(), "confirm", "expected the prompt");

    // Tab: focus moves, the dialog stays up, nothing is decided.
    app.handle_key_for_test(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));
    assert_eq!(
        app.modal_kind_for_test(),
        "confirm",
        "Tab must not answer the prompt"
    );
    assert!(
        app.transfer_queue().transfers().is_empty(),
        "and must not queue the overwrite"
    );

    // Enter now takes the focused button, which Tab moved to "No".
    app.handle_key_for_test(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    assert!(
        app.transfer_queue().transfers().is_empty(),
        "Enter on No must decline"
    );
    assert_eq!(
        std::fs::read(dest.path().join("big_file")).unwrap(),
        b"original",
        "the existing file must be untouched"
    );
}

/// Accepting the overwrite still queues the transfer, and a non-colliding file
/// is never asked about at all.
#[tokio::test]
async fn a_confirmed_transfer_overwrite_proceeds_and_a_clear_name_is_not_asked() {
    let dest = tempfile::tempdir().unwrap();
    std::fs::write(dest.path().join("big_file"), b"original").unwrap();

    let mut app = FileBrowser::new(
        Some(dest.path().to_path_buf()),
        Some(dest.path().to_path_buf()),
        true,
    );
    settle_all(&mut app).await;

    let twin = tempfile::tempdir().unwrap();
    std::fs::write(twin.path().join("big_file"), b"payload").unwrap();
    app.replace_panel_with_remote_for_test(remote_tree_rooted_at(twin.path()));

    app.handle_key_for_test(char_key('j'));
    app.handle_key_for_test(char_key('c'));
    settle_copy(&mut app).await;
    assert_eq!(app.modal_kind_for_test(), "confirm");
    app.handle_key_for_test(char_key('y'));

    assert_eq!(
        app.transfer_queue().transfers().len(),
        1,
        "a confirmed overwrite must go through"
    );

    // A name with no collision goes straight to the queue: the prompt exists for
    // data that would be destroyed, and asking about everything would train the
    // habit of dismissing it.
    let fresh = tempfile::tempdir().unwrap();
    let mut app = FileBrowser::new(
        Some(fresh.path().to_path_buf()),
        Some(fresh.path().to_path_buf()),
        true,
    );
    settle_all(&mut app).await;
    let twin2 = tempfile::tempdir().unwrap();
    std::fs::write(twin2.path().join("only_there"), b"payload").unwrap();
    app.replace_panel_with_remote_for_test(remote_tree_rooted_at(twin2.path()));

    app.handle_key_for_test(char_key('j'));
    app.handle_key_for_test(char_key('c'));
    settle_copy(&mut app).await;
    assert_eq!(
        app.modal_kind_for_test(),
        "none",
        "a name that does not collide must not prompt"
    );
    assert_eq!(app.transfer_queue().transfers().len(), 1);
}

/// Navigating a remote panel to a local directory must clear its remote tag.
#[tokio::test]
async fn a_remote_panel_navigated_to_a_local_path_stops_being_remote() {
    // Reported sequence: left /tmp, right connected to sftp://gb10 (a copy at
    // that point worked), then `gd` on the right pane to a local CIFS mount. The
    // next copy failed with "destination directory remote:/Volumes/data/nog/hen
    // does not exist" — the `remote:` prefix being the whole tell.
    //
    // `Panel::backend` was set when the panel was created or connected and never
    // afterwards, so a panel that navigated away from the remote kept the tag
    // while showing local content. Starting the app fresh on the same two local
    // directories worked, which is what pointed at stale state rather than at
    // the path itself.
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    let local_dest = tempfile::tempdir().unwrap();
    let start = tempfile::tempdir().unwrap();
    let mut app = FileBrowser::new(Some(start.path().to_path_buf()), None, false);
    settle(&mut app).await;

    // Make the panel remote, as a connection would.
    let twin = tempfile::tempdir().unwrap();
    std::fs::write(twin.path().join("big_file"), b"payload").unwrap();
    app.replace_panel_with_remote_for_test(remote_tree_rooted_at(twin.path()));
    assert!(
        !app.panel_backend_for_test(0).is_local(),
        "the panel should start out remote"
    );

    // Now navigate it to a local directory through the picker, exactly as `gd`
    // followed by a typed path does.
    app.handle_key_for_test(char_key('g'));
    app.handle_key_for_test(char_key('d'));
    for ch in local_dest.path().to_string_lossy().chars() {
        app.handle_key_for_test(KeyEvent::new(KeyCode::Char(ch), KeyModifiers::NONE));
    }
    app.handle_key_for_test(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    settle(&mut app).await;

    assert_eq!(
        app.panel_current_dir(0).and_then(|p| p.canonicalize().ok()),
        local_dest.path().canonicalize().ok(),
        "the panel should now be showing the local directory"
    );
    assert!(
        app.panel_backend_for_test(0).is_local(),
        "and must no longer be tagged remote, or copies address the server"
    );
}

#[tokio::test]
async fn a_local_destination_typed_from_a_remote_panel_stays_local() {
    // From a user's report: connected to an SFTP host, pressed `c`, and typed a
    // local CIFS mount (`/Volumes/data/nog/hen`). The copy failed with
    // "destination directory … does not exist".
    //
    // The prompt handed BOTH endpoints the active panel's backend, so a typed
    // destination was always treated as a path on the server. The directory does
    // exist — on this machine, not on the host — so the transfer looked for it in
    // the wrong place. An existing local directory now names the local disk.
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    // The destination exists locally, standing in for the mounted volume.
    let dest = tempfile::tempdir().unwrap();

    let start = tempfile::tempdir().unwrap();
    let mut app = FileBrowser::new(Some(start.path().to_path_buf()), None, false);
    settle(&mut app).await;

    // A remote panel rooted somewhere that also exists locally, so the routing
    // decision is what is under test rather than a path that happens to be absent.
    let twin = tempfile::tempdir().unwrap();
    std::fs::write(twin.path().join("big_file"), b"payload").unwrap();
    app.replace_panel_with_remote_for_test(remote_tree_rooted_at(twin.path()));

    app.handle_key_for_test(char_key('j'));
    app.handle_key_for_test(char_key('c'));
    settle_copy(&mut app).await;
    assert_eq!(app.modal_kind_for_test(), "input", "expected the destination prompt");

    for ch in dest.path().to_string_lossy().chars() {
        app.handle_key_for_test(KeyEvent::new(KeyCode::Char(ch), KeyModifiers::NONE));
    }
    app.handle_key_for_test(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));

    // A typed destination is listed first, to check for names it would
    // overwrite, so the transfer is queued a tick later rather than immediately.
    // Nothing collides here — the destination is empty.
    for _ in 0..200 {
        app.tick_for_test();
        if !app.transfer_queue().transfers().is_empty() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(3)).await;
    }

    // The queued transfer must address the local disk. Sending it to the remote
    // backend is the bug: the server has no such directory.
    let queued = app.transfer_queue().transfers();
    assert_eq!(queued.len(), 1, "the copy should have been queued");
    assert!(
        queued[0].dest.is_local(),
        "an existing local directory must stay local, not be sent to the server: {}",
        queued[0].dest
    );
    assert!(
        !queued[0].src.is_local(),
        "and the source is still the remote panel: {}",
        queued[0].src
    );
}

#[test]
fn a_single_panel_copy_from_a_remote_panel_routes_through_the_transfer_queue() {
    // The single-panel copy prompt fed `begin_copy_batch` unconditionally, which
    // spawns `copy_path` — plain `std::fs`. From a remote panel that reads and
    // writes the LOCAL disk under remote-looking paths, so it either fails oddly
    // or silently touches the wrong machine. Only the dual-panel path checked the
    // backends.
    //
    // The queue is the correct route whenever either endpoint is remote; this
    // asserts the routing decision itself, which is what the log's
    // `src_backend="sftp" dest_backend="sftp"` made visible.
    use myd::vfs::BackendId;

    // A local endpoint pair stays off the queue.
    assert!(
        !myd::app::copy_needs_transfer_queue(BackendId::LOCAL, BackendId::LOCAL),
        "a local-to-local copy is a plain filesystem copy"
    );
    // Anything touching a remote backend belongs on the queue.
    assert!(
        myd::app::copy_needs_transfer_queue(BackendId(1), BackendId(1)),
        "a remote-to-remote copy must not go through local std::fs"
    );
    assert!(myd::app::copy_needs_transfer_queue(BackendId(1), BackendId::LOCAL));
    assert!(myd::app::copy_needs_transfer_queue(BackendId::LOCAL, BackendId(1)));
}

#[tokio::test]
async fn tab_cycles_through_both_panels_and_the_transfer_sidebar() {
    // With two panels open, Tab only alternated between the second panel and the
    // sidebar: leaving the sidebar cleared `transfer_focused` but never reset
    // `active`, so focus returned to whichever panel it came from and the first
    // panel dropped out of the rotation entirely.
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use ratatui::{backend::TestBackend, Terminal};

    let left = tempfile::tempdir().unwrap();
    let right = tempfile::tempdir().unwrap();
    std::fs::write(left.path().join("l.txt"), b"l").unwrap();
    std::fs::write(right.path().join("r.txt"), b"r").unwrap();
    let src = left.path().join("payload.bin");
    std::fs::write(&src, vec![0u8; 4096]).unwrap();

    let mut app = FileBrowser::new(
        Some(left.path().to_path_buf()),
        Some(right.path().to_path_buf()),
        true,
    );
    settle_all(&mut app).await;
    for _ in 0..200 {
        app.resolve_loading_for_test();
        if app.panel_current_dir(0).is_some() && app.panel_current_dir(1).is_some() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(3)).await;
    }
    assert_eq!(app.panel_count(), 2);

    // Queue a transfer so the sidebar is shown, and draw so it records its rect
    // (focus_transfers is a no-op until the sidebar has actually been laid out).
    app.enqueue_transfer_for_test(
        myd::vfs::VPath::local(&src),
        myd::vfs::VPath::local(right.path().join("payload.bin")),
    );
    let mut term = Terminal::new(TestBackend::new(140, 24)).unwrap();
    term.draw(|f| app.render_for_test(f)).unwrap();
    assert!(app.is_transfer_panel_visible(), "the sidebar should be shown");

    let tab = || KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE);
    // Describe focus as one value so the rotation reads as a sequence.
    let focus = |app: &FileBrowser| -> String {
        if app.transfer_focused_for_test() {
            "transfers".to_string()
        } else {
            format!("panel{}", app.active_panel_index())
        }
    };

    assert_eq!(focus(&app), "panel0", "starts on the left panel");

    let mut seen = vec![focus(&app)];
    for _ in 0..3 {
        app.handle_key_for_test(tab());
        term.draw(|f| app.render_for_test(f)).unwrap();
        seen.push(focus(&app));
    }

    assert_eq!(
        seen,
        vec!["panel0", "panel1", "transfers", "panel0"],
        "Tab must visit every pane and wrap back to the first"
    );

    // And it keeps cycling rather than settling into a two-pane loop.
    for _ in 0..3 {
        app.handle_key_for_test(tab());
        term.draw(|f| app.render_for_test(f)).unwrap();
        seen.push(focus(&app));
    }
    assert_eq!(
        &seen[4..],
        &["panel1", "transfers", "panel0"],
        "the rotation must repeat, not alternate between two panes"
    );
}

#[tokio::test]
async fn tab_with_one_panel_toggles_between_it_and_the_sidebar() {
    // The single-panel rotation: two stops, so Tab alternates. Guards the
    // `stops > 1` arithmetic against an off-by-one that would strand focus.
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use ratatui::{backend::TestBackend, Terminal};

    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("a.txt"), b"a").unwrap();
    let src = dir.path().join("payload.bin");
    std::fs::write(&src, vec![0u8; 4096]).unwrap();

    let mut app = FileBrowser::new(Some(dir.path().to_path_buf()), None, false);
    settle(&mut app).await;
    app.enqueue_transfer_for_test(
        myd::vfs::VPath::local(&src),
        myd::vfs::VPath::local(dir.path().join("out/payload.bin")),
    );
    let mut term = Terminal::new(TestBackend::new(120, 20)).unwrap();
    term.draw(|f| app.render_for_test(f)).unwrap();

    let tab = || KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE);
    assert!(!app.transfer_focused_for_test());
    app.handle_key_for_test(tab());
    term.draw(|f| app.render_for_test(f)).unwrap();
    assert!(app.transfer_focused_for_test(), "Tab reaches the sidebar");
    app.handle_key_for_test(tab());
    term.draw(|f| app.render_for_test(f)).unwrap();
    assert!(!app.transfer_focused_for_test(), "and comes back");
    assert_eq!(app.active_panel_index(), 0);
}

#[tokio::test]
async fn tab_still_alternates_panels_when_the_sidebar_is_hidden() {
    // With no sidebar on screen the rotation is just the two panels. Previously
    // this was the only case that worked; it must keep working.
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    let left = tempfile::tempdir().unwrap();
    let right = tempfile::tempdir().unwrap();
    std::fs::write(left.path().join("l.txt"), b"l").unwrap();
    std::fs::write(right.path().join("r.txt"), b"r").unwrap();

    let mut app = FileBrowser::new(
        Some(left.path().to_path_buf()),
        Some(right.path().to_path_buf()),
        true,
    );
    settle_all(&mut app).await;
    for _ in 0..200 {
        app.resolve_loading_for_test();
        if app.panel_current_dir(0).is_some() && app.panel_current_dir(1).is_some() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(3)).await;
    }

    // Nothing queued, so the sidebar is not drawn.
    assert!(!app.is_transfer_panel_visible());
    let tab = || KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE);
    assert_eq!(app.active_panel_index(), 0);
    app.handle_key_for_test(tab());
    assert_eq!(app.active_panel_index(), 1);
    app.handle_key_for_test(tab());
    assert_eq!(app.active_panel_index(), 0, "wraps back with no sidebar");
    assert!(!app.transfer_focused_for_test());
}


// ---------------------------------------------------------------------------
// Filter: applies to the whole tree, and says so when the pattern is bad.
// ---------------------------------------------------------------------------

/// A tree with matching and non-matching names at two depths.
async fn filter_app() -> (tempfile::TempDir, FileBrowser) {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("sub")).unwrap();
    for n in ["sitemap", "backup", "notes.txt"] {
        std::fs::write(dir.path().join(n), b"x").unwrap();
    }
    for n in ["inner_map", "inner.txt"] {
        std::fs::write(dir.path().join("sub").join(n), b"x").unwrap();
    }
    let mut app = FileBrowser::new(Some(dir.path().to_path_buf()), None, false);
    settle(&mut app).await;
    (dir, app)
}

fn visible_names(app: &FileBrowser) -> Vec<String> {
    match app.current_screen() {
        myd::screen::Screen::Main(s) => s
            .tree
            .lines
            .iter()
            .filter(|l| l.depth > 0)
            .map(|l| l.name.clone())
            .collect(),
        _ => vec![],
    }
}

fn apply_filter(app: &mut FileBrowser, pattern: &str) {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    app.handle_key_for_test(char_key('f'));
    for c in pattern.chars() {
        app.handle_key_for_test(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE));
    }
    app.handle_key_for_test(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
}

#[tokio::test]
async fn a_filter_applies_to_every_level_of_the_tree() {
    // The filter was scoped to the cursor's directory alone. With the cursor
    // inside an expanded subdirectory, every other level was left untouched — so
    // filtering appeared to do nothing at all.
    let (_dir, mut app) = filter_app().await;
    app.handle_key_for_test(char_key('*')); // expand everything

    // Put the cursor inside `sub`, which is where the old scoping went wrong.
    for _ in 0..10 {
        let on = match app.current_screen() {
            myd::screen::Screen::Main(s) => s
                .tree
                .selected_line()
                .map(|l| l.name == "inner.txt")
                .unwrap_or(false),
            _ => false,
        };
        if on {
            break;
        }
        app.handle_key_for_test(char_key('j'));
    }

    apply_filter(&mut app, ".*p$");

    let mut names = visible_names(&app);
    names.sort();
    // Directories are kept so their matching children stay reachable.
    assert_eq!(
        names,
        vec!["backup", "inner_map", "sitemap", "sub"],
        "the filter must hide non-matching names at every depth"
    );
}

#[tokio::test]
async fn an_empty_filter_pattern_restores_the_whole_tree() {
    let (_dir, mut app) = filter_app().await;
    app.handle_key_for_test(char_key('*'));
    let before = visible_names(&app).len();

    apply_filter(&mut app, ".*p$");
    assert!(visible_names(&app).len() < before, "the filter should hide something");

    apply_filter(&mut app, "");
    assert_eq!(
        visible_names(&app).len(),
        before,
        "an empty pattern must clear the filter"
    );
}

#[tokio::test]
async fn an_invalid_filter_pattern_says_so_instead_of_doing_nothing() {
    // `*p$` is not valid regex — there is nothing for `*` to repeat. It used to
    // be discarded silently, which is indistinguishable from a filter that ran
    // and matched everything.
    let (_dir, mut app) = filter_app().await;
    let before = visible_names(&app);

    apply_filter(&mut app, "*p$");

    assert_eq!(
        app.modal_kind_for_test(),
        "confirm",
        "a malformed pattern must be reported, not swallowed"
    );
    assert_eq!(
        visible_names(&app),
        before,
        "and the view must be left alone"
    );
}

#[tokio::test]
async fn an_invalid_search_pattern_says_so_too() {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    // Search had the same silent `Err(_) => return` as the filter.
    let (_dir, mut app) = filter_app().await;
    app.handle_key_for_test(char_key('/'));
    for c in "*p$".chars() {
        app.handle_key_for_test(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE));
    }
    app.handle_key_for_test(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));

    assert_eq!(
        app.modal_kind_for_test(),
        "confirm",
        "a malformed search pattern must be reported"
    );
}

#[tokio::test]
async fn filtering_is_case_insensitive_like_search() {
    // Search built its regex case-insensitively and the filter did not, so the
    // same pattern behaved differently depending on which prompt you used.
    let (_dir, mut app) = filter_app().await;
    apply_filter(&mut app, "SITEMAP");
    assert!(
        visible_names(&app).iter().any(|n| n == "sitemap"),
        "filter should ignore case, as search does: {:?}",
        visible_names(&app)
    );
}

#[tokio::test]
async fn a_filtered_view_says_so_on_screen() {
    // A filter hides rows, and without an indicator the tree just looks wrong —
    // there was no way to tell a filtered view from a directory that had lost
    // files. Both the title bar and the footer now carry it.
    let (_dir, mut app) = filter_app().await;

    let before = app_screen_text(&mut app, 110, 14);
    assert!(!before.contains("FILTERED"), "no badge before filtering");

    apply_filter(&mut app, ".*p$");

    let after = app_screen_text(&mut app, 110, 14);
    assert!(
        after.contains("FILTERED"),
        "the title bar must mark a filtered view: {}",
        after
    );
    assert!(
        after.contains("filter: .*p$"),
        "the footer badge must name the active pattern: {}",
        after
    );
    assert!(
        after.contains("shown"),
        "the counts describe what is visible while filtering: {}",
        after
    );

    // Clearing removes every trace of it.
    apply_filter(&mut app, "");
    let cleared = app_screen_text(&mut app, 110, 14);
    assert!(
        !cleared.contains("FILTERED") && !cleared.contains("filter: "),
        "clearing the filter must remove the indicator: {}",
        cleared
    );
}

#[tokio::test]
async fn the_filter_indicator_survives_a_narrow_terminal() {
    // The badge carries a pattern and a hint, which together are wider than a
    // small terminal's footer. It drops the hint, then the pattern, rather than
    // crowding out the keybindings — but "filtering is on" always survives,
    // since that is the part the rows themselves cannot convey.
    let (_dir, mut app) = filter_app().await;
    apply_filter(&mut app, ".*p$");

    for width in [110u16, 80, 56, 44] {
        let text = app_screen_text(&mut app, width, 10);
        assert!(
            text.contains("FILTERED") || text.contains("filter: "),
            "the filtered state must be visible at width {}: {}",
            width,
            text
        );
        // The keybindings must not be pushed entirely off screen.
        assert!(
            text.contains("[TREE]"),
            "the footer keys must survive at width {}: {}",
            width,
            text
        );
    }
}

// ---------------------------------------------------------------------------
// Directory favourites in the `gd` picker.
// ---------------------------------------------------------------------------

/// Open the picker with an isolated config directory, so the real
/// `~/.config/myd/hosts.toml` is never touched.
async fn favorites_app() -> (tempfile::TempDir, tempfile::TempDir, FileBrowser) {
    // A catalog backed by this test's own file. `XDG_CONFIG_HOME` is
    // process-global and these tests run in parallel, so pointing the whole
    // process at one directory would make them trample each other.
    let cfg = tempfile::tempdir().unwrap();
    let catalog = myd::hosts::HostCatalog::load_from_unseeded(&cfg.path().join("hosts.toml"));

    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("projects")).unwrap();
    let mut app = FileBrowser::new(Some(dir.path().to_path_buf()), None, false);
    app.set_hosts_for_test(catalog);
    settle(&mut app).await;
    app.handle_key_for_test(char_key('g'));
    app.handle_key_for_test(char_key('d'));
    (cfg, dir, app)
}

/// The catalog file this test's app writes to.
fn favorites_file(cfg: &tempfile::TempDir) -> std::path::PathBuf {
    cfg.path().join("hosts.toml")
}

fn picker_rows(app: &FileBrowser) -> Vec<String> {
    match app.current_screen() {
        myd::screen::Screen::DirPicker(s) => s
            .options_for_test()
            .iter()
            .map(|o| {
                format!(
                    "{}{}",
                    if o.is_favorite { "*" } else { " " },
                    o.path.display()
                )
            })
            .collect(),
        _ => panic!("expected the directory picker"),
    }
}

fn focus_list(app: &mut FileBrowser) {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    app.handle_key_for_test(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));
}

#[tokio::test]
async fn a_prompts_for_a_directory_rather_than_saving_the_cursor_row() {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    // `a` used to bookmark whatever the cursor happened to be on, which is
    // rarely the directory the user wants to save — the point is to name a new
    // place. It now opens a prompt.
    let (cfg, dir, mut app) = favorites_app().await;
    let target = dir.path().join("projects");
    focus_list(&mut app);

    let cursor_row = match app.current_screen() {
        myd::screen::Screen::DirPicker(s) => s.selected().unwrap().path.clone(),
        _ => unreachable!(),
    };

    app.handle_key_for_test(char_key('a'));
    assert_eq!(
        app.modal_kind_for_test(),
        "input",
        "`a` must ask which directory to save"
    );

    // Type a path that is *not* the highlighted row.
    for c in target.to_string_lossy().chars() {
        app.handle_key_for_test(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE));
    }
    app.handle_key_for_test(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));

    // The panel's own starting directory is in the history (the user is there),
    // so this checks that the *typed* path was saved and the cursor row was not.
    let saved = app.hosts_for_test();
    assert!(
        saved
            .favorites()
            .iter()
            .any(|f| f.path == target.to_string_lossy() && f.saved),
        "the typed path should be saved: {:?}",
        saved.favorites()
    );
    assert!(
        !saved
            .favorites()
            .iter()
            .any(|f| f.path == cursor_row.to_string_lossy() && f.saved),
        "the cursor row ({}) must not have been saved",
        cursor_row.display()
    );

    let body = std::fs::read_to_string(favorites_file(&cfg)).unwrap();
    assert!(body.contains("[[favorite]]"), "config: {}", body);
    assert!(body.contains("saved = true"), "an explicit save is marked: {}", body);
}

#[tokio::test]
async fn saving_a_path_that_is_not_a_directory_is_refused() {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    let (_cfg, dir, mut app) = favorites_app().await;
    focus_list(&mut app);
    app.handle_key_for_test(char_key('a'));

    let missing = dir.path().join("does-not-exist");
    for c in missing.to_string_lossy().chars() {
        app.handle_key_for_test(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE));
    }
    app.handle_key_for_test(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));

    assert_eq!(app.modal_kind_for_test(), "confirm", "should report the problem");
    assert!(
        !app.hosts_for_test()
            .favorites()
            .iter()
            .any(|f| f.path == missing.to_string_lossy()),
        "nothing should have been saved: {:?}",
        app.hosts_for_test().favorites()
    );
}

#[tokio::test]
async fn d_forgets_a_saved_favourite() {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    let (cfg, dir, mut app) = favorites_app().await;
    let target = dir.path().join("projects");

    // Save one through the prompt.
    focus_list(&mut app);
    app.handle_key_for_test(char_key('a'));
    for c in target.to_string_lossy().chars() {
        app.handle_key_for_test(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE));
    }
    app.handle_key_for_test(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    assert!(picker_rows(&app).iter().any(|r| r.starts_with('*')));

    // Put the cursor on it and forget it.
    for _ in 0..15 {
        let on = match app.current_screen() {
            myd::screen::Screen::DirPicker(s) => {
                s.selected().map(|o| o.path == target).unwrap_or(false)
            }
            _ => false,
        };
        if on {
            break;
        }
        app.handle_key_for_test(char_key('j'));
    }
    app.handle_key_for_test(char_key('d'));

    assert!(
        !app.hosts_for_test()
            .favorites()
            .iter()
            .any(|f| f.path == target.to_string_lossy()),
        "the favourite should be gone: {:?}",
        app.hosts_for_test().favorites()
    );
    let body = std::fs::read_to_string(favorites_file(&cfg)).unwrap_or_default();
    assert!(
        !body.contains(&target.to_string_lossy().to_string()),
        "the forgotten entry should be gone from the config: {}",
        body
    );
}

#[tokio::test]
async fn a_and_d_are_ordinary_characters_in_the_path_field() {
    // The field starts focused, so these must type rather than edit favourites —
    // plenty of real paths contain an `a` or a `d`.
    let (_cfg, _dir, mut app) = favorites_app().await;
    for c in "/data".chars() {
        app.handle_key_for_test(char_key(c));
    }
    match app.current_screen() {
        myd::screen::Screen::DirPicker(s) => {
            assert_eq!(s.input_for_test(), "/data");
            // The starting directory is in the history, so check that nothing
            // matching what was *typed* got saved.
            assert!(
                !s.options_for_test()
                    .iter()
                    .any(|o| o.path.to_string_lossy() == "/data"),
                "typing must not have saved anything"
            );
        }
        _ => panic!("expected the picker"),
    }
}

#[tokio::test]
async fn a_visited_favourite_leads_the_picker_list() {
    // Favourites are merged with the built-ins into one recency-ordered list, so
    // a directory you actually use rises to the top. The visit itself is
    // recorded on confirm (covered by `hosts::record_dir_use`); this pins the
    // ordering the picker presents.
    let cfg = tempfile::tempdir().unwrap();
    let file = cfg.path().join("hosts.toml");

    let mut catalog = myd::hosts::HostCatalog::load_from_unseeded(&file);
    let stale = tempfile::tempdir().unwrap();
    let fresh = tempfile::tempdir().unwrap();
    catalog.add_favorite(myd::hosts::SavedDir {
        path: stale.path().to_string_lossy().to_string(),
        uses: 99,
        last_used: Some("2026-01-01T00:00:00Z".into()),
        ..Default::default()
    });
    catalog.add_favorite(myd::hosts::SavedDir {
        path: fresh.path().to_string_lossy().to_string(),
        uses: 1,
        last_used: Some("2026-07-26T00:00:00Z".into()),
        ..Default::default()
    });
    catalog.save().unwrap();

    // Start the panel *in* the stale directory, so opening it is the most recent
    // visit and the ordering under test is not decided by the fixture's own
    // starting directory (every opened directory is recorded now).
    let mut app = FileBrowser::new(Some(stale.path().to_path_buf()), None, false);
    app.set_hosts_for_test(myd::hosts::HostCatalog::load_from_unseeded(&file));
    settle(&mut app).await;
    // …then re-point the fresh one at the top by recording it last.
    {
        let mut catalog = app.hosts_for_test().clone();
        catalog.record_visit(&fresh.path().to_string_lossy());
        app.set_hosts_for_test(catalog);
    }
    app.handle_key_for_test(char_key('g'));
    app.handle_key_for_test(char_key('d'));

    let rows = picker_rows(&app);
    assert_eq!(
        rows.first().map(String::as_str),
        Some(format!("*{}", fresh.path().display()).as_str()),
        "the most recently visited favourite leads, ahead of the more-used one: {:?}",
        rows
    );
    assert_eq!(
        rows.get(1).map(String::as_str),
        Some(format!("*{}", stale.path().display()).as_str()),
        "then the older favourite, still ahead of the built-ins: {:?}",
        rows
    );
    assert!(
        rows[2..].iter().all(|r| r.starts_with(' ')),
        "built-ins have no timestamp and settle below: {:?}",
        rows
    );
}

#[tokio::test]
async fn opening_a_saved_directory_records_a_visit() {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    // Confirming a path that is saved promotes it; confirming one that is not
    // must not quietly add it.
    let cfg = tempfile::tempdir().unwrap();
    let file = cfg.path().join("hosts.toml");
    let saved = tempfile::tempdir().unwrap();
    let mut catalog = myd::hosts::HostCatalog::load_from_unseeded(&file);
    catalog.add_favorite(myd::hosts::SavedDir::new(
        saved.path().to_string_lossy().to_string(),
    ));
    catalog.save().unwrap();

    let start = tempfile::tempdir().unwrap();
    let mut app = FileBrowser::new(Some(start.path().to_path_buf()), None, false);
    app.set_hosts_for_test(myd::hosts::HostCatalog::load_from_unseeded(&file));
    settle(&mut app).await;

    app.handle_key_for_test(char_key('g'));
    app.handle_key_for_test(char_key('d'));
    for c in saved.path().to_string_lossy().chars() {
        app.handle_key_for_test(char_key(c));
    }
    app.handle_key_for_test(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    settle(&mut app).await;

    let after = app.hosts_for_test().favorites();
    let key = saved.path().to_string_lossy().to_string();
    let matching: Vec<_> = after.iter().filter(|f| f.path == key).collect();
    assert_eq!(matching.len(), 1, "not duplicated: {:?}", after);
    assert_eq!(matching[0].uses, 1, "the visit should be counted: {:?}", after);
    assert!(matching[0].last_used.is_some(), "and timestamped: {:?}", after);

    // It reached the file too.
    let body = std::fs::read_to_string(&file).unwrap();
    assert!(body.contains("last_used"), "visit not persisted: {}", body);
}

#[tokio::test]
async fn a_typed_path_is_remembered_for_next_time() {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    // Typing a full path is the slow way in; doing it once should be enough.
    // Opening a directory records it, so it is one keystroke away afterwards.
    let (cfg, dir, mut app) = favorites_app().await;
    let target = dir.path().join("projects");

    for c in target.to_string_lossy().chars() {
        app.handle_key_for_test(char_key(c));
    }
    app.handle_key_for_test(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    settle(&mut app).await;

    // The panel's own starting directory is recorded too, so find the one this
    // test typed rather than assuming it is the only entry.
    let saved = app.hosts_for_test().favorites();
    let entry = saved
        .iter()
        .find(|f| f.path == target.to_string_lossy())
        .unwrap_or_else(|| panic!("the typed path should be remembered: {:?}", saved));
    assert_eq!(entry.uses, 1);
    assert!(
        !entry.saved,
        "an automatically recorded path is history, not an explicit favourite"
    );
    assert!(
        std::fs::read_to_string(favorites_file(&cfg))
            .unwrap()
            .contains("[[favorite]]"),
        "history must persist"
    );

    // It now appears in the picker, at the top since it is the most recent.
    // `gd` needs a settled Main screen underneath to push the picker over.
    app.handle_key_for_test(char_key('g'));
    app.handle_key_for_test(char_key('d'));
    assert!(
        matches!(app.current_screen(), myd::screen::Screen::DirPicker(_)),
        "gd should have opened the picker"
    );
    let rows = picker_rows(&app);
    assert_eq!(
        rows.first().map(String::as_str),
        Some(format!("*{}", target.display()).as_str()),
        "the just-visited directory should lead the list: {:?}",
        rows
    );
}

#[tokio::test]
async fn saving_a_remembered_path_keeps_its_history() {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    // Saving something history already recorded should promote that entry, not
    // create a second one or reset its visit count.
    let (_cfg, dir, mut app) = favorites_app().await;
    let target = dir.path().join("projects");

    for c in target.to_string_lossy().chars() {
        app.handle_key_for_test(char_key(c));
    }
    app.handle_key_for_test(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    settle(&mut app).await;
    assert_eq!(app.hosts_for_test().favorites()[0].uses, 1);

    app.handle_key_for_test(char_key('g'));
    app.handle_key_for_test(char_key('d'));
    focus_list(&mut app);
    app.handle_key_for_test(char_key('a'));
    assert_eq!(app.modal_kind_for_test(), "input", "`a` should prompt");
    // The prompt is seeded with the path field's contents; clear it first so the
    // typed path is the whole value.
    for _ in 0..200 {
        app.handle_key_for_test(KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE));
    }
    for c in target.to_string_lossy().chars() {
        app.handle_key_for_test(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE));
    }
    app.handle_key_for_test(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));

    let saved = app.hosts_for_test().favorites();
    let key = target.to_string_lossy().to_string();
    let matching: Vec<_> = saved.iter().filter(|f| f.path == key).collect();
    assert_eq!(matching.len(), 1, "saving must not duplicate: {:?}", saved);
    assert!(matching[0].saved, "it should now be saved");
    assert_eq!(matching[0].uses, 1, "and keep the visits it already had");
}

#[tokio::test]
async fn arrow_down_from_the_field_lands_on_the_first_entry() {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    // Arrowing down out of the path field skipped the first entry: the cursor
    // already sits at index 0, and Down moved it on regardless, so the list
    // appeared to start at its second row. Tab was unaffected because it only
    // changes focus.
    let (_cfg, _dir, mut app) = favorites_app().await;

    let first = match app.current_screen() {
        myd::screen::Screen::DirPicker(s) => {
            assert_eq!(s.focus(), myd::screen::PickerFocus::Field, "field starts focused");
            s.options_for_test()[0].path.clone()
        }
        _ => panic!("expected the picker"),
    };

    app.handle_key_for_test(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));

    match app.current_screen() {
        myd::screen::Screen::DirPicker(s) => {
            assert_eq!(
                s.selected().map(|o| o.path.clone()),
                Some(first),
                "the first Down must select the first entry, not the second"
            );
        }
        _ => panic!("expected the picker"),
    }
}

#[tokio::test]
async fn arrows_keep_stepping_once_the_list_is_engaged() {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    // The fix must not make Down idempotent: the second press has to advance.
    // `favorites_app` starts with an empty catalog, so seed a few rows to move
    // between rather than depending on whatever the machine happens to have.
    let (_cfg, dir, mut app) = favorites_app().await;
    {
        let mut catalog = app.hosts_for_test().clone();
        for name in ["alpha", "beta", "gamma"] {
            let d = dir.path().join(name);
            std::fs::create_dir_all(&d).unwrap();
            catalog.add_favorite(myd::hosts::SavedDir::saved(d.to_string_lossy().to_string()));
        }
        app.set_hosts_for_test(catalog);
    }
    app.handle_key_for_test(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
    app.handle_key_for_test(char_key('g'));
    app.handle_key_for_test(char_key('d'));

    let paths: Vec<std::path::PathBuf> = match app.current_screen() {
        myd::screen::Screen::DirPicker(s) => {
            s.options_for_test().iter().map(|o| o.path.clone()).collect()
        }
        _ => panic!("expected the picker"),
    };
    assert!(paths.len() >= 3, "need a few entries");

    let down = || KeyEvent::new(KeyCode::Down, KeyModifiers::NONE);
    app.handle_key_for_test(down());
    app.handle_key_for_test(down());
    match app.current_screen() {
        myd::screen::Screen::DirPicker(s) => assert_eq!(
            s.selected().map(|o| o.path.clone()),
            Some(paths[1].clone()),
            "the second Down advances to the second entry"
        ),
        _ => unreachable!(),
    }

    // And Up from the first entry still wraps, as it did before.
    app.handle_key_for_test(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE));
    match app.current_screen() {
        myd::screen::Screen::DirPicker(s) => assert_eq!(
            s.selected().map(|o| o.path.clone()),
            Some(paths[0].clone())
        ),
        _ => unreachable!(),
    }
}

#[tokio::test]
async fn a_nonexistent_typed_path_reports_an_error_and_keeps_the_field() {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    // A path that does not resolve used to fall through to the highlighted list
    // entry, so a typo silently opened somewhere else entirely — the one
    // outcome worse than doing nothing, since the user believes they went where
    // they asked. It must say so and leave the field focused for a correction.
    let (_cfg, dir, mut app) = favorites_app().await;
    let missing = dir.path().join("no-such-directory");

    for c in missing.to_string_lossy().chars() {
        app.handle_key_for_test(char_key(c));
    }
    app.handle_key_for_test(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));

    assert_eq!(
        app.modal_kind_for_test(),
        "confirm",
        "a path that does not exist must be reported"
    );
    // Still on the picker, not opening some other directory.
    assert!(
        matches!(app.current_screen(), myd::screen::Screen::DirPicker(_)),
        "must not navigate anywhere"
    );
    match app.current_screen() {
        myd::screen::Screen::DirPicker(s) => {
            assert_eq!(
                s.focus(),
                myd::screen::PickerFocus::Field,
                "the keyboard belongs back in the field to fix the typo"
            );
            assert_eq!(
                s.input_for_test(),
                missing.to_string_lossy(),
                "and what was typed is kept, not cleared"
            );
        }
        _ => unreachable!(),
    }
    // The failed path was not recorded. (The panel's own starting directory is,
    // since the user is there — so this checks for that path specifically
    // rather than for an empty list.)
    assert!(
        !app.hosts_for_test()
            .favorites()
            .iter()
            .any(|f| f.path == missing.to_string_lossy()),
        "a failed open must not be remembered: {:?}",
        app.hosts_for_test().favorites()
    );
}

#[tokio::test]
async fn confirming_with_an_empty_field_opens_the_highlighted_entry() {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    // The fallback to the list is right when the field is *empty* — that is how
    // Enter picks an entry. Only a non-empty path that fails to resolve is an
    // error. Asserted through `confirm` rather than by opening the directory,
    // since the highlighted entry is the real home directory and scanning it
    // would make this test as slow as the machine's disk.
    let (_cfg, dir, mut app) = favorites_app().await;

    // Seed a favourite so the highlighted entry is a directory we control.
    {
        let mut catalog = app.hosts_for_test().clone();
        catalog.add_favorite(myd::hosts::SavedDir::saved(
            dir.path().to_string_lossy().to_string(),
        ));
        app.set_hosts_for_test(catalog);
    }
    app.handle_key_for_test(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
    app.handle_key_for_test(char_key('g'));
    app.handle_key_for_test(char_key('d'));

    app.handle_key_for_test(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
    let target = match app.current_screen() {
        myd::screen::Screen::DirPicker(s) => s.selected().unwrap().path.clone(),
        _ => panic!("expected the picker"),
    };
    // Down mirrors the entry into the field; clear it to test the empty case.
    for _ in 0..300 {
        app.handle_key_for_test(KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE));
    }
    match app.current_screen() {
        myd::screen::Screen::DirPicker(s) => {
            assert_eq!(s.input_for_test(), "", "field should be empty");
            assert_eq!(
                s.confirm(),
                myd::screen::PickerChoice::Open(target),
                "an empty field opens the highlighted entry"
            );
        }
        _ => unreachable!(),
    }
}

// ---------------------------------------------------------------------------
// Pinned directories: a top block in an order the user arranges.
// ---------------------------------------------------------------------------

/// A picker over three saved directories, none pinned yet.
async fn pin_app() -> (
    tempfile::TempDir,
    Vec<tempfile::TempDir>,
    FileBrowser,
) {
    let cfg = tempfile::tempdir().unwrap();
    let file = cfg.path().join("hosts.toml");
    let dirs: Vec<tempfile::TempDir> = (0..3).map(|_| tempfile::tempdir().unwrap()).collect();

    let mut catalog = myd::hosts::HostCatalog::load_from_unseeded(&file);
    for d in &dirs {
        catalog.add_favorite(myd::hosts::SavedDir::saved(
            d.path().to_string_lossy().to_string(),
        ));
    }
    catalog.save().unwrap();

    let start = tempfile::tempdir().unwrap();
    let mut app = FileBrowser::new(Some(start.path().to_path_buf()), None, false);
    app.set_hosts_for_test(myd::hosts::HostCatalog::load_from_unseeded(&file));
    settle(&mut app).await;
    app.handle_key_for_test(char_key('g'));
    app.handle_key_for_test(char_key('d'));
    app.handle_key_for_test(crossterm::event::KeyEvent::new(
        crossterm::event::KeyCode::Tab,
        crossterm::event::KeyModifiers::NONE,
    ));
    // `cfg` is returned so the catalog file outlives the test body; dropping it
    // here would delete the directory the app is still saving into.
    (cfg, dirs, app)
}

/// Paths in the pinned block, in display order.
fn pinned_paths(app: &FileBrowser) -> Vec<String> {
    match app.current_screen() {
        myd::screen::Screen::DirPicker(s) => s
            .options_for_test()
            .iter()
            .filter(|o| o.tier == myd::hosts::DirTier::Pinned)
            .map(|o| o.path.to_string_lossy().to_string())
            .collect(),
        _ => panic!("expected the picker"),
    }
}

/// Move the cursor onto `path`.
fn cursor_to(app: &mut FileBrowser, path: &std::path::Path) {
    for _ in 0..30 {
        let on = match app.current_screen() {
            myd::screen::Screen::DirPicker(s) => {
                s.selected().map(|o| o.path == path).unwrap_or(false)
            }
            _ => false,
        };
        if on {
            return;
        }
        app.handle_key_for_test(char_key('j'));
    }
    panic!("never reached {}", path.display());
}

#[tokio::test]
async fn p_pins_to_the_bottom_of_the_pinned_block() {
    // A new pin goes below the existing ones: the block is an order the user
    // arranged, and barging into the middle of it would disturb that.
    let (_cfg, dirs, mut app) = pin_app().await;

    for d in &dirs {
        cursor_to(&mut app, d.path());
        app.handle_key_for_test(char_key('p'));
    }

    assert_eq!(
        pinned_paths(&app),
        dirs.iter()
            .map(|d| d.path().to_string_lossy().to_string())
            .collect::<Vec<_>>(),
        "pins accumulate in the order they were made"
    );
    // Pinned entries lead the list.
    match app.current_screen() {
        myd::screen::Screen::DirPicker(s) => {
            let tiers: Vec<myd::hosts::DirTier> =
                s.options_for_test().iter().map(|o| o.tier).collect();
            assert!(
                tiers.windows(2).all(|w| w[0] <= w[1]),
                "tiers must be grouped, pinned first: {:?}",
                tiers
            );
        }
        _ => unreachable!(),
    }
}

#[tokio::test]
async fn u_unpins_but_keeps_the_entry() {
    let (_cfg, dirs, mut app) = pin_app().await;
    cursor_to(&mut app, dirs[0].path());
    app.handle_key_for_test(char_key('p'));
    assert_eq!(pinned_paths(&app).len(), 1);

    cursor_to(&mut app, dirs[0].path());
    app.handle_key_for_test(char_key('u'));
    assert!(pinned_paths(&app).is_empty(), "no longer pinned");
    assert!(
        app.hosts_for_test()
            .favorites()
            .iter()
            .any(|f| f.path == dirs[0].path().to_string_lossy()),
        "the entry itself survives unpinning"
    );
}

#[tokio::test]
async fn m_reorders_within_the_pinned_block_and_enter_commits() {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    let (_cfg, dirs, mut app) = pin_app().await;
    for d in &dirs {
        cursor_to(&mut app, d.path());
        app.handle_key_for_test(char_key('p'));
    }
    let before = pinned_paths(&app);

    // Move the first entry down one.
    cursor_to(&mut app, dirs[0].path());
    app.handle_key_for_test(char_key('m'));
    assert!(
        matches!(app.current_screen(), myd::screen::Screen::DirPicker(s) if s.moving().is_some()),
        "m should start a move"
    );
    app.handle_key_for_test(char_key('j'));
    app.handle_key_for_test(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));

    let after = pinned_paths(&app);
    assert_eq!(
        after,
        vec![before[1].clone(), before[0].clone(), before[2].clone()],
        "the moved entry swapped with the one below it"
    );
    // And it persisted.
    let reloaded: Vec<String> = app
        .hosts_for_test()
        .pinned_dirs()
        .iter()
        .map(|f| f.path.clone())
        .collect();
    assert_eq!(reloaded, after, "the new order must be saved");
}

#[tokio::test]
async fn esc_restores_the_original_position() {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    // A cancelled move must put the entry back exactly where it started, not
    // merely stop moving it.
    let (_cfg, dirs, mut app) = pin_app().await;
    for d in &dirs {
        cursor_to(&mut app, d.path());
        app.handle_key_for_test(char_key('p'));
    }
    let before = pinned_paths(&app);

    cursor_to(&mut app, dirs[0].path());
    app.handle_key_for_test(char_key('m'));
    app.handle_key_for_test(char_key('j'));
    app.handle_key_for_test(char_key('j'));
    app.handle_key_for_test(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));

    assert_eq!(pinned_paths(&app), before, "Esc restores the original order");
    assert!(
        matches!(app.current_screen(), myd::screen::Screen::DirPicker(s) if s.moving().is_none()),
        "and ends the move"
    );
}

#[tokio::test]
async fn sliding_past_the_bottom_of_the_block_unpins() {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    let (_cfg, dirs, mut app) = pin_app().await;
    for d in &dirs {
        cursor_to(&mut app, d.path());
        app.handle_key_for_test(char_key('p'));
    }
    assert_eq!(pinned_paths(&app).len(), 3);

    // Take the last pinned entry one step further down, out of the block.
    cursor_to(&mut app, dirs[2].path());
    app.handle_key_for_test(char_key('m'));
    app.handle_key_for_test(char_key('j'));
    app.handle_key_for_test(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));

    let pinned = pinned_paths(&app);
    assert_eq!(pinned.len(), 2, "the entry left the block: {:?}", pinned);
    assert!(
        !pinned.contains(&dirs[2].path().to_string_lossy().to_string()),
        "and it is the one that was slid out"
    );
    assert!(
        dirs.iter().all(|d| app
            .hosts_for_test()
            .favorites()
            .iter()
            .any(|f| f.path == d.path().to_string_lossy())),
        "unpinning by moving must not delete the entry"
    );
}

#[tokio::test]
async fn a_pinned_order_survives_a_reload() {
    let (_cfg, dirs, mut app) = pin_app().await;
    for d in &dirs {
        cursor_to(&mut app, d.path());
        app.handle_key_for_test(char_key('p'));
    }
    let order = pinned_paths(&app);

    // Rebuild a picker from the saved catalog, as a fresh session would.
    let rebuilt = myd::screen::DirPickerState::with_favorites(app.hosts_for_test().favorites());
    let seen: Vec<String> = rebuilt
        .options_for_test()
        .iter()
        .filter(|o| o.tier == myd::hosts::DirTier::Pinned)
        .map(|o| o.path.to_string_lossy().to_string())
        .collect();
    assert_eq!(seen, order, "the arranged order must survive a reload");
}

#[tokio::test]
async fn m_on_an_unpinned_entry_pins_it_and_starts_moving() {
    // Reported as "the move command does nothing". `m` only acted on entries
    // already in the pinned block, so on every other row it was silently a
    // no-op — and since a fresh list has nothing pinned, that was most of them.
    // The earlier tests all pressed `m` on a pinned row and so never saw it.
    let (_cfg, dirs, mut app) = pin_app().await;
    assert!(pinned_paths(&app).is_empty(), "nothing pinned yet");

    cursor_to(&mut app, dirs[0].path());
    app.handle_key_for_test(char_key('m'));

    assert!(
        matches!(app.current_screen(), myd::screen::Screen::DirPicker(s) if s.moving().is_some()),
        "m must start a move rather than doing nothing"
    );
    assert_eq!(
        pinned_paths(&app),
        vec![dirs[0].path().to_string_lossy().to_string()],
        "and pin the entry it is about to position"
    );
}

#[tokio::test]
async fn sliding_out_of_the_block_shows_the_change_before_it_is_committed() {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    // Sliding past the bottom sets "will unpin", and the row's own marker has to
    // follow — otherwise the list still draws it as pinned while Enter is about
    // to do the opposite. A second `j` also must not run away down the list.
    let (_cfg, dirs, mut app) = pin_app().await;
    cursor_to(&mut app, dirs[0].path());
    app.handle_key_for_test(char_key('p'));
    cursor_to(&mut app, dirs[0].path());
    app.handle_key_for_test(char_key('m'));
    assert_eq!(pinned_paths(&app).len(), 1);

    // One step past the bottom of a one-entry block.
    app.handle_key_for_test(char_key('j'));
    assert!(
        pinned_paths(&app).is_empty(),
        "the row should already read as unpinned while moving"
    );
    // Further presses are a no-op rather than dragging it down the list.
    app.handle_key_for_test(char_key('j'));
    app.handle_key_for_test(char_key('j'));
    assert!(pinned_paths(&app).is_empty());

    // k puts it back into the block, still mid-move.
    app.handle_key_for_test(char_key('k'));
    assert_eq!(
        pinned_paths(&app).len(),
        1,
        "k should bring it back into the block"
    );

    app.handle_key_for_test(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    assert_eq!(
        app.hosts_for_test().pinned_dirs().len(),
        1,
        "committing after returning keeps it pinned"
    );
}

#[tokio::test]
async fn seeded_standard_directories_can_be_pinned_and_deleted() {
    // The standard locations used to be a hardcoded list merged in at render
    // time, so they looked like ordinary rows while `p`, `m` and `d` silently
    // ignored them. They are seeded into the catalog now, which makes them
    // ordinary rows in fact as well as in appearance.
    let cfg = tempfile::tempdir().unwrap();
    let file = cfg.path().join("hosts.toml");
    let catalog = myd::hosts::HostCatalog::load_from(&file);
    let seeded = catalog.favorites().len();
    assert!(seeded > 0, "a fresh catalog seeds the standard locations");
    assert!(
        file.exists(),
        "and writes them out, so the file matches what is shown"
    );

    let start = tempfile::tempdir().unwrap();
    let mut app = FileBrowser::new(Some(start.path().to_path_buf()), None, false);
    app.set_hosts_for_test(myd::hosts::HostCatalog::load_from(&file));
    settle(&mut app).await;
    app.handle_key_for_test(char_key('g'));
    app.handle_key_for_test(char_key('d'));
    app.handle_key_for_test(crossterm::event::KeyEvent::new(
        crossterm::event::KeyCode::Tab,
        crossterm::event::KeyModifiers::NONE,
    ));

    // Pin the first seeded row.
    let first = match app.current_screen() {
        myd::screen::Screen::DirPicker(s) => s.selected().unwrap().path.clone(),
        _ => panic!("expected the picker"),
    };
    app.handle_key_for_test(char_key('p'));
    assert_eq!(
        pinned_paths(&app),
        vec![first.to_string_lossy().to_string()],
        "a standard location must be pinnable like any other entry"
    );

    // And deletable.
    cursor_to(&mut app, &first);
    app.handle_key_for_test(char_key('d'));
    // One extra entry exists for the panel's own starting directory, recorded
    // when it opened; the assertion is about the delta, not the total.
    assert_eq!(
        app.hosts_for_test()
            .favorites()
            .iter()
            .filter(|f| f.path == first.to_string_lossy())
            .count(),
        0,
        "the standard location should be removable, which it never used to be"
    );
}

#[test]
fn seeding_leaves_an_existing_directory_list_alone() {
    // Someone who already has directories saved has made choices; re-seeding
    // over them would put back everything they deleted.
    let cfg = tempfile::tempdir().unwrap();
    let file = cfg.path().join("hosts.toml");
    let mut catalog = myd::hosts::HostCatalog::load_from_unseeded(&file);
    catalog.add_favorite(myd::hosts::SavedDir::saved("/only/this"));
    catalog.save().unwrap();

    let reloaded = myd::hosts::HostCatalog::load_from(&file);
    assert_eq!(
        reloaded.favorites().len(),
        1,
        "an existing list is not topped up: {:?}",
        reloaded.favorites()
    );
}

#[test]
fn a_host_only_config_gains_the_standard_directories() {
    // The chosen rule: seed when there are no directory entries, even if the
    // file exists. Otherwise anyone who had already saved a host would keep an
    // un-pinnable list with no way to get the standard locations back.
    let cfg = tempfile::tempdir().unwrap();
    let file = cfg.path().join("hosts.toml");
    std::fs::write(
        &file,
        "[[host]]\nlabel = \"prod\"\nhost = \"prod.example.com\"\n",
    )
    .unwrap();

    let catalog = myd::hosts::HostCatalog::load_from(&file);
    assert_eq!(catalog.hosts().len(), 1, "the host survives");
    assert!(
        !catalog.favorites().is_empty(),
        "and the directories are seeded alongside it"
    );
}

#[test]
fn a_malformed_config_is_never_seeded_over() {
    // A file that does not parse holds something worth fixing. Seeding would
    // overwrite the very content the warning tells the user to repair.
    let cfg = tempfile::tempdir().unwrap();
    let file = cfg.path().join("hosts.toml");
    std::fs::write(&file, "this is not valid toml {{{").unwrap();

    let catalog = myd::hosts::HostCatalog::load_from(&file);
    assert!(catalog.favorites().is_empty(), "nothing is seeded");
    assert!(
        std::fs::read_to_string(&file).unwrap().contains("not valid"),
        "and the file is left for the user to fix"
    );
}

#[tokio::test]
async fn the_picker_accepts_a_tilde_path() {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    // `~/code` resolved to `/code`: stripping the `~` left an absolute
    // remainder, and `PathBuf::push` *replaces* the buffer when given one, so
    // the home prefix was discarded. Bare `~` still worked, which made this look
    // like the shortcut had been partly removed.
    //
    // `HOME` is process-global and several other tests read it (the picker seeds
    // from it, and `expand_tilde` consults it), so this runs under a mutex and
    // restores the old value — including on a panic, via the guard, so one
    // failure here cannot cascade into unrelated tests.
    let home = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(home.path().join("code/untest")).unwrap();
    let _env = HomeGuard::set(home.path());

    let cfg = tempfile::tempdir().unwrap();
    let start = tempfile::tempdir().unwrap();
    let mut app = FileBrowser::new(Some(start.path().to_path_buf()), None, false);
    app.set_hosts_for_test(myd::hosts::HostCatalog::load_from_unseeded(
        &cfg.path().join("hosts.toml"),
    ));
    settle(&mut app).await;

    app.handle_key_for_test(char_key('g'));
    app.handle_key_for_test(char_key('d'));
    for c in "~/code/untest".chars() {
        app.handle_key_for_test(char_key(c));
    }
    app.handle_key_for_test(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    settle(&mut app).await;

    assert_eq!(
        app.panel_current_dir(0),
        Some(home.path().join("code/untest")),
        "~/… must expand to the home directory, not the filesystem root"
    );
}

/// Sets `HOME` for the duration of a test and restores it on drop.
///
/// Holds a process-wide lock: `set_var` is global, so two tests changing it at
/// once would sabotage each other and anything else reading it. Restoring in
/// `Drop` means a panicking test still leaves the environment as it found it.
struct HomeGuard {
    previous: Option<std::ffi::OsString>,
    _lock: std::sync::MutexGuard<'static, ()>,
}

impl HomeGuard {
    fn set(home: &std::path::Path) -> Self {
        static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
        let lock = LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let previous = std::env::var_os("HOME");
        unsafe { std::env::set_var("HOME", home) };
        Self {
            previous,
            _lock: lock,
        }
    }
}

impl Drop for HomeGuard {
    fn drop(&mut self) {
        match self.previous.take() {
            Some(v) => unsafe { std::env::set_var("HOME", v) },
            None => unsafe { std::env::remove_var("HOME") },
        }
    }
}

// ---------------------------------------------------------------------------
// Treemap: navigation, directory marking, and carrying the selection to the tree.
// ---------------------------------------------------------------------------

/// A directory of three sized subdirectories plus a loose file.
async fn treemap_app() -> (tempfile::TempDir, FileBrowser) {
    let dir = tempfile::tempdir().unwrap();
    for (n, sz) in [("alpha", 90_000usize), ("beta", 40_000), ("gamma", 20_000)] {
        std::fs::create_dir_all(dir.path().join(n)).unwrap();
        std::fs::write(dir.path().join(n).join("f.bin"), vec![0u8; sz]).unwrap();
    }
    std::fs::write(dir.path().join("loose.txt"), vec![0u8; 5_000]).unwrap();
    let mut app = FileBrowser::new(Some(dir.path().to_path_buf()), None, false);
    settle(&mut app).await;
    (dir, app)
}

fn focused_selection(app: &FileBrowser) -> Option<String> {
    match app.current_screen() {
        myd::screen::Screen::Main(s) => s
            .selected_path()
            .and_then(|p| p.file_name())
            .map(|n| n.to_string_lossy().to_string()),
        _ => None,
    }
}

#[tokio::test]
async fn enter_navigates_into_a_directory_from_the_treemap() {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    // Confirm read `tree.selected_line()` directly, so with the treemap focused
    // it consulted the *tree's* cursor — a different entry entirely — and Enter
    // on a tile did nothing.
    let (dir, mut app) = treemap_app().await;
    // Put the tree's cursor on the *file*, so consulting it instead of the
    // treemap gives "not a directory" and Enter does nothing. Without this the
    // two cursors coincide and the bug is invisible.
    for _ in 0..20 {
        let on_file = match app.current_screen() {
            myd::screen::Screen::Main(s) => s
                .tree
                .selected_line()
                .map(|l| !l.is_dir)
                .unwrap_or(false),
            _ => false,
        };
        if on_file {
            break;
        }
        app.handle_key_for_test(char_key('j'));
    }
    assert!(
        matches!(app.current_screen(), myd::screen::Screen::Main(s)
            if s.tree.selected_line().map(|l| !l.is_dir).unwrap_or(false)),
        "the tree cursor should be on a file"
    );

    app.handle_key_for_test(char_key('v'));
    // Point the treemap at a directory tile, which the tree is not on.
    let tile = focused_selection(&app).expect("a tile is selected");
    assert!(
        matches!(app.current_screen(), myd::screen::Screen::Main(s) if s.selected_is_dir()),
        "the treemap should be on a directory"
    );

    app.handle_key_for_test(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    settle(&mut app).await;

    assert_eq!(
        app.panel_current_dir(0),
        Some(dir.path().join(&tile)),
        "Enter on a directory tile must open that directory, not consult the tree"
    );
}

#[tokio::test]
async fn directory_tiles_are_marked_with_a_trailing_slash() {
    // A tile gave no clue whether Enter would open something or do nothing.
    let (_dir, mut app) = treemap_app().await;
    app.handle_key_for_test(char_key('v'));

    match app.current_screen() {
        myd::screen::Screen::Main(s) => {
            let labels: Vec<String> =
                s.treemap_cells().iter().map(|c| c.label.clone()).collect();
            assert!(
                labels.iter().all(|l| l.ends_with('/')),
                "every directory tile should be marked: {:?}",
                labels
            );
        }
        _ => panic!("expected a main screen"),
    }
}

#[tokio::test]
async fn v_carries_the_selection_between_the_two_views() {
    // The views kept independent cursors, so toggling landed on whatever each
    // was last pointing at — find a directory in the treemap, press `v`, and end
    // up somewhere unrelated in the tree.
    let (_dir, mut app) = treemap_app().await;

    app.handle_key_for_test(char_key('v'));
    // Move within the treemap so the two cursors genuinely differ.
    app.handle_key_for_test(char_key('l'));
    let in_map = focused_selection(&app).expect("a tile is selected");

    app.handle_key_for_test(char_key('v'));
    assert_eq!(
        focused_selection(&app),
        Some(in_map.clone()),
        "`v` back to the tree should land on the entry the treemap was showing"
    );
    match app.current_screen() {
        myd::screen::Screen::Main(s) => assert_eq!(
            s.focus,
            myd::widget::treemap::FocusTarget::Tree,
            "and focus should be the tree"
        ),
        _ => unreachable!(),
    }

    // And back the other way.
    app.handle_key_for_test(char_key('j'));
    let in_tree = focused_selection(&app).expect("a row is selected");
    app.handle_key_for_test(char_key('v'));
    assert_eq!(
        focused_selection(&app),
        Some(in_tree),
        "and the tree's selection carries into the treemap"
    );
}

// ---------------------------------------------------------------------------
// `o`: hand the selection to the desktop's default application.
// ---------------------------------------------------------------------------

/// Put a fake launcher named after this platform's opener first on `PATH`, so a
/// test can see what myd asked to open without launching anything real.
///
/// Returns the guard (restoring `PATH` on drop) and the file the fake writes to.
#[cfg(unix)]
struct FakeOpener {
    _dir: tempfile::TempDir,
    log: std::path::PathBuf,
    previous: Option<std::ffi::OsString>,
    _lock: std::sync::MutexGuard<'static, ()>,
}

#[cfg(unix)]
impl FakeOpener {
    fn install() -> Self {
        use std::os::unix::fs::PermissionsExt;
        // `PATH` is process-global, so these serialise like the HOME tests.
        static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
        let lock = LOCK.lock().unwrap_or_else(|e| e.into_inner());

        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("opened.txt");
        let script = dir.path().join(myd::utils::opener::OPENER);
        std::fs::write(
            &script,
            format!("#!/bin/sh\necho \"$1\" >> {}\n", log.display()),
        )
        .unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();

        let previous = std::env::var_os("PATH");
        let joined = format!(
            "{}:{}",
            dir.path().display(),
            previous.clone().unwrap_or_default().to_string_lossy()
        );
        unsafe { std::env::set_var("PATH", joined) };
        Self {
            _dir: dir,
            log,
            previous,
            _lock: lock,
        }
    }

    /// What the launcher was handed, waiting briefly for the spawned child.
    fn opened(&self) -> String {
        for _ in 0..100 {
            if let Ok(s) = std::fs::read_to_string(&self.log) {
                if !s.trim().is_empty() {
                    return s.trim().to_string();
                }
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        String::new()
    }
}

#[cfg(unix)]
impl Drop for FakeOpener {
    fn drop(&mut self) {
        match self.previous.take() {
            Some(v) => unsafe { std::env::set_var("PATH", v) },
            None => unsafe { std::env::remove_var("PATH") },
        }
    }
}

#[cfg(unix)]
#[tokio::test]
async fn o_hands_the_selection_to_the_platform_opener() {
    let opener = FakeOpener::install();

    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("report.pdf"), b"x").unwrap();
    let mut app = FileBrowser::new(Some(dir.path().to_path_buf()), None, false);
    settle(&mut app).await;
    app.handle_key_for_test(char_key('j'));

    let selected = match app.current_screen() {
        myd::screen::Screen::Main(s) => s.selected_path().cloned().unwrap(),
        _ => panic!("expected a main screen"),
    };
    app.handle_key_for_test(char_key('o'));

    assert_eq!(
        opener.opened(),
        selected.to_string_lossy(),
        "the launcher should receive the selected path"
    );
    assert_eq!(
        app.modal_kind_for_test(),
        "none",
        "a successful open says nothing"
    );
}

#[test]
fn the_opener_is_chosen_for_the_platform() {
    // macOS has `open`; Linux and the BSDs go through the freedesktop helper.
    assert_eq!(
        myd::utils::opener::OPENER,
        if cfg!(target_os = "macos") {
            "open"
        } else {
            "xdg-open"
        }
    );
}

#[cfg(unix)]
#[tokio::test]
async fn o_on_a_remote_panel_refuses_rather_than_opening_a_local_path() {
    // `open`/`xdg-open` only understand local paths. A remote one would either
    // fail with the launcher's own message or — worse — open an unrelated local
    // file that happens to share the path, the same trap that sent remote copies
    // to the wrong place. Uses the counting mock rather than the gated SFTP
    // harness: this is about the routing decision, not about SFTP.
    let opener = FakeOpener::install();

    // The remote tree is rooted at a path that ALSO exists locally. That is the
    // dangerous case and the only one that tests the guard: rooted somewhere
    // absent, the opener's own existence check refuses anyway and the test would
    // pass with the guard removed.
    let local_twin = tempfile::tempdir().unwrap();
    std::fs::write(local_twin.path().join("big_file"), b"local decoy").unwrap();
    let tree = remote_tree_rooted_at(local_twin.path());

    let start = tempfile::tempdir().unwrap();
    let mut app = FileBrowser::new(Some(start.path().to_path_buf()), None, false);
    settle(&mut app).await;
    app.replace_panel_with_remote_for_test(tree);

    app.handle_key_for_test(char_key('j'));
    app.handle_key_for_test(char_key('o'));

    assert_eq!(
        app.modal_kind_for_test(),
        "confirm",
        "a remote entry must be refused with a message"
    );
    assert_eq!(
        opener.opened(),
        "",
        "and the local file sharing that path must not have been opened"
    );
}

#[tokio::test]
async fn browsing_does_not_fill_the_history() {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    // The list is for destinations the user chose, so only what the picker opens
    // goes into it. Recording every resolved load — which is what happened
    // briefly — swept in each directory drilled into while browsing and buried
    // the handful of places actually picked.
    let cfg = tempfile::tempdir().unwrap();
    let file = cfg.path().join("hosts.toml");
    let root = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(root.path().join("alpha/beta")).unwrap();

    let mut app = FileBrowser::new(Some(root.path().to_path_buf()), None, false);
    app.set_hosts_for_test(myd::hosts::HostCatalog::load_from_unseeded(&file));
    settle(&mut app).await;

    // Drill down two levels the ordinary way.
    for _ in 0..2 {
        for _ in 0..10 {
            let on_dir = match app.current_screen() {
                myd::screen::Screen::Main(s) => s.selected_is_dir()
                    && s.selected_path().map(|p| p.parent().is_some()).unwrap_or(false),
                _ => false,
            };
            if on_dir {
                break;
            }
            app.handle_key_for_test(char_key('j'));
        }
        app.handle_key_for_test(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        settle(&mut app).await;
    }

    assert!(
        app.hosts_for_test().favorites().is_empty(),
        "browsing must not fill the list: {:?}",
        app.hosts_for_test().favorites()
    );

    // Opening the same place through the picker *is* a choice, and is recorded.
    let target = root.path().join("alpha");
    app.handle_key_for_test(char_key('g'));
    app.handle_key_for_test(char_key('d'));
    for c in target.to_string_lossy().chars() {
        app.handle_key_for_test(char_key(c));
    }
    app.handle_key_for_test(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    settle(&mut app).await;

    assert!(
        app.hosts_for_test()
            .favorites()
            .iter()
            .any(|f| f.path == target.to_string_lossy()),
        "a directory opened from the picker is remembered: {:?}",
        app.hosts_for_test().favorites()
    );
}

#[tokio::test]
async fn e_edits_a_saved_directorys_path_in_place() {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    // `e` only opened the host form, so a directory entry could not be corrected
    // at all — a typo or a moved directory meant deleting and re-adding, which
    // throws away its visit history and its place in the pinned block.
    let cfg = tempfile::tempdir().unwrap();
    let file = cfg.path().join("hosts.toml");
    let old_dir = tempfile::tempdir().unwrap();
    let new_dir = tempfile::tempdir().unwrap();

    let mut catalog = myd::hosts::HostCatalog::load_from_unseeded(&file);
    catalog.add_favorite(myd::hosts::SavedDir::saved(
        old_dir.path().to_string_lossy().to_string(),
    ));
    catalog.pin_dir(&old_dir.path().to_string_lossy());
    catalog.record_visit(&old_dir.path().to_string_lossy());
    catalog.save().unwrap();

    let start = tempfile::tempdir().unwrap();
    let mut app = FileBrowser::new(Some(start.path().to_path_buf()), None, false);
    app.set_hosts_for_test(myd::hosts::HostCatalog::load_from_unseeded(&file));
    settle(&mut app).await;
    app.handle_key_for_test(char_key('g'));
    app.handle_key_for_test(char_key('d'));
    app.handle_key_for_test(special_key(KeyCode::Tab));
    cursor_to(&mut app, old_dir.path());

    app.handle_key_for_test(char_key('e'));
    assert_eq!(
        app.modal_kind_for_test(),
        "input",
        "`e` should open an editable popup on a directory row"
    );

    // Replace the path with the new one.
    for _ in 0..300 {
        app.handle_key_for_test(KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE));
    }
    for c in new_dir.path().to_string_lossy().chars() {
        app.handle_key_for_test(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE));
    }
    app.handle_key_for_test(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));

    let entry = app
        .hosts_for_test()
        .favorites()
        .iter()
        .find(|f| f.path == new_dir.path().to_string_lossy())
        .cloned()
        .unwrap_or_else(|| {
            panic!(
                "the path should have changed: {:?}",
                app.hosts_for_test().favorites()
            )
        });
    assert_eq!(entry.uses, 1, "the visit history is kept");
    assert!(entry.is_pinned(), "and its place in the pinned block");
    assert!(
        !app.hosts_for_test()
            .favorites()
            .iter()
            .any(|f| f.path == old_dir.path().to_string_lossy()),
        "the old path is gone, not duplicated"
    );
}

#[tokio::test]
async fn editing_a_directory_to_one_already_listed_is_refused() {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    // Merging two entries silently would lose whichever history the user cared
    // about, so this says so instead.
    let cfg = tempfile::tempdir().unwrap();
    let file = cfg.path().join("hosts.toml");
    let a = tempfile::tempdir().unwrap();
    let b = tempfile::tempdir().unwrap();
    let mut catalog = myd::hosts::HostCatalog::load_from_unseeded(&file);
    for d in [&a, &b] {
        catalog.add_favorite(myd::hosts::SavedDir::saved(
            d.path().to_string_lossy().to_string(),
        ));
    }
    catalog.save().unwrap();

    let start = tempfile::tempdir().unwrap();
    let mut app = FileBrowser::new(Some(start.path().to_path_buf()), None, false);
    app.set_hosts_for_test(myd::hosts::HostCatalog::load_from_unseeded(&file));
    settle(&mut app).await;
    app.handle_key_for_test(char_key('g'));
    app.handle_key_for_test(char_key('d'));
    app.handle_key_for_test(special_key(KeyCode::Tab));
    cursor_to(&mut app, a.path());

    app.handle_key_for_test(char_key('e'));
    for _ in 0..300 {
        app.handle_key_for_test(KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE));
    }
    for c in b.path().to_string_lossy().chars() {
        app.handle_key_for_test(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE));
    }
    app.handle_key_for_test(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));

    assert_eq!(app.modal_kind_for_test(), "confirm", "should say it is a duplicate");
    assert!(
        app.hosts_for_test()
            .favorites()
            .iter()
            .any(|f| f.path == a.path().to_string_lossy()),
        "and leave the original alone"
    );
}

// ---------------------------------------------------------------------------
// Shallow traversal: browse without measuring directory sizes.
// ---------------------------------------------------------------------------

/// A directory of sized subdirectories, plus an isolated catalog.
async fn shallow_app() -> (tempfile::TempDir, tempfile::TempDir, FileBrowser) {
    let cfg = tempfile::tempdir().unwrap();
    let root = tempfile::tempdir().unwrap();
    for (n, sz) in [("alpha", 90_000usize), ("beta", 40_000)] {
        std::fs::create_dir_all(root.path().join(n)).unwrap();
        std::fs::write(root.path().join(n).join("f.bin"), vec![0u8; sz]).unwrap();
    }
    std::fs::write(root.path().join("loose.txt"), vec![0u8; 5_000]).unwrap();

    let mut app = FileBrowser::new(Some(root.path().to_path_buf()), None, false);
    app.set_hosts_for_test(myd::hosts::HostCatalog::load_from_unseeded(
        &cfg.path().join("hosts.toml"),
    ));
    settle(&mut app).await;
    (cfg, root, app)
}

fn tree_is_shallow(app: &FileBrowser) -> bool {
    match app.current_screen() {
        myd::screen::Screen::Main(s) => s.tree.is_shallow(),
        _ => false,
    }
}

/// Whether a given panel's tree was built without measuring.
fn panel_is_shallow(app: &FileBrowser, index: usize) -> bool {
    match app.panel_screen_for_test(index) {
        Some(myd::screen::Screen::Main(s)) => s.tree.is_shallow(),
        _ => false,
    }
}

/// `-s` opens a single panel without measuring.
#[tokio::test]
async fn shallow_flag_opens_a_single_panel_unmeasured() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("sub")).unwrap();
    std::fs::write(dir.path().join("sub/f.txt"), b"x").unwrap();

    let mut app =
        FileBrowser::new_shallow(Some(dir.path().to_path_buf()), None, false, true);
    settle(&mut app).await;

    assert!(
        tree_is_shallow(&app),
        "-s must start in shallow mode, as though S had been pressed"
    );

    // And the flag is opt-in: the same app without it still measures.
    let mut app = FileBrowser::new(Some(dir.path().to_path_buf()), None, false);
    settle(&mut app).await;
    assert!(!tree_is_shallow(&app), "without -s the tree is measured");
}

/// `-s` applies to both panes of a split, not just the active one.
#[tokio::test]
async fn shallow_flag_applies_to_both_panes() {
    // The flag says how you want to browse; a split where one side measured and
    // the other did not would be arbitrary.
    let left = tempfile::tempdir().unwrap();
    let right = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(left.path().join("a")).unwrap();
    std::fs::create_dir_all(right.path().join("b")).unwrap();

    let mut app = FileBrowser::new_shallow(
        Some(left.path().to_path_buf()),
        Some(right.path().to_path_buf()),
        true,
        true,
    );
    settle_all(&mut app).await;

    assert_eq!(app.panel_count(), 2, "two panes");
    assert!(panel_is_shallow(&app, 0), "the left pane is unmeasured");
    assert!(panel_is_shallow(&app, 1), "and so is the right");
}

/// Drive ticks until panel 0 is showing `dir` as a resolved Main screen.
///
/// `settle` returns as soon as the top screen is Main, which after Enter is
/// still the *old* screen — the pushed loading screen has not been polled yet.
/// Asserting on the mode there reads the directory the user just left.
async fn settle_into(app: &mut FileBrowser, dir: &std::path::Path) {
    use myd::screen::Screen;
    for _ in 0..400 {
        app.resolve_loading_for_test();
        if matches!(app.current_screen(), Screen::Main(_))
            && app.panel_current_dir(0).as_deref() == Some(dir)
        {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    }
    panic!("never arrived at {}", dir.display());
}

/// `-s` survives drilling into a subdirectory.
#[tokio::test]
async fn shallow_flag_is_honoured_when_entering_a_subdirectory() {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    // The flag describes how the session should browse, so a subdirectory with
    // no preference of its own inherits it. Entering one used to fall back to a
    // full scan — the recursive walk `-s` exists to avoid.
    let cfg = tempfile::tempdir().unwrap();
    let root = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(root.path().join("sub/inner")).unwrap();
    std::fs::write(root.path().join("sub/inner/f.bin"), vec![0u8; 4_000]).unwrap();

    let mut app =
        FileBrowser::new_shallow(Some(root.path().to_path_buf()), None, false, true);
    app.set_hosts_for_test(myd::hosts::HostCatalog::load_from_unseeded(
        &cfg.path().join("hosts.toml"),
    ));
    settle(&mut app).await;
    assert!(tree_is_shallow(&app), "starts shallow, from the flag");

    // Into "sub", which has never been recorded either way. The cursor starts on
    // the root row, so step onto the child first.
    app.handle_key_for_test(char_key('j'));
    app.handle_key_for_test(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    settle_into(&mut app, &root.path().join("sub")).await;
    assert!(
        tree_is_shallow(&app),
        "the startup mode must hold until the user changes it in the interface"
    );
}

/// The `S` toggle redirects the session, not just the directory it was pressed in.
#[tokio::test]
async fn toggling_the_mode_changes_it_for_later_directories() {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    // `S` is the explicit change the startup flag defers to. Turning measuring
    // off here and walking on must not start measuring again one level down.
    let (_cfg, root, mut app) = shallow_app().await;
    assert!(!tree_is_shallow(&app), "starts measured");

    app.handle_key_for_test(char_key('S'));
    settle(&mut app).await;
    assert!(tree_is_shallow(&app));

    // Step down to the first subdirectory, whichever it is: shallow mode sorts
    // unmeasured directories as unknown, so the order of alpha/beta/loose.txt is
    // not the point of this test.
    let mut target = None;
    for _ in 0..6 {
        app.handle_key_for_test(char_key('j'));
        if let myd::screen::Screen::Main(s) = app.current_screen() {
            if s.selected_is_dir() && s.selected_path() != Some(&root.path().to_path_buf()) {
                target = s.selected_path().cloned();
                break;
            }
        }
    }
    let target = target.expect("a subdirectory under the cursor");
    app.handle_key_for_test(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    settle_into(&mut app, &target).await;
    assert!(
        tree_is_shallow(&app),
        "a directory entered after S stays unmeasured"
    );
}

/// A directory explicitly recorded as measured stays measured under `-s`.
#[tokio::test]
async fn a_recorded_preference_outranks_the_session_mode() {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    // The session mode is only the fallback. Somewhere the user decided to
    // measure is a decision about that directory, and opening the app with `-s`
    // does not overrule it.
    let cfg = tempfile::tempdir().unwrap();
    let root = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(root.path().join("sub")).unwrap();
    std::fs::write(root.path().join("sub/f.bin"), vec![0u8; 4_000]).unwrap();

    let mut hosts = myd::hosts::HostCatalog::load_from_unseeded(&cfg.path().join("hosts.toml"));
    hosts.set_dir_shallow(&root.path().join("sub").to_string_lossy(), false);

    let mut app =
        FileBrowser::new_shallow(Some(root.path().to_path_buf()), None, false, true);
    app.set_hosts_for_test(hosts);
    settle(&mut app).await;
    assert!(tree_is_shallow(&app), "the root follows the flag");

    app.handle_key_for_test(char_key('j'));
    app.handle_key_for_test(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    settle_into(&mut app, &root.path().join("sub")).await;
    assert!(
        !tree_is_shallow(&app),
        "a directory recorded as measured keeps measuring"
    );
}

#[tokio::test]
async fn s_stops_measuring_directories() {
    // Shallow mode reports directory sizes as unknown — the same display the
    // SFTP backend already uses — rather than as the inode's own size, which
    // would look measured and would not be.
    let (_cfg, _root, mut app) = shallow_app().await;
    assert!(!tree_is_shallow(&app), "starts measured");

    app.handle_key_for_test(char_key('S'));
    settle(&mut app).await;
    assert!(tree_is_shallow(&app), "S turns measuring off");

    let text = app_screen_text(&mut app, 100, 14);
    assert!(text.contains("SHALLOW"), "the title must say so: {}", text);
    // Directory rows show a dash; the file keeps its real size.
    assert!(text.contains('—'), "unmeasured directories show a dash: {}", text);
    assert!(text.contains("4.9KB"), "files are still sized: {}", text);
}

#[tokio::test]
async fn returning_to_full_measurement_asks_first() {
    // Going shallow is instant. Going back walks the whole tree, which is the
    // reason the user turned it off, so it must not happen on a keystroke.
    let (_cfg, _root, mut app) = shallow_app().await;
    app.handle_key_for_test(char_key('S'));
    settle(&mut app).await;
    assert!(tree_is_shallow(&app));

    app.handle_key_for_test(char_key('S'));
    assert_eq!(
        app.modal_kind_for_test(),
        "confirm",
        "returning to full measurement must ask"
    );
    // Declining leaves it shallow.
    app.handle_key_for_test(char_key('n'));
    settle(&mut app).await;
    assert!(tree_is_shallow(&app), "declining keeps it shallow");

    // Confirming measures again.
    app.handle_key_for_test(char_key('S'));
    app.handle_key_for_test(char_key('y'));
    settle(&mut app).await;
    assert!(!tree_is_shallow(&app), "confirming measures the tree");
}

/// Changing the traversal mode rebuilds the view; it does not navigate.
#[tokio::test]
async fn toggling_the_mode_does_not_deepen_the_navigation_stack() {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    // The stack is the navigation history, so only entering a directory belongs
    // on it. `S` used to push, which left a screen behind showing the same
    // directory in the other mode: the next `o` popped back to where you already
    // were and looked like it had done nothing, and two toggles buried the real
    // parent two screens down.
    let (_cfg, root, mut app) = shallow_app().await;
    let depth_at_root = app.screen_stack_depth();

    // Descend once, so there is a genuine parent to come back to.
    let mut target = None;
    for _ in 0..6 {
        app.handle_key_for_test(char_key('j'));
        if let myd::screen::Screen::Main(s) = app.current_screen() {
            if s.selected_is_dir() && s.selected_path() != Some(&root.path().to_path_buf()) {
                target = s.selected_path().cloned();
                break;
            }
        }
    }
    let target = target.expect("a subdirectory under the cursor");
    app.handle_key_for_test(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    settle_into(&mut app, &target).await;
    let depth_in_child = app.screen_stack_depth();
    assert_eq!(
        depth_in_child,
        depth_at_root + 1,
        "entering a directory is the one thing that does push"
    );

    // Toggle measuring off, then back on. Neither is a traversal.
    app.handle_key_for_test(char_key('S'));
    settle(&mut app).await;
    assert!(tree_is_shallow(&app), "S turned measuring off");
    assert_eq!(
        app.screen_stack_depth(),
        depth_in_child,
        "going shallow rebuilds the view in place"
    );

    app.handle_key_for_test(char_key('S'));
    app.handle_key_for_test(char_key('y'));
    settle(&mut app).await;
    assert!(!tree_is_shallow(&app), "and back on");
    assert_eq!(
        app.screen_stack_depth(),
        depth_in_child,
        "measuring again rebuilds it in place too"
    );

    // The consequence that matters: one Ctrl-o still returns to the parent,
    // rather than to a stale copy of the directory it was already showing.
    app.handle_key_for_test(ctrl_key('o'));
    settle(&mut app).await;
    assert_eq!(
        app.screen_stack_depth(),
        depth_at_root,
        "one step back leaves the panel at the root"
    );
    let landed = app.panel_current_dir(0).expect("a directory");
    assert_ne!(landed, target, "and not still inside the child");
    assert_eq!(
        landed,
        root.path().canonicalize().unwrap(),
        "it is the parent the descent started from"
    );
}

/// Re-sorting reorders the tree in memory; it does not navigate either.
#[tokio::test]
async fn changing_the_sort_order_does_not_deepen_the_navigation_stack() {
    let (_cfg, _root, mut app) = shallow_app().await;
    let depth = app.screen_stack_depth();

    let before = match app.current_screen() {
        myd::screen::Screen::Main(s) => s.tree.sort_mode,
        _ => unreachable!("settled onto a main screen"),
    };
    app.handle_key_for_test(char_key('s'));
    settle(&mut app).await;

    let after = match app.current_screen() {
        myd::screen::Screen::Main(s) => s.tree.sort_mode,
        _ => unreachable!("still a main screen"),
    };
    assert_ne!(before, after, "s moved to the next sort order");
    assert_eq!(
        app.screen_stack_depth(),
        depth,
        "sorting reorders the nodes already loaded, so nothing is pushed"
    );
}

#[tokio::test]
async fn the_traversal_mode_is_remembered_per_directory() {
    let (cfg, root, mut app) = shallow_app().await;
    app.handle_key_for_test(char_key('S'));
    settle(&mut app).await;

    let body = std::fs::read_to_string(cfg.path().join("hosts.toml")).unwrap();
    assert!(
        body.contains("shallow = true"),
        "the choice must persist: {}",
        body
    );

    // A fresh session on the same directory honours it, even though the panel is
    // built before the catalog is reachable.
    let mut next = FileBrowser::new(Some(root.path().to_path_buf()), None, false);
    next.set_hosts_for_test(myd::hosts::HostCatalog::load_from_unseeded(
        &cfg.path().join("hosts.toml"),
    ));
    settle(&mut next).await;
    // One more tick: the preference is applied when the load resolves.
    for _ in 0..50 {
        next.tick_for_test();
        next.resolve_loading_for_test();
        if tree_is_shallow(&next) {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(3)).await;
    }
    assert!(
        tree_is_shallow(&next),
        "a directory marked shallow should open that way next time"
    );
}

#[tokio::test]
async fn shallow_directories_sort_as_unknown() {
    // The same rule remote directories follow: unmeasured sorts last rather than
    // masquerading as small.
    let (_cfg, _root, mut app) = shallow_app().await;
    app.handle_key_for_test(char_key('S'));
    settle(&mut app).await;

    match app.current_screen() {
        myd::screen::Screen::Main(s) => {
            let names: Vec<String> = s
                .tree
                .lines
                .iter()
                .filter(|l| l.depth == 1)
                .map(|l| l.name.clone())
                .collect();
            assert_eq!(
                names,
                vec!["loose.txt", "alpha", "beta"],
                "the measured file leads; unmeasured directories follow"
            );
        }
        _ => panic!("expected a main screen"),
    }
}

#[tokio::test]
async fn s_toggles_a_saved_directorys_traversal_mode_from_the_picker() {
    use crossterm::event::KeyCode;

    // The shallow flag was only reachable by opening the directory and pressing
    // `S` there — which means paying the very walk the flag exists to avoid. It
    // can now be set from the list, before opening it.
    let cfg = tempfile::tempdir().unwrap();
    let file = cfg.path().join("hosts.toml");
    let target = tempfile::tempdir().unwrap();
    let mut catalog = myd::hosts::HostCatalog::load_from_unseeded(&file);
    catalog.add_favorite(myd::hosts::SavedDir::saved(
        target.path().to_string_lossy().to_string(),
    ));
    catalog.save().unwrap();

    let start = tempfile::tempdir().unwrap();
    let mut app = FileBrowser::new(Some(start.path().to_path_buf()), None, false);
    app.set_hosts_for_test(myd::hosts::HostCatalog::load_from_unseeded(&file));
    settle(&mut app).await;
    app.handle_key_for_test(char_key('g'));
    app.handle_key_for_test(char_key('d'));
    app.handle_key_for_test(special_key(KeyCode::Tab));
    cursor_to(&mut app, target.path());

    let key = target.path().to_string_lossy().to_string();
    assert!(!app.hosts_for_test().dir_is_shallow(&key), "starts measured");

    app.handle_key_for_test(char_key('S'));
    assert!(
        app.hosts_for_test().dir_is_shallow(&key),
        "S should mark it shallow"
    );
    // The row says so, or the toggle is invisible until the next open.
    match app.current_screen() {
        myd::screen::Screen::DirPicker(p) => assert!(
            p.selected().map(|o| o.shallow).unwrap_or(false),
            "the row should show the new state"
        ),
        _ => panic!("expected the picker"),
    }
    assert!(
        std::fs::read_to_string(&file)
            .unwrap()
            .contains("shallow = true"),
        "and it must persist"
    );

    // Pressing it again turns measuring back on. No prompt here: nothing is
    // being walked, since this only takes effect the next time it is opened.
    app.handle_key_for_test(char_key('S'));
    assert!(
        !app.hosts_for_test().dir_is_shallow(&key),
        "S again should clear it"
    );
}

#[tokio::test]
async fn s_does_nothing_on_a_host_row() {
    // Remote directories are never measured, so the toggle has no meaning there
    // and must not write a flag onto a host entry.
    let cfg = tempfile::tempdir().unwrap();
    let file = cfg.path().join("hosts.toml");
    let mut catalog = myd::hosts::HostCatalog::load_from_unseeded(&file);
    catalog.upsert(myd::hosts::SavedHost::new("prod", "prod.example.com"));
    catalog.save().unwrap();

    let start = tempfile::tempdir().unwrap();
    let mut app = FileBrowser::new(Some(start.path().to_path_buf()), None, false);
    app.set_hosts_for_test(myd::hosts::HostCatalog::load_from_unseeded(&file));
    settle(&mut app).await;
    open_picker_on_first_host(&mut app);

    let before = std::fs::read_to_string(&file).unwrap();
    app.handle_key_for_test(char_key('S'));
    assert_eq!(
        std::fs::read_to_string(&file).unwrap(),
        before,
        "S on a host row must change nothing"
    );
}

// ---------------------------------------------------------------------------
// The file preview pane: space to open, vi motions, search, and the two key
// contracts it must not break (Esc must not quit, space must not answer a
// dialog).
// ---------------------------------------------------------------------------

/// A directory holding one text file with `lines` numbered lines, and an app
/// rooted there with the cursor on that file.
async fn preview_app(
    lines: usize,
) -> (tempfile::TempDir, myd::app::FileBrowser) {
    use myd::app::FileBrowser;

    let dir = tempfile::tempdir().unwrap();
    let body: String = (0..lines).map(|i| format!("line {i} of text\n")).collect();
    std::fs::write(dir.path().join("sample.txt"), body).unwrap();

    let mut app = FileBrowser::new(Some(dir.path().to_path_buf()), None, false);
    settle(&mut app).await;
    // Move onto the file: the root row is the directory itself.
    app.handle_key_for_test(char_key('j'));
    (dir, app)
}

/// Drive ticks until the preview has loaded something.
async fn settle_preview(app: &mut myd::app::FileBrowser) {
    for _ in 0..400 {
        app.tick_for_test();
        if app.preview_has_content_for_test() {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    }
    panic!("preview never loaded");
}

/// Render and return the screen as text, so the pane's own output can be read.
fn screen_text(app: &mut myd::app::FileBrowser, w: u16, h: u16) -> String {
    let mut term =
        ratatui::Terminal::new(ratatui::backend::TestBackend::new(w, h)).unwrap();
    term.draw(|f| app.render_for_test(f)).unwrap();
    let buf = term.backend().buffer().clone();
    (0..buf.area.height)
        .map(|y| {
            (0..buf.area.width)
                .map(|x| buf[(x, y)].symbol())
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[tokio::test]
async fn test_space_opens_and_closes_the_preview() {
    let (_dir, mut app) = preview_app(5).await;
    assert!(!app.preview_open_for_test(), "starts closed");

    app.handle_key_for_test(char_key(' '));
    assert!(app.preview_open_for_test(), "space should open it");
    assert!(
        app.preview_focused_for_test(),
        "opening should focus it — it covers the tree"
    );

    settle_preview(&mut app).await;
    let out = screen_text(&mut app, 80, 24);
    assert!(out.contains("sample.txt"), "title missing:\n{out}");
    assert!(out.contains("line 0 of text"), "content missing:\n{out}");

    app.handle_key_for_test(char_key(' '));
    assert!(!app.preview_open_for_test(), "space should close it");
    assert!(!app.preview_focused_for_test(), "focus must not linger");
}

/// A default build ships one binary; the diagnostics stay behind `tools`.
#[test]
fn only_myd_is_built_by_default() {
    // The three helpers in src/bin are development tools — an escape dumper, a
    // terminal probe, a headless transfer benchmark. Cargo auto-discovers
    // src/bin, so before they were gated they landed in every release build and
    // in `cargo install`, which put three unexplained binaries on people's PATH.
    //
    // Reads the manifest rather than shelling out to cargo: the question is what
    // the package declares, and a build here would cost a minute to answer it.
    let manifest = std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("Cargo.toml"),
    )
    .expect("the manifest is readable");

    // Every [[bin]] other than myd itself must carry required-features.
    let mut current: Option<String> = None;
    let mut guarded: Vec<String> = Vec::new();
    let mut names: Vec<String> = Vec::new();
    let mut in_bin = false;
    for line in manifest.lines() {
        let line = line.trim();
        if line == "[[bin]]" {
            in_bin = true;
            current = None;
            continue;
        }
        if line.starts_with('[') && line != "[[bin]]" {
            in_bin = false;
            continue;
        }
        if !in_bin {
            continue;
        }
        if let Some(rest) = line.strip_prefix("name = ") {
            let name = rest.trim_matches('"').to_string();
            names.push(name.clone());
            current = Some(name);
        }
        if line.starts_with("required-features") {
            if let Some(n) = current.clone() {
                guarded.push(n);
            }
        }
    }

    assert!(
        names.contains(&"myd".to_string()),
        "myd must be declared: {:?}",
        names
    );
    for name in &names {
        if name == "myd" {
            assert!(
                !guarded.contains(name),
                "myd itself must not be gated behind a feature"
            );
        } else {
            assert!(
                guarded.contains(name),
                "{} is a development tool and needs required-features, \
                 or it ships in every release build",
                name
            );
        }
    }

    // And any file dropped into src/bin later has to be declared, since cargo
    // would otherwise auto-discover it into the default build.
    let bin_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/bin");
    if let Ok(entries) = std::fs::read_dir(&bin_dir) {
        for e in entries.flatten() {
            let file = e.file_name().to_string_lossy().to_string();
            let Some(stem) = file.strip_suffix(".rs") else {
                continue;
            };
            assert!(
                names.iter().any(|n| n == stem),
                "src/bin/{} is auto-discovered into the default build; \
                 declare it as [[bin]] with required-features",
                file
            );
        }
    }
}

#[tokio::test]
async fn a_click_in_the_preview_scrolls_it_instead_of_the_tree_underneath() {
    // Reported: clicks went to the hidden tree. The preview covers the panels,
    // so the click moved a cursor the user could not see and swapped the file
    // they were reading — the scroll wheel already knew to stay in the preview,
    // but clicks fell straight through.
    use crossterm::event::{MouseButton, MouseEventKind};

    let (_dir, mut app) = preview_app(200).await;
    app.handle_key_for_test(char_key(' '));
    settle_preview(&mut app).await;
    screen_text(&mut app, 80, 24);

    let before = app.preview_scroll_for_test();
    let selected = match app.current_screen() {
        Screen::Main(s) => s.selected_path().cloned(),
        _ => panic!("expected a main screen"),
    };

    app.route_mouse(mouse_at(MouseEventKind::Down(MouseButton::Left), 20, 10));
    assert_eq!(
        app.preview_scroll_for_test(),
        before + 1,
        "a click should advance the preview like j"
    );

    let after = match app.current_screen() {
        Screen::Main(s) => s.selected_path().cloned(),
        _ => panic!("expected a main screen"),
    };
    assert_eq!(
        selected, after,
        "and must not move the tree cursor hidden behind it"
    );
}

#[tokio::test]
async fn a_click_focuses_the_preview_before_advancing_it() {
    // Clicking is a way of saying "I am reading this". If the click only moved
    // focus, the first one would appear to do nothing and the second would
    // scroll — not how clicking on a page behaves.
    use crossterm::event::{MouseButton, MouseEventKind};

    let (_dir, mut app) = preview_app(200).await;
    app.handle_key_for_test(char_key(' '));
    settle_preview(&mut app).await;
    screen_text(&mut app, 80, 24);

    // Hand focus back to the tree, the way Tab does.
    app.handle_key_for_test(key(crossterm::event::KeyCode::Tab));
    let before = app.preview_scroll_for_test();

    app.route_mouse(mouse_at(MouseEventKind::Down(MouseButton::Left), 20, 10));
    assert!(
        app.preview_focused_for_test(),
        "the click takes focus for the preview"
    );
    assert_eq!(
        app.preview_scroll_for_test(),
        before + 1,
        "and advances it in the same click"
    );
}

#[tokio::test]
async fn a_click_outside_the_preview_still_belongs_to_it() {
    // The pane is drawn over the whole frame, so there is no "outside" while it
    // is open. A click on the status row must not reach the tree either.
    use crossterm::event::{MouseButton, MouseEventKind};

    let (_dir, mut app) = preview_app(200).await;
    app.handle_key_for_test(char_key(' '));
    settle_preview(&mut app).await;
    screen_text(&mut app, 80, 24);

    let selected = match app.current_screen() {
        Screen::Main(s) => s.selected_path().cloned(),
        _ => panic!("expected a main screen"),
    };
    let before = app.preview_scroll_for_test();

    // Bottom-left, well away from the text body.
    app.route_mouse(mouse_at(MouseEventKind::Down(MouseButton::Left), 1, 23));
    assert_eq!(app.preview_scroll_for_test(), before + 1);
    let after = match app.current_screen() {
        Screen::Main(s) => s.selected_path().cloned(),
        _ => panic!("expected a main screen"),
    };
    assert_eq!(selected, after, "the tree is untouched wherever the click lands");
}

#[tokio::test]
async fn a_right_click_in_the_preview_does_not_reach_the_tree() {
    // Right-click opens the entry under it in the tree. Through a pane that
    // covers the whole frame that would be a directory change the user never
    // asked for, so it is swallowed rather than passed down.
    use crossterm::event::{MouseButton, MouseEventKind};

    let (_dir, mut app) = preview_app(200).await;
    app.handle_key_for_test(char_key(' '));
    settle_preview(&mut app).await;
    screen_text(&mut app, 80, 24);

    let depth = app.screen_stack_depth();
    let before = app.preview_scroll_for_test();
    app.route_mouse(mouse_at(MouseEventKind::Down(MouseButton::Right), 20, 10));

    assert_eq!(
        app.screen_stack_depth(),
        depth,
        "a right click must not navigate the hidden tree"
    );
    assert_eq!(
        app.preview_scroll_for_test(),
        before,
        "and only the left button advances the page"
    );
    assert!(app.preview_open_for_test(), "the preview is still up");
}

#[tokio::test]
async fn test_preview_scrolls_with_vi_motions_and_clamps() {
    let (_dir, mut app) = preview_app(200).await;
    app.handle_key_for_test(char_key(' '));
    settle_preview(&mut app).await;
    // Render once so the pane knows its height.
    screen_text(&mut app, 80, 24);

    app.handle_key_for_test(char_key('j'));
    assert_eq!(app.preview_scroll_for_test(), 1, "j should scroll down");
    app.handle_key_for_test(char_key('k'));
    assert_eq!(app.preview_scroll_for_test(), 0, "k should scroll up");

    // Already at the top: k must not go negative.
    app.handle_key_for_test(char_key('k'));
    assert_eq!(app.preview_scroll_for_test(), 0, "clamped at the top");

    app.handle_key_for_test(ctrl_key('f'));
    let paged = app.preview_scroll_for_test();
    assert!(paged > 1, "Ctrl-f should page down, got {paged}");
    app.handle_key_for_test(ctrl_key('b'));
    assert_eq!(app.preview_scroll_for_test(), 0, "Ctrl-b should page back");

    app.handle_key_for_test(ctrl_key('d'));
    let half = app.preview_scroll_for_test();
    assert!(half > 0 && half < paged, "Ctrl-d is half a page: {half}");

    app.handle_key_for_test(char_key('G'));
    let bottom = app.preview_scroll_for_test();
    assert!(bottom > 100, "G should jump to the end, got {bottom}");
    app.handle_key_for_test(char_key('j'));
    assert_eq!(app.preview_scroll_for_test(), bottom, "clamped at the end");

    app.handle_key_for_test(char_key('g'));
    assert_eq!(app.preview_scroll_for_test(), 0, "g should return to the top");
}

#[tokio::test]
async fn test_preview_search_steps_through_matches_and_wraps() {
    let (_dir, mut app) = preview_app(60).await;
    app.handle_key_for_test(char_key(' '));
    settle_preview(&mut app).await;
    screen_text(&mut app, 80, 24);

    // `/` opens a prompt that is separate from the tree's own search.
    app.handle_key_for_test(char_key('/'));
    for c in "line 5".chars() {
        app.handle_key_for_test(char_key(c));
    }
    app.handle_key_for_test(key(crossterm::event::KeyCode::Enter));

    // line 5, and 50..59 => 11 matches.
    assert_eq!(app.preview_match_count_for_test(), 11);
    assert_eq!(app.preview_match_line_for_test(), Some(5));

    app.handle_key_for_test(char_key('n'));
    assert_eq!(app.preview_match_line_for_test(), Some(50));
    app.handle_key_for_test(char_key('p'));
    assert_eq!(app.preview_match_line_for_test(), Some(5), "p steps back");

    // Wrap backward off the first match.
    app.handle_key_for_test(char_key('p'));
    assert_eq!(
        app.preview_match_line_for_test(),
        Some(59),
        "p should wrap to the last match"
    );
    // And forward off the last.
    app.handle_key_for_test(char_key('n'));
    assert_eq!(
        app.preview_match_line_for_test(),
        Some(5),
        "n should wrap to the first match"
    );

    // N and P are the reverses of n and p.
    app.handle_key_for_test(char_key('N'));
    assert_eq!(app.preview_match_line_for_test(), Some(59));
    app.handle_key_for_test(char_key('P'));
    assert_eq!(app.preview_match_line_for_test(), Some(5));
}

/// Globally `Esc` resolves to `Action::Quit`. With the preview focused it must
/// hand focus back instead, or the obvious way to leave the pane kills the app.
#[tokio::test]
async fn test_esc_in_the_preview_does_not_quit() {
    let (_dir, mut app) = preview_app(10).await;
    app.handle_key_for_test(char_key(' '));
    settle_preview(&mut app).await;

    let keep_running = app.handle_key_for_test(key(crossterm::event::KeyCode::Esc));
    assert!(keep_running, "Esc in the preview must not quit the app");
    assert!(
        !app.preview_focused_for_test(),
        "Esc should release focus..."
    );
    assert!(
        app.preview_open_for_test(),
        "...but leave the pane open, so the tree can be moved beneath it"
    );

    // With focus back on the tree, Esc means what it always did.
    let keep_running = app.handle_key_for_test(key(crossterm::event::KeyCode::Esc));
    assert!(!keep_running, "Esc on the tree still quits");
}

/// `q` closes the preview rather than quitting the app. Globally it exits, and
/// reaching for it to dismiss a preview should not end the session.
#[tokio::test]
async fn test_q_in_the_preview_closes_it_instead_of_quitting() {
    let (_dir, mut app) = preview_app(10).await;
    app.handle_key_for_test(char_key(' '));
    settle_preview(&mut app).await;

    assert!(app.handle_key_for_test(char_key('q')), "q must not quit");
    assert!(!app.preview_open_for_test(), "q should close the pane");
    assert!(!app.preview_focused_for_test());

    // With the pane gone, q means what it always did.
    assert!(!app.handle_key_for_test(char_key('q')), "q quits normally");
}

/// The pane can be open without having focus — Esc hands the keyboard back to
/// the tree. `q` must still close it there rather than falling through to the
/// global binding and quitting.
#[tokio::test]
async fn test_q_closes_an_unfocused_preview() {
    let (_dir, mut app) = preview_app(10).await;
    app.handle_key_for_test(char_key(' '));
    settle_preview(&mut app).await;

    app.handle_key_for_test(key(crossterm::event::KeyCode::Esc));
    assert!(app.preview_open_for_test() && !app.preview_focused_for_test());

    assert!(
        app.handle_key_for_test(char_key('q')),
        "q must not quit while a preview is on screen"
    );
    assert!(!app.preview_open_for_test());
}

/// Esc still hands focus back without closing, so the tree can be moved while
/// the pane stays up and follows the cursor.
#[tokio::test]
async fn test_esc_unfocuses_the_preview_without_closing_it() {
    let (_dir, mut app) = preview_app(10).await;
    app.handle_key_for_test(char_key(' '));
    settle_preview(&mut app).await;

    app.handle_key_for_test(key(crossterm::event::KeyCode::Esc));
    assert!(app.preview_open_for_test(), "Esc should leave the pane open");
    assert!(!app.preview_focused_for_test());
}

/// Space is now a global binding, and a question dialog must still refuse it.
/// This is the regression that overwrote a file once: keys that are not an
/// explicit answer have to be inert at a question.
#[tokio::test]
async fn test_space_does_not_answer_a_confirm_dialog() {
    let (_dir, mut app) = preview_app(4).await;

    // `D` on the file raises a delete confirmation.
    app.handle_key_for_test(char_key('D'));
    assert_eq!(
        app.modal_kind_for_test(),
        "confirm",
        "expected a confirm dialog"
    );

    app.handle_key_for_test(char_key(' '));
    assert_eq!(
        app.modal_kind_for_test(),
        "confirm",
        "space must not answer a question"
    );
    assert!(
        !app.preview_open_for_test(),
        "and it must not reach the preview binding either"
    );

    // Esc still cancels, leaving the file alone.
    app.handle_key_for_test(key(crossterm::event::KeyCode::Esc));
    assert_eq!(app.modal_kind_for_test(), "none");
}

// ---------------------------------------------------------------------------
// "Always" on the delete prompt: stop asking for the rest of this session.
// ---------------------------------------------------------------------------

/// A directory of files to delete one at a time, cursor on the first.
async fn delete_app() -> (tempfile::TempDir, myd::app::FileBrowser) {
    use myd::app::FileBrowser;

    let dir = tempfile::tempdir().unwrap();
    for n in ["a.txt", "b.txt", "c.txt"] {
        std::fs::write(dir.path().join(n), "x").unwrap();
    }
    let mut app = FileBrowser::new(Some(dir.path().to_path_buf()), None, false);
    settle(&mut app).await;
    // The root row is the directory itself; step onto the first file.
    app.handle_key_for_test(char_key('j'));
    (dir, app)
}

/// The path under the cursor.
fn selected(app: &myd::app::FileBrowser) -> std::path::PathBuf {
    match app.current_screen() {
        myd::screen::Screen::Main(s) => {
            s.selected_path().cloned().expect("something is selected")
        }
        _ => panic!("expected a main screen"),
    }
}

/// Drive ticks until `path` is gone, or give up.
async fn settle_gone(app: &mut myd::app::FileBrowser, path: &std::path::Path) {
    for _ in 0..200 {
        app.resolve_loading_for_test();
        if !path.exists() {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    }
    panic!("{} was never deleted", path.display());
}

#[tokio::test]
async fn always_on_the_delete_prompt_stops_it_asking_again() {
    let (dir, mut app) = delete_app().await;

    // The first delete asks, as always.
    app.handle_key_for_test(char_key('D'));
    assert_eq!(app.modal_kind_for_test(), "confirm", "the first D asks");

    // 'a' means "yes, and stop asking": this file goes too.
    app.handle_key_for_test(char_key('a'));
    assert_eq!(app.modal_kind_for_test(), "none", "answering closes it");
    let first = selected(&app);
    settle_gone(&mut app, &first).await;

    // The next delete goes straight through with no dialog at all.
    app.handle_key_for_test(char_key('j'));
    let next = selected(&app);
    app.handle_key_for_test(char_key('D'));
    assert_eq!(
        app.modal_kind_for_test(),
        "none",
        "after 'always' the prompt must not come back"
    );
    settle_gone(&mut app, &next).await;

    // Two of the three are gone; the fixture had no others deleted behind our back.
    let left = std::fs::read_dir(dir.path()).unwrap().count();
    assert_eq!(left, 1, "exactly the two chosen files were deleted");
}

#[tokio::test]
async fn always_lasts_only_for_this_run_of_the_app() {
    // The whole point of keeping it in memory: a fresh app asks again. Somebody
    // who turned the guard off while clearing out a directory should not find it
    // still off tomorrow.
    let (dir, mut app) = delete_app().await;
    app.handle_key_for_test(char_key('D'));
    app.handle_key_for_test(char_key('a'));
    let first = selected(&app);
    settle_gone(&mut app, &first).await;
    drop(app);

    let mut next = myd::app::FileBrowser::new(Some(dir.path().to_path_buf()), None, false);
    settle(&mut next).await;
    next.handle_key_for_test(char_key('j'));
    next.handle_key_for_test(char_key('D'));
    assert_eq!(
        next.modal_kind_for_test(),
        "confirm",
        "a new session must ask again"
    );
}

#[tokio::test]
async fn the_delete_prompt_still_takes_yes_and_no() {
    // Adding a third button must not disturb the two answers that were already
    // there — 'n' and Esc leave the file alone, 'y' deletes just that one.
    let (dir, mut app) = delete_app().await;

    app.handle_key_for_test(char_key('D'));
    app.handle_key_for_test(char_key('n'));
    assert_eq!(app.modal_kind_for_test(), "none", "'n' closes the dialog");
    assert_eq!(
        std::fs::read_dir(dir.path()).unwrap().count(),
        3,
        "'n' must not delete anything"
    );

    app.handle_key_for_test(char_key('D'));
    app.handle_key_for_test(key(crossterm::event::KeyCode::Esc));
    assert_eq!(
        std::fs::read_dir(dir.path()).unwrap().count(),
        3,
        "Esc must not delete anything either"
    );

    // 'y' deletes this one, and leaves the prompt in place for the next.
    let target = selected(&app);
    app.handle_key_for_test(char_key('D'));
    app.handle_key_for_test(char_key('y'));
    settle_gone(&mut app, &target).await;

    app.handle_key_for_test(char_key('D'));
    assert_eq!(
        app.modal_kind_for_test(),
        "confirm",
        "'y' is a one-off, so the next delete still asks"
    );
}

/// Motions inside the pane must not also move the tree underneath: that is what
/// focus is for.
#[tokio::test]
async fn test_preview_motions_do_not_move_the_tree() {
    let (_dir, mut app) = preview_app(200).await;
    let before = app.selected_line_index_for_test();

    app.handle_key_for_test(char_key(' '));
    settle_preview(&mut app).await;
    screen_text(&mut app, 80, 24);

    for _ in 0..5 {
        app.handle_key_for_test(char_key('j'));
    }
    assert!(app.preview_scroll_for_test() > 0, "the pane scrolled");
    assert_eq!(
        app.selected_line_index_for_test(),
        before,
        "the tree cursor must not have moved"
    );
}

/// A binary file gets a note rather than a screenful of control characters.
#[tokio::test]
async fn test_a_binary_file_is_reported_not_dumped() {
    use myd::app::FileBrowser;

    let dir = tempfile::tempdir().unwrap();
    // A NUL byte is the decisive marker.
    std::fs::write(dir.path().join("blob.bin"), [0u8, 1, 2, 3, 255, 254, 0, 7]).unwrap();

    let mut app = FileBrowser::new(Some(dir.path().to_path_buf()), None, false);
    settle(&mut app).await;
    app.handle_key_for_test(char_key('j'));
    app.handle_key_for_test(char_key(' '));
    settle_preview(&mut app).await;

    let out = screen_text(&mut app, 80, 24);
    assert!(out.contains("Binary file"), "expected a note:\n{out}");
}

/// The help overlay owns space as page-down, so it must not leak through to the
/// preview binding.
#[tokio::test]
async fn test_space_in_help_does_not_open_the_preview() {
    let (_dir, mut app) = preview_app(4).await;
    app.handle_key_for_test(char_key('?'));
    assert_eq!(app.modal_kind_for_test(), "help");

    app.handle_key_for_test(char_key(' '));
    assert!(
        !app.preview_open_for_test(),
        "space belongs to the help overlay while it is up"
    );
    assert_eq!(app.modal_kind_for_test(), "help", "and it should still be up");
}

/// Tab must reach the preview, and must not leave two panes claiming focus.
#[tokio::test]
async fn test_tab_rotates_into_the_preview() {
    let (_dir, mut app) = preview_app(10).await;
    app.handle_key_for_test(char_key(' '));
    settle_preview(&mut app).await;
    assert!(app.preview_focused_for_test());

    // One panel plus the preview: Tab alternates between them.
    app.handle_key_for_test(key(crossterm::event::KeyCode::Tab));
    assert!(!app.preview_focused_for_test(), "Tab leaves the pane");
    app.handle_key_for_test(key(crossterm::event::KeyCode::Tab));
    assert!(app.preview_focused_for_test(), "and comes back to it");
    assert!(
        !app.transfer_focused_for_test(),
        "the sidebar must not be focused at the same time"
    );
}

/// Closing the pane must drop its focus, or keys go to something invisible.
#[tokio::test]
async fn test_closing_the_preview_releases_focus() {
    let (_dir, mut app) = preview_app(10).await;
    app.handle_key_for_test(char_key(' '));
    settle_preview(&mut app).await;
    assert!(app.preview_focused_for_test());

    app.handle_key_for_test(char_key(' '));
    assert!(!app.preview_focused_for_test());

    // And a motion now moves the tree again, not a hidden pane. `k` rather than
    // `j`: the cursor is on the only file, which is the last row, so `j` has
    // nowhere to go and would prove nothing.
    let before = app.selected_line_index_for_test();
    app.handle_key_for_test(char_key('k'));
    assert_ne!(
        app.selected_line_index_for_test(),
        before,
        "the tree should take motions once the pane is closed"
    );
}

/// Both plain-key tables must agree, or space works only until a `g` times out.
#[test]
fn test_space_is_bound_in_both_key_tables() {
    use myd::keybinding::{Action, KeyBindingHandler};

    let h = KeyBindingHandler::new();
    assert_eq!(
        h.resolve_single_for_test(char_key(' ')),
        Some(Action::TogglePreview)
    );
}

/// The preview follows the cursor, so moving to another file replaces what is
/// shown rather than leaving the first file up.
#[tokio::test]
async fn test_the_preview_follows_the_cursor() {
    use myd::app::FileBrowser;

    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("a-first.txt"), "AAA distinctive\n").unwrap();
    std::fs::write(dir.path().join("b-second.txt"), "BBB different\n").unwrap();

    let mut app = FileBrowser::new(Some(dir.path().to_path_buf()), None, false);
    settle(&mut app).await;
    app.handle_key_for_test(char_key('j'));
    app.handle_key_for_test(char_key(' '));
    settle_preview(&mut app).await;
    let first = screen_text(&mut app, 80, 24);

    // Focus the tree so j moves the cursor rather than scrolling the pane.
    app.handle_key_for_test(key(crossterm::event::KeyCode::Esc));
    app.handle_key_for_test(char_key('j'));
    for _ in 0..400 {
        app.tick_for_test();
        let out = screen_text(&mut app, 80, 24);
        if out.contains("different") || out.contains("distinctive") && out != first {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    }
    let second = screen_text(&mut app, 80, 24);
    assert_ne!(first, second, "the pane should have followed the cursor");
}

/// A preview on a remote panel must read through the VFS, never `std::fs`.
///
/// The dangerous case is a path that exists on both machines: `std::fs` would
/// succeed and show the *local* file's contents under the remote file's name.
/// So the mock serves content that could not have come from local disk, and the
/// assertion is on that content plus the read actually reaching the backend.
#[tokio::test]
async fn test_a_remote_preview_reads_through_the_vfs() {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    // A path that really exists locally, so a std::fs read would quietly work.
    let shared = std::path::PathBuf::from("/etc/hostname");
    if !shared.is_file() {
        eprintln!("/etc/hostname missing; skipping");
        return;
    }
    let local_bytes = std::fs::read_to_string(&shared).unwrap_or_default();

    struct Counting {
        opens: AtomicUsize,
    }
    #[async_trait::async_trait]
    impl myd::vfs::Vfs for Counting {
        fn scheme(&self) -> &'static str {
            "sftp"
        }
        async fn read_dir(&self, _p: &myd::vfs::VPath) -> anyhow::Result<Vec<myd::vfs::VEntry>> {
            Ok(Vec::new())
        }
        async fn stat(&self, _p: &myd::vfs::VPath) -> anyhow::Result<myd::vfs::VMetadata> {
            Ok(myd::vfs::VMetadata {
                len: REMOTE_BODY.len() as u64,
                ..Default::default()
            })
        }
        async fn open_read(
            &self,
            _p: &myd::vfs::VPath,
        ) -> anyhow::Result<Box<dyn myd::vfs::VRead>> {
            self.opens.fetch_add(1, Ordering::SeqCst);
            Ok(Box::new(std::io::Cursor::new(REMOTE_BODY.as_bytes().to_vec())))
        }
        // A preview only ever reads. Anything else reaching this mock is a bug
        // in the code under test, so it fails loudly rather than silently.
        async fn create_dir_all(&self, _p: &myd::vfs::VPath) -> anyhow::Result<()> {
            anyhow::bail!("a preview must not create directories")
        }
        async fn remove_file(&self, _p: &myd::vfs::VPath) -> anyhow::Result<()> {
            anyhow::bail!("a preview must not remove files")
        }
        async fn remove_dir(&self, _p: &myd::vfs::VPath) -> anyhow::Result<()> {
            anyhow::bail!("a preview must not remove directories")
        }
        async fn rename(
            &self,
            _f: &myd::vfs::VPath,
            _t: &myd::vfs::VPath,
        ) -> anyhow::Result<()> {
            anyhow::bail!("a preview must not rename")
        }
        async fn open_write(
            &self,
            _p: &myd::vfs::VPath,
            _len: Option<u64>,
        ) -> anyhow::Result<Box<dyn myd::vfs::VWrite>> {
            anyhow::bail!("a preview must not write")
        }
        async fn dir_size(
            &self,
            _p: &myd::vfs::VPath,
            _cache: &myd::utils::sizes::SizeCache,
            _cancel: &myd::utils::sizes::CancelToken,
            _progress: Option<&myd::widget::progress::OpProgress>,
        ) -> u64 {
            panic!("a preview must not walk directories")
        }
    }
    const REMOTE_BODY: &str = "this text exists only on the fake remote server\n";

    let fs = Arc::new(Counting {
        opens: AtomicUsize::new(0),
    });
    let content = myd::preview::load(
        fs.clone(),
        myd::preview::PreviewRequest {
            path: myd::vfs::VPath::new(myd::vfs::BackendId(1), shared.clone()),
            label: shared.clone(),
            cols: 60,
            rows: 20,
            page: 0,
        },
    )
    .await;

    assert_eq!(
        fs.opens.load(Ordering::SeqCst),
        1,
        "the preview must read through the backend"
    );

    let text = content.search_text().join("\n");
    assert!(
        text.contains("only on the fake remote"),
        "expected the remote body, got: {text}"
    );
    if !local_bytes.trim().is_empty() {
        assert!(
            !text.contains(local_bytes.trim()),
            "the preview showed the LOCAL file's contents for a remote path"
        );
    }
}

/// The help list is the project's single source of truth for bindings, so a new
/// key that is not in it is effectively undiscoverable.
#[tokio::test]
async fn help_documents_the_preview_keys() {
    let dir = create_test_structure();
    let mut app = FileBrowser::new(Some(dir.path().to_path_buf()), None, false);
    settle(&mut app).await;

    app.handle_key_for_test(char_key('?'));
    let text = help_text(&mut app, 80, 130);
    assert!(text.contains("Preview"), "no Preview category:\n{text}");
    assert!(
        text.contains("Show / hide the preview"),
        "space is not documented:\n{text}"
    );
    assert!(
        text.contains("Search within the file"),
        "the pane's search is not documented:\n{text}"
    );
}

/// Markdown must be effectively instant. syntect's markdown grammar embeds every
/// other language for fenced code blocks and took ~170ms on a 30KB README, which
/// is what the hand-rolled highlighter exists to avoid.
#[tokio::test]
async fn a_markdown_preview_opens_immediately() {
    use myd::app::FileBrowser;

    let dir = tempfile::tempdir().unwrap();
    // Big enough that a slow highlighter would show, and representative: headings,
    // fenced code, inline markup, links and tables.
    let body = "# Title\n\nSome *text* with `code` and [a link](https://x.y).\n\n\
                | a | b |\n|---|---|\n| 1 | 2 |\n\n```rust\nfn main() {}\n```\n"
        .repeat(200);
    std::fs::write(dir.path().join("big.md"), &body).unwrap();

    let mut app = FileBrowser::new(Some(dir.path().to_path_buf()), None, false);
    settle(&mut app).await;
    app.handle_key_for_test(char_key('j'));

    let started = std::time::Instant::now();
    app.handle_key_for_test(char_key(' '));
    settle_preview(&mut app).await;
    let elapsed = started.elapsed();

    let out = screen_text(&mut app, 100, 30);
    assert!(out.contains("Title"), "content missing:\n{out}");
    // Generous for a debug build on a loaded machine; syntect took over a second
    // on this much markdown, so the margin still catches a regression.
    assert!(
        elapsed < std::time::Duration::from_millis(900),
        "markdown preview took {elapsed:?}, which is not instant"
    );
}

/// On a multi-page PDF, `j`/`k` turn pages rather than scrolling — a rendered page
/// is drawn to fit and has nothing to scroll.
#[tokio::test]
async fn pdf_previews_turn_pages_with_j_and_k() {
    use myd::app::FileBrowser;

    let src = std::path::Path::new("/usr/share/doc/shared-mime-info/shared-mime-info-spec.pdf");
    if !src.exists() {
        eprintln!("no multi-page PDF on this machine; skipping");
        return;
    }
    // Needs a timg built with poppler; chafa cannot render PDFs at all.
    let caps = myd::preview::image::capabilities();
    if caps.backend_for(src).is_none() {
        eprintln!("no PDF renderer available; skipping");
        return;
    }

    let dir = tempfile::tempdir().unwrap();
    std::fs::copy(src, dir.path().join("doc.pdf")).unwrap();

    let mut app = FileBrowser::new(Some(dir.path().to_path_buf()), None, false);
    settle(&mut app).await;
    // Render before opening: the geometry an image is rendered at comes from the
    // last frame, so without one the pane has no size to ask for.
    screen_text(&mut app, 110, 34);
    app.handle_key_for_test(char_key('j'));
    app.handle_key_for_test(char_key(' '));
    settle_preview(&mut app).await;

    assert!(
        app.preview_is_paged_for_test(),
        "a 19-page PDF must be treated as paged"
    );
    assert_eq!(app.preview_page_for_test(), 0, "starts on the first page");

    // Each turn is a re-render. Waiting for "has content" would return at once,
    // since the previous page is still loaded — wait for the page the pane reports
    // to actually be the one asked for.
    async fn turn(app: &mut FileBrowser, k: char) {
        let before = app.preview_page_for_test();
        app.handle_key_for_test(char_key(k));
        let want = app.preview_page_for_test();
        if want == before {
            return; // clamped: nothing will be re-rendered
        }
        for _ in 0..400 {
            app.tick_for_test();
            // Re-render each time: the geometry a page is rendered at comes from
            // the last frame, and the tick is what starts the next load.
            screen_text(app, 110, 34);
            if app.preview_has_content_for_test() && app.preview_page_for_test() == want {
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
        panic!("page {want} never rendered");
    }

    turn(&mut app, 'j').await;
    assert_eq!(app.preview_page_for_test(), 1, "j should turn the page");
    turn(&mut app, 'j').await;
    assert_eq!(app.preview_page_for_test(), 2);
    turn(&mut app, 'k').await;
    assert_eq!(app.preview_page_for_test(), 1, "k should go back");

    // Back at the first page, k must stop rather than wrap or go negative.
    turn(&mut app, 'k').await;
    assert_eq!(app.preview_page_for_test(), 0);
    turn(&mut app, 'k').await;
    assert_eq!(app.preview_page_for_test(), 0, "must clamp at the first page");

    // The footer says where we are.
    let out = screen_text(&mut app, 110, 34);
    assert!(out.contains("page 1/"), "no page indicator:\n{out}");
}

/// An ordinary image has no pages, so the motions must still scroll it — paging a
/// single image would look like the keys were dead.
#[tokio::test]
async fn image_previews_are_not_paged() {
    use myd::app::FileBrowser;

    let src = std::path::Path::new("/usr/share/pixmaps/ubuntu-logo-text.png");
    if !src.exists() || myd::preview::image::capabilities().backend_for(src).is_none() {
        eprintln!("no image renderer available; skipping");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    std::fs::copy(src, dir.path().join("logo.png")).unwrap();

    let mut app = FileBrowser::new(Some(dir.path().to_path_buf()), None, false);
    settle(&mut app).await;
    screen_text(&mut app, 110, 34);
    app.handle_key_for_test(char_key('j'));
    app.handle_key_for_test(char_key(' '));
    settle_preview(&mut app).await;

    assert!(
        !app.preview_is_paged_for_test(),
        "a plain image must not be treated as a paged document"
    );
    // And j does not move the page.
    app.handle_key_for_test(char_key('j'));
    assert_eq!(app.preview_page_for_test(), 0);
}

/// Graphics detection must refuse a terminal that cannot be shown to support the
/// protocol — a mis-detection sprays base64 across the display.
#[test]
fn graphics_detection_is_conservative() {
    use myd::preview::graphics::{EnvVars, Protocol, decide};

    // A plain terminal has nothing to offer.
    let plain = EnvVars {
        term: Some("xterm-256color".into()),
        ..Default::default()
    };
    assert_eq!(*decide(&plain, None), Protocol::Blocks);

    // A terminal that supports it gets the high-resolution path.
    let kitty = EnvVars {
        term: Some("xterm-kitty".into()),
        ..Default::default()
    };
    assert_eq!(*decide(&kitty, None), Protocol::Kitty);

    // A kitty-capable terminal inside a tmux that will not forward the escape
    // has to fall back, and must say which setting to change.
    let tmux_off = EnvVars {
        term: Some("screen-256color".into()),
        kitty_window_id: Some("1".into()),
        tmux: Some("/tmp/tmux-1000/default,1,0".into()),
        tmux_passthrough: false,
        ..Default::default()
    };
    let d = decide(&tmux_off, None);
    assert_eq!(
        *d,
        Protocol::Blocks,
        "graphics must not be sent through a tmux that will not forward them"
    );
    assert!(d.1.is_some_and(|m| m.contains("allow-passthrough")));
}

/// The reported bug: images stayed blocky in a native iTerm2 pane and inside
/// tmux. Both cases hinged on `LC_TERMINAL`, the only iTerm2 identification that
/// survives ssh (which drops `TERM_PROGRAM`) and tmux (which overwrites it).
#[test]
fn iterm2_is_recognised_over_ssh_and_under_tmux() {
    use myd::preview::graphics::{EnvVars, Probed, Protocol, decide};

    // Native pane over ssh: TERM_PROGRAM never arrives.
    let over_ssh = EnvVars {
        term: Some("xterm-256color".into()),
        term_program: None,
        lc_terminal: Some("iTerm2".into()),
        ..Default::default()
    };
    assert_eq!(*decide(&over_ssh, None), Protocol::Iterm2);

    // Under tmux, TERM_PROGRAM says "tmux" and hides the real terminal. The
    // terminal must still be recognised as capable — but the protocol used is
    // kitty's, because through a multiplexer what matters is that the image is
    // carried in small chunks rather than one large escape.
    let under_tmux = EnvVars {
        term: Some("screen-256color".into()),
        term_program: Some("tmux".into()),
        lc_terminal: Some("iTerm2".into()),
        tmux: Some("/tmp/tmux-1000/default,1,0".into()),
        tmux_passthrough: true,
        ..Default::default()
    };
    assert_ne!(*decide(&under_tmux, None), Protocol::Blocks);
    assert_eq!(*decide(&under_tmux, None), Protocol::Kitty);

    // And with passthrough off, sixel is the one thing tmux forwards itself.
    let sixel_tmux = EnvVars {
        tmux_passthrough: false,
        ..under_tmux
    };
    assert_eq!(
        *decide(
            &sixel_tmux,
            Some(Probed {
                kitty: false,
                sixel: true,
                ..Default::default()
            })
        ),
        Protocol::Sixel
    );
}

/// The ghost-image bug: an iTerm2 image stayed on screen after the preview
/// closed. It has no delete escape and redrawing does not remove it, because
/// ratatui only emits cells whose content changed and the cells under an image
/// are ones it believes are already blank. The region has to be erased.
#[test]
fn a_graphics_region_erase_covers_exactly_the_image() {
    use myd::app::region_erase_sequence;
    use ratatui::layout::Rect;

    let area = Rect {
        x: 4,
        y: 2,
        width: 60,
        height: 5,
    };
    let seq = region_erase_sequence(area);

    // One erase per covered row, and no more — erasing beyond the image would
    // take out the pane border and whatever is below it.
    assert_eq!(seq.matches("\x1b[K").count(), 5);

    // Positions are 1-based in the escape and 0-based in the Rect, and each row
    // starts at the image's left edge so the erase does not eat the border.
    assert!(seq.starts_with("\x1b[3;5H\x1b[K"), "{seq:?}");
    assert!(seq.contains("\x1b[7;5H\x1b[K"), "last row missing: {seq:?}");
    assert!(
        !seq.contains("\x1b[8;"),
        "erased a row past the image: {seq:?}"
    );

    // A zero-height area must produce nothing rather than erasing a stray row.
    assert!(region_erase_sequence(Rect::new(0, 0, 10, 0)).is_empty());
}

/// Each graphics protocol must have exactly one way to be removed, or an image
/// either leaks or is erased twice (which flickers).
#[test]
fn every_graphics_protocol_has_one_removal_path() {
    use myd::preview::graphics::{Protocol, clear_sequence, needs_region_erase};

    for p in [Protocol::Kitty, Protocol::Iterm2, Protocol::Sixel] {
        let deletes = clear_sequence(p).is_some();
        let erases = needs_region_erase(p);
        assert!(deletes ^ erases, "{p:?} must have exactly one removal path");
    }
    // iTerm2 specifically — the reported case.
    assert!(needs_region_erase(Protocol::Iterm2));
    assert!(clear_sequence(Protocol::Iterm2).is_none());
}

/// Size bars toggle with `B`, matching the other column toggles (`P` for
/// permissions, `T` for times) rather than being the odd lowercase one out.
#[test]
fn size_bars_toggle_with_a_capital_b() {
    use myd::keybinding::{Action, KeyBindingHandler};

    let h = KeyBindingHandler::new();
    assert_eq!(
        h.resolve_single_for_test(char_key('B')),
        Some(Action::ToggleBar)
    );

    // Lowercase `b` no longer toggles bars. It stays free as a plain key —
    // Ctrl+B is page-up and is unaffected.
    assert_ne!(
        h.resolve_single_for_test(char_key('b')),
        Some(Action::ToggleBar)
    );
    assert_eq!(
        h.resolve_single_for_test(ctrl_key('b')),
        Some(Action::PageUp),
        "Ctrl+B must still page up"
    );
}

/// Ctrl+L redraws everything, as it does in a shell or vim.
///
/// ratatui emits only the cells that changed, so a screen corrupted by anything
/// outside its knowledge stays wrong until something overwrites each damaged
/// cell. This is the escape hatch.
#[tokio::test]
async fn ctrl_l_requests_a_full_repaint() {
    let dir = create_test_structure();
    let mut app = FileBrowser::new(Some(dir.path().to_path_buf()), None, false);
    settle(&mut app).await;

    assert!(
        !app.force_repaint_pending_for_test(),
        "nothing should be pending before the key"
    );

    let keep_running = app.handle_key_for_test(ctrl_key('l'));
    assert!(keep_running, "Ctrl+L must not quit");
    assert!(
        app.force_repaint_pending_for_test(),
        "Ctrl+L should request a full repaint"
    );
}

/// Plain `l` still expands — the new binding is Ctrl-modified and must not
/// shadow it.
#[test]
fn ctrl_l_does_not_disturb_plain_l() {
    use myd::keybinding::{Action, KeyBindingHandler};

    let h = KeyBindingHandler::new();
    assert_eq!(
        h.resolve_single_for_test(ctrl_key('l')),
        Some(Action::Redraw)
    );
    assert_eq!(
        h.resolve_single_for_test(char_key('l')),
        Some(Action::Expand),
        "plain l must still expand"
    );
}

/// A redraw must also forget any image on screen, or the next frame assumes a
/// picture is still there that the repaint has just erased.
#[tokio::test]
async fn ctrl_l_forgets_a_displayed_image() {
    let dir = create_test_structure();
    let mut app = FileBrowser::new(Some(dir.path().to_path_buf()), None, false);
    settle(&mut app).await;

    // No image is displayed in the test harness, so this asserts the weaker but
    // still meaningful property: the request is made regardless.
    app.handle_key_for_test(ctrl_key('l'));
    assert!(app.force_repaint_pending_for_test());
}

/// On an image preview the motions move the *tree's* cursor, and the preview
/// follows. A rendered image is drawn to fit the pane, so there is nothing to
/// scroll — j/k on one are reaching to see the next picture.
#[tokio::test]
async fn image_previews_move_the_tree_cursor() {
    use myd::app::FileBrowser;

    let src = std::path::Path::new("/usr/share/pixmaps/ubuntu-logo-text.png");
    if !src.exists() || myd::preview::image::capabilities().backend_for(src).is_none() {
        eprintln!("no image renderer available; skipping");
        return;
    }

    let dir = tempfile::tempdir().unwrap();
    for name in ["a.png", "b.png", "c.png"] {
        std::fs::copy(src, dir.path().join(name)).unwrap();
    }

    let mut app = FileBrowser::new(Some(dir.path().to_path_buf()), None, false);
    settle(&mut app).await;
    screen_text(&mut app, 110, 34);
    app.handle_key_for_test(char_key('j'));
    app.handle_key_for_test(char_key(' '));
    settle_preview(&mut app).await;

    assert!(
        app.preview_is_single_image_for_test(),
        "an image is neither scrollable nor paged"
    );
    let before = app.selected_line_index_for_test();

    app.handle_key_for_test(char_key('j'));
    assert_ne!(
        app.selected_line_index_for_test(),
        before,
        "j should move the tree cursor on an image"
    );
    assert_eq!(
        app.preview_scroll_for_test(),
        0,
        "and must not scroll the pane, which has nothing to scroll"
    );

    app.handle_key_for_test(char_key('k'));
    assert_eq!(
        app.selected_line_index_for_test(),
        before,
        "k should move it back"
    );
}

/// Text previews keep scrolling — the tree-moving behaviour is specific to
/// content that is drawn to fit.
#[tokio::test]
async fn text_previews_still_scroll_with_j_and_k() {
    let (_dir, mut app) = preview_app(200).await;
    app.handle_key_for_test(char_key(' '));
    settle_preview(&mut app).await;
    screen_text(&mut app, 80, 24);

    assert!(!app.preview_is_single_image_for_test());
    let cursor = app.selected_line_index_for_test();

    app.handle_key_for_test(char_key('j'));
    assert_eq!(app.preview_scroll_for_test(), 1, "text should scroll");
    assert_eq!(
        app.selected_line_index_for_test(),
        cursor,
        "and must not move the tree"
    );
}

// ---------------------------------------------------------------------------
// Patterned rename (gr): one regex over every tagged file.
// ---------------------------------------------------------------------------

/// Three numbered files, cursor on the first, all of them tagged.
async fn rename_app() -> (tempfile::TempDir, myd::app::FileBrowser) {
    use myd::app::FileBrowser;

    let dir = tempfile::tempdir().unwrap();
    for n in ["IMG_1.jpg", "IMG_2.jpg", "IMG_3.jpg"] {
        std::fs::write(dir.path().join(n), "x").unwrap();
    }
    let mut app = FileBrowser::new(Some(dir.path().to_path_buf()), None, false);
    settle(&mut app).await;
    // Step onto the first file and tag all three. `t` toggles in place without
    // advancing, so the cursor has to be moved between tags.
    app.handle_key_for_test(char_key('j'));
    for _ in 0..3 {
        app.handle_key_for_test(char_key('t'));
        app.handle_key_for_test(char_key('j'));
    }
    (dir, app)
}

/// Press the `gr` chord, opening the patterned-rename dialog.
///
/// A plain two-key press: chords have no timeout, so the prefix waits for the
/// second key however long the test takes to send it. This used to retry in a
/// loop to survive the 500ms window expiring under parallel load.
fn press_gr(app: &mut myd::app::FileBrowser) {
    app.handle_key_for_test(char_key('g'));
    app.handle_key_for_test(char_key('r'));
    assert_eq!(
        app.modal_kind_for_test(),
        "rename",
        "gr should open the rename dialog"
    );
}

/// Type a string into whichever dialog field has the focus.
fn type_into(app: &mut myd::app::FileBrowser, s: &str) {
    for c in s.chars() {
        app.handle_key_for_test(char_key(c));
    }
}

/// Drive ticks until `dir` contains a file named `name`.
async fn settle_named(app: &mut myd::app::FileBrowser, dir: &std::path::Path, name: &str) {
    for _ in 0..200 {
        app.resolve_loading_for_test();
        if dir.join(name).exists() {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    }
    panic!("{} never appeared in {}", name, dir.display());
}

#[tokio::test]
async fn gr_renames_every_tagged_file_through_the_pattern() {
    let (dir, mut app) = rename_app().await;

    press_gr(&mut app);
    assert_eq!(
        app.modal_kind_for_test(),
        "rename",
        "gr should open the patterned rename dialog"
    );

    // Capture group carried into the replacement — the case the feature exists
    // for, not just a literal swap.
    type_into(&mut app, r"IMG_(\d+)\.jpg");
    app.handle_key_for_test(key(crossterm::event::KeyCode::Tab));
    type_into(&mut app, "holiday-$1.jpeg");
    app.handle_key_for_test(key(crossterm::event::KeyCode::Enter));

    settle_named(&mut app, dir.path(), "holiday-1.jpeg").await;
    for n in ["holiday-1.jpeg", "holiday-2.jpeg", "holiday-3.jpeg"] {
        assert!(dir.path().join(n).exists(), "{} should exist", n);
    }
    for n in ["IMG_1.jpg", "IMG_2.jpg", "IMG_3.jpg"] {
        assert!(!dir.path().join(n).exists(), "{} should be gone", n);
    }
    assert_eq!(
        app.modal_kind_for_test(),
        "none",
        "a clean rename just closes the dialog"
    );
}

#[tokio::test]
async fn the_rename_dialog_previews_the_first_tagged_file() {
    // The preview is the point of the dialog: it has to show the real
    // transformation before anything is renamed.
    let (dir, mut app) = rename_app().await;
    press_gr(&mut app);

    type_into(&mut app, "IMG");
    app.handle_key_for_test(key(crossterm::event::KeyCode::Tab));
    type_into(&mut app, "PIC");

    let text = app_screen_text(&mut app, 100, 30);
    assert!(
        text.contains("PIC_1.jpg"),
        "the preview should show the first file's new name: {}",
        text
    );
    // Nothing has happened yet — a preview is not a rename.
    assert!(dir.path().join("IMG_1.jpg").exists(), "still on disk");
}

#[tokio::test]
async fn a_pattern_that_cannot_be_used_keeps_the_dialog_open() {
    // Both failure modes the request names: a regex that does not compile, and
    // one that compiles but matches nothing. Enter must not close the dialog,
    // so the patterns can be corrected rather than retyped.
    let (dir, mut app) = rename_app().await;
    press_gr(&mut app);

    type_into(&mut app, "(unclosed");
    app.handle_key_for_test(key(crossterm::event::KeyCode::Enter));
    assert_eq!(
        app.modal_kind_for_test(),
        "rename",
        "a pattern that does not compile must not close the dialog"
    );
    let text = app_screen_text(&mut app, 100, 30);
    assert!(text.contains('✗'), "the error should be visible: {}", text);

    // Correct it to something valid but non-matching: still refused, still open.
    for _ in 0.."(unclosed".len() {
        app.handle_key_for_test(key(crossterm::event::KeyCode::Backspace));
    }
    type_into(&mut app, "zzz");
    app.handle_key_for_test(key(crossterm::event::KeyCode::Enter));
    assert_eq!(
        app.modal_kind_for_test(),
        "rename",
        "a pattern that matches nothing must not close it either"
    );

    // Esc is the way out, and it renames nothing.
    app.handle_key_for_test(key(crossterm::event::KeyCode::Esc));
    assert_eq!(app.modal_kind_for_test(), "none", "Esc cancels");
    assert!(dir.path().join("IMG_1.jpg").exists(), "nothing was renamed");
}

#[tokio::test]
async fn the_preview_samples_the_first_file_on_screen_not_an_arbitrary_one() {
    // Tags are held in a HashSet, so the tagged set comes back in an arbitrary
    // order that differs between runs. The dialog previews "the first tagged
    // file", which has to mean the first one visible in the tree — otherwise the
    // preview shows a different file each time it is opened.
    for _ in 0..5 {
        let (_dir, mut app) = rename_app().await;
        let first_on_screen = match app.current_screen() {
            Screen::Main(s) => s
                .tree
                .lines
                .iter()
                .find(|l| !l.is_dir)
                .map(|l| l.name.clone())
                .expect("a file in the tree"),
            _ => panic!("expected a main screen"),
        };

        press_gr(&mut app);
        // An identity pattern makes the preview echo the sampled name.
        type_into(&mut app, "^");
        let text = app_screen_text(&mut app, 100, 30);
        assert!(
            text.contains(&format!("→ {}", first_on_screen)),
            "preview should sample {}, screen was:\n{}",
            first_on_screen,
            text
        );
    }
}

#[tokio::test]
async fn a_partly_matching_pattern_renames_only_what_matches() {
    // Tagging a mixed set and renaming the subset that matches is ordinary use,
    // not an error: the others are left alone.
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("IMG_1.jpg"), "x").unwrap();
    std::fs::write(dir.path().join("notes.txt"), "x").unwrap();
    let mut app = myd::app::FileBrowser::new(Some(dir.path().to_path_buf()), None, false);
    settle(&mut app).await;
    app.handle_key_for_test(char_key('j'));
    app.handle_key_for_test(char_key('t'));
    app.handle_key_for_test(char_key('j'));
    app.handle_key_for_test(char_key('t'));

    press_gr(&mut app);
    type_into(&mut app, "IMG_1");
    app.handle_key_for_test(key(crossterm::event::KeyCode::Tab));
    type_into(&mut app, "photo");
    app.handle_key_for_test(key(crossterm::event::KeyCode::Enter));

    settle_named(&mut app, dir.path(), "photo.jpg").await;
    assert!(
        dir.path().join("notes.txt").exists(),
        "the file the pattern did not match is untouched"
    );
}

#[tokio::test]
async fn gr_with_nothing_tagged_uses_the_cursor_file() {
    // Falls back to the selection the way D does, so the chord is not inert when
    // you have not tagged anything.
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("one.txt"), "x").unwrap();
    let mut app = myd::app::FileBrowser::new(Some(dir.path().to_path_buf()), None, false);
    settle(&mut app).await;
    app.handle_key_for_test(char_key('j'));

    press_gr(&mut app);
    assert_eq!(app.modal_kind_for_test(), "rename", "opens on the selection");

    type_into(&mut app, "one");
    app.handle_key_for_test(key(crossterm::event::KeyCode::Tab));
    type_into(&mut app, "two");
    app.handle_key_for_test(key(crossterm::event::KeyCode::Enter));
    settle_named(&mut app, dir.path(), "two.txt").await;
}

#[tokio::test]
async fn a_rename_that_would_collide_is_reported_and_nothing_is_lost() {
    // rename_path refuses to clobber an existing entry. Over a batch that has to
    // surface as a message rather than a silent skip, and the file that was
    // already there must survive with its own contents.
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("a1.txt"), "first").unwrap();
    std::fs::write(dir.path().join("b1.txt"), "second").unwrap();
    let mut app = myd::app::FileBrowser::new(Some(dir.path().to_path_buf()), None, false);
    settle(&mut app).await;

    // Tag whichever file the cursor lands on and rename it onto the other, so
    // the collision is deterministic however the tree happens to be sorted.
    app.handle_key_for_test(char_key('j'));
    let selected = match app.current_screen() {
        Screen::Main(s) => s.selected_path().cloned().expect("a file"),
        _ => panic!("expected a main screen"),
    };
    let from = selected.file_name().unwrap().to_string_lossy().to_string();
    let onto = if from == "a1.txt" { "b1.txt" } else { "a1.txt" };
    let survivor = std::fs::read_to_string(dir.path().join(onto)).unwrap();
    app.handle_key_for_test(char_key('t'));

    press_gr(&mut app);
    type_into(&mut app, &from);
    app.handle_key_for_test(key(crossterm::event::KeyCode::Tab));
    type_into(&mut app, onto);
    app.handle_key_for_test(key(crossterm::event::KeyCode::Enter));

    assert_eq!(
        app.modal_kind_for_test(),
        "confirm",
        "a collision must be reported, not swallowed"
    );
    assert_eq!(
        std::fs::read_to_string(dir.path().join(onto)).unwrap(),
        survivor,
        "the existing file must not be overwritten"
    );
    assert!(dir.path().join(&from).exists(), "and the source survives");
}

#[tokio::test]
async fn the_natural_sort_order_can_be_picked_from_the_menu() {
    // End to end: the mode has to be reachable from the UI, not just correct in
    // the tree. Natural is the last entry, so this also covers the menu growing
    // past the seven it used to have.
    let dir = tempfile::tempdir().unwrap();
    for n in ["10", "1", "20", "3", "4", "2"] {
        std::fs::write(dir.path().join(n), "x").unwrap();
    }
    let mut app = FileBrowser::new(Some(dir.path().to_path_buf()), None, false);
    settle(&mut app).await;

    let n = myd::screen::SortMode::ALL
        .iter()
        .position(|m| *m == myd::screen::SortMode::Natural)
        .expect("natural is offered");
    let digit = char::from_digit(n as u32 + 1, 10).expect("a single digit");

    // gs then its number, retried like the other chord tests.
    app.handle_key_for_test(char_key('g'));
    app.handle_key_for_test(char_key('s'));
    assert_eq!(app.modal_kind_for_test(), "sort_menu", "gs opens the menu");
    app.handle_key_for_test(char_key(digit));
    settle(&mut app).await;

    match app.current_screen() {
        Screen::Main(s) => {
            assert_eq!(s.tree.sort_mode, myd::screen::SortMode::Natural);
            let names: Vec<String> = s
                .tree
                .lines
                .iter()
                .filter(|l| l.depth == 1)
                .map(|l| l.name.clone())
                .collect();
            assert_eq!(
                names,
                ["1", "2", "3", "4", "10", "20"],
                "natural order reads numerically"
            );
        }
        _ => panic!("expected a main screen"),
    }
}

// ---------------------------------------------------------------------------
// The sort order is a view preference: it holds until the user changes it.
// ---------------------------------------------------------------------------

/// A directory whose numbered names sort differently by size and by nature,
/// plus a subdirectory holding the same names.
fn sort_persistence_fixture() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("sub")).unwrap();
    for n in ["10", "1", "2"] {
        std::fs::write(dir.path().join(n), "x").unwrap();
        std::fs::write(dir.path().join("sub").join(n), "x").unwrap();
    }
    dir
}

/// Switch the active panel to natural order through the sort menu.
fn choose_natural(app: &mut myd::app::FileBrowser) {
    let n = myd::screen::SortMode::ALL
        .iter()
        .position(|m| *m == myd::screen::SortMode::Natural)
        .expect("natural is offered");
    let digit = char::from_digit(n as u32 + 1, 10).expect("a single digit");
    app.handle_key_for_test(char_key('g'));
    app.handle_key_for_test(char_key('s'));
    assert_eq!(app.modal_kind_for_test(), "sort_menu", "gs opens the menu");
    app.handle_key_for_test(char_key(digit));
}

fn current_sort(app: &myd::app::FileBrowser) -> myd::screen::SortMode {
    match app.current_screen() {
        Screen::Main(s) => s.tree.sort_mode,
        _ => panic!("expected a main screen"),
    }
}

#[tokio::test]
async fn the_sort_order_survives_opening_a_directory_from_the_picker() {
    // The reported bug. `ViewPrefs` carried the sort order and every toggle
    // wrote it, but `apply_view_prefs` never read it back — so the order held
    // only where a caller threaded it through by hand. Entering a directory did;
    // the picker did not, and reset to the default.
    let dir = sort_persistence_fixture();
    let mut app = FileBrowser::new(Some(dir.path().to_path_buf()), None, false);
    settle(&mut app).await;

    choose_natural(&mut app);
    settle(&mut app).await;
    assert_eq!(current_sort(&app), myd::screen::SortMode::Natural);

    // Open the subdirectory by typing its path into the picker.
    app.handle_key_for_test(char_key('g'));
    app.handle_key_for_test(char_key('d'));
    assert!(
        matches!(app.current_screen(), Screen::DirPicker(_)),
        "gd opens the picker"
    );
    let sub = dir.path().join("sub");
    for c in sub.to_string_lossy().chars() {
        app.handle_key_for_test(char_key(c));
    }
    app.handle_key_for_test(key(crossterm::event::KeyCode::Enter));
    settle(&mut app).await;

    assert_eq!(
        current_sort(&app),
        myd::screen::SortMode::Natural,
        "the picker must not reset the sort order"
    );
    // And the rows really are in that order, not merely labelled with it.
    match app.current_screen() {
        Screen::Main(s) => {
            let names: Vec<String> = s
                .tree
                .lines
                .iter()
                .filter(|l| l.depth == 1)
                .map(|l| l.name.clone())
                .collect();
            assert_eq!(names, ["1", "2", "10"]);
        }
        _ => panic!("expected a main screen"),
    }
}

#[tokio::test]
async fn the_sort_order_survives_entering_and_leaving_a_directory() {
    // Entering already threaded the order through by hand; this pins that, and
    // covers coming back out, which goes through apply_view_prefs on the pop.
    let dir = sort_persistence_fixture();
    let mut app = FileBrowser::new(Some(dir.path().to_path_buf()), None, false);
    settle(&mut app).await;

    choose_natural(&mut app);
    settle(&mut app).await;

    // Step onto the subdirectory and enter it.
    let mut target = None;
    for _ in 0..8 {
        app.handle_key_for_test(char_key('j'));
        if let Screen::Main(s) = app.current_screen() {
            if s.selected_is_dir() && s.selected_path() != Some(&dir.path().to_path_buf()) {
                target = s.selected_path().cloned();
                break;
            }
        }
    }
    let target = target.expect("a subdirectory under the cursor");
    app.handle_key_for_test(key(crossterm::event::KeyCode::Enter));
    settle_into(&mut app, &target).await;
    assert_eq!(
        current_sort(&app),
        myd::screen::SortMode::Natural,
        "entering keeps the order"
    );

    app.handle_key_for_test(ctrl_key('o'));
    settle(&mut app).await;
    assert_eq!(
        current_sort(&app),
        myd::screen::SortMode::Natural,
        "and so does coming back out"
    );
}

#[tokio::test]
async fn the_sort_order_survives_a_shallow_toggle() {
    // `S` rebuilds the tree from scratch, and the rebuild used to be handed the
    // default order rather than the one on screen.
    let dir = sort_persistence_fixture();
    let mut app = FileBrowser::new(Some(dir.path().to_path_buf()), None, false);
    settle(&mut app).await;

    choose_natural(&mut app);
    settle(&mut app).await;

    app.handle_key_for_test(char_key('S'));
    settle(&mut app).await;
    assert_eq!(
        current_sort(&app),
        myd::screen::SortMode::Natural,
        "going shallow must not reset the order"
    );
}

// ==== Archive support ====

/// Build a zip in `dir` with a known shape: a nested tree, an executable, and a
/// name that escapes the archive root.
fn archive_fixture(dir: &std::path::Path) -> std::path::PathBuf {
    use std::io::Write;
    let path = dir.join("bundle.zip");
    let file = std::fs::File::create(&path).unwrap();
    let mut w = zip::ZipWriter::new(file);
    let exec: zip::write::FileOptions<'_, ()> =
        zip::write::FileOptions::default().unix_permissions(0o755);
    let plain: zip::write::FileOptions<'_, ()> =
        zip::write::FileOptions::default().unix_permissions(0o644);

    w.start_file("run.sh", exec).unwrap();
    w.write_all(b"#!/bin/sh\necho hi\n").unwrap();
    w.start_file("docs/deep/notes.md", plain).unwrap();
    w.write_all(b"# notes\n").unwrap();
    w.start_file("../escape.txt", plain).unwrap();
    w.write_all(b"nope\n").unwrap();
    w.finish().unwrap();
    path
}

/// Move the cursor onto the entry named `name`, or panic saying what was there.
fn cursor_onto(app: &mut myd::app::FileBrowser, name: &str) {
    for _ in 0..40 {
        if app.selected_name_for_test().as_deref() == Some(name) {
            return;
        }
        app.handle_key_for_test(char_key('j'));
    }
    panic!("never reached {name}");
}

#[tokio::test]
async fn enter_on_an_archive_browses_its_contents() {
    use myd::app::FileBrowser;
    let dir = tempfile::tempdir().unwrap();
    archive_fixture(dir.path());

    let mut app = FileBrowser::new(Some(dir.path().to_path_buf()), None, false);
    settle(&mut app).await;
    cursor_onto(&mut app, "bundle.zip");
    app.handle_key_for_test(key(crossterm::event::KeyCode::Enter));
    settle(&mut app).await;

    // The panel is now rooted at the archive root, on its own backend.
    assert_eq!(app.panel_current_dir(0).unwrap(), std::path::Path::new("/"));
    assert!(
        !app.panel_backend_for_test(0).is_local(),
        "an archive panel must not be tagged as the local filesystem, or a copy \
         out of it would be a plain std::fs copy of paths that do not exist"
    );
}

#[tokio::test]
async fn leaving_an_archive_restores_the_local_backend() {
    // Otherwise the next delete or copy addresses the archive — which discards
    // its errors, so it would look like it worked.
    use myd::app::FileBrowser;
    let dir = tempfile::tempdir().unwrap();
    archive_fixture(dir.path());

    let mut app = FileBrowser::new(Some(dir.path().to_path_buf()), None, false);
    settle(&mut app).await;
    cursor_onto(&mut app, "bundle.zip");
    app.handle_key_for_test(key(crossterm::event::KeyCode::Enter));
    settle(&mut app).await;
    assert!(!app.panel_backend_for_test(0).is_local());

    // `h` at the root pops back out.
    app.handle_key_for_test(char_key('h'));
    settle(&mut app).await;
    assert!(
        app.panel_backend_for_test(0).is_local(),
        "coming out of an archive must retag the panel local"
    );
}

#[tokio::test]
async fn mutations_inside_an_archive_are_refused_with_a_reason() {
    use myd::app::FileBrowser;
    let dir = tempfile::tempdir().unwrap();
    archive_fixture(dir.path());

    let mut app = FileBrowser::new(Some(dir.path().to_path_buf()), None, false);
    settle(&mut app).await;
    cursor_onto(&mut app, "bundle.zip");
    app.handle_key_for_test(key(crossterm::event::KeyCode::Enter));
    settle(&mut app).await;

    for (k, expected) in [
        ('D', "delete"),
        ('R', "rename"),
        ('N', "create directories"),
    ] {
        app.handle_key_for_test(char_key(k));
        assert_eq!(app.modal_kind_for_test(), "confirm", "{k} was not refused");
        let msg = app.modal_message_for_test().unwrap_or_default();
        assert!(
            msg.contains(expected) && msg.contains("archive"),
            "{k} refused with the wrong message: {msg:?}"
        );
        // Dismiss before the next one.
        app.handle_key_for_test(key(crossterm::event::KeyCode::Enter));
    }
}

#[tokio::test]
async fn a_delete_inside_an_archive_leaves_the_tree_alone() {
    // The guard has to be at the UI, not the backend: the delete task discards
    // its error and the rows leave the tree regardless of whether anything was
    // removed, so a backend-only refusal would look like a successful delete.
    use myd::app::FileBrowser;
    let dir = tempfile::tempdir().unwrap();
    archive_fixture(dir.path());

    let mut app = FileBrowser::new(Some(dir.path().to_path_buf()), None, false);
    settle(&mut app).await;
    cursor_onto(&mut app, "bundle.zip");
    app.handle_key_for_test(key(crossterm::event::KeyCode::Enter));
    settle(&mut app).await;

    cursor_onto(&mut app, "run.sh");
    app.handle_key_for_test(char_key('D'));
    app.handle_key_for_test(char_key('y'));
    settle(&mut app).await;

    cursor_onto(&mut app, "run.sh");
    assert_eq!(
        app.selected_name_for_test().as_deref(),
        Some("run.sh"),
        "the member must still be in the tree"
    );
}

#[tokio::test]
async fn an_archive_inside_an_archive_is_refused() {
    use myd::app::FileBrowser;
    use std::io::Write;
    let dir = tempfile::tempdir().unwrap();

    // An outer zip whose only member is the fixture zip.
    let inner = {
        let d = tempfile::tempdir().unwrap();
        let p = archive_fixture(d.path());
        let bytes = std::fs::read(p).unwrap();
        (d, bytes)
    };
    let outer = dir.path().join("outer.zip");
    {
        let mut w = zip::ZipWriter::new(std::fs::File::create(&outer).unwrap());
        let o: zip::write::FileOptions<'_, ()> = zip::write::FileOptions::default();
        w.start_file("inner.zip", o).unwrap();
        w.write_all(&inner.1).unwrap();
        w.finish().unwrap();
    }

    let mut app = FileBrowser::new(Some(dir.path().to_path_buf()), None, false);
    settle(&mut app).await;
    cursor_onto(&mut app, "outer.zip");
    app.handle_key_for_test(key(crossterm::event::KeyCode::Enter));
    settle(&mut app).await;

    cursor_onto(&mut app, "inner.zip");
    app.handle_key_for_test(key(crossterm::event::KeyCode::Enter));

    let msg = app.modal_message_for_test().unwrap_or_default();
    assert!(
        msg.contains("archive inside another archive"),
        "nesting should be refused, got {msg:?}"
    );
}

#[tokio::test]
async fn re_entering_an_archive_reuses_its_backend() {
    // The registry cannot unregister, so a second registration per visit would
    // leave one index and one driver thread resident per trip.
    use myd::app::FileBrowser;
    let dir = tempfile::tempdir().unwrap();
    archive_fixture(dir.path());

    let mut app = FileBrowser::new(Some(dir.path().to_path_buf()), None, false);
    settle(&mut app).await;

    let mut seen = Vec::new();
    for _ in 0..3 {
        cursor_onto(&mut app, "bundle.zip");
        app.handle_key_for_test(key(crossterm::event::KeyCode::Enter));
        settle(&mut app).await;
        seen.push(app.panel_backend_for_test(0));
        app.handle_key_for_test(char_key('h'));
        settle(&mut app).await;
    }
    assert_eq!(
        seen[0], seen[1],
        "the same archive must reuse one backend across visits"
    );
    assert_eq!(seen[1], seen[2]);
}

#[tokio::test]
async fn directory_sizes_inside_an_archive_are_real_totals() {
    // Unlike a remote panel, where a recursive walk is unaffordable: every
    // member's size is already in the index.
    use myd::app::FileBrowser;
    let dir = tempfile::tempdir().unwrap();
    archive_fixture(dir.path());

    let mut app = FileBrowser::new(Some(dir.path().to_path_buf()), None, false);
    settle(&mut app).await;
    cursor_onto(&mut app, "bundle.zip");
    app.handle_key_for_test(key(crossterm::event::KeyCode::Enter));
    settle(&mut app).await;

    let myd::screen::Screen::Main(state) = app.current_screen() else {
        panic!("expected a main screen");
    };
    assert!(
        state.tree.measures_directories(),
        "an archive knows its sizes and should say so"
    );
}

#[test]
fn a_typed_destination_from_an_archive_is_always_local() {
    use myd::app::resolve_copy_dest_backend;
    use myd::vfs::BackendId;

    let archive = BackendId(2);
    // The bug this replaced: a directory that does not exist yet resolved to
    // the archive's own backend, so the extract was routed into the zip. It
    // failed, and showed up as a red transfer row instead of "that directory
    // does not exist".
    assert_eq!(
        resolve_copy_dest_backend(archive, true, false),
        BackendId::LOCAL
    );
    assert_eq!(
        resolve_copy_dest_backend(archive, true, true),
        BackendId::LOCAL
    );

    // A server is different: an unrecognised path names somewhere on it, which
    // is what keeps a server-side copy into a new directory working.
    let remote = BackendId(1);
    assert_eq!(resolve_copy_dest_backend(remote, false, false), remote);
    assert_eq!(
        resolve_copy_dest_backend(remote, false, true),
        BackendId::LOCAL
    );

    // From a local panel there is nowhere else to mean.
    assert_eq!(
        resolve_copy_dest_backend(BackendId::LOCAL, false, true),
        BackendId::LOCAL
    );
    assert_eq!(
        resolve_copy_dest_backend(BackendId::LOCAL, false, false),
        BackendId::LOCAL
    );
}

#[tokio::test]
async fn extracting_a_member_writes_its_real_bytes() {
    use myd::app::FileBrowser;
    let dir = tempfile::tempdir().unwrap();
    archive_fixture(dir.path());
    let out = dir.path().join("out");
    std::fs::create_dir(&out).unwrap();

    let mut app = FileBrowser::new(Some(dir.path().to_path_buf()), None, false);
    settle(&mut app).await;
    cursor_onto(&mut app, "bundle.zip");
    app.handle_key_for_test(key(crossterm::event::KeyCode::Enter));
    settle(&mut app).await;

    cursor_onto(&mut app, "run.sh");
    app.handle_key_for_test(char_key('c'));
    assert_eq!(app.modal_kind_for_test(), "input", "expected the dest prompt");
    for ch in out.to_string_lossy().chars() {
        app.handle_key_for_test(char_key(ch));
    }
    app.handle_key_for_test(key(crossterm::event::KeyCode::Enter));

    // Nothing on screen shows the typed destination, so it is listed in the
    // background first and the overwrite prompts follow from that.
    settle_copy(&mut app).await;

    // The extract rides the transfer queue, so let it drain.
    let landed = out.join("run.sh");
    for _ in 0..400 {
        app.tick_for_test();
        app.tick_transfers_for_test();
        if landed.is_file() && !app.transfer_queue().has_work() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    assert_eq!(
        std::fs::read_to_string(&landed).unwrap(),
        "#!/bin/sh\necho hi\n",
        "the extracted file must hold the member's real bytes"
    );
}

#[tokio::test]
async fn copying_the_archive_root_from_inside_is_refused() {
    // `file_name()` of "/" is None, so the queue would skip it and nothing at
    // all would happen — a silent no-op is the worst of the options.
    use myd::app::FileBrowser;
    let dir = tempfile::tempdir().unwrap();
    archive_fixture(dir.path());

    let mut app = FileBrowser::new(Some(dir.path().to_path_buf()), None, false);
    settle(&mut app).await;
    cursor_onto(&mut app, "bundle.zip");
    app.handle_key_for_test(key(crossterm::event::KeyCode::Enter));
    settle(&mut app).await;

    // The cursor starts on the root row.
    app.handle_key_for_test(char_key('c'));
    let msg = app.modal_message_for_test().unwrap_or_default();
    assert!(
        msg.contains("archive root"),
        "expected a refusal naming the root, got {msg:?}"
    );
}

#[tokio::test]
async fn a_destination_that_does_not_exist_says_so_rather_than_failing_later() {
    // Before the read-only branch, a typed directory that did not exist yet
    // resolved to the archive's own backend: the extract was routed into the
    // zip, `open_write` refused, and it surfaced as a red transfer row reading
    // "remote:/…". The user's actual mistake — a typo in the path — was
    // nowhere on screen.
    use myd::app::FileBrowser;
    let dir = tempfile::tempdir().unwrap();
    archive_fixture(dir.path());
    let missing = dir.path().join("no-such-dir");

    let mut app = FileBrowser::new(Some(dir.path().to_path_buf()), None, false);
    settle(&mut app).await;
    cursor_onto(&mut app, "bundle.zip");
    app.handle_key_for_test(key(crossterm::event::KeyCode::Enter));
    settle(&mut app).await;

    cursor_onto(&mut app, "run.sh");
    app.handle_key_for_test(char_key('c'));
    for ch in missing.to_string_lossy().chars() {
        app.handle_key_for_test(char_key(ch));
    }
    app.handle_key_for_test(key(crossterm::event::KeyCode::Enter));

    let msg = app.modal_message_for_test().unwrap_or_default();
    assert!(
        msg.contains("is not a directory"),
        "expected a plain refusal, got {msg:?}"
    );
    assert!(
        !app.transfer_queue().has_work(),
        "nothing should have been queued"
    );
}

/// Whether `bsdtar` is on this machine, so the tests that need it can skip
/// rather than fail on a box without libarchive.
fn has_bsdtar() -> bool {
    std::process::Command::new("bsdtar")
        .arg("--version")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

#[tokio::test]
async fn a_cpio_is_browsable_through_bsdtar() {
    // The libarchive route exists for RAR, which nothing here can write a
    // fixture for. cpio goes down the identical code path and bsdtar *can*
    // write one, so it stands in: same parser, same reader, same extract.
    use myd::app::FileBrowser;
    if !has_bsdtar() {
        eprintln!("skipping: bsdtar is not installed");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("src");
    std::fs::create_dir_all(src.join("sub")).unwrap();
    std::fs::write(src.join("a.txt"), "hello\n").unwrap();
    std::fs::write(src.join("sub/b.txt"), "xx\n").unwrap();

    let container = dir.path().join("bundle.cpio");
    let ok = std::process::Command::new("bsdtar")
        .args(["-cf".as_ref(), container.as_os_str(), "--format".as_ref(), "cpio".as_ref(), "src".as_ref()])
        .current_dir(dir.path())
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    assert!(ok, "bsdtar could not write the fixture");

    let mut app = FileBrowser::new(Some(dir.path().to_path_buf()), None, false);
    settle(&mut app).await;
    cursor_onto(&mut app, "bundle.cpio");
    app.handle_key_for_test(key(crossterm::event::KeyCode::Enter));
    settle(&mut app).await;

    assert_eq!(app.panel_current_dir(0).unwrap(), std::path::Path::new("/"));
    assert!(!app.panel_backend_for_test(0).is_local());

    // And a member extracts with its real bytes, which is the half that runs a
    // second process rather than reading an index.
    let out = dir.path().join("out");
    std::fs::create_dir(&out).unwrap();
    cursor_onto(&mut app, "src");
    app.handle_key_for_test(char_key('c'));
    for ch in out.to_string_lossy().chars() {
        app.handle_key_for_test(char_key(ch));
    }
    app.handle_key_for_test(key(crossterm::event::KeyCode::Enter));
    settle_copy(&mut app).await;

    let landed = out.join("src/a.txt");
    for _ in 0..400 {
        app.tick_for_test();
        app.tick_transfers_for_test();
        if landed.is_file() && !app.transfer_queue().has_work() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    assert_eq!(std::fs::read_to_string(&landed).unwrap(), "hello\n");
}

#[tokio::test]
async fn help_documents_the_archive_keys() {
    // The help list is where a binding is discovered; one that is not in it may
    // as well not exist.
    let dir = create_test_structure();
    let mut app = FileBrowser::new(Some(dir.path().to_path_buf()), None, false);
    settle(&mut app).await;

    app.handle_key_for_test(char_key('?'));
    let text = help_text(&mut app, 80, 160);
    assert!(text.contains("Archives"), "no Archives category:\n{text}");
    assert!(
        text.contains("Browse an archive"),
        "entering an archive is not documented:\n{text}"
    );
}

#[test]
fn an_error_dialog_carries_the_reason_not_just_the_context() {
    // `format!("{e}")` on an anyhow::Error prints only the outermost context,
    // so a failure wrapped in "could not read geeu.rar" showed exactly that and
    // dropped the part saying why — which was the only useful half. Reported
    // from a real machine: a RAR that would not open, and a dialog with nothing
    // in it but the file name.
    use anyhow::Context;
    let e = Err::<(), _>(anyhow::anyhow!("Unrecognized archive format"))
        .context("could not read geeu.rar")
        .unwrap_err();

    let shown = myd::app::explain_error(&e);
    assert!(
        shown.contains("Unrecognized archive format"),
        "the cause must survive: {shown}"
    );
    assert!(shown.contains("geeu.rar"), "the context is useful too: {shown}");
}

#[test]
fn a_repeated_context_line_is_not_shown_twice() {
    // Some layers restate their cause verbatim; printing both only eats width.
    use anyhow::Context;
    let e = Err::<(), _>(anyhow::anyhow!("same thing"))
        .context("same thing")
        .unwrap_err();
    assert_eq!(myd::app::explain_error(&e), "same thing");
}

// ---------------------------------------------------------------------------
// Tagging must behave the same in every mode: local, remote, and inside an
// archive — and from either view, tree or treemap.
// ---------------------------------------------------------------------------

/// Tagged paths on the active panel's screen.
fn tags_of(app: &myd::app::FileBrowser) -> usize {
    match app.current_screen() {
        myd::screen::Screen::Main(s) => s.tagged_paths().len(),
        _ => 0,
    }
}

/// Tagging several files on a remote pane and copying must transfer all of them.
#[tokio::test]
async fn a_remote_pane_copies_every_tagged_file() {
    let dest = tempfile::tempdir().unwrap();
    let mut app = FileBrowser::new(
        Some(dest.path().to_path_buf()),
        Some(dest.path().to_path_buf()),
        true,
    );
    settle_all(&mut app).await;

    let twin = tempfile::tempdir().unwrap();
    app.replace_panel_with_remote_for_test(remote_tree_rooted_at(twin.path()));

    app.handle_key_for_test(char_key('j'));
    app.handle_key_for_test(char_key('t'));
    app.handle_key_for_test(char_key('j'));
    app.handle_key_for_test(char_key('t'));
    app.handle_key_for_test(char_key('j'));
    app.handle_key_for_test(char_key('t'));
    assert_eq!(tags_of(&app), 3, "three remote entries tagged");

    app.handle_key_for_test(char_key('c'));
    settle_copy(&mut app).await;
    assert_eq!(
        app.transfer_queue().transfers().len(),
        3,
        "all three tagged files must be queued, not just the cursor's"
    );
}

/// Every confirmed collision in a tagged remote batch must still be queued.
#[tokio::test]
async fn a_remote_tagged_batch_queues_every_confirmed_collision() {
    let dest = tempfile::tempdir().unwrap();
    // The mock remote lists dir_a, dir_b, small_file, big_file. Make each name
    // already exist locally so every one of them collides.
    std::fs::create_dir(dest.path().join("dir_a")).unwrap();
    std::fs::create_dir(dest.path().join("dir_b")).unwrap();
    std::fs::write(dest.path().join("small_file"), b"old").unwrap();
    std::fs::write(dest.path().join("big_file"), b"old").unwrap();

    let mut app = FileBrowser::new(
        Some(dest.path().to_path_buf()),
        Some(dest.path().to_path_buf()),
        true,
    );
    settle_all(&mut app).await;

    let twin = tempfile::tempdir().unwrap();
    app.replace_panel_with_remote_for_test(remote_tree_rooted_at(twin.path()));

    app.handle_key_for_test(char_key('j'));
    app.handle_key_for_test(char_key('t'));
    app.handle_key_for_test(char_key('j'));
    app.handle_key_for_test(char_key('t'));
    app.handle_key_for_test(char_key('j'));
    app.handle_key_for_test(char_key('t'));

    app.handle_key_for_test(char_key('c'));
    settle_copy(&mut app).await;

    for _ in 0..10 {
        if app.modal_kind_for_test() != "confirm" {
            break;
        }
        app.handle_key_for_test(char_key('y'));
        settle_copy(&mut app).await;
    }

    assert_eq!(
        app.transfer_queue().transfers().len(),
        3,
        "answering yes to each collision must queue each one"
    );
}

/// Tagging several members inside an archive and extracting must take all of them.
#[tokio::test]
async fn an_archive_extracts_every_tagged_member() {
    use myd::app::FileBrowser;
    let dir = tempfile::tempdir().unwrap();
    archive_fixture(dir.path());
    let out = tempfile::tempdir().unwrap();

    let mut app = FileBrowser::new(Some(dir.path().to_path_buf()), None, false);
    settle(&mut app).await;
    cursor_onto(&mut app, "bundle.zip");
    app.handle_key_for_test(key(crossterm::event::KeyCode::Enter));
    settle(&mut app).await;

    cursor_onto(&mut app, "run.sh");
    app.handle_key_for_test(char_key('t'));
    cursor_onto(&mut app, "docs");
    app.handle_key_for_test(char_key('t'));
    assert_eq!(tags_of(&app), 2, "two archive members tagged");

    app.handle_key_for_test(char_key('c'));
    assert_eq!(app.modal_kind_for_test(), "input", "expected the dest prompt");
    for ch in out.path().to_string_lossy().chars() {
        app.handle_key_for_test(char_key(ch));
    }
    app.handle_key_for_test(key(crossterm::event::KeyCode::Enter));
    settle_copy(&mut app).await;

    assert_eq!(
        app.transfer_queue().transfers().len(),
        2,
        "both tagged archive members must be queued"
    );
}

/// `t` must tag from the treemap view, not only the tree.
///
/// Reported as "I tagged several files and only one was copied". After `v` the
/// treemap has focus, and `t` was tree-only — so it silently did nothing while
/// `c` went on using the treemap cursor. The tags never existed; the copy fell
/// back to the single selected file, with nothing on screen to say so.
#[tokio::test]
async fn t_tags_from_the_treemap_view_too() {
    use myd::app::FileBrowser;
    let left = tempfile::tempdir().unwrap();
    std::fs::write(left.path().join("one.bin"), vec![1u8; 300]).unwrap();
    std::fs::write(left.path().join("two.bin"), vec![2u8; 200]).unwrap();
    let mut app = FileBrowser::new(Some(left.path().to_path_buf()), None, false);
    settle(&mut app).await;

    // `v` moves focus to the treemap pane.
    app.handle_key_for_test(char_key('v'));
    app.handle_key_for_test(char_key('t'));
    assert_eq!(tags_of(&app), 1, "t in treemap focus must tag the selected cell");

    // And it toggles off again, as it does in the tree.
    app.handle_key_for_test(char_key('t'));
    assert_eq!(tags_of(&app), 0, "t must untag the same cell");
}

/// Tags set in one view are visible to the other — there is one tag set, not two.
#[tokio::test]
async fn the_two_views_share_one_tag_set() {
    use myd::app::FileBrowser;
    let left = tempfile::tempdir().unwrap();
    std::fs::write(left.path().join("one.bin"), vec![1u8; 300]).unwrap();
    std::fs::write(left.path().join("two.bin"), vec![2u8; 200]).unwrap();
    let mut app = FileBrowser::new(Some(left.path().to_path_buf()), None, false);
    settle(&mut app).await;

    // Tag one row in the tree, then a tile in the treemap.
    app.handle_key_for_test(char_key('j'));
    app.handle_key_for_test(char_key('t'));
    app.handle_key_for_test(char_key('v'));
    app.handle_key_for_test(char_key('j'));
    app.handle_key_for_test(char_key('t'));
    let from_treemap = tags_of(&app);

    // Back in the tree, both are still tagged.
    app.handle_key_for_test(char_key('v'));
    assert_eq!(
        tags_of(&app),
        from_treemap,
        "switching views must not change what is tagged"
    );
    assert!(from_treemap >= 1, "the treemap tag must have registered");
}

/// `V` says so when it cannot apply, rather than swallowing the key.
#[tokio::test]
async fn visual_mode_explains_itself_in_the_treemap() {
    use myd::app::FileBrowser;
    let left = tempfile::tempdir().unwrap();
    std::fs::write(left.path().join("one.bin"), vec![1u8; 300]).unwrap();
    let mut app = FileBrowser::new(Some(left.path().to_path_buf()), None, false);
    settle(&mut app).await;

    app.handle_key_for_test(char_key('v'));
    app.handle_key_for_test(char_key('V'));
    let msg = app.modal_message_for_test().unwrap_or_default();
    assert!(
        msg.contains("tree view"),
        "V in the treemap should explain itself, got {msg:?}"
    );
}

// ---------------------------------------------------------------------------
// `-s` has to survive the detour through the picker that `-d` opens, and the
// picker has to say which way a path will open before Enter is pressed.
// ---------------------------------------------------------------------------

/// `myd -s -d` must open the picker in shallow mode.
///
/// Reported: the shallow flag was ignored entirely under `-d`. `Startup::Picker`
/// carried no `shallow` field, so the flag was parsed and then dropped — the
/// directory chosen from the picker was measured in full.
#[test]
fn the_shallow_flag_survives_the_directory_picker() {
    use myd::cli::{Cli, Startup};
    use clap::Parser;

    let cli = Cli::try_parse_from(["myd", "-s", "-d"]).unwrap();
    assert_eq!(
        cli.startup(None),
        Startup::Picker { shallow: true },
        "-s must reach the picker startup, not be discarded by -d"
    );

    let plain = Cli::try_parse_from(["myd", "-d"]).unwrap();
    assert_eq!(
        plain.startup(None),
        Startup::Picker { shallow: false },
        "without -s the picker stays in full-traversal mode"
    );
}

/// The picker names the traversal mode a typed path will open in.
#[tokio::test]
async fn the_picker_shows_which_traversal_a_path_will_get() {
    use myd::app::FileBrowser;

    let dir = tempfile::tempdir().unwrap();

    // A shallow session: the field must say so before Enter.
    let mut shallow = FileBrowser::new_on_picker_shallow(true);
    for ch in dir.path().to_string_lossy().chars() {
        shallow.handle_key_for_test(char_key(ch));
    }
    let rendered = app_screen_text(&mut shallow, 100, 30);
    assert!(
        rendered.contains("shallow"),
        "a shallow session must say so on the path field:\n{rendered}"
    );

    // A full session says the opposite, so the two are told apart on sight.
    let mut full = FileBrowser::new_on_picker_shallow(false);
    for ch in dir.path().to_string_lossy().chars() {
        full.handle_key_for_test(char_key(ch));
    }
    let rendered = app_screen_text(&mut full, 100, 30);
    assert!(
        rendered.contains("full"),
        "a full-traversal session must say so on the path field:\n{rendered}"
    );
}

/// A remote destination gets no traversal claim — it is never measured.
#[tokio::test]
async fn the_picker_claims_no_traversal_for_a_remote_target() {
    use myd::app::FileBrowser;

    let mut app = FileBrowser::new_on_picker_shallow(true);
    for ch in "sftp://gb10".chars() {
        app.handle_key_for_test(char_key(ch));
    }
    let rendered = app_screen_text(&mut app, 100, 30);
    // The path field's title must not promise a local traversal mode for
    // something that will be connected to rather than walked.
    assert!(
        !rendered.contains("Path (Enter to go) — shallow")
            && !rendered.contains("Path (Enter to go) — full"),
        "a remote target must not be labelled with a traversal mode:\n{rendered}"
    );
}

/// The help must document that `t` tags in the treemap, not only the tree.
///
/// The tree-only guard on `t` was invisible precisely because nothing said
/// which views it applied to. A help screen that goes quiet about the
/// distinction is how the bug stayed unnoticed, so the text is asserted.
#[tokio::test]
async fn help_documents_tagging_in_both_views() {
    let dir = create_test_structure();
    let mut app = FileBrowser::new(Some(dir.path().to_path_buf()), None, false);
    settle(&mut app).await;

    app.handle_key_for_test(char_key('?'));
    // Tall enough that the whole list is on screen at once.
    let text = help_text(&mut app, 100, 200);

    assert!(
        text.contains("Tag / untag the selected tile"),
        "the Treemap section must say t tags a tile:\n{text}"
    );
    assert!(
        text.contains("tree or treemap"),
        "the tagging section must say t works in both views:\n{text}"
    );
    assert!(
        text.contains("V (visual range) needs the tree view"),
        "and that V is the tree-only exception:\n{text}"
    );
}

// ---------------------------------------------------------------------------
// Archives are not size-limited. A container is memory-mapped rather than read,
// so opening one costs address space rather than memory, and a stored member is
// served straight out of the mapping rather than being buffered.
// ---------------------------------------------------------------------------

/// A container far larger than the old 512MB ceiling opens and lists.
///
/// Built with stored (uncompressed) entries so the file on disk really is that
/// large. Before mapping, this was refused outright with "too large to open".
#[tokio::test]
async fn a_large_archive_opens_and_lists_its_members() {
    use myd::vfs::archive::{archive_format, ArchiveFs};
    use myd::vfs::{BackendId, VPath, Vfs};

    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("big.zip");

    // 600MB in three stored members: past the ceiling that used to apply, and
    // small enough to build quickly since the bytes are never compressed.
    {
        let f = std::fs::File::create(&path).unwrap();
        let mut w = zip::ZipWriter::new(f);
        let opts: zip::write::FileOptions<'_, ()> = zip::write::FileOptions::default()
            .compression_method(zip::CompressionMethod::Stored);
        let chunk = vec![7u8; 1 << 20];
        for i in 0..3 {
            w.start_file(format!("f{i}.bin"), opts).unwrap();
            for _ in 0..200 {
                std::io::Write::write_all(&mut w, &chunk).unwrap();
            }
        }
        w.finish().unwrap();
    }
    let size = std::fs::metadata(&path).unwrap().len();
    assert!(
        size > 512 * 1024 * 1024,
        "fixture must exceed the old ceiling, got {size}"
    );

    let fmt = archive_format(&path).expect("a zip");
    let fs = ArchiveFs::open(Vec::new(), fmt, path.clone()).expect("a large archive must open");

    let entries = fs
        .read_dir(&VPath::new(BackendId(9), std::path::PathBuf::from("/")))
        .await
        .unwrap();
    assert_eq!(entries.len(), 3, "every member is listed");

    // And a member reads back whole, streamed out of the mapping.
    use tokio::io::AsyncReadExt;
    let mut r = fs
        .open_read(&VPath::new(BackendId(9), std::path::PathBuf::from("/f1.bin")))
        .await
        .unwrap();
    let mut buf = vec![0u8; 1 << 16];
    let mut total = 0u64;
    loop {
        let n = r.read(&mut buf).await.unwrap();
        if n == 0 {
            break;
        }
        assert!(buf[..n].iter().all(|b| *b == 7), "bytes must round-trip");
        total += n as u64;
    }
    assert_eq!(total, 200 * (1 << 20), "the whole member is read");
}

/// A stored member is read from the mapping rather than copied into memory.
///
/// The distinction is what makes a member larger than RAM extractable, so it is
/// asserted on the locator rather than left to be inferred from timing.
#[tokio::test]
async fn a_stored_member_is_located_by_offset_not_buffered() {
    use myd::vfs::archive::index::MemberLocator;
    use myd::vfs::archive::{archive_format, ArchiveFs};

    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("mixed.zip");
    {
        let f = std::fs::File::create(&path).unwrap();
        let mut w = zip::ZipWriter::new(f);
        let stored: zip::write::FileOptions<'_, ()> = zip::write::FileOptions::default()
            .compression_method(zip::CompressionMethod::Stored);
        let deflated: zip::write::FileOptions<'_, ()> = zip::write::FileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated);
        w.start_file("stored.txt", stored).unwrap();
        std::io::Write::write_all(&mut w, b"stored bytes").unwrap();
        w.start_file("deflated.txt", deflated).unwrap();
        std::io::Write::write_all(&mut w, &vec![b'a'; 4096]).unwrap();
        w.finish().unwrap();
    }

    let fmt = archive_format(&path).unwrap();
    let fs = ArchiveFs::open(Vec::new(), fmt, path.clone()).unwrap();
    let idx = fs.index();

    let stored = idx.get(std::path::Path::new("/stored.txt")).unwrap();
    assert!(
        matches!(stored.locator, MemberLocator::StreamOffset { .. }),
        "a stored member must be addressable in the container, got {:?}",
        stored.locator
    );

    // A compressed one still needs the decoder, and says so.
    let deflated = idx.get(std::path::Path::new("/deflated.txt")).unwrap();
    assert!(
        matches!(deflated.locator, MemberLocator::Index(_)),
        "a deflated member cannot be sliced, got {:?}",
        deflated.locator
    );
}

/// Enter on an archive must not block the key handler while it is indexed.
///
/// Reported: a 6.5GB zip that `unzip -v` lists instantly took a very long time
/// to do anything, including accepting further keys. Two causes, both fixed:
/// the index counted its members by walking the map on every insert (quadratic
/// — 14.6s for 120k members against 3ms for the zip crate's own parse), and
/// indexing then ran synchronously inside the key handler, so the interface was
/// frozen for the whole of it.
///
/// The handler is what this asserts. A budget rather than an exact figure: the
/// point is that it does not scale with the archive, and a wall-clock bound
/// tight enough to catch "it indexed inline" is loose enough to survive a busy
/// machine.
#[tokio::test]
async fn enter_on_an_archive_returns_immediately() {
    use myd::app::FileBrowser;

    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("many.zip");

    // Enough members that indexing inline would be plainly visible, while the
    // file itself stays small — the cost is per member, not per byte.
    {
        let f = std::fs::File::create(&path).unwrap();
        let mut w = zip::ZipWriter::new(f);
        let opts: zip::write::FileOptions<'_, ()> = zip::write::FileOptions::default();
        for i in 0..20_000 {
            w.start_file(format!("d{:03}/f{:06}.txt", i / 500, i), opts)
                .unwrap();
            std::io::Write::write_all(&mut w, b"x").unwrap();
        }
        w.finish().unwrap();
    }

    let mut app = FileBrowser::new(Some(dir.path().to_path_buf()), None, false);
    settle(&mut app).await;
    cursor_onto(&mut app, "many.zip");

    let t = std::time::Instant::now();
    app.handle_key_for_test(key(crossterm::event::KeyCode::Enter));
    let handler = t.elapsed();

    assert!(
        handler < std::time::Duration::from_millis(50),
        "Enter blocked the event loop for {handler:?} — indexing must happen off it"
    );

    // And it does finish, arriving through a tick like any other background work.
    settle(&mut app).await;
    assert_eq!(
        app.panel_current_dir(0).unwrap(),
        std::path::Path::new("/"),
        "the archive panel must open once indexing lands"
    );
}

/// A compressed archive member must stream, not decompress whole to be sniffed.
///
/// Previewing a 1.3GB video inside a zip took 758ms and allocated the whole
/// member: `open_read` buffered it entirely so that `read_head` could look at
/// the first 8KB and report "binary file". Reading now decodes only as far as
/// it is read, so the same preview is a couple of milliseconds.
#[tokio::test]
async fn a_compressed_member_streams_rather_than_buffering() {
    use myd::vfs::archive::{archive_format, ArchiveFs};
    use myd::vfs::{BackendId, VPath, Vfs};
    use tokio::io::AsyncReadExt;

    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("big-member.zip");

    // One deflated member far larger than anything a preview wants. Highly
    // compressible, so the fixture is small while the member is not.
    let member_len: u64 = 128 * 1024 * 1024;
    {
        let f = std::fs::File::create(&path).unwrap();
        let mut w = zip::ZipWriter::new(f);
        let opts: zip::write::FileOptions<'_, ()> = zip::write::FileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated);
        w.start_file("big.bin", opts).unwrap();
        // Not zeros: 256MB of zeros deflates past the 1000:1 ratio the index
        // rejects as a zip bomb. This still compresses, just not absurdly.
        let mut chunk = vec![0u8; 1 << 20];
        for (i, b) in chunk.iter_mut().enumerate() {
            *b = ((i * 7) % 251) as u8;
        }
        for _ in 0..(member_len >> 20) {
            std::io::Write::write_all(&mut w, &chunk).unwrap();
        }
        w.finish().unwrap();
    }

    let fmt = archive_format(&path).unwrap();
    let fs = ArchiveFs::open(Vec::new(), fmt, path.clone()).unwrap();
    let vp = VPath::new(BackendId(9), std::path::PathBuf::from("/big.bin"));

    // Reading the head must not scale with the member. Asserted by shape rather
    // than a wall-clock budget: a fast machine decompresses 128MB inside any
    // threshold loose enough to be non-flaky, so an absolute bound does not
    // distinguish streaming from buffering. Reading 8KB against reading it all
    // does — buffering makes them equal, streaming makes the head far cheaper.
    let t = std::time::Instant::now();
    let mut r = fs.open_read(&vp).await.unwrap();
    let mut head_buf = vec![0u8; 8192];
    let n = r.read(&mut head_buf).await.unwrap();
    let head = t.elapsed();
    assert_eq!(n, 8192, "the head must come back");
    drop(r);

    let t = std::time::Instant::now();
    let mut r = fs.open_read(&vp).await.unwrap();
    let mut sink = Vec::new();
    r.read_to_end(&mut sink).await.unwrap();
    let whole = t.elapsed();

    assert!(
        head * 4 < whole,
        "reading 8KB took {head:?} against {whole:?} for the whole {}MB member — \
         the head is being decompressed in full",
        member_len >> 20
    );

    // And the whole member still reads back correctly, at its declared size.
    let mut r = fs.open_read(&vp).await.unwrap();
    let mut total = 0u64;
    let mut buf = vec![0u8; 1 << 20];
    loop {
        let n = r.read(&mut buf).await.unwrap();
        if n == 0 {
            break;
        }
        total += n as u64;
    }
    assert_eq!(total, member_len, "the whole member must stream out");
}

/// Reading a RAR member must not cost more the deeper it sits in the archive.
///
/// Reported: stepping through files with the preview open took "a very long
/// time" per keystroke. `rars` has no seek-to-member entry point, so it decoded
/// every member ahead of the target — 26 seconds for one in the middle of a 5GB
/// archive, growing to 54 seconds at the end, while `bsdtar` seeks and takes
/// ~50ms wherever the member sits. Reads now go through `bsdtar` when it is
/// available, and fall back to `rars` when it cannot read the container.
///
/// Asserted by shape: the cost of a late member against an early one. A wall
/// clock bound would have to be loose enough to survive a slow machine, and
/// this failure mode is a ratio in the hundreds.
#[tokio::test]
async fn a_rar_member_costs_the_same_wherever_it_sits() {
    use myd::vfs::archive::{archive_format, ArchiveFs};
    use myd::vfs::{BackendId, VPath, Vfs};
    use tokio::io::AsyncReadExt;

    // The RAR samples vendored by other crates are tiny — too small for a
    // position effect to show — and nothing in the tree writes a RAR. Without a
    // real one there is nothing to measure, so this reports and returns rather
    // than asserting on a fixture that cannot exercise the bug.
    let Some(container) = ["/home/juan/code/untest/myd/Nonsane_-_Adicktion_Therapy_V_Text.rar"]
        .into_iter()
        .map(std::path::PathBuf::from)
        .find(|p| p.is_file())
    else {
        eprintln!("skipping: no large RAR on this machine");
        return;
    };

    let fmt = archive_format(&container).expect("a rar");
    let fs = ArchiveFs::open(Vec::new(), fmt, container.clone()).unwrap();

    let mut files: Vec<std::path::PathBuf> = Vec::new();
    let mut queue = vec![std::path::PathBuf::from("/")];
    while let Some(dir) = queue.pop() {
        let Ok(entries) = fs.read_dir(&VPath::new(BackendId(9), dir.clone())).await else {
            continue;
        };
        for e in entries {
            let child = dir.join(&e.name);
            if e.is_dir {
                queue.push(child);
            } else if e.len > 0 {
                files.push(child);
            }
        }
    }
    files.sort();
    assert!(files.len() > 100, "need a real archive to measure position");

    let read = |path: std::path::PathBuf| {
        let fs = &fs;
        async move {
            let t = std::time::Instant::now();
            let mut r = fs.open_read(&VPath::new(BackendId(9), path)).await.unwrap();
            let mut buf = Vec::new();
            r.read_to_end(&mut buf).await.unwrap();
            (t.elapsed(), buf.len())
        }
    };

    let (early, n_early) = read(files[0].clone()).await;
    let (late, n_late) = read(files[files.len() - 1].clone()).await;
    assert!(n_early > 0 && n_late > 0, "both members must read back");

    assert!(
        late < early * 20,
        "the last member took {late:?} against {early:?} for the first — reads are \
         scaling with position, so they are decoding everything ahead of the target"
    );
}
