#!/bin/sh
# Which iTerm2 image-sizing form does this terminal honour?
#
# myd tells iTerm2 how big to draw a preview in *cells* (width=60;height=20), so
# the image occupies exactly the rows the pane reserved. That is the only form
# which cannot overflow the pane. If previews come out too small, the terminal is
# not honouring it and something else has to be used.
#
# This draws the same image four ways, one at a time, waiting for a keypress
# between each. It shows them one per screen rather than in a list because an
# inline image is not part of the terminal's cell buffer: scrolling back
# redraws the cells and the picture is simply gone, so a list cannot be reviewed
# after the fact.
#
# Run it in the pane where previews look wrong -- inside tmux if that is where
# the problem is -- and note which option fills the width.
#
#   ./scripts/iterm_size_probe.sh [image] [cols] [rows]

IMG="${1:-/usr/share/pixmaps/ubuntu-logo-text.png}"
COLS="${2:-60}"
ROWS="${3:-20}"

if [ ! -f "$IMG" ]; then
    echo "no such image: $IMG" >&2
    echo "usage: $0 [image] [cols] [rows]" >&2
    exit 1
fi

# The image is prepared exactly as myd prepares it: rendered through timg at
# twice the target geometry with upscaling on, so the raster has enough pixels
# to fill a real cell. Testing the *original* file instead would ask a different
# question -- a 260x91 logo in a 60x20 cell box is letterboxed whatever the
# sizing form, which is not the case myd is in.
#
# Falls back to the file itself when timg is not installed; the sizing forms are
# still comparable, just against a smaller raster.
PAYLOAD=""
if command -v timg >/dev/null 2>&1; then
    # `-pi` writes iTerm2's own escape; the base64 body of that escape is the
    # upscaled PNG, which is exactly what myd ends up sending.
    PAYLOAD="$(timg -pi -g"$((COLS * 2))x$((ROWS * 2))" -U --frames=1 "$IMG" 2>/dev/null \
        | sed -n 's/.*inline=1://p' | tr -d '\007\n')"
fi
if [ -z "$PAYLOAD" ]; then
    echo "  (timg not available -- using the file as-is)" >&2
    PAYLOAD="$(base64 -w0 "$IMG" 2>/dev/null || base64 "$IMG" | tr -d '\n')"
fi

# A literal escape byte, for use in patterns where a `\033` spelling would not
# be interpreted.
ESC="$(printf '\033')"

# Read a single keypress without waiting for Enter, restoring the terminal
# afterwards however the read ends.
wait_for_key() {
    if [ -t 0 ]; then
        old="$(stty -g)"
        stty -echo -icanon min 1 time 0 2>/dev/null
        dd bs=1 count=1 2>/dev/null | cat >/dev/null
        stty "$old" 2>/dev/null
    else
        # Not a terminal (piped, or run from a script): nothing to wait on.
        sleep 1
    fi
}

# $1 = letter, $2 = description, $3 = the File= size arguments
show() {
    printf '\033[2J\033[H'
    printf '  Option %s of 4 — %s\n' "$1" "$2"
    printf '  sending: %s\n' "$3"
    printf '  target:  %sx%s cells%s\n\n' "$COLS" "$ROWS" \
        "$([ -n "$TMUX" ] && printf ' (through tmux passthrough)')"

    seq="$(printf '\033]1337;File=%s;inline=1:%s\a' "$3" "$PAYLOAD")"
    if [ -n "$TMUX" ]; then
        # tmux only forwards an escape it does not understand when it is wrapped,
        # and every ESC inside has to be doubled so tmux can tell the payload
        # from the wrapper's own terminator.
        #
        # The ESC in the sed pattern is a real byte, built by printf: `\033`
        # written literally is not interpreted by sed and silently matches
        # nothing, which produces a wrapper containing an undoubled payload and
        # therefore no image at all.
        printf '\033Ptmux;'
        printf '%s' "$seq" | sed "s/${ESC}/${ESC}${ESC}/g"
        printf '\033\\'
    else
        printf '%s' "$seq"
    fi

    printf '\n\n  Does this fill the %s-column width?  [any key for next]' "$COLS"
    wait_for_key
}

printf '\033[2J\033[H'
echo "  Image sizing probe"
echo
echo "  image:    $IMG"
echo "  target:   ${COLS}x${ROWS} cells"
echo "  tmux:     ${TMUX:+yes, passthrough-wrapped}${TMUX:-no}"
echo
echo "  Four sizing forms, one per screen. Note which ones fill the width."
echo
printf '  [any key to start]'
wait_for_key

show A "cells + preserveAspectRatio (what myd sends today)" \
       "width=${COLS};height=${ROWS};preserveAspectRatio=1"
show B "cells, no aspect flag" \
       "width=${COLS};height=${ROWS}"
show C "width in cells only, height follows the aspect ratio" \
       "width=${COLS}"
show D "percent of session" \
       "width=100%"

printf '\033[2J\033[H'
echo "  Done. Which options filled the width?"
echo
echo "    A  width=N;height=N;preserveAspectRatio=1   (current)"
echo "    B  width=N;height=N"
echo "    C  width=N"
echo "    D  width=100%"
echo
echo "  If none did, the terminal is ignoring the cell form entirely and the"
echo "  size has to be given in pixels instead."
echo
echo "  Worth trying too, since your tmux draws sixel itself rather than"
echo "  forwarding it:"
echo
echo "    MYD_PREVIEW_GRAPHICS=sixel myd"
echo
