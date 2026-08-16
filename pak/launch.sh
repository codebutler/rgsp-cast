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

show() {
    show2.elf --mode=simple --image="$PAK_DIR/cast.png" --bgcolor=0x000000 &
    SHOW_PID=$!
    sleep 2
    kill "$SHOW_PID" 2>/dev/null || true
}

if [ -f "$PID_FILE" ] && kill -0 "$(cat "$PID_FILE")" 2>/dev/null; then
    # Already casting: stop.
    kill -TERM "$(cat "$PID_FILE")" 2>/dev/null || true
    # The daemon removes its own pidfile on clean exit; give it a moment.
    i=0
    while [ -f "$PID_FILE" ] && [ $i -lt 20 ]; do i=$((i+1)); sleep 0.1; done
    rm -f "$PID_FILE"
    show
else
    # Not casting: start, detached, so it survives this script exiting.
    ( "$PAK_DIR/rgsp-host" >"$LOG" 2>&1 & )
    i=0
    while [ ! -f "$PID_FILE" ] && [ $i -lt 50 ]; do i=$((i+1)); sleep 0.1; done
    show
fi
