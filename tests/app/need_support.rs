//! The samples of the things that wait for the person, for the tests of the cards of part A3:
//! a question with and without a proposal of the Judge, a plan, a write that waits, the first
//! write of a verb, a rule that stopped a ticket, a paused ticket, and a proposal of the
//! Curator. Also the helper that opens the card of `n` with a list of items.

use serde_json::json;
use vision::app::chain::{Check, CheckFacts, OpFacts};
use vision::app::model::{App, Card};
use vision::app::msg::{ChildResult, Effect, JobDone, JobKind, Key, Msg};
use vision::app::needs::{FlagItem, NeedItem, NeedsCard, PlanItem, ProposalItem, WriteItem};
use vision::pipeline::PlanView;
use vision::questions::{CardView, ChoiceView, ProposalView};

use super::support::*;

/// A question of builder with two choices. With `judged`, the Judge proposed the first one.
pub fn question(judged: bool) -> CardView {
    CardView {
        id: "Q-0002".into(),
        ticket: "DEMO-12".into(),
        agent: "builder".into(),
        step: "build".into(),
        state: "asked".into(),
        question: "Must the export keep the order of the columns?".into(),
        context: "The ticket asks for a CSV file, and it does not say how the columns go.".into(),
        example: "The table shows date, client, total. The file can keep that order or sort the columns by name.".into(),
        choices: vec![
            ChoiceView {
                n: 1,
                letter: Some('a'),
                label: "Keep the order".into(),
                means: "the file has the columns of the table, in the same order".into(),
            },
            ChoiceView {
                n: 2,
                letter: Some('b'),
                label: "Sort by name".into(),
                means: "the file sorts the columns by their names".into(),
            },
        ],
        recommended: Some(1),
        why: "People compare the file with the table.".into(),
        evidence: "src/export.py:42".into(),
        proposal: judged.then(|| ProposalView {
            answer: "Keep the order".into(),
            choice: Some(1),
            reason: "D-0004 says that each export keeps the order of the screen.".into(),
            sources: vec!["D-0004".into()],
        }),
        judge_reason: (!judged).then(|| "No decision of the memory covers the order of the columns.".to_string()),
        command: String::new(),
    }
}

pub const PLAN_HASH: &str = "3f2a1b4c5d6e7f8091a2b3c4d5e6f708192a3b4c5d6e7f8091a2b3c4d5e6f7081";

pub fn plan_item(edited: bool) -> NeedItem {
    let mut text = String::from(
        "# Plan of DEMO-12\n\n## Decisions\n\n- The export keeps the order of the columns.\n- The file uses commas and a header row.\n\n## Proof\n\ntest_export.py fails on the base and passes after the change.\n",
    );
    for n in 1..=12 {
        text.push_str(&format!(
            "\n- Step {n} of the build writes one part of the export."
        ));
    }
    NeedItem::Plan(PlanItem {
        ticket: "DEMO-12".into(),
        step: "plan-ready".into(),
        plan: PlanView {
            text,
            hash: PLAN_HASH.into(),
            edited,
            waits: true,
        },
    })
}

pub const HEAD: &str = "1a2b3c4d5e6f708192a3b4c5d6e7f8091a2b3c4d";

/// The id of the operation of [`write`].
pub const OP: &str = "op-7de4e82b";

/// A write of the verb `tracker.describe` in the mode `mode`.
pub fn write(mode: &str) -> WriteItem {
    WriteItem {
        ticket: "DEMO-12".into(),
        op: OP.into(),
        verb: "tracker.describe".into(),
        about: Some("Replaces the description of the ticket in the tracker.".into()),
        leaves: true,
        mode: mode.into(),
        head: HEAD.into(),
        args: json!({"text": "Export the report of the month as a CSV file.\n\nThe file keeps the order of the columns of the table."}),
        intent: None,
        held: Vec::new(),
        step: "refine".into(),
        turn_held: true,
    }
}

/// An operation that waits for the approval of the person.
pub fn approval(mode: &str, held: &[&str]) -> NeedItem {
    let mut item = write(mode);
    item.held = held.iter().map(|s| s.to_string()).collect();
    NeedItem::Approval(item)
}

/// The first write of a verb in the mode shadow or hold, with the intent that the executor
/// recorded.
pub fn first_write(mode: &str) -> NeedItem {
    let mut item = write(mode);
    item.intent = (mode == "shadow").then(|| {
        json!({
            "verb": "tracker.describe",
            "args": {"text": "Export the report of the month as a CSV file.\n\nThe file keeps the order of the columns of the table."},
            "gates": "passed"
        })
    });
    NeedItem::FirstWrite(item)
}

pub fn flag_item(stopped: bool) -> FlagItem {
    FlagItem {
        ticket: "DEMO-12".into(),
        step: "build".into(),
        reason: "the proof did not pass at 1a2b3c4d5e6f: test_export.py failed on the change"
            .into(),
        since: Some(minutes_ago(12)),
        agent: Some("builder".into()),
        last: Some("builder".into()),
        branch: Some("vision/DEMO-12".into()),
        held: false,
        stopped,
    }
}

pub fn flag(stopped: bool) -> NeedItem {
    NeedItem::Flag(flag_item(stopped))
}

pub fn paused(stopped: bool) -> NeedItem {
    let mut item = flag_item(stopped);
    item.reason = "you took over the session".into();
    NeedItem::Paused(item)
}

pub fn proposal(replaying: bool) -> NeedItem {
    NeedItem::Proposal(ProposalItem {
        name: "P-0003".into(),
        summary: "Two answers of the person say the same thing as D-0004. The Curator merges them."
            .into(),
        open: 2,
        replaying,
    })
}

/// Home with one need of DEMO-12, then `n`, then the items as the load of the card.
pub fn open_with(size: (u16, u16), items: Vec<NeedItem>) -> App {
    let mut app = home_with(
        size,
        local(two_tickets(), vec![card_need("DEMO-12")]),
        Some(queue()),
    );
    let load = press_n(&mut app);
    send(
        &mut app,
        2.1,
        Msg::Needs {
            load,
            items: Box::new(Ok(items)),
        },
    );
    app
}

/// `n`, and the number of the load that it asked for.
pub fn press_n(app: &mut App) -> u64 {
    let fx = send(app, 2.0, Msg::Key(Key::Char('n')));
    fx.iter()
        .find_map(|e| match e {
            Effect::LoadNeeds(load) => Some(*load),
            _ => None,
        })
        .unwrap_or_else(|| panic!("n asked for no load: {fx:?}"))
}

/// The card of the things that wait.
pub fn need_card(app: &App) -> &NeedsCard {
    match &app.card {
        Some(Card::Need(card)) => card,
        other => panic!("no card of the needs: {other:?}"),
    }
}

/// The jobs that the effects hand to the worker.
pub fn jobs(effects: &[Effect]) -> Vec<vision::app::msg::Job> {
    effects
        .iter()
        .filter_map(|e| match e {
            Effect::Run(job) => Some(job.clone()),
            _ => None,
        })
        .collect()
}

/// Types a text into the line that is open.
pub fn type_text(app: &mut App, at: f64, text: &str) {
    for c in text.chars() {
        send(app, at, Msg::Key(Key::Char(c)));
    }
}

/// What [`drive`] did: the arguments of each action in order, and the checks that it read.
#[derive(Debug, Default)]
pub struct Driven {
    pub actions: Vec<Vec<String>>,
    pub checks: Vec<Check>,
    pub passes: usize,
    /// The other effects, such as the session of a take over.
    pub other: Vec<Effect>,
}

/// Plays the worker and the reads for the effects of the update until no job and no read is
/// left: each pass ends well, each action ends well unless its command is `fail`, and each read
/// of a check gives `facts`.
pub fn drive(
    app: &mut App,
    at: f64,
    effects: Vec<Effect>,
    fail: Option<&str>,
    facts: impl Fn(&Check) -> CheckFacts,
) -> Driven {
    let mut out = Driven::default();
    let mut todo: std::collections::VecDeque<Effect> = effects.into();
    let mut rounds = 0;
    while let Some(effect) = todo.pop_front() {
        rounds += 1;
        assert!(rounds < 500, "the chain never ends: {out:?}");
        let more = match effect {
            Effect::Run(job) => {
                let result = match &job.kind {
                    JobKind::Pass => {
                        out.passes += 1;
                        ok_result()
                    }
                    JobKind::Action(action) => {
                        let args = action.args();
                        let failed = fail.is_some_and(|f| args[0] == f);
                        out.actions.push(args);
                        if failed {
                            ChildResult {
                                code: Some(1),
                                stderr: "error: the command failed in the test\n".into(),
                                ..ChildResult::default()
                            }
                        } else {
                            ok_result()
                        }
                    }
                };
                send(
                    app,
                    at,
                    Msg::JobDone(JobDone {
                        id: job.id,
                        kind: job.kind.clone(),
                        result,
                    }),
                )
            }
            Effect::LoadCheck { chain, check } => {
                let found = facts(&check);
                out.checks.push(check);
                send(
                    app,
                    at,
                    Msg::Checked {
                        chain,
                        facts: Box::new(Ok(found)),
                    },
                )
            }
            Effect::LoadLocal | Effect::LoadNeeds(_) | Effect::LoadTicket(_) => Vec::new(),
            other => {
                out.other.push(other);
                Vec::new()
            }
        };
        todo.extend(more);
    }
    out
}

fn ok_result() -> ChildResult {
    ChildResult {
        code: Some(0),
        stdout: "done\n".into(),
        ..ChildResult::default()
    }
}

/// The facts of DEMO-12 that waits at `step` with the flag `needs-you`, with its verb in the
/// mode `mode` and the operations `ops`.
pub fn facts(step: &str, mode: &str, ops: Vec<OpFacts>) -> CheckFacts {
    CheckFacts {
        mode: Some(mode.into()),
        step: Some(step.into()),
        flags: vec!["needs-you".into()],
        reason: Some("tracker.describe is in the mode shadow".into()),
        plan: Some(PLAN_HASH.into()),
        ops,
    }
}

/// The operation of the sample write as a check reads it.
pub fn op_facts(state: &str, waits: bool) -> OpFacts {
    let item = write("ask");
    OpFacts {
        id: item.op,
        state: state.into(),
        head: item.head,
        args: item.args,
        waits,
        detail: String::new(),
    }
}
