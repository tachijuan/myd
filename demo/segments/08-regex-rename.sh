#!/usr/bin/env bash
# Beat 8 — renaming a whole set by regex, with the result visible before it runs.
set -euo pipefail

narrate "five files out of a camera. the names are useless."
clear_pane

start_myd '"$DEMO_ROOT/release"'
expect "File Tree" "tree opened"
send l
settle 4
hold 2.0

# Tag three of the five, leaving two behind: the point is that `gr` renames the
# staged set and skips whatever the pattern doesn't match.
send j; send t; hold 0.5
send j; send t; hold 0.5
send j; send t; hold 1.4

send g; send r
settle 4
expect "Patterned rename" "rename dialog"
hold 1.6

# The pattern, typed slowly enough to read.
type_text 'IMG_([0-9]+)_final_v2\.jpg' 0.07
hold 1.2
send Tab 0.6
type_text 'release-$1.jpg' 0.07
hold 1.0

# The dialog previews the transformation against the first tagged file before
# anything is renamed — this is the frame worth pausing on.
expect "→" "live preview of the rename"
hold 3.2

send Enter 1.0
settle 5
expect "release-0042.jpg" "files renamed"
hold 3.0

quit_myd
