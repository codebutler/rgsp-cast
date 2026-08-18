#!/bin/sh
# Install Cast.pak on the device, including hooks and the vendor libraries.
#
# The CedarC blobs are fetched on the device rather than shipped: they are
# proprietary, and extract-vendor-libs.sh verifies checksums against TrimUI's
# own firmware release.
set -eu

DEVICE=${1:?usage: $0 root@DEVICE}
HERE=$(cd "$(dirname "$0")" && pwd)
PAKDIR="$HERE/../dist/Tools/h700/Cast.pak"
DEST=/mnt/SDCARD/Tools/h700/Cast.pak
HOOKS=/mnt/SDCARD/.userdata/h700/.hooks

[ -d "$PAKDIR" ] || { echo "build first: make pak" >&2; exit 1; }

ssh "$DEVICE" "mkdir -p $DEST $HOOKS/boot.d $HOOKS/pre-launch.d $HOOKS/pre-sleep.d $HOOKS/post-resume.d"
scp -q -r "$PAKDIR"/* "$DEVICE:$DEST/"

# Hooks live under .userdata, not in the pak, so NextUI finds them.
for phase in boot pre-launch pre-sleep post-resume; do
    for f in "$PAKDIR/hooks/$phase.d"/*.sh; do
        [ -f "$f" ] || continue
        scp -q "$f" "$DEVICE:$HOOKS/$phase.d/"
    done
done
ssh "$DEVICE" "chmod +x $DEST/launch.sh $DEST/rgsp-host $HOOKS/*/*.sh"

# Vendor libraries, fetched here and pushed, never committed.
if [ ! -d "$HERE/../vendor-libs" ]; then
    "$HERE/extract-vendor-libs.sh"
fi
ssh "$DEVICE" "mkdir -p $DEST/lib/h700"
scp -q "$HERE/../vendor-libs"/* "$DEVICE:$DEST/lib/h700/"

ssh "$DEVICE" "lsmod | grep -q '^snd_aloop' || insmod $DEST/snd-aloop.ko"

cat <<EOF

Installed to $DEST
Hooks installed to $HOOKS

Launch it from Tools -> Cast. It toggles: once to start, again to stop.
Pair from Moonlight, then open the URL shown on screen in a browser.
EOF
