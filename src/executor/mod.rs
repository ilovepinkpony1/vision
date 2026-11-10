//! The executor (section 12): the only door to the outside. It holds the secrets, and it alone
//! starts the scripts of the adapter.
//!
//! The library API that the pipeline builds on:
//!
//! - Facts: [`observe_queue`] and [`observe_ticket`] call `bin/observe` and give typed values;
//!   [`TicketFacts::watch_text`] gives the value of a `watch` path for `on`.
//! - Operations: [`request`] records an operation as `pending` with its idempotency key, tests
//!   the gates when the verb has `leaves`, and runs it when the mode of the verb allows. It
//!   gives an [`OperationResult`] whose [`Outcome`] says what happened and whether the person is
//!   needed ([`Outcome::needs_you`]). [`lookup`] reads the stored operation of a request without
//!   running anything. [`reconcile`] asks `act` again about each `executing` or `uncertain`
//!   operation. [`approve`] runs an operation that waits for the person.
//! - Modes: [`mode_of`] gives the mode of a verb, and [`change_mode`] moves it on the ladder.
//! - Jobs and reads: [`job`] runs a job and stores its checks and its documents, and
//!   [`job_passed`] says whether a job passed at a commit. [`read`] serves a read verb that a
//!   turn asked for, and gives its documents.
//! - The script runner is [`Runner`] with [`Secrets`]; the gates are the pure functions of
//!   [`gates`].
//!
//! Each call that changes state must run under the lock of the tick: inside a tick, or after
//! [`crate::lock::TickLock::try_acquire`], as `vision approve` does. A refused gate ends a
//! command with [`crate::exit::Stop::GateRefused`] (exit code 2).

mod clean;
mod commands;
mod context;
pub mod gates;
mod jobs;
mod observe;
mod ops;
mod scripts;

use anyhow::Result;

use crate::cli::ExecutorCommand;
use crate::ctx::Ctx;
use crate::ledger::Ledger;
use crate::snapshot::Need;
use crate::tick::{JobRun, OperationRun, Pass};

pub use clean::{Document, RESERVED_DOCUMENTS};
pub use context::{diff, short, TicketContext};
pub use jobs::{
    job, job_passed, job_source, read, write_documents, JobResult, ReadAnswer, JOB_CHECK,
};
pub use observe::{observe_queue, observe_ticket, CiFacts, PrFacts, QueueTicket, TicketFacts};
pub use ops::{
    approve, approve_command, change_mode, commit_arg, demoted, idempotency_key, lookup, mode_of,
    operation, operations_of, promoted, reconcile, release_failed, request, OpState,
    OperationResult, Outcome, SetValues, APPROVED_PLAN, LADDER,
};
pub use scripts::{Answer, Runner, Script, ScriptError, Secrets, DEFAULT_TIMEOUT, PASSED_ENV};

/// `vision approve`, `promote`, and `demote`.
pub fn run(ctx: &Ctx, command: ExecutorCommand) -> Result<()> {
    ctx.require_setup()?;
    match command {
        ExecutorCommand::Approve { key, verb, commit } => {
            commands::approve(ctx, &key, &verb, &commit)
        }
        ExecutorCommand::Promote { verb } => commands::ladder(ctx, &verb, true),
        ExecutorCommand::Demote { verb } => commands::ladder(ctx, &verb, false),
    }
}

/// Tick step 3: reconcile each operation in the state `executing` or `uncertain`.
/// An `uncertain` operation never runs again by itself.
pub fn reconcile_operations(pass: &mut Pass) -> Result<()> {
    reconcile(pass.ctx, &pass.ledger)?;
    Ok(())
}

/// Tick step 7 for [`crate::tick::Action::RunJob`]: run one job of the adapter, and store its
/// checks and its documents. A job that does not pass is a result, not an error.
pub fn run_job(pass: &mut Pass, run: &JobRun) -> Result<()> {
    job(pass.ctx, &pass.ledger, run)?;
    Ok(())
}

/// Tick step 7 for [`crate::tick::Action::RunOperation`]: record the operation as `pending`
/// with its idempotency key, test the gates, and run it when the mode of its verb allows.
/// A refused, held, or failed operation is a result, which the pipeline reads with [`lookup`].
pub fn run_operation(pass: &mut Pass, operation: &OperationRun) -> Result<()> {
    request(pass.ctx, &pass.ledger, operation)?;
    Ok(())
}

/// The operations that wait for `vision approve`. `Need.id` is `<verb> <commit>`.
pub fn needs(_ctx: &Ctx, ledger: &Ledger) -> Result<Vec<Need>> {
    ops::needs(ledger)
}
