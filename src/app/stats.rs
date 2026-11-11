//! The data and the state of the screen Stats (`drawStats` of the design): the measures of
//! `vision report` for the last 7 days or for all time, the tickets that finished, and the cost
//! of each agent, which the report does not have.

use super::model::Fetch;

/// The numbers of one time window.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct StatsData {
    /// True for the last 7 days, false for all time.
    pub week: bool,
    pub cost_usd: f64,
    /// The tickets whose end step ran in the window.
    pub finished: usize,
    /// The questions that the memory or the Judge answered without the person.
    pub without_you: usize,
    /// Each question with an outcome.
    pub questions: usize,
    /// The minutes of the person, from the `human` rows.
    pub minutes: f64,
    /// The measures of each ticket, by key.
    pub tickets: Vec<TicketLine>,
    /// The time and the cost of each step of all tickets, in the order of the adapter.
    pub steps: Vec<StepBar>,
    /// The cost of each agent, in the order of the agents of Vision, then by name.
    pub agents: Vec<(String, f64)>,
    /// The cost of the turns that no ticket owns, such as the turns of the Curator, by agent.
    /// The cost of the window does not count them.
    pub outside: Vec<(String, f64)>,
}

/// The measures of one ticket.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct TicketLine {
    pub key: String,
    pub to_you: usize,
    pub by_memory: usize,
    pub by_judge: usize,
    pub minutes: f64,
    pub cost_usd: f64,
}

/// The time and the cost of one step.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct StepBar {
    pub step: String,
    pub seconds: i64,
    pub cost_usd: f64,
}

/// The screen Stats.
#[derive(Clone, Debug, PartialEq)]
pub struct StatsState {
    /// True for the last 7 days, false for all time.
    pub week: bool,
    pub data: Fetch<StatsData>,
}

impl Default for StatsState {
    fn default() -> Self {
        StatsState {
            week: true,
            data: Fetch::default(),
        }
    }
}
