//! The answers go back to the asking session (section 8.3, 11.3): when each card of one asking
//! turn has a final answer, they are appended to `answers.md` of the ticket folder, and the
//! session of the asking turn is resumed with one message that lists each question with its
//! answer, its writer, and its sources.
//!
//! A resume is recorded as the event `questions-resumed:<turn>`. When a process dies between
//! the resume and that event, the next tick finds the turn that resumed the session in the
//! ledger and records it, so that a session is never resumed two times for the same cards.

use anyhow::{bail, Context, Result};
use serde_json::{json, Value};

use super::{needless_text, Agents, Question};
use crate::ctx::Ctx;
use crate::ledger::questions::{intake_mark, resume_mark};
use crate::ledger::{Event, Ledger};
use crate::paths::{TicketKey, TurnId};
use crate::turn::{self, Refusal};
use crate::util::fs::{atomic_write, read_optional};

/// Resumes each asking session whose cards all have a final answer.
pub(crate) fn resume_all(ctx: &Ctx, ledger: &Ledger) -> Result<()> {
    let mut failed = Vec::new();
    for asking in ledger.turns_ready_to_resume()? {
        if let Err(e) = resume(ctx, ledger, &asking) {
            if let Some(refused) = e.downcast_ref::<Refusal>() {
                ctx.log(&format!(
                    "questions: the resume of {asking} with its answers waits: {refused}"
                ));
                continue;
            }
            ctx.log(&format!(
                "questions: could not resume {asking} with its answers: {e:#}"
            ));
            failed.push(asking);
        }
    }
    if !failed.is_empty() {
        bail!("could not resume {} with the answers", failed.join(", "));
    }
    Ok(())
}

fn resume(ctx: &Ctx, ledger: &Ledger, asking: &str) -> Result<()> {
    let row = ledger
        .turn(asking)?
        .with_context(|| format!("the ledger has no turn {asking}"))?;
    let key = TicketKey::parse(&row.ticket)?;
    let mut agents = Agents::default();
    let questions = ledger
        .questions_of_turn(asking)?
        .into_iter()
        .map(|r| Question::load(ledger, r, &mut agents))
        .collect::<Result<Vec<_>>>()?;
    if questions.is_empty() {
        return Ok(());
    }

    // A resume that a crash left without its event.
    if let (Some(session), Some(started)) = (&row.session, &row.started) {
        if let Some(found) = ledger.next_turn_of_session(session, started, asking)? {
            mark(ledger, &key, asking, &found.id, true)?;
            ctx.log(&format!(
                "questions: {asking} was already resumed by {}, so it is recorded",
                found.id
            ));
            return Ok(());
        }
    }
    let flags = ledger.ticket_flags(key.as_str())?;
    if !flags.is_empty() {
        ctx.log(&format!(
            "questions: {key} has the flag(s) {}, so the resume of {asking} with its answers waits",
            flags.join(", ")
        ));
        return Ok(());
    }

    let dropped = dropped_cards(ledger, asking)?;
    append_answers(ctx, &key, asking, &questions)?;
    let person = ctx.config()?.user.clone();
    let text = message(&questions, &dropped, &person);
    let launched = turn::resume(ctx, ledger, &TurnId::parse(asking)?, &text)?;
    mark(ledger, &key, asking, &launched.id.to_string(), false)?;
    ctx.log(&format!(
        "questions: resumed {asking} as {} with {} answer(s)",
        launched.id,
        questions.len()
    ));
    Ok(())
}

fn mark(ledger: &Ledger, key: &TicketKey, asking: &str, by: &str, found: bool) -> Result<()> {
    ledger.add_event(
        &Event::now("tick", "questions-resumed")
            .ticket(key.as_str())
            .external_id(&resume_mark(asking))
            .detail(json!({"turn": asking, "resumed_by": by, "found": found})),
    )?;
    Ok(())
}

/// The cards of the asking turn that were not valid, from the event of the intake.
fn dropped_cards(ledger: &Ledger, asking: &str) -> Result<Vec<Value>> {
    let detail = ledger.event_detail(&intake_mark(asking))?.flatten();
    let value: Value = detail
        .and_then(|d| serde_json::from_str(&d).ok())
        .unwrap_or(Value::Null);
    Ok(value
        .get("dropped")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default())
}

/// The writer of the answer of a card for people: `judge`, or the person with a note.
fn writer(question: &Question, person: &str) -> String {
    match &question.answer {
        Some(a) if a.by_judge() => "judge".into(),
        Some(a) => format!("{} (the person)", a.by),
        None => format!("{person} (the person)"),
    }
}

/// The sources of the answer of a card: the files that the Judge applied.
fn sources(question: &Question) -> Vec<String> {
    match &question.answer {
        Some(a) if a.by_judge() => question.sources(),
        _ => Vec::new(),
    }
}

/// The message of the resume: each question with its answer, its writer, and its sources.
pub(crate) fn message(questions: &[Question], dropped: &[Value], person: &str) -> String {
    let mut out = String::from("The answers to your questions.\n");
    for (n, q) in questions.iter().enumerate() {
        out.push_str(&format!("\n{}. {}\n", n + 1, q.card.text.trim()));
        if let Some(reason) = q.needless() {
            out.push_str(&format!(
                "   Answer: none. The person marked the question as needless: {}. Decide it yourself from the code and the files.\n",
                needless_text(reason)
            ));
            out.push_str(&format!("   Writer: {}\n", writer(q, person)));
            continue;
        }
        match &q.answer {
            Some(answer) => {
                out.push_str(&format!("   Answer: {}\n", answer.show()));
                out.push_str(&format!("   Writer: {}\n", writer(q, person)));
                let sources = sources(q);
                out.push_str(&format!(
                    "   Sources: {}\n",
                    if sources.is_empty() {
                        "none".to_string()
                    } else {
                        sources.join(", ")
                    }
                ));
                if !answer.why.trim().is_empty() {
                    out.push_str(&format!("   Why: {}\n", answer.why.trim()));
                }
                if !answer.when.trim().is_empty() {
                    out.push_str(&format!("   Applies when: {}\n", answer.when.trim()));
                }
                if let Some(decision) = &q.row.decision {
                    out.push_str(&format!("   Decision: {decision}\n"));
                }
            }
            None => out.push_str(
                "   Answer: none came. Decide it yourself from the code and the files.\n",
            ),
        }
    }
    for d in dropped {
        let n = d["card"].as_u64().unwrap_or(0);
        let text = d["text"].as_str().unwrap_or("").trim();
        let problems = super::strings(d.get("problems")).join(", ");
        out.push_str(&format!(
            "\nYour card {n}{} was not valid ({problems}), so nobody answered it. Decide it yourself, or ask it again as a valid card.\n",
            if text.is_empty() {
                String::new()
            } else {
                format!(" \"{text}\"")
            }
        ));
    }
    out.push_str("\nContinue the same step with these answers.");
    out
}

/// Appends each answer to `answers.md` of the ticket folder, once for each card.
fn append_answers(ctx: &Ctx, key: &TicketKey, asking: &str, questions: &[Question]) -> Result<()> {
    let path = ctx.paths.ticket_doc(key, "answers.md");
    let mut text = read_optional(&path)?.unwrap_or_default();
    let before = text.len();
    let person = ctx.config()?.user.clone();
    for q in questions {
        let header = format!("## {}:", q.row.id);
        if text.lines().any(|l| l.starts_with(&header)) {
            continue;
        }
        if !text.is_empty() && !text.ends_with("\n\n") {
            text.push_str(if text.ends_with('\n') { "\n" } else { "\n\n" });
        }
        text.push_str(&format!("{header} {}\n\n", q.card.text.trim()));
        text.push_str(&format!(
            "- Asked by: {}, step {}, turn {asking}\n",
            q.agent,
            if q.step.is_empty() { "-" } else { &q.step }
        ));
        match (q.needless(), &q.answer) {
            (Some(reason), _) => {
                text.push_str(&format!(
                    "- Answer: none. The person marked the question as needless: {}.\n",
                    needless_text(reason)
                ));
                text.push_str(&format!("- Writer: {}\n", writer(q, &person)));
            }
            (None, Some(answer)) => {
                text.push_str(&format!("- Answer: {}\n", answer.text));
                text.push_str(&format!("- Writer: {}\n", writer(q, &person)));
                let sources = sources(q);
                text.push_str(&format!(
                    "- Sources: {}\n",
                    if sources.is_empty() {
                        "none".to_string()
                    } else {
                        sources.join(", ")
                    }
                ));
                let why = answer.why.trim();
                text.push_str(&format!(
                    "- Why: {}\n",
                    if why.is_empty() { "-" } else { why }
                ));
                if !answer.when.trim().is_empty() {
                    text.push_str(&format!("- Applies when: {}\n", answer.when.trim()));
                }
                if let Some(decision) = &q.row.decision {
                    text.push_str(&format!("- Decision: {decision}\n"));
                }
            }
            (None, None) => text.push_str("- Answer: none\n"),
        }
    }
    if text.len() != before {
        atomic_write(&path, text.as_bytes())?;
    }
    Ok(())
}

/// Appends a correction of the person to `answers.md`, so that a later turn that reads the
/// earlier answers reads the right one.
pub(crate) fn append_correction(
    ctx: &Ctx,
    question: &Question,
    answer: &super::Answer,
    old: &str,
    new: &str,
) -> Result<()> {
    let key = TicketKey::parse(&question.row.ticket)?;
    let path = ctx.paths.ticket_doc(&key, "answers.md");
    let mut text = read_optional(&path)?.unwrap_or_default();
    if !text.is_empty() && !text.ends_with("\n\n") {
        text.push_str(if text.ends_with('\n') { "\n" } else { "\n\n" });
    }
    text.push_str(&format!(
        "## {} corrected: {}\n\n- Answer: {}\n- Writer: {} (the person)\n- Replaces: the answer {old} of the Judge\n- Decision: {new}\n",
        question.row.id,
        question.card.text.trim(),
        answer.text,
        answer.by
    ));
    if !answer.why.trim().is_empty() {
        text.push_str(&format!("- Why: {}\n", answer.why.trim()));
    }
    atomic_write(&path, text.as_bytes())
}
