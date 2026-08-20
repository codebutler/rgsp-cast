#!/bin/sh
# Ask the device what it has, without reinstalling anything.
#
# install-pak.sh checksums what it just copied, but that only catches drift
# at install time. This answers the question that has twice cost real time
# on this project -- "does what's actually running on the device match
# dist/?" -- on demand, e.g. before assuming a symptom is a code bug.
#
#   sh verify-pak.sh root@DEVICE
set -eu

DEVICE=${1:?usage: $0 root@DEVICE}
HERE=$(cd "$(dirname "$0")" && pwd)
. "$HERE/lib-checksum.sh"

PAKDIR="$HERE/../dist/Tools/h700/Cast.pak"
VENDORLIBS="$HERE/../vendor-libs"
DEST=/mnt/SDCARD/Tools/h700/Cast.pak
HOOKS=/mnt/SDCARD/.userdata/h700/.hooks

[ -d "$PAKDIR" ] || { echo "build first: make pak" >&2; exit 1; }

if ! ssh "$DEVICE" "[ -d $DEST ]"; then
    echo "FAIL: no install found at $DEST on $DEVICE" >&2
    exit 1
fi

MANIFEST=$(mktemp)
HOOKS_MANIFEST=$(mktemp)
trap 'rm -f "$MANIFEST" "$HOOKS_MANIFEST"' EXIT

manifest_of_dir "$PAKDIR" > "$MANIFEST"
if [ -d "$VENDORLIBS" ]; then
    for f in "$VENDORLIBS"/*; do
        [ -f "$f" ] || continue
        printf 'lib/h700/%s\t%s\n' "$(basename "$f")" "$f" >> "$MANIFEST"
    done
else
    echo "note: no local vendor-libs/ -- skipping lib/h700/*.so (run extract-vendor-libs.sh to check those too)" >&2
fi
for phase in boot pre-launch pre-sleep post-resume; do
    for f in "$PAKDIR/hooks/$phase.d"/*.sh; do
        [ -f "$f" ] || continue
        printf '%s.d/%s\t%s\n' "$phase" "$(basename "$f")" "$f" >> "$HOOKS_MANIFEST"
    done
done

FAILED=0
verify_manifest "$DEVICE" "$DEST" "$MANIFEST" || FAILED=1
verify_manifest "$DEVICE" "$HOOKS" "$HOOKS_MANIFEST" || FAILED=1

RUNNING=$(ssh "$DEVICE" "pidof rgsp-ui rgsp-host 2>/dev/null" || true)

if [ "$FAILED" != 0 ]; then
    echo "FAIL: device does not match dist/ -- reinstall with install-pak.sh" >&2
    [ -n "$RUNNING" ] && echo "(also currently running, pid(s): $RUNNING -- it is executing whatever it loaded last, which may differ from either dist/ or the file now on disk)" >&2
    exit 1
fi

N=$(wc -l < "$MANIFEST" | tr -d ' ')
H=$(wc -l < "$HOOKS_MANIFEST" | tr -d ' ')
echo "OK: $N pak file(s), $H hook file(s) on $DEVICE match dist/"

if [ -n "$RUNNING" ]; then
    echo "NOTE: rgsp-ui/rgsp-host running (pid(s): $RUNNING) -- files on disk match dist/, but a running process only picks this up on restart."
fi
