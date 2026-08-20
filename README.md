# myd

A **vi-like terminal file browser** built with [ratatui](https://ratatui.rs/) and [crossterm](https://crates.io/crates/crossterm).

Navigate your filesystem with familiar `vi` key bindings, inspect file details in a live sidebar, and visualize disk usage with proportional size bars — all from your terminal.

## Features

- **vi-style navigation** — `j`/`k` to move, `h`/`l` to collapse/expand, `gg`/`G` to jump to top/bottom, and more.
- **File tree with size visualization** — each entry shows a proportional size bar (green / amber / red) and human-readable file size.
- **Treemap view** — switch to a squarified treemap (`v`) that visualizes disk usage as proportional boxes, colored by file type so related content reads as one group. Navigate it with the same `hjkl` keys as the tree, `Enter` to open a directory, and `v` again to return to the tree with the same entry highlighted. Directory tiles end in a `/`.
- **Type-colored tiles** — each treemap tile is filled by content category (code, docs, images, video, audio, archives, data, binaries); directories take the color of whatever content dominates them, and a legend in the status bar names the colors on screen.
- **Cached sizes** — drilling into a subdirectory reuses the sizes already computed instead of rescanning the disk; press `r` for a manual rescan.
- **Tag and act on multiple files** — tag files with `t`, sweep a range in visual mode (`V`), then copy (`c`), move (`m`), delete (`D`) or bulk-rename (`gr`) every tagged file at once. Tagging works in the treemap as well as the tree, and both share one set of tags, so switching views with `v` never changes what is staged. Tagged rows are highlighted and tagged tiles carry a `▶` marker and a green outline. Untag one with `t`, all with `U`.
- **Patterned rename** — `gr` renames every tagged file by regex, with `$1` for the first capture group. The dialog previews the pattern against the first tagged file before it is applied, and files the pattern doesn't match are skipped rather than treated as failures.
- **Regex search & filter** — search names with `/` (regex, case-insensitive) and step through matches with `n` / `p`; `f` filters the whole tree to a regex, hiding everything that doesn't match. A malformed pattern is reported rather than ignored.
- **Create directories** — make a new directory in the current location with `N`.
- **Info panel** — optional sidebar (toggle with `Ctrl+p`) displaying name, type, size, permissions, owner/group, and timestamps for the selected item. `Tab` moves focus into it to edit permissions, owner and group — over the tagged files, or recursively through a directory — and `<` / `>` resize it. When there is room, the lower part previews the selected file, with `+` / `-` moving the split between the two.
- **File preview** — press `space` to open a pane over most of the screen showing what is actually *in* the selected file, and `space` again to close it. Text, markdown and around a hundred programming languages are syntax highlighted. The pane takes focus, so vi motions act on it rather than on the tree: `j`/`k` by a line, `Ctrl+F`/`Ctrl+B` by a page, `Ctrl+D`/`Ctrl+U` by half of one, `g`/`G` to the start and end. `/` searches within the file (regex, case-insensitive) and `n`/`p` step through the matches — `N`/`P` go the other way — with the current match highlighted and the count shown in the footer. `Esc` or `q` hands focus back to the tree without closing the pane, so you can move the cursor and watch the preview follow; `Tab` moves focus in and out. Binary files are reported rather than dumped, and a file too large to read in full is shown truncated and labelled as such. Works on remote panels: the file is read from the server, not from a same-named path on your own machine.
- **Image and PDF preview** — if [`timg`](https://timg.sh/) or [`chafa`](https://hpjansson.org/chafa/) is on your `PATH`, the preview pane draws images (PNG, JPEG, GIF, WebP, SVG, TIFF, …); `timg` is preferred where both are installed. A `timg` built with poppler also renders **PDFs** — `chafa` has no PDF loader, so a PDF on a chafa-only machine says so rather than failing obscurely. Neither tool is a dependency: without them the pane shows the file's details instead.
- **High-resolution images where the terminal allows** — in a terminal that implements the kitty graphics protocol (kitty, ghostty, Konsole 22.04+), iTerm2's inline images (iTerm2, WezTerm, mintty) or **sixel**, the image is handed over as real pixel data instead of being approximated with block characters. myd asks the terminal directly at startup what it supports, falling back to environment variables when there is nothing to ask. Detection is deliberately conservative, because a terminal that does not understand the sequence *prints* it — anything unrecognized falls back to blocks, which work everywhere. Override with `MYD_PREVIEW_GRAPHICS=kitty|iterm2|sixel|blocks`.
- **Works inside tmux** — tmux parses sixel itself, so sixel is preferred there and needs no configuration beyond telling tmux your terminal supports it. The kitty and iTerm2 protocols instead ride tmux's passthrough escape, which is off by default; see *Terminal graphics* below for the two settings involved. When myd has to fall back, the preview footer says which setting would change that rather than leaving you guessing.
- **Page through a PDF** — with a multi-page document open, `j`/`k` turn pages rather than scrolling (a rendered page is drawn to fit and has nothing to scroll); `g`/`G` jump to the first and last page, and the footer shows `page 5/19`. The page count comes from `pdfinfo` when poppler-utils is installed; without it you can still page forward, just without a known end.
- **Archive browsing** — press `space` on a `.zip`, `.tar.gz`, `.7z`, `.rar` (and the rest) to list what is inside it, with the path exactly as the archive stores it and the permissions where the format records them. Press `Enter` to go *in*: the archive becomes an ordinary-looking tree where tagging, filtering, searching, sorting, the treemap and the preview pane all work, and `c` extracts. Directory sizes inside an archive are real recursive totals, because every member's size is already in the index. Archives are read-only here, which the pane says in its title and footer — `D`, `R`, `N` and `m` refuse rather than half-working.
- **Sort modes** — cycle through *largest*, *smallest*, *dirs first*, *files first*, *newest* (mtime), *oldest* (mtime), and *recently accessed* (atime) with `s`, or pick one outright by its number: `5` is "sort by newest" on its own, and `gs` opens the numbered menu those digits come from (so `gs5` does the same thing the slower way, and clicking the `Sort:` indicator opens the same menu).
- **Toggle hidden files** — show or hide dotfiles with `H`.
- **Symlink support** — symlinked directories are traversable like real ones, both locally and over SFTP. Links are shown with a 🔗 icon, a distinct cyan italic name, and a trailing `@` (`@/` when the target is a directory) so they stay distinguishable without color.
- **Rename, move and delete** — rename with `R`, move to the other panel with `m`, delete with `D`. Like `mv`, a move within one filesystem is a rename (instant, whatever the file size); a move between two different hosts goes through the transfer queue like a copy — with per-file progress, rate and ETA in the transfer panel — and removes each source only once its copy has landed. If a name is already taken at the destination you're asked whether to overwrite it, skip that file, or cancel the whole move. All of these work on remote panels too.
- **One place to go anywhere** — `gd` lists your directories and saved remote hosts in one searchable picker; `Enter` opens a directory or connects to a host, whichever you are on. The path field takes a local path *or* an `sftp://[user@]host[:port][/path]` URL, so an address you have not saved is just something to type. The picker's path field and shortcut list are separate panes; `Tab` switches between them, and the focused one is outlined in cyan. Every directory you open *through the picker* is remembered — typed or picked from the list — so a path typed once is a keystroke away next time (browsing the tree does not fill the list); `a` prompts for a destination to keep — a path, or a `label = sftp://…` line to save a host — `d` forgets one, `e` edits a host, and `/` searches the whole list: narrow it to a single row and `Enter` opens that row immediately, otherwise `Enter` keeps the filtered list to pick from. `p` pins an entry to a block at the top of the list in an order you arrange (`m` to reorder — it pins the entry first if it isn't already — and `u` to unpin), with saved and recently-visited entries below. Stored in `~/.config/myd/hosts.toml`, which is seeded with the usual locations (home, Documents, Downloads, `/`, …) on first run — they are ordinary entries from then on, so you can pin, reorder or delete them. History is capped at the most recent twenty; anything saved or pinned is never trimmed.
- **Dual-panel mode** — view two independent directory trees side by side. Start split with `--dual` (or by passing two paths), switch the active panel with `Tab`, and copy (`c`) or move (`m`) tagged/selected files into the other panel's directory. Toggle the split on and off any time with `|`.
- **Persistent view preferences** — your chosen view (tree or treemap) and info-panel visibility stay put as you move between directories.
- **Progress overlays** — directory scans show a live files / directories / size count, and large copy or delete operations show an item-by-item progress bar.
- **Remote browsing over SFTP** — connect from the `gd` picker (pick a saved host, or type an `sftp://` URL into its path field) or launch with `myd sftp://[user@]host[:port][/path]`, and browse it in the active panel. A remote pane titles itself `SFTP (path)` in violet rather than `File Tree` in the ordinary colour, so on a split you can tell at a glance which machine a pane is showing. Pair it with dual-panel mode (`|`) to put a remote and a local tree side by side for copying. Remote trees are fully manageable: create directories (`N`), rename (`R`), delete (`D`) and move (`m`) all act on the server. Authentication uses your existing SSH setup — `ssh-agent`, `~/.ssh` keys (with a passphrase prompt for encrypted keys), and a password fallback — and honors `~/.ssh/config` aliases and `known_hosts`. No credentials are stored. Other protocols can be added without touching the UI.
- **Waiting happens in one pane, not the whole window** — a connection attempt, a move, a copy, or an archive being indexed shows its spinner *inside the pane doing the work*, and the other pane stays fully usable: `Tab` over to it and keep browsing while a slow host is still answering. `q` or `Esc` on the waiting pane abandons that operation and puts back what it was showing. Only a question that has to be answered — a password prompt, an overwrite confirmation — takes the whole window. And if you have moved to the other pane, a connection landing opens quietly in its own pane rather than pulling the keyboard back mid-keystroke.
- **Saved hosts** — kept in the same `gd` picker as your directories, most recently connected first, with vi navigation (`j`/`k`/`g`/`G`), `/` to search, and `a`/`e`/`d` to add, edit and delete entries. Saved to `~/.config/myd/hosts.toml`, which you can edit by hand. **Passwords are never stored** — an entry holds only where to connect and as whom, and authentication still goes through `ssh-agent`, your `~/.ssh` keys, or a prompt.
- **Shallow traversal** — press `S` to browse without measuring directory sizes, or start that way with `--shallow` (`-s`), which applies to both panes of a split. The recursive walk is the slowest thing myd does, and over a large archive or a network mount it is rarely worth waiting for just to look around; unmeasured directories show a dash and sort last, exactly as remote ones do. Turning measuring back on asks first, and the choice is remembered per directory — `S` in the `gd` picker sets it for a saved directory without opening it. The flag combines with `-d`, carrying through the picker to whatever you choose from it, and the picker's path field names the mode a path will open in — `shallow` or `full` — so which traversal you'll get is visible before `Enter`.
- **Open with the desktop** — press `o` to hand the selected file or directory to the system's default application (`open` on macOS, `xdg-open` on Linux and the BSDs). The launcher runs detached, so myd stays responsive and nothing it prints disturbs the display.
- **Open with a program you name** — press `O` for a dialog that asks for a command line, then runs it over your selection: type `vim -v` and it runs `vim -v` on every tagged file, passed as absolute paths after your own arguments. myd steps off the screen while the program has it and takes it back when the program exits, so a full-screen editor or pager behaves exactly as it would from the shell; a non-zero exit status is reported rather than vanishing with the screen. The command is split honouring quotes but is **not** passed to a shell — no globbing, no `$VAR`, and so no filename can ever be read as syntax. A bare name is looked up on `PATH`; anything with a `/` is used as written. The last command you ran is offered back the next time you press `O`.
- **Mouse support** — scroll with the wheel, click to focus a panel and select a row or treemap tile, right-click to open a directory. Press `Ctrl+N` to release the mouse when you want your terminal's own text selection back.
- **Non-blocking transfers** — copy (`c`) tagged files between a remote panel and a local one and the transfer runs in the background: the interface stays fully interactive, so you can keep browsing and queue more. Up to 16 transfers run at once and the rest stack up, and the files within a directory are copied concurrently — which is what makes a folder of small files usable over a high-latency link.
- **Transfer panel** — a right-hand sidebar that appears once you start a copy (and stays while transfers remain) showing queued, active, and finished transfers with per-transfer progress bars, transfer rate, and ETA. `Tab` reaches it after the browser panels, then wraps back to the first; `j`/`k` move between transfers and `K`, `Del` or `Backspace` (or a double-click) cancels the selected one after a confirmation. `C` clears the finished ones out of the list. Toggle it any time with `Ctrl+t`; cancel everything outstanding with `gx`. Quitting with transfers still running asks for confirmation first (`Ctrl+C` still force-quits).
- **Fast SFTP engine** — transfers are built around round trips rather than bandwidth, which is what governs a long link. Large files move through a deep pipeline of concurrent positioned reads (and, for uploads, positioned writes), the SSH channel window is sized so it isn't itself the ceiling, and Nagle is disabled. Against a simulated 150 ms link: downloads 12.7 → 37.9 MiB/s, uploads 1.3 → 14.9 MiB/s. Reproduce with `cargo test --release --test bench_transfer -- --ignored --nocapture`, or compare against the `sftp` binary on your own link with `scripts/compare_sftp.sh user@host 512M`.

## Requirements

- Rust 1.75+ (stable)
- A terminal with true color support
- Optional: [`timg`](https://timg.sh/) or [`chafa`](https://hpjansson.org/chafa/)
  for image previews in the preview pane. Neither is required — without them the
  pane falls back to showing the file's details. PDF previews need `timg` built
  with poppler (`timg --version` lists it); `chafa` cannot render PDFs.
- Optional: `pdfinfo` (poppler-utils) so the preview knows how many pages a PDF
  has. Without it you can still page forward, just without a known last page.
- Optional, for pixel-resolution images rather than block approximations: a
  terminal implementing the kitty graphics protocol, iTerm2 inline images, or
  sixel. Inside `tmux` see *Terminal graphics* below — sixel needs one setting,
  and the other two protocols need a different one.

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

# Choose from your saved directories and hosts instead of opening a path
myd --directory               # or -d, matching the gd chord

# Skip measuring directory sizes — handy on a network mount or a huge archive
myd --shallow /mnt/archive    # or -s; applies to both panes of a split
myd -s -d                     # carries through the picker to whatever you pick

# Dual-panel mode — two independent views side by side
myd --dual                    # split; left panel picks a directory
myd ~/Documents --dual        # left panel at ~/Documents, right picks a directory
myd ~/Documents ~/Downloads   # two paths implies dual: left and right roots

# Remote browsing over SFTP — the remote gets the window unless you ask to split
myd sftp://prod                       # host from ~/.ssh/config, auth via agent/keys
myd sftp://user@host:2222/var/log     # explicit user, port, and starting path
myd /tmp sftp://prod                  # split: local left, remote right (either order)
myd --dual sftp://prod                # split: remote left, current directory right
# ...or connect from inside the app with `gd`.
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
| `D`       | Delete tagged / selected (`y` / `n` / `a` to stop asking) |
| `R`       | Rename                                  |
| `gr`      | Rename every tagged file by regex       |
| `N`       | Create a new directory here            |
| `S`       | Browse without measuring directories    |
| `o`       | Open with the system default app        |
| `O`       | Open with a program you name             |
| `r`       | Refresh tree                            |

### Tagging & Selection

| Key       | Action                                        |
|-----------|-----------------------------------------------|
| `t`       | Tag / untag the selection (tree **or** treemap) |
| `V`       | Visual mode — sweep `j`/`k` to tag a range (tree only) |
| `U`       | Untag everything                               |
| `c`       | Copy tagged / selected files                   |
| `m`       | Move tagged / selected files to the other panel |
| `D`       | Delete tagged / selected files                 |
| `gr`      | Rename every tagged file by regex              |

Tagged files are highlighted; `c`, `m`, `D` and `gr` operate on the whole tagged set (or the file under the cursor when nothing is tagged).

`t` works in both views, and the two share a single set of tags — switching with `v` never changes what is staged. `V` is the exception: a visual range is a span of consecutive rows, and the treemap's tiles have no such order, so it applies in the tree view only and says so if pressed in the treemap.

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
| `1`-`8`   | Sort by that entry of the sort menu (`5` = newest) |
| `gs`      | Pick a sort order from a numbered menu (`gs5` = newest) |
| `H`       | Toggle hidden files           |
| `B`       | Toggle size bars              |
| `P`       | Toggle permissions column     |
| `T`       | Toggle modification-time column |
| `Ctrl+p`  | Toggle info panel             |
| `Tab`     | Focus the info panel (then `j`/`k`, `Enter` to edit, `<`/`>` and `+`/`-` to resize) |
| `Ctrl+l`  | Redraw the screen             |
| `space`   | Toggle the file preview pane  |
| `?` / `F1`| Help                          |

### Preview pane

Keys that act on the pane while it has focus. `space` opens it and gives it
focus; `Esc` or `q` hands focus back to the tree while leaving the pane open.

| Key               | Action                                      |
|-------------------|---------------------------------------------|
| `space`           | Open / close the preview                    |
| `j` / `k`         | Scroll a line; a PDF turns a page; an image moves to the next file |
| `Ctrl+F` / `Ctrl+B` | Scroll a page (a PDF turns a page; an image moves the tree) |
| `Ctrl+D` / `Ctrl+U` | Scroll half a page                        |
| `g` / `G`         | Jump to the start / end (first / last page) |
| `/`               | Search within the file (regex)              |
| `n` / `p`         | Next / previous match                       |
| `N` / `P`         | Previous / next match                       |
| `q`               | Close the preview                           |
| `Esc`             | Return focus to the tree, keep the pane open |
| `Tab`             | Move focus in and out of the pane           |

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
| `gd`      | Go to — saved directories and hosts, or type an address |
| `Ctrl+t`  | Show / hide the transfer panel                  |
| `gx`      | Cancel all queued and in-flight transfers       |

Inside the picker: `j`/`k`/`g`/`G` navigate, `Enter` opens a directory or connects to a host, `/` searches, `a` adds, `e` edits, `d` deletes, `Esc` closes. Typing an `sftp://[user@]host[:port][/path]` URL into the path field connects to an address you have not saved. Hosts are listed after the directories, most-recently-connected first, so whatever you just used is at the top. Entries live in `~/.config/myd/hosts.toml` and hold no passwords — only where to connect and as whom.

The transfer panel appears once you start a copy and lists queued, active, and finished transfers with per-transfer progress, rate, and ETA. Up to 16 transfers run at once; the rest wait their turn. Within a directory copy, files and subdirectories are also transferred concurrently, so a deep tree of small files is not paced by round-trip latency. The interface stays interactive throughout, so you can keep browsing and queue more. Toggle the panel any time with `Ctrl+t`.

Creating directories (`N`), renaming (`R`), deleting (`D`) and moving (`m`) all work on remote panels too, routed through the same backend the panel is browsing. Permissions and ownership can be changed on a remote panel too, through the info panel: SFTP carries both. The info panel otherwise shows what the directory listing provides — size, permissions, and timestamps — since owner, group, and creation time aren't part of it, and remote directory sizes cannot be measured cheaply because a `du`-style walk would be one round trip per directory. Such directories therefore show a dash (`—`) with an empty size bar and sort as *unknown* (grouped last) rather than as small — reporting the directory inode's own ~4 KB would deceive both the eye and the sort order.

### Screen-level

| Key       | Action                        |
|-----------|-------------------------------|
| `q` / `Esc` | Back out of the current state, or quit    |
| `Ctrl+o`  | Go back to the parent directory |
| `r` / `Ctrl+r` | Rescan (refresh sizes from disk) |
| `Ctrl+p`  | Toggle info panel             |

`q` and `Esc` undo one thing at a time before they quit: they cancel a running scan, close the `gd` picker, clear an active filter, and leave an archive — innermost first, so a filter inside an archive takes two presses. With nothing left to back out of, they quit (asking first if transfers are still running; `Ctrl+C` force-quits regardless).

### Mouse

| Action        | Effect                                        |
|---------------|-----------------------------------------------|
| Wheel         | Scroll the focused view                       |
| Left click    | Focus a panel, select a row or treemap tile   |
| Right click   | Select and open (enter a directory)           |
| `Ctrl+N`      | Release the mouse for terminal text selection |

With the preview open, a left click advances it like `j` — turning the page of a document, or stepping to the next file when you are looking at an image. A click-*drag* is ignored: the gesture is decided when the button comes up, so dragging across a picture does not page past it.

Mouse capture takes over your terminal's own click-drag selection. `Ctrl+N` hands it back and re-grabs it; most terminals also honour Shift+drag while captured.

## Tuning transfers

Every performance-relevant setting is an environment variable, so a slow link can be diagnosed without a rebuild:

| Variable                  | Default | What it controls                                  |
|---------------------------|---------|---------------------------------------------------|
| `MYD_SSH_WINDOW`          | 64 MiB  | SSH channel window — the hard ceiling on in-flight data (`window / rtt`) |
| `MYD_SSH_NODELAY`         | `true`  | Disable Nagle on the SSH socket                   |
| `MYD_SSH_MAX_PACKET`      | 32 KiB  | SSH maximum packet size — russh's default; exposed only so it can be ruled out, and raising it risks a disconnect from strict servers |
| `MYD_SFTP_WRITE_LIMIT`    | 16 MiB  | Outstanding bytes on the sequential write path    |
| `MYD_SFTP_MAX_PENDING`    | 1024    | SFTP requests in flight on one connection         |
| `MYD_CHUNK_SIZE`          | 256 KiB | Bytes per chunk (one wire request on the parallel path) |
| `MYD_CHUNKS_IN_FLIGHT`    | 32      | Concurrent chunk reads within one large file      |
| `MYD_MAX_PARALLEL`        | 16      | Concurrent transfers / files per directory level  |
| `MYD_GLOBAL_CONCURRENCY`  | 192     | Ceiling on concurrent operations in a recursive copy |

## Diagnostics

`MYD_TRACE=1` writes a log to `~/.cache/myd-trace.log` (override with `MYD_TRACE_FILE`). `MYD_LOG` takes a filter directive instead — `MYD_LOG=myd::transfer=debug` for just the transfer engine — and `MYD_LOG_FORMAT=json` makes it machine-readable.

The log records connection timing, the SFTP read limit the server negotiated, and one line per file with the path taken, chunk size, window depth and achieved rate. Per-chunk latencies are accumulated into a histogram and summarised once per file, so tracing never becomes the bottleneck it is measuring.

`myd-transfer` runs a single transfer with no TUI, for timing without the render loop in the way. It is one of three development tools kept out of ordinary builds behind the `tools` feature — a release build produces `myd` alone — so build it explicitly:

```bash
cargo build --release --features tools --bin myd-transfer
./target/release/myd-transfer sftp://user@host/big.bin /tmp/big.bin
scripts/compare_sftp.sh user@host 512M    # myd vs the native sftp client
```

The other two are `myd-escape`, which dumps the exact graphics escape the preview pane would send for a file, and `myd-sizeprobe`, which reports which kitty sizing keys a terminal honours. `cargo build --features tools` builds all three.

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

Toggle bars on/off with `B`.

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

Tiles can be tagged with `t` exactly as tree rows can, and the two views share one set of tags — `c`, `m` and `D` act on the same set from either. A tagged tile shows a `▶` marker and a green outline. Visual mode (`V`) is the one exception, and stays tree-only; see *Tagging & Multi-File Operations*.

## Tagging & Multi-File Operations

Most operations act on a single file — the one under the cursor. To act on several at once, **tag** them first:

- Press **`t`** to tag (or untag) the current selection. Tagged rows are highlighted with a bright marker so their staged state is obvious. This works in the **treemap** as well as the tree — a tagged tile carries a `▶` marker and a green outline, so it reads as tagged even when it is too small for a label. Both views share one set of tags, so switching with `v` never changes what is staged.
- Press **`V`** to enter **visual mode**, then move with `j`/`k` to sweep-tag a whole range. Leaving visual mode keeps the tags, so you can re-enter it elsewhere to tag files that aren't next to each other. Visual mode is tree-only: a range is a span of consecutive rows and the treemap's squarified tiles have no such order, so pressing `V` there says so rather than doing nothing.
- Press **`U`** to clear every tag.

Once files are tagged, **`c`** (copy), **`m`** (move), **`D`** (delete) and **`gr`** (patterned rename) operate on the entire tagged set instead of just the cursor. Copying tagged files into another directory prompts once per name collision; deleting asks for a single confirmation (`y`, `n`, or `a` to stop asking for the session). Tags are cleared automatically when the operation completes. When nothing is tagged, these fall back to the file under the cursor.

**`gr`** renames the whole tagged set by regex: enter a pattern and a replacement, where `$1` is the first capture group, and `Tab` moves between the two fields. The dialog previews the transformation against the first tagged file, so the pattern can be checked before it runs. Files the pattern doesn't match are skipped rather than reported as failures — tagging a mixed set and renaming only part of it is the ordinary case.

Large copies and deletes show a progress overlay with an item-by-item count and bar.

## Search & Filter

Press **`/`** to search entry names with a regular expression (case-insensitive). The cursor jumps to the first match; **`n`** and **`p`** then step to the next and previous matches, wrapping around at the ends.

Press **`f`** to *filter* the tree: enter a regex and only entries whose names match remain visible, at every level. A directory is kept when something beneath it matches, so matching files stay reachable. Filtering is a view mask — the tree data is untouched — and an empty pattern (or `Esc`) restores the full listing. An active filter is always visible: the title bar reads `FILTERED` and shows how many entries are shown rather than how many exist, and the footer carries the pattern.

## File Preview

Press **`space`** to open the preview pane over most of the screen, and `space`
again to close it. The pane shows the file under the cursor and follows it: with
focus handed back to the tree (`Esc` or `q`), moving the cursor re-reads the
preview, so you can walk a directory and watch its contents go by.

**Text** is syntax highlighted — around a hundred languages — using the file's
extension, or its `#!` line when it has no extension. Source files larger than
64 KB are shown as plain text instead: highlighting is linear in the text and a
4300-line file costs about a quarter of a second, which is not worth the colors
when the content is what you opened the pane for.

**Markdown** has its own highlighter rather than going through the general one,
which makes it about four thousand times faster (37µs against 170ms on a 30KB
README) and so exempt from that size limit. The general grammar embeds every other
language so that fenced code blocks can be highlighted in their own syntax; that
is a real feature, and far too slow for something that should feel instant. The
trade is that fenced code is colored as one block instead of per language. Only
the first megabyte of a large file is read, and the footer says `truncated` when
that happened. A **binary** file is reported with its size rather than dumped as
control characters.

**Images** are drawn by [`timg`](https://timg.sh/) or
[`chafa`](https://hpjansson.org/chafa/), whichever is on your `PATH` — `timg`
first, since it renders more formats. With neither installed the pane shows the
file's details and explains what is missing.

### Terminal graphics

How the image reaches the screen depends on the terminal. Three protocols carry
real pixel data:

| Protocol | Terminals | Quality |
|---|---|---|
| kitty | kitty, ghostty, Konsole 22.04+ | best |
| iTerm2 inline images | iTerm2, WezTerm, mintty | best |
| sixel | xterm (`-ti vt340`), foot, contour, mlterm, Windows Terminal, iTerm2 | good — a palette rather than truecolor |

Everywhere else the image is approximated with Unicode block characters, which is
universally safe but visibly blocky, since a block packs four pixels into one cell
and picks two colors for them.

myd **asks the terminal** at startup — a kitty graphics query and a Device
Attributes request, with a 100ms budget — and falls back to environment variables
when there is nothing to ask or nothing answers. Sixel can only be discovered this
way; no environment variable reports it.

Detection errs towards blocks on purpose: a terminal that does not understand a
graphics escape *prints* it, so a wrong guess sprays kilobytes of base64 over the
display. Force a choice with `MYD_PREVIEW_GRAPHICS=kitty`, `iterm2`, `sixel` or
`blocks`.

**Inside tmux** the two mechanisms are different, which is worth knowing because
the obvious setting only fixes one of them:

```sh
# sixel: tmux parses and re-draws it itself. Tell tmux your terminal has it.
set -as terminal-features ',*:sixel'

# kitty / iTerm2: these ride tmux's passthrough escape, which is off by default.
set -g allow-passthrough on
```

Sixel is preferred inside tmux even though the other two look better, because it
is the one that survives a multiplexer cleanly — it is a single escape sequence,
where a kitty image is a chain of dozens that each have to be wrapped. If myd ends
up on blocks anyway, the preview footer names the setting that would change that.

Block output is *captured and drawn as part of the interface* rather than written
to the terminal, so it can never corrupt the display. A graphics image necessarily
does reach the terminal directly: the pane leaves a hole for it and the escape is
written after the frame.

Because the image is not made of cells, myd has to bound it explicitly. The
kitty and iTerm2 escapes are told the size in **cells** (`c=`/`r=`, and
`width=`/`height=` without the `px` suffix) so the picture occupies exactly the
rows the pane reserved — left to itself the renderer describes the image in
pixels and the terminal decides how many rows that is, which is how an image
ends up spilling past its border. Sixel carries no such control, so it is asked
for a smaller raster instead.

myd works out how large a character cell is — from `TIOCGWINSZ` where the
kernel carries pixel dimensions, otherwise by asking the terminal (`ESC[16t`,
answered in the same round trip as the protocol probe) — and states the image's
size in **pixels**. The kernel is preferred because under tmux it describes the
*pane*, where the escape is answered by the outer terminal about its whole
window. The preview footer shows which cell size was used, or says the size is
unknown.
Both pixels and cells are valid in the escape, but only one is unambiguous:
sizing in cells leaves the terminal to multiply out, and iTerm2 reaches a
different answer inside tmux than it does natively — the same image filled a
native window and came out small in a tmux pane. When the terminal does not
report a cell size, the cell form is used as a fallback.

The kitty protocol has no working way to say "draw this in N cells" here — its
`c=`/`r=` keys are ignored by at least one terminal that otherwise implements
the protocol, measured directly. The image is drawn at whatever pixel size the
raster happens to be, so the raster is built to the pane's real pixel box
instead. That is why the cell size matters: get it wrong and the picture either
under-fills the pane or is upscaled until it looks pixelated.

**Inside tmux the kitty protocol is always used**, whatever the terminal's own
protocol is. This is about how the image is *carried*, not what is at the far
end: kitty splits a picture into roughly 4KB chunks, each its own escape, while
iTerm2 sends one sequence however large — and a single large escape is what a
multiplexer will not deliver. `timg` run inside tmux shows the same thing, drawing
nothing for a large photo unless forced to `-pk`. iTerm2 understands kitty's
protocol, so this costs nothing and removes the size ceiling entirely: images
keep full resolution in tmux rather than being shrunk to fit.

The per-escape limit still exists as a backstop, and `MYD_PREVIEW_MAX_PAYLOAD`
sets it in kilobytes (`0` removes it). It is judged per sequence rather than per
image, which is the point of chunking.

Where iTerm2 can decode the file itself — JPEG, PNG, GIF, WebP and the like —
the file's own bytes are sent and the renderer is skipped. That is a size fix
rather than a shortcut: `timg` re-encodes everything as PNG, which is the worst
case for a photograph, and a 537KB JPEG became a 2.3MB payload where the
original costs 716KB and looks better for not being re-encoded. Formats that
need rendering, and the other protocols, still go through `timg`.

Otherwise the raster is asked for at the geometry whose pixel box matches the
pane's real one, so it arrives with enough detail to fill the space and no more. Asking
for a blind multiple of the pane instead is what once drew a tall photo past the
bottom of its pane, and the overflow landed on rows nothing repaints — a blank
rectangle. That matters:
timg cannot ask a piped stdout how large a cell is and assumes 18 pixels, so on
a retina display — where a cell is nearer 30 — an image sized for the smaller
assumption arrives with too few pixels and draws small and soft inside its box.
Images are also rendered with upscaling, or a small picture would sit at its own
pixel size in the middle of a large pane rather than filling it. The oversampling
is capped, since the payload grows with the square of the raster and past a point
the extra pixels are beyond what any cell can show.

Removing an image is protocol-specific too, and redrawing the screen is not
enough for any of them — ratatui writes only the cells whose content changed, and
the cells under an image are ones it believes are already blank. A kitty image is
an object the terminal re-composites and has a real delete operation. iTerm2 and
sixel images belong to the cells they were drawn into and have no delete, so
those rows are erased explicitly and the next frame is a full repaint. All of
this happens when the preview closes, when the selection moves to another file,
and on exit.

A **PDF** needs a `timg` linked against poppler; `chafa` has no PDF loader, so on
a chafa-only machine a PDF says so rather than failing with an obscure error. With
a multi-page document open, `j`/`k` turn pages instead of scrolling — a rendered
page is drawn to fit, so there is nothing to scroll — `g`/`G` jump to the first and
last page, and the footer reads `page 5/19`. The count comes from `pdfinfo`; if
poppler-utils is not installed you can still page forward, without a known end.

The pane is focusable, which is what lets vi motions mean the pane rather than the
tree: `j`/`k`, `Ctrl+F`/`Ctrl+B`, `Ctrl+D`/`Ctrl+U`, `g`/`G`. **`/`** searches
within the file (regex, case-insensitive) and `n`/`p` step through matches with
wraparound — `N`/`P` reverse them — highlighting the current one and showing
`3/11` in the footer. `Tab` moves focus in and out of the pane, and the focused
pane is the one outlined in cyan.

Everything works the same on a remote panel: the file is read from the server over
SFTP, never from a same-named path on your own machine. A remote image is fetched
to a temporary file first (capped at 32 MB) because the renderers take a path
rather than a stream.

## Archives

Press **`space`** on an archive to see what is in it, and **`Enter`** to go in.

The listing shows each member's permissions, size, timestamp and **the path
exactly as the archive stores it** — a member written as `./docs/x.md` is shown
that way, not tidied up, because what is stored is what you are asking about. A
format that records no permissions shows `?---------` rather than a guessed
`0644`. Like any other preview it scrolls, and `/` searches it.

Pressing `Enter` opens the archive as a panel. It is a filesystem from there on:
tag with `t`, filter with `f`, search with `/`, sort with `s`, switch to the
treemap with `v`, preview a file inside it with `space`. Directory sizes are
real recursive totals — unlike a remote panel, an archive already knows every
member's size, so the size bars and the treemap mean something.

**Extracting** is the ordinary copy: `c` into the other pane in a split, or `c`
and a destination in a single pane. Directories extract recursively. It rides
the transfer queue, so a large extract shows progress and does not block the
interface.

**Archives are read-only.** `D`, `R`, `N` and `m` refuse with a message rather
than appearing to work — the pane says so in its title, which names the archive
and tints amber-brown, and in a `📦 ARCHIVE (read-only)` badge in the footer.
Leaving with `h` puts you back where you were, with the panel retagged so the
next delete addresses your own disk again.

### Creating an archive

**`gz`** packs the tagged files — or, with nothing tagged, whatever the cursor is
on — into a new archive. A dialog asks for the name and offers four formats:

| Format | | |
|---|---|---|
| **zip** | widest support | the default |
| **7z** | smallest, slower | written in process, no `7z` binary needed |
| **tar** | no compression | |
| **tgz** | tar plus gzip | |

`Tab` moves between the name, the formats and the buttons; `↑`/`↓` choose a
format, or press `1`–`4`. Changing the format renames the extension to match,
until you edit the name yourself. `Enter` creates it.

The archive lands in **the directory at the top of the tree** — what the panel
is rooted at — however deep the cursor has wandered into it. Archiving
`code/booker2` from a pane rooted at `code` puts `booker2.zip` in `code`, beside
the directory it holds rather than inside it. (This is deliberately not where
`N` creates a directory: `N` follows the cursor, because there the cursor marks
the destination. Here it marks the source.)

A selected directory goes in under its own name, so archiving `src/` gives you
an archive containing `src/main.rs`, and unpacking it recreates `src/` rather
than scattering its contents.

Writing streams, so the size of what you are packing is not bounded by memory,
and it runs in the panel — the other pane stays usable, and `q` stops it. An
interrupted or failed write leaves nothing behind: the archive is built in a
`.part` file beside the destination and renamed only once it is complete, so
there is never a half-written file wearing a real archive's name.

**No RAR.** Creating one needs RARLAB's proprietary `rar` tool; the format is
patent-encumbered and no free library writes it, libarchive included. myd
*reads* RAR fine (see below) — it just cannot make one.

The default format is configurable — see [Preferences](#preferences) below.

### Formats

Many more formats can be read than written — the list below is what myd
*reads*; `gz` writes the four in [Creating an archive](#creating-an-archive).

Read in-process, with no external tools: **zip** (and `.jar`, `.apk`, `.whl`,
`.xpi`, …), **tar** and its compressed forms (`.tar.gz`/`.tgz`, `.tar.bz2`,
`.tar.xz`, `.tar.zst`), single compressed files (`.gz`, `.bz2`, `.xz`, `.zst`,
shown as the one member they hold), and **7z**.

**Comic book archives** are the containers they are wearing another name, and
open as such: `.cbz` is a zip, `.cbr` a rar, `.cbt` a tar, `.cb7` a 7z. Press
`space` for the page list and `Enter` to page through them with the preview.

**RAR** is listed in process by [`rars`](https://lib.rs/crates/rars), which is
pure Rust and MIT/Apache and handles all three RAR generations including solid
archives, so browsing one needs nothing installed. Reading a member out of it
prefers `bsdtar` where you have it: `rars` cannot seek to a member and so
decodes everything ahead of it, which made stepping through a large archive
with the preview open cost tens of seconds per file. Without `bsdtar` it still
works, just with that cost for members late in the archive.

`.iso`, `.cab`, `.cpio`, `.lha`, `.xar`, `.deb` and `.rpm` go through
**`bsdtar`** (libarchive) if it is on your `PATH` — the long tail nobody would
write a reader for individually. Without `bsdtar` these say what is missing
rather than failing obscurely, the same arrangement as `timg`/`chafa` for
images.

Office formats are deliberately *not* treated as archives. `.docx` and `.odt`
are zips, but a table of `word/document.xml` entries is less useful than the
preview they get today.

### Limits

An archive on a **remote panel** is refused: opening one means downloading it
whole, and that should be a copy you asked for with visible progress. Copy it
across first.

An archive **inside another archive** is refused for browsing, though `space`
still lists it. It would compose, but the cost multiplies through a
stream-compressed outer archive and the nesting depth is the archive's choice
rather than yours. Extract the inner one first.

**There is no size limit.** A local container is memory-mapped rather than read,
so its size costs address space rather than memory: indexing a zip touches its
tail, indexing a tar walks the headers and skips the data, and neither faults in
the bulk of the file. A stored member — every member of a plain tar, and any zip
entry written uncompressed — is a contiguous run of bytes in the container, so
it is read straight out of the mapping and extracts in constant memory however
large it is.

A compressed tar is the one case that cannot be mapped: it has no directory, so
it has to be decompressed in full before anything can be indexed. That
decompression fills a buffer sized from the machine's actually-available memory
(a quarter of it, honouring a cgroup cap where there is one) and then spills the
rest to a temporary file, which is mapped and unlinked. Memory therefore stays
bounded whatever the stream expands to — a decompression bomb costs disk the OS
reclaims when the archive closes, rather than the machine's RAM.

The one remaining limit is on a container *on a remote panel*, which has nothing
to map: listing it means pulling it across the wire, so past 128 MB it says so
and suggests copying it here first. Opening one with `Enter` is already refused
for the same reason.

Names that escape the archive root ("Zip Slip") are refused at index time and
reported in the listing rather than silently dropped. Member *counts* are still
bounded (100,000 for a browse, 20,000 in a preview) since the index is a real
data structure per member — that is a count, not a size.

Extraction does **not** restore the executable bit — a `chmod +x` script comes
out of an archive non-executable. The mode is in the index and shown in the
listing; passing it through would mean widening the write path for every
backend.

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

## Preferences

Settings that outlive a session live in `~/.config/myd/prefs.toml` (or
`$XDG_CONFIG_HOME/myd/prefs.toml`; `$MYD_PREFS` names the file outright). It is
written on exit, and only when something has actually changed. The file is meant
to be edited by hand:

```toml
# Info panel width, as a percentage of the panel it sits in (15–60).
info_panel_pct = 30

# Rows shifted from the metadata to the preview beneath it. Negative
# gives the preview more.
info_meta_bias = 0

# The format `gz` offers first: "zip", "7z", "tar" or "tgz".
default_archive_format = "zip"
```

Every setting has a default, so a file that omits one — or one written by an
older version — still loads. A value that is out of range is brought back into
it and an unrecognised one falls back to its default, costing that setting
rather than the file: a typo in `default_archive_format` will not take your
panel width with it. A file that is malformed outright is left alone rather than
overwritten, so it can be repaired by hand.

## License

MIT
