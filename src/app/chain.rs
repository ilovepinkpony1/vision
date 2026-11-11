//! A chain: the steps that one key of a card runs one after the other. A step is an action (a
//! command of today through the worker), a pass of the engine, a check (a read of the ledger
//! and the configuration, while no job of the worker runs), or a session that gets the
//! terminal. Each step knows the state of the ticket after it, so that a chain that stops can
//! say what is true now.
//!
//! The check guards the safety rule of the app: the app acts only on the exact thing that the
//! card showed. [`judge`] is the pure decision of a check from the facts that the read gave.

use serde_json::Value;

use super::msg::{Action, Handoff};
use crate::executor::short;
use crate::pipeline::state::{NEEDS_YOU, STOPPED};

/// The most passes that a chain runs while it waits for the pipeline to ask for a write again.
pub const MAX_PASSES: u32 = 3;

/// What a step does.
#[derive(Clone, Debug, PartialEq)]
pub enum Work {
    Act(Action),
    Check(Check),
    /// A pass of the engine: the next pass that starts after the step before it.
    Pass,
    Session(Handoff),
}

/// A read and the condition that it tests.
#[derive(Clone, Debug, PartialEq)]
pub enum Check {
    /// The verb is in the mode `mode`, and the ticket waits at the step `step` with the flag
    /// `needs-you` and is not stopped.
    Ready {
        ticket: String,
        step: String,
        verb: String,
        mode: String,
    },
    /// The verb is in the mode `mode`.
    Mode {
        ticket: String,
        verb: String,
        mode: String,
    },
    /// The plan of the ticket still has the hash `hash`.
    Plan { ticket: String, hash: String },
    /// The operation `op` waits for the approval of the person at the commit `head` with the
    /// arguments `args`. With `passes`, a check that finds nothing yet asks for one more pass.
    Waits {
        ticket: String,
        op: String,
        verb: String,
        head: String,
        args: Value,
        passes: bool,
    },
    /// An operation of the verb of the ticket ran and is confirmed.
    Sent {
        ticket: String,
        op: String,
        verb: String,
    },
}

impl Check {
    pub fn ticket(&self) -> &str {
        match self {
            Check::Ready { ticket, .. }
            | Check::Mode { ticket, .. }
            | Check::Plan { ticket, .. }
            | Check::Waits { ticket, .. }
            | Check::Sent { ticket, .. } => ticket,
        }
    }

    /// The verb that the check reads, if any.
    pub fn verb(&self) -> Option<&str> {
        match self {
            Check::Ready { verb, .. }
            | Check::Mode { verb, .. }
            | Check::Waits { verb, .. }
            | Check::Sent { verb, .. } => Some(verb),
            Check::Plan { .. } => None,
        }
    }
}

/// What a check read.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct CheckFacts {
    /// The mode of the verb now.
    pub mode: Option<String>,
    /// The step of the ticket now.
    pub step: Option<String>,
    pub flags: Vec<String>,
    /// The reason of the flag `needs-you`.
    pub reason: Option<String>,
    /// The full hash of the plan of the ticket.
    pub plan: Option<String>,
    /// The operations of the verb of the ticket, oldest first.
    pub ops: Vec<OpFacts>,
}

/// One operation as a check reads it.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct OpFacts {
    pub id: String,
    /// pending, executing, confirmed, failed, or uncertain.
    pub state: String,
    pub head: String,
    pub args: Value,
    /// True when it waits for the approval of the person.
    pub waits: bool,
    pub detail: String,
}

/// The decision of a check.
#[derive(Clone, Debug, PartialEq)]
pub enum Verdict {
    /// The condition holds: the next step runs.
    Go,
    /// Nothing yet: one more pass, then the same check.
    Again,
    /// The chain stops with this reason.
    Stop(String),
    /// The thing changed after the card showed it: the chain stops, the card loads again and
    /// shows the item `select` with the note.
    Changed {
        select: Option<String>,
        note: String,
    },
}

/// The decision of a check on the facts that its read gave.
pub fn judge(check: &Check, facts: &CheckFacts) -> Verdict {
    let flagged = facts.flags.iter().any(|f| f == NEEDS_YOU);
    let stopped = facts.flags.iter().any(|f| f == STOPPED);
    match check {
        Check::Ready {
            ticket,
            step,
            verb,
            mode,
        } => {
            if let Some(stop) = mode_stop(verb, mode, facts) {
                return stop;
            }
            if facts.step.as_deref() != Some(step.as_str()) {
                return Verdict::Stop(format!(
                    "{ticket} is at the step {} now, not at the step {step} as the card showed.",
                    facts.step.as_deref().unwrap_or("-")
                ));
            }
            if stopped {
                return Verdict::Stop(format!("{ticket} is stopped now."));
            }
            if !flagged {
                return Verdict::Stop(format!(
                    "{ticket} no longer waits for you at the step {step}."
                ));
            }
            Verdict::Go
        }
        Check::Mode { verb, mode, .. } => mode_stop(verb, mode, facts).unwrap_or(Verdict::Go),
        Check::Plan { ticket, hash } => match &facts.plan {
            Some(now) if now == hash => Verdict::Go,
            Some(_) => Verdict::Changed {
                select: Some(format!("plan:{ticket}")),
                note: "The plan changed after the card showed it, so Vision did not approve it. Read the new plan, then press a again.".into(),
            },
            None => Verdict::Stop(format!("{ticket} has no plan now.")),
        },
        Check::Waits {
            op,
            verb,
            head,
            args,
            passes,
            ..
        } => {
            let waiting: Vec<&OpFacts> = facts
                .ops
                .iter()
                .filter(|o| o.state == "pending" && o.waits)
                .collect();
            // The approval names the operation by its id, so another write of the verb that
            // waits at the same commit does not run on it, and does not stop it.
            if waiting
                .iter()
                .any(|o| o.id == *op && o.head == *head && o.args == *args)
            {
                return Verdict::Go;
            }
            if let Some(other) = waiting.last() {
                return Verdict::Changed {
                    select: Some(format!("approval:{}", other.id)),
                    note: format!(
                        "The write {verb} that waits now is not the one that the card showed: its commit or its text changed. Vision did not approve it. Read it, then decide again."
                    ),
                };
            }
            if facts
                .ops
                .iter()
                .any(|o| o.id == *op && o.state == "confirmed")
            {
                return Verdict::Stop(format!("{verb} was already sent."));
            }
            if flagged {
                return Verdict::Stop(format!(
                    "The ticket stopped again: {}",
                    facts.reason.as_deref().unwrap_or("a rule stopped it")
                ));
            }
            if *passes {
                Verdict::Again
            } else {
                Verdict::Stop(format!("{verb} no longer waits for your approval."))
            }
        }
        Check::Sent { verb, .. } => {
            let last = facts.ops.last();
            if last.is_some_and(|o| o.state == "confirmed") {
                return Verdict::Go;
            }
            if let Some(other) = facts.ops.iter().find(|o| o.state == "pending" && o.waits) {
                return Verdict::Changed {
                    select: Some(format!("approval:{}", other.id)),
                    note: format!(
                        "{verb} is in the mode auto now, and this write still waits for your approval: its diff touches a sensitive path that the approved plan does not name."
                    ),
                };
            }
            if let Some(failed) = last.filter(|o| o.state == "failed") {
                return Verdict::Stop(format!("{verb} did not work: {}", failed.detail));
            }
            if flagged {
                return Verdict::Stop(format!(
                    "The ticket stopped again: {}",
                    facts.reason.as_deref().unwrap_or("a rule stopped it")
                ));
            }
            Verdict::Again
        }
    }
}

fn mode_stop(verb: &str, mode: &str, facts: &CheckFacts) -> Option<Verdict> {
    match facts.mode.as_deref() {
        Some(now) if now == mode => None,
        now => Some(Verdict::Stop(format!(
            "{verb} is in the mode {} now, not in the mode {mode}, so Vision does not change it.",
            now.unwrap_or("that nobody knows")
        ))),
    }
}

/// A commit as the cards show it: its short form, or `none` for a ticket with no commit.
pub fn commit_text(head: &str) -> String {
    if head.is_empty() {
        "none".to_string()
    } else {
        short(head).to_string()
    }
}

/// The mark of a step.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mark {
    Todo,
    Runs,
    Done,
    Failed,
}

/// One step of a chain.
#[derive(Clone, Debug, PartialEq)]
pub struct Step {
    pub work: Work,
    /// What the step does, for the card.
    pub label: String,
    /// False for a step that the card does not list, such as a check before a promote.
    pub shown: bool,
    /// What is true for the ticket after the step.
    pub after: String,
    pub mark: Mark,
}

impl Step {
    pub fn new(work: Work, label: impl Into<String>, after: impl Into<String>) -> Step {
        Step {
            work,
            label: label.into(),
            shown: true,
            after: after.into(),
            mark: Mark::Todo,
        }
    }

    /// A step that the card does not list.
    pub fn hidden(mut self) -> Step {
        self.shown = false;
        self
    }
}

/// Who started a chain, and so where its result goes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Owner {
    /// An item of the card of the things that wait, by its id.
    Need(String),
    /// The open card of a running turn: stop the turn, then open its session.
    TakeOver { key: String, agent: String },
    /// The card after a session that the person took over: start the ticket again.
    Continue(String),
    /// The screen Models: the pin or the reset of the model of this agent.
    Models(String),
}

/// How a chain ended.
#[derive(Clone, Debug, PartialEq)]
pub enum End {
    Done,
    /// A step failed or a check stopped the chain: the reason, and what is true now.
    Failed {
        text: String,
        state: String,
    },
}

/// A chain that runs, or that ended and still shows on its card.
#[derive(Clone, Debug, PartialEq)]
pub struct Chain {
    pub id: u64,
    pub owner: Owner,
    pub steps: Vec<Step>,
    /// The step that runs or comes next.
    pub at: usize,
    /// The job of the worker of the step that runs, or the pass that it waits for.
    pub job: Option<u64>,
    /// The step started: its job is queued, its read runs, or its session waits.
    pub started: bool,
    /// The passes that the chain ran.
    pub passes: u32,
    /// What is true before the first step.
    pub before: String,
    /// The notice after the last step.
    pub done: String,
    pub end: Option<End>,
}

impl Chain {
    pub fn new(
        id: u64,
        owner: Owner,
        steps: Vec<Step>,
        before: impl Into<String>,
        done: impl Into<String>,
    ) -> Chain {
        Chain {
            id,
            owner,
            steps,
            at: 0,
            job: None,
            started: false,
            passes: 0,
            before: before.into(),
            done: done.into(),
            end: None,
        }
    }

    /// The step that runs or comes next, while the chain runs.
    pub fn step(&self) -> Option<&Step> {
        if self.end.is_some() {
            return None;
        }
        self.steps.get(self.at)
    }

    pub fn running(&self) -> bool {
        self.end.is_none()
    }

    /// What is true now: the state after the last step that is done.
    pub fn state(&self) -> String {
        self.steps[..self.at.min(self.steps.len())]
            .iter()
            .rev()
            .find(|s| s.mark == Mark::Done)
            .map_or_else(|| self.before.clone(), |s| s.after.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn waits(passes: bool) -> Check {
        Check::Waits {
            ticket: "DEMO-12".into(),
            op: "op-1".into(),
            verb: "notes.write".into(),
            head: "abc1234def".into(),
            args: json!({"text": "Add subtract."}),
            passes,
        }
    }

    fn op(id: &str, state: &str, waits: bool, text: &str) -> OpFacts {
        OpFacts {
            id: id.into(),
            state: state.into(),
            head: "abc1234def".into(),
            args: json!({ "text": text }),
            waits,
            detail: String::new(),
        }
    }

    #[test]
    fn the_same_write_goes_and_a_changed_write_stops() {
        let mut facts = CheckFacts {
            ops: vec![op("op-1", "pending", true, "Add subtract.")],
            ..CheckFacts::default()
        };
        assert_eq!(judge(&waits(true), &facts), Verdict::Go);
        facts.ops[0].args = json!({"text": "Add divide."});
        assert!(matches!(
            judge(&waits(true), &facts),
            Verdict::Changed { select: Some(s), .. } if s == "approval:op-1"
        ));
        // A second write of the verb at the same commit does not stop the approval of the
        // first one: the approval names its operation.
        facts.ops = vec![
            op("op-1", "pending", true, "Add subtract."),
            op("op-2", "pending", true, "Add divide."),
        ];
        assert_eq!(judge(&waits(true), &facts), Verdict::Go);
        // Only another write waits: the card of that write shows.
        facts.ops = vec![
            op("op-1", "failed", false, "Add subtract."),
            op("op-2", "pending", true, "Add divide."),
        ];
        assert!(matches!(
            judge(&waits(true), &facts),
            Verdict::Changed { select: Some(s), .. } if s == "approval:op-2"
        ));
        facts.ops = vec![op("op-1", "pending", false, "Add subtract.")];
        assert_eq!(judge(&waits(true), &facts), Verdict::Again);
        assert!(matches!(judge(&waits(false), &facts), Verdict::Stop(_)));
        facts.flags = vec!["needs-you".into()];
        facts.reason = Some("refused by the size gate".into());
        assert!(matches!(judge(&waits(true), &facts), Verdict::Stop(t) if t.contains("size gate")));
    }

    #[test]
    fn a_mode_or_a_step_that_changed_stops_the_chain() {
        let ready = Check::Ready {
            ticket: "DEMO-12".into(),
            step: "draft".into(),
            verb: "notes.write".into(),
            mode: "shadow".into(),
        };
        let mut facts = CheckFacts {
            mode: Some("shadow".into()),
            step: Some("draft".into()),
            flags: vec!["needs-you".into()],
            ..CheckFacts::default()
        };
        assert_eq!(judge(&ready, &facts), Verdict::Go);
        facts.mode = Some("ask".into());
        assert!(matches!(judge(&ready, &facts), Verdict::Stop(t) if t.contains("mode ask now")));
        facts.mode = Some("shadow".into());
        facts.flags.clear();
        assert!(matches!(judge(&ready, &facts), Verdict::Stop(t) if t.contains("no longer waits")));
    }

    #[test]
    fn a_plan_with_another_hash_is_a_change() {
        let check = Check::Plan {
            ticket: "DEMO-12".into(),
            hash: "aaa".into(),
        };
        let mut facts = CheckFacts {
            plan: Some("aaa".into()),
            ..CheckFacts::default()
        };
        assert_eq!(judge(&check, &facts), Verdict::Go);
        facts.plan = Some("bbb".into());
        assert!(matches!(judge(&check, &facts), Verdict::Changed { .. }));
    }
}
