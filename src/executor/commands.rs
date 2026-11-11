//! `vision approve KEY VERB COMMIT`, `vision promote VERB`, and `vision demote VERB`.

use std::time::{Duration, Instant};

use anyhow::{bail, Result};

use super::context::short;
use super::ops::{self, OperationResult, Outcome};
use crate::config::Mode;
use crate::ctx::Ctx;
use crate::exit::Stop;
use crate::lock::TickLock;
use crate::paths::TicketKey;

/// How long `vision approve` waits for a running tick to release the lock.
const LOCK_WAIT: Duration = Duration::from_secs(30);

/// `vision approve`: runs the operations that wait at that commit, or only the operation of
/// `--op`, under the lock of the tick, and prints one line for each. A refused gate exits with code 2, and a failed or uncertain
/// operation with code 1.
pub fn approve(
    ctx: &Ctx,
    key: &TicketKey,
    verb: &str,
    commit: &str,
    only: Option<&str>,
) -> Result<()> {
    let _lock = wait_for_lock(ctx)?;
    let ledger = ctx.ledger()?;
    let results = ops::approve(ctx, &ledger, key, verb, commit, only)?;
    let mut refused = None;
    let mut failed = 0;
    for result in &results {
        println!("{}", line(result));
        match &result.outcome {
            Outcome::Refused { .. } => refused = Some(result.detail.clone()),
            Outcome::Failed | Outcome::Uncertain => failed += 1,
            _ => {}
        }
    }
    if let Some(reason) = refused {
        return Err(Stop::GateRefused(reason).into());
    }
    if failed > 0 {
        return Err(Stop::Reported(format!("{failed} operation(s) did not succeed")).into());
    }
    Ok(())
}

/// `vision promote VERB` and `vision demote VERB`.
pub fn ladder(ctx: &Ctx, verb: &str, up: bool) -> Result<()> {
    let ledger = ctx.ledger()?;
    let (old, new) = ops::change_mode(ctx, &ledger, verb, up)?;
    let what = match new {
        Mode::Auto => format!("each {verb} now runs without your approval, behind its gates"),
        Mode::Ask => format!("each {verb} now waits for `vision approve`"),
        Mode::Shadow => format!("each {verb} is now recorded, and nothing is called"),
        Mode::Hold => format!("each {verb} is now held, and nothing is called"),
    };
    println!("{verb}: {old} -> {new}. {}", capitalize(&what));
    Ok(())
}

fn line(result: &OperationResult) -> String {
    let mut text = format!(
        "{} of {} at {}: {}",
        result.verb,
        result.ticket,
        short(&result.head),
        result.outcome
    );
    if let Some(id) = &result.external_id {
        text.push_str(&format!(" ({id})"));
    }
    if !result.detail.is_empty() {
        text.push_str(&format!(". {}", result.detail));
    }
    text
}

fn capitalize(text: &str) -> String {
    let mut chars = text.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
}

fn wait_for_lock(ctx: &Ctx) -> Result<TickLock> {
    let deadline = Instant::now() + LOCK_WAIT;
    loop {
        if let Some(lock) = TickLock::try_acquire(&ctx.paths.lock())? {
            return Ok(lock);
        }
        if Instant::now() >= deadline {
            bail!(
                "a tick runs now and holds the lock {}. Run the command again in a minute",
                ctx.paths.lock().display()
            );
        }
        std::thread::sleep(Duration::from_millis(200));
    }
}
