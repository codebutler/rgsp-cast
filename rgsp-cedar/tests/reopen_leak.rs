//! Replaces tests/test_reopen_leak.c. Runs only on the device.
//!
//! Open -> capture -> close, twenty times in one process.
//!
//! The daemon does one of these cycles per Moonlight session, and nothing had
//! ever executed that path: the old CLI opened once and exited, so every
//! teardown was followed immediately by process death. Two things make it
//! worth checking. `dlclose` was deliberately dropped and `VendorLibs::load`
//! made idempotent so a reopen works at all (see `vendor_lib.rs`); and the
//! vendor logs `CdcIonFree ... errno:22` on every run, which is EINVAL on a
//! free - the shape of an allocation not coming back. Harmless in a process
//! that is about to exit, not harmless in a long-lived daemon.
//!
//! RSS is not the right instrument for that: ION buffers are kernel-side
//! allocations, so a failing free need not inflate this process's RSS at
//! all. The real instrument is ION accounting from debugfs, in particular
//! the "orphaned allocations" section - buffers whose owning client is gone,
//! which is precisely what a failing `CdcIonFree` would leave behind. RSS is
//! still tracked below because it is free and it is what the daemon's own
//! resource ceiling cares about, but the ION half is the one this test
//! exists for.

mod common;

use common::{on_device, LOCK};
use rgsp_cedar::capture::Capture;

const CYCLES: usize = 20;
const FRAMES_PER: usize = 5;

/// The C version only reported RSS growth; this asserts it. Chosen from
/// observed flat behaviour on-device (RSS stays within a few hundred kB
/// across 20 cycles once the allocator's steady-state churn settles), not
/// from any vendor spec - there is no documented bound on what one reopen
/// cycle costs. Wide enough to absorb ordinary allocator noise, tight enough
/// to catch a real per-cycle leak, which over 20 cycles would already show as
/// several megabytes.
const MAX_RSS_GROWTH_KB: i64 = 4096;

/// Same story as `MAX_RSS_GROWTH_KB`, but for orphaned ION bytes: chosen from
/// observed flat behaviour on-device, not a vendor spec. A failing
/// `CdcIonFree` leaves its buffer behind as an orphan every cycle, so a real
/// leak here grows linearly with `CYCLES` and would clear this threshold by
/// a wide margin long before 20 cycles.
const MAX_ION_ORPHAN_GROWTH_BYTES: i64 = 8 * 1024 * 1024;

/// Resident set size in kB from /proc/self/status, or `None` if unavailable
/// (e.g. not running on Linux).
fn rss_kb() -> Option<i64> {
    let status = std::fs::read_to_string("/proc/self/status").ok()?;
    for line in status.lines() {
        if let Some(rest) = line.strip_prefix("VmRSS:") {
            return rest.trim().split_whitespace().next()?.parse().ok();
        }
    }
    None
}

/// ION accounting, read from debugfs. Each heap file is a table of
///
/// ```text
///       client              pid             size
///   nextui.elf             1517          4153344
/// ```
///
/// followed by an "orphaned allocations" section listing buffers whose
/// client is gone - which is precisely what a failing `CdcIonFree` would
/// leave behind. Sums live bytes across every heap, this process's own
/// share, and the orphan total.
#[derive(Debug, Default, Clone, Copy)]
struct IonStat {
    total: i64,
    mine: i64,
    orphan: i64,
}

/// Where `IonStat` actually got its numbers from - printed every run so a
/// container/CI environment without ION debugfs is loud about not having
/// measured the leak class this test exists to catch, rather than silently
/// reporting zeroes that look like a clean pass.
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
enum IonSource {
    /// Read from the real ION debugfs heaps - the only source the orphan
    /// assertion trusts.
    Ion,
    /// debugfs was unreadable; fell back to /proc/meminfo's MemFree, which
    /// says nothing about orphaned allocations.
    MemFree,
    /// Neither was readable.
    Unavailable,
}

const ION_HEAPS: [&str; 3] = [
    "/sys/kernel/debug/ion/heaps/cma",
    "/sys/kernel/debug/ion/heaps/secure",
    "/sys/kernel/debug/ion/heaps/sys_user",
];

fn ion_read_heap(path: &str, my_pid: i64, st: &mut IonStat) {
    let Ok(text) = std::fs::read_to_string(path) else {
        return;
    };
    let mut in_orphan = false;
    for line in text.lines() {
        if line.contains("orphaned") {
            in_orphan = true;
            continue;
        }
        let mut fields = line.split_whitespace();
        let (Some(_name), Some(pid), Some(size)) =
            (fields.next(), fields.next(), fields.next())
        else {
            continue;
        };
        let (Ok(pid), Ok(size)) = (pid.parse::<i64>(), size.parse::<i64>()) else {
            continue;
        };
        if in_orphan {
            st.orphan += size;
        } else {
            st.total += size;
            if pid == my_pid {
                st.mine += size;
            }
        }
    }
}

fn ion_stat(my_pid: i64) -> (IonStat, IonSource) {
    let mut st = IonStat::default();
    for heap in ION_HEAPS {
        ion_read_heap(heap, my_pid, &mut st);
    }
    if st.total != 0 {
        return (st, IonSource::Ion);
    }

    // debugfs unreadable (or genuinely empty, indistinguishable from here) -
    // fall back to MemFree, matching the C. This source cannot say anything
    // about orphans, so the orphan assertion below only trusts `IonSource::Ion`.
    let Ok(meminfo) = std::fs::read_to_string("/proc/meminfo") else {
        return (st, IonSource::Unavailable);
    };
    for line in meminfo.lines() {
        if let Some(rest) = line.strip_prefix("MemFree:") {
            if let Some(kb) = rest.trim().split_whitespace().next().and_then(|s| s.parse::<i64>().ok())
            {
                st.total = kb * 1024;
            }
            break;
        }
    }
    (st, IonSource::MemFree)
}

#[test]
fn twenty_open_capture_close_cycles_do_not_leak() {
    if !on_device() {
        eprintln!("skipping: no /dev/fb0");
        return;
    }
    let _g = LOCK.lock().unwrap_or_else(|e| e.into_inner());

    let my_pid = std::process::id() as i64;
    let mut rss0: Option<i64> = None;
    let mut rss_last: Option<i64> = None;
    let mut ion0: Option<(IonStat, IonSource)> = None;
    let mut ion_last: Option<(IonStat, IonSource)> = None;

    eprintln!("cycle  rss_kB  d_rss   ion_total  d_ion   ion_mine  orphaned");
    for cycle in 0..CYCLES {
        let mut cap = Capture::open(720, 480, 30, 2_000_000)
            .unwrap_or_else(|e| panic!("cycle {cycle}: open failed: {e}"));
        for frame in 0..FRAMES_PER {
            cap.next()
                .unwrap_or_else(|e| panic!("cycle {cycle} frame {frame}: {e}"));
        }
        drop(cap);

        let rss = rss_kb();
        let (ion, source) = ion_stat(my_pid);
        let d_rss = rss.map(|r| r - *rss0.get_or_insert(r));
        let (ion_base, _) = *ion0.get_or_insert((ion, source));
        rss_last = rss.or(rss_last);
        ion_last = Some((ion, source));

        eprintln!(
            "{cycle:5}  {:>6}  {:>+5}   {:9}  {:+6}  {:8}  {:8}",
            rss.map_or("?".to_string(), |r| r.to_string()),
            d_rss.map_or("?".to_string(), |d| format!("{d:+}")),
            ion.total,
            ion.total - ion_base.total,
            ion.mine,
            ion.orphan,
        );
    }

    eprintln!("\n{CYCLES} cycles of open/{FRAMES_PER} frames/close completed");

    match (rss0, rss_last) {
        (Some(start), Some(end)) => {
            let growth = end - start;
            eprintln!("RSS  {start} -> {end} kB ({growth:+} kB over {CYCLES} cycles)");
            assert!(
                growth < MAX_RSS_GROWTH_KB,
                "RSS grew {growth} kB over {CYCLES} reopen cycles, over the \
                 {MAX_RSS_GROWTH_KB} kB threshold - CdcIonFree's errno:22 may \
                 be a real leak, not just noise"
            );
        }
        _ => eprintln!("RSS unavailable (not /proc/self/status) - growth was not measured"),
    }

    match (ion0, ion_last) {
        (Some((start, IonSource::Ion)), Some((end, IonSource::Ion))) => {
            let orphan_growth = end.orphan - start.orphan;
            eprintln!(
                "ION total {} -> {} bytes ({:+})",
                start.total,
                end.total,
                end.total - start.total
            );
            eprintln!("ION ours  {} -> {} bytes ({:+})", start.mine, end.mine, end.mine - start.mine);
            eprintln!(
                "ION orphaned {} -> {} bytes ({:+})",
                start.orphan, end.orphan, orphan_growth
            );
            assert!(
                orphan_growth < MAX_ION_ORPHAN_GROWTH_BYTES,
                "orphaned ION allocations grew {orphan_growth} bytes over {CYCLES} \
                 reopen cycles, over the {MAX_ION_ORPHAN_GROWTH_BYTES}-byte threshold \
                 - this is exactly the shape a failing CdcIonFree leaves behind"
            );
        }
        (Some((_, source)), _) | (None, Some((_, source))) => {
            eprintln!(
                "ION accounting UNAVAILABLE (source: {source:?}) - debugfs was not \
                 readable, so the orphaned-allocation leak this test exists to catch \
                 was NOT measured this run"
            );
        }
        (None, None) => {
            eprintln!(
                "ION accounting UNAVAILABLE - no reading was ever taken, so the \
                 orphaned-allocation leak this test exists to catch was NOT measured \
                 this run"
            );
        }
    }

    eprintln!("PASS: reopen works {CYCLES} times in one process");
}
