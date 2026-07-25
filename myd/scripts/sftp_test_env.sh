#!/usr/bin/env bash
# Spin up an isolated sshd + ssh-agent for the gated SFTP integration tests, and
# print the environment to export. Touches nothing under the user's real ~/.ssh.
#
# Usage:
#   eval "$(scripts/sftp_test_env.sh)"
#   cargo test --test sftp_integration -- --test-threads=1
#   scripts/sftp_test_env.sh stop     # tear it down
set -euo pipefail

DIR="${MYD_SFTP_DIR:-/tmp/myd-sftp-test}"
PORT="${MYD_SFTP_PORT:-22022}"
SFTP_SERVER="${MYD_SFTP_SERVER:-/usr/lib/openssh/sftp-server}"

if [[ "${1:-start}" == "stop" ]]; then
  [[ -f "$DIR/sshd.pid" ]] && kill "$(cat "$DIR/sshd.pid")" 2>/dev/null || true
  [[ -f "$DIR/agent.pid" ]] && kill "$(cat "$DIR/agent.pid")" 2>/dev/null || true
  rm -rf "$DIR"
  echo "stopped" >&2
  exit 0
fi

rm -rf "$DIR"
mkdir -p "$DIR/etc" "$DIR/data" "$DIR/fakehome/.ssh"
chmod 700 "$DIR" "$DIR/fakehome/.ssh"

ssh-keygen -t ed25519 -f "$DIR/etc/ssh_host_ed25519_key" -N '' -q
ssh-keygen -t ed25519 -f "$DIR/etc/client_key" -N '' -q
cp "$DIR/etc/client_key.pub" "$DIR/etc/authorized_keys"
chmod 600 "$DIR/etc/authorized_keys" "$DIR/etc/ssh_host_ed25519_key"

cat > "$DIR/etc/sshd_config" <<EOF
Port $PORT
ListenAddress 127.0.0.1
HostKey $DIR/etc/ssh_host_ed25519_key
AuthorizedKeysFile $DIR/etc/authorized_keys
PidFile $DIR/sshd.pid
UsePAM no
PasswordAuthentication no
PubkeyAuthentication yes
StrictModes no
Subsystem sftp $SFTP_SERVER
EOF

# Fixtures the tests expect.
echo "hello from remote" > "$DIR/data/greeting.txt"
dd if=/dev/urandom of="$DIR/data/blob.bin" bs=1M count=6 status=none

# A symlinked directory and file: SFTP READDIR reports the link's own type, so
# these cover that the backend resolves targets and stays traversable.
mkdir -p "$DIR/data/real_subdir"
echo "inside the real dir" > "$DIR/data/real_subdir/nested.txt"
ln -sfn real_subdir "$DIR/data/link_subdir"
ln -sf greeting.txt "$DIR/data/link_greeting.txt"

/usr/sbin/sshd -f "$DIR/etc/sshd_config"

eval "$(ssh-agent -s -a "$DIR/agent.sock")" >/dev/null
echo "$SSH_AGENT_PID" > "$DIR/agent.pid"
SSH_AUTH_SOCK="$DIR/agent.sock" ssh-add "$DIR/etc/client_key" 2>/dev/null

printf '127.0.0.1\n%s\n%s/etc/client_key\n%s/data\n' "$PORT" "$DIR" "$DIR" > "$DIR/testcfg"

# Emit the environment for `eval`.
echo "export SSH_AUTH_SOCK=$DIR/agent.sock"
echo "export MYD_SFTP_TEST=$DIR/testcfg"
echo "export MYD_SFTP_TEST_HOME=$DIR/fakehome"
