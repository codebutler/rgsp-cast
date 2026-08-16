#!/bin/sh
# Restart casting if it was running before the device slept. WiFi comes back
# asynchronously, so wait briefly for an address before starting.
RUN_DIR="${RGSP_RUN_DIR:-/tmp/rgsp}"
PAK_DIR="${RGSP_PAK_DIR:-/mnt/SDCARD/Tools/h700/Cast.pak}"

[ -f "$RUN_DIR/was-casting" ] || exit 0
rm -f "$RUN_DIR/was-casting"
[ -x "$PAK_DIR/rgsp-host" ] || exit 0

i=0
while [ $i -lt 20 ]; do
    ip addr show wlan0 2>/dev/null | grep -q 'inet ' && break
    i=$((i+1))
    usleep 500000 2>/dev/null || sleep 1
done

export LD_LIBRARY_PATH="$PAK_DIR/lib/h700:$LD_LIBRARY_PATH"
( "$PAK_DIR/rgsp-host" >>"$RUN_DIR/daemon.log" 2>&1 & )
exit 0
