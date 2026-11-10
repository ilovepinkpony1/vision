//! The intake of cards: each work turn that ended with the kind `questions` and has no rows yet
//! gets one row for each valid card, in one transaction with the event that records the
//! intake. A turn with no valid card counts as a failed turn: it gets one resume with
//! `continue`, like each other failed turn (section 7.6), and a second failure leaves it
//! `failed`, so that the pipeline sets the flag "needs you".

use ::time::OffsetDateTime;
use anyhow::{bail, Context, Result};
use serde_json::{json, Value};

use super::card;
use crate::ctx::Ctx;
use crate::ledger::questions::{intake_mark, NewQuestion};
use crate::ledger::{Event, Ledger};
use crate::paths::TurnId;
use crate::turn::{self, TurnStatus, CONTINUE};
use crate::util::time;

/// Takes in the cards of each turn that waits for it.
pub(crate) fn take_in(ctx: &Ctx, ledger: &mut Ledger, now: OffsetDateTime) -> Result<()> {
    let mut failed = Vec::new();
    for turn in ledger.turns_with_new_questions()? {
        if let Err(e) = take_in_turn(ctx, ledger, &turn, now) {
            ctx.log(&format!(
                "questions: could not take in the cards of {turn}: {e:#}"
            ));
            failed.push(turn);
        }
    }
    if !failed.is_empty() {
        bail!("could not take in the cards of {}", failed.join(", "));
    }
    Ok(())
}

fn take_in_turn(ctx: &Ctx, ledger: &mut Ledger, turn: &str, now: OffsetDateTime) -> Result<()> {
    let id = TurnId::parse(turn)?;
    let outcome = turn::outcome(ctx, ledger, &id)?;
    let items = outcome
        .result
        .as_ref()
        .and_then(|r| r.get("questions"))
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let mut valid = Vec::new();
    let mut dropped = Vec::new();
    for (n, item) in items.iter().enumerate() {
        match card::check(item) {
            Ok(_) => valid.push(item.to_string()),
            Err(problems) => {
                let text = item
                    .get("text")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string();
                dropped.push(json!({"card": n + 1, "text": text, "problems": problems}))
            }
        }
    }
    if valid.is_empty() {
        return no_valid_card(ctx, ledger, &outcome.ticket, &id, items.len(), &dropped);
    }
    let created = time::rfc3339(now);
    let rows: Vec<NewQuestion<'_>> = valid
        .iter()
        .map(|card| NewQuestion {
            ticket: &outcome.ticket,
            turn,
            card,
            created: &created,
        })
        .collect();
    let event = Event::now("tick", "questions-intake")
        .ticket(&outcome.ticket)
        .external_id(&intake_mark(turn))
        .detail(json!({"turn": turn, "cards": valid.len(), "dropped": dropped}));
    let ids = ledger.add_questions(&rows, &event)?;
    if !dropped.is_empty() {
        ctx.log(&format!(
            "questions: {turn} asked {} card(s) that are not valid, so nobody answers them: {}",
            dropped.len(),
            Value::Array(dropped.clone())
        ));
    }
    if !ids.is_empty() {
        ctx.log(&format!(
            "questions: took in {} card(s) of {turn}: {}",
            ids.len(),
            ids.join(", ")
        ));
    }
    Ok(())
}

/// A turn of the kind `questions` with no valid card is a failed turn. When it is not itself
/// the automatic resume of a failed turn, it gets one resume with `continue` and the problems.
/// A resume that a crash left behind counts, so that no turn gets two.
fn no_valid_card(
    ctx: &Ctx,
    ledger: &Ledger,
    ticket: &str,
    id: &TurnId,
    asked: usize,
    dropped: &[Value],
) -> Result<()> {
    let turn_text = id.to_string();
    let row = ledger
        .turn(&turn_text)?
        .with_context(|| format!("the ledger has no turn {id}"))?;
    let request = turn::request_of(ctx, ledger, id).ok();
    let earlier = match (&row.session, &row.started) {
        (Some(session), Some(started)) => {
            ledger.next_turn_of_session(session, started, &turn_text)?
        }
        _ => None,
    };
    let problems: Vec<String> = dropped
        .iter()
        .map(|d| {
            let n = d["card"].as_u64().unwrap_or(0);
            let list: Vec<String> = super::strings(d.get("problems"));
            format!("card {n}: {}", list.join(", "))
        })
        .collect();
    let mut retry = earlier.map(|r| r.id);
    let mut no_retry = None;
    if retry.is_none() {
        match &request {
            None => no_retry = Some("the turn folder has no launch.json".to_string()),
            Some(r) if r.retry_of.is_some() => {
                no_retry = Some(
                    "the turn already was the automatic resume of a failed turn, so this is the second failure"
                        .to_string(),
                )
            }
            Some(r) => match &row.session {
                None => no_retry = Some("the turn has no session id".to_string()),
                Some(session) => {
                    let mut again = r.clone();
                    again.resumes = Some(session.clone());
                    again.message = Some(format!(
                        "{CONTINUE}. Your turn ended with the kind questions, but no card was valid ({}). \
                         Each card needs the question in `text`, two or more choices with a `label`, \
                         and a `kind` of product, architecture, scope, or code.",
                        if problems.is_empty() {
                            "the list of cards was empty".to_string()
                        } else {
                            problems.join("; ")
                        }
                    ));
                    again.retry_of = Some(turn_text.clone());
                    match turn::launch(ctx, ledger, &again) {
                        Ok(launched) => retry = Some(launched.id.to_string()),
                        Err(e) => no_retry = Some(format!("the resume did not start: {e:#}")),
                    }
                }
            },
        }
    }
    let status = if retry.is_some() {
        TurnStatus::Retried
    } else {
        TurnStatus::Failed
    };
    ledger.set_turn_status(&turn_text, status.as_str())?;
    ledger.add_event(
        &Event::now("tick", "questions-invalid")
            .ticket(ticket)
            .external_id(&intake_mark(&turn_text))
            .detail(json!({
                "turn": turn_text, "cards": asked, "dropped": dropped,
                "status": status, "retry": retry, "no_retry": no_retry,
            })),
    )?;
    ctx.log(&format!(
        "questions: {id} ended with the kind questions and no valid card, so it is {}{}",
        status.as_str(),
        match (&retry, &no_retry) {
            (Some(r), _) => format!("; resumed as {r}"),
            (None, Some(why)) => format!(": {why}"),
            (None, None) => String::new(),
        }
    ));
    Ok(())
}
