//! The things that wait for the person, as the cards of the app show them: a question of an
//! agent, a plan, a write that waits for an approval, the first write of a verb in the mode
//! shadow or hold, a rule that stopped a ticket, a paused ticket, and a proposal of the Curator.
//! Plain data that [`super::load_needs`] reads, and the state of the card that shows them one at
//! a time.

use serde_json::Value;

use super::line::Line;
use crate::pipeline::PlanView;
use crate::questions::CardView;

/// One thing that waits for the person.
#[derive(Clone, Debug, PartialEq)]
pub enum NeedItem {
    Question(CardView),
    Plan(PlanItem),
    /// An operation that waits for `vision approve`: the mode ask, or a diff that touches a
    /// sensitive path that the approved plan does not name.
    Approval(WriteItem),
    /// An operation that the mode shadow or hold kept from running.
    FirstWrite(WriteItem),
    /// The flag `needs-you` with another reason.
    Flag(FlagItem),
    /// The flag `paused`: the person took over the session.
    Paused(FlagItem),
    Proposal(ProposalItem),
}

impl NeedItem {
    /// The name of the item, which stays the same over loads.
    pub fn id(&self) -> String {
        match self {
            NeedItem::Question(card) => format!("question:{}", card.id),
            NeedItem::Plan(plan) => format!("plan:{}", plan.ticket),
            NeedItem::Approval(write) => format!("approval:{}", write.op),
            NeedItem::FirstWrite(write) => format!("first-write:{}", write.op),
            NeedItem::Flag(flag) => format!("flag:{}", flag.ticket),
            NeedItem::Paused(flag) => format!("paused:{}", flag.ticket),
            NeedItem::Proposal(proposal) => format!("proposal:{}", proposal.name),
        }
    }

    /// The ticket of the item; a proposal of the Curator has none.
    pub fn ticket(&self) -> Option<&str> {
        match self {
            NeedItem::Question(card) => Some(&card.ticket),
            NeedItem::Plan(plan) => Some(&plan.ticket),
            NeedItem::Approval(write) | NeedItem::FirstWrite(write) => Some(&write.ticket),
            NeedItem::Flag(flag) | NeedItem::Paused(flag) => Some(&flag.ticket),
            NeedItem::Proposal(_) => None,
        }
    }

    /// One word for the kind, for the title of the card.
    pub fn word(&self) -> &'static str {
        match self {
            NeedItem::Question(_) => "question",
            NeedItem::Plan(_) => "plan",
            NeedItem::Approval(_) => "write",
            NeedItem::FirstWrite(_) => "first write",
            NeedItem::Flag(_) => "stopped by a rule",
            NeedItem::Paused(_) => "paused",
            NeedItem::Proposal(_) => "memory",
        }
    }
}

/// A plan that waits for `vision approve-plan`.
#[derive(Clone, Debug, PartialEq)]
pub struct PlanItem {
    pub ticket: String,
    /// The human step that waits for the approval.
    pub step: String,
    pub plan: PlanView,
}

/// One operation and what it sends.
#[derive(Clone, Debug, PartialEq)]
pub struct WriteItem {
    pub ticket: String,
    /// The id of the operation.
    pub op: String,
    pub verb: String,
    /// What the verb does in plain words, when the adapter says so.
    pub about: Option<String>,
    /// True when text or code leaves the machine, so that the gates run.
    pub leaves: bool,
    /// The mode of the verb now: hold, shadow, ask, or auto.
    pub mode: String,
    /// The commit that the operation acts on, in full. Empty for a ticket with no commit.
    pub head: String,
    /// The arguments of the operation, as the ledger keeps them: each secret is masked.
    pub args: Value,
    /// For the mode shadow: what the verb would do, as the executor recorded it.
    pub intent: Option<Value>,
    /// The sensitive paths that the diff touches and that the approved plan does not name.
    pub held: Vec<String>,
    /// The step of the ticket now.
    pub step: String,
    /// The turn of the step held its end facts, so a retry runs only its writes again.
    pub turn_held: bool,
}

/// A ticket with the flag `needs-you` or `paused`.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct FlagItem {
    pub ticket: String,
    pub step: String,
    /// Why the flag is set, in full.
    pub reason: String,
    /// When the flag was set, RFC 3339 UTC.
    pub since: Option<String>,
    /// The agent of the step, for a turn step.
    pub agent: Option<String>,
    /// The agent of the last work turn of the ticket, whose session `vision open` opens.
    pub last: Option<String>,
    pub branch: Option<String>,
    /// The turn of the step held its end facts, so a retry runs only its writes again.
    pub held: bool,
    pub stopped: bool,
}

/// A proposal of the Curator with open changes.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ProposalItem {
    pub name: String,
    pub summary: String,
    pub open: usize,
    /// True while its replays still run.
    pub replaying: bool,
}

/// The reply that Enter takes on a question card: the proposal of the Judge, else the
/// recommended choice.
pub fn enter_reply(card: &CardView) -> Option<Reply> {
    if let Some(proposal) = &card.proposal {
        return Some(match proposal.choice {
            Some(n) if n >= 1 => Reply::Choice(n - 1),
            _ => Reply::Text(proposal.answer.clone()),
        });
    }
    card.recommended
        .filter(|n| *n >= 1)
        .map(|n| Reply::Choice(n - 1))
}

/// An answer of the person to a question card: a choice by its index from 0, or a text.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Reply {
    Choice(usize),
    Text(String),
}

/// What the person types or picks on the card of the item that shows now.
#[derive(Clone, Debug, Default, PartialEq)]
pub enum Input {
    #[default]
    None,
    /// `e`: an answer in the words of the person.
    Words(Line),
    /// After a choice or a text: the reason, in one line.
    Why { reply: Reply, line: Line },
    /// `k`: where the answer already was.
    Needless,
}

/// The card of the things that wait: one item at a time, with the arrows to move between them.
#[derive(Clone, Debug, PartialEq)]
pub struct NeedsCard {
    /// The ticket of the live view when `n` came from there: only its items show.
    pub scope: Option<String>,
    /// The items, or `None` until the first load arrives.
    pub items: Option<Vec<NeedItem>>,
    /// The error of the last load.
    pub error: Option<String>,
    /// The index of the item that shows.
    pub at: usize,
    pub input: Input,
    /// The first row of the body of a long card.
    pub scroll: usize,
    /// One line about what just happened, such as a write that changed before its approval.
    pub note: Option<String>,
    /// The load whose result counts. A result of an older load is dropped.
    pub load: u64,
    /// The item to show when the next load arrives.
    pub select: Option<String>,
}

impl NeedsCard {
    pub fn new(scope: Option<String>, load: u64) -> NeedsCard {
        NeedsCard {
            scope,
            items: None,
            error: None,
            at: 0,
            input: Input::None,
            scroll: 0,
            note: None,
            load,
            select: None,
        }
    }

    /// The item that shows.
    pub fn item(&self) -> Option<&NeedItem> {
        self.items.as_ref()?.get(self.at)
    }

    /// The number of items.
    pub fn len(&self) -> usize {
        self.items.as_ref().map_or(0, Vec::len)
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}
