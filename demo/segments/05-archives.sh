#!/usr/bin/env bash
# Beat 5 — an archive is just a directory you haven't opened yet.
set -euo pipefail

narrate "a zip file. most browsers make you extract it first."
clear_pane

start_myd '"$DEMO_ROOT/archives"'
expect "File Tree" "tree opened"
hold 1.0

# space peeks inside without extracting anything.
select_file "bundle.zip"
send Space
settle 6
expect "bundle" "archive listing in the preview"
hold 2.6
send Space 0.8
settle 2

# Enter goes *in*: from here it behaves like any other tree.
send Enter
settle 6
hold 2.4

# Walk down into it. Members are listed as the archive stores them, and each
# level is read on demand, so descend a level at a time rather than expanding
# the whole thing at once.
select_file "bundle" 8
send l
settle 4
hold 1.6

# The treemap works in here too — sizes inside an archive are real recursive
# totals, because every member's size is already in the index.
send v
settle 4
expect "Treemap" "treemap inside the archive"
hold 2.4
send v
settle 3

# Preview a file that only exists inside the archive.
select_file "config" 10
send l
settle 4
select_file "settings.toml" 10
send Space
settle 6
hold 2.8
send Space 0.8
settle 2

# It is read-only throughout, which the pane title says rather than letting a
# delete half-work. `q` backs out one level at a time and then leaves.
send q
settle 4
hold 1.6

quit_myd
