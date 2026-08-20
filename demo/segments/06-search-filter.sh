#!/usr/bin/env bash
# Beat 6 — regex, as a first-class way to look at a tree.
set -euo pipefail

narrate "in a big tree, the question is usually 'where is the thing matching…'"
clear_pane

start_myd '"$DEMO_ROOT"'
expect "File Tree" "tree opened"

# Expand first: both search and filter act on what is expanded, so a collapsed
# tree would match nothing and look broken.
send '*'
settle 5
hold 1.6

# / searches names by regex; n and p step the matches.
send /
sleep 0.5
type_text 'rotated-0[1-9]'
send Enter 0.9
settle 4
hold 2.0
send n; hold 1.1
send n; hold 1.1
send n; hold 1.4

# f filters instead: everything that doesn't match disappears, at every level.
send f
sleep 0.5
type_text '\.(rs|py)$'
send Enter 0.9
settle 5
expect "FILTERED" "filter is announced in the title"
hold 3.0

# Esc clears it and the whole tree comes back.
send Escape 0.8
settle 4
hold 1.6

# A bad pattern is reported, not ignored.
send f
sleep 0.5
type_text '[unclosed'
send Enter 0.9
settle 4
hold 2.4
send Escape 0.8
settle 3

quit_myd
