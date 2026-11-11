//! The model of the live view of one ticket: the data that its loader reads each second, the
//! activity feed, the one-line input of `t`, and the pagers of `l`, `p`, and `d`. Plain data,
//! as the rest of the model.

use super::feed::Feed;
use super::line::Line;
use super::model::Fetch;
use super::pager::Pager;
use crate::pipeline::commands::{LogEntry, Measures};
use crate::pipeline::facts::StateFacts;
use crate::pipeline::TicketView;
use crate::snapshot::Need;

/// The live view of one ticket.
#[derive(Clone, Debug, PartialEq)]
pub struct Live {
    pub key: String,
    pub data: Fetch<TicketData>,
    pub feed: Feed,
    /// The line of `t`, while it is open.
    pub tell: Option<Line>,
    /// The pagers over the view, the top one last. Esc closes the top one.
    pub pagers: Vec<Pager>,
}

impl Live {
    pub fn new(key: &str) -> Live {
        Live {
            key: key.to_string(),
            data: Fetch::default(),
            feed: Feed::default(),
            tell: None,
            pagers: Vec::new(),
        }
    }
}

/// What the loader of the live view reads: the row of the status view, the facts of the state,
/// the steps with their measures, the team, the timeline, and the commits that the forge does
/// not have.
#[derive(Clone, Debug, PartialEq)]
pub struct TicketData {
    pub view: TicketView,
    pub facts: StateFacts,
    /// When Vision took it, RFC 3339 UTC.
    pub taken: Option<String>,
    /// The steps of the adapter, in order, with the measures of this ticket.
    pub steps: Vec<StepLine>,
    /// The agents of the model table, in order.
    pub team: Vec<Member>,
    /// The timeline of the ticket from the ledger, oldest first.
    pub timeline: Vec<LogEntry>,
    pub measures: Measures,
    /// The commits of the branch that the forge does not have, by the refs of the clone.
    pub unpushed: Option<usize>,
    /// The short hash of `plan.md`, when the plan step wrote it.
    pub plan: Option<String>,
    /// The jobs that an end fact of a step of the adapter names, such as `proof`: their marks
    /// show a dash before the first run.
    pub jobs: Vec<String>,
    /// The needs of this ticket.
    pub needs: Vec<Need>,
}

/// One step of the adapter with the measures of the ticket.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct StepLine {
    pub id: String,
    /// The kind of the step: turn, job, human, wait, or end.
    pub kind: String,
    /// The entries of the ticket into the step.
    pub entries: u32,
    /// The seconds that the ticket spent in the step.
    pub seconds: i64,
    /// The cost of the turns of the step.
    pub cost_usd: f64,
}

/// One agent of the model table.
#[derive(Clone, Debug, PartialEq)]
pub struct Member {
    pub agent: String,
    pub model: String,
    pub effort: String,
}
