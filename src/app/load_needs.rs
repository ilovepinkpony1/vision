//! The reads of the cards of the things that wait, each in a thread of the runtime, through the
//! functions of today that return data: the needs of the snapshot, the cards of `vision needs
//! --json` with the drafts of their forms, the plan of `vision plan`, the operations that wait
//! for `vision approve`, the operations that the mode shadow or hold kept from running, the
//! status rows, the open proposals of the Curator, and the notices that wait. The read of a
//! check of a chain is here too. None of them changes the state.
//!
//! The cards of one asking id become one item: the form of a set of questions.

use super::chain::{Check, CheckFacts, OpFacts};
use super::needs::{FlagItem, Gone, NeedItem, PlanItem, ProposalItem, QuestionSet, WriteItem};
use crate::adapter::{self, Adapter};
use crate::config::Config;
use crate::ctx::Ctx;
use crate::executor::{self, commit_arg, OperationResult, Outcome};
use crate::ledger::questions::{STATE_ANSWERED, STATE_NEEDLESS};
use crate::ledger::Ledger;
use crate::paths::{Paths, TicketKey};
use crate::pipeline::facts::{open_writes, state_facts_of, StateFacts};
use crate::pipeline::state::{flags_of, Counts, NEEDS_YOU, PAUSED, STOPPED};
use crate::pipeline::{plan_hash, plan_of, status_rows, TicketView};
use crate::questions::Answer;
use crate::snapshot::{collect_needs, NeedKind};

/// Each thing that waits for the person, in the order of the needs of the snapshot: the
/// questions, the flags and the plans, the writes, the proposals, and the notices. The
/// questions of one asking id are one form, at the place of its first question.
pub fn needs(paths: &Paths) -> Result<Vec<NeedItem>, String> {
    read_needs(paths).map_err(|e| format!("{e:#}"))
}

fn read_needs(paths: &Paths) -> anyhow::Result<Vec<NeedItem>> {
    let ctx = Ctx::new(paths.clone());
    ctx.require_setup()?;
    let ledger = ctx.ledger()?;
    let config = ctx.config()?;
    let adapter = adapter::load_with(config, paths).ok();
    let needs = collect_needs(&ctx, &ledger)?;
    let cards = crate::questions::card_views(&ledger)?;
    let waiting = executor::waiting(&ledger)?;
    let proposals = crate::curator::open_proposals(&ctx, &ledger)?;
    let notices = ledger.unread_notices()?;
    let views = status_rows(&ctx, &ledger, &needs)?;
    let mut items: Vec<NeedItem> = Vec::new();
    for need in &needs {
        let ticket = need.ticket.as_deref().unwrap_or("");
        let view = views.iter().find(|v| v.key == ticket);
        match need.kind {
            NeedKind::Card => {
                let Some(card) = cards.iter().find(|c| c.id == need.id) else {
                    continue;
                };
                let set = items.iter_mut().find_map(|item| match item {
                    NeedItem::Questions(set) if set.turn == card.turn => Some(set),
                    _ => None,
                });
                match set {
                    Some(set) => {
                        if !set.cards.iter().any(|c| c.id == card.id) {
                            set.cards.push(card.clone());
                        }
                    }
                    None => items.push(NeedItem::Questions(QuestionSet {
                        turn: card.turn.clone(),
                        ticket: card.ticket.clone(),
                        cards: vec![card.clone()],
                        saved: Default::default(),
                        gone: Vec::new(),
                    })),
                }
            }
            NeedKind::Plan => {
                let key = TicketKey::parse(ticket)?;
                if let Some(plan) = plan_of(&ctx, &key)?.filter(|p| p.waits) {
                    push(
                        &mut items,
                        NeedItem::Plan(PlanItem {
                            ticket: ticket.to_string(),
                            step: view.and_then(|v| v.step.clone()).unwrap_or_default(),
                            plan,
                        }),
                    );
                }
            }
            NeedKind::Operation => {
                let facts = state_facts_of(&ctx, &ledger, ticket)?.unwrap_or_default();
                for op in waiting.iter().filter(|op| {
                    op.ticket == ticket
                        && format!("{} {}", op.verb, commit_arg(&op.head)) == need.id
                }) {
                    let item = write_item(op, config, adapter.as_ref(), view, &facts);
                    push(&mut items, NeedItem::Approval(item));
                }
            }
            NeedKind::Flag => {
                let Some(view) = view else {
                    continue;
                };
                let facts = state_facts_of(&ctx, &ledger, ticket)?.unwrap_or_default();
                let flag = flag_item(&ledger, view, &facts, &need.id)?;
                if need.id == PAUSED {
                    push(&mut items, NeedItem::Paused(flag));
                    continue;
                }
                let writes = open_writes(&ctx, &ledger, ticket)?;
                if writes.is_empty() {
                    push(&mut items, NeedItem::Flag(flag));
                }
                for op in &writes {
                    let item = write_item(op, config, adapter.as_ref(), Some(view), &facts);
                    push(&mut items, NeedItem::FirstWrite(item));
                }
            }
            NeedKind::Notice => {
                if let Some(notice) = notices.iter().find(|n| n.name() == need.id) {
                    push(&mut items, NeedItem::Notice(notice.clone()));
                }
            }
            NeedKind::Proposal => {
                if let Some(p) = proposals.iter().find(|p| p.name == need.id) {
                    push(
                        &mut items,
                        NeedItem::Proposal(ProposalItem {
                            name: p.name.clone(),
                            summary: p.summary.clone(),
                            open: p.open,
                            replaying: p.replaying,
                        }),
                    );
                }
            }
        }
    }
    // The drafts of each form, from the drafts file of its ticket. The first form of a ticket
    // also names the drafts whose questions have a final answer now, or no longer exist.
    let waiting: Vec<&str> = cards.iter().map(|c| c.id.as_str()).collect();
    let mut files = std::collections::BTreeMap::new();
    for item in &mut items {
        if let NeedItem::Questions(set) = item {
            let first = !files.contains_key(&set.ticket);
            let drafts = files
                .entry(set.ticket.clone())
                .or_insert_with(|| super::drafts::read(paths, &set.ticket));
            set.saved = set
                .cards
                .iter()
                .filter_map(|c| drafts.get(&c.id).map(|d| (c.id.clone(), d.clone())))
                .collect();
            if first {
                set.gone = Vec::new();
                for id in drafts.keys().filter(|id| !waiting.contains(&id.as_str())) {
                    if let Some(who) = gone(&ledger, id)? {
                        set.gone.push((id.clone(), who));
                    }
                }
            }
        }
    }
    Ok(items)
}

/// Who answered a question that no longer waits, or `None` while its answer is not final, such
/// as an answer whose record waits in `recording` for a tick.
fn gone(ledger: &Ledger, id: &str) -> anyhow::Result<Option<Gone>> {
    let Some(row) = ledger.question(id)? else {
        return Ok(Some(Gone::Other));
    };
    Ok(match row.state.as_str() {
        STATE_ANSWERED => {
            let by_judge = row
                .answer
                .as_deref()
                .and_then(|text| serde_json::from_str::<Answer>(text).ok())
                .is_some_and(|answer| answer.by_judge());
            Some(if by_judge { Gone::Judge } else { Gone::Person })
        }
        STATE_NEEDLESS => Some(Gone::Person),
        _ => None,
    })
}

/// Adds an item, unless an item with its id is already there.
fn push(items: &mut Vec<NeedItem>, item: NeedItem) {
    if !items.iter().any(|i| i.id() == item.id()) {
        items.push(item);
    }
}

fn write_item(
    op: &OperationResult,
    config: &Config,
    adapter: Option<&Adapter>,
    view: Option<&TicketView>,
    facts: &StateFacts,
) -> WriteItem {
    let spec = adapter.and_then(|a| a.verb(&op.verb).ok());
    let mode = adapter
        .and_then(|a| executor::mode_of(config, a, &op.verb).ok())
        .map_or_else(|| op.mode.clone(), |m| m.as_str().to_string());
    WriteItem {
        ticket: op.ticket.clone(),
        op: op.id.clone(),
        verb: op.verb.clone(),
        about: spec.and_then(|v| v.about.clone()),
        leaves: spec.is_some_and(|v| v.leaves),
        mode,
        head: op.head.clone(),
        args: op.args.clone(),
        intent: op.intent.clone(),
        held: op.held.clone(),
        oversize: op.oversize.clone(),
        step: view.and_then(|v| v.step.clone()).unwrap_or_default(),
        writes_only: facts.writes_only(),
    }
}

fn flag_item(
    ledger: &Ledger,
    view: &TicketView,
    facts: &StateFacts,
    flag: &str,
) -> anyhow::Result<FlagItem> {
    let shown = view.flags.iter().find(|f| f.flag == flag);
    let last = ledger
        .last_turn(&view.key)?
        .and_then(|t| t.agent)
        .filter(|a| !a.is_empty());
    Ok(FlagItem {
        ticket: view.key.clone(),
        step: view.step.clone().unwrap_or_default(),
        reason: shown.map(|f| f.reason.clone()).unwrap_or_default(),
        since: shown.and_then(|f| f.since.clone()),
        agent: facts.agent.clone(),
        last,
        branch: view.branch.clone(),
        held: facts.held,
        act: facts.act,
        check: facts.check.clone(),
        writes_only: facts.writes_only(),
        stopped: view.flags.iter().any(|f| f.flag == STOPPED),
    })
}

/// The facts that a check of a chain reads: the mode of its verb, the step and the flags of its
/// ticket, the hash of the plan, and the operations of the verb of the ticket.
pub fn check_facts(paths: &Paths, check: &Check) -> Result<CheckFacts, String> {
    read_check(paths, check).map_err(|e| format!("{e:#}"))
}

fn read_check(paths: &Paths, check: &Check) -> anyhow::Result<CheckFacts> {
    let ctx = Ctx::new(paths.clone());
    ctx.require_setup()?;
    let ledger = ctx.ledger()?;
    let key = TicketKey::parse(check.ticket())?;
    let mut facts = CheckFacts::default();
    if let Some(verb) = check.verb() {
        let config = ctx.config()?;
        let adapter = adapter::load_with(config, paths)?;
        facts.mode = Some(
            executor::mode_of(config, &adapter, verb)?
                .as_str()
                .to_string(),
        );
        facts.ops = executor::operations_of(&ledger, &key)?
            .into_iter()
            .filter(|op| op.verb == verb)
            .map(|op| OpFacts {
                waits: matches!(op.outcome, Outcome::Waiting { .. }),
                state: op.state.as_str().to_string(),
                id: op.id,
                head: op.head,
                args: op.args,
                detail: op.detail,
            })
            .collect();
    }
    if let Some(row) = ledger.ticket(key.as_str())? {
        facts.flags = flags_of(&row);
        facts.reason = Counts::of(&row)
            .reasons
            .get(NEEDS_YOU)
            .map(|r| r.reason.clone());
        facts.step = row.state;
    }
    if matches!(check, Check::Plan { .. }) {
        facts.plan = std::fs::read(ctx.paths.ticket_doc(&key, "plan.md"))
            .ok()
            .map(|bytes| plan_hash(&bytes));
    }
    Ok(facts)
}
