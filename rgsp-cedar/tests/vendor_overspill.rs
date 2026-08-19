//! Replaces tests/test_vendor_overspill.c.
//!
//! How far past each vendor struct's end do the CedarC libraries write? The
//! answer is not zero, and the one case where it is not cost a segfault:
//! AlreadyUsedInputBuffer/ReturnOneAllocInputBuffer modify up to +24 bytes
//! past the end of `used`, every frame. As stack locals that spill landed on
//! scratch; as struct members it landed on live fields.
//!
//! The layout half runs anywhere. The sentinel half needs the device.

mod common;

use common::{on_device, LOCK};
use rgsp_cedar::capture::Capture;
use rgsp_cedar::vendor_abi::*;
use std::mem::size_of;

#[test]
fn vendor_struct_sizes_match_the_c_definitions() {
    // Measured from the pre-port C on aarch64 (gcc -O2, ubuntu:22.04, arm64
    // container) by compiling a throwaway program that includes
    // src/rgsp-cast.c and prints sizeof() for each struct. A mismatch means a
    // field type or a _tail size drifted during the port, which is how the
    // segfault in the C version was originally created.
    assert_eq!(size_of::<ScMemOpsS>(), 160);
    assert_eq!(size_of::<VencBaseConfig>(), 192);
    assert_eq!(size_of::<VencInputBuffer>(), 352);
    assert_eq!(size_of::<VencOutputBuffer>(), 320);
    assert_eq!(size_of::<VencAllocateBufferParam>(), 76);
    assert_eq!(size_of::<VencHeaderData>(), 512);
}

#[test]
fn output_buffer_tail_is_256_bytes() {
    let b = VencOutputBuffer::default();
    assert_eq!(b._tail.len(), 256, "measured +0 spill, but the slack is the standing rule");
}

/// How far past `used` do the vendor libraries write?
///
/// The C measured +24 bytes, every frame, from the
/// AlreadyUsedInputBuffer/ReturnOneAllocInputBuffer pair. Most of that write
/// is zeroes, which is why it was invisible to a scan for non-zero bytes and
/// why the guard is filled with 0xAA instead.
///
/// Asserts only that the spill stays inside the 4096-byte guard, not that it
/// is exactly 24: a vendor lib update that widened it should surface as the
/// hazard it is, not as a test bug. The measured value is reported so a change
/// is visible in the output.
#[test]
fn input_buffer_spill_stays_inside_the_guard() {
    if !on_device() {
        eprintln!("skipping: no /dev/fb0");
        return;
    }
    let _g = LOCK.lock().unwrap_or_else(|e| e.into_inner());

    let mut cap = Capture::open(720, 480, 30, 2_000_000).expect("open");
    for _ in 0..3 {
        cap.next().expect("frame");
    }

    let guard = cap.vendor_guard();
    let spill = guard.iter().rposition(|&b| b != 0xAA).map_or(0, |i| i + 1);
    eprintln!("vendor spill past `used`: {spill} bytes (C measured 24)");

    assert!(
        spill < guard.len(),
        "the vendor libraries wrote past the whole {}-byte guard - anything after \
         it in the struct is being corrupted",
        guard.len()
    );
}
