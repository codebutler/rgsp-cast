//! Integration tests for [`rgsp_ui::service::Service`] against a fake
//! `rgsp-host`: a shell script standing in for the daemon, following the
//! stub-daemon pattern in `tests/test_launch_sh.sh` (writes a pidfile,
//! optionally traps SIGTERM, and otherwise just sleeps). Stubs standing in
//! for a genuinely live daemon also hold an `flock(2)` on the pidfile for
//! their whole life, the way `rgsp-host`'s real `PidFile` does, since
//! `Service::stop` now checks that lock before signaling. One stub
//! (`start_succeeds_once_the_daemon_binds_the_socket`) uses `python3`
//! instead of plain `/bin/sh`, since binding a Unix socket needs more than
//! POSIX shell offers.
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
    // well-behaved stub in `tests/test_launch_sh.sh`. Holds an flock on the
    // pidfile for its whole life, like the real `rgsp-host` (see
    // `rgsp-host/src/daemon.rs`'s `PidFile`) — `stop()` now refuses to
    // signal a pid whose pidfile lock it can't confirm, so a stub standing
    // in for "the daemon is genuinely alive" has to hold it.
    let (pak_dir, run_dir) = temp_dirs("stop-signals-pid");
    write_stub(
        &pak_dir,
        r#"#!/bin/sh
PIDFILE="$RGSP_RUN_DIR/daemon.pid"
exec 9>"$PIDFILE"
flock -n 9 || exit 1
echo $$ >&9
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
PIDFILE="$RGSP_RUN_DIR/daemon.pid"
exec 9>"$PIDFILE"
flock -n 9 || exit 1
echo $$ >&9
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
    // Above the default pid_max; guaranteed not to be a live process. Nobody
    // holds the file's flock either (it was never opened by a daemon), so
    // this is caught by the lock probe before `stop()` would ever attempt to
    // signal a pid this far into unallocated territory.
    std::fs::write(run_dir.join("daemon.pid"), "999999").expect("write stale pidfile");

    let service = Service::new_with_timeouts(pak_dir, run_dir, Duration::from_secs(5), Duration::from_millis(300));
    service.stop().expect("stop() should tolerate a stale pidfile naming a pid that's already gone");
}

#[test]
fn stop_does_not_signal_a_live_pid_the_pidfile_lock_does_not_confirm() {
    // The recycled-pid scenario the flock check exists for: the pidfile
    // names a pid that is very much alive, but that process never touched
    // the pidfile's flock -- it is an innocent bystander that happens to
    // have been assigned the old daemon's pid after it crashed. `stop()`
    // must refuse to signal it.
    let (pak_dir, run_dir) = temp_dirs("stop-recycled-pid");

    let mut bystander = std::process::Command::new("sleep").arg("100").spawn().expect("spawn bystander process");
    let bystander_pid = bystander.id() as i32;
    std::fs::write(run_dir.join("daemon.pid"), bystander_pid.to_string()).expect("write pidfile naming the bystander");

    let service = Service::new_with_timeouts(pak_dir, run_dir, Duration::from_secs(5), Duration::from_millis(300));
    service.stop().expect("stop() should treat an unconfirmed pidfile as stale rather than error");

    // If stop() had actually sent SIGTERM, the bystander's default
    // disposition for it is to die; give a signal that was wrongly sent a
    // moment to land before checking. try_wait() returns Ok(None) while the
    // child is still running.
    std::thread::sleep(Duration::from_millis(200));
    assert!(
        bystander.try_wait().expect("poll bystander process").is_none(),
        "stop() must not signal a live pid whose pidfile lock it could not confirm"
    );

    let _ = bystander.kill();
    let _ = bystander.wait();
}

#[test]
fn start_succeeds_once_the_daemon_binds_the_socket() {
    // Unlike `start_fails_when_the_daemon_never_serves`, this stub actually
    // binds `run_dir/control.sock` (via `python3`, since a POSIX shell can't
    // bind a Unix socket itself) — without this test, deleting the
    // retry/poll loop entirely and failing after a single check would still
    // pass every other test in this file.
    let (pak_dir, run_dir) = temp_dirs("start-succeeds");
    write_stub(
        &pak_dir,
        r#"#!/bin/sh
echo $$ > "$RGSP_RUN_DIR/daemon.pid"
exec python3 -c "
import os, socket, signal
s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
s.bind(os.environ['RGSP_RUN_DIR'] + '/control.sock')
s.listen(5)
signal.pause()
"
"#,
    );

    let service = Service::new_with_timeouts(pak_dir, run_dir.clone(), Duration::from_secs(5), Duration::from_millis(300));
    let result = service.start();

    assert!(result.is_ok(), "start() must succeed once the daemon is actually listening, got {result:?}");

    let pid = wait_for_pidfile(&run_dir);
    kill_quietly(pid);
}

#[test]
fn start_fails_fast_when_the_binary_is_missing() {
    // No `rgsp-host` written into `pak_dir` at all -- a broken install.
    let (pak_dir, run_dir) = temp_dirs("start-missing-binary");

    let service = Service::new_with_timeouts(pak_dir, run_dir, Duration::from_secs(3), Duration::from_millis(50));
    let began = std::time::Instant::now();
    let result = service.start();
    let elapsed = began.elapsed();

    let err = result.expect_err("start() must fail when rgsp-host does not exist");
    assert!(
        elapsed < Duration::from_millis(500),
        "a missing binary should be reported immediately, not after waiting out the start timeout ({elapsed:?} elapsed against a 3s timeout)"
    );
    // Pins the *distinguishability* Important 2 asked for, not just failure:
    // this must not be the generic "never started accepting connections"
    // timeout message a broken install would otherwise look identical to.
    let message = format!("{err:#}");
    assert!(
        message.contains("does not exist"),
        "a missing binary's error should say so, not read like a stuck daemon: {message:?}"
    );
    assert!(
        !message.contains("did not start accepting connections"),
        "a missing binary must not be reported as the generic accept-timeout: {message:?}"
    );
}

#[test]
fn start_fails_fast_when_the_binary_is_not_executable() {
    let (pak_dir, run_dir) = temp_dirs("start-non-executable");
    let bin = pak_dir.join("rgsp-host");
    std::fs::write(&bin, "#!/bin/sh\necho hi\n").expect("write non-executable stub");
    // Deliberately no chmod +x -- this is the mode `install` leaves files in
    // by default, and a plausible botched-install state.

    let service = Service::new_with_timeouts(pak_dir, run_dir, Duration::from_secs(3), Duration::from_millis(50));
    let began = std::time::Instant::now();
    let result = service.start();
    let elapsed = began.elapsed();

    let err = result.expect_err("start() must fail when rgsp-host is not executable");
    assert!(
        elapsed < Duration::from_millis(500),
        "a non-executable binary should be reported immediately, not after waiting out the start timeout ({elapsed:?} elapsed against a 3s timeout)"
    );
    let message = format!("{err:#}");
    assert!(
        message.contains("not executable"),
        "a non-executable binary's error should say so, not read like a stuck daemon: {message:?}"
    );
    assert!(
        !message.contains("did not start accepting connections"),
        "a non-executable binary must not be reported as the generic accept-timeout: {message:?}"
    );
}
