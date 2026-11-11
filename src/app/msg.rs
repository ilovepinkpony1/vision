//! The messages that reach the update, and the effects that it gives back for the runtime to
//! run. A message is a key, a tick of the clock, a change of the window, or the result of a
//! background thread. An effect starts a load, hands a job to the worker, or leaves.

use super::model::{Local, Setup, SetupError, StartInfo};
use crate::pipeline::commands::PickRow;

/// One key, as the update sees it. Ctrl-C arrives as `Char('q')`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Key {
    Up,
    Down,
    Enter,
    Esc,
    Tab,
    Char(char),
    Other,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Msg {
    Key(Key),
    Resize {
        width: u16,
        height: u16,
    },
    /// The clock moved. The runtime sets the time before each message.
    Tick,
    /// The first stage of the loader: the configuration and the adapter.
    Setup(Result<Setup, SetupError>),
    /// The local data: the tickets, the needs, the limits, and the heartbeat.
    Local(Result<Local, String>),
    /// The queue of the tracker.
    Queue(Result<Vec<PickRow>, String>),
    /// What the start card shows for a ticket.
    StartInfo {
        key: String,
        info: Result<StartInfo, String>,
    },
    /// The worker ended a job.
    JobDone(JobDone),
    /// The process got SIGTERM or SIGHUP. Only the runtime reads it.
    Signal(i32),
}

/// What the update asks the runtime to do.
#[derive(Clone, Debug, PartialEq)]
pub enum Effect {
    LoadSetup,
    LoadLocal,
    LoadQueue,
    LoadStartInfo(String),
    /// Start the worker thread. The update asks for it once.
    StartWorker,
    /// Hand a job to the worker. The update hands over one job at a time.
    Run(Job),
    Quit,
}

/// One job of the worker.
#[derive(Clone, Debug, PartialEq)]
pub struct Job {
    pub id: u64,
    pub kind: JobKind,
}

#[derive(Clone, Debug, PartialEq)]
pub enum JobKind {
    /// One pass: the child `vision tick`.
    Pass,
    /// A change of the state, through the command of today.
    Action(Action),
}

/// A change of the state. Each one runs as a child process of the same binary. Part A2 and
/// part A3 add `stop`, `retry`, `answer`, `approve`, `approve-plan`, `promote`, and `tell`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Action {
    /// `vision take KEY`: start a ticket, or start a stopped ticket again.
    Take { key: String },
    /// `vision off`: the kill switch.
    Off,
    /// `vision on`.
    On,
}

impl Action {
    /// The arguments of the command.
    pub fn args(&self) -> Vec<String> {
        match self {
            Action::Take { key } => vec!["take".into(), key.clone()],
            Action::Off => vec!["off".into()],
            Action::On => vec!["on".into()],
        }
    }

    /// The action in words, such as `the take of DEMO-12`.
    pub fn label(&self) -> String {
        match self {
            Action::Take { key } => format!("The take of {key}"),
            Action::Off => "The kill switch".into(),
            Action::On => "The switch on".into(),
        }
    }
}

impl JobKind {
    /// The arguments of the child process.
    pub fn args(&self) -> Vec<String> {
        match self {
            JobKind::Pass => vec!["tick".into()],
            JobKind::Action(action) => action.args(),
        }
    }
}

/// The end of a job.
#[derive(Clone, Debug, PartialEq)]
pub struct JobDone {
    pub id: u64,
    pub kind: JobKind,
    pub result: ChildResult,
}

/// What a child process left.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ChildResult {
    /// The exit code, or `None` when a signal ended it or it did not start.
    pub code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
    /// Why the child could not start.
    pub error: Option<String>,
}

impl ChildResult {
    pub fn ok(&self) -> bool {
        self.code == Some(0) && self.error.is_none()
    }

    /// The text that says why the child failed: its standard error, else its standard output,
    /// else why it did not start.
    pub fn failure(&self) -> String {
        if let Some(error) = &self.error {
            return error.clone();
        }
        let text = if self.stderr.trim().is_empty() {
            self.stdout.trim()
        } else {
            self.stderr.trim()
        };
        if text.is_empty() {
            match self.code {
                Some(code) => format!("the command ended with the exit code {code} and no text"),
                None => "a signal ended the command".to_string(),
            }
        } else {
            text.to_string()
        }
    }
}
