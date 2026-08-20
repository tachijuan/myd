#!/usr/bin/env bash
# Record the myd feature tour.
#
#   ./record.sh              # every segment, then the concatenated full tour
#   ./record.sh 03           # one segment, by number or name fragment
#   ./record.sh --no-concat  # segments only
#   ./record.sh --concat     # rebuild the full tour from existing casts
#
# Each segment is recorded separately, so a beat that needs re-shooting costs
# one segment rather than the whole tour. The full tour is those same casts
# concatenated — asciicast v3 timestamps are relative intervals, so appending
# event lines is all that joining requires.
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO="$(cd "$HERE/.." && pwd)"

export MYD_BIN="${MYD_BIN:-$REPO/myd/target/release/myd}"
export COLS="${COLS:-120}"
export ROWS="${ROWS:-34}"
export SESSION="${SESSION:-myddemo}"

CAST_DIR="$HERE/cast"
WORK="${DEMO_WORK:-${TMPDIR:-/tmp}/myd-demo-$$}"
export DEMO_ROOT="$WORK/root"
export XDG_CONFIG_HOME="$WORK/cfg"      # sandboxes hosts.toml AND prefs.toml
export MYD_PREVIEW_GRAPHICS="${MYD_PREVIEW_GRAPHICS:-blocks}"

# Blocks, deliberately: kitty/iTerm2/sixel image data is written straight to
# stdout outside the ratatui frame, and that byte stream does not survive into a
# cast. Block rendering is ordinary truecolor text, which records and replays
# exactly. See myd/src/preview/graphics.rs.

die() { printf '\033[38;5;203merror: %s\033[0m\n' "$*" >&2; exit 1; }
info() { printf '\033[38;5;68m==> %s\033[0m\n' "$*" >&2; }

[[ -x "$MYD_BIN" ]] || die "myd binary not found at $MYD_BIN (cargo build --release)"
command -v asciinema >/dev/null || die "asciinema not installed (brew install asciinema)"
command -v tmux >/dev/null || die "tmux not installed"

mkdir -p "$CAST_DIR" "$WORK"

cleanup() {
  tmux kill-session -t "$SESSION" 2>/dev/null || true
  "$REPO/myd/scripts/sftp_test_env.sh" stop >/dev/null 2>&1 || true
  [[ -n "${KEEP_WORK:-}" ]] || rm -rf "$WORK"
}
trap cleanup EXIT

# Record one segment: fixtures, a live tmux session, asciinema attached to it,
# and the segment script driving from outside.
# The remote beat needs a server. Start the isolated sshd that the SFTP
# integration tests already use — its own host key, client key and agent on
# 127.0.0.1 — so the tour never needs a real host or a real credential.
start_sftp() {
  local env_script="$REPO/myd/scripts/sftp_test_env.sh"
  [[ -x "$env_script" ]] || die "missing $env_script"
  info "starting the sandboxed sftp server"
  eval "$("$env_script" 2>/dev/null)"
  export SSH_AUTH_SOCK
  local port="${MYD_SFTP_PORT:-22022}"
  export DEMO_SFTP_URL="sftp://$(id -un)@127.0.0.1:$port/tmp/myd-sftp-test/data"

  # HOME is redirected at the pane, exactly as tests/sftp_integration.rs does
  # it, so that myd's known_hosts write lands in the harness's throwaway home
  # rather than the user's real ~/.ssh.
  #
  # This is not only tidiness. The harness generates a fresh host key on every
  # start, so an entry left in the real known_hosts makes the *next* run look
  # like a changed host key, and myd rightly refuses to connect. Sandboxing HOME
  # keeps each recording independent of the last.
  export DEMO_HOME="${MYD_SFTP_TEST_HOME:-/tmp/myd-sftp-test/fakehome}"
  mkdir -p "$DEMO_HOME/.ssh"; chmod 700 "$DEMO_HOME/.ssh"
}

record_one() { # record_one <script>
  local script="$1"
  local name; name="$(basename "$script" .sh)"
  local out="$CAST_DIR/$name.cast"

  info "recording $name"
  "$HERE/fixtures.sh" "$DEMO_ROOT" 2>/dev/null
  rm -rf "$XDG_CONFIG_HOME"; mkdir -p "$XDG_CONFIG_HOME"

  # Only the remote beat pays for a server. start_session (lib.sh) forwards
  # SSH_AUTH_SOCK and DEMO_SFTP_URL into the pane when they are set.
  if [[ "$name" == *remote* ]]; then
    start_sftp
  fi

  # The segment needs the session up before asciinema attaches to it.
  ( set -euo pipefail
    source "$HERE/lib.sh"
    start_session
    tmux set-option -t "$SESSION" status off
  )

  # Drive the pane while the recorder watches it. The driver runs in the
  # background; asciinema in the foreground holds the recording open until the
  # pane exits.
  # The segment runs as its own process, not sourced into a `||` or `if`, so
  # that `set -e` still applies inside it: a failed `expect` must abandon the
  # segment at once rather than record the rest of a broken beat. (Bash disables
  # -e for the whole of a sourced script whose result is being tested.)
  #
  # Either way the pane is closed afterwards, because `tmux attach` — and so the
  # recorder waiting on it — only returns when the session ends. Skipping that
  # on the failure path would hang the run instead of reporting it.
  ( sleep 1.0
    rc=0
    bash -euo pipefail -c 'source "$1"; source "$2"' _ "$HERE/lib.sh" "$script" || rc=$?
    sleep 0.8
    tmux kill-session -t "$SESSION" 2>/dev/null || true
    exit "$rc"
  ) & local driver=$!

  asciinema rec --headless --window-size "${COLS}x${ROWS}" \
    -t "myd — $name" -i 2.0 --overwrite \
    -c "tmux attach -t $SESSION -r" "$out" >/dev/null 2>&1 || true

  local rc=0; wait "$driver" || rc=$?
  tmux kill-session -t "$SESSION" 2>/dev/null || true

  (( rc == 0 )) || die "$name failed its assertions (see above)"
  [[ -s "$out" ]] || die "$name produced no cast"
  info "  -> $out ($(wc -l < "$out") events)"
}

# Join the chapter casts into one tour. v3 intervals are relative, so the events
# simply follow one another; only the first file's header is kept.
concat_all() {
  local out="$CAST_DIR/00-full-tour.cast"
  local first=1 n=0
  : > "$out.tmp"
  # Numeric order, so 10 and 11 follow 9 rather than sorting beside 1. The tour
  # is assembled in beat order; `00-` is the output and never an input.
  local casts=()
  while IFS= read -r cast; do casts+=("$cast"); done < <(
    find "$CAST_DIR" -maxdepth 1 -name '[0-9][0-9]-*.cast' ! -name '00-*' | sort -V
  )
  for cast in "${casts[@]}"; do
    [[ -f "$cast" ]] || continue
    if (( first )); then
      # The first chapter's header becomes the tour's, retitled. Edited as JSON
      # rather than with sed: removing a field textually leaves a stray comma
      # and the result is not parseable.
      head -1 "$cast" | python3 -c '
import json, sys
header = json.loads(sys.stdin.readline())
header["title"] = "myd — a feature tour"
json.dump(header, sys.stdout, ensure_ascii=False)
print()
' >> "$out.tmp"
      first=0
    fi
    # Events only. The header goes, and so does each chapter's teardown: the
    # exit marker, the `[exited]` line tmux prints when the pane closes, and the
    # alternate-screen restore that follows it. Left in, the tour would announce
    # its own ending once per chapter.
    tail -n +2 "$cast" \
      | grep -v '"x"' \
      | grep -v '\[exited\]' >> "$out.tmp"
    n=$((n + 1))
  done
  (( n > 0 )) || die "no chapter casts to concatenate"
  mv "$out.tmp" "$out"
  info "full tour: $out ($n chapters, $(wc -l < "$out") events)"
}

# ---------------------------------------------------------------- main

filter="" do_concat=1 only_concat=0
for arg in "$@"; do
  case "$arg" in
    --no-concat) do_concat=0 ;;
    --concat)    only_concat=1 ;;
    -h|--help)   sed -n '2,10p' "$0"; exit 0 ;;
    *)           filter="$arg" ;;
  esac
done

if (( only_concat )); then concat_all; exit 0; fi

shopt -s nullglob
scripts=("$HERE"/segments/*.sh)
(( ${#scripts[@]} )) || die "no segments found"

matched=0
for script in "${scripts[@]}"; do
  if [[ -n "$filter" ]]; then
    [[ "$(basename "$script")" == *"$filter"* ]] || continue
  fi
  record_one "$script"
  matched=$((matched + 1))
done

(( matched )) || die "no segment matched '$filter'"

if (( do_concat )) && [[ -z "$filter" ]]; then
  concat_all
fi

info "done — play with: asciinema play $CAST_DIR/00-full-tour.cast"
