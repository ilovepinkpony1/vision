//! The `journal` table (section 16): one row for each turn with a journal entry or an archived
//! turn folder. It keeps what the index of MemPalace holds and what the object store holds, so
//! that a later tick or command does only the work that waits.

use anyhow::Result;
use rusqlite::{params, OptionalExtension, Row};
use serde::Serialize;

use super::events::EventRow;
use super::Ledger;

/// One row of `journal`.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct JournalRow {
    pub turn: String,
    pub ticket: String,
    /// The entry file relative to the home, or `None` for a turn with no entry.
    pub path: Option<String>,
    /// The SHA-256 of the entry text.
    pub hash: Option<String>,
    /// The hash of the facts of the ledger that the entry shows.
    pub signature: Option<String>,
    /// The last commit of the turn that the first write saw.
    pub head: Option<String>,
    pub written: Option<String>,
    /// The hash of the text that MemPalace holds.
    pub filed_hash: Option<String>,
    pub filed: Option<String>,
    /// When the object store confirmed the archive of the turn folder.
    pub archived: Option<String>,
    pub archive_bytes: Option<i64>,
    /// The hash of the entry text that the object store holds.
    pub entry_sent: Option<String>,
    /// When the local turn folder was deleted.
    pub removed: Option<String>,
}

impl JournalRow {
    /// True when the entry waits for the index: MemPalace holds no text, or an older one.
    pub fn waits_for_index(&self) -> bool {
        self.hash.is_some() && self.filed_hash != self.hash
    }

    /// True when the entry waits for the object store.
    pub fn entry_waits_for_archive(&self) -> bool {
        self.hash.is_some() && self.entry_sent != self.hash
    }
}

const COLUMNS: &str = "turn, coalesce(ticket, ''), path, hash, signature, head, written, \
     filed_hash, filed, archived, archive_bytes, entry_sent, removed";

fn journal_row(row: &Row<'_>) -> rusqlite::Result<JournalRow> {
    Ok(JournalRow {
        turn: row.get(0)?,
        ticket: row.get(1)?,
        path: row.get(2)?,
        hash: row.get(3)?,
        signature: row.get(4)?,
        head: row.get(5)?,
        written: row.get(6)?,
        filed_hash: row.get(7)?,
        filed: row.get(8)?,
        archived: row.get(9)?,
        archive_bytes: row.get(10)?,
        entry_sent: row.get(11)?,
        removed: row.get(12)?,
    })
}

/// The entry that a write records.
#[derive(Clone, Debug, PartialEq)]
pub struct EntryWrite<'a> {
    pub turn: &'a str,
    pub ticket: &'a str,
    pub path: &'a str,
    pub hash: &'a str,
    pub signature: &'a str,
    pub head: Option<&'a str>,
    pub written: &'a str,
}

impl Ledger {
    /// The row of one turn.
    pub fn journal_row(&self, turn: &str) -> Result<Option<JournalRow>> {
        let sql = format!("SELECT {COLUMNS} FROM journal WHERE turn = ?1");
        Ok(self
            .conn()
            .query_row(&sql, [turn], journal_row)
            .optional()?)
    }

    /// Each row, oldest turn first.
    pub fn journal_rows(&self) -> Result<Vec<JournalRow>> {
        let sql = format!("SELECT {COLUMNS} FROM journal ORDER BY turn");
        let mut stmt = self.conn().prepare(&sql)?;
        let rows = stmt.query_map([], journal_row)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// The rows whose entry waits for the index, oldest turn first.
    pub fn journal_rows_to_index(&self) -> Result<Vec<JournalRow>> {
        let sql = format!(
            "SELECT {COLUMNS} FROM journal
             WHERE hash IS NOT NULL AND (filed_hash IS NULL OR filed_hash <> hash) ORDER BY turn"
        );
        let mut stmt = self.conn().prepare(&sql)?;
        let rows = stmt.query_map([], journal_row)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Records a written entry. The columns of the index and of the archive stay.
    pub fn record_entry(&self, entry: &EntryWrite<'_>) -> Result<()> {
        self.conn().execute(
            "INSERT INTO journal (turn, ticket, path, hash, signature, head, written)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
             ON CONFLICT (turn) DO UPDATE SET ticket = ?2, path = ?3, hash = ?4, signature = ?5,
                 head = coalesce(?6, head), written = ?7",
            params![
                entry.turn,
                entry.ticket,
                entry.path,
                entry.hash,
                entry.signature,
                entry.head,
                entry.written
            ],
        )?;
        Ok(())
    }

    /// Keeps a new signature of an entry whose text did not change.
    pub fn set_entry_signature(&self, turn: &str, signature: &str) -> Result<()> {
        self.conn().execute(
            "UPDATE journal SET signature = ?2 WHERE turn = ?1",
            params![turn, signature],
        )?;
        Ok(())
    }

    /// Records that MemPalace holds the text with this hash.
    pub fn set_entry_filed(&self, turn: &str, hash: &str, at: &str) -> Result<()> {
        self.conn().execute(
            "UPDATE journal SET filed_hash = ?2, filed = ?3 WHERE turn = ?1",
            params![turn, hash, at],
        )?;
        Ok(())
    }

    /// Forgets what MemPalace holds, so that each entry waits for the index again.
    pub fn forget_filed(&self) -> Result<usize> {
        Ok(self
            .conn()
            .execute("UPDATE journal SET filed_hash = NULL, filed = NULL", [])?)
    }

    /// Records that the object store holds the turn folder with this size. A turn with no row
    /// gets one.
    pub fn set_turn_archived(&self, turn: &str, ticket: &str, bytes: i64, at: &str) -> Result<()> {
        self.conn().execute(
            "INSERT INTO journal (turn, ticket, archived, archive_bytes) VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT (turn) DO UPDATE SET archived = ?3, archive_bytes = ?4",
            params![turn, ticket, at, bytes],
        )?;
        Ok(())
    }

    /// Records that the object store holds the entry text with this hash.
    pub fn set_entry_sent(&self, turn: &str, hash: &str) -> Result<()> {
        self.conn().execute(
            "UPDATE journal SET entry_sent = ?2 WHERE turn = ?1",
            params![turn, hash],
        )?;
        Ok(())
    }

    /// Records that the local turn folder was deleted, or that it is back (`None`).
    pub fn set_turn_removed(&self, turn: &str, at: Option<&str>) -> Result<()> {
        self.conn().execute(
            "UPDATE journal SET removed = ?2 WHERE turn = ?1",
            params![turn, at],
        )?;
        Ok(())
    }

    /// The turn rows that ended at or after `since`, oldest first, for the tick of the journal.
    pub fn turn_ids_ended_since(&self, since: &str) -> Result<Vec<String>> {
        let mut stmt = self.conn().prepare(
            "SELECT id FROM turns WHERE status <> 'running' AND ended IS NOT NULL AND ended >= ?1
             ORDER BY started, id",
        )?;
        let rows = stmt.query_map([since], |row| row.get(0))?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// The events of a ticket of these kinds from `from` (inclusive) to `to` (exclusive, or no
    /// end), oldest first: what happened to the ticket after one turn and before the next.
    pub fn ticket_events_between(
        &self,
        ticket: &str,
        kinds: &[&str],
        from: &str,
        to: Option<&str>,
    ) -> Result<Vec<EventRow>> {
        let mut stmt = self.conn().prepare(
            "SELECT id, at, actor, ticket, kind, detail, external_id FROM events
             WHERE ticket = ?1 AND at >= ?2 AND (?3 IS NULL OR at < ?3) ORDER BY id",
        )?;
        let rows = stmt.query_map(params![ticket, from, to], |row| {
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
        let rows: Vec<EventRow> = rows.collect::<rusqlite::Result<_>>()?;
        Ok(rows
            .into_iter()
            .filter(|e| kinds.contains(&e.kind.as_str()))
            .collect())
    }

    /// The start of the first turn of a ticket that started after `after`, other than `not`
    /// and other than a turn of an agent of `skip`, such as the Judge.
    pub fn next_turn_start(
        &self,
        ticket: &str,
        after: &str,
        not: &str,
        skip: &[&str],
    ) -> Result<Option<String>> {
        let mut stmt = self.conn().prepare(
            "SELECT started, coalesce(agent, '') FROM turns WHERE ticket = ?1 AND started > ?2
             AND id <> ?3 ORDER BY started, id",
        )?;
        let rows = stmt.query_map(params![ticket, after, not], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?;
        for row in rows {
            let (started, agent) = row?;
            if !skip.contains(&agent.as_str()) {
                return Ok(Some(started));
            }
        }
        Ok(None)
    }

    /// The time of the newest event of a kind, such as the last run of the archive.
    pub fn last_event_at(&self, kind: &str) -> Result<Option<String>> {
        Ok(self
            .conn()
            .query_row(
                "SELECT at FROM events WHERE kind = ?1 ORDER BY id DESC LIMIT 1",
                [kind],
                |row| row.get(0),
            )
            .optional()?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_entry_keeps_the_columns_of_the_index_and_the_archive() {
        let ledger = Ledger::open_in_memory().unwrap();
        let write = EntryWrite {
            turn: "T1",
            ticket: "DEMO-12",
            path: "journal/DEMO-12/T1.md",
            hash: "h1",
            signature: "s1",
            head: Some("abc"),
            written: "2026-11-16T14:30:00Z",
        };
        ledger.record_entry(&write).unwrap();
        let row = ledger.journal_row("T1").unwrap().unwrap();
        assert!(row.waits_for_index());
        assert!(row.entry_waits_for_archive());
        assert_eq!(ledger.journal_rows_to_index().unwrap(), [row]);
        ledger
            .set_entry_filed("T1", "h1", "2026-11-16T14:31:00Z")
            .unwrap();
        assert!(ledger.journal_rows_to_index().unwrap().is_empty());
        ledger.set_entry_sent("T1", "h1").unwrap();
        ledger
            .set_turn_archived("T1", "DEMO-12", 120, "2026-11-16T15:00:00Z")
            .unwrap();
        let row = ledger.journal_row("T1").unwrap().unwrap();
        assert!(!row.waits_for_index());
        assert!(!row.entry_waits_for_archive());
        assert_eq!(row.archive_bytes, Some(120));

        // A rewrite with a new text waits for the index again, and the head of the first write
        // stays.
        ledger
            .record_entry(&EntryWrite {
                hash: "h2",
                head: None,
                ..write
            })
            .unwrap();
        let row = ledger.journal_row("T1").unwrap().unwrap();
        assert!(row.waits_for_index());
        assert!(row.entry_waits_for_archive());
        assert_eq!(row.head.as_deref(), Some("abc"));
        assert_eq!(row.archived.as_deref(), Some("2026-11-16T15:00:00Z"));
        assert_eq!(ledger.forget_filed().unwrap(), 1);
        assert_eq!(ledger.journal_row("T1").unwrap().unwrap().filed_hash, None);
    }

    #[test]
    fn an_archive_of_a_turn_with_no_entry_gets_a_row() {
        let ledger = Ledger::open_in_memory().unwrap();
        ledger
            .set_turn_archived("T9", "curator", 10, "2026-11-16T15:00:00Z")
            .unwrap();
        let row = ledger.journal_row("T9").unwrap().unwrap();
        assert_eq!(row.path, None);
        assert!(!row.waits_for_index());
        ledger
            .set_turn_removed("T9", Some("2026-12-01T00:00:00Z"))
            .unwrap();
        assert!(ledger.journal_row("T9").unwrap().unwrap().removed.is_some());
    }
}
