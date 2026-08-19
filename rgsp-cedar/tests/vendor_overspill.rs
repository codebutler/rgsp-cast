//! Replaces tests/test_vendor_overspill.c.
//!
//! How far past each vendor struct's end do the CedarC libraries write? The
//! answer is not zero, and the one case where it is not cost a segfault:
//! AlreadyUsedInputBuffer/ReturnOneAllocInputBuffer modify up to +24 bytes
//! past the end of `used`, every frame. As stack locals that spill landed on
//! scratch; as struct members it landed on live fields.
//!
//! The layout half runs anywhere. The sentinel half needs the device.

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
