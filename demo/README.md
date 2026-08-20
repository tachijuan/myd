# The myd demo tour

A recorded walkthrough of what myd does, built so it can be re-recorded after
any release rather than hand-shot once and left to rot.

```bash
demo/record.sh            # every chapter, then the concatenated tour
demo/verify.sh            # check the casts show what they claim to
asciinema play demo/cast/00-full-tour.cast
```

Output lands in `demo/cast/` (git-ignored): eleven chapter casts plus
`00-full-tour.cast`, about 6½ minutes end to end.

## The arc

Each chapter re-frames the tool as something bigger than the last.

| # | Chapter | What it reveals |
|---|---------|-----------------|
| 01 | navigation | vi keys, sizes, sorting, columns, hidden files |
| 02 | treemap | the same tree as proportional space |
| 03 | preview | syntax highlighting, info panel, search *inside* a file |
| 04 | media | a photograph and a five-page PDF, in a terminal |
| 05 | archives | walking *into* a zip: filter, treemap, preview, read-only |
| 06 | search & filter | regex search, then a regex mask over the whole tree |
| 07 | tagging | `t`, visual sweep, and one tag set shared with the treemap |
| 08 | regex rename | `gr`, with the result previewed before it runs |
| 09 | dual panel | two trees, copy between them, create and rename |
| 10 | remote | SFTP browsing and a live background transfer |
| 11 | closing | the help screen, and how to install it |

## Usage

```bash
demo/record.sh              # all chapters + tour
demo/record.sh 08           # just one, by number or name fragment
demo/record.sh --no-concat  # chapters only
demo/record.sh --concat     # rebuild the tour from existing casts
```

Requires `asciinema`, `tmux`, and a release build at
`myd/target/release/myd` (override with `MYD_BIN`). `verify.sh` additionally
needs `pyte` (`pip install pyte`, or point `DEMO_PYTHON` at an interpreter that
has it). The image and archive fixtures need `gs` and `zip`.

## How it works

`record.sh` starts a detached tmux session, points `asciinema` at it with
`tmux attach`, and runs a segment script that types into the pane from outside
with `tmux send-keys`. What lands in the cast is a real myd process being
driven, not a reconstruction.

Because `tmux capture-pane` can read the pane back, segments **assert** on what
is actually drawn (`expect`, `select_file` in `lib.sh`). A beat that silently
stops working fails the recording instead of shipping as a broken cast.

## Things that will bite you

Four constraints are baked into the harness. Each was a bug first.

**The pane size must be pinned.** `tmux new-session -x/-y` is only an initial
size; tmux resizes a session to fit whoever attaches. Left alone, the pane came
up at the client's size (144x54) while asciinema recorded a 120x34 window, so
myd's footer — drawn on the last row, carrying the `VISUAL` and tagged
indicators — was cropped out of every recording. `start_session` sets
`window-size manual` and resizes explicitly.

**Images only survive as blocks.** Under kitty/iTerm2/sixel the preview is an
escape sequence written straight to stdout, outside the ratatui frame
(`myd/src/preview/graphics.rs`), and that byte stream does not replay in a cast.
Recording forces `MYD_PREVIEW_GRAPHICS=blocks`, which is ordinary truecolor
text. The tour therefore shows the *fallback* rendering — the real thing looks
considerably better.

**A cast cannot be grepped.** tmux repaints by moving the cursor and emitting
only what changed, so on-screen text is never a contiguous run of bytes in the
file: "Treemap" is plainly visible while the stream contains no such string.
`verify.sh` replays each cast through a terminal emulator and reads the screen.

**Nothing may touch the real config.** `XDG_CONFIG_HOME` is redirected, which
covers both `hosts.toml` and `prefs.toml`, so recording never writes to the
`gd` picker's history. The remote chapter also redirects `HOME`, because the
sandboxed server regenerates its host key on every start and a `known_hosts`
entry left in the user's `~/.ssh` makes the *next* run look like a changed host
key — which myd rightly refuses to connect to.

## The remote chapter

Chapter 10 uses `myd/scripts/sftp_test_env.sh`, the same isolated sshd the SFTP
integration tests use: its own host key, client key and agent, on
`127.0.0.1:22022`. No real host, no real credential. `record.sh` starts and
stops it around the chapter.

## Fixtures

`fixtures.sh` builds the filesystem from scratch on every run — deterministic
names, sizes and mtimes, so a re-record is diffable rather than merely similar.
It reuses the repo's photo when present and generates a placeholder when it is
not (the JPEG is git-ignored), so the tour records on a fresh clone either way.
