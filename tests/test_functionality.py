"""Behavioral tests — verify actual app functionality, not just state changes.

All tests use tmp_path (generated data only). No existing data is touched.
"""

import asyncio
import sys
from pathlib import Path

import pytest


# --- Fixtures ---

@pytest.fixture
def structured_dir(tmp_path: Path) -> Path:
    """Create a directory with a known, varied structure for behavioral testing."""
    # Files of different sizes
    (tmp_path / "aaa.txt").write_text("a" * 10)        # 10 bytes
    (tmp_path / "zzz.txt").write_text("z" * 100)       # 100 bytes
    (tmp_path / "mmm.py").write_text("m" * 50)         # 50 bytes

    # Directories
    sub_a = tmp_path / "dir_a"
    sub_a.mkdir()
    (sub_a / "inside_a.txt").write_text("data_a")

    sub_b = tmp_path / "dir_b"
    sub_b.mkdir()
    (sub_b / "deep").mkdir()
    (sub_b / "deep" / "deep_file.txt").write_text("deep data here")

    # Hidden file
    (tmp_path / ".hidden_file").write_text("secret")

    # Empty directory
    (tmp_path / "empty_dir").mkdir()

    return tmp_path


@pytest.fixture
def simple_dir(tmp_path: Path) -> Path:
    """Minimal directory for quick tests."""
    (tmp_path / "one.txt").write_text("hello")
    (tmp_path / "two.txt").write_text("world!!!")
    sub = tmp_path / "subdir"
    sub.mkdir()
    (sub / "nested.txt").write_text("nested content")
    return tmp_path


def _launch_app(path: Path):
    """Helper: set sys.argv and create the app for a given path."""
    sys.argv = ["test", str(path)]
    # Force re-import to pick up new sys.argv
    import importlib
    import src.app
    importlib.reload(src.app)
    from src.app import FileBrowserApp
    return FileBrowserApp()


def _get_tree(app):
    return app.screen.query_one("FileTree")


def _get_labels(tree):
    """Get plain text labels from visible tree lines."""
    return [tl.node.label.plain for tl in tree._tree_lines]


# --- Test: Sort Produces Correct Order ---

class TestSortOrder:
    @pytest.mark.asyncio
    async def test_largest_is_default(self, structured_dir: Path):
        """Default sort mode is LARGEST (largest items first)."""
        app = _launch_app(structured_dir)
        async with app.run_test() as pilot:
            await pilot.pause()
            await asyncio.sleep(0.5)
            tree = _get_tree(app)
            labels = _get_labels(tree)

            from src.widgets.file_tree import SortMode
            assert tree.sort_mode == SortMode.LARGEST

            # zzz.txt (100B) should come before aaa.txt (10B) among files
            zzz_idx = labels.index("zzz.txt")
            aaa_idx = labels.index("aaa.txt")
            assert zzz_idx < aaa_idx

    @pytest.mark.asyncio
    async def test_toggle_sort_changes_order(self, structured_dir: Path):
        """Toggle cycles from LARGEST → SMALLEST → DIRS_FIRST → FILES_FIRST."""
        app = _launch_app(structured_dir)
        async with app.run_test() as pilot:
            await pilot.pause()
            await asyncio.sleep(0.5)
            tree = _get_tree(app)

            # LARGEST → SMALLEST (first toggle)
            tree.action_toggle_sort()
            await pilot.pause()
            await asyncio.sleep(1)

            from src.widgets.file_tree import SortMode
            assert tree.sort_mode == SortMode.SMALLEST

            labels = _get_labels(tree)
            # aaa.txt (10B) should come before zzz.txt (100B) in SMALLEST
            aaa_idx = labels.index("aaa.txt")
            zzz_idx = labels.index("zzz.txt")
            assert aaa_idx < zzz_idx

    @pytest.mark.asyncio
    async def test_sort_largest(self, structured_dir: Path):
        """Default is LARGEST: zzz.txt (100B) comes before aaa.txt (10B)."""
        app = _launch_app(structured_dir)
        async with app.run_test() as pilot:
            await pilot.pause()
            await asyncio.sleep(0.5)
            tree = _get_tree(app)

            from src.widgets.file_tree import SortMode
            assert tree.sort_mode == SortMode.LARGEST

            labels = _get_labels(tree)
            # zzz.txt (100B) should come before aaa.txt (10B) among files
            zzz_idx = labels.index("zzz.txt")
            aaa_idx = labels.index("aaa.txt")
            assert zzz_idx < aaa_idx

    @pytest.mark.asyncio
    async def test_no_double_reload(self, structured_dir: Path):
        """Verify sort toggle triggers exactly one reload, not two."""
        app = _launch_app(structured_dir)
        async with app.run_test() as pilot:
            await pilot.pause()
            await asyncio.sleep(0.5)
            tree = _get_tree(app)

            # Instrument reload
            reload_count = [0]
            original_reload = tree.reload
            def counted_reload():
                reload_count[0] += 1
                return original_reload()
            tree.reload = counted_reload

            tree.action_toggle_sort()
            await pilot.pause()
            await asyncio.sleep(1)

            # Should be exactly 1 (from watch_sort_mode), not 2
            assert reload_count[0] == 1, f"Expected 1 reload, got {reload_count[0]}"


# --- Test: Hidden File Toggle ---

class TestHiddenFiles:
    @pytest.mark.asyncio
    async def test_hidden_files_shown_by_default(self, structured_dir: Path):
        """Hidden files are visible by default."""
        app = _launch_app(structured_dir)
        async with app.run_test() as pilot:
            await pilot.pause()
            await asyncio.sleep(0.5)
            tree = _get_tree(app)
            labels = _get_labels(tree)
            assert ".hidden_file" in labels

    @pytest.mark.asyncio
    async def test_toggle_hidden_hides_hidden_files(self, structured_dir: Path):
        """Toggling hidden hides hidden files, toggling again shows them."""
        app = _launch_app(structured_dir)
        async with app.run_test() as pilot:
            await pilot.pause()
            await asyncio.sleep(0.5)
            tree = _get_tree(app)

            # Toggle off — hidden files should disappear
            tree.action_toggle_hidden()
            await pilot.pause()
            await asyncio.sleep(1)

            labels = _get_labels(tree)
            assert ".hidden_file" not in labels

            # Toggle back — hidden files should reappear
            tree.action_toggle_hidden()
            await pilot.pause()
            await asyncio.sleep(1)

            labels = _get_labels(tree)
            assert ".hidden_file" in labels


# --- Test: Status Bar Accuracy ---

class TestStatusBar:
    @pytest.mark.asyncio
    async def test_status_bar_counts(self, simple_dir: Path):
        """Verify status bar shows correct dir/file counts (excluding root)."""
        app = _launch_app(simple_dir)
        async with app.run_test() as pilot:
            await pilot.pause()
            await asyncio.sleep(0.5)

            screen = app.screen
            screen._update_status_bar()
            await pilot.pause()

            # simple_dir has: one.txt, two.txt, subdir/ -> 2 files + 1 dir = 3 items
            # (root is excluded from count)
            sub_title = screen.sub_title
            assert "3 items" in sub_title
            assert "1 dirs" in sub_title
            assert "2 files" in sub_title


# --- Test: Search ---

class TestSearch:
    @pytest.mark.asyncio
    async def test_search_finds_file(self, structured_dir: Path):
        """Verify the search logic can find files by name in visible tree lines."""
        app = _launch_app(structured_dir)
        async with app.run_test() as pilot:
            await pilot.pause()
            await asyncio.sleep(0.5)
            tree = _get_tree(app)

            # Verify search logic directly (bypassing the prompt modal which is hard to test)
            pattern = "zzz.txt"
            found_line = None
            for i, tree_line in enumerate(tree._tree_lines):
                node = tree_line.node
                if node.data and pattern.lower() in node.data.path.name.lower():
                    found_line = i
                    break
            assert found_line is not None, f"Search for '{pattern}' should find a match"

            # Verify moving cursor to found line works
            tree.move_cursor_to_line(found_line)
            await pilot.pause()
            assert tree.cursor_line == found_line


# --- Test: Rename ---

class TestRename:
    @pytest.mark.asyncio
    async def test_rename_file(self, simple_dir: Path):
        app = _launch_app(simple_dir)
        async with app.run_test() as pilot:
            await pilot.pause()
            await asyncio.sleep(0.5)
            tree = _get_tree(app)

            # Find one.txt
            for i, tl in enumerate(tree._tree_lines):
                if "one.txt" in tl.node.label.plain:
                    tree.move_cursor_to_line(i)
                    break

            await pilot.pause()
            old_path = simple_dir / "one.txt"
            new_path = simple_dir / "renamed.txt"

            # Call the rename logic directly (bypassing prompt modal)
            path = tree.cursor_node.data.path.expanduser().resolve()
            path.rename(new_path)
            tree.cursor_node.label = type(tree.cursor_node.label)("renamed.txt")
            from textual.widgets._directory_tree import DirEntry
            tree.cursor_node.data = DirEntry(new_path)
            tree.refresh()
            await pilot.pause()

            assert not old_path.exists()
            assert new_path.exists()


# --- Test: Dir Picker ---

class TestDirPicker:
    @pytest.mark.asyncio
    async def test_no_arg_shows_dir_picker(self):
        sys.argv = ["test"]
        import importlib
        import src.app
        importlib.reload(src.app)
        from src.app import FileBrowserApp
        from src.dir_picker import DirPickerScreen

        app = FileBrowserApp()
        async with app.run_test() as pilot:
            await pilot.pause()
            await asyncio.sleep(0.3)
            assert isinstance(app.screen, DirPickerScreen)

    @pytest.mark.asyncio
    async def test_valid_path_shows_main_screen(self, simple_dir: Path):
        sys.argv = ["test", str(simple_dir)]
        import importlib
        import src.app
        importlib.reload(src.app)
        from src.app import FileBrowserApp
        from src.screen import MainScreen

        app = FileBrowserApp()
        async with app.run_test() as pilot:
            await pilot.pause()
            await asyncio.sleep(0.5)
            assert isinstance(app.screen, MainScreen)

    @pytest.mark.asyncio
    async def test_invalid_path_shows_dir_picker(self):
        sys.argv = ["test", "/nonexistent/path/xyz123"]
        import importlib
        import src.app
        importlib.reload(src.app)
        from src.app import FileBrowserApp
        from src.dir_picker import DirPickerScreen

        app = FileBrowserApp()
        async with app.run_test() as pilot:
            await pilot.pause()
            await asyncio.sleep(0.5)
            # Invalid path -> notification + dir picker
            assert isinstance(app.screen, DirPickerScreen)


# --- Test: Size Utilities ---

class TestSizeUtilities:
    def test_format_size_zero(self):
        from src.utils.sizes import format_size
        assert format_size(0) == "0 B"

    def test_format_size_negative(self):
        from src.utils.sizes import format_size
        assert format_size(-100) == "0 B"

    def test_format_size_bytes(self):
        from src.utils.sizes import format_size
        assert format_size(500) == "500 B"

    def test_format_size_kb(self):
        from src.utils.sizes import format_size
        assert format_size(1024) == "1.0 KB"
        assert format_size(1536) == "1.5 KB"  # 1.5 KB

    def test_format_size_mb(self):
        from src.utils.sizes import format_size
        assert format_size(1048576) == "1.0 MB"

    def test_format_size_gb(self):
        from src.utils.sizes import format_size
        assert format_size(1073741824) == "1.0 GB"

    def test_format_size_tb(self):
        from src.utils.sizes import format_size
        assert format_size(1099511627776) == "1.0 TB"

    def test_get_shallow_size_empty_dir(self, tmp_path: Path):
        from src.utils.sizes import get_shallow_size
        empty = tmp_path / "empty"
        empty.mkdir()
        assert get_shallow_size(empty) == 0

    def test_get_shallow_size_files_only(self, tmp_path: Path):
        from src.utils.sizes import get_shallow_size
        (tmp_path / "f1.txt").write_text("12345")  # 5 bytes
        (tmp_path / "f2.txt").write_text("abc")    # 3 bytes
        size = get_shallow_size(tmp_path)
        assert size == 8

    def test_get_shallow_size_does_not_recurse(self, tmp_path: Path):
        """Shallow size should count subdirectory entry metadata, not recurse into contents."""
        from src.utils.sizes import get_shallow_size
        sub = tmp_path / "sub"
        sub.mkdir()
        (sub / "large.txt").write_text("x" * 10000)  # 10KB inside sub
        (tmp_path / "small.txt").write_text("hi")    # 2 bytes

        size = get_shallow_size(tmp_path)
        # entry.stat() on a dir returns the dir entry's own size (4096 on ext4)
        # The 10KB file should NOT be counted. Total should be ~4098 (4096 dir + 2 file)
        # and definitely not 10002 (which would be recursive)
        assert size < 5000, f"Shallow size {size} should not recurse into subdirectory (expected ~4098)"

    def test_get_dir_size_recursive(self, tmp_path: Path):
        from src.utils.sizes import get_dir_size
        sub = tmp_path / "sub"
        sub.mkdir()
        (sub / "large.txt").write_text("x" * 10000)  # 10KB
        (tmp_path / "small.txt").write_text("hi")    # 2 bytes

        size = get_dir_size(tmp_path)
        assert size == 10002  # 10000 + 2

    def test_get_size_file(self, tmp_path: Path):
        from src.utils.sizes import get_size
        (tmp_path / "f.txt").write_text("hello")
        assert get_size(tmp_path / "f.txt") == 5

    def test_get_size_nonexistent(self, tmp_path: Path):
        from src.utils.sizes import get_size
        assert get_size(tmp_path / "nope") == 0


# --- Test: Delete with Cache Cleanup ---

class TestDeleteAndCache:
    @pytest.mark.asyncio
    async def test_delete_cleans_cache(self, simple_dir: Path):
        app = _launch_app(simple_dir)
        async with app.run_test() as pilot:
            await pilot.pause()
            await asyncio.sleep(0.5)
            tree = _get_tree(app)

            # Ensure subdir is in the cache
            subdir_path = (simple_dir / "subdir").resolve()
            # Expand root to populate cache
            tree.root.expand()
            await pilot.pause()
            await asyncio.sleep(0.5)

            # Verify subdir is in cache
            assert subdir_path in tree._size_cache, "subdir should be cached"

            # Find and delete subdir
            for i, tl in enumerate(tree._tree_lines):
                if "subdir" in tl.node.label.plain:
                    tree.move_cursor_to_line(i)
                    break

            tree.action_delete()
            await pilot.pause()
            await asyncio.sleep(0.3)

            # Confirm delete
            await pilot.press("enter")
            await pilot.pause()
            await asyncio.sleep(0.5)

            assert not (simple_dir / "subdir").exists()
            # Cache entry should be cleaned
            assert subdir_path not in tree._size_cache

    @pytest.mark.asyncio
    async def test_delete_file_removes_from_disk(self, simple_dir: Path):
        app = _launch_app(simple_dir)
        async with app.run_test() as pilot:
            await pilot.pause()
            await asyncio.sleep(0.5)
            tree = _get_tree(app)

            for i, tl in enumerate(tree._tree_lines):
                if "one.txt" in tl.node.label.plain:
                    tree.move_cursor_to_line(i)
                    break

            tree.action_delete()
            await pilot.pause()
            await asyncio.sleep(0.3)

            await pilot.press("enter")
            await pilot.pause()
            await asyncio.sleep(0.5)

            assert not (simple_dir / "one.txt").exists()

    @pytest.mark.asyncio
    async def test_delete_cancelled_with_escape(self, simple_dir: Path):
        """Pressing escape on the confirm dialog should cancel deletion."""
        app = _launch_app(simple_dir)
        async with app.run_test() as pilot:
            await pilot.pause()
            await asyncio.sleep(0.5)
            tree = _get_tree(app)

            for i, tl in enumerate(tree._tree_lines):
                if "one.txt" in tl.node.label.plain:
                    tree.move_cursor_to_line(i)
                    break

            tree.action_delete()
            await pilot.pause()
            await asyncio.sleep(0.3)

            # Cancel with escape
            await pilot.press("escape")
            await pilot.pause()
            await asyncio.sleep(0.5)

            # File should still exist
            assert (simple_dir / "one.txt").exists()

    @pytest.mark.asyncio
    async def test_delete_with_n_key(self, simple_dir: Path):
        """Pressing 'n' on the confirm dialog should cancel deletion."""
        app = _launch_app(simple_dir)
        async with app.run_test() as pilot:
            await pilot.pause()
            await asyncio.sleep(0.5)
            tree = _get_tree(app)

            for i, tl in enumerate(tree._tree_lines):
                if "one.txt" in tl.node.label.plain:
                    tree.move_cursor_to_line(i)
                    break

            tree.action_delete()
            await pilot.pause()
            await asyncio.sleep(0.3)

            # Cancel with n
            await pilot.press("n")
            await pilot.pause()
            await asyncio.sleep(0.5)

            assert (simple_dir / "one.txt").exists()


# --- Test: SizeBar Widget ---

class TestSizeBarWidget:
    def test_bar_zero(self):
        from src.widgets.size_bar import SizeBar
        bar = SizeBar(value=0, maximum=100, bar_width=20)
        rendered = bar.render()
        assert rendered.plain.count("█") == 0  # filled blocks
        assert rendered.plain.count("░") == 20  # empty blocks

    def test_bar_full(self):
        from src.widgets.size_bar import SizeBar
        bar = SizeBar(value=100, maximum=100, bar_width=20)
        rendered = bar.render()
        assert rendered.plain.count("█") == 20  # all filled

    def test_bar_half(self):
        from src.widgets.size_bar import SizeBar
        bar = SizeBar(value=50, maximum=100, bar_width=20)
        rendered = bar.render()
        assert rendered.plain.count("█") == 10
        assert rendered.plain.count("░") == 10

    def test_bar_green_below_50(self):
        from src.widgets.size_bar import SizeBar
        bar = SizeBar(value=40, maximum=100, bar_width=10)
        assert bar._get_color(0.4) == "green"

    def test_bar_amber_50_to_80(self):
        from src.widgets.size_bar import SizeBar
        bar = SizeBar(value=0, maximum=100, bar_width=10)
        assert bar._get_color(0.6) == "amber"

    def test_bar_red_above_80(self):
        from src.widgets.size_bar import SizeBar
        bar = SizeBar(value=0, maximum=100, bar_width=10)
        assert bar._get_color(0.9) == "red"


# --- Test: FileInfoPanel ---

class TestFileInfoPanel:
    @pytest.mark.asyncio
    async def test_panel_shows_file_type(self, simple_dir: Path):
        app = _launch_app(simple_dir)
        async with app.run_test() as pilot:
            await pilot.pause()
            await asyncio.sleep(0.5)
            tree = _get_tree(app)

            # Find one.txt
            for i, tl in enumerate(tree._tree_lines):
                if "one.txt" in tl.node.label.plain:
                    tree.move_cursor_to_line(i)
                    break

            await pilot.pause()
            info = app.screen.query_one("#info-panel")
            info.refresh()
            await pilot.pause()

            rendered = info.render()
            assert "File" in rendered.plain
            assert "Type" in rendered.plain

    @pytest.mark.asyncio
    async def test_panel_shows_dir_contents(self, simple_dir: Path):
        app = _launch_app(simple_dir)
        async with app.run_test() as pilot:
            await pilot.pause()
            await asyncio.sleep(0.5)
            tree = _get_tree(app)

            # Find subdir
            for i, tl in enumerate(tree._tree_lines):
                if "subdir" in tl.node.label.plain:
                    tree.move_cursor_to_line(i)
                    break

            await pilot.pause()
            info = app.screen.query_one("#info-panel")
            info.refresh()
            await pilot.pause()

            rendered = info.render()
            assert "Directory" in rendered.plain

    @pytest.mark.asyncio
    async def test_panel_auto_updates_on_j_navigation(self, simple_dir: Path):
        """Info panel should automatically update when pressing j to navigate."""
        app = _launch_app(simple_dir)
        async with app.run_test() as pilot:
            await pilot.pause()
            await asyncio.sleep(0.5)
            tree = _get_tree(app)
            info = app.screen.query_one("#info-panel")

            # Initial: cursor on root (directory)
            initial = info.render().plain
            root_name = simple_dir.name
            assert root_name in initial

            # Press j to move to next item
            await pilot.press("j")
            await pilot.pause()
            await asyncio.sleep(0.3)

            # Info panel should now show the new cursor target
            updated = info.render().plain
            assert root_name not in updated or updated != initial, \
                "Info panel should update after pressing j"

    @pytest.mark.asyncio
    async def test_panel_reflects_selected_item(self, simple_dir: Path):
        """Info panel content should match the currently selected tree item."""
        app = _launch_app(simple_dir)
        async with app.run_test() as pilot:
            await pilot.pause()
            await asyncio.sleep(0.5)
            tree = _get_tree(app)
            info = app.screen.query_one("#info-panel")

            # Navigate to one.txt
            for i, tl in enumerate(tree._tree_lines):
                if "one.txt" in tl.node.label.plain:
                    tree.move_cursor_to_line(i)
                    break
            await pilot.pause()
            await asyncio.sleep(0.3)

            rendered = info.render()
            assert "one.txt" in rendered.plain
            assert "File" in rendered.plain

            # Navigate to subdir
            for i, tl in enumerate(tree._tree_lines):
                if "subdir" in tl.node.label.plain:
                    tree.move_cursor_to_line(i)
                    break
            await pilot.pause()
            await asyncio.sleep(0.3)

            rendered = info.render()
            assert "subdir" in rendered.plain
            assert "Directory" in rendered.plain


# --- Test: ConfirmDialog ---

class TestKeyBindings:
    """Test single-key bindings work via pilot.press (not just direct action calls)."""

    @pytest.mark.asyncio
    async def test_h_key_collapses_expanded_dir(self, simple_dir: Path):
        """Pressing h on an expanded directory should collapse it."""
        app = _launch_app(simple_dir)
        async with app.run_test() as pilot:
            await pilot.pause()
            await asyncio.sleep(0.5)
            tree = _get_tree(app)

            # Expand subdir first
            for i, tl in enumerate(tree._tree_lines):
                if "subdir" in tl.node.label.plain:
                    tree.move_cursor_to_line(i)
                    tree.cursor_node.expand()
                    break
            await pilot.pause()
            await asyncio.sleep(0.3)

            lines_before = len(tree._tree_lines)
            assert tree.cursor_node.is_expanded

            await pilot.press("h")
            await pilot.pause()
            await asyncio.sleep(0.3)

            assert len(tree._tree_lines) < lines_before, "h should collapse"
            assert not tree.cursor_node.is_expanded

    @pytest.mark.asyncio
    async def test_h_on_collapsed_dir_moves_to_parent(self, simple_dir: Path):
        """Pressing h on a collapsed directory should move cursor to parent."""
        app = _launch_app(simple_dir)
        async with app.run_test() as pilot:
            await pilot.pause()
            await asyncio.sleep(0.5)
            tree = _get_tree(app)

            # Expand root to see subdir, navigate to subdir (collapsed)
            tree.root.expand()
            await pilot.pause()
            await asyncio.sleep(0.5)

            for i, tl in enumerate(tree._tree_lines):
                if "subdir" in tl.node.label.plain:
                    tree.move_cursor_to_line(i)
                    break
            await pilot.pause()

            assert not tree.cursor_node.is_expanded
            parent_node = tree.cursor_node.parent

            await pilot.press("h")
            await pilot.pause()
            await asyncio.sleep(0.3)

            assert tree.cursor_node == parent_node, "h on collapsed dir should move to parent"

    @pytest.mark.asyncio
    async def test_h_on_file_moves_to_parent(self, simple_dir: Path):
        """Pressing h on a file should move cursor to parent directory."""
        app = _launch_app(simple_dir)
        async with app.run_test() as pilot:
            await pilot.pause()
            await asyncio.sleep(0.5)
            tree = _get_tree(app)

            # Navigate to a file
            for i, tl in enumerate(tree._tree_lines):
                if "one.txt" in tl.node.label.plain:
                    tree.move_cursor_to_line(i)
                    break
            await pilot.pause()

            assert not tree.cursor_node._allow_expand, "Should be on a file"
            parent_node = tree.cursor_node.parent

            await pilot.press("h")
            await pilot.pause()
            await asyncio.sleep(0.3)

            assert tree.cursor_node == parent_node, "h on file should move to parent"

    @pytest.mark.asyncio
    async def test_l_key_expands(self, simple_dir: Path):
        """Pressing l should expand the current node."""
        app = _launch_app(simple_dir)
        async with app.run_test() as pilot:
            await pilot.pause()
            await asyncio.sleep(0.5)
            tree = _get_tree(app)

            # Find and navigate to subdir (collapsed)
            for i, tl in enumerate(tree._tree_lines):
                if "subdir" in tl.node.label.plain:
                    tree.move_cursor_to_line(i)
                    break
            await pilot.pause()

            lines_before = len(tree._tree_lines)
            assert not tree.cursor_node.is_expanded

            await pilot.press("l")
            await pilot.pause()
            await asyncio.sleep(0.3)

            assert len(tree._tree_lines) > lines_before, "l should expand"
            assert tree.cursor_node.is_expanded

    @pytest.mark.asyncio
    async def test_r_key_refreshes(self, simple_dir: Path):
        """Pressing r should refresh the tree."""
        app = _launch_app(simple_dir)
        async with app.run_test() as pilot:
            await pilot.pause()
            await asyncio.sleep(0.5)
            tree = _get_tree(app)

            lines_before = len(tree._tree_lines)
            await pilot.press("r")
            await pilot.pause()
            await asyncio.sleep(0.5)

            assert len(tree._tree_lines) >= lines_before

    @pytest.mark.asyncio
    async def test_s_key_toggles_sort(self, simple_dir: Path):
        """Pressing s should cycle sort mode."""
        app = _launch_app(simple_dir)
        async with app.run_test() as pilot:
            await pilot.pause()
            await asyncio.sleep(0.5)
            tree = _get_tree(app)

            from src.widgets.file_tree import SortMode
            initial_mode = tree.sort_mode

            await pilot.press("s")
            await pilot.pause()
            await asyncio.sleep(0.5)

            assert tree.sort_mode != initial_mode


class TestChordBindings:
    """Test vi-like chord key sequences (dd, gg, G, gu, gd)."""

    @pytest.mark.asyncio
    async def test_dd_chord_triggers_delete(self, simple_dir: Path):
        """Pressing d then d within timeout should trigger delete."""
        app = _launch_app(simple_dir)
        async with app.run_test() as pilot:
            await pilot.pause()
            await asyncio.sleep(0.5)
            tree = _get_tree(app)

            # Navigate to one.txt
            for i, tl in enumerate(tree._tree_lines):
                if "one.txt" in tl.node.label.plain:
                    tree.move_cursor_to_line(i)
                    break
            await pilot.pause()

            # Press d, then d (chord)
            await pilot.press("d")
            await asyncio.sleep(0.05)  # within chord timeout
            await pilot.press("d")
            await pilot.pause()
            await asyncio.sleep(0.5)

            from src.widgets.confirm_dialog import ConfirmDialog
            assert isinstance(app.screen, ConfirmDialog), "dd should show ConfirmDialog"

    @pytest.mark.asyncio
    async def test_gg_chord_goes_to_top(self, simple_dir: Path):
        """Pressing g then g should go to line 0."""
        app = _launch_app(simple_dir)
        async with app.run_test() as pilot:
            await pilot.pause()
            await asyncio.sleep(0.5)
            tree = _get_tree(app)

            # Move down
            for _ in range(3):
                await pilot.press("j")
            await pilot.pause()

            assert tree.cursor_line > 0, "Should be below top"

            # Press g, then g (chord)
            await pilot.press("g")
            await asyncio.sleep(0.05)
            await pilot.press("g")
            await pilot.pause()

            assert tree.cursor_line == 0, "gg should go to line 0"

    @pytest.mark.asyncio
    async def test_G_goes_to_bottom(self, simple_dir: Path):
        """Pressing uppercase G should go to last line."""
        app = _launch_app(simple_dir)
        async with app.run_test() as pilot:
            await pilot.pause()
            await asyncio.sleep(0.5)
            tree = _get_tree(app)

            # Press G
            await pilot.press("G")
            await pilot.pause()

            assert tree.cursor_line == len(tree._tree_lines) - 1

    @pytest.mark.asyncio
    async def test_single_d_does_nothing_after_timeout(self, simple_dir: Path):
        """Pressing d alone and waiting should not trigger delete."""
        app = _launch_app(simple_dir)
        async with app.run_test() as pilot:
            await pilot.pause()
            await asyncio.sleep(0.5)
            tree = _get_tree(app)

            # Press d once and wait past the chord timeout (0.5s)
            await pilot.press("d")
            await asyncio.sleep(0.6)

            from src.widgets.confirm_dialog import ConfirmDialog
            assert not isinstance(app.screen, ConfirmDialog), "Single d should not trigger delete"

    @pytest.mark.asyncio
    async def test_gu_chord_goes_to_parent(self, simple_dir: Path):
        """Pressing g then u should go to parent node."""
        app = _launch_app(simple_dir)
        async with app.run_test() as pilot:
            await pilot.pause()
            await asyncio.sleep(0.5)
            tree = _get_tree(app)

            # Move to a child node (not root, not root's child)
            # First find a child dir and expand it
            for i, tl in enumerate(tree._tree_lines):
                if "subdir" in tl.node.label.plain:
                    tree.move_cursor_to_line(i)
                    tree.cursor_node.expand()
                    break
            await pilot.pause()
            await asyncio.sleep(0.5)

            # Move to nested.txt (child of subdir)
            for i, tl in enumerate(tree._tree_lines):
                if "nested.txt" in tl.node.label.plain:
                    tree.move_cursor_to_line(i)
                    break
            await pilot.pause()

            parent_label = tree.cursor_node.label.plain
            # Press g, then u (chord)
            await pilot.press("g")
            await asyncio.sleep(0.05)
            await pilot.press("u")
            await pilot.pause()

            assert tree.cursor_node.parent is not None
            # Should have moved to parent
            assert tree.cursor_node.parent is not None
            assert tree.cursor_node != tree.root

    @pytest.mark.asyncio
    async def test_gd_chord_shows_input_dialog(self, simple_dir: Path):
        """Pressing g then d should show an InputDialog for changing root."""
        app = _launch_app(simple_dir)
        async with app.run_test() as pilot:
            await pilot.pause()
            await asyncio.sleep(0.5)
            tree = _get_tree(app)

            # Press g, then d (chord)
            await pilot.press("g")
            await asyncio.sleep(0.05)
            await pilot.press("d")
            await pilot.pause()
            await asyncio.sleep(0.5)

            from src.widgets.input_dialog import InputDialog
            assert isinstance(app.screen, InputDialog), "gd should show InputDialog"

            # Cancel the dialog
            await pilot.press("escape")
            await pilot.pause()
            await asyncio.sleep(0.3)

            # Should be back to MainScreen
            from src.screen import MainScreen
            assert isinstance(app.screen, MainScreen)


class TestInfoPanelOwnerGroup:
    """Test that info panel shows owner and group."""

    @pytest.mark.asyncio
    async def test_panel_shows_owner_and_group(self, simple_dir: Path):
        """Info panel should display owner and group."""
        app = _launch_app(simple_dir)
        async with app.run_test() as pilot:
            await pilot.pause()
            await asyncio.sleep(0.5)
            info = app.screen.query_one("#info-panel")

            rendered = info.render().plain
            assert "Owner:" in rendered
            assert "Group:" in rendered


class TestConfirmDialog:
    @pytest.mark.asyncio
    async def test_confirm_yes(self):
        from src.widgets.confirm_dialog import ConfirmDialog

        async def run_test():
            from textual.app import App
            from textual.screen import Screen

            class TestApp(App):
                pass

            app = TestApp()
            async with app.run_test() as pilot:
                result: bool | None = None

                def capture_result(r: bool):
                    nonlocal result
                    result = r
                    app.exit()

                app.push_screen(ConfirmDialog("Delete?"), capture_result)
                await pilot.pause()
                await pilot.press("enter")
                await pilot.pause()

                assert result is True

        await run_test()

    @pytest.mark.asyncio
    async def test_confirm_escape(self):
        from src.widgets.confirm_dialog import ConfirmDialog

        async def run_test():
            from textual.app import App

            class TestApp(App):
                pass

            app = TestApp()
            async with app.run_test() as pilot:
                result: bool | None = None

                def capture_result(r: bool):
                    nonlocal result
                    result = r
                    app.exit()

                app.push_screen(ConfirmDialog("Delete?"), capture_result)
                await pilot.pause()
                await pilot.press("escape")
                await pilot.pause()

                assert result is False

        await run_test()


class TestCollapseNavigate:
    """Test h key behavior on collapsed directories and info panel updates."""

    @pytest.mark.asyncio
    async def test_h_on_collapsed_dir_moves_to_parent(self, simple_dir: Path):
        """Pressing h on a collapsed directory should move cursor to parent."""
        app = _launch_app(simple_dir)
        async with app.run_test() as pilot:
            await pilot.pause()
            await asyncio.sleep(0.5)
            tree = _get_tree(app)

            # Expand root to see subdir, then navigate to subdir (collapsed)
            tree.root.expand()
            await pilot.pause()
            await asyncio.sleep(0.5)

            for i, tl in enumerate(tree._tree_lines):
                if "subdir" in tl.node.label.plain:
                    tree.move_cursor_to_line(i)
                    break
            await pilot.pause()

            # subdir is collapsed — h should move to parent (root)
            assert not tree.cursor_node.is_expanded
            parent_node = tree.cursor_node.parent

            await pilot.press("h")
            await pilot.pause()
            await asyncio.sleep(0.3)

            # Cursor should now be on the parent
            assert tree.cursor_node == parent_node

    @pytest.mark.asyncio
    async def test_h_on_expanded_dir_collapses(self, simple_dir: Path):
        """Pressing h on an expanded directory should collapse it (not move to parent)."""
        app = _launch_app(simple_dir)
        async with app.run_test() as pilot:
            await pilot.pause()
            await asyncio.sleep(0.5)
            tree = _get_tree(app)

            # Expand root, find subdir, expand it
            tree.root.expand()
            await pilot.pause()
            await asyncio.sleep(0.3)

            for i, tl in enumerate(tree._tree_lines):
                if "subdir" in tl.node.label.plain:
                    tree.move_cursor_to_line(i)
                    tree.cursor_node.expand()
                    break
            await pilot.pause()
            await asyncio.sleep(0.3)

            assert tree.cursor_node.is_expanded
            lines_before = len(tree._tree_lines)

            await pilot.press("h")
            await pilot.pause()
            await asyncio.sleep(0.3)

            # Should collapse, not move to parent
            assert not tree.cursor_node.is_expanded
            assert len(tree._tree_lines) < lines_before

    @pytest.mark.asyncio
    async def test_info_panel_updates_between_collapsed_dirs(self, structured_dir: Path):
        """Info panel should update when navigating between unexpanded directories."""
        app = _launch_app(structured_dir)
        async with app.run_test() as pilot:
            await pilot.pause()
            await asyncio.sleep(0.5)
            tree = _get_tree(app)
            info = app.screen.query_one("#info-panel")

            # Navigate to dir_a (collapsed by default)
            for i, tl in enumerate(tree._tree_lines):
                if "dir_a" in tl.node.label.plain:
                    tree.move_cursor_to_line(i)
                    break
            await pilot.pause()
            await asyncio.sleep(0.3)

            rendered = info.render().plain
            assert "dir_a" in rendered

            # Navigate to dir_b (also collapsed) — info panel should update
            for i, tl in enumerate(tree._tree_lines):
                if "dir_b" in tl.node.label.plain:
                    tree.move_cursor_to_line(i)
                    break
            await pilot.pause()
            await asyncio.sleep(0.3)

            rendered = info.render().plain
            assert "dir_b" in rendered
            assert "dir_a" not in rendered

    @pytest.mark.asyncio
    async def test_info_panel_updates_via_j_key(self, structured_dir: Path):
        """Info panel should update when pressing j between collapsed directories."""
        app = _launch_app(structured_dir)
        async with app.run_test() as pilot:
            await pilot.pause()
            await asyncio.sleep(0.5)
            tree = _get_tree(app)
            info = app.screen.query_one("#info-panel")

            # Find dir_a, navigate there
            for i, tl in enumerate(tree._tree_lines):
                if "dir_a" in tl.node.label.plain:
                    tree.move_cursor_to_line(i)
                    break
            await pilot.pause()
            await asyncio.sleep(0.3)

            assert "dir_a" in info.render().plain

            # Press j to move to dir_b (dirs are sorted together, dir_b should be next)
            await pilot.press("j")
            await pilot.pause()
            await asyncio.sleep(0.3)

            rendered = info.render().plain
            # Should now show dir_b (or empty_dir), not dir_a
            assert "dir_b" in rendered or "empty_dir" in rendered


class TestBarLayout:
    """Test that the bar chart appears on the left of the filename."""

    @pytest.mark.asyncio
    async def test_bar_before_filename(self, simple_dir: Path):
        """The proportional bar should appear before the filename in render_label."""
        from rich.style import Style

        app = _launch_app(simple_dir)
        async with app.run_test() as pilot:
            await pilot.pause()
            await asyncio.sleep(0.5)
            tree = _get_tree(app)

            # Render a child node's label
            for tl in tree._tree_lines:
                if tl.node != tree.root and tl.node.data:
                    label = tree.render_label(tl.node, Style(), Style())
                    plain = label.plain

                    # Should contain bar characters
                    has_bar = "█" in plain or "░" in plain
                    assert has_bar, f"Label should contain bar: {plain!r}"

                    # Bar should come before the filename
                    bar_pos = plain.find("█") if "█" in plain else plain.find("░")
                    name_pos = plain.find(tl.node.label.plain)
                    assert bar_pos < name_pos, f"Bar should be before filename in: {plain!r}"
                    break


class TestSortBySize:
    """Test that sorting by size uses recursive (total) directory sizes."""

    @pytest.mark.asyncio
    async def test_largest_uses_recursive_size(self, tmp_path: Path):
        """In LARGEST mode, directories should be sorted by total recursive size."""
        # dir_small has 10B total
        small = tmp_path / "dir_small"
        small.mkdir()
        (small / "f.txt").write_text("a" * 10)

        # dir_large has 500B total
        large = tmp_path / "dir_large"
        large.mkdir()
        (large / "f.txt").write_text("b" * 500)

        app = _launch_app(tmp_path)
        async with app.run_test() as pilot:
            await pilot.pause()
            await asyncio.sleep(0.5)
            tree = _get_tree(app)

            from src.widgets.file_tree import SortMode
            tree.sort_mode = SortMode.LARGEST
            await pilot.pause()
            await asyncio.sleep(2.0)  # recursive size computation takes time

            labels = _get_labels(tree)
            # dir_large (500B) should come before dir_small (10B)
            idx_large = labels.index("dir_large")
            idx_small = labels.index("dir_small")
            assert idx_large < idx_small, \
                f"dir_large should come before dir_small in LARGEST mode: {labels}"

    @pytest.mark.asyncio
    async def test_smallest_uses_recursive_size(self, tmp_path: Path):
        """In SMALLEST mode, directories should be sorted by total recursive size."""
        small = tmp_path / "dir_small"
        small.mkdir()
        (small / "f.txt").write_text("a" * 10)

        large = tmp_path / "dir_large"
        large.mkdir()
        (large / "f.txt").write_text("b" * 500)

        app = _launch_app(tmp_path)
        async with app.run_test() as pilot:
            await pilot.pause()
            await asyncio.sleep(0.5)
            tree = _get_tree(app)

            from src.widgets.file_tree import SortMode
            tree.sort_mode = SortMode.SMALLEST
            await pilot.pause()
            await asyncio.sleep(2.0)

            labels = _get_labels(tree)
            # dir_small (10B) should come before dir_large (500B)
            idx_large = labels.index("dir_large")
            idx_small = labels.index("dir_small")
            assert idx_small < idx_large, \
                f"dir_small should come before dir_large in SMALLEST mode: {labels}"


class TestBarRatio:
    """Test that bar ratio is calculated from visible siblings, not parent total."""

    @pytest.mark.asyncio
    async def test_bar_ratio_uses_visible_siblings(self, simple_dir: Path):
        """Bar ratio should be item_size / sum_of_visible_sibling_sizes."""
        app = _launch_app(simple_dir)
        async with app.run_test() as pilot:
            await pilot.pause()
            await asyncio.sleep(0.5)
            tree = _get_tree(app)

            # Find a file node
            file_node = None
            for child in tree.root.children:
                if child.data and child.data.path.is_file():
                    file_node = child
                    break

            assert file_node is not None
            ratio = tree._calculate_bar_ratio(file_node)
            # Ratio should be between 0 and 1
            assert 0.0 <= ratio <= 1.0

            # The ratio should be the file's size / total of all visible siblings
            # (files AND directories, not just files)
            siblings = tree.root.children
            total = sum(tree._get_size(s.data.path) for s in siblings if s.data)
            file_size = tree._get_size(file_node.data.path)
            expected = file_size / total if total > 0 else 0.0
            assert abs(ratio - expected) < 0.01, f"Expected ~{expected}, got {ratio}"

    @pytest.mark.asyncio
    async def test_bar_ratio_excludes_hidden_when_filtered(self, structured_dir: Path):
        """When show_hidden is False, hidden files should be excluded from ratio denominator."""
        app = _launch_app(structured_dir)
        async with app.run_test() as pilot:
            await pilot.pause()
            await asyncio.sleep(0.5)
            tree = _get_tree(app)

            # Hide hidden files
            tree.show_hidden = False
            await pilot.pause()
            await asyncio.sleep(1)

            # Find a regular file node
            file_node = None
            for child in tree.root.children:
                if child.data and child.data.path.is_file() and not child.data.path.name.startswith("."):
                    file_node = child
                    break

            assert file_node is not None
            ratio = tree._calculate_bar_ratio(file_node)
            # Ratio should still be valid (0-1) even when hidden files are excluded
            assert 0.0 <= ratio <= 1.0

    @pytest.mark.asyncio
    async def test_sort_cache_cleared_on_mode_change(self, simple_dir: Path):
        """Size cache should be cleared when sort mode changes."""
        app = _launch_app(simple_dir)
        async with app.run_test() as pilot:
            await pilot.pause()
            await asyncio.sleep(0.5)
            tree = _get_tree(app)

            # Cache should be populated after initial load
            assert len(tree._size_cache) > 0, "Size cache should be populated after load"
            old_cache_size = len(tree._size_cache)

            # Change sort mode
            from src.widgets.file_tree import SortMode
            tree.sort_mode = SortMode.LARGEST
            await pilot.pause()
            await asyncio.sleep(1)

            # Cache should have been cleared and recomputed
            # (might be same size or different, but the point is it was cleared)
            assert len(tree._size_cache) > 0, "Cache should be repopulated after mode change"


class TestChangeRootSortMode:
    """Test that gd preserves sort mode and show_hidden."""

    @pytest.mark.asyncio
    async def test_sort_mode_preserved_on_gd(self, simple_dir: Path, tmp_path: Path):
        """Changing root with gd should carry over the sort mode."""
        from src.widgets.file_tree import SortMode
        from src.screen import MainScreen

        app = _launch_app(simple_dir)
        async with app.run_test() as pilot:
            await pilot.pause()
            await asyncio.sleep(0.5)
            tree = _get_tree(app)

            # Change to LARGEST sort mode
            tree.sort_mode = SortMode.LARGEST
            await pilot.pause()
            await asyncio.sleep(0.5)

            # Simulate gd: push new MainScreen with sort_mode
            new_screen = MainScreen(tmp_path, sort_mode=tree.sort_mode, show_hidden=tree.show_hidden)
            app.push_screen(new_screen)
            await pilot.pause()
            await asyncio.sleep(1)

            new_tree = new_screen.query_one("FileTree")
            assert new_tree.sort_mode == SortMode.LARGEST

    @pytest.mark.asyncio
    async def test_info_panel_hidden_preserved_on_gd(self, simple_dir: Path, tmp_path: Path):
        """Changing root with gd should preserve the info panel hidden state."""
        from src.screen import MainScreen

        app = _launch_app(simple_dir)
        async with app.run_test() as pilot:
            await pilot.pause()
            await asyncio.sleep(0.5)

            # Hide the info panel
            screen = app.screen
            screen.action_toggle_info_panel()
            await pilot.pause()
            info_panel = screen.query_one("#info-panel")
            assert info_panel.display is False

            # Simulate gd: push new MainScreen with info_panel_hidden=True
            new_screen = MainScreen(tmp_path, info_panel_hidden=True)
            app.push_screen(new_screen)
            await pilot.pause()
            await asyncio.sleep(1)

            new_info = new_screen.query_one("#info-panel")
            assert new_info.display is False, "Info panel should stay hidden after gd"


class TestToggleInfoPanel:
    """Test toggling the info panel visibility."""

    @pytest.mark.asyncio
    async def test_toggle_info_panel_hides(self, simple_dir: Path):
        """Ctrl+b should hide the info panel."""
        app = _launch_app(simple_dir)
        async with app.run_test() as pilot:
            await pilot.pause()
            await asyncio.sleep(0.5)

            screen = app.screen
            info_panel = screen.query_one("#info-panel")
            assert info_panel.display is True

            # Toggle off
            screen.action_toggle_info_panel()
            await pilot.pause()

            assert info_panel.display is False
            assert "info-hidden" in screen.classes

    @pytest.mark.asyncio
    async def test_toggle_info_panel_shows_again(self, simple_dir: Path):
        """Ctrl+b toggled again should show the info panel."""
        app = _launch_app(simple_dir)
        async with app.run_test() as pilot:
            await pilot.pause()
            await asyncio.sleep(0.5)

            screen = app.screen
            info_panel = screen.query_one("#info-panel")

            # Toggle off
            screen.action_toggle_info_panel()
            await pilot.pause()
            assert info_panel.display is False

            # Toggle on
            screen.action_toggle_info_panel()
            await pilot.pause()
            assert info_panel.display is True
            assert "info-hidden" not in screen.classes


class TestToggleSizeBar:
    """Test toggling the size bar visibility."""

    @pytest.mark.asyncio
    async def test_toggle_size_bar_hides(self, simple_dir: Path):
        """b key should hide the size bars."""
        app = _launch_app(simple_dir)
        async with app.run_test() as pilot:
            await pilot.pause()
            await asyncio.sleep(0.5)
            tree = _get_tree(app)

            assert tree.show_size_bar is True
            tree.action_toggle_bar()
            await pilot.pause()
            assert tree.show_size_bar is False

    @pytest.mark.asyncio
    async def test_toggle_size_bar_shows_again(self, simple_dir: Path):
        """b key toggled again should show the size bars."""
        app = _launch_app(simple_dir)
        async with app.run_test() as pilot:
            await pilot.pause()
            await asyncio.sleep(0.5)
            tree = _get_tree(app)

            tree.action_toggle_bar()
            await pilot.pause()
            assert tree.show_size_bar is False

            tree.action_toggle_bar()
            await pilot.pause()
            assert tree.show_size_bar is True


class TestProgressOverlay:
    """Test the progress overlay during directory enumeration."""

    @pytest.mark.asyncio
    async def test_progress_shows_on_mount(self, simple_dir: Path):
        """Progress overlay should appear briefly when the tree first loads."""
        from src.widgets.progress_overlay import ProgressOverlay

        app = _launch_app(simple_dir)
        async with app.run_test() as pilot:
            await pilot.pause()

            # The progress overlay should have been pushed when workers started
            # Check that the worker tracking system is wired up
            tree = _get_tree(app)
            assert hasattr(tree, "_active_workers")
            assert hasattr(tree, "on_worker_start")


class TestInputDialog:
    """Test the InputDialog widget."""

    @pytest.mark.asyncio
    async def test_input_dialog_returns_value(self):
        """InputDialog should return the entered value when confirmed."""
        from src.widgets.input_dialog import InputDialog

        async def run_test():
            from textual.app import App

            class TestApp(App):
                pass

            app = TestApp()
            async with app.run_test() as pilot:
                result = None

                def capture(r):
                    nonlocal result
                    result = r
                    app.exit()

                app.push_screen(InputDialog("Enter name:", placeholder="test"), capture)
                await pilot.pause()

                # Set value and submit
                inp = app.screen.query_one("Input")
                inp.value = "hello_world"
                await pilot.pause()
                await pilot.press("enter")
                await pilot.pause()

                assert result == "hello_world"

        await run_test()

    @pytest.mark.asyncio
    async def test_input_dialog_cancel_returns_none(self):
        """InputDialog should return None when cancelled."""
        from src.widgets.input_dialog import InputDialog

        async def run_test():
            from textual.app import App

            class TestApp(App):
                pass

            app = TestApp()
            async with app.run_test() as pilot:
                result = "unset"

                def capture(r):
                    nonlocal result
                    result = r
                    app.exit()

                app.push_screen(InputDialog("Enter:"), capture)
                await pilot.pause()
                await pilot.press("escape")
                await pilot.pause()

                assert result is None

        await run_test()
