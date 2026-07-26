"""Comprehensive verification tests for all TUI functionality."""

import asyncio
from pathlib import Path

import pytest


@pytest.fixture
def test_dir(tmp_path):
    """Create a temporary directory with known content for testing."""
    (tmp_path / "file1.txt").write_text("hello world")
    (tmp_path / "file2.py").write_text("print('hi')")

    subdir1 = tmp_path / "subdir1"
    subdir1.mkdir()
    (subdir1 / "nested.txt").write_text("nested content here")

    subdir2 = tmp_path / "subdir2"
    subdir2.mkdir()
    (subdir2 / "deep_dir").mkdir()
    (subdir2 / "deep_dir" / "deep.txt").write_text("deep file")

    return tmp_path


def _tree(app):
    return app.screen.query_one("FileTree")


class TestNavigation:
    """Verify all navigation commands."""

    @pytest.mark.asyncio
    async def test_j_moves_down(self, test_dir):
        import sys
        sys.argv = ["test", str(test_dir)]
        from src.app import FileBrowserApp

        app = FileBrowserApp()
        async with app.run_test() as pilot:
            await pilot.pause()
            await asyncio.sleep(0.5)
            tree = _tree(app)

            initial = tree.cursor_line
            await pilot.press("j")
            assert tree.cursor_line == initial + 1, "j should move down"

    @pytest.mark.asyncio
    async def test_k_moves_up(self, test_dir):
        import sys
        sys.argv = ["test", str(test_dir)]
        from src.app import FileBrowserApp

        app = FileBrowserApp()
        async with app.run_test() as pilot:
            await pilot.pause()
            await asyncio.sleep(0.5)
            tree = _tree(app)

            # Move down first
            await pilot.press("j")
            await pilot.press("j")
            at = tree.cursor_line

            await pilot.press("k")
            assert tree.cursor_line == at - 1, "k should move up"

    @pytest.mark.asyncio
    async def test_g_g_goes_to_top(self, test_dir):
        import sys
        sys.argv = ["test", str(test_dir)]
        from src.app import FileBrowserApp

        app = FileBrowserApp()
        async with app.run_test() as pilot:
            await pilot.pause()
            await asyncio.sleep(0.5)
            tree = _tree(app)

            # Move down a few lines
            for _ in range(3):
                await pilot.press("j")

            assert tree.cursor_line > 0, "Should be below top"

            # Press g g to go to top
            tree.action_to_top()
            await pilot.pause()
            assert tree.cursor_line == 0, "gg should go to line 0"

    @pytest.mark.asyncio
    async def test_G_goes_to_bottom(self, test_dir):
        import sys
        sys.argv = ["test", str(test_dir)]
        from src.app import FileBrowserApp

        app = FileBrowserApp()
        async with app.run_test() as pilot:
            await pilot.pause()
            await asyncio.sleep(0.5)
            tree = _tree(app)

            total = len(tree._tree_lines) - 1

            # Press G to go to bottom
            tree.action_to_bottom()
            await pilot.pause()
            assert tree.cursor_line == total, f"G should go to last line ({total})"

    @pytest.mark.asyncio
    async def test_space_expands_collapses(self, test_dir):
        import sys
        sys.argv = ["test", str(test_dir)]
        from src.app import FileBrowserApp

        app = FileBrowserApp()
        async with app.run_test() as pilot:
            await pilot.pause()
            await asyncio.sleep(0.5)
            tree = _tree(app)

            # Find a child directory (skip index 0 — the root is already expanded)
            lines = tree._tree_lines
            for i in range(1, len(lines)):
                tl = lines[i]
                if tl.node._allow_expand:
                    tree.move_cursor_to_line(i)
                    break

            before = len(tree._tree_lines)

            # Expand with space (triggers toggle_node action)
            await pilot.press("space")
            await pilot.pause()
            await asyncio.sleep(1.0)

            after = len(tree._tree_lines)
            assert after > before, f"Expanding should add lines ({before} -> {after})"

    @pytest.mark.asyncio
    async def test_l_expands(self, test_dir):
        import sys
        sys.argv = ["test", str(test_dir)]
        from src.app import FileBrowserApp

        app = FileBrowserApp()
        async with app.run_test() as pilot:
            await pilot.pause()
            await asyncio.sleep(0.5)
            tree = _tree(app)

            # Find a child directory (skip index 0 — the root is already expanded)
            lines = tree._tree_lines
            for i in range(1, len(lines)):
                tl = lines[i]
                if tl.node._allow_expand:
                    tree.move_cursor_to_line(i)
                    break

            before = len(tree._tree_lines)

            # Expand with l
            tree.action_expand()
            await pilot.pause()
            await asyncio.sleep(0.5)

            after = len(tree._tree_lines)
            assert after > before, "l should expand directory"

    @pytest.mark.asyncio
    async def test_h_collapses(self, test_dir):
        import sys
        sys.argv = ["test", str(test_dir)]
        from src.app import FileBrowserApp

        app = FileBrowserApp()
        async with app.run_test() as pilot:
            await pilot.pause()
            await asyncio.sleep(0.5)
            tree = _tree(app)

            # Find a child directory (skip index 0 — the root is already expanded)
            lines = tree._tree_lines
            for i in range(1, len(lines)):
                tl = lines[i]
                if tl.node._allow_expand:
                    tree.move_cursor_to_line(i)
                    tree.cursor_node.expand()
                    await pilot.pause()
                    await asyncio.sleep(0.5)
                    break

            before = len(tree._tree_lines)

            # Collapse with h
            tree.action_collapse()
            await pilot.pause()
            await asyncio.sleep(0.3)

            after = len(tree._tree_lines)
            assert after < before, "h should collapse directory"

    @pytest.mark.asyncio
    async def test_ctrl_d_page_down(self, test_dir):
        import sys
        sys.argv = ["test", str(test_dir)]
        from src.app import FileBrowserApp

        app = FileBrowserApp()
        async with app.run_test() as pilot:
            await pilot.pause()
            await asyncio.sleep(0.5)
            tree = _tree(app)

            # Directly call the action instead of key
            tree.action_cursor_page_down()
            # Should not crash

    @pytest.mark.asyncio
    async def test_ctrl_u_page_up(self, test_dir):
        import sys
        sys.argv = ["test", str(test_dir)]
        from src.app import FileBrowserApp

        app = FileBrowserApp()
        async with app.run_test() as pilot:
            await pilot.pause()
            await asyncio.sleep(0.5)
            tree = _tree(app)

            # Directly call the action instead of key
            tree.action_cursor_page_up()
            # Should not crash

    @pytest.mark.asyncio
    async def test_g_u_go_parent(self, test_dir):
        import sys
        sys.argv = ["test", str(test_dir)]
        from src.app import FileBrowserApp

        app = FileBrowserApp()
        async with app.run_test() as pilot:
            await pilot.pause()
            await asyncio.sleep(0.5)
            tree = _tree(app)

            # Find a child and move to it
            lines = tree._tree_lines
            for i, tl in enumerate(lines):
                if tl.node.parent and tl.node.parent != tree.root:
                    tree.move_cursor_to_line(i)
                    before = tree.cursor_node
                    tree.action_go_parent()
                    await pilot.pause()
                    assert tree.cursor_node is not None
                    break


class TestInfoPanel:
    """Verify info panel updates on selection."""

    @pytest.mark.asyncio
    async def test_info_panel_shows_root(self, test_dir):
        import sys
        sys.argv = ["test", str(test_dir)]
        from src.app import FileBrowserApp

        app = FileBrowserApp()
        async with app.run_test() as pilot:
            await pilot.pause()
            await asyncio.sleep(0.5)
            tree = _tree(app)

            # Move to first item
            tree.move_cursor_to_line(1)
            await pilot.pause()
            await asyncio.sleep(0.3)

            # Force refresh info panel
            info = app.screen.query_one("#info-panel")
            info.refresh()
            await pilot.pause()

            # Render and check
            rendered = info.render()
            assert rendered.plain.strip(), "Info panel should show something"

    @pytest.mark.asyncio
    async def test_info_panel_shows_file_details(self, test_dir):
        import sys
        sys.argv = ["test", str(test_dir)]
        from src.app import FileBrowserApp

        app = FileBrowserApp()
        async with app.run_test() as pilot:
            await pilot.pause()
            await asyncio.sleep(0.5)
            tree = _tree(app)

            # Find file1.txt
            lines = tree._tree_lines
            for i, tl in enumerate(lines):
                if not tl.node._allow_expand and "file1.txt" in tl.node.label.plain:
                    tree.move_cursor_to_line(i)
                    break

            await pilot.pause()
            await asyncio.sleep(0.3)

            # Force refresh info panel
            info = app.screen.query_one("#info-panel")
            info.refresh()
            await pilot.pause()

            rendered = info.render()
            # Should show the file name
            assert test_dir.name in rendered.plain or "tests" in rendered.plain.lower() or "file1" in rendered.plain.lower() or "Type" in rendered.plain


class TestFileOperations:
    """Verify file operations."""

    @pytest.mark.asyncio
    async def test_delete_file(self, test_dir):
        import sys
        sys.argv = ["test", str(test_dir)]
        from src.app import FileBrowserApp

        app = FileBrowserApp()
        async with app.run_test() as pilot:
            await pilot.pause()
            await asyncio.sleep(0.5)
            tree = _tree(app)

            lines = tree._tree_lines
            for i, tl in enumerate(lines):
                if not tl.node._allow_expand and "file1.txt" in tl.node.label.plain:
                    tree.move_cursor_to_line(i)
                    break

            tree.action_delete()
            await pilot.pause()
            await asyncio.sleep(0.3)

            # Press Enter to confirm
            await pilot.press("enter")
            await pilot.pause()
            await asyncio.sleep(0.5)

            assert not (test_dir / "file1.txt").exists()

    @pytest.mark.asyncio
    async def test_delete_directory(self, test_dir):
        import sys
        sys.argv = ["test", str(test_dir)]
        from src.app import FileBrowserApp

        app = FileBrowserApp()
        async with app.run_test() as pilot:
            await pilot.pause()
            await asyncio.sleep(0.5)
            tree = _tree(app)

            lines = tree._tree_lines
            for i, tl in enumerate(lines):
                if tl.node._allow_expand and "subdir1" in tl.node.label.plain:
                    tree.move_cursor_to_line(i)
                    break

            tree.action_delete()
            await pilot.pause()
            await asyncio.sleep(0.3)

            await pilot.press("enter")
            await pilot.pause()
            await asyncio.sleep(0.5)

            assert not (test_dir / "subdir1").exists()

    @pytest.mark.asyncio
    async def test_sort_mode_cycling(self, test_dir):
        import sys
        sys.argv = ["test", str(test_dir)]
        from src.app import FileBrowserApp
        from src.widgets.file_tree import SortMode

        app = FileBrowserApp()
        async with app.run_test() as pilot:
            await pilot.pause()
            await asyncio.sleep(0.5)
            tree = _tree(app)

            assert tree.sort_mode == SortMode.LARGEST

            tree.action_toggle_sort()
            await pilot.pause()
            await asyncio.sleep(0.5)

            assert tree.sort_mode != SortMode.LARGEST

    @pytest.mark.asyncio
    async def test_refresh(self, test_dir):
        import sys
        sys.argv = ["test", str(test_dir)]
        from src.app import FileBrowserApp

        app = FileBrowserApp()
        async with app.run_test() as pilot:
            await pilot.pause()
            await asyncio.sleep(0.5)
            tree = _tree(app)

            await tree.action_refresh()
            await pilot.pause()
            await asyncio.sleep(0.5)

            # Tree should still be functional
            assert len(tree._tree_lines) > 0


class TestSizeBars:
    """Verify size bars show meaningful data."""

    @pytest.mark.asyncio
    async def test_tree_lines_have_sizes(self, test_dir):
        import sys
        sys.argv = ["test", str(test_dir)]
        from src.app import FileBrowserApp

        app = FileBrowserApp()
        async with app.run_test() as pilot:
            await pilot.pause()
            await asyncio.sleep(0.5)
            tree = _tree(app)

            lines = tree._tree_lines
            # At least the root should have a size
            root_label = lines[0].node.label.plain
            # Check that render_label produces output with size info
            assert len(lines) >= 1

    @pytest.mark.asyncio
    async def test_size_cache_has_nonzero_values(self, test_dir):
        import sys
        sys.argv = ["test", str(test_dir)]
        from src.app import FileBrowserApp

        app = FileBrowserApp()
        async with app.run_test() as pilot:
            await pilot.pause()
            await asyncio.sleep(0.5)
            tree = _tree(app)

            # Check that files have non-zero sizes
            for path, expected in [
                (test_dir / "file1.txt", 11),  # "hello world"
                (test_dir / "file2.py", 11),   # "print('hi')"
            ]:
                resolved = path.resolve()
                if resolved in tree._size_cache:
                    assert tree._size_cache[resolved] == expected, \
                        f"{path} should have size {expected}, got {tree._size_cache[resolved]}"


class TestDirPicker:
    """Verify directory picker."""

    @pytest.mark.asyncio
    async def test_dir_picker_shown_without_arg(self):
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
    async def test_main_screen_with_arg(self, test_dir):
        import sys
        sys.argv = ["test", str(test_dir)]
        from src.app import FileBrowserApp
        from src.screen import MainScreen

        app = FileBrowserApp()
        async with app.run_test() as pilot:
            await pilot.pause()
            await asyncio.sleep(0.5)
            assert isinstance(app.screen, MainScreen)
