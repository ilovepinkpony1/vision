//! The pipeline (section 11): steps, states, and end facts.
//!
//! Not built yet. A later milestone fills [`run`], the tick hooks [`test_end_facts`],
//! [`select_actions`], and [`set_flag`], and [`needs`].

use anyhow::Result;

use crate::cli::PipelineCommand;
use crate::ctx::Ctx;
use crate::exit::not_built;
use crate::ledger::Ledger;
use crate::snapshot::Need;
use crate::tick::{Action, FlagSet, Pass, TurnStart};
use crate::turn::LaunchRequest;

/// `vision pick`, `take`, `plan`, `approve-plan`, `log`, `diff`, `stop`, `retry`, and `report`.
pub fn run(_ctx: &Ctx, command: PipelineCommand) -> Result<()> {
    Err(not_built(command.name()))
}

/// Tick step 5: for each ticket, read the facts of its step and test its end fact.
pub fn test_end_facts(_pass: &mut Pass) -> Result<()> {
    Ok(())
}

/// Tick step 6: the next action of each ticket. A ticket with nothing to do has no action.
pub fn select_actions(_pass: &mut Pass) -> Result<Vec<Action>> {
    Ok(Vec::new())
}

/// Tick step 7 for [`Action::SetFlag`]: set the flag on the ticket.
pub fn set_flag(_pass: &mut Pass, _flag: &FlagSet) -> Result<()> {
    Ok(())
}

/// The launch request of a turn step: the working folder, the facts of the step, the session
/// view, and the expected documents of `start`, for [`crate::turn::launch`].
pub fn launch_request(_pass: &mut Pass, start: &TurnStart) -> Result<LaunchRequest> {
    Err(not_built(&format!(
        "the launch request of the step {}",
        start.step
    )))
}

/// The plans that wait for `vision approve-plan`, and the tickets with the flag "needs you".
pub fn needs(_ctx: &Ctx, _ledger: &Ledger) -> Result<Vec<Need>> {
    Ok(Vec::new())
}
