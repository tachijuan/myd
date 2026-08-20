#!/usr/bin/env bash
# Beat 9 — two places at once.
set -euo pipefail

narrate "one pane is a browser. two panes is a file manager."
clear_pane

start_myd '"$DEMO_ROOT/project" "$DEMO_ROOT/inbox"'
settle 5
expect "project" "left panel"
expect "inbox" "right panel"
hold 2.0

# Tab moves the focus between the panes.
send Tab
settle 3
hold 1.4
send Tab
settle 3
hold 1.4

# Stage a few files on the left.
send l
settle 4
send j; send t; hold 0.5
send j; send t; hold 0.5
send j; send t; hold 1.4

# c copies them into whatever the other pane is showing.
send c
settle 5
hold 2.2

# Look at the result on the right: the copied files are really there.
send Tab
settle 4
send r
settle 5
if [[ ! -e "$DEMO_ROOT/inbox/catalogue.tsv" ]]; then
  printf '\033[38;5;203m  ✗ the copy did not land in inbox/\033[0m\n' >&2
  ls -la "$DEMO_ROOT/inbox" >&2; exit 1
fi
printf '\033[38;5;71m  ✓ files copied into the other panel\033[0m\n' >&2
hold 2.6

# N makes a directory, R renames — the ordinary management keys, on either side.
send N
settle 3
type_text 'approved'
send Enter 0.8
settle 4
expect "approved" "directory created"
hold 2.2

# | collapses back to a single pane whenever the split is not wanted.
send '|'
settle 4
hold 1.8

quit_myd
