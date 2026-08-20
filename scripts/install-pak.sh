#!/bin/sh
# Install Cast.pak on the device, including hooks and the vendor libraries.
#
# The CedarC blobs are fetched on the device rather than shipped: they are
# proprietary, and extract-vendor-libs.sh verifies checksums against TrimUI's
# own firmware release.
#
# Twice on this project a stale binary on the device has masqueraded as a
# code bug -- once a pre-control-socket rgsp-host, once a stale rgsp-ui that
# hadn't picked up a wire-format change. So this script does two more things
# past "copy the files": it checksums every file it copies and fails loudly
# on any mismatch, and it warns if rgsp-ui/rgsp-host is still running
# afterwards -- overwriting the file on disk does not change what a process
# already running from the old inode is executing.
set -eu

DEVICE=${1:?usage: $0 root@DEVICE}
HERE=$(cd "$(dirname "$0")" && pwd)
. "$HERE/lib-checksum.sh"

PAKDIR="$HERE/../dist/Tools/h700/Cast.pak"
VENDORLIBS="$HERE/../vendor-libs"
DEST=/mnt/SDCARD/Tools/h700/Cast.pak
STAGING=$DEST.new
HOOKS=/mnt/SDCARD/.userdata/h700/.hooks

[ -d "$PAKDIR" ] || { echo "build first: make pak" >&2; exit 1; }

ssh "$DEVICE" "mkdir -p $DEST $HOOKS/boot.d $HOOKS/pre-launch.d $HOOKS/pre-sleep.d $HOOKS/post-resume.d \
    && rm -rf $STAGING && mkdir -p $STAGING"

# Land everything in a staging dir next to $DEST first, then move each file
# into place with `mv` (a rename, same filesystem) rather than overwriting
# $DEST directly. rgsp-host/rgsp-ui may be running: scp-ing straight over a
# running executable's inode fails with ETXTBSY, and a rename is the
# standard way around that -- it repoints the directory entry without
# touching the inode the running process still holds open.
scp -q -r "$PAKDIR"/* "$DEVICE:$STAGING/"

# Hooks live under .userdata, not in the pak, so NextUI finds them. These are
# plain shell scripts read by an interpreter, not exec'd as their own text
# segment, so ETXTBSY does not apply here -- copied straight in as before.
for phase in boot pre-launch pre-sleep post-resume; do
    for f in "$PAKDIR/hooks/$phase.d"/*.sh; do
        [ -f "$f" ] || continue
        scp -q "$f" "$DEVICE:$HOOKS/$phase.d/"
    done
done

# Vendor libraries, fetched here and pushed, never committed.
if [ ! -d "$VENDORLIBS" ]; then
    "$HERE/extract-vendor-libs.sh"
fi
ssh "$DEVICE" "mkdir -p $STAGING/lib/h700"
scp -q "$VENDORLIBS"/* "$DEVICE:$STAGING/lib/h700/"

ssh "$DEVICE" "chmod +x $STAGING/launch.sh $STAGING/rgsp-host $STAGING/rgsp-ui $HOOKS/*/*.sh"

# Move staged files into place one at a time so any single ETXTBSY (or other
# failure) names the file instead of aborting a bulk operation half-done.
ssh "$DEVICE" "
    cd $STAGING &&
    find . -type f | while read -r f; do
        mkdir -p \"$DEST/\$(dirname \"\$f\")\" &&
        mv \"\$f\" \"$DEST/\$f\" || exit 1
    done &&
    cd / && rm -rf $STAGING
"

ssh "$DEVICE" "lsmod | grep -q '^snd_aloop' || insmod $DEST/snd-aloop.ko"

echo "== verifying what landed on the device matches what was built =="
MANIFEST=$(mktemp)
HOOKS_MANIFEST=$(mktemp)
trap 'rm -f "$MANIFEST" "$HOOKS_MANIFEST"' EXIT

manifest_of_dir "$PAKDIR" > "$MANIFEST"
for f in "$VENDORLIBS"/*; do
    [ -f "$f" ] || continue
    printf 'lib/h700/%s\t%s\n' "$(basename "$f")" "$f" >> "$MANIFEST"
done
for phase in boot pre-launch pre-sleep post-resume; do
    for f in "$PAKDIR/hooks/$phase.d"/*.sh; do
        [ -f "$f" ] || continue
        printf '%s.d/%s\t%s\n' "$phase" "$(basename "$f")" "$f" >> "$HOOKS_MANIFEST"
    done
done

FAILED=0
verify_manifest "$DEVICE" "$DEST" "$MANIFEST" || FAILED=1
verify_manifest "$DEVICE" "$HOOKS" "$HOOKS_MANIFEST" || FAILED=1
if [ "$FAILED" != 0 ]; then
    echo "FAIL: the device does not match dist/ -- see mismatches above" >&2
    exit 1
fi
N=$(wc -l < "$MANIFEST" | tr -d ' ')
H=$(wc -l < "$HOOKS_MANIFEST" | tr -d ' ')
echo "verified: $N pak file(s), $H hook file(s), all match dist/"

# Overwriting the file on disk does not change what an already-running
# process is executing -- this is trap 2 from the header comment. Warn
# plainly rather than let it masquerade as a code bug a second time.
RUNNING=$(ssh "$DEVICE" "pidof rgsp-ui rgsp-host 2>/dev/null" || true)
if [ -n "$RUNNING" ]; then
    cat <<EOF

NOTE: rgsp-ui and/or rgsp-host is still running on the device (pid(s):
$RUNNING). It is running the OLD binary -- the copy on disk was just
updated, but a running process keeps executing what it already loaded.
Reopen Cast from the NextUI menu (or press A on the Home screen to
restart the capture service) to pick up what was just installed.
EOF
fi

cat <<EOF

Installed to $DEST
Hooks installed to $HOOKS

Launch it from Tools -> Cast. Pair from Moonlight; the PIN prompt is on
the device now, not a browser.
EOF
