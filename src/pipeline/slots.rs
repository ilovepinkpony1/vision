//! The slots (section 11.3). Each clone of `clones` is a slot, and a ticket that is not stopped
//! holds the slot that its row names. A ticket at a wait step that only watches needs no work
//! tree. When another ticket needs a slot and none is free, it gives its slot and stays in the
//! background, where the tick still watches it with the head of its row. The tickets that need a
//! slot form a queue: first the tickets that started, then the takes that wait for their first
//! slot. [`assign`] gives the slots in that order, at each tick and at each `vision take`.

use std::path::{Path, PathBuf};

use ::time::OffsetDateTime;
use anyhow::{bail, Context, Result};
use serde_json::json;

use super::commands::{base_remote, same_dir, start_take, switch_error};
use super::decide::OpFact;
use super::machine::{active_tickets, dirty_lines, head_of, in_tx, op_facts, save, Ticket};
use super::state::{
    flags_text, Counts, FlagReason, QueuedTake, SlotNote, NEEDS_YOU, STOPPED, WAITS_FOR_SLOT,
};
use crate::adapter::{self, Adapter, Step, StepKind};
use crate::ctx::Ctx;
use crate::executor::{short, Outcome};
use crate::ledger::{Event, Ledger, TicketRow};
use crate::paths::{expand_tilde, tilde_display, TicketKey};
use crate::tick::Pass;
use crate::util::{git, time};

/// A take of `vision take` that joins the end of the queue. It starts in the same pass when it
/// gets a slot. Else its row waits in the queue.
#[derive(Clone, Debug)]
pub(crate) struct NewTake {
    pub key: TicketKey,
    pub title: Option<String>,
    pub branch: String,
    pub pr: Option<i64>,
    pub take: QueuedTake,
}

impl NewTake {
    /// The take of a row that waits in the queue.
    fn of_row(ticket: &Ticket, take: &QueuedTake, adapter: &Adapter) -> NewTake {
        NewTake {
            key: ticket.key.clone(),
            title: ticket.row.title.clone(),
            branch: ticket
                .row
                .branch
                .clone()
                .filter(|b| !b.trim().is_empty())
                .unwrap_or_else(|| adapter.branch_for(&ticket.key)),
            pr: ticket.row.pr,
            take: take.clone(),
        }
    }
}

/// A ticket that got a slot in one pass.
#[derive(Clone, Debug)]
pub(crate) struct Took {
    pub key: String,
    pub clone: PathBuf,
    /// The ticket that gave the slot and went to the background, if one did.
    pub from: Option<String>,
    /// The line of a take that started.
    pub line: Option<String>,
}

/// What one pass of the slots did.
#[derive(Clone, Debug, Default)]
pub(crate) struct Given {
    pub took: Vec<Took>,
    /// Each ticket that waits for a slot, with its place in the queue from 1.
    pub waits: Vec<(String, usize)>,
    /// Why each slot is not free, such as `/work/app: DEMO-12 holds it (step build)`.
    pub busy: Vec<String>,
}

/// Tick step 4: gives the slots to the tickets that need one. A ticket whose move into a clone
/// fails gets the flag `needs-you`, and the other tickets still get their slots.
pub fn tick(pass: &mut Pass) -> Result<()> {
    assign(pass.ctx, &pass.ledger, pass.now, None).map(|_| ())
}

/// The place in the queue in words, such as `1st` or `22nd`.
pub fn ordinal(n: usize) -> String {
    let suffix = match (n % 10, n % 100) {
        (1, r) if r != 11 => "st",
        (2, r) if r != 12 => "nd",
        (3, r) if r != 13 => "rd",
        _ => "th",
    };
    format!("{n}{suffix}")
}

/// True when the step needs no work tree: a wait step whose `start` entries are all
/// `confirmed`, or that has none.
pub(crate) fn only_watches(
    ctx: &Ctx,
    ledger: &Ledger,
    ticket: &Ticket,
    step: &Step,
) -> Result<bool> {
    if !matches!(step.kind, StepKind::Wait(_)) {
        return Ok(false);
    }
    if step.start.is_empty() {
        return Ok(true);
    }
    let facts = op_facts(ctx, ledger, ticket, &step.start, None)?;
    Ok(facts.iter().all(|(_, fact)| {
        matches!(
            fact,
            OpFact::Known {
                outcome: Outcome::Confirmed,
                ..
            }
        )
    }))
}

/// True when the ticket can be at the step with no clone: the step only watches, or it is the
/// end step, which only reads the facts and frees the clone.
pub(crate) fn needs_no_slot(
    ctx: &Ctx,
    ledger: &Ledger,
    ticket: &Ticket,
    step: &Step,
) -> Result<bool> {
    Ok(matches!(step.kind, StepKind::End) || only_watches(ctx, ledger, ticket, step)?)
}

/// The take of a row that waits for its first slot.
fn queued_take(ticket: &Ticket) -> Option<&QueuedTake> {
    if ticket.has_clone() {
        return None;
    }
    ticket.counts.slot.as_ref().and_then(|s| s.take.as_ref())
}

/// One slot of `clones`, as one pass sees it.
struct Slot {
    path: PathBuf,
    /// The index of the ticket that holds it.
    holder: Option<usize>,
    /// No ticket holds it, and its work tree is clean: a ticket can take it now.
    free: bool,
    /// Why it is not free when no ticket of the list holds it.
    why: Option<String>,
}

/// One ticket of the queue: an index of the ticket list, or the new take.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Wants {
    Row(usize),
    New,
}

/// Gives the slots in the order of the queue. `new` is a take of `vision take`: it joins the end
/// of the queue, and an error of its own start is the error of this call, with no row of it
/// written. Each other ticket whose start or move into a clone fails gets the flag `needs-you`.
pub(crate) fn assign(
    ctx: &Ctx,
    ledger: &Ledger,
    now: OffsetDateTime,
    new: Option<&NewTake>,
) -> Result<Given> {
    let config = ctx.config()?;
    let clones = config.clone_paths()?;
    if clones.is_empty() {
        if new.is_some() {
            bail!(
                "config.toml lists no clone in `clones`. Make a clone of the project that only Vision uses, and add its absolute path to clones"
            );
        }
        return Ok(Given::default());
    }
    let adapter = adapter::load(ctx)?;
    let at = time::rfc3339(now);
    let mut tickets = active_tickets(ledger)?;
    let queue = queue_of(ctx, ledger, &adapter, &tickets, new.is_some());
    let mut slots = slots_of(&clones, &tickets);
    let mut idle: Vec<Option<bool>> = vec![None; tickets.len()];
    let mut given = Given::default();
    let mut moved = vec![false; tickets.len()];
    let mut waiting: Vec<Wants> = Vec::new();
    for wants in queue {
        if !waiting.is_empty() {
            waiting.push(wants);
            continue;
        }
        let last = match wants {
            Wants::Row(i) => tickets[i]
                .counts
                .slot
                .as_ref()
                .and_then(|s| s.last.clone()),
            Wants::New => None,
        };
        let pick = match free_slot(&slots, last.as_deref()) {
            Some(s) => Some(s),
            None => idle_slot(ctx, ledger, &adapter, &tickets, &slots, &mut idle),
        };
        let Some(s) = pick else {
            waiting.push(wants);
            continue;
        };
        let to = match wants {
            Wants::Row(i) => tickets[i].key.to_string(),
            Wants::New => new.map(|n| n.key.to_string()).unwrap_or_default(),
        };
        let mut from = None;
        if let Some(h) = slots[s].holder {
            if let Err(e) = release(ctx, ledger, &mut tickets[h], &slots[s].path, &to, &at) {
                ctx.log(&format!(
                    "slots: {} could not give the clone {}: {e:#}",
                    tickets[h].key,
                    slots[s].path.display()
                ));
                idle[h] = Some(false);
                waiting.push(wants);
                continue;
            }
            idle[h] = Some(false);
            from = Some(tickets[h].key.to_string());
            slots[s].holder = None;
        }
        let path = slots[s].path.clone();
        slots[s].free = false;
        let started = match wants {
            Wants::New => match new {
                Some(new) => start_take(ctx, ledger, &adapter, new, &path, None).map(Some),
                None => Ok(None),
            },
            Wants::Row(i) => match queued_take(&tickets[i]).cloned() {
                Some(take) => {
                    let row_take = NewTake::of_row(&tickets[i], &take, &adapter);
                    let taken = tickets[i].row.taken.clone();
                    start_take(ctx, ledger, &adapter, &row_take, &path, taken.as_deref())
                        .map(Some)
                }
                None => bring_back(ctx, ledger, &adapter, &mut tickets[i], &path, &at, true)
                    .map(|()| None),
            },
        };
        match started {
            Ok(line) => {
                if let Wants::Row(i) = wants {
                    slots[s].holder = Some(i);
                    moved[i] = true;
                } else {
                    slots[s].why = Some(format!("{}: {to} holds it", path.display()));
                }
                given.took.push(Took {
                    key: to,
                    clone: path,
                    from,
                    line,
                });
            }
            Err(e) => {
                slots[s].why = Some(format!(
                    "{}: the move of {to} into it failed",
                    path.display()
                ));
                match wants {
                    Wants::New => return Err(e),
                    Wants::Row(i) => {
                        flag_needs_you(ctx, ledger, &mut tickets[i], &format!("{e:#}"), &at)?;
                        moved[i] = true;
                    }
                }
            }
        }
    }
    for (n, wants) in waiting.iter().enumerate() {
        let place = n + 1;
        match *wants {
            Wants::Row(i) => {
                wait_flag(ctx, ledger, &mut tickets[i], place, &at)?;
                given.waits.push((tickets[i].key.to_string(), place));
            }
            Wants::New => {
                if let Some(new) = new {
                    queue_new(ctx, ledger, new, place, &at)?;
                    given.waits.push((new.key.to_string(), place));
                }
            }
        }
    }
    // A flag of the queue that no longer holds, such as for a ticket that a person moved to a
    // step that only watches, goes.
    for (i, ticket) in tickets.iter_mut().enumerate() {
        let waits = waiting.contains(&Wants::Row(i));
        if !waits && !moved[i] && ticket.flags.iter().any(|f| f == WAITS_FOR_SLOT) {
            ticket.flags.retain(|f| f != WAITS_FOR_SLOT);
            ticket.counts.reasons.remove(WAITS_FOR_SLOT);
            save(ledger, ticket, &at, None)?;
        }
    }
    for slot in &slots {
        match (slot.holder, &slot.why) {
            (Some(h), _) => given.busy.push(format!(
                "{}: {} holds it (step {})",
                slot.path.display(),
                tickets[h].key,
                tickets[h].state()
            )),
            (None, Some(why)) => given.busy.push(why.clone()),
            (None, None) => {}
        }
    }
    Ok(given)
}

/// A continue of a stopped ticket that holds no slot, or whose clone another ticket holds: a
/// free slot, else the slot of an idle ticket. The clone moves to the branch of the ticket at
/// the tip of that branch, and the row names the clone. With no slot, it stops with the reason.
pub(crate) fn claim(ctx: &Ctx, ledger: &Ledger, adapter: &Adapter, ticket: &mut Ticket) -> Result<()> {
    let config = ctx.config()?;
    let clones = config.clone_paths()?;
    let at = time::now_rfc3339();
    let mut tickets: Vec<Ticket> = active_tickets(ledger)?
        .into_iter()
        .filter(|t| t.key != ticket.key)
        .collect();
    let mut slots = slots_of(&clones, &tickets);
    let mut idle: Vec<Option<bool>> = vec![None; tickets.len()];
    let last = ticket.counts.slot.as_ref().and_then(|s| s.last.clone());
    let pick = match free_slot(&slots, last.as_deref()) {
        Some(s) => Some(s),
        None => idle_slot(ctx, ledger, adapter, &tickets, &slots, &mut idle),
    };
    let Some(s) = pick else {
        let mut text = format!("no slot is free for {} now:", ticket.key);
        for slot in &slots {
            match (slot.holder, &slot.why) {
                (Some(h), _) => text.push_str(&format!(
                    "\n  {}: {} holds it (step {})",
                    slot.path.display(),
                    tickets[h].key,
                    tickets[h].state()
                )),
                (None, Some(why)) => text.push_str(&format!("\n  {why}")),
                (None, None) => {}
            }
        }
        text.push_str(&format!(
            "\nRun `vision take {}` again when a slot is free",
            ticket.key
        ));
        bail!(text);
    };
    if let Some(h) = slots[s].holder {
        release(ctx, ledger, &mut tickets[h], &slots[s].path, ticket.key.as_str(), &at)?;
        slots[s].holder = None;
    }
    let path = slots[s].path.clone();
    bring_back(ctx, ledger, adapter, ticket, &path, &at, false)
}

/// A take of a stopped ticket that never started: it goes back to the end of the queue, and the
/// slots are given at once. Gives the line of the take.
pub(crate) fn requeue(ctx: &Ctx, ledger: &Ledger, mut ticket: Ticket) -> Result<String> {
    let now = time::now();
    let at = time::rfc3339(now);
    ticket
        .flags
        .retain(|f| f != STOPPED && f != NEEDS_YOU && f != WAITS_FOR_SLOT);
    for flag in [STOPPED, NEEDS_YOU, WAITS_FOR_SLOT] {
        ticket.counts.reasons.remove(flag);
    }
    let mut step = String::new();
    if let Some(take) = ticket.counts.slot.as_mut().and_then(|s| s.take.as_mut()) {
        take.at = at.clone();
        step = take.step.clone();
    }
    save(
        ledger,
        &ticket,
        &at,
        Some(
            Event::now("user", "take")
                .ticket(ticket.key.as_str())
                .detail(json!({"restart": true, "queued": true, "step": step})),
        ),
    )?;
    let given = assign(ctx, ledger, now, None)?;
    Ok(take_line(ticket.key.as_str(), &step, &given))
}

/// The line of a take after [`assign`]: the line of its start, or its place in the queue.
pub(crate) fn take_line(key: &str, step: &str, given: &Given) -> String {
    if let Some(took) = given.took.iter().find(|t| t.key == key) {
        let mut line = took.line.clone().unwrap_or_else(|| {
            format!(
                "{key} starts again at the step {step} in the clone {}.",
                took.clone.display()
            )
        });
        if let Some(from) = &took.from {
            line.push_str(&format!(
                " {from} gave its slot: it waits in the background, and Vision still watches it."
            ));
        }
        return line;
    }
    let place = given
        .waits
        .iter()
        .find(|(k, _)| k == key)
        .map_or(1, |(_, p)| *p);
    let mut text = format!(
        "Queued {key} at the step {step}: no slot is free, and it is {} in line. It starts when a slot is free.",
        ordinal(place)
    );
    for why in &given.busy {
        text.push_str(&format!("\n  {why}"));
    }
    text
}

/// What a take of the start card would get now: a free clone, the slot of an idle ticket, or a
/// place in the queue. It reads only: it gives no slot and fetches nothing.
pub fn next_slot(ctx: &Ctx) -> Result<String> {
    ctx.require_setup()?;
    let ledger = ctx.ledger()?;
    let config = ctx.config()?;
    let clones = config.clone_paths()?;
    if clones.is_empty() {
        bail!(
            "config.toml lists no clone in `clones`. Make a clone of the project that only Vision uses, and add its absolute path to clones"
        );
    }
    let adapter = adapter::load(ctx)?;
    let tickets = active_tickets(&ledger)?;
    let queue = queue_of(ctx, &ledger, &adapter, &tickets, false);
    let slots = slots_of(&clones, &tickets);
    let show = |p: &Path| tilde_display(p);
    if queue.is_empty() {
        if let Some(slot) = slots.iter().find(|s| s.free) {
            return Ok(format!("{} is free", show(&slot.path)));
        }
        for slot in &slots {
            let Some(h) = slot.holder else { continue };
            if is_idle(ctx, &ledger, &adapter, &tickets[h], &slot.path, false) {
                return Ok(format!(
                    "{}: {} only watches, so it gives its slot",
                    show(&slot.path),
                    tickets[h].key
                ));
            }
        }
    }
    let mut text = format!(
        "no slot is free, so the take puts it in the queue, {} in line",
        ordinal(queue.len() + 1)
    );
    for slot in &slots {
        if let Some(h) = slot.holder {
            text.push_str(&format!(
                "\n{}: {} holds it (step {})",
                show(&slot.path),
                tickets[h].key,
                tickets[h].state()
            ));
        }
    }
    bail!(text)
}

/// The queue: the tickets that started and need a slot, in the order of the entry into their
/// step, then the takes that wait, in the order of the take, then the new take.
fn queue_of(
    ctx: &Ctx,
    ledger: &Ledger,
    adapter: &Adapter,
    tickets: &[Ticket],
    new: bool,
) -> Vec<Wants> {
    let mut started = Vec::new();
    let mut takes = Vec::new();
    for (i, ticket) in tickets.iter().enumerate() {
        if ticket.has_clone() || ticket.flags.iter().any(|f| f != WAITS_FOR_SLOT) {
            continue;
        }
        if let Some(take) = queued_take(ticket) {
            takes.push((take.at.clone(), i));
            continue;
        }
        let Some(step) = adapter.step(ticket.state()) else {
            continue;
        };
        match needs_no_slot(ctx, ledger, ticket, step) {
            Ok(true) => {}
            Ok(false) => started.push((ticket.visit().at, i)),
            Err(e) => ctx.log(&format!(
                "slots: could not read whether {} needs a slot: {e:#}",
                ticket.key
            )),
        }
    }
    started.sort();
    takes.sort();
    let mut out: Vec<Wants> = started
        .into_iter()
        .chain(takes)
        .map(|(_, i)| Wants::Row(i))
        .collect();
    if new {
        out.push(Wants::New);
    }
    out
}

/// The slots in the order of `clones`: who holds each one, and whether it is free.
fn slots_of(clones: &[PathBuf], tickets: &[Ticket]) -> Vec<Slot> {
    clones
        .iter()
        .map(|clone| {
            let holder = tickets.iter().position(|t| {
                !t.flags.iter().any(|f| f == STOPPED)
                    && t.clone_dir().is_ok_and(|d| same_dir(&d, clone))
            });
            let (free, why) = match holder {
                Some(_) => (false, None),
                None if !clone.is_dir() || !git::is_repo_root(clone) => (
                    false,
                    Some(format!(
                        "{}: it is not the top folder of a git clone",
                        clone.display()
                    )),
                ),
                None => match dirty_lines(clone) {
                    Ok(dirty) if dirty.is_empty() => (true, None),
                    Ok(dirty) => (
                        false,
                        Some(format!(
                            "{}: its work tree has {} change(s), such as `{}`. Commit or clean them",
                            clone.display(),
                            dirty.len(),
                            dirty[0].trim()
                        )),
                    ),
                    Err(e) => (false, Some(format!("{}: {e:#}", clone.display()))),
                },
            };
            Slot {
                path: clone.clone(),
                holder,
                free,
                why,
            }
        })
        .collect()
}

/// The free slot for a ticket: the clone that it held last when that one is free, else the
/// first free clone.
fn free_slot(slots: &[Slot], last: Option<&str>) -> Option<usize> {
    if let Some(last) = last.and_then(|l| expand_tilde(l).ok()) {
        if let Some(s) = slots.iter().position(|s| s.free && same_dir(&s.path, &last)) {
            return Some(s);
        }
    }
    slots.iter().position(|s| s.free)
}

/// The slot of an idle ticket: of the idle tickets, the one that began to watch first.
/// `idle` keeps the answer for each ticket of the list, so that each one is tested once.
fn idle_slot(
    ctx: &Ctx,
    ledger: &Ledger,
    adapter: &Adapter,
    tickets: &[Ticket],
    slots: &[Slot],
    idle: &mut [Option<bool>],
) -> Option<usize> {
    let mut best: Option<(String, usize)> = None;
    for (s, slot) in slots.iter().enumerate() {
        let Some(h) = slot.holder else { continue };
        let is = *idle[h].get_or_insert_with(|| {
            is_idle(ctx, ledger, adapter, &tickets[h], &slot.path, true)
        });
        if !is {
            continue;
        }
        let visit = tickets[h].visit();
        let since = visit.since.unwrap_or(visit.at);
        if best.as_ref().is_none_or(|(b, _)| since < *b) {
            best = Some((since, s));
        }
    }
    best.map(|(_, s)| s)
}

/// True when the ticket can give its slot: it has no flag, no turn of it runs, its step only
/// watches, its work tree is clean, and its head is on the remote. `fetch` lets the test fetch
/// the branch when no remote ref holds the head yet.
fn is_idle(
    ctx: &Ctx,
    ledger: &Ledger,
    adapter: &Adapter,
    ticket: &Ticket,
    clone: &Path,
    fetch: bool,
) -> bool {
    if !ticket.flags.is_empty() {
        return false;
    }
    let Some(step) = adapter.step(ticket.state()) else {
        return false;
    };
    if !only_watches(ctx, ledger, ticket, step).unwrap_or(false) {
        return false;
    }
    if ledger
        .running_turns_of_ticket(ticket.key.as_str())
        .map_or(true, |n| n > 0)
    {
        return false;
    }
    if !clone.is_dir() || !dirty_lines(clone).is_ok_and(|d| d.is_empty()) {
        return false;
    }
    let Ok(head) = head_of(clone) else {
        return false;
    };
    on_remote(clone, ticket, &head, adapter, fetch)
}

/// True when the remote has the head, so that no commit stays only in the clone: the pull
/// request of the last look has it, or a remote branch of the clone holds it, also after a
/// fetch of the branch when `fetch` allows one.
fn on_remote(clone: &Path, ticket: &Ticket, head: &str, adapter: &Adapter, fetch: bool) -> bool {
    let pr_head = ticket
        .counts
        .seen
        .as_ref()
        .and_then(|s| s.pr.as_ref())
        .and_then(|p| p.head.as_deref());
    if pr_head == Some(head) {
        return true;
    }
    let held = || {
        git::run(
            clone,
            &[
                "for-each-ref",
                "--count=1",
                "--format=%(refname)",
                "--contains",
                head,
                "refs/remotes",
            ],
        )
        .is_ok_and(|t| !t.trim().is_empty())
    };
    if held() {
        return true;
    }
    if !fetch {
        return false;
    }
    let remote = base_remote(clone, adapter).unwrap_or_else(|| "origin".to_string());
    let branch = ticket
        .row
        .branch
        .clone()
        .unwrap_or_else(|| adapter.branch_for(&ticket.key));
    git::run(clone, &["fetch", "--quiet", &remote, &branch]).is_ok() && held()
}

/// An idle ticket gives its slot to `to`: its row names no clone, and its head is the head of
/// the clone. The clone keeps the branch until the next ticket switches it.
fn release(
    ctx: &Ctx,
    ledger: &Ledger,
    ticket: &mut Ticket,
    clone: &Path,
    to: &str,
    at: &str,
) -> Result<()> {
    let head = head_of(clone)?;
    let text = clone.display().to_string();
    let note = ticket.counts.slot.get_or_insert_with(SlotNote::default);
    note.last = Some(text.clone());
    note.background = Some(at.to_string());
    let key = ticket.key.as_str();
    in_tx(ledger, |ledger| {
        ledger.set_ticket_clone(key, None, at)?;
        ledger.set_ticket_values(key, None, Some(&head), at)?;
        ledger.set_ticket_state(
            key,
            ticket.state(),
            &flags_text(&ticket.flags),
            &ticket.counts.text(),
            at,
        )?;
        ledger.add_event(
            &Event::now("tick", "slot")
                .ticket(key)
                .detail(json!({"gave": text, "to": to, "head": head, "step": ticket.state()})),
        )?;
        Ok(())
    })?;
    ticket.row.clone = None;
    ticket.row.head = Some(head.clone());
    ctx.log(&format!(
        "slots: {key} gave the clone {text} to {to}, and it waits in the background at the step {} ({})",
        ticket.state(),
        short(&head)
    ));
    Ok(())
}

/// A ticket that started takes the clone: the clone switches to the branch of the ticket, and
/// the row names the clone. With `exact`, the branch must point to the head of the row, as for a
/// ticket that comes back from the background. Without it, the tip of the branch is the head,
/// as for a continue of a stopped ticket. A branch that the clone does not have starts at the
/// head of the row, after a fetch when the clone does not have that commit.
pub(crate) fn bring_back(
    ctx: &Ctx,
    ledger: &Ledger,
    adapter: &Adapter,
    ticket: &mut Ticket,
    clone: &Path,
    at: &str,
    exact: bool,
) -> Result<()> {
    let key = ticket.key.clone();
    let text = clone.display().to_string();
    let branch = ticket
        .row
        .branch
        .clone()
        .filter(|b| !b.trim().is_empty())
        .unwrap_or_else(|| adapter.branch_for(&key));
    let verify = |reference: &str| {
        git::run(clone, &["rev-parse", "--verify", "--quiet", reference])
            .ok()
            .filter(|s| !s.trim().is_empty())
    };
    let recorded = ticket.row.head.clone().filter(|h| !h.trim().is_empty());
    let head = match (verify(&format!("refs/heads/{branch}")), recorded) {
        (Some(tip), Some(head)) if exact && tip != head => bail!(
            "the branch {branch} points to {} in the clone {text}, not to the head {} that {key} had in the background. Look at the branch. A continue takes its tip: run `vision stop {key}`, then `vision take {key}`",
            short(&tip),
            short(&head)
        ),
        (Some(tip), _) => tip,
        (None, Some(head)) => {
            if verify(&format!("{head}^{{commit}}")).is_none() {
                git::run(clone, &["fetch", "--quiet", "--all", "--prune"])
                    .with_context(|| format!("could not fetch in the clone {text}"))?;
            }
            git::run(clone, &["branch", "--no-track", &branch, &head]).with_context(|| {
                format!(
                    "could not make the branch {branch} at {} in the clone {text}",
                    short(&head)
                )
            })?;
            head
        }
        (None, None) => bail!(
            "{key} has no head in its row, and the clone {text} has no branch {branch}, so no slot can take it. Run `vision stop {key}`"
        ),
    };
    let on = git::run(clone, &["symbolic-ref", "--quiet", "--short", "HEAD"]).unwrap_or_default();
    if on != branch {
        git::run(clone, &["switch", "--quiet", &branch]).map_err(|e| {
            switch_error(
                e,
                clone,
                &branch,
                &format!("vision retry {key}"),
                format!("could not switch the clone {text} to the branch {branch} of {key}"),
            )
        })?;
    }
    ticket.flags.retain(|f| f != WAITS_FOR_SLOT);
    ticket.counts.reasons.remove(WAITS_FOR_SLOT);
    let note = ticket.counts.slot.get_or_insert_with(SlotNote::default);
    note.last = Some(text.clone());
    note.background = None;
    in_tx(ledger, |ledger| {
        ledger.set_ticket_clone(key.as_str(), Some(&text), at)?;
        ledger.set_ticket_values(key.as_str(), None, Some(&head), at)?;
        ledger.set_ticket_state(
            key.as_str(),
            ticket.state(),
            &flags_text(&ticket.flags),
            &ticket.counts.text(),
            at,
        )?;
        ledger.add_event(
            &Event::now("tick", "slot")
                .ticket(key.as_str())
                .detail(json!({"took": text, "head": head, "step": ticket.state()})),
        )?;
        Ok(())
    })?;
    ticket.row.clone = Some(text.clone());
    ticket.row.head = Some(head.clone());
    ctx.log(&format!(
        "slots: {key} took the clone {text} on its branch {branch} ({}) at the step {}",
        short(&head),
        ticket.state()
    ));
    Ok(())
}

/// The flag `waits-for-slot` with the place in the queue. A new place changes only the reason.
fn wait_flag(ctx: &Ctx, ledger: &Ledger, ticket: &mut Ticket, place: usize, at: &str) -> Result<()> {
    let reason = format!("{} in line: no slot is free", ordinal(place));
    let had = ticket.flags.iter().any(|f| f == WAITS_FOR_SLOT);
    let old = ticket.counts.reasons.get(WAITS_FOR_SLOT).cloned();
    if had && old.as_ref().is_some_and(|r| r.reason == reason) {
        return Ok(());
    }
    if !had {
        ticket.flags.push(WAITS_FOR_SLOT.into());
    }
    ticket.counts.reasons.insert(
        WAITS_FOR_SLOT.into(),
        FlagReason {
            reason: reason.clone(),
            at: old.map_or_else(|| at.to_string(), |r| r.at),
            by: None,
        },
    );
    let event = (!had).then(|| {
        Event::now("tick", "flag")
            .ticket(ticket.key.as_str())
            .detail(json!({"flag": WAITS_FOR_SLOT, "reason": reason, "step": ticket.state()}))
    });
    save(ledger, ticket, at, event)?;
    if !had {
        ctx.log(&format!(
            "slots: {} waits for a slot at the step {}: {reason}",
            ticket.key,
            ticket.state()
        ));
    }
    Ok(())
}

/// A ticket whose start or move into a clone failed: the flag `needs-you` with the reason. It
/// leaves the queue until `vision retry` clears the flag.
fn flag_needs_you(
    ctx: &Ctx,
    ledger: &Ledger,
    ticket: &mut Ticket,
    reason: &str,
    at: &str,
) -> Result<()> {
    ticket.flags.retain(|f| f != WAITS_FOR_SLOT);
    ticket.counts.reasons.remove(WAITS_FOR_SLOT);
    if !ticket.flags.iter().any(|f| f == NEEDS_YOU) {
        ticket.flags.push(NEEDS_YOU.into());
    }
    ticket.counts.reasons.insert(
        NEEDS_YOU.into(),
        FlagReason {
            reason: reason.to_string(),
            at: at.to_string(),
            by: None,
        },
    );
    save(
        ledger,
        ticket,
        at,
        Some(
            Event::now("tick", "flag")
                .ticket(ticket.key.as_str())
                .detail(json!({"flag": NEEDS_YOU, "reason": reason, "step": ticket.state()})),
        ),
    )?;
    ctx.log(&format!(
        "slots: {} got the flag {NEEDS_YOU}: {reason}",
        ticket.key
    ));
    Ok(())
}

/// The row of a take that gets no slot: the step, the branch, the flag, and no clone, with the
/// event `queued`.
fn queue_new(ctx: &Ctx, ledger: &Ledger, new: &NewTake, place: usize, at: &str) -> Result<()> {
    let reason = format!("{} in line: no slot is free", ordinal(place));
    let mut counts = Counts {
        slot: Some(SlotNote {
            take: Some(new.take.clone()),
            ..SlotNote::default()
        }),
        ..Counts::default()
    };
    counts.reasons.insert(
        WAITS_FOR_SLOT.into(),
        FlagReason {
            reason,
            at: at.to_string(),
            by: None,
        },
    );
    let row = TicketRow {
        key: new.key.to_string(),
        title: new.title.clone(),
        state: Some(new.take.step.clone()),
        flags: Some(flags_text(&[WAITS_FOR_SLOT.to_string()])),
        clone: None,
        branch: Some(new.branch.clone()),
        pr: new.pr,
        head: None,
        taken: Some(at.to_string()),
        updated: Some(at.to_string()),
        counts: Some(counts.text()),
    };
    let dir = ctx.paths.ticket_dir(&new.key);
    std::fs::create_dir_all(&dir).with_context(|| format!("could not make {}", dir.display()))?;
    in_tx(ledger, |ledger| {
        ledger.put_ticket(&row)?;
        ledger.add_event(&Event::now("user", "queued").ticket(new.key.as_str()).detail(json!({
            "step": new.take.step, "place": place, "outside": new.take.outside,
            "branch": new.branch, "pr": new.pr,
        })))?;
        Ok(())
    })?;
    ctx.log(&format!(
        "slots: {} waits in the queue for its first slot, {} in line, at the step {}",
        new.key,
        ordinal(place),
        new.take.step
    ));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_place_in_the_queue_reads_as_an_ordinal() {
        let words: Vec<String> = [1, 2, 3, 4, 11, 12, 13, 21, 22, 23, 101, 111]
            .into_iter()
            .map(ordinal)
            .collect();
        assert_eq!(
            words,
            ["1st", "2nd", "3rd", "4th", "11th", "12th", "13th", "21st", "22nd", "23rd", "101st", "111th"]
        );
    }

    #[test]
    fn a_take_line_names_the_slot_and_the_ticket_that_gave_it_or_the_place_in_the_queue() {
        let given = Given {
            took: vec![Took {
                key: "DEMO-13".into(),
                clone: PathBuf::from("/w/clone-1"),
                from: Some("DEMO-12".into()),
                line: Some("Took DEMO-13 in the clone /w/clone-1.".into()),
            }],
            waits: vec![("DEMO-14".into(), 2)],
            busy: vec!["/w/clone-1: DEMO-13 holds it (step refine)".into()],
        };
        assert_eq!(
            take_line("DEMO-13", "refine", &given),
            "Took DEMO-13 in the clone /w/clone-1. DEMO-12 gave its slot: it waits in the background, and Vision still watches it."
        );
        assert_eq!(
            take_line("DEMO-14", "refine", &given),
            "Queued DEMO-14 at the step refine: no slot is free, and it is 2nd in line. It starts when a slot is free.\n  /w/clone-1: DEMO-13 holds it (step refine)"
        );
    }
}
