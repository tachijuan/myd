"""FileTree widget - a DirectoryTree with vi-like navigation, size display, and sort modes."""

from __future__ import annotations

import asyncio
import os
from enum import StrEnum
from pathlib import Path
from typing import ClassVar, Iterable

from rich.style import Style
from rich.text import Text
from textual import events
from textual.app import ComposeResult
from textual.binding import Binding
from textual.reactive import reactive
from textual import work
from textual.worker import get_current_worker
from textual.widgets import DirectoryTree
from textual.widgets._directory_tree import DirEntry
from textual.widgets._tree import TreeNode

# Import our size utilities
from src.utils.sizes import format_size, get_size


class SortMode(StrEnum):
    """Sort order modes for the file tree."""

    DIRS_FIRST = "dirs-first"
    FILES_FIRST = "files-first"
    LARGEST = "largest"
    SMALLEST = "smallest"


class FileTree(DirectoryTree):
    """A directory tree with vi-like navigation, size bars, and sort modes.

    Extends DirectoryTree to add:
    - Vi key bindings (j/k, h/l, gg/G, dd, etc.)
    - Inline size display with proportional bars
    - Configurable sort order
    - File/directory deletion
    """

    COMPONENT_CLASSES: ClassVar[set[str]] = {
        "directory-tree--extension",
        "directory-tree--file",
        "directory-tree--folder",
        "directory-tree--hidden",
        "file-tree--size-text",
        "file-tree--size-bar",
    }

    BINDINGS = [
        # Navigation
        Binding("j", "cursor_down", "Down", show=True),
        Binding("k", "cursor_up", "Up", show=True),
        Binding("h", "collapse", "Collapse", show=True),
        Binding("l", "expand", "Expand", show=True),
        Binding("g g", "to_top", "Top", show=True),
        Binding("G", "to_bottom", "Bottom", show=True),
        Binding("ctrl+d", "cursor_page_down", "Page Dn", show=True),
        Binding("ctrl+u", "cursor_page_up", "Page Up", show=True),
        Binding("g d", "change_root", "New Root", show=True),
        Binding("g u", "go_parent", "Parent", show=True),
        # File operations
        Binding("d d", "delete", "Delete", show=True),
        Binding("r", "refresh", "Refresh", show=True),
        Binding("R", "rename", "Rename", show=True),
        # Sort and view
        Binding("s", "toggle_sort", "Sort", show=True),
        Binding("H", "toggle_hidden", "Hidden", show=True),
        Binding("b", "toggle_bar", "Bars", show=True),
        Binding("0", "collapse_all", "Collapse All", show=True),
        Binding("*", "expand_all", "Expand All", show=True),
        # Search
        Binding("/", "search", "Search", show=True),
    ]

    # Reactive attributes
    sort_mode = reactive(SortMode.LARGEST, init=False)
    show_hidden = reactive(True, init=False)
    show_size_bar = reactive(True, init=False)

    # Size cache: path -> size_in_bytes
    _size_cache: dict[Path, int]
    _active_workers: int
    _progress_dismiss: object  # callback to dismiss progress overlay

    def __init__(self, path: str | Path, *, name: str | None = None, id: str | None = None,
                 classes: str | None = None, disabled: bool = False) -> None:
        """Initialize the FileTree.

        Args:
            path: Root directory path.
            name: Widget name.
            id: Widget ID.
            classes: CSS classes.
            disabled: Whether the widget is disabled.
        """
        self._size_cache = {}
        self._active_workers = 0
        self._progress_dismiss = None
        self._progress_timer = None
        super().__init__(path, name=name, id=id, classes=classes, disabled=disabled)

    def on_mount(self) -> None:
        """Initialize the size cache and expand root."""
        self._compute_sizes_for_parent(self.root)

    # Debounce: only show the progress overlay if enumeration takes longer
    # than this threshold (seconds). Small directories finish instantly and
    # the overlay would never be visually rendered if pushed and dismissed
    # in the same event cycle.
    PROGRESS_DELAY = 0.5

    def on_worker_start(self) -> None:
        """Debounce-show the progress overlay when a worker starts."""
        self._active_workers += 1
        if self._active_workers == 1 and self.is_mounted:
            # First worker starting — arm a delay timer instead of pushing
            # the overlay immediately. If all workers finish before the
            # delay expires the overlay is never shown (avoids flash-on-then-
            # -off for tiny directories).
            self._progress_timer = self.set_timer(
                self.PROGRESS_DELAY,
                self._show_progress,
            )

    def on_worker_complete(self) -> None:
        """Cancel the progress timer when all workers finish."""
        self._active_workers = max(0, self._active_workers - 1)
        if self._active_workers == 0 and self.is_mounted:
            # Worker finished — cancel the pending timer so the overlay is
            # never pushed (or dismiss it if it's already visible).
            if self._progress_timer is not None:
                self._progress_timer.cancel()
                self._progress_timer = None
            self._dismiss_progress(None)

    def _show_progress(self) -> None:
        """Actually push the progress overlay after the debounce delay."""
        self._progress_timer = None
        if not self.is_mounted or self._active_workers == 0:
            return
        from src.widgets.progress_overlay import ProgressOverlay
        self.app.push_screen(
            ProgressOverlay(),
            lambda result: self._dismiss_progress(result),
        )

    def _dismiss_progress(self, result: None) -> None:
        """Dismiss the progress overlay if it's currently shown."""
        from src.widgets.progress_overlay import ProgressOverlay
        for screen in self.app.screen_stack:
            if isinstance(screen, ProgressOverlay):
                screen.dismiss(None)
                return

    # Chord detection state
    _chord_key: str = ""
    _chord_timer: object  # timer handle

    CHORD_TIMEOUT = 0.5  # seconds to wait for second key in a chord

    def watch_cursor_line(self, previous_line: int, line: int) -> None:
        """Refresh the info panel when the cursor moves.

        Override of Tree.watch_cursor_line to ensure the sibling
        FileInfoPanel updates when navigation happens. The standard
        NodeHighlighted message doesn't reliably reach sibling widgets,
        so we refresh the panel directly here.
        """
        # Call parent implementation (posts NodeHighlighted, refreshes nodes)
        super().watch_cursor_line(previous_line, line)

        # Defer the info panel refresh to the next tick so _tree_lines
        # has been fully updated and cursor_node resolves correctly.
        if self.is_mounted and previous_line != line:
            self.call_after_refresh(self._refresh_info_panel)

    async def _on_key(self, event: events.Key) -> None:
        """Handle chord key sequences and forward to normal key processing.

        Textual 8.2.x stores chord bindings like "d d" in key_to_bindings,
        but key events are always single characters, so these bindings are
        never matched. We implement chord detection manually here.

        This overrides Widget._on_key to intercept key events before
        the binding system processes them.
        """
        key = event.key

        # Check for chord sequences
        if self._chord_key:
            combined = self._chord_key + key
            chord_actions = {
                "dd": "action_delete",
                "gg": "action_to_top",
                "gu": "action_go_parent",
                "gd": "action_change_root",
            }
            if combined in chord_actions:
                action_name = chord_actions[combined]
                if hasattr(self, action_name):
                    result = getattr(self, action_name)()
                    if asyncio.iscoroutine(result):
                        await result
                self._chord_key = ""
                return  # handled — don't forward

            # Unknown chord combo, cancel
            self._chord_key = ""

        # Check if this key starts a chord sequence
        if key in ("d", "g"):
            self._chord_key = key
            self.set_timer(self.CHORD_TIMEOUT, self._clear_chord)
            return  # consumed — wait for second key

        # Handle standalone "G" (uppercase, go to bottom)
        if key == "G":
            self.action_to_bottom()
            return

        # Forward to normal key processing (bindings, dispatch_key)
        await super()._on_key(event)

    def _clear_chord(self) -> None:
        """Clear pending chord state after timeout."""
        self._chord_key = ""

    def _compute_sizes_for_parent(self, node: TreeNode[DirEntry]) -> None:
        """Pre-compute sizes for children of a node.

        For files, use stat(). For directories, use shallow size by default,
        or recursive size when sorting by size (LARGEST/SMALLEST).

        Args:
            node: The parent node whose children to size.
        """
        from src.utils.sizes import get_dir_size, get_shallow_size

        if node.data is None:
            return
        path = node.data.path.expanduser().resolve()
        if not path.is_dir():
            return

        use_recursive = self.sort_mode in (SortMode.LARGEST, SortMode.SMALLEST)

        try:
            for entry in path.iterdir():
                resolved = entry.resolve()
                try:
                    if entry.is_file():
                        self._size_cache[resolved] = entry.stat().st_size
                    elif entry.is_dir():
                        if use_recursive:
                            self._size_cache[resolved] = get_dir_size(entry)
                        else:
                            self._size_cache[resolved] = get_shallow_size(entry)
                except OSError:
                    self._size_cache[resolved] = 0
        except OSError:
            pass

    def _get_size(self, path: Path) -> int:
        """Get cached size or compute it.

        Args:
            path: File or directory path.

        Returns:
            Size in bytes.
        """
        resolved = path.resolve()
        if resolved in self._size_cache:
            return self._size_cache[resolved]

        use_recursive = self.sort_mode in (SortMode.LARGEST, SortMode.SMALLEST)
        size = get_size(path, recursive=use_recursive)
        self._size_cache[resolved] = size
        return size

    def _get_parent_size(self, node: TreeNode[DirEntry]) -> int:
        """Get the total size of a node's parent directory.

        Args:
            node: The child node.

        Returns:
            Parent directory size in bytes, or 0 if unavailable.
        """
        if node.data is None or node.parent is None:
            return 0
        if node.parent.data is None:
            return 0

        parent_path = node.parent.data.path.expanduser().resolve()
        return self._get_size(parent_path)

    def _calculate_bar_ratio(self, node: TreeNode[DirEntry]) -> float:
        """Calculate the ratio of an item's size relative to its visible siblings.

        The denominator is the total size of all visible siblings (respecting
        show_hidden), so the bar reflects the item's share of the current view.

        Args:
            node: The tree node to calculate the ratio for.

        Returns:
            Ratio from 0.0 to 1.0, or 0.0 if no siblings exist.
        """
        if node.parent is None or node.parent.data is None:
            return 0.0
        if node.data is None:
            return 0.0

        # Use cached sizes — _load_directory populates the cache with the
        # correct strategy (shallow or recursive) for the current sort mode,
        # so _get_size returns the right value without recomputing.
        # This is critical: calling get_dir_size() here would block the UI
        # thread with a synchronous recursive disk walk during every render.

        # Compute total size of all visible siblings
        total_size = 0
        for sibling in node.parent.children:
            if sibling.data is None:
                continue
            sp = sibling.data.path.expanduser().resolve()
            if not self.show_hidden and sibling.data.path.name.startswith("."):
                continue
            try:
                total_size += self._get_size(sp)
            except OSError:
                pass

        if total_size == 0:
            return 0.0

        # This node's size
        my_path = node.data.path.expanduser().resolve()
        try:
            my_size = self._get_size(my_path)
        except OSError:
            return 0.0

        return my_size / total_size

    def _make_inline_bar(self, ratio: float, bar_width: int = 12) -> Text:
        """Create an inline proportional bar.

        Args:
            ratio: Proportion from 0.0 to 1.0.
            bar_width: Number of characters for the bar.

        Returns:
            Text object with the bar.
        """
        ratio = min(max(ratio, 0.0), 1.0)

        if ratio < 0.5:
            color = "green"
        elif ratio < 0.8:
            color = "amber"
        else:
            color = "red"

        filled = int(ratio * bar_width)
        empty = bar_width - filled

        text = Text()
        text.append("[", style="ansi_bright_black")
        text.append("█" * filled, style=color)
        text.append("░" * empty, style="ansi_bright_black")
        text.append("]", style="ansi_bright_black")

        return text

    def _sort_key(self, path: Path) -> tuple[int, ...]:
        """Generate a sort key for a path based on current sort mode.

        Args:
            path: The path to sort.

        Returns:
            A tuple suitable for sorting.
        """
        is_dir = path.is_dir()
        name = path.name.lower()
        size = self._get_size(path)

        match self.sort_mode:
            case SortMode.DIRS_FIRST:
                # Directories first (False=0), then files (True=1), alphabetical within
                return (not is_dir, 0, name)
            case SortMode.FILES_FIRST:
                # Files first, then directories, alphabetical within
                return (is_dir, 0, name)
            case SortMode.LARGEST:
                # Largest first (negate size for descending), then alphabetical
                return (-size, name)
            case SortMode.SMALLEST:
                # Smallest first, then alphabetical
                return (size, name)

        # Default: dirs first
        return (not is_dir, 0, name)

    @work(thread=True, exit_on_error=False)
    def _load_directory(self, node: TreeNode[DirEntry]) -> list[Path]:
        """Load directory contents with custom sorting.

        Override of DirectoryTree._load_directory to support custom sort modes.

        Args:
            node: The tree node to load.

        Returns:
            Sorted list of paths.
        """
        from textual.worker import get_current_worker

        from src.utils.sizes import get_dir_size, get_shallow_size

        assert node.data is not None
        path = node.data.path
        path = path.expanduser().resolve()

        worker = get_current_worker()
        paths = list(self._directory_content(path, worker))
        paths = self.filter_paths(paths)

        # Use recursive size when sorting by size; shallow otherwise
        use_recursive = self.sort_mode in (SortMode.LARGEST, SortMode.SMALLEST)

        # Compute sizes for sorting and display
        for p in paths:
            resolved = p.resolve()
            if resolved not in self._size_cache:
                try:
                    if p.is_file():
                        self._size_cache[resolved] = p.stat().st_size
                    elif p.is_dir():
                        if use_recursive:
                            self._size_cache[resolved] = get_dir_size(p)
                        else:
                            self._size_cache[resolved] = get_shallow_size(p)
                except OSError:
                    self._size_cache[resolved] = 0

        # Sort using custom key
        sorted_paths = sorted(paths, key=lambda p: self._sort_key(p))

        # Pre-compute sizes for the next level
        self._compute_sizes_for_parent(node)

        return sorted_paths

    def filter_paths(self, paths: Iterable[Path]) -> list[Path]:
        """Filter paths based on show_hidden setting.

        Args:
            paths: Iterable of paths to filter.

        Returns:
            Filtered list of paths.
        """
        if self.show_hidden:
            return list(paths)
        return [p for p in paths if not p.name.startswith(".")]

    def render_label(
        self, node: TreeNode[DirEntry], base_style: Style, style: Style
    ) -> Text:
        """Render a tree node label with size bar on the left and size on the right.

        Layout: [bar] icon filename  size

        The bar is on the left so all bars align vertically. The size appears
        after the filename for reference. The percentage is omitted to reduce
        visual noise.

        Args:
            node: The tree node to render.
            base_style: Base style from the tree.
            style: Current style.

        Returns:
            Rich Text object with the label.
        """
        node_label = node._label.copy()
        node_label.stylize(style)

        if not self.is_mounted:
            return node_label

        # Compute size info
        ratio = 0.0
        size_str = ""
        if node.data and node.data.path:
            path = node.data.path.expanduser().resolve()
            try:
                size = self._get_size(path)
                size_str = format_size(size)
                ratio = self._calculate_bar_ratio(node)
            except (OSError, AttributeError):
                size_str = ""

        # Build left-side bar (skip root which has no parent)
        text = Text()
        if self.show_size_bar and size_str:
            bar = self._make_inline_bar(ratio)
            bar.stylize(self.get_component_rich_style(
                "file-tree--size-bar", partial=True
            ))
            text.append(bar)
            text.append(" ")

        # Icon and file/folder styling
        if node._allow_expand:
            icon = self.ICON_NODE_EXPANDED if node.is_expanded else self.ICON_NODE
            icon_text = Text(icon, style=base_style + Style(color="blue"))
            node_label.stylize_before(
                self.get_component_rich_style("directory-tree--folder", partial=True)
            )
        else:
            icon_text = Text(self.ICON_FILE, style=base_style)
            node_label.stylize_before(
                self.get_component_rich_style("directory-tree--file", partial=True),
            )
            node_label.highlight_regex(
                r"\..+$",
                self.get_component_rich_style("directory-tree--extension", partial=True),
            )

        # Hidden file styling
        if node_label.plain.startswith("."):
            node_label.stylize_before(
                self.get_component_rich_style("directory-tree--hidden", partial=True)
            )

        text.append(icon_text)
        text.append(" ")
        text.append(node_label)

        # Size text on the right
        if size_str:
            size_text = Text(f"  {size_str}")
            size_text.stylize(self.get_component_rich_style(
                "file-tree--size-text", partial=True
            ))
            text.append(size_text)

        return text

    # --- Vi-like action handlers ---

    def action_to_top(self) -> None:
        """Move cursor to the first line."""
        self.move_cursor_to_line(0)

    def action_to_bottom(self) -> None:
        """Move cursor to the last line."""
        self.move_cursor_to_line(len(self._tree_lines) - 1)

    def action_go_parent(self) -> None:
        """Move cursor to the parent directory."""
        node = self.cursor_node
        if node and node.parent:
            self.move_cursor(node.parent)

    def action_change_root(self) -> None:
        """Prompt for a new root directory path and navigate to it."""
        from src.widgets.input_dialog import InputDialog

        self.focus()
        self.app.push_screen(
            InputDialog(
                message="Change root directory:",
                placeholder="Enter path...",
                title="New Root",
            ),
            self._handle_change_root,
        )

    def _handle_change_root(self, result: str | None) -> None:
        """Handle the result of the change-root input dialog."""
        if not result or not result.strip():
            return

        path = Path(result.strip()).expanduser().resolve()

        if not path.exists():
            self.app.notify(f"Path does not exist: {path}", title="✗")
            return

        if not path.is_dir():
            self.app.notify(f"Not a directory: {path}", title="⚠")
            return

        # Check if info panel is hidden on the current screen
        info_hidden = False
        try:
            info_panel = self.screen.query_one("#info-panel")
            info_hidden = not info_panel.display
        except Exception:
            pass

        from src.screen import MainScreen
        self.app.push_screen(
            MainScreen(
                path,
                sort_mode=self.sort_mode,
                show_hidden=self.show_hidden,
                info_panel_hidden=info_hidden,
            )
        )

    def _refresh_info_panel(self) -> None:
        """Refresh the info panel to reflect the current cursor position."""
        try:
            info = self.screen.query_one("#info-panel")
            info.refresh()
        except Exception:
            pass

    def action_collapse(self) -> None:
        """Collapse the current node, or move to parent if already collapsed.

        For directories: collapse if expanded, move to parent if collapsed.
        For files: move to parent directory.
        """
        node = self.cursor_node
        if node is None or node.data is None:
            return
        if node._allow_expand:
            # It's a directory
            if node.is_expanded:
                node.collapse()
            elif node.parent is not None:
                # Already collapsed — move cursor to parent
                self.move_cursor(node.parent)
        elif node.parent is not None:
            # It's a file — move to parent directory
            self.move_cursor(node.parent)

    def action_expand(self) -> None:
        """Expand the current node."""
        node = self.cursor_node
        if node and node.data:
            node.expand()

    def action_cursor_page_down(self) -> None:
        """Scroll down half a page."""
        self.scroll_down(animate=False)

    def action_cursor_page_up(self) -> None:
        """Scroll up half a page."""
        self.scroll_up(animate=False)

    def action_toggle_sort(self) -> None:
        """Cycle through sort modes."""
        modes = list(SortMode)
        current_index = modes.index(self.sort_mode)
        next_index = (current_index + 1) % len(modes)
        self.sort_mode = modes[next_index]

        # Notify the user of the new sort mode
        self.app.notify(f"Sort mode: {self.sort_mode.value}")
        # reload() is triggered by watch_sort_mode — no need to call it here

    def watch_sort_mode(self, old_mode: SortMode, new_mode: SortMode) -> None:
        """Watch for sort mode changes, clear cache, and reload."""
        # Don't reload on initial mount to avoid double-loading
        if old_mode is not None and old_mode != new_mode:
            # Clear size cache so fresh sizes are computed with the new mode
            # (recursive vs shallow differ between sort modes)
            self._size_cache.clear()
            self.reload()

    def action_toggle_hidden(self) -> None:
        """Toggle visibility of hidden files."""
        self.show_hidden = not self.show_hidden
        self.app.notify(f"Hidden files: {'shown' if self.show_hidden else 'hidden'}")
        self.reload()

    def action_toggle_bar(self) -> None:
        """Toggle visibility of size bars."""
        self.show_size_bar = not self.show_size_bar
        self.app.notify(f"Size bars: {'shown' if self.show_size_bar else 'hidden'}")
        # Clear the tree's internal line cache and re-render all lines so
        # the change is visible immediately without waiting for navigation.
        self._clear_line_cache()
        self.refresh_lines(0, max(len(self._tree_lines), 1))

    def action_collapse_all(self) -> None:
        """Collapse all nodes."""
        self.collapse_all()

    def action_expand_all(self) -> None:
        """Expand all nodes."""
        self.expand_all()

    def action_delete(self) -> None:
        """Delete the currently selected file or directory."""
        from src.widgets.confirm_dialog import ConfirmDialog

        node = self.cursor_node
        if node is None or node.data is None:
            return

        path = node.data.path.expanduser().resolve()
        parent = node.parent

        # Build deletion message
        if path.is_dir():
            # Count items
            try:
                file_count = sum(1 for _ in path.rglob("*") if _.is_file())
                dir_count = sum(1 for _ in path.rglob("*") if _.is_dir())
                size = sum(_.stat().st_size for _ in path.rglob("*") if _.is_file())
                size_str = format_size(size)
            except OSError:
                file_count = 0
                dir_count = 0
                size_str = "unknown"

            msg = (f"Delete directory '{path.name}'?\n"
                   f"{file_count} files, {dir_count} subdirs, {size_str}")
        else:
            size = path.stat().st_size
            msg = f"Delete file '{path.name}'? ({format_size(size)})"

        # Show confirmation dialog
        self._delete_path = path
        self._delete_node = node
        self._delete_parent = parent

        self.app.push_screen(ConfirmDialog(msg, title="Delete"), self._handle_delete_result)

    def _handle_delete_result(self, result: bool) -> None:
        """Handle delete confirmation result.

        Args:
            result: True if confirmed, False otherwise.
        """
        if not result:
            return

        # Perform deletion
        path = self._delete_path
        node = self._delete_node
        parent = self._delete_parent

        try:
            if path.is_dir():
                # Remove directory and contents
                for child in sorted(path.rglob("*"), reverse=True):
                    if child.is_file() or child.is_symlink():
                        child.unlink()
                    elif child.is_dir():
                        child.rmdir()
                path.rmdir()
            else:
                path.unlink()

            # Remove from tree
            if parent and node in parent.children:
                node.remove()

            # Clear size cache entries for deleted paths
            to_remove = [p for p in self._size_cache if p in path.parents or p == path]
            for p in to_remove:
                self._size_cache.pop(p, None)

            # Refresh parent
            if parent:
                self.reload_node(parent)

            self.app.notify(f"Deleted: {path.name}", title="✓")

        except FileNotFoundError:
            self.app.notify("File already deleted", title="⚠")
        except PermissionError:
            self.app.notify("Permission denied", title="✗")
        except OSError as e:
            self.app.notify(f"Error: {e}", title="✗")

    async def action_refresh(self) -> None:
        """Refresh the tree."""
        self._size_cache.clear()
        await self.reload()
        self.app.notify("Tree refreshed", title="↻")

    def action_rename(self) -> None:
        """Prompt for a new name and rename the currently selected file/directory."""
        from src.widgets.input_dialog import InputDialog

        node = self.cursor_node
        if node is None or node.data is None:
            return

        path = node.data.path.expanduser().resolve()
        self._rename_node = node
        self.focus()

        self.app.push_screen(
            InputDialog(
                message=f"Rename '{path.name}'?",
                placeholder=path.name,
                title="Rename",
            ),
            lambda result, node=node, path=path: self._handle_rename(result, node, path),
        )

    def _handle_rename(self, result: str | None, node: TreeNode[DirEntry], path: Path) -> None:
        """Handle the result of the rename input dialog."""
        if not result or not result.strip():
            return

        new_name = result.strip()
        parent_dir = path.parent
        new_path = parent_dir / new_name

        try:
            path.rename(new_path)
            node.label = Text(new_name)
            node.data = DirEntry(new_path)
            self.refresh()
            self.app.notify(f"Renamed to '{new_name}'", title="✓")

        except FileExistsError:
            self.app.notify("A file with that name already exists", title="✗")
        except PermissionError:
            self.app.notify("Permission denied", title="✗")
        except OSError as e:
            self.app.notify(f"Error: {e}", title="✗")

    def action_search(self) -> None:
        """Prompt for search term and move cursor to the first match."""
        from src.widgets.input_dialog import InputDialog

        self.focus()
        self.app.push_screen(
            InputDialog(
                message="Search files:",
                placeholder="/pattern/",
                title="Search",
            ),
            self._handle_search,
        )

    def _handle_search(self, result: str | None) -> None:
        """Handle the result of the search input dialog."""
        if not result or not result.strip():
            return

        pattern = result.strip().lstrip("/").lower()

        # Search through tree lines
        for i, tree_line in enumerate(self._tree_lines):
            node = tree_line.node
            if node.data and pattern in node.data.path.name.lower():
                self.move_cursor_to_line(i)
                self.app.notify(f"Found: {node.data.path.name}", title="🔍")
                return

        self.app.notify(f"No matches for '{pattern}'", title="🔍")
