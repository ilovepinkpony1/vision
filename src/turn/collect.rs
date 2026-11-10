//! The collection of ended turns (tick step 4) and the outcome of a turn.
//!
//! For each turn row with the status `running`, the collector reads `turn.json` and the
//! stream. When the turn ended, it writes the row (status, reason, times, session id, cost,
//! the four usage numbers, the refused tool calls, and both limit readings with their reset
//! times), writes `result.json`, and adds an event. A turn whose supervisor is dead while
//! `turn.json` says it runs gets the reason `crash`.
//!
//! A turn that ended with no valid result object, with an error, or with a crash gets one
//! automatic resume of its session with the message `continue` (section 7.6). When that turn
//! fails too, its status is `failed`, so that the pipeline can set the flag "needs you".

use std::path::Path;

use ::time::OffsetDateTime;
use anyhow::{bail, Result};
use serde::Serialize;
use serde_json::{json, Value};

use crate::ctx::Ctx;
use crate::ledger::turns::{TurnEnd, TurnRow, WindowReading, STATUS_RUNNING};
use crate::ledger::{Event, Ledger};
use crate::paths::{TicketKey, TurnId, TurnPaths};
use crate::tick::Pass;
use crate::turn::launch::{self, LaunchFile, SUPERVISOR_LOG};
use crate::turn::state::{self, Phase, Reason, TurnState};
use crate::turn::stream::{self, Reading, Summary, Usage};
use crate::turn::{inbox, roles, schema};
use crate::util::fs::{atomic_write, ensure_dir, read_optional};
use crate::util::time;

/// The message of the automatic resume.
pub const CONTINUE: &str = "continue";

/// Seconds that a supervisor may take to start before a turn with no live supervisor counts
/// as crashed.
pub const START_GRACE_SECS: i64 = 30;

/// The status of a turn row.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum TurnStatus {
    /// The supervisor lives, or no tick collected the turn yet.
    Running,
    /// The turn ended with a valid result object. The pipeline reads its `kind`.
    Done,
    /// The turn failed, and the collector started one automatic resume with `continue`.
    Retried,
    /// The turn failed, and no automatic resume is left. The pipeline sets "needs you".
    Failed,
}

impl TurnStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            TurnStatus::Running => STATUS_RUNNING,
            TurnStatus::Done => "done",
            TurnStatus::Retried => "retried",
            TurnStatus::Failed => "failed",
        }
    }

    pub fn parse(text: &str) -> Option<TurnStatus> {
        [
            TurnStatus::Running,
            TurnStatus::Done,
            TurnStatus::Retried,
            TurnStatus::Failed,
        ]
        .into_iter()
        .find(|s| s.as_str() == text)
    }
}

/// What a collection did with one turn.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Collected {
    pub turn: String,
    pub ticket: String,
    pub status: TurnStatus,
    pub reason: Reason,
    /// The turn that resumes this one with `continue`.
    pub retry: Option<String>,
}

/// The outcome of a turn, from its row, `turn.json`, and `result.json`.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct TurnOutcome {
    pub id: String,
    pub ticket: String,
    pub agent: Option<String>,
    pub step: Option<String>,
    pub kind: Option<String>,
    pub status: TurnStatus,
    pub reason: Option<Reason>,
    /// The result object, such as `{"kind": "questions", ...}`.
    pub result: Option<Value>,
    pub session: Option<String>,
    pub resumes: Option<String>,
    pub model: Option<String>,
    pub effort: Option<String>,
    pub started: Option<String>,
    pub ended: Option<String>,
    pub cost_usd: Option<f64>,
    pub usage: Usage,
    pub denials: i64,
    pub five_hour: Option<WindowReading>,
    pub seven_day: Option<WindowReading>,
    /// For the status `retried`: the turn that resumes it with `continue`.
    pub retried_by: Option<String>,
    /// For people: what went wrong.
    pub detail: Option<String>,
}

/// Tick step 4.
pub fn collect_ended_turns(pass: &mut Pass) -> Result<()> {
    collect(pass.ctx, &pass.ledger, pass.now).map(|_| ())
}

/// Collects each turn that ended. A failure of one turn does not stop the others, but the
/// call then fails and names them.
pub fn collect(ctx: &Ctx, ledger: &Ledger, now: OffsetDateTime) -> Result<Vec<Collected>> {
    let mut collected = Vec::new();
    let mut failed = Vec::new();
    for row in ledger.turns_with_status(STATUS_RUNNING)? {
        match collect_one(ctx, ledger, &row, now) {
            Ok(Some(done)) => collected.push(done),
            Ok(None) => {}
            Err(e) => {
                ctx.log(&format!("could not collect the turn {}: {e:#}", row.id));
                failed.push(row.id);
            }
        }
    }
    if !failed.is_empty() {
        bail!(
            "could not collect the turn(s) {}. See {}",
            failed.join(", "),
            ctx.paths.log_file().display()
        );
    }
    Ok(collected)
}

fn collect_one(
    ctx: &Ctx,
    ledger: &Ledger,
    row: &TurnRow,
    now: OffsetDateTime,
) -> Result<Option<Collected>> {
    let key = TicketKey::parse(&row.ticket)?;
    let id = TurnId::parse(&row.id)?;
    let turn = ctx.paths.turn(&key, &id);
    let current = TurnState::read(&turn.status).unwrap_or(None);
    let age = row
        .started
        .as_deref()
        .and_then(|s| time::parse_rfc3339(s).ok())
        .map_or(i64::MAX, |at| time::seconds_between(at, now));
    let ended = match current {
        Some(s) if s.is_ended() => s,
        Some(s) if s.state != Phase::Queued => {
            if state::supervisor_alive(&turn.dir) {
                return Ok(None);
            }
            crashed(&turn, s)?
        }
        other => {
            if age < START_GRACE_SECS || state::supervisor_alive(&turn.dir) {
                return Ok(None);
            }
            crashed(&turn, other.unwrap_or_else(|| TurnState::queued(&row.id)))?
        }
    };
    let reason = ended.reason.unwrap_or(Reason::Error);
    let summary = stream::summarize(&turn.stream)?;
    let launch_file = LaunchFile::read(&turn.dir).ok();
    let agent = row.agent.clone().unwrap_or_default();
    let object = summary
        .result
        .as_ref()
        .and_then(|r| r.structured_output.clone());
    let problems = match &object {
        None => vec!["the session gave no result object".to_string()],
        Some(object) => match roles::schema(&ctx.paths, &agent) {
            Ok(schema) => schema::problems(&schema, object),
            Err(e) => {
                ctx.log(&format!("turn {id}: could not read the schema: {e:#}"));
                Vec::new()
            }
        },
    };
    let valid = reason == Reason::Result && problems.is_empty();
    let session = summary.session_id.clone().or(ended.session.clone());
    let result = summary.result.clone().unwrap_or_default();
    let end = TurnEnd {
        status: if valid {
            TurnStatus::Done.as_str().into()
        } else {
            TurnStatus::Failed.as_str().into()
        },
        reason: reason.as_str().into(),
        ended: ended.ended.clone().unwrap_or_else(|| time::rfc3339(now)),
        session: session.clone(),
        cost_usd: result.cost_usd,
        input: result.usage.input,
        cache_creation: result.usage.cache_creation,
        cache_read: result.usage.cache_read,
        output: result.usage.output,
        denials: result.denials,
        five_hour: window(summary.five_hour.as_ref()),
        seven_day: window(summary.seven_day.as_ref()),
    };
    ledger.end_turn(&row.id, &end)?;
    atomic_write(
        &turn.result,
        format!(
            "{}\n",
            serde_json::to_string_pretty(&object.clone().unwrap_or(Value::Null))?
        )
        .as_bytes(),
    )?;
    free_undelivered(ctx, ledger, &key, &turn)?;

    let mut status = if valid {
        TurnStatus::Done
    } else {
        TurnStatus::Failed
    };
    let mut retry = None;
    let mut retry_error = None;
    if !valid {
        if let Some(why_not) = no_retry(reason, &summary, launch_file.as_ref(), session.as_deref())
        {
            retry_error = Some(why_not);
        } else if let (Some(file), Some(session)) = (&launch_file, &session) {
            let mut request = file.request.clone();
            request.resumes = Some(session.clone());
            request.message = Some(CONTINUE.into());
            request.retry_of = Some(row.id.clone());
            match launch::launch(ctx, ledger, &request) {
                Ok(launched) => {
                    ledger.set_turn_status(&row.id, TurnStatus::Retried.as_str())?;
                    status = TurnStatus::Retried;
                    retry = Some(launched.id.to_string());
                }
                Err(e) => retry_error = Some(format!("the automatic resume did not start: {e:#}")),
            }
        }
    }
    let detail = ended.detail.clone();
    ledger.add_event(
        &Event::now("tick", "turn-ended")
            .ticket(&row.ticket)
            .external_id(&format!("turn-ended:{}", row.id))
            .detail(json!({
                "turn": row.id, "status": status, "reason": reason,
                "cost_usd": result.cost_usd,
                "result_kind": object.as_ref().and_then(|o| o.get("kind")).cloned(),
                "problems": problems, "detail": detail, "retry": retry, "no_retry": retry_error,
            })),
    )?;
    ctx.log(&format!(
        "turn {id}: collected with the status {} and the reason {reason}{}",
        status.as_str(),
        retry
            .as_deref()
            .map(|r| format!("; resumed with `continue` as {r}"))
            .unwrap_or_default()
    ));
    Ok(Some(Collected {
        turn: row.id.clone(),
        ticket: row.ticket.clone(),
        status,
        reason,
        retry,
    }))
}

/// Why a failed turn gets no automatic resume, or `None` when it gets one.
fn no_retry(
    reason: Reason,
    summary: &Summary,
    launch_file: Option<&LaunchFile>,
    session: Option<&str>,
) -> Option<String> {
    let Some(file) = launch_file else {
        return Some("the turn folder has no launch.json".into());
    };
    if file.request.no_retry {
        return Some("the request of the turn asked for no automatic resume".into());
    }
    if let Some(first) = &file.request.retry_of {
        return Some(format!(
            "the turn already was the automatic resume of {first}, so this is the second failure"
        ));
    }
    match reason {
        Reason::Result | Reason::Error | Reason::Crash => {}
        Reason::Timeout | Reason::Contract | Reason::Killed => {
            return Some(format!(
                "a turn that ended with the reason {reason} is not resumed by itself"
            ));
        }
    }
    if summary
        .result
        .as_ref()
        .is_some_and(|r| r.subtype.contains("budget"))
    {
        return Some("the turn used its budget".into());
    }
    if session.is_none() || !summary.init_seen && summary.result.is_none() {
        return Some("the session never started, so there is nothing to resume".into());
    }
    None
}

/// Marks a turn whose supervisor died as crashed, and kills what is left of its session.
fn crashed(turn: &TurnPaths, status: TurnState) -> Result<TurnState> {
    if let Some(child) = status.child_pid {
        state::kill_orphan_group(child);
    }
    let log = read_optional(&turn.dir.join(SUPERVISOR_LOG))
        .ok()
        .flatten()
        .map(|text| {
            let last = text
                .lines()
                .rev()
                .find(|l| !l.trim().is_empty())
                .unwrap_or("");
            if last.is_empty() {
                String::new()
            } else {
                format!(". Its log ends with: {}", last.trim())
            }
        })
        .unwrap_or_default();
    let ended = status.ended(
        Reason::Crash,
        Some(format!(
            "the supervisor stopped before it wrote the end{log}"
        )),
    );
    // A turn folder that someone removed gets a new one, so that the row can still end.
    ensure_dir(&turn.dir)?;
    ended.write(&turn.status)?;
    Ok(ended)
}

/// Moves the messages that the session never got to the inbox of the ticket, so that the
/// next turn gets them.
fn free_undelivered(ctx: &Ctx, ledger: &Ledger, key: &TicketKey, turn: &TurnPaths) -> Result<()> {
    let to = inbox::ticket_inbox(&ctx.paths, key);
    for (path, message) in inbox::waiting(&turn.inbox)? {
        inbox::move_to(&path, &to)?;
        if let Some(id) = message.id {
            ledger.set_message_turn(id, None)?;
        }
    }
    Ok(())
}

fn window(reading: Option<&Reading>) -> Option<WindowReading> {
    reading.map(|r| WindowReading {
        part: r.part,
        resets: r
            .resets_at
            .and_then(|s| OffsetDateTime::from_unix_timestamp(s).ok())
            .map(time::rfc3339),
    })
}

/// The outcome of a turn.
pub fn outcome(ctx: &Ctx, ledger: &Ledger, id: &TurnId) -> Result<TurnOutcome> {
    let Some(row) = ledger.turn(&id.to_string())? else {
        bail!("the ledger has no turn {id}");
    };
    let key = TicketKey::parse(&row.ticket)?;
    let turn = ctx.paths.turn(&key, id);
    let status_file = TurnState::read(&turn.status).unwrap_or(None);
    let result = read_result(&turn.result);
    let status = row
        .status
        .as_deref()
        .and_then(TurnStatus::parse)
        .unwrap_or(TurnStatus::Failed);
    let retried_by = match (&status, &row.session, &row.started) {
        (TurnStatus::Retried, Some(session), Some(started)) => ledger
            .next_turn_of_session(session, started, &row.id)?
            .map(|r| r.id),
        _ => None,
    };
    let reading = |part: Option<f64>, resets: &Option<String>| {
        part.map(|part| WindowReading {
            part,
            resets: resets.clone(),
        })
    };
    Ok(TurnOutcome {
        id: row.id.clone(),
        ticket: row.ticket.clone(),
        agent: row.agent.clone(),
        step: row.step.clone(),
        kind: row.kind.clone(),
        status,
        reason: row
            .reason
            .as_deref()
            .and_then(Reason::parse)
            .or_else(|| status_file.as_ref().and_then(|s| s.reason)),
        result,
        session: row.session.clone(),
        resumes: row.resumes.clone(),
        model: row.model.clone(),
        effort: row.effort.clone(),
        started: row.started.clone(),
        ended: row.ended.clone(),
        cost_usd: row.cost_usd,
        usage: Usage {
            input: row.input.unwrap_or(0),
            cache_creation: row.cache_creation.unwrap_or(0),
            cache_read: row.cache_read.unwrap_or(0),
            output: row.output.unwrap_or(0),
        },
        denials: row.denials.unwrap_or(0),
        five_hour: reading(row.five_hour, &row.five_hour_resets),
        seven_day: reading(row.seven_day, &row.seven_day_resets),
        retried_by,
        detail: status_file.and_then(|s| s.detail),
    })
}

fn read_result(path: &Path) -> Option<Value> {
    let text = read_optional(path).ok().flatten()?;
    serde_json::from_str::<Value>(&text)
        .ok()
        .filter(|v| !v.is_null())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn statuses_round_trip() {
        for status in [
            TurnStatus::Running,
            TurnStatus::Done,
            TurnStatus::Retried,
            TurnStatus::Failed,
        ] {
            assert_eq!(TurnStatus::parse(status.as_str()), Some(status));
        }
    }

    #[test]
    fn a_reset_time_becomes_rfc3339() {
        let reading = Reading {
            part: 0.5,
            resets_at: Some(1794412800),
        };
        assert_eq!(
            window(Some(&reading)).unwrap().resets.as_deref(),
            Some("2026-11-11T16:00:00Z")
        );
    }
}
