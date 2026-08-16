// Runs only on the device (needs /dev/fb0 and the Cedar libs).
// Skips cleanly elsewhere so the suite stays green on a laptop.
use rgsp_host::capture::Capture;
use std::sync::Mutex;

/// Capture is single-instance per process by design (see CAPTURE_OPEN), so
/// tests that open one must not run concurrently. cargo's harness is threaded
/// by default, and on the device — where neither test short-circuits — an
/// unserialised pair races for the guard and one fails.
static CAPTURE_TEST_LOCK: Mutex<()> = Mutex::new(());

#[test]
fn captures_annexb_frames_starting_with_a_keyframe() {
    if !std::path::Path::new("/dev/fb0").exists() {
        eprintln!("skipping: no /dev/fb0");
        return;
    }

    // Serialize with other Capture-opening tests.
    let _guard = CAPTURE_TEST_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());

    let mut cap = Capture::open(720, 480, 30, 2_000_000).expect("open");

    let frame = cap.next().expect("first frame");
    assert!(frame.data.len() > 4);
    assert_eq!(&frame.data[..4], &[0, 0, 0, 1], "annex-b start code");
    assert!(frame.is_keyframe, "first frame must be a keyframe");

    for _ in 0..30 {
        let f = cap.next().expect("frame");
        assert!(!f.data.is_empty());
    }

    cap.request_idr();
    let f = cap.next().expect("forced idr");
    assert!(f.is_keyframe);
}

#[test]
fn single_instance_guard_enforces_one_capture_per_process() {
    if !std::path::Path::new("/dev/fb0").exists() {
        eprintln!("skipping: no /dev/fb0");
        return;
    }

    // Serialize with other Capture-opening tests.
    let _guard = CAPTURE_TEST_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());

    // Open the first capture.
    let cap1 = Capture::open(720, 480, 30, 2_000_000).expect("first open");

    // Attempt to open a second capture while the first is held.
    match Capture::open(720, 480, 30, 2_000_000) {
        Err(e) => {
            assert!(
                e.to_string().contains("already open"),
                "error message should mention 'already open', got: {}",
                e
            );
        }
        Ok(_) => panic!("second open should fail while first is held"),
    }

    // Drop the first capture.
    drop(cap1);

    // Now we should be able to open a capture again.
    Capture::open(720, 480, 30, 2_000_000).expect("open should succeed after first is dropped");
}
