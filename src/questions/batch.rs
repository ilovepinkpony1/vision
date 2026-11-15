//! The batch of an ask step (section 11.3): the cards of some documents of the ticket folder,
//! which go to the Judge and the person at once, as the cards of a turn do. No turn asked
//! them, so their asking id is a batch id, `ask:<KEY>:<step>:<visit>`, and no session resumes
//! with the answers: the pipeline writes them to `answers.md` and moves the ticket.

use anyhow::Result;
use serde_json::Value;

use super::{Agents, Question};
use crate::ctx::Ctx;
use crate::ledger::questions::{NewQuestion, STATE_ANSWERED, STATE_NEEDLESS};
use crate::ledger::{Event, Ledger};
use crate::paths::TicketKey;

/// The start of each batch id.
pub const PREFIX: &str = "ask:";

/// The batch id of one visit of an ask step.
pub fn id(key: &TicketKey, step: &str, seq: u32) -> String {
    format!("{PREFIX}{key}:{step}:{seq}")
}

/// The parts of a batch id.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Batch {
    pub key: String,
    pub step: String,
    pub seq: u32,
}

/// The parts of a batch id, or `None` for a turn id.
pub fn parse(id: &str) -> Option<Batch> {
    let rest = id.strip_prefix(PREFIX)?;
    let mut parts = rest.splitn(3, ':');
    let key = parts.next()?.to_string();
    let step = parts.next()?.to_string();
    let seq = parts.next()?.parse().ok()?;
    Some(Batch { key, step, seq })
}

/// Adds the rows of the cards of a batch, each card as JSON, in one transaction with `event`.
/// A batch that already has rows gets none.
pub fn add(
    ledger: &Ledger,
    key: &TicketKey,
    batch: &str,
    cards: &[String],
    created: &str,
    event: &Event,
) -> Result<Vec<String>> {
    let rows: Vec<NewQuestion<'_>> = cards
        .iter()
        .map(|card| NewQuestion {
            ticket: key.as_str(),
            turn: batch,
            card,
            created,
        })
        .collect();
    ledger.add_question_batch(&rows, event)
}

/// The cards of a batch with their answers, in the order of the cards.
pub fn questions(ledger: &Ledger, batch: &str) -> Result<Vec<Question>> {
    let mut agents = Agents::default();
    ledger
        .questions_of_turn(batch)?
        .into_iter()
        .map(|row| Question::load(ledger, row, &mut agents))
        .collect()
}

/// True when each card has a final answer: an answer, or the mark needless.
pub fn complete(questions: &[Question]) -> bool {
    questions
        .iter()
        .all(|q| q.row.state == STATE_ANSWERED || q.row.state == STATE_NEEDLESS)
}

/// True when the answer of a card changes the plan: it is not the recommended choice, the card
/// had no recommendation, or the answer is in the words of the person. A card that the person
/// marked as needless changes nothing.
pub fn changes(question: &Question) -> bool {
    if question.needless().is_some() {
        return false;
    }
    let Some(answer) = &question.answer else {
        return true;
    };
    let Some(recommended) = question.card.recommended_index() else {
        return true;
    };
    answer.choice != Some(recommended)
}

/// Appends the answers of a batch to `answers.md`, once for each card, in the form of the
/// answers of a turn.
pub fn write_answers(
    ctx: &Ctx,
    key: &TicketKey,
    batch: &str,
    questions: &[Question],
) -> Result<()> {
    super::resume::append_answers(ctx, key, batch, questions)
}

/// The agent that asked a card of a batch: `asked_by` of the card.
pub(crate) fn asker(card: &str) -> Option<String> {
    serde_json::from_str::<Value>(card)
        .ok()?
        .get("asked_by")?
        .as_str()
        .map(str::to_string)
        .filter(|a| !a.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_batch_id_round_trips_and_a_turn_id_is_no_batch() {
        let key = TicketKey::parse("DEMO-12").unwrap();
        let text = id(&key, "questions", 7);
        assert_eq!(text, "ask:DEMO-12:questions:7");
        assert_eq!(
            parse(&text),
            Some(Batch {
                key: "DEMO-12".into(),
                step: "questions".into(),
                seq: 7
            })
        );
        assert_eq!(parse("20261116T143000Z-DEMO-12-build-1"), None);
        assert_eq!(parse("ask:DEMO-12:questions:x"), None);
    }
}
