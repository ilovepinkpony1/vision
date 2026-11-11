//! The tick (section 6): one pass, with no daemon. A system timer runs `vision tick` each minute.
//!
//! 1. Take the lock. If a different tick holds it, exit with code 0.
//! 2. Write the heartbeat. If the `off` file exists, write the snapshot and exit.
//! 3. Reconcile each operation in the state `executing` or `uncertain`: [`executor::reconcile_operations`].
//! 4. Collect each turn that ended: [`turn::collect_ended_turns`].
//! 5. For each ticket, test the end fact of its step: [`pipeline::test_end_facts`].
//! 6. Select the next action of each ticket: [`pipeline::select_actions`].
//! 7. Do each action that its mode and the limits allow: [`act`].
//! 8. Write the snapshot. Release the lock.
//!
//! The steps 3 to 7 are hooks in the modules of later milestones. Each hook must be safe to run
//! two times, so that a crash at any line leaves a state that the next tick repairs.

use std::time::Instant;

use ::time::OffsetDateTime;
use anyhow::{bail, Context, Result};
use serde_json::Value;

use crate::ctx::Ctx;
use crate::ledger::Ledger;
use crate::lock::TickLock;
use crate::paths::TicketKey;
use crate::snapshot::{collect_needs, LimitsView, Outcome, Snapshot, TickReport, FORMAT};
use crate::turn::TurnKind;
use crate::util::fs::atomic_write;
use crate::util::time;
use crate::{curator, executor, pipeline, questions, turn};

/// What the steps 3 to 7 of one tick share.
#[derive(Debug)]
pub struct Pass<'a> {
    pub ctx: &'a Ctx,
    /// One connection for the whole pass. Use `ledger.conn_mut()` for a transaction.
    pub ledger: Ledger,
    /// The start of the tick, in UTC with whole seconds.
    pub now: OffsetDateTime,
}

/// The next action of one ticket, as step 6 selects it. "Nothing" is no action.
#[derive(Clone, Debug, PartialEq)]
pub enum Action {
    /// Start a turn: [`turn::start_turn`].
    StartTurn(TurnStart),
    /// Run a job of the adapter, such as the proof: [`executor::run_job`].
    RunJob(JobRun),
    /// Run an operation through the executor: [`executor::run_operation`].
    RunOperation(OperationRun),
    /// Set a flag on a ticket, such as "needs you": [`pipeline::set_flag`].
    SetFlag(FlagSet),
}

/// A turn to start (section 7).
#[derive(Clone, Debug, PartialEq)]
pub struct TurnStart {
    pub ticket: TicketKey,
    /// The step id of the adapter, such as `build`.
    pub step: String,
    /// The agent, such as `builder`.
    pub agent: String,
    pub kind: TurnKind,
    /// The session id that the turn resumes, or `None` for a new session.
    pub resumes: Option<String>,
    /// The `message` of `facts.json`, such as the answers to the cards, or `continue`.
    pub message: Option<String>,
}

/// A job of the adapter to run (section 11).
#[derive(Clone, Debug, PartialEq)]
pub struct JobRun {
    pub ticket: TicketKey,
    pub step: String,
    /// The job name, such as `proof`.
    pub job: String,
    pub args: Value,
}

/// An operation to run (section 12).
#[derive(Clone, Debug, PartialEq)]
pub struct OperationRun {
    pub ticket: TicketKey,
    /// The verb, such as `git.push`.
    pub verb: String,
    pub args: Value,
    /// The turn whose end fact led to the operation, if any.
    pub turn: Option<String>,
}

/// A flag to set on a ticket.
#[derive(Clone, Debug, PartialEq)]
pub struct FlagSet {
    pub ticket: TicketKey,
    /// The flag, such as `needs-you`.
    pub flag: String,
    /// Why, for the person.
    pub reason: String,
}

impl Action {
    pub fn ticket(&self) -> &TicketKey {
        match self {
            Action::StartTurn(a) => &a.ticket,
            Action::RunJob(a) => &a.ticket,
            Action::RunOperation(a) => &a.ticket,
            Action::SetFlag(a) => &a.ticket,
        }
    }
}

/// How a tick ended, when it did not fail.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TickOutcome {
    /// A different tick held the lock.
    Skipped,
    /// The `off` file was present.
    Off,
    /// Each step ran.
    Done,
}

/// `vision tick`: one pass, and one line of output.
pub fn command(ctx: &Ctx) -> Result<()> {
    let started = Instant::now();
    match run(ctx)? {
        TickOutcome::Skipped => println!("tick skipped: another tick holds the lock"),
        TickOutcome::Off => {
            println!("tick: Vision is off, so only the heartbeat and the snapshot were written. Run `vision on` to resume")
        }
        TickOutcome::Done => println!("tick done in {} ms", started.elapsed().as_millis()),
    }
    Ok(())
}

/// One pass of section 6.
pub fn run(ctx: &Ctx) -> Result<TickOutcome> {
    ctx.require_setup()?;

    // Step 1. The lock is released when `_lock` drops at the end of this function.
    let Some(_lock) = TickLock::try_acquire(&ctx.paths.lock())? else {
        ctx.log("tick skipped: another tick holds the lock");
        return Ok(TickOutcome::Skipped);
    };
    // A record of a job or an operation that a crash left: nothing runs now.
    crate::running::clear(&ctx.paths);
    let now = time::now();
    let mut report = TickReport {
        started: time::rfc3339(now),
        steps: vec!["lock".into()],
        ..TickReport::default()
    };

    // Step 2.
    atomic_write(
        &ctx.paths.heartbeat(),
        format!("{}\n", report.started).as_bytes(),
    )?;
    report.steps.push("heartbeat".into());
    let off = ctx.paths.off().exists();
    ctx.config()?;
    let ledger = ctx.ledger()?;

    // Steps 3 to 7.
    let (ledger, result) = if off {
        report.outcome = Outcome::Off;
        (ledger, Ok(()))
    } else {
        let mut pass = Pass { ctx, ledger, now };
        let result = run_steps(&mut pass, &mut report.steps);
        (pass.ledger, result)
    };
    if let Err(e) = &result {
        report.outcome = Outcome::Failed;
        report.error = Some(format!("{e:#}"));
        ctx.log(&format!("tick failed: {e:#}"));
    }

    // Step 8.
    report.steps.push("snapshot".into());
    let written = write_snapshot(ctx, &ledger, report, off);
    match (result, written) {
        (Err(e), Err(w)) => {
            ctx.log(&format!("tick could not write the snapshot: {w:#}"));
            Err(e)
        }
        (Err(e), Ok(())) | (Ok(()), Err(e)) => Err(e),
        (Ok(()), Ok(())) => Ok(if off {
            TickOutcome::Off
        } else {
            TickOutcome::Done
        }),
    }
}

fn run_steps(pass: &mut Pass, done: &mut Vec<String>) -> Result<()> {
    executor::reconcile_operations(pass).context("step 3, reconcile the operations")?;
    done.push("reconcile".into());
    turn::collect_ended_turns(pass).context("step 4, collect the ended turns")?;
    questions::tick(pass).context("step 4, the cards and the Judge")?;
    done.push("collect".into());
    pipeline::test_end_facts(pass).context("step 5, test the end facts")?;
    done.push("end-facts".into());
    let actions = pipeline::select_actions(pass).context("step 6, select the next actions")?;
    done.push("select".into());
    act(pass, actions).context("step 7, act")?;
    done.push("act".into());
    curator::tick(pass).context("step 7, the Curator")?;
    Ok(())
}

/// Step 7: hands each action to the module that owns it. The owner tests the mode of the verb
/// or the limits before it acts. The `off` file stops the remaining actions at once. A failed
/// action does not stop the actions of other tickets, but the tick then ends as failed.
pub fn act(pass: &mut Pass, actions: Vec<Action>) -> Result<()> {
    let mut failed = 0;
    for action in actions {
        if pass.ctx.paths.off().exists() {
            pass.ctx
                .log("tick: the off file appeared, so the remaining actions wait");
            break;
        }
        let result = match &action {
            Action::StartTurn(start) => turn::start_turn(pass, start),
            Action::RunJob(job) => executor::run_job(pass, job),
            Action::RunOperation(operation) => executor::run_operation(pass, operation),
            Action::SetFlag(flag) => pipeline::set_flag(pass, flag),
        };
        if let Err(e) = result {
            failed += 1;
            pass.ctx.log(&format!(
                "tick: an action of {} failed: {e:#}",
                action.ticket()
            ));
        }
    }
    if failed > 0 {
        bail!(
            "{failed} action(s) failed. See {}",
            pass.ctx.paths.log_file().display()
        );
    }
    Ok(())
}

fn write_snapshot(ctx: &Ctx, ledger: &Ledger, mut report: TickReport, off: bool) -> Result<()> {
    let config = ctx.config()?;
    report.ended = time::now_rfc3339();
    let snapshot = Snapshot {
        format: FORMAT,
        written: report.ended.clone(),
        off,
        limits: LimitsView::new(&config.limits, ledger.latest_limits()?),
        needs: collect_needs(ctx, ledger)?,
        tickets: ledger.tickets()?,
        tick: report,
    };
    snapshot.write(&ctx.paths.state())
}
