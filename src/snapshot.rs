//! `state.json`: the snapshot that the tick writes at its step 8, and that `vision status`
//! and `vision prompt` read.

use std::path::Path;

use anyhow::{Context, Result};
use serde::de::IgnoredAny;
use serde::{Deserialize, Serialize};

use crate::config::Limits;
use crate::ctx::Ctx;
use crate::ledger::{Ledger, LimitReading, LimitReadings, TicketRow};
use crate::util::fs::{atomic_write, read_optional};
use crate::{curator, executor, pipeline, questions};

/// The version of the snapshot format.
pub const FORMAT: u32 = 1;

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Snapshot {
    pub format: u32,
    /// When the snapshot was written, RFC 3339 UTC.
    pub written: String,
    /// True when the `off` file was present.
    pub off: bool,
    pub tick: TickReport,
    pub limits: LimitsView,
    /// The items that wait for the person.
    pub needs: Vec<Need>,
    pub tickets: Vec<TicketRow>,
}

/// What the last tick did.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct TickReport {
    pub started: String,
    pub ended: String,
    pub outcome: Outcome,
    /// The steps that ran, in order, such as `lock`, `heartbeat`, `reconcile`, ..., `snapshot`.
    pub steps: Vec<String>,
    pub error: Option<String>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Outcome {
    /// Each step ran.
    #[default]
    Done,
    /// The `off` file was present: only the heartbeat and the snapshot were written.
    Off,
    /// A step failed. `error` says why.
    Failed,
}

/// One item that waits for the person, such as a card or a plan to approve.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Need {
    pub kind: NeedKind,
    pub ticket: Option<String>,
    /// The id that the command of the person names, such as a question id or a plan hash.
    pub id: String,
    /// One line for people.
    pub text: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum NeedKind {
    /// A card that the Judge sent to the person, or a proposal to accept.
    Card,
    /// A plan that waits for `vision approve-plan`.
    Plan,
    /// An operation in the mode ask that waits for `vision approve`.
    Operation,
    /// A ticket with the flag "needs you".
    Flag,
    /// A proposal of the Curator that waits for `vision memory review`.
    Proposal,
}

/// The limits of section 4 with the newest reading of each window.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct LimitsView {
    pub five_hour: LimitView,
    pub seven_day: LimitView,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct LimitView {
    /// No new turn above this part of the window.
    pub stop_at: f64,
    pub reading: Option<LimitReading>,
}

impl LimitsView {
    pub fn new(limits: &Limits, readings: LimitReadings) -> LimitsView {
        LimitsView {
            five_hour: LimitView {
                stop_at: limits.five_hour,
                reading: readings.five_hour,
            },
            seven_day: LimitView {
                stop_at: limits.seven_day,
                reading: readings.seven_day,
            },
        }
    }
}

impl Snapshot {
    /// Reads `state.json`, or gives `None` before the first tick.
    pub fn read(path: &Path) -> Result<Option<Snapshot>> {
        let Some(text) = read_optional(path)? else {
            return Ok(None);
        };
        let snapshot = serde_json::from_str(&text).with_context(|| {
            format!(
                "{} is not a valid snapshot. The next tick writes it again",
                path.display()
            )
        })?;
        Ok(Some(snapshot))
    }

    pub fn write(&self, path: &Path) -> Result<()> {
        let mut text = serde_json::to_string_pretty(self)?;
        text.push('\n');
        atomic_write(path, text.as_bytes())
    }

    /// The number of items that wait for the person, read without the rest of the snapshot.
    /// Zero before the first tick.
    pub fn needs_count(path: &Path) -> Result<usize> {
        #[derive(Deserialize)]
        struct Count {
            #[serde(default)]
            needs: Vec<IgnoredAny>,
        }
        let Some(text) = read_optional(path)? else {
            return Ok(0);
        };
        let count: Count = serde_json::from_str(&text)?;
        Ok(count.needs.len())
    }
}

/// The items that wait for the person, from each module that knows some.
pub fn collect_needs(ctx: &Ctx, ledger: &Ledger) -> Result<Vec<Need>> {
    let mut needs = questions::needs(ctx, ledger)?;
    needs.extend(pipeline::needs(ctx, ledger)?);
    needs.extend(executor::needs(ctx, ledger)?);
    needs.extend(curator::needs(ctx, ledger)?);
    Ok(needs)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_snapshot_round_trips_and_counts_its_needs() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.json");
        assert_eq!(Snapshot::read(&path).unwrap(), None);
        assert_eq!(Snapshot::needs_count(&path).unwrap(), 0);
        let snapshot = Snapshot {
            format: FORMAT,
            off: true,
            needs: vec![Need {
                kind: NeedKind::Plan,
                ticket: Some("DEMO-12".into()),
                id: "ab12".into(),
                text: "approve the plan".into(),
            }],
            ..Snapshot::default()
        };
        snapshot.write(&path).unwrap();
        assert_eq!(Snapshot::read(&path).unwrap(), Some(snapshot));
        assert_eq!(Snapshot::needs_count(&path).unwrap(), 1);
    }

    #[test]
    fn a_broken_snapshot_is_an_error_that_says_what_happens_next() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.json");
        std::fs::write(&path, "{").unwrap();
        let err = format!("{:#}", Snapshot::read(&path).unwrap_err());
        assert!(err.contains("next tick"), "{err}");
    }
}
