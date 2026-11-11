//! Questions and the Judge (section 8): cards, the Judge, the answers of the person, and the
//! decisions.
//!
//! The library API that later milestones build on:
//!
//! - [`tick`] is the hook of the tick. `tick.rs` calls it right after the turn module collected
//!   the ended turns. It takes in the cards of each work turn that ended with the kind
//!   `questions`, applies the result of each Judge turn, sends the cards of a late Judge to the
//!   person, starts one Judge turn for the new cards of each asking turn, and resumes each
//!   asking session whose cards all have a final answer. It is safe to run two times.
//! - [`needs`] gives the cards that wait for the person, for the snapshot.
//! - [`run`] is `vision needs`, `answer`, `answers`, and `correct`.
//! - [`replay`] replays the last answers of the person with a Judge turn against a memory
//!   folder, for the Curator and for `vision model eval`. It writes nothing.
//! - [`rules::rule`] is the pure function of the rules in code (section 8.3), and
//!   [`card::check`] the test of a card.
//!
//! The pipeline must not resume a turn whose result has the kind `questions`, and must not
//! start a Judge turn: this module owns both.

pub mod card;
mod commands;
mod intake;
mod judge;
mod record;
pub mod replay;
mod resume;
pub mod rules;
mod tick;
mod view;

use std::collections::BTreeMap;
use std::fs;

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::cli::{Needless, QuestionsCommand};
use crate::ctx::Ctx;
use crate::ledger::questions::{QuestionRow, STATE_ASKED, STATE_PROPOSED};
use crate::ledger::Ledger;
use crate::memory::store::short;
use crate::snapshot::{Need, NeedKind};
use crate::turn::{self, LaunchRequest, Launched};

pub use card::{Answer, Card, Reply};
pub use commands::{answer_card, correct_card, mark_needless, Answered, Corrected, PersonAnswer};
pub use replay::{
    replay, replay_end, replay_ended, replay_kill, replay_start, ReplayItem, ReplayOptions,
    ReplayReport, ReplayStarted,
};
pub use rules::{Entry, Outcome, Ruling};
pub use tick::tick;

/// The agent of the Judge.
pub const JUDGE: &str = "judge";
/// The step of a Judge turn of the tick.
pub const JUDGE_STEP: &str = "judge";

/// `vision needs`, `answer`, `answers`, and `correct`.
pub fn run(ctx: &Ctx, command: QuestionsCommand) -> Result<()> {
    match command {
        QuestionsCommand::Needs { json } => view::needs_command(ctx, json),
        QuestionsCommand::Answer(args) => commands::answer_command(ctx, args),
        QuestionsCommand::Answers { days, json } => commands::answers_command(ctx, days, json),
        QuestionsCommand::Correct { id, answer, why } => {
            commands::correct_command(ctx, &id, &answer, why.as_deref())
        }
    }
}

/// The cards that wait for the person: each card in the state `proposed` or `asked`. The tick
/// puts them in the snapshot, and `vision status` and `vision prompt` count them.
pub fn needs(_ctx: &Ctx, ledger: &Ledger) -> Result<Vec<Need>> {
    let mut agents = Agents::default();
    let mut needs = Vec::new();
    for row in ledger.questions_in_states(&[STATE_PROPOSED, STATE_ASKED])? {
        let question = Question::load(ledger, row, &mut agents)?;
        let mut text = format!(
            "{} asks: {}",
            question.agent,
            short(&question.card.text, 100)
        );
        if let Some(proposal) = question.proposal() {
            text.push_str(&format!(
                " (the Judge proposes: {})",
                short(&proposal.text(&question.card), 40)
            ));
        }
        needs.push(Need {
            kind: NeedKind::Card,
            ticket: Some(question.row.ticket.clone()),
            id: question.row.id.clone(),
            text,
        });
    }
    Ok(needs)
}

/// What the row keeps of the Judge in `proposal`: its entry for the card, and why the rules in
/// code changed its outcome.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct JudgeView {
    pub judge: Option<Entry>,
    pub rule: Option<String>,
}

/// A row of `questions` with its parsed card, its answer, and the asking agent and step.
#[derive(Clone, Debug)]
pub struct Question {
    pub row: QuestionRow,
    pub card: Card,
    pub view: JudgeView,
    pub answer: Option<Answer>,
    /// The agent of the asking turn.
    pub agent: String,
    /// The step of the asking turn.
    pub step: String,
}

/// The agent and the step of each asking turn, read once.
#[derive(Debug, Default)]
pub(crate) struct Agents(BTreeMap<String, (String, String)>);

impl Agents {
    fn of(&mut self, ledger: &Ledger, turn: &str) -> Result<(String, String)> {
        if let Some(known) = self.0.get(turn) {
            return Ok(known.clone());
        }
        let row = ledger.turn(turn)?;
        let found = (
            row.as_ref()
                .and_then(|r| r.agent.clone())
                .unwrap_or_else(|| "agent".into()),
            row.and_then(|r| r.step).unwrap_or_default(),
        );
        self.0.insert(turn.to_string(), found.clone());
        Ok(found)
    }
}

impl Question {
    /// Parses a row. A card that no longer parses becomes an empty card, so that the person
    /// still sees and can answer the row.
    pub(crate) fn load(ledger: &Ledger, row: QuestionRow, agents: &mut Agents) -> Result<Question> {
        let card = serde_json::from_str::<Card>(&row.card).unwrap_or_default();
        let view = row
            .proposal
            .as_deref()
            .and_then(|p| serde_json::from_str(p).ok())
            .unwrap_or_default();
        let answer = row
            .answer
            .as_deref()
            .and_then(|a| serde_json::from_str(a).ok());
        let (agent, step) = agents.of(ledger, &row.turn)?;
        Ok(Question {
            row,
            card,
            view,
            answer,
            agent,
            step,
        })
    }

    /// The row with this id. The error says what to do next.
    pub fn find(ledger: &Ledger, id: &str) -> Result<Question> {
        let id = id.trim();
        let Some(row) = ledger.question(id)? else {
            bail!("there is no card {id}. Run `vision needs` to see the cards that wait for you");
        };
        Question::load(ledger, row, &mut Agents::default())
    }

    /// The proposal of the Judge, when the final outcome is `propose`.
    pub fn proposal(&self) -> Option<Reply> {
        if self.row.outcome.as_deref() != Some(Outcome::Propose.as_str()) {
            return None;
        }
        self.view.judge.as_ref().and_then(|e| e.reply(&self.card))
    }

    /// The sources that the Judge named, from the row.
    pub fn sources(&self) -> Vec<String> {
        self.row
            .sources
            .as_deref()
            .and_then(|s| serde_json::from_str(s).ok())
            .unwrap_or_default()
    }

    /// The reason of the Judge, or why the rules sent the card to the person.
    pub fn judge_reason(&self) -> Option<String> {
        let reason = self
            .view
            .judge
            .as_ref()
            .map(|e| e.reason.trim().to_string())
            .filter(|r| !r.is_empty());
        match (&self.view.rule, reason) {
            (Some(rule), Some(reason)) => Some(format!("{reason} ({rule})")),
            (Some(rule), None) => Some(rule.clone()),
            (None, reason) => reason,
        }
    }

    /// The mark of a needless card.
    pub fn needless(&self) -> Option<&str> {
        self.row
            .flag
            .as_deref()
            .and_then(|f| f.strip_prefix("needless:"))
    }
}

/// The name of a reason of a needless card, as the row keeps it.
pub fn needless_name(reason: Needless) -> &'static str {
    match reason {
        Needless::Code => "code",
        Needless::Ticket => "ticket",
        Needless::Memory => "memory",
        Needless::NoMatter => "no-matter",
    }
}

/// The reason of a needless card for people and for the asking session.
pub fn needless_text(reason: &str) -> &'static str {
    match reason {
        "code" => "the code has the answer",
        "ticket" => "the ticket has the answer",
        "memory" => "the memory has the answer",
        "no-matter" => "each choice is fine",
        _ => "the person gave no reason",
    }
}

/// Writes a turn and the rows that claim it in one transaction of the ledger, and then starts
/// its supervisor, so that a crash never leaves rows that claim no turn, or a turn that a
/// later tick starts a second time. `claim` runs inside the transaction.
pub(crate) fn launch_claimed(
    ctx: &Ctx,
    ledger: &Ledger,
    request: &LaunchRequest,
    claim: impl FnOnce(&Ledger, &Launched) -> Result<()>,
) -> Result<Launched> {
    ledger
        .conn()
        .execute_batch("BEGIN IMMEDIATE")
        .context("could not start a transaction of the ledger")?;
    let written =
        turn::prepare(ctx, ledger, request).and_then(|launched| match claim(ledger, &launched) {
            Ok(()) => Ok(launched),
            Err(e) => {
                let _ = fs::remove_dir_all(&launched.dir);
                Err(e)
            }
        });
    let mut launched = match written {
        Ok(launched) => launched,
        Err(e) => {
            let _ = ledger.conn().execute_batch("ROLLBACK");
            return Err(e);
        }
    };
    if let Err(e) = ledger.conn().execute_batch("COMMIT") {
        let _ = ledger.conn().execute_batch("ROLLBACK");
        let _ = fs::remove_dir_all(&launched.dir);
        return Err(anyhow::Error::from(e).context("could not write the turn to the ledger"));
    }
    launched.supervisor_pid = Some(turn::start_supervisor(ctx, &launched)?);
    Ok(launched)
}

/// The value of a JSON list of strings, or an empty list.
pub(crate) fn strings(value: Option<&Value>) -> Vec<String> {
    value
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}
