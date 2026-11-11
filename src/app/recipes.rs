//! The chain of each key of a card: its steps in order, what each step does in words, what is
//! true for the ticket after it, and the notice at the end. Pure functions of the data that the
//! card showed, so that each chain names the exact thing: the id of the card, the hash of the
//! plan, or the verb with its commit and its arguments.
//!
//! The trust ladder stays as it is: a chain changes a mode only with `vision promote`, one step
//! at a time, and a check before each promote makes sure that the mode is still the one that
//! the card showed.

use super::chain::{commit_text, Chain, Check, Owner, Step, Work};
use super::msg::{Action, Given, Handoff};
use super::needs::{FlagItem, PlanItem, ProposalItem, WriteItem};
use crate::config::Mode;
use crate::executor::{commit_arg, promoted, LADDER};
use crate::pipeline::SHORT_HASH;
use crate::questions::CardView;

/// The words of a place of a needless card.
pub fn needless_words(place: &str) -> &'static str {
    match place {
        "code" => "the code had the answer",
        "ticket" => "the ticket had the answer",
        "memory" => "the memory had the answer",
        _ => "each choice is fine",
    }
}

/// `vision answer`: a choice, a text, or a needless card.
pub fn answer(id: u64, card: &CardView, given: Given, why: &str) -> Chain {
    let q = &card.id;
    let (label, done) = match &given {
        Given::Needless(place) => (
            format!("mark {q} as needless"),
            format!(
                "Marked {q} as needless: {}. {} decides it itself.",
                needless_words(place),
                card.agent
            ),
        ),
        _ if why.trim().is_empty() => (
            format!("save your answer to {q}"),
            format!(
                "Saved your answer to {q}. {} gets it when each question of its turn has an answer.",
                card.agent
            ),
        ),
        _ => (
            format!("save your answer to {q} with its reason"),
            format!(
                "Saved your answer to {q} with its reason. {} gets it when each question of its turn has an answer.",
                card.agent
            ),
        ),
    };
    let action = Action::Answer {
        id: q.clone(),
        given,
        why: why.trim().to_string(),
    };
    Chain::new(
        id,
        Owner::Need(format!("question:{q}")),
        vec![Step::new(
            Work::Act(action),
            label,
            "Vision keeps your answer.",
        )],
        format!("{q} still waits for your answer."),
        done,
    )
}

/// `vision approve-plan KEY HASH` with the full hash that the card showed, after a check that
/// the plan on disk still has it.
pub fn approve_plan(id: u64, item: &PlanItem) -> Chain {
    let key = &item.ticket;
    let hash = &item.plan.hash;
    let short = &hash[..SHORT_HASH.min(hash.len())];
    Chain::new(
        id,
        Owner::Need(format!("plan:{key}")),
        vec![
            Step::new(
                Work::Check(Check::Plan {
                    ticket: key.clone(),
                    hash: hash.clone(),
                }),
                format!("check that the plan still has the hash {short}"),
                format!("The plan of {key} still waits for your approval."),
            )
            .hidden(),
            Step::new(
                Work::Act(Action::ApprovePlan {
                    key: key.clone(),
                    hash: hash.clone(),
                }),
                format!("approve the plan {short} of {key}"),
                format!("The plan {short} of {key} is approved."),
            ),
        ],
        format!("The plan of {key} still waits for your approval."),
        format!("Approved the plan {short}. {key} moves on to its next step at the next pass."),
    )
}

/// The check that the operation of the card still waits, unchanged.
fn waits(item: &WriteItem, passes: bool) -> Check {
    Check::Waits {
        ticket: item.ticket.clone(),
        op: item.op.clone(),
        verb: item.verb.clone(),
        head: item.head.clone(),
        args: item.args.clone(),
        passes,
    }
}

fn approve_action(item: &WriteItem) -> Action {
    Action::Approve {
        key: item.ticket.clone(),
        verb: item.verb.clone(),
        commit: commit_arg(&item.head).to_string(),
        op: item.op.clone(),
    }
}

/// The text of the mode auto for people: what it means for later writes of the verb.
pub fn auto_words(item: &WriteItem) -> String {
    if item.leaves {
        format!(
            "Vision never asks again for {}, except for a diff that touches a sensitive path that the approved plan does not name.",
            item.verb
        )
    } else {
        format!("Vision never asks again for {}.", item.verb)
    }
}

/// The write card of an operation that waits: `y` approves it; with `auto`, the verb then also
/// moves from the mode ask to the mode auto.
pub fn approve(id: u64, item: &WriteItem, auto: bool) -> Chain {
    let (key, verb) = (&item.ticket, &item.verb);
    let at = commit_text(&item.head);
    let mut steps = vec![
        Step::new(
            Work::Check(waits(item, false)),
            format!("check that the same write waits: {verb} at {at}"),
            format!("{verb} of {key} still waits for your approval. Nothing was sent."),
        ),
        Step::new(
            Work::Act(approve_action(item)),
            format!("approve {verb} at {at}"),
            format!("{verb} of {key} ran."),
        ),
    ];
    let mut done = format!("Sent: {verb} of {key} ran.");
    if auto {
        steps.push(
            Step::new(
                Work::Check(Check::Mode {
                    ticket: key.clone(),
                    verb: verb.clone(),
                    mode: Mode::Ask.as_str().into(),
                }),
                format!("check that {verb} is still in the mode ask"),
                format!("{verb} of {key} ran, and {verb} is still in the mode ask."),
            )
            .hidden(),
        );
        steps.push(Step::new(
            Work::Act(Action::Promote { verb: verb.clone() }),
            format!("move {verb} from ask to auto"),
            format!("{verb} of {key} ran, and {verb} is in the mode auto now."),
        ));
        // The card said what the mode auto means: the notice stays in one line.
        done = format!("Sent: {verb} of {key} ran. {verb} is in the mode auto now.");
    }
    Chain::new(
        id,
        Owner::Need(format!("approval:{}", item.op)),
        steps,
        format!("{verb} of {key} still waits for your approval. Nothing was sent."),
        done,
    )
}

/// The modes from `from` up to `to`, one step of the ladder at a time.
fn ladder(from: &str, to: Mode) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let Some(mut mode) = LADDER.iter().copied().find(|m| m.as_str() == from) else {
        return out;
    };
    while mode != to {
        let Some(next) = promoted(mode) else {
            break;
        };
        out.push((mode.as_str().to_string(), next.as_str().to_string()));
        mode = next;
    }
    out
}

/// The words of the change of mode of a key of the first write card, such as `shadow to ask`,
/// or `None` when the verb is already there or above it.
pub fn climb(item: &WriteItem, auto: bool) -> Option<String> {
    let to = if auto { Mode::Auto } else { Mode::Ask };
    let steps = ladder(&item.mode, to);
    let (first, last) = (steps.first()?, steps.last()?);
    Some(format!("{} to {}", first.0, last.1))
}

/// True when `y` of the first write card can run: the verb is not above the mode ask.
pub fn can_ask(item: &WriteItem) -> bool {
    matches!(item.mode.as_str(), "hold" | "shadow" | "ask")
}

/// The first write card: a verb in the mode shadow or hold. `y` promotes the verb up to the mode
/// ask, retries the ticket, waits for a pass to ask for the same operation again, and approves
/// it when it has the same verb, commit, and arguments. With `auto`, the verb goes up to the
/// mode auto, the ticket retries, and a pass sends the operation.
pub fn first_write(id: u64, item: &WriteItem, auto: bool) -> Chain {
    let (key, verb, step) = (&item.ticket, &item.verb, &item.step);
    let at = commit_text(&item.head);
    let target = if auto { Mode::Auto } else { Mode::Ask };
    let before = format!("Nothing was sent, and {key} still waits at the step {step}.");
    let mut steps = vec![Step::new(
        Work::Check(Check::Ready {
            ticket: key.clone(),
            step: step.clone(),
            verb: verb.clone(),
            mode: item.mode.clone(),
        }),
        format!("check that {verb} is still in the mode {}", item.mode),
        before.clone(),
    )
    .hidden()];
    for (i, (from, to)) in ladder(&item.mode, target).into_iter().enumerate() {
        if i > 0 {
            steps.push(
                Step::new(
                    Work::Check(Check::Mode {
                        ticket: key.clone(),
                        verb: verb.clone(),
                        mode: from.clone(),
                    }),
                    format!("check that {verb} is still in the mode {from}"),
                    format!("{verb} is in the mode {from} now. {before}"),
                )
                .hidden(),
            );
        }
        steps.push(Step::new(
            Work::Act(Action::Promote { verb: verb.clone() }),
            format!("move {verb} from {from} to {to}"),
            format!("{verb} is in the mode {to} now. {before}"),
        ));
    }
    let mode = target.as_str();
    let retried = if auto {
        format!(
            "{verb} is in the mode {mode} now. {key} goes on at the step {step}, and the next pass sends {verb}."
        )
    } else {
        format!(
            "{verb} is in the mode {mode} now. {key} goes on at the step {step}, and the next pass asks for {verb} again. Nothing was sent yet."
        )
    };
    let retry_label = if item.turn_held {
        format!("run the writes of the step {step} of {key} again")
    } else {
        format!("start the step {step} of {key} again")
    };
    steps.push(Step::new(
        Work::Act(Action::Retry { key: key.clone() }),
        retry_label,
        retried.clone(),
    ));
    let pass_label = if auto {
        format!("a pass sends {verb}")
    } else {
        format!("a pass asks for {verb} again")
    };
    steps.push(Step::new(Work::Pass, pass_label, retried.clone()));
    let done = if auto {
        steps.push(Step::new(
            Work::Check(Check::Sent {
                ticket: key.clone(),
                op: item.op.clone(),
                verb: verb.clone(),
            }),
            format!("check that {verb} ran"),
            format!("{verb} of {key} ran, and {verb} is in the mode auto."),
        ));
        format!("Sent: {verb} of {key} ran. {verb} is in the mode auto now.")
    } else {
        steps.push(Step::new(
            Work::Check(waits(item, true)),
            format!("check that the same write waits: {verb} at {at}"),
            retried,
        ));
        steps.push(Step::new(
            Work::Act(approve_action(item)),
            format!("approve {verb} at {at}"),
            format!("{verb} of {key} ran. Vision asks you each time for {verb}."),
        ));
        format!("Sent: {verb} of {key} ran. Vision asks you each time for {verb}.")
    };
    Chain::new(
        id,
        Owner::Need(format!("first-write:{}", item.op)),
        steps,
        before,
        done,
    )
}

/// `vision retry KEY` for a ticket that a rule stopped, or that the person paused.
pub fn retry(id: u64, item: &FlagItem, paused: bool) -> Chain {
    let (key, step) = (&item.ticket, &item.step);
    let (owner, label, after, before) = if paused {
        (
            format!("paused:{key}"),
            format!("continue {key}"),
            format!("{key} goes on, and its next turn resumes the session that you worked in."),
            format!("{key} is still paused."),
        )
    } else {
        let label = if item.held {
            format!("run the writes of the step {step} of {key} again")
        } else {
            format!("start the step {step} of {key} again")
        };
        (
            format!("flag:{key}"),
            label,
            format!("{key} goes on at the step {step}."),
            format!("{key} still waits for you at the step {step}."),
        )
    };
    Chain::new(
        id,
        Owner::Need(owner),
        vec![Step::new(
            Work::Act(Action::Retry { key: key.clone() }),
            label,
            after.clone(),
        )],
        before,
        after,
    )
}

/// The review of a proposal of the Curator in the terminal.
pub fn review(id: u64, item: &ProposalItem) -> Chain {
    let name = &item.name;
    Chain::new(
        id,
        Owner::Need(format!("proposal:{name}")),
        vec![Step::new(
            Work::Session(Handoff::Review(name.clone())),
            format!("open the review of {name} in this terminal"),
            "You left the review.",
        )],
        format!("The changes of {name} still wait for your review."),
        format!("You left the review of {name}."),
    )
}

/// The open card of a running turn: stop the turn, then open its session.
pub fn take_over(id: u64, key: &str, agent: &str) -> Chain {
    Chain::new(
        id,
        Owner::TakeOver {
            key: key.to_string(),
            agent: agent.to_string(),
        },
        vec![
            Step::new(
                Work::Act(Action::Stop {
                    key: key.to_string(),
                }),
                format!("stop the turn of {agent}"),
                format!(
                    "{key} is stopped. The turn of {agent} that ran is lost, and its session keeps what the agent did."
                ),
            ),
            Step::new(
                Work::Session(Handoff::Session(key.to_string())),
                format!("open the session of {agent} in this terminal"),
                format!("{key} is stopped and paused."),
            ),
        ],
        format!("The turn of {agent} still runs."),
        String::new(),
    )
}

/// The card after a session that the person took over: start the ticket again with the take,
/// then continue it with the retry. When the session opened, the ticket is paused, so the next
/// turn resumes the session that the person worked in.
pub fn continue_after(id: u64, key: &str, opened: bool, owner: Owner) -> Chain {
    let (taken, done) = if opened {
        (
            format!("{key} is no longer stopped, and it is still paused. Press n to continue it."),
            format!("{key} goes on. Its next turn resumes the session that you worked in."),
        )
    } else {
        (
            format!("{key} is no longer stopped. Press n to see what waits for it."),
            format!("{key} goes on at its step."),
        )
    };
    let label = if opened {
        format!("continue {key}: its next turn resumes the session")
    } else {
        format!("continue {key} at its step")
    };
    let steps = vec![
        Step::new(
            Work::Act(Action::Take {
                key: key.to_string(),
            }),
            format!("start {key} again at its step"),
            taken,
        ),
        Step::new(
            Work::Act(Action::Retry {
                key: key.to_string(),
            }),
            label,
            done.clone(),
        ),
    ];
    let before = if opened {
        format!("{key} is stopped and paused.")
    } else {
        format!("{key} is stopped.")
    };
    Chain::new(id, owner, steps, before, done)
}

/// Models: Enter pins the values that show, with `vision model set`.
pub fn model_set(id: u64, agent: &str, model: &str, effort: &str) -> Chain {
    Chain::new(
        id,
        Owner::Models(agent.to_string()),
        vec![Step::new(
            Work::Act(Action::ModelSet {
                agent: agent.to_string(),
                model: model.to_string(),
                effort: effort.to_string(),
            }),
            format!("pin {model} {effort} for {agent}"),
            format!("{agent} uses {model} {effort}."),
        )],
        format!("{agent} keeps its model."),
        // The screen says that a change holds for the next turn: the notice stays in one line.
        format!("{agent} uses {model} {effort} from its next turn."),
    )
}

/// Models: `u` goes back to the default, with `vision model reset`.
pub fn model_reset(id: u64, agent: &str, model: &str, effort: &str) -> Chain {
    Chain::new(
        id,
        Owner::Models(agent.to_string()),
        vec![Step::new(
            Work::Act(Action::ModelReset {
                agent: agent.to_string(),
            }),
            format!("go back to the default for {agent}"),
            format!("{agent} uses the default {model} {effort}."),
        )],
        format!("{agent} keeps its model."),
        format!("{agent} uses the default again from its next turn: {model} {effort}."),
    )
}
