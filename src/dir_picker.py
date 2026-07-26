"""DirPickerScreen - modal for selecting a starting directory."""

from __future__ import annotations

from pathlib import Path

from textual.screen import Screen
from textual.widgets import Button, Footer, Input, Label, OptionList
from textual.widgets._option_list import Option


class DirPickerScreen(Screen):
    """Full-screen directory picker shown at startup.

    Allows the user to either:
    - Type a path in the Input field
    - Select from a list of common directories
    """

    CSS = """
    Screen {
        layout: vertical;
        height: 100%;
        align: center middle;
    }

    #title-label {
        width: 60%;
        height: auto;
        margin: 2;
        content-align: center top;
        text-style: bold;
        color: $accent;
    }

    #path-input {
        width: 60%;
        margin: 1;
    }

    #dir-list {
        width: 60%;
        height: 1fr;
        margin: 1;
        border: solid $accent;
    }

    #go-button {
        width: 60%;
        margin: 1;
    }

    Footer {
        dock: bottom;
        height: 1;
    }
    """

    def compose(self) -> list:
        """Compose the screen."""
        yield Label("Select a starting directory (ESC to quit)", id="title-label")
        yield Input(placeholder="Enter path or press TAB to select from list...",
                     id="path-input")
        yield OptionList(id="dir-list")
        yield Button("Go", id="go-button", variant="primary")
        yield Footer()

    def on_mount(self) -> None:
        """Set up the directory list on mount."""
        home = Path.home()
        cwd = Path.cwd()

        common_dirs = [
            (str(home), "~ (Home)"),
            (str(cwd), f". (Current: {cwd.name})"),
            (str(home / "Desktop"), "Desktop"),
            (str(home / "Documents"), "Documents"),
            (str(home / "Downloads"), "Downloads"),
            (str(home / "Pictures"), "Pictures"),
            (str(home / "Music"), "Music"),
            (str(home / "Videos"), "Videos"),
            ("/", "/ (Root)"),
            ("/tmp", "/tmp"),
        ]

        valid_dirs = [(path, label) for path, label in common_dirs if Path(path).exists()]

        option_list = self.query_one(OptionList)
        option_list.add_options(
            Option(label, id=path) for path, label in valid_dirs
        )

        input_widget = self.query_one(Input)
        input_widget.focus()

    def on_key(self, event) -> None:
        """Handle key events."""
        if event.key == "escape":
            self.app.exit()

        if event.key == "enter":
            input_widget = self.query_one(Input)
            if input_widget.has_focus and input_widget.value:
                self._handle_path(input_widget.value)
                return

            option_list = self.query_one(OptionList)
            if option_list.has_focus and option_list.highlighted is not None:
                selected_path = option_list.options[option_list.highlighted].id
                if selected_path:
                    self._handle_path(selected_path)
                return

    def on_button_pressed(self, event: Button.Pressed) -> None:
        """Handle button press."""
        if event.button.id == "go-button":
            input_widget = self.query_one(Input)
            if input_widget.value:
                self._handle_path(input_widget.value)
                return

            option_list = self.query_one(OptionList)
            if option_list.highlighted is not None:
                selected_path = option_list.options[option_list.highlighted].id
                if selected_path:
                    self._handle_path(selected_path)

    def on_option_list_highlighted(self, event: OptionList.Highlighted) -> None:
        """Update input when an option is highlighted."""
        if event.option_id:
            input_widget = self.query_one(Input)
            input_widget.value = str(event.option_id)

    def _handle_path(self, path_str: str) -> None:
        """Handle a path selection."""
        from src.screen import MainScreen

        path = Path(path_str).expanduser().resolve()

        if not path.exists():
            self.app.notify(f"Path does not exist: {path}", title="✗")
            return

        if not path.is_dir():
            self.app.notify(f"Not a directory: {path}", title="⚠")
            return

        self.app.push_screen(MainScreen(path))
