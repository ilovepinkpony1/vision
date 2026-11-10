//! The queries of the Curator (section 9.4): its runs, proposals, and baselines as `events`,
//! the signals of the ledger, and the review rows of people.
//!
//! The Curator keeps its state in `events`, keyed by `external_id`, so that it needs no table
//! of its own: `curator-started:<turn>`, `curator-applied:<turn>`, `curator-failed:<turn>`,
//! `curator-proposal:<branch>`, `curator-baseline:<key>`, and `curator-closed:<branch>`.

use anyhow::Result;
use rusqlite::{params, OptionalExtension, Row};
use serde::Serialize;
use serde_json::Value;

use super::events::EventRow;
use super::questions::HumanRow;
use super::Ledger;

/// One row of `review_rows`, with its row id.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct ReviewRow {
    pub rowid: i64,
    pub round: Option<String>,
    pub comment_id: Option<i64>,
    pub reviewer: Option<String>,
    pub verdict: Option<String>,
    pub fix: Option<i64>,
    pub verdict_final: Option<String>,
    pub fix_final: Option<i64>,
}

fn event_row(row: &Row<'_>) -> rusqlite::Result<EventRow> {
    Ok(EventRow {
        id: row.get(0)?,
        at: row.get(1)?,
        actor: row.get(2)?,
        ticket: row.get(3)?,
        kind: row.get(4)?,
        detail: row.get(5)?,
        external_id: row.get(6)?,
    })
}

const EVENT_COLUMNS: &str =
    "id, coalesce(at, ''), coalesce(actor, ''), ticket, coalesce(kind, ''), detail, external_id";

impl Ledger {
    /// The event with this external id.
    pub fn event_by_external_id(&self, external_id: &str) -> Result<Option<EventRow>> {
        let sql = format!("SELECT {EVENT_COLUMNS} FROM events WHERE external_id = ?1");
        Ok(self
            .conn()
            .query_row(&sql, [external_id], event_row)
            .optional()?)
    }

    /// The newest events of one kind first.
    pub fn events_of_kind(&self, kind: &str, limit: u32) -> Result<Vec<EventRow>> {
        let sql =
            format!("SELECT {EVENT_COLUMNS} FROM events WHERE kind = ?1 ORDER BY id DESC LIMIT ?2");
        let mut stmt = self.conn().prepare(&sql)?;
        let rows = stmt.query_map(params![kind, limit], event_row)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// The newest event of one kind.
    pub fn latest_event(&self, kind: &str) -> Result<Option<EventRow>> {
        Ok(self.events_of_kind(kind, 1)?.into_iter().next())
    }

    /// The oldest event of the ledger, such as the one of `vision init`.
    pub fn first_event(&self) -> Result<Option<EventRow>> {
        let sql = format!("SELECT {EVENT_COLUMNS} FROM events ORDER BY id LIMIT 1");
        Ok(self.conn().query_row(&sql, [], event_row).optional()?)
    }

    /// Replaces the detail of the event with this external id. Gives false when there is none.
    pub fn set_event_detail(&self, external_id: &str, detail: &Value) -> Result<bool> {
        let changed = self.conn().execute(
            "UPDATE events SET detail = ?2 WHERE external_id = ?1",
            params![external_id, detail.to_string()],
        )?;
        Ok(changed > 0)
    }

    /// The number of turn rows of an agent that still have the status `running`.
    pub fn running_turns_of_agent(&self, agent: &str) -> Result<i64> {
        Ok(self.conn().query_row(
            "SELECT count(*) FROM turns WHERE agent = ?1 AND status = 'running'",
            [agent],
            |row| row.get(0),
        )?)
    }

    /// The highest row id of `review_rows`, or 0.
    pub fn max_review_rowid(&self) -> Result<i64> {
        Ok(self.conn().query_row(
            "SELECT coalesce(max(rowid), 0) FROM review_rows",
            [],
            |row| row.get(0),
        )?)
    }

    /// The rows of `review_rows` after a row id, oldest first.
    pub fn review_rows_after(&self, rowid: i64, limit: u32) -> Result<Vec<ReviewRow>> {
        let mut stmt = self.conn().prepare(
            "SELECT rowid, round, comment_id, reviewer, verdict, fix, verdict_final, fix_final
             FROM review_rows WHERE rowid > ?1 ORDER BY rowid LIMIT ?2",
        )?;
        let rows = stmt.query_map(params![rowid, limit], |row| {
            Ok(ReviewRow {
                rowid: row.get(0)?,
                round: row.get(1)?,
                comment_id: row.get(2)?,
                reviewer: row.get(3)?,
                verdict: row.get(4)?,
                fix: row.get(5)?,
                verdict_final: row.get(6)?,
                fix_final: row.get(7)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// The number of rows of `review_rows` after a row id.
    pub fn count_review_rows_after(&self, rowid: i64) -> Result<i64> {
        Ok(self.conn().query_row(
            "SELECT count(*) FROM review_rows WHERE rowid > ?1",
            [rowid],
            |row| row.get(0),
        )?)
    }

    /// The highest id of `human`, or 0.
    pub fn max_human_id(&self) -> Result<i64> {
        Ok(self
            .conn()
            .query_row("SELECT coalesce(max(id), 0) FROM human", [], |row| {
                row.get(0)
            })?)
    }

    /// The `human` rows of one kind after an id, oldest first, with their ids.
    pub fn human_rows_after(&self, id: i64, kind: &str) -> Result<Vec<(i64, HumanRow)>> {
        let mut stmt = self.conn().prepare(
            "SELECT id, coalesce(at, ''), ticket, coalesce(kind, ''), coalesce(object, ''), value, reason, seconds
             FROM human WHERE id > ?1 AND kind = ?2 ORDER BY id",
        )?;
        let rows = stmt.query_map(params![id, kind], |row| {
            Ok((
                row.get(0)?,
                HumanRow {
                    at: row.get(1)?,
                    ticket: row.get(2)?,
                    kind: row.get(3)?,
                    object: row.get(4)?,
                    value: row.get(5)?,
                    reason: row.get(6)?,
                    seconds: row.get(7)?,
                },
            ))
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// The ids of the cards with a proposal of the Judge that the person changed, answered
    /// after `since` (or ever), oldest first.
    pub fn changed_proposals_since(&self, since: Option<&str>) -> Result<Vec<String>> {
        self.question_ids(
            "SELECT id FROM questions
             WHERE outcome = 'propose' AND took_proposal = 0 AND (?1 IS NULL OR answered > ?1)
             ORDER BY answered, length(id), id",
            since,
        )
    }

    /// The ids of the cards that the person marked as needless after `since` (or ever),
    /// oldest first.
    pub fn needless_since(&self, since: Option<&str>) -> Result<Vec<String>> {
        self.question_ids(
            "SELECT id FROM questions
             WHERE state = 'needless' AND (?1 IS NULL OR answered > ?1)
             ORDER BY answered, length(id), id",
            since,
        )
    }

    fn question_ids(&self, sql: &str, since: Option<&str>) -> Result<Vec<String>> {
        let mut stmt = self.conn().prepare(sql)?;
        let rows = stmt.query_map(params![since], |row| row.get(0))?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ledger::Event;
    use serde_json::json;

    #[test]
    fn events_are_found_by_kind_and_external_id_and_can_be_updated() {
        let ledger = Ledger::open_in_memory().unwrap();
        assert_eq!(ledger.first_event().unwrap(), None);
        for n in 1..=3 {
            ledger
                .add_event(
                    &Event::now("tick", "curator-started")
                        .external_id(&format!("curator-started:t{n}"))
                        .detail(json!({"n": n})),
                )
                .unwrap();
        }
        assert_eq!(
            ledger
                .latest_event("curator-started")
                .unwrap()
                .unwrap()
                .external_id
                .as_deref(),
            Some("curator-started:t3")
        );
        assert_eq!(
            ledger.events_of_kind("curator-started", 2).unwrap().len(),
            2
        );
        assert!(ledger
            .set_event_detail("curator-started:t1", &json!({"n": 9}))
            .unwrap());
        assert!(!ledger.set_event_detail("nothing", &json!({})).unwrap());
        let row = ledger
            .event_by_external_id("curator-started:t1")
            .unwrap()
            .unwrap();
        assert_eq!(row.detail.as_deref(), Some(r#"{"n":9}"#));
        assert_eq!(ledger.first_event().unwrap().unwrap().id, row.id);
    }

    #[test]
    fn review_rows_and_human_rows_come_after_a_mark() {
        let ledger = Ledger::open_in_memory().unwrap();
        assert_eq!(ledger.max_review_rowid().unwrap(), 0);
        for id in [7, 8] {
            ledger
                .conn()
                .execute(
                    "INSERT INTO review_rows (round, comment_id, reviewer, verdict, fix) VALUES ('DEMO-12:1', ?1, 'someone', 'valid', 1)",
                    [id],
                )
                .unwrap();
        }
        let first = ledger.review_rows_after(0, 10).unwrap();
        assert_eq!(first.len(), 2);
        assert_eq!(first[0].comment_id, Some(7));
        assert_eq!(
            ledger.review_rows_after(first[0].rowid, 10).unwrap().len(),
            1
        );
        assert_eq!(ledger.count_review_rows_after(first[1].rowid).unwrap(), 0);
        assert_eq!(ledger.max_review_rowid().unwrap(), first[1].rowid);

        for kind in ["correct", "answer", "correct"] {
            ledger
                .add_human(&HumanRow {
                    at: "2026-11-10T10:00:00Z".into(),
                    kind: kind.into(),
                    object: "Q-0001".into(),
                    ..HumanRow::default()
                })
                .unwrap();
        }
        let corrections = ledger.human_rows_after(0, "correct").unwrap();
        assert_eq!(corrections.len(), 2);
        assert_eq!(
            ledger
                .human_rows_after(corrections[0].0, "correct")
                .unwrap()
                .len(),
            1
        );
        assert_eq!(ledger.max_human_id().unwrap(), 3);
    }
}
