//! The answers of several cards at once (section 8.4): `vision answer --file FILE`, and the send
//! of the question form of the app. One ledger transaction starts each answer, or none: a card
//! that no longer waits, a choice that does not exist, or an empty text refuses the whole set,
//! and the error names the card and the reason. The decision files then go to the memory in
//! one commit. If that commit fails, each card waits for the person again.
//!
//! When the second transaction fails after that commit, each answer is saved: the memory has
//! its decision file, and its row waits in `recording` until a tick finishes it. The error then
//! starts with [`SAVED`] and says so.
//!
//! The file is a JSON object with one entry for each card id. The app keeps its drafts in the
//! same form:
//!
//! ```json
//! {"Q-0001": {"choice": 2, "why": "W", "when": "C"},
//!  "Q-0002": {"text": "T"},
//!  "Q-0003": {"needless": "code"}}
//! ```
//!
//! `choice` counts from 1, as `vision needs` shows the choices. A needless card takes no `why`
//! and no `when`, as with `vision answer ID --needless`.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use anyhow::{anyhow, bail, Context, Result};
use serde::{Deserialize, Serialize};

use super::card::Reply;
use super::commands::{
    agreed, answer_human, answered_event, needless_event, needless_human, needless_line, next_line,
    prepare, saved_line, turn_complete, waiting, Answered, PersonAnswer, Prepared,
};
use super::{needless_name, record, Question};
use crate::cli::Needless;
use crate::ctx::Ctx;
use crate::ledger::questions::HumanRow;
use crate::ledger::Ledger;
use crate::memory::{DecisionId, Memory, NewDecision, UseKind};
use crate::util::time;

/// One answer of the file.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Entry {
    /// The number of a choice, from 1.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub choice: Option<u64>,
    /// An answer in the words of the person.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    /// For a card that did not need the person: where the answer already was, `code`,
    /// `ticket`, `memory`, or `no-matter`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub needless: Option<String>,
    /// Why this answer is right.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub why: String,
    /// Where the decision applies.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub when: String,
}

/// What one answer of a set gives: an answer, or the mark needless with its place.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SetReply {
    Answer(PersonAnswer),
    Needless(Needless),
}

/// One answer of a set.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SetAnswer {
    /// The id of the card.
    pub id: String,
    pub reply: SetReply,
}

/// The place of a needless card by its name, such as `no-matter`.
pub fn needless_of(name: &str) -> Option<Needless> {
    Some(match name.trim() {
        "code" => Needless::Code,
        "ticket" => Needless::Ticket,
        "memory" => Needless::Memory,
        "no-matter" => Needless::NoMatter,
        _ => return None,
    })
}

impl Entry {
    /// What the entry gives, or why it is not valid.
    pub fn reply(&self) -> Result<SetReply, String> {
        let given = [
            self.choice.is_some(),
            self.text.is_some(),
            self.needless.is_some(),
        ];
        if given.iter().filter(|g| **g).count() != 1 {
            return Err("give exactly one of choice, text, or needless".into());
        }
        if let Some(place) = &self.needless {
            if !self.why.trim().is_empty() || !self.when.trim().is_empty() {
                return Err("a needless card takes no why and no when".into());
            }
            return needless_of(place).map(SetReply::Needless).ok_or_else(|| {
                format!("`{place}` is not a place of a needless card. Use code, ticket, memory, or no-matter")
            });
        }
        let reply = match (self.choice, &self.text) {
            (Some(0), _) => {
                return Err("choice counts from 1, as `vision needs` shows the choices".into())
            }
            (Some(n), _) => Reply::Choice(usize::try_from(n - 1).unwrap_or(usize::MAX)),
            (None, Some(text)) if text.trim().is_empty() => {
                return Err("the text of the answer is empty".into())
            }
            (None, Some(text)) => Reply::Text(text.trim().to_string()),
            (None, None) => return Err("give exactly one of choice, text, or needless".into()),
        };
        Ok(SetReply::Answer(PersonAnswer {
            reply,
            why: self.why.trim().to_string(),
            when: self.when.trim().to_string(),
            seconds: None,
        }))
    }
}

/// The entries of a file of answers, by card id. The error says what the form must be.
pub fn parse_entries(text: &str) -> Result<BTreeMap<String, Entry>> {
    serde_json::from_str(text).context(
        "the file is not a JSON object of answers, such as {\"Q-0001\": {\"choice\": 2, \"why\": \"W\"}}",
    )
}

/// The text of a file of answers.
pub fn render_entries(entries: &BTreeMap<String, Entry>) -> String {
    let mut text = serde_json::to_string_pretty(entries).unwrap_or_else(|_| "{}".into());
    text.push('\n');
    text
}

/// The answers of a file, in the order of their card ids.
pub fn parse(text: &str) -> Result<Vec<SetAnswer>> {
    let entries = parse_entries(text)?;
    if entries.is_empty() {
        bail!("the file has no answer");
    }
    let mut answers = Vec::new();
    for (id, entry) in entries {
        let reply = entry
            .reply()
            .map_err(|e| anyhow!("the answer to {} in the file is not valid: {e}", id.trim()))?;
        answers.push(SetAnswer {
            id: id.trim().to_string(),
            reply,
        });
    }
    answers.sort_by(|a, b| (a.id.len(), &a.id).cmp(&(b.id.len(), &b.id)));
    Ok(answers)
}

/// One answer of the set after its test, before its decision file.
enum Started {
    Answer {
        prepared: Box<Prepared>,
        decision: Box<NewDecision>,
        human: HumanRow,
    },
    Needless {
        question: Box<Question>,
        name: &'static str,
        human: HumanRow,
    },
}

impl Started {
    fn question(&self) -> &Question {
        match self {
            Started::Answer { prepared, .. } => &prepared.question,
            Started::Needless { question, .. } => question,
        }
    }
}

/// The words at the start of the error of a set when Vision saved each answer and only the end
/// of their records in the ledger failed. The app reads them to tell this end from a refusal.
pub const SAVED: &str = "Vision saved each answer of the set";

/// The error of a set whose second transaction failed: the decision files `ids` are in the
/// memory, the row of each answer waits in `recording`, and each needless mark is final.
fn unfinished(ids: &[DecisionId], error: &anyhow::Error) -> String {
    let names: Vec<String> = ids.iter().map(ToString::to_string).collect();
    let minutes = record::RECORDING_GRACE_SECS / 60;
    match names.as_slice() {
        [] => format!("{SAVED}, but the ledger could not add their events: {error:#}"),
        [one] => format!(
            "{SAVED}, and the memory has the decision file {one}. The ledger could not finish its record: {error:#}. A tick after {minutes} minutes finishes it"
        ),
        many => format!(
            "{SAVED}, and the memory has the decision files {}. The ledger could not finish their records: {error:#}. A tick after {minutes} minutes finishes them",
            many.join(", ")
        ),
    }
}

/// The reason in front of each refusal of a set.
fn refused(id: &str) -> String {
    format!("no answer was recorded, because the answer to {id} was refused")
}

/// Reads and tests one answer, and builds what the set writes for it. Writes nothing.
fn start(ledger: &Ledger, memory: &Memory, answer: &SetAnswer, at: &str) -> Result<Started> {
    match &answer.reply {
        SetReply::Answer(given) => {
            // `prepare` refuses an empty text, and the memory tests the decision when it
            // writes it.
            let prepared = prepare(ledger, memory, &answer.id, given)?;
            let decision = record::new_decision(memory, &prepared.question, &prepared.answer)?;
            let human = answer_human(&prepared.question, &prepared.answer, at, given.seconds);
            Ok(Started::Answer {
                prepared: Box::new(prepared),
                decision: Box::new(decision),
                human,
            })
        }
        SetReply::Needless(reason) => {
            let question = waiting(ledger, &answer.id)?;
            let name = needless_name(*reason);
            let human = needless_human(&question, name, at, None);
            Ok(Started::Needless {
                question: Box::new(question),
                name,
                human,
            })
        }
    }
}

/// Records the answers of a set: each answer, or none. Each answer is the answer of `vision
/// answer` for one card: a decision file with the person as the writer, the row of the card,
/// one `human` row, and the event. A needless mark gets no decision file. Gives what each
/// answer did, in the order of the set.
pub fn answer_set(
    ctx: &Ctx,
    ledger: &Ledger,
    memory: &Memory,
    answers: &[SetAnswer],
) -> Result<Vec<Answered>> {
    if answers.is_empty() {
        bail!("there is no answer to record");
    }
    let mut ids = BTreeSet::new();
    for answer in answers {
        if !ids.insert(answer.id.trim()) {
            bail!(
                "no answer was recorded, because {} has two answers",
                answer.id.trim()
            );
        }
    }
    let at = time::now_rfc3339();
    // Each answer is read and tested before anything is written.
    let mut started = Vec::new();
    for answer in answers {
        started
            .push(start(ledger, memory, answer, &at).with_context(|| refused(answer.id.trim()))?);
    }
    // One transaction: each card still waits for the person, and each gets its answer in the
    // state `recording`, or its needless mark, with its `human` row.
    ledger.in_transaction(|ledger| {
        for item in &started {
            let id = item.question().row.id.as_str();
            let done = match item {
                Started::Answer {
                    prepared, human, ..
                } => ledger.start_person_answer(
                    id,
                    &serde_json::to_string(&prepared.answer)?,
                    prepared.took,
                    &at,
                    human,
                )?,
                Started::Needless { name, human, .. } => {
                    let marked = ledger.set_question_needless(id, name, &at)?;
                    if marked {
                        ledger.add_human(human)?;
                    }
                    marked
                }
            };
            if !done {
                bail!(
                    "{}: {id} got an answer from a different command meanwhile",
                    refused(id)
                );
            }
        }
        Ok(())
    })?;
    // The decision files in one commit. If it fails, each card waits again.
    let news: Vec<NewDecision> = started
        .iter()
        .filter_map(|item| match item {
            Started::Answer { decision, .. } => Some((**decision).clone()),
            Started::Needless { .. } => None,
        })
        .collect();
    let decisions = match memory.add_decisions(&news) {
        Ok(decisions) => decisions,
        Err(e) => {
            undo(ledger, &started)?;
            return Err(e.context(
                "no answer was recorded, because the decision files could not be written. Each card still waits for your answer",
            ));
        }
    };
    let ids: Vec<DecisionId> = decisions.into_iter().map(|d| d.id).collect();
    let mut decisions = ids.iter().copied();
    let mut out = Vec::new();
    let finished = ledger.in_transaction(|ledger| {
        for item in &started {
            let question = item.question();
            let id = question.row.id.clone();
            match item {
                Started::Answer { prepared, .. } => {
                    let decision = decisions
                        .next()
                        .context("the memory gave fewer decision files than answers")?;
                    ledger.finish_recording(&id, &decision.to_string())?;
                    ledger.add_event(&answered_event(question, decision, prepared.took))?;
                    out.push(Answered {
                        id,
                        decision: Some(decision),
                        took_proposal: prepared.took,
                        turn_complete: false,
                    });
                }
                Started::Needless { name, .. } => {
                    ledger.add_event(&needless_event(question, name))?;
                    out.push(Answered {
                        id,
                        decision: None,
                        took_proposal: None,
                        turn_complete: false,
                    });
                }
            }
        }
        Ok(())
    });
    // The answers are saved now: an error says so, and a tick finishes each record.
    if let Err(e) = finished {
        let text = unfinished(&ids, &e);
        ctx.log(&format!("questions: {text}"));
        bail!(text);
    }
    let mut principles = Vec::new();
    for (item, answered) in started.iter().zip(&mut out) {
        let question = item.question();
        match (item, &answered.decision) {
            (Started::Answer { prepared, .. }, Some(decision)) => {
                ctx.log(&record::answered_line(
                    &answered.id,
                    &prepared.answer.by,
                    *decision,
                ));
                principles.extend(agreed(memory, question, prepared.took));
            }
            (Started::Needless { name, .. }, _) => {
                ctx.log(&needless_line(&answered.id, name));
            }
            _ => {}
        }
        answered.turn_complete = turn_complete(ledger, &question.row.turn)?;
    }
    record::count_ids(ctx, memory, &principles, UseKind::Agreed, memory.person());
    Ok(out)
}

/// Undoes the first transaction of [`answer_set`]: each card waits for the person again, and
/// the `human` rows of the set go.
fn undo(ledger: &Ledger, started: &[Started]) -> Result<()> {
    ledger.in_transaction(|ledger| {
        for item in started {
            match item {
                Started::Answer {
                    prepared, human, ..
                } => ledger.undo_person_answer(
                    &prepared.question.row.id,
                    &prepared.question.row.state,
                    human,
                )?,
                Started::Needless {
                    question, human, ..
                } => ledger.undo_question_needless(&question.row, human)?,
            }
        }
        Ok(())
    })
}

/// `vision answer --file FILE`.
pub fn answer_file_command(ctx: &Ctx, path: &Path) -> Result<()> {
    ctx.require_setup()?;
    let text = std::fs::read_to_string(path)
        .with_context(|| format!("could not read the answers in {}", path.display()))?;
    let answers = parse(&text)?;
    let ledger = ctx.ledger()?;
    let memory = Memory::from_ctx(ctx)?;
    let answered = answer_set(ctx, &ledger, &memory, &answers)?;
    for one in &answered {
        println!("{}", saved_line(one));
    }
    println!("{}", next_line(answered.iter().all(|a| a.turn_complete)));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn entry(json: serde_json::Value) -> Entry {
        serde_json::from_value(json).unwrap()
    }

    #[test]
    fn an_entry_gives_one_reply_or_says_why_not() {
        let choice = entry(json!({"choice": 2, "why": " People compare. ", "when": "always"}));
        assert_eq!(
            choice.reply(),
            Ok(SetReply::Answer(PersonAnswer {
                reply: Reply::Choice(1),
                why: "People compare.".into(),
                when: "always".into(),
                seconds: None,
            }))
        );
        assert!(matches!(
            entry(json!({"text": "Keep it"})).reply(),
            Ok(SetReply::Answer(PersonAnswer { reply: Reply::Text(t), .. })) if t == "Keep it"
        ));
        assert_eq!(
            entry(json!({"needless": "no-matter"})).reply(),
            Ok(SetReply::Needless(Needless::NoMatter))
        );
        for (bad, why) in [
            (json!({}), "exactly one"),
            (json!({"choice": 1, "text": "x"}), "exactly one"),
            (json!({"choice": 0}), "counts from 1"),
            (json!({"text": "  "}), "empty"),
            (json!({"needless": "code", "why": "w"}), "no why"),
            (json!({"needless": "somewhere"}), "not a place"),
        ] {
            let text = entry(bad.clone()).reply().unwrap_err();
            assert!(text.contains(why), "{bad}: {text}");
        }
        assert!(serde_json::from_value::<Entry>(json!({"choise": 1})).is_err());
    }

    #[test]
    fn a_file_parses_in_the_order_of_the_ids_and_renders_back() {
        let text = r#"{"Q-0010": {"needless": "code"}, "Q-0002": {"choice": 1}, "Q-0009": {"text": "T", "why": "W"}}"#;
        let answers = parse(text).unwrap();
        let ids: Vec<&str> = answers.iter().map(|a| a.id.as_str()).collect();
        assert_eq!(ids, ["Q-0002", "Q-0009", "Q-0010"]);
        let entries = parse_entries(text).unwrap();
        assert_eq!(parse_entries(&render_entries(&entries)).unwrap(), entries);
        assert!(render_entries(&entries).contains("\"why\": \"W\""));
        assert!(!render_entries(&entries).contains("\"when\""));
        let error = parse(r#"{"Q-0001": {"choice": 0}}"#).unwrap_err();
        assert!(format!("{error:#}").contains("the answer to Q-0001 in the file"));
        assert!(parse("{}").is_err());
        assert!(format!("{:#}", parse("[1]").unwrap_err()).contains("JSON object of answers"));
    }
}
