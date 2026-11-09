//! The latest readings of the subscription windows, from the `turns` table.

use anyhow::Result;
use rusqlite::OptionalExtension;
use serde::{Deserialize, Serialize};

use super::Ledger;

/// One reading of a subscription window, as the turn that saw it recorded it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct LimitReading {
    /// The used part of the window, from 0 to 1.
    pub part: f64,
    /// When the turn ended, or started when it still runs.
    pub at: String,
    pub turn: String,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct LimitReadings {
    pub five_hour: Option<LimitReading>,
    pub seven_day: Option<LimitReading>,
}

impl Ledger {
    /// The newest reading of each window, or `None` before the first turn reports one.
    pub fn latest_limits(&self) -> Result<LimitReadings> {
        Ok(LimitReadings {
            five_hour: self.latest_reading("five_hour")?,
            seven_day: self.latest_reading("seven_day")?,
        })
    }

    fn latest_reading(&self, column: &str) -> Result<Option<LimitReading>> {
        // `column` is one of two fixed names, never input.
        let sql = format!(
            "SELECT {column}, coalesce(ended, started, ''), id FROM turns WHERE {column} IS NOT NULL
             ORDER BY coalesce(ended, started) DESC LIMIT 1"
        );
        let reading = self
            .conn()
            .query_row(&sql, [], |row| {
                Ok(LimitReading {
                    part: row.get(0)?,
                    at: row.get(1)?,
                    turn: row.get(2)?,
                })
            })
            .optional()?;
        Ok(reading)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_newest_reading_of_each_window_wins() {
        let ledger = Ledger::open_in_memory().unwrap();
        assert_eq!(ledger.latest_limits().unwrap(), LimitReadings::default());
        ledger
            .conn()
            .execute_batch(
                "INSERT INTO turns (id, ended, five_hour, seven_day) VALUES ('t1', '2026-11-16T10:00:00Z', 0.2, 0.5);
                 INSERT INTO turns (id, ended, five_hour, seven_day) VALUES ('t2', '2026-11-16T11:00:00Z', 0.3, NULL);",
            )
            .unwrap();
        let limits = ledger.latest_limits().unwrap();
        assert_eq!(limits.five_hour.unwrap().turn, "t2");
        let seven = limits.seven_day.unwrap();
        assert_eq!((seven.part, seven.turn.as_str()), (0.5, "t1"));
    }
}
