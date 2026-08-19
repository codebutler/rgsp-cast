//! Starting and stopping the `rgsp-host` daemon from the UI.
//!
//! Readiness is the control socket accepting a connection, **not** the
//! pidfile appearing: `rgsp-host` writes its pidfile before the RPC server
//! binds (see `rgsp-host/src/main.rs`, "Step 1" vs. "Step 3"), so a caller
//! that trusts the pidfile can connect before anything is listening. The
//! pidfile still matters — it is the SIGTERM target and the flock that
//! enforces one daemon — it is just not the readiness signal.
//!
//! [`Service::start`] and [`Service::stop`] therefore both poll a raw Unix
//! socket connect rather than trusting the pidfile's mere existence: connect
//! succeeding is the only authoritative "the daemon is answering", and
//! connect failing is the only authoritative "it is not" (mirrors the same
//! design point documented on [`crate::rpc::Control`]).

use std::io::ErrorKind;
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::process::Command;
use std::time::{Duration, Instant};

use anyhow::Context;

/// How long [`Service::start`] retries connecting before giving up.
const DEFAULT_START_TIMEOUT: Duration = Duration::from_secs(5);

/// How long [`Service::stop`] waits for the socket to stop answering after
/// signaling, before giving up.
const DEFAULT_STOP_TIMEOUT: Duration = Duration::from_secs(15);

/// How often to retry the connect probe while polling for a readiness or
/// shutdown transition. Not exposed for injection: it only affects how
/// finely a wait is sliced, not how long tests have to wait for real, so
/// tests get their speedup entirely from shorter `start`/`stop` timeouts.
const POLL_INTERVAL: Duration = Duration::from_millis(20);

/// Starts and stops the `rgsp-host` daemon for a pak installed at `pak_dir`,
/// coordinating through `run_dir` (pidfile, control socket, log).
///
/// `run_dir` must agree with wherever the daemon actually puts those files —
/// on the device that's the daemon's hardcoded `/tmp/rgsp` — so this is a
/// parameter for testability, not a knob that changes daemon behavior.
pub struct Service {
    pak_dir: PathBuf,
    run_dir: PathBuf,
    start_timeout: Duration,
    stop_timeout: Duration,
}

impl Service {
    /// Production entry point: 5s to start, 15s to stop, matching the pak's
    /// own `launch.sh` wait budgets.
    pub fn new(pak_dir: PathBuf, run_dir: PathBuf) -> Service {
        Service::new_with_timeouts(pak_dir, run_dir, DEFAULT_START_TIMEOUT, DEFAULT_STOP_TIMEOUT)
    }

    /// As [`Service::new`], but with the start/stop wait budgets overridden.
    /// Exists so tests can use sub-second timeouts instead of waiting out
    /// the real 5s/15s production budgets; production code should always go
    /// through [`Service::new`].
    pub fn new_with_timeouts(
        pak_dir: PathBuf,
        run_dir: PathBuf,
        start_timeout: Duration,
        stop_timeout: Duration,
    ) -> Service {
        Service { pak_dir, run_dir, start_timeout, stop_timeout }
    }

    fn pidfile_path(&self) -> PathBuf {
        self.run_dir.join("daemon.pid")
    }

    fn socket_path(&self) -> PathBuf {
        self.run_dir.join("control.sock")
    }

    /// True if something is listening on the control socket right now. A
    /// plain connect probe, not a full [`crate::rpc::Control::connect`]:
    /// accepting the connection is what "answering" means here, and a probe
    /// this cheap is what makes tight polling loops affordable.
    fn socket_answers(&self) -> bool {
        UnixStream::connect(self.socket_path()).is_ok()
    }

    /// Spawns `rgsp-host` detached and waits for it to start answering on
    /// the control socket, up to `start_timeout`.
    ///
    /// Detachment uses the same subshell-backgrounding trick as
    /// `pak/launch.sh` (`( "$PAK_DIR/rgsp-host" >"$LOG" 2>&1 & )`) rather
    /// than `Command::spawn` directly: the daemon becomes a child of the
    /// short-lived `sh`, not of this process, so it is reparented to init
    /// once `sh` exits instead of staying a zombie-in-waiting under a
    /// long-running UI process that never calls `wait` on it.
    pub fn start(&self) -> anyhow::Result<()> {
        std::fs::create_dir_all(&self.run_dir)
            .with_context(|| format!("creating run dir {}", self.run_dir.display()))?;

        let bin = self.pak_dir.join("rgsp-host");
        let log = self.run_dir.join("daemon.log");
        let status = Command::new("sh")
            .arg("-c")
            .arg(r#""$1" >"$2" 2>&1 &"#)
            .arg("sh") // becomes $0 inside the -c script
            .arg(&bin) // $1
            .arg(&log) // $2
            .env("RGSP_RUN_DIR", &self.run_dir)
            .status()
            .with_context(|| format!("launching {}", bin.display()))?;
        if !status.success() {
            anyhow::bail!("failed to launch {}: shell exited with {status}", bin.display());
        }

        let deadline = Instant::now() + self.start_timeout;
        loop {
            if self.socket_answers() {
                return Ok(());
            }
            if Instant::now() >= deadline {
                anyhow::bail!(
                    "{} did not start accepting connections on {} within {:?}",
                    bin.display(),
                    self.socket_path().display(),
                    self.start_timeout,
                );
            }
            std::thread::sleep(POLL_INTERVAL);
        }
    }

    /// SIGTERMs the pid recorded in the pidfile, then waits for the control
    /// socket to stop answering, up to `stop_timeout`.
    ///
    /// A missing pidfile is treated as "already stopped" (`Ok(())`), not an
    /// error: there is nothing to signal. A pidfile that exists but doesn't
    /// parse as a pid *is* an error — that's corruption, not "not running",
    /// and silently ignoring it would leave a live daemon signaled never.
    pub fn stop(&self) -> anyhow::Result<()> {
        let pidfile = self.pidfile_path();
        let contents = match std::fs::read_to_string(&pidfile) {
            Ok(s) => s,
            Err(e) if e.kind() == ErrorKind::NotFound => return Ok(()),
            Err(e) => return Err(e).with_context(|| format!("reading {}", pidfile.display())),
        };
        let pid: libc::pid_t = contents
            .trim()
            .parse()
            .with_context(|| format!("{} does not contain a valid pid: {:?}", pidfile.display(), contents))?;

        // SAFETY: `kill(2)` with a caller-controlled pid and no other side
        // effects on our own memory; failure is reported through errno.
        if unsafe { libc::kill(pid, libc::SIGTERM) } != 0 {
            let err = std::io::Error::last_os_error();
            // ESRCH: no such process. The daemon this pidfile named is
            // already gone — fall through to confirm the socket agrees,
            // rather than treating a stale pidfile as a signaling failure.
            if err.raw_os_error() != Some(libc::ESRCH) {
                return Err(err).with_context(|| format!("sending SIGTERM to pid {pid}"));
            }
        }

        let deadline = Instant::now() + self.stop_timeout;
        loop {
            if !self.socket_answers() {
                return Ok(());
            }
            if Instant::now() >= deadline {
                anyhow::bail!(
                    "pid {pid} did not stop answering on {} within {:?}",
                    self.socket_path().display(),
                    self.stop_timeout,
                );
            }
            std::thread::sleep(POLL_INTERVAL);
        }
    }
}
