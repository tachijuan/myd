"""FileBrowserApp - the main Textual application."""

from __future__ import annotations

import sys
from pathlib import Path

from textual.app import App, ComposeResult
from textual.binding import Binding
from textual.screen import Screen


class FileBrowserApp(App):
    """A vi-like TUI file browser and manager.

    Features:
    - Directory tree navigation with vi-like key bindings
    - File/directory deletion with confirmation
    - Disk space visualization with proportional bars
    - Configurable sort order (dirs first, files first, largest, smallest)
    """

    CSS_PATH = Path(__file__).parent / "styles.tcss"

    BINDINGS = [
        Binding("q", "quit", "Quit"),
        Binding("ctrl+r", "rebuild", "Rebuild"),
    ]

    def on_mount(self) -> None:
        """Set up the app on mount."""
        # Check for command-line path argument
        if len(sys.argv) > 1:
            path = Path(sys.argv[1]).expanduser().resolve()

            if not path.exists():
                self.notify(f"Path does not exist: {path}", title="✗")
                self.push_screen(self._get_dir_picker())
                return

            if not path.is_dir():
                self.notify(f"Not a directory: {path}", title="⚠")
                self.push_screen(self._get_dir_picker())
                return

            from src.screen import MainScreen
            self.push_screen(MainScreen(path))
        else:
            # Show directory picker
            self.push_screen(self._get_dir_picker())

    def _get_dir_picker(self) -> Screen:
        """Get a DirPickerScreen instance.

        Returns:
            A DirPickerScreen instance.
        """
        from src.dir_picker import DirPickerScreen
        return DirPickerScreen()


def main() -> None:
    """Entry point for the application."""
    app = FileBrowserApp()
    app.run()


if __name__ == "__main__":
    main()
