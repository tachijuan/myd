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
