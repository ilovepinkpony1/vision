//! The things that wait for the person, as the cards of the app show them: the questions of one
//! asking turn or ask step as one form, a plan, a write that waits for an approval, the first
//! write of a verb in the mode shadow or hold, a rule that stopped a ticket, a paused ticket, a
//! proposal of the Curator, and a notice of a gate.
//!
//! Plain data that [`super::load_needs`] reads, and the state of the card that shows them one at
//! a time. The form of a set of questions keeps a draft answer for each question until the send
//! (section 15.3): the draft model is here, and [`super::drafts`] keeps the drafts in a file.

use std::collections::BTreeMap;

use serde_json::Value;

use super::line::Line;
use crate::ledger::notices::NoticeRow;
use crate::pipeline::PlanView;
use crate::questions::batch;
use crate::questions::set::Entry;
use crate::questions::CardView;

/// One thing that waits for the person.
#[derive(Clone, Debug, PartialEq)]
pub enum NeedItem {
    /// The questions of one asking turn, or of one ask step, as one form.
    Questions(QuestionSet),
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
    /// What a gate found that does not block the ticket, such as the low issues of a QA run.
    /// It waits until the person marks it read.
    Notice(NoticeRow),
}

impl NeedItem {
    /// The name of the item, which stays the same over loads.
    pub fn id(&self) -> String {
        match self {
            NeedItem::Questions(set) => format!("questions:{}", set.turn),
            NeedItem::Plan(plan) => format!("plan:{}", plan.ticket),
            NeedItem::Approval(write) => format!("approval:{}", write.op),
            NeedItem::FirstWrite(write) => format!("first-write:{}", write.op),
            NeedItem::Flag(flag) => format!("flag:{}", flag.ticket),
            NeedItem::Paused(flag) => format!("paused:{}", flag.ticket),
            NeedItem::Proposal(proposal) => format!("proposal:{}", proposal.name),
            NeedItem::Notice(notice) => format!("notice:{}", notice.name()),
        }
    }

    /// The ticket of the item; a proposal of the Curator has none.
    pub fn ticket(&self) -> Option<&str> {
        match self {
            NeedItem::Questions(set) => Some(&set.ticket),
            NeedItem::Plan(plan) => Some(&plan.ticket),
            NeedItem::Approval(write) | NeedItem::FirstWrite(write) => Some(&write.ticket),
            NeedItem::Flag(flag) | NeedItem::Paused(flag) => Some(&flag.ticket),
            NeedItem::Proposal(_) => None,
            NeedItem::Notice(notice) => Some(&notice.ticket),
        }
    }

    /// One word for the kind, for the title of the card.
    pub fn word(&self) -> &'static str {
        match self {
            NeedItem::Questions(_) => "questions",
            NeedItem::Plan(_) => "plan",
            NeedItem::Approval(_) => "write",
            NeedItem::FirstWrite(_) => "first write",
            NeedItem::Flag(_) => "stopped by a rule",
            NeedItem::Paused(_) => "paused",
            NeedItem::Proposal(_) => "memory",
            NeedItem::Notice(_) => "notice",
        }
    }

    /// The set of questions of the item, for a form.
    pub fn set(&self) -> Option<&QuestionSet> {
        match self {
            NeedItem::Questions(set) => Some(set),
            _ => None,
        }
    }
}

/// The questions of one asking id that wait for the person: one form.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct QuestionSet {
    /// The asking id: the id of the asking turn, or the batch id of an ask step.
    pub turn: String,
    pub ticket: String,
    /// The questions, in the order of the cards.
    pub cards: Vec<CardView>,
    /// The drafts that the drafts file of the ticket held for these questions when the load
    /// read it. The card takes them at its first load, and keeps its own after that.
    pub saved: BTreeMap<String, Draft>,
    /// The questions of the ticket whose drafts the file still held, and that have a final
    /// answer now or no longer exist, with who answered each. A question whose answer waits in
    /// `recording` keeps its draft until a tick finishes its record. Only the first form of a
    /// ticket has them.
    pub gone: Vec<(String, Gone)>,
}

/// Who answered a question whose draft goes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Gone {
    /// The Judge answered it.
    Judge,
    /// The person answered it or marked it as needless: with a command, or with a send whose
    /// records a tick finished.
    Person,
    /// The ledger no longer has it.
    Other,
}

/// The mark of one question of a form.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mark {
    /// The question has a draft answer.
    Drafted,
    /// The Judge proposed an answer, and the question has no draft.
    Proposed,
    /// No draft and no proposal.
    Open,
}

impl QuestionSet {
    pub fn len(&self) -> usize {
        self.cards.len()
    }

    pub fn is_empty(&self) -> bool {
        self.cards.is_empty()
    }

    /// The page of the review, after the last question.
    pub fn review(&self) -> usize {
        self.cards.len()
    }

    /// The step of the ask step, for the batch of an ask step.
    pub fn ask_step(&self) -> Option<String> {
        batch::parse(&self.turn).map(|b| b.step)
    }

    /// Who asks, in words: `builder asks at the step build`, or `the ask step questions`.
    pub fn asker(&self) -> String {
        if let Some(step) = self.ask_step() {
            return format!("the ask step {step}");
        }
        let first = self.cards.first();
        let agent = first.map_or("agent", |c| c.agent.as_str());
        match first.map(|c| c.step.as_str()).filter(|s| !s.is_empty()) {
            Some(step) => format!("{agent} asks at the step {step}"),
            None => format!("{agent} asks"),
        }
    }

    /// The mark of the question `i`.
    pub fn mark(&self, i: usize, drafts: &BTreeMap<String, Draft>) -> Mark {
        match self.cards.get(i) {
            Some(card) if drafts.contains_key(&card.id) => Mark::Drafted,
            Some(card) if card.proposal.is_some() => Mark::Proposed,
            _ => Mark::Open,
        }
    }

    /// The questions with no draft, by their index.
    pub fn missing(&self, drafts: &BTreeMap<String, Draft>) -> Vec<usize> {
        (0..self.cards.len())
            .filter(|i| self.mark(*i, drafts) != Mark::Drafted)
            .collect()
    }

    /// The page to show first: the first question with no draft, or the review page when each
    /// question has one.
    pub fn first_page(&self, drafts: &BTreeMap<String, Draft>) -> usize {
        self.missing(drafts)
            .first()
            .copied()
            .unwrap_or(self.review())
    }

    /// The page after a draft of the question `i`: the next question after it with no draft,
    /// or the review page.
    pub fn after_draft(&self, i: usize, drafts: &BTreeMap<String, Draft>) -> usize {
        self.missing(drafts)
            .into_iter()
            .find(|n| *n > i)
            .unwrap_or(self.review())
    }

    /// The answers that a send gives, in the order of the questions: the draft of each one. The
    /// error lists each question with no draft, by its index.
    pub fn answers(
        &self,
        drafts: &BTreeMap<String, Draft>,
    ) -> Result<Vec<(String, Draft)>, Vec<usize>> {
        let missing = self.missing(drafts);
        if !missing.is_empty() {
            return Err(missing);
        }
        Ok(self
            .cards
            .iter()
            .filter_map(|c| drafts.get(&c.id).map(|d| (c.id.clone(), d.clone())))
            .collect())
    }

    /// The index of the first question of the set whose id a text names, such as the reason of
    /// a refused send.
    pub fn named_in(&self, text: &str) -> Option<usize> {
        self.cards.iter().position(|c| {
            text.match_indices(c.id.as_str()).any(|(at, _)| {
                // `Q-0001` must not match inside `Q-00012`.
                !text[at + c.id.len()..]
                    .chars()
                    .next()
                    .is_some_and(|ch| ch.is_ascii_alphanumeric())
            })
        })
    }
}

/// The draft answer of the person to one question. It stays until the send.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Draft {
    /// A choice or words, with the reason of the person. The reason is empty for none.
    Answer { reply: Reply, why: String },
    /// The question was needless: where the answer already was (`code`, `ticket`, `memory`, or
    /// `no-matter`).
    Needless(String),
}

impl Draft {
    /// The entry of the drafts file and of `vision answer --file`.
    pub fn entry(&self) -> Entry {
        match self {
            Draft::Answer { reply, why } => {
                let (choice, text) = match reply {
                    Reply::Choice(i) => (Some(*i as u64 + 1), None),
                    Reply::Text(text) => (None, Some(text.clone())),
                };
                Entry {
                    choice,
                    text,
                    why: why.trim().to_string(),
                    ..Entry::default()
                }
            }
            Draft::Needless(place) => Entry {
                needless: Some(place.clone()),
                ..Entry::default()
            },
        }
    }

    /// The draft of an entry of the drafts file, or `None` for an entry that is not valid.
    pub fn of_entry(entry: &Entry) -> Option<Draft> {
        use crate::questions::set::SetReply;
        let reply = entry.reply().ok()?;
        Some(match reply {
            SetReply::Answer(given) => Draft::Answer {
                reply: match given.reply {
                    crate::questions::Reply::Choice(i) => Reply::Choice(i),
                    crate::questions::Reply::Text(text) => Reply::Text(text),
                },
                why: given.why,
            },
            SetReply::Needless(reason) => {
                Draft::Needless(crate::questions::needless_name(reason).to_string())
            }
        })
    }
}

/// A part of the form that a click can hit. The view computes where each one is, in one place
/// for each page.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Target {
    /// A tab of the question row: a question by its index, or the review page.
    Page(usize),
    /// A choice of the question that shows, by its index from 0.
    Choice(usize),
    /// A row of the review page, by the index of its question.
    Row(usize),
    /// The send of the review page.
    Send,
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
    /// A retry runs only the writes of the step again: its turn held its end facts, or it is
    /// an act step, which runs no turn ([`crate::pipeline::facts::StateFacts::writes_only`]).
    pub writes_only: bool,
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
    /// The step is an act step: it runs no turn, so a retry runs only its writes again.
    pub act: bool,
    /// A retry runs only the writes of the step again: `held` or `act`
    /// ([`crate::pipeline::facts::StateFacts::writes_only`]).
    pub writes_only: bool,
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

/// The reply that Enter takes on a question with no draft: the proposal of the Judge, else the
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
    /// The draft answers of the questions, by question id. The first load fills them from the
    /// drafts files; after it, the card holds them, and each change goes to the file.
    pub drafts: BTreeMap<String, Draft>,
    /// True after a load gave the drafts of the files.
    pub drafts_read: bool,
    /// The page of a form: the index of a question, or the number of its questions for the
    /// review page.
    pub page: usize,
    /// The selected row of the review page, by the index of its question.
    pub row: usize,
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
            drafts: BTreeMap::new(),
            drafts_read: false,
            page: 0,
            row: 0,
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

    /// The form that shows, when the item is a set of questions.
    pub fn set(&self) -> Option<&QuestionSet> {
        self.item().and_then(NeedItem::set)
    }

    /// The question that shows on a page of a form, by its index.
    pub fn question(&self) -> Option<usize> {
        let set = self.set()?;
        (self.page < set.len()).then_some(self.page)
    }

    /// True when the review page of a form shows.
    pub fn reviewing(&self) -> bool {
        self.set().is_some_and(|set| self.page >= set.review())
    }

    /// The drafts of the questions of one ticket, for its drafts file.
    pub fn drafts_of(&self, ticket: &str) -> Vec<(String, Draft)> {
        let ids: Vec<&str> = self
            .items
            .iter()
            .flatten()
            .filter_map(NeedItem::set)
            .filter(|set| set.ticket == ticket)
            .flat_map(|set| set.cards.iter().map(|c| c.id.as_str()))
            .collect();
        self.drafts
            .iter()
            .filter(|(id, _)| ids.contains(&id.as_str()))
            .map(|(id, draft)| (id.clone(), draft.clone()))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::questions::ProposalView;

    fn card(id: &str, proposal: bool) -> CardView {
        CardView {
            id: id.into(),
            ticket: "DEMO-12".into(),
            turn: "20261116T140000Z-DEMO-12-build-1".into(),
            agent: "builder".into(),
            step: "build".into(),
            proposal: proposal.then(ProposalView::default),
            ..CardView::default()
        }
    }

    fn set(n: usize) -> QuestionSet {
        QuestionSet {
            turn: "20261116T140000Z-DEMO-12-build-1".into(),
            ticket: "DEMO-12".into(),
            cards: (1..=n)
                .map(|i| card(&format!("Q-000{i}"), i == 2))
                .collect(),
            saved: BTreeMap::new(),
            gone: Vec::new(),
        }
    }

    fn choice(i: usize, why: &str) -> Draft {
        Draft::Answer {
            reply: Reply::Choice(i),
            why: why.into(),
        }
    }

    #[test]
    fn the_marks_the_missing_questions_and_the_pages_follow_the_drafts() {
        let set = set(3);
        let mut drafts = BTreeMap::new();
        assert_eq!(
            (0..3).map(|i| set.mark(i, &drafts)).collect::<Vec<_>>(),
            [Mark::Open, Mark::Proposed, Mark::Open]
        );
        assert_eq!(set.first_page(&drafts), 0);
        assert_eq!(set.answers(&drafts), Err(vec![0, 1, 2]));
        drafts.insert("Q-0001".to_string(), choice(1, "People compare."));
        assert_eq!(set.mark(0, &drafts), Mark::Drafted);
        assert_eq!(set.first_page(&drafts), 1);
        // After a draft, the form goes to the next question with no draft after it.
        assert_eq!(set.after_draft(0, &drafts), 1);
        drafts.insert("Q-0003".to_string(), Draft::Needless("code".into()));
        assert_eq!(
            set.after_draft(2, &drafts),
            set.review(),
            "no question after it waits"
        );
        assert_eq!(set.after_draft(0, &drafts), 1);
        assert_eq!(set.missing(&drafts), [1]);
        drafts.insert("Q-0002".to_string(), choice(0, ""));
        assert_eq!(set.first_page(&drafts), 3, "each question has a draft");
        let answers = set.answers(&drafts).unwrap();
        let ids: Vec<&str> = answers.iter().map(|(id, _)| id.as_str()).collect();
        assert_eq!(ids, ["Q-0001", "Q-0002", "Q-0003"]);
    }

    #[test]
    fn a_draft_goes_to_an_entry_of_the_file_and_back() {
        for draft in [
            choice(1, "People compare."),
            Draft::Answer {
                reply: Reply::Text("Keep the order".into()),
                why: String::new(),
            },
            Draft::Needless("no-matter".into()),
        ] {
            assert_eq!(Draft::of_entry(&draft.entry()), Some(draft.clone()));
        }
        assert_eq!(
            choice(1, "w").entry().choice,
            Some(2),
            "a choice counts from 1"
        );
        let bad = Entry {
            needless: Some("nowhere".into()),
            ..Entry::default()
        };
        assert_eq!(Draft::of_entry(&bad), None);
    }

    #[test]
    fn the_asker_names_the_agent_or_the_ask_step_and_a_text_names_a_question() {
        let mut one = set(2);
        assert_eq!(one.asker(), "builder asks at the step build");
        one.turn = "ask:DEMO-12:questions:1".into();
        assert_eq!(one.asker(), "the ask step questions");
        let text = "no answer was recorded, because the answer to Q-0002 was refused: Q-0002 already has an answer";
        assert_eq!(one.named_in(text), Some(1));
        assert_eq!(one.named_in("Q-00012 is not here"), None);
    }

    #[test]
    fn the_drafts_of_a_ticket_are_the_drafts_of_its_questions() {
        let mut card = NeedsCard::new(None, 1);
        let mut other = set(1);
        other.ticket = "DEMO-7".into();
        other.turn = "t-7".into();
        other.cards[0].id = "Q-0009".into();
        card.items = Some(vec![
            NeedItem::Questions(set(2)),
            NeedItem::Questions(other),
        ]);
        card.drafts.insert("Q-0001".into(), choice(0, ""));
        card.drafts.insert("Q-0009".into(), choice(1, ""));
        card.drafts.insert("Q-0099".into(), choice(1, ""));
        let ids: Vec<String> = card
            .drafts_of("DEMO-12")
            .into_iter()
            .map(|(id, _)| id)
            .collect();
        assert_eq!(ids, ["Q-0001"]);
        assert_eq!(card.question(), Some(0));
        card.page = 2;
        assert!(card.reviewing());
        assert_eq!(card.question(), None);
    }
}
