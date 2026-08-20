#!/usr/bin/env bash
# Beat 11 — the rest of it, and where to get it.
set -euo pipefail

narrate "there is more than fits in one recording — ? lists all of it"
clear_pane

start_myd '"$DEMO_ROOT"'
expect "File Tree" "tree opened"
hold 1.0

send '?'
settle 5
hold 4.0

# A second page of bindings, where there is one.
send C-f
settle 3
hold 3.0
send Escape 0.8
settle 3

quit_myd
clear_pane

narrate "myd — a vi-like file browser for the terminal"
narrate "cargo install --path myd"
hold 2.5
