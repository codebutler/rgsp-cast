use std::fs::OpenOptions;
use std::io::{Error, ErrorKind, Result, Seek, SeekFrom, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::os::unix::io::AsRawFd;
use std::path::{Path, PathBuf};

/// A lock held for the daemon's lifetime via flock(2).
///
/// Exclusion comes from an advisory file lock, not from inspecting the PID.
/// The kernel drops the lock automatically when the holder exits, so a pidfile
/// left behind by a killed process is simply an unlocked file — no staleness
/// to detect and no unlink/recreate race to lose. The PID is written for humans
/// and for launch.sh, which reads it to decide whether casting is active.
///
/// Security: Opened with O_NOFOLLOW to refuse symlinks planted in world-writable /tmp.
#[derive(Debug)]
pub struct PidFile {
    path: PathBuf,
    file: std::fs::File,
}

impl PidFile {
    /// Acquire an exclusive advisory lock on the pidfile.
    ///
    /// On success, holds the lock for the lifetime of the returned PidFile.
    /// Returns ErrorKind::AlreadyExists if another process holds the lock.
    pub fn acquire(path: &Path) -> Result<PidFile> {
        // Ensure parent directory exists.
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        // Open the file with O_NOFOLLOW (refuse symlinks in world-writable /tmp).
        // Use create(true) not create_new: an existing unlocked file is reusable now.
        let file = OpenOptions::new()
            .write(true)
            .create(true)
            .custom_flags(libc::O_NOFOLLOW)
            .mode(0o644)
            .open(path)?;

        // Try to acquire an exclusive non-blocking lock.
        let fd = file.as_raw_fd();
        let result = unsafe { libc::flock(fd, libc::LOCK_EX | libc::LOCK_NB) };

        if result != 0 {
            let err = std::io::Error::last_os_error();
            if err.kind() == ErrorKind::WouldBlock {
                return Err(Error::new(
                    ErrorKind::AlreadyExists,
                    format!("could not acquire lock on {}", path.display()),
                ));
            }
            return Err(err);
        }

        // Lock acquired. Truncate, seek to start, and write our PID.
        let mut file = file;
        file.set_len(0)?;
        file.seek(SeekFrom::Start(0))?;
        write!(file, "{}", std::process::id())?;
        file.flush()?;

        Ok(PidFile {
            path: path.to_path_buf(),
            file,
        })
    }

    /// Release the lock and remove the pidfile.
    ///
    /// The lock is dropped automatically when the file is closed, but we unlink
    /// while still holding it to ensure no race with a new acquire.
    pub fn release(self) {
        let _ = std::fs::remove_file(&self.path);
        drop(self.file);
    }
}
