//! The Curator (section 9.4): signals, proposals, replay, and review.
//!
//! The library API that later milestones build on:
//!
//! - [`tick`] is the hook of the tick. `tick.rs` calls it at the end of step 7, after the
//!   actions of the tickets, so that a Curator that fails never holds a ticket back. It moves
//!   the replays of each open proposal one step on ([`runs::advance_replays`]), applies the
//!   result of each Curator turn that the collector collected in step 4 ([`runs::apply`]: the
//!   checks, one proposal branch, the rule "no line is lost", and the start of the first
//!   replay), and it starts a Curator turn when one is due: a ticket merged since the last
//!   run started, or 7 days passed since the last good run and the signals are not empty.
//!   It never runs two Curator turns at the same time, and it never waits for a replay.
//! - [`record_merged`] records the event that makes a run due: the pipeline calls it when a
//!   ticket reaches its end with a merged pull request. The event has the kind
//!   [`EVENT_MERGED`] (`ticket-merged`), the ticket key, and the external id
//!   `ticket-merged:<KEY>:<PR>`.
//! - [`needs`] gives each open proposal, for the snapshot.
//! - [`run`] is `vision curate`, [`review`] is `vision memory review`, and [`eval`] is
//!   `vision model eval`. [`last_eval_line`] gives the line of `vision model recommend` about
//!   the last evaluation.
//! - [`signals::collect`] collects the signals, and [`check::check`] is the pure test of one
//!   change of the Curator.

pub mod check;
mod curate;
pub mod eval;
pub mod records;
pub mod replays;
pub mod review;
pub mod runs;
pub mod signals;

use anyhow::Result;

use crate::cli::ReviewArgs;
use crate::ctx::Ctx;
use crate::ledger::Ledger;
use crate::memory::store::short;
use crate::memory::Memory;
use crate::snapshot::{Need, NeedKind};
use crate::tick::Pass;

pub use curate::waiting_text;
pub use records::{record_merged, EVENT_EVAL, EVENT_MERGED};
pub use review::review_lines;
pub use runs::{CURATOR, STEP};
pub use signals::{Mark, Signals};

/// `vision curate`: run the Curator now.
pub fn run(ctx: &Ctx) -> Result<()> {
    curate::command(ctx)
}

/// `vision memory review`.
pub fn review(ctx: &Ctx, args: ReviewArgs) -> Result<()> {
    review::command(ctx, args)
}

/// `vision model eval AGENT --models A,B [--n N]`.
pub fn eval(ctx: &Ctx, agent: &str, models: &[String], n: Option<u32>, json: bool) -> Result<()> {
    eval::command(ctx, agent, models, n, json)
}

/// The hook of the tick. See the module text.
pub fn tick(pass: &mut Pass) -> Result<()> {
    runs::tick(pass)
}

/// The line of `vision model recommend` about the last evaluation of an agent.
pub fn last_eval_line(ledger: &Ledger, agent: &str) -> Result<Option<String>> {
    eval::last_line(ledger, agent)
}

/// Each proposal with an open change: it waits for `vision memory review`. A memory that
/// cannot be read gives no need, so that the snapshot is still written.
pub fn needs(ctx: &Ctx, ledger: &Ledger) -> Result<Vec<Need>> {
    let Ok(memory) = Memory::from_ctx(ctx) else {
        return Ok(Vec::new());
    };
    let Ok(proposals) = memory.proposals() else {
        return Ok(Vec::new());
    };
    let replaying: Vec<String> = runs::replaying(ledger)
        .unwrap_or_default()
        .into_iter()
        .map(|r| r.proposal)
        .collect();
    Ok(proposals
        .into_iter()
        .filter(|p| p.problem.is_none() && p.open > 0)
        .map(|p| Need {
            kind: NeedKind::Proposal,
            ticket: None,
            text: format!(
                "the Curator proposes {} change(s): {}{}",
                p.open,
                short(&p.summary, 80),
                if replaying.contains(&p.name) {
                    " (its replays still run)"
                } else {
                    ""
                }
            ),
            id: p.name,
        })
        .collect())
}
