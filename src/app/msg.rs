//! The messages that reach the update, and the effects that it gives back for the runtime to
//! run. A message is a key, a tick of the clock, a change of the window, or the result of a
//! background thread. An effect starts a load, hands a job to the worker, or leaves.

use super::feed::{Batch, StreamLine};
use super::live::TicketData;
use super::model::{Local, Setup, SetupError, StartInfo};
use crate::pipeline::commands::PickRow;
use crate::pipeline::{DiffView, PlanView};

/// One key, as the update sees it. Ctrl-C arrives as `Cancel`, which acts as `q`, and as Esc
/// in the line of `t`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Key {
    Up,
    Down,
    Left,
    Right,
    Home,
    End,
    PageUp,
    PageDown,
    Backspace,
    Delete,
    Enter,
    Esc,
    Tab,
    Char(char),
    /// Ctrl-C.
    Cancel,
    Other,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Msg {
    Key(Key),
    /// A paste of text, when the terminal sends it as one piece.
    Paste(String),
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
    /// The data of the live view of a ticket.
    Ticket {
        key: String,
        data: Box<Result<TicketData, String>>,
    },
    /// New events of a stream file of the ticket of the live view.
    Stream {
        key: String,
        batch: Batch,
    },
    /// The plan of a ticket, or `None` when it has none yet.
    Plan {
        key: String,
        plan: Result<Option<PlanView>, String>,
    },
    /// The commits of a ticket that the forge does not have.
    Diff {
        key: String,
        diff: Result<DiffView, String>,
    },
    /// The events of one turn.
    TurnEvents {
        key: String,
        turn: String,
        events: Result<Vec<StreamLine>, String>,
    },
    /// The session that `vision open` gave to the person ended, and the app has the terminal
    /// again.
    SessionEnded {
        key: String,
        result: ChildResult,
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
    /// Load the data of the live view of a ticket.
    LoadTicket(String),
    /// Follow the stream files of a ticket in a thread, or stop that thread with `None`.
    Follow(Option<String>),
    LoadPlan(String),
    LoadDiff(String),
    LoadTurn {
        key: String,
        turn: String,
    },
    /// Give the terminal to `vision open KEY`, wait for it, and take the terminal again.
    OpenSession(String),
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

/// A change of the state. Each one runs as a child process of the same binary. Part A3 adds
/// `answer`, `approve`, `approve-plan`, and `promote`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Action {
    /// `vision take KEY`: start a ticket, or start a stopped ticket again.
    Take { key: String },
    /// `vision off`: the kill switch.
    Off,
    /// `vision on`.
    On,
    /// `vision stop KEY`.
    Stop { key: String },
    /// `vision retry KEY`.
    Retry { key: String },
    /// `vision tell KEY -- TEXT`.
    Tell { key: String, text: String },
}

impl Action {
    /// The arguments of the command.
    pub fn args(&self) -> Vec<String> {
        match self {
            Action::Take { key } => vec!["take".into(), key.clone()],
            Action::Off => vec!["off".into()],
            Action::On => vec!["on".into()],
            Action::Stop { key } => vec!["stop".into(), key.clone()],
            Action::Retry { key } => vec!["retry".into(), key.clone()],
            // `--` keeps a text that starts with `-` from looking like an option.
            Action::Tell { key, text } => {
                vec!["tell".into(), key.clone(), "--".into(), text.clone()]
            }
        }
    }

    /// The action in words, such as `the take of DEMO-12`.
    pub fn label(&self) -> String {
        match self {
            Action::Take { key } => format!("The take of {key}"),
            Action::Off => "The kill switch".into(),
            Action::On => "The switch on".into(),
            Action::Stop { key } => format!("The stop of {key}"),
            Action::Retry { key } => format!("The retry of {key}"),
            Action::Tell { key, .. } => format!("The message to {key}"),
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
