#!/bin/sh
# ALSA config is read when a client opens the PCM, so this is the last moment
# to point a launching game at the cast sink. Only acts while casting.
RUN_DIR="${RGSP_RUN_DIR:-/tmp/rgsp}"
PID_FILE="$RUN_DIR/daemon.pid"
ASOUNDRC="${USERDATA_PATH:-/mnt/SDCARD/.userdata/h700}/.asoundrc"

[ -f "$PID_FILE" ] || exit 0
kill -0 "$(cat "$PID_FILE" 2>/dev/null)" 2>/dev/null || exit 0

grep -q 'hw:Loopback,0,0' "$ASOUNDRC" 2>/dev/null && exit 0

cat > "$ASOUNDRC" <<'EOF'
# rgsp-cast: routing playback into the kernel loopback while casting.
pcm.!default {
    type plug
    slave.pcm "hw:Loopback,0,0"
}
EOF
exit 0
