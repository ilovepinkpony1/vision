//! Reading the `tickets` table for the status view.

use anyhow::Result;
use serde::{Deserialize, Serialize};

use super::Ledger;

/// One row of `tickets`. JSON values such as `flags` and `counts` stay as text.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct TicketRow {
    pub key: String,
    pub state: Option<String>,
    pub flags: Option<String>,
    pub clone: Option<String>,
    pub branch: Option<String>,
    pub pr: Option<i64>,
    pub head: Option<String>,
    pub taken: Option<String>,
    pub updated: Option<String>,
    pub counts: Option<String>,
}

impl Ledger {
    /// Each ticket, by key.
    pub fn tickets(&self) -> Result<Vec<TicketRow>> {
        let mut stmt = self.conn().prepare(
            "SELECT key, state, flags, clone, branch, pr, head, taken, updated, counts FROM tickets ORDER BY key",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok(TicketRow {
                key: row.get(0)?,
                state: row.get(1)?,
                flags: row.get(2)?,
                clone: row.get(3)?,
                branch: row.get(4)?,
                pr: row.get(5)?,
                head: row.get(6)?,
                taken: row.get(7)?,
                updated: row.get(8)?,
                counts: row.get(9)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tickets_read_back_by_key() {
        let ledger = Ledger::open_in_memory().unwrap();
        ledger
            .conn()
            .execute_batch(
                "INSERT INTO tickets (key, state, pr) VALUES ('DEMO-13', 'plan', NULL);
                 INSERT INTO tickets (key, state, pr, flags) VALUES ('DEMO-12', 'build', 7, '[\"needs-you\"]');",
            )
            .unwrap();
        let tickets = ledger.tickets().unwrap();
        assert_eq!(tickets.len(), 2);
        assert_eq!(tickets[0].key, "DEMO-12");
        assert_eq!(tickets[0].pr, Some(7));
        assert_eq!(tickets[1].state.as_deref(), Some("plan"));
    }
}
