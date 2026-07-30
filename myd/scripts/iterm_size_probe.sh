#!/bin/sh
# Which iTerm2 image-sizing form does this terminal honour?
#
# myd tells iTerm2 how big to draw a preview in *cells* (width=60;height=20),
# so the image occupies exactly the rows the pane reserved. That is the only
# form which cannot overflow the pane. If images come out too small, the
# terminal is not honouring it and something else has to be used.
#
# This draws the same image four ways. Run it in the terminal where previews
# look wrong -- inside tmux if that is where the problem is -- and report which
# block filled the width.
#
#   ./scripts/iterm_size_probe.sh [image] [cols] [rows]
IMG="${1:-/usr/share/pixmaps/ubuntu-logo-text.png}"
COLS="${2:-60}"
ROWS="${3:-20}"

b64() { base64 -w0 "$1"; }
emit() {                     # $1 = label, $2 = File= args
  printf '\n=== %s ===\n' "$1"
  payload="$(b64 "$IMG")"
  seq="$(printf '\033]1337;File=%s;inline=1:%s\a' "$2" "$payload")"
  if [ -n "$TMUX" ]; then
    # wrap for tmux passthrough: double every ESC
    printf '\033Ptmux;'
    printf '%s' "$seq" | sed 's/\o033/\o033\o033/g'
    printf '\033\\'
  else
    printf '%s' "$seq"
  fi
  printf '\n'
}

echo "image: $IMG   target: ${COLS}x${ROWS} cells   TMUX=${TMUX:+yes}"
emit "A: cells + preserveAspectRatio" "width=${COLS};height=${ROWS};preserveAspectRatio=1"
emit "B: cells, no aspect flag"       "width=${COLS};height=${ROWS}"
emit "C: width in cells only"         "width=${COLS}"
emit "D: percent of session"          "width=100%"
echo
echo "Which block filled the width? Report A/B/C/D."
