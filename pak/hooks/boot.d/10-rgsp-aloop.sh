#!/bin/sh
# Load the ALSA loopback that casting captures from. The stock kernel is built
# with CONFIG_SND_ALOOP unset, so this module supplies it; it matches the stock
# kernel's vermagic and symbol CRCs and loads without --force.
# Also performs crash recovery if .asoundrc was left pointing to loopback.
PAK_DIR="${RGSP_PAK_DIR:-/mnt/SDCARD/Tools/h700/Cast.pak}"
LOG="${RGSP_LOG_PATH:-/mnt/SDCARD/.userdata/h700/logs/rgsp-hooks.log}"
RUN_DIR="${RGSP_RUN_DIR:-/tmp/rgsp}"
ASOUNDRC="${USERDATA_PATH:-/mnt/SDCARD/.userdata/h700}/.asoundrc"

# Ensure log directory exists
mkdir -p "$(dirname "$LOG")" 2>/dev/null || true

# Crash recovery: if .asoundrc contains our loopback config but the daemon
# is not running, it's a stale file from a crash. Remove it to restore audio.
if grep -q '^# rgsp-cast:' "$ASOUNDRC" 2>/dev/null; then
    if ! { [ -f "$RUN_DIR/daemon.pid" ] && kill -0 "$(cat "$RUN_DIR/daemon.pid" 2>/dev/null)" 2>/dev/null; }; then
        rm -f "$ASOUNDRC"
        echo "$(date): stale .asoundrc removed (crash recovery)" >> "$LOG" 2>/dev/null || true
    fi
fi

# Load the module if not already loaded
[ -f "$PAK_DIR/snd-aloop.ko" ] || exit 0
lsmod 2>/dev/null | grep -q '^snd_aloop' && exit 0

if insmod "$PAK_DIR/snd-aloop.ko" 2>>"$LOG"; then
    echo "$(date): snd-aloop loaded" >> "$LOG" 2>/dev/null || true
else
    echo "$(date): snd-aloop failed to load" >> "$LOG" 2>/dev/null || true
fi

exit 0
