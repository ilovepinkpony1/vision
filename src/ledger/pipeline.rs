//! The queries of the pipeline (section 11.3): the ticket rows that it writes, the `spans` of
//! the steps, the end fact of a turn, the `findings` and `review_rows` of the review steps, and
//! the rows that `vision log` and `vision report` read.

use anyhow::Result;
use rusqlite::{params, OptionalExtension};
use serde::Serialize;

use super::events::EventRow;
use super::questions::HumanRow;
use super::{Ledger, TicketRow};

/// One row of `spans`: the time of one visit of a step.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct SpanRow {
    pub ticket: String,
    /// The step id.
    pub state: String,
    /// What the step waits on, such as `agent:builder` or `person:approve-plan`.
    pub waits_on: Option<String>,
    /// How the ticket entered the step, such as `take`, `next`, or `retry`.
    pub reason: Option<String>,
    pub started: String,
    pub ended: Option<String>,
}

/// One row of `findings`.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct FindingRow {
    pub ticket: String,
    /// The entry number of the step that found it.
    pub round: i64,
    pub fingerprint: String,
    pub priority: Option<String>,
    /// `review` or `qa`, from the `findings` key of the step in `adapter.toml`.
    pub source: String,
    /// `kept` or `rejected`.
    pub verdict: String,
    pub reason_code: Option<String>,
    pub fix_commit: Option<String>,
    /// True when an earlier round of the ticket had the same finding.
    pub came_back: bool,
}

/// One row of `review_rows`: a review comment of a person with the verdict of the step.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct ReviewRowNew {
    /// `<KEY>:<entry number of the step>`.
    pub round: String,
    pub comment_id: i64,
    pub reviewer: Option<String>,
    pub verdict: String,
    /// 1 when the verdict came with a fix commit.
    pub fix: i64,
}

/// One card of a ticket, as `vision log` and `vision report` read it.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct CardRow {
    pub id: String,
    pub ticket: String,
    pub state: String,
    pub outcome: Option<String>,
    pub basis: Option<String>,
    /// The card as JSON.
    pub card: String,
    /// The answer as JSON.
    pub answer: Option<String>,
    pub created: Option<String>,
    pub answered: Option<String>,
}

/// The cost of the turns of one step of a ticket.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct StepCost {
    pub ticket: String,
    pub step: String,
    pub turns: i64,
    pub cost_usd: f64,
}

impl Ledger {
    /// Writes the whole row of a ticket: a new one, or one that replaces an ended ticket.
    pub fn put_ticket(&self, row: &TicketRow) -> Result<()> {
        self.conn().execute(
            "INSERT INTO tickets (key, state, flags, clone, branch, pr, head, taken, updated, counts)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
             ON CONFLICT (key) DO UPDATE SET state = excluded.state, flags = excluded.flags,
                 clone = excluded.clone, branch = excluded.branch, pr = excluded.pr,
                 head = excluded.head, taken = excluded.taken, updated = excluded.updated,
                 counts = excluded.counts",
            params![
                row.key,
                row.state,
                row.flags,
                row.clone,
                row.branch,
                row.pr,
                row.head,
                row.taken,
                row.updated,
                row.counts
            ],
        )?;
        Ok(())
    }

    /// Writes the step, the flags, and the counters of a ticket. Gives false when it has no row.
    pub fn set_ticket_state(
        &self,
        key: &str,
        state: &str,
        flags: &str,
        counts: &str,
        updated: &str,
    ) -> Result<bool> {
        Ok(self.conn().execute(
            "UPDATE tickets SET state = ?2, flags = ?3, counts = ?4, updated = ?5 WHERE key = ?1",
            params![key, state, flags, counts, updated],
        )? == 1)
    }

    /// Writes the clone of a ticket, or frees it with `None`.
    pub fn set_ticket_clone(&self, key: &str, clone: Option<&str>, updated: &str) -> Result<bool> {
        Ok(self.conn().execute(
            "UPDATE tickets SET clone = ?2, updated = ?3 WHERE key = ?1",
            params![key, clone, updated],
        )? == 1)
    }

    /// Opens the span of a step.
    pub fn open_span(&self, span: &SpanRow) -> Result<()> {
        self.conn().execute(
            "INSERT INTO spans (ticket, state, waits_on, reason, started, ended) VALUES (?1, ?2, ?3, ?4, ?5, NULL)",
            params![span.ticket, span.state, span.waits_on, span.reason, span.started],
        )?;
        Ok(())
    }

    /// Closes each open span of a ticket. Gives the number of spans that it closed.
    pub fn close_spans(&self, ticket: &str, ended: &str) -> Result<usize> {
        Ok(self.conn().execute(
            "UPDATE spans SET ended = ?2 WHERE ticket = ?1 AND ended IS NULL",
            params![ticket, ended],
        )?)
    }

    /// The spans of one ticket, or of each ticket, oldest first.
    pub fn spans_of(&self, ticket: Option<&str>) -> Result<Vec<SpanRow>> {
        let mut stmt = self.conn().prepare(
            "SELECT coalesce(ticket, ''), coalesce(state, ''), waits_on, reason, coalesce(started, ''), ended
             FROM spans WHERE ?1 IS NULL OR ticket = ?1 ORDER BY rowid",
        )?;
        let rows = stmt.query_map(params![ticket], |row| {
            Ok(SpanRow {
                ticket: row.get(0)?,
                state: row.get(1)?,
                waits_on: row.get(2)?,
                reason: row.get(3)?,
                started: row.get(4)?,
                ended: row.get(5)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// The ids of the turns of one step of a ticket, in the order they were written.
    pub fn step_turn_ids(&self, ticket: &str, step: &str) -> Result<Vec<String>> {
        let mut stmt = self
            .conn()
            .prepare("SELECT id FROM turns WHERE ticket = ?1 AND step = ?2 ORDER BY rowid")?;
        let rows = stmt.query_map(params![ticket, step], |row| row.get(0))?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// The number of turn rows of a ticket that still have the status `running`.
    pub fn running_turns_of_ticket(&self, ticket: &str) -> Result<i64> {
        Ok(self.conn().query_row(
            "SELECT count(*) FROM turns WHERE ticket = ?1 AND status = 'running'",
            [ticket],
            |row| row.get(0),
        )?)
    }

    /// Writes `turns.end_fact`: `held`, or `failed:<fact>`.
    pub fn set_end_fact(&self, turn: &str, value: &str) -> Result<()> {
        self.conn().execute(
            "UPDATE turns SET end_fact = ?2 WHERE id = ?1",
            params![turn, value],
        )?;
        Ok(())
    }

    /// Writes `turns.principles`: the principles that the turn got, as a JSON list.
    pub fn set_turn_principles(&self, turn: &str, principles: &str) -> Result<()> {
        self.conn().execute(
            "UPDATE turns SET principles = ?2 WHERE id = ?1 AND principles IS NULL",
            params![turn, principles],
        )?;
        Ok(())
    }

    /// Adds a finding.
    pub fn add_finding(&self, row: &FindingRow) -> Result<()> {
        self.conn().execute(
            "INSERT INTO findings (ticket, round, fingerprint, priority, source, verdict, reason_code, fix_commit, came_back)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![
                row.ticket,
                row.round,
                row.fingerprint,
                row.priority,
                row.source,
                row.verdict,
                row.reason_code,
                row.fix_commit,
                i64::from(row.came_back)
            ],
        )?;
        Ok(())
    }

    /// The findings of a ticket, oldest first.
    pub fn findings_of(&self, ticket: &str) -> Result<Vec<FindingRow>> {
        let mut stmt = self.conn().prepare(
            "SELECT coalesce(ticket, ''), coalesce(round, 0), coalesce(fingerprint, ''), priority,
                    coalesce(source, ''), coalesce(verdict, ''), reason_code, fix_commit, coalesce(came_back, 0)
             FROM findings WHERE ticket = ?1 ORDER BY rowid",
        )?;
        let rows = stmt.query_map([ticket], |row| {
            Ok(FindingRow {
                ticket: row.get(0)?,
                round: row.get(1)?,
                fingerprint: row.get(2)?,
                priority: row.get(3)?,
                source: row.get(4)?,
                verdict: row.get(5)?,
                reason_code: row.get(6)?,
                fix_commit: row.get(7)?,
                came_back: row.get::<_, i64>(8)? != 0,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Adds a review row. Gives false, and adds nothing, when the round has that comment.
    pub fn add_review_row(&self, row: &ReviewRowNew) -> Result<bool> {
        Ok(self.conn().execute(
            "INSERT INTO review_rows (round, comment_id, reviewer, verdict, fix) VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT (round, comment_id) DO NOTHING",
            params![row.round, row.comment_id, row.reviewer, row.verdict, row.fix],
        )? == 1)
    }

    /// True when the ticket has a check of this kind and result at this commit.
    pub fn has_check(&self, ticket: &str, kind: &str, head: &str, result: &str) -> Result<bool> {
        Ok(self
            .conn()
            .query_row(
                "SELECT 1 FROM checks WHERE ticket = ?1 AND kind = ?2 AND head = ?3 AND result = ?4 LIMIT 1",
                params![ticket, kind, head, result],
                |_| Ok(()),
            )
            .optional()?
            .is_some())
    }

    /// The newest check of each kind and source of a ticket.
    pub fn latest_checks(&self, ticket: &str) -> Result<Vec<super::operations::CheckRow>> {
        let mut stmt = self.conn().prepare(
            "SELECT coalesce(c.ticket, ''), coalesce(c.head, ''), coalesce(c.kind, ''), coalesce(c.result, ''),
                    coalesce(c.command, ''), coalesce(c.seconds, 0), coalesce(c.cause, ''), coalesce(c.source, ''),
                    coalesce(c.at, '')
             FROM checks c WHERE c.ticket = ?1 AND c.rowid = (
                 SELECT max(d.rowid) FROM checks d WHERE d.ticket = c.ticket
                    AND coalesce(d.kind, '') = coalesce(c.kind, '') AND coalesce(d.source, '') = coalesce(c.source, ''))
             ORDER BY c.rowid",
        )?;
        let rows = stmt.query_map([ticket], |row| {
            Ok(super::operations::CheckRow {
                ticket: row.get(0)?,
                head: row.get(1)?,
                kind: row.get(2)?,
                result: row.get(3)?,
                command: row.get(4)?,
                seconds: row.get(5)?,
                cause: row.get(6)?,
                source: row.get(7)?,
                at: row.get(8)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// The events of a ticket, oldest first.
    pub fn events_of_ticket(&self, ticket: &str) -> Result<Vec<EventRow>> {
        let mut stmt = self.conn().prepare(
            "SELECT id, coalesce(at, ''), coalesce(actor, ''), ticket, coalesce(kind, ''), detail, external_id
             FROM events WHERE ticket = ?1 ORDER BY id",
        )?;
        let rows = stmt.query_map([ticket], |row| {
            Ok(EventRow {
                id: row.get(0)?,
                at: row.get(1)?,
                actor: row.get(2)?,
                ticket: row.get(3)?,
                kind: row.get(4)?,
                detail: row.get(5)?,
                external_id: row.get(6)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// The cards of one ticket, or of each ticket, oldest first.
    pub fn cards_of(&self, ticket: Option<&str>) -> Result<Vec<CardRow>> {
        let mut stmt = self.conn().prepare(
            "SELECT id, coalesce(ticket, ''), coalesce(state, ''), outcome, basis, coalesce(card, '{}'),
                    answer, created, answered
             FROM questions WHERE ?1 IS NULL OR ticket = ?1 ORDER BY created, length(id), id",
        )?;
        let rows = stmt.query_map(params![ticket], |row| {
            Ok(CardRow {
                id: row.get(0)?,
                ticket: row.get(1)?,
                state: row.get(2)?,
                outcome: row.get(3)?,
                basis: row.get(4)?,
                card: row.get(5)?,
                answer: row.get(6)?,
                created: row.get(7)?,
                answered: row.get(8)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// The number of cards of a ticket that have no final answer yet.
    pub fn open_cards(&self, ticket: &str) -> Result<i64> {
        Ok(self.conn().query_row(
            "SELECT count(*) FROM questions WHERE ticket = ?1 AND state NOT IN ('answered', 'needless')",
            [ticket],
            |row| row.get(0),
        )?)
    }

    /// The `human` rows of one ticket, or of each row with a ticket, oldest first.
    pub fn human_of(&self, ticket: Option<&str>) -> Result<Vec<HumanRow>> {
        let mut stmt = self.conn().prepare(
            "SELECT coalesce(at, ''), ticket, coalesce(kind, ''), coalesce(object, ''), value, reason, seconds
             FROM human WHERE ticket IS NOT NULL AND (?1 IS NULL OR ticket = ?1) ORDER BY id",
        )?;
        let rows = stmt.query_map(params![ticket], |row| {
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

    /// The cost of the turns of each step of each ticket that started at or after `since`.
    pub fn step_costs(&self, since: Option<&str>) -> Result<Vec<StepCost>> {
        let mut stmt = self.conn().prepare(
            "SELECT coalesce(ticket, ''), coalesce(step, ''), count(*), coalesce(sum(cost_usd), 0)
             FROM turns WHERE ?1 IS NULL OR started >= ?1
             GROUP BY ticket, step ORDER BY ticket, min(rowid)",
        )?;
        let rows = stmt.query_map(params![since], |row| {
            Ok(StepCost {
                ticket: row.get(0)?,
                step: row.get(1)?,
                turns: row.get(2)?,
                cost_usd: row.get(3)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spans_open_and_close() {
        let ledger = Ledger::open_in_memory().unwrap();
        let span = SpanRow {
            ticket: "DEMO-12".into(),
            state: "build".into(),
            waits_on: Some("agent:builder".into()),
            reason: Some("next".into()),
            started: "2026-11-16T14:30:00Z".into(),
            ended: None,
        };
        ledger.open_span(&span).unwrap();
        assert_eq!(ledger.close_spans("DEMO-12", "t2").unwrap(), 1);
        assert_eq!(ledger.close_spans("DEMO-12", "t3").unwrap(), 0);
        let rows = ledger.spans_of(Some("DEMO-12")).unwrap();
        assert_eq!(rows[0].ended.as_deref(), Some("t2"));
        assert!(ledger.spans_of(Some("DEMO-13")).unwrap().is_empty());
    }

    #[test]
    fn a_review_row_is_written_once_and_findings_read_back() {
        let ledger = Ledger::open_in_memory().unwrap();
        let row = ReviewRowNew {
            round: "DEMO-12:1".into(),
            comment_id: 7,
            reviewer: Some("someone".into()),
            verdict: "valid".into(),
            fix: 1,
        };
        assert!(ledger.add_review_row(&row).unwrap());
        assert!(!ledger.add_review_row(&row).unwrap());
        let finding = FindingRow {
            ticket: "DEMO-12".into(),
            round: 1,
            fingerprint: "f".into(),
            source: "review".into(),
            verdict: "kept".into(),
            ..FindingRow::default()
        };
        ledger.add_finding(&finding).unwrap();
        assert_eq!(ledger.findings_of("DEMO-12").unwrap(), vec![finding]);
    }

    #[test]
    fn a_ticket_row_is_written_whole_and_then_in_parts() {
        let ledger = Ledger::open_in_memory().unwrap();
        let row = TicketRow {
            key: "DEMO-12".into(),
            state: Some("refine".into()),
            flags: Some("[]".into()),
            clone: Some("/c".into()),
            ..TicketRow::default()
        };
        ledger.put_ticket(&row).unwrap();
        assert!(ledger
            .set_ticket_state("DEMO-12", "plan", "[\"paused\"]", "{}", "t")
            .unwrap());
        assert!(ledger.set_ticket_clone("DEMO-12", None, "t").unwrap());
        let back = ledger.ticket("DEMO-12").unwrap().unwrap();
        assert_eq!(back.state.as_deref(), Some("plan"));
        assert_eq!(back.clone, None);
        assert!(!ledger
            .set_ticket_state("DEMO-13", "plan", "[]", "{}", "t")
            .unwrap());
    }
}
