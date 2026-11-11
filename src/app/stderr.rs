//! The redirect of standard error. Library code prints warnings to standard error in some
//! paths, and such text would break the screen of the app. While the app owns the terminal,
//! file descriptor 2 points at a log file in the home folder; it comes back when the app
//! leaves, on each way out, a panic too.

use std::fs::OpenOptions;
use std::io;
use std::os::fd::AsRawFd;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicI32, Ordering};

use crate::paths::Paths;

/// The copy of the first file descriptor 2, while a redirect is active. -1 when none is.
static SAVED: AtomicI32 = AtomicI32::new(-1);

/// The log file of the app: `log/app.err` of the home. With no log folder (a home that is not
/// set up), the text goes to `/dev/null`.
pub fn log_path(paths: &Paths) -> PathBuf {
    if paths.log_dir().is_dir() {
        paths.log_dir().join("app.err")
    } else {
        PathBuf::from("/dev/null")
    }
}

/// An active redirect. Dropping it points file descriptor 2 back.
#[derive(Debug)]
pub struct Redirect {
    _private: (),
}

/// Points file descriptor 2 at the end of `path`.
pub fn redirect(path: &Path) -> io::Result<Redirect> {
    let file = OpenOptions::new().create(true).append(true).open(path)?;
    // SAFETY: plain calls on file descriptors that this process owns; no memory is shared.
    unsafe {
        let saved = libc::fcntl(2, libc::F_DUPFD_CLOEXEC, 3);
        if saved < 0 {
            return Err(io::Error::last_os_error());
        }
        if libc::dup2(file.as_raw_fd(), 2) < 0 {
            let error = io::Error::last_os_error();
            libc::close(saved);
            return Err(error);
        }
        let old = SAVED.swap(saved, Ordering::SeqCst);
        if old >= 0 {
            libc::close(old);
        }
    }
    Ok(Redirect { _private: () })
}

/// Points file descriptor 2 back at what it was. Safe to call more than once.
pub fn restore() {
    let saved = SAVED.swap(-1, Ordering::SeqCst);
    if saved >= 0 {
        // SAFETY: `saved` is a descriptor that `redirect` made and nothing else closes.
        unsafe {
            libc::dup2(saved, 2);
            libc::close(saved);
        }
    }
}

impl Drop for Redirect {
    fn drop(&mut self) {
        restore();
    }
}
