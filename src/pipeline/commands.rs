//! The ticket commands: `vision pick`, `take`, `plan`, `approve-plan`, `log`, `diff`, `stop`,
//! `retry`, and `report`. A command that changes the state of a ticket takes the lock of the
//! tick first, as `vision approve` does, and waits up to 30 seconds for a running tick.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use serde::Serialize;
use serde_json::{json, Value};

use super::machine::{dirty_lines, head_of, human_mark, in_tx, span, visit_turns, Ticket};
use super::state::{flags_text, Counts, FlagReason, NEEDS_YOU, PAUSED, STOPPED};
use super::view::{plan_hash, SHORT_HASH};
use crate::adapter::{self, Adapter, StepKind};
use crate::config::Config;
use crate::ctx::Ctx;
use crate::executor::{self, short, OpState, QueueTicket};
use crate::exit::Stop;
use crate::ledger::questions::HumanRow;
use crate::ledger::{Event, Ledger, TicketRow};
use crate::lock::TickLock;
use crate::paths::TicketKey;
use crate::util::fs::atomic_write;
use crate::util::{git, time};

/// How long a command waits for a running tick to release the lock.
const LOCK_WAIT: Duration = Duration::from_secs(30);

/// The message of the first turn after the person worked in the session with `vision open`.
pub const BY_HAND: &str = "The person worked in this session by hand with `vision open`. Read the state of the clone first (git status, git log, and the files that changed), then continue the step from there.";

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

fn ticket_of(ledger: &Ledger, key: &TicketKey) -> Result<Ticket> {
    let Some(row) = ledger.ticket(key.as_str())? else {
        bail!("Vision has no ticket {key}. Run `vision take {key}` to start it");
    };
    if row.state.as_deref().is_none_or(str::is_empty) {
        bail!("Vision has no ticket {key}. Run `vision take {key}` to start it");
    }
    Ticket::of(row)
}

fn seconds_since(at: &str) -> Option<i64> {
    time::parse_rfc3339(at)
        .ok()
        .map(|at| time::seconds_between(at, time::now()).max(0))
}

// ----- pick ------------------------------------------------------------------------------

/// One ticket of `vision pick`.
#[derive(Clone, Debug, Serialize)]
pub struct PickRow {
    pub key: String,
    pub title: String,
    pub status: String,
    pub eligible: bool,
    pub reason: String,
    /// The step of a ticket that Vision already holds.
    pub taken: Option<String>,
}

/// `vision pick [--json]`.
pub fn pick(ctx: &Ctx, json: bool) -> Result<()> {
    ctx.require_setup()?;
    let ledger = ctx.ledger()?;
    let queue = executor::observe_queue(ctx)?;
    let held = held_tickets(&ledger)?;
    let rows: Vec<PickRow> = queue
        .into_iter()
        .map(|t| PickRow {
            taken: held.get(t.key.as_str()).cloned(),
            key: t.key.to_string(),
            title: t.title,
            status: t.status,
            eligible: t.eligible,
            reason: t.reason,
        })
        .collect();
    if json {
        println!("{}", serde_json::to_string_pretty(&rows)?);
        return Ok(());
    }
    if rows.is_empty() {
        println!("The queue of the tracker is empty.");
        return Ok(());
    }
    let mut table = vec![["KEY", "STATUS", "TAKE", "TITLE"].map(String::from)];
    for row in &rows {
        let take = match (&row.taken, row.eligible) {
            (Some(step), _) => format!("taken ({step})"),
            (None, true) => "yes".into(),
            (None, false) => format!("no: {}", row.reason),
        };
        table.push([row.key.clone(), row.status.clone(), take, row.title.clone()]);
    }
    print!("{}", render_table(&table));
    if rows.iter().any(|r| r.eligible && r.taken.is_none()) {
        println!("\nStart one with `vision take KEY`.");
    }
    Ok(())
}

/// The tickets that Vision holds: a row with a step that is not finished, by key, with the
/// step (and `stopped` when it is stopped).
fn held_tickets(ledger: &Ledger) -> Result<BTreeMap<String, String>> {
    let mut out = BTreeMap::new();
    for ticket in super::machine::active_tickets(ledger)? {
        let step = if ticket.flags.iter().any(|f| f == STOPPED) {
            format!("{}, stopped", ticket.state())
        } else {
            ticket.state().to_string()
        };
        out.insert(ticket.key.to_string(), step);
    }
    Ok(out)
}

/// A table with aligned columns.
pub fn render_table<const N: usize>(rows: &[[String; N]]) -> String {
    let widths: Vec<usize> = (0..N)
        .map(|i| rows.iter().map(|r| r[i].chars().count()).max().unwrap_or(0))
        .collect();
    let mut out = String::new();
    for row in rows {
        let cells: Vec<String> = row
            .iter()
            .zip(&widths)
            .map(|(cell, width)| format!("{cell:<width$}"))
            .collect();
        let _ = writeln!(out, "{}", cells.join("  ").trim_end());
    }
    out
}

// ----- take ------------------------------------------------------------------------------

/// `vision take KEY...`.
pub fn take(ctx: &Ctx, keys: &[TicketKey]) -> Result<()> {
    ctx.require_setup()?;
    let _lock = wait_for_lock(ctx)?;
    let ledger = ctx.ledger()?;
    let config = ctx.config()?;
    let adapter = adapter::load_with(config, &ctx.paths)?;
    let mut queue: Option<Vec<QueueTicket>> = None;
    let mut failed = Vec::new();
    for key in keys {
        match take_one(ctx, &ledger, config, &adapter, &mut queue, key) {
            Ok(line) => println!("{line}"),
            Err(e) => {
                eprintln!("error: {key}: {e:#}");
                failed.push(key.to_string());
            }
        }
    }
    if !failed.is_empty() {
        return Err(Stop::Reported(format!("could not take {}", failed.join(", "))).into());
    }
    Ok(())
}

fn take_one(
    ctx: &Ctx,
    ledger: &Ledger,
    config: &Config,
    adapter: &Adapter,
    queue: &mut Option<Vec<QueueTicket>>,
    key: &TicketKey,
) -> Result<String> {
    if key.is_reserved() {
        bail!("{key} is a reserved key: Vision uses it for turns with no ticket. Take a ticket of the tracker");
    }
    if let Some(row) = ledger.ticket(key.as_str())? {
        let ticket = Ticket::of(row)?;
        if !ticket.state().is_empty() && ticket.counts.finished.is_none() {
            if ticket.flags.iter().any(|f| f == STOPPED) {
                return restart(ctx, ledger, adapter, ticket);
            }
            bail!(
                "Vision already holds {key}: it is at the step {}. `vision status` shows it",
                ticket.state()
            );
        }
    }
    if queue.is_none() {
        *queue = Some(executor::observe_queue(ctx)?);
    }
    let found = queue
        .as_ref()
        .and_then(|q| q.iter().find(|t| t.key == *key))
        .cloned();
    let Some(found) = found else {
        bail!("the queue of the tracker has no ticket {key}. `vision pick` lists the queue");
    };
    if !found.eligible {
        bail!(
            "{key} is not eligible: {}. `vision pick` lists the eligible tickets",
            if found.reason.is_empty() {
                "the tracker gave no reason"
            } else {
                &found.reason
            }
        );
    }
    let clone = free_clone(ledger, config)?;
    let clone_text = clone.display().to_string();
    git::run(&clone, &["fetch", "--quiet", "--all", "--prune"])
        .with_context(|| format!("could not fetch in the clone {clone_text}"))?;
    let base = &adapter.base;
    if git::run(
        &clone,
        &[
            "rev-parse",
            "--verify",
            "--quiet",
            &format!("{base}^{{commit}}"),
        ],
    )
    .is_err()
    {
        bail!(
            "the base ref {base} of the adapter is missing in the clone {clone_text}. Fetch it there, or fix base in adapter.toml"
        );
    }
    let branch = adapter.branch_for(key);
    git::run(
        &clone,
        &["switch", "--quiet", "--no-track", "-C", &branch, base],
    )
    .with_context(|| {
        format!("could not make the branch {branch} from {base} in the clone {clone_text}")
    })?;
    let head = head_of(&clone)?;
    let entry = adapter.entry().clone();
    let now = time::now_rfc3339();
    let turns = u32::try_from(ledger.step_turn_ids(key.as_str(), &entry.id)?.len())?;
    let mut counts = Counts::default();
    let visit = counts.enter(&entry, "take", &now, turns, Some(head.clone()), true);
    let row = TicketRow {
        key: key.to_string(),
        state: Some(entry.id.clone()),
        flags: Some(flags_text(&[])),
        clone: Some(clone_text.clone()),
        branch: Some(branch.clone()),
        pr: None,
        head: Some(head.clone()),
        taken: Some(now.clone()),
        updated: Some(now.clone()),
        counts: Some(counts.text()),
    };
    let dir = ctx.paths.ticket_dir(key);
    std::fs::create_dir_all(&dir).with_context(|| format!("could not make {}", dir.display()))?;
    let ticket = Ticket::of(row.clone())?;
    in_tx(ledger, |ledger| {
        ledger.put_ticket(&row)?;
        ledger.close_spans(key.as_str(), &now)?;
        ledger.open_span(&span(&ticket, &entry, "take", &now))?;
        ledger.add_event(
            &Event::now("user", "take")
                .ticket(key.as_str())
                .detail(json!({"clone": clone_text, "branch": branch, "base": head, "step": entry.id, "seq": visit.seq})),
        )?;
        Ok(())
    })?;
    ctx.log(&format!(
        "take: {key} in {clone_text} on {branch} from {base} ({})",
        short(&head)
    ));
    Ok(format!(
        "Took {key} in the clone {clone_text} on the branch {branch} from {base} ({}). The next tick starts the step {}.",
        short(&head),
        entry.id
    ))
}

/// The tickets that hold a clone: a row with a step that is not finished and not stopped.
fn clone_holders(ledger: &Ledger) -> Result<Vec<(PathBuf, String, String)>> {
    let mut out = Vec::new();
    for ticket in super::machine::active_tickets(ledger)? {
        if ticket.flags.iter().any(|f| f == STOPPED) {
            continue;
        }
        if let Ok(dir) = ticket.clone_dir() {
            out.push((dir, ticket.key.to_string(), ticket.state().to_string()));
        }
    }
    Ok(out)
}

fn same_dir(a: &Path, b: &Path) -> bool {
    a == b
        || match (a.canonicalize(), b.canonicalize()) {
            (Ok(a), Ok(b)) => a == b,
            _ => false,
        }
}

/// The first clone of `clones` that no ticket holds and whose work tree is clean.
fn free_clone(ledger: &Ledger, config: &Config) -> Result<PathBuf> {
    let clones = config.clone_paths()?;
    if clones.is_empty() {
        bail!(
            "config.toml lists no clone in `clones`. Make a clone of the project that only Vision uses, and add its absolute path to clones"
        );
    }
    let holders = clone_holders(ledger)?;
    let mut why = Vec::new();
    for clone in clones {
        if let Some((_, key, step)) = holders.iter().find(|(dir, _, _)| same_dir(dir, &clone)) {
            why.push(format!("{}: {key} holds it (step {step})", clone.display()));
            continue;
        }
        if !clone.is_dir() || !git::is_repo_root(&clone) {
            why.push(format!(
                "{}: it is not the top folder of a git clone",
                clone.display()
            ));
            continue;
        }
        let dirty = dirty_lines(&clone)?;
        if !dirty.is_empty() {
            why.push(format!(
                "{}: its work tree has {} change(s), such as `{}`. Commit or clean them",
                clone.display(),
                dirty.len(),
                dirty[0].trim()
            ));
            continue;
        }
        return Ok(clone);
    }
    bail!(
        "no clone is free:\n  {}\nAdd a clone to `clones` in config.toml, or wait until a ticket ends",
        why.join("\n  ")
    )
}

/// `vision take` of a stopped ticket: it starts again at the same step, in the clone that holds
/// its kept branch.
fn restart(ctx: &Ctx, ledger: &Ledger, adapter: &Adapter, mut ticket: Ticket) -> Result<String> {
    let key = ticket.key.clone();
    let clone = ticket.clone_dir().with_context(|| {
        format!("{key} is stopped, but its row names no clone that holds its branch")
    })?;
    if let Some((_, other, step)) = clone_holders(ledger)?
        .into_iter()
        .find(|(dir, k, _)| *k != key.as_str() && same_dir(dir, &clone))
    {
        bail!(
            "the clone {} holds the kept branch of {key}, but {other} holds that clone now (step {step}). Wait for {other} to end, or run `vision stop {other}`",
            clone.display()
        );
    }
    let branch = ticket
        .row
        .branch
        .clone()
        .unwrap_or_else(|| adapter.branch_for(&key));
    let current = git::run(&clone, &["rev-parse", "--abbrev-ref", "HEAD"]).unwrap_or_default();
    if current != branch {
        let dirty = dirty_lines(&clone)?;
        if !dirty.is_empty() {
            bail!(
                "the clone {} has changes in its work tree, so the branch {branch} cannot come back. Commit or clean them first",
                clone.display()
            );
        }
        git::run(&clone, &["switch", "--quiet", &branch]).with_context(|| {
            format!(
                "the branch {branch} of {key} is gone from the clone {}",
                clone.display()
            )
        })?;
    }
    let Some(step) = adapter.step(ticket.state()).cloned() else {
        bail!(
            "{key} stopped at the step {}, which is not in the adapter {} now",
            ticket.state(),
            adapter.name
        );
    };
    let now = time::now_rfc3339();
    ticket.flags.retain(|f| f != STOPPED);
    ticket.counts.reasons.remove(STOPPED);
    let turns = u32::try_from(ledger.step_turn_ids(key.as_str(), &step.id)?.len())?;
    let head = head_of(&clone).ok();
    let visit = ticket
        .counts
        .enter(&step, "restart", &now, turns, head, false);
    in_tx(ledger, |ledger| {
        ledger.set_ticket_state(
            key.as_str(),
            &step.id,
            &flags_text(&ticket.flags),
            &ticket.counts.text(),
            &now,
        )?;
        ledger.close_spans(key.as_str(), &now)?;
        ledger.open_span(&span(&ticket, &step, "restart", &now))?;
        ledger.add_event(
            &Event::now("user", "take")
                .ticket(key.as_str())
                .detail(json!({"restart": true, "step": step.id, "seq": visit.seq})),
        )?;
        Ok(())
    })?;
    ctx.log(&format!("take: {key} starts again at the step {}", step.id));
    Ok(format!(
        "{key} starts again at the step {} in the clone {} on the branch {branch}.",
        step.id,
        clone.display()
    ))
}

// ----- plan and approve-plan -------------------------------------------------------------

/// `vision plan KEY`.
pub fn plan(ctx: &Ctx, key: &TicketKey) -> Result<()> {
    ctx.require_setup()?;
    let path = ctx.paths.ticket_doc(key, "plan.md");
    let Ok(bytes) = std::fs::read(&path) else {
        bail!("{key} has no plan yet: the plan step writes plan.md. `vision status` shows where the ticket is");
    };
    let hash = plan_hash(&bytes);
    let text = String::from_utf8_lossy(&bytes);
    println!(
        "The plan of {key}, hash {} (in full: {hash}):\n",
        &hash[..SHORT_HASH]
    );
    print!("{text}");
    if !text.ends_with('\n') {
        println!();
    }
    if let Ok(agent) = std::fs::read(ctx.paths.ticket_doc(key, "plan.agent.md")) {
        if agent != bytes {
            println!("\nYou edited plan.md after the agent wrote it. The first text of the agent stays in plan.agent.md.");
        }
    }
    let ledger = ctx.ledger()?;
    if let Some(row) = ledger.ticket(key.as_str())? {
        let visit = Ticket::of(row)?.visit();
        if visit.action.as_deref() == Some("approve-plan") {
            println!(
                "\nApprove it with: vision approve-plan {key} {}",
                &hash[..SHORT_HASH]
            );
        }
    }
    Ok(())
}

/// True when `given` is the full hash, or a prefix of at least 7 of its characters.
pub fn hash_fits(full: &str, given: &str) -> bool {
    let given = given.trim().to_ascii_lowercase();
    given.len() >= 7 && given.bytes().all(|b| b.is_ascii_hexdigit()) && full.starts_with(&given)
}

/// `vision approve-plan KEY HASH`.
pub fn approve_plan(ctx: &Ctx, key: &TicketKey, given: &str) -> Result<()> {
    ctx.require_setup()?;
    let _lock = wait_for_lock(ctx)?;
    let ledger = ctx.ledger()?;
    let ticket = ticket_of(&ledger, key)?;
    let visit = ticket.visit();
    if ticket.flags.iter().any(|f| f == STOPPED) {
        bail!("{key} is stopped. Run `vision take {key}` to start it again first");
    }
    if visit.kind != "human" || visit.action.as_deref() != Some("approve-plan") {
        bail!(
            "{key} is at the step {}, so no plan of it waits for your approval",
            ticket.state()
        );
    }
    let mark = human_mark(key, visit.seq, "approve-plan");
    if ledger.event_detail(&mark)?.is_some() {
        println!("The plan of {key} is already approved. The next tick moves the ticket on.");
        return Ok(());
    }
    let Ok(bytes) = std::fs::read(ctx.paths.ticket_doc(key, "plan.md")) else {
        bail!("{key} has no plan.md, so there is nothing to approve");
    };
    let full = plan_hash(&bytes);
    if !hash_fits(&full, given) {
        bail!(
            "the hash {} does not fit the plan of now, whose hash is {}. The plan changed, or the hash is wrong. Run `vision plan {key}` and approve the hash that it shows",
            given.trim(),
            &full[..SHORT_HASH]
        );
    }
    atomic_write(&ctx.paths.ticket_doc(key, "approved-plan.md"), &bytes)?;
    let now = time::now_rfc3339();
    in_tx(&ledger, |ledger| {
        ledger.add_human(&HumanRow {
            at: now.clone(),
            ticket: Some(key.to_string()),
            kind: "approve-plan".into(),
            object: full.clone(),
            value: Some(full[..SHORT_HASH].to_string()),
            reason: None,
            seconds: seconds_since(&visit.at),
        })?;
        ledger.add_event(
            &Event::now("user", "human")
                .ticket(key.as_str())
                .external_id(&mark)
                .detail(json!({"action": "approve-plan", "hash": full, "step": visit.step})),
        )?;
        Ok(())
    })?;
    ctx.log(&format!("approve-plan: {key} {}", &full[..SHORT_HASH]));
    println!(
        "Approved the plan {} of {key}: approved-plan.md holds it. The next tick moves the ticket on.",
        &full[..SHORT_HASH]
    );
    Ok(())
}

// ----- stop and retry --------------------------------------------------------------------

/// `vision stop KEY`.
pub fn stop(ctx: &Ctx, key: &TicketKey) -> Result<()> {
    ctx.require_setup()?;
    let _lock = wait_for_lock(ctx)?;
    let ledger = ctx.ledger()?;
    let mut ticket = ticket_of(&ledger, key)?;
    if ticket.counts.finished.is_some() {
        bail!("{key} is done, so there is nothing to stop");
    }
    if ticket.flags.iter().any(|f| f == STOPPED) {
        println!("{key} is already stopped. Run `vision take {key}` to start it again.");
        return Ok(());
    }
    let mut killed = Vec::new();
    for running in crate::turn::running_turns(ctx, &ledger)? {
        if running.ticket != key.as_str() {
            continue;
        }
        if crate::turn::kill_turn(ctx, key, &running.turn_id()?)? {
            killed.push(running.id);
        }
    }
    let seconds = ticket
        .counts
        .reasons
        .get(NEEDS_YOU)
        .and_then(|r| seconds_since(&r.at));
    let now = time::now_rfc3339();
    ticket.flags.push(STOPPED.into());
    ticket.counts.reasons.insert(
        STOPPED.into(),
        FlagReason {
            reason: "you stopped it with vision stop".into(),
            at: now.clone(),
        },
    );
    in_tx(&ledger, |ledger| {
        ledger.set_ticket_state(
            key.as_str(),
            ticket.state(),
            &flags_text(&ticket.flags),
            &ticket.counts.text(),
            &now,
        )?;
        ledger.add_human(&HumanRow {
            at: now.clone(),
            ticket: Some(key.to_string()),
            kind: "stop".into(),
            object: key.to_string(),
            value: Some(ticket.state().to_string()),
            reason: None,
            seconds,
        })?;
        ledger.add_event(
            &Event::now("user", "stop")
                .ticket(key.as_str())
                .detail(json!({"step": ticket.state(), "killed": killed})),
        )?;
        Ok(())
    })?;
    ctx.log(&format!("stop: {key} at the step {}", ticket.state()));
    let clone = ticket.row.clone.clone().unwrap_or_else(|| "-".into());
    println!(
        "Stopped {key} at the step {}.{} The branch stays in the clone {clone}, which other tickets can use now. Run `vision take {key}` to start it again.",
        ticket.state(),
        if killed.is_empty() {
            String::new()
        } else {
            format!(" Killed the turn(s) {}.", killed.join(", "))
        }
    );
    Ok(())
}

/// `vision retry KEY`.
pub fn retry(ctx: &Ctx, key: &TicketKey) -> Result<()> {
    ctx.require_setup()?;
    let _lock = wait_for_lock(ctx)?;
    let ledger = ctx.ledger()?;
    let config = ctx.config()?;
    let adapter = adapter::load_with(config, &ctx.paths)?;
    let mut ticket = ticket_of(&ledger, key)?;
    if ticket.counts.finished.is_some() {
        bail!("{key} is done, so there is nothing to retry");
    }
    if ticket.flags.iter().any(|f| f == STOPPED) {
        bail!("{key} is stopped. Run `vision take {key}` to start it again");
    }
    for op in executor::operations_of(&ledger, key)? {
        if matches!(op.state, OpState::Uncertain | OpState::Executing) {
            bail!(
                "the operation {} ({} at {}) is {}: the reconcile step of the tick must settle it first. Run `vision tick`, then retry",
                op.id,
                op.verb,
                short(&op.head),
                op.state.as_str()
            );
        }
    }
    if let Some(running) = crate::turn::running_turn_of(ctx, &ledger, key)? {
        bail!(
            "the turn {} of {key} runs now. Wait for it, or run `vision stop {key}`",
            running.id
        );
    }
    let open = ledger.open_cards(key.as_str())?;
    if open > 0 {
        bail!("{open} card(s) of {key} wait for an answer. Answer them with `vision needs` first");
    }
    let Some(step) = adapter.step(ticket.state()).cloned() else {
        bail!(
            "{key} is at the step {}, which is not in the adapter {}. Fix adapter.toml, or run `vision stop {key}`",
            ticket.state(),
            adapter.name
        );
    };
    let released = executor::release_failed(&ledger, key)?;
    let paused = ticket.flags.iter().any(|f| f == PAUSED);
    let cleared: Vec<String> = ticket
        .flags
        .iter()
        .filter(|f| *f == NEEDS_YOU || *f == PAUSED)
        .cloned()
        .collect();
    let reason = ticket
        .counts
        .reasons
        .get(NEEDS_YOU)
        .map(|r| r.reason.clone());
    let seconds = cleared
        .iter()
        .filter_map(|f| ticket.counts.reasons.get(f))
        .filter_map(|r| seconds_since(&r.at))
        .max();
    ticket.flags.retain(|f| f != NEEDS_YOU && f != PAUSED);
    for flag in &cleared {
        ticket.counts.reasons.remove(flag);
    }
    // A step whose turn result held and that stopped at an operation goes on with its `after`
    // verbs: a new turn would redo work that is done.
    let visit = ticket.visit();
    let held = matches!(step.kind, StepKind::Turn(_))
        && !paused
        && visit.step == step.id
        && visit_turns(&ledger, key, &step.id, visit.turns)?
            .last()
            .is_some_and(|t| t.end_fact.as_deref() == Some("held"));
    let now = time::now_rfc3339();
    let mut new_visit = None;
    if !held {
        let ids = ledger.step_turn_ids(key.as_str(), &step.id)?;
        let turns = u32::try_from(ids.len())?;
        let head = ticket.clone_dir().ok().and_then(|c| head_of(&c).ok());
        let mut next = ticket
            .counts
            .enter(&step, "retry", &now, turns, head, false);
        if matches!(step.kind, StepKind::Turn(_)) {
            if paused {
                let mut session = None;
                for id in ids.iter().rev() {
                    if let Some(row) = ledger.turn(id)? {
                        if let Some(s) = row.session.filter(|s| !s.is_empty()) {
                            session = Some(s);
                            break;
                        }
                    }
                }
                next.message = Some(BY_HAND.to_string());
                next.resume = session;
            } else if let Some(reason) = &reason {
                next.message = Some(format!(
                    "This step starts again after it stopped: {reason}. The person ran `vision retry`. Read the state of the clone first, then do the step."
                ));
            }
        }
        ticket.counts.visit = Some(next.clone());
        new_visit = Some(next);
    }
    in_tx(&ledger, |ledger| {
        ledger.set_ticket_state(
            key.as_str(),
            &step.id,
            &flags_text(&ticket.flags),
            &ticket.counts.text(),
            &now,
        )?;
        if new_visit.is_some() {
            ledger.close_spans(key.as_str(), &now)?;
            ledger.open_span(&span(&ticket, &step, "retry", &now))?;
        }
        ledger.add_human(&HumanRow {
            at: now.clone(),
            ticket: Some(key.to_string()),
            kind: "retry".into(),
            object: key.to_string(),
            value: Some(step.id.clone()),
            reason: reason.clone(),
            seconds,
        })?;
        ledger.add_event(
            &Event::now("user", "retry")
                .ticket(key.as_str())
                .detail(json!({
                    "step": step.id, "cleared": cleared, "released": released,
                    "new_visit": new_visit.as_ref().map(|v| v.seq),
                })),
        )?;
        Ok(())
    })?;
    ctx.log(&format!("retry: {key} at the step {}", step.id));
    let mut text = format!("{key} goes on at the step {}", step.id);
    if !cleared.is_empty() {
        text.push_str(&format!(" (cleared: {})", cleared.join(", ")));
    }
    if released > 0 {
        text.push_str(&format!(". {released} failed operation(s) can run again"));
    }
    if held {
        text.push_str(
            ". Its turn result held, so the next tick runs the operations of the step again",
        );
    } else if paused && new_visit.as_ref().is_some_and(|v| v.resume.is_some()) {
        text.push_str(". The next turn resumes the session that you worked in");
    } else {
        text.push_str(". The next tick enters the step again");
    }
    println!("{text}.");
    Ok(())
}

// ----- log -------------------------------------------------------------------------------

/// One line of `vision log`.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct LogEntry {
    pub at: String,
    /// `event`, `turn`, `card`, or `operation`.
    pub source: String,
    pub kind: String,
    pub id: Option<String>,
    pub text: String,
}

fn cut(text: &str, max: usize) -> String {
    let flat: String = text.chars().filter(|c| !c.is_control()).collect();
    if flat.chars().count() > max {
        let mut out: String = flat.chars().take(max).collect();
        out.push_str(" ...");
        out
    } else {
        flat
    }
}

/// The timeline of one ticket, oldest first.
pub fn timeline(ledger: &Ledger, key: &TicketKey) -> Result<Vec<LogEntry>> {
    let mut out = Vec::new();
    for e in ledger.events_of_ticket(key.as_str())? {
        if matches!(e.kind.as_str(), "turn-started" | "turn-ended") {
            continue;
        }
        out.push(LogEntry {
            at: e.at.clone(),
            source: "event".into(),
            kind: e.kind.clone(),
            id: e.external_id.clone(),
            text: format!(
                "{} {}{}",
                e.actor,
                e.kind,
                e.detail
                    .as_deref()
                    .map(|d| format!(" {}", cut(d, 240)))
                    .unwrap_or_default()
            ),
        });
    }
    for t in ledger.turns_of_ticket(key.as_str())? {
        let agent = t.agent.clone().unwrap_or_default();
        let step = t.step.clone().unwrap_or_default();
        if let Some(started) = &t.started {
            out.push(LogEntry {
                at: started.clone(),
                source: "turn".into(),
                kind: "started".into(),
                id: Some(t.id.clone()),
                text: format!(
                    "turn {} started: {agent} at the step {step}{}",
                    t.id,
                    if t.resumes.is_some() {
                        ", resuming its session"
                    } else {
                        ""
                    }
                ),
            });
        }
        if let Some(ended) = &t.ended {
            out.push(LogEntry {
                at: ended.clone(),
                source: "turn".into(),
                kind: "ended".into(),
                id: Some(t.id.clone()),
                text: format!(
                    "turn {} ended: {} ({}), ${:.4}{}",
                    t.id,
                    t.status.as_deref().unwrap_or("?"),
                    t.reason.as_deref().unwrap_or("?"),
                    t.cost_usd.unwrap_or(0.0),
                    t.end_fact
                        .as_deref()
                        .map(|f| format!(", end fact {f}"))
                        .unwrap_or_default()
                ),
            });
        }
    }
    for c in ledger.cards_of(Some(key.as_str()))? {
        let card: Value = serde_json::from_str(&c.card).unwrap_or(Value::Null);
        let question = card.get("text").and_then(Value::as_str).unwrap_or("");
        if let Some(created) = &c.created {
            out.push(LogEntry {
                at: created.clone(),
                source: "card".into(),
                kind: "asked".into(),
                id: Some(c.id.clone()),
                text: format!("card {} asked: {}", c.id, cut(question, 160)),
            });
        }
        if let Some(answered) = &c.answered {
            let answer: Value = c
                .answer
                .as_deref()
                .and_then(|a| serde_json::from_str(a).ok())
                .unwrap_or(Value::Null);
            out.push(LogEntry {
                at: answered.clone(),
                source: "card".into(),
                kind: c.state.clone(),
                id: Some(c.id.clone()),
                text: format!(
                    "card {} {} ({}, basis {}): {}",
                    c.id,
                    c.state,
                    c.outcome.as_deref().unwrap_or("-"),
                    c.basis.as_deref().unwrap_or("-"),
                    cut(
                        answer.get("text").and_then(Value::as_str).unwrap_or(""),
                        120
                    )
                ),
            });
        }
    }
    for op in executor::operations_of(ledger, key)? {
        let at = op
            .ended
            .clone()
            .or_else(|| op.started.clone())
            .unwrap_or_default();
        out.push(LogEntry {
            at,
            source: "operation".into(),
            kind: op.state.as_str().into(),
            id: Some(op.id.clone()),
            text: format!(
                "operation {} at {}: {}{}",
                op.verb,
                short(&op.head),
                op.outcome,
                if op.detail.is_empty() {
                    String::new()
                } else {
                    format!(": {}", cut(&op.detail, 200))
                }
            ),
        });
    }
    // Times have whole seconds. In one second, a turn ends, the tick acts on it, and the next
    // turn starts: that order is kept.
    let rank = |e: &LogEntry| match (e.source.as_str(), e.kind.as_str()) {
        ("turn", "ended") => 0,
        ("card", _) => 1,
        ("event", _) => 2,
        ("operation", _) => 3,
        _ => 4,
    };
    out.sort_by(|a, b| a.at.cmp(&b.at).then(rank(a).cmp(&rank(b))));
    Ok(out)
}

/// `vision log KEY [--json]`.
pub fn log(ctx: &Ctx, key: &TicketKey, json: bool) -> Result<()> {
    ctx.require_setup()?;
    let ledger = ctx.ledger()?;
    let entries = timeline(&ledger, key)?;
    if json {
        println!("{}", serde_json::to_string_pretty(&entries)?);
        return Ok(());
    }
    if entries.is_empty() {
        println!("{key} has no history yet. Run `vision take {key}` to start it.");
        return Ok(());
    }
    for e in entries {
        println!("{}  {:<9}  {}", e.at, e.source, e.text);
    }
    Ok(())
}

// ----- diff ------------------------------------------------------------------------------

/// What `vision diff KEY --json` prints.
#[derive(Clone, Debug, Serialize)]
pub struct DiffView {
    pub ticket: String,
    pub branch: String,
    pub clone: String,
    /// The ref of the forge that the commits are compared with.
    pub compared_with: String,
    pub commits: Vec<DiffCommit>,
}

#[derive(Clone, Debug, Serialize)]
pub struct DiffCommit {
    pub commit: String,
    pub subject: String,
}

/// `vision diff KEY [--json]`: the commits of the branch that the forge does not have yet.
pub fn diff(ctx: &Ctx, key: &TicketKey, json: bool) -> Result<()> {
    ctx.require_setup()?;
    let ledger = ctx.ledger()?;
    let adapter = adapter::load(ctx)?;
    let ticket = ticket_of(&ledger, key)?;
    let clone = ticket
        .clone_dir()
        .with_context(|| format!("{key} holds no clone now, so there is no branch to compare"))?;
    let branch = ticket
        .row
        .branch
        .clone()
        .unwrap_or_else(|| adapter.branch_for(key));
    let remotes = git::run(&clone, &["remote"]).unwrap_or_default();
    let remotes: Vec<&str> = remotes.lines().map(str::trim).collect();
    let wanted = adapter.base.split('/').next().unwrap_or("origin");
    let remote = if remotes.contains(&wanted) {
        Some(wanted.to_string())
    } else {
        remotes.first().map(|r| r.to_string())
    };
    if let Some(remote) = &remote {
        if let Err(e) = git::run(&clone, &["fetch", "--quiet", remote]) {
            eprintln!("warning: could not fetch {remote}, so the comparison uses the refs of the clone: {e:#}");
        }
    }
    let remote_ref = remote.as_ref().map(|r| format!("{r}/{branch}"));
    let compared_with = match &remote_ref {
        Some(r)
            if git::run(
                &clone,
                &[
                    "rev-parse",
                    "--verify",
                    "--quiet",
                    &format!("refs/remotes/{r}"),
                ],
            )
            .is_ok() =>
        {
            r.clone()
        }
        _ => adapter.base.clone(),
    };
    let local = format!("refs/heads/{branch}");
    let text = git::run(
        &clone,
        &[
            "log",
            "--format=%H%x09%s",
            &format!("{compared_with}..{local}"),
        ],
    )
    .with_context(|| {
        format!(
            "could not compare {branch} with {compared_with} in {}",
            clone.display()
        )
    })?;
    let commits: Vec<DiffCommit> = text
        .lines()
        .filter_map(|l| l.split_once('\t'))
        .map(|(c, s)| DiffCommit {
            commit: c.to_string(),
            subject: s.to_string(),
        })
        .collect();
    let view = DiffView {
        ticket: key.to_string(),
        branch: branch.clone(),
        clone: clone.display().to_string(),
        compared_with: compared_with.clone(),
        commits,
    };
    if json {
        println!("{}", serde_json::to_string_pretty(&view)?);
        return Ok(());
    }
    if view.commits.is_empty() {
        println!("The forge has each commit of {branch} ({compared_with} is up to date).");
        return Ok(());
    }
    println!(
        "{} commit(s) of {branch} that {compared_with} does not have yet:",
        view.commits.len()
    );
    for c in &view.commits {
        println!("  {}  {}", short(&c.commit), c.subject);
    }
    Ok(())
}

// ----- report ----------------------------------------------------------------------------

/// The measures of one ticket, or of all.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct Measures {
    /// The cards that reached the person: the outcomes propose and ask.
    pub cards_to_person: usize,
    /// The cards that a decision or a principle of the memory answered.
    pub cards_by_memory: usize,
    /// The cards that the Judge answered alone from its inference.
    pub cards_by_judge: usize,
    /// The minutes of the person, from the `human` rows.
    pub human_minutes: f64,
    /// The seconds in each step, from `spans`.
    pub step_seconds: BTreeMap<String, i64>,
    /// The cost of the turns of each step.
    pub step_cost_usd: BTreeMap<String, f64>,
    pub cost_usd: f64,
}

impl Measures {
    fn add(&mut self, other: &Measures) {
        self.cards_to_person += other.cards_to_person;
        self.cards_by_memory += other.cards_by_memory;
        self.cards_by_judge += other.cards_by_judge;
        self.human_minutes += other.human_minutes;
        for (k, v) in &other.step_seconds {
            *self.step_seconds.entry(k.clone()).or_default() += v;
        }
        for (k, v) in &other.step_cost_usd {
            *self.step_cost_usd.entry(k.clone()).or_default() += v;
        }
        self.cost_usd += other.cost_usd;
    }
}

/// What `vision report --json` prints.
#[derive(Clone, Debug, Serialize)]
pub struct Report {
    /// The start of the window, or `None` for all time.
    pub since: Option<String>,
    pub tickets: BTreeMap<String, Measures>,
    pub total: Measures,
    /// The steps of the measures in the order of the step list of the adapter, then each step
    /// that the adapter no longer has, by name.
    pub steps: Vec<String>,
}

/// The measures of each ticket and in total.
pub fn report_of(ledger: &Ledger, since: Option<&str>) -> Result<Report> {
    let mut tickets: BTreeMap<String, Measures> = BTreeMap::new();
    let reserved = |t: &str| TicketKey::parse(t).map_or(true, |k| k.is_reserved());
    let after = |at: Option<&str>| since.is_none_or(|s| at.is_some_and(|a| a >= s));
    for c in ledger.cards_of(None)? {
        if reserved(&c.ticket) || !after(c.created.as_deref()) {
            continue;
        }
        let m = tickets.entry(c.ticket.clone()).or_default();
        match (c.outcome.as_deref(), c.basis.as_deref()) {
            (Some("propose" | "ask"), _) => m.cards_to_person += 1,
            (Some("answer"), Some("decision" | "principle")) => m.cards_by_memory += 1,
            (Some("answer"), _) => m.cards_by_judge += 1,
            _ => {}
        }
    }
    for h in ledger.human_of(None)? {
        let Some(ticket) = h.ticket.clone() else {
            continue;
        };
        if reserved(&ticket) || !after(Some(&h.at)) {
            continue;
        }
        tickets.entry(ticket).or_default().human_minutes += h.seconds.unwrap_or(0) as f64 / 60.0;
    }
    let now = time::now();
    for s in ledger.spans_of(None)? {
        if reserved(&s.ticket) {
            continue;
        }
        let (Ok(start), end) = (
            time::parse_rfc3339(&s.started),
            s.ended
                .as_deref()
                .and_then(|e| time::parse_rfc3339(e).ok())
                .unwrap_or(now),
        ) else {
            continue;
        };
        let start = match since.and_then(|s| time::parse_rfc3339(s).ok()) {
            Some(window) if window > start => window,
            _ => start,
        };
        if end < start {
            continue;
        }
        *tickets
            .entry(s.ticket.clone())
            .or_default()
            .step_seconds
            .entry(s.state.clone())
            .or_default() += time::seconds_between(start, end);
    }
    for c in ledger.step_costs(since)? {
        if reserved(&c.ticket) {
            continue;
        }
        let m = tickets.entry(c.ticket.clone()).or_default();
        *m.step_cost_usd.entry(c.step.clone()).or_default() += c.cost_usd;
        m.cost_usd += c.cost_usd;
    }
    let mut total = Measures::default();
    for m in tickets.values() {
        total.add(m);
    }
    let steps = step_order(
        &[],
        total.step_seconds.keys().chain(total.step_cost_usd.keys()),
    );
    Ok(Report {
        since: since.map(str::to_string),
        tickets,
        total,
        steps,
    })
}

/// A cost in dollars for people: two decimals, and `<$0.01` for a cost above zero that is below
/// one cent, so that a cheap turn never shows as free.
pub fn money(cost: f64) -> String {
    if cost > 0.0 && cost < 0.01 {
        "<$0.01".to_string()
    } else {
        format!("${cost:.2}")
    }
}

/// The steps of the measures in the order of the step list of the adapter, then each step that
/// the adapter no longer has, by name.
pub fn step_order<'a>(
    adapter_steps: &[String],
    names: impl IntoIterator<Item = &'a String>,
) -> Vec<String> {
    let names: BTreeSet<&String> = names.into_iter().collect();
    let mut out: Vec<String> = adapter_steps
        .iter()
        .filter(|s| names.contains(s))
        .cloned()
        .collect();
    for name in names {
        if !adapter_steps.contains(name) {
            out.push(name.clone());
        }
    }
    out
}

/// `vision report [--week] [--json]`.
pub fn report(ctx: &Ctx, week: bool, json: bool) -> Result<()> {
    ctx.require_setup()?;
    let ledger = ctx.ledger()?;
    let since = week.then(|| time::rfc3339(time::now() - ::time::Duration::days(7)));
    let mut report = report_of(&ledger, since.as_deref())?;
    // A broken adapter never breaks the report: the steps then go by name.
    let adapter_steps: Vec<String> = crate::adapter::load(ctx)
        .map(|a| a.steps.iter().map(|s| s.id.clone()).collect())
        .unwrap_or_default();
    report.steps = step_order(
        &adapter_steps,
        report
            .total
            .step_seconds
            .keys()
            .chain(report.total.step_cost_usd.keys()),
    );
    if json {
        println!("{}", serde_json::to_string_pretty(&report)?);
        return Ok(());
    }
    print!("{}", render_report(&report));
    Ok(())
}

/// The text of `vision report`.
pub fn render_report(report: &Report) -> String {
    let mut out = String::new();
    let window = match &report.since {
        Some(s) => format!("since {s}"),
        None => "for all time".into(),
    };
    if report.tickets.is_empty() {
        out.push_str(&format!("No ticket has measures {window}.\n"));
        return out;
    }
    out.push_str(&format!("Measures {window}\n\n"));
    let mut table = vec![[
        "TICKET",
        "CARDS TO YOU",
        "BY MEMORY",
        "BY JUDGE",
        "YOUR MIN",
        "COST",
    ]
    .map(String::from)];
    let line = |name: &str, m: &Measures| {
        [
            name.to_string(),
            m.cards_to_person.to_string(),
            m.cards_by_memory.to_string(),
            m.cards_by_judge.to_string(),
            format!("{:.1}", m.human_minutes),
            money(m.cost_usd),
        ]
    };
    for (key, m) in &report.tickets {
        table.push(line(key, m));
    }
    table.push(line("total", &report.total));
    out.push_str(&render_table(&table));
    for (key, m) in report
        .tickets
        .iter()
        .chain(std::iter::once((&"total".to_string(), &report.total)))
    {
        let own: BTreeSet<&String> = m
            .step_seconds
            .keys()
            .chain(m.step_cost_usd.keys())
            .collect();
        if own.is_empty() {
            continue;
        }
        let steps = step_order(&report.steps, own);
        out.push_str(&format!("\n{key}, by step:\n"));
        for step in steps {
            let seconds = m.step_seconds.get(&step).copied().unwrap_or(0);
            let cost = m.step_cost_usd.get(&step).copied().unwrap_or(0.0);
            out.push_str(&format!(
                "  {step:<16} {:>6}  {}\n",
                time::short_age(seconds),
                money(cost)
            ));
        }
    }
    out
}

#[cfg(test)]
mod report_tests {
    use super::*;

    #[test]
    fn a_cost_below_one_cent_is_not_free() {
        assert_eq!(money(0.0), "$0.00");
        assert_eq!(money(0.0014), "<$0.01");
        assert_eq!(money(0.009), "<$0.01");
        assert_eq!(money(0.01), "$0.01");
        assert_eq!(money(2.5), "$2.50");
    }

    #[test]
    fn the_steps_follow_the_adapter_and_an_old_step_comes_last() {
        let adapter: Vec<String> = ["refine", "plan", "build", "review", "ci"]
            .map(String::from)
            .to_vec();
        let names: Vec<String> = ["ci", "build", "old-step", "refine", "another-old"]
            .map(String::from)
            .to_vec();
        assert_eq!(
            step_order(&adapter, &names),
            ["refine", "build", "ci", "another-old", "old-step"]
        );
        assert_eq!(step_order(&[], &names)[0], "another-old");
    }

    #[test]
    fn the_report_lists_the_steps_in_the_order_of_the_adapter() {
        let mut m = Measures::default();
        m.step_seconds.insert("review".into(), 60);
        m.step_seconds.insert("build".into(), 120);
        m.step_seconds.insert("gone".into(), 5);
        m.step_cost_usd.insert("build".into(), 1.25);
        m.step_cost_usd.insert("review".into(), 0.004);
        m.cost_usd = 1.254;
        let mut report = Report {
            since: None,
            tickets: BTreeMap::from([("DEMO-12".to_string(), m.clone())]),
            total: m,
            steps: Vec::new(),
        };
        report.steps = step_order(
            &["build".to_string(), "review".to_string()],
            report.total.step_seconds.keys(),
        );
        let text = render_report(&report);
        let lines: Vec<&str> = text.lines().collect();
        let at = lines
            .iter()
            .position(|l| *l == "DEMO-12, by step:")
            .unwrap();
        assert!(lines[at + 1].starts_with("  build "), "{text}");
        assert!(lines[at + 1].ends_with("$1.25"), "{text}");
        assert!(lines[at + 2].starts_with("  review "), "{text}");
        assert!(lines[at + 2].ends_with("<$0.01"), "{text}");
        assert!(lines[at + 3].starts_with("  gone "), "{text}");
        assert!(lines[at + 3].ends_with("$0.00"), "{text}");
    }
}
