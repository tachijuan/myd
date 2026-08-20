#!/usr/bin/env bash
# Beat 1 — it's vi, for your filesystem.
set -euo pipefail

narrate "myd — a vi-like file browser for the terminal"
narrate "every key you already know from vi works here"
clear_pane

start_myd '"$DEMO_ROOT"'
expect "File Tree" "tree opened"
hold

# j/k move, l expands, h collapses. Slow enough to follow.
keys j j j
hold 1.0
send l          # expand project/
settle 3
keys j j
hold 1.0
send h          # back out
send k

# Jump to the ends.
send G
hold 1.2
send g; send g
hold 1.2

# Sizes are real, recursive, and sorted. `s` cycles the order.
send s
settle 3
expect "Sort:" "sort indicator"
hold 1.4
send s
settle 3
hold 1.2

# Column toggles: permissions, mtime, size bars.
send P
hold 1.2
send T
hold 1.4
send P
send T

# Hidden files are shown by default; H hides them, and again brings them back.
expect ".editorconfig" "dotfiles visible to begin with"
send H
settle 3
if frame | grep -qF ".editorconfig"; then
  printf '\033[38;5;203m  ✗ H did not hide dotfiles\033[0m\n' >&2; exit 1
fi
printf '\033[38;5;71m  ✓ dotfiles hidden\033[0m\n' >&2
hold 1.4
send H
settle 3
expect ".editorconfig" "dotfiles back"

quit_myd
