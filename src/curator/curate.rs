//! `vision curate`: run the Curator now. It starts a Curator turn (or finds the one that the
//! tick started), waits for it with a time limit, applies its result, and prints what waits
//! for review.
//!
//! The command takes the lock of the tick while it collects the ended turns and applies a
//! result, so that a tick never does the same work at the same time. It waits for the turn
//! without the lock.

use std::time::{Duration, Instant};

use anyhow::{bail, Result};

use super::records::{
    self, Applied, Failed, ProposalRecord, Started, EVENT_APPLIED, EVENT_FAILED, EVENT_PROPOSAL,
};
use super::runs::{self, Chain, Start};
use crate::ctx::Ctx;
use crate::exit::Stop;
use crate::ledger::Ledger;
use crate::lock::TickLock;
use crate::memory::Memory;
use crate::turn::{self, TurnKind};
use crate::util::time;

/// How long the command waits for the lock of the tick, which a tick holds while it applies a
/// result of the Curator with its replays.
const LOCK_WAIT: Duration = Duration::from_secs(45 * 60);

/// `vision curate`.
pub fn command(ctx: &Ctx) -> Result<()> {
    ctx.require_setup()?;
    let config = ctx.config()?;
    let ledger = ctx.ledger()?;
    let memory = Memory::from_ctx(ctx)?;
    let first = {
        let _lock = tick_lock(ctx)?;
        collect_and_apply(ctx, &ledger, &memory)?;
        match runs::start(ctx, &ledger, &memory, "vision curate", "user", time::now())? {
            Start::Started { turn, signals } => {
                println!(
                    "Started the Curator turn {turn} with {}.",
                    signals.describe()
                );
                Some(turn)
            }
            Start::Busy { turn } => {
                println!("A Curator turn already runs ({turn}), so this waits for it.");
                Some(turn)
            }
            Start::NoSignals { since } => {
                let since = since
                    .map(|s| format!(" since the last Curator run ({s})"))
                    .unwrap_or_default();
                println!("No signal{since}, so no Curator turn started.");
                None
            }
        }
    };
    if let Some(first) = first {
        let minutes = config.time.for_kind(TurnKind::Reader).saturating_mul(2);
        println!("Waiting for it, at most {minutes} minutes.");
        let limit = Duration::from_secs(minutes.saturating_mul(60) + 120);
        wait(ctx, &ledger, &memory, &first, limit)?;
    }
    print!("{}", waiting_text(&ledger, &memory)?);
    Ok(())
}

/// Takes the lock of the tick, and waits for it while a tick holds it.
fn tick_lock(ctx: &Ctx) -> Result<TickLock> {
    let start = Instant::now();
    let mut told = false;
    loop {
        if let Some(lock) = TickLock::try_acquire(&ctx.paths.lock())? {
            return Ok(lock);
        }
        if !told && start.elapsed() > Duration::from_secs(2) {
            eprintln!("Waiting for the tick to end...");
            told = true;
        }
        if start.elapsed() > LOCK_WAIT {
            bail!(
                "a tick held its lock ({}) for {} minutes. Try again later",
                ctx.paths.lock().display(),
                LOCK_WAIT.as_secs() / 60
            );
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// Collects the ended turns, as tick step 4 does, and applies each ended run of the Curator.
/// The lock of the tick must be held.
fn collect_and_apply(ctx: &Ctx, ledger: &Ledger, memory: &Memory) -> Result<()> {
    if let Err(e) = turn::collect(ctx, ledger, time::now()) {
        eprintln!("warning: {e:#}");
    }
    if let Err(e) = runs::apply_pending(ctx, ledger, memory) {
        eprintln!("warning: {e:#}");
    }
    Ok(())
}

/// Waits until the run of `first` is applied or failed.
fn wait(ctx: &Ctx, ledger: &Ledger, memory: &Memory, first: &str, limit: Duration) -> Result<()> {
    let deadline = Instant::now() + limit;
    let has_run = records::read::<Started>(ledger, records::EVENT_STARTED, first)?.is_some();
    loop {
        if let Some(applied) = records::read::<Applied>(ledger, EVENT_APPLIED, first)? {
            print!("{}", applied_text(ledger, &applied)?);
            return Ok(());
        }
        if let Some(failed) = records::read::<Failed>(ledger, EVENT_FAILED, first)? {
            eprintln!(
                "error: the Curator run {first} ended with no result to apply: {}",
                failed.reason
            );
            eprintln!("Its signals come again in the next run. Run `vision curate` again later.");
            return Err(Stop::Reported("curate".into()).into());
        }
        let chain = runs::chain(ctx, ledger, first)?;
        let current = chain.turn().to_string();
        let ended = !matches!(chain, Chain::Running(_)) || runs::turn_ended(ctx, &current);
        if ended {
            if !has_run && !matches!(chain, Chain::Running(_)) {
                println!("The Curator turn {current} ended.");
                return Ok(());
            }
            let _lock = tick_lock(ctx)?;
            collect_and_apply(ctx, ledger, memory)?;
            continue;
        }
        if Instant::now() > deadline {
            bail!(
                "the Curator turn {current} did not end in time. It goes on: the next tick applies its result, or run `vision curate` again"
            );
        }
        std::thread::sleep(Duration::from_millis(200));
    }
}

/// What one applied run did, for people.
fn applied_text(ledger: &Ledger, applied: &Applied) -> Result<String> {
    let mut out = format!("The Curator turn {} ended.", applied.last_turn);
    if !applied.summary.is_empty() {
        out.push_str(&format!(" Its summary: {}", applied.summary));
    }
    out.push('\n');
    if let Some(note) = &applied.note {
        out.push_str(&format!("{}.\n", capital(note)));
    } else if let Some(name) = &applied.proposal {
        out.push_str(&format!(
            "It proposes {} change(s) in {name}.\n",
            applied.kept
        ));
    }
    if let Some(name) = &applied.proposal {
        if let Some(record) = records::read::<ProposalRecord>(ledger, EVENT_PROPOSAL, name)? {
            for line in &record.lost {
                out.push_str(&format!("  lost: {line}\n"));
            }
        }
    }
    if !applied.dropped.is_empty() {
        out.push_str(&format!(
            "Dropped {} change(s) of the Curator:\n",
            applied.dropped.len()
        ));
        for dropped in &applied.dropped {
            out.push_str(&format!("  {}\n", dropped.line()));
        }
    }
    Ok(out)
}

fn capital(text: &str) -> String {
    let mut chars = text.chars();
    match chars.next() {
        Some(c) => c.to_uppercase().collect::<String>() + chars.as_str(),
        None => String::new(),
    }
}

/// The open proposals with their replay numbers, for people.
pub fn waiting_text(ledger: &Ledger, memory: &Memory) -> Result<String> {
    let open: Vec<_> = memory
        .proposals()?
        .into_iter()
        .filter(|p| p.open > 0)
        .collect();
    if open.is_empty() {
        return Ok("Nothing waits for review.\n".into());
    }
    let mut out = String::from("Waiting for review:\n");
    for info in &open {
        out.push_str(&format!(
            "  {}: {} open change(s). {}\n",
            info.name,
            info.open,
            crate::memory::store::short(&info.summary, 80)
        ));
        let record = records::read::<ProposalRecord>(ledger, EVENT_PROPOSAL, &info.name)?;
        if let Some(replays) = record.and_then(|r| r.replays) {
            match (&replays.today, &replays.proposed) {
                (Some(today), Some(proposed)) => out.push_str(&format!(
                    "    Replay of {} answers: today {}; proposed {}; {} card(s) changed side.\n",
                    replays.cards,
                    today.line(),
                    proposed.line(),
                    replays.changed().len()
                )),
                _ => {
                    for note in &replays.notes {
                        out.push_str(&format!("    Replay: {note}.\n"));
                    }
                }
            }
        }
    }
    out.push_str("Run `vision memory review` to accept, edit, or reject each change.\n");
    Ok(out)
}
