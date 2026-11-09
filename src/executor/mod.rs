//! The executor (section 12): verbs, modes, operations, and gates.
//!
//! Not built yet. A later milestone fills [`run`], the tick hooks [`reconcile_operations`],
//! [`run_job`], and [`run_operation`], and [`needs`]. It changes a mode with
//! [`crate::config::ConfigEditor::set_mode`], and a refused gate ends a command with
//! [`crate::exit::Stop::GateRefused`] (exit code 2).

use anyhow::Result;

use crate::cli::ExecutorCommand;
use crate::ctx::Ctx;
use crate::exit::not_built;
use crate::ledger::Ledger;
use crate::snapshot::Need;
use crate::tick::{JobRun, OperationRun, Pass};

/// `vision approve`, `promote`, and `demote`.
pub fn run(_ctx: &Ctx, command: ExecutorCommand) -> Result<()> {
    Err(not_built(command.name()))
}

/// Tick step 3: reconcile each operation in the state `executing` or `uncertain`.
/// An `uncertain` operation never runs again by itself.
pub fn reconcile_operations(_pass: &mut Pass) -> Result<()> {
    Ok(())
}

/// Tick step 7 for [`crate::tick::Action::RunJob`]: run one job of the adapter.
pub fn run_job(_pass: &mut Pass, _job: &JobRun) -> Result<()> {
    Ok(())
}

/// Tick step 7 for [`crate::tick::Action::RunOperation`]: record the operation as `pending`
/// with its idempotency key, test the gates, and run it when the mode of its verb allows.
pub fn run_operation(_pass: &mut Pass, _operation: &OperationRun) -> Result<()> {
    Ok(())
}

/// The operations in the mode ask that wait for `vision approve`.
pub fn needs(_ctx: &Ctx, _ledger: &Ledger) -> Result<Vec<Need>> {
    Ok(Vec::new())
}
