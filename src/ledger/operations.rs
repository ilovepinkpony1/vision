//! The queries of the executor (section 12): the `operations` rows, one `outcomes` row for each
//! reconcile, the `checks` of the jobs, and the values that an operation sets on a ticket row.
//!
//! The states of an operation: `pending` (recorded, nothing runs yet), `executing` (the
//! executor called `act`), and the ends `confirmed`, `failed`, and `uncertain`. An `uncertain`
//! operation is not final: each tick reconciles it, and it never runs again by itself.
//!
//! The `args` column holds the call of the operation as JSON: `{"args": {...}, "head": "...",
//! "branch": "...", "clone": "..."}`. The `result` column holds what the executor knows of its
//! end as JSON. Both are written with each secret masked.

use anyhow::Result;
use rusqlite::{params, params_from_iter, OptionalExtension, Row};
use serde::Serialize;

use super::{Ledger, TicketRow};

pub const STATE_PENDING: &str = "pending";
pub const STATE_EXECUTING: &str = "executing";
pub const STATE_CONFIRMED: &str = "confirmed";
pub const STATE_FAILED: &str = "failed";
pub const STATE_UNCERTAIN: &str = "uncertain";

/// One row of `operations`. JSON values stay as text.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct OperationRow {
    pub id: String,
    /// The idempotency key.
    pub idem: Option<String>,
    /// Who asked for it: `rule` or `turn`.
    pub source: Option<String>,
    pub turn: Option<String>,
    pub ticket: String,
    pub verb: String,
    /// The call as JSON.
    pub args: String,
    /// The mode of the verb when the executor last looked at the operation.
    pub mode: String,
    pub state: String,
    pub external_id: Option<String>,
    pub started: Option<String>,
    pub ended: Option<String>,
    /// What the executor knows of the end, as JSON.
    pub result: Option<String>,
}

/// A new row in the state `pending`.
#[derive(Clone, Debug, PartialEq)]
pub struct NewOperation<'a> {
    pub id: &'a str,
    pub idem: &'a str,
    pub source: &'a str,
    pub turn: Option<&'a str>,
    pub ticket: &'a str,
    pub verb: &'a str,
    pub args: &'a str,
    pub mode: &'a str,
    pub result: &'a str,
}

/// One row of `outcomes`.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct OutcomeRow {
    pub op: String,
    /// `reconcile`.
    pub kind: String,
    pub check_at: Option<String>,
    pub checked_at: Option<String>,
    pub state: Option<String>,
    pub detail: Option<String>,
}

/// One row of `checks`.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct CheckRow {
    pub ticket: String,
    /// The commit that the check ran at.
    pub head: String,
    /// Such as `spec`, `ci`, or `job` for the result of a whole job.
    pub kind: String,
    /// Such as `pass`, `fail`, `green`, or `red`.
    pub result: String,
    pub command: String,
    pub seconds: i64,
    /// `code`, `infra`, `flaky`, `env`, `not ours`, `unknown`, or empty.
    pub cause: String,
    /// Where the check came from, such as `job:proof`.
    pub source: String,
    pub at: String,
}

const OPERATION_COLUMNS: &str = "id, idem, source, turn, coalesce(ticket, ''), coalesce(verb, ''), \
     coalesce(args, '{}'), coalesce(mode, ''), coalesce(state, ''), external_id, started, ended, result";

fn operation_row(row: &Row<'_>) -> rusqlite::Result<OperationRow> {
    Ok(OperationRow {
        id: row.get(0)?,
        idem: row.get(1)?,
        source: row.get(2)?,
        turn: row.get(3)?,
        ticket: row.get(4)?,
        verb: row.get(5)?,
        args: row.get(6)?,
        mode: row.get(7)?,
        state: row.get(8)?,
        external_id: row.get(9)?,
        started: row.get(10)?,
        ended: row.get(11)?,
        result: row.get(12)?,
    })
}

impl Ledger {
    /// Adds an operation in the state `pending`. Gives false, and adds nothing, when an
    /// operation with the same idempotency key exists.
    pub fn add_operation(&self, op: &NewOperation<'_>) -> Result<bool> {
        let added = self.conn().execute(
            "INSERT INTO operations (id, idem, source, turn, ticket, verb, args, mode, state, result)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10) ON CONFLICT (idem) DO NOTHING",
            params![
                op.id,
                op.idem,
                op.source,
                op.turn,
                op.ticket,
                op.verb,
                op.args,
                op.mode,
                STATE_PENDING,
                op.result
            ],
        )?;
        Ok(added == 1)
    }

    pub fn operation(&self, id: &str) -> Result<Option<OperationRow>> {
        let sql = format!("SELECT {OPERATION_COLUMNS} FROM operations WHERE id = ?1");
        Ok(self
            .conn()
            .query_row(&sql, [id], operation_row)
            .optional()?)
    }

    pub fn operation_by_idem(&self, idem: &str) -> Result<Option<OperationRow>> {
        let sql = format!("SELECT {OPERATION_COLUMNS} FROM operations WHERE idem = ?1");
        Ok(self
            .conn()
            .query_row(&sql, [idem], operation_row)
            .optional()?)
    }

    /// The operations in these states, oldest first.
    pub fn operations_in_states(&self, states: &[&str]) -> Result<Vec<OperationRow>> {
        if states.is_empty() {
            return Ok(Vec::new());
        }
        let marks = vec!["?"; states.len()].join(", ");
        let sql = format!(
            "SELECT {OPERATION_COLUMNS} FROM operations WHERE state IN ({marks}) ORDER BY rowid"
        );
        let mut stmt = self.conn().prepare(&sql)?;
        let rows = stmt.query_map(params_from_iter(states.iter()), operation_row)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// The operations of a ticket, of one verb or of each, oldest first.
    pub fn operations_of(&self, ticket: &str, verb: Option<&str>) -> Result<Vec<OperationRow>> {
        let sql = format!(
            "SELECT {OPERATION_COLUMNS} FROM operations WHERE ticket = ?1 AND (?2 IS NULL OR verb = ?2) ORDER BY rowid"
        );
        let mut stmt = self.conn().prepare(&sql)?;
        let rows = stmt.query_map(params![ticket, verb], operation_row)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Writes the mode and the result of an operation that is still `pending`. Gives false
    /// when it is no longer pending.
    pub fn update_pending_operation(&self, id: &str, mode: &str, result: &str) -> Result<bool> {
        let changed = self.conn().execute(
            "UPDATE operations SET mode = ?2, result = ?3 WHERE id = ?1 AND state = ?4",
            params![id, mode, result, STATE_PENDING],
        )?;
        Ok(changed == 1)
    }

    /// Moves a `pending` operation to `executing`. Gives false when it was not pending, so that
    /// only one caller runs it.
    pub fn claim_operation(&self, id: &str, mode: &str, started: &str) -> Result<bool> {
        let changed = self.conn().execute(
            "UPDATE operations SET state = ?2, mode = ?3, started = ?4 WHERE id = ?1 AND state = ?5",
            params![id, STATE_EXECUTING, mode, started, STATE_PENDING],
        )?;
        Ok(changed == 1)
    }

    /// Moves an `executing` operation back to `pending`, for a call that could not start.
    pub fn unclaim_operation(&self, id: &str) -> Result<()> {
        self.conn().execute(
            "UPDATE operations SET state = ?2, started = NULL WHERE id = ?1 AND state = ?3",
            params![id, STATE_PENDING, STATE_EXECUTING],
        )?;
        Ok(())
    }

    /// Writes the state, the external id, the end time, and the result of an operation.
    pub fn end_operation(
        &self,
        id: &str,
        state: &str,
        external_id: Option<&str>,
        ended: Option<&str>,
        result: &str,
    ) -> Result<()> {
        self.conn().execute(
            "UPDATE operations SET state = ?2, external_id = coalesce(?3, external_id), ended = ?4, result = ?5
             WHERE id = ?1",
            params![id, state, external_id, ended, result],
        )?;
        Ok(())
    }

    pub fn add_outcome(&self, row: &OutcomeRow) -> Result<()> {
        self.conn().execute(
            "INSERT INTO outcomes (op, kind, check_at, checked_at, state, detail) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![row.op, row.kind, row.check_at, row.checked_at, row.state, row.detail],
        )?;
        Ok(())
    }

    /// The `outcomes` rows of one operation, oldest first.
    pub fn outcomes_of(&self, op: &str) -> Result<Vec<OutcomeRow>> {
        let mut stmt = self.conn().prepare(
            "SELECT coalesce(op, ''), coalesce(kind, ''), check_at, checked_at, state, detail
             FROM outcomes WHERE op = ?1 ORDER BY rowid",
        )?;
        let rows = stmt.query_map([op], |row| {
            Ok(OutcomeRow {
                op: row.get(0)?,
                kind: row.get(1)?,
                check_at: row.get(2)?,
                checked_at: row.get(3)?,
                state: row.get(4)?,
                detail: row.get(5)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// The row of one ticket.
    pub fn ticket(&self, key: &str) -> Result<Option<TicketRow>> {
        Ok(self
            .conn()
            .query_row(
                "SELECT key, state, flags, clone, branch, pr, head, taken, updated, counts, title FROM tickets WHERE key = ?1",
                [key],
                |row| {
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
                        title: row.get(10)?,
                    })
                },
            )
            .optional()?)
    }

    /// Writes the values that an operation set on a ticket row: the pull request number and the
    /// last commit. A `None` value keeps the old one. Gives false when the ticket has no row.
    pub fn set_ticket_values(
        &self,
        key: &str,
        pr: Option<i64>,
        head: Option<&str>,
        updated: &str,
    ) -> Result<bool> {
        let changed = self.conn().execute(
            "UPDATE tickets SET pr = coalesce(?2, pr), head = coalesce(?3, head), updated = ?4 WHERE key = ?1",
            params![key, pr, head, updated],
        )?;
        Ok(changed == 1)
    }

    pub fn add_check(&self, row: &CheckRow) -> Result<()> {
        self.conn().execute(
            "INSERT INTO checks (ticket, head, kind, result, command, seconds, cause, source, at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![
                row.ticket,
                row.head,
                row.kind,
                row.result,
                row.command,
                row.seconds,
                row.cause,
                row.source,
                row.at
            ],
        )?;
        Ok(())
    }

    /// The checks of a ticket, at one commit or at each, oldest first.
    pub fn checks_of(&self, ticket: &str, head: Option<&str>) -> Result<Vec<CheckRow>> {
        let mut stmt = self.conn().prepare(
            "SELECT coalesce(ticket, ''), coalesce(head, ''), coalesce(kind, ''), coalesce(result, ''),
                    coalesce(command, ''), coalesce(seconds, 0), coalesce(cause, ''), coalesce(source, ''),
                    coalesce(at, '')
             FROM checks WHERE ticket = ?1 AND (?2 IS NULL OR head = ?2) ORDER BY rowid",
        )?;
        let rows = stmt.query_map(params![ticket, head], |row| {
            Ok(CheckRow {
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
}

#[cfg(test)]
mod tests {
    use super::*;

    fn new<'a>(id: &'a str, idem: &'a str) -> NewOperation<'a> {
        NewOperation {
            id,
            idem,
            source: "rule",
            turn: None,
            ticket: "DEMO-12",
            verb: "git.push",
            args: "{}",
            mode: "auto",
            result: "{}",
        }
    }

    #[test]
    fn one_row_for_each_key_and_one_claim() {
        let ledger = Ledger::open_in_memory().unwrap();
        assert!(ledger.add_operation(&new("op-1", "k1")).unwrap());
        assert!(!ledger.add_operation(&new("op-2", "k1")).unwrap());
        assert_eq!(ledger.operation_by_idem("k1").unwrap().unwrap().id, "op-1");
        assert!(ledger.operation("op-2").unwrap().is_none());
        assert!(ledger.claim_operation("op-1", "auto", "t").unwrap());
        assert!(!ledger.claim_operation("op-1", "auto", "t").unwrap());
        assert!(!ledger
            .update_pending_operation("op-1", "ask", "{}")
            .unwrap());
        let rows = ledger.operations_in_states(&[STATE_EXECUTING]).unwrap();
        assert_eq!(rows.len(), 1);
        ledger.unclaim_operation("op-1").unwrap();
        assert_eq!(
            ledger.operation("op-1").unwrap().unwrap().state,
            STATE_PENDING
        );
        ledger
            .end_operation(
                "op-1",
                STATE_CONFIRMED,
                Some("x-1"),
                Some("t2"),
                "{\"detail\":\"ok\"}",
            )
            .unwrap();
        let row = ledger.operation("op-1").unwrap().unwrap();
        assert_eq!(row.external_id.as_deref(), Some("x-1"));
        assert_eq!(row.ended.as_deref(), Some("t2"));
        assert_eq!(
            ledger
                .operations_of("DEMO-12", Some("git.push"))
                .unwrap()
                .len(),
            1
        );
        assert!(ledger
            .operations_of("DEMO-12", Some("pr.open"))
            .unwrap()
            .is_empty());
    }

    #[test]
    fn ticket_values_and_checks() {
        let ledger = Ledger::open_in_memory().unwrap();
        assert!(!ledger
            .set_ticket_values("DEMO-12", Some(7), None, "t")
            .unwrap());
        ledger
            .conn()
            .execute(
                "INSERT INTO tickets (key, head) VALUES ('DEMO-12', 'abc')",
                [],
            )
            .unwrap();
        assert!(ledger
            .set_ticket_values("DEMO-12", Some(7), None, "t")
            .unwrap());
        let row = ledger.ticket("DEMO-12").unwrap().unwrap();
        assert_eq!((row.pr, row.head.as_deref()), (Some(7), Some("abc")));
        let check = CheckRow {
            ticket: "DEMO-12".into(),
            head: "abc".into(),
            kind: "spec".into(),
            result: "pass".into(),
            source: "job:proof".into(),
            ..CheckRow::default()
        };
        ledger.add_check(&check).unwrap();
        assert_eq!(
            ledger.checks_of("DEMO-12", Some("abc")).unwrap(),
            vec![check]
        );
        assert!(ledger.checks_of("DEMO-12", Some("def")).unwrap().is_empty());
    }
}
