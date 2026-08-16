use rgsp_host::daemon::PidFile;
use std::io::ErrorKind;
use std::os::unix::fs as unix_fs;

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

#[test]
fn refuses_to_follow_a_symlink_at_the_pidfile_path() {
    // We run as root on the device and the path is world-writable, so a
    // symlink planted here must be refused, not followed.
    let dir = std::env::temp_dir().join("rgsp-pidtest-symlink");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let target = dir.join("victim");
    std::fs::write(&target, "original").unwrap();
    let path = dir.join("daemon.pid");
    unix_fs::symlink(&target, &path).unwrap();

    let result = PidFile::acquire(&path);
    assert!(result.is_err(), "must refuse a symlinked pidfile");
    assert_eq!(
        std::fs::read_to_string(&target).unwrap(),
        "original",
        "the symlink target must not be written through"
    );
    let _ = std::fs::remove_dir_all(&dir);
}
