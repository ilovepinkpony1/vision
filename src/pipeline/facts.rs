//! The facts of the state of each ticket as data, for the app. The column NOW of `vision
//! status` says the same thing in one line for the command line, with the names of commands;
//! these facts let the app say it in its own words, with its own keys. They read only the
//! ledger, the ticket rows, the ticket folders, and the adapter, as the status view does, and
//! a broken adapter only leaves the facts that need it empty.

use std::collections::BTreeMap;

use anyhow::Result;
use serde::Serialize;

use super::decide::OpFact;
use super::machine::{op_facts, visit_turns, Ticket};
use super::state::{Visit, WAITS_FOR_LIMIT};
use crate::adapter::{Adapter, EntryKind, StepKind};
use crate::ctx::Ctx;
use crate::executor::{self, short, CiFacts, Outcome, PrFacts};
use crate::ledger::questions::STATE_JUDGING;
use crate::ledger::turns::Window;
use crate::ledger::Ledger;
use crate::turn::{RunningTurn, TurnStatus};
use crate::util::time;

/// The facts of one ticket.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct StateFacts {
    pub key: String,
    /// The agent of a turn step.
    pub agent: Option<String>,
    /// The action of a human step, such as `approve-plan`.
    pub action: Option<String>,
    /// The work turn that runs now.
    pub work: Option<TurnFact>,
    /// The turn of the Judge that runs now for cards of the ticket.
    pub judge: Option<TurnFact>,
    /// The cards of the ticket that the Judge reads now.
    pub judging_cards: usize,
    /// The last turn of the visit ended, and the next pass tests its end facts: a turn that
    /// ended well, or a turn whose session ended and that no pass collected yet.
    pub testing: bool,
    /// The last turn of the visit held its end facts, so a retry runs the writes of the step
    /// again instead of a new turn.
    pub held: bool,
    /// The first `before` or `after` entry of the step that is not done.
    pub entry: Option<EntryFact>,
    /// The cards of the ticket that wait for an answer.
    pub open_cards: usize,
    /// The verb of an operation of the ticket whose outside state is not known.
    pub uncertain: Option<String>,
    /// A wait step: what it watches and what it saw.
    pub wait: Option<WaitFacts>,
    /// The window that holds the ticket back, when it has the flag `waits-for-limit`.
    pub limit: Option<LimitFact>,
    /// The end step finished the ticket.
    pub finished: bool,
    /// The pull request that was merged, when the ticket finished with one.
    pub merged_pr: Option<i64>,
    /// The entries into each step, by step id.
    pub entries: BTreeMap<String, u32>,
    /// The work turns of the ticket so far, the running one too.
    pub turns: usize,
    /// The agents that did a turn for the ticket, the Judge too, in the order of their first
    /// turn.
    pub agents: Vec<String>,
    /// The verb of an entry of the step that the mode shadow or hold kept from running: its
    /// first write waits for the decision of the person.
    pub first_write: Option<String>,
}

/// A turn that runs now.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct TurnFact {
    pub id: String,
    pub agent: String,
    pub step: String,
    pub model: Option<String>,
    pub effort: Option<String>,
    /// RFC 3339 UTC.
    pub started: Option<String>,
    /// The number of this turn among the work turns of the ticket, from 1.
    pub n: usize,
}

/// One `before` or `after` entry with its state.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct EntryFact {
    /// Such as `git.push` or `job:second-review`.
    pub label: String,
    /// True for a `before` entry, false for an `after` entry.
    pub before: bool,
    pub state: EntryState,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "state", content = "text", rename_all = "lowercase")]
pub enum EntryState {
    /// It runs at the next pass.
    Next,
    /// It runs, or it was recorded and runs at the next pass.
    Runs,
    /// It waits for the approval of the person.
    Waits,
    /// Its outside state is not known.
    Uncertain,
    /// Its arguments cannot be built.
    Cannot(String),
    /// Another outcome, in words, such as `failed`.
    Other(String),
}

/// What a wait step watches, and the facts of the last look.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct WaitFacts {
    /// The path into the answer of `observe`, such as `ci.state`.
    pub watch: Option<String>,
    /// The value of the last look.
    pub value: Option<String>,
    /// When the visit began to watch, and when it looked last.
    pub since: Option<String>,
    pub observed: Option<String>,
    /// A step with `from`: the moment (RFC 3339 UTC) when it starts. The step does nothing
    /// before it.
    pub opens: Option<String>,
    pub pr: Option<PrFacts>,
    pub ci: Option<CiFacts>,
    /// The checks of the kind of the first part of the path (such as `ci`) at the commit of
    /// the last look.
    pub checks: Checks,
}

/// Checks by their result.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
pub struct Checks {
    pub passed: usize,
    pub running: usize,
    pub failed: usize,
}

impl Checks {
    pub fn total(&self) -> usize {
        self.passed + self.running + self.failed
    }

    /// Adds one check by its result.
    pub fn add(&mut self, result: &str) {
        match result {
            "pass" | "passed" | "green" | "success" | "ok" => self.passed += 1,
            "fail" | "failed" | "red" | "failure" | "error" => self.failed += 1,
            _ => self.running += 1,
        }
    }
}

/// A subscription window that holds a ticket back.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct LimitFact {
    /// `five_hour` or `seven_day`.
    pub window: String,
    pub part: f64,
    /// The part at which new turns stop.
    pub stop: f64,
    /// When the window resets, RFC 3339 UTC, when the ledger has it.
    pub resets: Option<String>,
}

/// The facts of each ticket that is not reserved, by key.
pub fn state_facts(ctx: &Ctx, ledger: &Ledger) -> Result<Vec<StateFacts>> {
    let running = crate::turn::running_turns(ctx, ledger)?;
    // A broken adapter only leaves the facts that need it empty.
    let adapter = crate::adapter::load(ctx).ok();
    let mut out = Vec::new();
    for row in ledger.tickets()? {
        let Ok(ticket) = Ticket::of(row) else {
            continue;
        };
        if ticket.key.is_reserved() {
            continue;
        }
        out.push(facts_of(ctx, ledger, &ticket, &running, adapter.as_ref())?);
    }
    Ok(out)
}

/// The facts of one ticket, or `None` when the ledger has no such ticket.
pub fn state_facts_of(ctx: &Ctx, ledger: &Ledger, key: &str) -> Result<Option<StateFacts>> {
    let Some(row) = ledger.ticket(key)? else {
        return Ok(None);
    };
    let Ok(ticket) = Ticket::of(row) else {
        return Ok(None);
    };
    let running = crate::turn::running_turns(ctx, ledger)?;
    let adapter = crate::adapter::load(ctx).ok();
    Ok(Some(facts_of(
        ctx,
        ledger,
        &ticket,
        &running,
        adapter.as_ref(),
    )?))
}

fn facts_of(
    ctx: &Ctx,
    ledger: &Ledger,
    ticket: &Ticket,
    running: &[RunningTurn],
    adapter: Option<&Adapter>,
) -> Result<StateFacts> {
    let key = ticket.key.as_str();
    let visit = ticket.visit();
    let turns = ledger.turns_of_ticket(key)?;
    let is_alone = |agent: Option<&str>| {
        agent
            .and_then(crate::models::agent)
            .is_some_and(|spec| spec.alone)
    };
    let mut agents: Vec<String> = Vec::new();
    for turn in &turns {
        if let Some(agent) = turn.agent.as_deref().filter(|a| !a.is_empty()) {
            if !agents.iter().any(|a| a == agent) {
                agents.push(agent.to_string());
            }
        }
    }
    let work_ids: Vec<&str> = turns
        .iter()
        .filter(|t| !is_alone(t.agent.as_deref()))
        .map(|t| t.id.as_str())
        .collect();
    let fact_of = |run: &RunningTurn| {
        let row = turns.iter().find(|t| t.id == run.id);
        TurnFact {
            id: run.id.clone(),
            agent: run.agent.clone().unwrap_or_default(),
            step: run.step.clone().unwrap_or_default(),
            model: row.and_then(|r| r.model.clone()),
            effort: row.and_then(|r| r.effort.clone()),
            started: run.started.clone(),
            n: work_ids
                .iter()
                .position(|id| *id == run.id)
                .map_or(work_ids.len(), |i| i + 1),
        }
    };
    let mine: Vec<&RunningTurn> = running.iter().filter(|t| t.ticket == key).collect();
    let work = mine
        .iter()
        .rev()
        .find(|t| !is_alone(t.agent.as_deref()))
        .map(|t| fact_of(t));
    let judge = mine
        .iter()
        .rev()
        .find(|t| t.agent.as_deref() == Some("judge"))
        .map(|t| fact_of(t));
    let judging_cards = if judge.is_some() {
        ledger
            .questions_in_states(&[STATE_JUDGING])?
            .iter()
            .filter(|q| q.ticket == key)
            .count()
    } else {
        0
    };
    let mut facts = StateFacts {
        key: key.to_string(),
        agent: visit.agent.clone(),
        action: visit.action.clone(),
        work,
        judge,
        judging_cards,
        open_cards: usize::try_from(ledger.open_cards(key)?).unwrap_or(0),
        finished: ticket.counts.finished.is_some(),
        merged_pr: ticket
            .counts
            .seen
            .as_ref()
            .and_then(|s| s.pr.as_ref())
            .filter(|pr| pr.merged)
            .map(|pr| pr.number),
        entries: ticket.counts.entries.clone(),
        turns: work_ids.len(),
        agents,
        ..StateFacts::default()
    };
    for op in executor::operations_of(ledger, &ticket.key)?
        .into_iter()
        .rev()
    {
        if op.state == executor::OpState::Uncertain {
            facts.uncertain = Some(op.verb.clone());
            break;
        }
    }
    if ticket.flags.iter().any(|f| f == WAITS_FOR_LIMIT) {
        facts.limit = limit_fact(ctx, ledger)?;
    }
    if visit.kind == "turn" && !visit.step.is_empty() {
        let visit_rows = visit_turns(ledger, &ticket.key, &visit.step, visit.turns)?;
        if let Some(last) = visit_rows.last() {
            let done = last.status.as_deref() == Some(TurnStatus::Done.as_str());
            // A turn whose session ended and that no pass collected yet: the next pass reads
            // its result and tests it.
            let ended = last.status.as_deref() == Some(TurnStatus::Running.as_str())
                && facts.work.is_none();
            facts.testing = ended || (done && last.end_fact.is_none());
            facts.held = done && last.end_fact.as_deref() == Some("held");
        }
        if let Some(adapter) = adapter {
            facts.entry = entry_fact(ctx, ledger, ticket, &visit, adapter, &visit_rows)?;
        }
        facts.first_write = match &facts.entry {
            Some(EntryFact {
                label,
                state: EntryState::Other(text),
                ..
            }) if kept_back(text) => Some(label.clone()),
            _ => None,
        };
    }
    if visit.kind == "wait" {
        facts.wait = Some(wait_facts(ledger, ticket, &visit, adapter)?);
        if let Some(step) = adapter.and_then(|a| a.step(&visit.step)) {
            facts.first_write = op_facts(ctx, ledger, ticket, &step.start, None)?
                .into_iter()
                .find(|(_, fact)| {
                    matches!(fact, OpFact::Known { outcome, .. } if kept_back(&outcome.to_string()))
                })
                .map(|(label, _)| label);
        }
    }
    Ok(facts)
}

/// True for the outcome of an operation that the mode shadow or hold kept from running.
fn kept_back(outcome: &str) -> bool {
    outcome == Outcome::Shadowed.name() || outcome == Outcome::Held.name()
}

/// The operations of the step of a ticket that the mode shadow or hold kept from running, as
/// the next pass would ask for them again: each verb entry of the visit of now (the `before`
/// entries before its first turn, the `after` entries after a turn that held, or the `start`
/// entries of a wait step) whose stored operation at the commit of now is pending in one of
/// these modes. The app shows each one on the card of a first write.
pub fn open_writes(
    ctx: &Ctx,
    ledger: &Ledger,
    key: &str,
) -> Result<Vec<executor::OperationResult>> {
    let Some(row) = ledger.ticket(key)? else {
        return Ok(Vec::new());
    };
    let Ok(ticket) = Ticket::of(row) else {
        return Ok(Vec::new());
    };
    let adapter = crate::adapter::load(ctx)?;
    let visit = ticket.visit();
    let Some(step) = adapter.step(&visit.step) else {
        return Ok(Vec::new());
    };
    let mut entries: Vec<(&crate::adapter::Entry, Option<String>)> = Vec::new();
    match &step.kind {
        StepKind::Turn(_) => {
            let rows = visit_turns(ledger, &ticket.key, &visit.step, visit.turns)?;
            match rows.last() {
                Some(last) => {
                    let held = last.status.as_deref() == Some(TurnStatus::Done.as_str())
                        && last.end_fact.as_deref() == Some("held");
                    if held {
                        entries.extend(step.after.iter().map(|e| (e, Some(last.id.clone()))));
                    }
                }
                None => entries.extend(
                    step.before
                        .iter()
                        .enumerate()
                        .filter(|(i, e)| !visit.before_is_done(*i, &e.label()))
                        .map(|(_, e)| (e, None)),
                ),
            }
        }
        StepKind::Wait(_) => entries.extend(step.start.iter().map(|e| (e, None))),
        _ => {}
    }
    let mut out = Vec::new();
    for (entry, turn) in entries {
        if entry.kind != EntryKind::Verb {
            continue;
        }
        let Ok(args) = super::machine::entry_args(ctx, &ticket.key, entry) else {
            continue;
        };
        let run = crate::tick::OperationRun {
            ticket: ticket.key.clone(),
            verb: entry.name.clone(),
            args,
            turn,
        };
        if let Some(op) = executor::lookup(ctx, ledger, &run)? {
            if op.state == executor::OpState::Pending && kept_back(op.outcome.name()) {
                out.push(op);
            }
        }
    }
    Ok(out)
}

/// The first entry of the step that is not done: an `after` entry after a turn that held its
/// end facts, else a `before` entry before the first turn of the visit.
fn entry_fact(
    ctx: &Ctx,
    ledger: &Ledger,
    ticket: &Ticket,
    visit: &Visit,
    adapter: &Adapter,
    visit_rows: &[crate::ledger::turns::TurnRow],
) -> Result<Option<EntryFact>> {
    let Some(step) = adapter.step(&visit.step) else {
        return Ok(None);
    };
    if let Some(last) = visit_rows.last() {
        let held = last.status.as_deref() == Some(TurnStatus::Done.as_str())
            && last.end_fact.as_deref() == Some("held");
        if !held {
            return Ok(None);
        }
        let facts = op_facts(ctx, ledger, ticket, &step.after, Some(&last.id))?;
        return Ok(first_open(facts, false));
    }
    let mut facts = Vec::new();
    for (index, entry) in step.before.iter().enumerate() {
        let label = entry.label();
        if visit.before_is_done(index, &label) {
            continue;
        }
        let fact = match entry.kind {
            // A job that is not done in this visit runs at the next entry.
            EntryKind::Job => OpFact::NotAsked,
            EntryKind::Verb => op_facts(ctx, ledger, ticket, std::slice::from_ref(entry), None)?
                .pop()
                .map_or(OpFact::NotAsked, |(_, fact)| fact),
        };
        facts.push((label, fact));
    }
    Ok(first_open(facts, true))
}

fn first_open(facts: Vec<(String, OpFact)>, before: bool) -> Option<EntryFact> {
    for (label, fact) in facts {
        let state = match fact {
            OpFact::Known {
                outcome: Outcome::Confirmed,
                ..
            } => continue,
            OpFact::NotAsked => EntryState::Next,
            OpFact::BadArgs(text) => EntryState::Cannot(text),
            OpFact::Known { outcome, .. } => match outcome {
                Outcome::Pending | Outcome::Executing => EntryState::Runs,
                Outcome::Waiting { .. } => EntryState::Waits,
                Outcome::Uncertain => EntryState::Uncertain,
                other => EntryState::Other(other.to_string()),
            },
        };
        return Some(EntryFact {
            label,
            before,
            state,
        });
    }
    None
}

fn wait_facts(
    ledger: &Ledger,
    ticket: &Ticket,
    visit: &Visit,
    adapter: Option<&Adapter>,
) -> Result<WaitFacts> {
    let watch = adapter
        .and_then(|a| a.step(&visit.step))
        .and_then(|step| match &step.kind {
            StepKind::Wait(wait) => Some(wait.watch.clone()),
            _ => None,
        });
    let seen = ticket.counts.seen.as_ref();
    let ci = seen.and_then(|s| s.ci.clone());
    let mut checks = Checks::default();
    if let Some(kind) = watch.as_deref().and_then(|w| w.split('.').next()) {
        let head = ci
            .as_ref()
            .and_then(|c| c.head.clone())
            .or_else(|| ticket.row.head.clone());
        if let Some(head) = head {
            for check in ledger.checks_of(ticket.key.as_str(), Some(&head))? {
                if check.kind == kind {
                    checks.add(&check.result);
                }
            }
        }
    }
    Ok(WaitFacts {
        watch,
        value: visit.watched.clone(),
        since: visit.since.clone(),
        observed: visit.observed.clone(),
        opens: visit.opens.clone(),
        pr: seen.and_then(|s| s.pr.clone()),
        ci: ci.map(|mut c| {
            if let Some(head) = &c.head {
                c.head = Some(short(head).to_string());
            }
            c
        }),
        checks,
    })
}

/// The window that is at or above its stop value, else the fullest one with a reading.
fn limit_fact(ctx: &Ctx, ledger: &Ledger) -> Result<Option<LimitFact>> {
    let config = ctx.config()?;
    let now = time::now_rfc3339();
    let mut best: Option<LimitFact> = None;
    for (window, stop) in [
        (Window::FiveHour, config.limits.five_hour),
        (Window::SevenDay, config.limits.seven_day),
    ] {
        let Some(reading) = ledger.active_reading(window, &now)? else {
            continue;
        };
        let fact = LimitFact {
            window: window.column().to_string(),
            part: reading.part,
            stop,
            resets: reading.resets,
        };
        let above = fact.part >= fact.stop;
        let better = match &best {
            None => true,
            Some(old) => above && old.part < old.stop,
        };
        if better {
            best = Some(fact);
        }
    }
    Ok(best)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn checks_count_by_their_result() {
        let mut checks = Checks::default();
        for result in ["pass", "green", "red", "fail", "pending", "queued"] {
            checks.add(result);
        }
        assert_eq!(
            checks,
            Checks {
                passed: 2,
                running: 2,
                failed: 2
            }
        );
        assert_eq!(checks.total(), 6);
    }
}
