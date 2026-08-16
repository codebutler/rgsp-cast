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
    let path = std::env::temp_dir().join("rgsp-status-test.fifo");
    let _ = std::fs::remove_file(&path);
    let w = StatusWriter::new(path.clone());
    w.publish(&Status::Starting); // must return promptly and not panic
}
