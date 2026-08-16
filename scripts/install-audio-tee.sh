#!/bin/sh
# Install (or remove) an ALSA capture tee on the device.
#
#   ./install-audio-tee.sh root@DEVICE          install
#   ./install-audio-tee.sh root@DEVICE --remove restore the stock config
#
# The audio path is loopback-based: see scripts/build-snd-aloop.sh, which builds
# a matching snd-aloop.ko for the stock kernel.
#
# This script covers devices where that module is not loaded. The stock kernel
# has no loopback and the codec is exclusive as configured — one client at a
# time, no dmix — so audio cannot be captured from outside the playing process.
# Instead we put an alsa-lib
# `type file` plugin in front of the default PCM, in pipe mode: ALSA spawns
# rgsp-audio-pump and feeds it the stream, which rgsp-cast then reads over a
# Unix socket. Nothing touches the filesystem.
#
# This works because NextUI/minarch uses SDL2 with SDL_AUDIODRIVER=alsa
# (MinUI.pak/launch.sh:40), which opens the PCM named "default" — the one
# asound.conf controls. minarch.elf links libasound directly, so alsa-lib
# config applies. (NextUI also ships libtinyalsa, which would bypass all of
# this, but the emulator does not use it for playback.)
#
# The tee attaches when a client OPENS the PCM, so after installing you must
# relaunch the game for it to take effect.
set -eu

DEVICE=${1:?usage: $0 root@DEVICE [--remove]}
MODE=${2:-install}

if [ "$MODE" = "--remove" ]; then
    ssh "$DEVICE" 'if [ -f /etc/asound.conf.orig ]; then
        cp /etc/asound.conf.orig /etc/asound.conf && echo "restored stock asound.conf"
    else
        echo "no backup at /etc/asound.conf.orig" >&2; exit 1
    fi'
    echo "Relaunch the game for it to take effect."
    exit 0
fi

HERE=$(cd "$(dirname "$0")" && pwd)
PUMP_DIR=/mnt/SDCARD/.userdata/h700/rgsp

[ -f "$HERE/../bin/rgsp-audio-pump" ] || { echo "build first: make all" >&2; exit 1; }

# The pump lives on the SD card, not /tmp: asound.conf references it by path and
# must still resolve after a reboot (/tmp is a tmpfs), and .userdata survives
# NextUI updates.
ssh "$DEVICE" "mkdir -p $PUMP_DIR"
scp -q "$HERE/../bin/rgsp-audio-pump" "$DEVICE:$PUMP_DIR/"
ssh "$DEVICE" "chmod 755 $PUMP_DIR/rgsp-audio-pump"
scp -q "$HERE/../etc/asound.conf.tee" "$DEVICE:/tmp/asound.conf.tee"
ssh "$DEVICE" '
    [ -f /etc/asound.conf.orig ] || cp /etc/asound.conf /etc/asound.conf.orig
    cp /tmp/asound.conf.tee /etc/asound.conf
    # aplay -L parses asound.conf without opening hardware, so it validates the
    # config even while a game holds the exclusive codec.
    if aplay -L >/dev/null 2>&1; then
        echo "installed; config parses"
    else
        echo "config failed to parse - restoring" >&2
        cp /etc/asound.conf.orig /etc/asound.conf
        exit 1
    fi
'
cat <<EOF

Installed. Backup at /etc/asound.conf.orig

Now relaunch the game (the tee attaches on PCM open), then:
    make run DEVICE=$DEVICE DURATION=30

To remove:  $0 $DEVICE --remove
EOF
