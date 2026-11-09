//! The `events` table: one row for each thing that happened, with an optional external id.

use anyhow::Result;
use rusqlite::params;
use serde::Serialize;
use serde_json::Value;

use super::Ledger;
use crate::util::time;

/// A new event. Build it with [`Event::now`] and the setters.
#[derive(Clone, Debug, PartialEq)]
pub struct Event {
    pub at: String,
    /// Who did it: `user`, `tick`, an agent name, or an adapter script.
    pub actor: String,
    pub ticket: Option<String>,
    pub kind: String,
    /// Stored as JSON text. `Null` is stored as SQL NULL.
    pub detail: Value,
    /// An id from outside, such as a review comment id. The ledger keeps one event for each.
    pub external_id: Option<String>,
}

impl Event {
    /// An event at this time.
    pub fn now(actor: &str, kind: &str) -> Event {
        Event {
            at: time::now_rfc3339(),
            actor: actor.to_string(),
            ticket: None,
            kind: kind.to_string(),
            detail: Value::Null,
            external_id: None,
        }
    }

    pub fn ticket(mut self, key: &str) -> Event {
        self.ticket = Some(key.to_string());
        self
    }

    pub fn detail(mut self, detail: Value) -> Event {
        self.detail = detail;
        self
    }

    pub fn external_id(mut self, id: &str) -> Event {
        self.external_id = Some(id.to_string());
        self
    }
}

/// An event as the ledger stores it.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct EventRow {
    pub id: i64,
    pub at: String,
    pub actor: String,
    pub ticket: Option<String>,
    pub kind: String,
    pub detail: Option<String>,
    pub external_id: Option<String>,
}

impl Ledger {
    /// Adds an event and gives its row id. With an external id that the ledger already has,
    /// it adds nothing and gives `None`, so that a repeated tick does not record it twice.
    pub fn add_event(&self, event: &Event) -> Result<Option<i64>> {
        let detail = (!event.detail.is_null()).then(|| event.detail.to_string());
        let added = self.conn().execute(
            "INSERT INTO events (at, actor, ticket, kind, detail, external_id) VALUES (?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT (external_id) DO NOTHING",
            params![event.at, event.actor, event.ticket, event.kind, detail, event.external_id],
        )?;
        Ok((added == 1).then(|| self.conn().last_insert_rowid()))
    }

    /// The newest events first, of one ticket or of all.
    pub fn recent_events(&self, ticket: Option<&str>, limit: u32) -> Result<Vec<EventRow>> {
        let mut stmt = self.conn().prepare(
            "SELECT id, at, actor, ticket, kind, detail, external_id FROM events
             WHERE ?1 IS NULL OR ticket = ?1 ORDER BY id DESC LIMIT ?2",
        )?;
        let rows = stmt.query_map(params![ticket, limit], |row| {
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn an_external_id_is_recorded_once() {
        let ledger = Ledger::open_in_memory().unwrap();
        let event = Event::now("tick", "comment")
            .ticket("DEMO-12")
            .external_id("c-7");
        assert!(ledger.add_event(&event).unwrap().is_some());
        assert_eq!(ledger.add_event(&event).unwrap(), None);
        assert!(ledger
            .add_event(&Event::now("user", "off"))
            .unwrap()
            .is_some());
        assert!(ledger
            .add_event(&Event::now("user", "on"))
            .unwrap()
            .is_some());
        assert_eq!(ledger.recent_events(None, 10).unwrap().len(), 3);
    }

    #[test]
    fn events_read_back_newest_first() {
        let ledger = Ledger::open_in_memory().unwrap();
        ledger
            .add_event(&Event::now("user", "take").ticket("DEMO-12"))
            .unwrap();
        ledger
            .add_event(
                &Event::now("tick", "state")
                    .ticket("DEMO-12")
                    .detail(json!({"state": "plan"})),
            )
            .unwrap();
        ledger
            .add_event(&Event::now("tick", "state").ticket("DEMO-13"))
            .unwrap();
        let rows = ledger.recent_events(Some("DEMO-12"), 10).unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].detail.as_deref(), Some(r#"{"state":"plan"}"#));
        assert_eq!(rows[1].kind, "take");
        assert_eq!(rows[1].detail, None);
    }
}
