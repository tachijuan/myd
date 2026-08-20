#!/usr/bin/env bash
# Beat 10 — the same browser, on another machine.
#
# The server is the isolated sshd from myd/scripts/sftp_test_env.sh: its own
# host key, its own client key, its own agent, on 127.0.0.1:22022. Nothing here
# touches a real host or a real credential. record.sh starts it and exports
# SSH_AUTH_SOCK before this segment runs.
set -euo pipefail

: "${DEMO_SFTP_URL:?record.sh must export DEMO_SFTP_URL}"

narrate "everything so far was local. none of it had to be."
clear_pane

start_myd '"$DEMO_SFTP_URL"'
if ! wait_for "SFTP" 20; then
  printf '\033[38;5;203m  ✗ never connected to the demo host\033[0m\n' >&2
  frame >&2; exit 1
fi
printf '\033[38;5;71m  ✓ connected over SFTP\033[0m\n' >&2
settle 6
hold 2.6

# Remote directories show a dash rather than a size: measuring one would be a
# round trip per directory, so myd declines to guess.
send j
settle 3
hold 1.6
send l
settle 5
hold 2.0
send h
settle 3

# The preview reads from the server, not from a same-named local path.
select_file "greeting.txt" 12
send Space
settle 6
hold 2.6
send Space 0.8
settle 3

quit_myd

narrate "and with a local pane beside it, you can move files between them"
clear_pane

# Remote on the left, local on the right.
start_myd '"$DEMO_SFTP_URL" "$DEMO_ROOT/inbox"'
if ! wait_for "SFTP" 20; then
  printf '\033[38;5;203m  ✗ split did not connect\033[0m\n' >&2
  frame >&2; exit 1
fi
settle 6
hold 2.2

# Focus the remote (left) pane before staging: the copy has to run *from* the
# server for the transfer queue to be involved at all.
#
# Which pane starts focused is not worth assuming — it depends on how the panes
# were opened — so press Tab until the cursor actually moves on the left, up to
# one full rotation (two panels, plus the transfer panel once it exists).
export PANE_COL=0
focused_left() {
  local before after
  before="$(selected 0)"
  send j 0.3
  after="$(selected 0)"
  [[ "$before" != "$after" ]]
}
for _ in 1 2 3; do
  focused_left && break
  send Tab
  settle 3
done
hold 1.2

# Stage the large file and copy it across: 6 MB, so the transfer is long enough
# to watch the progress bar, rate and ETA actually move.
select_file "blob.bin" 12
send t
hold 1.0
send c
settle 4

# The transfer panel appears on its own and shows progress, rate and ETA while
# the interface stays interactive.
if wait_for "Transfers" 10 || wait_for "blob.bin" 10; then
  printf '\033[38;5;71m  ✓ transfer panel\033[0m\n' >&2
fi
hold 3.2
settle 8
hold 2.4

# Ctrl+t hides it again.
send C-t
settle 3
hold 1.6

quit_myd
