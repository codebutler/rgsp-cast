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

# Wait for daemon to clean up (remove PID file), with 3-second timeout
# The stream must be down before device sleeps; a hung hook is worse than slow shutdown
i=0
while [ $i -lt 30 ]; do
    [ -f "$PID_FILE" ] || exit 0  # Daemon cleaned up, we're done
    i=$((i+1))
    usleep 100000 2>/dev/null || sleep 0.1
done

# Timed out, but don't block the system — log and return anyway
echo "$(date): daemon did not stop within 3 seconds" >> "$RUN_DIR/pre-sleep.log" 2>/dev/null || true
exit 0
