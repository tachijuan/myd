#!/usr/bin/env bash
# Shared helpers for driving myd inside a tmux pane while asciinema records it.
#
# The recording is a real myd process being typed at — not a reconstruction — so
# what lands in the cast is what the program actually drew. tmux is the seam:
# `send-keys` types, `capture-pane` reads the frame back, which is what lets a
# segment wait for a state and assert it arrived instead of sleeping and hoping.

set -euo pipefail

MYD_BIN="${MYD_BIN:?MYD_BIN must point at the myd binary}"
SESSION="${SESSION:-myddemo}"
DEMO_ROOT="${DEMO_ROOT:?DEMO_ROOT must be set}"

# Pacing. Overridable so a segment can slow down for a beat that needs reading.
KEY_PAUSE="${KEY_PAUSE:-0.45}"       # between ordinary keystrokes
TYPE_DELAY="${TYPE_DELAY:-0.055}"    # per character when typing text
READ_PAUSE="${READ_PAUSE:-1.6}"      # to let the viewer take a frame in

# ---------------------------------------------------------------- pane control

# Start the demo session running a shell. The recording attaches to this pane
# (see record.sh), so everything that happens in it — narration typed at the
# prompt and myd itself — lands in the cast as one continuous session.
#
# The status bar is turned off: it is tmux's furniture, not myd's, and a green
# bar across the bottom of every frame would read as part of the program.
start_session() {
  local env_args=(
    -e "XDG_CONFIG_HOME=$XDG_CONFIG_HOME"
    -e "MYD_PREVIEW_GRAPHICS=${MYD_PREVIEW_GRAPHICS:-blocks}"
    -e "MYD_BIN=$MYD_BIN"
    -e "DEMO_ROOT=$DEMO_ROOT"
  )
  # The remote beat's server, when record.sh has started one. DEMO_HOME
  # redirects HOME so the known_hosts entry for the throwaway server lands in
  # the harness's home rather than the user's.
  [[ -n "${SSH_AUTH_SOCK:-}" ]] && env_args+=(-e "SSH_AUTH_SOCK=$SSH_AUTH_SOCK")
  [[ -n "${DEMO_SFTP_URL:-}" ]] && env_args+=(-e "DEMO_SFTP_URL=$DEMO_SFTP_URL")
  [[ -n "${DEMO_HOME:-}"     ]] && env_args+=(-e "HOME=$DEMO_HOME")

  tmux kill-session -t "$SESSION" 2>/dev/null || true
  tmux new-session -d -s "$SESSION" -x "${COLS:-120}" -y "${ROWS:-34}" \
    "${env_args[@]}" \
    "${DEMO_SHELL:-bash --noprofile --norc}"

  # `-x/-y` above is only a starting size: tmux resizes a session to fit whoever
  # attaches, and the recorder attaching at its own size would silently reshape
  # the pane. That is not cosmetic — myd draws its footer on the last row, so a
  # pane taller than the recorded window puts the status line, and the VISUAL /
  # tagged indicators on it, permanently outside the frame.
  tmux set-option -t "$SESSION" window-size manual 2>/dev/null || true
  tmux resize-window -t "$SESSION" -x "${COLS:-120}" -y "${ROWS:-34}" 2>/dev/null || true
  # A clean, minimal prompt: the demo's voice is the narration, and a real
  # user's $PS1 would be noise that differs on every machine.
  tmux send-keys -t "$SESSION" -l \
    'PS1="\[\033[38;5;244m\]\$ \[\033[0m\]"; clear; unset HISTFILE'
  tmux send-keys -t "$SESSION" Enter
  sleep 0.6
}

# Run myd in the demo pane and wait for its first frame.
start_myd() { # start_myd [args...]
  tmux send-keys -t "$SESSION" -l "\$MYD_BIN $*"
  tmux send-keys -t "$SESSION" Enter
  settle 8
}

# Leave myd and get the prompt back for the next narration line.
quit_myd() {
  send q 0.6
  # A confirmation may be up (transfers running); answering it is harmless
  # when it is not.
  settle 4
}

# The pane as plain text, no escapes.
frame() {
  tmux capture-pane -t "$SESSION" -p 2>/dev/null || true
}

# ---------------------------------------------------------------- timing

# Wait until the frame stops changing, so pacing follows the program rather than
# a guessed sleep. Bounded: a pane that never settles must not hang the record.
settle() { # settle [max_seconds]
  local max="${1:-6}" prev="" cur="" stable=0 waited=0
  while (( $(echo "$waited < $max" | bc -l) )); do
    cur="$(frame)"
    if [[ "$cur" == "$prev" && -n "$cur" ]]; then
      stable=$((stable + 1))
      (( stable >= 2 )) && return 0
    else
      stable=0
    fi
    prev="$cur"
    sleep 0.25
    waited="$(echo "$waited + 0.25" | bc -l)"
  done
  return 0
}

# Wait for a pattern to appear in the frame. Returns non-zero if it never does.
wait_for() { # wait_for <pattern> [max_seconds]
  local pat="$1" max="${2:-10}" waited=0
  while (( $(echo "$waited < $max" | bc -l) )); do
    frame | grep -qF -- "$pat" && return 0
    sleep 0.25
    waited="$(echo "$waited + 0.25" | bc -l)"
  done
  return 1
}

# ---------------------------------------------------------------- input

# Send keys as myd sees them. Names like Enter/Escape/Space pass through tmux's
# key vocabulary; anything else is sent literally (-l) so that characters which
# are also tmux key names — 'v', 'c', 'q' — are not reinterpreted.
send() { # send <key> [pause]
  local key="$1" pause="${2:-$KEY_PAUSE}"
  case "$key" in
    Enter|Escape|Space|Tab|BSpace|Up|Down|Left|Right|C-*|M-*|F1)
      tmux send-keys -t "$SESSION" "$key" ;;
    *)
      tmux send-keys -t "$SESSION" -l "$key" ;;
  esac
  sleep "$pause"
}

# Send several keys in order, e.g. `keys g d` or `keys j j j`.
keys() { local k; for k in "$@"; do send "$k"; done; }

# Type a string character by character, so it reads as typing rather than paste.
type_text() { # type_text <text> [per_char_delay]
  local text="$1" delay="${2:-$TYPE_DELAY}" i ch
  for (( i = 0; i < ${#text}; i++ )); do
    ch="${text:i:1}"
    tmux send-keys -t "$SESSION" -l "$ch"
    sleep "$delay"
  done
}

# ---------------------------------------------------------------- narration

# Type a `# ...` comment at the demo pane's prompt, between segments. This is
# the demo's voice: it sets up what the next reveal is meant to show.
#
# It is typed into the pane rather than printed by this script, because the
# recording watches the pane. Anything this script writes to its own stdout is
# not in the cast.
narrate() { # narrate <text...>
  type_text "# $*"
  sleep 0.5
  tmux send-keys -t "$SESSION" Enter
  sleep "${NARRATE_PAUSE:-1.2}"
}

# Clear the demo pane, so a beat starts on an empty screen.
clear_pane() {
  tmux send-keys -t "$SESSION" -l "clear"
  tmux send-keys -t "$SESSION" Enter
  sleep 0.4
}

# A pause on a cleared screen, used to separate beats.
beat() { sleep "${1:-$READ_PAUSE}"; }

# ---------------------------------------------------------------- assertions

# Fail the recording when the screen does not show what the segment claims to
# demonstrate. Without this a broken beat records silently and is only noticed
# when someone watches the finished cast.
expect() { # expect <pattern> [label]
  local pat="$1" label="${2:-$1}"
  if frame | grep -qF -- "$pat"; then
    printf '\033[38;5;71m  ✓ %s\033[0m\n' "$label" >&2
  else
    printf '\033[38;5;203m  ✗ expected %s\033[0m\n' "$label" >&2
    printf '%s\n' "----- frame -----" >&2
    frame >&2
    printf '%s\n' "-----------------" >&2
    return 1
  fi
}

# Leave the current frame up for a moment, so a viewer can read it.
hold() { beat "${1:-$READ_PAUSE}"; }

# ---------------------------------------------------------------- selection

# The name on the selected row of the pane that currently has focus.
#
# Both panes of a split draw their own `›` cursor, so the marker alone does not
# say which one is focused, and they share screen lines so a naive match reads
# whichever pane is leftmost. `pane_col` therefore fixes which column range to
# read, and the row is cut at the next pane boundary.
#
# PANE_COL: 0 = single pane or the left one, 1 = the right pane of a split.
selected() { # selected [pane_index]
  local col="${1:-${PANE_COL:-0}}"
  frame | awk -v want="$col" '
    {
      # Split the line at pane boundaries drawn between the two trees.
      n = split($0, part, /[█║][│┃]/)
      seg = (want + 1 <= n) ? part[want + 1] : part[1]
      if (seg ~ /›/) { print seg; exit }
    }' |
    sed -e 's/.*›[[:space:]]*//' \
        -e 's/[│┃║].*$//' \
        -e 's/^[^[:alnum:]._-]*//' \
        -e 's/[[:space:]]*[█▲▼]*[[:space:]]*$//' \
        -e 's/[[:space:]]*$//'
}

# Move the cursor onto a named entry by pressing j, and fail if it never lands.
# Segments should never assume a sort order puts a file at a known offset — the
# demo's sort mode changes, and a fixture added later shifts every position.
select_file() { # select_file <name> [max_steps]
  local want="$1" max="${2:-25}" i
  for (( i = 0; i < max; i++ )); do
    [[ "$(selected)" == *"$want"* ]] && { printf '\033[38;5;71m  ✓ on %s\033[0m\n' "$want" >&2; return 0; }
    send j 0.28
  done
  printf '\033[38;5;203m  ✗ never reached %s (stopped on "%s")\033[0m\n' "$want" "$(selected)" >&2
  frame >&2
  return 1
}
