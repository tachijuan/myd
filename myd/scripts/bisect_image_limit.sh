#!/bin/sh
# Find the largest inline image this terminal will actually draw.
#
# Four explanations for the blank previews have been ruled out by measurement:
# payload size at the tmux layer (768KB of incompressible data forwarded
# intact), PNG re-encoding (the JPEG still failed once sent as JPEG at 716KB),
# write fragmentation (a TTY takes the whole payload in one write), and a
# malformed escape (a working and a failing capture are byte-for-byte the same
# shape). What is left is the terminal's own limit, which only the terminal can
# tell us.
#
# This draws the same image at increasing payload sizes and waits between each,
# so the size where it stops appearing is visible. Run it in the pane where
# previews fail.
#
#   ./scripts/bisect_image_limit.sh [image]

IMG="${1:-/usr/share/pixmaps/ubuntu-logo-text.png}"
[ -f "$IMG" ] || { echo "no such file: $IMG" >&2; exit 1; }

ESC="$(printf '\033')"

wait_key() {
    if [ -t 0 ]; then
        old="$(stty -g)"; stty -echo -icanon min 1 time 0 2>/dev/null
        dd bs=1 count=1 2>/dev/null | cat >/dev/null
        stty "$old" 2>/dev/null
    fi
}

send() {   # $1 = label, $2 = payload file
    printf '\033[2J\033[H'
    bytes=$(wc -c < "$2")
    printf '  %s  (%s base64 bytes)\n\n' "$1" "$bytes"
    seq_file="$2"
    if [ -n "$TMUX" ]; then
        printf '\033Ptmux;'
        { printf '\033]1337;File=size=1;width=60;height=20;preserveAspectRatio=1;inline=1:'
          cat "$seq_file"; printf '\a'; } | sed "s/${ESC}/${ESC}${ESC}/g"
        printf '\033\\'
    else
        printf '\033]1337;File=size=1;width=60;height=20;preserveAspectRatio=1;inline=1:'
        cat "$seq_file"; printf '\a'
    fi
    printf '\n\n  Did the image appear?  [any key for the next size]'
    wait_key
}

echo "  Inline image size bisect"
echo "  tmux: ${TMUX:+yes}${TMUX:-no}"
echo
echo "  The same picture at growing payload sizes. Note the last one that drew."
printf '\n  [any key to start]'
wait_key

for kb in 8 32 64 128 256 512 1024; do
    # Scale the source up until its base64 is about the target size, so every
    # step is a real decodable image rather than filler.
    timg -pi -g$((kb * 2))x$((kb / 2 + 8)) -U --frames=1 "$IMG" 2>/dev/null \
        | sed -n 's/.*inline=1://p' | tr -d '\007\n' > /tmp/bisect.b64
    [ -s /tmp/bisect.b64 ] || continue
    send "target ~${kb}KB" /tmp/bisect.b64
done

printf '\033[2J\033[H'
echo "  Done. The largest size that drew is the terminal's practical limit."
echo
echo "  If even the smallest failed, the problem is not size at all and the"
echo "  escape is being rejected for another reason."
echo
