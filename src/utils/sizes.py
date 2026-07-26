"""File and directory size calculation utilities."""

from __future__ import annotations

import os
from pathlib import Path
from typing import Final

# Human-readable size units
_SIZE_UNITS: Final = ("B", "KB", "MB", "GB", "TB")


def format_size(size_bytes: int) -> str:
    """Format bytes into a human-readable string.

    Args:
        size_bytes: Size in bytes.

    Returns:
        Formatted string like "1.2 KB" or "3.4 MB".
    """
    if size_bytes < 0:
        return "0 B"
    if size_bytes == 0:
        return "0 B"

    size = float(size_bytes)
    unit_index = 0

    while size >= 1024 and unit_index < len(_SIZE_UNITS) - 1:
        size /= 1024
        unit_index += 1

    if unit_index == 0:
        return f"{int(size)} {_SIZE_UNITS[unit_index]}"
    return f"{size:.1f} {_SIZE_UNITS[unit_index]}"


def get_file_size(path: Path) -> int:
    """Get size of a single file.

    Args:
        path: Path to the file.

    Returns:
        Size in bytes, or 0 on error.
    """
    try:
        return path.stat().st_size
    except OSError:
        return 0


def get_shallow_size(path: Path) -> int:
    """Get shallow size of a directory (only direct children).

    This is fast because it doesn't recurse into subdirectories.
    For subdirectories, only the directory entry itself is counted.

    Args:
        path: Path to the directory.

    Returns:
        Total size in bytes of direct children.
    """
    total = 0
    if not path.is_dir():
        return get_file_size(path)

    try:
        for entry in path.iterdir():
            try:
                stat = entry.stat()
                total += stat.st_size
            except OSError:
                pass
    except OSError:
        pass

    return total


def get_dir_size(path: Path) -> int:
    """Recursively compute directory size (like du).

    Walks the entire directory tree and sums all file sizes.

    Args:
        path: Path to the directory.

    Returns:
        Total size in bytes of all files in the directory tree.
    """
    total = 0
    if not path.is_dir():
        return get_file_size(path)

    try:
        for dirpath, dirnames, filenames in os.walk(str(path)):
            # Skip hidden directories by default
            dirnames[:] = [d for d in dirnames if not d.startswith(".")]
            for filename in filenames:
                file_path = Path(dirpath) / filename
                try:
                    total += file_path.stat().st_size
                except OSError:
                    pass
    except OSError:
        pass

    return total


def get_size(path: Path, recursive: bool = False) -> int:
    """Get size of a file or directory.

    Args:
        path: Path to the file or directory.
        recursive: If True and path is a directory, recurse into subdirectories.
                   If False, only count direct children.

    Returns:
        Size in bytes.
    """
    if path.is_file():
        return get_file_size(path)
    if recursive:
        return get_dir_size(path)
    return get_shallow_size(path)
