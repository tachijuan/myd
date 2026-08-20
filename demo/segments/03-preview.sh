#!/usr/bin/env bash
# Beat 3 — it reads files, not just lists them.
set -euo pipefail

narrate "browsing is only half of it. myd reads what is in the file, too."
clear_pane

start_myd '"$DEMO_ROOT/project"'
expect "File Tree" "tree opened"

# The info panel: permissions, owner, timestamps, without leaving the tree.
send C-p
settle 3
hold 1.8

# Walk into src/ and open the preview on a real source file.
send l
settle 3
select_file "src"
send l
settle 3
select_file "main.rs"
hold 0.8
send Space
settle 6
expect "main.rs" "preview open on main.rs"
hold 2.2

# The pane has focus: vi motions move through the file, not the tree.
keys j j j j j
hold 1.2
send C-f
hold 1.6
send C-b
hold 1.2

# Search inside the file, and step the matches.
send /
sleep 0.5
type_text "widget"
send Enter 0.9
settle 3
hold 1.6
send n; hold 1.0
send n; hold 1.4
send Escape 0.6

# Focus is back on the tree, and the preview follows the cursor.
send j
settle 3
hold 1.8
send j
settle 3
hold 1.8

send Space 0.6      # close the preview
send C-p 0.6        # and the info panel
settle 2

quit_myd
