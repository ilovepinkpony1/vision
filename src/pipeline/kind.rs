//! The kind of a ticket and `when` of its steps (section 11.2).
//!
//! The kind comes from the issue type that `observe` gives in `issue.type`, through `[kinds]`
//! of the adapter. The first observe after the take decides it, and `vision kind` changes it.
//! A step whose `when` does not hold is skipped: the ticket moves through its `next` in the
//! same tick, with the event `skipped` and a span whose `waits_on` is `skipped`.

use anyhow::{bail, Context, Result};
use serde_json::{json, Value};

use super::machine::{in_tx, move_to, save, write_issue, Ticket};
use crate::adapter::when::{holds, Holds, WidthVerdict};
use crate::adapter::{self, Adapter, Step, StepKind, NARROW, SAME, WIDE};
use crate::ctx::Ctx;
use crate::executor::TicketFacts;
use crate::ledger::{Event, Ledger};
use crate::paths::TicketKey;
use crate::util::fs::read_optional;
use crate::util::time;

/// The issue type that `observe` gives: `issue.type` of its answer.
pub fn issue_type(facts: &TicketFacts) -> Option<String> {
    facts
        .issue
        .get("type")
        .and_then(Value::as_str)
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

/// Sets the kind of a ticket that has none, from the issue type of an observe answer, when the
/// adapter has `[kinds]`. Gives the event that records it.
pub(crate) fn decide_kind(
    ctx: &Ctx,
    ticket: &mut Ticket,
    facts: &TicketFacts,
) -> Result<Option<Event>> {
    if ticket.counts.kind.is_some() {
        return Ok(None);
    }
    let adapter = adapter::load(ctx)?;
    let Some(kinds) = &adapter.kinds else {
        return Ok(None);
    };
    let found = issue_type(facts);
    let kind = kinds.of(found.as_deref());
    ticket.counts.kind = Some(kind.clone());
    ctx.log(&format!(
        "pipeline: {} is a {kind} ticket (the issue type {})",
        ticket.key,
        found.as_deref().unwrap_or("is missing")
    ));
    Ok(Some(
        Event::now("tick", "kind")
            .ticket(ticket.key.as_str())
            .detail(json!({"kind": kind, "issue_type": found, "how": "observe"})),
    ))
}

/// Whether `when` of a step holds for the ticket.
pub(crate) fn step_holds(
    ctx: &Ctx,
    ledger: &Ledger,
    adapter: &Adapter,
    ticket: &mut Ticket,
    step: &Step,
) -> Result<Holds> {
    words_hold(ctx, ledger, adapter, ticket, &step.when)
}

/// Whether the words of a `when` hold for the ticket. A kind word of a ticket with no kind
/// asks `observe` first, which decides the kind. A width word reads the document of `[width]`
/// as it is now.
pub(crate) fn words_hold(
    ctx: &Ctx,
    ledger: &Ledger,
    adapter: &Adapter,
    ticket: &mut Ticket,
    words: &[String],
) -> Result<Holds> {
    if words.is_empty() {
        return Ok(Holds::Yes);
    }
    let needs_kind = words.iter().any(|w| w != WIDE && w != NARROW);
    if needs_kind && ticket.counts.kind.is_none() && adapter.kinds.is_some() {
        write_issue(ctx, ledger, ticket)?;
    }
    let width = adapter.width.clone();
    let doc = width.as_ref().map(|w| w.doc.clone()).unwrap_or_default();
    let key = ticket.key.clone();
    let read = || -> Option<WidthVerdict> {
        let width = width.as_ref()?;
        let text = read_optional(&ctx.paths.ticket_doc(&key, &width.doc)).ok()??;
        Some(width.judge(&text))
    };
    Ok(holds(words, ticket.counts.kind.as_deref(), &read, &doc))
}

/// Where a skipped step sends the ticket: its `next`, `on.same` of an ask step, or `on.none`
/// of a turn step that moves by its commits, because a step that did not run made no commit.
pub(crate) fn skip_target(step: &Step) -> Option<String> {
    match &step.kind {
        StepKind::Ask(_) => step.on.get(SAME).cloned(),
        StepKind::Turn(_) if step.on.contains_key(crate::adapter::COMMITS) => {
            step.on.get(crate::adapter::NONE).cloned()
        }
        _ => step.next.clone(),
    }
}

/// Skips a step: its span gets `waits_on` `skipped`, the event `skipped` says why, and the
/// ticket moves to `to`.
pub(crate) fn skip(
    ctx: &Ctx,
    ledger: &Ledger,
    adapter: &Adapter,
    ticket: &mut Ticket,
    from: &Step,
    to: &str,
    why: &str,
) -> Result<()> {
    let now = time::now_rfc3339();
    in_tx(ledger, |ledger| {
        ledger.mark_open_span(ticket.key.as_str(), "skipped", &format!("skipped: {why}"))?;
        ledger.add_event(
            &Event::now("tick", "skipped")
                .ticket(ticket.key.as_str())
                .detail(json!({"step": from.id, "why": why, "to": to, "at": now})),
        )?;
        Ok(())
    })?;
    ctx.log(&format!(
        "pipeline: skipped {} of {}: {why}",
        from.id, ticket.key
    ));
    move_to(ctx, ledger, adapter, ticket, from, to, "skip")
}

/// `vision kind KEY NAME`: changes the kind of a ticket. The name must be a kind of `[kinds]`.
pub fn kind_command(ctx: &Ctx, key: &TicketKey, name: &str) -> Result<()> {
    ctx.require_setup()?;
    let _lock = super::commands::wait_for_lock(ctx)?;
    let ledger = ctx.ledger()?;
    let adapter = adapter::load(ctx)?;
    let Some(kinds) = &adapter.kinds else {
        bail!(
            "the adapter {} has no [kinds], so its tickets have no kind",
            adapter.name
        );
    };
    let name = name.trim();
    if !kinds.has(name) {
        bail!(
            "`{name}` is not a kind of the adapter {}. The kinds: {}",
            adapter.name,
            kinds.names().join(", ")
        );
    }
    let row = ledger
        .ticket(key.as_str())?
        .with_context(|| format!("Vision does not hold {key}. Run `vision take {key}` first"))?;
    let mut ticket = Ticket::of(row)?;
    let old = ticket.counts.kind.clone();
    ticket.counts.kind = Some(name.to_string());
    save(
        &ledger,
        &ticket,
        &time::now_rfc3339(),
        Some(
            Event::now("user", "kind")
                .ticket(key.as_str())
                .detail(json!({"kind": name, "old": old, "how": "vision kind"})),
        ),
    )?;
    ctx.log(&format!("kind: {key} is now a {name} ticket"));
    match old {
        Some(old) if old == name => println!("{key} is already a {name} ticket."),
        Some(old) => println!(
            "{key} is now a {name} ticket (it was a {old} ticket). The later steps follow the new kind."
        ),
        None => println!("{key} is now a {name} ticket. The later steps follow it."),
    }
    Ok(())
}
