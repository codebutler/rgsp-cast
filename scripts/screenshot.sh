#!/bin/sh
# Grab a screenshot off the device's framebuffer.
#
# /dev/fb0 is raw pixels with no header, so the geometry has to come from the
# device too. Everything needed is in sysfs — busybox's fbset is too stripped
# down to report it (no -i). The framebuffer is taller than the panel (double
# buffering) and its lines may be padded, so we pull just the first frame and
# crop it back to the mode's visible size.
#
#   sh screenshot.sh root@DEVICE [out.png]
#
# Channel order is the one thing sysfs does not expose; BGRA is what the H700
# display driver uses. Override with PIXFMT= if a port ever differs.
set -eu

DEVICE=${1:?usage: $0 root@DEVICE [out.png]}
OUT=${2:-screenshot.png}

command -v ffmpeg >/dev/null || { echo "ffmpeg not found" >&2; exit 1; }

# One round trip for everything we need to interpret the bytes.
FB=/sys/class/graphics/fb0
INFO=$(ssh "$DEVICE" "cd $FB && echo M \$(cat modes) && echo V \$(cat virtual_size) \
                      && echo S \$(cat stride) && echo B \$(cat bits_per_pixel)")

BPP=$(echo "$INFO" | awk '/^B/{print $2}')
STRIDE=$(echo "$INFO" | awk '/^S/{print $2}')
[ -n "$BPP" ] && [ -n "$STRIDE" ] || { echo "cannot read fb geometry from $FB" >&2; exit 1; }

# modes reads like "U:720x480p-59" — the panel's real size, as opposed to
# virtual_size, which counts every buffer behind it.
XRES=$(echo "$INFO" | awk '/^M/{print $2}' | sed -n 's/.*:\([0-9]*\)x\([0-9]*\).*/\1/p')
YRES=$(echo "$INFO" | awk '/^M/{print $2}' | sed -n 's/.*:\([0-9]*\)x\([0-9]*\).*/\2/p')
if [ -z "$XRES" ]; then
    XRES=$(echo "$INFO" | awk -F'[ ,]' '/^V/{print $2}')
    YRES=$(echo "$INFO" | awk -F'[ ,]' '/^V/{print $3}')
    echo "note: no mode reported, falling back to virtual_size ${XRES}x${YRES}" >&2
fi

XVIRT=$(( STRIDE * 8 / BPP ))
case "$BPP" in
    32) PIXFMT=${PIXFMT:-bgra} ;;
    24) PIXFMT=${PIXFMT:-bgr24} ;;
    16) PIXFMT=${PIXFMT:-rgb565le} ;;
    *)  echo "unsupported depth: ${BPP}bpp" >&2; exit 1 ;;
esac

BYTES=$(( STRIDE * YRES ))
echo "fb ${XRES}x${YRES} (stride ${XVIRT}px) ${BPP}bpp $PIXFMT, reading ${BYTES}B" >&2

# gzip on the wire: the framebuffer is mostly flat colour and the link is slow.
ssh "$DEVICE" "dd if=/dev/fb0 bs=$BYTES count=1 2>/dev/null | gzip -1" | gzip -dc | \
    ffmpeg -hide_banner -loglevel error -y \
        -f rawvideo -pix_fmt "$PIXFMT" -s "${XVIRT}x${YRES}" -i - \
        -vf "crop=${XRES}:${YRES}:0:0" -frames:v 1 "$OUT"

echo "wrote $OUT"
