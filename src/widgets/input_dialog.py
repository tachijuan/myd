"""InputDialog - a simple modal text input dialog."""

from __future__ import annotations

from textual.screen import ModalScreen
from textual.widgets import Label, Button, Input
from textual.containers import Horizontal, Vertical


class InputDialog(ModalScreen[str | None]):
    """A modal dialog that prompts for text input.

    Returns the entered string if confirmed, None otherwise.
    """

    CSS = """
    InputDialog {
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

    #title-label {
        width: auto;
        height: auto;
        text-align: center;
        text-style: bold;
        color: $accent;
    }

    #message-label {
        width: auto;
        height: auto;
        text-align: center;
        margin-bottom: 1;
    }

    #input-widget {
        width: 100%;
        margin: 0 2;
    }

    #buttons {
        width: 100%;
        height: auto;
        align: center middle;
        layout: horizontal;
        margin-top: 1;
    }

    Button {
        width: 10;
        margin: 0 2;
    }
    """

    def __init__(
        self,
        message: str,
        placeholder: str = "",
        title: str = "Input",
    ) -> None:
        """Initialize the InputDialog.

        Args:
            message: The prompt message.
            placeholder: Placeholder text for the input field.
            title: The dialog title.
        """
        super().__init__()
        self.message = message
        self.placeholder = placeholder
        self.title = title

    def compose(self):
        """Compose the dialog."""
        with Vertical(id="dialog-box"):
            yield Label(f"{self.title}", id="title-label")
            yield Label(self.message, id="message-label")
            yield Input(placeholder=self.placeholder, id="input-widget")
            with Horizontal(id="buttons"):
                yield Button("OK", id="ok-button", variant="primary")
                yield Button("Cancel", id="cancel-button", variant="default")

    def on_mount(self) -> None:
        """Focus the input field on mount."""
        input_widget = self.query_one("#input-widget", Input)
        input_widget.focus()

    def on_key(self, event) -> None:
        """Handle key events."""
        if event.key == "escape":
            self.dismiss(None)

    def on_button_pressed(self, event: Button.Pressed) -> None:
        """Handle button press."""
        if event.button.id == "ok-button":
            input_widget = self.query_one("#input-widget", Input)
            self.dismiss(input_widget.value)
        else:
            self.dismiss(None)

    def on_input_submitted(self, event: Input.Submitted) -> None:
        """Handle Enter key in the input field."""
        self.dismiss(event.value)
