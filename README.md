# myd

A **vi-like terminal file browser** built with [ratatui](https://ratatui.rs/) and [crossterm](https://crates.io/crates/crossterm).

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
- **Treemap view** — switch to a squarified treemap (`v`) that visualizes disk usage as proportional boxes, with `~` shortcuts for home-directory paths.
- **Progress overlay** — non-blocking directory enumeration with a progress indicator for large trees.

## Requirements

- Rust 1.75+ (stable)
- A terminal with true color support

## Installation

```bash
# Clone and install
git clone http://umbrel:8085/juan/myd.git
cd myd
cargo install --path . --locked
```

Or build and run in release mode:

```bash
cd myd
cargo build --release
./target/release/myd
./target/release/myd ~/Documents
```

## Usage

```bash
# Start with the directory picker (choose from common directories)
myd

# Start at a specific path
myd ~/Documents
myd /var/log
```

## Key Bindings

### Navigation

| Key       | Action                        |
|-----------|-------------------------------|
| `j`       | Move cursor down              |
| `k`       | Move cursor up                |
| `h`       | Collapse dir / Go to parent   |
| `l`       | Expand directory              |
| `gg`      | Jump to top                   |
| `G`       | Jump to bottom                |
| `Ctrl+d`  | Page down                     |
| `Ctrl+u`  | Page up                       |
| `g u`     | Go to parent directory        |
| `g d`     | Change root directory         |
| `0`       | Collapse all                  |
| `*`       | Expand all                    |

### File Operations

| Key       | Action                        |
|-----------|-------------------------------|
| `dd`      | Delete (with confirmation)    |
| `R`       | Rename                        |
| `r`       | Refresh tree                  |

### View

| Key       | Action                        |
|-----------|-------------------------------|
| `v`       | Toggle tree / treemap view    |
| `s`       | Cycle sort mode               |
| `H`       | Toggle hidden files           |
| `b`       | Toggle size bars              |
| `/`       | Search by name                |
| `?` / `F1`| Help                          |

### Screen-level

| Key       | Action                        |
|-----------|-------------------------------|
| `q`       | Quit / Go back                |
| `Esc`     | Quit                          |
| `Ctrl+r`  | Refresh                       |
| `Ctrl+b`  | Toggle info panel             |

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

## Treemap View

Press `v` to toggle between the file tree and a squarified treemap. The treemap visualizes disk usage as proportional rectangular boxes — larger files and directories take up more space. Navigate with `j`/`k`/`h`/`l` to move between boxes.

```
+-------------------+-----------+
|                   |  +------+ |
|     projects      |  | docs | |
|     (245 MB)      |  |(52M) | |
|                   |  |      | |
+-------------------+  +------+ |
|           +-------------------+
|   readme |
|  (1.2 KB) |
+-----------+
```

Paths use `~` for the home directory and intelligently truncate long paths so the filename always takes priority when a box is narrow.

## License

MIT
