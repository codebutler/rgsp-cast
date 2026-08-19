#!/bin/sh
# Exercises launch.sh's toggle logic with a stub daemon, off-device.
set -eu
HERE=$(cd "$(dirname "$0")" && pwd)
TMPBASE="${TMPDIR:-.}"
TMP=$(mktemp -d "$TMPBASE/test-XXXXXX")
cleanup() {
    # SIGINT, not SIGTERM: that is the only signal show2 honours (see the stub).
    [ -f "${SHOW2_PIDS:-}" ] && while read -r p; do kill -INT "$p" 2>/dev/null || true; done < "$SHOW2_PIDS"
    rm -rf "$TMP"
    killall -9 rgsp-host 2>/dev/null || true
}
trap cleanup EXIT

# CRITICAL: Verify the committed launch.sh is executable (fix for mode bit regression)
# Skip gracefully if not in a git repository (e.g., from tarball)
if git rev-parse --git-dir >/dev/null 2>&1; then
    if ! git ls-tree HEAD pak/launch.sh | grep -q '^100755'; then
        echo "FAIL: pak/launch.sh is not executable in git repo"
        exit 1
    fi
else
    echo "INFO: not in git repo, skipping executable bit check"
fi

mkdir -p "$TMP/pak"
cp "$HERE/../pak/launch.sh" "$TMP/pak/"

# Detect if usleep is available to determine iteration counts
# usleep path: 100ms per iteration → 150 iters = 15s, 50 iters = 5s
# fallback (sleep 0.25): 250ms per iteration → 60 iters = 15s, 20 iters = 5s
if usleep 1 2>/dev/null; then
    STOP_ITERS=150
    START_ITERS=50
else
    STOP_ITERS=60
    START_ITERS=20
fi

# Stub daemon: traps SIGTERM and removes its own PID file on exit (emulates real daemon).
# This test MUST fail if the signal is not delivered, so no workarounds here.
# Use a loop that keeps sleeping so SIGTERM is properly handled (not masked by sleep duration).
cat > "$TMP/pak/rgsp-host" <<'EOF'
#!/bin/sh
PIDFILE="$RGSP_RUN_DIR/daemon.pid"
mkdir -p "$(dirname "$PIDFILE")"
echo $$ > "$PIDFILE"
trap 'rm -f "$PIDFILE"; exit 0' TERM
while :; do
  usleep 100000 2>/dev/null || sleep 0.1
done
EOF
chmod +x "$TMP/pak/rgsp-host" "$TMP/pak/launch.sh"

# Stub show2 so the script does not need NextUI. It models the real
# show2.elf's signal handling, which is what these assertions exist to catch:
# SDL's parachute swallows SIGTERM into an SDL_QUIT nobody polls, so show2
# keeps running in BOTH --mode=simple and --mode=progress and exits only on
# SIGINT (confirmed against show2.cpp and on-device). It holds /dev/fb0 and
# /dev/mali0 the whole time, so one that outlives the pak page-flips against
# nextui.elf and the launcher UI stays corrupted until the device reboots.
mkdir -p "$TMP/bin"
cat > "$TMP/bin/show2.elf" <<'SHOW2STUB'
#!/bin/sh
echo $$ >> "$SHOW2_PIDS"
trap '' TERM
trap 'exit 0' INT
while :; do usleep 100000 2>/dev/null || sleep 1; done
SHOW2STUB
chmod +x "$TMP/bin/show2.elf"

export PATH="$TMP/bin:$PATH"
export RGSP_RUN_DIR="$TMP/run"
export SHARED_USERDATA_PATH="$TMP/userdata"
export SHOW2_PIDS="$TMP/show2.pids"
: > "$SHOW2_PIDS"

# No show2 may outlive launch.sh. A survivor is the framebuffer-corruption bug.
assert_no_show2() {
    leaked=
    while read -r p; do
        kill -0 "$p" 2>/dev/null && leaked="$leaked $p"
    done < "$SHOW2_PIDS"
    if [ -n "$leaked" ]; then
        echo "FAIL: $1: show2 outlived launch.sh (pids:$leaked)"
        echo "      a live show2 holds /dev/fb0 and corrupts the launcher UI"
        exit 1
    fi
}

LAUNCH_N=0
run_launch() {
    LAUNCH_N=$((LAUNCH_N + 1))
    sh "$TMP/pak/launch.sh"
    assert_no_show2 "launch #$LAUNCH_N"
}

echo "--- first launch should start the daemon ---"
run_launch
PID=$(cat "$TMP/run/daemon.pid")
kill -0 "$PID" || { echo "FAIL: daemon not running"; exit 1; }

echo "--- second launch should stop it (SIGTERM must be delivered and handled) ---"
run_launch
sleep 1
if kill -0 "$PID" 2>/dev/null; then echo "FAIL: daemon still running"; exit 1; fi
[ -f "$TMP/run/daemon.pid" ] && { echo "FAIL: pidfile left behind"; exit 1; }

echo "--- daemon that ignores SIGTERM should leave pidfile alone ---"
# Create a new daemon that ignores SIGTERM and does not respond to it
cat > "$TMP/pak/rgsp-host" <<'STUBSTUCK'
#!/bin/sh
mkdir -p "$(dirname "$RGSP_RUN_DIR/daemon.pid")"
echo $$ > "$RGSP_RUN_DIR/daemon.pid"
# Ignore SIGTERM, just keep sleeping
trap '' TERM
sleep 300
STUBSTUCK
chmod +x "$TMP/pak/rgsp-host"

run_launch
PID2=$(cat "$TMP/run/daemon.pid")
kill -0 "$PID2" || { echo "FAIL: daemon not running"; exit 1; }

echo "--- third launch should detect stuck daemon ---"
run_launch
sleep 1
# Daemon should still be running
if ! kill -0 "$PID2" 2>/dev/null; then echo "FAIL: stuck daemon was killed"; exit 1; fi
# PID file should still exist (not deleted) - critical assertion: never delete while daemon alive
[ -f "$TMP/run/daemon.pid" ] || { echo "FAIL: pidfile was deleted while daemon running"; exit 1; }
# Verify it's the same PID
[ "$(cat "$TMP/run/daemon.pid")" = "$PID2" ] || { echo "FAIL: PID changed"; exit 1; }

# Clean up the stuck daemon
kill -9 "$PID2" 2>/dev/null || true

echo PASS
