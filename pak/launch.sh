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
    # Use usleep (100ms) with fallback to sleep 0.25 for BusyBox compatibility.
    i=0
    while [ -f "$PID_FILE" ] && [ $i -lt 150 ]; do i=$((i+1)); usleep 100000 2>/dev/null || sleep 0.25; done
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
    # Use usleep (100ms) with fallback to sleep 0.25 for BusyBox compatibility.
    while [ ! -f "$PID_FILE" ] && [ $i -lt 50 ]; do i=$((i+1)); usleep 100000 2>/dev/null || sleep 0.25; done
    show_status "Casting started"
fi
