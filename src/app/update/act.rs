//! The keys of the other cards of the things that wait:
//!
//! - The plan (`mPlan`): `a` approves the plan with the hash that the card showed, `x` opens the
//!   card that stops the ticket.
//! - A write that waits: `y` approves it, `a` approves it and moves its verb to the mode auto.
//! - The first write of a verb in the mode shadow or hold (`mWrite`): `y` sends it and asks
//!   each time, `a` sends it and never asks again, `n` leaves it as it is.
//! - A rule that stopped the ticket: `r` tries the step again, `o` opens the session, `x` stops
//!   the ticket. When the ticket is also stopped, Enter opens the card that starts it again.
//! - A paused ticket: Enter or `r` continues it; a ticket that is also stopped gets the take
//!   first.
//! - A proposal of the Curator: Enter opens its review in the terminal.

use super::super::chain::Owner;
use super::super::model::{App, Card, OpenCard, ResumeCard, Run, StopCard, Tone};
use super::super::msg::Key;
use super::super::needs::{FlagItem, NeedItem};
use super::super::recipes;
use super::{chain, notice};

pub fn on_key(app: &mut App, item: &NeedItem, key: Key) {
    let id = app.seq + 1;
    let started = match (item, key) {
        (NeedItem::Plan(plan), Key::Char('a')) => Some(recipes::approve_plan(id, plan)),
        (NeedItem::Plan(plan), Key::Char('x')) => {
            stop_card(app, &plan.ticket, &plan.step, None);
            None
        }
        (NeedItem::Approval(write), Key::Char('y')) => Some(recipes::approve(id, write, false)),
        (NeedItem::Approval(write), Key::Char('a')) if write.mode == "ask" => {
            Some(recipes::approve(id, write, true))
        }
        (NeedItem::FirstWrite(write), Key::Char('y')) if recipes::can_ask(write) => {
            Some(recipes::first_write(id, write, false))
        }
        (NeedItem::FirstWrite(write), Key::Char('a')) => {
            Some(recipes::first_write(id, write, true))
        }
        (NeedItem::FirstWrite(_), Key::Char('n')) => {
            app.card = None;
            chain::drop_ended(app);
            None
        }
        (NeedItem::Flag(flag), Key::Enter) if flag.stopped => {
            resume_card(app, flag);
            None
        }
        (NeedItem::Flag(flag), Key::Char('r')) if !flag.stopped => {
            Some(recipes::retry(id, flag, false))
        }
        (NeedItem::Flag(flag), Key::Char('o')) if !flag.stopped => {
            open_card(app, flag);
            None
        }
        (NeedItem::Flag(flag), Key::Char('x')) if !flag.stopped => {
            stop_card(app, &flag.ticket, &flag.step, flag.branch.clone());
            None
        }
        // A ticket that the person took over is stopped and paused: the take, then the retry.
        (NeedItem::Paused(flag), Key::Enter | Key::Char('r')) if flag.stopped => {
            let owner = Owner::Need(item.id());
            Some(recipes::continue_after(id, &flag.ticket, true, owner))
        }
        (NeedItem::Paused(flag), Key::Enter | Key::Char('r')) => {
            Some(recipes::retry(id, flag, true))
        }
        (NeedItem::Proposal(proposal), Key::Enter) => Some(recipes::review(id, proposal)),
        _ => None,
    };
    if let Some(chain) = started {
        app.seq = id;
        chain::start(app, chain);
    }
}

/// The card of part A2 that stops the ticket.
fn stop_card(app: &mut App, key: &str, step: &str, branch: Option<String>) {
    let branch = branch.or_else(|| app.held_ticket(key).and_then(|h| h.view.branch.clone()));
    app.card = Some(Card::Stop(StopCard {
        key: key.to_string(),
        step: step.to_string(),
        branch,
        agent: None,
        run: Run::Idle,
    }));
}

/// The card of part A1 that starts a stopped ticket again.
fn resume_card(app: &mut App, flag: &FlagItem) {
    let title = app
        .held_ticket(&flag.ticket)
        .map(|h| app.title_of(h).to_string())
        .unwrap_or_default();
    let clone = app
        .held_ticket(&flag.ticket)
        .and_then(|h| h.view.clone.clone());
    app.card = Some(Card::Resume(ResumeCard {
        key: flag.ticket.clone(),
        title,
        step: flag.step.clone(),
        clone,
        branch: flag.branch.clone(),
        run: Run::Idle,
    }));
}

/// The card of part A2 that opens the session of the last turn.
fn open_card(app: &mut App, flag: &FlagItem) {
    if flag.last.is_none() {
        notice(
            app,
            format!(
                "{} has no turn yet, so there is no session to open.",
                flag.ticket
            ),
            Tone::Info,
        );
        return;
    }
    app.card = Some(Card::Open(OpenCard {
        key: flag.ticket.clone(),
        running: None,
        last: flag.last.clone(),
    }));
}
