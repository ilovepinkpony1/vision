//! A team step (section 11.3): the members whose `when` holds work in parallel, each in its own
//! turn and session. Each member has its own end facts and its own rounds. When the end facts
//! of each started member hold, the `after` verbs run once and the ticket moves to `next`.
//!
//! A member never gets the documents of another member: the loader refuses such a `reads`,
//! and the answers of the read verbs of a member stay in its own list of the visit.

use ::time::OffsetDateTime;
use anyhow::Result;

use super::decide::{cap_exceeded, cap_text, decide, ops_next, List, Next};
use super::kind::words_hold;
use super::machine::{
    agent_turns, cap_value, clear_limit, fall_back, flag_action, gather, move_to, op_facts,
    open_items, request_action, run_before, serve, start_action, write_issue, Before, Ticket,
};
use super::state::NEEDS_YOU;
use crate::adapter::when::Holds;
use crate::adapter::{Adapter, Member, Step};
use crate::ctx::Ctx;
use crate::ledger::Ledger;
use crate::tick::Action;

/// The members of a team step whose `when` holds for the ticket, or the reason of a flag.
pub(crate) fn active_members(
    ctx: &Ctx,
    ledger: &Ledger,
    adapter: &Adapter,
    ticket: &mut Ticket,
    step: &Step,
) -> Result<Result<Vec<Member>, String>> {
    let mut active = Vec::new();
    for member in step.team() {
        match words_hold(ctx, ledger, adapter, ticket, &member.when)? {
            Holds::Yes => active.push(member.clone()),
            Holds::No(_) => {}
            Holds::NoDoc(why) => {
                return Ok(Err(format!(
                    "the member {} of the step {}: {why}",
                    member.agent, step.id
                )))
            }
        }
    }
    Ok(Ok(active))
}

/// The next actions of a ticket at a team step: the turns of the members that start, or the
/// `after` verbs, or the move to `next`.
pub(crate) fn select(
    ctx: &Ctx,
    ledger: &Ledger,
    adapter: &Adapter,
    ticket: &mut Ticket,
    step: &Step,
    now: OffsetDateTime,
) -> Result<Vec<Action>> {
    let active = match active_members(ctx, ledger, adapter, ticket, step)? {
        Ok(active) => active,
        Err(reason) => return Ok(vec![flag_action(ticket, NEEDS_YOU, &reason)]),
    };
    let visit = ticket.visit();
    let mut fresh = true;
    for member in active
        .iter()
        .filter(|m| !visit.team_held.contains(&m.agent))
    {
        if !agent_turns(ledger, ticket, &step.id, &member.agent, true)?.is_empty() {
            fresh = false;
        }
    }
    // The cap of the step counts its entries, as for a step of one agent.
    if fresh {
        if let Some(name) = &step.cap {
            if let Some(value) = cap_value(ctx, name)? {
                if cap_exceeded(visit.n, Some(value), visit.skip_cap) {
                    let open = open_items(ctx, ledger, ticket, step)?;
                    return Ok(vec![flag_action(
                        ticket,
                        NEEDS_YOU,
                        &cap_text(&step.id, name, value, visit.n, &open),
                    )]);
                }
            }
        }
    }
    let mut starts: Vec<(Member, Option<String>, Option<String>)> = Vec::new();
    let mut serves: Vec<(Member, String, String)> = Vec::new();
    let mut fallbacks: Vec<(Member, String)> = Vec::new();
    let mut done = 0;
    let mut flag: Option<(&'static str, String)> = None;
    let mut limit_cleared = false;
    for member in &active {
        if visit.team_held.contains(&member.agent) {
            done += 1;
            continue;
        }
        let view = step.member_view(member);
        let facts = gather(ctx, ledger, adapter, ticket, &view, now, true)?;
        match decide(&facts) {
            // The view moves when the end facts of the member hold: the member is done.
            Next::Move { .. } => done += 1,
            Next::Start { message, .. } => starts.push((member.clone(), None, message)),
            // A member has no gate, so it never gets an extra round.
            Next::Resume {
                session, message, ..
            } => starts.push((member.clone(), Some(session), Some(message))),
            Next::Serve { turn, session } => serves.push((member.clone(), turn, session)),
            Next::Fallback { why } => fallbacks.push((member.clone(), why)),
            Next::Flag { flag: name, reason } => {
                if flag.is_none() {
                    flag = Some((
                        name,
                        format!(
                            "the member {} of the step {}: {reason}",
                            member.agent, step.id
                        ),
                    ));
                }
            }
            Next::ClearLimit => limit_cleared = true,
            // A member is a turn, and only a wait step gives a late flag.
            Next::Nothing(_)
            | Next::Request { .. }
            | Next::RunJob
            | Next::Finish
            | Next::Late { .. } => {}
        }
    }
    if limit_cleared {
        clear_limit(ctx, ledger, ticket)?;
        return Ok(Vec::new());
    }
    // A member that used its rounds stops the ticket. The turns of the other members still
    // run, and their results stay.
    if let Some((name, reason)) = flag {
        return Ok(vec![flag_action(ticket, name, &reason)]);
    }
    if done == active.len() {
        let last = last_member_turn(ledger, ticket, step, &active)?;
        let after = op_facts(ctx, ledger, ticket, &step.after, last.as_deref())?;
        return match ops_next(List::After, &after) {
            Some(Next::Request { index, .. }) => match step.after.get(index) {
                Some(entry) => request_action(ctx, ticket, entry, last).map(|a| vec![a]),
                None => Ok(Vec::new()),
            },
            Some(Next::Flag { flag, reason }) => Ok(vec![flag_action(ticket, flag, &reason)]),
            Some(_) => Ok(Vec::new()),
            None => {
                let Some(next) = step.next.clone() else {
                    return Ok(vec![flag_action(
                        ticket,
                        NEEDS_YOU,
                        &format!("the step {} has no next step. Fix adapter.toml", step.id),
                    )]);
                };
                move_to(ctx, ledger, adapter, ticket, step, &next, "next")?;
                Ok(Vec::new())
            }
        };
    }
    let mut actions = Vec::new();
    // The entry of the step: its `before` entries and `observe` run once, before the first
    // member starts.
    if fresh && !starts.is_empty() {
        match run_before(ctx, ledger, ticket, step)? {
            Before::Done => {}
            Before::Wait => return Ok(Vec::new()),
            Before::Flag(action) => return Ok(vec![action]),
        }
        write_issue(ctx, ledger, ticket)?;
    }
    for (member, session, message) in starts {
        actions.push(start_action(
            ticket,
            &step.member_view(&member),
            session,
            message,
        ));
    }
    for (member, turn, session) in serves {
        let view = step.member_view(&member);
        if let Some(action) = serve(ctx, ledger, adapter, ticket, &view, &turn, &session, true)? {
            actions.push(action);
        }
    }
    for (member, why) in fallbacks {
        let view = step.member_view(&member);
        if let Some(action) = fall_back(ctx, ledger, adapter, ticket, &view, &why, true)? {
            actions.push(action);
        }
    }
    Ok(actions)
}

/// The newest turn of the members in this visit, for the operations of `after`.
fn last_member_turn(
    ledger: &Ledger,
    ticket: &Ticket,
    step: &Step,
    active: &[Member],
) -> Result<Option<String>> {
    let mut newest: Option<(String, String)> = None;
    for member in active {
        for turn in agent_turns(ledger, ticket, &step.id, &member.agent, true)? {
            let started = turn.started.clone().unwrap_or_default();
            if newest.as_ref().is_none_or(|(at, _)| started >= *at) {
                newest = Some((started, turn.id.clone()));
            }
        }
    }
    Ok(newest.map(|(_, id)| id))
}

/// The state of each member of a team step in the visit, for the status view and the app:
/// `done`, `runs`, `waits`, or `not started`, in the order of the team. Members whose `when`
/// does not hold are left out. Reads only the ledger and the ticket folder.
pub(crate) fn member_states(
    ledger: &Ledger,
    ticket: &Ticket,
    step: &Step,
    kind_and_width: impl Fn(&[String]) -> bool,
) -> Vec<(String, &'static str)> {
    let visit = ticket.visit();
    let mut out = Vec::new();
    for member in step.team() {
        if !kind_and_width(&member.when) {
            continue;
        }
        if visit.team_held.contains(&member.agent) {
            out.push((member.agent.clone(), "done"));
            continue;
        }
        let turns = agent_turns(ledger, ticket, &step.id, &member.agent, true).unwrap_or_default();
        let state = match turns.last() {
            None => "not started",
            Some(t) if t.status.as_deref() == Some("running") => "runs",
            Some(t) if t.end_fact.as_deref() == Some("held") => "done",
            Some(t) if t.status.as_deref() == Some("failed") => "failed",
            Some(_) => "waits",
        };
        out.push((member.agent.clone(), state));
    }
    out
}
