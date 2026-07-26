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
            tm.render(f, a, cursor, None);
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
    app.handle_key_for_test(ctrl_key('b'));
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
    app.handle_key_for_test(ctrl_key('b'));
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
            .find(|c| c.label == name)
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
    for (label, host, uses) in [
        ("prod", "prod.example.com", 30u64),
        ("backup", "10.0.0.5", 20),
        ("scratch", "dev.local", 10),
        ("france", "fr.example.com", 5),
    ] {
        let mut h = SavedHost::new(label, host);
        h.uses = uses;
        h.user = Some("juan".into());
        hosts.push(h);
    }
    myd::hosts::HostCatalog::in_memory(hosts)
}

/// `gr` must open the saved-host picker, not the old free-text prompt.
#[tokio::test]
async fn gr_opens_the_dialing_directory() {
    let dir = create_test_structure();
    let mut app = FileBrowser::new(Some(dir.path().to_path_buf()), None, false);
    app.set_hosts_for_test(test_catalog());
    app.resolve_loading_for_test();

    app.handle_key_for_test(char_key('g'));
    app.handle_key_for_test(char_key('r'));

    assert_eq!(app.modal_kind_for_test(), "host_picker");
    // Ranked by use, so the most-used host is preselected.
    assert_eq!(app.picker_selection_for_test().as_deref(), Some("prod"));
    assert_eq!(
        app.picker_visible_count_for_test(),
        3,
        "the quick view should offer the top three"
    );
}

/// With nothing saved there is nothing to pick, so `gr` should go straight to
/// the typed-address prompt rather than showing an empty list.
#[tokio::test]
async fn gr_with_an_empty_catalog_prompts_for_an_address() {
    let dir = create_test_structure();
    let mut app = FileBrowser::new(Some(dir.path().to_path_buf()), None, false);
    app.set_hosts_for_test(myd::hosts::HostCatalog::in_memory(vec![]));
    app.resolve_loading_for_test();

    app.handle_key_for_test(char_key('g'));
    app.handle_key_for_test(char_key('r'));

    assert_eq!(app.modal_kind_for_test(), "input");
}

/// `gs` opens the full list directly.
#[tokio::test]
async fn gs_opens_the_full_host_list() {
    let dir = create_test_structure();
    let mut app = FileBrowser::new(Some(dir.path().to_path_buf()), None, false);
    app.set_hosts_for_test(test_catalog());
    app.resolve_loading_for_test();

    app.handle_key_for_test(char_key('g'));
    app.handle_key_for_test(char_key('s'));

    assert_eq!(app.modal_kind_for_test(), "host_picker");
    assert_eq!(app.picker_visible_count_for_test(), 4);
}

/// The picker owns j/k and `/` — they must navigate and search rather than
/// reaching the global keybindings or the chord detector.
#[tokio::test]
async fn picker_vi_navigation_and_search_work_through_the_app() {
    let dir = create_test_structure();
    let mut app = FileBrowser::new(Some(dir.path().to_path_buf()), None, false);
    app.set_hosts_for_test(test_catalog());
    app.resolve_loading_for_test();

    app.handle_key_for_test(char_key('g'));
    app.handle_key_for_test(char_key('s'));

    // j moves the cursor (hosts are sorted by label in the full view).
    let first = app.picker_selection_for_test().unwrap();
    app.handle_key_for_test(char_key('j'));
    assert_ne!(app.picker_selection_for_test().unwrap(), first);

    // / filters incrementally, and the cursor maps back to the right host.
    app.handle_key_for_test(char_key('/'));
    for c in "fra".chars() {
        app.handle_key_for_test(char_key(c));
    }
    assert_eq!(app.picker_visible_count_for_test(), 1);
    assert_eq!(app.picker_selection_for_test().as_deref(), Some("france"));

    // Still the picker; none of those keys leaked into the file tree.
    assert_eq!(app.modal_kind_for_test(), "host_picker");
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
    app.handle_key_for_test(char_key('s'));
    app.handle_key_for_test(char_key('a'));
    assert_eq!(app.modal_kind_for_test(), "input");

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
    assert_eq!(app.modal_kind_for_test(), "host_picker");
}

/// Deleting asks first, and removes only on confirmation.
#[tokio::test]
async fn deleting_a_host_requires_confirmation() {
    let dir = create_test_structure();
    let mut app = FileBrowser::new(Some(dir.path().to_path_buf()), None, false);
    app.set_hosts_for_test(test_catalog());
    app.resolve_loading_for_test();

    app.handle_key_for_test(char_key('g'));
    app.handle_key_for_test(char_key('s'));
    let doomed = app.picker_selection_for_test().unwrap();

    app.handle_key_for_test(char_key('d'));
    assert_eq!(app.modal_kind_for_test(), "confirm");

    // Decline: the host stays.
    app.handle_key_for_test(char_key('n'));
    assert!(app.hosts_for_test().find(&doomed).is_some());
    assert_eq!(app.modal_kind_for_test(), "host_picker");

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
    app.handle_key_for_test(char_key('s'));
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
    app.handle_key_for_test(char_key('s'));

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
