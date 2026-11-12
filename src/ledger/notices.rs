//! The notices (section 15): what a gate qa with notify = true found that does not block a
//! ticket, such as the low issues of a QA report. A notice never stops a ticket and never sets
//! a flag. It waits until the person reads it.

use anyhow::{Context, Result};
use rusqlite::{params, OptionalExtension, Row};
use serde::{Deserialize, Serialize};

use super::Ledger;

/// One issue of a notice.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct NoticeIssue {
    /// The id of the report, such as `ISSUE-003`.
    pub id: String,
    /// The severity, such as `low`.
    pub severity: String,
    /// The title of the issue.
    pub title: String,
}

/// One row of `notices`.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct NoticeRow {
    pub id: i64,
    pub ticket: String,
    pub step: String,
    pub turn: String,
    /// The kind of the gate, such as `qa`.
    pub gate: String,
    /// The document that the gate read, such as `qa.md`.
    pub doc: String,
    pub issues: Vec<NoticeIssue>,
    /// What else the gate saw, such as a count table that disagrees with the issues.
    pub note: Option<String>,
    pub created: String,
    /// When the person read it, or `None` while it waits.
    pub read: Option<String>,
}

impl NoticeRow {
    /// The id that the person types, such as `N-3`.
    pub fn name(&self) -> String {
        notice_name(self.id)
    }
}

/// The id that the person types for the notice `id`, such as `N-3`.
pub fn notice_name(id: i64) -> String {
    format!("N-{id}")
}

/// Reads `N-3` or `3`.
pub fn parse_notice_name(text: &str) -> Option<i64> {
    let text = text.trim();
    let digits = text
        .strip_prefix("N-")
        .or_else(|| text.strip_prefix("n-"))
        .unwrap_or(text);
    digits.parse().ok().filter(|id| *id > 0)
}

/// A notice to record.
#[derive(Clone, Debug, PartialEq)]
pub struct NewNotice<'a> {
    pub ticket: &'a str,
    pub step: &'a str,
    pub turn: &'a str,
    pub gate: &'a str,
    pub doc: &'a str,
    pub issues: &'a [NoticeIssue],
    pub note: Option<&'a str>,
    pub created: &'a str,
}

const COLUMNS: &str = "id, ticket, step, turn, gate, doc, issues, note, created, read";

fn row(row: &Row<'_>) -> rusqlite::Result<(NoticeRow, String)> {
    Ok((
        NoticeRow {
            id: row.get(0)?,
            ticket: row.get::<_, Option<String>>(1)?.unwrap_or_default(),
            step: row.get::<_, Option<String>>(2)?.unwrap_or_default(),
            turn: row.get::<_, Option<String>>(3)?.unwrap_or_default(),
            gate: row.get::<_, Option<String>>(4)?.unwrap_or_default(),
            doc: row.get::<_, Option<String>>(5)?.unwrap_or_default(),
            issues: Vec::new(),
            note: row.get(7)?,
            created: row.get::<_, Option<String>>(8)?.unwrap_or_default(),
            read: row.get(9)?,
        },
        row.get::<_, Option<String>>(6)?.unwrap_or_default(),
    ))
}

fn with_issues((mut notice, issues): (NoticeRow, String)) -> Result<NoticeRow> {
    if !issues.is_empty() {
        notice.issues = serde_json::from_str(&issues)
            .with_context(|| format!("the issues of the notice {} are not valid", notice.name()))?;
    }
    Ok(notice)
}

impl Ledger {
    /// Records a notice. A turn gives at most one notice for each document, so a second call
    /// for the same turn and document records nothing and gives `None`. Else gives its id.
    pub fn add_notice(&self, notice: &NewNotice<'_>) -> Result<Option<i64>> {
        let added = self.conn().execute(
            "INSERT OR IGNORE INTO notices (ticket, step, turn, gate, doc, issues, note, created)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                notice.ticket,
                notice.step,
                notice.turn,
                notice.gate,
                notice.doc,
                serde_json::to_string(notice.issues)?,
                notice.note,
                notice.created
            ],
        )?;
        Ok((added > 0).then(|| self.conn().last_insert_rowid()))
    }

    /// Each notice that waits for the person, oldest first.
    pub fn unread_notices(&self) -> Result<Vec<NoticeRow>> {
        let mut stmt = self.conn().prepare(&format!(
            "SELECT {COLUMNS} FROM notices WHERE read IS NULL ORDER BY id"
        ))?;
        let rows = stmt.query_map([], row)?;
        rows.map(|r| with_issues(r?)).collect()
    }

    /// The notices of a ticket, read or not, oldest first.
    pub fn notices_of(&self, ticket: &str) -> Result<Vec<NoticeRow>> {
        let mut stmt = self.conn().prepare(&format!(
            "SELECT {COLUMNS} FROM notices WHERE ticket = ?1 ORDER BY id"
        ))?;
        let rows = stmt.query_map([ticket], row)?;
        rows.map(|r| with_issues(r?)).collect()
    }

    /// One notice by its id.
    pub fn notice(&self, id: i64) -> Result<Option<NoticeRow>> {
        let found = self
            .conn()
            .query_row(
                &format!("SELECT {COLUMNS} FROM notices WHERE id = ?1"),
                [id],
                row,
            )
            .optional()?;
        found.map(with_issues).transpose()
    }

    /// Marks a notice as read at `at`. Gives false when it was read before.
    pub fn mark_notice_read(&self, id: i64, at: &str) -> Result<bool> {
        let changed = self.conn().execute(
            "UPDATE notices SET read = ?2 WHERE id = ?1 AND read IS NULL",
            params![id, at],
        )?;
        Ok(changed > 0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn new<'a>(turn: &'a str, issues: &'a [NoticeIssue]) -> NewNotice<'a> {
        NewNotice {
            ticket: "DEMO-12",
            step: "qa",
            turn,
            gate: "qa",
            doc: "qa.md",
            issues,
            note: None,
            created: "2026-11-16T14:30:00Z",
        }
    }

    #[test]
    fn a_notice_is_recorded_once_and_waits_until_it_is_read() {
        let ledger = Ledger::open_in_memory().unwrap();
        let issues = vec![NoticeIssue {
            id: "ISSUE-002".into(),
            severity: "low".into(),
            title: "A typo".into(),
        }];
        let id = ledger.add_notice(&new("T1", &issues)).unwrap().unwrap();
        assert_eq!(ledger.add_notice(&new("T1", &issues)).unwrap(), None);
        let waiting = ledger.unread_notices().unwrap();
        assert_eq!(waiting.len(), 1);
        assert_eq!(waiting[0].issues, issues);
        assert_eq!(waiting[0].name(), format!("N-{id}"));
        assert!(ledger.mark_notice_read(id, "2026-11-16T15:00:00Z").unwrap());
        assert!(!ledger.mark_notice_read(id, "2026-11-16T15:01:00Z").unwrap());
        assert!(ledger.unread_notices().unwrap().is_empty());
        let read = ledger.notice(id).unwrap().unwrap();
        assert_eq!(read.read.as_deref(), Some("2026-11-16T15:00:00Z"));
        assert_eq!(ledger.notices_of("DEMO-12").unwrap().len(), 1);
        assert_eq!(ledger.notice(id + 1).unwrap(), None);
    }

    #[test]
    fn a_notice_name_reads_with_or_without_its_prefix() {
        assert_eq!(parse_notice_name("N-3"), Some(3));
        assert_eq!(parse_notice_name("3"), Some(3));
        assert_eq!(parse_notice_name(" n-12 "), Some(12));
        assert_eq!(parse_notice_name("N-0"), None);
        assert_eq!(parse_notice_name("Q-0001"), None);
        assert_eq!(notice_name(7), "N-7");
    }
}
