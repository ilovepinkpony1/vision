//! The decisions of the pipeline (section 11.3) as pure functions. The tick hooks gather the
//! facts of a ticket, call [`decide`], and apply its answer; they decide nothing themselves.
//!
//! - [`decide`]: the next thing that one ticket needs, from the facts of its step.
//! - [`check_end_facts`]: the end facts of a turn result.
//! - [`wait_target`] and [`ci_result`]: where a wait step moves, and the CI check it records.

use std::collections::{BTreeMap, BTreeSet};

use crate::adapter::Expect;
use crate::executor::Outcome;
use crate::turn::TurnStatus;

use super::state::{NEEDS_YOU, PAUSED, STOPPED, WAITS_FOR_LIMIT};

/// The last turn of the visit of a turn step.
#[derive(Clone, Debug, PartialEq)]
pub struct LastTurn {
    pub id: String,
    pub status: TurnStatus,
    /// What went wrong, for a failed turn: the reason and its detail.
    pub reason: String,
    /// The `kind` of its result object: `result`, `questions`, or `requests`.
    pub kind: Option<String>,
    pub session: Option<String>,
    /// `turns.end_fact`: `held`, `failed:<fact>`, or `None` before the test.
    pub end_fact: Option<String>,
}

/// The state of one entry of `after` or `start`.
#[derive(Clone, Debug, PartialEq)]
pub enum OpFact {
    /// The executor has no operation of it at the commit of now.
    NotAsked,
    /// Its arguments cannot be built, such as a `{doc:NAME}` with no document.
    BadArgs(String),
    /// The stored operation.
    Known { outcome: Outcome, detail: String },
}

/// A rule of the turn module that refuses a new turn now.
#[derive(Clone, Debug, PartialEq)]
pub enum Refused {
    /// A subscription window: the flag `waits-for-limit`, which clears by itself.
    Limit(String),
    /// Any other rule, such as a budget: the flag `needs-you`.
    Other(String),
}

/// Which list of entries an operation belongs to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum List {
    After,
    Start,
}

/// The facts of the step of one ticket.
#[derive(Clone, Debug, PartialEq)]
pub enum KindFacts {
    Turn {
        /// The last turn of the visit, or `None` before the first one.
        last: Option<LastTurn>,
        /// The turns of the visit whose end fact failed.
        failures: u32,
        /// The message of the resume after a failed end fact: the fact and what was found.
        failure: Option<String>,
        /// The entries of `after`, in order, with their label.
        after: Vec<(String, OpFact)>,
        /// The session and the message of the first turn of the visit.
        resume: Option<String>,
        message: Option<String>,
    },
    Job {
        /// The result of the job of the step at the commit of now: `None` before it ran.
        passed: Option<bool>,
        detail: String,
        after: Vec<(String, OpFact)>,
    },
    Human {
        action: String,
        done: bool,
    },
    Wait {
        start: Vec<(String, OpFact)>,
        watch: String,
        /// The value of `watch` at the last observe.
        watched: Option<String>,
        /// The step of `on` for that value.
        target: Option<String>,
        /// `wait` of the step: the values that mean "not yet". `None`: each value waits.
        waits: Option<Vec<String>>,
        /// `limit` of the step in seconds.
        limit: Option<u64>,
        /// The seconds since the visit began to watch, or `None` before its first look.
        waited: Option<i64>,
    },
    End {
        finished: bool,
    },
}

/// What [`decide`] reads.
#[derive(Clone, Debug, PartialEq)]
pub struct Facts {
    pub step: String,
    pub kind: KindFacts,
    /// The step after this one: `next` of the step.
    pub next: Option<String>,
    /// A turn step with `on`: the step of `on` for the findings of its held turn. It wins over
    /// `next`.
    pub branch: Option<Branch>,
    pub flags: Vec<String>,
    /// A turn of the ticket runs, such as a work turn or a Judge turn.
    pub running: bool,
    /// The cards of the ticket that have no final answer.
    pub open_cards: i64,
    /// The rule that refuses a turn of this step now.
    pub refusal: Option<Refused>,
    /// The cap of the step: its name and its value.
    pub cap: Option<(String, u32)>,
    /// The entry number of the visit.
    pub n: u32,
    /// The person let the ticket go on above the cap.
    pub skip_cap: bool,
    /// What is open, for the message of a cap: such as the open findings of `review.md`.
    pub open: Vec<String>,
}

/// Where a turn step with `on` moves: the step, and how, such as `on findings`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Branch {
    pub to: String,
    pub how: String,
}

/// The step of `on` of a turn step for the number of kept findings of its turn: `findings`
/// for one or more, `none` for none.
pub fn findings_branch(on: &BTreeMap<String, String>, kept: usize) -> Option<Branch> {
    let key = if kept > 0 {
        crate::adapter::FINDINGS
    } else {
        crate::adapter::NONE
    };
    on.get(key).map(|to| Branch {
        to: to.clone(),
        how: format!("on {key}"),
    })
}

/// The next thing that one ticket needs.
#[derive(Clone, Debug, PartialEq)]
pub enum Next {
    /// Nothing now. The text says why, for the log.
    Nothing(String),
    /// Clear the flag `waits-for-limit`: the limit allows a turn again.
    ClearLimit,
    /// Set a flag with its reason.
    Flag { flag: &'static str, reason: String },
    /// The entry of a turn step: the `before` jobs, `observe`, and the first turn of the visit.
    Start {
        resume: Option<String>,
        message: Option<String>,
    },
    /// Resume the session of the last turn with a message, after a failed end fact.
    Resume { session: String, message: String },
    /// Serve the read verbs of a turn that ended with requests, then resume its session.
    Serve { turn: String, session: String },
    /// Request the entry `index` of `after` or `start` through the executor.
    Request { list: List, index: usize },
    /// Run the job of a job step.
    RunJob,
    /// Move to the step `to`.
    Move { to: String, how: String },
    /// The end step: free the clone, close the span, and record a merged pull request.
    Finish,
}

fn flag(reason: impl Into<String>) -> Next {
    Next::Flag {
        flag: NEEDS_YOU,
        reason: reason.into(),
    }
}

/// True when the visit is one entry above the cap of its step.
pub fn cap_exceeded(n: u32, cap: Option<u32>, skip: bool) -> bool {
    !skip && cap.is_some_and(|cap| n > cap)
}

/// The next thing that one ticket needs, from the facts of its step.
pub fn decide(facts: &Facts) -> Next {
    for (name, why) in [
        (STOPPED, "it is stopped"),
        (PAUSED, "the person took over the session"),
        (NEEDS_YOU, "it needs the person"),
    ] {
        if facts.flags.iter().any(|f| f == name) {
            return Next::Nothing(format!("nothing starts, because {why}"));
        }
    }
    if facts.flags.iter().any(|f| f == WAITS_FOR_LIMIT) {
        return match &facts.refusal {
            Some(Refused::Limit(text)) => Next::Nothing(format!("it waits for the limit: {text}")),
            _ => Next::ClearLimit,
        };
    }
    if let Some(other) = facts.flags.first() {
        return Next::Nothing(format!("nothing starts, because of the flag {other}"));
    }
    if facts.running {
        return Next::Nothing("a turn of the ticket runs".into());
    }
    match &facts.kind {
        KindFacts::Turn {
            last,
            failures,
            failure,
            after,
            resume,
            message,
        } => match last {
            None => {
                let cap = facts.cap.as_ref().map(|(_, v)| *v);
                if cap_exceeded(facts.n, cap, facts.skip_cap) {
                    let (name, value) = facts.cap.clone().unwrap_or_default();
                    return flag(cap_text(&facts.step, &name, value, facts.n, &facts.open));
                }
                if let Some(refused) = refusal_flag(&facts.refusal) {
                    return refused;
                }
                Next::Start {
                    resume: resume.clone(),
                    message: message.clone(),
                }
            }
            Some(turn) => turn_next(facts, turn, *failures, failure.as_deref(), after),
        },
        KindFacts::Job {
            passed,
            detail,
            after,
        } => match passed {
            None => Next::RunJob,
            Some(false) => flag(format!(
                "the job of the step {} did not pass: {detail}",
                facts.step
            )),
            Some(true) => after_next(facts, List::After, after),
        },
        KindFacts::Human { action, done } => {
            if *done {
                move_next(facts)
            } else {
                Next::Nothing(format!("it waits for the person: vision {action}"))
            }
        }
        KindFacts::Wait {
            start,
            watch,
            watched,
            target,
            waits,
            limit,
            waited,
        } => {
            if let Some(stop) = ops_next(List::Start, start) {
                return stop;
            }
            if let Some(to) = target {
                return Next::Move {
                    to: to.clone(),
                    how: format!("on {}", watched.as_deref().unwrap_or("-")),
                };
            }
            if let (Some(waits), Some(value)) = (waits, watched) {
                if !waits.contains(value) {
                    return flag(format!(
                        "the step {} watches {watch}, and its value `{value}` is in neither on nor wait, so nothing moves the ticket. Fix the outside state or adapter.toml, then run vision retry",
                        facts.step
                    ));
                }
            }
            if let (Some(limit), Some(waited)) = (limit, waited) {
                if *waited > i64::try_from(*limit).unwrap_or(i64::MAX) {
                    return flag(format!(
                        "the step {} watched {watch} for {waited} s, above its limit of {limit} s. The last value: {}",
                        facts.step,
                        watched.as_deref().unwrap_or("not observed yet")
                    ));
                }
            }
            Next::Nothing(format!(
                "it waits for {watch}, which is {}",
                watched.as_deref().unwrap_or("not observed yet")
            ))
        }
        KindFacts::End { finished } => {
            if *finished {
                Next::Nothing("the ticket is done".into())
            } else {
                Next::Finish
            }
        }
    }
}

fn turn_next(
    facts: &Facts,
    turn: &LastTurn,
    failures: u32,
    failure: Option<&str>,
    after: &[(String, OpFact)],
) -> Next {
    match turn.status {
        TurnStatus::Running => Next::Nothing(format!("the turn {} runs", turn.id)),
        TurnStatus::Retried => Next::Nothing(format!(
            "the turn {} failed, and its automatic resume runs",
            turn.id
        )),
        TurnStatus::Failed => flag(format!("the turn {} failed: {}", turn.id, turn.reason)),
        TurnStatus::Done => match turn.kind.as_deref() {
            Some("questions") => {
                if facts.open_cards > 0 {
                    Next::Nothing(format!(
                        "it waits for the answers to {} card(s)",
                        facts.open_cards
                    ))
                } else {
                    Next::Nothing(
                        "the cards are answered, and the session resumes with them".into(),
                    )
                }
            }
            Some("requests") => {
                if let Some(refused) = refusal_flag(&facts.refusal) {
                    return refused;
                }
                match &turn.session {
                    Some(session) => Next::Serve {
                        turn: turn.id.clone(),
                        session: session.clone(),
                    },
                    None => flag(format!(
                        "the turn {} asked for facts, but it has no session to resume",
                        turn.id
                    )),
                }
            }
            Some("result") => match turn.end_fact.as_deref() {
                None => Next::Nothing(format!("the end facts of {} are tested", turn.id)),
                Some("held") => after_next(facts, List::After, after),
                Some(failed) => {
                    let fact = failed.strip_prefix("failed:").unwrap_or(failed);
                    let found = failure.unwrap_or("");
                    if failures >= 2 {
                        return flag(format!(
                            "the end fact {fact} did not hold after a resume: {found}"
                        ));
                    }
                    if let Some(refused) = refusal_flag(&facts.refusal) {
                        return refused;
                    }
                    match &turn.session {
                        Some(session) => Next::Resume {
                            session: session.clone(),
                            message: resume_message(fact, found),
                        },
                        None => flag(format!(
                            "the end fact {fact} did not hold, and the turn {} has no session to resume: {found}",
                            turn.id
                        )),
                    }
                }
            },
            other => flag(format!(
                "the turn {} ended with the kind {}, which the pipeline does not know",
                turn.id,
                other.unwrap_or("none")
            )),
        },
    }
}

/// The message of the resume after a failed end fact.
pub fn resume_message(fact: &str, found: &str) -> String {
    format!(
        "The end fact {fact} does not hold: {found}. Make it hold, then end the turn with the kind result again, with each document of the step."
    )
}

fn refusal_flag(refusal: &Option<Refused>) -> Option<Next> {
    match refusal {
        None => None,
        Some(Refused::Limit(text)) => Some(Next::Flag {
            flag: WAITS_FOR_LIMIT,
            reason: text.clone(),
        }),
        Some(Refused::Other(text)) => Some(flag(text.clone())),
    }
}

fn move_next(facts: &Facts) -> Next {
    if let Some(branch) = &facts.branch {
        return Next::Move {
            to: branch.to.clone(),
            how: branch.how.clone(),
        };
    }
    match &facts.next {
        Some(to) => Next::Move {
            to: to.clone(),
            how: "next".into(),
        },
        None => flag(format!(
            "the step {} has no next step. Fix adapter.toml",
            facts.step
        )),
    }
}

fn after_next(facts: &Facts, list: List, entries: &[(String, OpFact)]) -> Next {
    ops_next(list, entries).unwrap_or_else(|| move_next(facts))
}

/// The first entry that is not confirmed decides; `None` when each one is confirmed.
fn ops_next(list: List, entries: &[(String, OpFact)]) -> Option<Next> {
    for (index, (label, fact)) in entries.iter().enumerate() {
        let detail = |d: &str| {
            if d.is_empty() {
                String::new()
            } else {
                format!(": {d}")
            }
        };
        let next = match fact {
            OpFact::NotAsked => Next::Request { list, index },
            OpFact::BadArgs(text) => flag(text.clone()),
            OpFact::Known { outcome, detail: d } => match outcome {
                Outcome::Confirmed => continue,
                Outcome::Pending => Next::Request { list, index },
                Outcome::Executing => Next::Nothing(format!("{label} runs")),
                Outcome::Uncertain => Next::Nothing(format!(
                    "{label} is uncertain, and the reconcile asks again at each tick{}",
                    detail(d)
                )),
                Outcome::Waiting { command } => {
                    Next::Nothing(format!("{label} waits for the approval: {command}"))
                }
                Outcome::Held => flag(format!("{label} is held{}", detail(d))),
                Outcome::Shadowed => flag(format!("{label} is in the mode shadow{}", detail(d))),
                Outcome::Refused { gate } => flag(format!(
                    "{label} was refused by the {gate} gate{}",
                    detail(d)
                )),
                Outcome::Failed => flag(format!("{label} failed{}", detail(d))),
            },
        };
        return Some(next);
    }
    None
}

fn cap_text(step: &str, name: &str, value: u32, n: u32, open: &[String]) -> String {
    let mut text = format!("the step {step} reached its cap {name} = {value}: this is entry {n}");
    if open.is_empty() {
        text.push_str(". Nothing open is listed");
    } else {
        text.push_str(&format!(". Open: {}", open.join("; ")));
    }
    text
}

// ----- End facts -------------------------------------------------------------------------

/// The result of a job at the commit of now, for an end fact `job:<name>`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum JobFact {
    NotRun,
    Passed,
    Failed(String),
}

/// What [`check_end_facts`] reads.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct FactInputs {
    /// The documents of the result: name -> text.
    pub docs: BTreeMap<String, String>,
    /// The documents of the result whose text only pointed to a file. They are not in `docs`.
    pub pointers: BTreeSet<String>,
    /// The last commit of the clone, and the one at the start of the visit.
    pub head: Option<String>,
    pub base: Option<String>,
    /// The lines of `git status --porcelain` of the clone.
    pub dirty: Vec<String>,
    pub jobs: BTreeMap<String, JobFact>,
    /// The human actions that the person did.
    pub human: BTreeSet<String>,
}

/// The verdict on the end facts of a step.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Check {
    Held,
    /// The first fact that does not hold, and what was found, for the agent.
    Failed {
        fact: String,
        found: String,
    },
    /// A job of an end fact has no run at the commit of now: run it, then test again.
    RunJob(String),
}

/// Tests the end facts in their order, the jobs last, because a job costs the most.
pub fn check_end_facts(expects: &[Expect], inputs: &FactInputs) -> Check {
    let short = |c: &Option<String>| {
        c.as_deref()
            .map(|c| c.get(..12).unwrap_or(c).to_string())
            .unwrap_or_else(|| "-".into())
    };
    for expect in expects {
        let found = match expect {
            Expect::Doc(name) => match inputs.docs.get(name) {
                None if inputs.pointers.contains(name) => Some(format!(
                    "the document {name} of the result only points to a file. A turn cannot write a file for a document. Put the full text of {name} in `content` of `documents`"
                )),
                None => Some(format!("the result has no document {name}")),
                Some(text) if text.trim().is_empty() => {
                    Some(format!("the document {name} of the result is empty"))
                }
                Some(_) => None,
            },
            Expect::Commits => match (&inputs.head, &inputs.base) {
                (None, _) => Some("the clone has no commit".into()),
                (Some(head), Some(base)) if head == base => Some(format!(
                    "the branch is still at {}: it has no commit since the start of the step",
                    short(&inputs.head)
                )),
                _ => None,
            },
            Expect::Clean => (!inputs.dirty.is_empty()).then(|| {
                let mut lines: Vec<&str> =
                    inputs.dirty.iter().map(String::as_str).take(10).collect();
                if inputs.dirty.len() > 10 {
                    lines.push("...");
                }
                format!(
                    "the work tree of the clone has changes that no commit holds: {}",
                    lines.join(", ")
                )
            }),
            Expect::Human(action) => (!inputs.human.contains(action))
                .then(|| format!("the person did not run vision {action}")),
            Expect::Job(_) => None,
        };
        if let Some(found) = found {
            return Check::Failed {
                fact: expect.to_string(),
                found,
            };
        }
    }
    for expect in expects {
        let Expect::Job(name) = expect else { continue };
        match inputs.jobs.get(name).unwrap_or(&JobFact::NotRun) {
            JobFact::NotRun => return Check::RunJob(name.clone()),
            JobFact::Passed => {}
            JobFact::Failed(detail) => {
                return Check::Failed {
                    fact: expect.to_string(),
                    found: format!(
                        "the job {name} did not pass at {}: {detail}",
                        short(&inputs.head)
                    ),
                }
            }
        }
    }
    Check::Held
}

// ----- Wait steps ------------------------------------------------------------------------

/// The step of `on` for the watched value.
pub fn wait_target(on: &BTreeMap<String, String>, watched: Option<&str>) -> Option<String> {
    watched.and_then(|value| on.get(value).cloned())
}

/// The result of the CI check that the `ci` facts of an observe answer give, or `None`.
///
/// `green` and `red` are the two end states of a CI run in the contract of `observe`
/// (section 11.1). Each other state, such as `pending` or `none`, is a run that did not end,
/// and it records nothing.
pub fn ci_result(state: &str) -> Option<&'static str> {
    match state {
        "green" => Some("green"),
        "red" => Some("red"),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn turn(status: TurnStatus, kind: Option<&str>, end_fact: Option<&str>) -> LastTurn {
        LastTurn {
            id: "T1".into(),
            status,
            reason: "error: the session crashed".into(),
            kind: kind.map(str::to_string),
            session: Some("s-1".into()),
            end_fact: end_fact.map(str::to_string),
        }
    }

    fn turn_facts(last: Option<LastTurn>, after: Vec<(String, OpFact)>) -> Facts {
        Facts {
            step: "build".into(),
            kind: KindFacts::Turn {
                last,
                failures: 0,
                failure: None,
                after,
                resume: None,
                message: None,
            },
            next: Some("review".into()),
            branch: None,
            flags: Vec::new(),
            running: false,
            open_cards: 0,
            refusal: None,
            cap: None,
            n: 1,
            skip_cap: false,
            open: Vec::new(),
        }
    }

    fn known(outcome: Outcome) -> OpFact {
        OpFact::Known {
            outcome,
            detail: "the detail".into(),
        }
    }

    fn is_flag(next: &Next, name: &str, part: &str) -> bool {
        matches!(next, Next::Flag { flag, reason } if *flag == name && reason.contains(part))
    }

    #[test]
    fn the_entry_of_a_turn_step_starts_its_first_turn() {
        let facts = turn_facts(None, Vec::new());
        assert_eq!(
            decide(&facts),
            Next::Start {
                resume: None,
                message: None
            }
        );
        let mut paused = facts.clone();
        if let KindFacts::Turn {
            resume, message, ..
        } = &mut paused.kind
        {
            *resume = Some("s-9".into());
            *message = Some("the person worked here".into());
        }
        assert_eq!(
            decide(&paused),
            Next::Start {
                resume: Some("s-9".into()),
                message: Some("the person worked here".into())
            }
        );
    }

    #[test]
    fn a_refusal_for_a_limit_waits_and_each_other_refusal_needs_you() {
        let mut facts = turn_facts(None, Vec::new());
        facts.refusal = Some(Refused::Limit("refused by limits.five_hour".into()));
        assert!(is_flag(
            &decide(&facts),
            WAITS_FOR_LIMIT,
            "limits.five_hour"
        ));
        facts.refusal = Some(Refused::Other("refused by budget.ticket".into()));
        assert!(is_flag(&decide(&facts), NEEDS_YOU, "budget.ticket"));
    }

    #[test]
    fn the_limit_flag_clears_when_the_limit_allows_a_turn() {
        let mut facts = turn_facts(None, Vec::new());
        facts.flags = vec![WAITS_FOR_LIMIT.into()];
        facts.refusal = Some(Refused::Limit("still at 70%".into()));
        assert!(matches!(decide(&facts), Next::Nothing(t) if t.contains("still at 70%")));
        facts.refusal = None;
        assert_eq!(decide(&facts), Next::ClearLimit);
        facts.refusal = Some(Refused::Other("refused by budget.week".into()));
        assert_eq!(
            decide(&facts),
            Next::ClearLimit,
            "the next decision flags it"
        );
    }

    #[test]
    fn each_other_flag_and_a_running_turn_start_nothing() {
        for name in [NEEDS_YOU, PAUSED, STOPPED, "unknown"] {
            let mut facts = turn_facts(None, Vec::new());
            facts.flags = vec![name.into()];
            assert!(matches!(decide(&facts), Next::Nothing(_)), "{name}");
        }
        let mut facts = turn_facts(None, Vec::new());
        facts.running = true;
        assert!(matches!(decide(&facts), Next::Nothing(t) if t.contains("runs")));
    }

    #[test]
    fn questions_wait_for_the_cards_and_their_resume() {
        let mut facts = turn_facts(
            Some(turn(TurnStatus::Done, Some("questions"), None)),
            vec![],
        );
        facts.open_cards = 2;
        assert!(matches!(decide(&facts), Next::Nothing(t) if t.contains("2 card")));
        facts.open_cards = 0;
        assert!(matches!(decide(&facts), Next::Nothing(t) if t.contains("resumes")));
        let running = turn_facts(Some(turn(TurnStatus::Running, None, None)), vec![]);
        assert!(matches!(decide(&running), Next::Nothing(_)));
    }

    #[test]
    fn requests_are_served_and_the_session_resumes() {
        let facts = turn_facts(Some(turn(TurnStatus::Done, Some("requests"), None)), vec![]);
        assert_eq!(
            decide(&facts),
            Next::Serve {
                turn: "T1".into(),
                session: "s-1".into()
            }
        );
        let mut limited = facts.clone();
        limited.refusal = Some(Refused::Limit("x".into()));
        assert!(is_flag(&decide(&limited), WAITS_FOR_LIMIT, "x"));
    }

    #[test]
    fn a_result_waits_for_its_end_facts() {
        let facts = turn_facts(Some(turn(TurnStatus::Done, Some("result"), None)), vec![]);
        assert!(matches!(decide(&facts), Next::Nothing(t) if t.contains("end facts")));
    }

    #[test]
    fn a_failed_end_fact_resumes_once_then_needs_you() {
        let mut facts = turn_facts(
            Some(turn(
                TurnStatus::Done,
                Some("result"),
                Some("failed:doc:plan.md"),
            )),
            vec![],
        );
        if let KindFacts::Turn {
            failures, failure, ..
        } = &mut facts.kind
        {
            *failures = 1;
            *failure = Some("the result has no document plan.md".into());
        }
        match decide(&facts) {
            Next::Resume { session, message } => {
                assert_eq!(session, "s-1");
                assert!(message.contains("The end fact doc:plan.md does not hold: the result has no document plan.md"), "{message}");
            }
            other => panic!("{other:?}"),
        }
        if let KindFacts::Turn { failures, .. } = &mut facts.kind {
            *failures = 2;
        }
        assert!(is_flag(
            &decide(&facts),
            NEEDS_YOU,
            "doc:plan.md did not hold after a resume"
        ));
    }

    #[test]
    fn a_held_result_requests_each_after_verb_in_order_then_moves() {
        let after = vec![
            ("git.push".to_string(), known(Outcome::Confirmed)),
            ("pr.open".to_string(), OpFact::NotAsked),
        ];
        let facts = turn_facts(
            Some(turn(TurnStatus::Done, Some("result"), Some("held"))),
            after,
        );
        assert_eq!(
            decide(&facts),
            Next::Request {
                list: List::After,
                index: 1
            }
        );
        let done = turn_facts(
            Some(turn(TurnStatus::Done, Some("result"), Some("held"))),
            vec![("git.push".to_string(), known(Outcome::Confirmed))],
        );
        assert_eq!(
            decide(&done),
            Next::Move {
                to: "review".into(),
                how: "next".into()
            }
        );
    }

    #[test]
    fn an_operation_that_cannot_go_on_stops_the_ticket() {
        let held = |outcome: Outcome| {
            decide(&turn_facts(
                Some(turn(TurnStatus::Done, Some("result"), Some("held"))),
                vec![("git.push".to_string(), known(outcome))],
            ))
        };
        assert!(
            matches!(held(Outcome::Waiting { command: "vision approve DEMO-12 git.push abc".into() }), Next::Nothing(t) if t.contains("vision approve"))
        );
        assert!(matches!(held(Outcome::Uncertain), Next::Nothing(t) if t.contains("uncertain")));
        assert!(matches!(held(Outcome::Executing), Next::Nothing(_)));
        assert_eq!(
            held(Outcome::Pending),
            Next::Request {
                list: List::After,
                index: 0
            }
        );
        assert!(is_flag(&held(Outcome::Held), NEEDS_YOU, "held"));
        assert!(is_flag(&held(Outcome::Shadowed), NEEDS_YOU, "shadow"));
        assert!(is_flag(
            &held(Outcome::Refused {
                gate: "secrets".into()
            }),
            NEEDS_YOU,
            "secrets gate: the detail"
        ));
        assert!(is_flag(
            &held(Outcome::Failed),
            NEEDS_YOU,
            "failed: the detail"
        ));
        let bad = decide(&turn_facts(
            Some(turn(TurnStatus::Done, Some("result"), Some("held"))),
            vec![(
                "pr.open".to_string(),
                OpFact::BadArgs("no document pr.md".into()),
            )],
        ));
        assert!(is_flag(&bad, NEEDS_YOU, "no document pr.md"));
    }

    #[test]
    fn a_failed_turn_needs_you_with_its_reason() {
        let facts = turn_facts(Some(turn(TurnStatus::Failed, None, None)), vec![]);
        assert!(is_flag(&decide(&facts), NEEDS_YOU, "the session crashed"));
        let retried = turn_facts(Some(turn(TurnStatus::Retried, None, None)), vec![]);
        assert!(matches!(decide(&retried), Next::Nothing(_)));
    }

    #[test]
    fn one_entry_above_the_cap_needs_you_with_what_is_open() {
        let mut facts = turn_facts(None, Vec::new());
        facts.cap = Some(("review_rounds".into(), 3));
        facts.n = 3;
        assert!(matches!(decide(&facts), Next::Start { .. }));
        facts.n = 4;
        facts.open = vec!["F2: the export loses the header".into()];
        assert!(is_flag(
            &decide(&facts),
            NEEDS_YOU,
            "reached its cap review_rounds = 3: this is entry 4. Open: F2: the export loses the header"
        ));
        facts.skip_cap = true;
        assert!(matches!(decide(&facts), Next::Start { .. }));
    }

    #[test]
    fn a_wait_step_runs_its_start_verbs_then_moves_on_the_watched_value() {
        let wait = |start: Vec<(String, OpFact)>, watched: Option<&str>, target: Option<&str>| {
            let mut facts = turn_facts(None, Vec::new());
            facts.step = "ci".into();
            facts.next = None;
            facts.kind = KindFacts::Wait {
                start,
                watch: "ci.state".into(),
                watched: watched.map(str::to_string),
                target: target.map(str::to_string),
                waits: None,
                limit: None,
                waited: Some(10_000),
            };
            decide(&facts)
        };
        assert_eq!(
            wait(vec![("ci.run".into(), OpFact::NotAsked)], None, None),
            Next::Request {
                list: List::Start,
                index: 0
            }
        );
        let started = vec![("ci.run".to_string(), known(Outcome::Confirmed))];
        assert!(
            matches!(wait(started.clone(), Some("pending"), None), Next::Nothing(t) if t.contains("pending"))
        );
        assert_eq!(
            wait(started, Some("red"), Some("fix-ci")),
            Next::Move {
                to: "fix-ci".into(),
                how: "on red".into()
            }
        );
    }

    #[test]
    fn a_wait_step_flags_a_value_that_nothing_names_and_a_visit_above_its_limit() {
        let wait = |watched: Option<&str>,
                    waits: Option<&[&str]>,
                    limit: Option<u64>,
                    waited: Option<i64>| {
            let mut facts = turn_facts(None, Vec::new());
            facts.step = "ci".into();
            facts.next = None;
            facts.kind = KindFacts::Wait {
                start: vec![("ci.run".to_string(), known(Outcome::Confirmed))],
                watch: "ci.state".into(),
                watched: watched.map(str::to_string),
                target: None,
                waits: waits.map(|w| w.iter().map(|v| v.to_string()).collect()),
                limit,
                waited,
            };
            decide(&facts)
        };
        let pending: &[&str] = &["pending", "none"];
        assert!(matches!(
            wait(Some("pending"), Some(pending), None, Some(5)),
            Next::Nothing(_)
        ));
        assert!(is_flag(
            &wait(Some("skipped"), Some(pending), None, Some(5)),
            NEEDS_YOU,
            "its value `skipped` is in neither on nor wait"
        ));
        assert!(
            matches!(wait(Some("skipped"), None, None, Some(5)), Next::Nothing(_)),
            "with no wait key, each value waits, as before"
        );
        assert!(
            matches!(wait(None, Some(pending), None, None), Next::Nothing(_)),
            "not observed yet"
        );
        assert!(matches!(
            wait(Some("pending"), Some(pending), Some(60), Some(60)),
            Next::Nothing(_)
        ));
        assert!(is_flag(
            &wait(Some("pending"), Some(pending), Some(60), Some(61)),
            NEEDS_YOU,
            "watched ci.state for 61 s, above its limit of 60 s. The last value: pending"
        ));
        assert!(is_flag(
            &wait(None, None, Some(60), Some(61)),
            NEEDS_YOU,
            "The last value: not observed yet"
        ));
        assert!(
            matches!(wait(None, None, Some(60), None), Next::Nothing(_)),
            "the clock has not started"
        );
    }

    #[test]
    fn a_turn_step_with_on_moves_by_the_kept_findings_of_its_turn() {
        let on: BTreeMap<String, String> = [
            ("findings".to_string(), "fix-qa".to_string()),
            ("none".to_string(), "ready".to_string()),
        ]
        .into();
        let found = findings_branch(&on, 2).unwrap();
        assert_eq!(
            (found.to.as_str(), found.how.as_str()),
            ("fix-qa", "on findings")
        );
        let none = findings_branch(&on, 0).unwrap();
        assert_eq!((none.to.as_str(), none.how.as_str()), ("ready", "on none"));
        assert_eq!(findings_branch(&BTreeMap::new(), 1), None);
        let mut facts = turn_facts(
            Some(turn(TurnStatus::Done, Some("result"), Some("held"))),
            vec![("git.push".to_string(), known(Outcome::Confirmed))],
        );
        facts.next = None;
        facts.branch = Some(found);
        assert_eq!(
            decide(&facts),
            Next::Move {
                to: "fix-qa".into(),
                how: "on findings".into()
            }
        );
        // The after verbs run first.
        if let KindFacts::Turn { after, .. } = &mut facts.kind {
            after[0].1 = OpFact::NotAsked;
        }
        assert_eq!(
            decide(&facts),
            Next::Request {
                list: List::After,
                index: 0
            }
        );
    }

    #[test]
    fn a_human_step_waits_for_its_command() {
        let mut facts = turn_facts(None, Vec::new());
        facts.step = "plan-ready".into();
        facts.next = Some("build".into());
        facts.kind = KindFacts::Human {
            action: "approve-plan".into(),
            done: false,
        };
        assert!(matches!(decide(&facts), Next::Nothing(t) if t.contains("vision approve-plan")));
        facts.kind = KindFacts::Human {
            action: "approve-plan".into(),
            done: true,
        };
        assert_eq!(
            decide(&facts),
            Next::Move {
                to: "build".into(),
                how: "next".into()
            }
        );
    }

    #[test]
    fn the_end_step_finishes_once() {
        let mut facts = turn_facts(None, Vec::new());
        facts.kind = KindFacts::End { finished: false };
        assert_eq!(decide(&facts), Next::Finish);
        facts.kind = KindFacts::End { finished: true };
        assert!(matches!(decide(&facts), Next::Nothing(_)));
    }

    #[test]
    fn a_job_step_runs_its_job_and_moves_when_it_passes() {
        let mut facts = turn_facts(None, Vec::new());
        facts.kind = KindFacts::Job {
            passed: None,
            detail: String::new(),
            after: vec![],
        };
        assert_eq!(decide(&facts), Next::RunJob);
        facts.kind = KindFacts::Job {
            passed: Some(false),
            detail: "two tests fail".into(),
            after: vec![],
        };
        assert!(is_flag(&decide(&facts), NEEDS_YOU, "two tests fail"));
        facts.kind = KindFacts::Job {
            passed: Some(true),
            detail: String::new(),
            after: vec![],
        };
        assert!(matches!(decide(&facts), Next::Move { .. }));
    }

    fn inputs() -> FactInputs {
        FactInputs {
            docs: [("pr.md".to_string(), "Add subtract\n\nBody".to_string())].into(),
            head: Some("b".repeat(40)),
            base: Some("a".repeat(40)),
            ..FactInputs::default()
        }
    }

    #[test]
    fn each_end_fact_is_tested_and_the_jobs_come_last() {
        let expects: Vec<Expect> = ["commits", "clean", "doc:pr.md", "job:proof"]
            .iter()
            .map(|t| Expect::parse(t).unwrap())
            .collect();
        let mut i = inputs();
        assert_eq!(check_end_facts(&expects, &i), Check::RunJob("proof".into()));
        i.jobs.insert("proof".into(), JobFact::Passed);
        assert_eq!(check_end_facts(&expects, &i), Check::Held);
        i.jobs
            .insert("proof".into(), JobFact::Failed("the tests fail".into()));
        assert!(
            matches!(check_end_facts(&expects, &i), Check::Failed { fact, found } if fact == "job:proof" && found.contains("the tests fail"))
        );
        i.dirty = vec![" M calc.py".into()];
        assert!(
            matches!(check_end_facts(&expects, &i), Check::Failed { fact, found } if fact == "clean" && found.contains("M calc.py"))
        );
        i.base = i.head.clone();
        assert!(
            matches!(check_end_facts(&expects, &i), Check::Failed { fact, .. } if fact == "commits")
        );
        let mut docs = inputs();
        docs.docs.insert("pr.md".into(), "  \n".into());
        assert!(
            matches!(check_end_facts(&[Expect::Doc("pr.md".into())], &docs), Check::Failed { found, .. } if found.contains("empty"))
        );
        docs.docs.clear();
        assert!(
            matches!(check_end_facts(&[Expect::Doc("pr.md".into())], &docs), Check::Failed { found, .. } if found.contains("no document pr.md"))
        );
        // A document that only pointed to a file gets its own text, so that the agent knows
        // what to change.
        docs.pointers.insert("pr.md".into());
        assert!(
            matches!(check_end_facts(&[Expect::Doc("pr.md".into())], &docs), Check::Failed { found, .. } if found.contains("only points to a file") && found.contains("full text of pr.md"))
        );
        let human = Expect::Human("approve-plan".into());
        assert!(matches!(
            check_end_facts(std::slice::from_ref(&human), &docs),
            Check::Failed { .. }
        ));
        docs.human.insert("approve-plan".into());
        assert_eq!(check_end_facts(&[human], &docs), Check::Held);
    }

    const ADAPTER: &str = r#"
[[step]]
id = "ci"
kind = "wait"
watch = "ci.state"
on = { green = "ready", red = "fix" }

[[step]]
id = "fix"
kind = "human"
action = "approve-plan"
next = "ci"

[[step]]
id = "ready"
kind = "wait"
watch = "pr.draft"
on = { "false" = "done" }

[[step]]
id = "done"
kind = "end"
"#;

    #[test]
    fn the_ci_check_comes_from_the_two_end_states_of_the_ci_facts() {
        let adapter =
            crate::adapter::Adapter::parse(ADAPTER, std::path::Path::new("/a/toy")).unwrap();
        let ci = adapter.step("ci").unwrap();
        assert_eq!(ci_result("red"), Some("red"));
        assert_eq!(ci_result("green"), Some("green"));
        assert_eq!(ci_result("pending"), None, "it still runs");
        assert_eq!(ci_result("none"), None, "no run at this commit");
        assert_eq!(wait_target(&ci.on, Some("red")).as_deref(), Some("fix"));
        assert_eq!(wait_target(&ci.on, Some("pending")), None);
        assert_eq!(wait_target(&ci.on, None), None);
    }
}
