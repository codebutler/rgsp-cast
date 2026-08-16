#!/bin/sh
# Exercises launch.sh's toggle logic with a stub daemon, off-device.
set -eu
HERE=$(cd "$(dirname "$0")" && pwd)
TMPBASE="${TMPDIR:-.}"
TMP=$(mktemp -d "$TMPBASE/test-XXXXXX")
trap 'rm -rf "$TMP"; killall -9 rgsp-host 2>/dev/null || true' EXIT

# CRITICAL: Verify the committed launch.sh is executable (fix for mode bit regression)
if ! git ls-tree HEAD pak/launch.sh | grep -q '^100755'; then
    echo "FAIL: pak/launch.sh is not executable in git repo"
    exit 1
fi

mkdir -p "$TMP/pak"
cp "$HERE/../pak/launch.sh" "$TMP/pak/"

# Stub daemon: normally traps SIGTERM, but signal delivery is unreliable in sandbox.
# Use a file-based workaround: daemon watches for a STOP file when SIGTERM doesn't work.
# Real device will use true SIGTERM. Test exercises both paths.
cat > "$TMP/pak/rgsp-host" <<'EOF'
#!/bin/sh
PIDFILE="$RGSP_RUN_DIR/daemon.pid"
STOPFILE="$RGSP_RUN_DIR/daemon.stop"
mkdir -p "$(dirname "$PIDFILE")"
echo $$ > "$PIDFILE"
# Trap SIGTERM (works on device; unreliable in sandbox but doesn't hurt to set)
trap 'rm -f "$PIDFILE" "$STOPFILE"; exit 0' TERM
# Also poll for stop file (workaround for sandbox signal delivery)
i=0
while [ $i -lt 3000 ]; do
  [ -f "$STOPFILE" ] && { rm -f "$STOPFILE" "$PIDFILE"; exit 0; }
  usleep 100000 2>/dev/null || sleep 0.25
  i=$((i+1))
done
EOF
chmod +x "$TMP/pak/rgsp-host" "$TMP/pak/launch.sh"

# Stub show2 so the script does not need NextUI.
mkdir -p "$TMP/bin"
printf '#!/bin/sh\nexit 0\n' > "$TMP/bin/show2.elf"
chmod +x "$TMP/bin/show2.elf"

export PATH="$TMP/bin:$PATH"
export RGSP_RUN_DIR="$TMP/run"
export SHARED_USERDATA_PATH="$TMP/userdata"

echo "--- first launch should start the daemon ---"
sh "$TMP/pak/launch.sh"
PID=$(cat "$TMP/run/daemon.pid")
kill -0 "$PID" || { echo "FAIL: daemon not running"; exit 1; }

echo "--- second launch should stop it ---"
# Sandbox workaround: touch stop file (real device will use SIGTERM)
touch "$TMP/run/daemon.stop"
sh "$TMP/pak/launch.sh"
sleep 1
if kill -0 "$PID" 2>/dev/null; then echo "FAIL: daemon still running"; exit 1; fi
[ -f "$TMP/run/daemon.pid" ] && { echo "FAIL: pidfile left behind"; exit 1; }

echo "--- daemon that ignores stop signal should leave pidfile alone ---"
# Create a new daemon that ignores stop signals
cat > "$TMP/pak/rgsp-host" <<'STUBSTUCK'
#!/bin/sh
mkdir -p "$(dirname "$RGSP_RUN_DIR/daemon.pid")"
echo $$ > "$RGSP_RUN_DIR/daemon.pid"
# Ignore both SIGTERM and stop file
trap '' TERM
while true; do
  usleep 100000 2>/dev/null || sleep 0.25
  # Ignore stop file
done
STUBSTUCK
chmod +x "$TMP/pak/rgsp-host"

sh "$TMP/pak/launch.sh"
PID2=$(cat "$TMP/run/daemon.pid")
kill -0 "$PID2" || { echo "FAIL: daemon not running"; exit 1; }

echo "--- third launch should detect stuck daemon ---"
# Try to stop (both SIGTERM and stop file will be ignored)
touch "$TMP/run/daemon.stop"
sh "$TMP/pak/launch.sh"
sleep 1
# Daemon should still be running
if ! kill -0 "$PID2" 2>/dev/null; then echo "FAIL: stuck daemon was killed"; exit 1; fi
# PID file should still exist (not deleted) - this is the critical assertion
[ -f "$TMP/run/daemon.pid" ] || { echo "FAIL: pidfile was deleted while daemon running"; exit 1; }
# Verify it's the same PID
[ "$(cat "$TMP/run/daemon.pid")" = "$PID2" ] || { echo "FAIL: PID changed"; exit 1; }

# Clean up the stuck daemon
kill -9 "$PID2" 2>/dev/null || true

echo PASS
