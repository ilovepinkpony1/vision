//! The decision file of an answer, and the uses of the principles.
//!
//! A row first gets the state `recording` with its answer. The decision file is then written
//! through the memory store, and the row gets its id and the state `answered`. When a process
//! dies between the two, a later tick finds the row in `recording`, finds the decision file
//! that the first try wrote (or writes it), and ends the recording, so that an answer never
//! gets two decision files.

use ::time::OffsetDateTime;
use anyhow::{Context, Result};

use super::{Agents, Answer, Question};
use crate::ctx::Ctx;
use crate::ledger::questions::STATE_RECORDING;
use crate::ledger::Ledger;
use crate::memory::{
    Basis, DecisionId, FileType, Filter, Memory, MemoryFile, MemoryId, NewDecision, PrincipleId,
    UseKind, Writer,
};
use crate::util::time;

/// Seconds after which a tick takes over a recording that a different process started.
pub const RECORDING_GRACE_SECS: i64 = 120;

/// The new decision of the answer of a row.
pub(crate) fn new_decision(
    memory: &Memory,
    question: &Question,
    answer: &Answer,
) -> Result<NewDecision> {
    let (by, basis) = if answer.by_judge() {
        let basis = match question.row.basis.as_deref() {
            Some("decision") => Basis::Decision,
            Some("principle") => Basis::Principle,
            _ => Basis::Inference,
        };
        (Writer::Judge, basis)
    } else {
        (Writer::person(&answer.by)?, Basis::Answer)
    };
    let mut why = answer.why.trim().to_string();
    let mut sources = Vec::new();
    if answer.by_judge() {
        let mut other = Vec::new();
        for source in question.sources() {
            match MemoryId::parse(&source) {
                Ok(id) if memory.exists(&id.to_string()) => {
                    if !sources.contains(&id) {
                        sources.push(id);
                    }
                }
                _ => other.push(source.trim().to_string()),
            }
        }
        other.retain(|s| !s.is_empty());
        if !other.is_empty() {
            why = format!("{why}\n\nOther evidence: {}", other.join(", "))
                .trim()
                .to_string();
        }
    }
    Ok(NewDecision {
        at: None,
        by,
        basis,
        ticket: Some(question.row.ticket.clone()),
        step: (!question.step.is_empty()).then(|| question.step.clone()),
        asked_by: Some(question.agent.clone()),
        kind: question.card.memory_kind(),
        area: question.card.area.clone(),
        sources,
        question: question.card.text.trim().to_string(),
        choices: question.card.labels(),
        answer: answer.text.clone(),
        why,
        applies_when: answer.when.trim().to_string(),
    })
}

/// A decision file that an earlier try wrote for this answer: the same writer, ticket, step,
/// asking agent, question, and answer.
fn earlier_decision(memory: &Memory, new: &NewDecision) -> Result<Option<DecisionId>> {
    let filter = Filter {
        only: Some(FileType::Decision),
        writer: Some(new.by.name().to_string()),
        ..Filter::default()
    };
    let found = memory
        .list(&filter)?
        .files
        .into_iter()
        .find_map(|file| match file {
            MemoryFile::Decision(d)
                if d.ticket == new.ticket
                    && d.step == new.step
                    && d.asked_by == new.asked_by
                    && d.question.trim() == new.question.trim()
                    && d.answer.trim() == new.answer.trim() =>
            {
                Some(d.id)
            }
            _ => None,
        });
    Ok(found)
}

/// Writes the decision file of a row in `recording` and ends the recording. With
/// `look_first`, it first looks for the file of an earlier try. Gives the decision id.
pub(crate) fn record(
    ctx: &Ctx,
    ledger: &Ledger,
    memory: &Memory,
    question: &Question,
    answer: &Answer,
    look_first: bool,
) -> Result<DecisionId> {
    let new = new_decision(memory, question, answer)?;
    let earlier = if look_first {
        earlier_decision(memory, &new)?
    } else {
        None
    };
    let id = match earlier {
        Some(id) => id,
        None => {
            let id = memory
                .add_decision(&new)
                .with_context(|| {
                    format!("could not write the decision file of {}", question.row.id)
                })?
                .id;
            crate::pipeline::crash_point("decision-committed");
            id
        }
    };
    ledger.finish_recording(&question.row.id, &id.to_string())?;
    ctx.log(&format!(
        "questions: {} answered by {} as {id}",
        question.row.id, answer.by
    ));
    Ok(id)
}

/// Ends each recording that a different process started more than
/// [`RECORDING_GRACE_SECS`] ago and did not end.
pub(crate) fn recover(
    ctx: &Ctx,
    ledger: &Ledger,
    memory: &Memory,
    now: OffsetDateTime,
) -> Result<()> {
    let mut agents = Agents::default();
    let mut failed = 0;
    for row in ledger.questions_in_states(&[STATE_RECORDING])? {
        let started = row
            .answered
            .as_deref()
            .and_then(|a| time::parse_rfc3339(a).ok());
        if started.is_some_and(|at| time::seconds_between(at, now) < RECORDING_GRACE_SECS) {
            continue;
        }
        let question = Question::load(ledger, row, &mut agents)?;
        let Some(answer) = question.answer.clone() else {
            ctx.log(&format!(
                "questions: {} is in recording with no answer, so it waits for a person",
                question.row.id
            ));
            continue;
        };
        if let Err(e) = record(ctx, ledger, memory, &question, &answer, true) {
            failed += 1;
            ctx.log(&format!("questions: {e:#}"));
        }
    }
    if failed > 0 {
        anyhow::bail!("{failed} decision file(s) could not be written");
    }
    Ok(())
}

/// The principles among the sources of one answer that exist in the memory, each once.
pub(crate) fn principles(memory: &Memory, sources: &[String]) -> Vec<PrincipleId> {
    let mut out = Vec::new();
    for source in sources {
        if let Ok(MemoryId::Principle(id)) = MemoryId::parse(source) {
            if memory.exists(&id.to_string()) && !out.contains(&id) {
                out.push(id);
            }
        }
    }
    out
}

/// Adds one use of `kind` for each item of `ids`: a principle that two answers name gets two.
/// A failure is only a log line, because a count must never stop an answer.
pub(crate) fn count_ids(
    ctx: &Ctx,
    memory: &Memory,
    ids: &[PrincipleId],
    kind: UseKind,
    writer: &Writer,
) {
    if ids.is_empty() {
        return;
    }
    let uses: Vec<(PrincipleId, UseKind)> = ids.iter().map(|id| (*id, kind)).collect();
    if let Err(e) = memory.count_uses(&uses, writer) {
        ctx.log(&format!(
            "questions: could not count the uses ({kind}) of the principles: {e:#}"
        ));
    }
}

/// Adds one use of `kind` to each principle among the sources of one answer.
pub(crate) fn count(
    ctx: &Ctx,
    memory: &Memory,
    sources: &[String],
    kind: UseKind,
    writer: &Writer,
) {
    count_ids(ctx, memory, &principles(memory, sources), kind, writer);
}
