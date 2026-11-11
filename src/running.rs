//! The record of the job or the operation that a pass runs now: `$VISION_HOME/running.json`.
//!
//! A pass can take minutes, because a job of the adapter (such as the proof) or an operation
//! runs inside it, and the ledger says nothing about it until it ends. So the executor writes
//! this small file with an atomic write when it starts a script for a job or an operation, and
//! removes it when the script ends. The app and `vision status` read it to say what runs.
//!
//! Only a holder of the lock of the tick runs a job or an operation, so the record names the
//! process that wrote it. A crash can leave the file behind without harm: a reader trusts the
//! record only while the process that wrote it still holds the lock of the tick, and the next
//! tick removes a record that it finds when it takes the lock. The ledger stays the evidence;
//! this file is only a view of the moment.

use std::fs::File;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::paths::Paths;
use crate::util::fs::{atomic_write, read_optional};
use crate::util::time;

/// What a pass runs now.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Running {
    /// `job`, `operation`, `reconcile`, or `read`.
    pub what: String,
    /// The job, such as `proof`, or the verb, such as `git.push`.
    pub name: String,
    pub ticket: String,
    /// When the script started, RFC 3339 UTC.
    pub started: String,
    /// The process that runs the script and holds the lock of the tick.
    pub pid: u32,
}

impl Running {
    /// The record in words, such as `the job proof` or `the operation git.push`.
    pub fn label(&self) -> String {
        match self.what.as_str() {
            "reconcile" => format!("the check of the operation {}", self.name),
            "read" => format!("the read {}", self.name),
            other => format!("the {other} {}", self.name),
        }
    }
}

/// The written record. Dropping it removes the file.
#[derive(Debug)]
pub struct Guard {
    path: Option<PathBuf>,
}

impl Drop for Guard {
    fn drop(&mut self) {
        if let Some(path) = self.path.take() {
            let _ = std::fs::remove_file(path);
        }
    }
}

/// Writes the record of a job or an operation that starts now. A failed write only means that
/// the screen says less: the work goes on, and the guard then removes nothing.
pub fn start(paths: &Paths, what: &str, name: &str, ticket: &str) -> Guard {
    let record = Running {
        what: what.to_string(),
        name: name.to_string(),
        ticket: ticket.to_string(),
        started: time::now_rfc3339(),
        pid: std::process::id(),
    };
    let path = paths.running();
    let written = serde_json::to_vec(&record)
        .map_err(anyhow::Error::from)
        .and_then(|bytes| atomic_write(&path, &bytes));
    Guard {
        path: written.ok().map(|()| path),
    }
}

/// Removes a record that a crash left. Only a holder of the lock of the tick calls it, before it
/// runs anything, so each record that it finds is stale.
pub fn clear(paths: &Paths) {
    let _ = std::fs::remove_file(paths.running());
}

/// The record, or `None` when no job and no operation runs: no file, a file that cannot be
/// read, or a stale record whose writer no longer holds the lock of the tick.
pub fn read(paths: &Paths) -> Option<Running> {
    let text = read_optional(&paths.running()).ok()??;
    let record: Running = serde_json::from_str(&text).ok()?;
    is_live(&paths.lock(), &record).then_some(record)
}

/// True when the writer of the record holds the lock of the tick now. The lock file names the
/// process that took the lock last. A different process, or a lock that nobody holds, means
/// that the record is stale. The test of the lock comes last, and only for a process that
/// lives, so that a reader almost never touches the lock.
fn is_live(lock: &Path, record: &Running) -> bool {
    let holder = read_optional(lock)
        .ok()
        .flatten()
        .and_then(|text| text.split_whitespace().next()?.parse::<u32>().ok());
    if holder != Some(record.pid) || !process_lives(record.pid) {
        return false;
    }
    lock_held(lock)
}

fn process_lives(pid: u32) -> bool {
    let Ok(pid) = libc::pid_t::try_from(pid) else {
        return false;
    };
    if pid <= 1 {
        return false;
    }
    // SAFETY: the signal 0 only tests that the process exists; nothing is sent.
    let result = unsafe { libc::kill(pid, 0) };
    result == 0 || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

/// True when a process holds the lock of the tick. A shared lock that this reader gets means
/// that nobody holds the exclusive lock.
fn lock_held(lock: &Path) -> bool {
    let Ok(file) = File::open(lock) else {
        return false;
    };
    match fs2::FileExt::try_lock_shared(&file) {
        Ok(()) => {
            let _ = fs2::FileExt::unlock(&file);
            false
        }
        Err(_) => true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lock::TickLock;

    #[test]
    fn the_record_is_there_while_the_work_runs_and_gone_after() {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::new(dir.path());
        let _lock = TickLock::try_acquire(&paths.lock()).unwrap().unwrap();
        assert_eq!(read(&paths), None);
        {
            let _guard = start(&paths, "job", "proof", "DEMO-12");
            let record = read(&paths).expect("the record while the job runs");
            assert_eq!(record.label(), "the job proof");
            assert_eq!(record.ticket, "DEMO-12");
            assert_eq!(record.pid, std::process::id());
        }
        assert!(!paths.running().exists());
        assert_eq!(read(&paths), None);
    }

    #[test]
    fn a_record_without_the_lock_is_stale() {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::new(dir.path());
        let guard = start(&paths, "operation", "git.push", "DEMO-12");
        // Nobody holds the lock of the tick: a crash left the record.
        assert!(paths.running().exists());
        assert_eq!(read(&paths), None);
        std::mem::forget(guard);
        // A different holder of the lock: the record is stale too.
        std::fs::write(paths.lock(), "1 2026-11-16T14:30:00Z\n").unwrap();
        assert_eq!(read(&paths), None);
        clear(&paths);
        assert!(!paths.running().exists());
    }

    #[test]
    fn a_broken_record_is_no_record() {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::new(dir.path());
        std::fs::write(paths.running(), "{").unwrap();
        assert_eq!(read(&paths), None);
    }
}
