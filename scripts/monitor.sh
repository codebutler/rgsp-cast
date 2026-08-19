#!/bin/sh
# Sample CPU/GPU/thermal counters into a log for offline analysis.
# Raw values only — no arithmetic on-device, so sampling stays cheap and the
# measurement does not distort what it measures.
#
#   sh monitor.sh <seconds> <outfile> [watch_pid]
SECS=${1:-35}
OUT=${2:-/tmp/venc/mon.log}
WATCH=${3:-}
INTERVAL_MS=250

: > "$OUT"
echo "# start $(cut -d' ' -f1 /proc/uptime)" >> "$OUT"
echo "# cores $(grep -c ^processor /proc/cpuinfo)" >> "$OUT"
echo "# gpu_freqs $(cat /sys/class/devfreq/gpu/available_frequencies 2>/dev/null)" >> "$OUT"
echo "# trans_stat_begin" >> "$OUT"
sed 's/^/# /' /sys/class/devfreq/gpu/trans_stat 2>/dev/null >> "$OUT"

N=$(( SECS * 1000 / INTERVAL_MS ))
i=0
while [ $i -lt $N ]; do
    T=$(cut -d' ' -f1 /proc/uptime)
    echo "T $T" >> "$OUT"
    grep '^cpu' /proc/stat | sed 's/^/S /' >> "$OUT"
    printf 'F' >> "$OUT"
    for c in 0 1 2 3; do
        printf ' %s' "$(cat /sys/devices/system/cpu/cpu$c/cpufreq/scaling_cur_freq 2>/dev/null)" >> "$OUT"
    done
    echo >> "$OUT"
    echo "G $(cat /sys/class/devfreq/gpu/cur_freq 2>/dev/null) $(cat /sys/kernel/debug/mali0/ipa_current_power 2>/dev/null)" >> "$OUT"
    printf 'H' >> "$OUT"
    for z in /sys/class/thermal/thermal_zone0 /sys/class/thermal/thermal_zone1 /sys/class/thermal/thermal_zone2 /sys/class/thermal/thermal_zone3; do
        printf ' %s' "$(cat $z/temp 2>/dev/null)" >> "$OUT"
    done
    echo >> "$OUT"
    # per-process jiffies: utime+stime for the capture and for the frontend
    for p in $WATCH $(pidof rgsp-cast 2>/dev/null) $(pidof minarch.elf 2>/dev/null) $(pidof nextui.elf 2>/dev/null); do
        [ -r /proc/$p/stat ] || continue
        echo "P $p $(cut -d' ' -f2,14,15 /proc/$p/stat 2>/dev/null)" >> "$OUT"
    done
    i=$((i+1))
    usleep $(( INTERVAL_MS * 1000 )) 2>/dev/null || sleep 0.25
done

echo "# trans_stat_end" >> "$OUT"
sed 's/^/# /' /sys/class/devfreq/gpu/trans_stat 2>/dev/null >> "$OUT"
echo "# end $(cut -d' ' -f1 /proc/uptime)" >> "$OUT"
