"""ProgressOverlay - a modal screen shown while directory enumeration is in progress."""

from __future__ import annotations

from textual.screen import ModalScreen
from textual.containers import Center
from textual.widgets._progress_bar import Bar
from textual.widgets import Label
from textual.app import ComposeResult


class ProgressOverlay(ModalScreen[None]):
    """Show a modal progress bar while files are being enumerated.

    Dismissed automatically when the caller passes `None` as the result.
    """

    DEFAULT_CSS = """
    ProgressOverlay {
        align: center middle;
        background: rgba(0, 0, 0, 0.6);
    }

    ProgressOverlay > .progress-card {
        width: 50;
        height: 5;
        layout: vertical;
        align: center middle;
        background: $surface;
        border: thick $accent;
        padding: 1;
    }

    ProgressOverlay > .progress-card > Label {
        width: 1fr;
        text-align: center;
        text-style: bold;
        color: $accent;
    }

    ProgressOverlay > .progress-card > Bar {
        width: 1fr;
    }
    """

    def compose(self) -> ComposeResult:
        with Center(classes="progress-card"):
            yield Label("Enumerating files...", id="progress-label")
            yield Bar(id="progress-bar", show_percentage=False)

    def on_key(self, key) -> None:
        """Suppress all key input while the overlay is active."""
        pass
