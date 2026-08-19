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
//!
//! The pid itself is likewise not trusted on its own: [`Service::stop`]
//! confirms it names the daemon by probing the pidfile's `flock(2)` (see
//! [`Service::pidfile_lock_is_held`]) before signaling, since a pid alone can
//! have been recycled by an unrelated process after a crashed daemon left
//! its pidfile behind.

use std::fs::OpenOptions;
use std::io::ErrorKind;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::os::unix::io::AsRawFd;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
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
    /// long-running UI process that never calls `wait` on it. That is the
    /// full extent of the detachment, matching `launch.sh`: there is no
    /// `setsid`, so the daemon keeps `sh`'s session and controlling
    /// terminal. Stdin is still redirected from `/dev/null` (only stdout and
    /// stderr go to the log) so the daemon can never block reading from a
    /// terminal that outlives this call.
    ///
    /// Checks that `bin` exists and is executable before spawning anything,
    /// so a broken install (missing or non-executable `rgsp-host`) fails
    /// immediately with its own message rather than looking identical to "it
    /// started but never bound the socket" for the full `start_timeout`.
    pub fn start(&self) -> anyhow::Result<()> {
        std::fs::create_dir_all(&self.run_dir)
            .with_context(|| format!("creating run dir {}", self.run_dir.display()))?;

        let bin = self.pak_dir.join("rgsp-host");
        let metadata = std::fs::metadata(&bin)
            .with_context(|| format!("{} does not exist or is not accessible", bin.display()))?;
        if !metadata.is_file() || metadata.permissions().mode() & 0o111 == 0 {
            anyhow::bail!("{} exists but is not executable", bin.display());
        }

        let log = self.run_dir.join("daemon.log");
        let status = Command::new("sh")
            .arg("-c")
            .arg(r#""$1" </dev/null >"$2" 2>&1 &"#)
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
    ///
    /// Before signaling, confirms the pid is actually the daemon's by
    /// probing the pidfile's flock (see [`Self::pidfile_lock_is_held`]): a
    /// pidfile is not proof of identity on its own; the pid it names can
    /// have been recycled by an unrelated process since the daemon that
    /// wrote it crashed. Signaling on the pid alone risks SIGTERMing that
    /// stranger.
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

        if !Self::pidfile_lock_is_held(&pidfile)? {
            // Nobody holds the lock: the pidfile is stale, whether because
            // the daemon it named already exited (the kernel drops the
            // flock the moment the holder exits or crashes) or because it
            // was never a daemon's pidfile in the first place. Either way
            // there is nothing here to signal, and `pid` is not trustworthy
            // enough to try.
            return Ok(());
        }

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

    /// True if some process currently holds the exclusive `flock(2)` on
    /// `pidfile` — i.e. the pid it names is the daemon's, not a stale
    /// leftover. False means *we* were able to grab the lock instead, which
    /// only happens when no daemon holds it.
    ///
    /// This mirrors `rgsp-host`'s own `PidFile::acquire`
    /// (`rgsp-host/src/daemon.rs`): the daemon takes this same flock for its
    /// entire lifetime and the kernel releases it automatically on exit or
    /// crash, so the flock — not the pid's mere liveness — is what
    /// authoritatively answers "is the process named here really the
    /// daemon". A pid on its own is not enough: PIDs get recycled, so a
    /// pidfile surviving a crash can end up naming an unrelated live
    /// process.
    fn pidfile_lock_is_held(pidfile: &Path) -> anyhow::Result<bool> {
        let file = match OpenOptions::new().read(true).custom_flags(libc::O_NOFOLLOW).open(pidfile) {
            Ok(f) => f,
            // Vanished between our caller reading its contents and this
            // probe: nothing to hold a lock on any more.
            Err(e) if e.kind() == ErrorKind::NotFound => return Ok(false),
            Err(e) => return Err(e).with_context(|| format!("opening {} to probe its lock", pidfile.display())),
        };

        // SAFETY: `flock(2)` on an fd we just opened ourselves; failure is
        // reported through errno, and no other side effects on our memory.
        if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0 {
            // We got it, so nobody else had it. Release immediately -- this
            // call only probes ownership, it doesn't claim the pidfile.
            // SAFETY: same fd, still open, still ours.
            unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_UN) };
            return Ok(false);
        }

        let err = std::io::Error::last_os_error();
        if err.kind() == ErrorKind::WouldBlock {
            return Ok(true);
        }
        Err(err).with_context(|| format!("probing the lock on {}", pidfile.display()))
    }
}
