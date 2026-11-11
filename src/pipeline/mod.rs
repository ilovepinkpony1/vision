//! The pipeline (section 11.3): it moves each ticket through the steps of the adapter, and it
//! owns the ticket commands.
//!
//! - The tick hooks: [`test_end_facts`] (step 5), [`select_actions`] (step 6), [`set_flag`]
//!   (step 7), and [`launch_request`], which turns a step into the request of a turn.
//! - [`decide`] holds the decisions as pure functions; the hooks only gather the facts and
//!   apply the answer.
//! - [`needs`] gives the plans that wait for approval and the tickets that need the person;
//!   [`status_rows`] gives the rows of `vision status`.
//! - [`run`] is `vision pick`, `take`, `plan`, `approve-plan`, `log`, `diff`, `stop`, `retry`,
//!   and `report`.
//!
//! The core knows no project: the steps, the verbs, and the jobs are data of `adapter.toml`.

pub mod commands;
pub mod decide;
pub mod docs;
mod machine;
mod request;
pub mod state;
pub mod view;

use anyhow::Result;

use crate::cli::PipelineCommand;
use crate::ctx::Ctx;
use crate::ledger::Ledger;
use crate::snapshot::Need;
use crate::tick::{Action, FlagSet, Pass, TurnStart};
use crate::turn::LaunchRequest;

pub use commands::{
    hash_fits, money, render_report, report_of, step_order, timeline, LogEntry, Measures, Report,
    BY_HAND,
};
pub use machine::{crash_point, CRASH_CODE, CRASH_ENV};
pub use view::{plan_hash, TicketView, SHORT_HASH};

/// `vision pick`, `take`, `plan`, `approve-plan`, `log`, `diff`, `stop`, `retry`, and `report`.
pub fn run(ctx: &Ctx, command: PipelineCommand) -> Result<()> {
    match command {
        PipelineCommand::Pick { json } => commands::pick(ctx, json),
        PipelineCommand::Take { keys } => commands::take(ctx, &keys),
        PipelineCommand::Plan { key } => commands::plan(ctx, &key),
        PipelineCommand::ApprovePlan { key, hash } => commands::approve_plan(ctx, &key, &hash),
        PipelineCommand::Log { key, json } => commands::log(ctx, &key, json),
        PipelineCommand::Diff { key, json } => commands::diff(ctx, &key, json),
        PipelineCommand::Stop { key } => commands::stop(ctx, &key),
        PipelineCommand::Retry { key } => commands::retry(ctx, &key),
        PipelineCommand::Report { week, json } => commands::report(ctx, week, json),
    }
}

/// Tick step 5: for each ticket, read the facts of its step and test its end facts.
pub fn test_end_facts(pass: &mut Pass) -> Result<()> {
    machine::test_end_facts(pass)
}

/// Tick step 6: the next action of each ticket. A ticket with nothing to do has no action.
pub fn select_actions(pass: &mut Pass) -> Result<Vec<Action>> {
    machine::select_actions(pass)
}

/// Tick step 7 for [`Action::SetFlag`]: set the flag on the ticket.
pub fn set_flag(pass: &mut Pass, flag: &FlagSet) -> Result<()> {
    machine::set_flag(pass, flag)
}

/// The launch request of a turn step: the working folder, the facts of the step, the session
/// view, and the expected documents of `start`, for [`crate::turn::launch`].
pub fn launch_request(pass: &mut Pass, start: &TurnStart) -> Result<LaunchRequest> {
    request::build(pass.ctx, &pass.ledger, start)
}

/// The plans that wait for `vision approve-plan`, and the tickets with the flag "needs you"
/// or "paused".
pub fn needs(ctx: &Ctx, ledger: &Ledger) -> Result<Vec<Need>> {
    view::needs(ctx, ledger)
}

/// The rows of `vision status`, one for each ticket. `needs` are the needs of each module.
pub fn status_rows(ctx: &Ctx, ledger: &Ledger, needs: &[Need]) -> Result<Vec<TicketView>> {
    view::rows(ctx, ledger, needs)
}
