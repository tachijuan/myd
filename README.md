# myd

A **vi-like terminal file browser** built with [ratatui](https://ratatui.rs/) and [crossterm](https://crates.io/crates/crossterm).

Navigate your filesystem with familiar `vi` key bindings, inspect file details in a live sidebar, and visualize disk usage with proportional size bars — all from your terminal.

## Features

- **vi-style navigation** — `j`/`k` to move, `h`/`l` to collapse/expand, `gg`/`G` to jump to top/bottom, and more.
- **File tree with size visualization** — each entry shows a proportional size bar (green / amber / red) and human-readable file size.
- **Treemap view** — switch to a squarified treemap (`v`) that visualizes disk usage as proportional boxes, colored by file type so related content reads as one group. Navigate it with the same `hjkl` keys as the tree.
- **Type-colored tiles** — each treemap tile is filled by content category (code, docs, images, video, audio, archives, data, binaries); directories take the color of whatever content dominates them, and a legend in the status bar names the colors on screen.
- **Cached sizes** — drilling into a subdirectory reuses the sizes already computed instead of rescanning the disk; press `r` for a manual rescan.
- **Tag and act on multiple files** — tag files with `t`, sweep a range in visual mode (`V`), then copy (`c`), move (`m`) or delete (`D`) every tagged file at once. Tagged rows are highlighted. Untag one with `t`, all with `U`.
- **Regex search & filter** — search names with `/` (regex, case-insensitive) and step through matches with `n` / `p`; `f` filters the whole tree to a regex, hiding everything that doesn't match. A malformed pattern is reported rather than ignored.
- **Create directories** — make a new directory in the current location with `N`.
- **Info panel** — optional sidebar (toggle with `Ctrl+p`) displaying name, type, size, permissions, owner/group, and timestamps for the selected item.
- **Sort modes** — cycle through *largest*, *smallest*, *dirs first*, *files first*, *newest* (mtime), *oldest* (mtime), and *recently accessed* (atime) with `s`.
- **Toggle hidden files** — show or hide dotfiles with `H`.
- **Symlink support** — symlinked directories are traversable like real ones, both locally and over SFTP. Links are shown with a 🔗 icon, a distinct cyan italic name, and a trailing `@` (`@/` when the target is a directory) so they stay distinguishable without color.
- **Rename, move and delete** — rename with `R`, move to the other panel with `m`, delete with `D`. Like `mv`, a move within one filesystem is a rename (instant, whatever the file size); a move between two different hosts goes through the transfer queue like a copy — with per-file progress, rate and ETA in the transfer panel — and removes each source only once its copy has landed. If a name is already taken at the destination you're asked whether to overwrite it, skip that file, or cancel the whole move. All of these work on remote panels too.
- **Change root** — jump to any directory with `gd` without losing your sort and view settings. The picker's path field and shortcut list are separate panes; `Tab` switches between them, and the focused one is outlined in cyan. Every directory you open is remembered, so a path typed once is a keystroke away next time; `a` prompts for a directory to keep permanently and `d` forgets one. Entries are starred, ordered most-recently-visited first alongside the built-in locations, and stored in `~/.config/myd/hosts.toml`. History is capped at the most recent twenty; anything saved with `a` is pinned and never trimmed.
- **Dual-panel mode** — view two independent directory trees side by side. Start split with `--dual` (or by passing two paths), switch the active panel with `Tab`, and copy (`c`) or move (`m`) tagged/selected files into the other panel's directory. Toggle the split on and off any time with `|`.
- **Persistent view preferences** — your chosen view (tree or treemap) and info-panel visibility stay put as you move between directories.
- **Progress overlays** — directory scans show a live files / directories / size count, and large copy or delete operations show an item-by-item progress bar.
- **Remote browsing over SFTP** — connect to a remote host with `gr` (or launch with `myd sftp://[user@]host[:port][/path]`) and browse it in the active panel. Pair it with dual-panel mode (`|`) to put a remote and a local tree side by side for copying. Remote trees are fully manageable: create directories (`N`), rename (`R`), delete (`D`) and move (`m`) all act on the server. Authentication uses your existing SSH setup — `ssh-agent`, `~/.ssh` keys (with a passphrase prompt for encrypted keys), and a password fallback — and honors `~/.ssh/config` aliases and `known_hosts`. No credentials are stored. Other protocols can be added without touching the UI.
- **Dialing directory** — `gr` offers your three most recently used remote locations (most recent first); `gs` opens the full saved list with vi navigation (`j`/`k`/`g`/`G`), `/` to search, and `a`/`e`/`d` to add, edit and delete entries. Saved to `~/.config/myd/hosts.toml`, which you can edit by hand. **Passwords are never stored** — an entry holds only where to connect and as whom, and authentication still goes through `ssh-agent`, your `~/.ssh` keys, or a prompt.
- **Mouse support** — scroll with the wheel, click to focus a panel and select a row or treemap tile, right-click to open a directory. Press `Ctrl+N` to release the mouse when you want your terminal's own text selection back.
- **Non-blocking transfers** — copy (`c`) tagged files between a remote panel and a local one and the transfer runs in the background: the interface stays fully interactive, so you can keep browsing and queue more. Up to 16 transfers run at once and the rest stack up, and the files within a directory are copied concurrently — which is what makes a folder of small files usable over a high-latency link.
- **Transfer panel** — a right-hand sidebar that appears once you start a copy (and stays while transfers remain) showing queued, active, and finished transfers with per-transfer progress bars, transfer rate, and ETA. `Tab` reaches it after the browser panels, then wraps back to the first; `j`/`K` move between transfers and `k` (or a double-click) cancels the selected one after a confirmation. Toggle it any time with `Ctrl+t`; cancel everything outstanding with `gx`. Quitting with transfers still running asks for confirmation first (`Ctrl+C` still force-quits).
- **Fast SFTP engine** — transfers are built around round trips rather than bandwidth, which is what governs a long link. Large files move through a deep pipeline of concurrent positioned reads (and, for uploads, positioned writes), the SSH channel window is sized so it isn't itself the ceiling, and Nagle is disabled. Against a simulated 150 ms link: downloads 12.7 → 37.9 MiB/s, uploads 1.3 → 14.9 MiB/s. Reproduce with `cargo test --release --test bench_transfer -- --ignored --nocapture`, or compare against the `sftp` binary on your own link with `scripts/compare_sftp.sh user@host 512M`.

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
# Start in the current directory (press `gd` for the directory picker)
myd

# Start at a specific path
myd ~/Documents
myd /var/log

# Dual-panel mode — two independent views side by side
myd --dual                    # split; left panel picks a directory
myd ~/Documents --dual        # left panel at ~/Documents, right picks a directory
myd ~/Documents ~/Downloads   # two paths implies dual: left and right roots

# Remote browsing over SFTP — opens the remote in a panel beside your local files
myd sftp://prod                       # host from ~/.ssh/config, auth via agent/keys
myd sftp://user@host:2222/var/log     # explicit user, port, and starting path
# ...or connect from inside the app with `gr`.
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
| `Ctrl+f`  | Page down (full screen)       |
| `Ctrl+b`  | Page up (full screen)         |
| `Ctrl+d`  | Half page down                |
| `Ctrl+u`  | Half page up                  |
| `g u`     | Go to parent directory        |
| `g d`     | Change root directory         |
| `0`       | Collapse all                  |
| `*`       | Expand all                    |

### File Operations

| Key       | Action                                 |
|-----------|----------------------------------------|
| `D`       | Delete tagged / selected (confirmation)|
| `R`       | Rename                                  |
| `N`       | Create a new directory here            |
| `r`       | Refresh tree                            |

### Tagging & Selection

| Key       | Action                                        |
|-----------|-----------------------------------------------|
| `t`       | Tag / untag the file under the cursor          |
| `V`       | Visual mode — sweep `j`/`k` to tag a range     |
| `U`       | Untag everything                               |
| `c`       | Copy tagged / selected files                   |
| `m`       | Move tagged / selected files to the other panel |
| `D`       | Delete tagged / selected files                 |

Tagged files are highlighted; `c`, `m` and `D` operate on the whole tagged set (or the file under the cursor when nothing is tagged).

### Search & Filter

| Key       | Action                                     |
|-----------|--------------------------------------------|
| `/`       | Search names by regex (case-insensitive)   |
| `n`       | Jump to the next match (down the tree)     |
| `p`       | Jump to the previous match (up the tree)   |
| `f`       | Filter the tree by regex                   |

Search wraps around at the ends. Filtering hides non-matching entries at every level of the tree; an empty pattern (or `Esc`) clears it. While a filter is active the title bar reads `FILTERED` and the footer shows the pattern, so a masked view is never mistaken for the real contents. Both are case-insensitive, and a pattern that isn't valid regex is reported instead of being silently ignored.

### View

| Key       | Action                        |
|-----------|-------------------------------|
| `v`       | Toggle tree / treemap view    |
| `s`       | Cycle sort mode               |
| `H`       | Toggle hidden files           |
| `b`       | Toggle size bars              |
| `P`       | Toggle permissions column     |
| `T`       | Toggle modification-time column |
| `Ctrl+p`  | Toggle info panel             |
| `?` / `F1`| Help                          |

### Panels

| Key       | Action                                          |
|-----------|-------------------------------------------------|
| `\|`      | Toggle single / dual-panel layout               |
| `Tab`     | Rotate focus: each panel, then the transfer panel |
| `c`       | Copy tagged / selected into the other panel      |

A copy where one panel is remote runs as a background **transfer** instead of a blocking copy — see below.

### Remote & transfers

| Key       | Action                                          |
|-----------|-------------------------------------------------|
| `gr`      | Connect — your recent hosts, or type an address  |
| `gs`      | The dialing directory (all saved hosts)          |
| `Ctrl+t`  | Show / hide the transfer panel                  |
| `gx`      | Cancel all queued and in-flight transfers       |

Inside the dialing directory: `j`/`k`/`g`/`G` navigate, `Enter` connects, `/` searches, `a` adds, `e` edits, `d` deletes, `Esc` closes. Hosts are ordered most-recently-connected first, so whatever you just used is at the top. Entries live in `~/.config/myd/hosts.toml` and hold no passwords — only where to connect and as whom.

The transfer panel appears once you start a copy and lists queued, active, and finished transfers with per-transfer progress, rate, and ETA. Up to 16 transfers run at once; the rest wait their turn. Within a directory copy, files and subdirectories are also transferred concurrently, so a deep tree of small files is not paced by round-trip latency. The interface stays interactive throughout, so you can keep browsing and queue more. Toggle the panel any time with `Ctrl+t`.

Creating directories (`N`), renaming (`R`), deleting (`D`) and moving (`m`) all work on remote panels too, routed through the same backend the panel is browsing. The info panel shows what the directory listing provides — size and timestamps — since owner, group, and creation time aren't part of it, and remote directory sizes cannot be measured cheaply because a `du`-style walk would be one round trip per directory. Such directories therefore show a dash (`—`) with an empty size bar and sort as *unknown* (grouped last) rather than as small — reporting the directory inode's own ~4 KB would deceive both the eye and the sort order.

### Screen-level

| Key       | Action                        |
|-----------|-------------------------------|
| `q`       | Quit immediately              |
| `Esc`     | Quit                          |
| `Ctrl+o`  | Go back to the parent directory |
| `r` / `Ctrl+r` | Rescan (refresh sizes from disk) |
| `Ctrl+p`  | Toggle info panel             |

### Mouse

| Action        | Effect                                        |
|---------------|-----------------------------------------------|
| Wheel         | Scroll the focused view                       |
| Left click    | Focus a panel, select a row or treemap tile   |
| Right click   | Select and open (enter a directory)           |
| `Ctrl+N`      | Release the mouse for terminal text selection |

Mouse capture takes over your terminal's own click-drag selection. `Ctrl+N` hands it back and re-grabs it; most terminals also honour Shift+drag while captured.

## Tuning transfers

Every performance-relevant setting is an environment variable, so a slow link can be diagnosed without a rebuild:

| Variable                  | Default | What it controls                                  |
|---------------------------|---------|---------------------------------------------------|
| `MYD_SSH_WINDOW`          | 64 MiB  | SSH channel window — the hard ceiling on in-flight data (`window / rtt`) |
| `MYD_SSH_NODELAY`         | `true`  | Disable Nagle on the SSH socket                   |
| `MYD_SFTP_WRITE_LIMIT`    | 16 MiB  | Outstanding bytes on the sequential write path    |
| `MYD_SFTP_MAX_PENDING`    | 1024    | SFTP requests in flight on one connection         |
| `MYD_CHUNK_SIZE`          | 256 KiB | Bytes per chunk (one wire request on the parallel path) |
| `MYD_CHUNKS_IN_FLIGHT`    | 32      | Concurrent chunk reads within one large file      |
| `MYD_MAX_PARALLEL`        | 16      | Concurrent transfers / files per directory level  |
| `MYD_GLOBAL_CONCURRENCY`  | 192     | Ceiling on concurrent operations in a recursive copy |

## Diagnostics

`MYD_TRACE=1` writes a log to `~/.cache/myd-trace.log` (override with `MYD_TRACE_FILE`). `MYD_LOG` takes a filter directive instead — `MYD_LOG=myd::transfer=debug` for just the transfer engine — and `MYD_LOG_FORMAT=json` makes it machine-readable.

The log records connection timing, the SFTP read limit the server negotiated, and one line per file with the path taken, chunk size, window depth and achieved rate. Per-chunk latencies are accumulated into a histogram and summarised once per file, so tracing never becomes the bottleneck it is measuring.

`myd-transfer` runs a single transfer with no TUI, for timing without the render loop in the way:

```bash
myd-transfer sftp://user@host/big.bin /tmp/big.bin
scripts/compare_sftp.sh user@host 512M    # myd vs the native sftp client
```

## Sort Modes

Press `s` to cycle through:

1. **Largest** — sorted by descending size (recursive directory sizes).
2. **Smallest** — sorted by ascending size (recursive directory sizes).
3. **Dirs first** — directories listed before files, alphabetical within each group.
4. **Files first** — files listed before directories, alphabetical within each group.
5. **Newest** — most recently modified first (mtime).
6. **Oldest** — least recently modified first (mtime).
7. **Recently accessed** — most recently accessed first (atime).

The time-based sorts use the timestamps from the directory listing, so they work identically on local and remote (SFTP) trees with no extra latency.

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

## Tagging & Multi-File Operations

Most operations act on a single file — the one under the cursor. To act on several at once, **tag** them first:

- Press **`t`** to tag (or untag) the file under the cursor. Tagged rows are highlighted with a bright marker so their staged state is obvious.
- Press **`V`** to enter **visual mode**, then move with `j`/`k` to sweep-tag a whole range. Leaving visual mode keeps the tags, so you can re-enter it elsewhere to tag files that aren't next to each other.
- Press **`U`** to clear every tag.

Once files are tagged, **`c`** (copy) and **`D`** (delete) operate on the entire tagged set instead of just the cursor. Copying tagged files into another directory prompts once per name collision; deleting asks for a single confirmation. Tags are cleared automatically when the operation completes. When nothing is tagged, `c` and `D` fall back to the file under the cursor.

Large copies and deletes show a progress overlay with an item-by-item count and bar.

## Search & Filter

Press **`/`** to search entry names with a regular expression (case-insensitive). The cursor jumps to the first match; **`n`** and **`p`** then step to the next and previous matches, wrapping around at the ends.

Press **`f`** to *filter* the tree: enter a regex and only entries whose names match remain visible, at every level. A directory is kept when something beneath it matches, so matching files stay reachable. Filtering is a view mask — the tree data is untouched — and an empty pattern (or `Esc`) restores the full listing. An active filter is always visible: the title bar reads `FILTERED` and shows how many entries are shown rather than how many exist, and the footer carries the pattern.

## Dual-Panel Mode

Run `myd` with `--dual` (or pass two directory paths) to view two independent trees side by side. Each panel keeps its own root, cursor, expansion state, and navigation history — everything you do (navigate, sort, toggle hidden, switch to the treemap) applies only to the active panel.

```
+---------------------------+---------------------------+
| File Tree (~/Documents)   | File Tree (~/Downloads)   |  <- active panel:
|   projects                |   installers              |     bright border
| > report.pdf              |   archive.zip             |     (other: dimmed)
|   notes.txt               |   photo.jpg               |
+---------------------------+---------------------------+
```

- **`Tab`** rotates focus through every visible pane — each panel left to right, then the transfer sidebar — and wraps around; the focused pane has a bright border, the others dimmed.
- **`|`** toggles the split. Splitting opens the new panel at the active panel's current directory and focuses it; unsplitting drops the inactive panel and keeps the one you were on.
- **`c`** copies the tagged files (or, if none are tagged, the item under the cursor) in the active panel into the **directory the other panel is currently viewing**. Each name collision is confirmed individually. Directories are copied recursively, and the destination panel refreshes automatically when the copy completes.
- **`m`** moves them instead. Within one filesystem that's a rename, so it finishes instantly however large the files are. Between two different hosts the bytes are copied through the transfer queue and each source is removed only once its own copy has landed — a failed or cancelled copy leaves the source untouched. A name already taken at the destination prompts you to overwrite it, skip that file, or cancel the whole move.

Each panel can independently change its root (`gd`), so you can, for example, browse a backup on one side and your working tree on the other, then copy files across with a single `c`.

## License

MIT
