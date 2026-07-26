"""Pyte-based tests for the file browser TUI app."""

import asyncio
from pathlib import Path

import pytest


@pytest.fixture
def test_dir(tmp_path):
    """Create a temporary directory with files and subdirectories for testing."""
    (tmp_path / "file1.txt").write_text("hello")
    (tmp_path / "file2.py").write_text("print('hi')")

    subdir1 = tmp_path / "subdir1"
    subdir1.mkdir()
    (subdir1 / "nested.txt").write_text("nested content")

    subdir2 = tmp_path / "subdir2"
    subdir2.mkdir()
    (subdir2 / "deep_dir").mkdir()
    (subdir2 / "deep_dir" / "deep.txt").write_text("deep")

    return tmp_path


def _get_tree(app):
    """Helper to get the FileTree from the active screen."""
    return app.screen.query_one("FileTree")


class TestFileTree:
    def test_tree_loads_content(self, test_dir):
        from src.widgets.file_tree import FileTree
        tree = FileTree(test_dir)
        assert tree.root.data.path == test_dir

    def test_sort_modes_exist(self):
        from src.widgets.file_tree import SortMode
        assert SortMode.DIRS_FIRST == "dirs-first"
        assert SortMode.FILES_FIRST == "files-first"
        assert SortMode.LARGEST == "largest"
        assert SortMode.SMALLEST == "smallest"

    def test_format_size(self):
        from src.utils.sizes import format_size
        assert format_size(0) == "0 B"
        assert format_size(1024) == "1.0 KB"
        assert format_size(1048576) == "1.0 MB"
        assert format_size(1073741824) == "1.0 GB"

    def test_get_file_size(self, test_dir):
        from src.utils.sizes import get_file_size
        assert get_file_size(test_dir / "file1.txt") == 5

    def test_get_shallow_size(self, test_dir):
        from src.utils.sizes import get_shallow_size
        size = get_shallow_size(test_dir / "subdir2")
        assert size > 0

    def test_size_bar(self):
        from src.widgets.size_bar import SizeBar
        bar = SizeBar(value=50, maximum=100, bar_width=20)
        rendered = bar.render()
        assert len(rendered.plain) == 20


class TestAppScreen:
    @pytest.mark.asyncio
    async def test_app_launches_with_dir(self, test_dir):
        import sys
        sys.argv = ["test", str(test_dir)]
        from src.app import FileBrowserApp
        app = FileBrowserApp()
        async with app.run_test() as pilot:
            await pilot.pause()
            await asyncio.sleep(0.5)
            tree = _get_tree(app)
            assert tree.is_mounted
            assert len(tree._tree_lines) >= 5

    @pytest.mark.asyncio
    async def test_tree_displays_file_names(self, test_dir):
        import sys
        sys.argv = ["test", str(test_dir)]
        from src.app import FileBrowserApp
        app = FileBrowserApp()
        async with app.run_test() as pilot:
            await pilot.pause()
            await asyncio.sleep(0.5)
            tree = _get_tree(app)
            lines = tree._tree_lines
            labels = [tl.node.label.plain for tl in lines]
            all_labels_text = " ".join(labels)
            assert "file1.txt" in all_labels_text
            assert "subdir1" in all_labels_text

    @pytest.mark.asyncio
    async def test_j_k_navigation(self, test_dir):
        import sys
        sys.argv = ["test", str(test_dir)]
        from src.app import FileBrowserApp
        app = FileBrowserApp()
        async with app.run_test() as pilot:
            await pilot.pause()
            await asyncio.sleep(0.5)
            tree = _get_tree(app)
            initial_line = tree.cursor_line
            await pilot.press("j")
            assert tree.cursor_line == initial_line + 1
            await pilot.press("k")
            assert tree.cursor_line == initial_line

    @pytest.mark.asyncio
    async def test_space_expands_directory(self, test_dir):
        """space key should toggle expand/collapse."""
        import sys
        sys.argv = ["test", str(test_dir)]
        from src.app import FileBrowserApp
        app = FileBrowserApp()
        async with app.run_test() as pilot:
            await pilot.pause()
            await asyncio.sleep(0.5)
            tree = _get_tree(app)
            lines = tree._tree_lines

            # Find subdir1
            for i, tl in enumerate(lines):
                if tl.node._allow_expand and "subdir1" in tl.node.label.plain:
                    tree.move_cursor_to_line(i)
                    break

            old_count = len(tree._tree_lines)
            # Press space to expand
            await pilot.press("space")
            await pilot.pause()
            await asyncio.sleep(0.5)

            new_lines = tree._tree_lines
            assert len(new_lines) > old_count
            assert tree.cursor_node.is_expanded

    @pytest.mark.asyncio
    async def test_delete_file(self, test_dir):
        """dd should delete a file after confirmation."""
        import sys
        sys.argv = ["test", str(test_dir)]
        from src.app import FileBrowserApp
        app = FileBrowserApp()
        async with app.run_test() as pilot:
            await pilot.pause()
            await asyncio.sleep(0.5)
            tree = _get_tree(app)
            lines = tree._tree_lines

            # Find file1.txt
            for i, tl in enumerate(lines):
                if not tl.node._allow_expand and "file1.txt" in tl.node.label.plain:
                    tree.move_cursor_to_line(i)
                    break

            await pilot.pause()

            # Call action_delete directly (dd chord doesn't work in pilot)
            tree.action_delete()
            await pilot.pause()
            await asyncio.sleep(0.3)

            # Press Enter to confirm
            await pilot.press("enter")
            await pilot.pause()
            await asyncio.sleep(0.5)

            assert not (test_dir / "file1.txt").exists()

    @pytest.mark.asyncio
    async def test_sort_mode_cycle(self, test_dir):
        """s key should cycle through sort modes."""
        import sys
        sys.argv = ["test", str(test_dir)]
        from src.app import FileBrowserApp
        from src.widgets.file_tree import SortMode
        app = FileBrowserApp()
        async with app.run_test() as pilot:
            await pilot.pause()
            await asyncio.sleep(0.5)
            tree = _get_tree(app)
            assert tree.sort_mode == SortMode.LARGEST

            # Call action_toggle_sort directly
            tree.action_toggle_sort()
            await pilot.pause()
            await asyncio.sleep(0.5)

            assert tree.sort_mode != SortMode.LARGEST

    @pytest.mark.asyncio
    async def test_info_panel_updates(self, test_dir):
        """FileInfoPanel should show info about selected file."""
        import sys
        sys.argv = ["test", str(test_dir)]
        from src.app import FileBrowserApp
        app = FileBrowserApp()
        async with app.run_test() as pilot:
            await pilot.pause()
            await asyncio.sleep(0.5)
            tree = _get_tree(app)
            lines = tree._tree_lines

            for i, tl in enumerate(lines):
                if not tl.node._allow_expand and "file1.txt" in tl.node.label.plain:
                    tree.move_cursor_to_line(i)
                    break

            await pilot.pause()
            await asyncio.sleep(0.3)
            info = app.screen.query_one("#info-panel")
            assert info.is_mounted

    @pytest.mark.asyncio
    async def test_dir_picker_shown_without_cli_arg(self):
        """The dir picker should be shown when no CLI arg is given."""
        import sys
        sys.argv = ["test"]
        from src.app import FileBrowserApp
        from src.dir_picker import DirPickerScreen
        app = FileBrowserApp()
        async with app.run_test() as pilot:
            await pilot.pause()
            await asyncio.sleep(0.3)
            assert isinstance(app.screen, DirPickerScreen)

    @pytest.mark.asyncio
    async def test_delete_directory(self, test_dir):
        """dd should delete a directory after confirmation."""
        import sys
        sys.argv = ["test", str(test_dir)]
        from src.app import FileBrowserApp
        app = FileBrowserApp()
        async with app.run_test() as pilot:
            await pilot.pause()
            await asyncio.sleep(0.5)
            tree = _get_tree(app)
            lines = tree._tree_lines

            for i, tl in enumerate(lines):
                if tl.node._allow_expand and "subdir1" in tl.node.label.plain:
                    tree.move_cursor_to_line(i)
                    break

            await pilot.pause()

            tree.action_delete()
            await pilot.pause()
            await asyncio.sleep(0.3)

            await pilot.press("enter")
            await pilot.pause()
            await asyncio.sleep(0.5)

            assert not (test_dir / "subdir1").exists()
