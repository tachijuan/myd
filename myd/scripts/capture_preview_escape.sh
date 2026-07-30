#!/bin/sh
# Capture the exact bytes myd would send for one file, so the escape can be
# replayed and inspected outside the app.
#
# Three theories about the blank previews have now been ruled out: payload size
# (tmux forwarded 768KB of incompressible data intact), PNG re-encoding (the
# JPEG still failed at 716KB), and write fragmentation (a TTY takes it in one
# write). What is left is the escape itself, so this dumps it.
#
#   ./scripts/capture_preview_escape.sh <file> [cols] [rows] > /tmp/esc.bin
#
# Then, in the pane where previews fail:
#   cat /tmp/esc.bin        # does the image appear?
FILE="${1:?usage: $0 <file> [cols] [rows]}"
COLS="${2:-140}"
ROWS="${3:-37}"
exec "$(dirname "$0")/../target/release/myd-escape" "$FILE" "$COLS" "$ROWS"
