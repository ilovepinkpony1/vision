//! The runs of the Curator: when one is due, its turn, and the application of its result.
//!
//! A run starts one reader turn of the agent `curator` for the step `curate` under the
//! reserved key `curator`. Its working folder is a folder of the run with the signals file,
//! which the session view gets, and the memory repo is an extra folder that the session reads.
//! The run is the chain of that turn and its automatic resume, if any. When the chain ends
//! with a result, [`apply`] checks each change, builds one proposal branch with the store,
//! runs the checker of "no line is lost", starts the first replay, and stores it all in the
//! ledger. Later ticks move the replays on ([`advance_replays`]), one turn at a time, so that
//! no tick holds its lock while the Judge replays.
//! Only one run is open at a time: a new turn needs no running Curator turn and no run that
//! waits to be applied.

use ::time::OffsetDateTime;
use anyhow::{bail, Context, Result};
use serde_json::{json, Value};

use super::check::{self, Base};
use super::records::{
    self, Applied, Dropped, Failed, ProposalRecord, Started, EVENT_APPLIED, EVENT_FAILED,
    EVENT_MERGED, EVENT_PROPOSAL, EVENT_STARTED, OPEN, REJECTED,
};
use super::replays;
use super::signals::{self, Signals};
use crate::ctx::Ctx;
use crate::ledger::{EventRow, Ledger};
use crate::memory::{CuratorChange, Memory};
use crate::paths::{TicketKey, TurnId};
use crate::questions;
use crate::tick::Pass;
use crate::turn::{
    self, LaunchRequest, Refusal, TurnKind, TurnOutcome, TurnState, TurnStatus, ViewFile,
};
use crate::util::fs::{atomic_write, ensure_dir};
use crate::util::time;

/// The agent of the Curator, and the reserved key of its turns.
pub const CURATOR: &str = "curator";
/// The step of a Curator turn.
pub const STEP: &str = "curate";
/// The days between two runs when no ticket merged.
pub const WEEK_DAYS: i64 = 7;

/// The reserved key of the turns of the Curator.
pub fn key() -> Result<TicketKey> {
    TicketKey::parse(CURATOR)
}

/// A run that started and is not applied or failed yet.
#[derive(Clone, Debug)]
pub struct Pending {
    pub row: EventRow,
    pub started: Started,
}

/// The runs that wait for their result, oldest first.
pub fn pending(ledger: &Ledger) -> Result<Vec<Pending>> {
    let mut out = Vec::new();
    for row in ledger.events_of_kind(EVENT_STARTED, 20)? {
        let Some(started) = records::detail_of::<Started>(&row) else {
            continue;
        };
        let done = ledger
            .event_by_external_id(&records::id_of(EVENT_APPLIED, &started.turn))?
            .is_some()
            || ledger
                .event_by_external_id(&records::id_of(EVENT_FAILED, &started.turn))?
                .is_some();
        if !done {
            out.push(Pending { row, started });
        }
    }
    out.reverse();
    Ok(out)
}

/// Where the chain of turns of a run is.
#[derive(Clone, Debug)]
pub enum Chain {
    /// This turn runs, or no tick collected it yet.
    Running(String),
    /// This turn ended with a valid result.
    Done(String, Box<TurnOutcome>),
    /// This turn failed, and no automatic resume is left.
    Failed(String, Box<TurnOutcome>),
}

impl Chain {
    /// The last turn of the chain.
    pub fn turn(&self) -> &str {
        match self {
            Chain::Running(t) | Chain::Done(t, _) | Chain::Failed(t, _) => t,
        }
    }
}

/// Follows the automatic resumes from the first turn of a run to its last turn.
pub fn chain(ctx: &Ctx, ledger: &Ledger, first: &str) -> Result<Chain> {
    let mut id = first.to_string();
    for _ in 0..10 {
        let outcome = turn::outcome(ctx, ledger, &TurnId::parse(&id)?)?;
        match (outcome.status, outcome.retried_by.clone()) {
            (TurnStatus::Retried, Some(next)) => id = next,
            (TurnStatus::Running | TurnStatus::Retried, _) => return Ok(Chain::Running(id)),
            (TurnStatus::Done, _) => return Ok(Chain::Done(id, Box::new(outcome))),
            (TurnStatus::Failed, _) => return Ok(Chain::Failed(id, Box::new(outcome))),
        }
    }
    bail!("the Curator turn {first} has too many automatic resumes")
}

/// True when `turn.json` of a turn of the Curator says that it ended.
pub fn turn_ended(ctx: &Ctx, id: &str) -> bool {
    let (Ok(key), Ok(id)) = (key(), TurnId::parse(id)) else {
        return false;
    };
    TurnState::read(&ctx.paths.turn(&key, &id).status)
        .ok()
        .flatten()
        .is_some_and(|s| s.is_ended())
}

/// Applies each run whose chain ended: a result becomes a proposal, a failure is recorded.
/// A run that cannot be applied is recorded as failed with the reason, so that it never
/// blocks the next run; its signals come again in the next run. Gives the applied runs.
pub fn apply_pending(ctx: &Ctx, ledger: &Ledger, memory: &Memory) -> Result<Vec<Applied>> {
    let mut out = Vec::new();
    let mut errors = Vec::new();
    for run in pending(ledger)? {
        let first = run.started.turn.clone();
        let failure = match chain(ctx, ledger, &first) {
            Ok(Chain::Running(_)) => None,
            Ok(Chain::Done(last, outcome)) => {
                match apply(
                    ctx,
                    ledger,
                    memory,
                    &run.started,
                    &last,
                    outcome.result.as_ref(),
                ) {
                    Ok(applied) => {
                        out.push(applied);
                        None
                    }
                    Err(e) => {
                        errors.push(format!("{first}: {e:#}"));
                        Some((last, format!("its result could not be applied: {e:#}")))
                    }
                }
            }
            Ok(Chain::Failed(last, outcome)) => {
                let reason = outcome.reason.map_or("unknown".into(), |r| r.to_string());
                let detail = outcome
                    .detail
                    .as_deref()
                    .map(|d| format!(": {d}"))
                    .unwrap_or_default();
                Some((
                    last,
                    format!("the turn failed with the reason {reason}{detail}"),
                ))
            }
            Err(e) => Some((first.clone(), format!("its turn could not be read: {e:#}"))),
        };
        if let Some((last, reason)) = failure {
            ctx.log(&format!("curator: the run {first} failed: {reason}"));
            records::add(
                ledger,
                "tick",
                EVENT_FAILED,
                &first,
                &Failed {
                    turn: first.clone(),
                    last_turn: last,
                    reason,
                },
            )?;
        }
    }
    if !errors.is_empty() {
        bail!("could not apply the Curator run(s): {}", errors.join("; "));
    }
    Ok(out)
}

/// The output of a Curator turn: its summary and each change, or why a change does not parse.
struct Output {
    summary: String,
    changes: Vec<(Value, Result<CuratorChange, String>)>,
}

fn parse_output(result: Option<&Value>) -> Result<Output> {
    let result = result.context("the Curator turn gave no result object")?;
    let summary = result
        .get("summary")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim()
        .to_string();
    let changes = result
        .get("changes")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .map(|value| {
            let parsed = serde_json::from_value::<CuratorChange>(value.clone())
                .map_err(|e| format!("it does not fit the schema of the Curator: {e}"));
            (value, parsed)
        })
        .collect();
    Ok(Output { summary, changes })
}

/// What a change names, for people: its path, its target, or its question.
pub fn names_of(value: &Value) -> String {
    for key in ["path", "target", "question"] {
        if let Some(text) = value.get(key).and_then(Value::as_str) {
            return crate::memory::store::short(text, 60);
        }
    }
    String::new()
}

fn op_of(value: &Value) -> String {
    value
        .get("op")
        .and_then(Value::as_str)
        .unwrap_or("?")
        .to_string()
}

/// The proposal of a run that an earlier try built, after a stop between the build and the end.
fn proposal_of_turn(ledger: &Ledger, turn: &str) -> Result<Option<ProposalRecord>> {
    Ok(ledger
        .events_of_kind(EVENT_PROPOSAL, 20)?
        .iter()
        .filter_map(records::detail_of::<ProposalRecord>)
        .find(|r| r.turn == turn))
}

/// Applies the result of a run: checks each change, builds the proposal, and finishes it.
pub fn apply(
    ctx: &Ctx,
    ledger: &Ledger,
    memory: &Memory,
    started: &Started,
    last: &str,
    result: Option<&Value>,
) -> Result<Applied> {
    let output = parse_output(result)?;
    let mut applied = Applied {
        turn: started.turn.clone(),
        last_turn: last.to_string(),
        mark: started.mark.clone(),
        summary: output.summary.clone(),
        changes: output.changes.len(),
        ..Applied::default()
    };
    let earlier = proposal_of_turn(ledger, &started.turn)?;
    let record = match earlier {
        Some(record) => Some(record),
        None => {
            memory.commit_hand_edits()?;
            let (files, _) = memory.snapshot()?;
            let base = Base::new(&files);
            let mut dropped = Vec::new();
            let mut valid: Vec<CuratorChange> = Vec::new();
            let mut places: Vec<(usize, &Value)> = Vec::new();
            for (i, (value, parsed)) in output.changes.iter().enumerate() {
                let checked = parsed
                    .clone()
                    .map(|c| check::tidy(&c))
                    .and_then(|c| check::check(&c, &base).map(|()| c));
                match checked {
                    Ok(change) => {
                        valid.push(check::with_evidence(&change));
                        places.push((i + 1, value));
                    }
                    Err(reason) => dropped.push(Dropped {
                        index: i + 1,
                        op: op_of(value),
                        names: names_of(value),
                        reason,
                    }),
                }
            }
            // A stop between the build of the branch and its record left a branch with the
            // summary of this run and no record: the run adopts it, and builds no second one.
            let orphan = if valid.is_empty() {
                None
            } else {
                orphan_proposal(ledger, memory, &output.summary)?
            };
            if let Some(orphan) = orphan {
                dropped.sort_by_key(|d| d.index);
                applied.dropped = dropped.clone();
                let record = ProposalRecord {
                    proposal: orphan.name.clone(),
                    turn: started.turn.clone(),
                    summary: output.summary.clone(),
                    created: orphan.created.clone(),
                    dropped,
                    state: OPEN.into(),
                    lost: Vec::new(),
                    replays: None,
                };
                records::put(ledger, "tick", EVENT_PROPOSAL, &orphan.name, &record)?;
                ctx.log(&format!(
                    "curator: {} has no record after a stop, so the run {} takes it",
                    orphan.name, started.turn
                ));
                return finish_applied(ctx, ledger, memory, started, applied, Some(record));
            }
            let built = if valid.is_empty() {
                None
            } else {
                Some(memory.build_proposal(&output.summary, &valid, time::now().date())?)
            };
            crate::pipeline::crash_point("curator-proposal-built");
            if let Some(built) = &built {
                for skipped in &built.skipped {
                    if let Some((index, value)) = places.get(skipped.index) {
                        dropped.push(Dropped {
                            index: *index,
                            op: op_of(value),
                            names: names_of(value),
                            reason: skipped.reason.clone(),
                        });
                    }
                }
            }
            dropped.sort_by_key(|d| d.index);
            applied.dropped = dropped.clone();
            match built.and_then(|b| b.proposal) {
                None => None,
                Some(file) => {
                    let record = ProposalRecord {
                        proposal: file.name.clone(),
                        turn: started.turn.clone(),
                        summary: output.summary.clone(),
                        created: file.created.clone(),
                        dropped,
                        state: OPEN.into(),
                        lost: Vec::new(),
                        replays: None,
                    };
                    records::put(ledger, "tick", EVENT_PROPOSAL, &file.name, &record)?;
                    Some(record)
                }
            }
        }
    };
    finish_applied(ctx, ledger, memory, started, applied, record)
}

/// An open proposal branch with this summary and no record: the branch of a run that stopped
/// between the build and its record.
fn orphan_proposal(
    ledger: &Ledger,
    memory: &Memory,
    summary: &str,
) -> Result<Option<crate::memory::ProposalInfo>> {
    for info in memory.proposals()? {
        if info.problem.is_none()
            && info.summary == summary
            && records::read::<ProposalRecord>(ledger, EVENT_PROPOSAL, &info.name)?.is_none()
        {
            return Ok(Some(info));
        }
    }
    Ok(None)
}

/// The end of [`apply`]: the replays of the proposal start, and the run is applied.
fn finish_applied(
    ctx: &Ctx,
    ledger: &Ledger,
    memory: &Memory,
    started: &Started,
    mut applied: Applied,
    record: Option<ProposalRecord>,
) -> Result<Applied> {
    if let Some(record) = record {
        applied.dropped = record.dropped.clone();
        let record = finish(ctx, ledger, memory, record)?;
        applied.proposal = Some(record.proposal.clone());
        if record.state == REJECTED {
            applied.note = Some(format!(
                "Vision rejected {} because it loses {} line(s) that its ARCHIVE.md does not hold",
                record.proposal,
                record.lost.len()
            ));
        } else if let Ok(info) = memory.proposals() {
            applied.kept = info
                .iter()
                .find(|p| p.name == record.proposal)
                .map_or(0, |p| p.open + p.accepted + p.rejected);
        }
    } else if applied.changes == 0 {
        applied.note = Some("the Curator proposed no change".into());
    } else {
        applied.note = Some("each change of the Curator was dropped".into());
    }
    records::add(ledger, "tick", EVENT_APPLIED, &started.turn, &applied)?;
    ctx.log(&format!(
        "curator: applied the run {}: {}{} change(s) dropped",
        started.turn,
        applied
            .proposal
            .as_deref()
            .map(|p| format!("{p}, "))
            .unwrap_or_default(),
        applied.dropped.len()
    ));
    Ok(applied)
}

/// The end of a proposal that the store built: Vision rejects it when it loses a line, and
/// else plans both replays and starts the first replay turn. A later tick reads it and starts
/// the second one ([`advance_replays`]). The record goes to the ledger, keyed by the branch.
pub fn finish(
    ctx: &Ctx,
    ledger: &Ledger,
    memory: &Memory,
    mut record: ProposalRecord,
) -> Result<ProposalRecord> {
    let name = record.proposal.clone();
    if record.state != OPEN || record.replays.is_some() {
        return Ok(record);
    }
    let lost = memory.proposal_lost_lines(&name)?;
    if !lost.is_empty() {
        let why = format!(
            "Vision rejected it: it loses {} line(s) that its ARCHIVE.md does not hold",
            lost.len()
        );
        memory.reject_all(&name, Some(&why))?;
        memory.delete_proposal(&name)?;
        record.state = REJECTED.into();
        record.lost = lost.iter().map(ToString::to_string).collect();
        records::put(ledger, "tick", EVENT_PROPOSAL, &name, &record)?;
        ctx.log(&format!("curator: {name}: {why}"));
        return Ok(record);
    }
    let view = memory.proposal(&name)?;
    record.replays = Some(replays::begin(ctx, ledger, &view)?);
    records::put(ledger, "tick", EVENT_PROPOSAL, &name, &record)?;
    crate::pipeline::crash_point("curator-replays-planned");
    // The run is applied also when its first replay cannot start now: the record says what
    // runs next, and the next tick tries again.
    if let Err(e) = replays::advance(ctx, ledger, memory, &mut record, time::now()) {
        ctx.log(&format!(
            "curator: the replays of {name} wait for the next tick: {e:#}"
        ));
    }
    Ok(record)
}

/// The open proposals whose replays still run, newest first.
pub fn replaying(ledger: &Ledger) -> Result<Vec<ProposalRecord>> {
    Ok(ledger
        .events_of_kind(EVENT_PROPOSAL, 20)?
        .iter()
        .filter_map(records::detail_of::<ProposalRecord>)
        .filter(|r| r.state == OPEN && r.replays.as_ref().is_some_and(|p| !p.is_done()))
        .collect())
}

/// Moves the replays of each open proposal one step on: a replay turn that ended gets its
/// numbers, and the next replay turn starts. It never waits for a turn. Gives the proposals
/// whose replays are done now.
pub fn advance_replays(
    ctx: &Ctx,
    ledger: &Ledger,
    memory: &Memory,
    now: OffsetDateTime,
) -> Result<Vec<String>> {
    let mut done = Vec::new();
    let mut errors = Vec::new();
    for mut record in replaying(ledger)? {
        match replays::advance(ctx, ledger, memory, &mut record, now) {
            Ok(()) => {
                if record
                    .replays
                    .as_ref()
                    .is_some_and(replays::Replays::is_done)
                {
                    ctx.log(&format!(
                        "curator: the replays of {} are done, so it is ready for review",
                        record.proposal
                    ));
                    done.push(record.proposal.clone());
                }
            }
            Err(e) => errors.push(format!("{}: {e:#}", record.proposal)),
        }
    }
    if !errors.is_empty() {
        bail!("could not move the replays of {}", errors.join("; "));
    }
    Ok(done)
}

/// What a start did.
#[derive(Clone, Debug)]
pub enum Start {
    /// A new turn runs with these signals.
    Started { turn: String, signals: Box<Signals> },
    /// The signals are empty, so no turn started.
    NoSignals { since: Option<String> },
    /// A run is open: its turn runs, or its result waits to be applied.
    Busy { turn: String },
}

/// Starts a Curator turn with the signals since the last good run, unless a run is open or
/// the signals are empty. A refusal of the limits or the budget is a [`Refusal`] in the error.
pub fn start(
    ctx: &Ctx,
    ledger: &Ledger,
    memory: &Memory,
    why: &str,
    actor: &str,
    now: OffsetDateTime,
) -> Result<Start> {
    if let Some(run) = pending(ledger)?.into_iter().next() {
        return Ok(Start::Busy {
            turn: run.started.turn,
        });
    }
    if let Some(row) = ledger
        .turns_with_status(crate::ledger::turns::STATUS_RUNNING)?
        .into_iter()
        .find(|t| t.agent.as_deref() == Some(CURATOR))
    {
        return Ok(Start::Busy { turn: row.id });
    }
    let since = records::last_good(ledger)?.map(|(_, applied)| applied.mark);
    let collected = signals::collect(ctx, ledger, memory, since.as_ref(), now)?;
    if collected.signals.is_empty() {
        return Ok(Start::NoSignals {
            since: since.map(|m| m.at),
        });
    }
    let config = ctx.config()?;
    let key = key()?;
    let stamp = format!(
        "{}-{}",
        time::compact(now),
        &uuid::Uuid::new_v4().simple().to_string()[..8]
    );
    let dir = ctx.paths.ticket_dir(&key).join(STEP).join(stamp);
    ensure_dir(&dir)?;
    let file = dir.join("signals.json");
    atomic_write(
        &file,
        format!("{}\n", serde_json::to_string_pretty(&collected.signals)?).as_bytes(),
    )?;
    let mut request = LaunchRequest::new(key, CURATOR, STEP, TurnKind::Reader, &dir);
    request.facts = json!({
        "memory": memory.dir().display().to_string(),
        "signals": ".vision/turns/{turn}/signals.json",
        "person": config.user,
    });
    request.view = vec![ViewFile {
        from: file.clone(),
        to: "turns/{turn}/signals.json".into(),
    }];
    request.add_dirs = vec![memory.dir().to_path_buf()];
    let counts = collected.signals.counts();
    let mark = collected.mark.clone();
    let launched = questions::launch_claimed(ctx, ledger, &request, |ledger, launched| {
        if ledger.running_turns_of_agent(CURATOR)? > 1 {
            bail!("a different Curator turn started meanwhile");
        }
        if !pending(ledger)?.is_empty() {
            bail!("a different Curator run started meanwhile");
        }
        records::add(
            ledger,
            actor,
            EVENT_STARTED,
            &launched.id.to_string(),
            &Started {
                turn: launched.id.to_string(),
                why: why.to_string(),
                since: since.clone(),
                mark: mark.clone(),
                signals: file.display().to_string(),
                counts: counts.clone(),
            },
        )?;
        Ok(())
    });
    let launched = match launched {
        Ok(launched) => launched,
        Err(e) => {
            let _ = std::fs::remove_dir_all(&dir);
            return Err(e);
        }
    };
    ctx.log(&format!(
        "curator: started the turn {} ({why}) with {}",
        launched.id,
        collected.signals.describe()
    ));
    Ok(Start::Started {
        turn: launched.id.to_string(),
        signals: Box::new(collected.signals),
    })
}

/// Why a run is due now, or `None`: a ticket merged since the last run started, or
/// [`WEEK_DAYS`] days passed since the last good run (and since the last start). The tick
/// still starts no turn when the signals are empty.
pub fn due(ledger: &Ledger, now: OffsetDateTime) -> Result<Option<String>> {
    let last_started = ledger.latest_event(EVENT_STARTED)?;
    if let Some(merged) = ledger.latest_event(EVENT_MERGED)? {
        if last_started.as_ref().is_none_or(|s| merged.id > s.id) {
            let ticket = merged.ticket.unwrap_or_else(|| "a ticket".into());
            return Ok(Some(format!("{ticket} merged")));
        }
    }
    let parse = |text: &str| time::parse_rfc3339(text).ok();
    let mut anchor = records::last_good(ledger)?.and_then(|(_, a)| parse(&a.mark.at));
    if let Some(started) = &last_started {
        anchor = anchor.max(parse(&started.at));
    }
    if anchor.is_none() {
        anchor = ledger.first_event()?.and_then(|e| parse(&e.at));
    }
    match anchor {
        Some(at) if time::seconds_between(at, now) < WEEK_DAYS * 86_400 => Ok(None),
        _ => Ok(Some(format!(
            "{WEEK_DAYS} days passed since the last good run"
        ))),
    }
}

/// The hook of the tick: applies each run whose turn the collector collected, and starts a
/// Curator turn when one is due. A refusal of the limits or the budget waits for a later tick.
pub fn tick(pass: &mut Pass) -> Result<()> {
    let ctx = pass.ctx;
    let memory = Memory::from_ctx(ctx).context("the Curator needs the memory")?;
    let mut failed = Vec::new();
    if let Err(e) = advance_replays(ctx, &pass.ledger, &memory, pass.now) {
        ctx.log(&format!("curator: {e:#}"));
        failed.push("replays");
    }
    if let Err(e) = apply_pending(ctx, &pass.ledger, &memory) {
        ctx.log(&format!("curator: {e:#}"));
        failed.push("apply");
    }
    let started = due(&pass.ledger, pass.now).and_then(|why| match why {
        Some(why) => start(ctx, &pass.ledger, &memory, &why, "tick", pass.now).map(Some),
        None => Ok(None),
    });
    match started {
        Ok(_) => {}
        Err(e) if e.downcast_ref::<Refusal>().is_some() => {
            ctx.log(&format!("curator: the Curator turn waits: {e:#}"));
        }
        Err(e) => {
            ctx.log(&format!("curator: could not start the Curator: {e:#}"));
            failed.push("start");
        }
    }
    if !failed.is_empty() {
        bail!(
            "the Curator step(s) {} failed. See {}",
            failed.join(", "),
            ctx.paths.log_file().display()
        );
    }
    Ok(())
}
