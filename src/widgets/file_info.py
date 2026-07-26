"""FileInfoPanel widget - displays detailed information about the selected file or directory."""

from __future__ import annotations

import grp
import os
import pwd
from datetime import datetime
from pathlib import Path
from typing import ClassVar

from rich.text import Text
from textual.widget import Widget
from textual.widgets._directory_tree import DirEntry
from textual.widgets._tree import TreeNode, Tree


class FileInfoPanel(Widget):
    """Display detailed information about the selected file/directory.

    Shows: name, type, size, permissions, timestamps, and a size bar.
    """

    DEFAULT_CSS = """
    FileInfoPanel {
        layout: vertical;
        padding: 1;
        border: solid $accent;
        margin: 1;
    }

    FileInfoPanel > .info-header {
        text-style: bold;
        color: $accent;
    }

    FileInfoPanel > .info-section {
        padding: 0;
    }
    """

    def __init__(self, *, name: str | None = None, id: str | None = None,
                 classes: str | None = None) -> None:
        """Initialize the FileInfoPanel.

        Args:
            name: Widget name.
            id: Widget ID.
            classes: CSS classes.
        """
        super().__init__(name=name, id=id, classes=classes)
        self._tree: Tree[DirEntry] | None = None

    def on_mount(self) -> None:
        """Find the FileTree widget in the app."""
        # Don't look for the tree yet - it may not be mounted
        self._tree = None

    def _get_tree(self) -> Tree[DirEntry] | None:
        """Get the FileTree widget.

        Returns:
            The FileTree widget, or None if not found.
        """
        if self._tree is None:
            from src.widgets.file_tree import FileTree
            try:
                self._tree = self.screen.query_one(FileTree)
            except Exception:
                return None
        return self._tree

    def _get_node(self) -> TreeNode[DirEntry] | None:
        """Get the currently selected node.

        Returns:
            The cursor node, or None.
        """
        tree = self._get_tree()
        if tree is None:
            return None
        return tree.cursor_node

    def _format_permissions(self, mode: int) -> str:
        """Format file permissions as a string like 'drwxr-xr-x'.

        Args:
            mode: The file mode bits.

        Returns:
            Permission string.
        """
        type_map = {
            "p": "pipe",
            "c": "char",
            "b": "block",
            "d": "dir",
            "l": "link",
            "s": "socket",
            "-": "file",
        }

        if mode & 0o60000 == 0o40000:  # directory
            letter = "d"
        elif mode & 0o170000 == 0o120000:  # symlink
            letter = "l"
        else:
            letter = "-"

        perms = ""
        for shift in (6, 3, 0):
            bits = (mode >> shift) & 7
            perms += "r" if bits & 4 else "-"
            perms += "w" if bits & 2 else "-"
            perms += "x" if bits & 1 else "-"

        return letter + perms

    def _count_items(self, path: Path) -> tuple[int, int]:
        """Count files and directories in a directory.

        Args:
            path: Directory path.

        Returns:
            Tuple of (file_count, dir_count).
        """
        file_count = 0
        dir_count = 0
        try:
            for entry in path.iterdir():
                if entry.is_dir():
                    dir_count += 1
                else:
                    file_count += 1
        except OSError:
            pass
        return file_count, dir_count

    def render(self) -> Text:
        """Render the info panel.

        Returns:
            Rich Text with file/directory details.
        """
        node = self._get_node()
        if node is None or node.data is None:
            return Text("No selection", style="dim")

        path = node.data.path.expanduser().resolve()

        # Get file stats
        try:
            stat = path.stat()
        except OSError:
            return Text(f"Cannot access '{path.name}'", style="red")

        # Build info text
        lines: list[tuple[str, str | None]] = []

        # Header
        lines.append((f"  {path.name}", "bold green"))

        lines.append(("", None))  # Empty line

        # Type
        if path.is_dir():
            lines.append(("Type:", None))
            lines.append(("Directory", "blue"))
        else:
            lines.append(("Type:", None))
            lines.append(("File", "cyan"))

        lines.append(("", None))

        # Size
        from src.utils.sizes import format_size
        size = stat.st_size
        lines.append(("Size:", None))
        lines.append((format_size(size), "yellow"))

        # For directories, also show item count
        if path.is_dir():
            file_count, dir_count = self._count_items(path)
            lines.append(("", None))
            lines.append(("Contents:", None))
            lines.append((f"{file_count} files, {dir_count} directories", "cyan"))

        lines.append(("", None))

        # Permissions
        lines.append(("Permissions:", None))
        lines.append((self._format_permissions(stat.st_mode), "magenta"))

        lines.append(("", None))

        # Owner / Group
        try:
            owner = pwd.getpwuid(stat.st_uid).pw_name
        except (KeyError, OverflowError):
            owner = str(stat.st_uid)
        try:
            group = grp.getgrgid(stat.st_gid).gr_name
        except (KeyError, OverflowError):
            group = str(stat.st_gid)

        lines.append(("Owner:", None))
        lines.append((owner, "cyan"))

        lines.append(("", None))
        lines.append(("Group:", None))
        lines.append((group, "cyan"))

        lines.append(("", None))

        # Timestamps
        lines.append(("Created:", None))
        created = datetime.fromtimestamp(stat.st_ctime)
        lines.append((created.strftime("%Y-%m-%d %H:%M"), "dim"))

        lines.append(("", None))
        lines.append(("Modified:", None))
        modified = datetime.fromtimestamp(stat.st_mtime)
        lines.append((modified.strftime("%Y-%m-%d %H:%M"), "dim"))

        lines.append(("", None))
        lines.append(("Accessed:", None))
        accessed = datetime.fromtimestamp(stat.st_atime)
        lines.append((accessed.strftime("%Y-%m-%d %H:%M"), "dim"))

        lines.append(("", None))

        # Path
        lines.append(("Path:", None))
        lines.append((str(path), "dim"))

        # Build text
        text = Text()
        for content, style in lines:
            if style:
                text.append(Text(content + "\n", style=style))
            else:
                text.append(Text(content + "\n"))

        return text

    def on_node_highlighted(self, event: Tree.NodeHighlighted[DirEntry]) -> None:
        """Refresh when the tree selection changes.

        Args:
            event: The node highlighted event.
        """
        self.refresh()
