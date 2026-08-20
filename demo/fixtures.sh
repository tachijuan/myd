#!/usr/bin/env bash
# Build the filesystem the demo explores.
#
# Everything here is deterministic — fixed names, fixed sizes, fixed mtimes — so
# that re-recording after a release produces a diffable cast rather than a new
# one that merely looks similar. Sizes are chosen so the treemap has visible
# structure: a few large tiles, a long tail of small ones.
#
# Usage: fixtures.sh <root>       # builds, wiping any previous contents
set -euo pipefail

ROOT="${1:?usage: fixtures.sh <root>}"
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

rm -rf "$ROOT"
mkdir -p "$ROOT"

# Deterministic filler. `head -c` on /dev/zero then a fixed pattern keeps the
# bytes reproducible; /dev/urandom would change the compressed sizes each run.
filler() { # filler <bytes> <path>
  local bytes="$1" path="$2"
  mkdir -p "$(dirname "$path")"
  yes 'lorem ipsum dolor sit amet consectetur adipiscing elit' 2>/dev/null |
    head -c "$bytes" > "$path" || true
}

# ---------------------------------------------------------------- project/
# A source tree, for syntax highlighting and the code category in the treemap.
mkdir -p "$ROOT/project/src" "$ROOT/project/docs" "$ROOT/project/tests"

cat > "$ROOT/project/src/main.rs" <<'RS'
//! Entry point for the widget service.

use std::collections::HashMap;
use anyhow::{Context, Result};

/// A widget, as the catalogue understands one.
#[derive(Debug, Clone)]
pub struct Widget {
    pub id: u64,
    pub name: String,
    pub tags: Vec<String>,
}

impl Widget {
    /// Build a widget with no tags yet.
    pub fn new(id: u64, name: impl Into<String>) -> Self {
        Self { id, name: name.into(), tags: Vec::new() }
    }

    /// Whether this widget carries every one of `wanted`.
    pub fn matches(&self, wanted: &[String]) -> bool {
        wanted.iter().all(|w| self.tags.contains(w))
    }
}

fn load_catalogue(path: &str) -> Result<HashMap<u64, Widget>> {
    let raw = std::fs::read_to_string(path)
        .with_context(|| format!("reading catalogue from {path}"))?;
    let mut out = HashMap::new();
    for (n, line) in raw.lines().enumerate() {
        let mut parts = line.splitn(2, '\t');
        let id: u64 = parts.next().unwrap_or_default().parse()?;
        let name = parts.next().unwrap_or("unnamed");
        out.insert(id, Widget::new(id, name));
        if n > 10_000 { break; }
    }
    Ok(out)
}

fn main() -> Result<()> {
    let catalogue = load_catalogue("catalogue.tsv")?;
    println!("loaded {} widgets", catalogue.len());
    Ok(())
}
RS

cat > "$ROOT/project/src/server.py" <<'PY'
"""Serve the widget catalogue over HTTP."""

import asyncio
import json
from dataclasses import dataclass, field


@dataclass
class Widget:
    id: int
    name: str
    tags: list[str] = field(default_factory=list)

    def matches(self, wanted: list[str]) -> bool:
        """True when every wanted tag is present."""
        return all(tag in self.tags for tag in wanted)


class Catalogue:
    def __init__(self, path: str) -> None:
        self.path = path
        self._items: dict[int, Widget] = {}

    async def load(self) -> None:
        with open(self.path) as handle:
            for line in handle:
                ident, _, name = line.partition("\t")
                self._items[int(ident)] = Widget(int(ident), name.strip())

    async def search(self, tags: list[str]) -> list[Widget]:
        await asyncio.sleep(0)  # yield to the loop
        return [w for w in self._items.values() if w.matches(tags)]


async def main() -> None:
    catalogue = Catalogue("catalogue.tsv")
    await catalogue.load()
    print(json.dumps({"count": len(catalogue._items)}))


if __name__ == "__main__":
    asyncio.run(main())
PY

cat > "$ROOT/project/docs/README.md" <<'MD'
# Widget Service

A small catalogue service, used here to give the preview pane something
worth syntax highlighting.

## Design

The catalogue is loaded once at startup and held in memory. Lookups are
by id; searches scan, because the catalogue is small and the scan is
faster than maintaining an index nobody queries.

| Endpoint        | Method | Purpose                 |
|-----------------|--------|-------------------------|
| `/widgets`      | GET    | List the whole catalogue |
| `/widgets/{id}` | GET    | Fetch one widget         |
| `/search`       | POST   | Filter by tag            |

## Running

```bash
cargo run --release -- --catalogue catalogue.tsv
```

Set `WIDGET_LOG=debug` for per-request timing.

> Note: the service holds no state between restarts. That is deliberate —
> the catalogue is generated upstream and this process only serves it.
MD

filler 4200  "$ROOT/project/docs/architecture.md"
filler 2100  "$ROOT/project/tests/integration.rs"
filler 900   "$ROOT/project/Cargo.toml"
filler 60000 "$ROOT/project/catalogue.tsv"

# ---------------------------------------------------------------- media/
# The photo drives the image-preview beat. Reuse the repo's real photo when it
# is there (it is git-ignored, so it may not be) and fall back to a generated
# one, so the demo records either way.
mkdir -p "$ROOT/media"
PHOTO_SRC="$(ls "$HERE/../myd"/dji_fly_*.JPG 2>/dev/null | head -1 || true)"
if [[ -n "$PHOTO_SRC" && -f "$PHOTO_SRC" ]]; then
  cp "$PHOTO_SRC" "$ROOT/media/aerial-coastline.jpg"
else
  # A gradient, via Ghostscript, so the beat still has a real image to show.
  gs -q -dNOPAUSE -dBATCH -sDEVICE=jpeg -r72 -g800x600 \
     -sOutputFile="$ROOT/media/aerial-coastline.jpg" \
     -c "0 0 1 setrgbcolor 0 0 800 600 rectfill showpage" >/dev/null 2>&1 || true
fi

# A multi-page PDF for the paging beat. PostScript in, PDF out — no extra
# dependency beyond the Ghostscript that is already here.
{
  for page in 1 2 3 4 5; do
    cat <<PS
%%Page: $page $page
/Helvetica-Bold findfont 42 scalefont setfont
72 680 moveto (Quarterly Report) show
/Helvetica findfont 22 scalefont setfont
72 630 moveto (Page $page of 5) show
/Helvetica findfont 13 scalefont setfont
72 580 moveto (This document exists so the preview pane has more than one) show
72 560 moveto (page to turn. j and k move between pages; G jumps to the last.) show
72 500 moveto (Section $page) show
showpage
PS
  done
} > "$ROOT/media/.report.ps"
gs -q -dNOPAUSE -dBATCH -sDEVICE=pdfwrite \
   -sOutputFile="$ROOT/media/quarterly-report.pdf" "$ROOT/media/.report.ps" >/dev/null 2>&1 || true
rm -f "$ROOT/media/.report.ps"

filler 240000 "$ROOT/media/screencast.mp4"
filler 180000 "$ROOT/media/soundtrack.mp3"

# ---------------------------------------------------------------- release/
# Badly named files, for the regex-rename beat. The mixed set is the point:
# `gr` skips what the pattern does not match rather than failing on it.
mkdir -p "$ROOT/release"
for n in 0042 0043 0044 0051 0067; do
  filler 12000 "$ROOT/release/IMG_${n}_final_v2.jpg"
done
filler 800 "$ROOT/release/notes.txt"
filler 800 "$ROOT/release/checksums.txt"

# ---------------------------------------------------------------- archives/
mkdir -p "$ROOT/archives"
STAGE="$(mktemp -d)"
mkdir -p "$STAGE/bundle/config" "$STAGE/bundle/assets"
cat > "$STAGE/bundle/config/settings.toml" <<'TOML'
[server]
host = "0.0.0.0"
port = 8080
workers = 4

[logging]
level = "info"
format = "json"
TOML
cat > "$STAGE/bundle/README.md" <<'MD'
# Bundle

An ordinary archive. Press Enter on it in myd and it becomes a tree:
filter it, sort it, view it as a treemap, preview what is inside, and
press c to extract. It stays read-only throughout.
MD
filler 30000 "$STAGE/bundle/assets/sprites.bin"
filler 8000  "$STAGE/bundle/assets/palette.dat"
filler 5000  "$STAGE/bundle/CHANGELOG.md"

( cd "$STAGE" && zip -qr "$ROOT/archives/bundle.zip" bundle )
( cd "$STAGE" && tar czf "$ROOT/archives/backup.tar.gz" bundle )
rm -rf "$STAGE"

# ---------------------------------------------------------------- logs/
# A wide, skewed directory: gives the treemap a dominant tile and a long tail,
# and gives sorting and filtering enough rows to be worth watching.
mkdir -p "$ROOT/logs"
filler 900000 "$ROOT/logs/access.log"
filler 120000 "$ROOT/logs/error.log"
for i in $(seq -w 1 12); do
  filler $((3000 + 10#$i * 700)) "$ROOT/logs/rotated-$i.log.txt"
done

# ---------------------------------------------------------------- dotfiles
# For the H (hidden files) beat.
filler 400 "$ROOT/.editorconfig"
filler 600 "$ROOT/.gitignore"
mkdir -p "$ROOT/.cache"
filler 20000 "$ROOT/.cache/index.bin"

# ---------------------------------------------------------------- transfer/
# The local side of the dual-panel copy beat.
mkdir -p "$ROOT/inbox"
filler 300 "$ROOT/inbox/.keep"

# Fixed mtimes so the time-based sorts (5 = newest) are stable across runs, and
# ordered so that "newest" is visibly different from alphabetical.
touch -d '2026-01-04 09:15:00' "$ROOT/logs"/*.log "$ROOT/logs"/*.log.txt 2>/dev/null || true
touch -d '2026-02-11 14:02:00' "$ROOT/project/src"/* 2>/dev/null || true
touch -d '2026-03-19 08:40:00' "$ROOT/project/docs"/* 2>/dev/null || true
touch -d '2026-04-27 17:31:00' "$ROOT/release"/* 2>/dev/null || true
touch -d '2026-05-30 11:20:00' "$ROOT/archives"/* 2>/dev/null || true
touch -d '2026-06-08 19:05:00' "$ROOT/media"/* 2>/dev/null || true

echo "fixtures built at $ROOT" >&2
