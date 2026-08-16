#!/bin/sh
# Cast.pak - toggle casting from the RG SP to a Moonlight client.
#
# Launching this pak starts the daemon and returns to the menu; the stream
# outlives the pak so you can go launch a game. Launching it again stops.
PAK_DIR="$(dirname "$0")"
PAK_NAME="$(basename "$PAK_DIR" .pak)"

_base="${SHARED_USERDATA_PATH:-/mnt/SDCARD/.userdata/${PLATFORM:-h700}}"
export HOME="$_base/$PAK_NAME"
mkdir -p "$HOME"

RUN_DIR="${RGSP_RUN_DIR:-/tmp/rgsp}"
PID_FILE="$RUN_DIR/daemon.pid"
LOG="$RUN_DIR/daemon.log"
mkdir -p "$RUN_DIR"

# The vendor CedarC libraries live in the pak, fetched at install time.
export LD_LIBRARY_PATH="$PAK_DIR/lib/${PLATFORM:-h700}:$LD_LIBRARY_PATH"

# Detect usleep availability to set appropriate iteration counts and sleep command.
# usleep path: 100ms per iteration → 30 iters = 3s, 50 iters = 5s, 150 iters = 15s
# fallback (sleep 1, integer): 1s per iteration → 3 iters = 3s, 5 iters = 5s, 15 iters = 15s
# (Fallback must be integer: on BusyBox without FEATURE_FANCY_SLEEP, 0.25 parses as 0)
if usleep 1 2>/dev/null; then
    SLEEP_CMD="usleep 100000"
    STOP_WAIT_ITERS=150  # ~15 seconds
    START_WAIT_ITERS=50  # ~5 seconds
else
    SLEEP_CMD="sleep 1"  # Integer fallback
    STOP_WAIT_ITERS=15   # ~15 seconds
    START_WAIT_ITERS=5   # ~5 seconds
fi

show_status() {
    # Show status with optional text message (progress mode if text provided, simple otherwise).
    # Usage: show_status [text_message]
    if [ $# -gt 0 ]; then
        # Progress mode with text (allows user to distinguish states)
        show2.elf --mode=progress --image="$PAK_DIR/cast.png" --bgcolor=0x000000 \
                  --fontcolor=0xFFFFFF --text="$1" &
    else
        # Simple mode without text
        show2.elf --mode=simple --image="$PAK_DIR/cast.png" --bgcolor=0x000000 &
    fi
    SHOW_PID=$!
    sleep 2
    kill "$SHOW_PID" 2>/dev/null || true
}

if [ -f "$PID_FILE" ] && kill -0 "$(cat "$PID_FILE")" 2>/dev/null; then
    # Already casting: stop.
    PID="$(cat "$PID_FILE")"
    kill -TERM "$PID" 2>/dev/null || true
    # The daemon removes its own pidfile on clean exit; give it up to ~15 seconds.
    i=0
    while [ -f "$PID_FILE" ] && [ $i -lt "$STOP_WAIT_ITERS" ]; do i=$((i+1)); $SLEEP_CMD; done
    # If file is gone, daemon exited cleanly. If still there, leave it alone:
    # the daemon is still shutting down normally. Deleting the file would cause
    # the next launch to start a second instance, which loses the flock.
    if [ ! -f "$PID_FILE" ]; then
        # Daemon exited cleanly.
        show_status "Casting stopped"
    else
        # Daemon still running after timeout (normal during shutdown). Inform user.
        show_status "Still stopping — try again in a moment"
    fi
else
    # Not casting: start, detached in subshell so it survives this script exiting.
    # Subshell form matches Cast-Pak precedent and ensures daemon survives pak termination.
    ( "$PAK_DIR/rgsp-host" >"$LOG" 2>&1 & )
    i=0
    # Wait up to ~5 seconds for daemon to write PID file.
    while [ ! -f "$PID_FILE" ] && [ $i -lt "$START_WAIT_ITERS" ]; do i=$((i+1)); $SLEEP_CMD; done
    show_status "Casting started"
fi
