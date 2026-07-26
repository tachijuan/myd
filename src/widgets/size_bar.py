"""SizeBar widget - horizontal proportional bar for disk space visualization."""

from __future__ import annotations

from typing import ClassVar

from rich.text import Text
from textual.widget import Widget


class SizeBar(Widget):
    """A horizontal bar showing space usage proportion.

    Uses block characters (█░) to display a proportional bar.
    Color is determined by the ratio: green (<50%), amber (50-80%), red (>80%).

    Attributes:
        value: The current value (used space).
        maximum: The maximum value (total space).
        width: Number of characters for the bar display.
    """

    DEFAULT_CSS = """
    SizeBar {
        height: 1;
    }
    """

    def __init__(
        self,
        value: float = 0.0,
        maximum: float = 1.0,
        bar_width: int = 40,
    ) -> None:
        """Initialize the SizeBar.

        Args:
            value: Current value (used space).
            maximum: Maximum value (total space).
            bar_width: Number of characters for the bar.
        """
        super().__init__()
        self.value = value
        self.maximum = maximum
        self.bar_width = bar_width

    def _get_color(self, ratio: float) -> str:
        """Get color based on ratio.

        Args:
            ratio: The ratio of value to maximum (0.0 to 1.0).

        Returns:
            Color name string.
        """
        if ratio < 0.5:
            return "green"
        if ratio < 0.8:
            return "amber"
        return "red"

    def render(self) -> Text:
        """Render the bar as a Rich Text object.

        Returns:
            A Text object with the proportional bar.
        """
        ratio = self.value / self.maximum if self.maximum > 0 else 0.0
        ratio = min(max(ratio, 0.0), 1.0)  # Clamp to [0, 1]

        color = self._get_color(ratio)
        filled = int(ratio * self.bar_width)
        empty = self.bar_width - filled

        text = Text()
        text.append("█" * filled, style=color)
        text.append("░" * empty, style="ansi_bright_black")

        return text

    def watch_value(self) -> None:
        """Refresh when value changes."""
        self.refresh()

    def watch_maximum(self) -> None:
        """Refresh when maximum changes."""
        self.refresh()
