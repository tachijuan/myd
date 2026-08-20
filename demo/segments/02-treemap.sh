#!/usr/bin/env bash
# Beat 2 — the same tree, as space. It stops being a file list and becomes a
# disk analyser.
set -euo pipefail

narrate "a list tells you what is there. it doesn't tell you where the space went."
clear_pane

start_myd '"$DEMO_ROOT/logs"'
expect "File Tree" "tree opened"
hold 1.2

# The size bars already hint at the shape; the treemap makes it the whole view.
send v
settle 4
expect "Treemap" "treemap view"
hold 2.0

# Same hjkl, on tiles instead of rows.
keys l l
hold 1.0
keys j j
hold 1.0
send h
hold 1.6

# The legend names the colours, and tiles are coloured by content type.
send k
hold 2.0

# Back to the tree, with the same entry still selected.
send v
settle 3
expect "File Tree" "back to the tree"
hold 1.4

quit_myd
