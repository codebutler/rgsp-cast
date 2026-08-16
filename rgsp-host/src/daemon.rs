use std::fs::OpenOptions;
use std::io::{Error, ErrorKind, Read, Result, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};

/// A PID file that a single daemon instance holds for its lifetime.
///
/// A pid file left behind by a killed process must not wedge the daemon
/// permanently, so an existing file whose PID is not running is reclaimed.
///
/// Security: Created with O_EXCL (atomic creation, prevents TOCTOU) and O_NOFOLLOW
/// (refuse symlinks in world-writable /tmp). Existing files opened with O_NOFOLLOW
/// to prevent symlink-follow arbitrary write as root.
#[derive(Debug)]
pub struct PidFile {
    path: PathBuf,
}

impl PidFile {
    pub fn acquire(path: &Path) -> Result<PidFile> {
        // Ensure parent directory exists.
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        // Try to create atomically. O_EXCL ensures only one process wins the race.
        match create_exclusive(path) {
            Ok(mut file) => {
                // We won the race. Write our PID and return.
                write!(file, "{}", std::process::id())?;
                return Ok(PidFile {
                    path: path.to_path_buf(),
                });
            }
            Err(e) if e.kind() == ErrorKind::AlreadyExists => {
                // File exists. Check if the holder is alive.
                match read_pid_no_follow(path) {
                    Ok(pid) => {
                        if process_is_alive(pid) {
                            return Err(Error::new(
                                ErrorKind::AlreadyExists,
                                format!("rgsp-host already running as pid {pid}"),
                            ));
                        }
                    }
                    Err(e) if e.raw_os_error() == Some(libc::ELOOP) => {
                        // Path is a symlink. Refuse it.
                        return Err(Error::new(
                            ErrorKind::PermissionDenied,
                            "pidfile path is a symlink; refusing to follow",
                        ));
                    }
                    Err(_) => {
                        // Unparseable or unreadable file counts as stale.
                    }
                }

                // The holder is stale. Try once to unlink and recreate.
                let _ = std::fs::remove_file(path);
                match create_exclusive(path) {
                    Ok(mut file) => {
                        write!(file, "{}", std::process::id())?;
                        return Ok(PidFile {
                            path: path.to_path_buf(),
                        });
                    }
                    Err(e) => {
                        // Retry also hit AlreadyExists — another process won the race.
                        if e.kind() == ErrorKind::AlreadyExists {
                            return Err(Error::new(
                                ErrorKind::AlreadyExists,
                                "pidfile claimed by concurrent process",
                            ));
                        }
                        return Err(e);
                    }
                }
            }
            Err(e) => Err(e),
        }
    }

    pub fn release(self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

/// Create a file atomically with O_EXCL and O_NOFOLLOW.
/// Returns AlreadyExists if file already exists (even as a symlink).
fn create_exclusive(path: &Path) -> Result<std::fs::File> {
    OpenOptions::new()
        .write(true)
        .create_new(true)
        .custom_flags(libc::O_NOFOLLOW)
        .mode(0o644)
        .open(path)
}

/// Read PID from file, refusing to follow symlinks.
fn read_pid_no_follow(path: &Path) -> Result<i32> {
    let mut file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)?;

    let mut buf = String::new();
    file.read_to_string(&mut buf)?;
    buf.trim().parse::<i32>().map_err(|_| {
        Error::new(
            ErrorKind::InvalidData,
            "pidfile contains non-numeric data",
        )
    })
}

fn process_is_alive(pid: i32) -> bool {
    // Signal 0 performs error checking without sending anything.
    unsafe { libc::kill(pid, 0) == 0 }
}
