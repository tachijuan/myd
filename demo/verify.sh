#!/usr/bin/env bash
# Check that the recorded casts actually contain the features they claim to.
#
#   ./verify.sh
#
# Grepping a cast does not work, and neither does concatenating its writes.
# tmux repaints by moving the cursor and emitting only what changed, so text on
# screen is never a contiguous run of bytes in the file — "Treemap" is on the
# display while the stream contains no such string anywhere.
#
# The only way to know what a cast shows is to feed it to a terminal emulator
# and read the screen, which is what this does (via pyte).
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
CAST_DIR="$HERE/cast"

[[ -d "$CAST_DIR" ]] || { echo "no casts yet — run ./record.sh" >&2; exit 1; }

# pyte is import-only tooling for this check, and is not needed to record.
PY_BIN="${DEMO_PYTHON:-}"
if [[ -z "$PY_BIN" ]]; then
  for candidate in python3 /home/juan/miniconda3/bin/python3; do
    if command -v "$candidate" >/dev/null 2>&1 && "$candidate" -c 'import pyte' 2>/dev/null; then
      PY_BIN="$candidate"; break
    fi
  done
fi
[[ -n "$PY_BIN" ]] || {
  echo "verify needs pyte: pip install pyte (or set DEMO_PYTHON)" >&2; exit 1; }

"$PY_BIN" - "$CAST_DIR" <<'PY'
import json, sys, pathlib
import pyte

cast_dir = pathlib.Path(sys.argv[1])
COLS, ROWS = 120, 34

# What each chapter has to show for the tour to be worth publishing.
expected = {
    "01-navigation":    ["File Tree", "Sort:", ".editorconfig"],
    "02-treemap":       ["Treemap", "File Tree"],
    # The file is longer than the pane, so only what was scrolled to is ever on
    # screen; assert on the top of the file, which is what the beat opens on.
    "03-preview":       ["main.rs", "pub struct Widget", "Entry point"],
    "04-media":         ["aerial-coastline.jpg", "quarterly-report.pdf", "page"],
    "05-archives":      ["ARCHIVE", "bundle", "settings.toml", "Treemap"],
    "06-search-filter": ["FILTERED", "rotated"],
    # `▶` is the tag marker in both views; the footer wording is not load-bearing.
    "07-tagging":       ["▶", "Treemap", "VISUAL"],
    "08-regex-rename":  ["Patterned rename", "release-0042.jpg", "IMG_0042"],
    "09-dual-panel":    ["inbox", "approved", "catalogue.tsv"],
    "10-remote":        ["SFTP", "blob.bin", "greeting.txt"],
    "11-closing":       ["cargo install --path myd", "vi-like file browser"],
}

def stream(path):
    """Everything the cast ever put on screen.

    The cast is replayed into a terminal emulator and the display is sampled
    after each write, because a frame that is drawn and then overdrawn is still
    something the viewer saw. Sampling only the final screen would miss almost
    every feature in the tour.
    """
    screen = pyte.Screen(COLS, ROWS)
    feed = pyte.Stream(screen)
    seen = []
    with path.open() as handle:
        handle.readline()                      # header
        for line in handle:
            line = line.strip()
            if not line:
                continue
            event = json.loads(line)
            if len(event) >= 3 and event[1] == "o":
                feed.feed(event[2])
                seen.append("\n".join(screen.display))
    return "\n".join(seen)

def duration(path):
    with path.open() as handle:
        handle.readline()
        return sum(json.loads(l)[0] for l in handle if l.strip())

failed = []
total = 0.0

for name, wanted in expected.items():
    path = cast_dir / f"{name}.cast"
    if not path.exists():
        print(f"  \033[38;5;203m✗ {name}: not recorded\033[0m")
        failed.append(name)
        continue
    text = stream(path)
    secs = duration(path)
    total += secs
    missing = [w for w in wanted if w not in text]
    if missing:
        print(f"  \033[38;5;203m✗ {name} ({secs:.0f}s): missing {missing}\033[0m")
        failed.append(name)
    else:
        print(f"  \033[38;5;71m✓ {name} ({secs:.0f}s)\033[0m")

tour = cast_dir / "00-full-tour.cast"
if tour.exists():
    text = stream(tour)
    every = [w for ws in expected.values() for w in ws]
    missing = [w for w in every if w not in text]
    secs = duration(tour)
    mins = secs / 60
    if missing:
        print(f"  \033[38;5;203m✗ full tour: missing {missing}\033[0m")
        failed.append("00-full-tour")
    else:
        print(f"  \033[38;5;71m✓ full tour ({mins:.1f} min, every feature present)\033[0m")
    # The tour should not announce its own ending once per chapter.
    exits = text.count("[exited]")
    if exits:
        print(f"  \033[38;5;203m✗ full tour: {exits} '[exited]' markers leaked in\033[0m")
        failed.append("00-full-tour-exits")
else:
    print("  \033[38;5;203m✗ full tour: not built\033[0m")
    failed.append("00-full-tour")

print()
if failed:
    print(f"\033[38;5;203m{len(failed)} problem(s): {', '.join(failed)}\033[0m")
    sys.exit(1)
print(f"\033[38;5;71mall chapters verified — {total/60:.1f} min of material\033[0m")
PY
