#!/usr/bin/env python3
"""Render a captured terminal screen to a PNG.

Reads a raw byte stream (tmux capture-pane -e, which keeps the SGR sequences)
on stdin, feeds it to a pyte screen, and draws the resulting cells with Pillow.

Going through a terminal emulator rather than parsing the escapes directly is
the same reasoning demo/verify.sh gives for using pyte: a repaint stream is not
the screen, and only an emulator knows what is actually displayed.
"""
import sys
import pyte
from PIL import Image, ImageDraw, ImageFont

FONT_DIR = "/usr/share/fonts/truetype/dejavu"
REGULAR = f"{FONT_DIR}/DejaVuSansMono.ttf"
BOLD = f"{FONT_DIR}/DejaVuSansMono-Bold.ttf"
EMOJI = "/usr/share/fonts/truetype/noto/NotoColorEmoji.ttf"

# Cell metrics. 17px gives crisp box-drawing without the file being huge.
FONT_SIZE = 17
CELL_W = 10
CELL_H = 22
PAD = 16
# Rounded-corner window chrome, so the shot reads as a terminal rather than as
# a screenshot of nothing in particular.
TITLEBAR = 30

# xterm-256 palette. pyte hands back either a name or a 6-hex-digit string.
NAMED = {
    "black": "1c1c26", "red": "e06c75", "green": "98c379", "brown": "d19a66",
    "blue": "61afef", "magenta": "c678dd", "cyan": "56b6c2", "white": "dcdfe4",
    "brightblack": "5c6370", "brightred": "e06c75", "brightgreen": "98c379",
    "brightbrown": "e5c07b", "brightblue": "61afef", "brightmagenta": "c678dd",
    "brightcyan": "56b6c2", "brightwhite": "ffffff",
    "default": None,
}
BG = (24, 24, 34)
FG = (220, 223, 228)


def cube(n):
    """xterm-256 index to RGB."""
    if n < 16:
        base = [
            (28, 28, 38), (231, 106, 110), (137, 202, 120), (219, 171, 96),
            (97, 175, 239), (198, 120, 221), (86, 182, 194), (200, 205, 214),
            (92, 99, 112), (231, 106, 110), (137, 202, 120), (229, 192, 123),
            (97, 175, 239), (198, 120, 221), (86, 182, 194), (255, 255, 255),
        ]
        return base[n]
    if n < 232:
        n -= 16
        levels = [0, 95, 135, 175, 215, 255]
        return (levels[n // 36], levels[(n // 6) % 6], levels[n % 6])
    v = 8 + (n - 232) * 10
    return (v, v, v)


def color(spec, default):
    if spec is None or spec == "default":
        return default
    if isinstance(spec, str):
        if spec in NAMED:
            hexv = NAMED[spec]
            return default if hexv is None else tuple(
                int(hexv[i:i + 2], 16) for i in (0, 2, 4))
        if len(spec) == 6:
            try:
                return tuple(int(spec[i:i + 2], 16) for i in (0, 2, 4))
            except ValueError:
                return default
        if spec.isdigit():
            return cube(int(spec))
    return default


# Only the pictographic blocks. Deliberately *not* "anything above U+2100":
# myd draws its size bars from █ and ░ (U+2588/U+2591) and its frames from the
# box-drawing range, and routing those to the emoji font erased both.
EMOJI_RANGES = [
    (0x1F300, 0x1FAFF),  # pictographs, transport, symbols, extended-A
    (0x1F000, 0x1F0FF),  # mahjong, dominoes, cards
    (0x2600, 0x27BF),    # misc symbols and dingbats
    (0xFE0F, 0xFE0F),    # variation selector-16
]


# Block elements: (ink fraction, how it fills the cell).
BLOCKS = {
    "\u2588": (1.0, "full"),
    "\u2591": (0.22, "shade"), "\u2592": (0.45, "shade"), "\u2593": (0.7, "shade"),
    "\u258f": (0.125, "left"), "\u258e": (0.25, "left"), "\u258d": (0.375, "left"),
    "\u258c": (0.5, "left"), "\u258b": (0.625, "left"), "\u258a": (0.75, "left"),
    "\u2589": (0.875, "left"),
    "\u2581": (0.125, "bottom"), "\u2582": (0.25, "bottom"), "\u2583": (0.375, "bottom"),
    "\u2584": (0.5, "bottom"), "\u2585": (0.625, "bottom"), "\u2586": (0.75, "bottom"),
    "\u2587": (0.875, "bottom"),
}


# Quadrant blocks, as the set of quarter-cells they fill. chafa and timg build
# an image out of these plus the halves above, so without them an image preview
# renders as scattered punctuation instead of a picture.
QUADRANTS = {
    "\u2596": {(0, 1)},                      # lower left
    "\u2597": {(1, 1)},                      # lower right
    "\u2598": {(0, 0)},                      # upper left
    "\u259d": {(1, 0)},                      # upper right
    "\u2599": {(0, 0), (0, 1), (1, 1)},
    "\u259a": {(0, 0), (1, 1)},
    "\u259b": {(0, 0), (1, 0), (0, 1)},
    "\u259c": {(0, 0), (1, 0), (1, 1)},
    "\u259e": {(1, 0), (0, 1)},
    "\u259f": {(1, 0), (0, 1), (1, 1)},
    "\u2580": {(0, 0), (1, 0)},              # upper half
    "\u2584": {(0, 1), (1, 1)},              # lower half
    "\u258c": {(0, 0), (0, 1)},              # left half
    "\u2590": {(1, 0), (1, 1)},              # right half
}


def is_emoji(c):
    n = ord(c)
    return any(lo <= n <= hi for lo, hi in EMOJI_RANGES)


def render(data, cols, rows, out):
    screen = pyte.Screen(cols, rows)
    stream = pyte.Stream(screen)
    # tmux capture-pane separates rows with a bare newline and never resets the
    # column, so pyte would carry each line's end position into the next and
    # smear the content diagonally. Feed it CRLF and reset the pen per row: the
    # capture is a grid of finished lines, not a live repaint stream.
    text = data.decode("utf-8", "replace")
    for i, line in enumerate(text.split("\n")[:rows]):
        stream.feed(f"\x1b[{i + 1};1H\x1b[0m")
        stream.feed(line)

    W = cols * CELL_W + PAD * 2
    H = rows * CELL_H + PAD * 2 + TITLEBAR
    img = Image.new("RGB", (W, H), BG)
    d = ImageDraw.Draw(img)

    # Window chrome: a bar and three dots.
    d.rectangle([0, 0, W, TITLEBAR], fill=(38, 38, 50))
    for i, c in enumerate([(255, 95, 86), (255, 189, 46), (39, 201, 63)]):
        cx = 18 + i * 18
        d.ellipse([cx - 5, TITLEBAR // 2 - 5, cx + 5, TITLEBAR // 2 + 5], fill=c)

    reg = ImageFont.truetype(REGULAR, FONT_SIZE)
    bold = ImageFont.truetype(BOLD, FONT_SIZE)
    # Noto Color Emoji is a bitmap font and only accepts its native size; myd
    # uses emoji for the file-type icons, so without this every one of them
    # draws as a missing-glyph box.
    try:
        emoji = ImageFont.truetype(EMOJI, 109)
    except OSError:
        emoji = None

    for y in range(rows):
        for x in range(cols):
            ch = screen.buffer[y][x]
            fg = color(ch.fg, FG)
            bg = color(ch.bg, BG)
            if ch.reverse:
                fg, bg = bg, fg
            px = PAD + x * CELL_W
            py = PAD + TITLEBAR + y * CELL_H
            if bg != BG:
                d.rectangle([px, py, px + CELL_W, py + CELL_H], fill=bg)
            if not ch.data or ch.data == " ":
                continue
            # Block elements are painted as rectangles rather than drawn as
            # glyphs. The font leaves a hairline between adjacent cells, which
            # turns myd's size bars and its scrollbar into dashed columns
            # instead of the solid runs they are on a real terminal.
            if ch.data in QUADRANTS:
                hw, hh = CELL_W / 2, CELL_H / 2
                d.rectangle([px, py, px + CELL_W, py + CELL_H], fill=bg)
                for (qx, qy) in QUADRANTS[ch.data]:
                    d.rectangle(
                        [px + qx * hw, py + qy * hh,
                         px + (qx + 1) * hw, py + (qy + 1) * hh],
                        fill=fg)
                continue
            if ch.data in BLOCKS:
                frac, align = BLOCKS[ch.data]
                if align == "full":
                    d.rectangle([px, py, px + CELL_W, py + CELL_H], fill=fg)
                elif align == "shade":
                    # ░▒▓ are a proportion of ink, not a partial cell: blend
                    # toward the background instead of filling part of it.
                    blend = tuple(
                        int(b + (f - b) * frac) for f, b in zip(fg, bg))
                    d.rectangle([px, py, px + CELL_W, py + CELL_H], fill=blend)
                elif align == "left":
                    d.rectangle([px, py, px + int(CELL_W * frac), py + CELL_H], fill=fg)
                elif align == "bottom":
                    d.rectangle([px, py + int(CELL_H * (1 - frac)), px + CELL_W, py + CELL_H], fill=fg)
                continue
            if emoji is not None and is_emoji(ch.data[0]):
                # Rendered to its own layer and scaled down: the bitmap strike
                # is 109px and cannot be asked for a smaller size directly.
                tile = Image.new("RGBA", (109, 109), (0, 0, 0, 0))
                ImageDraw.Draw(tile).text((0, 0), ch.data, font=emoji, embedded_color=True)
                box = tile.getbbox()
                if box:
                    tile = tile.crop(box).resize(
                        (CELL_W * 2 - 2, CELL_H - 6), Image.LANCZOS)
                    img.paste(tile, (px, py + 4), tile)
                continue
            d.text((px, py), ch.data, font=bold if ch.bold else reg, fill=fg)

    img.save(out)
    print(f"wrote {out} ({W}x{H})", file=sys.stderr)


if __name__ == "__main__":
    cols, rows, out = int(sys.argv[1]), int(sys.argv[2]), sys.argv[3]
    render(sys.stdin.buffer.read(), cols, rows, out)
