"""MainScreen - the primary screen layout with file tree and info panel."""

from __future__ import annotations

from pathlib import Path

from textual.screen import Screen
from textual.containers import Horizontal
from textual.widgets import Footer, Header
from textual.widgets._directory_tree import DirEntry
from textual.widgets._tree import TreeNode, Tree


class MainScreen(Screen):
    """Primary screen with file tree and info panel."""

    CSS = """
    Screen {
        layout: vertical;
        height: 100%;
    }

    Header {
        dock: top;
        height: 1;
    }

    #main-hbox {
        height: 1fr;
        layout: horizontal;
    }

    FileTree {
        width: 70%;
        border: solid $accent;
        padding: 0;
    }

    MainScreen.info-hidden FileTree {
        width: 100%;
    }

    #info-panel {
        width: 30%;
        border: solid $accent;
        padding: 1;
        overflow-y: auto;
    }

    Footer {
        dock: bottom;
        height: 1;
    }

    /* Size bar styling */
    FileTree .file-tree--size-text {
        color: $text;
    }

    FileTree .file-tree--size-bar {
        color: $text-muted;
    }
    """

    BINDINGS = [
        ("q", "quit", "Quit"),
        ("ctrl+r", "action_refresh", "Refresh"),
        ("ctrl+b", "toggle_info_panel", "Info"),
    ]

    def __init__(self, start_path: str | Path, *, sort_mode=None, show_hidden: bool | None = None,
                 info_panel_hidden: bool = False,
                 name: str | None = None, id: str | None = None, classes: str | None = None) -> None:
        """Initialize the MainScreen.

        Args:
            start_path: The root directory path for the file tree.
            sort_mode: Sort mode to carry over from a previous screen.
            show_hidden: Whether to show hidden files (default: use FileTree default).
            info_panel_hidden: Whether the info panel was hidden on the previous screen.
            name: Screen name.
            id: Screen ID.
            classes: CSS classes.
        """
        super().__init__(name=name, id=id, classes=classes)
        self.start_path = Path(start_path).expanduser().resolve()
        self._sort_mode = sort_mode
        self._show_hidden = show_hidden
        self._info_panel_hidden = info_panel_hidden

    def compose(self):
        """Compose the screen layout."""
        from src.widgets.file_info import FileInfoPanel
        from src.widgets.file_tree import FileTree

        yield Header()
        with Horizontal(id="main-hbox"):
            yield FileTree(
                self.start_path,
                id="file-tree",
            )
            yield FileInfoPanel(id="info-panel")
        yield Footer()

    def on_mount(self) -> None:
        """Set up the screen on mount."""
        from src.widgets.file_tree import FileTree

        # Set the title
        self.title = f"File Browser — {self.start_path}"

        # Focus the file tree
        tree = self.query_one(FileTree)
        tree.focus()

        # Carry over sort mode and hidden files setting
        if self._sort_mode is not None:
            tree.sort_mode = self._sort_mode
        if self._show_hidden is not None:
            tree.show_hidden = self._show_hidden

        # Restore info panel visibility state
        if self._info_panel_hidden:
            info_panel = self.query_one("#info-panel")
            info_panel.display = False
            self.add_class("info-hidden")

        # Expand the root node
        tree.root.expand()

    async def action_refresh(self) -> None:
        """Refresh the file tree."""
        from src.widgets.file_tree import FileTree
        tree = self.query_one(FileTree)
        await tree.action_refresh()

    def action_toggle_info_panel(self) -> None:
        """Toggle the visibility of the info panel."""
        info_panel = self.query_one("#info-panel")

        if info_panel.display:
            info_panel.display = False
            self.add_class("info-hidden")
            self.app.notify("Info panel hidden", title="Hide")
        else:
            info_panel.display = True
            self.remove_class("info-hidden")
            self.app.notify("Info panel shown", title="Show")

        # Refocus the tree so key bindings continue working
        from src.widgets.file_tree import FileTree
        self.query_one(FileTree).focus()

    def on_key(self, event) -> None:
        """Handle screen-level key events.

        Args:
            event: The key event.
        """
        # Handle Escape to quit
        if event.key == "escape":
            self.app.exit()

    def on_node_highlighted(self, event: Tree.NodeHighlighted[DirEntry]) -> None:
        """Refresh the info panel when the tree selection changes.

        NodeHighlighted is a Message that bubbles up from the FileTree.
        Since FileInfoPanel is a sibling (not a parent), it never receives
        the event directly. The screen catches it here and relays to the panel.
        """
        try:
            info = self.query_one("#info-panel")
            info.refresh()
        except Exception:
            pass

    def on_descendant_focus(self) -> None:
        """Update status bar when focus changes."""
        self._update_status_bar()

    def _update_status_bar(self) -> None:
        """Update the status bar with current stats."""
        from src.widgets.file_tree import FileTree, SortMode

        try:
            tree = self.query_one(FileTree)
            sort_mode = tree.sort_mode

            # Count nodes (exclude root from counts)
            total = len(tree._tree_lines) - 1  # subtract root
            dirs = sum(1 for tl in tree._tree_lines if tl.node != tree.root and tl.node._allow_expand)
            files = total - dirs

            # Update header subtitle
            self.sub_title = f"{total} items │ {dirs} dirs │ {files} files │ Sort: {sort_mode.value}"

        except Exception:
            pass
