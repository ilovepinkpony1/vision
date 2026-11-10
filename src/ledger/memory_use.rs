//! The `memory_use` table: the files of the memory that a turn got, and how it applied them.
//!
//! `file` holds the id of the file, such as `P-007` or `D-0142`, and `version` the version that
//! [`crate::memory::Memory::principles_file`] gives. A reader also accepts a path such as
//! `principles/P-007.md` in `file`.

use anyhow::Result;
use rusqlite::params;
use serde::Serialize;

use super::Ledger;

/// One row of `memory_use`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct MemoryUse {
    pub turn: String,
    /// The id of the file, such as `P-007`.
    pub file: String,
    pub version: Option<String>,
    /// True when the result of the turn lists the file in `applied`.
    pub applied: bool,
    /// How the turn applied it, from `applied[].how`.
    pub how: Option<String>,
}

impl Ledger {
    /// Adds one row.
    pub fn add_memory_use(&self, row: &MemoryUse) -> Result<()> {
        self.conn().execute(
            "INSERT INTO memory_use (turn, file, version, applied, how) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![row.turn, row.file, row.version, i64::from(row.applied), row.how],
        )?;
        Ok(())
    }

    /// The rows of one file, newest first, at most `limit`.
    pub fn memory_uses(&self, id: &str, limit: u32) -> Result<Vec<MemoryUse>> {
        let mut stmt = self.conn().prepare(
            "SELECT turn, file, version, applied, how FROM memory_use
             WHERE file = ?1 OR file LIKE '%/' || ?1 || '.md'
             ORDER BY rowid DESC LIMIT ?2",
        )?;
        let rows = stmt.query_map(params![id, limit], |row| {
            Ok(MemoryUse {
                turn: row.get::<_, Option<String>>(0)?.unwrap_or_default(),
                file: row.get::<_, Option<String>>(1)?.unwrap_or_default(),
                version: row.get(2)?,
                applied: row.get::<_, Option<i64>>(3)?.unwrap_or(0) != 0,
                how: row.get(4)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uses_read_back_newest_first() {
        let ledger = Ledger::open_in_memory().unwrap();
        for (turn, file, applied) in [
            ("t1", "P-007", true),
            ("t2", "principles/P-007.md", false),
            ("t3", "P-008", true),
        ] {
            ledger
                .add_memory_use(&MemoryUse {
                    turn: turn.into(),
                    file: file.into(),
                    version: Some("abc".into()),
                    applied,
                    how: applied.then(|| "kept the CSV".to_string()),
                })
                .unwrap();
        }
        let rows = ledger.memory_uses("P-007", 10).unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].turn, "t2");
        assert!(!rows[0].applied);
        assert_eq!(rows[1].how.as_deref(), Some("kept the CSV"));
        assert_eq!(ledger.memory_uses("P-007", 1).unwrap().len(), 1);
    }
}
