"""ConfirmDialog - a simple modal confirmation dialog."""

from __future__ import annotations

from textual.screen import ModalScreen
from textual.widgets import Label, Button
from textual.containers import Horizontal, Vertical


class ConfirmDialog(ModalScreen[bool]):
    """A modal dialog that asks for confirmation.

    Returns True if confirmed, False otherwise.
    """

    CSS = """
    ConfirmDialog {
        align: center middle;
    }

    #dialog-box {
        width: 60;
        height: auto;
        border: thick $accent;
        padding: 1 2;
        margin: 2;
        background: $surface;
    }

    #message-label {
        width: auto;
        height: auto;
        text-align: center;
        text-style: bold;
        margin-bottom: 1;
    }

    #buttons {
        width: 100%;
        height: auto;
        align: center middle;
        layout: horizontal;
    }

    Button {
        width: 10;
        margin: 0 2;
    }
    """

    def __init__(self, message: str, title: str = "Confirm") -> None:
        """Initialize the ConfirmDialog.

        Args:
            message: The confirmation message.
            title: The dialog title.
        """
        super().__init__()
        self.message = message
        self.title = title

    def compose(self):
        """Compose the dialog."""
        with Vertical(id="dialog-box"):
            yield Label(f"{self.title}", id="title-label")
            yield Label(self.message, id="message-label")
            with Horizontal(id="buttons"):
                yield Button("Yes", id="yes-button", variant="warning")
                yield Button("No", id="no-button", variant="default")

    def on_button_pressed(self, event: Button.Pressed) -> None:
        """Handle button press.

        Args:
            event: The button pressed event.
        """
        if event.button.id == "yes-button":
            self.dismiss(True)
        else:
            self.dismiss(False)

    def on_key(self, event) -> None:
        """Handle key events.

        Args:
            event: The key event.
        """
        if event.key == "escape":
            self.dismiss(False)
        if event.key in ("y", "enter"):
            self.dismiss(True)
        if event.key == "n":
            self.dismiss(False)
