#!/usr/bin/env bash
# Beat 7 — staging a set of files, and carrying it between views.
set -euo pipefail

narrate "operations act on one file — unless you stage several first"
clear_pane

start_myd '"$DEMO_ROOT/release"'
expect "File Tree" "tree opened"
send l
settle 4
hold 1.2

# t tags one at a time.
send j
send t
hold 0.9
send j
send t
hold 1.4

# V sweeps a range: move with j and every row crossed is tagged.
send j
send V
settle 2
keys j j
hold 1.6
# A second V leaves visual mode and keeps the range. (Esc here would back out
# of the browser itself, since visual mode is not a state it unwinds.)
send V 0.6
settle 2
hold 1.6

# The tags belong to the tree, not to the view: switch to the treemap and the
# same files are still staged, marked with an outline and a ▶.
send v
settle 5
expect "Treemap" "treemap view"
hold 2.6
send v
settle 4
hold 1.4

# U clears the staging.
send U
settle 3
hold 1.6

quit_myd
