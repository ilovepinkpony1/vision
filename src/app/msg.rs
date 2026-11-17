//! The messages that reach the update, and the effects that it gives back for the runtime to
//! run. A message is a key, a click or a step of the wheel, a tick of the clock, a change of the
//! window, or the result of a background thread. An effect starts a load, hands a job to the
//! worker, gives the terminal to a child, writes the drafts file of a question form, or leaves.

use super::chain::{Check, CheckFacts};
use super::feed::{Batch, StreamLine};
use super::live::TicketData;
use super::model::{ContinueView, Local, Setup, SetupError, StartInfo};
use super::model_table::ModelLine;
use super::needs::{Draft, NeedItem};
use super::picture::Picture;
use super::stats::StatsData;
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

/// A press of the left button of the mouse, or one step of its wheel, at a cell of the window.
/// The thread of the keys sends no message for a release, a move, or another button.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Mouse {
    pub kind: MouseKind,
    pub column: u16,
    pub row: u16,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MouseKind {
    /// A press of the left button.
    Click,
    WheelUp,
    WheelDown,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Msg {
    Key(Key),
    /// A click or a step of the wheel. It acts on the parts of the last frame.
    Mouse(Mouse),
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
    /// The loader found no change since the local data that it gave last.
    LocalSame,
    /// The queue of the tracker.
    Queue(Result<Vec<PickRow>, String>),
    /// What the start card shows for a ticket.
    StartInfo {
        key: String,
        info: Result<StartInfo, String>,
    },
    /// What the continue card shows for a stopped or paused ticket: what the rules found.
    Resume {
        key: String,
        view: Box<Result<ContinueView, String>>,
    },
    /// The data of the live view of a ticket.
    Ticket {
        key: String,
        data: Box<Result<TicketData, String>>,
    },
    /// The loader found no change since the data of the live view of this ticket that it gave
    /// last.
    TicketSame {
        key: String,
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
    /// The review of a proposal of the Curator ended, and the app has the terminal again.
    ReviewEnded {
        proposal: String,
        result: ChildResult,
    },
    /// The things that wait for the person, from the load with this number.
    Needs {
        load: u64,
        items: Box<Result<Vec<NeedItem>, String>>,
    },
    /// What the read of a check of a chain found.
    Checked {
        chain: u64,
        facts: Box<Result<CheckFacts, String>>,
    },
    /// The rows of the screen Models.
    Models(Box<Result<Vec<ModelLine>, String>>),
    /// The numbers of the screen Stats, for the last 7 days or for all time.
    Stats {
        week: bool,
        data: Box<Result<StatsData, String>>,
    },
    /// The picture of the loader from `loader.ans`, or `None` for the android.
    Picture(Box<Option<Picture>>),
    /// The worker ended a job.
    JobDone(JobDone),
    /// A write of a drafts file failed: why.
    DraftsNotKept(String),
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
    /// Read what the continue card shows for a ticket: the call of observe of its rules.
    LoadResume(String),
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
    /// Give the terminal to `vision memory review --proposal NAME` in the same way.
    OpenReview(String),
    /// Read the things that wait for the person. The number names the load.
    LoadNeeds(u64),
    /// Read the facts of the check of a chain.
    LoadCheck {
        chain: u64,
        check: Check,
    },
    LoadModels,
    LoadStats {
        week: bool,
    },
    /// Read `loader.ans` of the home folder.
    LoadPicture,
    /// Write the drafts of the question forms of a ticket in place of its drafts file. The
    /// runtime writes it at once, so that a later send or a later load reads the same drafts.
    SaveDrafts {
        ticket: String,
        drafts: Vec<(String, Draft)>,
    },
    /// Remove the drafts of these questions from the drafts file of a ticket, such as after
    /// the send of their set.
    DropDrafts {
        ticket: String,
        ids: Vec<String>,
    },
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

/// A change of the state. Each one runs as a child process of the same binary.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Action {
    /// `vision take KEY`: start a ticket, or start a stopped ticket again.
    Take { key: String },
    /// `vision take KEY --step STEP`: continue a stopped ticket at a step.
    TakeAt { key: String, step: String },
    /// `vision off`: the kill switch.
    Off,
    /// `vision on`.
    On,
    /// `vision stop KEY`.
    Stop { key: String },
    /// `vision retry KEY`.
    Retry { key: String },
    /// `vision retry KEY --step STEP`: continue a paused ticket at a step.
    RetryAt { key: String, step: String },
    /// `vision tell KEY -- TEXT`.
    Tell { key: String, text: String },
    /// `vision answer ID --choice N | --text=T | --needless WHERE [--why=W]`.
    Answer {
        id: String,
        given: Given,
        /// The reason of the person. Empty for none.
        why: String,
    },
    /// `vision answer --file FILE`: the answers of one question form, each one or none. FILE is
    /// in the folder of the ticket: the worker writes it before the command runs and removes it
    /// after, and the command runs in the home folder, so the path of its arguments finds it.
    AnswerSet {
        ticket: String,
        answers: Vec<(String, Draft)>,
    },
    /// `vision approve-plan KEY HASH`.
    ApprovePlan { key: String, hash: String },
    /// `vision approve KEY VERB COMMIT --op ID`, with `-` for a ticket with no commit. The id
    /// names the one operation that the card showed, so that the command itself, under the
    /// lock of the tick, refuses a write that changed.
    Approve {
        key: String,
        verb: String,
        commit: String,
        op: String,
    },
    /// `vision promote VERB`: one step up the ladder.
    Promote { verb: String },
    /// `vision model set AGENT MODEL --effort E`.
    ModelSet {
        agent: String,
        model: String,
        effort: String,
    },
    /// `vision model reset AGENT`.
    ModelReset { agent: String },
    /// `vision needs --read ID`: marks a notice as read.
    ReadNotice { id: String },
}

/// The answer of `vision answer`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Given {
    /// The number of a choice, from 1, as `vision needs` shows it.
    Choice(usize),
    /// An answer in the words of the person.
    Text(String),
    /// The card was needless: where the answer already was (`code`, `ticket`, `memory`, or
    /// `no-matter`).
    Needless(String),
}

impl Action {
    /// The arguments of the command. A text goes as `--text=T` and a reason as `--why=W`, so
    /// that a text that starts with `-` never looks like an option.
    pub fn args(&self) -> Vec<String> {
        match self {
            Action::Take { key } => vec!["take".into(), key.clone()],
            Action::TakeAt { key, step } => {
                vec!["take".into(), key.clone(), "--step".into(), step.clone()]
            }
            Action::Off => vec!["off".into()],
            Action::On => vec!["on".into()],
            Action::Stop { key } => vec!["stop".into(), key.clone()],
            Action::Retry { key } => vec!["retry".into(), key.clone()],
            Action::RetryAt { key, step } => {
                vec!["retry".into(), key.clone(), "--step".into(), step.clone()]
            }
            // `--` keeps a text that starts with `-` from looking like an option.
            Action::Tell { key, text } => {
                vec!["tell".into(), key.clone(), "--".into(), text.clone()]
            }
            Action::Answer { id, given, why } => {
                let mut args = vec!["answer".to_string(), id.clone()];
                match given {
                    Given::Choice(n) => args.extend(["--choice".to_string(), n.to_string()]),
                    Given::Text(text) => args.push(format!("--text={text}")),
                    Given::Needless(place) => {
                        args.extend(["--needless".to_string(), place.clone()]);
                        return args;
                    }
                }
                if !why.trim().is_empty() {
                    args.push(format!("--why={}", why.trim()));
                }
                args
            }
            Action::AnswerSet { ticket, .. } => {
                vec!["answer".into(), "--file".into(), send_path(ticket)]
            }
            Action::ApprovePlan { key, hash } => {
                vec!["approve-plan".into(), key.clone(), hash.clone()]
            }
            Action::Approve {
                key,
                verb,
                commit,
                op,
            } => vec![
                "approve".into(),
                key.clone(),
                verb.clone(),
                commit.clone(),
                "--op".into(),
                op.clone(),
            ],
            Action::Promote { verb } => vec!["promote".into(), verb.clone()],
            Action::ModelSet {
                agent,
                model,
                effort,
            } => vec![
                "model".into(),
                "set".into(),
                agent.clone(),
                model.clone(),
                "--effort".into(),
                effort.clone(),
            ],
            Action::ModelReset { agent } => vec!["model".into(), "reset".into(), agent.clone()],
            Action::ReadNotice { id } => vec!["needs".into(), "--read".into(), id.clone()],
        }
    }

    /// The action in words, such as `the take of DEMO-12`.
    pub fn label(&self) -> String {
        match self {
            Action::Take { key } => format!("The take of {key}"),
            Action::TakeAt { key, step } | Action::RetryAt { key, step } => {
                format!("The continue of {key} at {step}")
            }
            Action::Off => "The kill switch".into(),
            Action::On => "The switch on".into(),
            Action::Stop { key } => format!("The stop of {key}"),
            Action::Retry { key } => format!("The retry of {key}"),
            Action::Tell { key, .. } => format!("The message to {key}"),
            Action::Answer { id, .. } => format!("The answer to {id}"),
            Action::AnswerSet { ticket, .. } => format!("The send of the answers of {ticket}"),
            Action::ApprovePlan { key, .. } => format!("The approval of the plan of {key}"),
            Action::Approve { key, verb, .. } => format!("The approval of {verb} of {key}"),
            Action::Promote { verb } => format!("The promote of {verb}"),
            Action::ModelSet { agent, .. } => format!("The pin of the model of {agent}"),
            Action::ModelReset { agent } => format!("The reset of the model of {agent}"),
            Action::ReadNotice { id } => format!("The read of the notice {id}"),
        }
    }
}

/// The file of the send of a question form, as a path in the home folder.
fn send_path(ticket: &str) -> String {
    format!("tickets/{ticket}/{}", super::drafts::SEND_FILE)
}

impl Action {
    /// The file that the command reads: its path in the home folder and its text. The worker
    /// writes it before the command runs in the home folder, and removes it after.
    pub fn input(&self) -> Option<(String, String)> {
        match self {
            Action::AnswerSet { ticket, answers } => {
                Some((send_path(ticket), super::drafts::text(answers)))
            }
            _ => None,
        }
    }
}

/// A child that gets the terminal: the session of a ticket, or the review of a proposal.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Handoff {
    Session(String),
    Review(String),
}

impl Handoff {
    /// The arguments of the child.
    pub fn args(&self) -> Vec<String> {
        match self {
            Handoff::Session(key) => vec!["open".into(), key.clone()],
            Handoff::Review(name) => vec![
                "memory".into(),
                "review".into(),
                "--proposal".into(),
                name.clone(),
            ],
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

    /// The file that the child reads, as [`Action::input`] gives it.
    pub fn input(&self) -> Option<(String, String)> {
        match self {
            JobKind::Pass => None,
            JobKind::Action(action) => action.input(),
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
