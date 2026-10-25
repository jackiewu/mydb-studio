//! Whether the app is running, for the MCP server (a separate process).
//!
//! The app holds an exclusive lock on [`crate::RUNNING_FLAG`] while it runs.
//! The OS drops the lock when the process ends, even on a crash or force
//! quit, so a leftover file or a reused process id can't pass for a running
//! app.

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::Path;

/// The running flag, locked while this is kept (the app keeps it for its
/// whole run).
pub struct Held(#[allow(dead_code)] File);

/// Creates the running flag and locks it, waiting while another process
/// holds it.
pub fn hold(path: &Path) -> std::io::Result<Held> {
    // Not truncated before it is ours: another app may still hold it.
    let mut file = OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(false)
        .open(path)?;
    imp::lock(&file)?;
    file.set_len(0)?;
    write!(file, "{}", std::process::id())?;
    Ok(Held(file))
}

/// Whether some process holds the running flag at `path`.
pub fn is_held(path: &Path) -> bool {
    match File::open(path) {
        Ok(file) => imp::held_by_other(&file),
        Err(_) => false,
    }
}

#[cfg(unix)]
mod imp {
    use std::fs::File;
    use std::os::fd::AsRawFd;

    pub fn lock(file: &File) -> std::io::Result<()> {
        loop {
            // SAFETY: flock on a file descriptor we own.
            if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX) } == 0 {
                return Ok(());
            }
            let err = std::io::Error::last_os_error();
            if err.kind() != std::io::ErrorKind::Interrupted {
                return Err(err);
            }
        }
    }

    pub fn held_by_other(file: &File) -> bool {
        let fd = file.as_raw_fd();
        // SAFETY: flock on a file descriptor we own; released right away.
        if unsafe { libc::flock(fd, libc::LOCK_SH | libc::LOCK_NB) } == 0 {
            unsafe { libc::flock(fd, libc::LOCK_UN) };
            false
        } else {
            std::io::Error::last_os_error().raw_os_error() == Some(libc::EWOULDBLOCK)
        }
    }
}

#[cfg(not(unix))]
mod imp {
    use std::fs::File;

    pub fn lock(_file: &File) -> std::io::Result<()> {
        Ok(())
    }

    /// Without file locks, the flag being there is all there is to go on.
    pub fn held_by_other(_file: &File) -> bool {
        true
    }
}

#[cfg(all(test, unix))]
mod tests {
    #[test]
    fn held_while_open_and_free_after() {
        let dir = std::env::temp_dir().join(format!("mydb-running-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("running.flag");
        assert!(!super::is_held(&path));

        // A leftover file from a crash isn't held.
        std::fs::write(&path, "12345").unwrap();
        assert!(!super::is_held(&path));

        // flock locks belong to the open file, so a second open in this
        // process stands in for another process.
        let held = super::hold(&path).unwrap();
        assert!(super::is_held(&path));
        drop(held);
        assert!(!super::is_held(&path));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
