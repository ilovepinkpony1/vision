//! What the ticket row keeps for the pipeline: the flags, and the counters in `counts`.
//!
//! `tickets.counts` is a JSON object (see [`Counts`]): the number of entries into each step, the
//! current visit of the step with its progress, the reason of each flag, and the last facts of
//! `observe`. The progress that the ledger already proves (the turns, the operations, the
//! checks, the events) is never copied here, so that a crash cannot make the two disagree.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::adapter::{Step, StepKind};
use crate::executor::{CiFacts, PrFacts, TicketFacts};
use crate::ledger::TicketRow;

/// A rule stopped the ticket. `vision retry` clears it.
pub const NEEDS_YOU: &str = "needs-you";
/// A limit of the subscription refused a turn. The tick clears it by itself.
pub const WAITS_FOR_LIMIT: &str = "waits-for-limit";
/// `vision stop` stopped the ticket. `vision take` starts it again.
pub const STOPPED: &str = "stopped";
/// The person took over the session with `vision open`. `vision retry` clears it.
pub use crate::turn::PAUSED;

/// The counters of a ticket row.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Counts {
    /// The entries into each step, by step id. A step with a `cap` tests this number.
    pub entries: BTreeMap<String, u32>,
    /// The number of visits so far. Each visit has its own number in `visit.seq`.
    pub visits: u32,
    /// The visit of the current step.
    pub visit: Option<Visit>,
    /// Why each flag is set, and since when.
    pub reasons: BTreeMap<String, FlagReason>,
    /// The last facts of `observe` for the status view.
    pub seen: Option<Seen>,
    /// When the end step finished the ticket.
    pub finished: Option<String>,
}

/// One visit of a step: from its entry to the move to the next step.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Visit {
    /// The number of the visit in the ticket, from 1.
    pub seq: u32,
    pub step: String,
    /// The entry number of this step, from 1. A retry keeps it.
    pub n: u32,
    /// When the visit started.
    pub at: String,
    /// How the ticket entered: `take`, `next`, `on <value>`, `retry`, or `restart`.
    pub how: String,
    /// The number of turns of this step before the visit. The turns after it belong to it.
    pub turns: u32,
    /// The last commit at the entry. The end fact `commits` compares with it.
    pub base: Option<String>,
    /// The kind of the step: turn, job, human, wait, or end.
    pub kind: String,
    /// The agent of a turn step.
    pub agent: Option<String>,
    /// The action of a human step.
    pub action: Option<String>,
    /// True when the person let the ticket go on above the cap of the step.
    pub skip_cap: bool,
    /// The session that the first turn of the visit resumes.
    pub resume: Option<String>,
    /// The message of the first turn of the visit.
    pub message: Option<String>,
    /// More documents of the ticket folder for the later turns of the visit, such as the
    /// answers of read verbs.
    pub extra: Vec<String>,
    /// A wait step: when `observe` last ran, and the watched value then.
    pub observed: Option<String>,
    pub watched: Option<String>,
    /// A wait step: when the visit began to watch, after its `start` verbs. The `limit` of the
    /// step counts from here.
    pub since: Option<String>,
}

/// Why a flag is set.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct FlagReason {
    pub reason: String,
    pub at: String,
}

/// The last facts of `observe`, as the status view shows them.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Seen {
    pub at: String,
    pub tracker_status: Option<String>,
    pub pr: Option<PrFacts>,
    pub ci: Option<CiFacts>,
    pub envs: Map<String, Value>,
}

impl Seen {
    pub fn of(facts: &TicketFacts, at: &str) -> Seen {
        Seen {
            at: at.to_string(),
            tracker_status: facts.tracker_status.clone(),
            pr: facts.pr.clone(),
            ci: facts.ci.clone(),
            envs: facts.envs.clone(),
        }
    }
}

impl Counts {
    /// The counters of a row. A missing or broken value is empty, so that the next tick
    /// repairs the visit.
    pub fn of(row: &TicketRow) -> Counts {
        row.counts
            .as_deref()
            .and_then(|text| serde_json::from_str(text).ok())
            .unwrap_or_default()
    }

    pub fn text(&self) -> String {
        serde_json::to_string(self).unwrap_or_else(|_| "{}".to_string())
    }

    /// Starts a new visit of `step`. A visit that counts as an entry adds one to the entries of
    /// the step; a retry or a restart does not.
    pub fn enter(
        &mut self,
        step: &Step,
        how: &str,
        at: &str,
        turns: u32,
        base: Option<String>,
        counts_entry: bool,
    ) -> Visit {
        self.visits += 1;
        let entries = self.entries.entry(step.id.clone()).or_insert(0);
        if counts_entry || *entries == 0 {
            *entries += 1;
        }
        let (agent, action) = match &step.kind {
            StepKind::Turn(turn) => (Some(turn.agent.clone()), None),
            StepKind::Human(human) => (None, Some(human.action.clone())),
            _ => (None, None),
        };
        let visit = Visit {
            seq: self.visits,
            step: step.id.clone(),
            n: *entries,
            at: at.to_string(),
            how: how.to_string(),
            turns,
            base,
            kind: step.kind.name().to_string(),
            agent,
            action,
            skip_cap: !counts_entry,
            ..Visit::default()
        };
        self.visit = Some(visit.clone());
        visit
    }
}

/// The flags of a row, as the JSON list of `tickets.flags`.
pub fn flags_of(row: &TicketRow) -> Vec<String> {
    row.flags
        .as_deref()
        .filter(|t| !t.trim().is_empty())
        .and_then(|t| serde_json::from_str(t).ok())
        .unwrap_or_default()
}

pub fn flags_text(flags: &[String]) -> String {
    serde_json::to_string(flags).unwrap_or_else(|_| "[]".to_string())
}

/// What a step waits on, for the `spans` row.
pub fn waits_on(step: &Step) -> String {
    match &step.kind {
        StepKind::Turn(turn) => format!("agent:{}", turn.agent),
        StepKind::Job(job) => format!("job:{}", job.job),
        StepKind::Human(human) => format!("person:{}", human.action),
        StepKind::Wait(wait) => format!("watch:{}", wait.watch),
        StepKind::End => "end".to_string(),
    }
}

/// The command that clears a flag, for people.
pub fn clear_command(flag: &str, key: &str) -> String {
    match flag {
        NEEDS_YOU | PAUSED => format!("vision retry {key}"),
        STOPPED => format!("vision take {key}"),
        WAITS_FOR_LIMIT => "none: the tick clears it when the limit allows a turn again".into(),
        _ => format!("vision retry {key}"),
    }
}
