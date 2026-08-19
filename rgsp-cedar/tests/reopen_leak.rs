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

#[test]
fn twenty_open_capture_close_cycles_do_not_grow_rss() {
    if !on_device() {
        eprintln!("skipping: no /dev/fb0");
        return;
    }
    let _g = LOCK.lock().unwrap_or_else(|e| e.into_inner());

    let mut rss0: Option<i64> = None;
    let mut rss_last: Option<i64> = None;

    eprintln!("cycle  rss_kB  d_rss");
    for cycle in 0..CYCLES {
        let mut cap = Capture::open(720, 480, 30, 2_000_000)
            .unwrap_or_else(|e| panic!("cycle {cycle}: open failed: {e}"));
        for frame in 0..FRAMES_PER {
            cap.next()
                .unwrap_or_else(|e| panic!("cycle {cycle} frame {frame}: {e}"));
        }
        drop(cap);

        let rss = rss_kb();
        if let Some(rss) = rss {
            let baseline = *rss0.get_or_insert(rss);
            rss_last = Some(rss);
            eprintln!("{cycle:5}  {rss:6}  {:+5}", rss - baseline);
        }
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
        _ => eprintln!("RSS unavailable (not /proc/self/status) - reopen ran but growth was not measured"),
    }
    eprintln!("PASS: reopen works {CYCLES} times in one process");
}
