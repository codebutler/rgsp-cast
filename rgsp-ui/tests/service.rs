//! Integration tests for [`rgsp_ui::service::Service`] against a fake
//! `rgsp-host`: a shell script standing in for the daemon, following the
//! stub-daemon pattern in `tests/test_launch_sh.sh` (writes a pidfile,
//! optionally traps SIGTERM, and otherwise just sleeps).
//!
//! These use `Service::new_with_timeouts` rather than the production
//! `Service::new` so the 5s/15s production waits don't make the suite slow;
//! the timeout values themselves are exercised by the "ignores SIGTERM"
//! test below, which relies on the short stop timeout actually elapsing.

use rgsp_ui::service::Service;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// A fresh `<tmp>/rgsp-ui-service-test-<pid>-<n>/{pak,run}` pair, isolated
/// per test and per call within a test (the counter) so parallel `cargo
/// test` runs and repeated calls in one test never collide.
fn temp_dirs(name: &str) -> (PathBuf, PathBuf) {
    use std::sync::atomic::{AtomicU32, Ordering};
    static COUNTER: AtomicU32 = AtomicU32::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let base = std::env::temp_dir().join(format!("rgsp-ui-service-test-{name}-{}-{n}", std::process::id()));
    let pak_dir = base.join("pak");
    let run_dir = base.join("run");
    std::fs::create_dir_all(&pak_dir).expect("create pak dir");
    std::fs::create_dir_all(&run_dir).expect("create run dir");
    (pak_dir, run_dir)
}

/// Writes an executable `rgsp-host` shell script into `pak_dir`.
fn write_stub(pak_dir: &Path, script: &str) {
    let path = pak_dir.join("rgsp-host");
    std::fs::write(&path, script).expect("write stub");
    let mut perms = std::fs::metadata(&path).expect("stat stub").permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(&path, perms).expect("chmod stub");
}

/// Reads the pid out of `run_dir/daemon.pid`, retrying briefly since the
/// stub daemon writes it asynchronously after `Service::start` spawns it.
fn wait_for_pidfile(run_dir: &Path) -> i32 {
    let path = run_dir.join("daemon.pid");
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    loop {
        if let Ok(s) = std::fs::read_to_string(&path)
            && let Ok(pid) = s.trim().parse()
        {
            return pid;
        }
        if std::time::Instant::now() >= deadline {
            panic!("stub daemon never wrote a pid to {}", path.display());
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn kill_quietly(pid: i32) {
    unsafe {
        libc::kill(pid, libc::SIGKILL);
    }
}

#[test]
fn start_fails_when_the_daemon_never_serves() {
    // Writes its pidfile immediately, then just sleeps — it never binds
    // `run_dir/control.sock`. This is the whole point of the design: a UI
    // that trusted the pidfile alone would report this as running.
    let (pak_dir, run_dir) = temp_dirs("start-never-serves");
    write_stub(
        &pak_dir,
        r#"#!/bin/sh
echo $$ > "$RGSP_RUN_DIR/daemon.pid"
while :; do sleep 1; done
"#,
    );

    let service = Service::new_with_timeouts(pak_dir, run_dir.clone(), Duration::from_millis(500), Duration::from_secs(15));
    let result = service.start();

    assert!(result.is_err(), "start() must fail when the socket never answers, got {result:?}");

    // Prove the daemon really did run (and really did write the pidfile) so
    // this failure is attributable to the missing socket, not a spawn
    // failure that would make the test pass for the wrong reason.
    let pid = wait_for_pidfile(&run_dir);
    kill_quietly(pid);
}

/// Reads `run_dir/term_received.pid`, retrying briefly: a POSIX shell only
/// runs a trap once its current foreground command (here, `sleep 0.1`)
/// returns, so the file can land up to ~100ms after the signal — well after
/// `stop()` itself has already returned, since its readiness wait has
/// nothing to poll once the stub's socket was never bound.
fn wait_for_term_marker(run_dir: &Path) -> i32 {
    let path = run_dir.join("term_received.pid");
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    loop {
        if let Ok(s) = std::fs::read_to_string(&path)
            && let Ok(pid) = s.trim().parse()
        {
            return pid;
        }
        if std::time::Instant::now() >= deadline {
            panic!("stub daemon never recorded a signal at {}", path.display());
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn stop_signals_the_pid_from_the_pidfile() {
    // Traps SIGTERM, records that it was this process (`$$`) that received
    // it, then removes its own pidfile and exits — mirroring the
    // well-behaved stub in `tests/test_launch_sh.sh`.
    let (pak_dir, run_dir) = temp_dirs("stop-signals-pid");
    write_stub(
        &pak_dir,
        r#"#!/bin/sh
PIDFILE="$RGSP_RUN_DIR/daemon.pid"
echo $$ > "$PIDFILE"
trap 'echo "$$" > "$RGSP_RUN_DIR/term_received.pid"; rm -f "$PIDFILE"; exit 0' TERM
while :; do sleep 0.1; done
"#,
    );

    // Spawn directly rather than via `Service::start` — start() would fail
    // here too, since this stub never binds the socket either. What's under
    // test is stop()'s signaling, not start()'s readiness poll.
    let mut child = std::process::Command::new(pak_dir.join("rgsp-host"))
        .env("RGSP_RUN_DIR", &run_dir)
        .spawn()
        .expect("spawn stub daemon");
    let started_pid = wait_for_pidfile(&run_dir);
    assert_eq!(started_pid, child.id() as i32, "test setup: pidfile should hold the spawned child's own pid");

    let service = Service::new_with_timeouts(pak_dir, run_dir.clone(), Duration::from_secs(5), Duration::from_millis(500));
    service.stop().expect("stop should succeed against a well-behaved daemon");

    let signaled = wait_for_term_marker(&run_dir);
    assert_eq!(signaled, started_pid, "stop() must SIGTERM exactly the pid named in the pidfile");

    let _ = child.wait();
}

#[test]
fn stop_times_out_against_a_daemon_that_ignores_sigterm() {
    let (pak_dir, run_dir) = temp_dirs("stop-ignores-sigterm");
    write_stub(
        &pak_dir,
        r#"#!/bin/sh
echo $$ > "$RGSP_RUN_DIR/daemon.pid"
trap '' TERM
while :; do sleep 0.1; done
"#,
    );

    let mut child = std::process::Command::new(pak_dir.join("rgsp-host"))
        .env("RGSP_RUN_DIR", &run_dir)
        .spawn()
        .expect("spawn stub daemon");
    let pid = wait_for_pidfile(&run_dir);

    // A shell stub can't bind a Unix socket, but `stop()`'s readiness wait
    // needs *something* answering `run_dir/control.sock` to time out
    // against — otherwise the wait loop exits on its very first check, for
    // the wrong reason (nothing there at all, not "the daemon won't die").
    // A bound-but-never-accepted listener still makes `connect` succeed, so
    // this reproduces "the daemon is still up" without the stub needing to
    // speak the control protocol.
    let _listener = std::os::unix::net::UnixListener::bind(run_dir.join("control.sock")).expect("bind fake socket");

    let service = Service::new_with_timeouts(pak_dir, run_dir.clone(), Duration::from_secs(5), Duration::from_millis(300));
    let result = service.stop();

    assert!(result.is_err(), "stop() must fail rather than silently report success against a stuck daemon");

    kill_quietly(pid);
    let _ = child.wait();
}

#[test]
fn stop_is_a_no_op_when_there_is_no_pidfile() {
    // Nothing has ever run here — no `daemon.pid` at all.
    let (pak_dir, run_dir) = temp_dirs("stop-no-pidfile");

    let service = Service::new_with_timeouts(pak_dir, run_dir, Duration::from_secs(5), Duration::from_millis(300));
    service.stop().expect("stop() against an already-stopped service should succeed");
}

#[test]
fn stop_fails_on_a_malformed_pidfile() {
    let (pak_dir, run_dir) = temp_dirs("stop-malformed-pidfile");
    std::fs::write(run_dir.join("daemon.pid"), "not-a-pid").expect("write malformed pidfile");

    let service = Service::new_with_timeouts(pak_dir, run_dir, Duration::from_secs(5), Duration::from_millis(300));
    let result = service.stop();

    assert!(result.is_err(), "a pidfile that doesn't contain a pid must not be silently ignored");
}

#[test]
fn stop_succeeds_on_a_stale_pidfile_whose_process_is_gone() {
    let (pak_dir, run_dir) = temp_dirs("stop-stale-pidfile");
    // Above the default pid_max; guaranteed not to be a live process.
    std::fs::write(run_dir.join("daemon.pid"), "999999").expect("write stale pidfile");

    let service = Service::new_with_timeouts(pak_dir, run_dir, Duration::from_secs(5), Duration::from_millis(300));
    service.stop().expect("stop() should tolerate signaling a pid that's already gone");
}
