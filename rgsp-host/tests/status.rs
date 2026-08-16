use rgsp_host::status::{Status, StatusWriter};

#[test]
fn status_lines_lead_with_what_the_user_needs() {
    assert_eq!(
        Status::AwaitingPairing {
            url: "http://192.168.1.50:47990/pin".into()
        }
        .line(),
        "Pair at http://192.168.1.50:47990/pin"
    );
    assert_eq!(
        Status::Ready {
            addr: "192.168.1.50".into()
        }
        .line(),
        "Ready - 192.168.1.50"
    );
    assert_eq!(
        Status::Connected {
            client: "Apple TV".into(),
            width: 720,
            height: 480,
            fps: 30
        }
        .line(),
        "Connected - Apple TV 720x480 30fps"
    );
    assert_eq!(Status::Stopped.line(), "Casting stopped");
}

#[test]
fn publish_never_blocks_when_the_fifo_has_no_reader() {
    // show2 may not be running. A status update must never wedge the daemon.
    //
    // A real FIFO, not a missing path: opening a *missing* path fails with
    // ENOENT immediately, which is the easy case and not the one that matters.
    // The case the daemon actually hits is show2 having exited while its FIFO
    // remains — an open(2) that blocks forever without O_NONBLOCK, and returns
    // ENXIO with it. Only a real FIFO with no reader exercises that.
    let path = std::env::temp_dir().join("rgsp-status-test.fifo");
    let _ = std::fs::remove_file(&path);
    let c_path = std::ffi::CString::new(path.as_os_str().as_encoded_bytes()).expect("path");
    assert_eq!(
        unsafe { libc::mkfifo(c_path.as_ptr(), 0o644) },
        0,
        "mkfifo failed: {}",
        std::io::Error::last_os_error()
    );

    // Published on another thread with a deadline, so that losing O_NONBLOCK
    // fails this test in two seconds instead of hanging the suite forever —
    // the failure mode it exists to catch is precisely an open(2) that never
    // returns.
    let (tx, rx) = std::sync::mpsc::channel();
    let publish_path = path.clone();
    std::thread::spawn(move || {
        StatusWriter::new(publish_path).publish(&Status::Starting);
        let _ = tx.send(());
    });

    let finished = rx.recv_timeout(std::time::Duration::from_secs(2));
    let _ = std::fs::remove_file(&path);
    assert!(
        finished.is_ok(),
        "publish blocked on a reader-less FIFO; show2 exiting would wedge the daemon"
    );
}

#[test]
fn publish_survives_a_missing_fifo() {
    // show2 may never have started at all.
    let path = std::env::temp_dir().join("rgsp-status-test-missing.fifo");
    let _ = std::fs::remove_file(&path);
    StatusWriter::new(path).publish(&Status::Starting);
}
