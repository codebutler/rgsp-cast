//! Shared device-test scaffolding.
//!
//! `Capture` is single-instance per process by design (see
//! `rgsp_cedar::capture`), so tests that open one must not run concurrently
//! with each other. Each integration test file (`capture_api.rs`,
//! `vendor_overspill.rs`, `idr_cadence.rs`, `reopen_leak.rs`) compiles to its
//! own binary and therefore its own process, but the `#[test]` functions
//! within one binary run on cargo's threaded harness. A `LOCK` declared
//! per-file only serialises the tests in that file; one runner could still
//! land mid-capture in another binary's test at the same time under a
//! parallel-capable runner (e.g. `cargo-nextest`, which runs test binaries
//! concurrently unlike plain `cargo test`). Sharing this module across all
//! four files at least keeps the guard consistent and ready for that case,
//! rather than four subtly-different copies drifting apart.
#![allow(dead_code)]

use std::sync::Mutex;

pub static LOCK: Mutex<()> = Mutex::new(());

pub fn on_device() -> bool {
    std::path::Path::new("/dev/fb0").exists()
}
