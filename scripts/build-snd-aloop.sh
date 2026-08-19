#!/bin/sh
# Build snd-aloop.ko for the RG SP's stock kernel.
#
# The stock kernel is 4.9.170 with CONFIG_SND_ALOOP unset, so the ALSA loopback
# device does not exist on the device. This builds it as a loadable module that
# matches the stock kernel exactly - same vermagic, same symbol CRCs - so it
# loads with plain insmod, no --force, no ABI risk.
#
# Why the Allwinner BSP tree and not mainline 4.9.170: mainline builds fine and
# produces the right vermagic, but its module_layout CRC is 0xac56b7a1 against
# the stock kernel's 0x3491861c. Allwinner patched the core headers, so mainline
# would need a forced load - and module_layout covers struct module itself, so
# forcing across that difference risks memory corruption rather than a clean
# rejection. The sun50iw9 BSP tree matches exactly.
#
# Everything runs inside an arm64 container:
#   - native build, so no cross toolchain and no compiler mismatch
#   - the kernel tree has filenames differing only by case (ipt_ECN.h vs
#     ipt_ecn.h); cloning onto macOS silently loses them and the build dies in
#     net/ipv4/netfilter, so the clone must happen inside Linux too
#   - the tree lives in a docker volume, so retries are incremental
#
# Usage:  ./scripts/build-snd-aloop.sh        -> bin/snd-aloop.ko
set -eu

HERE=$(cd "$(dirname "$0")" && pwd)
OUT="$HERE/../bin"
CONFIG="$HERE/reference/stock-kernel-4.9.170.config"
VOLUME=rgsp-kbuild
IMAGE=ubuntu:18.04          # gcc 7.5; modern gcc will not build a 4.9 kernel

[ -f "$CONFIG" ] || { echo "missing $CONFIG" >&2; exit 1; }
mkdir -p "$OUT"

docker volume create "$VOLUME" >/dev/null
docker run --rm --platform linux/arm64 \
    -v "$OUT":/out -v "$CONFIG":/stock.config:ro -v "$VOLUME":/build \
    "$IMAGE" /bin/sh -eux -c '
    sed -i "s|archive.ubuntu.com|old-releases.ubuntu.com|g; \
            s|security.ubuntu.com|old-releases.ubuntu.com|g" /etc/apt/sources.list
    export DEBIAN_FRONTEND=noninteractive
    apt-get update -qq
    apt-get install -y -qq build-essential bc bison flex libssl-dev libelf-dev \
                           kmod git ca-certificates >/dev/null

    if [ ! -d /build/linux ]; then
        git clone -q --depth 1 -b orange-pi-4.9-sun50iw9 \
            https://github.com/orangepi-xunlong/linux-orangepi.git /build/linux
        cd /build/linux
        # assert the case-colliding files survived the clone
        ls net/ipv4/netfilter/ipt_ECN.c >/dev/null
        cp /stock.config .config
        scripts/config --module SND_ALOOP
        # OrangePi patches phy_device.c to call yt8511_config_out_125m()
        # unconditionally; the handheld config has no ethernet PHY driver.
        # Must be =y, not =m - the caller is built in.
        scripts/config --enable MOTORCOMM_PHY
        # setlocalversion appends "+" for an untagged git tree, which would make
        # vermagic "4.9.170+" and fail the (exact) match against the kernel.
        printf "" > .scmversion
        make olddefconfig >/dev/null
    fi

    cd /build/linux
    make -j"$(nproc)"
    cp sound/drivers/snd-aloop.ko /out/
    modinfo /out/snd-aloop.ko | grep -E "vermagic|srcversion"
'

echo
echo "built: $OUT/snd-aloop.ko"
echo
echo "Install on the device:"
echo "  scp $OUT/snd-aloop.ko root@DEVICE:/tmp/"
echo "  ssh root@DEVICE 'insmod /tmp/snd-aloop.ko && cat /proc/asound/cards'"
echo
echo "It must load WITHOUT --force. If insmod reports a vermagic or symbol"
echo "mismatch, stop - do not force it - and re-check the build against"
echo "scripts/reference/stock-kernel-4.9.170.config."
