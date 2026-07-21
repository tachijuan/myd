# file-browser-tui

A **vi-like terminal file browser and manager** built with the [Textual](https://textual.textualize.io/) TUI framework.

Navigate your filesystem with familiar `vi` key bindings, inspect file details in a live sidebar, and visualize disk usage with proportional size bars — all from your terminal.

## Features

- **vi-style navigation** — `j`/`k` to move, `h`/`l` to collapse/expand, `gg`/`G` to jump to top/bottom, `dd` to delete, and more.
- **File tree with size visualization** — each entry shows a proportional size bar (green / amber / red) and human-readable file size.
- **Info panel** — live sidebar displaying name, type, size, permissions, owner/group, and timestamps for the selected item.
- **Sort modes** — cycle through *dirs first*, *files first*, *largest*, and *smallest* with `s`.
- **Toggle hidden files** — show or hide dotfiles with `H`.
- **Search** — find files by name with `/`.
- **Rename and delete** — rename with `R`, delete with `dd` (both with confirmation dialogs).
- **Change root** — jump to any directory with `gd` without losing your sort and view settings.
- **Progress overlay** — non-blocking directory enumeration with a progress indicator for large trees.
- **Rust port (myd)** — a port using [ratatui](https://ratatui.rs/) and [crossterm](https://crates.io/crates/crossterm) lives in the `myd/` subdirectory.

## Requirements

- Python 3.12+
- [Textual](https://pypi.org/project/textual/) 8.2.x (installed automatically as a dependency)
- A terminal with true color support

## Installation

```bash
# Clone the repository
git clone <repo-url>
cd untest

# Install in development mode
pip install -e .

# Or install with dev dependencies (pytest, ruff)
pip install -e ".[dev]"
```

## Usage

```bash
# Start with the directory picker (choose from common directories)
file-browser

# Start at a specific path
file-browser ~/Documents
file-browser /var/log

# Run directly without installation
python -m src
python -m src ~/Downloads
```

## Key Bindings

### Navigation

| Key       | Action              |
|-----------|---------------------|
| `j`       | Move cursor down    |
| `k`       | Move cursor up      |
| `h`       | Collapse / Go to parent |
| `l`       | Expand directory    |
| `gg`      | Jump to top         |
| `G`       | Jump to bottom      |
| `Ctrl+d`  | Page down           |
| `Ctrl+u`  | Page up             |
| `g u`     | Go to parent directory |
| `g d`     | Change root directory |
| `0`       | Collapse all        |
| `*`       | Expand all          |

### File Operations

| Key       | Action              |
|-----------|---------------------|
| `dd`      | Delete (with confirmation) |
| `R`       | Rename              |
| `r`       | Refresh tree        |

### View

| Key       | Action              |
|-----------|---------------------|
| `s`       | Cycle sort mode     |
| `H`       | Toggle hidden files |
| `b`       | Toggle size bars    |
| `/`       | Search by name      |

### Screen-level

| Key       | Action              |
|-----------|---------------------|
| `q`       | Quit                |
| `Esc`     | Quit                |
| `Ctrl+r`  | Refresh             |
| `Ctrl+b`  | Toggle info panel   |

## Sort Modes

Press `s` to cycle through:

1. **Dirs first** — directories listed before files, alphabetical within each group.
2. **Files first** — files listed before directories, alphabetical within each group.
3. **Largest** — sorted by descending size (recursive directory sizes).
4. **Smallest** — sorted by ascending size (recursive directory sizes).

## Size Bars

Each file and directory in the tree has a proportional size bar:

```
[██████░░░░] 📁 projects    245 MB
[██░░░░░░░░] 📁 documents   52 MB
[░░░░░░░░░░] 📄 readme.txt  1.2 KB
```

Colors indicate the item's share of its parent directory's total size:

- **Green** — less than 50% of parent
- **Amber** — 50–80% of parent
- **Red** — more than 80% of parent

Toggle bars on/off with `b`.

## Project Structure

```
├── src/
│   ├── __init__.py
│   ├── __main__.py        # Entry point (`python -m src`)
│   ├── app.py             # Main Textual application
│   ├── screen.py          # MainScreen layout (tree + info panel)
│   ├── dir_picker.py      # Startup directory picker screen
│   ├── styles.tcss        # Textual CSS styling
│   ├── utils/
│   │   └── sizes.py       # Size calculation and formatting utilities
│   └── widgets/
│       ├── file_tree.py   # Custom DirectoryTree with vi bindings and size bars
│       ├── file_info.py   # FileInfoPanel — detailed file metadata sidebar
│       ├── size_bar.py    # Proportional disk space bar widget
│       ├── confirm_dialog.py  # Modal confirmation dialog
│       ├── input_dialog.py    # Modal text input dialog
│       └── progress_overlay.py # Enumeration progress overlay
├── tests/
│   ├── test_app.py
│   ├── test_functionality.py
│   └── test_verification.py
├── myd/                    # Rust port (ratatui/crossterm)
│   ├── Cargo.toml
│   ├── src/
│   └── tests/
├── pyproject.toml
└── README.md
```

## Testing

```bash
# Run the test suite
pytest

# Run with verbose output
pytest -v

# Lint with ruff
ruff check src/
```

## Rust Port

A Rust implementation using [ratatui](https://ratatui.rs/) and [crossterm](https://crates.io/crates/crossterm) lives in the `myd/` subdirectory.

```bash
cd myd
cargo run
cargo run -- ~/Documents
```

## License

MIT
