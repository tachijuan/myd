#!/usr/bin/env bash
# Beat 4 — photographs and PDFs, in a terminal.
#
# Recorded with MYD_PREVIEW_GRAPHICS=blocks. On a kitty/iTerm2/sixel terminal
# myd hands the image over as real pixel data and it looks far better than this
# — but that byte stream is written outside the ratatui frame and does not
# survive into a cast, so the tour shows the fallback that records honestly.
set -euo pipefail

narrate "text is easy. what about a photograph?"
clear_pane

start_myd '"$DEMO_ROOT/media"'
expect "File Tree" "tree opened"
hold 1.0

select_file "aerial-coastline.jpg"
send Space
settle 8
hold 3.2

send Space 0.8      # close, so the next beat starts clean
settle 3
quit_myd

narrate "and a PDF — j and k turn the pages, G jumps to the last"
clear_pane

start_myd '"$DEMO_ROOT/media"'
settle 3
select_file "quarterly-report.pdf"
send Space
settle 8
expect "page" "pdf page indicator"
hold 2.6

send j
settle 6
hold 2.2
send j
settle 6
hold 2.2
send G
settle 6
hold 2.6

send Space 0.8
settle 2
quit_myd
