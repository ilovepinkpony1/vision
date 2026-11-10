//! `turn.json`: the status that the supervisor writes (section 7.4), the lock that shows that a
//! supervisor lives, and the signals that stop a process group.
//!
//! A supervisor holds an exclusive lock on `supervisor.lock` in the turn folder for its whole
//! life. The system releases the lock when the process ends, even when it is killed, so the
//! lock is a better test of life than a pid: a pid can be reused, and a zombie still has one.

use std::fmt;
use std::fs::{File, OpenOptions};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::util::fs::{atomic_write, read_optional};
use crate::util::time;

/// The phase of a turn in `turn.json`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Phase {
    /// The runner wrote the turn, and the supervisor has not started yet.
    Queued,
    /// The supervisor runs, and `claude` is starting.
    Starting,
    /// `claude` runs.
    Running,
    /// The turn ended. `reason` says why.
    Ended,
}

/// Why a turn ended (section 7.4).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Reason {
    /// The session gave a `result` event and ended.
    Result,
    /// The session failed: no result, an error result, or `claude` could not start.
    Error,
    /// The time limit of the kind killed it.
    Timeout,
    /// The tools, the permission mode, or the MCP servers differed from the contract.
    Contract,
    /// A person or `vision off` killed it.
    Killed,
    /// The supervisor died before it wrote the end.
    Crash,
}

impl Reason {
    pub fn as_str(self) -> &'static str {
        match self {
            Reason::Result => "result",
            Reason::Error => "error",
            Reason::Timeout => "timeout",
            Reason::Contract => "contract",
            Reason::Killed => "killed",
            Reason::Crash => "crash",
        }
    }

    pub fn parse(text: &str) -> Option<Reason> {
        [
            Reason::Result,
            Reason::Error,
            Reason::Timeout,
            Reason::Contract,
            Reason::Killed,
            Reason::Crash,
        ]
        .into_iter()
        .find(|r| r.as_str() == text)
    }
}

impl fmt::Display for Reason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// The content of `turn.json`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TurnState {
    pub turn: String,
    pub state: Phase,
    /// The pid of the supervisor.
    #[serde(default)]
    pub pid: Option<u32>,
    /// The pid of `claude`, which is also the id of its process group.
    #[serde(default)]
    pub child_pid: Option<u32>,
    /// When this state was written.
    pub at: String,
    #[serde(default)]
    pub started: Option<String>,
    #[serde(default)]
    pub ended: Option<String>,
    #[serde(default)]
    pub exit_code: Option<i32>,
    #[serde(default)]
    pub reason: Option<Reason>,
    #[serde(default)]
    pub session: Option<String>,
    /// For people: what went wrong.
    #[serde(default)]
    pub detail: Option<String>,
}

impl TurnState {
    /// The state that the runner writes before it starts the supervisor.
    pub fn queued(turn: &str) -> TurnState {
        TurnState {
            turn: turn.to_string(),
            state: Phase::Queued,
            pid: None,
            child_pid: None,
            at: time::now_rfc3339(),
            started: None,
            ended: None,
            exit_code: None,
            reason: None,
            session: None,
            detail: None,
        }
    }

    /// This state, ended with `reason` now.
    pub fn ended(mut self, reason: Reason, detail: Option<String>) -> TurnState {
        let now = time::now_rfc3339();
        self.state = Phase::Ended;
        self.at.clone_from(&now);
        self.ended = Some(now);
        self.reason = Some(reason);
        if detail.is_some() {
            self.detail = detail;
        }
        self
    }

    pub fn is_ended(&self) -> bool {
        self.state == Phase::Ended
    }

    /// Reads `turn.json`, or gives `None` when it does not exist.
    pub fn read(path: &Path) -> Result<Option<TurnState>> {
        let Some(text) = read_optional(path)? else {
            return Ok(None);
        };
        let state = serde_json::from_str(&text)
            .with_context(|| format!("{} is not a valid turn status", path.display()))?;
        Ok(Some(state))
    }

    /// Writes `turn.json` with an atomic rename.
    pub fn write(&self, path: &Path) -> Result<()> {
        let mut text = serde_json::to_string_pretty(self)?;
        text.push('\n');
        atomic_write(path, text.as_bytes())
    }
}

/// The lock file of the supervisor of a turn.
pub fn lock_path(turn_dir: &Path) -> PathBuf {
    turn_dir.join("supervisor.lock")
}

/// The held lock of a supervisor. The system releases it when the process ends.
#[derive(Debug)]
pub struct SupervisorLock {
    _file: File,
}

/// Takes the lock of the supervisor, and waits up to one second for a short test of another
/// process to let it go. Gives `None` when another supervisor holds it.
pub fn take_lock(turn_dir: &Path) -> Result<Option<SupervisorLock>> {
    let path = lock_path(turn_dir);
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&path)
        .with_context(|| format!("could not open {}", path.display()))?;
    let until = Instant::now() + Duration::from_secs(1);
    loop {
        match fs2::FileExt::try_lock_exclusive(&file) {
            Ok(()) => return Ok(Some(SupervisorLock { _file: file })),
            Err(e) if e.raw_os_error() == fs2::lock_contended_error().raw_os_error() => {
                if Instant::now() >= until {
                    return Ok(None);
                }
                std::thread::sleep(Duration::from_millis(20));
            }
            Err(e) => return Err(e).with_context(|| format!("could not lock {}", path.display())),
        }
    }
}

/// True when a supervisor holds the lock of this turn folder.
pub fn supervisor_alive(turn_dir: &Path) -> bool {
    let Ok(file) = File::open(lock_path(turn_dir)) else {
        return false;
    };
    match fs2::FileExt::try_lock_shared(&file) {
        Ok(()) => {
            let _ = fs2::FileExt::unlock(&file);
            false
        }
        // Contended, or a failure: count it as alive, so that nothing kills or collects it.
        Err(_) => true,
    }
}

/// Sends `signal` to a process. False when it does not exist.
pub fn signal_pid(pid: u32, signal: libc::c_int) -> bool {
    let Ok(pid) = libc::pid_t::try_from(pid) else {
        return false;
    };
    if pid <= 1 {
        return false;
    }
    // SAFETY: kill only sends a signal to the process with this pid.
    unsafe { libc::kill(pid, signal) == 0 }
}

/// Sends `signal` to each process of a process group. False when the group is empty.
pub fn signal_group(pgid: u32, signal: libc::c_int) -> bool {
    let Ok(pgid) = libc::pid_t::try_from(pgid) else {
        return false;
    };
    if pgid <= 1 {
        return false;
    }
    // SAFETY: killpg only sends a signal to the processes of this group.
    unsafe { libc::killpg(pgid, signal) == 0 }
}

/// True when a process group still has a member.
pub fn group_alive(pgid: u32) -> bool {
    signal_group(pgid, 0)
}

/// Kills a process group that outlived its supervisor: TERM, a short wait, then KILL.
pub fn kill_orphan_group(pgid: u32) {
    if !group_alive(pgid) {
        return;
    }
    signal_group(pgid, libc::SIGTERM);
    let until = Instant::now() + Duration::from_millis(500);
    while group_alive(pgid) && Instant::now() < until {
        std::thread::sleep(Duration::from_millis(20));
    }
    signal_group(pgid, libc::SIGKILL);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_state_round_trips_and_ends() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("turn.json");
        assert_eq!(TurnState::read(&path).unwrap(), None);
        let state = TurnState::queued("t1");
        state.write(&path).unwrap();
        assert_eq!(TurnState::read(&path).unwrap(), Some(state.clone()));
        let ended = state.ended(Reason::Killed, Some("by test".into()));
        assert!(ended.is_ended());
        assert_eq!(ended.reason, Some(Reason::Killed));
        let text = serde_json::to_string(&ended).unwrap();
        assert!(text.contains("\"reason\":\"killed\""), "{text}");
        assert_eq!(Reason::parse("crash"), Some(Reason::Crash));
    }

    #[test]
    fn the_lock_shows_life_and_is_free_after_the_holder_drops() {
        let dir = tempfile::tempdir().unwrap();
        assert!(!supervisor_alive(dir.path()), "no lock file means dead");
        let held = take_lock(dir.path()).unwrap().unwrap();
        assert!(supervisor_alive(dir.path()));
        drop(held);
        assert!(!supervisor_alive(dir.path()));
    }

    #[test]
    fn impossible_pids_get_no_signal() {
        assert!(!signal_pid(0, 0));
        assert!(!signal_pid(1, 0));
        assert!(!signal_group(0, 0));
        assert!(!group_alive(u32::MAX));
    }
}
