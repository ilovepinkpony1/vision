//! The queries of questions (section 8): the `questions` rows of the cards, and the `human`
//! rows of the answers and the corrections of the person.
//!
//! The states of a card: `new` (taken in, no Judge yet), `judging` (a Judge turn runs for it),
//! `recording` (the answer is known and its decision file is being written), `answered`,
//! `proposed` and `asked` (both wait for the person), and `needless` (the person marked it).
//! `answered` and `needless` are final.

use anyhow::Result;
use rusqlite::{params, params_from_iter, OptionalExtension, Row};
use serde::Serialize;

use super::Ledger;

pub const STATE_NEW: &str = "new";
pub const STATE_JUDGING: &str = "judging";
pub const STATE_RECORDING: &str = "recording";
pub const STATE_ANSWERED: &str = "answered";
pub const STATE_PROPOSED: &str = "proposed";
pub const STATE_ASKED: &str = "asked";
pub const STATE_NEEDLESS: &str = "needless";

/// The external id of the event that records the intake of the cards of one turn.
pub fn intake_mark(turn: &str) -> String {
    format!("questions-intake:{turn}")
}

/// The external id of the event that records the resume of one asking turn.
pub fn resume_mark(turn: &str) -> String {
    format!("questions-resumed:{turn}")
}

/// One row of `questions`. JSON values stay as text.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct QuestionRow {
    pub id: String,
    pub ticket: String,
    /// The asking turn.
    pub turn: String,
    /// The card as JSON.
    pub card: String,
    pub state: String,
    pub outcome: Option<String>,
    pub basis: Option<String>,
    pub sure: Option<bool>,
    /// The view of the Judge as JSON: `{"judge": <its entry or null>, "rule": <text or null>}`.
    pub proposal: Option<String>,
    /// The sources of the Judge as a JSON list.
    pub sources: Option<String>,
    /// The final answer as JSON: `{"choice", "text", "by", "why", "when"}`.
    pub answer: Option<String>,
    pub took_proposal: Option<bool>,
    /// `needless:<reason>` for a card that the person marked as needless.
    pub flag: Option<String>,
    /// The id of the decision file of the answer.
    pub decision: Option<String>,
    pub judge_turn: Option<String>,
    pub created: Option<String>,
    pub shown: Option<String>,
    pub answered: Option<String>,
}

/// A new row, before it has an id.
#[derive(Clone, Debug, PartialEq)]
pub struct NewQuestion<'a> {
    pub ticket: &'a str,
    pub turn: &'a str,
    pub card: &'a str,
    pub created: &'a str,
}

/// What the rules in code decided for one card, as the row keeps it.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct RulingRow<'a> {
    pub state: &'a str,
    pub outcome: &'a str,
    pub basis: Option<&'a str>,
    pub sure: Option<bool>,
    pub proposal: &'a str,
    pub sources: Option<&'a str>,
    /// For the outcome `answer`: the answer of the Judge.
    pub answer: Option<&'a str>,
    pub answered: Option<&'a str>,
}

/// One row of `human`.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct HumanRow {
    pub at: String,
    pub ticket: Option<String>,
    /// `answer`, `needless`, or `correct`.
    pub kind: String,
    /// The question id.
    pub object: String,
    pub value: Option<String>,
    pub reason: Option<String>,
    pub seconds: Option<i64>,
}

const QUESTION_COLUMNS: &str =
    "id, coalesce(ticket, ''), coalesce(turn, ''), coalesce(card, '{}'), coalesce(state, ''), \
     outcome, basis, sure, proposal, sources, answer, took_proposal, flag, decision, judge_turn, \
     created, shown, answered";

fn question_row(row: &Row<'_>) -> rusqlite::Result<QuestionRow> {
    Ok(QuestionRow {
        id: row.get(0)?,
        ticket: row.get(1)?,
        turn: row.get(2)?,
        card: row.get(3)?,
        state: row.get(4)?,
        outcome: row.get(5)?,
        basis: row.get(6)?,
        sure: row.get::<_, Option<i64>>(7)?.map(|v| v != 0),
        proposal: row.get(8)?,
        sources: row.get(9)?,
        answer: row.get(10)?,
        took_proposal: row.get::<_, Option<i64>>(11)?.map(|v| v != 0),
        flag: row.get(12)?,
        decision: row.get(13)?,
        judge_turn: row.get(14)?,
        created: row.get(15)?,
        shown: row.get(16)?,
        answered: row.get(17)?,
    })
}

/// The SQL that orders the rows of one batch in the order of the cards.
const BY_ID: &str = "ORDER BY length(id), id";

impl Ledger {
    /// Adds the rows of the cards of one turn, each with the next free id `Q-0001`, in one
    /// transaction with the event `intake`. Gives the ids. A turn that already has rows gets
    /// none, and the call gives an empty list, so that a repeated tick adds nothing.
    pub fn add_questions(
        &mut self,
        rows: &[NewQuestion<'_>],
        event: &super::Event,
    ) -> Result<Vec<String>> {
        let tx = self
            .conn_mut()
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let mut ids = Vec::new();
        if let Some(first) = rows.first() {
            let had: i64 = tx.query_row(
                "SELECT count(*) FROM questions WHERE turn = ?1",
                [first.turn],
                |r| r.get(0),
            )?;
            if had > 0 {
                return Ok(ids);
            }
        }
        let mut next: i64 = tx.query_row(
            "SELECT coalesce(max(CAST(substr(id, 3) AS INTEGER)), 0) FROM questions WHERE id LIKE 'Q-%'",
            [],
            |r| r.get(0),
        )?;
        for row in rows {
            next += 1;
            let id = format!("Q-{next:04}");
            tx.execute(
                "INSERT INTO questions (id, ticket, turn, card, state, created) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![id, row.ticket, row.turn, row.card, STATE_NEW, row.created],
            )?;
            ids.push(id);
        }
        let detail = (!event.detail.is_null()).then(|| event.detail.to_string());
        tx.execute(
            "INSERT INTO events (at, actor, ticket, kind, detail, external_id) VALUES (?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT (external_id) DO NOTHING",
            params![event.at, event.actor, event.ticket, event.kind, detail, event.external_id],
        )?;
        tx.commit()?;
        Ok(ids)
    }

    /// One row, or `None`.
    pub fn question(&self, id: &str) -> Result<Option<QuestionRow>> {
        let sql = format!("SELECT {QUESTION_COLUMNS} FROM questions WHERE id = ?1");
        Ok(self.conn().query_row(&sql, [id], question_row).optional()?)
    }

    /// The row whose answer is this decision file, or `None`.
    pub fn question_by_decision(&self, decision: &str) -> Result<Option<QuestionRow>> {
        let sql = format!(
            "SELECT {QUESTION_COLUMNS} FROM questions WHERE decision = ?1 ORDER BY id LIMIT 1"
        );
        Ok(self
            .conn()
            .query_row(&sql, [decision], question_row)
            .optional()?)
    }

    /// The rows of the cards of one asking turn, in the order of the cards.
    pub fn questions_of_turn(&self, turn: &str) -> Result<Vec<QuestionRow>> {
        let sql = format!("SELECT {QUESTION_COLUMNS} FROM questions WHERE turn = ?1 {BY_ID}");
        let mut stmt = self.conn().prepare(&sql)?;
        let rows = stmt.query_map([turn], question_row)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// The rows in one of these states, oldest first.
    pub fn questions_in_states(&self, states: &[&str]) -> Result<Vec<QuestionRow>> {
        if states.is_empty() {
            return Ok(Vec::new());
        }
        let marks = vec!["?"; states.len()].join(", ");
        let sql = format!(
            "SELECT {QUESTION_COLUMNS} FROM questions WHERE state IN ({marks})
             ORDER BY created, length(id), id"
        );
        let mut stmt = self.conn().prepare(&sql)?;
        let rows = stmt.query_map(params_from_iter(states.iter()), question_row)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// The work turns that ended with the kind `questions` and have no rows yet, oldest first.
    /// The collector records the kind of the result in its event `turn-ended`.
    pub fn turns_with_new_questions(&self) -> Result<Vec<String>> {
        let mut stmt = self.conn().prepare(
            "SELECT t.id FROM turns t JOIN events e ON e.external_id = 'turn-ended:' || t.id
             WHERE t.status = 'done' AND coalesce(t.agent, '') NOT IN ('judge', 'curator')
               AND json_valid(e.detail) AND json_extract(e.detail, '$.result_kind') = 'questions'
               AND NOT EXISTS (SELECT 1 FROM questions q WHERE q.turn = t.id)
             ORDER BY t.started, t.id",
        )?;
        let rows = stmt.query_map([], |row| row.get(0))?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// The asking turns whose cards each have a final answer, and that were not resumed yet,
    /// oldest first.
    pub fn turns_ready_to_resume(&self) -> Result<Vec<String>> {
        let mut stmt = self.conn().prepare(
            "SELECT q.turn FROM questions q
             WHERE NOT EXISTS (SELECT 1 FROM events e WHERE e.external_id = 'questions-resumed:' || q.turn)
             GROUP BY q.turn
             HAVING sum(CASE WHEN q.state IN ('answered', 'needless') THEN 0 ELSE 1 END) = 0
             ORDER BY min(q.created), q.turn",
        )?;
        let rows = stmt.query_map([], |row| row.get(0))?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// True when the asking turn was resumed with its answers.
    pub fn questions_resumed(&self, turn: &str) -> Result<bool> {
        Ok(self.event_detail(&resume_mark(turn))?.is_some())
    }

    /// The detail of the event with this external id: `Some(None)` for an event with no detail.
    pub fn event_detail(&self, external_id: &str) -> Result<Option<Option<String>>> {
        Ok(self
            .conn()
            .query_row(
                "SELECT detail FROM events WHERE external_id = ?1",
                [external_id],
                |row| row.get(0),
            )
            .optional()?)
    }

    /// Gives the rows to a Judge turn: `new` -> `judging`. Gives the number of rows.
    pub fn set_questions_judging(&self, ids: &[String], judge_turn: &str) -> Result<usize> {
        let mut n = 0;
        for id in ids {
            n += self.conn().execute(
                "UPDATE questions SET state = ?3, judge_turn = ?2 WHERE id = ?1 AND state = ?4",
                params![id, judge_turn, STATE_JUDGING, STATE_NEW],
            )?;
        }
        Ok(n)
    }

    /// Moves the rows that a Judge turn judges to the turn that resumes it with `continue`.
    pub fn move_judge_turn(&self, from: &str, to: &str) -> Result<usize> {
        Ok(self.conn().execute(
            "UPDATE questions SET judge_turn = ?2 WHERE judge_turn = ?1 AND state = ?3",
            params![from, to, STATE_JUDGING],
        )?)
    }

    /// Writes what the rules decided for one card that is still in one of the states `from`.
    /// Gives false when the row was no longer in such a state.
    pub fn set_question_ruling(
        &self,
        id: &str,
        from: &[&str],
        ruling: &RulingRow<'_>,
        judge_turn: Option<&str>,
    ) -> Result<bool> {
        let marks = (0..from.len())
            .map(|i| format!("?{}", i + 11))
            .collect::<Vec<_>>()
            .join(", ");
        let sql = format!(
            "UPDATE questions SET state = ?2, outcome = ?3, basis = ?4, sure = ?5, proposal = ?6,
                 sources = ?7, answer = ?8, answered = ?9, judge_turn = coalesce(?10, judge_turn)
             WHERE id = ?1 AND state IN ({marks})"
        );
        let mut values: Vec<rusqlite::types::Value> = vec![
            id.to_string().into(),
            ruling.state.to_string().into(),
            ruling.outcome.to_string().into(),
            opt_text(ruling.basis),
            ruling.sure.map(i64::from).into(),
            ruling.proposal.to_string().into(),
            opt_text(ruling.sources),
            opt_text(ruling.answer),
            opt_text(ruling.answered),
            opt_text(judge_turn),
        ];
        values.extend(from.iter().map(|s| s.to_string().into()));
        Ok(self.conn().execute(&sql, params_from_iter(values))? == 1)
    }

    /// Starts the answer of the person: a row that waits for the person gets `recording` with
    /// the answer, and the `human` row of the answer is written in the same transaction. So a
    /// crash after the commit of the decision file never loses the `human` row: the row comes
    /// first, and a later tick ends the recording. Gives false, and writes nothing, when the row
    /// no longer waited for the person.
    pub fn start_person_answer(
        &self,
        id: &str,
        answer: &str,
        took_proposal: Option<bool>,
        at: &str,
        human: &HumanRow,
    ) -> Result<bool> {
        self.in_transaction(|ledger| {
            let started = ledger.conn().execute(
                "UPDATE questions SET state = ?2, answer = ?3, took_proposal = ?4, answered = ?5
                 WHERE id = ?1 AND state IN (?6, ?7)",
                params![
                    id,
                    STATE_RECORDING,
                    answer,
                    took_proposal.map(i64::from),
                    at,
                    STATE_PROPOSED,
                    STATE_ASKED
                ],
            )? == 1;
            if started {
                ledger.add_human(human)?;
            }
            Ok(started)
        })
    }

    /// Undoes [`Ledger::start_person_answer`] after the decision file could not be written: the
    /// row waits for the person again, and the `human` row of that answer goes.
    pub fn undo_person_answer(&self, id: &str, state: &str, human: &HumanRow) -> Result<()> {
        self.in_transaction(|ledger| {
            let undone = ledger.conn().execute(
                "UPDATE questions SET state = ?2, answer = NULL, took_proposal = NULL, answered = NULL
                 WHERE id = ?1 AND state = ?3",
                params![id, state, STATE_RECORDING],
            )?;
            if undone == 1 {
                ledger.conn().execute(
                    "DELETE FROM human WHERE id = (SELECT max(id) FROM human
                     WHERE object = ?1 AND kind = ?2 AND at = ?3)",
                    params![id, human.kind, human.at],
                )?;
            }
            Ok(())
        })
    }

    /// Ends a recording: the row gets its decision file and the state `answered`.
    pub fn finish_recording(&self, id: &str, decision: &str) -> Result<bool> {
        Ok(self.conn().execute(
            "UPDATE questions SET state = ?2, decision = ?3 WHERE id = ?1 AND state = ?4",
            params![id, STATE_ANSWERED, decision, STATE_RECORDING],
        )? == 1)
    }

    /// Marks a card that waits for the person as needless, with its reason.
    pub fn set_question_needless(&self, id: &str, reason: &str, at: &str) -> Result<bool> {
        Ok(self.conn().execute(
            "UPDATE questions SET state = ?2, flag = ?3, answered = ?4
             WHERE id = ?1 AND state IN (?5, ?6)",
            params![
                id,
                STATE_NEEDLESS,
                format!("needless:{reason}"),
                at,
                STATE_PROPOSED,
                STATE_ASKED
            ],
        )? == 1)
    }

    /// Records the first time that the person saw the card.
    pub fn set_question_shown(&self, id: &str, at: &str) -> Result<()> {
        self.conn().execute(
            "UPDATE questions SET shown = ?2 WHERE id = ?1 AND shown IS NULL",
            params![id, at],
        )?;
        Ok(())
    }

    /// Replaces the answer of the Judge with the correction of the person.
    pub fn set_question_correction(&self, id: &str, answer: &str, decision: &str) -> Result<()> {
        self.conn().execute(
            "UPDATE questions SET answer = ?2, decision = ?3 WHERE id = ?1",
            params![id, answer, decision],
        )?;
        Ok(())
    }

    /// The rows that the Judge answered alone, newest first. With `since`, only the answers at
    /// or after that time (RFC 3339 UTC).
    pub fn questions_answered_by_judge(&self, since: Option<&str>) -> Result<Vec<QuestionRow>> {
        let sql = format!(
            "SELECT {QUESTION_COLUMNS} FROM questions
             WHERE outcome = 'answer' AND state = ?1 AND decision IS NOT NULL
               AND (?2 IS NULL OR answered >= ?2)
             ORDER BY answered DESC, length(id) DESC, id DESC"
        );
        let mut stmt = self.conn().prepare(&sql)?;
        let rows = stmt.query_map(params![STATE_ANSWERED, since], question_row)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// The last `limit` rows that the person answered (not marked as needless), newest first.
    /// A corrected answer of the Judge counts, because the answer is now the person's.
    pub fn questions_answered_by_person(&self, limit: u32) -> Result<Vec<QuestionRow>> {
        let sql = format!(
            "SELECT {QUESTION_COLUMNS} FROM questions
             WHERE state = ?1 AND json_valid(answer) AND json_extract(answer, '$.by') <> 'judge'
             ORDER BY answered DESC, length(id) DESC, id DESC LIMIT ?2"
        );
        let mut stmt = self.conn().prepare(&sql)?;
        let rows = stmt.query_map(params![STATE_ANSWERED, limit], question_row)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// The clone of a ticket, from its row.
    pub fn ticket_clone(&self, key: &str) -> Result<Option<String>> {
        let clone: Option<Option<String>> = self
            .conn()
            .query_row("SELECT clone FROM tickets WHERE key = ?1", [key], |row| {
                row.get(0)
            })
            .optional()?;
        Ok(clone.flatten().filter(|c| !c.trim().is_empty()))
    }

    /// Adds a row to `human`.
    pub fn add_human(&self, row: &HumanRow) -> Result<i64> {
        self.conn().execute(
            "INSERT INTO human (at, ticket, kind, object, value, reason, seconds)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                row.at,
                row.ticket,
                row.kind,
                row.object,
                row.value,
                row.reason,
                row.seconds
            ],
        )?;
        Ok(self.conn().last_insert_rowid())
    }

    /// The `human` rows of one question, oldest first.
    pub fn human_rows(&self, object: &str) -> Result<Vec<HumanRow>> {
        let mut stmt = self.conn().prepare(
            "SELECT coalesce(at, ''), ticket, coalesce(kind, ''), coalesce(object, ''), value, reason, seconds
             FROM human WHERE object = ?1 ORDER BY id",
        )?;
        let rows = stmt.query_map([object], |row| {
            Ok(HumanRow {
                at: row.get(0)?,
                ticket: row.get(1)?,
                kind: row.get(2)?,
                object: row.get(3)?,
                value: row.get(4)?,
                reason: row.get(5)?,
                seconds: row.get(6)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }
}

fn opt_text(value: Option<&str>) -> rusqlite::types::Value {
    match value {
        Some(text) => rusqlite::types::Value::Text(text.to_string()),
        None => rusqlite::types::Value::Null,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ledger::Event;
    use serde_json::json;

    fn new(turn: &str) -> NewQuestion<'_> {
        NewQuestion {
            ticket: "DEMO-12",
            turn,
            card: "{}",
            created: "2026-11-16T14:30:00Z",
        }
    }

    fn intake(turn: &str) -> Event {
        Event::now("tick", "questions-intake")
            .ticket("DEMO-12")
            .external_id(&intake_mark(turn))
            .detail(json!({"turn": turn}))
    }

    #[test]
    fn ids_count_up_and_a_turn_is_taken_in_once() {
        let mut ledger = Ledger::open_in_memory().unwrap();
        let ids = ledger
            .add_questions(&[new("t1"), new("t1")], &intake("t1"))
            .unwrap();
        assert_eq!(ids, ["Q-0001", "Q-0002"]);
        assert!(ledger
            .add_questions(&[new("t1")], &intake("t1"))
            .unwrap()
            .is_empty());
        let ids = ledger.add_questions(&[new("t2")], &intake("t2")).unwrap();
        assert_eq!(ids, ["Q-0003"]);
        assert!(ledger.event_detail(&intake_mark("t2")).unwrap().is_some());
        let rows = ledger.questions_of_turn("t1").unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].state, STATE_NEW);
    }

    #[test]
    fn ids_past_9999_keep_their_order() {
        let mut ledger = Ledger::open_in_memory().unwrap();
        ledger
            .conn()
            .execute_batch("INSERT INTO questions (id, turn, state) VALUES ('Q-9999', 't0', 'new')")
            .unwrap();
        let ids = ledger.add_questions(&[new("t1")], &intake("t1")).unwrap();
        assert_eq!(ids, ["Q-10000"]);
        let all: Vec<String> = ledger
            .questions_in_states(&[STATE_NEW])
            .unwrap()
            .into_iter()
            .map(|r| r.id)
            .collect();
        assert_eq!(all, ["Q-9999", "Q-10000"]);
    }

    #[test]
    fn a_turn_is_ready_to_resume_when_each_card_is_final() {
        let mut ledger = Ledger::open_in_memory().unwrap();
        let ids = ledger
            .add_questions(&[new("t1"), new("t1")], &intake("t1"))
            .unwrap();
        assert!(ledger.turns_ready_to_resume().unwrap().is_empty());
        ledger.set_questions_judging(&ids, "j1").unwrap();
        let ruling = RulingRow {
            state: STATE_ASKED,
            outcome: "ask",
            proposal: "{}",
            ..RulingRow::default()
        };
        assert!(ledger
            .set_question_ruling(&ids[0], &[STATE_JUDGING], &ruling, None)
            .unwrap());
        assert!(!ledger
            .set_question_ruling(&ids[0], &[STATE_JUDGING], &ruling, None)
            .unwrap());
        assert!(ledger
            .set_question_needless(&ids[0], "code", "now")
            .unwrap());
        let answer = RulingRow {
            state: STATE_RECORDING,
            outcome: "answer",
            proposal: "{}",
            answer: Some("{}"),
            ..RulingRow::default()
        };
        ledger
            .set_question_ruling(&ids[1], &[STATE_JUDGING], &answer, Some("j2"))
            .unwrap();
        assert!(ledger.turns_ready_to_resume().unwrap().is_empty());
        assert!(ledger.finish_recording(&ids[1], "D-0001").unwrap());
        assert_eq!(ledger.turns_ready_to_resume().unwrap(), ["t1"]);
        let row = ledger.question(&ids[1]).unwrap().unwrap();
        assert_eq!(row.judge_turn.as_deref(), Some("j2"));
        assert_eq!(
            ledger.question_by_decision("D-0001").unwrap().map(|r| r.id),
            Some(ids[1].clone())
        );
        ledger
            .add_event(&Event::now("tick", "questions-resumed").external_id(&resume_mark("t1")))
            .unwrap();
        assert!(ledger.turns_ready_to_resume().unwrap().is_empty());
        assert!(ledger.questions_resumed("t1").unwrap());
    }
}
