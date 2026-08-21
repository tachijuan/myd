# README screenshots

The images in `doc/screenshots/`, and how to remake them.

Terminal screenshots are captured rather than mocked: myd is driven in a real
tmux pane, the pane is captured with its escape sequences intact, and the
result is rendered to a PNG. Nothing here draws a picture of what the UI is
*supposed* to look like — the shots show what it actually does.

## Remaking them

1. Build the release binary: `cargo build --release --manifest-path myd/Cargo.toml`
2. Build a tree to browse: `demo/fixtures.sh /path/to/tree` (deterministic
   names and sizes, so a re-shoot is comparable to the last one)
3. Start a pane on an **isolated tmux socket** and drive it:

   ```bash
   tmux -L mydshots new-session -d -s shots -x 150 -y 38 \
     "env HOME=$SANDBOX XDG_CONFIG_HOME=$SANDBOX/cfg MYD_PREVIEW_GRAPHICS=blocks \
      myd/target/release/myd /path/to/tree"
   tmux -L mydshots send-keys -t shots j    # …drive it to the state you want
   ```

4. Capture:

   ```bash
   tmux -L mydshots capture-pane -t shots -p -e \
     | python3 demo/shots/render.py 150 38 doc/screenshots/whatever.png
   ```

`render.py` needs `pyte` and `Pillow`.

## Things that will bite you

**Use `-L mydshots`.** The default tmux socket is very likely the one your own
session is on, and `kill-server` or `kill-session` against it will take your
session down with it. A private socket is also how you get the pane size you
asked for: a session on the default socket is clamped to the attached client's
terminal, so `-x 150 -y 38` silently becomes whatever your window happens to be.

**`MYD_PREVIEW_GRAPHICS=blocks`.** kitty/iTerm2/sixel image data is written
straight to the terminal outside the ratatui frame, and does not survive a
`capture-pane`. Blocks are real characters and do. The README says so where the
image shot is shown, so nobody thinks that is the best myd can render.

**Sandbox `HOME` and `XDG_CONFIG_HOME`.** Otherwise the shot picks up your own
`prefs.toml` and `hosts.toml` — and publishes your saved hosts to the README.

**`capture-pane` returns scrollback too.** On a pane whose history is not
empty, the output is longer than the screen and neither the first nor the last
`ROWS` lines are what is displayed. Killing the session and starting a fresh one
is the reliable fix.

## Why a renderer rather than a screenshot tool

There is no ImageMagick, `termshot` or `freeze` on this machine, and adding one
to take five pictures is a poor trade. `render.py` is ~200 lines and does the
job with two libraries the demo verifier already wants.

Rendering is not simply "draw each character": block elements (`█ ░ ▀ ▄ ▌`) and
quadrants (`▖ ▗ ▘ ▝ ▚ ▞`) are painted as rectangles instead of glyphs. Drawn as
text they leave hairline gaps that turn the size bars into dashed columns and an
image preview into scattered punctuation. Emoji go to Noto Color Emoji, which is
a bitmap font that only renders at its native 109px and has to be scaled down.

## The treemap shot needs its own tree

`demo/fixtures.sh` is built for the recorded tour, where one 7MB photo giving
the treemap a single dominant tile is fine. For a screenshot it is not: the
picture is one big rectangle and five slivers, which shows the algorithm doing
nothing.

A treemap reads well when several categories are *comparable* in size, so the
shot uses a directory built for it — around ten files between 12KB and 96KB,
spread across images, docs, code, data, archives and other:

```bash
filler() { yes 'lorem ipsum dolor sit amet' | head -c "$1" > "$2"; }
filler 96000 bundle/screenshot.png
filler 74000 bundle/manual.md
filler 61000 bundle/engine.rs
filler 58000 bundle/dataset.csv
filler 47000 bundle/archive.tar.gz
filler 39000 bundle/theme.css
filler 31000 bundle/notes.txt
filler 24000 bundle/setup.toml
filler 18000 bundle/logo.svg
filler 12000 bundle/build.log
```

Root the pane on that directory and press `v`. Files rather than directories,
because a directory tile takes the colour of whatever dominates it — a screen
of those is one hue per box and says less about the colour coding than a screen
of named files does.

Watch the tile labels: myd truncates a name to fit its box, so a narrow tile can
show `config.tom` and read as a rendering bug in a still image. Rename the
fixture rather than accept the truncation (`setup.toml` above was `config.toml`).

## The archive shots

`demo/fixtures.sh` already builds `archives/bundle.zip` and `backup.tar.gz`
from a small staged tree, and those are what the listing and browsing shots
use — no extra fixture needed.

Three states, in the order the README shows them:

- **Listing** — `space` on the zip. Shows the header line (members, stored vs
  uncompressed, ratio) and per-member permissions and compression.
- **Browsing** — `Enter` on it, then `l` into `bundle`, tag two files with `t`,
  and `Ctrl+p` for the info panel. That one frame carries the amber title, the
  `📦 ARCHIVE (read-only)` badge, `▶ 2 tagged` in the footer, and a markdown
  preview read out of the archive.
- **Creating** — root a pane on `project`, tag a few entries, `gz`. The dialog
  shows the summary line, the name field and the four formats.

Press Enter on the create dialog once and check the result before keeping the
screenshot: a picture of a dialog is worth nothing if the button does not work.
Delete the archive afterwards, or the next shot of that directory has an extra
file in it.
