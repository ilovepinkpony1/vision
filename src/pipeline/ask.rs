//! The ask step and the check step (section 11.3).
//!
//! An ask step reads the cards of its documents, drops a card whose question repeats an
//! earlier one, and creates all of them at once as one batch of the ticket. They go through
//! the Judge exactly as the cards of a turn do. When each card has an answer, the answers go to
//! `answers.md`, and the ticket moves by `on`: `changed` when an answer changes the plan, else
//! `same`. A batch with no card moves to `same` at once.
//!
//! A check step reads one document: a line equal to `stop_line` stops the ticket for the
//! person, a line equal to `line` moves it to `next`, and else it moves to `fail`.

use std::collections::BTreeSet;

use anyhow::Result;
use serde_json::{json, Value};

use super::decide::{cap_exceeded, cap_text};
use super::machine::{cap_value, flag_action, move_to, open_items, save, Ticket};
use super::state::NEEDS_YOU;
use crate::adapter::{Adapter, Expect, Step, StepKind, CHANGED, SAME};
use crate::ctx::Ctx;
use crate::ledger::questions::intake_mark;
use crate::ledger::{Event, Ledger};
use crate::questions::{batch, card};
use crate::tick::Action;
use crate::util::fs::read_optional;
use crate::util::time;

/// The flag of a visit above the cap of its step, or `None`.
fn cap_flag(ctx: &Ctx, ledger: &Ledger, ticket: &Ticket, step: &Step) -> Result<Option<Action>> {
    let visit = ticket.visit();
    let Some(name) = &step.cap else {
        return Ok(None);
    };
    let Some(value) = cap_value(ctx, name)? else {
        return Ok(None);
    };
    if !cap_exceeded(visit.n, Some(value), visit.skip_cap) {
        return Ok(None);
    }
    let open = open_items(ctx, ledger, ticket, step)?;
    Ok(Some(flag_action(
        ticket,
        NEEDS_YOU,
        &cap_text(&step.id, name, value, visit.n, &open),
    )))
}

/// The agent that wrote a document of the ticket folder: the agent of the newest turn of the
/// ticket at a step whose end facts name the document. With no such turn, the agent of the
/// first such step, or `agent`.
fn writer_of(ledger: &Ledger, adapter: &Adapter, ticket: &Ticket, doc: &str) -> Result<String> {
    let names = |expects: &[Expect]| {
        expects.iter().any(|e| match e {
            Expect::Doc(name) => name == doc,
            Expect::Gate(gate) => gate.doc == doc,
            _ => false,
        })
    };
    let mut steps: Vec<(String, String)> = Vec::new();
    for step in &adapter.steps {
        let StepKind::Turn(turn) = &step.kind else {
            continue;
        };
        if turn.team.is_empty() {
            if names(&step.expects) {
                steps.push((step.id.clone(), turn.agent.clone()));
            }
        } else {
            for member in &turn.team {
                if names(&member.expects) {
                    steps.push((step.id.clone(), member.agent.clone()));
                }
            }
        }
    }
    for row in ledger.turns_of_ticket(ticket.key.as_str())?.iter().rev() {
        let found = steps.iter().find(|(step, agent)| {
            row.step.as_deref() == Some(step.as_str()) && row.agent.as_deref() == Some(agent)
        });
        if let Some((_, agent)) = found {
            return Ok(agent.clone());
        }
    }
    Ok(steps
        .first()
        .map(|(_, agent)| agent.clone())
        .unwrap_or_else(|| "agent".to_string()))
}

/// The next action of a ticket at an ask step.
pub(crate) fn select_ask(
    ctx: &Ctx,
    ledger: &Ledger,
    adapter: &Adapter,
    ticket: &mut Ticket,
    step: &Step,
) -> Result<Vec<Action>> {
    let StepKind::Ask(ask) = &step.kind else {
        return Ok(Vec::new());
    };
    let visit = ticket.visit();
    let id = batch::id(&ticket.key, &step.id, visit.seq);
    let questions = batch::questions(ledger, &id)?;
    if questions.is_empty() {
        if let Some(flag) = cap_flag(ctx, ledger, ticket, step)? {
            return Ok(vec![flag]);
        }
        let mut cards: Vec<String> = Vec::new();
        let mut seen = BTreeSet::new();
        let mut missing = Vec::new();
        let mut repeated = Vec::new();
        for doc in &ask.docs {
            let Some(text) = read_optional(&ctx.paths.ticket_doc(&ticket.key, doc))? else {
                // The document of a step that was skipped, or that this path does not have.
                missing.push(doc.clone());
                continue;
            };
            let items = match serde_json::from_str::<Value>(&text) {
                Ok(Value::Array(items)) => items,
                Ok(_) => {
                    return Ok(vec![flag_action(
                        ticket,
                        NEEDS_YOU,
                        &format!("the step {}: the document {doc} is not a JSON array of cards. Fix it in the ticket folder, then run vision retry", step.id),
                    )])
                }
                Err(e) => {
                    return Ok(vec![flag_action(
                        ticket,
                        NEEDS_YOU,
                        &format!("the step {}: the document {doc} is not valid JSON ({e}). Fix it in the ticket folder, then run vision retry", step.id),
                    )])
                }
            };
            let writer = writer_of(ledger, adapter, ticket, doc)?;
            for (n, item) in items.iter().enumerate() {
                let checked = match card::check(item) {
                    Ok(checked) => checked,
                    Err(problems) => {
                        return Ok(vec![flag_action(
                            ticket,
                            NEEDS_YOU,
                            &format!(
                                "the step {}: the card {} of {doc} is not valid: {}. Fix it in the ticket folder, then run vision retry",
                                step.id,
                                n + 1,
                                problems.join(", ")
                            ),
                        )])
                    }
                };
                if !seen.insert(card::fold(&checked.text)) {
                    repeated.push(checked.text.trim().to_string());
                    continue;
                }
                let mut value = item.clone();
                if let Value::Object(map) = &mut value {
                    map.insert("asked_by".into(), json!(writer));
                    map.insert("source".into(), json!(doc));
                }
                cards.push(value.to_string());
            }
        }
        let now = time::now_rfc3339();
        let detail = json!({
            "turn": id, "step": step.id, "cards": cards.len(), "dropped": [],
            "missing": missing, "repeated": repeated,
        });
        if cards.is_empty() {
            save(
                ledger,
                ticket,
                &now,
                Some(
                    Event::now("tick", "ask")
                        .ticket(ticket.key.as_str())
                        .detail(detail),
                ),
            )?;
            let to = step.on.get(SAME).cloned().unwrap_or_default();
            move_to(ctx, ledger, adapter, ticket, step, &to, "on same")?;
            return Ok(Vec::new());
        }
        let event = Event::now("tick", "questions-intake")
            .ticket(ticket.key.as_str())
            .external_id(&intake_mark(&id))
            .detail(detail);
        let ids = batch::add(ledger, &ticket.key, &id, &cards, &now, &event)?;
        ctx.log(&format!(
            "pipeline: the step {} of {} asks {} card(s) as one batch: {}{}",
            step.id,
            ticket.key,
            ids.len(),
            ids.join(", "),
            if missing.is_empty() {
                String::new()
            } else {
                format!(". No document {}, so it counts as []", missing.join(", "))
            }
        ));
        return Ok(Vec::new());
    }
    if !batch::complete(&questions) {
        return Ok(Vec::new());
    }
    batch::write_answers(ctx, &ticket.key, &id, &questions)?;
    let changed: Vec<String> = questions
        .iter()
        .filter(|q| batch::changes(q))
        .map(|q| q.row.id.clone())
        .collect();
    let (key, how) = if changed.is_empty() {
        (SAME, "on same")
    } else {
        (CHANGED, "on changed")
    };
    let to = step.on.get(key).cloned().unwrap_or_default();
    save(
        ledger,
        ticket,
        &time::now_rfc3339(),
        Some(
            Event::now("tick", "ask-answered")
                .ticket(ticket.key.as_str())
                .detail(
                    json!({"batch": id, "cards": questions.len(), "changed": changed, "to": to}),
                ),
        ),
    )?;
    move_to(ctx, ledger, adapter, ticket, step, &to, how)?;
    Ok(Vec::new())
}

/// The next action of a ticket at a check step.
pub(crate) fn select_check(
    ctx: &Ctx,
    ledger: &Ledger,
    adapter: &Adapter,
    ticket: &mut Ticket,
    step: &Step,
) -> Result<Vec<Action>> {
    let StepKind::Check(check) = &step.kind else {
        return Ok(Vec::new());
    };
    if let Some(flag) = cap_flag(ctx, ledger, ticket, step)? {
        return Ok(vec![flag]);
    }
    let Some(text) = read_optional(&ctx.paths.ticket_doc(&ticket.key, &check.doc))? else {
        return Ok(vec![flag_action(
            ticket,
            NEEDS_YOU,
            &format!(
                "the step {} reads {}, which does not exist",
                step.id, check.doc
            ),
        )]);
    };
    let has = |wanted: &str| text.lines().any(|line| line.trim() == wanted);
    if let Some(stop) = &check.stop_line {
        if has(stop) {
            // Vision never stops a ticket by itself. Each adapter gives its stop line a meaning
            // of its own, such as a fix that no allowed place can hold, so the reason names only
            // the line, the document, and the choices of the person.
            return Ok(vec![flag_action(
                ticket,
                NEEDS_YOU,
                &format!(
                    "the step {} found the stop line `{stop}` in {}, so the person decides: run `vision retry {key} --step STEP` or `vision stop {key}`",
                    step.id,
                    check.doc,
                    key = ticket.key
                ),
            )]);
        }
    }
    let holds = has(&check.line);
    let (to, how) = if holds {
        match &step.next {
            Some(next) => (next.clone(), "check holds".to_string()),
            None => {
                return Ok(vec![flag_action(
                    ticket,
                    NEEDS_YOU,
                    &format!("the step {} has no next step. Fix adapter.toml", step.id),
                )])
            }
        }
    } else {
        (check.fail.clone(), format!("fail {}", check.line))
    };
    save(
        ledger,
        ticket,
        &time::now_rfc3339(),
        Some(
            Event::now("tick", "check")
                .ticket(ticket.key.as_str())
                .detail(json!({
                    "step": step.id, "doc": check.doc, "line": check.line, "holds": holds, "to": to,
                })),
        ),
    )?;
    move_to(ctx, ledger, adapter, ticket, step, &to, &how)?;
    Ok(Vec::new())
}
