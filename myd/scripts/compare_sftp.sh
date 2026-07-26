#!/usr/bin/env bash
# Compare myd's transfer engine against the native `sftp` binary on the same
# link and the same file.
#
# The simulated-latency benchmarks can't model TCP congestion control, so this
# is the arbiter for "are we at parity with sftp?" on a real high-latency link.
#
# Usage:
#   scripts/compare_sftp.sh user@host [size] [remote_dir]
#
#   size        payload to generate, in `dd`-friendly form (default 256M)
#   remote_dir  writable directory on the remote (default /tmp)
#
# Example:
#   scripts/compare_sftp.sh juan@prod.example.com 512M /srv/scratch
#
# Uses key-based auth only — it runs sftp in batch mode, so a password prompt
# would hang. Set up ssh-agent or a key first.

set -uo pipefail

TARGET="${1:-}"
SIZE="${2:-256M}"
REMOTE_DIR="${3:-/tmp}"

if [[ -z "$TARGET" ]]; then
    sed -n '2,20p' "$0" | sed 's/^# \{0,1\}//'
    exit 1
fi

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
MYD_TRANSFER="$ROOT/target/release/myd-transfer"

if [[ ! -x "$MYD_TRANSFER" ]]; then
    echo "building myd-transfer (release)..." >&2
    (cd "$ROOT" && cargo build --release --bin myd-transfer) || exit 1
fi

WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"; ssh -o BatchMode=yes "$TARGET" "rm -f $REMOTE_DIR/myd-bench.bin" 2>/dev/null' EXIT

LOCAL_SRC="$WORK/payload.bin"
REMOTE_PATH="$REMOTE_DIR/myd-bench.bin"

echo "Generating $SIZE payload..." >&2
dd if=/dev/urandom of="$LOCAL_SRC" bs=1M count="${SIZE%[MG]}" \
   $([[ "$SIZE" == *G ]] && echo "count=$((${SIZE%G} * 1024))") 2>/dev/null
BYTES=$(stat -c %s "$LOCAL_SRC")
echo "Payload: $BYTES bytes" >&2

# Report a rate from a byte count and an elapsed seconds value.
rate() {
    awk -v b="$1" -v s="$2" 'BEGIN { if (s > 0) printf "%.2f", (b/1048576)/s; else printf "n/a" }'
}

# Time a command, echoing elapsed seconds.
timeit() {
    local start end
    start=$(date +%s.%N)
    "$@" >/dev/null 2>&1
    local rc=$?
    end=$(date +%s.%N)
    awk -v a="$start" -v b="$end" 'BEGIN { printf "%.3f", b-a }'
    return $rc
}

echo >&2
echo "=== Upload: local -> $TARGET:$REMOTE_PATH ===" >&2

SFTP_UP=$(timeit sftp -o BatchMode=yes -o Compression=no -b - "$TARGET" <<EOF
put $LOCAL_SRC $REMOTE_PATH
EOF
)
SFTP_UP_RC=$?

ssh -o BatchMode=yes "$TARGET" "rm -f $REMOTE_PATH" 2>/dev/null

MYD_UP=$(timeit "$MYD_TRANSFER" "$LOCAL_SRC" "sftp://$TARGET$REMOTE_PATH")
MYD_UP_RC=$?

echo >&2
echo "=== Download: $TARGET:$REMOTE_PATH -> local ===" >&2

# Make sure something is there to download.
sftp -o BatchMode=yes -o Compression=no -b - "$TARGET" >/dev/null 2>&1 <<EOF
put $LOCAL_SRC $REMOTE_PATH
EOF

SFTP_DOWN=$(timeit sftp -o BatchMode=yes -o Compression=no -b - "$TARGET" <<EOF
get $REMOTE_PATH $WORK/sftp-down.bin
EOF
)
SFTP_DOWN_RC=$?

MYD_DOWN=$(timeit "$MYD_TRANSFER" "sftp://$TARGET$REMOTE_PATH" "$WORK/myd-down.bin")
MYD_DOWN_RC=$?

# Verify myd actually moved the bytes correctly; a fast wrong answer is no good.
INTEGRITY="not checked"
if [[ -f "$WORK/myd-down.bin" ]]; then
    if cmp -s "$LOCAL_SRC" "$WORK/myd-down.bin"; then
        INTEGRITY="OK (byte-identical)"
    else
        INTEGRITY="*** MISMATCH ***"
    fi
fi

echo
printf '%-10s %-10s %10s %12s %8s\n' TOOL DIRECTION SECONDS "MiB/s" RATIO
printf '%-10s %-10s %10s %12s %8s\n' ---- --------- ------- ----- -----

SFTP_UP_R=$(rate "$BYTES" "$SFTP_UP")
MYD_UP_R=$(rate "$BYTES" "$MYD_UP")
SFTP_DOWN_R=$(rate "$BYTES" "$SFTP_DOWN")
MYD_DOWN_R=$(rate "$BYTES" "$MYD_DOWN")

printf '%-10s %-10s %10s %12s %8s\n' sftp upload   "$SFTP_UP"   "$SFTP_UP_R"   "1.00x"
printf '%-10s %-10s %10s %12s %8s\n' myd  upload   "$MYD_UP"    "$MYD_UP_R" \
    "$(awk -v a="$MYD_UP_R" -v b="$SFTP_UP_R" 'BEGIN { if (b>0) printf "%.2fx", a/b; else print "n/a" }')"
printf '%-10s %-10s %10s %12s %8s\n' sftp download "$SFTP_DOWN" "$SFTP_DOWN_R" "1.00x"
printf '%-10s %-10s %10s %12s %8s\n' myd  download "$MYD_DOWN"  "$MYD_DOWN_R" \
    "$(awk -v a="$MYD_DOWN_R" -v b="$SFTP_DOWN_R" 'BEGIN { if (b>0) printf "%.2fx", a/b; else print "n/a" }')"

echo
echo "Integrity: $INTEGRITY"
echo "Exit codes: sftp_up=$SFTP_UP_RC myd_up=$MYD_UP_RC sftp_down=$SFTP_DOWN_RC myd_down=$MYD_DOWN_RC"
echo
echo "Ratio > 1.00x means myd is faster than the native sftp client."
echo
echo "For deeper detail, re-run myd-transfer with diagnostics on:"
echo "  MYD_TRACE=1 MYD_TRACE_FILE=/tmp/myd.log $MYD_TRANSFER \\"
echo "      sftp://$TARGET$REMOTE_PATH /tmp/out.bin"
echo "  grep -E 'observed|transfer_complete' /tmp/myd.log"
