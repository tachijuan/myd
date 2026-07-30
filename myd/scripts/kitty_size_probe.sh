#!/bin/sh
# Does this terminal honour kitty's c=/r= sizing keys?
#
# Inside tmux myd now sends images with the kitty protocol, because it chunks a
# picture into ~4KB escapes that a multiplexer will actually deliver. The size
# it should occupy is stated with the c= and r= control keys. If images arrive
# tiny, the likely reason is that those keys are being ignored and the terminal
# is sizing by the raster's own pixels instead.
#
# This draws the same image four ways, one per screen. Run it in the pane where
# previews look wrong and note which one fills the pane.
#
#   ./scripts/kitty_size_probe.sh [image] [cols] [rows]

IMG="${1:-/usr/share/pixmaps/ubuntu-logo-text.png}"
COLS="${2:-60}"
ROWS="${3:-20}"
[ -f "$IMG" ] || { echo "no such file: $IMG" >&2; exit 1; }
command -v timg >/dev/null || { echo "timg is required" >&2; exit 1; }

ESC="$(printf '\033')"

wait_key() {
    if [ -t 0 ]; then
        old="$(stty -g)"; stty -echo -icanon min 1 time 0 2>/dev/null
        dd bs=1 count=1 2>/dev/null | cat >/dev/null
        stty "$old" 2>/dev/null
    fi
}

# $1 label, $2 extra control keys (may be empty), $3 timg geometry
show() {
    printf '\033[2J\033[H'
    printf '  %s\n' "$1"
    printf '  controls: a=T,f=100,m=…%s   raster: -g%s\n\n' "${2:+,$2}" "$3"

    timg -pk -g"$3" -U --frames=1 "$IMG" 2>/dev/null \
        | sed "s/${ESC}\[?25[lh]//g" > /tmp/kprobe.raw

    # Insert the extra keys into the first chunk's control section only; the
    # continuation chunks carry no controls.
    if [ -n "$2" ]; then
        sed "0,/${ESC}_G[^;]*/s/\(${ESC}_Ga=T[^;]*\)/\1,$2/" /tmp/kprobe.raw > /tmp/kprobe.esc
    else
        cp /tmp/kprobe.raw /tmp/kprobe.esc
    fi

    if [ -n "$TMUX" ]; then
        # Each escape needs its own passthrough envelope, with every ESC inside
        # doubled. Done with the same logic the app uses, in python so the
        # framing is exact — an unterminated wrapper delivers nothing.
        python3 - /tmp/kprobe.esc <<'PYWRAP'
import sys, re
data = open(sys.argv[1], 'rb').read()
out = []
# Split into individual APC sequences; anything else passes through.
for part in re.split(rb'(\x1b_G.*?\x1b\\)', data, flags=re.S):
    if not part:
        continue
    if part.startswith(b'\x1b_G'):
        out.append(b'\x1bPtmux;' + part.replace(b'\x1b', b'\x1b\x1b') + b'\x1b\\')
    else:
        out.append(part)
sys.stdout.buffer.write(b''.join(out))
PYWRAP
    else
        cat /tmp/kprobe.esc
    fi

    printf '\n\n  Does this fill the %sx%s pane?  [any key for next]' "$COLS" "$ROWS"
    wait_key
}

printf '\033[2J\033[H'
echo "  kitty sizing probe"
echo "  image: $IMG    target: ${COLS}x${ROWS} cells    tmux: ${TMUX:+yes}${TMUX:-no}"
echo
echo "  Four ways of asking for the same size. Note which fill the pane."
printf '\n  [any key to start]'
wait_key

show "A: c=/r= cells, small raster (what myd sends)" "c=$COLS,r=$ROWS" "${COLS}x${ROWS}"
show "B: c=/r= cells, large raster"                  "c=$COLS,r=$ROWS" "$((COLS*2))x$((ROWS*2))"
show "C: no sizing keys, raster sized to the pane"   ""                "${COLS}x${ROWS}"
show "D: no sizing keys, large raster"               ""                "$((COLS*2))x$((ROWS*2))"

printf '\033[2J\033[H'
cat <<'MSG'
  Done. Which filled the pane?

    A / B fill  -> c= and r= are honoured; sizing is fine.
    C / D fill  -> the keys are ignored and the terminal sizes by the
                   raster's pixels, so the raster must be built to the
                   pane's real pixel size instead.
    none fill   -> something else is scaling the image down.
MSG
echo
