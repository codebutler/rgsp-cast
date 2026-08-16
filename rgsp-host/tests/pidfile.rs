use rgsp_host::daemon::PidFile;
use std::io::ErrorKind;

#[test]
fn second_acquire_fails_while_first_is_held() {
    let dir = std::env::temp_dir().join("rgsp-pidtest");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("daemon.pid");
    let _ = std::fs::remove_file(&path);

    let first = PidFile::acquire(&path).expect("first acquire works");
    let second = PidFile::acquire(&path);
    assert!(second.is_err());
    assert_eq!(second.unwrap_err().kind(), ErrorKind::AlreadyExists);

    first.release();
    // Once released, the slot is free again.
    PidFile::acquire(&path).expect("acquire after release works");
    let _ = std::fs::remove_file(&path);
}

#[test]
fn stale_pidfile_is_reclaimed() {
    let dir = std::env::temp_dir().join("rgsp-pidtest");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("stale.pid");
    // PID 999999 is above the default pid_max and cannot be running.
    std::fs::write(&path, "999999").unwrap();

    PidFile::acquire(&path).expect("stale pidfile is reclaimed");
    let _ = std::fs::remove_file(&path);
}
