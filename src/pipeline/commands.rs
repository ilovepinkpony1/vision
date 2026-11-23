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

use super::machine::{dirty_lines, head_of, human_mark, in_tx, save, span, visit_turns, Ticket};
use super::resume::{self, Checked, Head, Resumed, RESUMED};
use super::slots::{self, NewTake};
use super::state::{
    flags_text, Counts, FlagReason, QueuedTake, SlotNote, Visit, NEEDS_YOU, PAUSED, RESUME,
    STOPPED, WAITS_FOR_SLOT,
};
use super::view::{plan_hash, SHORT_HASH};
use crate::adapter::{self, Adapter, Step, StepKind};
use crate::config::Config;
use crate::ctx::Ctx;
use crate::executor::{self, short, OpState, QueueTicket};
use crate::exit::Stop;
use crate::ledger::questions::HumanRow;
use crate::ledger::{Event, EventRow, Ledger, TicketRow};
use crate::lock::TickLock;
use crate::paths::TicketKey;
use crate::util::fs::atomic_write;
use crate::util::{git, time};

/// How long a command waits for a running tick to release the lock.
const LOCK_WAIT: Duration = Duration::from_secs(30);

/// The message of the first turn after the person worked in the session with `vision open`.
pub const BY_HAND: &str = "The person worked in this session by hand with `vision open`. Read the state of the clone first (git status, git log, and the files that changed), then continue the step from there.";

pub(crate) fn wait_for_lock(ctx: &Ctx) -> Result<TickLock> {
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
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct PickRow {
    pub key: String,
    pub title: String,
    pub status: String,
    pub eligible: bool,
    pub reason: String,
    /// The step of a ticket that Vision already holds.
    pub taken: Option<String>,
    /// The branch of a ticket that is in progress outside Vision: a take starts it at the step
    /// where it is.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
    /// The open pull request of such a ticket.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pr: Option<i64>,
}

impl PickRow {
    /// True for a ticket that is in progress outside Vision: not eligible for a fresh start,
    /// and a take starts it at the step where it is.
    pub fn outside(&self) -> bool {
        !self.eligible && self.branch.is_some()
    }
}

/// The rows of `vision pick`: the queue of the tracker, with the step of each ticket that
/// Vision already holds. It only reads.
pub fn pick_rows(ctx: &Ctx) -> Result<Vec<PickRow>> {
    ctx.require_setup()?;
    let ledger = ctx.ledger()?;
    let queue = executor::observe_queue(ctx)?;
    let held = held_tickets(&ledger)?;
    Ok(queue
        .into_iter()
        .map(|t| PickRow {
            taken: held.get(t.key.as_str()).cloned(),
            key: t.key.to_string(),
            title: t.title,
            status: t.status,
            eligible: t.eligible,
            reason: t.reason,
            branch: t.branch,
            pr: t.pr,
        })
        .collect())
}

/// `vision pick [--json]`.
pub fn pick(ctx: &Ctx, json: bool) -> Result<()> {
    let rows = pick_rows(ctx)?;
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
            (None, false) if row.outside() => "yes, at the step where it is".into(),
            (None, false) => format!("no: {}", row.reason),
        };
        table.push([row.key.clone(), row.status.clone(), take, row.title.clone()]);
    }
    print!("{}", render_table(&table));
    if rows
        .iter()
        .any(|r| (r.eligible || r.outside()) && r.taken.is_none())
    {
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

/// `vision take KEY...`, and `vision take KEY --step STEP`, which continues one stopped ticket
/// at STEP (see [`restart`]), or takes one ticket that is in progress outside Vision at STEP (see
/// [`take_outside`]).
pub fn take(ctx: &Ctx, keys: &[TicketKey], step: Option<&str>) -> Result<()> {
    ctx.require_setup()?;
    if step.is_some() && keys.len() != 1 {
        bail!("`--step` continues or takes one ticket: name one key, such as `vision take DEMO-12 --step in-review`");
    }
    let _lock = wait_for_lock(ctx)?;
    let ledger = ctx.ledger()?;
    let config = ctx.config()?;
    let adapter = adapter::load_with(config, &ctx.paths)?;
    let mut queue: Option<Vec<QueueTicket>> = None;
    let mut failed = Vec::new();
    for key in keys {
        match take_one(ctx, &ledger, &adapter, &mut queue, key, step) {
            Ok(line) => println!("{line}"),
            Err(e) => {
                // A suggestion of the rules is the answer of the command, not an error.
                match e.downcast_ref::<resume::Suggests>() {
                    Some(resume::Suggests(text)) => println!("{text}"),
                    None => eprintln!("error: {key}: {e:#}"),
                }
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
    adapter: &Adapter,
    queue: &mut Option<Vec<QueueTicket>>,
    key: &TicketKey,
    step: Option<&str>,
) -> Result<String> {
    if key.is_reserved() {
        bail!("{key} is a reserved key: Vision uses it for turns with no ticket. Take a ticket of the tracker");
    }
    if let Some(row) = ledger.ticket(key.as_str())? {
        let ticket = Ticket::of(row)?;
        if !ticket.state().is_empty() && ticket.counts.finished.is_none() {
            if ticket.flags.iter().any(|f| f == STOPPED) {
                return restart(ctx, ledger, adapter, ticket, step);
            }
            if let Some(step) = step {
                bail!(
                    "{key} is not stopped, so `vision take --step` has nothing to continue. To move it to another step, run `vision retry {key} --step {step}`"
                );
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
        if step.is_some() {
            bail!(
                "`vision take --step` continues a stopped ticket, or takes a ticket that is in progress outside Vision, and the queue of the tracker has no ticket {key}. `vision pick` lists the queue"
            );
        }
        bail!("the queue of the tracker has no ticket {key}. `vision pick` lists the queue");
    };
    if !found.eligible {
        if let Some(branch) = found.branch.clone() {
            return take_outside(ctx, ledger, adapter, &found, &branch, step);
        }
    }
    if step.is_some() {
        bail!(
            "`vision take --step` continues a stopped ticket, or takes a ticket that is in progress outside Vision, and {key} is neither. Run `vision take {key}` to start it"
        );
    }
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
    let entry = adapter.entry().clone();
    let new = NewTake {
        key: key.clone(),
        title: Some(found.title.trim().to_string()).filter(|t| !t.is_empty()),
        branch: adapter.branch_for(key),
        pr: None,
        take: QueuedTake {
            at: time::now_rfc3339(),
            step: entry.id.clone(),
            outside: false,
            event: Value::Null,
        },
    };
    take_or_queue(ctx, ledger, &new)
}

/// Puts a take at the end of the queue and gives the slots at once (section 11.3): the line of
/// its start, or its place in the queue. An error of its own start leaves no row of it.
fn take_or_queue(ctx: &Ctx, ledger: &Ledger, new: &NewTake) -> Result<String> {
    let given = slots::assign(ctx, ledger, time::now(), Some(new))?;
    Ok(slots::take_line(new.key.as_str(), &new.take.step, &given))
}

/// The work of a take in the clone of its slot: a fetch, the branch, and the row of the ticket
/// at its first step, with the event `take`. A take that waited in the queue keeps the time of
/// the take in `taken`. Gives the line of the take.
pub(super) fn start_take(
    ctx: &Ctx,
    ledger: &Ledger,
    adapter: &Adapter,
    new: &NewTake,
    clone: &Path,
    taken: Option<&str>,
) -> Result<String> {
    let key = &new.key;
    let take = &new.take;
    let target = known_step(adapter, &take.step)?;
    let clone_text = clone.display().to_string();
    git::run(clone, &["fetch", "--quiet", "--all", "--prune"])
        .with_context(|| format!("could not fetch in the clone {clone_text}"))?;
    let base = &adapter.base;
    let branch = &new.branch;
    if take.outside {
        let again = format!("vision take {key} --step {}", target.id);
        let remote = base_remote(clone, adapter).unwrap_or_else(|| "origin".to_string());
        switch_outside(clone, &remote, branch, &again)?;
    } else {
        if git::run(
            clone,
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
        git::run(
            clone,
            &["switch", "--quiet", "--no-track", "-C", branch, base],
        )
        .map_err(|e| {
            switch_error(
                e,
                clone,
                branch,
                &format!("vision take {key}"),
                format!("could not make the branch {branch} from {base} in the clone {clone_text}"),
            )
        })?;
    }
    let head = head_of(clone)?;
    let now = time::now_rfc3339();
    let turns = u32::try_from(ledger.step_turn_ids(key.as_str(), &target.id)?.len())?;
    let mut counts = Counts {
        slot: Some(SlotNote {
            last: Some(clone_text.clone()),
            ..SlotNote::default()
        }),
        ..Counts::default()
    };
    let visit = counts.enter(&target, "take", &now, turns, Some(head.clone()), true);
    let row = TicketRow {
        key: key.to_string(),
        title: new.title.clone(),
        state: Some(target.id.clone()),
        flags: Some(flags_text(&[])),
        clone: Some(clone_text.clone()),
        branch: Some(branch.clone()),
        pr: new.pr,
        head: Some(head.clone()),
        taken: Some(taken.unwrap_or(&now).to_string()),
        updated: Some(now.clone()),
        counts: Some(counts.text()),
    };
    let dir = ctx.paths.ticket_dir(key);
    std::fs::create_dir_all(&dir).with_context(|| format!("could not make {}", dir.display()))?;
    let ticket = Ticket::of(row.clone())?;
    let mut detail = json!({
        "clone": clone_text, "branch": branch, "base": head, "step": target.id, "seq": visit.seq,
    });
    if let (Some(more), Some(map)) = (take.event.as_object(), detail.as_object_mut()) {
        for (name, value) in more {
            map.insert(name.clone(), value.clone());
        }
    }
    if taken.is_some() {
        detail["queued"] = json!(take.at);
    }
    in_tx(ledger, |ledger| {
        ledger.put_ticket(&row)?;
        ledger.close_spans(key.as_str(), &now)?;
        ledger.open_span(&span(&ticket, &target, "take", &now))?;
        ledger.add_event(
            &Event::now("user", "take")
                .ticket(key.as_str())
                .detail(detail.clone()),
        )?;
        Ok(())
    })?;
    if take.outside {
        ctx.log(&format!(
            "take: {key} from outside Vision in {clone_text} on {branch} at the step {} ({})",
            target.id,
            short(&head)
        ));
        Ok(format!(
            "Took {key} in the clone {clone_text} on its branch {branch} ({}), {}. The next tick enters the step {}.",
            short(&head),
            match new.pr {
                Some(n) => format!("with the pull request #{n}"),
                None => "with no pull request".to_string(),
            },
            target.id
        ))
    } else {
        ctx.log(&format!(
            "take: {key} in {clone_text} on {branch} from {base} ({})",
            short(&head)
        ));
        Ok(format!(
            "Took {key} in the clone {clone_text} on the branch {branch} from {base} ({}). The next tick starts the step {}.",
            short(&head),
            target.id
        ))
    }
}

/// `vision take` of a ticket that is in progress outside Vision: it has a branch, and maybe an
/// open pull request, that someone made by hand.
///
/// With no STEP, it reads the facts and the rules, prints where the ticket is and the step that
/// the rules suggest, and changes nothing. With a STEP, it takes the ticket there: in a free
/// clone, on the branch of the ticket with the commits of the remote, at that step. An end step
/// is refused, because Vision takes no ticket that is done.
fn take_outside(
    ctx: &Ctx,
    ledger: &Ledger,
    adapter: &Adapter,
    found: &QueueTicket,
    branch: &str,
    to: Option<&str>,
) -> Result<String> {
    let key = &found.key;
    // The facts and the rules: one call of observe with the branch and the pull request of the
    // queue, before anything changes.
    let checked = resume::check_outside(ctx, adapter, key, branch, found.pr);
    let Some(name) = to else {
        let text = resume::outside_text(key, branch, found.pr, checked.as_ref());
        return Err(resume::Suggests(text).into());
    };
    let target = known_step(adapter, name)?;
    if matches!(target.kind, StepKind::End) {
        bail!(
            "`{}` is the end step of the adapter {}, and Vision takes no ticket that is done. Name the step where {key} waits now",
            target.id,
            adapter.name
        );
    }
    let pr = checked.as_ref().and_then(Checked::pr).or(found.pr);
    let facts = checked.as_ref().map(Checked::words).unwrap_or_default();
    let suggestion = checked
        .as_ref()
        .and_then(Checked::suggested)
        .map(str::to_string);
    let new = NewTake {
        key: key.clone(),
        title: Some(found.title.trim().to_string()).filter(|t| !t.is_empty()),
        branch: branch.to_string(),
        pr,
        take: QueuedTake {
            at: time::now_rfc3339(),
            step: target.id.clone(),
            outside: true,
            event: json!({"outside": true, "pr": pr, "suggestion": suggestion, "facts": facts}),
        },
    };
    take_or_queue(ctx, ledger, &new)
}

/// The remote of the clone for the base ref of the adapter, such as `origin` of `origin/main`.
/// A clone without that remote gives its first remote, and a clone with no remote gives `None`.
pub(super) fn base_remote(clone: &Path, adapter: &Adapter) -> Option<String> {
    let remotes = git::run(clone, &["remote"]).unwrap_or_default();
    let remotes: Vec<&str> = remotes
        .lines()
        .map(str::trim)
        .filter(|r| !r.is_empty())
        .collect();
    let wanted = adapter.base.split('/').next().unwrap_or("origin");
    if remotes.contains(&wanted) {
        Some(wanted.to_string())
    } else {
        remotes.first().map(|r| r.to_string())
    }
}

/// Puts the clone on the branch of a ticket that is in progress outside Vision, with the commits
/// of the remote. A branch that the clone does not have starts at the remote branch. A local
/// branch that the remote branch is ahead of moves forward to it. A local branch with commits
/// that the remote does not have keeps them, and the next push sends them. A local branch that
/// went apart from the remote stops the take, and nothing moves. No commit goes away.
pub(super) fn switch_outside(clone: &Path, remote: &str, branch: &str, again: &str) -> Result<()> {
    let at =
        |reference: String| git::run(clone, &["rev-parse", "--verify", "--quiet", &reference]).ok();
    let local = at(format!("refs/heads/{branch}"));
    let tracking = format!("{remote}/{branch}");
    let theirs = at(format!("refs/remotes/{tracking}"));
    let ancestor =
        |a: &str, b: &str| git::run(clone, &["merge-base", "--is-ancestor", a, b]).is_ok();
    let switch = |args: &[&str]| -> Result<()> {
        git::run(clone, args).map(|_| ()).map_err(|e| {
            switch_error(
                e,
                clone,
                branch,
                again,
                format!(
                    "could not switch the clone {} to the branch {branch}",
                    clone.display()
                ),
            )
        })
    };
    match (local, theirs) {
        (None, None) => bail!(
            "the branch {branch} is neither in the clone {} nor on {remote} after a fetch. Push it, then run `{again}` again",
            clone.display()
        ),
        (None, Some(_)) => switch(&["switch", "--quiet", "--no-track", "-c", branch, &tracking]),
        (Some(_), None) => switch(&["switch", "--quiet", branch]),
        (Some(mine), Some(theirs)) => {
            if mine != theirs && !ancestor(&mine, &theirs) && !ancestor(&theirs, &mine) {
                bail!(
                    "the branch {branch} in the clone {} and {tracking} went apart: neither holds the head of the other. Merge or reset the branch in the clone by hand, then run `{again}` again",
                    clone.display()
                );
            }
            switch(&["switch", "--quiet", branch])?;
            if mine != theirs && ancestor(&mine, &theirs) {
                git::run(clone, &["merge", "--ff-only", "--quiet", &theirs]).with_context(
                    || {
                        format!(
                            "could not move the branch {branch} of the clone {} forward to {tracking}",
                            clone.display()
                        )
                    },
                )?;
            }
            Ok(())
        }
    }
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

/// The error of a `git switch` to `branch` in `clone` that git refused. Git lets one worktree
/// hold a branch at a time: when another worktree of the repo holds the branch, the error names
/// that worktree and says what to do, and `again` is the command to run after the fix. Else
/// `what` says what failed.
pub(super) fn switch_error(
    e: anyhow::Error,
    clone: &Path,
    branch: &str,
    again: &str,
    what: String,
) -> anyhow::Error {
    let Some(holder) = git::worktree_holding(clone, branch) else {
        return e.context(what);
    };
    let fix = if holder.is_dir() {
        format!(
            "Switch that worktree to another branch, such as with `git -C {} switch --detach`, or remove that worktree",
            holder.display()
        )
    } else {
        format!(
            "Its folder is gone, so remove its record with `git -C {} worktree prune`",
            clone.display()
        )
    };
    e.context(format!(
        "the worktree {} holds the branch {branch}, and git lets one worktree hold a branch at a time, so the clone {} cannot switch to it. {fix}, then run `{again}` again",
        holder.display(),
        clone.display()
    ))
}

pub(super) fn same_dir(a: &Path, b: &Path) -> bool {
    a == b
        || match (a.canonicalize(), b.canonicalize()) {
            (Ok(a), Ok(b)) => a == b,
            _ => false,
        }
}

/// The clone that `vision take` would use now: the first clone of `clones` that no ticket
/// holds and whose work tree is clean. It only reads; the error says why no clone is free.
pub fn next_clone(ctx: &Ctx) -> Result<PathBuf> {
    ctx.require_setup()?;
    let ledger = ctx.ledger()?;
    free_clone(&ledger, ctx.config()?)
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

/// `vision take` of a stopped ticket: it starts again in the clone that holds its kept branch.
///
/// With no STEP, it starts again at the same step. When a `[[resume]]` rule suggests another
/// step, it does not start: it prints the suggestion and the two commands, and nothing
/// changes. With a STEP, the ticket continues there: at its stored step as a plain take, and
/// at another step as the move of `vision retry --step`, which also clears the flags of a
/// retry. When the rules ran, the continue takes the head of the pull request that someone
/// pushed by hand, and its event is `resumed`.
fn restart(
    ctx: &Ctx,
    ledger: &Ledger,
    adapter: &Adapter,
    mut ticket: Ticket,
    to: Option<&str>,
) -> Result<String> {
    let key = ticket.key.clone();
    // A take that never started goes back to the end of the queue (section 11.3).
    if let Some(take) = ticket
        .counts
        .slot
        .as_ref()
        .and_then(|s| s.take.as_ref())
        .filter(|_| !ticket.has_clone())
    {
        if let Some(name) = to.filter(|name| *name != take.step) {
            bail!(
                "{key} never started, so it starts at the step {} of its take, not at {name}. Run `vision take {key}`, then `vision retry {key} --step {name}` after its start",
                take.step
            );
        }
        return slots::requeue(ctx, ledger, ticket);
    }
    // A ticket that holds no slot, or whose clone another ticket holds now, takes a slot first:
    // a free one, or the slot of a ticket that only watches.
    let taken = match ticket.clone_dir() {
        Ok(clone) => clone_holders(ledger)?
            .into_iter()
            .any(|(dir, k, _)| k != key.as_str() && same_dir(&dir, &clone)),
        Err(_) => true,
    };
    if taken {
        slots::claim(ctx, ledger, adapter, &mut ticket)?;
    }
    let clone = ticket.clone_dir()?;
    let branch = ticket
        .row
        .branch
        .clone()
        .unwrap_or_else(|| adapter.branch_for(&key));
    let Some(step) = adapter.step(ticket.state()).cloned() else {
        bail!(
            "{key} stopped at the step {}, which is not in the adapter {} now",
            ticket.state(),
            adapter.name
        );
    };
    let target = to.map(|name| known_step(adapter, name)).transpose()?;
    if target.as_ref().is_some_and(|t| t.id != step.id) {
        refuse_busy(ctx, ledger, &key)?;
    }
    // The facts and the rules of [[resume]]: one call of observe and the head of the pull
    // request, before anything changes. The take switches the clone to the branch first.
    let checked = resume::check(ctx, ledger, adapter, &ticket, true);
    let suggested = checked
        .as_ref()
        .and_then(Checked::suggested)
        .map(str::to_string);
    let target = match target {
        Some(target) => {
            refuse_end(adapter, &key, &target, suggested.as_deref())?;
            target
        }
        None => {
            if let (Some(checked), Some(other)) = (&checked, &suggested) {
                if *other != step.id {
                    let text = resume::suggest_text(&key, &step.id, checked);
                    return Err(resume::Suggests(text).into());
                }
            }
            step.clone()
        }
    };
    let current = git::run(&clone, &["rev-parse", "--abbrev-ref", "HEAD"]).unwrap_or_default();
    if current != branch {
        let dirty = dirty_lines(&clone)?;
        if !dirty.is_empty() {
            bail!(
                "the clone {} has changes in its work tree, so the branch {branch} cannot come back. Commit or clean them first",
                clone.display()
            );
        }
        let again = match to {
            Some(name) => format!("vision take {key} --step {name}"),
            None => format!("vision take {key}"),
        };
        git::run(&clone, &["switch", "--quiet", &branch]).map_err(|e| {
            switch_error(
                e,
                &clone,
                &branch,
                &again,
                format!(
                    "the branch {branch} of {key} is gone from the clone {}",
                    clone.display()
                ),
            )
        })?;
    }
    let resumed = match &checked {
        Some(checked) => {
            let again = match to {
                Some(name) => format!("run `vision take {key} --step {name}` again"),
                None => format!("run `vision take {key}` again"),
            };
            Some(head_for_continue(ctx, ledger, &ticket, checked, &again)?)
        }
        None => None,
    };
    if target.id != step.id {
        return move_ticket(ctx, ledger, ticket, &target, resumed.as_ref(), None);
    }
    let now = time::now_rfc3339();
    // A flag needs-you that the test of the head of a continue set goes with the stop: this
    // continue tested the head again.
    let mut drop = vec![STOPPED];
    if resume::resume_flag(&ticket) {
        drop.push(NEEDS_YOU);
    }
    let cleared = clear(&mut ticket, &drop);
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
        record_pr(ledger, &key, resumed.as_ref(), &now)?;
        ledger.close_spans(key.as_str(), &now)?;
        ledger.open_span(&span(&ticket, &step, "restart", &now))?;
        match &resumed {
            Some(resumed) => {
                ledger.add_human(&resume_row(&key, &step.id, &cleared, &now))?;
                ledger.add_event(&Event::now("user", RESUMED).ticket(key.as_str()).detail(
                    resumed_detail(
                        resumed,
                        &step.id,
                        &step.id,
                        "restart",
                        &cleared,
                        0,
                        Some(&visit),
                    ),
                ))?;
            }
            None => {
                ledger.add_event(
                    &Event::now("user", "take")
                        .ticket(key.as_str())
                        .detail(json!({"restart": true, "step": step.id, "seq": visit.seq})),
                )?;
            }
        }
        Ok(())
    })?;
    ctx.log(&format!("take: {key} starts again at the step {}", step.id));
    let mut text = format!(
        "{key} starts again at the step {} in the clone {} on the branch {branch}.",
        step.id,
        clone.display()
    );
    if let Some(resumed) = &resumed {
        for line in resumed.lines(&step.id) {
            text.push_str(&format!(" {line}."));
        }
    }
    Ok(text)
}

/// The head of the pull request for a continue that ran the rules. When the clone has the head
/// one fast-forward ahead of the branch, the branch moves forward to it. When the pull request
/// went on without commits of Vision that never left the machine, the branch moves to its head,
/// and a ref keeps the old tip. When the clone cannot
/// take it, the ticket gets the flag needs-you with the reason and the fix, and nothing moves.
/// Gives what the event `resumed` records.
fn head_for_continue(
    ctx: &Ctx,
    ledger: &Ledger,
    ticket: &Ticket,
    checked: &Checked,
    again: &str,
) -> Result<Resumed> {
    let head = &checked.head;
    let clone = ticket.clone_dir()?;
    if let Some(problem) = head.problem(checked.pr(), &clone.display().to_string()) {
        let reason = format!("{}. {}, then {again}", problem.what, problem.fix);
        flag_resume(ctx, ledger, ticket, &reason)?;
        bail!(
            "{reason}. Nothing moved, and {} has the flag needs-you until a continue takes the head",
            ticket.key
        );
    }
    if let Head::Forward { to, .. } = head {
        resume::take_head(&clone, to)?;
        ctx.log(&format!(
            "resume: {} took the head {} of the pull request",
            ticket.key,
            short(to)
        ));
    }
    if let Head::Replaced {
        from,
        to,
        commits,
        kept,
    } = head
    {
        resume::replace_head(&clone, from, to, kept)?;
        ctx.log(&format!(
            "resume: {} took the head {} of the pull request, which went on without {} commit(s) of Vision that never left the machine; {kept} keeps the old tip {}",
            ticket.key,
            short(to),
            commits.len(),
            short(from)
        ));
    }
    Ok(Resumed::of(checked, ticket.state(), ticket.row.pr))
}

/// Writes in the ticket row what a continue found of the open pull request: its number when the
/// row names none, and its head when the branch of the clone holds it after the continue. A
/// number that the row names already stays.
fn record_pr(ledger: &Ledger, key: &TicketKey, resumed: Option<&Resumed>, now: &str) -> Result<()> {
    let Some(resumed) = resumed else {
        return Ok(());
    };
    if resumed.pr_written.is_some() || resumed.pr_head.is_some() {
        ledger.set_ticket_values(
            key.as_str(),
            resumed.pr_written,
            resumed.pr_head.as_deref(),
            now,
        )?;
    }
    Ok(())
}

/// Sets the flag needs-you of a continue that cannot take the head, marked as set by the
/// resume, so that the next continue clears it. A flag needs-you of a rule keeps its reason.
fn flag_resume(ctx: &Ctx, ledger: &Ledger, ticket: &Ticket, reason: &str) -> Result<()> {
    let mut ticket = ticket.clone();
    let now = time::now_rfc3339();
    let ours = ticket
        .counts
        .reasons
        .get(NEEDS_YOU)
        .is_none_or(|r| r.by.as_deref() == Some(RESUME));
    if !ticket.flags.iter().any(|f| f == NEEDS_YOU) {
        ticket.flags.push(NEEDS_YOU.into());
    }
    if ours {
        ticket.counts.reasons.insert(
            NEEDS_YOU.into(),
            FlagReason {
                reason: reason.to_string(),
                at: now.clone(),
                by: Some(RESUME.into()),
            },
        );
    }
    save(
        ledger,
        &ticket,
        &now,
        Some(
            Event::now("vision", "flag")
                .ticket(ticket.key.as_str())
                .detail(json!({"flag": NEEDS_YOU, "reason": reason, "step": ticket.state(), "by": RESUME})),
        ),
    )?;
    ctx.log(&format!(
        "resume: {} got the flag needs-you: {reason}",
        ticket.key
    ));
    Ok(())
}

/// The `human` row of a continue that ran the rules.
fn resume_row(key: &TicketKey, step: &str, cleared: &Cleared, now: &str) -> HumanRow {
    HumanRow {
        at: now.to_string(),
        ticket: Some(key.to_string()),
        kind: resume::HUMAN.into(),
        object: key.to_string(),
        value: Some(step.to_string()),
        reason: cleared.reason.clone(),
        seconds: cleared.seconds,
    }
}

/// The detail of the event `resumed`: what the rules found and the head, with the flags that
/// the continue cleared, the failed operations that it released, and the new visit.
fn resumed_detail(
    resumed: &Resumed,
    from: &str,
    step: &str,
    how: &str,
    cleared: &Cleared,
    released: usize,
    visit: Option<&Visit>,
) -> Value {
    let mut detail = resumed.detail(from, step, how);
    if let Value::Object(fields) = &mut detail {
        fields.insert("cleared".into(), json!(cleared.flags));
        fields.insert("released".into(), json!(released));
        fields.insert("n".into(), json!(visit.map(|v| v.n)));
        fields.insert("seq".into(), json!(visit.map(|v| v.seq)));
    }
    detail
}

// ----- plan and approve-plan -------------------------------------------------------------

/// The plan of a ticket: its text, its hash, and whether it waits for an approval now.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct PlanView {
    pub text: String,
    /// The full hash: sha256 of the bytes of `plan.md`.
    pub hash: String,
    /// True when the person edited `plan.md` after the agent wrote it.
    pub edited: bool,
    /// True when the step of the ticket waits for the approval of this plan.
    pub waits: bool,
}

/// The plan of a ticket, or `None` when its plan step has not written `plan.md` yet.
pub fn plan_of(ctx: &Ctx, key: &TicketKey) -> Result<Option<PlanView>> {
    ctx.require_setup()?;
    let path = ctx.paths.ticket_doc(key, "plan.md");
    let Ok(bytes) = std::fs::read(&path) else {
        return Ok(None);
    };
    let hash = plan_hash(&bytes);
    let edited =
        std::fs::read(ctx.paths.ticket_doc(key, "plan.agent.md")).is_ok_and(|agent| agent != bytes);
    let ledger = ctx.ledger()?;
    let waits = match ledger.ticket(key.as_str())? {
        Some(row) => Ticket::of(row)?.visit().action.as_deref() == Some("approve-plan"),
        None => false,
    };
    Ok(Some(PlanView {
        text: String::from_utf8_lossy(&bytes).into_owned(),
        hash,
        edited,
        waits,
    }))
}

/// `vision plan KEY`.
pub fn plan(ctx: &Ctx, key: &TicketKey) -> Result<()> {
    let Some(plan) = plan_of(ctx, key)? else {
        bail!("{key} has no plan yet: the plan step writes plan.md. `vision status` shows where the ticket is");
    };
    let hash = &plan.hash;
    let text = &plan.text;
    println!(
        "The plan of {key}, hash {} (in full: {hash}):\n",
        &hash[..SHORT_HASH]
    );
    print!("{text}");
    if !text.ends_with('\n') {
        println!();
    }
    if plan.edited {
        println!("\nYou edited plan.md after the agent wrote it. The first text of the agent stays in plan.agent.md.");
    }
    if plan.waits {
        println!(
            "\nApprove it with: vision approve-plan {key} {}",
            &hash[..SHORT_HASH]
        );
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
    // A job of the ticket in the background stops too: it stores no result and runs again
    // when the ticket goes on.
    let jobs = crate::executor::runs::stop(&ctx.paths, Some(key.as_str()));
    let seconds = ticket
        .counts
        .reasons
        .get(NEEDS_YOU)
        .and_then(|r| seconds_since(&r.at));
    let now = time::now_rfc3339();
    // A ticket in the queue of the slots leaves it.
    let queued = ticket.flags.iter().any(|f| f == WAITS_FOR_SLOT);
    ticket.flags.retain(|f| f != WAITS_FOR_SLOT);
    ticket.counts.reasons.remove(WAITS_FOR_SLOT);
    ticket.flags.push(STOPPED.into());
    ticket.counts.reasons.insert(
        STOPPED.into(),
        FlagReason {
            reason: "you stopped it with vision stop".into(),
            at: now.clone(),
            by: None,
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
                .detail(json!({"step": ticket.state(), "killed": killed, "jobs": jobs})),
        )?;
        Ok(())
    })?;
    ctx.log(&format!("stop: {key} at the step {}", ticket.state()));
    let place = match ticket.row.clone.as_deref().filter(|c| !c.trim().is_empty()) {
        Some(clone) => {
            format!(" The branch stays in the clone {clone}, which other tickets can use now.")
        }
        None if queued => " It leaves the queue of the slots.".to_string(),
        None => " It holds no slot.".to_string(),
    };
    println!(
        "Stopped {key} at the step {}.{}{place} Run `vision take {key}` to start it again.",
        ticket.state(),
        match (killed.is_empty(), jobs.is_empty()) {
            (true, true) => String::new(),
            (false, true) => format!(" Killed the turn(s) {}.", killed.join(", ")),
            (true, false) => format!(" Stopped the job(s) {}.", jobs.join(", ")),
            (false, false) => format!(
                " Killed the turn(s) {}, and stopped the job(s) {}.",
                killed.join(", "),
                jobs.join(", ")
            ),
        }
    );
    Ok(())
}

/// The word of a move by the person with `vision retry KEY --step STEP`: the way of its visit,
/// the reason of its span, and the kind of its event and of its `human` row.
pub const MOVE: &str = "move";

/// The flags that a retry, a move, or a continue clears, with the reason of `needs-you`, and
/// the seconds since the older of the flags was set.
struct Cleared {
    flags: Vec<String>,
    reason: Option<String>,
    seconds: Option<i64>,
}

/// Clears the flags of `names` that the ticket has, and their reasons. A plain retry and a
/// move clear `needs-you` and `paused`, and a continue of a stopped ticket clears the stop too.
fn clear(ticket: &mut Ticket, names: &[&str]) -> Cleared {
    let flags: Vec<String> = ticket
        .flags
        .iter()
        .filter(|f| names.contains(&f.as_str()))
        .cloned()
        .collect();
    let reason = names
        .contains(&NEEDS_YOU)
        .then(|| {
            ticket
                .counts
                .reasons
                .get(NEEDS_YOU)
                .map(|r| r.reason.clone())
        })
        .flatten();
    let seconds = flags
        .iter()
        .filter_map(|f| ticket.counts.reasons.get(f))
        .filter_map(|r| seconds_since(&r.at))
        .max();
    ticket.flags.retain(|f| !names.contains(&f.as_str()));
    for flag in &flags {
        ticket.counts.reasons.remove(flag);
    }
    Cleared {
        flags,
        reason,
        seconds,
    }
}

/// The step STEP of `--step`: a step of the adapter.
pub(super) fn known_step(adapter: &Adapter, name: &str) -> Result<Step> {
    let Some(step) = adapter.step(name) else {
        let steps: Vec<&str> = adapter
            .steps
            .iter()
            .filter(|s| !matches!(s.kind, StepKind::End))
            .map(|s| s.id.as_str())
            .collect();
        bail!(
            "the adapter {} has no step `{name}`. Move the ticket to one of its steps: {}",
            adapter.name,
            steps.join(", ")
        );
    };
    Ok(step.clone())
}

/// Only the pipeline moves a ticket to the end step: the path itself, or a `[[resume]]` rule
/// that suggests it now, such as for a merged pull request.
fn refuse_end(
    adapter: &Adapter,
    key: &TicketKey,
    step: &Step,
    suggested: Option<&str>,
) -> Result<()> {
    if matches!(step.kind, StepKind::End) && suggested != Some(step.id.as_str()) {
        bail!(
            "`{}` is the end step of the adapter {}, and only the pipeline moves a ticket there. To stop the ticket, run `vision stop {key}`",
            step.id,
            adapter.name
        );
    }
    Ok(())
}

/// The refusals of a retry and of a move: an operation that the reconcile step of the tick
/// must settle first, a turn that runs, and cards that wait for an answer.
fn refuse_busy(ctx: &Ctx, ledger: &Ledger, key: &TicketKey) -> Result<()> {
    for op in executor::operations_of(ledger, key)? {
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
    if let Some(running) = crate::turn::running_turn_of(ctx, ledger, key)? {
        bail!(
            "the turn {} of {key} runs now. Wait for it, or run `vision stop {key}`",
            running.id
        );
    }
    let open = ledger.open_cards(key.as_str())?;
    if open > 0 {
        bail!("{open} card(s) of {key} wait for an answer. Answer them with `vision needs` first");
    }
    Ok(())
}

/// `vision retry KEY [--step STEP]`. With a STEP that is not the current step, the ticket moves
/// to that step (see [`move_ticket`]). A STEP that is the current step is a plain retry. A
/// paused ticket that the person continues with a STEP runs the `[[resume]]` rules first: the
/// head of the pull request, and the event `resumed`. A plain retry runs no rule.
pub fn retry(ctx: &Ctx, key: &TicketKey, to: Option<&str>, new_branch: bool) -> Result<()> {
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
    let target = to.map(|name| known_step(&adapter, name)).transpose()?;
    refuse_busy(ctx, &ledger, key)?;
    let Some(step) = adapter.step(ticket.state()).cloned() else {
        bail!(
            "{key} is at the step {}, which is not in the adapter {}. Fix adapter.toml, or run `vision stop {key}`",
            ticket.state(),
            adapter.name
        );
    };
    let paused = ticket.flags.iter().any(|f| f == PAUSED);
    if new_branch {
        // As a plain retry, it ends a pause: the person is done in the session. The next turn
        // starts a new session, because the old one knows only the old branch.
        let target = target.unwrap_or_else(|| step.clone());
        refuse_end(&adapter, key, &target, None)?;
        let text = new_branch_move(ctx, &ledger, &adapter, ticket, &target)?;
        println!("{text}");
        return Ok(());
    }
    // The person continues a paused ticket at a named step: the rules run, with one call of
    // observe, before anything changes.
    let checked = match (&target, paused) {
        (Some(_), true) => resume::check(ctx, &ledger, &adapter, &ticket, false),
        _ => None,
    };
    if let Some(target) = &target {
        refuse_end(
            &adapter,
            key,
            target,
            checked.as_ref().and_then(Checked::suggested),
        )?;
    }
    let resumed = match &checked {
        Some(checked) => {
            let again = format!(
                "run `vision retry {key} --step {}` again",
                to.unwrap_or(&step.id)
            );
            Some(head_for_continue(ctx, &ledger, &ticket, checked, &again)?)
        }
        None => None,
    };
    if let Some(target) = target.filter(|t| t.id != step.id) {
        let text = move_ticket(ctx, &ledger, ticket, &target, resumed.as_ref(), None)?;
        println!("{text}");
        return Ok(());
    }
    let released = executor::release_failed(&ledger, key)?;
    let cleared = clear(&mut ticket, &[NEEDS_YOU, PAUSED]);
    // A step whose turn result held and that stopped at an operation goes on with its `after`
    // verbs: a new turn would redo work that is done.
    let visit = ticket.visit();
    // A team step keeps the members whose end facts held: only the others start again.
    let mut team_held: Vec<String> = Vec::new();
    if visit.step == step.id && !paused {
        for member in step.team() {
            let turns =
                super::machine::agent_turns(&ledger, &ticket, &step.id, &member.agent, true)?;
            if visit.team_held.contains(&member.agent)
                || turns
                    .last()
                    .is_some_and(|t| t.end_fact.as_deref() == Some("held"))
            {
                team_held.push(member.agent.clone());
            }
        }
    }
    let held = match &step.kind {
        StepKind::Turn(turn) if !turn.team.is_empty() => {
            !paused && turn.team.iter().all(|m| team_held.contains(&m.agent))
        }
        StepKind::Turn(_) => {
            !paused
                && visit.step == step.id
                && visit_turns(&ledger, key, &step.id, visit.turns)?
                    .last()
                    .is_some_and(|t| t.end_fact.as_deref() == Some("held"))
        }
        _ => false,
    };
    let now = time::now_rfc3339();
    let mut new_visit = None;
    if !held {
        let ids = ledger.step_turn_ids(key.as_str(), &step.id)?;
        let turns = u32::try_from(ids.len())?;
        let head = ticket.clone_dir().ok().and_then(|c| head_of(&c).ok());
        let mut next = ticket
            .counts
            .enter(&step, "retry", &now, turns, head, false);
        next.team_held = team_held.clone();
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
            } else if let Some(reason) = &cleared.reason {
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
        record_pr(ledger, key, resumed.as_ref(), &now)?;
        if new_visit.is_some() {
            ledger.close_spans(key.as_str(), &now)?;
            ledger.open_span(&span(&ticket, &step, "retry", &now))?;
        }
        match &resumed {
            Some(resumed) => {
                ledger.add_human(&resume_row(key, &step.id, &cleared, &now))?;
                ledger.add_event(&Event::now("user", RESUMED).ticket(key.as_str()).detail(
                    resumed_detail(
                        resumed,
                        &step.id,
                        &step.id,
                        "retry",
                        &cleared,
                        released,
                        new_visit.as_ref(),
                    ),
                ))?;
            }
            None => {
                ledger.add_human(&HumanRow {
                    at: now.clone(),
                    ticket: Some(key.to_string()),
                    kind: "retry".into(),
                    object: key.to_string(),
                    value: Some(step.id.clone()),
                    reason: cleared.reason.clone(),
                    seconds: cleared.seconds,
                })?;
                ledger.add_event(&Event::now("user", "retry").ticket(key.as_str()).detail(
                    json!({
                        "step": step.id, "cleared": cleared.flags, "released": released,
                        "new_visit": new_visit.as_ref().map(|v| v.seq),
                    }),
                ))?;
            }
        }
        Ok(())
    })?;
    ctx.log(&format!("retry: {key} at the step {}", step.id));
    let mut text = format!("{key} goes on at the step {}", step.id);
    if !cleared.flags.is_empty() {
        text.push_str(&format!(" (cleared: {})", cleared.flags.join(", ")));
    }
    if released > 0 {
        text.push_str(&format!(". {released} failed operation(s) can run again"));
    }
    if held {
        text.push_str(
            ". Its turn result held, so the next tick runs the operations of the step again",
        );
    } else if matches!(step.kind, StepKind::Act(_)) {
        text.push_str(
            ". The step runs no turn, so the next tick runs the operations of the step again",
        );
    } else if paused && new_visit.as_ref().is_some_and(|v| v.resume.is_some()) {
        text.push_str(". The next turn resumes the session that you worked in");
    } else {
        text.push_str(". The next tick enters the step again");
    }
    if let Some(resumed) = &resumed {
        for line in resumed.lines(&step.id) {
            text.push_str(&format!(". {line}"));
        }
    }
    println!("{text}.");
    Ok(())
}

/// `vision retry KEY --new-branch`: the ticket enters the step on a new branch, the branch of the
/// pattern of the adapter, made from the base ref after a fetch. The old branch stays in git, and
/// the row drops the number of its pull request. A ticket with no clone takes a slot first.
fn new_branch_move(
    ctx: &Ctx,
    ledger: &Ledger,
    adapter: &Adapter,
    mut ticket: Ticket,
    target: &Step,
) -> Result<String> {
    let key = ticket.key.clone();
    if !ticket.has_clone() {
        slots::claim(ctx, ledger, adapter, &mut ticket)?;
    }
    let clone = ticket.clone_dir()?;
    let clone_text = clone.display().to_string();
    let old = ticket.row.branch.clone().unwrap_or_default();
    let branch = adapter.branch_for(&key);
    if branch == old {
        bail!(
            "{key} is on the branch {branch} already, the branch of the pattern of the adapter, so there is no new branch to make. Run `vision retry {key} --step {}` to move it on this branch",
            target.id
        );
    }
    let dirty = dirty_lines(&clone)?;
    if !dirty.is_empty() {
        bail!(
            "the clone {clone_text} has {} change(s) in its work tree, such as `{}`, so its branch cannot change. Commit or clean them first",
            dirty.len(),
            dirty[0].trim()
        );
    }
    git::run(&clone, &["fetch", "--quiet", "--all", "--prune"])
        .with_context(|| format!("could not fetch in the clone {clone_text}"))?;
    let base = &adapter.base;
    let at = |reference: String| {
        git::run(&clone, &["rev-parse", "--verify", "--quiet", &reference])
            .ok()
            .filter(|s| !s.trim().is_empty())
    };
    if at(format!("{base}^{{commit}}")).is_none() {
        bail!(
            "the base ref {base} of the adapter is missing in the clone {clone_text}. Fetch it there, or fix base in adapter.toml"
        );
    }
    if let Some(tip) = at(format!("refs/heads/{branch}")) {
        bail!(
            "the clone {clone_text} has a branch {branch} already, at {}. If it holds nothing that you need, delete it with `git -C {clone_text} branch -D {branch}`, then run this again",
            short(&tip)
        );
    }
    let remote = base_remote(&clone, adapter).unwrap_or_else(|| "origin".to_string());
    if at(format!("refs/remotes/{remote}/{branch}")).is_some() {
        bail!(
            "{remote} has a branch {branch} already, so the new branch would meet it at its first push. Look at that branch, and delete it on {remote} if nobody needs it, then run this again"
        );
    }
    git::run(
        &clone,
        &["switch", "--quiet", "--no-track", "-c", &branch, base],
    )
    .map_err(|e| {
        switch_error(
            e,
            &clone,
            &branch,
            &format!("vision retry {key} --step {} --new-branch", target.id),
            format!("could not make the branch {branch} from {base} in the clone {clone_text}"),
        )
    })?;
    let head = head_of(&clone)?;
    let now = time::now_rfc3339();
    let pr = ticket.row.pr;
    in_tx(ledger, |ledger| {
        ledger.set_ticket_branch(key.as_str(), &branch, &head, &now)?;
        ledger.add_event(
            &Event::now("user", "branch")
                .ticket(key.as_str())
                .detail(json!({
                    "from": old, "to": branch, "base": head, "clone": clone_text, "pr": pr,
                })),
        )?;
        Ok(())
    })?;
    ticket.row.branch = Some(branch.clone());
    ticket.row.head = Some(head.clone());
    ticket.row.pr = None;
    ctx.log(&format!(
        "retry: {key} moved from the branch {old} to the new branch {branch} from {base} ({})",
        short(&head)
    ));
    let note = format!(
        "The ticket runs on a new branch now: {branch}, made from {base} at {}. The old branch {old} stays in git, but it is no longer the branch of the ticket, so build nothing on it.",
        short(&head)
    );
    let mut text = format!(
        "{key} is on the new branch {branch} from {base} ({}) in the clone {clone_text}, and the old branch {old} stays in git. ",
        short(&head)
    );
    if let Some(n) = pr {
        text.push_str(&format!(
            "The pull request #{n} of the old branch stays open: close it if nobody needs it. "
        ));
    }
    text.push_str(&move_ticket(
        ctx,
        ledger,
        ticket,
        target,
        None,
        Some(&note),
    )?);
    Ok(text)
}

/// The message of the first turn of a turn step after the person moved the ticket there.
fn moved_message(from: &str) -> String {
    format!(
        "The person moved the ticket to this step from the step {from} with `vision retry --step`. Read the state of the clone first (git status, git log, and the files that changed), then do the step."
    )
}

/// The move of `vision retry KEY --step STEP`, and of `vision take KEY --step STEP` of a stopped
/// ticket, to a step that is not the current step. It has the rules of a plain retry: the
/// failed operations of the ticket can run again, the flags `needs-you` and `paused` clear,
/// and the new visit does not count as an entry for the cap of its step. A take also clears
/// the stop. The visit resumes no session, and no turn or team member of an earlier visit
/// counts for it, so the next tick acts on the step as after a move of the pipeline into it.
/// The new state, the spans, the `human` row, and the event are one transaction: the event
/// `move`, or `resumed` when the rules ran. Gives the text for the person.
fn move_ticket(
    ctx: &Ctx,
    ledger: &Ledger,
    mut ticket: Ticket,
    target: &Step,
    resumed: Option<&Resumed>,
    note: Option<&str>,
) -> Result<String> {
    let key = ticket.key.clone();
    let from = ticket.state().to_string();
    let released = executor::release_failed(ledger, &key)?;
    let cleared = clear(&mut ticket, &[STOPPED, NEEDS_YOU, PAUSED]);
    let now = time::now_rfc3339();
    let turns = u32::try_from(ledger.step_turn_ids(key.as_str(), &target.id)?.len())?;
    let head = ticket.clone_dir().ok().and_then(|c| head_of(&c).ok());
    let mut visit = ticket.counts.enter(target, MOVE, &now, turns, head, false);
    if matches!(target.kind, StepKind::Turn(_)) {
        let mut message = moved_message(&from);
        if let Some(note) = note {
            message.push(' ');
            message.push_str(note);
        }
        visit.message = Some(message);
        ticket.counts.visit = Some(visit.clone());
    }
    ticket.row.state = Some(target.id.clone());
    in_tx(ledger, |ledger| {
        ledger.set_ticket_state(
            key.as_str(),
            &target.id,
            &flags_text(&ticket.flags),
            &ticket.counts.text(),
            &now,
        )?;
        record_pr(ledger, &key, resumed, &now)?;
        ledger.close_spans(key.as_str(), &now)?;
        ledger.open_span(&span(&ticket, target, MOVE, &now))?;
        match resumed {
            Some(resumed) => {
                ledger.add_human(&resume_row(&key, &target.id, &cleared, &now))?;
                ledger.add_event(&Event::now("user", RESUMED).ticket(key.as_str()).detail(
                    resumed_detail(
                        resumed,
                        &from,
                        &target.id,
                        MOVE,
                        &cleared,
                        released,
                        Some(&visit),
                    ),
                ))?;
            }
            None => {
                ledger.add_human(&HumanRow {
                    at: now.clone(),
                    ticket: Some(key.to_string()),
                    kind: MOVE.into(),
                    object: key.to_string(),
                    value: Some(target.id.clone()),
                    reason: cleared.reason.clone(),
                    seconds: cleared.seconds,
                })?;
                ledger.add_event(&Event::now("user", MOVE).ticket(key.as_str()).detail(json!({
                    "from": from, "to": target.id, "cleared": cleared.flags, "released": released,
                    "n": visit.n, "seq": visit.seq,
                })))?;
            }
        }
        Ok(())
    })?;
    ctx.log(&format!("retry: {key} moved from {from} to {}", target.id));
    let mut text = format!("Moved {key} from the step {from} to the step {}", target.id);
    if !cleared.flags.is_empty() {
        text.push_str(&format!(" (cleared: {})", cleared.flags.join(", ")));
    }
    if released > 0 {
        text.push_str(&format!(". {released} failed operation(s) can run again"));
    }
    if let Some(resumed) = resumed {
        for line in resumed.lines(&target.id) {
            text.push_str(&format!(". {line}"));
        }
    }
    if target.cap.is_some() {
        text.push_str(&format!(
            ". The move does not count for the cap of {}",
            target.id
        ));
    }
    text.push_str(&format!(". The next tick enters the step {}", target.id));
    Ok(format!("{text}."))
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
    /// The facts of the line for the app, which says them in its own words: the actor of an
    /// event, the agent of a turn or of a card, and the detail as JSON. `vision log` does not
    /// print them.
    #[serde(skip)]
    pub who: String,
    #[serde(skip)]
    pub detail: Value,
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

/// Whom a message of the person went to, from the detail of its event `told`: the agent of
/// the running turn, `the running turn` for a turn with no agent, or `the next turn`.
pub fn told_to(detail: &Value) -> String {
    let text = |key: &str| detail.get(key).and_then(Value::as_str);
    match (text("to"), text("turn")) {
        (Some(agent), _) if !agent.trim().is_empty() => agent.trim().to_string(),
        (_, Some(_)) => "the running turn".to_string(),
        _ => "the next turn".to_string(),
    }
}

/// The timeline of one ticket, oldest first. A message that the person sent with `vision tell`
/// is the event `told`.
pub fn timeline(ledger: &Ledger, key: &TicketKey) -> Result<Vec<LogEntry>> {
    let mut out: Vec<LogEntry> = ledger
        .events_of_ticket(key.as_str())?
        .iter()
        .filter_map(event_entry)
        .collect();
    out.extend(other_entries(ledger, key)?);
    sort_timeline(&mut out);
    Ok(out)
}

/// One event of a ticket as a line of its timeline, or `None` for the start and the end of a
/// turn, which the rows of `turns` give. An event never changes after its write, so a reader
/// can keep its line.
pub fn event_entry(e: &EventRow) -> Option<LogEntry> {
    if matches!(e.kind.as_str(), "turn-started" | "turn-ended") {
        return None;
    }
    let detail: Value = e
        .detail
        .as_deref()
        .and_then(|d| serde_json::from_str(d).ok())
        .unwrap_or(Value::Null);
    let field = |name: &str| detail.get(name).and_then(Value::as_str).unwrap_or("-");
    let text = match e.kind.as_str() {
        "skipped" => format!("skipped {}: {}", field("step"), cut(field("why"), 240)),
        "fallback" => format!("fallback {}: {}", field("agent"), cut(field("why"), 240)),
        "kind" => format!("{} kind {}", e.actor, field("kind")),
        "branch" => format!(
            "{} moved the ticket from the branch {} to the new branch {}",
            e.actor,
            field("from"),
            field("to")
        ),
        "slot" if detail.get("gave").is_some() => format!(
            "{} gave the clone {} to {}: the ticket waits in the background at the step {}",
            e.actor,
            field("gave"),
            field("to"),
            field("step")
        ),
        "slot" => format!(
            "{} took the clone {} at the step {}",
            e.actor,
            field("took"),
            field("step")
        ),
        "queued" => format!(
            "{} queued the ticket at the step {}: no slot was free, and it was {} in line",
            e.actor,
            field("step"),
            detail
                .get("place")
                .and_then(Value::as_u64)
                .and_then(|n| usize::try_from(n).ok())
                .map_or_else(|| "-".to_string(), slots::ordinal)
        ),
        MOVE => format!(
            "{} moved the ticket from {} to {}",
            e.actor,
            field("from"),
            field("to")
        ),
        RESUMED => format!("{} {}", e.actor, resume::resumed_words(&detail)),
        "told" => format!(
            "{} told {}: {}",
            e.actor,
            told_to(&detail),
            cut(field("text"), 240)
        ),
        _ => format!(
            "{} {}{}",
            e.actor,
            e.kind,
            e.detail
                .as_deref()
                .map(|d| format!(" {}", cut(d, 240)))
                .unwrap_or_default()
        ),
    };
    Some(LogEntry {
        who: e.actor.clone(),
        detail,
        at: e.at.clone(),
        source: "event".into(),
        kind: e.kind.clone(),
        id: e.external_id.clone(),
        text,
    })
}

/// The lines of the turns, the cards, and the operations of a ticket, in that order and not
/// sorted. These rows change after their write, such as a turn that ends, so a reader reads
/// them again.
pub fn other_entries(ledger: &Ledger, key: &TicketKey) -> Result<Vec<LogEntry>> {
    let mut out = Vec::new();
    for t in ledger.turns_of_ticket(key.as_str())? {
        let agent = t.agent.clone().unwrap_or_default();
        let step = t.step.clone().unwrap_or_default();
        let detail = json!({
            "step": step, "model": t.model, "effort": t.effort, "resumes": t.resumes.is_some(),
            "status": t.status, "reason": t.reason, "cost": t.cost_usd, "end_fact": t.end_fact,
        });
        if let Some(started) = &t.started {
            out.push(LogEntry {
                who: agent.clone(),
                detail: detail.clone(),
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
                who: agent.clone(),
                detail: detail.clone(),
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
        let asked_by = match ledger.question(&c.id)? {
            Some(q) => ledger.turn(&q.turn)?.and_then(|t| t.agent),
            None => None,
        }
        .or_else(|| {
            card.get("asked_by")
                .and_then(Value::as_str)
                .map(str::to_string)
        })
        .unwrap_or_default();
        if let Some(created) = &c.created {
            out.push(LogEntry {
                who: asked_by.clone(),
                detail: json!({"question": question}),
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
                who: asked_by.clone(),
                detail: json!({
                    "question": question, "outcome": c.outcome, "basis": c.basis,
                    "answer": answer.get("text").and_then(Value::as_str),
                }),
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
            who: "vision".into(),
            detail: json!({
                "verb": op.verb, "head": short(&op.head), "outcome": op.outcome.to_string(),
                "detail": op.detail,
            }),
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
    Ok(out)
}

/// The place of a line in one second of the timeline. Times have whole seconds. In one second,
/// a turn ends, the tick acts on it, and the next turn starts: that order is kept.
pub fn timeline_rank(e: &LogEntry) -> u8 {
    match (e.source.as_str(), e.kind.as_str()) {
        ("turn", "ended") => 0,
        ("card", _) => 1,
        ("event", _) => 2,
        ("operation", _) => 3,
        _ => 4,
    }
}

/// Sorts the lines of a timeline by time, then by [`timeline_rank`]. The sort is stable: lines
/// that tie keep their order.
pub fn sort_timeline(lines: &mut [LogEntry]) {
    lines.sort_by(|a, b| {
        a.at.cmp(&b.at)
            .then(timeline_rank(a).cmp(&timeline_rank(b)))
    });
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
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct DiffView {
    pub ticket: String,
    pub branch: String,
    pub clone: String,
    /// The ref of the forge that the commits are compared with.
    pub compared_with: String,
    pub commits: Vec<DiffCommit>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct DiffCommit {
    pub commit: String,
    pub subject: String,
}

/// The commits of the branch of a ticket that the forge does not have yet. With `fetch`, it
/// fetches the remote first; without it, it compares with the refs that the clone has, which
/// needs no network.
pub fn diff_of(ctx: &Ctx, key: &TicketKey, fetch: bool) -> Result<DiffView> {
    ctx.require_setup()?;
    let ledger = ctx.ledger()?;
    diff_with(ctx, &ledger, key, fetch)
}

/// [`diff_of`] with the connection of the caller, such as the one that the loader of the app
/// keeps.
pub fn diff_with(ctx: &Ctx, ledger: &Ledger, key: &TicketKey, fetch: bool) -> Result<DiffView> {
    let adapter = adapter::load(ctx)?;
    let ticket = ticket_of(ledger, key)?;
    let clone = ticket
        .clone_dir()
        .with_context(|| format!("{key} holds no clone now, so there is no branch to compare"))?;
    let branch = ticket
        .row
        .branch
        .clone()
        .unwrap_or_else(|| adapter.branch_for(key));
    let remote = base_remote(&clone, &adapter);
    if let (Some(remote), true) = (&remote, fetch) {
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
    Ok(DiffView {
        ticket: key.to_string(),
        branch,
        clone: clone.display().to_string(),
        compared_with,
        commits,
    })
}

/// `vision diff KEY [--json]`: the commits of the branch that the forge does not have yet.
pub fn diff(ctx: &Ctx, key: &TicketKey, json: bool) -> Result<()> {
    let view = diff_of(ctx, key, true)?;
    let branch = &view.branch;
    let compared_with = &view.compared_with;
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
    /// The kind of the ticket, such as `bug`. `None` in total, and for a ticket with no kind.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
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

/// The turns and the cost of one agent in the tickets of the window.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct AgentMeasure {
    pub agent: String,
    /// `vision` for an agent of Vision, `adapter` for an agent that an adapter defines.
    pub source: String,
    pub turns: i64,
    pub cost_usd: f64,
}

/// What `vision report --json` prints.
#[derive(Clone, Debug, Serialize)]
pub struct Report {
    /// The start of the window, or `None` for all time.
    pub since: Option<String>,
    pub tickets: BTreeMap<String, Measures>,
    pub total: Measures,
    /// Each agent with turns in the tickets of the window: the agents of Vision in the order
    /// of section 10, then the agents of an adapter by name.
    pub agents: Vec<AgentMeasure>,
    /// The steps of the measures in the order of the step list of the adapter, then each step
    /// that the adapter no longer has, by name.
    pub steps: Vec<String>,
    /// The escape check (section 11.6): the checked pull requests and the escapes of the
    /// window.
    pub escapes: crate::escapes::EscapeMeasures,
    /// The priorities that `[escapes]` counts, in its order, for the text.
    #[serde(skip)]
    pub counted: Vec<String>,
}

/// The measures of each ticket and in total.
pub fn report_of(ledger: &Ledger, since: Option<&str>) -> Result<Report> {
    report_at(ledger, since, time::now())
}

/// True for a ticket that the report does not count: a reserved key, or a text that is no key.
fn reserved_ticket(ticket: &str) -> bool {
    TicketKey::parse(ticket).map_or(true, |k| k.is_reserved())
}

/// Adds one card to the measures of its ticket by its outcome and its basis.
fn count_card(m: &mut Measures, outcome: Option<&str>, basis: Option<&str>) {
    match (outcome, basis) {
        (Some("propose" | "ask"), _) => m.cards_to_person += 1,
        (Some("answer"), Some("decision" | "principle")) => m.cards_by_memory += 1,
        (Some("answer"), _) => m.cards_by_judge += 1,
        _ => {}
    }
}

/// The seconds of one span, from the start of the window `since` when it started before it,
/// to its end or to `now`. `None` for a span whose times do not parse, or that ended before
/// the window.
fn span_seconds(
    span: &crate::ledger::pipeline::SpanRow,
    since: Option<&str>,
    now: ::time::OffsetDateTime,
) -> Option<i64> {
    let (Ok(start), end) = (
        time::parse_rfc3339(&span.started),
        span.ended
            .as_deref()
            .and_then(|e| time::parse_rfc3339(e).ok())
            .unwrap_or(now),
    ) else {
        return None;
    };
    let start = match since.and_then(|s| time::parse_rfc3339(s).ok()) {
        Some(window) if window > start => window,
        _ => start,
    };
    (end >= start).then(|| time::seconds_between(start, end))
}

/// The report with `now` as the end of each span that is still open.
pub fn report_at(
    ledger: &Ledger,
    since: Option<&str>,
    now: ::time::OffsetDateTime,
) -> Result<Report> {
    let mut tickets: BTreeMap<String, Measures> = BTreeMap::new();
    let reserved = reserved_ticket;
    let after = |at: Option<&str>| since.is_none_or(|s| at.is_some_and(|a| a >= s));
    for c in ledger.cards_of(None)? {
        if reserved(&c.ticket) || !after(c.created.as_deref()) {
            continue;
        }
        let m = tickets.entry(c.ticket.clone()).or_default();
        count_card(m, c.outcome.as_deref(), c.basis.as_deref());
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
    for s in ledger.spans_of(None)? {
        if reserved(&s.ticket) {
            continue;
        }
        let Some(seconds) = span_seconds(&s, since, now) else {
            continue;
        };
        *tickets
            .entry(s.ticket.clone())
            .or_default()
            .step_seconds
            .entry(s.state.clone())
            .or_default() += seconds;
    }
    for c in ledger.step_costs(since)? {
        if reserved(&c.ticket) {
            continue;
        }
        let m = tickets.entry(c.ticket.clone()).or_default();
        *m.step_cost_usd.entry(c.step.clone()).or_default() += c.cost_usd;
        m.cost_usd += c.cost_usd;
    }
    for row in ledger.tickets()? {
        if let Some(m) = tickets.get_mut(&row.key) {
            m.kind = super::state::Counts::of(&row).kind;
        }
    }
    let mut total = Measures::default();
    for m in tickets.values() {
        total.add(m);
    }
    let steps = step_order(
        &[],
        total.step_seconds.keys().chain(total.step_cost_usd.keys()),
    );
    let mut by_agent: BTreeMap<String, (i64, f64)> = BTreeMap::new();
    for cost in ledger.agent_costs(since)? {
        if cost.agent.is_empty() || reserved(&cost.ticket) {
            continue;
        }
        let entry = by_agent.entry(cost.agent).or_default();
        entry.0 += cost.turns;
        entry.1 += cost.cost_usd;
    }
    let mut agents = Vec::new();
    for name in crate::models::agent_names() {
        if let Some((turns, cost_usd)) = by_agent.remove(name) {
            agents.push(AgentMeasure {
                agent: name.to_string(),
                source: crate::models::origin_of(name).to_string(),
                turns,
                cost_usd,
            });
        }
    }
    for (name, (turns, cost_usd)) in by_agent {
        agents.push(AgentMeasure {
            source: crate::models::origin_of(&name).to_string(),
            agent: name,
            turns,
            cost_usd,
        });
    }
    Ok(Report {
        since: since.map(str::to_string),
        tickets,
        total,
        agents,
        steps,
        escapes: crate::escapes::measures(ledger, None, since)?,
        counted: crate::config::Escapes::default().count,
    })
}

/// The measures of one ticket for all time, as [`report_of`] gives them for it, from the rows
/// of that ticket only: its cards, the minutes of the person, the seconds and the cost of each
/// step, and its kind. A reserved ticket, or one with no row to count, has the empty measures.
pub fn measures_of(ledger: &Ledger, key: &str) -> Result<Measures> {
    measures_at(ledger, key, time::now())
}

/// The measures of one ticket with `now` as the end of a span that is still open.
pub fn measures_at(ledger: &Ledger, key: &str, now: ::time::OffsetDateTime) -> Result<Measures> {
    let mut m = Measures::default();
    if reserved_ticket(key) {
        return Ok(m);
    }
    // The report keeps a ticket, with its kind, only when one of its rows counts.
    let mut counted = false;
    for c in ledger.cards_of(Some(key))? {
        counted = true;
        count_card(&mut m, c.outcome.as_deref(), c.basis.as_deref());
    }
    for h in ledger.human_of(Some(key))? {
        counted = true;
        m.human_minutes += h.seconds.unwrap_or(0) as f64 / 60.0;
    }
    for s in ledger.spans_of(Some(key))? {
        let Some(seconds) = span_seconds(&s, None, now) else {
            continue;
        };
        counted = true;
        *m.step_seconds.entry(s.state.clone()).or_default() += seconds;
    }
    for c in ledger.step_costs_of(key)? {
        counted = true;
        *m.step_cost_usd.entry(c.step.clone()).or_default() += c.cost_usd;
        m.cost_usd += c.cost_usd;
    }
    if counted {
        m.kind = ledger
            .ticket(key)?
            .and_then(|row| super::state::Counts::of(&row).kind);
    }
    Ok(m)
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

/// `vision report [KEY] [--week] [--json]`.
pub fn report(ctx: &Ctx, key: Option<&TicketKey>, week: bool, json: bool) -> Result<()> {
    ctx.require_setup()?;
    let ledger = ctx.ledger()?;
    let since = week.then(|| time::rfc3339(time::now() - ::time::Duration::days(7)));
    if let Some(key) = key {
        return report_ticket(ctx, &ledger, key, since.as_deref(), json);
    }
    let mut report = report_of(&ledger, since.as_deref())?;
    report.counted = ctx.config()?.escapes.count.clone();
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

/// What `vision report KEY --json` prints: the measures of one ticket, its escape checks, and
/// each escape.
#[derive(Clone, Debug, Serialize)]
pub struct TicketReport {
    pub ticket: String,
    pub since: Option<String>,
    pub measures: Measures,
    pub escapes: crate::escapes::EscapeMeasures,
    pub checks: Vec<crate::ledger::review::EscapeCheckRow>,
    pub escape_rows: Vec<crate::ledger::review::EscapeRow>,
}

/// `vision report KEY`: the measures of one ticket, and each escape of its reviews.
fn report_ticket(
    ctx: &Ctx,
    ledger: &Ledger,
    key: &TicketKey,
    since: Option<&str>,
    json: bool,
) -> Result<()> {
    let report = report_of(ledger, since)?;
    let view = TicketReport {
        ticket: key.to_string(),
        since: since.map(str::to_string),
        measures: report
            .tickets
            .get(key.as_str())
            .cloned()
            .unwrap_or_default(),
        escapes: crate::escapes::measures(ledger, Some(key.as_str()), since)?,
        checks: ledger.escape_checks_of(key.as_str())?,
        escape_rows: ledger.escapes(Some(key.as_str()), since)?,
    };
    if json {
        println!("{}", serde_json::to_string_pretty(&view)?);
        return Ok(());
    }
    print!(
        "{}",
        render_ticket_report(&view, &ctx.config()?.escapes.count)
    );
    Ok(())
}

/// The text of `vision report KEY`.
pub fn render_ticket_report(view: &TicketReport, counted: &[String]) -> String {
    let window = match &view.since {
        Some(s) => format!("since {s}"),
        None => "for all time".into(),
    };
    let m = &view.measures;
    let mut out = format!("Measures of {} {window}\n\n", view.ticket);
    out.push_str(&render_table(&[
        ["CARDS TO YOU", "BY MEMORY", "BY JUDGE", "YOUR MIN", "COST"].map(String::from),
        [
            m.cards_to_person.to_string(),
            m.cards_by_memory.to_string(),
            m.cards_by_judge.to_string(),
            format!("{:.1}", m.human_minutes),
            money(m.cost_usd),
        ],
    ]));
    out.push_str(&format!(
        "\nReview escapes: {}\n",
        crate::escapes::measures_line(&view.escapes, counted)
    ));
    let waiting = view.checks.iter().filter(|c| c.checked.is_none()).count();
    for check in view.checks.iter().filter(|c| c.checked.is_some()) {
        out.push_str(&format!(
            "  {} reviewed #{} after the pass of {}: {} comment(s), {} counted, {} escaped\n",
            check.reviewer,
            check
                .pr
                .map(|p| p.to_string())
                .unwrap_or_else(|| "-".into()),
            check.pass_turn,
            check.comments,
            check.counted,
            check.escapes
        ));
    }
    if waiting > 0 {
        out.push_str(&format!(
            "  {waiting} check(s) wait for the first review of their reviewer\n"
        ));
    }
    if view.escape_rows.is_empty() {
        out.push_str("No escape.\n");
        return out;
    }
    out.push_str("\nEscapes:\n");
    for e in &view.escape_rows {
        let place = match (&e.path, e.line) {
            (Some(p), Some(l)) => format!("{p}:{l}"),
            (Some(p), None) => p.clone(),
            _ => "-".into(),
        };
        out.push_str(&format!(
            "  P{}  {place}  {}  ({})\n",
            e.priority.as_deref().unwrap_or("-"),
            e.title.as_deref().unwrap_or(""),
            e.reviewer
        ));
        if let Some(url) = &e.url {
            out.push_str(&format!("      {url}\n"));
        }
    }
    out
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
        let name = match &m.kind {
            Some(kind) => format!("{key} ({kind})"),
            None => key.clone(),
        };
        table.push(line(&name, m));
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
    if !report.agents.is_empty() {
        out.push_str("\nBy agent:\n");
        for a in &report.agents {
            out.push_str(&format!(
                "  {:<16} {:<8} {:>4} {}  {}\n",
                a.agent,
                a.source,
                a.turns,
                if a.turns == 1 { "turn " } else { "turns" },
                money(a.cost_usd)
            ));
        }
    }
    out.push_str(&format!(
        "\nReview escapes: {}\n",
        crate::escapes::measures_line(&report.escapes, &report.counted)
    ));
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
            agents: vec![
                AgentMeasure {
                    agent: "builder".into(),
                    source: "vision".into(),
                    turns: 2,
                    cost_usd: 1.25,
                },
                AgentMeasure {
                    agent: "cr-codex".into(),
                    source: "adapter".into(),
                    turns: 1,
                    cost_usd: 0.0,
                },
            ],
            steps: Vec::new(),
            escapes: Default::default(),
            counted: Vec::new(),
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
        // Each agent shows where it comes from: Vision or the adapter.
        let at = lines.iter().position(|l| *l == "By agent:").unwrap();
        let builder: Vec<&str> = lines[at + 1].split_whitespace().collect();
        assert_eq!(
            builder,
            ["builder", "vision", "2", "turns", "$1.25"],
            "{text}"
        );
        let lane: Vec<&str> = lines[at + 2].split_whitespace().collect();
        assert_eq!(
            lane,
            ["cr-codex", "adapter", "1", "turn", "$0.00"],
            "{text}"
        );
    }
}

#[cfg(test)]
mod log_tests {
    use super::*;

    #[test]
    fn a_message_of_the_person_names_whom_it_went_to() {
        let to = |detail: Value| told_to(&detail);
        assert_eq!(to(json!({"to": "scout", "turn": "t1"})), "scout");
        assert_eq!(to(json!({"to": null, "turn": "t1"})), "the running turn");
        assert_eq!(to(json!({"to": " ", "turn": "t1"})), "the running turn");
        assert_eq!(to(json!({"to": null, "turn": null})), "the next turn");
        assert_eq!(to(Value::Null), "the next turn");
    }
}

#[cfg(test)]
mod outside_tests {
    use super::*;

    const BRANCH: &str = "feature/demo-12";
    const AGAIN: &str = "vision take DEMO-12 --step in-review";

    fn git_in(dir: &Path, args: &[&str]) -> String {
        let author = git::Author {
            name: "test",
            email: "test@example.invalid",
        };
        git::run_as(dir, args, author).unwrap()
    }

    /// A commit of one file in `dir`. Gives the commit.
    fn commit(dir: &Path, name: &str) -> String {
        std::fs::write(dir.join(name), name).unwrap();
        git_in(dir, &["add", "--all"]);
        git_in(dir, &["commit", "--quiet", "-m", name]);
        git_in(dir, &["rev-parse", "HEAD"])
    }

    /// A commit of the person on the branch of the ticket, pushed, and fetched by the clone.
    /// Gives the commit.
    fn pushed(person: &Path, clone: &Path, name: &str) -> String {
        let commit = commit(person, name);
        git_in(person, &["push", "--quiet", "origin", BRANCH]);
        git_in(clone, &["fetch", "--quiet", "--all"]);
        commit
    }

    /// The branch that the clone is on, and its head.
    fn on(clone: &Path) -> (String, String) {
        (
            git_in(clone, &["rev-parse", "--abbrev-ref", "HEAD"]),
            git_in(clone, &["rev-parse", "HEAD"]),
        )
    }

    #[test]
    fn the_clone_takes_the_branch_of_the_remote_and_keeps_its_own_commits() {
        // A bare remote with main, a checkout of the person, and the clone of Vision on main.
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let remote = root.join("remote.git");
        let person = root.join("person");
        let clone = root.join("clone");
        let path = |p: &Path| p.to_str().unwrap().to_string();
        git_in(root, &["init", "--quiet", "--bare", &path(&remote)]);
        git_in(root, &["clone", "--quiet", &path(&remote), &path(&person)]);
        commit(&person, "calc.sh");
        git_in(
            &person,
            &["push", "--quiet", "origin", "HEAD:refs/heads/main"],
        );
        git_in(&remote, &["symbolic-ref", "HEAD", "refs/heads/main"]);
        git_in(root, &["clone", "--quiet", &path(&remote), &path(&clone)]);
        git_in(&person, &["switch", "--quiet", "-c", BRANCH]);

        // A branch that the clone does not have starts at the remote branch.
        let first = pushed(&person, &clone, "a.txt");
        switch_outside(&clone, "origin", BRANCH, AGAIN).unwrap();
        assert_eq!(on(&clone), (BRANCH.to_string(), first));

        // A branch that the remote branch is ahead of moves forward to it.
        git_in(&clone, &["switch", "--quiet", "main"]);
        let second = pushed(&person, &clone, "b.txt");
        switch_outside(&clone, "origin", BRANCH, AGAIN).unwrap();
        assert_eq!(on(&clone), (BRANCH.to_string(), second));

        // A branch with commits that the remote does not have keeps them.
        let mine = commit(&clone, "c.txt");
        git_in(&clone, &["switch", "--quiet", "main"]);
        switch_outside(&clone, "origin", BRANCH, AGAIN).unwrap();
        assert_eq!(on(&clone), (BRANCH.to_string(), mine.clone()));

        // A branch that went apart from the remote stops the take, and nothing moves.
        git_in(&clone, &["switch", "--quiet", "main"]);
        pushed(&person, &clone, "d.txt");
        let apart = format!(
            "{:#}",
            switch_outside(&clone, "origin", BRANCH, AGAIN).unwrap_err()
        );
        assert!(
            apart.contains(&format!(
                "the branch {BRANCH} in the clone {} and origin/{BRANCH} went apart",
                clone.display()
            )) && apart.ends_with(&format!("then run `{AGAIN}` again")),
            "{apart}"
        );
        assert_eq!(on(&clone).0, "main");
        assert_eq!(git_in(&clone, &["rev-parse", BRANCH]), mine);

        // A branch that is neither in the clone nor on the remote stops the take.
        let gone = format!(
            "{:#}",
            switch_outside(&clone, "origin", "feature/gone", AGAIN).unwrap_err()
        );
        assert!(
            gone.contains("the branch feature/gone is neither in the clone")
                && gone.ends_with(&format!("Push it, then run `{AGAIN}` again")),
            "{gone}"
        );
    }
}
