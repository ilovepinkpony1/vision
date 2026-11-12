//! The ledger (section 5): SQLite in WAL mode with a busy timeout of 5 seconds.
//!
//! `PRAGMA user_version` holds the schema version. Each migration is one numbered SQL file in
//! `src/ledger/migrations/` that `build.rs` embeds; a later milestone adds a migration by adding
//! a file. Opening a ledger applies the migrations that it lacks.
//!
//! The typed queries here are the few that the base needs. A later milestone keeps its own
//! queries in its own module and uses [`Ledger::conn`].

pub mod curator;
mod events;
mod health;
mod limits;
pub mod memory_use;
pub mod notices;
pub mod operations;
pub mod pipeline;
pub mod questions;
mod tickets;
pub mod turns;

use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{bail, Context, Result};
use rusqlite::{Connection, OpenFlags, TransactionBehavior};

pub use events::{Event, EventRow};
pub use health::HealthRow;
pub use limits::{LimitReading, LimitReadings};
pub use tickets::TicketRow;

/// One embedded migration.
struct Migration {
    version: u32,
    name: &'static str,
    sql: &'static str,
}

include!(concat!(env!("OUT_DIR"), "/migrations.rs"));

/// The schema version that this binary knows: the number of the last migration.
pub const SCHEMA_VERSION: u32 = MIGRATIONS.len() as u32;

const BUSY_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Debug, thiserror::Error)]
pub enum LedgerError {
    #[error("no ledger at {0}. Run `vision init` to create it")]
    Missing(PathBuf),
    #[error(
        "the ledger has schema version {found}, but this vision knows only versions up to {known}. \
         Install a newer vision, or restore a backup of the ledger"
    )]
    TooNew { found: u32, known: u32 },
}

/// An open connection to the ledger.
#[derive(Debug)]
pub struct Ledger {
    conn: Connection,
    migrated_from: Option<u32>,
}

impl Ledger {
    /// Opens the ledger, and creates the file when it does not exist.
    pub fn create(path: &Path) -> Result<Ledger> {
        let flags = OpenFlags::SQLITE_OPEN_READ_WRITE
            | OpenFlags::SQLITE_OPEN_CREATE
            | OpenFlags::SQLITE_OPEN_NO_MUTEX;
        Ledger::open_with(path, flags)
    }

    /// Opens a ledger that exists.
    pub fn open(path: &Path) -> Result<Ledger> {
        if !path.is_file() {
            return Err(LedgerError::Missing(path.to_path_buf()).into());
        }
        Ledger::open_with(
            path,
            OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )
    }

    /// A ledger in memory with the full schema, for tests.
    pub fn open_in_memory() -> Result<Ledger> {
        let conn = Connection::open_in_memory().context("could not open a ledger in memory")?;
        Ledger::setup(conn, false)
    }

    fn open_with(path: &Path, flags: OpenFlags) -> Result<Ledger> {
        let conn = Connection::open_with_flags(path, flags)
            .with_context(|| format!("could not open the ledger {}", path.display()))?;
        Ledger::setup(conn, true)
            .with_context(|| format!("could not open the ledger {}", path.display()))
    }

    fn setup(mut conn: Connection, on_disk: bool) -> Result<Ledger> {
        conn.busy_timeout(BUSY_TIMEOUT)?;
        if on_disk {
            let mode: String =
                conn.pragma_update_and_check(None, "journal_mode", "WAL", |row| row.get(0))?;
            if !mode.eq_ignore_ascii_case("wal") {
                bail!("SQLite could not use WAL mode here (it uses {mode}). Put VISION_HOME on a local disk");
            }
            conn.pragma_update(None, "synchronous", "NORMAL")?;
        }
        conn.pragma_update(None, "foreign_keys", true)?;
        let migrated_from = migrate(&mut conn)?;
        Ok(Ledger {
            conn,
            migrated_from,
        })
    }

    /// The schema version of the open ledger.
    pub fn schema_version(&self) -> Result<u32> {
        user_version(&self.conn)
    }

    /// The version before this open applied migrations, or `None` when none ran.
    pub fn migrated_from(&self) -> Option<u32> {
        self.migrated_from
    }

    pub fn conn(&self) -> &Connection {
        &self.conn
    }

    /// For transactions: `ledger.conn_mut().transaction()`.
    pub fn conn_mut(&mut self) -> &mut Connection {
        &mut self.conn
    }

    /// Runs `f` in one immediate transaction: each write of `f`, or none. Inside a transaction
    /// that is already open, `f` runs as a part of it.
    pub fn in_transaction<T>(&self, f: impl FnOnce(&Ledger) -> Result<T>) -> Result<T> {
        if !self.conn.is_autocommit() {
            return f(self);
        }
        self.conn.execute_batch("BEGIN IMMEDIATE")?;
        match f(self) {
            Ok(value) => {
                if let Err(e) = self.conn.execute_batch("COMMIT") {
                    let _ = self.conn.execute_batch("ROLLBACK");
                    return Err(e.into());
                }
                Ok(value)
            }
            Err(e) => {
                let _ = self.conn.execute_batch("ROLLBACK");
                Err(e)
            }
        }
    }
}

fn user_version(conn: &Connection) -> Result<u32> {
    Ok(conn.pragma_query_value(None, "user_version", |row| row.get(0))?)
}

fn check_known(found: u32) -> Result<()> {
    if found > SCHEMA_VERSION {
        return Err(LedgerError::TooNew {
            found,
            known: SCHEMA_VERSION,
        }
        .into());
    }
    Ok(())
}

/// Applies the missing migrations in one transaction. Gives the old version when any ran.
fn migrate(conn: &mut Connection) -> Result<Option<u32>> {
    let current = user_version(conn)?;
    check_known(current)?;
    if current == SCHEMA_VERSION {
        return Ok(None);
    }
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    // A different process may have migrated while this one waited for the lock.
    let current = user_version(&tx)?;
    check_known(current)?;
    if current == SCHEMA_VERSION {
        return Ok(None);
    }
    for migration in MIGRATIONS.iter().filter(|m| m.version > current) {
        tx.execute_batch(migration.sql).with_context(|| {
            format!(
                "migration {} failed, so the ledger stays at version {current}. Report this as a bug",
                migration.name
            )
        })?;
    }
    tx.pragma_update(None, "user_version", SCHEMA_VERSION)?;
    tx.commit()?;
    Ok(Some(current))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The tables of section 5.
    const TABLES: [&str; 16] = [
        "audits",
        "checks",
        "events",
        "findings",
        "health",
        "human",
        "memory_use",
        "messages",
        "model_changes",
        "operations",
        "outcomes",
        "questions",
        "review_rows",
        "spans",
        "tickets",
        "turns",
    ];

    fn table_names(ledger: &Ledger) -> Vec<String> {
        let mut stmt = ledger
            .conn()
            .prepare("SELECT name FROM sqlite_master WHERE type = 'table' ORDER BY name")
            .unwrap();
        stmt.query_map([], |row| row.get(0))
            .unwrap()
            .map(|r| r.unwrap())
            .collect()
    }

    #[test]
    fn migrations_are_numbered_from_one() {
        for (index, migration) in MIGRATIONS.iter().enumerate() {
            assert_eq!(migration.version as usize, index + 1, "{}", migration.name);
        }
        assert_eq!(MIGRATIONS.len(), SCHEMA_VERSION as usize);
    }

    #[test]
    fn a_new_ledger_has_the_full_schema() {
        let ledger = Ledger::open_in_memory().unwrap();
        assert_eq!(ledger.schema_version().unwrap(), SCHEMA_VERSION);
        assert_eq!(ledger.migrated_from(), Some(0));
        // A later migration may add tables; each table of section 5 must stay.
        let names = table_names(&ledger);
        for table in TABLES {
            assert!(
                names.iter().any(|n| n == table),
                "the table {table} is missing"
            );
        }
    }

    #[test]
    fn a_file_ledger_uses_wal_and_migrates_once() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ledger.sqlite");
        let ledger = Ledger::create(&path).unwrap();
        assert_eq!(ledger.migrated_from(), Some(0));
        let mode: String = ledger
            .conn()
            .pragma_query_value(None, "journal_mode", |r| r.get(0))
            .unwrap();
        assert_eq!(mode, "wal");
        drop(ledger);
        let again = Ledger::open(&path).unwrap();
        assert_eq!(again.migrated_from(), None);
        assert_eq!(again.schema_version().unwrap(), SCHEMA_VERSION);
    }

    #[test]
    fn open_does_not_create_a_missing_ledger() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ledger.sqlite");
        let err = Ledger::open(&path).unwrap_err();
        assert!(err.to_string().contains("vision init"), "{err}");
        assert!(!path.exists());
    }

    #[test]
    fn a_newer_ledger_is_a_clear_error() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ledger.sqlite");
        let ledger = Ledger::create(&path).unwrap();
        ledger
            .conn()
            .pragma_update(None, "user_version", SCHEMA_VERSION + 1)
            .unwrap();
        drop(ledger);
        let err = format!("{:#}", Ledger::open(&path).unwrap_err());
        assert!(
            err.contains(&format!("schema version {}", SCHEMA_VERSION + 1)),
            "{err}"
        );
        assert!(err.contains("Install a newer vision"), "{err}");
    }

    #[test]
    fn migration_3_gives_each_ticket_a_title_that_is_empty_before_it() {
        let mut conn = Connection::open_in_memory().unwrap();
        for migration in MIGRATIONS.iter().filter(|m| m.version <= 2) {
            conn.execute_batch(migration.sql).unwrap();
        }
        conn.pragma_update(None, "user_version", 2).unwrap();
        conn.execute_batch("INSERT INTO tickets (key, state) VALUES ('DEMO-1', 'build')")
            .unwrap();
        assert_eq!(migrate(&mut conn).unwrap(), Some(2));
        let ledger = Ledger {
            conn,
            migrated_from: Some(2),
        };
        let old = ledger.ticket("DEMO-1").unwrap().unwrap();
        assert_eq!(old.title, None, "a ticket of before has no title");
        assert_eq!(old.state.as_deref(), Some("build"));
        ledger
            .put_ticket(&TicketRow {
                key: "DEMO-12".into(),
                state: Some("refine".into()),
                title: Some("Export the report".into()),
                ..TicketRow::default()
            })
            .unwrap();
        let rows = ledger.tickets().unwrap();
        assert_eq!(rows[1].title.as_deref(), Some("Export the report"));
    }

    #[test]
    fn migration_4_adds_the_notices_and_keeps_each_row_of_before() {
        let mut conn = Connection::open_in_memory().unwrap();
        for migration in MIGRATIONS.iter().filter(|m| m.version <= 3) {
            conn.execute_batch(migration.sql).unwrap();
        }
        conn.pragma_update(None, "user_version", 3).unwrap();
        conn.execute_batch(
            "INSERT INTO tickets (key, state, title) VALUES ('DEMO-1', 'qa', 'Export')",
        )
        .unwrap();
        assert_eq!(migrate(&mut conn).unwrap(), Some(3));
        let ledger = Ledger {
            conn,
            migrated_from: Some(3),
        };
        let old = ledger.ticket("DEMO-1").unwrap().unwrap();
        assert_eq!(old.title.as_deref(), Some("Export"));
        assert!(ledger.unread_notices().unwrap().is_empty());
        let issues = [notices::NoticeIssue {
            id: "ISSUE-001".into(),
            severity: "low".into(),
            title: "A typo".into(),
        }];
        let notice = notices::NewNotice {
            ticket: "DEMO-1",
            step: "qa",
            turn: "T1",
            gate: "qa",
            doc: "qa.md",
            issues: &issues,
            note: None,
            created: "2026-11-16T14:30:00Z",
        };
        assert!(ledger.add_notice(&notice).unwrap().is_some());
        assert_eq!(ledger.unread_notices().unwrap().len(), 1);
    }

    #[test]
    fn a_failed_migration_changes_nothing() {
        let mut conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE tickets (key TEXT)")
            .unwrap();
        let err = format!("{:#}", migrate(&mut conn).unwrap_err());
        assert!(err.contains("stays at version 0"), "{err}");
        assert_eq!(user_version(&conn).unwrap(), 0);
        let turns: i64 = conn
            .query_row(
                "SELECT count(*) FROM sqlite_master WHERE name = 'turns'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(turns, 0);
    }
}
