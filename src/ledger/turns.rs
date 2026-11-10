//! The queries of turns (section 7) and models (section 10): the `turns` rows, the spend that
//! the budgets test, the limit readings with their reset times, the `messages` of the person,
//! the `model_changes`, and the evidence of `vision model recommend`.

use anyhow::Result;
use rusqlite::{params, OptionalExtension, Row};
use serde::Serialize;

use super::Ledger;

/// The status of a turn row while its supervisor lives, or until a tick collects it.
pub const STATUS_RUNNING: &str = "running";

/// A new row of `turns`, written before the supervisor starts.
#[derive(Clone, Debug, PartialEq)]
pub struct NewTurn<'a> {
    pub id: &'a str,
    pub ticket: &'a str,
    pub agent: &'a str,
    pub step: &'a str,
    pub kind: &'a str,
    /// The session id of the turn: a new id, or the id that it resumes.
    pub session: &'a str,
    pub resumes: Option<&'a str>,
    pub model: &'a str,
    pub effort: &'a str,
    pub started: &'a str,
    pub role_hash: &'a str,
}

/// One row of `turns`. JSON values such as `principles` stay as text.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct TurnRow {
    pub id: String,
    pub ticket: String,
    pub agent: Option<String>,
    pub step: Option<String>,
    pub kind: Option<String>,
    pub session: Option<String>,
    pub resumes: Option<String>,
    pub model: Option<String>,
    pub effort: Option<String>,
    pub started: Option<String>,
    pub ended: Option<String>,
    pub status: Option<String>,
    pub reason: Option<String>,
    pub cost_usd: Option<f64>,
    pub input: Option<i64>,
    pub cache_creation: Option<i64>,
    pub cache_read: Option<i64>,
    pub output: Option<i64>,
    pub denials: Option<i64>,
    pub five_hour: Option<f64>,
    pub seven_day: Option<f64>,
    pub five_hour_resets: Option<String>,
    pub seven_day_resets: Option<String>,
    pub role_hash: Option<String>,
    pub principles: Option<String>,
    pub end_fact: Option<String>,
}

const TURN_COLUMNS: &str =
    "id, coalesce(ticket, ''), agent, step, kind, session, resumes, model, effort, \
     started, ended, status, reason, cost_usd, input, cache_creation, cache_read, output, denials, \
     five_hour, seven_day, five_hour_resets, seven_day_resets, role_hash, principles, end_fact";

fn turn_row(row: &Row<'_>) -> rusqlite::Result<TurnRow> {
    Ok(TurnRow {
        id: row.get(0)?,
        ticket: row.get(1)?,
        agent: row.get(2)?,
        step: row.get(3)?,
        kind: row.get(4)?,
        session: row.get(5)?,
        resumes: row.get(6)?,
        model: row.get(7)?,
        effort: row.get(8)?,
        started: row.get(9)?,
        ended: row.get(10)?,
        status: row.get(11)?,
        reason: row.get(12)?,
        cost_usd: row.get(13)?,
        input: row.get(14)?,
        cache_creation: row.get(15)?,
        cache_read: row.get(16)?,
        output: row.get(17)?,
        denials: row.get(18)?,
        five_hour: row.get(19)?,
        seven_day: row.get(20)?,
        five_hour_resets: row.get(21)?,
        seven_day_resets: row.get(22)?,
        role_hash: row.get(23)?,
        principles: row.get(24)?,
        end_fact: row.get(25)?,
    })
}

/// A limit reading of one window, with its reset time.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct WindowReading {
    /// The used part of the window, from 0 to 1.
    pub part: f64,
    /// RFC 3339 UTC, or `None` when the session did not report it.
    pub resets: Option<String>,
}

/// What the collector writes on a turn row when the turn ended.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct TurnEnd {
    pub status: String,
    pub reason: String,
    pub ended: String,
    pub session: Option<String>,
    pub cost_usd: Option<f64>,
    pub input: i64,
    pub cache_creation: i64,
    pub cache_read: i64,
    pub output: i64,
    pub denials: i64,
    pub five_hour: Option<WindowReading>,
    pub seven_day: Option<WindowReading>,
}

/// The newest limit reading of one window that is still in force.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct ActiveReading {
    pub part: f64,
    pub resets: Option<String>,
    pub turn: String,
}

/// One row of `messages`.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct MessageRow {
    pub id: i64,
    pub turn: Option<String>,
    pub at: String,
    pub source: String,
    pub text: String,
    pub delivered: Option<String>,
}

/// A change of the model of an agent, for `model_changes`.
#[derive(Clone, Debug, PartialEq)]
pub struct ModelChange<'a> {
    pub at: &'a str,
    pub agent: &'a str,
    pub model: &'a str,
    pub effort: &'a str,
    pub old_model: &'a str,
    pub old_effort: &'a str,
    /// `set` or `reset`.
    pub source: &'a str,
}

/// The ended turns of one agent with one model, for `vision model recommend`.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct ModelTurns {
    pub model: String,
    pub turns: i64,
    pub cost_usd: f64,
    /// The turns whose `end_fact` is known.
    pub end_fact_known: i64,
    /// The turns whose `end_fact` is `held`.
    pub end_fact_held: i64,
    /// The distinct tickets of these turns.
    pub tickets: i64,
}

impl Ledger {
    /// Adds the row of a new turn with the status `running`.
    pub fn insert_turn(&self, turn: &NewTurn<'_>) -> Result<()> {
        self.conn().execute(
            "INSERT INTO turns (id, ticket, agent, step, kind, session, resumes, model, effort, started, status, role_hash)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
            params![
                turn.id,
                turn.ticket,
                turn.agent,
                turn.step,
                turn.kind,
                turn.session,
                turn.resumes,
                turn.model,
                turn.effort,
                turn.started,
                STATUS_RUNNING,
                turn.role_hash
            ],
        )?;
        Ok(())
    }

    /// One turn row, or `None`.
    pub fn turn(&self, id: &str) -> Result<Option<TurnRow>> {
        let sql = format!("SELECT {TURN_COLUMNS} FROM turns WHERE id = ?1");
        Ok(self.conn().query_row(&sql, [id], turn_row).optional()?)
    }

    /// The rows with this status, oldest first.
    pub fn turns_with_status(&self, status: &str) -> Result<Vec<TurnRow>> {
        let sql =
            format!("SELECT {TURN_COLUMNS} FROM turns WHERE status = ?1 ORDER BY started, id");
        let mut stmt = self.conn().prepare(&sql)?;
        let rows = stmt.query_map([status], turn_row)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// The turns of one ticket, oldest first.
    pub fn turns_of_ticket(&self, ticket: &str) -> Result<Vec<TurnRow>> {
        let sql =
            format!("SELECT {TURN_COLUMNS} FROM turns WHERE ticket = ?1 ORDER BY started, id");
        let mut stmt = self.conn().prepare(&sql)?;
        let rows = stmt.query_map([ticket], turn_row)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// The newest turn of one ticket.
    pub fn last_turn(&self, ticket: &str) -> Result<Option<TurnRow>> {
        let sql = format!(
            "SELECT {TURN_COLUMNS} FROM turns WHERE ticket = ?1 ORDER BY started DESC, id DESC LIMIT 1"
        );
        Ok(self.conn().query_row(&sql, [ticket], turn_row).optional()?)
    }

    /// The first turn that resumes `session` and started at or after `after`, other than `not`.
    pub fn next_turn_of_session(
        &self,
        session: &str,
        after: &str,
        not: &str,
    ) -> Result<Option<TurnRow>> {
        let sql = format!(
            "SELECT {TURN_COLUMNS} FROM turns WHERE resumes = ?1 AND started >= ?2 AND id <> ?3
             ORDER BY started, id LIMIT 1"
        );
        Ok(self
            .conn()
            .query_row(&sql, params![session, after, not], turn_row)
            .optional()?)
    }

    /// The number of turns of one step of one ticket.
    pub fn count_turns(&self, ticket: &str, step: &str) -> Result<u32> {
        Ok(self.conn().query_row(
            "SELECT count(*) FROM turns WHERE ticket = ?1 AND step = ?2",
            params![ticket, step],
            |row| row.get(0),
        )?)
    }

    /// Writes the end of a turn.
    pub fn end_turn(&self, id: &str, end: &TurnEnd) -> Result<()> {
        let part = |r: &Option<WindowReading>| r.as_ref().map(|r| r.part);
        let resets = |r: &Option<WindowReading>| r.as_ref().and_then(|r| r.resets.clone());
        self.conn().execute(
            "UPDATE turns SET status = ?2, reason = ?3, ended = ?4, session = coalesce(?5, session),
                 cost_usd = ?6, input = ?7, cache_creation = ?8, cache_read = ?9, output = ?10,
                 denials = ?11, five_hour = ?12, five_hour_resets = ?13, seven_day = ?14,
                 seven_day_resets = ?15
             WHERE id = ?1",
            params![
                id,
                end.status,
                end.reason,
                end.ended,
                end.session,
                end.cost_usd,
                end.input,
                end.cache_creation,
                end.cache_read,
                end.output,
                end.denials,
                part(&end.five_hour),
                resets(&end.five_hour),
                part(&end.seven_day),
                resets(&end.seven_day)
            ],
        )?;
        Ok(())
    }

    /// Changes the status of a turn row.
    pub fn set_turn_status(&self, id: &str, status: &str) -> Result<()> {
        self.conn().execute(
            "UPDATE turns SET status = ?2 WHERE id = ?1",
            params![id, status],
        )?;
        Ok(())
    }

    /// The dollars that the turns of one ticket cost.
    pub fn spent_on_ticket(&self, ticket: &str) -> Result<f64> {
        Ok(self.conn().query_row(
            "SELECT coalesce(sum(cost_usd), 0) FROM turns WHERE ticket = ?1",
            [ticket],
            |row| row.get(0),
        )?)
    }

    /// The dollars that the turns cost that started at or after `since` (RFC 3339 UTC).
    pub fn spent_since(&self, since: &str) -> Result<f64> {
        Ok(self.conn().query_row(
            "SELECT coalesce(sum(cost_usd), 0) FROM turns WHERE started >= ?1",
            [since],
            |row| row.get(0),
        )?)
    }

    /// The newest reading of a window (`five_hour` or `seven_day`) whose reset time is after
    /// `now`. A reading with no reset time stays in force until a newer reading replaces it.
    pub fn active_reading(&self, window: Window, now: &str) -> Result<Option<ActiveReading>> {
        let column = window.column();
        // `column` is one of two fixed names, never input.
        let sql = format!(
            "SELECT {column}, {column}_resets, id FROM turns WHERE {column} IS NOT NULL
             ORDER BY coalesce(ended, started) DESC, id DESC LIMIT 1"
        );
        let newest = self
            .conn()
            .query_row(&sql, [], |row| {
                Ok(ActiveReading {
                    part: row.get(0)?,
                    resets: row.get(1)?,
                    turn: row.get(2)?,
                })
            })
            .optional()?;
        Ok(newest.filter(|r| r.resets.as_deref().is_none_or(|resets| resets > now)))
    }

    /// Adds a message of the person and gives its id.
    pub fn add_message(
        &self,
        turn: Option<&str>,
        at: &str,
        source: &str,
        text: &str,
    ) -> Result<i64> {
        self.conn().execute(
            "INSERT INTO messages (turn, at, source, text) VALUES (?1, ?2, ?3, ?4)",
            params![turn, at, source, text],
        )?;
        Ok(self.conn().last_insert_rowid())
    }

    /// Ties a message to a turn, or frees it for the next turn with `None`.
    pub fn set_message_turn(&self, id: i64, turn: Option<&str>) -> Result<()> {
        self.conn().execute(
            "UPDATE messages SET turn = ?2 WHERE id = ?1",
            params![id, turn],
        )?;
        Ok(())
    }

    /// Records when the session got a message.
    pub fn set_message_delivered(&self, id: i64, at: &str) -> Result<()> {
        self.conn().execute(
            "UPDATE messages SET delivered = ?2 WHERE id = ?1",
            params![id, at],
        )?;
        Ok(())
    }

    /// One message, or `None`.
    pub fn message(&self, id: i64) -> Result<Option<MessageRow>> {
        Ok(self
            .conn()
            .query_row(
                "SELECT id, turn, at, source, text, delivered FROM messages WHERE id = ?1",
                [id],
                |row| {
                    Ok(MessageRow {
                        id: row.get(0)?,
                        turn: row.get(1)?,
                        at: row.get(2)?,
                        source: row.get(3)?,
                        text: row.get(4)?,
                        delivered: row.get(5)?,
                    })
                },
            )
            .optional()?)
    }

    /// Adds a row to `model_changes`.
    pub fn add_model_change(&self, change: &ModelChange<'_>) -> Result<()> {
        self.conn().execute(
            "INSERT INTO model_changes (at, agent, model, effort, old_model, old_effort, source)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                change.at,
                change.agent,
                change.model,
                change.effort,
                change.old_model,
                change.old_effort,
                change.source
            ],
        )?;
        Ok(())
    }

    /// The ended turns of one agent, grouped by model.
    pub fn model_turns(&self, agent: &str) -> Result<Vec<ModelTurns>> {
        let mut stmt = self.conn().prepare(
            "SELECT model, count(*), coalesce(sum(cost_usd), 0),
                    count(end_fact), sum(CASE WHEN end_fact = 'held' THEN 1 ELSE 0 END),
                    count(DISTINCT ticket)
             FROM turns WHERE agent = ?1 AND model IS NOT NULL AND ended IS NOT NULL
             GROUP BY model ORDER BY model",
        )?;
        let rows = stmt.query_map([agent], |row| {
            Ok(ModelTurns {
                model: row.get(0)?,
                turns: row.get(1)?,
                cost_usd: row.get(2)?,
                end_fact_known: row.get(3)?,
                end_fact_held: row.get::<_, Option<i64>>(4)?.unwrap_or(0),
                tickets: row.get(5)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// A count from another table over the tickets or the turns of one agent with one model,
    /// for the extra numbers of `vision model recommend`. `sql` takes `?1` = agent, `?2` = model.
    pub fn model_count(&self, sql: &str, agent: &str, model: &str) -> Result<i64> {
        Ok(self
            .conn()
            .query_row(sql, params![agent, model], |row| row.get(0))?)
    }

    /// The flags of a ticket, as the JSON list of `tickets.flags`.
    pub fn ticket_flags(&self, key: &str) -> Result<Vec<String>> {
        let flags: Option<Option<String>> = self
            .conn()
            .query_row("SELECT flags FROM tickets WHERE key = ?1", [key], |row| {
                row.get(0)
            })
            .optional()?;
        let text = flags.flatten().unwrap_or_default();
        if text.trim().is_empty() {
            return Ok(Vec::new());
        }
        Ok(serde_json::from_str(&text).unwrap_or_default())
    }

    /// Adds a flag to a ticket, and makes the row when the ticket has none.
    pub fn add_ticket_flag(&self, key: &str, flag: &str, now: &str) -> Result<()> {
        let mut flags = self.ticket_flags(key)?;
        if !flags.iter().any(|f| f == flag) {
            flags.push(flag.to_string());
        }
        let text = serde_json::to_string(&flags)?;
        self.conn().execute(
            "INSERT INTO tickets (key, flags, updated) VALUES (?1, ?2, ?3)
             ON CONFLICT (key) DO UPDATE SET flags = excluded.flags, updated = excluded.updated",
            params![key, text, now],
        )?;
        Ok(())
    }
}

/// A subscription window of section 4.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Window {
    FiveHour,
    SevenDay,
}

impl Window {
    /// The column of `turns`, which is also the key of `[limits]`.
    pub fn column(self) -> &'static str {
        match self {
            Window::FiveHour => "five_hour",
            Window::SevenDay => "seven_day",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn new_turn<'a>(id: &'a str, ticket: &'a str, started: &'a str) -> NewTurn<'a> {
        NewTurn {
            id,
            ticket,
            agent: "scout",
            step: "refine",
            kind: "reader",
            session: "s-1",
            resumes: None,
            model: "claude-fable-5-2",
            effort: "high",
            started,
            role_hash: "h",
        }
    }

    #[test]
    fn a_turn_row_is_written_ended_and_read_back() {
        let ledger = Ledger::open_in_memory().unwrap();
        ledger
            .insert_turn(&new_turn("t1", "DEMO-12", "2026-11-16T14:30:00Z"))
            .unwrap();
        assert_eq!(ledger.count_turns("DEMO-12", "refine").unwrap(), 1);
        assert_eq!(ledger.turns_with_status(STATUS_RUNNING).unwrap().len(), 1);
        let end = TurnEnd {
            status: "done".into(),
            reason: "result".into(),
            ended: "2026-11-16T14:31:00Z".into(),
            session: Some("s-2".into()),
            cost_usd: Some(0.25),
            input: 1,
            cache_creation: 2,
            cache_read: 3,
            output: 4,
            denials: 1,
            five_hour: Some(WindowReading {
                part: 0.4,
                resets: Some("2026-11-16T18:00:00Z".into()),
            }),
            seven_day: None,
        };
        ledger.end_turn("t1", &end).unwrap();
        let row = ledger.turn("t1").unwrap().unwrap();
        assert_eq!(row.status.as_deref(), Some("done"));
        assert_eq!(row.session.as_deref(), Some("s-2"));
        assert_eq!(
            (row.input, row.output, row.denials),
            (Some(1), Some(4), Some(1))
        );
        assert_eq!(
            row.five_hour_resets.as_deref(),
            Some("2026-11-16T18:00:00Z")
        );
        assert_eq!(row.seven_day, None);
        assert_eq!(ledger.spent_on_ticket("DEMO-12").unwrap(), 0.25);
        assert_eq!(ledger.spent_since("2026-11-17T00:00:00Z").unwrap(), 0.0);
    }

    #[test]
    fn a_reading_after_its_reset_time_is_not_in_force() {
        let ledger = Ledger::open_in_memory().unwrap();
        ledger
            .conn()
            .execute_batch(
                "INSERT INTO turns (id, ended, five_hour, five_hour_resets) VALUES ('t1', '2026-11-16T10:00:00Z', 0.9, '2026-11-16T12:00:00Z');",
            )
            .unwrap();
        let before = ledger
            .active_reading(Window::FiveHour, "2026-11-16T11:00:00Z")
            .unwrap();
        assert_eq!(before.unwrap().part, 0.9);
        let after = ledger
            .active_reading(Window::FiveHour, "2026-11-16T12:00:01Z")
            .unwrap();
        assert_eq!(after, None);
        assert_eq!(
            ledger
                .active_reading(Window::SevenDay, "2026-11-16T11:00:00Z")
                .unwrap(),
            None
        );
    }

    #[test]
    fn flags_are_added_once() {
        let ledger = Ledger::open_in_memory().unwrap();
        ledger.add_ticket_flag("DEMO-12", "paused", "now").unwrap();
        ledger.add_ticket_flag("DEMO-12", "paused", "now").unwrap();
        assert_eq!(ledger.ticket_flags("DEMO-12").unwrap(), ["paused"]);
        assert!(ledger.ticket_flags("DEMO-13").unwrap().is_empty());
    }

    #[test]
    fn messages_record_their_turn_and_delivery() {
        let ledger = Ledger::open_in_memory().unwrap();
        let id = ledger.add_message(None, "a", "user", "hello").unwrap();
        ledger.set_message_turn(id, Some("t1")).unwrap();
        ledger.set_message_delivered(id, "b").unwrap();
        let row = ledger.message(id).unwrap().unwrap();
        assert_eq!(row.turn.as_deref(), Some("t1"));
        assert_eq!(row.delivered.as_deref(), Some("b"));
    }
}
