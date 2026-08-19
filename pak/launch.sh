#!/bin/sh
# Cast.pak - the on-device cast UI. The daemon it controls outlives this
# process, so nothing here may still be drawing when the script returns:
# NextUI restarts nextui.elf the moment it does, and two processes on
# /dev/fb0 corrupt the launcher until reboot.
PAK_DIR="$(dirname "$0")"
PAK_NAME="$(basename "$PAK_DIR" .pak)"

_base="${SHARED_USERDATA_PATH:-/mnt/SDCARD/.userdata/${PLATFORM:-h700}}"
export HOME="$_base/$PAK_NAME"
mkdir -p "$HOME" "${RGSP_RUN_DIR:-/tmp/rgsp}"

export LD_LIBRARY_PATH="$PAK_DIR/lib/${PLATFORM:-h700}:$LD_LIBRARY_PATH"
exec "$PAK_DIR/rgsp-ui"
