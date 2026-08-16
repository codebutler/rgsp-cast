#!/bin/sh
# Deep sleep fully stops the USB controllers and takes WiFi with them, so a live
# session would hang the client rather than reconnect. Stop cleanly and record
# that we were casting, so post-resume can bring it back.
RUN_DIR="${RGSP_RUN_DIR:-/tmp/rgsp}"
PID_FILE="$RUN_DIR/daemon.pid"

[ -f "$PID_FILE" ] || exit 0
PID=$(cat "$PID_FILE" 2>/dev/null) || exit 0
kill -0 "$PID" 2>/dev/null || exit 0

touch "$RUN_DIR/was-casting"
kill -TERM "$PID" 2>/dev/null || true
exit 0
