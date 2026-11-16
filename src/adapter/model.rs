//! `adapter.toml` (section 11.2): typed values, the parser, and the tests of the step list.
//!
//! The parser collects each problem of the file and reports them together. Each problem names
//! the file, the step (or the table), and what is wrong.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use toml::Table;

use super::{table_to_json, toml_to_json, valid_document_name, Origin, FILE};
use crate::config::{Caps, Effort, Mode, Runner};
use crate::models::Fallback;
use crate::paths::{expand_tilde, TicketKey};
use crate::turn::TurnKind;

/// The branch of a ticket when `branch` is missing.
pub const DEFAULT_BRANCH: &str = "vision/{key}";
/// The ref that a new branch starts from when `base` is missing.
pub const DEFAULT_BASE: &str = "origin/main";
/// Seconds between two observe calls of a wait step when `every` is missing.
pub const DEFAULT_EVERY: u64 = 60;
/// The key of `on` of a turn step for a turn whose documents hold a kept finding.
pub const FINDINGS: &str = "findings";
/// The key of `on` of a turn step for a turn whose documents hold no kept finding, or for a
/// turn that made no commit.
pub const NONE: &str = "none";
/// The key of `on` of a turn step for a turn after which the branch head differs from the
/// head at the entry into the step.
pub const COMMITS: &str = "commits";
/// The effort of an agent of `[agents.<name>]` when `effort` is missing.
pub const DEFAULT_AGENT_EFFORT: Effort = Effort::High;
/// The turns that one visit of a turn step gets for failed end facts when `rounds` is missing:
/// the first turn and one resume.
pub const DEFAULT_ROUNDS: u32 = 2;
/// The most turns that `rounds` can give one visit, with `extra_rounds`.
pub const MAX_ROUNDS: u32 = 20;
/// The most turns that `extra_rounds` can add to one visit.
pub const MAX_EXTRA_ROUNDS: u32 = 10;

/// A parsed and tested `adapter.toml`.
#[derive(Clone, Debug, PartialEq)]
pub struct Adapter {
    /// `name`, or the name of the folder.
    pub name: String,
    /// The adapter folder: it holds `adapter.toml` and `roles/`.
    pub dir: PathBuf,
    /// The folder whose `bin/` answers the calls: `scripts`, or the adapter folder. For a
    /// built-in adapter, the adapter folder: it is the working folder of each call.
    pub scripts: PathBuf,
    /// The name of a built-in adapter, from `scripts = "builtin:<name>"`. `None` for scripts
    /// in `bin/`.
    pub builtin: Option<String>,
    /// The branch of a ticket, with `{key}` for the ticket key.
    pub branch: String,
    /// The ref that a new branch starts from, such as `origin/main`.
    pub base: String,
    /// The steps in the order of the file. The first one is the entry. Exactly one is the end.
    pub steps: Vec<Step>,
    /// The verbs by name.
    pub verbs: BTreeMap<String, Verb>,
    /// The jobs by name.
    pub jobs: BTreeMap<String, Job>,
    /// The envs of `[envs.<name>]`, as free tables.
    pub envs: BTreeMap<String, Map<String, Value>>,
    /// `[kinds]`: the kind of a ticket from its issue type. `None`: the tickets have no kind.
    pub kinds: Option<Kinds>,
    /// `[width]`: wide or narrow from the files that a document lists. `None`: no width.
    pub width: Option<Width>,
    /// `[agents.<name>]`: the agents that the adapter defines, by name. A step can name one of
    /// them as it names an agent of Vision.
    pub agents: BTreeMap<String, AdapterAgent>,
    /// Where [`super::load`] found the folder. `None` for a parsed text.
    pub origin: Option<Origin>,
}

/// One agent that the adapter defines in `[agents.<name>]`: its role file, its model, and the
/// program of its turns. The configuration of the person (`[agents.<name>]` of config.toml)
/// wins over these values, as it wins over the defaults of Vision.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AdapterAgent {
    pub name: String,
    /// The role file, relative to the adapter folder, such as `roles/cr-codex.md`. The default
    /// is `roles/<name>.md`. The scripts folder is the second place to look, as for the role
    /// files that replace those of Vision.
    pub role: String,
    pub model: String,
    pub effort: Effort,
    /// The program of its turns. Only a reader turn runs on Codex.
    pub runner: Runner,
    /// The model with Claude and its effort that take the work of a failed turn on Codex.
    pub fallback: Option<Fallback>,
    /// Why the adapter chose this model. Empty when the file gives no reason.
    pub reason: String,
}

/// `[kinds]`: the kind of a ticket from the issue type that `observe` gives in `issue.type`.
/// The kind names are words of the adapter. The core knows none of them.
#[derive(Clone, Debug, PartialEq)]
pub struct Kinds {
    /// `map`: issue type -> kind, such as `Bug = "bug"`.
    pub map: BTreeMap<String, String>,
    /// `default`: the kind of an issue type that `map` does not name.
    pub default: String,
}

impl Kinds {
    /// Each kind name: the values of `map` and `default`, sorted, each one once.
    pub fn names(&self) -> Vec<String> {
        let mut names: BTreeSet<String> = self.map.values().cloned().collect();
        names.insert(self.default.clone());
        names.into_iter().collect()
    }

    /// True when `name` is a kind of this table.
    pub fn has(&self, name: &str) -> bool {
        self.default == name || self.map.values().any(|v| v == name)
    }

    /// The kind of an issue type: the value of `map` for the same text, else for the same text
    /// in another case, else `default`.
    pub fn of(&self, issue_type: Option<&str>) -> String {
        let Some(found) = issue_type.map(str::trim).filter(|t| !t.is_empty()) else {
            return self.default.clone();
        };
        self.map
            .get(found)
            .or_else(|| {
                self.map
                    .iter()
                    .find(|(k, _)| k.eq_ignore_ascii_case(found))
                    .map(|(_, v)| v)
            })
            .cloned()
            .unwrap_or_else(|| self.default.clone())
    }
}

/// `[width]`: a plan is wide when its document lists `files` files or more in its section
/// `## Files`, or a file that matches one of `paths`. Else it is narrow.
#[derive(Clone, Debug, PartialEq)]
pub struct Width {
    /// The document of the ticket folder whose section `## Files` lists the changed files.
    pub doc: String,
    /// This many files or more is wide.
    pub files: usize,
    /// Globs: a listed file that matches one is wide, such as `db/**`.
    pub paths: Vec<String>,
}

/// The word of `when` for a wide plan.
pub const WIDE: &str = "wide";
/// The word of `when` for a narrow plan.
pub const NARROW: &str = "narrow";
/// The document of `[width]` when `doc` is missing.
pub const DEFAULT_WIDTH_DOC: &str = "plan.md";
/// The number of files of `[width]` when `files` is missing.
pub const DEFAULT_WIDTH_FILES: usize = 3;
/// The keys of `on` of an ask step.
pub const CHANGED: &str = "changed";
pub const SAME: &str = "same";

/// One `[[step]]`.
#[derive(Clone, Debug, PartialEq)]
pub struct Step {
    /// Unique in the file, and valid as a part of a turn id, such as `fix-ci`.
    pub id: String,
    pub kind: StepKind,
    /// The step after this one. Each step but a wait step and the end has one.
    pub next: Option<String>,
    /// A wait step: the value of `watch` (as text) -> the next step. A turn step with
    /// `findings`: `findings` and `none` -> the next step, by the kept findings of its turn.
    pub on: BTreeMap<String, String>,
    /// A key of `[caps]`: the step counts each entry.
    pub cap: Option<String>,
    /// `rounds` of a turn step: the turns that one visit gets for failed end facts. After that
    /// many failed turns, the ticket gets the flag needs-you. [`DEFAULT_ROUNDS`] when missing.
    pub rounds: u32,
    /// `extra_rounds` of a turn step with a gate: after `rounds` failed turns of a visit, one
    /// more turn each time, up to this many, while the loop converges: the failing items of
    /// the gate are fewer than in the turn before, and none is critical. 0 when missing.
    pub extra_rounds: u32,
    /// `fail` of a turn step: the end fact of a gate (such as `gate:qa:qa.md`) -> the step that
    /// the ticket goes to at once when that gate fails on a blocking item of its document.
    pub fail: BTreeMap<String, String>,
    /// The end facts that the step expects.
    pub expects: Vec<Expect>,
    /// The arguments of each end-fact job that `expects` gives as a table, such as
    /// `{ job = "proof", args = { plan = "{doc:approved-plan.md}" } }`, by the job name. A job of
    /// a plain `job:<name>` has no entry: its arguments are empty.
    pub expect_args: BTreeMap<String, Map<String, Value>>,
    /// What runs when each end fact holds: verbs, usually.
    pub after: Vec<Entry>,
    /// What runs first: jobs, usually. Their documents become inputs.
    pub before: Vec<Entry>,
    /// What runs at the entry of the step, such as `ci.run`.
    pub start: Vec<Entry>,
    /// `when`: kind names, `wide`, or `narrow`. The kinds are ORed, the widths are ORed, and
    /// the two groups are ANDed. A step whose `when` does not hold is skipped. Empty: always.
    pub when: Vec<String>,
}

impl Step {
    /// The arguments of the end-fact job `job`: the `args` of its table in `expects`, or none.
    pub fn expect_args_of(&self, job: &str) -> Map<String, Value> {
        self.expect_args.get(job).cloned().unwrap_or_default()
    }

    /// The members of a team step, or none.
    pub fn team(&self) -> &[Member] {
        match &self.kind {
            StepKind::Turn(turn) => &turn.team,
            _ => &[],
        }
    }

    /// The member of a team step with this agent.
    pub fn member(&self, agent: &str) -> Option<&Member> {
        self.team().iter().find(|m| m.agent == agent)
    }

    /// One member of a team step as a step of its own: its agent, its kind of turn, its reads
    /// (after the reads of the step), its end facts, and its rounds. It has no `before`, no
    /// `after`, no `fail`, and no cap: those belong to the team step. Its `next` says that
    /// the member is done.
    pub fn member_view(&self, member: &Member) -> Step {
        let (reads, with) = match &self.kind {
            StepKind::Turn(turn) => (turn.reads.clone(), turn.with.clone()),
            _ => (Vec::new(), Map::new()),
        };
        let mut reads = reads;
        for name in &member.reads {
            if !reads.contains(name) {
                reads.push(name.clone());
            }
        }
        Step {
            id: self.id.clone(),
            kind: StepKind::Turn(TurnStep {
                agent: member.agent.clone(),
                turn: member.turn,
                reads,
                with,
                findings: None,
                team: Vec::new(),
            }),
            next: self.next.clone(),
            on: BTreeMap::new(),
            cap: None,
            rounds: member.rounds,
            extra_rounds: 0,
            fail: BTreeMap::new(),
            expects: member.expects.clone(),
            expect_args: member.expect_args.clone(),
            after: Vec::new(),
            before: Vec::new(),
            start: Vec::new(),
            when: member.when.clone(),
        }
    }
}

/// One member of `team` of a turn step: one agent that works in its own turn and session, in
/// parallel with the other members.
#[derive(Clone, Debug, PartialEq)]
pub struct Member {
    pub agent: String,
    pub turn: TurnKind,
    /// More documents of the ticket folder for this member. A member never reads a document
    /// that another member of the team expects.
    pub reads: Vec<String>,
    pub expects: Vec<Expect>,
    pub expect_args: BTreeMap<String, Map<String, Value>>,
    /// `when` of the member, with the words of `when` of a step.
    pub when: Vec<String>,
    /// The turns that one visit gets for failed end facts of this member: `rounds` of the
    /// member, else `rounds` of the step.
    pub rounds: u32,
}

/// The kinds of step (section 11).
#[derive(Clone, Debug, PartialEq)]
pub enum StepKind {
    Turn(TurnStep),
    Job(JobStep),
    Human(HumanStep),
    Wait(WaitStep),
    /// The cards of some documents go to the Judge and the person as one batch.
    Ask(AskStep),
    /// A line of a document decides where the ticket goes.
    Check(CheckStep),
    /// The verbs of `run` run in order, as the `after` verbs of a turn step whose end facts
    /// hold.
    Act(ActStep),
    End,
}

impl StepKind {
    pub fn name(&self) -> &'static str {
        match self {
            StepKind::Turn(_) => "turn",
            StepKind::Job(_) => "job",
            StepKind::Human(_) => "human",
            StepKind::Wait(_) => "wait",
            StepKind::Ask(_) => "ask",
            StepKind::Check(_) => "check",
            StepKind::Act(_) => "act",
            StepKind::End => "end",
        }
    }
}

/// An act step: it runs no turn. When a ticket enters it, the verbs of `run` run in order, and
/// the ticket moves to `next` when each one is confirmed.
#[derive(Clone, Debug, PartialEq)]
pub struct ActStep {
    /// The verbs, each one a name or a table with arguments, as an entry of `after`. Each one
    /// is a verb that changes the outside: never a job and never a read verb.
    pub run: Vec<Entry>,
}

/// A turn step: one agent works in a session, or the members of `team` work in parallel.
#[derive(Clone, Debug, PartialEq)]
pub struct TurnStep {
    /// The agent, such as `builder`. Empty for a team step.
    pub agent: String,
    /// The kind of turn: reader, worker, or tester.
    pub turn: TurnKind,
    /// Documents of the ticket folder that the session gets, such as `approved-plan.md`. A
    /// team step gives them to each member.
    pub reads: Vec<String>,
    /// `with`: values that go into the facts file under `with`.
    pub with: Map<String, Value>,
    /// `findings`: the source of the findings that the documents of the step list, such as
    /// `review` or `qa`. The pipeline stores one `findings` row for each one. `None`: none.
    pub findings: Option<String>,
    /// `team`: the members that work in parallel, each in its own turn and session. Empty for
    /// a step of one agent.
    pub team: Vec<Member>,
}

impl TurnStep {
    /// The agents of the step: its agent, or each member of its team.
    pub fn agents(&self) -> Vec<&str> {
        if self.team.is_empty() {
            vec![self.agent.as_str()]
        } else {
            self.team.iter().map(|m| m.agent.as_str()).collect()
        }
    }
}

/// An ask step: it reads the cards of its documents, and they go to the Judge and the person
/// as one batch. `on.changed` or `on.same` says where the ticket goes after the answers.
#[derive(Clone, Debug, PartialEq)]
pub struct AskStep {
    /// The documents of the ticket folder with a JSON array of cards each.
    pub docs: Vec<String>,
}

/// A check step: a line of a document decides where the ticket goes.
#[derive(Clone, Debug, PartialEq)]
pub struct CheckStep {
    /// The document of the ticket folder, such as `plan.md`.
    pub doc: String,
    /// A line equal to this text (trimmed) moves the ticket to `next`.
    pub line: String,
    /// A line equal to this text sets the flag needs-you: the person stops the ticket.
    pub stop_line: Option<String>,
    /// The step when no line holds. The move counts as an entry of that step.
    pub fail: String,
}

/// A job step: the adapter runs one job.
#[derive(Clone, Debug, PartialEq)]
pub struct JobStep {
    pub job: String,
    pub args: Map<String, Value>,
}

/// A human step: it waits for a command of the person, such as `approve-plan`.
#[derive(Clone, Debug, PartialEq)]
pub struct HumanStep {
    pub action: String,
}

/// A wait step: it asks `observe` each `every` seconds and moves on the value of `watch`.
#[derive(Clone, Debug, PartialEq)]
pub struct WaitStep {
    /// A path into the answer of `observe` for the ticket, such as `ci.state`. A path that does
    /// not exist in the answer gives the value `none`.
    pub watch: String,
    pub every: u64,
    /// `wait`: the values that mean "not yet", such as `pending`. With this key, each value that
    /// is in neither `on` nor `wait` sets the flag needs-you at once. `None`: any value waits.
    pub wait: Option<Vec<String>>,
    /// `limit`: the seconds that one visit may watch. A visit that watches longer sets the flag
    /// needs-you. `None`: no limit.
    pub limit: Option<u64>,
    /// `from`: a local time of day. The visit does nothing before the first moment with that
    /// time after its entry: no start verb and no look. `None`: it starts at once.
    pub from: Option<LocalTime>,
}

/// A time of day in the time zone of the machine, such as `09:00`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LocalTime {
    pub hour: u8,
    pub minute: u8,
}

impl LocalTime {
    /// Reads `HH:MM` or `H:MM`, such as `09:00` or `7:30`.
    pub fn parse(text: &str) -> Option<LocalTime> {
        let (hour, minute) = text.trim().split_once(':')?;
        let digits = |t: &str, max: usize| {
            (1..=max).contains(&t.len()) && t.bytes().all(|b| b.is_ascii_digit())
        };
        if !digits(hour, 2) || !digits(minute, 2) || minute.len() != 2 {
            return None;
        }
        let (hour, minute): (u8, u8) = (hour.parse().ok()?, minute.parse().ok()?);
        (hour < 24 && minute < 60).then_some(LocalTime { hour, minute })
    }
}

impl fmt::Display for LocalTime {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:02}:{:02}", self.hour, self.minute)
    }
}

/// One end fact of a step (section 11.2).
#[derive(Clone, Debug, PartialEq)]
pub enum Expect {
    /// `doc:<name>`: the result has that document, and it is not empty.
    Doc(String),
    /// `commits`: the branch has a commit that it did not have at the start of the turn.
    Commits,
    /// `clean`: the work tree of the clone is clean.
    Clean,
    /// `job:<name>`: the job passes at the last commit.
    Job(String),
    /// `human:<action>`: the person did that action.
    Human(String),
    /// `{ gate = "cr", doc = "cr.md", ... }`: code reads a document of the turn and decides.
    Gate(Gate),
}

/// A gate of `expects`: code reads one document of the turn result and decides whether it
/// passes. The pipeline reads the document; the agent never decides. This is not a gate of
/// the executor (section 12).
#[derive(Clone, Debug, PartialEq)]
pub struct Gate {
    /// The document of the result that the gate reads, such as `cr.md`.
    pub doc: String,
    pub rule: GateRule,
}

/// What a gate tests.
#[derive(Clone, Debug, PartialEq)]
pub enum GateRule {
    /// `gate = "cr"`: the report of a code review. It passes when no finding has a Net above
    /// `net_max` and no finding is tagged BLOCKING.
    Cr { net_max: f64 },
    /// `gate = "qa"`: the report of a QA run. It passes when no issue has a severity of
    /// `block`. With `notify`, the issues of a report that passes become one notice for the
    /// person.
    Qa { block: Vec<Severity>, notify: bool },
}

/// The `net_max` of a cr gate with no such key: a finding of +5 or more fails.
pub const CR_NET_MAX: f64 = 4.0;
/// The `block` of a qa gate with no such key: each severity but low.
pub const QA_BLOCK: [Severity; 3] = [Severity::Critical, Severity::High, Severity::Medium];

impl Gate {
    /// The kind of the gate: `cr` or `qa`.
    pub fn kind(&self) -> &'static str {
        match self.rule {
            GateRule::Cr { .. } => "cr",
            GateRule::Qa { .. } => "qa",
        }
    }

    /// The name of the gate as an end fact, such as `gate:qa:qa.md`.
    pub fn fact(&self) -> String {
        format!("gate:{}:{}", self.kind(), self.doc)
    }
}

/// The severity of an issue of a QA report, the most severe first.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Severity {
    Critical,
    High,
    Medium,
    Low,
}

impl Severity {
    /// Each severity, the most severe first.
    pub const ALL: [Severity; 4] = [
        Severity::Critical,
        Severity::High,
        Severity::Medium,
        Severity::Low,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Severity::Critical => "critical",
            Severity::High => "high",
            Severity::Medium => "medium",
            Severity::Low => "low",
        }
    }

    /// Reads a word such as `High` or `medium`, in any case.
    pub fn parse(word: &str) -> Option<Severity> {
        Severity::ALL
            .into_iter()
            .find(|s| s.as_str().eq_ignore_ascii_case(word))
    }
}

impl fmt::Display for Severity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl Expect {
    /// Reads `doc:pr.md`, `commits`, `clean`, `job:proof`, or `human:approve-plan`.
    pub fn parse(text: &str) -> Result<Expect, String> {
        let bad = || {
            format!(
                "`{text}` is not an end fact. Use doc:<name>, commits, clean, job:<name>, or human:<action>"
            )
        };
        match text {
            "commits" => return Ok(Expect::Commits),
            "clean" => return Ok(Expect::Clean),
            _ => {}
        }
        let (kind, name) = text.split_once(':').ok_or_else(bad)?;
        match kind {
            "doc" if valid_document_name(name) => Ok(Expect::Doc(name.to_string())),
            "doc" => Err(format!(
                "`{text}` names a document that is not a plain file name, such as doc:pr.md"
            )),
            "job" if valid_name(name, true) => Ok(Expect::Job(name.to_string())),
            "human" if valid_name(name, false) => Ok(Expect::Human(name.to_string())),
            _ => Err(bad()),
        }
    }
}

impl fmt::Display for Expect {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Expect::Doc(name) => write!(f, "doc:{name}"),
            Expect::Commits => f.write_str("commits"),
            Expect::Clean => f.write_str("clean"),
            Expect::Job(name) => write!(f, "job:{name}"),
            Expect::Human(action) => write!(f, "human:{action}"),
            Expect::Gate(gate) => f.write_str(&gate.fact()),
        }
    }
}

/// What an entry of `after`, `before`, or `start` runs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EntryKind {
    Verb,
    Job,
}

/// One entry of `after`, `before`, or `start`: `"git.push"`, `"job:proof"`,
/// `{ verb = "deploy", args = { env = "staging" } }`, or `{ job = "app-start", args = {...} }`.
#[derive(Clone, Debug, PartialEq)]
pub struct Entry {
    pub kind: EntryKind,
    pub name: String,
    /// The arguments of the call. Empty for a plain name.
    pub args: Map<String, Value>,
}

impl Entry {
    /// `git.push` for a verb, `job:proof` for a job.
    pub fn label(&self) -> String {
        match self.kind {
            EntryKind::Verb => self.name.clone(),
            EntryKind::Job => format!("job:{}", self.name),
        }
    }
}

/// One `[verbs."<name>"]`.
#[derive(Clone, Debug, PartialEq)]
pub struct Verb {
    pub name: String,
    /// The mode at the start. `[modes]` of the configuration wins. `ask` when missing.
    pub mode: Mode,
    /// Text or code leaves the machine, so the gates run first.
    pub leaves: bool,
    /// A read verb changes nothing outside. It needs no gate, no approval, and no idempotency
    /// key, and its answer carries documents. A turn asks for it with a result of the kind
    /// `requests`.
    pub read: bool,
    /// Seconds before the executor kills `bin/act`. `None` takes the default.
    pub timeout: Option<u64>,
    /// `once = true`: the verb runs one time for a ticket. Its idempotency key leaves out the
    /// commit, so the same verb with the same arguments never runs again at a later commit.
    /// A verb with `leaves` cannot have it: its gates judge what leaves at each commit.
    pub once: bool,
    /// `about`: one line for a session: what a read verb gives and which arguments it takes.
    pub about: Option<String>,
}

/// One `[jobs.<name>]`.
#[derive(Clone, Debug, PartialEq)]
pub struct Job {
    pub name: String,
    /// Seconds before the executor kills `bin/job`. `None` takes the default.
    pub timeout: Option<u64>,
    /// True when `bin/job` gets the secrets for this job. A job runs code of the project, such
    /// as the tests that an agent wrote, so the default is false. Only a job that reads the
    /// tracker or the forge, and runs no code of the project, sets it.
    pub secrets: bool,
    /// `command`: a shell command of the person that the job runs. The call of `bin/job` gets
    /// the table of the job, and the built-in adapter runs the command. `None`: a job that the
    /// adapter knows by its name, such as the proof.
    pub command: Option<String>,
    /// `doc`: the document of the ticket folder that gets the standard output of `command`.
    pub doc: Option<String>,
    /// `env`: the environment of the call. Only a job with `command` can have `full`.
    pub env: JobEnv,
    /// The keys of the table of a job that a built-in adapter knows by its name, such as the
    /// precheck of jira-github, as JSON. The call of `bin/job` gets them in `table`. Empty for
    /// each other job.
    pub settings: Map<String, Value>,
}

/// The environment of the call of a job.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum JobEnv {
    /// `PATH`, `HOME`, `LANG`, and `TMPDIR`, as each script gets them.
    #[default]
    Plain,
    /// The environment of the tick, for a command of the person that needs the login of the
    /// person. A variable whose value is a secret of `[secrets]` stays out, unless the job has
    /// `secrets = true`. A turn never gets it.
    Full,
    /// The environment of a work session (section 7.3): the names of its allow list,
    /// `claude.env_pass`, and the values of `claude.env`, with no secret. A job that runs the
    /// commands of the project as a session does, such as the precheck, gets it.
    Session,
}

impl JobEnv {
    pub fn as_str(self) -> &'static str {
        match self {
            JobEnv::Plain => "plain",
            JobEnv::Full => "full",
            JobEnv::Session => "session",
        }
    }

    /// Reads `plain`, `session`, or `full`.
    pub fn parse(text: &str) -> Option<JobEnv> {
        [JobEnv::Plain, JobEnv::Session, JobEnv::Full]
            .into_iter()
            .find(|e| e.as_str() == text)
    }
}

/// The keys of `[caps]` of the configuration, which a step can name in `cap`.
pub fn cap_names() -> Vec<String> {
    match serde_json::to_value(Caps::default()) {
        Ok(Value::Object(map)) => map.keys().cloned().collect(),
        _ => Vec::new(),
    }
}

impl Adapter {
    /// Reads and tests `adapter.toml` of `dir`. The folder of `scripts` must exist.
    pub fn read(dir: &Path) -> Result<Adapter> {
        let file = dir.join(FILE);
        let text = std::fs::read_to_string(&file)
            .with_context(|| format!("could not read {}", file.display()))?;
        let adapter = Adapter::parse(&text, dir)?;
        if !adapter.scripts.is_dir() {
            bail!(
                "{}: `scripts` names {}, which is not a folder. Fix the path: it is relative to the adapter folder, or absolute",
                file.display(),
                adapter.scripts.display()
            );
        }
        Ok(adapter)
    }

    /// Parses and tests the text of `adapter.toml` of the folder `dir`. It reads no file.
    pub fn parse(text: &str, dir: &Path) -> Result<Adapter> {
        let file = dir.join(FILE);
        let mut table: Table = toml::from_str(text).map_err(|e| {
            anyhow::anyhow!(
                "{} is not valid TOML: {}",
                file.display(),
                e.to_string().trim_end()
            )
        })?;
        let mut problems = Problems::default();
        let adapter = parse_table(&mut table, dir, &mut problems);
        if !problems.list.is_empty() {
            bail!(
                "{} is not a valid adapter. Fix it, then run the command again:\n  - {}",
                file.display(),
                problems.list.join("\n  - ")
            );
        }
        Ok(adapter)
    }

    /// The first step of the file.
    pub fn entry(&self) -> &Step {
        &self.steps[0]
    }

    /// The step with the kind `end`.
    pub fn end(&self) -> &Step {
        self.steps
            .iter()
            .find(|s| s.kind == StepKind::End)
            .unwrap_or(&self.steps[0])
    }

    pub fn step(&self, id: &str) -> Option<&Step> {
        self.steps.iter().find(|s| s.id == id)
    }

    /// The verb, or an error that names the verbs of the adapter.
    pub fn verb(&self, name: &str) -> Result<&Verb> {
        self.verbs.get(name).with_context(|| {
            format!(
                "the adapter `{}` has no verb `{name}`. Its verbs: {}",
                self.name,
                names(self.verbs.keys())
            )
        })
    }

    /// The job, or an error that names the jobs of the adapter.
    pub fn job(&self, name: &str) -> Result<&Job> {
        self.jobs.get(name).with_context(|| {
            format!(
                "the adapter `{}` has no job `{name}`. Its jobs: {}",
                self.name,
                names(self.jobs.keys())
            )
        })
    }

    /// The branch of a ticket, such as `vision/DEMO-12`.
    pub fn branch_for(&self, key: &TicketKey) -> String {
        self.branch.replace("{key}", key.as_str())
    }

    /// The path of a script, such as `<scripts>/bin/act`. A built-in adapter has no such file.
    pub fn script_path(&self, name: &str) -> PathBuf {
        self.scripts.join("bin").join(name)
    }

    /// The role file of an agent in the adapter: the file of `role` of an agent that the
    /// adapter defines, else `roles/<agent>.md`, which replaces `agents/<agent>.md` of Vision.
    /// The adapter folder comes first, then the scripts folder.
    pub fn role_file(&self, agent: &str) -> Option<PathBuf> {
        if !valid_name(agent, false) {
            return None;
        }
        let relative = match self.agents.get(agent) {
            Some(defined) => PathBuf::from(&defined.role),
            None => Path::new("roles").join(format!("{agent}.md")),
        };
        [&self.dir, &self.scripts]
            .iter()
            .map(|dir| dir.join(&relative))
            .find(|path| path.is_file())
    }

    /// Each agent that the adapter defines must have its role file.
    pub fn check_agents(&self) -> Result<()> {
        for agent in self.agents.values() {
            if self.role_file(&agent.name).is_none() {
                bail!(
                    "{}: [agents.{}]: the role file {} does not exist in {}. Write it, or fix role",
                    self.dir.join(FILE).display(),
                    agent.name,
                    agent.role,
                    self.dir.display()
                );
            }
        }
        Ok(())
    }
}

fn names<'a>(keys: impl Iterator<Item = &'a String>) -> String {
    let list: Vec<&str> = keys.map(String::as_str).collect();
    if list.is_empty() {
        "none".to_string()
    } else {
        list.join(", ")
    }
}

#[derive(Default)]
struct Problems {
    list: Vec<String>,
}

impl Problems {
    fn add(&mut self, at: &str, what: impl fmt::Display) {
        self.list.push(format!("{at}: {what}"));
    }
}

const TOP_KEYS: [&str; 11] = [
    "name", "branch", "base", "scripts", "step", "verbs", "jobs", "envs", "kinds", "width",
    "agents",
];

fn parse_table(table: &mut Table, dir: &Path, problems: &mut Problems) -> Adapter {
    let folder_name = dir
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "adapter".to_string());
    let name = string(table.remove("name"), "the key `name`", problems).unwrap_or(folder_name);
    let branch = string(table.remove("branch"), "the key `branch`", problems)
        .unwrap_or_else(|| DEFAULT_BRANCH.to_string());
    check_branch(&branch, problems);
    let base = string(table.remove("base"), "the key `base`", problems)
        .unwrap_or_else(|| DEFAULT_BASE.to_string());
    if base.trim().is_empty() || base.contains(char::is_whitespace) {
        problems.add(
            "the key `base`",
            "it must be a git ref with no space, such as origin/main",
        );
    }
    let (scripts, builtin) = match string(table.remove("scripts"), "the key `scripts`", problems) {
        Some(text) => match text.strip_prefix(crate::adapters::PREFIX) {
            Some(name) => {
                if !crate::adapters::is_builtin(name) {
                    problems.add(
                        "the key `scripts`",
                        format!(
                            "`{text}` names no built-in adapter. The built-in adapters: {}",
                            crate::adapters::NAMES
                                .iter()
                                .map(|n| format!("{}{n}", crate::adapters::PREFIX))
                                .collect::<Vec<_>>()
                                .join(", ")
                        ),
                    );
                }
                (dir.to_path_buf(), Some(name.to_string()))
            }
            None => (scripts_dir(&text, dir, problems), None),
        },
        None => (dir.to_path_buf(), None),
    };
    let verbs = parse_verbs(table.remove("verbs"), problems);
    let jobs = parse_jobs(table.remove("jobs"), builtin.as_deref(), problems);
    let envs = parse_envs(table.remove("envs"), problems);
    let kinds = parse_kinds(table.remove("kinds"), problems);
    let width = parse_width(table.remove("width"), problems);
    let agents = parse_agents(table.remove("agents"), problems);
    let steps = parse_steps(table.remove("step"), problems);
    for key in table.keys() {
        problems.add(
            &format!("the key `{key}`"),
            format!(
                "it is not a key of adapter.toml. The keys: {}",
                TOP_KEYS.join(", ")
            ),
        );
    }
    let adapter = Adapter {
        name,
        dir: dir.to_path_buf(),
        scripts,
        builtin,
        branch,
        base,
        steps,
        verbs,
        jobs,
        envs,
        kinds,
        width,
        agents,
        origin: None,
    };
    check_links(&adapter, problems);
    check_when(&adapter, problems);
    check_teams(&adapter, problems);
    check_agent_uses(&adapter, problems);
    adapter
}

/// The keys of `[agents.<name>]`.
const AGENT_KEYS: [&str; 6] = ["role", "model", "effort", "runner", "fallback", "reason"];

/// `[agents.<name>]`: the agents that the adapter defines. A name of an agent of Vision is
/// refused: its role file changes with `roles/<name>.md`, and its model with `vision model set`.
fn parse_agents(
    value: Option<toml::Value>,
    problems: &mut Problems,
) -> BTreeMap<String, AdapterAgent> {
    let mut agents = BTreeMap::new();
    let Some(value) = value else {
        return agents;
    };
    let toml::Value::Table(table) = value else {
        problems.add(
            "[agents]",
            "it holds one table for each agent, such as [agents.cr-codex] with model = \"gpt-6.1-sol\"",
        );
        return agents;
    };
    for (name, item) in table {
        let at = format!("[agents.{name}]");
        if !valid_name(&name, false) {
            problems.add(
                &at,
                "an agent name has letters, digits, '-', or '_', starts with a letter or a digit, and has up to 64 characters",
            );
            continue;
        }
        if crate::models::agent(&name).is_some() {
            problems.add(
                &at,
                format!("`{name}` is an agent of Vision, so the adapter cannot define it. To give it another role file, write roles/{name}.md in the adapter folder. To give it another model, run vision model set {name} <model>"),
            );
            continue;
        }
        let toml::Value::Table(mut fields) = item else {
            problems.add(
                &at,
                "it must be a table, such as [agents.cr-codex] with model = \"gpt-6.1-sol\"",
            );
            continue;
        };
        let role = string(fields.remove("role"), &at, problems)
            .unwrap_or_else(|| format!("roles/{name}.md"));
        let relative = Path::new(&role);
        let inside = !role.trim().is_empty()
            && relative.is_relative()
            && relative
                .components()
                .all(|c| matches!(c, std::path::Component::Normal(_)));
        if !inside {
            problems.add(
                &at,
                format!("role names `{role}`. It is a path in the adapter folder, such as role = \"roles/{name}.md\", with no `..`"),
            );
        }
        let model = match string(fields.remove("model"), &at, problems) {
            Some(text) => match crate::models::full_model_id(&text) {
                Ok(model) => model,
                Err(e) => {
                    problems.add(&at, format!("model: {e}"));
                    text
                }
            },
            None => {
                problems.add(&at, "it needs model, such as model = \"claude-opus-5-5\"");
                String::new()
            }
        };
        let effort =
            agent_effort(fields.remove("effort"), &at, problems).unwrap_or(DEFAULT_AGENT_EFFORT);
        let runner = match fields.remove("runner") {
            None => Runner::Claude,
            Some(value) => match value.clone().try_into::<Runner>() {
                Ok(runner) => runner,
                Err(_) => {
                    problems.add(&at, format!("runner is claude or codex, not `{value}`"));
                    Runner::Claude
                }
            },
        };
        let fallback = match fields.remove("fallback") {
            None => None,
            Some(toml::Value::Table(mut given)) => {
                let here = format!("{at} fallback");
                let model = match string(given.remove("model"), &here, problems) {
                    Some(text) => match crate::models::full_model_id(&text) {
                        Ok(model) => Some(model),
                        Err(e) => {
                            problems.add(&here, format!("model: {e}"));
                            None
                        }
                    },
                    None => {
                        problems.add(
                            &here,
                            "it names the model with Claude, such as fallback = { model = \"claude-opus-5-5\", effort = \"high\" }",
                        );
                        None
                    }
                };
                let effort = agent_effort(given.remove("effort"), &here, problems)
                    .unwrap_or(DEFAULT_AGENT_EFFORT);
                for key in given.keys() {
                    problems.add(
                        &here,
                        format!("`{key}` is not a key of fallback. The keys: model, effort"),
                    );
                }
                model.map(|model| Fallback { model, effort })
            }
            Some(other) => {
                problems.add(
                    &at,
                    format!("fallback is a table, such as fallback = {{ model = \"claude-opus-5-5\", effort = \"high\" }}, not `{other}`"),
                );
                None
            }
        };
        if fallback.is_some() && runner == Runner::Claude {
            problems.add(
                &at,
                "fallback names the model with Claude that takes the work of a failed turn on Codex, so it needs runner = \"codex\"",
            );
        }
        let reason = string(fields.remove("reason"), &at, problems)
            .map(|r| r.trim().to_string())
            .unwrap_or_default();
        for key in fields.keys() {
            problems.add(
                &at,
                format!(
                    "`{key}` is not a key of an agent. The keys: {}",
                    AGENT_KEYS.join(", ")
                ),
            );
        }
        agents.insert(
            name.clone(),
            AdapterAgent {
                name,
                role,
                model,
                effort,
                runner,
                fallback,
                reason,
            },
        );
    }
    agents
}

/// The effort of `[agents.<name>]` or of its fallback: low, medium, high, xhigh, or max.
fn agent_effort(value: Option<toml::Value>, at: &str, problems: &mut Problems) -> Option<Effort> {
    let value = value?;
    match value.clone().try_into::<Effort>() {
        Ok(effort) => Some(effort),
        Err(_) => {
            problems.add(
                at,
                format!("effort is low, medium, high, xhigh, or max, not `{value}`"),
            );
            None
        }
    }
}

/// Each turn step and each team member names an agent of Vision or an agent of `[agents]`.
/// An agent of `[agents]` on Codex runs only reader turns, and at least one step uses it so.
fn check_agent_uses(adapter: &Adapter, problems: &mut Problems) {
    let mut uses: BTreeMap<&str, Vec<(&str, TurnKind)>> = BTreeMap::new();
    for step in &adapter.steps {
        let StepKind::Turn(turn) = &step.kind else {
            continue;
        };
        let agents: Vec<(&str, TurnKind)> = if turn.team.is_empty() {
            vec![(turn.agent.as_str(), turn.turn)]
        } else {
            turn.team
                .iter()
                .map(|m| (m.agent.as_str(), m.turn))
                .collect()
        };
        for (agent, kind) in agents {
            if agent.is_empty() {
                continue;
            }
            if crate::models::agent(agent).is_none() && !adapter.agents.contains_key(agent) {
                problems.add(
                    &format!("step `{}`", step.id),
                    format!(
                        "the agent `{agent}` is no agent of Vision and no agent of [agents] of this file. Define it with [agents.{agent}] and its role file, or use one of {}",
                        crate::models::agent_names().join(", ")
                    ),
                );
            }
            uses.entry(agent)
                .or_default()
                .push((step.id.as_str(), kind));
        }
    }
    for agent in adapter.agents.values() {
        if agent.runner != Runner::Codex {
            continue;
        }
        let at = format!("[agents.{}]", agent.name);
        let found = uses.get(agent.name.as_str()).cloned().unwrap_or_default();
        for (step, kind) in &found {
            if *kind != TurnKind::Reader {
                problems.add(
                    &at,
                    format!("it runs on Codex, but the step `{step}` makes it a {kind}. Only a reader turn runs on Codex: make it a reader there, or set runner = \"claude\""),
                );
            }
        }
        if !found.iter().any(|(_, kind)| *kind == TurnKind::Reader) {
            problems.add(
                &at,
                "it runs on Codex, but no step makes it a reader. Only a reader turn runs on Codex: name it in a step with turn = \"reader\", or remove it",
            );
        }
    }
}

/// `[kinds]`: `map` from issue types to kinds, and `default`.
fn parse_kinds(value: Option<toml::Value>, problems: &mut Problems) -> Option<Kinds> {
    let value = value?;
    let at = "[kinds]";
    let toml::Value::Table(mut table) = value else {
        problems.add(
            at,
            "it must be a table, such as [kinds] with map = { Bug = \"bug\" } and default = \"feature\"",
        );
        return None;
    };
    let mut map = BTreeMap::new();
    match table.remove("map") {
        None => {}
        Some(toml::Value::Table(items)) => {
            for (issue_type, kind) in items {
                match kind {
                    toml::Value::String(kind) if valid_name(&kind, false) => {
                        map.insert(issue_type, kind);
                    }
                    other => problems.add(
                        at,
                        format!("map.{issue_type} must name a kind with letters, digits, '-', or '_', such as \"bug\", not `{other}`"),
                    ),
                }
            }
        }
        Some(other) => problems.add(
            at,
            format!("map must be a table from an issue type to a kind, such as map = {{ Bug = \"bug\" }}, not `{other}`"),
        ),
    }
    let default = string(table.remove("default"), at, problems);
    for key in table.keys() {
        problems.add(
            at,
            format!("`{key}` is not a key of [kinds]. The keys: map, default"),
        );
    }
    let Some(default) = default else {
        problems.add(
            at,
            "it needs default, the kind of an issue type that map does not name, such as default = \"feature\"",
        );
        return None;
    };
    if !valid_name(&default, false) {
        problems.add(
            at,
            format!("default `{default}` is not a kind name: letters, digits, '-', or '_'"),
        );
    }
    for word in [WIDE, NARROW] {
        if default == word || map.values().any(|v| v == word) {
            problems.add(
                at,
                format!("`{word}` is a word of [width], so it cannot be a kind"),
            );
        }
    }
    Some(Kinds { map, default })
}

/// `[width]`: `doc`, `files`, and `paths`.
fn parse_width(value: Option<toml::Value>, problems: &mut Problems) -> Option<Width> {
    let value = value?;
    let at = "[width]";
    let toml::Value::Table(mut table) = value else {
        problems.add(
            at,
            "it must be a table, such as [width] with doc = \"plan.md\", files = 3, paths = [\"db/**\"]",
        );
        return None;
    };
    let doc =
        string(table.remove("doc"), at, problems).unwrap_or_else(|| DEFAULT_WIDTH_DOC.to_string());
    if !valid_document_name(&doc) {
        problems.add(
            at,
            format!("doc names `{doc}`, which is not a plain file name of the ticket folder"),
        );
    }
    let files = match table.remove("files") {
        None => DEFAULT_WIDTH_FILES,
        Some(toml::Value::Integer(n)) if (1..=10_000).contains(&n) => n as usize,
        Some(other) => {
            problems.add(
                at,
                format!("files is the number of files that makes a plan wide, such as files = 3, not `{other}`"),
            );
            DEFAULT_WIDTH_FILES
        }
    };
    let paths = strings(table.remove("paths"), &format!("{at} paths"), problems);
    for path in &paths {
        if path.trim().is_empty() {
            problems.add(at, "paths holds an empty glob");
        }
    }
    for key in table.keys() {
        problems.add(
            at,
            format!("`{key}` is not a key of [width]. The keys: doc, files, paths"),
        );
    }
    Some(Width { doc, files, paths })
}

/// The words of each `when`: a kind of `[kinds]`, or `wide` or `narrow` with `[width]`. The end
/// step and a wait step have no `when`, because they have no `next` to skip to.
fn check_when(adapter: &Adapter, problems: &mut Problems) {
    let check = |at: &str, words: &[String], problems: &mut Problems| {
        for word in words {
            if word == WIDE || word == NARROW {
                if adapter.width.is_none() {
                    problems.add(
                        at,
                        format!("when names `{word}`, but the adapter has no [width] that says which plan is wide"),
                    );
                }
            } else if let Some(kinds) = &adapter.kinds {
                if !kinds.has(word) {
                    problems.add(
                        at,
                        format!(
                            "when names `{word}`, which is no kind of [kinds] and not wide or narrow. The kinds: {}",
                            kinds.names().join(", ")
                        ),
                    );
                }
            } else {
                problems.add(
                    at,
                    format!("when names `{word}`, but the adapter has no [kinds], so a ticket has no kind. Add [kinds], or use wide or narrow"),
                );
            }
        }
    };
    for step in &adapter.steps {
        let at = format!("step `{}`", step.id);
        check(&at, &step.when, problems);
        for member in step.team() {
            check(
                &format!("{at} team member `{}`", member.agent),
                &member.when,
                problems,
            );
        }
    }
}

/// The rules of a team: at most one member that changes the clone, no agent two times, and no
/// member that reads a document that another member expects.
fn check_teams(adapter: &Adapter, problems: &mut Problems) {
    for step in &adapter.steps {
        let team = step.team();
        if team.is_empty() {
            continue;
        }
        let at = format!("step `{}`", step.id);
        let writers: Vec<&str> = team
            .iter()
            .filter(|m| m.turn != TurnKind::Reader)
            .map(|m| m.agent.as_str())
            .collect();
        if writers.len() > 1 {
            problems.add(
                &at,
                format!(
                    "the team has more than one worker or tester ({}), but the clone holds one of them. Make the others readers",
                    writers.join(", ")
                ),
            );
        }
        let mut seen = BTreeSet::new();
        for member in team {
            if !seen.insert(member.agent.as_str()) {
                problems.add(
                    &at,
                    format!("the team names the agent `{}` more than once", member.agent),
                );
            }
        }
        let shared = match &step.kind {
            StepKind::Turn(turn) => turn.reads.clone(),
            _ => Vec::new(),
        };
        for member in team {
            for expect in &member.expects {
                if let Expect::Job(job) = expect {
                    if !adapter.jobs.contains_key(job) {
                        problems.add(&at, missing_job(job, &format!("expects job:{job}")));
                    }
                }
            }
            for other in team.iter().filter(|o| o.agent != member.agent) {
                for name in shared.iter().chain(&member.reads) {
                    let expected = other.expects.iter().any(|e| match e {
                        Expect::Doc(doc) => doc == name,
                        Expect::Gate(gate) => gate.doc == *name,
                        _ => false,
                    });
                    if expected {
                        problems.add(
                            &at,
                            format!(
                                "the member `{}` reads {name}, which the member `{}` expects. A member never gets the documents of another member",
                                member.agent, other.agent
                            ),
                        );
                    }
                }
            }
        }
    }
}

fn check_branch(branch: &str, problems: &mut Problems) {
    if !branch.contains("{key}") {
        problems.add(
            "the key `branch`",
            "it must hold {key}, such as \"vision/{key}\"",
        );
    }
    let bad = branch.contains(char::is_whitespace)
        || branch.starts_with(['-', '/'])
        || branch.ends_with(['/', '.'])
        || branch.contains("..")
        || branch.contains(['~', '^', ':', '?', '*', '[', '\\']);
    if bad {
        problems.add(
            "the key `branch`",
            format!("`{branch}` cannot be the name of a git branch"),
        );
    }
}

fn scripts_dir(path: &str, dir: &Path, problems: &mut Problems) -> PathBuf {
    match expand_tilde(path) {
        Ok(p) if p.is_absolute() => p,
        Ok(p) => dir.join(p),
        Err(e) => {
            problems.add("the key `scripts`", format!("{e:#}"));
            dir.to_path_buf()
        }
    }
}

fn parse_verbs(value: Option<toml::Value>, problems: &mut Problems) -> BTreeMap<String, Verb> {
    let mut verbs = BTreeMap::new();
    let Some(value) = value else {
        return verbs;
    };
    let toml::Value::Table(table) = value else {
        problems.add(
            "[verbs]",
            "it must be a table of tables, such as [verbs.\"git.push\"]",
        );
        return verbs;
    };
    for (name, value) in table {
        let at = format!("[verbs.\"{name}\"]");
        if !valid_name(&name, true) {
            problems.add(
                &at,
                "a verb name has letters, digits, '.', '-', or '_', such as git.push",
            );
        }
        let toml::Value::Table(mut fields) = value else {
            problems.add(&at, "it must be a table, such as { mode = \"ask\" }");
            continue;
        };
        let mode_text = string(fields.remove("mode"), &at, problems);
        let leaves = boolean(fields.remove("leaves"), &at, problems).unwrap_or(false);
        let read = boolean(fields.remove("read"), &at, problems).unwrap_or(false);
        let timeout = seconds(fields.remove("timeout"), &format!("{at} timeout"), problems);
        let once = boolean(fields.remove("once"), &at, problems).unwrap_or(false);
        let about = string(fields.remove("about"), &format!("{at} about"), problems);
        if let Some(text) = &about {
            if text.trim().is_empty() || text.contains(['\n', '\r']) || text.chars().count() > 300 {
                problems.add(
                    &at,
                    "about is one line of up to 300 characters: what the verb gives and which arguments it takes",
                );
            }
        }
        for key in fields.keys() {
            problems.add(
                &at,
                format!("`{key}` is not a key of a verb. The keys: mode, leaves, read, timeout, once, about"),
            );
        }
        let mode = match mode_text.as_deref() {
            None => Mode::Ask,
            Some(text) => parse_mode(text).unwrap_or_else(|| {
                problems.add(
                    &at,
                    format!("the mode `{text}` is not one of hold, shadow, ask, auto"),
                );
                Mode::Ask
            }),
        };
        if read && (leaves || mode_text.is_some()) {
            problems.add(
                &at,
                "a read verb changes nothing outside, so it has no mode and no leaves",
            );
        }
        if read && once {
            problems.add(
                &at,
                "a read verb has no idempotency key, so once does not belong to it",
            );
        }
        if leaves && once {
            problems.add(
                &at,
                "the gates judge what leaves at each commit, so a verb with leaves cannot have once",
            );
        }
        verbs.insert(
            name.clone(),
            Verb {
                name,
                mode,
                leaves,
                read,
                timeout,
                once,
                about: about.map(|t| t.trim().to_string()),
            },
        );
    }
    verbs
}

fn parse_mode(text: &str) -> Option<Mode> {
    match text {
        "hold" => Some(Mode::Hold),
        "shadow" => Some(Mode::Shadow),
        "ask" => Some(Mode::Ask),
        "auto" => Some(Mode::Auto),
        _ => None,
    }
}

fn parse_jobs(
    value: Option<toml::Value>,
    builtin: Option<&str>,
    problems: &mut Problems,
) -> BTreeMap<String, Job> {
    let mut jobs = BTreeMap::new();
    let Some(value) = value else {
        return jobs;
    };
    let toml::Value::Table(table) = value else {
        problems.add(
            "[jobs]",
            "it must be a table of tables, such as [jobs.proof]",
        );
        return jobs;
    };
    for (name, value) in table {
        let at = format!("[jobs.{name}]");
        if !valid_name(&name, true) {
            problems.add(
                &at,
                "a job name has letters, digits, '.', '-', or '_', such as proof",
            );
        }
        let toml::Value::Table(mut fields) = value else {
            problems.add(&at, "it must be a table, such as { timeout = 600 }");
            continue;
        };
        let timeout = seconds(fields.remove("timeout"), &format!("{at} timeout"), problems);
        let secrets = boolean(fields.remove("secrets"), &at, problems).unwrap_or(false);
        let command = string(fields.remove("command"), &format!("{at} command"), problems);
        if command
            .as_deref()
            .is_some_and(|c| c.trim().is_empty() || c.contains('\0'))
        {
            problems.add(
                &at,
                "command is the shell command of the job, such as command = \"./bin/check\"",
            );
        }
        let doc = string(fields.remove("doc"), &format!("{at} doc"), problems);
        if let Some(doc) = &doc {
            if !valid_document_name(doc) {
                problems.add(
                    &at,
                    format!("doc names `{doc}`, which is not a plain file name of the ticket folder, such as doc = \"regression.md\""),
                );
            } else if crate::executor::RESERVED_DOCUMENTS.contains(&doc.as_str()) {
                problems.add(
                    &at,
                    format!("doc names `{doc}`, which belongs to the person or the pipeline. Name a different document"),
                );
            }
        }
        // A job that a built-in adapter knows by its name can have keys of its own, such as
        // the commands of the precheck.
        let kind = builtin.and_then(|b| crate::adapters::job_settings(b, &name));
        let mut settings = Map::new();
        if let Some(kind) = kind {
            for key in kind.keys {
                if let Some(value) = fields.remove(*key) {
                    settings.insert((*key).to_string(), toml_to_json(&value));
                }
            }
            if command.is_some() && !settings.is_empty() {
                problems.add(
                    &at,
                    format!("a job with command runs that command, so it has none of the keys of the job {name}: {}", kind.keys.join(", ")),
                );
            }
            for problem in (kind.check)(&settings) {
                problems.add(&at, problem);
            }
        }
        let env_text = string(fields.remove("env"), &format!("{at} env"), problems);
        let env = match env_text.as_deref() {
            None => kind
                .filter(|_| command.is_none())
                .map_or(JobEnv::Plain, |k| k.env),
            Some(text) => JobEnv::parse(text).unwrap_or_else(|| {
                problems.add(&at, format!("env `{text}` is not plain, session, or full"));
                JobEnv::Plain
            }),
        };
        if command.is_none() && (doc.is_some() || env == JobEnv::Full) {
            problems.add(
                &at,
                "doc and env belong to a job with command, and env = \"full\" only to such a job. A job that the adapter knows by its name keeps the plain environment or the environment of a session",
            );
        }
        if env == JobEnv::Session && secrets {
            problems.add(
                &at,
                "env = \"session\" is the environment of a work session, which never holds a secret, so the job cannot have secrets = true",
            );
        }
        if kind.is_some_and(|k| k.no_secrets)
            && command.is_none()
            && (secrets || env == JobEnv::Full)
        {
            problems.add(
                &at,
                format!("the job {name} runs code of the project, so it gets no secret and never the environment of the tick: leave out secrets = true and env = \"full\""),
            );
        }
        let timeout = match (timeout, kind.filter(|_| command.is_none())) {
            (None, Some(kind)) => Some((kind.timeout)(&settings).0),
            (Some(given), Some(kind)) => {
                let least = (kind.timeout)(&settings).1;
                if given <= least {
                    problems.add(
                        &at,
                        format!("timeout is {given} s, but the job {name} needs more than {least} s: its own time limits stop it first"),
                    );
                }
                Some(given)
            }
            (timeout, None) => timeout,
        };
        for key in fields.keys() {
            let more = kind
                .filter(|_| command.is_none())
                .map(|k| format!(", {}", k.keys.join(", ")))
                .unwrap_or_default();
            problems.add(
                &at,
                format!(
                    "`{key}` is not a key of a job. The keys: timeout, secrets, command, doc, env{more}"
                ),
            );
        }
        jobs.insert(
            name.clone(),
            Job {
                name,
                timeout,
                secrets,
                command: command.map(|c| c.trim().to_string()),
                doc,
                env,
                settings,
            },
        );
    }
    jobs
}

fn parse_envs(
    value: Option<toml::Value>,
    problems: &mut Problems,
) -> BTreeMap<String, Map<String, Value>> {
    let mut envs = BTreeMap::new();
    let Some(value) = value else {
        return envs;
    };
    let toml::Value::Table(table) = value else {
        problems.add(
            "[envs]",
            "it must be a table of tables, such as [envs.staging]",
        );
        return envs;
    };
    for (name, value) in table {
        match value {
            toml::Value::Table(fields) => {
                envs.insert(name, table_to_json(&fields));
            }
            _ => problems.add(&format!("[envs.{name}]"), "it must be a table"),
        }
    }
    envs
}

/// The keys that each kind of step can have, next to `id` and `kind`.
fn allowed_keys(kind: &str) -> &'static [&'static str] {
    match kind {
        "turn" => &[
            "agent",
            "turn",
            "reads",
            "expects",
            "with",
            "before",
            "after",
            "start",
            "next",
            "cap",
            "findings",
            "on",
            "rounds",
            "fail",
            "extra_rounds",
            "team",
            "when",
        ],
        "job" => &[
            "job", "args", "expects", "before", "after", "start", "next", "cap", "when",
        ],
        "human" => &["action", "expects", "after", "start", "next", "cap", "when"],
        "wait" => &[
            "start", "watch", "on", "every", "wait", "limit", "cap", "from",
        ],
        "ask" => &["docs", "on", "cap", "when"],
        "check" => &["doc", "line", "stop_line", "next", "fail", "cap", "when"],
        "act" => ACT_KEYS,
        _ => &[],
    }
}

/// The keys of an act step, next to `id` and `kind`.
const ACT_KEYS: &[&str] = &["run", "next", "cap", "when"];

/// The step kinds that the loader knows.
const STEP_KINDS: [&str; 8] = ["turn", "job", "human", "wait", "ask", "check", "act", "end"];

fn parse_steps(value: Option<toml::Value>, problems: &mut Problems) -> Vec<Step> {
    let Some(value) = value else {
        problems.add(
            "the steps",
            "the file has no [[step]]. Add the steps, the first one being the entry",
        );
        return Vec::new();
    };
    let toml::Value::Array(items) = value else {
        problems.add(
            "the steps",
            "`step` must be an array of tables: use [[step]]",
        );
        return Vec::new();
    };
    if items.is_empty() {
        problems.add(
            "the steps",
            "the file has no [[step]]. Add the steps, the first one being the entry",
        );
    }
    let mut steps = Vec::new();
    for (index, item) in items.into_iter().enumerate() {
        let toml::Value::Table(table) = item else {
            problems.add(
                &format!("step {}", index + 1),
                "it must be a table: use [[step]]",
            );
            continue;
        };
        if let Some(step) = parse_step(index, table, problems) {
            steps.push(step);
        }
    }
    steps
}

fn parse_step(index: usize, mut table: Table, problems: &mut Problems) -> Option<Step> {
    let number = format!("step {}", index + 1);
    let id = match string(table.remove("id"), &number, problems) {
        Some(id) => id,
        None => {
            problems.add(&number, "it has no id, such as id = \"build\"");
            return None;
        }
    };
    let at = format!("step `{id}`");
    if !valid_step_id(&id) {
        problems.add(
            &at,
            "a step id has letters, digits, '-', or '_', starts with a letter or a digit, and has up to 64 characters",
        );
    }
    let Some(kind) = string(table.remove("kind"), &at, problems) else {
        problems.add(
            &at,
            "it has no kind. Use turn, job, human, wait, ask, check, act, or end",
        );
        return None;
    };
    if !STEP_KINDS.contains(&kind.as_str()) {
        problems.add(
            &at,
            format!(
                "the kind `{kind}` is not known. Use turn, job, human, wait, ask, check, act, or end"
            ),
        );
        return None;
    }
    let allowed = allowed_keys(&kind);
    let extra: Vec<String> = table
        .keys()
        .filter(|k| !allowed.contains(&k.as_str()))
        .cloned()
        .collect();
    for key in extra {
        table.remove(&key);
        let why = match (kind.as_str(), key.as_str()) {
            ("end", "when") => "the end step has no when: a ticket always ends there".to_string(),
            ("wait", "when") => "a wait step has no when: it has no next to skip to".to_string(),
            ("act", _) => format!(
                "the key `{key}` does not belong to an act step. Its keys: {}",
                ACT_KEYS.join(", ")
            ),
            ("job" | "human" | "check" | "end", "on") => format!(
                "a {kind} step has no on: only a turn step moves by on (its findings, or its commits), and a wait step and an ask step by their values"
            ),
            _ => format!("the key `{key}` does not belong to a {kind} step"),
        };
        problems.add(&at, why);
    }
    let next = string(table.remove("next"), &at, problems);
    let cap = match table.remove("cap") {
        None => None,
        Some(toml::Value::Integer(n)) if (1..=1000).contains(&n) => Some(n.to_string()),
        Some(toml::Value::String(name)) => Some(name),
        Some(other) => {
            problems.add(
                &at,
                format!("cap is a key of [caps], such as cap = \"review_rounds\", or a number of entries, such as cap = 2, not `{other}`"),
            );
            None
        }
    };
    let when = strings(table.remove("when"), &format!("{at} when"), problems);
    let rounds = match table.remove("rounds") {
        None => DEFAULT_ROUNDS,
        Some(toml::Value::Integer(n)) if (1..=i64::from(MAX_ROUNDS)).contains(&n) => n as u32,
        Some(other) => {
            problems.add(
                &at,
                format!("rounds is the number of turns of one visit for failed end facts, from 1 to {MAX_ROUNDS}, such as rounds = 3, not `{other}`"),
            );
            DEFAULT_ROUNDS
        }
    };
    let (expects, expect_args) = expects(table.remove("expects"), &at, problems);
    let extra_rounds = extra_rounds(
        table.remove("extra_rounds"),
        rounds,
        &expects,
        &at,
        problems,
    );
    let fail_value = table.remove("fail");
    let (fail, check_fail) = if kind == "check" {
        let target = string(fail_value, &at, problems);
        if target.is_none() {
            problems.add(
                &at,
                "a check step names the step of a failed check in fail, such as fail = \"investigate\"",
            );
        }
        (BTreeMap::new(), target.unwrap_or_default())
    } else {
        (
            fail_routes(fail_value, &expects, &at, problems),
            String::new(),
        )
    };
    if kind != "turn" && expects.iter().any(|e| matches!(e, Expect::Gate(_))) {
        problems.add(
            &at,
            "a gate reads a document of the result of a turn, so only a turn step can have one",
        );
    }
    let after = entries(table.remove("after"), &format!("{at} after"), problems);
    let before = entries(table.remove("before"), &format!("{at} before"), problems);
    let start = entries(table.remove("start"), &format!("{at} start"), problems);
    let kind = match kind.as_str() {
        "turn" => {
            let turn = turn_step(&mut table, &at, problems, rounds);
            if !turn.team.is_empty() {
                if !expects.is_empty() {
                    problems.add(
                        &at,
                        "a step with team names the end facts of each member in team, so it has no expects",
                    );
                }
                if !fail.is_empty() || turn.findings.is_some() {
                    problems.add(
                        &at,
                        "a step with team has no fail and no findings: each member has its own end facts and rounds",
                    );
                }
            }
            StepKind::Turn(turn)
        }
        "ask" => {
            let docs = strings(table.remove("docs"), &format!("{at} docs"), problems);
            if docs.is_empty() {
                problems.add(
                    &at,
                    "an ask step names the documents of its cards, such as docs = [\"questions-analyst.json\"]",
                );
            }
            for name in &docs {
                if !valid_document_name(name) {
                    problems.add(
                        &at,
                        format!("docs names `{name}`, which is not a plain file name of the ticket folder"),
                    );
                }
            }
            StepKind::Ask(AskStep { docs })
        }
        "check" => {
            let doc = string(table.remove("doc"), &at, problems).unwrap_or_default();
            if !valid_document_name(&doc) {
                problems.add(
                    &at,
                    "a check step names the document that it reads, such as doc = \"plan.md\"",
                );
            }
            let line = string(table.remove("line"), &at, problems).unwrap_or_default();
            if line.trim().is_empty() || line.contains(['\n', '\r']) {
                problems.add(
                    &at,
                    "a check step names the line that moves the ticket on, such as line = \"Reproduced: yes\"",
                );
            }
            let stop_line = string(table.remove("stop_line"), &at, problems);
            if stop_line
                .as_deref()
                .is_some_and(|l| l.trim().is_empty() || l.contains(['\n', '\r']))
            {
                problems.add(
                    &at,
                    "stop_line is one line, such as stop_line = \"Decision: drop\"",
                );
            }
            StepKind::Check(CheckStep {
                doc,
                line: line.trim().to_string(),
                stop_line: stop_line.map(|l| l.trim().to_string()),
                fail: check_fail,
            })
        }
        "job" => {
            let job = string(table.remove("job"), &at, problems).unwrap_or_default();
            if job.is_empty() {
                problems.add(&at, "a job step names its job, such as job = \"proof\"");
            }
            let args = object(table.remove("args"), &format!("{at} args"), problems);
            StepKind::Job(JobStep { job, args })
        }
        "human" => {
            let action = string(table.remove("action"), &at, problems).unwrap_or_default();
            if !valid_name(&action, false) {
                problems.add(
                    &at,
                    "a human step names its action, such as action = \"approve-plan\"",
                );
            }
            StepKind::Human(HumanStep { action })
        }
        "wait" => StepKind::Wait(wait_step(&mut table, &at, problems)),
        "act" => StepKind::Act(act_step(&mut table, &at, problems)),
        _ => StepKind::End,
    };
    let on = match &kind {
        StepKind::Wait(_) | StepKind::Turn(_) | StepKind::Ask(_) => {
            on_table(table.remove("on"), &at, problems)
        }
        _ => BTreeMap::new(),
    };
    match &kind {
        StepKind::End => {}
        StepKind::Ask(_) => {
            let keys: Vec<&str> = on.keys().map(String::as_str).collect();
            if keys != [CHANGED, SAME] {
                problems.add(
                    &at,
                    format!(
                        "on of an ask step has exactly the keys changed and same, such as on = {{ changed = \"amend\", same = \"reproduced\" }}, not {}",
                        if keys.is_empty() { "none".to_string() } else { keys.join(", ") }
                    ),
                );
            }
        }
        StepKind::Turn(turn) if !on.is_empty() => {
            let keys: Vec<&str> = on.keys().map(String::as_str).collect();
            if on.contains_key(COMMITS) {
                // The route by commits: the head after the turn against the head at the entry.
                if on.contains_key(FINDINGS) {
                    problems.add(
                        &at,
                        "on moves a turn step by its commits or by its findings, never by both",
                    );
                } else if keys != [COMMITS, NONE] {
                    problems.add(
                        &at,
                        format!(
                            "on of a turn step that moves by its commits has exactly the keys commits and none, such as on = {{ commits = \"review-lanes\", none = \"ci\" }}, not {}",
                            keys.join(", ")
                        ),
                    );
                }
                if !turn.team.is_empty() {
                    problems.add(
                        &at,
                        "a step with team moves by next when each member is done, so it has no on",
                    );
                }
            } else {
                if turn.findings.is_none() {
                    problems.add(
                        &at,
                        "a turn step moves on its findings, so on needs the key findings, such as findings = \"qa\"",
                    );
                }
                if keys != [FINDINGS, NONE] {
                    problems.add(
                        &at,
                        format!(
                            "on of a turn step has exactly the keys findings and none, such as on = {{ findings = \"fix\", none = \"ready\" }}, or commits and none, such as on = {{ commits = \"review-lanes\", none = \"ci\" }}, not {}",
                            keys.join(", ")
                        ),
                    );
                }
            }
            if next.is_some() {
                problems.add(
                    &at,
                    "a turn step with on moves by on, so it has no next",
                );
            }
        }
        StepKind::Wait(_) if on.is_empty() => problems.add(
            &at,
            "a wait step moves on the value of watch, so it needs on, such as on = { green = \"ready\" }",
        ),
        StepKind::Wait(wait) => {
            for value in wait.wait.iter().flatten() {
                if on.contains_key(value) {
                    problems.add(
                        &at,
                        format!("`{value}` is in on and in wait. A value moves the ticket, or it waits"),
                    );
                }
            }
        }
        _ if next.is_none() => problems.add(
            &at,
            "it has no next. Each step but a wait step and the end names the step after it",
        ),
        _ => {}
    }
    Some(Step {
        id,
        kind,
        next,
        on,
        cap,
        rounds,
        extra_rounds,
        fail,
        expects,
        expect_args,
        after,
        before,
        start,
        when,
    })
}

/// The end facts of `expects`: each one a text such as `doc:pr.md`, or a table for a job with
/// arguments, such as `{ job = "proof", args = { plan = "{doc:approved-plan.md}" } }`. Gives the
/// end facts in their order and the arguments of each table, by the job name.
fn expects(
    value: Option<toml::Value>,
    at: &str,
    problems: &mut Problems,
) -> (Vec<Expect>, BTreeMap<String, Map<String, Value>>) {
    let mut list = Vec::new();
    let mut args = BTreeMap::new();
    let Some(value) = value else {
        return (list, args);
    };
    let toml::Value::Array(items) = value else {
        problems.add(&format!("{at} expects"), "it must be a list of strings");
        return (list, args);
    };
    for item in items {
        let expect = match item {
            toml::Value::String(text) => match Expect::parse(&text) {
                Ok(expect) => expect,
                Err(e) => {
                    problems.add(at, e);
                    continue;
                }
            },
            toml::Value::Table(table) if table.contains_key("gate") => {
                match gate(table, at, problems) {
                    Some(gate) => Expect::Gate(gate),
                    None => continue,
                }
            }
            toml::Value::Table(mut table) => {
                let job = string(table.remove("job"), at, problems);
                let given = object(
                    table.remove("args"),
                    &format!("{at} expects args"),
                    problems,
                );
                for key in table.keys() {
                    problems.add(
                        at,
                        format!("`{key}` is not a key of an end fact. A table of expects has job and args, such as {{ job = \"proof\", args = {{ plan = \"{{doc:approved-plan.md}}\" }} }}, or it names a gate, such as {{ gate = \"cr\", doc = \"cr.md\" }}"),
                    );
                }
                let Some(job) = job.filter(|j| valid_name(j, true)) else {
                    problems.add(
                        at,
                        "a table of expects names one job, such as { job = \"proof\", args = {} }",
                    );
                    continue;
                };
                if let Err(e) = check_doc_refs(&Value::Object(given.clone())) {
                    problems.add(at, format!("expects job:{job}: {e}"));
                }
                args.insert(job.clone(), given);
                Expect::Job(job)
            }
            other => {
                problems.add(
                    &format!("{at} expects"),
                    format!("`{other}` is not an end fact: use a text such as \"doc:pr.md\", or a table such as {{ job = \"proof\", args = {{}} }}"),
                );
                continue;
            }
        };
        // The text of an end fact names it: a gate by its kind and its document.
        if list
            .iter()
            .any(|e: &Expect| e.to_string() == expect.to_string())
        {
            problems.add(
                at,
                format!("expects names {expect} more than once. Name each end fact once"),
            );
            continue;
        }
        list.push(expect);
    }
    (list, args)
}

/// `extra_rounds` of a turn step: from 0 to [`MAX_EXTRA_ROUNDS`], only with a gate in
/// `expects`, and with `rounds` at most [`MAX_ROUNDS`] in all.
fn extra_rounds(
    value: Option<toml::Value>,
    rounds: u32,
    expects: &[Expect],
    at: &str,
    problems: &mut Problems,
) -> u32 {
    let extra = match value {
        None => return 0,
        Some(toml::Value::Integer(n)) if (0..=i64::from(MAX_EXTRA_ROUNDS)).contains(&n) => n as u32,
        Some(other) => {
            problems.add(
                at,
                format!("extra_rounds is the number of turns that a converging loop adds, from 0 to {MAX_EXTRA_ROUNDS}, such as extra_rounds = 2, not `{other}`"),
            );
            return 0;
        }
    };
    if extra > 0 && !expects.iter().any(|e| matches!(e, Expect::Gate(_))) {
        problems.add(
            at,
            "extra_rounds needs a gate in expects: a loop converges when the failing items of its gate get fewer",
        );
    }
    if rounds + extra > MAX_ROUNDS {
        problems.add(
            at,
            format!(
                "rounds + extra_rounds is {}, above {MAX_ROUNDS}",
                rounds + extra
            ),
        );
    }
    extra
}

/// `fail` of a turn step: a table from a gate of `expects` to a step. A key is the kind of the
/// gate, such as `qa`, or its end fact, such as `gate:qa:qa.md`, for a step with two gates of
/// one kind. Gives the routes by the end fact of the gate.
fn fail_routes(
    value: Option<toml::Value>,
    expects: &[Expect],
    at: &str,
    problems: &mut Problems,
) -> BTreeMap<String, String> {
    let mut routes = BTreeMap::new();
    let Some(value) = value else {
        return routes;
    };
    let toml::Value::Table(table) = value else {
        problems.add(
            at,
            "fail must be a table from a gate to a step, such as fail = { qa = \"fix-qa\" }",
        );
        return routes;
    };
    let gates: Vec<&Gate> = expects
        .iter()
        .filter_map(|e| match e {
            Expect::Gate(gate) => Some(gate),
            _ => None,
        })
        .collect();
    for (key, target) in table {
        let toml::Value::String(target) = target else {
            problems.add(
                at,
                format!("fail.{key} must name a step, such as \"fix-qa\""),
            );
            continue;
        };
        let named: Vec<&&Gate> = gates
            .iter()
            .filter(|g| g.kind() == key || g.fact() == key)
            .collect();
        match named.as_slice() {
            [gate] => {
                if routes.insert(gate.fact(), target).is_some() {
                    problems.add(
                        at,
                        format!("fail names the gate {} more than once", gate.fact()),
                    );
                }
            }
            [] if Expect::parse(&key).is_ok() => problems.add(
                at,
                format!("fail.{key}: only a gate of expects can have a fail route. A failed {key} resumes the turn, up to rounds"),
            ),
            [] => problems.add(
                at,
                format!("fail.{key} names no gate of expects. Name a gate by its kind, such as fail = {{ qa = \"fix-qa\" }}, or by its end fact, such as \"gate:qa:qa.md\""),
            ),
            [first, ..] => problems.add(
                at,
                format!(
                    "fail.{key} names {} gates of expects. Name one by its end fact, such as \"{}\"",
                    named.len(),
                    first.fact()
                ),
            ),
        }
    }
    routes
}

/// A gate of `expects`: `{ gate = "cr", doc = "cr.md", net_max = 4 }` or
/// `{ gate = "qa", doc = "qa.md", block = ["critical", "high", "medium"], notify = true }`. A
/// missing `doc` is `<gate>.md`, a missing `net_max` is [`CR_NET_MAX`], a missing `block` is
/// [`QA_BLOCK`], and a missing `notify` is false.
fn gate(mut table: Table, at: &str, problems: &mut Problems) -> Option<Gate> {
    let kind = string(table.remove("gate"), at, problems)?;
    let keys: &[&str] = match kind.as_str() {
        "cr" => &["doc", "net_max"],
        "qa" => &["doc", "block", "notify"],
        other => {
            problems.add(
                at,
                format!("the gate `{other}` is not known. Use gate = \"cr\" or gate = \"qa\""),
            );
            return None;
        }
    };
    let mut fine = true;
    for key in table.keys().filter(|k| !keys.contains(&k.as_str())) {
        problems.add(
            at,
            format!(
                "`{key}` is not a key of the gate {kind}. Its keys: gate, {}",
                keys.join(", ")
            ),
        );
        fine = false;
    }
    let doc = match table.remove("doc") {
        None => format!("{kind}.md"),
        Some(value) => string(Some(value), at, problems)?,
    };
    if !valid_document_name(&doc) {
        problems.add(
            at,
            format!("the gate {kind} reads `{doc}`, which is not a plain file name, such as doc = \"{kind}.md\""),
        );
        fine = false;
    }
    let rule = if kind == "cr" {
        let net_max = match table.remove("net_max") {
            None => CR_NET_MAX,
            Some(toml::Value::Integer(n)) => n as f64,
            Some(toml::Value::Float(n)) if n.is_finite() => n,
            Some(other) => {
                problems.add(
                    at,
                    format!("net_max of the gate cr must be a number, such as net_max = 4, not `{other}`"),
                );
                return None;
            }
        };
        GateRule::Cr { net_max }
    } else {
        let block = match table.remove("block") {
            None => QA_BLOCK.to_vec(),
            Some(value) => {
                let mut block = Vec::new();
                for word in strings(Some(value), &format!("{at} block"), problems) {
                    match Severity::parse(&word) {
                        Some(severity) if !block.contains(&severity) => block.push(severity),
                        Some(_) => {}
                        None => {
                            problems.add(
                                at,
                                format!("block of the gate qa names `{word}`, which is not a severity. Use critical, high, medium, or low"),
                            );
                            fine = false;
                        }
                    }
                }
                block.sort();
                block
            }
        };
        let notify = match table.remove("notify") {
            None => false,
            Some(toml::Value::Boolean(b)) => b,
            Some(other) => {
                problems.add(
                    at,
                    format!("notify of the gate qa must be true or false, not `{other}`"),
                );
                return None;
            }
        };
        GateRule::Qa { block, notify }
    };
    fine.then_some(Gate { doc, rule })
}

/// Tests each `{doc:NAME}` of the strings of a value: it closes, and it names a plain file of
/// the ticket folder.
fn check_doc_refs(value: &Value) -> Result<(), String> {
    match value {
        Value::String(text) => {
            let mut rest = text.as_str();
            while let Some(start) = rest.find("{doc:") {
                let after = &rest[start + 5..];
                let Some(end) = after.find('}') else {
                    return Err(format!("`{text}` opens {{doc: and never closes it"));
                };
                let name = &after[..end];
                if !valid_document_name(name) {
                    return Err(format!(
                        "`{{doc:{name}}}` does not name a plain file of the ticket folder"
                    ));
                }
                rest = &after[end + 1..];
            }
            Ok(())
        }
        Value::Array(items) => items.iter().try_for_each(check_doc_refs),
        Value::Object(map) => map.values().try_for_each(check_doc_refs),
        _ => Ok(()),
    }
}

/// The kind of turn of `turn = "..."`.
fn turn_kind(value: Option<toml::Value>, at: &str, problems: &mut Problems) -> TurnKind {
    match string(value, at, problems).as_deref() {
        Some("reader") => TurnKind::Reader,
        Some("worker") => TurnKind::Worker,
        Some("tester") => TurnKind::Tester,
        Some(other) => {
            problems.add(
                at,
                format!("the kind of turn `{other}` is not one of reader, worker, tester"),
            );
            TurnKind::Reader
        }
        None => {
            problems.add(
                at,
                "a turn step names its kind of turn, such as turn = \"worker\"",
            );
            TurnKind::Reader
        }
    }
}

/// A list of document names of the ticket folder, such as `reads`.
fn document_names(value: Option<toml::Value>, at: &str, problems: &mut Problems) -> Vec<String> {
    let names = strings(value, &format!("{at} reads"), problems);
    for name in &names {
        if !valid_document_name(name) {
            problems.add(
                at,
                format!(
                    "reads names `{name}`, which is not a plain file name of the ticket folder"
                ),
            );
        }
    }
    names
}

/// `team` of a turn step: a list of tables, each with `agent`, `turn`, `expects`, and the
/// optional `reads`, `when`, and `rounds`.
fn team(value: toml::Value, at: &str, problems: &mut Problems, rounds: u32) -> Vec<Member> {
    let toml::Value::Array(items) = value else {
        problems.add(
            at,
            "team must be a list of members, such as team = [{ agent = \"cause\", turn = \"reader\", expects = [\"doc:findings-cause.md\"] }]",
        );
        return Vec::new();
    };
    if items.is_empty() {
        problems.add(at, "team names no member");
    }
    let mut members = Vec::new();
    for (index, item) in items.into_iter().enumerate() {
        let toml::Value::Table(mut fields) = item else {
            problems.add(at, format!("team member {} must be a table", index + 1));
            continue;
        };
        let agent = string(fields.remove("agent"), at, problems).unwrap_or_default();
        let here = format!("{at} team member `{agent}`");
        if agent.is_empty() || !valid_name(&agent, false) {
            problems.add(
                &format!("{at} team member {}", index + 1),
                "a member names its agent, such as agent = \"cause\"",
            );
        }
        let turn = turn_kind(fields.remove("turn"), &here, problems);
        let reads = document_names(fields.remove("reads"), &here, problems);
        let (expects, expect_args) = expects(fields.remove("expects"), &here, problems);
        if expects.is_empty() {
            problems.add(
                &here,
                "a member names its end facts, such as expects = [\"doc:findings-cause.md\"]",
            );
        }
        let when = strings(fields.remove("when"), &format!("{here} when"), problems);
        let member_rounds = match fields.remove("rounds") {
            None => rounds,
            Some(toml::Value::Integer(n)) if (1..=i64::from(MAX_ROUNDS)).contains(&n) => n as u32,
            Some(other) => {
                problems.add(
                    &here,
                    format!("rounds is a number from 1 to {MAX_ROUNDS}, not `{other}`"),
                );
                rounds
            }
        };
        for key in fields.keys() {
            problems.add(
                &here,
                format!("`{key}` is not a key of a member. The keys: agent, turn, expects, reads, when, rounds"),
            );
        }
        members.push(Member {
            agent,
            turn,
            reads,
            expects,
            expect_args,
            when,
            rounds: member_rounds,
        });
    }
    members
}

fn turn_step(table: &mut Table, at: &str, problems: &mut Problems, rounds: u32) -> TurnStep {
    let members = match table.remove("team") {
        Some(value) => team(value, at, problems, rounds),
        None => Vec::new(),
    };
    if !members.is_empty() {
        for key in ["agent", "turn"] {
            if table.remove(key).is_some() {
                problems.add(
                    at,
                    format!("a step with team names the {key} of each member in team, so it has no {key}"),
                );
            }
        }
        let reads = document_names(table.remove("reads"), at, problems);
        let with = object(table.remove("with"), &format!("{at} with"), problems);
        let findings = string(table.remove("findings"), at, problems);
        return TurnStep {
            agent: String::new(),
            turn: TurnKind::Reader,
            reads,
            with,
            findings,
            team: members,
        };
    }
    let agent = string(table.remove("agent"), at, problems).unwrap_or_default();
    if agent.is_empty() {
        problems.add(
            at,
            "a turn step names its agent, such as agent = \"builder\"",
        );
    } else if !valid_name(&agent, false) {
        problems.add(at, format!("`{agent}` is not a valid agent name"));
    }
    let turn = turn_kind(table.remove("turn"), at, problems);
    let reads = document_names(table.remove("reads"), at, problems);
    let with = object(table.remove("with"), &format!("{at} with"), problems);
    let findings = string(table.remove("findings"), at, problems);
    if let Some(source) = &findings {
        if !valid_name(source, false) {
            problems.add(
                at,
                format!("findings names the source `{source}`, which is not a plain name such as review or qa"),
            );
        }
    }
    TurnStep {
        agent,
        turn,
        reads,
        with,
        findings,
        team: Vec::new(),
    }
}

/// `run` of an act step: one entry or more, each one a verb in the form of an entry of `after`.
/// A job is refused: a job step or `before` runs a job. The verbs themselves are tested
/// against `[verbs]` with the other links of the file.
fn act_step(table: &mut Table, at: &str, problems: &mut Problems) -> ActStep {
    let here = format!("{at} run");
    let value = table.remove("run");
    let missing = value.is_none();
    let empty = matches!(&value, Some(toml::Value::Array(items)) if items.is_empty());
    let run = entries(value, &here, problems);
    if missing {
        problems.add(
            at,
            "an act step names the verbs that it runs in run, such as run = [\"git.push\", { verb = \"pr.open\", args = { text = \"{doc:pr.md}\" } }]",
        );
    } else if empty {
        problems.add(
            at,
            "run of an act step names one verb or more, such as run = [\"git.push\"], not an empty list",
        );
    }
    for entry in &run {
        if entry.kind == EntryKind::Job {
            problems.add(
                &here,
                format!(
                    "`{}` is a job, and an act step runs only verbs. A job step or before of a turn step runs a job",
                    entry.label()
                ),
            );
        }
    }
    ActStep { run }
}

fn wait_step(table: &mut Table, at: &str, problems: &mut Problems) -> WaitStep {
    let watch = string(table.remove("watch"), at, problems).unwrap_or_default();
    let valid_watch = !watch.is_empty()
        && watch.split('.').all(|part| {
            !part.is_empty()
                && part
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_'))
        });
    if !valid_watch {
        problems.add(
            at,
            "a wait step names a path into the answer of observe in watch, such as watch = \"ci.state\"",
        );
    }
    let every = match table.remove("every") {
        None => DEFAULT_EVERY,
        Some(value) => {
            seconds(Some(value), &format!("{at} every"), problems).unwrap_or(DEFAULT_EVERY)
        }
    };
    let wait = table
        .remove("wait")
        .map(|value| strings(Some(value), &format!("{at} wait"), problems));
    let limit = seconds(table.remove("limit"), &format!("{at} limit"), problems);
    let from = match string(table.remove("from"), &format!("{at} from"), problems) {
        None => None,
        Some(text) => {
            let time = LocalTime::parse(&text);
            if time.is_none() {
                problems.add(
                    at,
                    format!("from is a local time of day, such as from = \"09:00\", not `{text}`"),
                );
            }
            time
        }
    };
    WaitStep {
        watch,
        every,
        wait,
        limit,
        from,
    }
}

fn on_table(
    value: Option<toml::Value>,
    at: &str,
    problems: &mut Problems,
) -> BTreeMap<String, String> {
    let mut on = BTreeMap::new();
    let Some(value) = value else {
        return on;
    };
    let toml::Value::Table(table) = value else {
        problems.add(at, "on must be a table, such as on = { green = \"ready\" }");
        return on;
    };
    for (value, target) in table {
        match target {
            toml::Value::String(target) => {
                on.insert(value, target);
            }
            _ => problems.add(
                at,
                format!("on.{value} must name a step, such as \"ready\""),
            ),
        }
    }
    on
}

/// The tests across the file: unique ids, targets, verbs, jobs, caps, one end, and the entry.
fn check_links(adapter: &Adapter, problems: &mut Problems) {
    let mut seen: BTreeMap<&str, usize> = BTreeMap::new();
    for (index, step) in adapter.steps.iter().enumerate() {
        if let Some(first) = seen.insert(&step.id, index) {
            problems.add(
                &format!("step `{}`", step.id),
                format!(
                    "the id is used by more than one step (steps {} and {}). Each step needs its own id",
                    first + 1,
                    index + 1
                ),
            );
        }
    }
    let ids: BTreeSet<&str> = adapter.steps.iter().map(|s| s.id.as_str()).collect();
    let caps = cap_names();
    for step in &adapter.steps {
        let at = format!("step `{}`", step.id);
        if let Some(next) = &step.next {
            if !ids.contains(next.as_str()) {
                problems.add(
                    &at,
                    format!("next names the step `{next}`, which does not exist"),
                );
            }
        }
        for (value, target) in &step.on {
            if !ids.contains(target.as_str()) {
                problems.add(
                    &at,
                    format!("on.{value} names the step `{target}`, which does not exist"),
                );
            }
        }
        for (fact, target) in &step.fail {
            if !ids.contains(target.as_str()) {
                problems.add(
                    &at,
                    format!("fail of {fact} names the step `{target}`, which does not exist"),
                );
            }
        }
        for (list, entries) in [
            ("after", &step.after),
            ("before", &step.before),
            ("start", &step.start),
        ] {
            for entry in entries {
                check_entry(adapter, entry, &format!("{at} {list}"), problems);
            }
        }
        if let StepKind::Act(act) = &step.kind {
            let here = format!("{at} run");
            for entry in act.run.iter().filter(|e| e.kind == EntryKind::Verb) {
                check_entry(adapter, entry, &here, problems);
                if adapter.verbs.get(&entry.name).is_some_and(|v| v.read) {
                    problems.add(
                        &here,
                        format!(
                            "`{}` is a read verb: it changes nothing outside, so an act step does not run it. A turn asks for it with a result of the kind requests",
                            entry.name
                        ),
                    );
                }
            }
        }
        for expect in &step.expects {
            if let Expect::Job(job) = expect {
                if !adapter.jobs.contains_key(job) {
                    problems.add(&at, missing_job(job, &format!("expects job:{job}")));
                }
            }
        }
        if let StepKind::Job(job) = &step.kind {
            if !job.job.is_empty() && !adapter.jobs.contains_key(&job.job) {
                problems.add(&at, missing_job(&job.job, "job"));
            }
        }
        if let Some(cap) = &step.cap {
            if !caps.contains(cap) && cap.parse::<u32>().is_err() {
                problems.add(
                    &at,
                    format!(
                        "cap `{cap}` is not a key of [caps] or a number. The keys: {}",
                        caps.join(", ")
                    ),
                );
            }
        }
        if let StepKind::Check(check) = &step.kind {
            if !check.fail.is_empty() && !ids.contains(check.fail.as_str()) {
                problems.add(
                    &at,
                    format!("fail names the step `{}`, which does not exist", check.fail),
                );
            }
        }
    }
    let ends: Vec<&str> = adapter
        .steps
        .iter()
        .filter(|s| s.kind == StepKind::End)
        .map(|s| s.id.as_str())
        .collect();
    match ends.len() {
        0 if !adapter.steps.is_empty() => problems.add(
            "the steps",
            "no step has the kind end. Add exactly one, such as id = \"done\" with kind = \"end\"",
        ),
        0 | 1 => {}
        _ => problems.add(
            "the steps",
            format!(
                "exactly one step has the kind end, but {} do: {}",
                ends.len(),
                ends.join(", ")
            ),
        ),
    }
    if let Some(first) = adapter.steps.first() {
        if first.kind == StepKind::End {
            problems.add(
                &format!("step `{}`", first.id),
                "the first step is the entry of a ticket, so it cannot be the end",
            );
        }
    }
}

fn check_entry(adapter: &Adapter, entry: &Entry, at: &str, problems: &mut Problems) {
    match entry.kind {
        EntryKind::Verb if !adapter.verbs.contains_key(&entry.name) => problems.add(
            at,
            format!(
                "the verb `{}` is not in [verbs]. Add [verbs.\"{}\"], or fix the name",
                entry.name, entry.name
            ),
        ),
        EntryKind::Job if !adapter.jobs.contains_key(&entry.name) => {
            problems.add(at, missing_job(&entry.name, &format!("job:{}", entry.name)))
        }
        _ => {}
    }
}

fn missing_job(job: &str, what: &str) -> String {
    format!("{what} names the job `{job}`, which is not in [jobs]. Add [jobs.{job}], which can be empty")
}

fn entries(value: Option<toml::Value>, at: &str, problems: &mut Problems) -> Vec<Entry> {
    let Some(value) = value else {
        return Vec::new();
    };
    let toml::Value::Array(items) = value else {
        problems.add(at, "it must be a list, such as [\"git.push\"]");
        return Vec::new();
    };
    items
        .into_iter()
        .filter_map(|item| entry(item, at, problems))
        .collect()
}

fn entry(item: toml::Value, at: &str, problems: &mut Problems) -> Option<Entry> {
    match item {
        toml::Value::String(text) => {
            let (kind, name) = match text.strip_prefix("job:") {
                Some(name) => (EntryKind::Job, name.to_string()),
                None => (EntryKind::Verb, text.clone()),
            };
            if !valid_name(&name, true) {
                problems.add(at, format!("`{text}` is not a verb name or job:<name>"));
                return None;
            }
            Some(Entry {
                kind,
                name,
                args: Map::new(),
            })
        }
        toml::Value::Table(mut table) => {
            let verb = string(table.remove("verb"), at, problems);
            let job = string(table.remove("job"), at, problems);
            let args = object(table.remove("args"), &format!("{at} args"), problems);
            for key in table.keys() {
                problems.add(
                    at,
                    format!("`{key}` is not a key of an entry. The keys: verb or job, and args"),
                );
            }
            let (kind, name) = match (verb, job) {
                (Some(verb), None) => (EntryKind::Verb, verb),
                (None, Some(job)) => (EntryKind::Job, job),
                _ => {
                    problems.add(
                        at,
                        "a table entry names one verb or one job, such as { verb = \"deploy\", args = { env = \"staging\" } }",
                    );
                    return None;
                }
            };
            if !valid_name(&name, true) {
                problems.add(at, format!("`{name}` is not a valid name"));
                return None;
            }
            Some(Entry { kind, name, args })
        }
        _ => {
            problems.add(
                at,
                "each entry is a name, or a table such as { verb = \"deploy\", args = {} }",
            );
            None
        }
    }
}

fn string(value: Option<toml::Value>, at: &str, problems: &mut Problems) -> Option<String> {
    match value? {
        toml::Value::String(text) => Some(text),
        other => {
            problems.add(at, format!("a value must be a string, not `{other}`"));
            None
        }
    }
}

fn strings(value: Option<toml::Value>, at: &str, problems: &mut Problems) -> Vec<String> {
    let Some(value) = value else {
        return Vec::new();
    };
    let toml::Value::Array(items) = value else {
        problems.add(at, "it must be a list of strings");
        return Vec::new();
    };
    items
        .into_iter()
        .filter_map(|item| match item {
            toml::Value::String(text) => Some(text),
            other => {
                problems.add(at, format!("`{other}` is not a string"));
                None
            }
        })
        .collect()
}

fn boolean(value: Option<toml::Value>, at: &str, problems: &mut Problems) -> Option<bool> {
    match value? {
        toml::Value::Boolean(b) => Some(b),
        other => {
            problems.add(at, format!("`{other}` must be true or false"));
            None
        }
    }
}

fn seconds(value: Option<toml::Value>, at: &str, problems: &mut Problems) -> Option<u64> {
    match value? {
        toml::Value::Integer(n) if n > 0 => Some(n as u64),
        other => {
            problems.add(at, format!("`{other}` must be a number of seconds above 0"));
            None
        }
    }
}

fn object(value: Option<toml::Value>, at: &str, problems: &mut Problems) -> Map<String, Value> {
    match value {
        None => Map::new(),
        Some(toml::Value::Table(table)) => table_to_json(&table),
        Some(other) => {
            problems.add(
                at,
                format!(
                    "it must be a table, such as {{ env = \"staging\" }}, not `{}`",
                    toml_to_json(&other)
                ),
            );
            Map::new()
        }
    }
}

/// A name of a verb, a job, an agent, or an action: letters, digits, `-`, `_`, and `.` when
/// `dot` is true, starting with a letter or a digit, up to 64 characters.
fn valid_name(name: &str, dot: bool) -> bool {
    (1..=64).contains(&name.len())
        && name.starts_with(|c: char| c.is_ascii_alphanumeric())
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_' || (dot && c == '.'))
}

/// A step id is a part of a turn id: the rule of [`crate::paths::TurnId`].
fn valid_step_id(id: &str) -> bool {
    valid_name(id, false) && !id.ends_with('-')
}

#[cfg(test)]
mod tests {
    use super::*;

    const GOOD: &str = r#"
name = "toy"
branch = "work/{key}"
base = "origin/trunk"

[[step]]
id = "refine"
kind = "turn"
agent = "scout"
turn = "reader"
reads = ["issue.json"]
expects = ["doc:refine.md"]
after = ["tracker.describe"]
next = "build"

[[step]]
id = "build"
kind = "turn"
agent = "builder"
turn = "worker"
before = ["job:lint", { job = "app-start", args = { env = "local" } }]
expects = ["commits", "clean", "doc:pr.md", "job:proof"]
after = ["git.push", { verb = "deploy", args = { env = "staging" } }]
with = { env = "staging", depth = 2 }
cap = "review_rounds"
next = "ci"

[[step]]
id = "ci"
kind = "wait"
start = ["ci.run"]
watch = "ci.state"
on = { green = "approve", red = "build" }
every = 30

[[step]]
id = "approve"
kind = "human"
action = "approve-merge"
next = "proof"

[[step]]
id = "proof"
kind = "job"
job = "proof"
args = { green = true }
next = "done"

[[step]]
id = "done"
kind = "end"

[verbs."tracker.describe"]
mode = "auto"
leaves = true

[verbs."git.push"]
mode = "shadow"
leaves = true
timeout = 90

[verbs.deploy]

[verbs."ci.run"]
mode = "auto"

[verbs."tracker.issue"]
read = true

[jobs.proof]
timeout = 1800

[jobs.lint]

[jobs.app-start]

[envs.staging]
url = "http://localhost:3000"
"#;

    fn parse(text: &str) -> Result<Adapter> {
        Adapter::parse(text, Path::new("/adapters/toy"))
    }

    fn error(text: &str) -> String {
        format!("{:#}", parse(text).unwrap_err())
    }

    /// The good file with one line replaced.
    fn with(old: &str, new: &str) -> String {
        assert!(GOOD.contains(old), "{old}");
        GOOD.replacen(old, new, 1)
    }

    #[test]
    fn a_good_file_parses_into_typed_values() {
        let adapter = parse(GOOD).unwrap();
        assert_eq!(adapter.name, "toy");
        assert_eq!(adapter.base, "origin/trunk");
        assert_eq!(adapter.scripts, Path::new("/adapters/toy"));
        assert_eq!(adapter.entry().id, "refine");
        assert_eq!(adapter.end().id, "done");
        let key = TicketKey::parse("DEMO-12").unwrap();
        assert_eq!(adapter.branch_for(&key), "work/DEMO-12");
        let build = adapter.step("build").unwrap();
        let StepKind::Turn(turn) = &build.kind else {
            panic!("build is a turn")
        };
        assert_eq!(turn.agent, "builder");
        assert_eq!(turn.turn, TurnKind::Worker);
        assert_eq!(
            turn.with,
            serde_json::json!({"env": "staging", "depth": 2})
                .as_object()
                .unwrap()
                .clone()
        );
        assert_eq!(
            build.expects,
            [
                Expect::Commits,
                Expect::Clean,
                Expect::Doc("pr.md".into()),
                Expect::Job("proof".into())
            ]
        );
        assert_eq!(build.before[0].label(), "job:lint");
        assert_eq!(build.before[1].kind, EntryKind::Job);
        assert_eq!(build.before[1].args["env"], "local");
        assert_eq!(build.after[1].name, "deploy");
        assert_eq!(build.after[1].args["env"], "staging");
        assert_eq!(build.cap.as_deref(), Some("review_rounds"));
        let ci = adapter.step("ci").unwrap();
        assert_eq!(
            ci.kind,
            StepKind::Wait(WaitStep {
                watch: "ci.state".into(),
                every: 30,
                wait: None,
                limit: None,
                from: None,
            })
        );
        assert_eq!(ci.on["red"], "build");
        assert_eq!(adapter.verbs["git.push"].mode, Mode::Shadow);
        assert_eq!(adapter.verbs["git.push"].timeout, Some(90));
        assert_eq!(
            adapter.verbs["deploy"].mode,
            Mode::Ask,
            "ask is the default"
        );
        assert!(adapter.verbs["tracker.issue"].read);
        assert_eq!(adapter.jobs["proof"].timeout, Some(1800));
        assert!(!adapter.jobs["proof"].secrets, "a job gets no secret");
        let reads = parse(&with("[jobs.lint]", "[jobs.lint]\nsecrets = true")).unwrap();
        assert!(reads.jobs["lint"].secrets);
        assert!(
            error(&with("[jobs.lint]", "[jobs.lint]\nsecrets = \"yes\"")).contains("[jobs.lint]")
        );
        assert_eq!(adapter.envs["staging"]["url"], "http://localhost:3000");
        let StepKind::Job(job) = &adapter.step("proof").unwrap().kind else {
            panic!("proof is a job step")
        };
        assert_eq!(job.args["green"], true);
    }

    #[test]
    fn missing_keys_take_their_defaults() {
        let text = "[[step]]\nid = \"a\"\nkind = \"human\"\naction = \"go\"\nnext = \"b\"\n[[step]]\nid = \"b\"\nkind = \"end\"\n";
        let adapter = parse(text).unwrap();
        assert_eq!(adapter.name, "toy", "the folder name");
        assert_eq!(adapter.branch, DEFAULT_BRANCH);
        assert_eq!(adapter.base, DEFAULT_BASE);
    }

    #[test]
    fn scripts_is_relative_to_the_adapter_folder_or_absolute() {
        let text = with("base = \"origin/trunk\"", "scripts = \"../demo\"");
        assert_eq!(
            parse(&text).unwrap().scripts,
            Path::new("/adapters/toy/../demo")
        );
        let text = with("base = \"origin/trunk\"", "scripts = \"/srv/public\"");
        assert_eq!(parse(&text).unwrap().scripts, Path::new("/srv/public"));
        assert_eq!(parse(&text).unwrap().builtin, None);
    }

    #[test]
    fn scripts_can_name_a_built_in_adapter() {
        let text = with(
            "base = \"origin/trunk\"",
            "scripts = \"builtin:jira-github\"",
        );
        let adapter = parse(&text).unwrap();
        assert_eq!(adapter.builtin.as_deref(), Some("jira-github"));
        assert_eq!(
            adapter.scripts,
            Path::new("/adapters/toy"),
            "the adapter folder"
        );
        assert_eq!(parse(GOOD).unwrap().builtin, None);
        let text = with(
            "base = \"origin/trunk\"",
            "scripts = \"builtin:jira-gitlab\"",
        );
        let err = error(&text);
        assert!(
            err.contains("the key `scripts`: `builtin:jira-gitlab` names no built-in adapter. The built-in adapters: builtin:jira-github"),
            "{err}"
        );
    }

    #[test]
    fn a_job_of_a_built_in_adapter_can_have_keys_of_its_own() {
        let builtin = with(
            "base = \"origin/trunk\"",
            "scripts = \"builtin:jira-github\"",
        );
        let text = builtin.replacen(
            "[jobs.lint]",
            "[jobs.precheck]\nlint = [{ run = \"bin/lint {files}\", globs = [\"**/*.rb\"] }]\nspecs = \"bin/rspec {files}\"\ncap = \"5m\"\ncap_max = \"20m\"\n\n[jobs.lint]",
            1,
        );
        let adapter = parse(&text).unwrap();
        let precheck = &adapter.jobs["precheck"];
        assert_eq!(
            precheck.env,
            JobEnv::Session,
            "a precheck runs as a session does"
        );
        assert_eq!(precheck.timeout, Some(1200 + 6 * 300));
        assert_eq!(precheck.settings["specs"], "bin/rspec {files}");
        assert_eq!(precheck.settings["lint"][0]["globs"][0], "**/*.rb");
        assert!(adapter.jobs["lint"].settings.is_empty());
        for (keys, expected) in [
            ("specs = 3", "specs is one command"),
            ("secrets = true", "the job precheck runs code of the project, so it gets no secret"),
            ("env = \"full\"", "env = \"full\" only to such a job"),
            ("timeout = 60\ncap_max = \"10m\"", "timeout is 60 s, but the job precheck needs more than 600 s"),
            ("lint = [\"ssh deploy@host make\"]", "the executor refuses: a shell on a remote machine (ssh)"),
            ("command = \"x\"\nspecs = \"y\"", "a job with command runs that command, so it has none of the keys of the job precheck"),
            ("speks = \"x\"", "`speks` is not a key of a job. The keys: timeout, secrets, command, doc, env, lint, specs"),
        ] {
            let text = builtin.replacen("[jobs.lint]", &format!("[jobs.precheck]\n{keys}\n\n[jobs.lint]"), 1);
            let err = error(&text);
            assert!(err.contains(expected), "expected `{expected}` in:\n{err}");
        }
        // A precheck of an adapter with scripts in bin/ is a job like any other.
        let err = error(&with(
            "[jobs.lint]",
            "[jobs.precheck]\nspecs = \"x\"\n\n[jobs.lint]",
        ));
        assert!(err.contains("`specs` is not a key of a job"), "{err}");
        // Any job can have the environment of a session, with no secret.
        let session = with("[jobs.lint]", "[jobs.lint]\nenv = \"session\"");
        assert_eq!(parse(&session).unwrap().jobs["lint"].env, JobEnv::Session);
        let err = error(&with(
            "[jobs.lint]",
            "[jobs.lint]\nenv = \"session\"\nsecrets = true",
        ));
        assert!(
            err.contains("env = \"session\" is the environment of a work session"),
            "{err}"
        );
    }

    #[test]
    fn each_error_names_the_file_the_step_and_the_problem() {
        let cases: Vec<(String, &str)> = vec![
            (with("id = \"ci\"", "id = \"refine\""), "step `refine`: the id is used by more than one step (steps 1 and 3)"),
            (with("kind = \"wait\"", "kind = \"loop\""), "step `ci`: the kind `loop` is not known"),
            (with("next = \"ci\"", "next = \"cii\""), "step `build`: next names the step `cii`, which does not exist"),
            (with("red = \"build\"", "red = \"fix\""), "step `ci`: on.red names the step `fix`, which does not exist"),
            (with("agent = \"scout\"\n", ""), "step `refine`: a turn step names its agent"),
            (with("turn = \"reader\"", "turn = \"writer\""), "step `refine`: the kind of turn `writer` is not one of reader, worker, tester"),
            (with("turn = \"reader\"\n", ""), "step `refine`: a turn step names its kind of turn"),
            (with("after = [\"tracker.describe\"]", "after = [\"tracker.close\"]"), "step `refine` after: the verb `tracker.close` is not in [verbs]"),
            (with("start = [\"ci.run\"]", "start = [\"ci.start\"]"), "step `ci` start: the verb `ci.start` is not in [verbs]"),
            (with("before = [\"job:lint\"", "before = [\"merge\""), "step `build` before: the verb `merge` is not in [verbs]"),
            (with("\"job:lint\"", "\"job:format\""), "step `build` before: job:format names the job `format`, which is not in [jobs]"),
            (with("\"job:proof\"]", "\"job:prove\"]"), "step `build`: expects job:prove names the job `prove`, which is not in [jobs]"),
            (with("job = \"proof\"\nargs", "job = \"other\"\nargs"), "step `proof`: job names the job `other`"),
            (with("cap = \"review_rounds\"", "cap = \"rounds\""), "step `build`: cap `rounds` is not a key of [caps]"),
            (with("kind = \"end\"", "kind = \"human\"\naction = \"x\"\nnext = \"refine\""), "the steps: no step has the kind end"),
            (with("action = \"approve-merge\"\nnext = \"proof\"", "action = \"approve-merge\"\nnext = \"proof\"\n[[step]]\nid = \"stop\"\nkind = \"end\""), "exactly one step has the kind end, but 2 do: stop, done"),
            (with("expects = [\"doc:refine.md\"]", "expects = [\"refined\"]"), "step `refine`: `refined` is not an end fact"),
            (with("expects = [\"doc:refine.md\"]", "expects = [\"doc:../x\"]"), "step `refine`: `doc:../x` names a document that is not a plain file name"),
            (with("watch = \"ci.state\"", "watch = \"ci..state\""), "step `ci`: a wait step names a path into the answer of observe"),
            (with("on = { green = \"approve\", red = \"build\" }", ""), "step `ci`: a wait step moves on the value of watch, so it needs on"),
            (with("next = \"proof\"", ""), "step `approve`: it has no next"),
            (with("action = \"approve-merge\"", "agent = \"builder\"\naction = \"approve-merge\""), "step `approve`: the key `agent` does not belong to a human step"),
            (with("branch = \"work/{key}\"", "branch = \"work/main\""), "the key `branch`: it must hold {key}"),
            (with("mode = \"shadow\"", "mode = \"sometimes\""), "[verbs.\"git.push\"]: the mode `sometimes` is not one of hold, shadow, ask, auto"),
            (with("[verbs.\"tracker.issue\"]\nread = true", "[verbs.\"tracker.issue\"]\nread = true\nleaves = true"), "a read verb changes nothing outside, so it has no mode and no leaves"),
            (with("timeout = 1800", "timeout = 0"), "[jobs.proof] timeout: `0` must be a number of seconds above 0"),
            (with("[jobs.lint]", "[jobs.lint]\nretries = 2"), "[jobs.lint]: `retries` is not a key of a job"),
            (with("name = \"toy\"", "name = \"toy\"\ncolour = \"blue\""), "the key `colour`: it is not a key of adapter.toml"),
            (with("{ verb = \"deploy\", args = { env = \"staging\" } }", "{ verb = \"deploy\", job = \"lint\" }"), "a table entry names one verb or one job"),
            (with("with = { env = \"staging\", depth = 2 }", "with = \"staging\""), "step `build` with: it must be a table"),
            (with("id = \"refine\"\n", ""), "step 1: it has no id"),
            (with("id = \"done\"", "id = \"done/now\""), "step `done/now`: a step id has letters, digits"),
        ];
        for (text, expected) in cases {
            let err = error(&text);
            assert!(
                err.starts_with("/adapters/toy/adapter.toml is not a valid adapter"),
                "{err}"
            );
            assert!(err.contains(expected), "expected `{expected}` in:\n{err}");
        }
    }

    #[test]
    fn the_entry_cannot_be_the_end_and_an_empty_file_has_no_steps() {
        let err = error("[[step]]\nid = \"done\"\nkind = \"end\"\n");
        assert!(
            err.contains("the first step is the entry of a ticket, so it cannot be the end"),
            "{err}"
        );
        let err = error("name = \"x\"\n");
        assert!(err.contains("the file has no [[step]]"), "{err}");
        let err = error("name = [");
        assert!(err.contains("is not valid TOML"), "{err}");
    }

    #[test]
    fn several_problems_are_reported_together() {
        let text = with("next = \"ci\"", "next = \"cii\"").replacen(
            "cap = \"review_rounds\"",
            "cap = \"rounds\"",
            1,
        );
        let err = error(&text);
        assert!(err.contains("next names the step `cii`"), "{err}");
        assert!(err.contains("cap `rounds`"), "{err}");
    }

    #[test]
    fn caps_are_the_keys_of_the_configuration() {
        let caps = cap_names();
        for cap in ["review_rounds", "ci_fix", "comments"] {
            assert!(caps.contains(&cap.to_string()), "{caps:?}");
        }
    }

    #[test]
    fn an_end_fact_job_can_have_arguments_with_documents() {
        let text = with(
            "expects = [\"commits\", \"clean\", \"doc:pr.md\", \"job:proof\"]",
            "expects = [\"commits\", { job = \"proof\", args = { plan = \"{doc:approved-plan.md}\", depth = 2 } }]",
        );
        let adapter = parse(&text).unwrap();
        let build = adapter.step("build").unwrap();
        assert_eq!(
            build.expects,
            [Expect::Commits, Expect::Job("proof".into())]
        );
        let args = build.expect_args_of("proof");
        assert_eq!(args["plan"], "{doc:approved-plan.md}");
        assert_eq!(args["depth"], 2);
        assert!(
            adapter
                .step("refine")
                .unwrap()
                .expect_args_of("proof")
                .is_empty(),
            "a plain end fact has no arguments"
        );
        let cases = [
            (
                "{ job = \"proof\", env = \"x\" }",
                "`env` is not a key of an end fact",
            ),
            ("{ args = {} }", "a table of expects names one job"),
            (
                "{ job = \"prove\" }",
                "expects job:prove names the job `prove`, which is not in [jobs]",
            ),
            (
                "{ job = \"proof\", args = { plan = \"{doc:plan.md\" } }",
                "opens {doc: and never closes it",
            ),
            (
                "{ job = \"proof\", args = { plan = \"{doc:../x}\" } }",
                "does not name a plain file",
            ),
            (
                "\"job:proof\", { job = \"proof\" }",
                "expects names job:proof more than once",
            ),
            ("3", "`3` is not an end fact"),
        ];
        for (entry, expected) in cases {
            let text = with(
                "expects = [\"commits\", \"clean\", \"doc:pr.md\", \"job:proof\"]",
                &format!("expects = [{entry}]"),
            );
            let err = error(&text);
            assert!(err.contains(expected), "expected `{expected}` in:\n{err}");
        }
    }

    #[test]
    fn a_turn_step_can_name_a_gate_of_cr_or_qa() {
        let line = "expects = [\"commits\", \"clean\", \"doc:pr.md\", \"job:proof\"]";
        let text = with(
            line,
            "expects = [\"commits\", { gate = \"cr\", doc = \"cr.md\", net_max = 4 }, { gate = \"qa\", doc = \"qa.md\", block = [\"high\", \"Critical\", \"medium\"], notify = true }]",
        );
        let adapter = parse(&text).unwrap();
        let build = adapter.step("build").unwrap();
        assert_eq!(
            build.expects,
            [
                Expect::Commits,
                Expect::Gate(Gate {
                    doc: "cr.md".into(),
                    rule: GateRule::Cr { net_max: 4.0 }
                }),
                Expect::Gate(Gate {
                    doc: "qa.md".into(),
                    rule: GateRule::Qa {
                        block: QA_BLOCK.to_vec(),
                        notify: true
                    }
                }),
            ]
        );
        assert_eq!(build.expects[1].to_string(), "gate:cr:cr.md");
        assert_eq!(build.expects[2].to_string(), "gate:qa:qa.md");

        // The keys that a gate leaves out take their defaults.
        let text = with(line, "expects = [{ gate = \"cr\" }, { gate = \"qa\" }]");
        let adapter = parse(&text).unwrap();
        assert_eq!(
            adapter.step("build").unwrap().expects,
            [
                Expect::Gate(Gate {
                    doc: "cr.md".into(),
                    rule: GateRule::Cr {
                        net_max: CR_NET_MAX
                    }
                }),
                Expect::Gate(Gate {
                    doc: "qa.md".into(),
                    rule: GateRule::Qa {
                        block: QA_BLOCK.to_vec(),
                        notify: false
                    }
                }),
            ]
        );
        let text = with(line, "expects = [{ gate = \"cr\", net_max = 2.5 }]");
        assert_eq!(
            parse(&text).unwrap().step("build").unwrap().expects[0],
            Expect::Gate(Gate {
                doc: "cr.md".into(),
                rule: GateRule::Cr { net_max: 2.5 }
            })
        );

        let cases = [
            ("{ gate = \"lint\" }", "the gate `lint` is not known"),
            (
                "{ gate = \"cr\", block = [\"low\"] }",
                "`block` is not a key of the gate cr. Its keys: gate, doc, net_max",
            ),
            (
                "{ gate = \"qa\", net_max = 3 }",
                "`net_max` is not a key of the gate qa",
            ),
            (
                "{ gate = \"cr\", net_max = \"four\" }",
                "net_max of the gate cr must be a number",
            ),
            (
                "{ gate = \"qa\", block = [\"severe\"] }",
                "block of the gate qa names `severe`, which is not a severity",
            ),
            (
                "{ gate = \"qa\", notify = \"yes\" }",
                "notify of the gate qa must be true or false",
            ),
            (
                "{ gate = \"cr\", doc = \"../cr.md\" }",
                "the gate cr reads `../cr.md`, which is not a plain file name",
            ),
            (
                "{ gate = \"cr\" }, { gate = \"cr\", net_max = 6 }",
                "expects names gate:cr:cr.md more than once",
            ),
        ];
        for (entry, expected) in cases {
            let err = error(&with(line, &format!("expects = [{entry}]")));
            assert!(err.contains(expected), "expected `{expected}` in:\n{err}");
        }
        // A gate reads the result of a turn, so a job step cannot have one.
        let err = error(&with(
            "job = \"proof\"\nargs = { green = true }",
            "job = \"proof\"\nargs = { green = true }\nexpects = [{ gate = \"cr\" }]",
        ));
        assert!(
            err.contains("step `proof`: a gate reads a document of the result of a turn"),
            "{err}"
        );
    }

    #[test]
    fn a_verb_can_run_once_for_a_ticket_and_a_read_verb_can_say_what_it_gives() {
        let text = with(
            "[verbs.deploy]\n",
            "[verbs.deploy]\nonce = true\n",
        )
        .replacen(
            "[verbs.\"tracker.issue\"]\nread = true",
            "[verbs.\"tracker.issue\"]\nread = true\nabout = \" One issue as issue-KEY.json. args: key \"",
            1,
        );
        let adapter = parse(&text).unwrap();
        assert!(adapter.verbs["deploy"].once);
        assert!(!adapter.verbs["git.push"].once, "false by default");
        assert_eq!(
            adapter.verbs["tracker.issue"].about.as_deref(),
            Some("One issue as issue-KEY.json. args: key")
        );
        assert_eq!(adapter.verbs["git.push"].about, None);
        for (old, new, expected) in [
            (
                "[verbs.deploy]\n",
                "[verbs.deploy]\nonce = \"yes\"\n",
                "must be true or false",
            ),
            (
                "[verbs.\"tracker.issue\"]\nread = true",
                "[verbs.\"tracker.issue\"]\nread = true\nonce = true",
                "once does not belong to it",
            ),
            (
                "mode = \"shadow\"\nleaves = true",
                "mode = \"shadow\"\nleaves = true\nonce = true",
                "a verb with leaves cannot have once",
            ),
            (
                "[verbs.deploy]\n",
                "[verbs.deploy]\nabout = \"two\\nlines\"\n",
                "about is one line",
            ),
            (
                "[verbs.deploy]\n",
                "[verbs.deploy]\nabout = \"  \"\n",
                "about is one line",
            ),
        ] {
            let err = error(&with(old, new));
            assert!(err.contains(expected), "expected `{expected}` in:\n{err}");
        }
    }

    #[test]
    fn a_wait_step_names_the_values_that_wait_and_its_limit() {
        let text = with(
            "every = 30",
            "every = 30\nwait = [\"pending\", \"none\"]\nlimit = 3600",
        );
        let adapter = parse(&text).unwrap();
        let StepKind::Wait(ci) = &adapter.step("ci").unwrap().kind else {
            panic!("ci is a wait step")
        };
        assert_eq!(
            ci.wait.as_deref(),
            Some(&["pending".to_string(), "none".to_string()][..])
        );
        assert_eq!(ci.limit, Some(3600));
        for (new, expected) in [
            ("wait = [\"red\"]", "`red` is in on and in wait"),
            (
                "wait = \"pending\"",
                "step `ci` wait: it must be a list of strings",
            ),
            (
                "limit = 0",
                "step `ci` limit: `0` must be a number of seconds above 0",
            ),
        ] {
            let err = error(&with("every = 30", &format!("every = 30\n{new}")));
            assert!(err.contains(expected), "expected `{expected}` in:\n{err}");
        }
        let err = error(&with(
            "action = \"approve-merge\"",
            "action = \"approve-merge\"\nlimit = 5",
        ));
        assert!(
            err.contains("the key `limit` does not belong to a human step"),
            "{err}"
        );
    }

    #[test]
    fn a_turn_step_with_findings_can_move_on_them() {
        let refine = "after = [\"tracker.describe\"]\nnext = \"build\"";
        let good = with(
            refine,
            "after = [\"tracker.describe\"]\nfindings = \"qa\"\non = { findings = \"build\", none = \"ci\" }",
        );
        let adapter = parse(&good).unwrap();
        let step = adapter.step("refine").unwrap();
        assert_eq!(step.next, None);
        assert_eq!(step.on[FINDINGS], "build");
        assert_eq!(step.on[NONE], "ci");
        for (new, expected) in [
            (
                "on = { findings = \"build\", none = \"ci\" }",
                "on needs the key findings",
            ),
            (
                "findings = \"qa\"\non = { findings = \"build\", none = \"ci\", red = \"ci\" }",
                "exactly the keys findings and none",
            ),
            (
                "findings = \"qa\"\non = { findings = \"build\" }",
                "exactly the keys findings and none",
            ),
            (
                "findings = \"qa\"\non = { findings = \"fix\", none = \"ci\" }",
                "on.findings names the step `fix`, which does not exist",
            ),
            (
                "findings = \"qa\"\non = { findings = \"build\", none = \"ci\" }\nnext = \"build\"",
                "a turn step with on moves by on, so it has no next",
            ),
        ] {
            let err = error(&with(
                refine,
                &format!("after = [\"tracker.describe\"]\n{new}"),
            ));
            assert!(err.contains(expected), "expected `{expected}` in:\n{err}");
        }
        let err = error(&with(
            "next = \"done\"",
            "on = { findings = \"done\", none = \"done\" }",
        ));
        assert!(
            err.contains("step `proof`: a job step has no on: only a turn step moves by on"),
            "{err}"
        );
    }

    #[test]
    fn a_turn_step_can_move_by_its_commits() {
        let build = "cap = \"review_rounds\"\nnext = \"ci\"";
        let good = with(
            build,
            "cap = \"review_rounds\"\non = { commits = \"refine\", none = \"ci\" }",
        );
        let adapter = parse(&good).unwrap();
        let step = adapter.step("build").unwrap();
        assert_eq!(step.next, None);
        assert_eq!(step.on[COMMITS], "refine");
        assert_eq!(step.on[NONE], "ci");
        // The findings key of a step only stores its findings, so it goes with commits too.
        let with_findings = with(
            build,
            "findings = \"review\"\non = { commits = \"refine\", none = \"ci\" }",
        );
        parse(&with_findings).unwrap();
        for (new, expected) in [
            (
                "on = { commits = \"refine\" }",
                "exactly the keys commits and none",
            ),
            (
                "on = { commits = \"refine\", none = \"ci\", red = \"ci\" }",
                "exactly the keys commits and none",
            ),
            (
                "findings = \"qa\"\non = { commits = \"refine\", findings = \"ci\", none = \"ci\" }",
                "on moves a turn step by its commits or by its findings, never by both",
            ),
            (
                "on = { commits = \"refine\", none = \"ci\" }\nnext = \"ci\"",
                "a turn step with on moves by on, so it has no next",
            ),
            (
                "on = { commits = \"lanes\", none = \"ci\" }",
                "on.commits names the step `lanes`, which does not exist",
            ),
            (
                "on = { commits = \"refine\", none = \"gone\" }",
                "on.none names the step `gone`, which does not exist",
            ),
        ] {
            let err = error(&with(build, &format!("cap = \"review_rounds\"\n{new}")));
            assert!(err.contains(expected), "expected `{expected}` in:\n{err}");
        }
        // Only a turn step moves by its commits.
        let err = error(&with(
            "action = \"approve-merge\"\nnext = \"proof\"",
            "action = \"approve-merge\"\non = { commits = \"proof\", none = \"proof\" }",
        ));
        assert!(
            err.contains("step `approve`: a human step has no on"),
            "{err}"
        );
        let err = error(&with(
            "next = \"done\"",
            "on = { commits = \"done\", none = \"done\" }",
        ));
        assert!(err.contains("step `proof`: a job step has no on"), "{err}");
        // A team moves by next when each member is done.
        let team = investigate_with(
            "]\nnext = \"analyze\"",
            "]\non = { commits = \"analyze\", none = \"analyze\" }",
        );
        let err = error(&team);
        assert!(
            err.contains("step `investigate`: a step with team moves by next when each member is done, so it has no on"),
            "{err}"
        );
    }

    #[test]
    fn a_turn_step_has_rounds_and_its_default_keeps_one_resume() {
        let adapter = parse(GOOD).unwrap();
        assert_eq!(adapter.step("build").unwrap().rounds, DEFAULT_ROUNDS);
        assert_eq!(DEFAULT_ROUNDS, 2, "the first turn and one resume");
        let text = with(
            "cap = \"review_rounds\"",
            "cap = \"review_rounds\"\nrounds = 3",
        );
        assert_eq!(parse(&text).unwrap().step("build").unwrap().rounds, 3);
        for bad in ["rounds = 0", "rounds = 21", "rounds = \"3\""] {
            let err = error(&with(
                "cap = \"review_rounds\"",
                &format!("cap = \"review_rounds\"\n{bad}"),
            ));
            assert!(
                err.contains("step `build`: rounds is the number of turns of one visit"),
                "{bad}: {err}"
            );
        }
        let err = error(&with(
            "action = \"approve-merge\"",
            "action = \"approve-merge\"\nrounds = 2",
        ));
        assert!(
            err.contains("the key `rounds` does not belong to a human step"),
            "{err}"
        );
    }

    #[test]
    fn extra_rounds_need_a_gate_and_stay_in_their_bounds() {
        let line = "expects = [\"commits\", \"clean\", \"doc:pr.md\", \"job:proof\"]";
        let gate = "expects = [\"commits\", { gate = \"cr\" }]";
        assert_eq!(parse(GOOD).unwrap().step("build").unwrap().extra_rounds, 0);
        let text = with(line, &format!("{gate}\nextra_rounds = 3"));
        assert_eq!(parse(&text).unwrap().step("build").unwrap().extra_rounds, 3);
        let err = error(&with(line, &format!("{line}\nextra_rounds = 2")));
        assert!(
            err.contains("step `build`: extra_rounds needs a gate in expects"),
            "{err}"
        );
        for bad in [
            "extra_rounds = 11",
            "extra_rounds = -1",
            "extra_rounds = \"2\"",
        ] {
            let err = error(&with(line, &format!("{gate}\n{bad}")));
            assert!(
                err.contains(
                    "extra_rounds is the number of turns that a converging loop adds, from 0 to 10"
                ),
                "{bad}: {err}"
            );
        }
        let err = error(&with(
            line,
            &format!("{gate}\nrounds = 15\nextra_rounds = 6"),
        ));
        assert!(
            err.contains("rounds + extra_rounds is 21, above 20"),
            "{err}"
        );
        let err = error(&with(
            "action = \"approve-merge\"",
            "action = \"approve-merge\"\nextra_rounds = 1",
        ));
        assert!(
            err.contains("the key `extra_rounds` does not belong to a human step"),
            "{err}"
        );
    }

    #[test]
    fn a_fail_route_names_a_gate_and_a_step() {
        let line = "expects = [\"commits\", \"clean\", \"doc:pr.md\", \"job:proof\"]";
        let gates =
            "expects = [\"commits\", { gate = \"qa\", doc = \"qa.md\" }, { gate = \"cr\" }]";
        // By the kind of the gate, or by its end fact.
        for fail in [
            "fail = { qa = \"approve\" }",
            "fail = { \"gate:qa:qa.md\" = \"approve\" }",
        ] {
            let text = with(line, &format!("{gates}\n{fail}"));
            let adapter = parse(&text).unwrap();
            let build = adapter.step("build").unwrap();
            assert_eq!(
                build.fail,
                [("gate:qa:qa.md".to_string(), "approve".to_string())].into(),
                "{fail}"
            );
        }
        assert!(parse(GOOD).unwrap().step("build").unwrap().fail.is_empty());
        for (fail, expected) in [
            (
                "fail = { qa = \"fix-qa\" }",
                "step `build`: fail of gate:qa:qa.md names the step `fix-qa`, which does not exist",
            ),
            (
                "fail = { commits = \"approve\" }",
                "fail.commits: only a gate of expects can have a fail route",
            ),
            (
                "fail = { \"job:proof\" = \"approve\" }",
                "fail.job:proof: only a gate of expects can have a fail route",
            ),
            (
                "fail = { lint = \"approve\" }",
                "fail.lint names no gate of expects",
            ),
            (
                "fail = { qa = \"approve\", \"gate:qa:qa.md\" = \"ci\" }",
                "fail names the gate gate:qa:qa.md more than once",
            ),
            ("fail = { qa = 3 }", "fail.qa must name a step"),
            (
                "fail = \"approve\"",
                "fail must be a table from a gate to a step",
            ),
        ] {
            let err = error(&with(line, &format!("{gates}\n{fail}")));
            assert!(err.contains(expected), "expected `{expected}` in:\n{err}");
        }
        // Two gates of one kind need the end fact.
        let two = "expects = [{ gate = \"qa\", doc = \"qa.md\" }, { gate = \"qa\", doc = \"qa-env.md\" }]";
        let err = error(&with(
            line,
            &format!("{two}\nfail = {{ qa = \"approve\" }}"),
        ));
        assert!(
            err.contains("fail.qa names 2 gates of expects. Name one by its end fact, such as \"gate:qa:qa.md\""),
            "{err}"
        );
        let text = with(
            line,
            &format!("{two}\nfail = {{ \"gate:qa:qa-env.md\" = \"approve\" }}"),
        );
        assert_eq!(
            parse(&text).unwrap().step("build").unwrap().fail["gate:qa:qa-env.md"],
            "approve"
        );
        // Only a turn step has gates, so only a turn step has fail.
        let err = error(&with(
            "action = \"approve-merge\"",
            "action = \"approve-merge\"\nfail = { qa = \"done\" }",
        ));
        assert!(
            err.contains("the key `fail` does not belong to a human step"),
            "{err}"
        );
    }

    #[test]
    fn a_wait_step_can_start_at_a_local_time() {
        let text = with("every = 30", "every = 30\nfrom = \"09:00\"");
        let adapter = parse(&text).unwrap();
        let StepKind::Wait(ci) = &adapter.step("ci").unwrap().kind else {
            panic!("ci is a wait step")
        };
        assert_eq!(ci.from, Some(LocalTime { hour: 9, minute: 0 }));
        assert_eq!(ci.from.unwrap().to_string(), "09:00");
        assert_eq!(
            LocalTime::parse("7:30"),
            Some(LocalTime {
                hour: 7,
                minute: 30
            })
        );
        for bad in ["24:00", "9", "09:60", "9:5", "nine", "09:00:00", ""] {
            assert_eq!(LocalTime::parse(bad), None, "{bad}");
        }
        let err = error(&with("every = 30", "every = 30\nfrom = \"9am\""));
        assert!(
            err.contains(
                "step `ci`: from is a local time of day, such as from = \"09:00\", not `9am`"
            ),
            "{err}"
        );
        let err = error(&with(
            "cap = \"review_rounds\"",
            "cap = \"review_rounds\"\nfrom = \"09:00\"",
        ));
        assert!(
            err.contains("the key `from` does not belong to a turn step"),
            "{err}"
        );
    }

    #[test]
    fn a_job_can_name_a_command_its_document_and_its_environment() {
        let text = with(
            "[jobs.lint]",
            "[jobs.lint]\ncommand = \" ./bin/check --all \"\ndoc = \"lint.txt\"\nenv = \"full\"\nsecrets = true",
        );
        let adapter = parse(&text).unwrap();
        let lint = &adapter.jobs["lint"];
        assert_eq!(lint.command.as_deref(), Some("./bin/check --all"));
        assert_eq!(lint.doc.as_deref(), Some("lint.txt"));
        assert_eq!(lint.env, JobEnv::Full);
        assert!(lint.secrets);
        let proof = &adapter.jobs["proof"];
        assert_eq!(proof.command, None);
        assert_eq!(proof.doc, None);
        assert_eq!(proof.env, JobEnv::Plain, "plain is the default");
        let plain = with(
            "[jobs.lint]",
            "[jobs.lint]\ncommand = \"true\"\nenv = \"plain\"",
        );
        assert_eq!(parse(&plain).unwrap().jobs["lint"].env, JobEnv::Plain);
        for (keys, expected) in [
            (
                "command = \"  \"",
                "command is the shell command of the job",
            ),
            (
                "command = \"x\"\nenv = \"some\"",
                "env `some` is not plain, session, or full",
            ),
            (
                "command = \"x\"\ndoc = \"../x\"",
                "doc names `../x`, which is not a plain file name",
            ),
            (
                "command = \"x\"\ndoc = \"plan.md\"",
                "doc names `plan.md`, which belongs to the person or the pipeline",
            ),
            (
                "doc = \"lint.txt\"",
                "doc and env belong to a job with command",
            ),
            ("env = \"full\"", "doc and env belong to a job with command"),
            (
                "command = 3",
                "[jobs.lint] command: a value must be a string",
            ),
        ] {
            let err = error(&with("[jobs.lint]", &format!("[jobs.lint]\n{keys}")));
            assert!(err.contains(expected), "expected `{expected}` in:\n{err}");
        }
    }

    #[test]
    fn end_facts_round_trip() {
        for text in [
            "doc:pr.md",
            "commits",
            "clean",
            "job:proof",
            "human:approve-plan",
        ] {
            assert_eq!(Expect::parse(text).unwrap().to_string(), text);
        }
        assert!(Expect::parse("job:").is_err());
        assert!(Expect::parse("human:a b").is_err());
    }

    /// The good file with three agents of its own: two readers on Claude and one on Codex.
    fn with_agents() -> String {
        let text = with(
            "[[step]]\nid = \"approve\"",
            "[[step]]\nid = \"lanes\"\nkind = \"turn\"\nteam = [\n  { agent = \"cr-a\", turn = \"reader\", expects = [\"doc:findings-cr-a.md\"] },\n  { agent = \"cr-codex\", turn = \"reader\", expects = [\"doc:findings-cr-codex.md\"] },\n]\nnext = \"approve\"\n\n[[step]]\nid = \"approve\"",
        );
        format!(
            "{text}\n[agents.cr-a]\nmodel = \"opus\"\nreason = \"It reads for faults.\"\n\n[agents.cr-codex]\nrole = \"roles/lane.md\"\nmodel = \"gpt-6.1-sol\"\neffort = \"xhigh\"\nrunner = \"codex\"\nfallback = {{ model = \"claude-opus-5-5\" }}\n\n[agents.cr-b]\nmodel = \"claude-sonnet-5-5\"\neffort = \"medium\"\n"
        )
    }

    #[test]
    fn an_adapter_defines_agents_with_a_role_file_a_model_and_a_runner() {
        let dir = tempfile::tempdir().unwrap();
        let adapter_dir = dir.path().join("toy");
        let adapter = Adapter::parse(&with_agents(), &adapter_dir).unwrap();
        let names: Vec<&str> = adapter.agents.keys().map(String::as_str).collect();
        assert_eq!(names, ["cr-a", "cr-b", "cr-codex"]);
        let a = &adapter.agents["cr-a"];
        assert_eq!(a.role, "roles/cr-a.md", "the default role file");
        assert_eq!(
            a.model, "claude-opus-5-5",
            "a short name becomes the full id"
        );
        assert_eq!(
            (a.effort, a.runner, &a.fallback),
            (Effort::High, Runner::Claude, &None)
        );
        assert_eq!(a.reason, "It reads for faults.");
        let codex = &adapter.agents["cr-codex"];
        assert_eq!(codex.role, "roles/lane.md");
        assert_eq!((codex.effort, codex.runner), (Effort::Xhigh, Runner::Codex));
        assert_eq!(
            codex.fallback,
            Some(Fallback {
                model: "claude-opus-5-5".into(),
                effort: Effort::High
            })
        );
        // The role files must exist, in the adapter folder or in the scripts folder.
        let err = adapter.check_agents().unwrap_err().to_string();
        assert!(
            err.contains("[agents.cr-a]: the role file roles/cr-a.md does not exist"),
            "{err}"
        );
        std::fs::create_dir_all(adapter_dir.join("roles")).unwrap();
        for name in ["cr-a.md", "cr-b.md", "lane.md"] {
            std::fs::write(adapter_dir.join("roles").join(name), "# Lane\n").unwrap();
        }
        adapter.check_agents().unwrap();
        assert_eq!(
            adapter.role_file("cr-codex"),
            Some(adapter_dir.join("roles/lane.md"))
        );
        // A role file of an agent of Vision still replaces its role file.
        assert_eq!(adapter.role_file("reviewer"), None);
        std::fs::write(adapter_dir.join("roles/reviewer.md"), "# Lead\n").unwrap();
        assert_eq!(
            adapter.role_file("reviewer"),
            Some(adapter_dir.join("roles/reviewer.md"))
        );
    }

    #[test]
    fn the_loader_refuses_a_wrong_agent_of_the_adapter() {
        let good = with_agents();
        let cases: Vec<(String, &str)> = vec![
            (
                GOOD.replacen("agent = \"scout\"", "agent = \"planner\"", 1),
                "step `refine`: the agent `planner` is no agent of Vision and no agent of [agents] of this file",
            ),
            (
                good.replacen("{ agent = \"cr-a\"", "{ agent = \"cr-x\"", 1),
                "step `lanes`: the agent `cr-x` is no agent of Vision and no agent of [agents]",
            ),
            (
                format!("{good}\n[agents.reviewer]\nmodel = \"opus\"\n"),
                "[agents.reviewer]: `reviewer` is an agent of Vision, so the adapter cannot define it. To give it another role file, write roles/reviewer.md",
            ),
            (
                good.replacen(
                    "{ agent = \"cr-codex\", turn = \"reader\"",
                    "{ agent = \"cr-codex\", turn = \"worker\"",
                    1,
                ),
                "[agents.cr-codex]: it runs on Codex, but the step `lanes` makes it a worker",
            ),
            (
                good.replacen("  { agent = \"cr-codex\", turn = \"reader\", expects = [\"doc:findings-cr-codex.md\"] },\n", "", 1),
                "[agents.cr-codex]: it runs on Codex, but no step makes it a reader",
            ),
            (
                good.replacen("model = \"claude-sonnet-5-5\"\n", "", 1),
                "[agents.cr-b]: it needs model",
            ),
            (
                good.replacen("effort = \"medium\"", "effort = \"huge\"", 1),
                "[agents.cr-b]: effort is low, medium, high, xhigh, or max, not `\"huge\"`",
            ),
            (
                good.replacen("runner = \"codex\"", "runner = \"gpt\"", 1),
                "[agents.cr-codex]: runner is claude or codex",
            ),
            (
                good.replacen("effort = \"medium\"", "effort = \"medium\"\nfallback = { model = \"opus\" }", 1),
                "[agents.cr-b]: fallback names the model with Claude that takes the work of a failed turn on Codex, so it needs runner = \"codex\"",
            ),
            (
                good.replacen("role = \"roles/lane.md\"", "role = \"../other/lane.md\"", 1),
                "[agents.cr-codex]: role names `../other/lane.md`",
            ),
            (
                good.replacen("role = \"roles/lane.md\"", "role = \"/etc/lane.md\"", 1),
                "[agents.cr-codex]: role names `/etc/lane.md`",
            ),
            (
                good.replacen("reason = \"It reads for faults.\"", "colour = \"red\"", 1),
                "[agents.cr-a]: `colour` is not a key of an agent. The keys: role, model, effort, runner, fallback, reason",
            ),
            (
                good.replacen("fallback = { model = \"claude-opus-5-5\" }", "fallback = { effort = \"high\" }", 1),
                "[agents.cr-codex] fallback: it names the model with Claude",
            ),
        ];
        for (text, expected) in cases {
            let err = error(&text);
            assert!(err.contains(expected), "expected `{expected}` in:\n{err}");
        }
    }

    #[test]
    fn the_review_team_demo_has_its_agents_two_teams_and_a_route_by_commits() {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("adapters/demo-review-team");
        let adapter = Adapter::read(&dir).unwrap();
        adapter.check_agents().unwrap();
        let names: Vec<&str> = adapter.agents.keys().map(String::as_str).collect();
        assert_eq!(names, ["cr-codex", "cr-correctness", "cr-design"]);
        assert_eq!(adapter.agents["cr-codex"].runner, Runner::Codex);
        assert!(adapter.agents["cr-codex"].fallback.is_some());
        assert_eq!(adapter.agents["cr-design"].runner, Runner::Claude);
        // The lead keeps the Reviewer of Vision, with the role file of the adapter.
        assert_eq!(
            adapter.role_file("reviewer"),
            Some(dir.join("roles/reviewer.md"))
        );
        let lanes = adapter.step("review-lanes").unwrap();
        assert_eq!(lanes.cap.as_deref(), Some("2"));
        assert_eq!(lanes.team().len(), 3);
        let challenge = adapter.step("review-challenge").unwrap();
        for member in challenge.team() {
            assert!(
                !member
                    .reads
                    .contains(&format!("findings-{}.md", member.agent)),
                "{member:?}"
            );
            assert_eq!(member.reads.len(), 2, "{member:?}");
        }
        let lead = adapter.step("review").unwrap();
        assert_eq!(lead.next, None);
        assert_eq!(lead.on[COMMITS], "review-lanes");
        assert_eq!(lead.on[NONE], "open-pr");
        assert!(lead
            .expects
            .iter()
            .any(|e| matches!(e, Expect::Gate(g) if g.doc == "cr.md")));
        // Nothing leaves the machine before the review: the build and the lead push nothing,
        // and the act step open-pr pushes the branch and opens the pull request.
        assert!(adapter.step("build").unwrap().after.is_empty());
        assert!(lead.after.is_empty());
        let open = adapter.step("open-pr").unwrap();
        let StepKind::Act(act) = &open.kind else {
            panic!("open-pr is an act step: {open:?}");
        };
        let run: Vec<String> = act.run.iter().map(Entry::label).collect();
        assert_eq!(run, ["git.push", "pr.open"]);
        assert_eq!(act.run[1].args["text"], "{doc:pr.md}");
        assert_eq!(open.next.as_deref(), Some("ci"));
    }

    #[test]
    fn an_act_step_runs_verbs_in_order_and_moves_to_next() {
        let act = "[[step]]\nid = \"open-pr\"\nkind = \"act\"\nrun = [\"git.push\", { verb = \"deploy\", args = { env = \"staging\", text = \"{doc:pr.md}\" } }]\ncap = 2\nwhen = [\"wide\"]\nnext = \"ci\"\n\n";
        let text = format!(
            "{}\n[width]\ndoc = \"plan.md\"\n",
            with(
                "[[step]]\nid = \"ci\"",
                &format!("{act}[[step]]\nid = \"ci\"")
            )
        );
        let adapter = parse(&text).unwrap();
        let step = adapter.step("open-pr").unwrap();
        assert_eq!(step.kind.name(), "act");
        let StepKind::Act(spec) = &step.kind else {
            panic!("open-pr is an act step: {step:?}");
        };
        let labels: Vec<String> = spec.run.iter().map(Entry::label).collect();
        assert_eq!(labels, ["git.push", "deploy"]);
        assert_eq!(spec.run[1].kind, EntryKind::Verb);
        assert_eq!(spec.run[1].args["env"], "staging");
        assert_eq!(spec.run[1].args["text"], "{doc:pr.md}");
        assert_eq!(step.next.as_deref(), Some("ci"));
        assert_eq!(step.cap.as_deref(), Some("2"));
        assert_eq!(step.when, ["wide"]);
        assert!(step.after.is_empty() && step.before.is_empty() && step.start.is_empty());
        assert!(step.on.is_empty() && step.expects.is_empty());
    }

    #[test]
    fn the_loader_refuses_a_wrong_act_step() {
        let good =
            "[[step]]\nid = \"open-pr\"\nkind = \"act\"\nrun = [\"git.push\"]\nnext = \"ci\"\n\n";
        let act = |body: &str| {
            with(
                "[[step]]\nid = \"ci\"",
                &format!(
                    "[[step]]\nid = \"open-pr\"\nkind = \"act\"\n{body}\n\n[[step]]\nid = \"ci\""
                ),
            )
        };
        parse(&with(
            "[[step]]\nid = \"ci\"",
            &format!("{good}[[step]]\nid = \"ci\""),
        ))
        .unwrap();
        let cases: Vec<(String, &str)> = vec![
            (
                act("run = [\"git.push\"]\nagent = \"builder\"\nnext = \"ci\""),
                "step `open-pr`: the key `agent` does not belong to an act step. Its keys: run, next, cap, when",
            ),
            (
                act("run = [\"git.push\"]\nafter = [\"git.push\"]\nnext = \"ci\""),
                "step `open-pr`: the key `after` does not belong to an act step",
            ),
            (
                act("run = [\"git.push\"]\non = { commits = \"ci\", none = \"ci\" }"),
                "step `open-pr`: the key `on` does not belong to an act step",
            ),
            (
                act("run = [\"git.push\"]\nexpects = [\"clean\"]\nnext = \"ci\""),
                "step `open-pr`: the key `expects` does not belong to an act step",
            ),
            (
                act("run = []\nnext = \"ci\""),
                "step `open-pr`: run of an act step names one verb or more, such as run = [\"git.push\"], not an empty list",
            ),
            (
                act("next = \"ci\""),
                "step `open-pr`: an act step names the verbs that it runs in run",
            ),
            (
                act("run = \"git.push\"\nnext = \"ci\""),
                "step `open-pr` run: it must be a list",
            ),
            (
                act("run = [\"git.push\", \"git.force\"]\nnext = \"ci\""),
                "step `open-pr` run: the verb `git.force` is not in [verbs]",
            ),
            (
                act("run = [\"tracker.issue\"]\nnext = \"ci\""),
                "step `open-pr` run: `tracker.issue` is a read verb: it changes nothing outside, so an act step does not run it",
            ),
            (
                act("run = [\"git.push\", \"job:lint\"]\nnext = \"ci\""),
                "step `open-pr` run: `job:lint` is a job, and an act step runs only verbs",
            ),
            (
                act("run = [{ job = \"lint\" }]\nnext = \"ci\""),
                "step `open-pr` run: `job:lint` is a job, and an act step runs only verbs",
            ),
            (
                act("run = [\"git.push\"]\nnext = \"nowhere\""),
                "step `open-pr`: next names the step `nowhere`, which does not exist",
            ),
            (
                act("run = [\"git.push\"]"),
                "step `open-pr`: it has no next",
            ),
            (
                act("run = [\"git.push\"]\nnext = \"ci\"\nwhen = [\"bug\"]"),
                "step `open-pr`: when names `bug`, but the adapter has no [kinds]",
            ),
            (
                act("run = [\"git.push\"]\nnext = \"ci\"\ncap = \"rounds\""),
                "step `open-pr`: cap `rounds` is not a key of [caps]",
            ),
        ];
        for (text, expected) in cases {
            let err = error(&text);
            assert!(err.contains(expected), "expected `{expected}` in:\n{err}");
        }
        // The unknown kind names each kind, act too.
        let err = error(&with("kind = \"wait\"", "kind = \"loop\""));
        assert!(
            err.contains("Use turn, job, human, wait, ask, check, act, or end"),
            "{err}"
        );
    }

    /// The adapter of the demo with the investigation before the plan.
    const INVESTIGATE: &str = include_str!("../../adapters/demo-investigate/adapter.toml");

    /// The investigation adapter with one text replaced.
    fn investigate_with(old: &str, new: &str) -> String {
        assert!(INVESTIGATE.contains(old), "{old}");
        INVESTIGATE.replacen(old, new, 1)
    }

    #[test]
    fn the_investigation_path_parses_kinds_width_teams_and_the_new_steps() {
        let adapter = parse(INVESTIGATE).unwrap();
        assert_eq!(adapter.kinds.as_ref().unwrap().of(Some("Bug")), "bug");
        assert_eq!(adapter.kinds.as_ref().unwrap().of(Some("Epic")), "feature");
        assert_eq!(
            adapter.kinds.as_ref().unwrap().names(),
            ["bug", "chore", "feature"]
        );
        let width = adapter.width.as_ref().unwrap();
        assert_eq!((width.doc.as_str(), width.files), ("plan.md", 3));
        assert_eq!(width.paths, ["db/**"]);
        let team = adapter.step("investigate").unwrap();
        let StepKind::Turn(turn) = &team.kind else {
            panic!("{team:?}");
        };
        let agents: Vec<&str> = turn.team.iter().map(|m| m.agent.as_str()).collect();
        assert_eq!(agents, ["repro", "cause", "history", "points", "scope"]);
        assert_eq!(turn.team[0].turn, TurnKind::Worker);
        assert_eq!(turn.team[0].when, ["bug"]);
        assert_eq!(adapter.step("critic").unwrap().when, ["wide"]);
        let StepKind::Ask(ask) = &adapter.step("questions").unwrap().kind else {
            panic!("questions");
        };
        assert_eq!(
            ask.docs,
            ["questions-analyst.json", "questions-critic.json"]
        );
        let check = adapter.step("reproduced").unwrap();
        let StepKind::Check(gate) = &check.kind else {
            panic!("reproduced");
        };
        assert_eq!(gate.line, "Reproduced: yes");
        assert_eq!(gate.stop_line.as_deref(), Some("Decision: drop"));
        assert_eq!(gate.fail, "investigate");
        assert_eq!(check.cap.as_deref(), Some("2"));
        // From the build on, the path is that of the demo: the act step open-pr pushes the
        // branch and opens the pull request after the review.
        assert!(adapter.step("build").unwrap().after.is_empty());
        let review = adapter.step("review").unwrap();
        assert!(review.after.is_empty());
        assert_eq!(review.next.as_deref(), Some("open-pr"));
        let open = adapter.step("open-pr").unwrap();
        assert!(matches!(open.kind, StepKind::Act(_)), "{open:?}");
        assert_eq!(open.next.as_deref(), Some("ci"));
    }

    #[test]
    fn the_loader_refuses_a_wrong_team_when_kinds_width_ask_or_check() {
        let two_workers = investigate_with(
            "{ agent = \"cause\", turn = \"reader\"",
            "{ agent = \"cause\", turn = \"worker\"",
        );
        let reads_other = investigate_with(
            "{ agent = \"cause\", turn = \"reader\", expects",
            "{ agent = \"cause\", turn = \"reader\", reads = [\"findings-repro.md\"], expects",
        );
        let cases: Vec<(String, &str)> = vec![
            (two_workers, "step `investigate`: the team has more than one worker or tester (repro, cause)"),
            (reads_other, "the member `cause` reads findings-repro.md, which the member `repro` expects"),
            (investigate_with("when = [\"wide\"]", "when = [\"huge\"]"), "step `critic`: when names `huge`, which is no kind of [kinds] and not wide or narrow"),
            (investigate_with("[width]\ndoc = \"plan.md\"", "[sizes]\ndoc = \"plan.md\""), "step `critic`: when names `wide`, but the adapter has no [width]"),
            (investigate_with("[kinds]\nmap", "[types]\nmap"), "when names `bug`, but the adapter has no [kinds], so a ticket has no kind"),
            (investigate_with("default = \"feature\"\n", ""), "[kinds]: it needs default"),
            (investigate_with("Task = \"chore\"", "Task = \"wide\""), "`wide` is a word of [width], so it cannot be a kind"),
            (investigate_with("same = \"reproduced\"", "kept = \"reproduced\""), "step `questions`: on of an ask step has exactly the keys changed and same"),
            (investigate_with("fail = \"investigate\"\ncap = 2", "fail = \"dig\"\ncap = 2"), "step `reproduced`: fail names the step `dig`, which does not exist"),
            (investigate_with("line = \"Reproduced: yes\"\n", ""), "step `reproduced`: a check step names the line that moves the ticket on"),
            (investigate_with("docs = [\"questions-analyst.json\", \"questions-critic.json\"]\n", ""), "step `questions`: an ask step names the documents of its cards"),
            (investigate_with("id = \"done\"\nkind = \"end\"", "id = \"done\"\nkind = \"end\"\nwhen = [\"bug\"]"), "step `done`: the end step has no when"),
            (investigate_with("reads = [\"issue.json\", \"refine.md\"]\nteam", "agent = \"repro\"\nreads = [\"issue.json\", \"refine.md\"]\nteam"), "step `investigate`: a step with team names the agent of each member in team, so it has no agent"),
            (investigate_with("{ agent = \"history\", turn = \"reader\"", "{ agent = \"cause\", turn = \"reader\""), "step `investigate`: the team names the agent `cause` more than once"),
        ];
        for (text, expected) in cases {
            let err = error(&text);
            assert!(err.contains(expected), "expected `{expected}` in:\n{err}");
        }
    }
}
