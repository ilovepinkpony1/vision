//! The `health` table: one row for each test that `vision doctor` ran.

use anyhow::Result;
use rusqlite::params;
use serde::Serialize;

use super::Ledger;

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct HealthRow {
    pub at: String,
    pub test: String,
    /// False only for a failure. A warning is `ok` with a detail that starts with `warning:`.
    pub ok: bool,
    pub detail: String,
}

impl Ledger {
    pub fn add_health(&self, row: &HealthRow) -> Result<()> {
        self.conn().execute(
            "INSERT INTO health (at, test, ok, detail) VALUES (?1, ?2, ?3, ?4)",
            params![row.at, row.test, row.ok, row.detail],
        )?;
        Ok(())
    }

    /// The newest row of each test, by test name.
    pub fn latest_health(&self) -> Result<Vec<HealthRow>> {
        let mut stmt = self.conn().prepare(
            "SELECT at, test, ok, detail FROM health
             WHERE rowid IN (SELECT max(rowid) FROM health GROUP BY test) ORDER BY test",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok(HealthRow {
                at: row.get(0)?,
                test: row.get(1)?,
                ok: row.get(2)?,
                detail: row.get(3)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn latest_health_keeps_the_newest_row_of_each_test() {
        let ledger = Ledger::open_in_memory().unwrap();
        let row = |test: &str, ok: bool, detail: &str| HealthRow {
            at: "2026-11-16T14:30:00Z".into(),
            test: test.into(),
            ok,
            detail: detail.into(),
        };
        ledger.add_health(&row("git", false, "missing")).unwrap();
        ledger
            .add_health(&row("git", true, "git version 2"))
            .unwrap();
        ledger.add_health(&row("config", true, "valid")).unwrap();
        let latest = ledger.latest_health().unwrap();
        assert_eq!(
            latest,
            vec![
                row("config", true, "valid"),
                row("git", true, "git version 2")
            ]
        );
    }
}
