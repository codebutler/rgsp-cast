#!/bin/sh
# Cast.pak - the on-device cast UI. The daemon it controls outlives this
# process, so nothing here may still be drawing when the script returns:
# NextUI restarts nextui.elf the moment it does, and two processes on
# /dev/fb0 corrupt the launcher until reboot.
# Absolute, and exported: rgsp-ui reads RGSP_PAK_DIR to find the `rgsp-host`
# binary it starts. `cd` before `pwd` because `dirname "$0"` is "." when the
# launcher (or scripts/smoke-ui.sh) runs this from inside the pak directory,
# and a relative path would not survive rgsp-ui's own working directory.
PAK_DIR="$(cd "$(dirname "$0")" && pwd)"
export RGSP_PAK_DIR="$PAK_DIR"
PAK_NAME="$(basename "$PAK_DIR" .pak)"

_base="${SHARED_USERDATA_PATH:-/mnt/SDCARD/.userdata/${PLATFORM:-h700}}"
export HOME="$_base/$PAK_NAME"
mkdir -p "$HOME" "${RGSP_RUN_DIR:-/tmp/rgsp}"

export LD_LIBRARY_PATH="$PAK_DIR/lib/${PLATFORM:-h700}:$LD_LIBRARY_PATH"
exec "$PAK_DIR/rgsp-ui"
