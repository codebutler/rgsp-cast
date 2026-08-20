#!/bin/sh
# Device smoke test for Cast.pak's launch.sh -> rgsp-ui hand-off.
#
# What this checks:
#   1. rgsp-ui actually starts under the environment NextUI's launcher
#      exports for a pak (DEVICE/RGXX_MODEL/PLATFORM), i.e. it does not fall
#      back to the generic 640x480 panel size and refuse to run.
#   2. The framebuffer holds real content while rgsp-ui is running.
#   3. The assertion that matters most, kept permanent: launching and then
#      ending the pak leaves no *new* process holding /dev/fb0. This
#      generalises past the show2 bug (55d57f2) to anything the pak ever
#      spawns -- a process left holding the framebuffer corrupts the
#      launcher until reboot.
#
#   4. The daemon rgsp-ui starts does not inherit the UI's framebuffer.
#      rgsp-ui is re-run with --smoke-start-daemon (a non-interactive entry
#      point that brings the display up, calls Service::start, and exits)
#      because there is no input injection here to press A on the Home
#      screen. rgsp-host outlives the UI by design, so if it inherited the
#      UI's /dev/fb0 fd it would hold the framebuffer forever -- the exact
#      corrupt-the-launcher-until-reboot failure this pak exists to prevent.
#      Descriptors opened by C (GFX_init/SDL/msettings) carry no CLOEXEC,
#      which is why this is not automatic; see the pre_exec in
#      rgsp-ui/src/service.rs.
#
#      This is a before/after diff, not "only nextui.elf may hold it": on
#      real hardware several unrelated boot daemons (wpa_supplicant,
#      wpa_cli, rtk_hciattach, udhcpc, bluetoothd, bluealsa) already hold an
#      inherited fd on /dev/fb0 at idle, from before this pak ever runs --
#      confirmed via /proc/<pid>/fd on this device, present even with the
#      pak never launched. A literal "only nextui.elf" check would fail on
#      a clean device for reasons that have nothing to do with this pak.
#
# What this deliberately does NOT check:
#   - The actual Tools -> Cast menu entry, or the B-button exit path. There
#     is no input-injection here, so "ending the session" below is done by
#     SIGTERMing rgsp-ui directly rather than pressing B. That takes a
#     different code path than a clean exit: SIGTERM with no handler
#     installed just terminates the process, so it's the *kernel* reclaiming
#     the fb0 fd on process death, not rgsp-ui's own Ui::drop()/GFX_quit
#     teardown. The Drop path is only exercised by a human pressing B.
#   - This script launches rgsp-ui over ssh while nextui.elf is still live
#     and undisturbed -- the real launcher stops itself before handing off
#     to a pak. Two processes drawing to fb0 at once is the exact failure
#     mode this project exists to prevent, so any on-screen weirdness while
#     this script runs is a harness artifact of that overlap, not evidence
#     of a pak bug. It does not affect the after-the-fact fb0 assertion.
#   - Pairing/PIN entry, which also needs button presses.
#   - Pixel-perfect screen content -- only that the framebuffer is not
#     blank, which is enough to know rendering is happening at all.
#
#   sh smoke-ui.sh root@DEVICE
set -eu

DEVICE=${1:?usage: $0 root@DEVICE}
PAKDIR=/mnt/SDCARD/Tools/h700/Cast.pak
HERE=$(cd "$(dirname "$0")" && pwd)

# Mirror what NextUI's own launcher exports for every pak (see the comment
# in pak/launch.sh). Without these, rgsp-ui's panel-geometry check refuses
# to start it -- see rgsp-ui/src/ui.rs. PATH/LD_LIBRARY_PATH mirror the
# system runtime the launcher itself runs under (libmsettings.so and friends
# live under .system, not in the pak) -- an interactive ssh session does not
# inherit these the way a launcher-spawned process would.
PAK_ENV="DEVICE=rgsp RGXX_MODEL=RGSP PLATFORM=h700 \
SDCARD_PATH=/mnt/SDCARD USERDATA_PATH=/mnt/SDCARD/.userdata/h700 \
SHARED_USERDATA_PATH=/mnt/SDCARD/.userdata/h700 RGSP_RUN_DIR=/tmp/rgsp \
PATH=/mnt/SDCARD/.system/h700/bin:\$PATH \
LD_LIBRARY_PATH=/mnt/SDCARD/.system/h700/lib:/usr/lib:/usr/lib/aarch64-linux-gnu"

fb0_holders() {
    # One pid per line: "<pid> <comm>". Not just fuser's pid list -- comm
    # names are what make the before/after diff below readable and let us
    # single out an offender by name rather than a bare pid.
    ssh "$DEVICE" '
        for p in $(fuser /dev/fb0 2>/dev/null); do
            echo "$p $(cat /proc/$p/comm 2>/dev/null || echo "?")"
        done
    '
}

echo "== clearing any stray instance from a previous run =="
ssh "$DEVICE" "killall rgsp-ui rgsp-host 2>/dev/null; rm -f /tmp/rgsp/daemon.pid; true"
sleep 1

echo "== recording the baseline set of processes holding /dev/fb0 =="
BASELINE=$(fb0_holders)
echo "$BASELINE"

echo "== launching Cast.pak's launch.sh in the background =="
ssh "$DEVICE" "cd $PAKDIR && env $PAK_ENV nohup sh launch.sh >/tmp/rgsp-smoke.log 2>&1 &"

echo "== waiting for rgsp-ui to come up =="
UI_PID=""
i=0
while [ -z "$UI_PID" ] && [ $i -lt 15 ]; do
    UI_PID=$(ssh "$DEVICE" "pidof rgsp-ui" 2>/dev/null || true)
    i=$((i+1))
    sleep 1
done
if [ -z "$UI_PID" ]; then
    echo "FAIL: rgsp-ui never appeared in the process list after launch.sh ran"
    echo "--- /tmp/rgsp-smoke.log on device ---"
    ssh "$DEVICE" "cat /tmp/rgsp-smoke.log" 2>/dev/null || true
    exit 1
fi
echo "rgsp-ui running as pid $UI_PID"

echo "== capturing the framebuffer while rgsp-ui is actively redrawing =="
# screenshot.sh reads one static frame off a fixed offset -- it only shows
# real content while the target is continuously flipping between its two
# buffers (a blank-white read from the NextUI launcher itself, sitting idle,
# confirmed this the hard way). rgsp-ui's main loop redraws and flips every
# iteration unconditionally, with no idle/dirty tracking, so capturing while
# it is up is a fair test of what's actually on screen -- not a workaround.
sh "$HERE/screenshot.sh" "$DEVICE" /tmp/rgsp-ui-smoke.png

# Best-effort, non-fatal: warn (don't fail) on a uniform capture. rgsp-ui's
# continuous redraw makes a uniform frame unlikely, but a single sampled
# frame can still land on a genuinely flat moment (e.g. between screens), so
# this is a hint for a human glancing at the log, not the load-bearing
# assertion -- that one is the fb0-ownership check below.
if command -v ffmpeg >/dev/null 2>&1; then
    UNIQUE=$(ffmpeg -v quiet -i /tmp/rgsp-ui-smoke.png -vf format=gray -f rawvideo - \
             | od -An -tu1 -v | tr -s ' ' '\n' | sort -un | wc -l | tr -d ' ')
    if [ "$UNIQUE" = "1" ]; then
        echo "WARN: /tmp/rgsp-ui-smoke.png is a single flat color -- look at it by hand"
    else
        echo "screenshot has $UNIQUE distinct gray levels, not flat"
    fi
fi
echo "wrote /tmp/rgsp-ui-smoke.png"

echo "== ending the session (SIGTERM stands in for the B-button exit; no"
echo "   input injection exists here for a human to press it) =="
ssh "$DEVICE" "kill -TERM $UI_PID" 2>/dev/null || true
i=0
while ssh "$DEVICE" "kill -0 $UI_PID" 2>/dev/null && [ $i -lt 15 ]; do
    i=$((i+1)); sleep 1
done
if ssh "$DEVICE" "kill -0 $UI_PID" 2>/dev/null; then
    echo "FAIL: rgsp-ui did not exit within 15s of SIGTERM"
    ssh "$DEVICE" "kill -KILL $UI_PID" 2>/dev/null || true
    exit 1
fi
echo "rgsp-ui exited"

echo "== starting the daemon the way the UI does, from a process holding fb0 =="
# --smoke-start-daemon stands in for pressing A on Home: there is no input
# injection here. It runs *after* the interactive rgsp-ui above has exited,
# because two processes drawing to fb0 at once is the failure this project
# exists to prevent. RGSP_PAK_DIR is exported explicitly -- launch.sh sets it
# for the interactive path, but this invocation bypasses launch.sh.
if ! ssh "$DEVICE" "cd $PAKDIR && env $PAK_ENV RGSP_PAK_DIR=$PAKDIR \
        LD_LIBRARY_PATH=$PAKDIR/lib/h700:/mnt/SDCARD/.system/h700/lib:/usr/lib:/usr/lib/aarch64-linux-gnu \
        ./rgsp-ui --smoke-start-daemon"; then
    echo "FAIL: rgsp-ui --smoke-start-daemon could not start rgsp-host"
    ssh "$DEVICE" "tail -40 /tmp/rgsp/daemon.log" 2>/dev/null || true
    exit 1
fi
echo "rgsp-host started and rgsp-ui exited"

echo "== the assertion that matters: no NEW process holds /dev/fb0 =="
# Diff against the baseline recorded before the pak ever ran, rather than
# asserting a fixed allow-list -- see the file header for why a bare
# "only nextui.elf" check produces a false failure on this hardware. Any
# pid present now but absent from the baseline is something this pak's run
# left behind, which is exactly the show2 class of bug this test guards
# against, permanently and for anything the pak ever spawns (not just
# show2 specifically).
AFTER=$(fb0_holders)
# comm needs sorted real files, not process substitution, to stay POSIX sh.
BASE_SORTED=$(mktemp)
AFTER_SORTED=$(mktemp)
trap 'rm -f "$BASE_SORTED" "$AFTER_SORTED"' EXIT
echo "$BASELINE" | sort > "$BASE_SORTED"
echo "$AFTER" | sort > "$AFTER_SORTED"
NEW=$(comm -13 "$BASE_SORTED" "$AFTER_SORTED")
if [ -n "$NEW" ]; then
    echo "FAIL: /dev/fb0 gained new holder(s) that survived the pak's run:"
    echo "$NEW"
    exit 1
fi

# Belt-and-braces: name-check the offenders this test exists for, regardless
# of the baseline. A prior smoke run that itself left something behind would
# otherwise get grandfathered into BASELINE and silently mask a real leak.
# rgsp-host is checked too, and deliberately so: it is started above by the
# UI, it outlives the pak, and it has no business ever touching /dev/fb0 --
# it captures through the Cedar VE, not the framebuffer. An fd on fb0 here
# could only be one it inherited from rgsp-ui, which is the leak.
NAMED_BAD=$(echo "$AFTER" | awk '$2 == "rgsp-ui" || $2 == "rgsp-host" || $2 == "show2.elf"')
if [ -n "$NAMED_BAD" ]; then
    echo "FAIL: known-offender process still holds /dev/fb0: $NAMED_BAD"
    ssh "$DEVICE" "killall rgsp-host 2>/dev/null; true"
    exit 1
fi

# The daemon is meant to outlive the pak, so nothing above stops it. Leave
# the device as this script found it.
echo "== stopping the daemon this run started =="
ssh "$DEVICE" "killall rgsp-host 2>/dev/null; true"

echo "PASS: no new process is holding /dev/fb0 after the pak returned"
