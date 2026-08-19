//! Replaces tests/test_capture_api.c. Runs only on the device.

use rgsp_cedar::capture::Capture;
use std::sync::Mutex;

/// Capture is single-instance per process by design, so tests that open one
/// must not run concurrently. cargo's harness is threaded by default, and on
/// the device - where neither test short-circuits - an unserialised pair races
/// for the guard and one fails.
static LOCK: Mutex<()> = Mutex::new(());

fn on_device() -> bool {
    std::path::Path::new("/dev/fb0").exists()
}

#[test]
fn captures_annexb_frames_starting_with_a_keyframe() {
    if !on_device() {
        eprintln!("skipping: no /dev/fb0");
        return;
    }
    let _g = LOCK.lock().unwrap_or_else(|e| e.into_inner());

    let mut cap = Capture::open(720, 480, 30, 2_000_000).expect("open");

    let frame = cap.next().expect("first frame");
    assert!(frame.data.len() > 4);
    assert_eq!(&frame.data[..4], &[0, 0, 0, 1], "annex-b start code");
    // Frame 0 must be a keyframe because the encoder emits SPS/PPS + IDR first.
    assert!(frame.is_keyframe);

    for _ in 0..30 {
        let f = cap.next().expect("frame");
        assert!(!f.data.is_empty());
    }

    cap.request_idr();
    assert!(cap.next().expect("forced idr").is_keyframe);
}

#[test]
fn single_instance_guard_enforces_one_capture_per_process() {
    if !on_device() {
        eprintln!("skipping: no /dev/fb0");
        return;
    }
    let _g = LOCK.lock().unwrap_or_else(|e| e.into_inner());

    let cap1 = Capture::open(720, 480, 30, 2_000_000).expect("first open");
    match Capture::open(720, 480, 30, 2_000_000) {
        Err(e) => assert!(e.to_string().contains("already open"), "got: {e}"),
        Ok(_) => panic!("second open should fail while first is held"),
    }
    drop(cap1);
    Capture::open(720, 480, 30, 2_000_000)
        .expect("open should succeed after the first is dropped");
}

#[test]
fn a_failed_capture_is_terminal() {
    if !on_device() {
        eprintln!("skipping: no /dev/fb0");
        return;
    }
    // A failure is terminal: the capture object is dead and every later call
    // returns the original error. A failed frame can leave the encoder's input
    // buffer submitted or unacquired, so driving it again would work on
    // inconsistent state. Nothing on-device induces a failure reliably, so
    // this asserts the sticky-flag plumbing via the public error text only if
    // a failure happens to occur; otherwise it is a no-op.
    let _g = LOCK.lock().unwrap_or_else(|e| e.into_inner());

    let mut cap = Capture::open(720, 480, 30, 2_000_000).expect("open");
    let mut first_error: Option<String> = None;
    for _ in 0..8 {
        match cap.next() {
            Ok(_) => {}
            Err(e) => {
                let text = e.to_string();
                match &first_error {
                    None => first_error = Some(text),
                    Some(original) => assert_eq!(
                        original, &text,
                        "a later call must repeat the original diagnosis"
                    ),
                }
            }
        }
    }
    if let Some(original) = first_error {
        // Once dead, it stays dead: the very next call must fail with the
        // same text rather than driving the encoder again.
        let again = match cap.next() {
            Ok(_) => panic!("a failed capture must stay failed"),
            Err(e) => e.to_string(),
        };
        assert_eq!(again, original);
    }
}

/// A requested size that is not the panel's must still be rejected. The
/// framebuffer's own open() validates only the bit depth, so this check lives
/// in Capture::open_scaled_ex and would otherwise vanish silently.
#[test]
fn a_requested_size_that_is_not_the_panel_is_rejected() {
    if !on_device() {
        eprintln!("skipping: no /dev/fb0");
        return;
    }
    let _g = LOCK.lock().unwrap_or_else(|e| e.into_inner());

    let e = match Capture::open(1920, 1080, 30, 2_000_000) {
        Ok(_) => panic!("1920x1080 is not the panel"),
        Err(e) => e,
    };
    assert!(
        e.to_string().contains("scaling is not supported"),
        "got: {e}"
    );
    // The failed open must have released the single-capture slot.
    Capture::open(720, 480, 30, 2_000_000).expect("the slot is free again after a failed open");
}
