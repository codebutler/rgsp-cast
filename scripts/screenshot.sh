#!/bin/sh
# Grab a screenshot off the device's framebuffer.
#
# /dev/fb0 is raw pixels with no header, so the geometry has to come from the
# device too. Everything needed is in sysfs — busybox's fbset is too stripped
# down to report it (no -i). The framebuffer is taller than the panel (double
# buffering) and its lines may be padded, so we pull just the visible page and
# crop it back to the mode's visible size.
#
# Double buffering means offset 0 is not always the page on screen — which
# half is visible is given by the fb's pan offset (/sys/class/graphics/fb0/pan,
# "xoffset,yoffset"), which we read fresh on every run. Reading offset 0
# unconditionally, as an earlier version of this script did, silently
# captures the *other* page: fine when whatever is on screen redraws both
# buffers, but a blank or stale image for anything that draws once and sits
# static. See rgsp-cedar/src/framebuffer.rs for the same fix done properly
# via FBIOGET_VSCREENINFO, which is the ioctl this shells out to sysfs for.
#
#   sh screenshot.sh root@DEVICE [out.png]
#
# Channel order is the one thing sysfs does not expose; BGRA is what the H700
# display driver uses. Override with PIXFMT= if a port ever differs.
set -eu

DEVICE=${1:?usage: $0 root@DEVICE [out.png]}
OUT=${2:-screenshot.png}

command -v ffmpeg >/dev/null || { echo "ffmpeg not found" >&2; exit 1; }

# One round trip for everything we need to interpret the bytes, including
# `pan` — the fb's current xoffset,yoffset. This is still a separate ssh
# connection from the dd below, so a page flip between the two is possible
# in principle; for the static screens this tool targets that race is not a
# practical concern, but it is not atomic.
FB=/sys/class/graphics/fb0
INFO=$(ssh "$DEVICE" "cd $FB && echo M \$(cat modes) && echo V \$(cat virtual_size) \
                      && echo S \$(cat stride) && echo B \$(cat bits_per_pixel) \
                      && echo P \$(cat pan 2>/dev/null)")

BPP=$(echo "$INFO" | awk '/^B/{print $2}')
STRIDE=$(echo "$INFO" | awk '/^S/{print $2}')
[ -n "$BPP" ] && [ -n "$STRIDE" ] || { echo "cannot read fb geometry from $FB" >&2; exit 1; }

# pan reads "xoffset,yoffset" in pixels — yoffset selects which page of the
# double buffer is on screen. No pan node means no way to tell the pages
# apart from sysfs alone; the honest move is to fail rather than guess 0.
PAN=$(echo "$INFO" | awk '/^P/{print $2}')
YOFFSET=$(echo "$PAN" | cut -d, -f2)
case "$YOFFSET" in
    ''|*[!0-9]*)
        echo "no usable pan offset from $FB/pan (got '$PAN'); cannot tell which" \
             "framebuffer page is visible" >&2
        exit 1
        ;;
esac

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

echo "fb ${XRES}x${YRES} (stride ${XVIRT}px) ${BPP}bpp $PIXFMT, yoffset=$YOFFSET" >&2

# dd's skip/count are in units of bs, and bs is one scanline (STRIDE bytes),
# so skip=$YOFFSET lands exactly on the visible page's first line regardless
# of how tall the virtual framebuffer is. gzip on the wire: the framebuffer
# is mostly flat colour and the link is slow.
RAW=$(mktemp "${TMPDIR:-/tmp}/screenshot.XXXXXX")
trap 'rm -f "$RAW"' EXIT
ssh "$DEVICE" "dd if=/dev/fb0 bs=$STRIDE skip=$YOFFSET count=$YRES 2>/dev/null | gzip -1" | \
    gzip -dc >"$RAW"

ffmpeg -hide_banner -loglevel error -y \
    -f rawvideo -pix_fmt "$PIXFMT" -s "${XVIRT}x${YRES}" -i "$RAW" \
    -vf "crop=${XRES}:${YRES}:0:0" -frames:v 1 "$OUT"

# A silent uniform-colour capture is exactly what let the offset-0 version of
# this bug go unnoticed for so long: it decodes to a valid, unremarkable-
# looking PNG, just not one of the actual screen. Flag it instead of just
# writing it out quietly.
STATS=$(ffmpeg -hide_banner -loglevel error -i "$OUT" \
    -vf "signalstats,metadata=print:file=-" -f null - 2>&1)
YMIN=$(echo "$STATS" | sed -n 's/^lavfi.signalstats.YMIN=//p')
YMAX=$(echo "$STATS" | sed -n 's/^lavfi.signalstats.YMAX=//p')
UMIN=$(echo "$STATS" | sed -n 's/^lavfi.signalstats.UMIN=//p')
UMAX=$(echo "$STATS" | sed -n 's/^lavfi.signalstats.UMAX=//p')
VMIN=$(echo "$STATS" | sed -n 's/^lavfi.signalstats.VMIN=//p')
VMAX=$(echo "$STATS" | sed -n 's/^lavfi.signalstats.VMAX=//p')
if [ "$YMIN" = "$YMAX" ] && [ "$UMIN" = "$UMAX" ] && [ "$VMIN" = "$VMAX" ]; then
    echo "WARNING: $OUT is a single flat colour (Y=$YMIN U=$UMIN V=$VMIN) —" \
         "this usually means the wrong framebuffer page was captured" >&2
fi

echo "wrote $OUT"
