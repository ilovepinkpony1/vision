//! The lock of the tick: an exclusive `flock` on `$VISION_HOME/lock`, taken with fs2.
//!
//! The lock lives as long as the [`TickLock`] value. The system releases it when the process
//! ends, so a crashed tick never leaves a stale lock.

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::Path;

use anyhow::{Context, Result};

use crate::util::time;

/// The held lock of the tick.
#[derive(Debug)]
pub struct TickLock {
    _file: File,
}

impl TickLock {
    /// Takes the lock without waiting. Gives `None` when a different process holds it.
    pub fn try_acquire(path: &Path) -> Result<Option<TickLock>> {
        let mut file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(path)
            .with_context(|| format!("could not open the lock file {}", path.display()))?;
        if let Err(e) = fs2::FileExt::try_lock_exclusive(&file) {
            if e.raw_os_error() == fs2::lock_contended_error().raw_os_error() {
                return Ok(None);
            }
            return Err(e).with_context(|| format!("could not lock {}", path.display()));
        }
        // The pid and the time help a person who looks at the file. The lock is the flock itself.
        file.set_len(0)?;
        writeln!(file, "{} {}", std::process::id(), time::now_rfc3339())?;
        Ok(Some(TickLock { _file: file }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_second_holder_gets_none_until_the_first_drops() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("lock");
        let first = TickLock::try_acquire(&path).unwrap();
        assert!(first.is_some());
        assert!(TickLock::try_acquire(&path).unwrap().is_none());
        drop(first);
        // A different test of this process can start a child at this moment. Between its fork
        // and its exec, that child still holds a copy of the lock file, so the lock can stay
        // held for a very short time after the drop. Allow for that.
        let mut second = None;
        for _ in 0..100 {
            second = TickLock::try_acquire(&path).unwrap();
            if second.is_some() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert!(second.is_some());
    }
}
