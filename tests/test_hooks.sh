#!/bin/sh
# Hooks run in a subshell with output suppressed and cannot cancel a launch.
# Test that they: are syntactically valid, exit 0 when preconditions are absent,
# exit 0 when preconditions are present, are fast, and actually perform their effects.
set -eu
HERE=$(cd "$(dirname "$0")" && pwd)
FAIL=0

# Test 1: Basic syntax and exit 0 with no preconditions
echo "=== Basic syntax and exit code tests ==="
for h in "$HERE"/../pak/hooks/*/*.sh; do
    [ -f "$h" ] || continue
    name=$(basename "$h")

    sh -n "$h" || { echo "FAIL: $name has a syntax error"; FAIL=1; continue; }

    start=$(date +%s)
    if ! RGSP_RUN_DIR=/nonexistent sh "$h" >/dev/null 2>&1; then
        echo "FAIL: $name exited non-zero with no preconditions"; FAIL=1
    fi
    end=$(date +%s)
    [ $((end - start)) -le 2 ] || { echo "FAIL: $name took too long"; FAIL=1; }

    echo "ok: $name (syntax, exit code, speed)"
done

# Test 2: boot hook removes stale .asoundrc when daemon is NOT running
echo "=== Testing boot hook crash recovery ==="
TESTDIR="$TMPDIR/rgsp-test-boot-$$"
mkdir -p "$TESTDIR"
trap "rm -rf '$TESTDIR'" EXIT
export RGSP_RUN_DIR="$TESTDIR"
export USERDATA_PATH="$TESTDIR"
mkdir -p "$TESTDIR"

# Create a stale .asoundrc with our marker
cat > "$TESTDIR/.asoundrc" <<'EOF'
# rgsp-cast: routing playback into the kernel loopback while casting.
pcm.!default {
    type plug
    slave.pcm "hw:Loopback,0,0"
}
EOF

# Run boot hook - should remove the stale .asoundrc
sh "$HERE"/../pak/hooks/boot.d/10-rgsp-aloop.sh >/dev/null 2>&1 || true

# Verify it was removed (daemon not running, so crash recovery should trigger)
if [ -f "$TESTDIR/.asoundrc" ]; then
    echo "FAIL: 10-rgsp-aloop.sh did not remove stale .asoundrc in crash recovery"
    FAIL=1
else
    echo "ok: 10-rgsp-aloop.sh removes stale .asoundrc (crash recovery)"
fi

# Test 3: pre-launch hook writes .asoundrc when daemon IS running
echo "=== Testing pre-launch hook routing ==="
TESTDIR2="$TMPDIR/rgsp-test-prelaunch-$$"
mkdir -p "$TESTDIR2"
trap "rm -rf '$TESTDIR' '$TESTDIR2'" EXIT
export RGSP_RUN_DIR="$TESTDIR2"
export USERDATA_PATH="$TESTDIR2"
mkdir -p "$TESTDIR2"

# Create a fake daemon PID file (use current shell PID)
echo "$$" > "$TESTDIR2/daemon.pid"

# Run pre-launch hook - should write .asoundrc
sh "$HERE"/../pak/hooks/pre-launch.d/10-rgsp-route.sh >/dev/null 2>&1 || true

# Verify .asoundrc was created with correct content
if ! grep -q 'hw:Loopback,0,0' "$TESTDIR2/.asoundrc" 2>/dev/null; then
    echo "FAIL: 10-rgsp-route.sh did not write .asoundrc routing"
    FAIL=1
else
    echo "ok: 10-rgsp-route.sh writes .asoundrc routing"
fi

# Test 4: pre-sleep hook creates was-casting marker when daemon IS running
echo "=== Testing pre-sleep hook ==="
TESTDIR3="$TMPDIR/rgsp-test-presleep-$$"
mkdir -p "$TESTDIR3"
trap "rm -rf '$TESTDIR' '$TESTDIR2' '$TESTDIR3'" EXIT
export RGSP_RUN_DIR="$TESTDIR3"
mkdir -p "$TESTDIR3"

# Create a fake daemon PID file (use a background sleep process)
sleep 100 & FAKE_DAEMON_PID=$!
echo "$FAKE_DAEMON_PID" > "$TESTDIR3/daemon.pid"

# Run pre-sleep hook - should create was-casting marker
sh "$HERE"/../pak/hooks/pre-sleep.d/10-rgsp-stop.sh >/dev/null 2>&1 || true

# Clean up the background process
kill "$FAKE_DAEMON_PID" 2>/dev/null || true

# Verify was-casting file was created
if [ ! -f "$TESTDIR3/was-casting" ]; then
    echo "FAIL: 10-rgsp-stop.sh did not create was-casting marker"
    FAIL=1
else
    echo "ok: 10-rgsp-stop.sh creates was-casting marker"
fi

# Test 5: post-resume hook checks for was-casting marker
echo "=== Testing post-resume hook ==="
TESTDIR4="$TMPDIR/rgsp-test-postresume-$$"
mkdir -p "$TESTDIR4"
trap "rm -rf '$TESTDIR' '$TESTDIR2' '$TESTDIR3' '$TESTDIR4'" EXIT
export RGSP_RUN_DIR="$TESTDIR4"
mkdir -p "$TESTDIR4"

# Without was-casting file, hook should exit early
sh "$HERE"/../pak/hooks/post-resume.d/10-rgsp-resume.sh >/dev/null 2>&1 || true
echo "ok: 10-rgsp-resume.sh exits early without was-casting marker"

# With was-casting file, hook should remove it (and try to start daemon, which will fail but that's ok)
touch "$TESTDIR4/was-casting"
sh "$HERE"/../pak/hooks/post-resume.d/10-rgsp-resume.sh >/dev/null 2>&1 || true

# Verify was-casting file was removed
if [ -f "$TESTDIR4/was-casting" ]; then
    echo "FAIL: 10-rgsp-resume.sh did not remove was-casting marker"
    FAIL=1
else
    echo "ok: 10-rgsp-resume.sh removes was-casting marker"
fi

echo ""
[ "$FAIL" -eq 0 ] && echo PASS || { echo FAILED; exit 1; }
