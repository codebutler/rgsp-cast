use std::io::{Error, ErrorKind, Result};
use std::path::{Path, PathBuf};

/// A PID file that a single daemon instance holds for its lifetime.
///
/// A pid file left behind by a killed process must not wedge the daemon
/// permanently, so an existing file whose PID is not running is reclaimed.
#[derive(Debug)]
pub struct PidFile {
    path: PathBuf,
}

impl PidFile {
    pub fn acquire(path: &Path) -> Result<PidFile> {
        if let Ok(existing) = std::fs::read_to_string(path) {
            if let Ok(pid) = existing.trim().parse::<i32>() {
                if process_is_alive(pid) {
                    return Err(Error::new(
                        ErrorKind::AlreadyExists,
                        format!("rgsp-host already running as pid {pid}"),
                    ));
                }
            }
        }
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(path, std::process::id().to_string())?;
        Ok(PidFile { path: path.to_path_buf() })
    }

    pub fn release(self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

fn process_is_alive(pid: i32) -> bool {
    // Signal 0 performs error checking without sending anything.
    unsafe { libc::kill(pid, 0) == 0 }
}
