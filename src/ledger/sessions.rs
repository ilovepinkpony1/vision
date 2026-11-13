//! The `sessions` table (section 16): one row for each session file of Claude Code that the
//! archive knows. It keeps what the archive holds of the file, the summary of a session, and
//! what the index of MemPalace holds of the summary, so that a later run does only the work
//! that waits.

use anyhow::Result;
use rusqlite::{params, OptionalExtension, Row};
use serde::Serialize;

use super::Ledger;

/// One row of `sessions`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct SessionRow {
    /// The file relative to the folder of the session files.
    pub path: String,
    /// The project folder, such as `-Users-me-work`.
    pub project: String,
    /// The id of the session that the file belongs to.
    pub session: String,
    /// True for the file of the session itself, false for the file of a subagent.
    pub main: bool,
    /// The SHA-256 of the content that the archive holds.
    pub hash: Option<String>,
    pub size: Option<i64>,
    /// The change time of the file at the copy, in nanoseconds since 1970.
    pub mtime: Option<i64>,
    pub key: Option<String>,
    /// When the archive confirmed the copy.
    pub archived: Option<String>,
    /// The size of the gzip file in the archive.
    pub archive_bytes: Option<i64>,
    /// The summary entry relative to the home.
    pub entry: Option<String>,
    pub summary_hash: Option<String>,
    /// The content hash that the summary was built from.
    pub summary_of: Option<String>,
    /// The hash of the summary text that MemPalace holds.
    pub filed_hash: Option<String>,
    pub filed: Option<String>,
}

impl SessionRow {
    /// True when the summary of a main file waits: the archive holds a content that the
    /// summary does not show yet.
    pub fn summary_waits(&self) -> bool {
        self.main && self.archived.is_some() && self.summary_of != self.hash
    }

    /// True when the summary waits for the index.
    pub fn waits_for_index(&self) -> bool {
        self.summary_hash.is_some() && self.filed_hash != self.summary_hash
    }
}

const COLUMNS: &str = "path, project, session, main, hash, size, mtime, key, archived, \
     archive_bytes, entry, summary_hash, summary_of, filed_hash, filed";

fn session_row(row: &Row<'_>) -> rusqlite::Result<SessionRow> {
    Ok(SessionRow {
        path: row.get(0)?,
        project: row.get(1)?,
        session: row.get(2)?,
        main: row.get::<_, i64>(3)? != 0,
        hash: row.get(4)?,
        size: row.get(5)?,
        mtime: row.get(6)?,
        key: row.get(7)?,
        archived: row.get(8)?,
        archive_bytes: row.get(9)?,
        entry: row.get(10)?,
        summary_hash: row.get(11)?,
        summary_of: row.get(12)?,
        filed_hash: row.get(13)?,
        filed: row.get(14)?,
    })
}

/// A copy of a session file that the archive confirmed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionArchived<'a> {
    pub path: &'a str,
    pub project: &'a str,
    pub session: &'a str,
    pub main: bool,
    pub hash: &'a str,
    pub size: i64,
    pub mtime: i64,
    pub key: &'a str,
    pub archived: &'a str,
    pub archive_bytes: i64,
}

impl Ledger {
    /// The row of one session file.
    pub fn session_row(&self, path: &str) -> Result<Option<SessionRow>> {
        let sql = format!("SELECT {COLUMNS} FROM sessions WHERE path = ?1");
        Ok(self
            .conn()
            .query_row(&sql, [path], session_row)
            .optional()?)
    }

    /// Each row, by path.
    pub fn session_rows(&self) -> Result<Vec<SessionRow>> {
        self.session_query(&format!("SELECT {COLUMNS} FROM sessions ORDER BY path"), [])
    }

    /// The rows of the files of one session, the main file first.
    pub fn session_files(&self, session: &str) -> Result<Vec<SessionRow>> {
        self.session_query(
            &format!("SELECT {COLUMNS} FROM sessions WHERE session = ?1 ORDER BY main DESC, path"),
            [session],
        )
    }

    /// The rows whose summary waits for the index, by path.
    pub fn session_rows_to_index(&self) -> Result<Vec<SessionRow>> {
        self.session_query(
            &format!(
                "SELECT {COLUMNS} FROM sessions WHERE summary_hash IS NOT NULL
                 AND (filed_hash IS NULL OR filed_hash <> summary_hash) ORDER BY path"
            ),
            [],
        )
    }

    fn session_query<P: rusqlite::Params>(&self, sql: &str, args: P) -> Result<Vec<SessionRow>> {
        let mut stmt = self.conn().prepare(sql)?;
        let rows = stmt.query_map(args, session_row)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Records a confirmed copy. The columns of the summary and of the index stay.
    pub fn record_session_archived(&self, copy: &SessionArchived<'_>) -> Result<()> {
        self.conn().execute(
            "INSERT INTO sessions (path, project, session, main, hash, size, mtime, key, archived,
                                   archive_bytes)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
             ON CONFLICT (path) DO UPDATE SET project = ?2, session = ?3, main = ?4, hash = ?5,
                 size = ?6, mtime = ?7, key = ?8, archived = ?9, archive_bytes = ?10",
            params![
                copy.path,
                copy.project,
                copy.session,
                i64::from(copy.main),
                copy.hash,
                copy.size,
                copy.mtime,
                copy.key,
                copy.archived,
                copy.archive_bytes
            ],
        )?;
        Ok(())
    }

    /// Keeps a new change time of a file whose content the archive already holds.
    pub fn set_session_mtime(&self, path: &str, mtime: i64) -> Result<()> {
        self.conn().execute(
            "UPDATE sessions SET mtime = ?2 WHERE path = ?1",
            params![path, mtime],
        )?;
        Ok(())
    }

    /// Records the summary of a session: its entry, the hash of its text, and the content hash
    /// that it shows.
    pub fn set_session_summary(
        &self,
        path: &str,
        entry: &str,
        summary_hash: &str,
        summary_of: &str,
    ) -> Result<()> {
        self.conn().execute(
            "UPDATE sessions SET entry = ?2, summary_hash = ?3, summary_of = ?4 WHERE path = ?1",
            params![path, entry, summary_hash, summary_of],
        )?;
        Ok(())
    }

    /// Records that MemPalace holds the summary text with this hash.
    pub fn set_session_filed(&self, path: &str, hash: &str, at: &str) -> Result<()> {
        self.conn().execute(
            "UPDATE sessions SET filed_hash = ?2, filed = ?3 WHERE path = ?1",
            params![path, hash, at],
        )?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_copy_keeps_the_summary_and_the_index_until_they_change() {
        let ledger = Ledger::open_in_memory().unwrap();
        let copy = SessionArchived {
            path: "-p/s1.jsonl",
            project: "-p",
            session: "s1",
            main: true,
            hash: "h1",
            size: 10,
            mtime: 5,
            key: "vision/sessions/-p/s1.jsonl.gz",
            archived: "2026-11-16T14:30:00Z",
            archive_bytes: 4,
        };
        ledger.record_session_archived(&copy).unwrap();
        let row = ledger.session_row("-p/s1.jsonl").unwrap().unwrap();
        assert!(row.summary_waits());
        assert!(!row.waits_for_index());
        ledger
            .set_session_summary("-p/s1.jsonl", "session-journal/-p/s1.md", "t1", "h1")
            .unwrap();
        let row = ledger.session_row("-p/s1.jsonl").unwrap().unwrap();
        assert!(!row.summary_waits());
        assert!(row.waits_for_index());
        assert_eq!(ledger.session_rows_to_index().unwrap(), [row]);
        ledger
            .set_session_filed("-p/s1.jsonl", "t1", "2026-11-16T14:31:00Z")
            .unwrap();
        assert!(ledger.session_rows_to_index().unwrap().is_empty());

        // A file that grew: a new copy, and the summary waits again.
        ledger
            .record_session_archived(&SessionArchived {
                hash: "h2",
                size: 20,
                ..copy
            })
            .unwrap();
        let row = ledger.session_row("-p/s1.jsonl").unwrap().unwrap();
        assert!(row.summary_waits());
        assert_eq!(row.filed_hash.as_deref(), Some("t1"));
        ledger.set_session_mtime("-p/s1.jsonl", 9).unwrap();
        assert_eq!(
            ledger.session_row("-p/s1.jsonl").unwrap().unwrap().mtime,
            Some(9)
        );

        // A subagent file belongs to the session and has no summary.
        ledger
            .record_session_archived(&SessionArchived {
                path: "-p/s1/subagents/agent-a.jsonl",
                main: false,
                key: "vision/sessions/-p/s1/subagents/agent-a.jsonl.gz",
                ..copy
            })
            .unwrap();
        let files = ledger.session_files("s1").unwrap();
        assert_eq!(files.len(), 2);
        assert!(files[0].main);
        assert!(!files[1].summary_waits());
        assert_eq!(ledger.session_rows().unwrap().len(), 2);
    }
}
