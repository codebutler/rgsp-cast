#!/bin/sh
# Extract the Allwinner CedarC encoder libraries from TrimUI Smart Pro firmware.
#
# The RG SP's own Anbernic firmware ships no CedarC runtime at all — not even
# the decode stack — so the libraries have to come from another device in the
# same sun50iw9 family. TrimUI Smart Pro (H618) firmware carries a current
# build, and it is glibc so it loads on BaseOS unmodified.
#
# These are proprietary vendor binaries. They are NOT redistributed with this
# project; run this to produce them locally.
#
#   ./extract-vendor-libs.sh                    # download the pinned firmware
#   ./extract-vendor-libs.sh /path/to.awimg     # use a local .awimg or .zip
#   ./extract-vendor-libs.sh '' /tmp/libs       # choose the output directory
#
# Requires Docker (the rootfs is ext4, which macOS cannot mount natively).
set -eu

# ── pinned firmware ────────────────────────────────────────────────────────
# TrimUI Smart Pro v1.1.1, published 2025-12-01. Pinned because the version
# matters: CedarC builds older than the H616/H618 refuse this silicon with
# "the driver do not support the ic 12011".
#
# Note the sibling asset in the same release — sd_recovery_*.zip, 2.0 GB — is a
# PhoenixCard burn image whose payload is not mountable. It is the wrong file.
FW_URL='https://github.com/trimui/firmware_smartpro/releases/download/v1.1.1/trimui_tg5040_20251128_v1.1.1.zip'
FW_ZIP_SHA256='43950ed504b83cca2f553168bf8dfcc17e6d689c02dc621f0f0f26954d182d18'
FW_IMG_SHA256='b2c8d1eb3d42aca027babbf9aa038fdcfc60ef4430b529b6e26ccf1280541ebd'
FW_IMG_NAME='trimui_tg5040.awimg'

# rootfs.fex offset from this build's IMAGEWTY file table. NOT 4096-aligned, so
# `dd bs=4096` lands in the wrong place — mount by byte offset. A future
# firmware will move this; override ROOTFS_OFFSET after re-deriving it.
ROOTFS_OFFSET=${ROOTFS_OFFSET:-18095104}   # 0x1141c00

SRC=${1:-}
OUT=${2:-vendor-libs}
CACHE=${CACHE:-.firmware-cache}
IMAGE=${IMAGE:-ubuntu:22.04}

sha256_of() {
    if command -v sha256sum >/dev/null 2>&1; then sha256sum "$1" | cut -d' ' -f1
    else shasum -a 256 "$1" | cut -d' ' -f1; fi
}

# ── obtain the .awimg ──────────────────────────────────────────────────────
if [ -z "$SRC" ]; then
    mkdir -p "$CACHE"
    ZIP="$CACHE/$(basename "$FW_URL")"
    if [ ! -f "$ZIP" ]; then
        echo "downloading $(basename "$FW_URL") (240 MB)..."
        curl -fL --progress-bar -o "$ZIP.part" "$FW_URL"
        mv "$ZIP.part" "$ZIP"
    fi
    got=$(sha256_of "$ZIP")
    if [ "$got" != "$FW_ZIP_SHA256" ]; then
        echo "checksum mismatch for $ZIP" >&2
        echo "  expected $FW_ZIP_SHA256" >&2
        echo "  got      $got" >&2
        echo "The pinned release may have been re-uploaded. Verify before trusting it." >&2
        exit 1
    fi
    SRC="$CACHE/$FW_IMG_NAME"
    [ -f "$SRC" ] || unzip -o -q "$ZIP" "$FW_IMG_NAME" -d "$CACHE"
elif [ "${SRC%.zip}" != "$SRC" ]; then
    d=$(dirname "$SRC")
    unzip -o -q "$SRC" "$FW_IMG_NAME" -d "$d"
    SRC="$d/$FW_IMG_NAME"
fi

[ -f "$SRC" ] || { echo "no such file: $SRC" >&2; exit 1; }

got=$(sha256_of "$SRC")
if [ "$got" != "$FW_IMG_SHA256" ]; then
    echo "warning: $SRC is not the pinned v1.1.1 image" >&2
    echo "  expected $FW_IMG_SHA256" >&2
    echo "  got      $got" >&2
    echo "  continuing; if the mount fails, re-derive ROOTFS_OFFSET" >&2
fi

# ── extract ────────────────────────────────────────────────────────────────
mkdir -p "$OUT"
OUT_ABS=$(cd "$OUT" && pwd)
SRC_ABS=$(cd "$(dirname "$SRC")" && pwd)/$(basename "$SRC")

docker run --rm --privileged --platform linux/arm64 \
    -v "$SRC_ABS":/fw.awimg:ro -v "$OUT_ABS":/out "$IMAGE" sh -euc '
    mkdir -p /mnt/r
    mount -o loop,offset='"$ROOTFS_OFFSET"',ro /fw.awimg /mnt/r
    for l in /mnt/r/usr/lib/libvenc*.so* /mnt/r/usr/lib/libVE.so* \
             /mnt/r/usr/lib/libMemAdapter.so* /mnt/r/usr/lib/libcdc_base.so* \
             /mnt/r/usr/lib/libvideoengine.so*; do
        [ -e "$l" ] && cp -L "$l" /out/
    done
    umount /mnt/r
    ls -l /out
'

cat <<EOF

Extracted to $OUT_ABS

Copy to the device and point LD_LIBRARY_PATH at it:
    scp -r $OUT root@<device>:/tmp/venc/lib-trimui
    make run DEVICE=root@<device> DURATION=30
EOF
