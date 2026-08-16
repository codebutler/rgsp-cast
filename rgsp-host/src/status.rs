//! Status lines for show2.elf's on-screen overlay.
//!
//! The device has no console while a game is running, so the only way the user
//! learns what the daemon is doing is a line of text drawn by show2.elf, which
//! reads `TEXT:` commands from a FIFO.

use std::fs::OpenOptions;
use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::path::PathBuf;

/// Default FIFO show2.elf reads from (Task 11 launches show2 against this path).
pub const DEFAULT_FIFO: &str = "/tmp/show2.fifo";

#[derive(Clone, Debug)]
pub enum Status {
    Starting,
    AwaitingPairing { url: String },
    Ready { addr: String },
    Connected { client: String, width: u32, height: u32, fps: u32 },
    Stopped,
}

impl Status {
    pub fn line(&self) -> String {
        match self {
            Status::Starting => "Starting...".to_string(),
            Status::AwaitingPairing { url } => format!("Pair at {url}"),
            Status::Ready { addr } => format!("Ready - {addr}"),
            Status::Connected {
                client,
                width,
                height,
                fps,
            } => {
                format!("Connected - {client} {width}x{height} {fps}fps")
            }
            Status::Stopped => "Casting stopped".to_string(),
        }
    }
}

/// Publishes to show2.elf's FIFO. show2 may not be running, so every write is
/// non-blocking and failures are ignored - a status line is never worth
/// stalling the stream for.
#[derive(Clone)]
pub struct StatusWriter {
    fifo: PathBuf,
}

impl StatusWriter {
    pub fn new(fifo: PathBuf) -> StatusWriter {
        StatusWriter { fifo }
    }

    pub fn publish(&self, s: &Status) {
        let line = s.line();
        tracing::info!("status: {line}");
        // O_NONBLOCK on a FIFO with no reader fails with ENXIO rather than
        // blocking; on a missing path it fails with ENOENT. Both are expected
        // whenever show2 is not running, so neither is reported.
        let _ = OpenOptions::new()
            .write(true)
            .custom_flags(libc::O_NONBLOCK)
            .open(&self.fifo)
            .and_then(|mut f| writeln!(f, "TEXT:{line}"));
    }
}
