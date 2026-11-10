//! What the Curator keeps in the ledger: one event for each step of a run, keyed by an
//! external id, and the `human` rows of the review.
//!
//! - `curator-started:<turn>`: a run started, with its signals and its [`Mark`] ([`Started`]).
//! - `curator-applied:<turn>`: the run ended well, and its result is applied ([`Applied`]).
//!   The newest one is the last good run: the next signals start at its mark.
//! - `curator-failed:<turn>`: the run ended with no result that the runner could apply.
//! - `curator-proposal:<branch>`: the proposal of a run, with the summary of the Curator, the
//!   dropped changes, and both replays ([`ProposalRecord`]).
//! - `curator-baseline:<key>`: a replay against `main`, for one commit, model, and set of cards.
//! - `curator-closed:<branch>`: the review closed the proposal and deleted its branch.
//! - `ticket-merged:<KEY>:<PR>`: the pipeline records it when a ticket reaches its end with a
//!   merged pull request ([`EVENT_MERGED`]). The tick then starts a Curator turn.
//! - `model-eval`: one run of `vision model eval`.
//!
//! A `human` row of the kind `proposal-accept` or `proposal-reject` records each verdict of the
//! person, with the object `<branch> change <n>`.

use anyhow::Result;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::replays::Replays;
use super::signals::Mark;
use crate::ledger::{Event, EventRow, Ledger};

/// The event that the pipeline records when a ticket reaches its end with a merged pull request.
pub const EVENT_MERGED: &str = "ticket-merged";
pub const EVENT_STARTED: &str = "curator-started";
pub const EVENT_APPLIED: &str = "curator-applied";
pub const EVENT_FAILED: &str = "curator-failed";
pub const EVENT_PROPOSAL: &str = "curator-proposal";
pub const EVENT_BASELINE: &str = "curator-baseline";
pub const EVENT_CLOSED: &str = "curator-closed";
/// The event of one run of `vision model eval`.
pub const EVENT_EVAL: &str = "model-eval";

/// The kind of a `human` row of an accepted change.
pub const HUMAN_ACCEPT: &str = "proposal-accept";
/// The kind of a `human` row of a rejected change.
pub const HUMAN_REJECT: &str = "proposal-reject";

/// The detail of `curator-started`.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Started {
    /// The first turn of the run. An automatic resume continues it with a new turn.
    pub turn: String,
    /// Why the run started, such as `vision curate` or `DEMO-12 merged`.
    pub why: String,
    /// The mark of the last good run, where the signals start.
    pub since: Option<Mark>,
    /// The mark of these signals: the next run starts here when this one ends well.
    pub mark: Mark,
    /// The signals file.
    pub signals: String,
    pub counts: std::collections::BTreeMap<String, usize>,
}

/// One change of the Curator that the runner dropped, and why.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Dropped {
    /// The place of the change in the list of the Curator, from 1.
    pub index: usize,
    pub op: String,
    /// What the change names: a path, a target, or a question.
    pub names: String,
    pub reason: String,
}

impl Dropped {
    /// One line for people.
    pub fn line(&self) -> String {
        let names = if self.names.is_empty() {
            String::new()
        } else {
            format!(" {}", self.names)
        };
        format!(
            "change {} of the Curator ({}{names}): {}",
            self.index, self.op, self.reason
        )
    }
}

/// The detail of `curator-applied`: a run that ended well.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Applied {
    pub turn: String,
    /// The turn whose result was applied: the first turn, or its automatic resume.
    pub last_turn: String,
    pub mark: Mark,
    pub summary: String,
    pub proposal: Option<String>,
    /// The changes of the Curator, and how many of them the proposal holds.
    pub changes: usize,
    pub kept: usize,
    pub dropped: Vec<Dropped>,
    /// Why the run made no proposal, such as a proposal that lost a line.
    pub note: Option<String>,
}

/// The detail of `curator-failed`.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Failed {
    pub turn: String,
    pub last_turn: String,
    pub reason: String,
}

/// The state of a proposal record.
pub const OPEN: &str = "open";
pub const REJECTED: &str = "rejected";
pub const CLOSED: &str = "closed";

/// The detail of `curator-proposal:<branch>`.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ProposalRecord {
    pub proposal: String,
    pub turn: String,
    pub summary: String,
    pub created: String,
    pub dropped: Vec<Dropped>,
    /// `open`, `rejected` (Vision rejected it: it lost a line), or `closed` (reviewed).
    pub state: String,
    /// The lines that the proposal lost, when Vision rejected it for them.
    pub lost: Vec<String>,
    pub replays: Option<Replays>,
}

/// The external id of an event of a run or a proposal.
pub fn id_of(kind: &str, key: &str) -> String {
    format!("{kind}:{key}")
}

/// The detail of an event as a typed value, or `None` when it has none or does not parse.
pub fn detail_of<T: DeserializeOwned>(row: &EventRow) -> Option<T> {
    row.detail
        .as_deref()
        .and_then(|d| serde_json::from_str(d).ok())
}

/// The detail of the event with this kind and key.
pub fn read<T: DeserializeOwned>(ledger: &Ledger, kind: &str, key: &str) -> Result<Option<T>> {
    Ok(ledger
        .event_by_external_id(&id_of(kind, key))?
        .as_ref()
        .and_then(detail_of))
}

/// Adds the event with this kind and key once. Gives false when it was already there.
pub fn add<T: Serialize>(
    ledger: &Ledger,
    actor: &str,
    kind: &str,
    key: &str,
    detail: &T,
) -> Result<bool> {
    let event = Event::now(actor, kind)
        .external_id(&id_of(kind, key))
        .detail(serde_json::to_value(detail)?);
    Ok(ledger.add_event(&event)?.is_some())
}

/// Adds the event, or replaces its detail when it is there.
pub fn put<T: Serialize>(
    ledger: &Ledger,
    actor: &str,
    kind: &str,
    key: &str,
    detail: &T,
) -> Result<()> {
    let value: Value = serde_json::to_value(detail)?;
    if !ledger.set_event_detail(&id_of(kind, key), &value)? {
        add(ledger, actor, kind, key, detail)?;
    }
    Ok(())
}

/// The last good run: the newest `curator-applied`.
pub fn last_good(ledger: &Ledger) -> Result<Option<(EventRow, Applied)>> {
    Ok(ledger
        .latest_event(EVENT_APPLIED)?
        .and_then(|row| detail_of::<Applied>(&row).map(|a| (row, a))))
}

/// Records that a ticket reached its end with a merged pull request: the event
/// `ticket-merged` with the external id `ticket-merged:<KEY>:<PR>`. A second call for the same
/// pull request adds nothing. Gives true when the event is new.
pub fn record_merged(ledger: &Ledger, key: &str, pr: i64, head: Option<&str>) -> Result<bool> {
    let event = Event::now("tick", EVENT_MERGED)
        .ticket(key)
        .external_id(&format!("{EVENT_MERGED}:{key}:{pr}"))
        .detail(serde_json::json!({"pr": pr, "head": head}));
    Ok(ledger.add_event(&event)?.is_some())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn records_round_trip_through_the_events() {
        let ledger = Ledger::open_in_memory().unwrap();
        let applied = Applied {
            turn: "t1".into(),
            summary: "Tidy.".into(),
            dropped: vec![Dropped {
                index: 2,
                op: "add".into(),
                names: "principles/NEW-1.md".into(),
                reason: "it names one ticket".into(),
            }],
            ..Applied::default()
        };
        assert!(add(&ledger, "tick", EVENT_APPLIED, "t1", &applied).unwrap());
        assert!(!add(&ledger, "tick", EVENT_APPLIED, "t1", &applied).unwrap());
        let read_back: Applied = read(&ledger, EVENT_APPLIED, "t1").unwrap().unwrap();
        assert_eq!(read_back, applied);
        assert_eq!(
            read_back.dropped[0].line(),
            "change 2 of the Curator (add principles/NEW-1.md): it names one ticket"
        );
        let (_, last) = last_good(&ledger).unwrap().unwrap();
        assert_eq!(last.turn, "t1");

        let mut record = ProposalRecord {
            proposal: "proposal/2026-11-16-1".into(),
            state: OPEN.into(),
            ..ProposalRecord::default()
        };
        put(
            &ledger,
            "tick",
            EVENT_PROPOSAL,
            &record.proposal.clone(),
            &record,
        )
        .unwrap();
        record.state = CLOSED.into();
        put(
            &ledger,
            "tick",
            EVENT_PROPOSAL,
            &record.proposal.clone(),
            &record,
        )
        .unwrap();
        let back: ProposalRecord = read(&ledger, EVENT_PROPOSAL, "proposal/2026-11-16-1")
            .unwrap()
            .unwrap();
        assert_eq!(back.state, CLOSED);

        assert!(record_merged(&ledger, "DEMO-12", 7, Some("abc")).unwrap());
        assert!(!record_merged(&ledger, "DEMO-12", 7, Some("abc")).unwrap());
        let merged = ledger.latest_event(EVENT_MERGED).unwrap().unwrap();
        assert_eq!(
            merged.external_id.as_deref(),
            Some("ticket-merged:DEMO-12:7")
        );
        assert_eq!(merged.ticket.as_deref(), Some("DEMO-12"));
    }
}
