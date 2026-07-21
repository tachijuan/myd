use std::time::Instant;

/// Integration test that verifies FileTree loads quickly even with many entries.
/// Also verifies that sizes are computed correctly (non-zero for files with content).
#[test]
fn test_load_performance() {
    let test_dir = tempfile::tempdir().unwrap();

    // Create a directory with many subdirectories containing files
    for i in 0..20 {
        let sub = test_dir.path().join(format!("sub_{:02}", i));
        std::fs::create_dir_all(&sub).unwrap();
        for j in 0..10 {
            std::fs::write(sub.join(format!("file_{:02}.txt", j)), "x".repeat(100 * (j + 1))).unwrap();
        }
    }
    // Also add some files at the top level
    for i in 0..15 {
        std::fs::write(test_dir.path().join(format!("file_{:02}.dat", i)), "y".repeat(500 * (i + 1))).unwrap();
    }

    let start = Instant::now();
    let tree = file_browser::widget::file_tree::FileTree::new(
        test_dir.path().to_path_buf(),
        file_browser::screen::SortMode::Largest,
        true, // show hidden
        true, // show size bar
    );
    let load_time = start.elapsed();

    // Should load in under 500ms (generous limit)
    eprintln!("Load time: {:?}", load_time);
    assert!(load_time < std::time::Duration::from_millis(500), "Load took {:?}", load_time);

    // Should have root + 20 subdirs + 15 files = 36 lines
    assert_eq!(tree.lines.len(), 36, "Expected 36 lines, got {}", tree.lines.len());

    // Verify sizes are computed when rendering
    let text = tree.render_text();
    assert_eq!(text.lines.len(), 36);

    // Check that file sizes are non-zero in the cache after rendering
    let mut non_zero_count = 0;
    for line in &tree.lines {
        if let Some(size) = tree.size_cache.get(&line.resolved_path) {
            if size > 0 {
                non_zero_count += 1;
            }
        }
    }
    // All 15 files should have non-zero sizes, plus 20 dirs with shallow sizes > 0
    assert!(non_zero_count >= 15, "Expected at least 15 non-zero sizes, got {}", non_zero_count);
}

/// Verify that rendering after load is fast (no blocking I/O).
#[test]
fn test_render_performance() {
    let test_dir = tempfile::tempdir().unwrap();

    for i in 0..30 {
        std::fs::write(test_dir.path().join(format!("file_{:02}.txt", i)), "data".repeat(i * 10)).unwrap();
    }

    let tree = file_browser::widget::file_tree::FileTree::new(
        test_dir.path().to_path_buf(),
        file_browser::screen::SortMode::Largest,
        true,
        true,
    );

    // Render multiple times — should all be fast since sizes are cached
    let start = Instant::now();
    for _ in 0..20 {
        let _ = tree.render_text();
    }
    let render_time = start.elapsed();
    eprintln!("20x render time: {:?}", render_time);
    assert!(render_time < std::time::Duration::from_millis(100), "20x render took {:?}", render_time);
}

/// Verify navigating into subdirectories is fast.
#[test]
fn test_navigate_depth_performance() {
    let base = tempfile::tempdir().unwrap();

    // Create 5 levels of nesting, each with 10 subdirectories and 5 files
    let mut current = base.path().to_path_buf();
    for depth in 0..5 {
        for i in 0..10 {
            let sub = current.join(format!("sub_{}_{}", depth, i));
            std::fs::create_dir_all(&sub).unwrap();
        }
        for i in 0..5 {
            std::fs::write(current.join(format!("file_{}_{}.txt", depth, i)), "content").unwrap();
        }
        // Navigate into first subdirectory
        current = current.join(format!("sub_{}_0", depth));
    }

    // Load the deepest directory (level 5 — has 10 subs + 5 files = 16 lines including root)
    for i in 0..10 {
        std::fs::create_dir_all(current.join(format!("sub_5_{}", i))).unwrap();
    }
    for i in 0..5 {
        std::fs::write(current.join(format!("file_5_{}.txt", i)), "content").unwrap();
    }

    let start = Instant::now();
    let tree = file_browser::widget::file_tree::FileTree::new(
        current.clone(),
        file_browser::screen::SortMode::Largest,
        true,
        true,
    );
    let load_time = start.elapsed();
    eprintln!("Deepest dir load time: {:?}", load_time);
    assert!(load_time < std::time::Duration::from_millis(200), "Deep load took {:?}", load_time);
    assert_eq!(tree.lines.len(), 16); // root + 10 subs + 5 files
}

/// Verify rendering an expanded tree (multiple levels visible) is fast.
/// This is the j/k navigation hot path — every keystroke triggers a full re-render.
#[test]
fn test_expanded_tree_render() {
    let base = tempfile::tempdir().unwrap();

    // Create a flat directory with many entries at multiple levels
    for i in 0..30 {
        let sub = base.path().join(format!("sub_{:02}", i));
        std::fs::create_dir_all(&sub).unwrap();
        for j in 0..5 {
            std::fs::write(sub.join(format!("file_{:02}.txt", j)), "data".repeat(100)).unwrap();
        }
    }
    for i in 0..20 {
        std::fs::write(base.path().join(format!("file_{:02}.txt", i)), "data".repeat(200)).unwrap();
    }

    let mut tree = file_browser::widget::file_tree::FileTree::new(
        base.path().to_path_buf(),
        file_browser::screen::SortMode::Largest,
        true,
        true,
    );

    // Expand all nodes to simulate a fully expanded tree
    tree.expand_all();

    // Should have root + 30 subs + 150 files + 20 top-level files = 201 lines
    eprintln!("Expanded tree lines: {}", tree.lines.len());
    assert!(tree.lines.len() > 100, "Expected >100 lines, got {}", tree.lines.len());

    // First render computes sizes
    let _ = tree.render_text();

    // Subsequent renders should be very fast (all cached, O(N) precomputation)
    let start = Instant::now();
    for _ in 0..50 {
        let _ = tree.render_text();
    }
    let render_time = start.elapsed();
    eprintln!("50x expanded render time: {:?}", render_time);
    assert!(
        render_time < std::time::Duration::from_millis(100),
        "50x expanded render took {:?}",
        render_time
    );
}
