//! The answers of the person (section 8.4): `vision answer`, `vision answers`, and
//! `vision correct`, with the library calls that `vision needs` shares.

use anyhow::{anyhow, bail, Context, Result};
use serde::Serialize;
use serde_json::json;

use super::card::{Answer, Reply};
use super::record::{self, count};
use super::{needless_name, Agents, Question};
use crate::cli::{AnswerArgs, Needless};
use crate::ctx::Ctx;
use crate::ledger::questions::{HumanRow, STATE_ANSWERED, STATE_ASKED, STATE_PROPOSED};
use crate::ledger::{Event, Ledger};
use crate::memory::store::short;
use crate::memory::{DecisionId, Memory, MemoryFile, MemoryId, NewDecision, Status, UseKind};
use crate::paths::TicketKey;
use crate::turn::{self, Told};
use crate::util::time;

/// An answer of the person to one card.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PersonAnswer {
    pub reply: Reply,
    pub why: String,
    pub when: String,
    /// The seconds that the card was on the screen, in `vision needs`.
    pub seconds: Option<i64>,
}

/// What an answer of the person did.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Answered {
    pub id: String,
    /// The decision file, or `None` for a needless card.
    pub decision: Option<DecisionId>,
    pub took_proposal: Option<bool>,
    /// True when each card of the asking turn now has a final answer.
    pub turn_complete: bool,
}

/// A card that waits for the person, or an error that says why it does not.
fn waiting(ledger: &Ledger, id: &str) -> Result<Question> {
    let question = Question::find(ledger, id)?;
    match question.row.state.as_str() {
        STATE_PROPOSED | STATE_ASKED => Ok(question),
        STATE_ANSWERED if question.answer.as_ref().is_some_and(Answer::by_judge) => bail!(
            "the Judge already answered {id}. If its answer is wrong, run `vision correct {id} \"RIGHT ANSWER\"`"
        ),
        STATE_ANSWERED | "recording" => bail!("{id} already has an answer"),
        "needless" => bail!("{id} is already marked as needless"),
        _ => bail!(
            "the Judge still judges {id}, so it does not wait for you yet. Run `vision needs` after the next tick"
        ),
    }
}

fn turn_complete(ledger: &Ledger, turn: &str) -> Result<bool> {
    Ok(ledger
        .questions_of_turn(turn)?
        .iter()
        .all(|r| r.state == STATE_ANSWERED || r.state == "needless"))
}

/// Records an answer of the person: a decision file with the person as the writer, the
/// reason, and where it applies; the row; and one `human` row. When the person took the
/// proposal of the Judge unchanged, each principle that the proposal named counts as agreed.
pub fn answer_card(
    ctx: &Ctx,
    ledger: &Ledger,
    memory: &Memory,
    id: &str,
    given: &PersonAnswer,
) -> Result<Answered> {
    let question = waiting(ledger, id)?;
    let id = question.row.id.clone();
    if let Reply::Choice(i) = given.reply {
        if i >= question.card.options.len() {
            bail!(
                "{id} has {} choices, so choose a number from 1 to {}",
                question.card.options.len(),
                question.card.options.len()
            );
        }
    }
    if let Reply::Text(text) = &given.reply {
        if text.trim().is_empty() {
            bail!("the answer is empty. Give a choice with --choice N, or words with --text");
        }
    }
    let person = memory.person().name().to_string();
    let mut answer = Answer::new(&given.reply, &question.card, &person);
    answer.why = given.why.trim().to_string();
    answer.when = given.when.trim().to_string();
    let proposal = question.proposal();
    let took = proposal
        .as_ref()
        .map(|p| answer.reply().same(p, &question.card));
    let took = match question.row.outcome.as_deref() {
        Some("propose") => Some(took.unwrap_or(false)),
        _ => None,
    };
    let at = time::now_rfc3339();
    let previous = question.row.state.clone();
    if !ledger.start_person_answer(&id, &serde_json::to_string(&answer)?, took, &at)? {
        bail!("{id} got an answer from a different command meanwhile");
    }
    let decision = match record::record(ctx, ledger, memory, &question, &answer, false) {
        Ok(decision) => decision,
        Err(e) => {
            ledger.undo_person_answer(&id, &previous)?;
            return Err(e.context(format!("{id} still waits for your answer")));
        }
    };
    ledger.add_human(&HumanRow {
        at: at.clone(),
        ticket: Some(question.row.ticket.clone()),
        kind: "answer".into(),
        object: id.clone(),
        value: Some(answer.text.clone()),
        reason: (!answer.why.is_empty()).then(|| answer.why.clone()),
        seconds: given.seconds,
    })?;
    if took == Some(true) {
        let sources = question
            .view
            .judge
            .as_ref()
            .map(|e| e.sources.clone())
            .unwrap_or_default();
        count(ctx, memory, &sources, UseKind::Agreed, memory.person());
    }
    ledger.add_event(
        &Event::now("user", "card-answered")
            .ticket(&question.row.ticket)
            .detail(json!({"card": id, "decision": decision.to_string(), "took_proposal": took})),
    )?;
    Ok(Answered {
        turn_complete: turn_complete(ledger, &question.row.turn)?,
        id,
        decision: Some(decision),
        took_proposal: took,
    })
}

/// Marks a card as needless. It gets no decision file. The asking session hears that the
/// person marked it as needless for this reason, and decides it itself.
pub fn mark_needless(
    ctx: &Ctx,
    ledger: &Ledger,
    id: &str,
    reason: Needless,
    seconds: Option<i64>,
) -> Result<Answered> {
    let question = waiting(ledger, id)?;
    let id = question.row.id.clone();
    let name = needless_name(reason);
    let at = time::now_rfc3339();
    if !ledger.set_question_needless(&id, name, &at)? {
        bail!("{id} got an answer from a different command meanwhile");
    }
    ledger.add_human(&HumanRow {
        at,
        ticket: Some(question.row.ticket.clone()),
        kind: "needless".into(),
        object: id.clone(),
        value: Some(name.into()),
        reason: None,
        seconds,
    })?;
    ledger.add_event(
        &Event::now("user", "card-needless")
            .ticket(&question.row.ticket)
            .detail(json!({"card": id, "reason": name})),
    )?;
    ctx.log(&format!("questions: {id} marked as needless ({name})"));
    Ok(Answered {
        turn_complete: turn_complete(ledger, &question.row.turn)?,
        id,
        decision: None,
        took_proposal: None,
    })
}

/// `vision answer ID --choice N | --text "T" [--why "W"] [--when "C"]`, or `--needless REASON`.
pub fn answer_command(ctx: &Ctx, args: AnswerArgs) -> Result<()> {
    ctx.require_setup()?;
    let ledger = ctx.ledger()?;
    let answered = if let Some(reason) = args.needless {
        mark_needless(ctx, &ledger, &args.id, reason, None)?
    } else {
        let reply = match (args.choice, args.text) {
            (Some(0), _) => bail!("--choice counts from 1, as `vision needs` shows the choices"),
            (Some(n), _) => Reply::Choice(usize::try_from(n - 1).unwrap_or(usize::MAX)),
            (None, Some(text)) => Reply::Text(text),
            (None, None) => bail!("give --choice N, --text \"T\", or --needless REASON"),
        };
        let memory = Memory::from_ctx(ctx)?;
        let given = PersonAnswer {
            reply,
            why: args.why.unwrap_or_default(),
            when: args.when.unwrap_or_default(),
            seconds: None,
        };
        answer_card(ctx, &ledger, &memory, &args.id, &given)?
    };
    println!("{}", answered_line(&answered));
    Ok(())
}

/// One line for people about an answer.
pub(crate) fn answered_line(answered: &Answered) -> String {
    let saved = match &answered.decision {
        Some(decision) => format!("Saved the answer to {} as {decision}.", answered.id),
        None => format!(
            "Marked {} as needless. It gets no decision file.",
            answered.id
        ),
    };
    let next = if answered.turn_complete {
        "Each card of its turn has an answer now, so the next tick sends the answers back to the agent."
    } else {
        "The answers go back to the agent when each card of its turn has one."
    };
    format!("{saved} {next}")
}

// ----- vision answers --------------------------------------------------------------------

/// One answer that the Judge gave alone.
#[derive(Clone, Debug, PartialEq, Serialize)]
struct JudgeAnswer {
    id: String,
    decision: Option<String>,
    ticket: String,
    asked_by: String,
    step: String,
    question: String,
    answer: String,
    /// The number of the choice, from 1.
    choice: Option<usize>,
    basis: Option<String>,
    sources: Vec<String>,
    reason: String,
    answered: Option<String>,
    corrected: bool,
    /// The answer of the person that replaced it.
    correction: Option<String>,
}

/// `vision answers [--days N]`: the answers that the Judge gave alone, newest first.
pub fn answers_command(ctx: &Ctx, days: Option<u32>, json: bool) -> Result<()> {
    ctx.require_setup()?;
    let ledger = ctx.ledger()?;
    let since = days.map(|d| time::rfc3339(time::now() - ::time::Duration::days(i64::from(d))));
    let mut agents = Agents::default();
    let mut list = Vec::new();
    for row in ledger.questions_answered_by_judge(since.as_deref())? {
        let question = Question::load(&ledger, row, &mut agents)?;
        let judge = question.view.judge.clone();
        let judge_reply = judge.as_ref().and_then(|e| e.reply(&question.card));
        let corrected = !ledger
            .human_rows(&question.row.id)?
            .iter()
            .all(|h| h.kind != "correct");
        let answer = judge_reply
            .as_ref()
            .map(|r| Answer::new(r, &question.card, "judge"))
            .or_else(|| question.answer.clone())
            .unwrap_or_default();
        list.push(JudgeAnswer {
            id: question.row.id.clone(),
            decision: question.row.decision.clone(),
            ticket: question.row.ticket.clone(),
            asked_by: question.agent.clone(),
            step: question.step.clone(),
            question: question.card.text.trim().to_string(),
            choice: answer.choice.map(|i| i + 1),
            answer: answer.text,
            basis: question.row.basis.clone(),
            sources: question.sources(),
            reason: judge.map(|e| e.reason).unwrap_or_default(),
            answered: question.row.answered.clone(),
            correction: corrected
                .then(|| question.answer.as_ref().map(|a| a.text.clone()))
                .flatten(),
            corrected,
        });
    }
    if json {
        println!("{}", serde_json::to_string_pretty(&list)?);
        return Ok(());
    }
    if list.is_empty() {
        println!(
            "The Judge gave no answer alone{}.",
            days.map(|d| format!(" in the last {d} day(s)"))
                .unwrap_or_default()
        );
        return Ok(());
    }
    let rows: Vec<Vec<String>> = list
        .iter()
        .map(|a| {
            vec![
                a.id.clone(),
                a.decision.clone().unwrap_or_else(|| "-".into()),
                a.ticket.clone(),
                a.answered
                    .as_deref()
                    .map(|t| t.get(..10).unwrap_or(t).to_string())
                    .unwrap_or_else(|| "-".into()),
                a.basis.clone().unwrap_or_else(|| "-".into()),
                if a.sources.is_empty() {
                    "-".into()
                } else {
                    a.sources.join(",")
                },
                match &a.correction {
                    Some(right) => format!(
                        "{} -> {} (corrected)",
                        short(&a.answer, 30),
                        short(right, 30)
                    ),
                    None => short(&a.answer, 40),
                },
                short(&a.question, 70),
            ]
        })
        .collect();
    print!(
        "{}",
        table(
            &["ID", "DECISION", "TICKET", "DAY", "BASIS", "SOURCES", "ANSWER", "QUESTION"],
            &rows
        )
    );
    println!("\nIf an answer is wrong: vision correct ID \"RIGHT ANSWER\" [--why \"W\"]");
    Ok(())
}

/// A table with a header and columns that line up. The last column has no padding.
pub(crate) fn table(header: &[&str], rows: &[Vec<String>]) -> String {
    let mut widths: Vec<usize> = header.iter().map(|h| h.chars().count()).collect();
    for row in rows {
        for (i, cell) in row.iter().enumerate() {
            if let Some(w) = widths.get_mut(i) {
                *w = (*w).max(cell.chars().count());
            }
        }
    }
    let line = |cells: Vec<&str>| -> String {
        let last = cells.len().saturating_sub(1);
        let mut out = String::new();
        for (i, cell) in cells.iter().enumerate() {
            out.push_str(cell);
            if i < last {
                let pad = widths[i] - cell.chars().count() + 2;
                out.push_str(&" ".repeat(pad));
            }
        }
        out.trim_end().to_string() + "\n"
    };
    let mut out = line(header.to_vec());
    for row in rows {
        out.push_str(&line(row.iter().map(String::as_str).collect()));
    }
    out
}

// ----- vision correct --------------------------------------------------------------------

/// What a correction did.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Corrected {
    pub id: String,
    /// The decision file of the Judge, now superseded.
    pub old: DecisionId,
    /// The decision file of the person.
    pub new: DecisionId,
    /// Where the correction went: `None` when the answers did not go back to the session yet,
    /// so that they carry the right answer.
    pub told: Option<Told>,
}

/// The decision of the person that already superseded `old`, when a correction stopped after
/// the commit of the memory.
fn superseded_by(memory: &Memory, old: DecisionId) -> Result<Option<DecisionId>> {
    let listing = memory.list(&crate::memory::Filter {
        only: Some(crate::memory::FileType::Decision),
        ..Default::default()
    })?;
    Ok(listing.files.into_iter().find_map(|f| match f {
        MemoryFile::Decision(d) if d.supersedes == Some(old) && d.by.is_person() => Some(d.id),
        _ => None,
    }))
}

/// Replaces an answer of the Judge with the right answer of the person: a decision of the
/// person that supersedes the decision of the Judge, the row, one `human` row of the kind
/// `correct`, the count `corrected` of each principle that the wrong answer used, and a
/// message to the ticket. `id` is a card id or the id of a decision file of the Judge.
pub fn correct_card(
    ctx: &Ctx,
    ledger: &Ledger,
    memory: &Memory,
    id: &str,
    right: &str,
    why: Option<&str>,
) -> Result<Corrected> {
    let id = id.trim();
    let right = right.trim();
    if right.is_empty() {
        bail!("the right answer is empty. Give it in quotes, such as vision correct {id} \"CSV\"");
    }
    let question = match MemoryId::parse(id) {
        Ok(MemoryId::Decision(d)) => {
            let row = ledger.question_by_decision(&d.to_string())?.ok_or_else(|| {
                anyhow!("no card has the decision {d} as its answer. Run `vision answers` to see the answers of the Judge")
            })?;
            Question::load(ledger, row, &mut Agents::default())?
        }
        Ok(MemoryId::Principle(_)) => bail!(
            "{id} is a principle. Give the id of a card or of a decision of the Judge, as `vision answers` shows them"
        ),
        Err(_) => Question::find(ledger, id)?,
    };
    let qid = question.row.id.clone();
    let judged =
        question.row.outcome.as_deref() == Some("answer") && question.row.state == STATE_ANSWERED;
    if !judged {
        bail!(
            "the Judge did not answer {qid} alone, so there is nothing to correct. Run `vision answers` to see its answers"
        );
    }
    let decision_text = question.row.decision.clone().context(
        "the answer of the Judge has no decision file yet. Try again after the next tick",
    )?;
    let current = DecisionId::parse(&decision_text)?;
    let judge_entry = question.view.judge.clone();
    let wrong = judge_entry
        .as_ref()
        .and_then(|e| e.reply(&question.card))
        .map(|r| r.text(&question.card))
        .unwrap_or_default();

    let person = memory.person().name().to_string();
    let reply = match question.card.choice_named(right) {
        Some(i) => Reply::Choice(i),
        None => Reply::Text(right.to_string()),
    };
    let mut answer = Answer::new(&reply, &question.card, &person);
    answer.why = why.unwrap_or("").trim().to_string();

    let judged_reply = judge_entry.as_ref().and_then(|e| e.reply(&question.card));
    let by_judge = question.answer.as_ref().is_some_and(Answer::by_judge);
    if by_judge && judged_reply.is_some_and(|r| r.same(&reply, &question.card)) {
        bail!("the Judge already answered {qid} with \"{wrong}\", so there is nothing to correct");
    }
    if !by_judge {
        bail!(
            "{qid} was already corrected: its answer is the decision {current} of the person. \
             Change it with `vision memory edit {current}`"
        );
    }
    let new_decision: NewDecision = record::new_decision(memory, &question, &answer)?;
    let state = memory.decision(current)?.status;
    let new = match state {
        Status::Active => memory.supersede(current, &new_decision)?.id,
        // A correction that stopped after the commit of the memory.
        _ => match superseded_by(memory, current)? {
            Some(id) => id,
            None => bail!(
                "the decision {current} of the Judge has the status {state}, so it cannot be corrected"
            ),
        },
    };
    let old = current;
    let at = time::now_rfc3339();
    ledger.set_question_correction(&qid, &serde_json::to_string(&answer)?, &new.to_string())?;
    let already = ledger
        .human_rows(&qid)?
        .iter()
        .any(|h| h.kind == "correct" && h.value.as_deref() == Some(answer.text.as_str()));
    if !already {
        ledger.add_human(&HumanRow {
            at: at.clone(),
            ticket: Some(question.row.ticket.clone()),
            kind: "correct".into(),
            object: qid.clone(),
            value: Some(answer.text.clone()),
            reason: (!answer.why.is_empty()).then(|| answer.why.clone()),
            seconds: None,
        })?;
        let sources = judge_entry.map(|e| e.sources).unwrap_or_default();
        count(ctx, memory, &sources, UseKind::Corrected, memory.person());
    }

    let told = if ledger.questions_resumed(&question.row.turn)? {
        super::resume::append_correction(
            ctx,
            &question,
            &answer,
            &old.to_string(),
            &new.to_string(),
        )?;
        let key = TicketKey::parse(&question.row.ticket)?;
        let mut text = format!(
            "A correction from the person: the answer to \"{}\" ({qid}) is \"{}\", not \"{}\" as the Judge said. \
             The decision {new} replaces {old}.",
            question.card.text.trim(),
            answer.text,
            wrong
        );
        if !answer.why.is_empty() {
            text.push_str(&format!(" Why: {}", answer.why));
        }
        Some(turn::tell(ctx, ledger, &key, &text, "user")?)
    } else {
        None
    };
    ledger.add_event(
        &Event::now("user", "correct")
            .ticket(&question.row.ticket)
            .detail(
                json!({"card": qid, "old": old.to_string(), "new": new.to_string(), "told": told}),
            ),
    )?;
    ctx.log(&format!("questions: {qid} corrected: {new} replaces {old}"));
    Ok(Corrected {
        id: qid,
        old,
        new,
        told,
    })
}

/// `vision correct ID "RIGHT ANSWER" [--why "W"]`.
pub fn correct_command(ctx: &Ctx, id: &str, right: &str, why: Option<&str>) -> Result<()> {
    ctx.require_setup()?;
    let ledger = ctx.ledger()?;
    let memory = Memory::from_ctx(ctx)?;
    let corrected = correct_card(ctx, &ledger, &memory, id, right, why)?;
    println!(
        "Corrected {}: {} replaces the decision {} of the Judge.",
        corrected.id, corrected.new, corrected.old
    );
    match &corrected.told {
        Some(Told::Running(turn)) => println!(
            "Sent the correction to the running turn {turn}. The session gets it at its next step."
        ),
        Some(Told::Stored) => {
            println!("No turn of the ticket runs, so the correction waits for its next turn.")
        }
        None => println!(
            "The answers did not go back to the agent yet, so they carry the right answer."
        ),
    }
    Ok(())
}
