# myd

A **vi-like terminal file browser** built with [ratatui](https://ratatui.rs/) and [crossterm](https://crates.io/crates/crossterm).

Navigate your filesystem with familiar `vi` key bindings, inspect file details in a live sidebar, and visualize disk usage with proportional size bars — all from your terminal.

## Features

- **vi-style navigation** — `j`/`k` to move, `h`/`l` to collapse/expand, `gg`/`G` to jump to top/bottom, `dd` to delete, and more.
- **File tree with size visualization** — each entry shows a proportional size bar (green / amber / red) and human-readable file size.
- **Treemap view** — switch to a squarified treemap (`v`) that visualizes disk usage as proportional boxes, colored by file type so related content reads as one group. Navigate it with the same `hjkl` keys as the tree.
- **Type-colored tiles** — each treemap tile is filled by content category (code, docs, images, video, audio, archives, data, binaries); directories take the color of whatever content dominates them, and a legend in the status bar names the colors on screen.
- **Cached sizes** — drilling into a subdirectory reuses the sizes already computed instead of rescanning the disk; press `r` for a manual rescan.
- **Info panel** — optional sidebar (toggle with `t`) displaying name, type, size, permissions, owner/group, and timestamps for the selected item.
- **Sort modes** — cycle through *dirs first*, *files first*, *largest*, and *smallest* with `s`.
- **Toggle hidden files** — show or hide dotfiles with `H`.
- **Search** — find files by name with `/`.
- **Rename and delete** — rename with `R`, delete with `dd` (both with confirmation dialogs).
- **Change root** — jump to any directory with `gd` without losing your sort and view settings.
- **Persistent view preferences** — your chosen view (tree or treemap) and info-panel visibility stay put as you move between directories.
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
| `h`       | Collapse dir / go to parent (tree); move left, or step up to parent at the left edge (treemap) |
| `l`       | Expand directory (tree) / move right (treemap) |
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
| `t`       | Toggle info panel             |
| `/`       | Search by name                |
| `?` / `F1`| Help                          |

### Screen-level

| Key       | Action                        |
|-----------|-------------------------------|
| `q`       | Quit immediately              |
| `Esc`     | Quit                          |
| `Ctrl+o`  | Go back to the parent directory |
| `r` / `Ctrl+r` | Rescan (refresh sizes from disk) |
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

Press `v` to toggle between the file tree and a squarified treemap. The treemap visualizes disk usage as proportional rectangular boxes — larger files and directories take up more space. Navigate with `j`/`k`/`h`/`l` to move between boxes, `l`/Enter to descend into a directory, and `h` to move left (stepping up to the parent directory when already on a left-edge tile).

```
+-------------------+-----------+
|                   |  +------+ |
|     projects      |  | docs | |
|                   |  |      | |
|                   |  |      | |
+-------------------+  +------+ |
|          +--------------------+
|  readme  |
|          |
+----------+
```

Each tile is filled with a color for its content category — **code**, **docs**, **images**, **video**, **audio**, **archives**, **data**, **binaries**, or **other**. A file's color comes from its extension; a directory takes the color of the category holding most of its bytes. A legend in the status bar names the categories currently on screen. When a tile is too narrow to show its full name, the selected item's name appears in the status bar instead.

## License

MIT
