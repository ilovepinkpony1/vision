//! The state of a ticket in the words of the app: one pure function from the facts of a ticket
//! (its step, the turns that run, its flags, its needs, the record of what a pass runs, the
//! facts of a wait, and the limits) to the kind of the state, a tone, a short line for Home,
//! and the lines of the panel NOW of the live view.
//!
//! The texts name the keys of the app, never a command, and never a word of the code. The
//! status view of the command line keeps its own line.

use time::OffsetDateTime;

use super::text::count;
use crate::pipeline::facts::{EntryState, StateFacts, WaitFacts};
use crate::pipeline::state::{NEEDS_YOU, PAUSED, STOPPED, WAITS_FOR_LIMIT};
use crate::pipeline::TicketView;
use crate::running::Running;
use crate::snapshot::{Need, NeedKind};
use crate::util::time as vtime;

/// The kind of the state of a ticket.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// A work turn of the ticket runs.
    Agent,
    /// A turn of the Judge runs for cards of the ticket, and no work turn runs.
    Judge,
    /// A pass runs a job or an operation of the ticket.
    Runs,
    /// The ticket needs the person.
    Needs,
    /// The ticket waits for something outside, such as CI or a review.
    Waits,
    /// The next step starts at the next pass.
    Next,
    /// A limit of the subscription holds the ticket back.
    Limit,
    Stopped,
    /// The person took over the session.
    Paused,
    Done,
}

/// How the state looks.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tone {
    /// Work runs: the color of the agent, or the accent.
    Work,
    /// The person is needed: the warning color.
    Need,
    /// The ticket waits: the color of waiting.
    Wait,
    /// Nothing moves: dim.
    Rest,
    Done,
}

/// The state of the engine, as far as the ticket cares.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Pass {
    /// No pass runs or waits.
    Idle,
    /// A pass waits in the queue of the worker.
    Waits,
    /// A pass runs now.
    Runs,
    /// The person paused the engine.
    Paused,
    /// The kill switch is on.
    Off,
}

/// What the state function reads.
#[derive(Clone, Copy, Debug)]
pub struct Input<'a> {
    pub view: &'a TicketView,
    pub facts: &'a StateFacts,
    /// The needs of this ticket.
    pub needs: &'a [Need],
    /// The record of what a pass runs now, when it is for this ticket.
    pub running: Option<&'a Running>,
    pub pass: Pass,
    pub wall: OffsetDateTime,
    /// The offset of the local time from UTC, in seconds.
    pub offset_seconds: i32,
}

/// The state in words.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Now {
    pub kind: Kind,
    pub tone: Tone,
    /// One line for Home.
    pub short: String,
    /// The title of the panel NOW.
    pub title: String,
    /// The lines under the title.
    pub lines: Vec<String>,
    /// The agent of the turn that runs: its color and its face.
    pub agent: Option<String>,
    /// The key that acts on this state, and its label.
    pub key: Option<(&'static str, &'static str)>,
    /// The seconds since the work of this state started, while it runs.
    pub seconds: Option<i64>,
    /// True while a pass runs or waits, so that a spinner moves.
    pub spinner: bool,
    /// True when the next pass acts on the ticket, with nothing to wait for. The engine of
    /// the app then starts that pass at once.
    pub ready: bool,
}

impl Now {
    fn new(kind: Kind, tone: Tone, short: String, title: String, lines: Vec<String>) -> Now {
        Now {
            kind,
            tone,
            short,
            title,
            lines,
            agent: None,
            key: None,
            seconds: None,
            spinner: false,
            ready: false,
        }
    }
}

/// The state of one ticket.
pub fn now(input: &Input) -> Now {
    let view = input.view;
    let facts = input.facts;
    let has_flag = |name: &str| view.flags.iter().any(|f| f.flag == name);
    let step = view.step.as_deref().unwrap_or("-");
    if facts.finished {
        return done(facts);
    }
    if has_flag(STOPPED) {
        let mut now = Now::new(
            Kind::Stopped,
            Tone::Rest,
            "stopped · Enter starts it again".into(),
            "stopped".into(),
            vec![
                format!("You stopped it at the step {step}. Its branch stays in its clone."),
                "Press Enter to start it again.".into(),
            ],
        );
        now.key = Some(("enter", "resume"));
        return now;
    }
    if has_flag(PAUSED) {
        let mut now = Now::new(
            Kind::Paused,
            Tone::Need,
            "paused · you took over its session".into(),
            "you took over its session".into(),
            vec![
                "No new turn starts while you work in the session.".into(),
                "Press r to continue. The next turn resumes it.".into(),
            ],
        );
        now.key = Some(("r", "continue"));
        return now;
    }
    if let Some(turn) = &facts.work {
        let seconds = seconds_since(turn.started.as_deref(), input.wall);
        let minutes = view
            .works
            .as_ref()
            .map_or(seconds.unwrap_or(0) / 60, |w| w.minutes);
        let mut now = Now::new(
            Kind::Agent,
            Tone::Work,
            format!("{} works · {minutes} min", turn.agent),
            format!("{} works", turn.agent),
            vec![format!(
                "step {} · turn {} of this ticket",
                turn.step, turn.n
            )],
        );
        now.agent = Some(turn.agent.clone());
        now.seconds = seconds;
        now.key = Some(("t", "tell"));
        return now;
    }
    if let Some(record) = input.running.filter(|r| r.ticket == view.key) {
        let seconds = seconds_since(Some(&record.started), input.wall).unwrap_or(0);
        let label = record.label();
        let mut now = Now::new(
            Kind::Runs,
            Tone::Work,
            format!("{label} runs · {}", mmss(seconds)),
            format!("{label} runs"),
            vec![
                "A pass of the engine runs it now. No agent works on the ticket in this time."
                    .into(),
            ],
        );
        now.seconds = Some(seconds);
        now.spinner = true;
        return now;
    }
    if let Some(now) = needs(input, step) {
        return now;
    }
    if let Some(judge) = &facts.judge {
        let what = match facts.judging_cards {
            0 => "the questions".to_string(),
            n => count(n, "question"),
        };
        let mut now = Now::new(
            Kind::Judge,
            Tone::Work,
            format!("the Judge reads {what}"),
            format!("the Judge reads {what}"),
            vec![
                "It answers from the memory when it is sure.".into(),
                "It sends each other question to you.".into(),
            ],
        );
        now.agent = Some(judge.agent.clone());
        now.seconds = seconds_since(judge.started.as_deref(), input.wall);
        return now;
    }
    if has_flag(WAITS_FOR_LIMIT) {
        return limit(input);
    }
    if let Some(verb) = &facts.uncertain {
        return Now::new(
            Kind::Waits,
            Tone::Wait,
            format!("waits for the check of {verb}"),
            format!("waits for the check of {verb}"),
            vec![
                format!("Nobody knows yet whether {verb} reached the outside."),
                "Each pass asks again. Nothing else runs for the ticket until then.".into(),
            ],
        );
    }
    let kind = view.kind.as_deref().unwrap_or("");
    if kind == "wait" {
        if let Some(wait) = &facts.wait {
            return waits(input, wait);
        }
    }
    if kind == "human" {
        let what = match facts.action.as_deref() {
            Some("approve-plan") => "your approval of the plan".to_string(),
            Some(action) => format!("you: {action}"),
            None => "you".to_string(),
        };
        return Now::new(
            Kind::Waits,
            Tone::Wait,
            format!("waits for {what}"),
            format!("waits for {what}"),
            vec![format!(
                "The step {step} waits for it. Nothing for it is ready yet."
            )],
        );
    }
    next(input, step)
}

fn done(facts: &StateFacts) -> Now {
    let (short, line) = match facts.merged_pr {
        Some(pr) => (
            format!("done · pull request #{pr} merged"),
            format!("The pull request #{pr} is merged."),
        ),
        None => ("done".to_string(), "The end step finished it.".to_string()),
    };
    Now::new(
        Kind::Done,
        Tone::Done,
        short,
        "done".into(),
        vec![line, "Nothing runs for it any more.".into()],
    )
}

/// The ticket needs the person: a question, the plan, a write that waits, or a rule that
/// stopped it.
fn needs(input: &Input, step: &str) -> Option<Now> {
    let view = input.view;
    let mine: Vec<&Need> = input
        .needs
        .iter()
        .filter(|n| n.ticket.as_deref() == Some(view.key.as_str()))
        .collect();
    let cards = mine.iter().filter(|n| n.kind == NeedKind::Card).count();
    let plans = mine.iter().filter(|n| n.kind == NeedKind::Plan).count();
    let writes: Vec<&&Need> = mine
        .iter()
        .filter(|n| n.kind == NeedKind::Operation)
        .collect();
    let flag = view.flags.iter().find(|f| f.flag == NEEDS_YOU);
    // A flag that the mode shadow or hold set: the first write of a verb waits.
    let first = flag.and(input.facts.first_write.as_deref());
    let mut parts = Vec::new();
    if cards > 0 {
        parts.push(count(cards, "question"));
    }
    if plans > 0 {
        parts.push("the plan".to_string());
    }
    if !writes.is_empty() {
        parts.push(count(writes.len(), "write"));
    }
    match (first, flag) {
        (Some(verb), _) => parts.push(format!("the first write {verb}")),
        (None, Some(flag)) => parts.push(cut_words(&flag.reason, 60)),
        (None, None) => {}
    }
    if parts.is_empty() {
        return None;
    }
    let short = format!("needs you: {}", parts.join(", "));
    let (title, lines, key) = if parts.len() > 1 {
        (
            format!("{} things wait for you", parts.len()),
            vec![parts.join(", "), "Press n to see each one.".to_string()],
            ("n", "needs"),
        )
    } else if cards > 0 {
        let first = mine
            .iter()
            .find(|n| n.kind == NeedKind::Card)
            .map(|n| n.text.clone())
            .unwrap_or_default();
        (
            if cards == 1 {
                "1 question for you".to_string()
            } else {
                format!("{cards} questions for you")
            },
            vec![
                first,
                "The Judge could not answer it from the memory.".to_string(),
            ],
            ("n", "answer"),
        )
    } else if plans > 0 {
        (
            "the plan waits for you".to_string(),
            vec![
                "Read the plan, then approve it, so that the build can start.".to_string(),
                "Press n to read it and approve it.".to_string(),
            ],
            ("n", "approve"),
        )
    } else if !writes.is_empty() {
        let verb = writes[0].id.split_whitespace().next().unwrap_or("a verb");
        (
            format!("the write {verb} waits for you"),
            vec![
                "It goes to the tracker or the forge only after your approval.".to_string(),
                cut_words(&writes[0].text, 90),
            ],
            ("n", "approve"),
        )
    } else if let Some(verb) = first {
        (
            format!("the first write {verb} waits for you"),
            vec![
                format!("The mode of {verb} kept it back, so nothing was sent."),
                "Press n to read the write, and decide.".to_string(),
            ],
            ("n", "decide"),
        )
    } else {
        let reason = flag.map(|f| f.reason.clone()).unwrap_or_default();
        (
            "a rule stopped the ticket".to_string(),
            vec![
                cut_words(&reason, 160),
                format!("Fix the cause, then press r to start the step {step} again."),
            ],
            ("r", "retry"),
        )
    };
    let mut now = Now::new(Kind::Needs, Tone::Need, short, title, lines);
    now.key = Some(key);
    Some(now)
}

/// The words of one need for the bar "needs you" of Home.
pub fn need_line(need: &Need) -> String {
    match need.kind {
        NeedKind::Card => need.text.clone(),
        NeedKind::Plan => "the plan waits for your approval".into(),
        NeedKind::Operation => {
            let verb = need.id.split_whitespace().next().unwrap_or("a verb");
            format!("the write {verb} waits for your approval")
        }
        NeedKind::Flag if need.id == PAUSED => "paused: you took over its session".into(),
        NeedKind::Flag => {
            let reason = need.text.split_once(": ").map_or("", |(_, r)| r);
            format!("a rule stopped it: {reason}")
        }
        NeedKind::Proposal => "a change of the memory waits for your review".into(),
        NeedKind::Notice => format!("a notice to read: {}", need.text),
    }
}

fn limit(input: &Input) -> Now {
    let Some(limit) = &input.facts.limit else {
        return Now::new(
            Kind::Limit,
            Tone::Wait,
            "waits for the limit of the subscription".into(),
            "waits for the limit of the subscription".into(),
            vec!["A window of the subscription is full. Vision tries again at each pass.".into()],
        );
    };
    let window = window_name(&limit.window);
    let part = percent(limit.part);
    let stop = percent(limit.stop);
    let reset = match limit
        .resets
        .as_deref()
        .and_then(|at| local_time(at, input.wall, input.offset_seconds))
    {
        Some(when) => format!("It resets {when}."),
        None => "Vision tries again at each pass.".to_string(),
    };
    Now::new(
        Kind::Limit,
        Tone::Wait,
        format!("waits for the {window} limit · {part} of {stop}"),
        format!("waits for the {window} limit"),
        vec![
            format!("The {window} window is at {part}, and new turns stop at {stop}."),
            reset,
        ],
    )
}

fn waits(input: &Input, wait: &WaitFacts) -> Now {
    let view = input.view;
    if let Some(opens) = wait
        .opens
        .as_deref()
        .and_then(|at| vtime::parse_rfc3339(at).ok())
        .filter(|at| *at > input.wall)
    {
        let local = opens + time::Duration::seconds(i64::from(input.offset_seconds));
        let hm = format!("{:02}:{:02}", local.hour(), local.minute());
        let when = local_time(&vtime::rfc3339(opens), input.wall, input.offset_seconds)
            .unwrap_or_else(|| format!("at {hm}"));
        let step = view.step.as_deref().unwrap_or("-");
        return Now::new(
            Kind::Waits,
            Tone::Wait,
            format!("waits until {hm}"),
            format!("waits until {hm}"),
            vec![
                format!("The step {step} starts {when}."),
                "Nothing runs for it before then: no write and no look.".into(),
            ],
        );
    }
    let pr = view.pr.or(wait.pr.as_ref().map(|p| p.number));
    let pr_text = pr.map_or("the pull request".to_string(), |n| {
        format!("the pull request #{n}")
    });
    let path = wait.watch.as_deref().unwrap_or("");
    let mut parts = path.split('.');
    let head = parts.next().unwrap_or("");
    let field = parts.next().unwrap_or("");
    let value = wait.value.as_deref();
    let look = match (&wait.observed, value) {
        (None, _) | (_, None) => "Vision looks at the next pass.".to_string(),
        (Some(at), _) => match seconds_since(Some(at), input.wall) {
            Some(s) if s >= 120 => format!("Vision looked {} ago.", age(s)),
            _ => "Vision looks again each minute.".to_string(),
        },
    };
    let (short, title, mut lines) = match head {
        "ci" => {
            let checks = &wait.checks;
            let commit = wait
                .ci
                .as_ref()
                .and_then(|c| c.head.clone())
                .map_or("The last commit".to_string(), |h| format!("At {h}"));
            let state = if checks.total() > 0 {
                format!(
                    "{} of {} checks passed, {} run, {} failed",
                    checks.passed,
                    checks.total(),
                    checks.running,
                    checks.failed
                )
            } else {
                match value {
                    Some("none") | None => "no result yet".to_string(),
                    Some(v) => format!("its state is {v}"),
                }
            };
            (
                format!("waits for CI · {state}"),
                "waits for CI".to_string(),
                vec![format!("{commit}: {state}.")],
            )
        }
        "pr" if field == "draft" => (
            format!("waits until {pr_text} is ready for review"),
            format!("waits for {pr_text}"),
            vec![format!(
                "{pr_text} is a draft until somebody marks it ready for review."
            )],
        ),
        "pr" if field == "merged" => (
            format!("waits for the merge of {pr_text}"),
            format!("waits for the merge of {pr_text}"),
            vec![approvals(wait)],
        ),
        "pr" | "review" => {
            let mut lines = vec![approvals(wait)];
            if let Some(facts) = &wait.pr {
                if facts.changes_requested {
                    lines.push("A reviewer asked for changes.".into());
                }
                if facts.comments_waiting > 0 {
                    lines.push(format!(
                        "{} wait for an answer.",
                        count(facts.comments_waiting as usize, "comment")
                    ));
                }
            }
            (
                format!("waits for the review of {pr_text}"),
                format!("waits for the review of {pr_text}"),
                lines,
            )
        }
        "envs" => (
            format!("waits for the env {field}"),
            format!("waits for the env {field}"),
            vec![match value {
                Some(v) => format!("The value of {path} is {v} now."),
                None => format!("Vision has not read {path} yet."),
            }],
        ),
        _ => (
            format!("waits until {path} changes"),
            format!("waits until {path} changes"),
            vec![match value {
                Some(v) => format!("It is {v} now."),
                None => "Vision has not read it yet.".to_string(),
            }],
        ),
    };
    lines.push(look);
    Now::new(Kind::Waits, Tone::Wait, short, title, lines)
}

fn approvals(wait: &WaitFacts) -> String {
    match wait.pr.as_ref().map(|p| p.approved_by.as_slice()) {
        Some([]) | None => "Nobody approved it yet.".to_string(),
        Some(names) => format!("Approved by {}.", names.join(", ")),
    }
}

/// The next step starts at the next pass.
fn next(input: &Input, step: &str) -> Now {
    let facts = input.facts;
    let agent = facts.agent.as_deref().unwrap_or("the agent");
    let kind = input.view.kind.as_deref().unwrap_or("");
    // An entry that waits for the approval of the person or for a check is not ready: a pass
    // each minute is enough for it. An entry that a mode or a gate kept back, or that cannot
    // run, is ready: the next pass sets the flag that asks the person.
    let mut ready = true;
    let what = if let Some(entry) = &facts.entry {
        let label = entry_words(&entry.label);
        ready = !matches!(entry.state, EntryState::Waits | EntryState::Uncertain);
        match &entry.state {
            EntryState::Next if entry.before => {
                format!("{label} runs at the next pass, before {agent} starts")
            }
            EntryState::Next => format!("{label} runs at the next pass"),
            EntryState::Runs => format!("{label} runs at the next pass"),
            EntryState::Waits => format!("{label} waits for your approval"),
            EntryState::Uncertain => format!("waits for the check of {label}"),
            EntryState::Cannot(text) => format!("{label} cannot run: {}", cut_words(text, 60)),
            EntryState::Other(text) => format!("{label} is {text}"),
        }
    } else if facts.held {
        // The turn held its end facts, and each write after it is done.
        "the next pass moves it to its next step".to_string()
    } else if !facts.agents.is_empty() && facts.open_cards > 0 {
        // The last turn asked questions: its work is not tested before they have answers.
        "the next pass goes on with the questions for the Judge".to_string()
    } else if facts.testing {
        format!("the next pass tests the work of {agent}")
    } else {
        match kind {
            "turn" => format!("{agent} starts at the next pass"),
            "job" => format!("the job of the step {step} runs at the next pass"),
            "end" => "the next pass finishes it".to_string(),
            _ => "the next pass moves it on".to_string(),
        }
    };
    let pass = match input.pass {
        Pass::Runs => "A pass runs now.",
        Pass::Waits => "A pass waits for the job of the engine that runs now.",
        Pass::Idle => "The next pass starts within a minute.",
        Pass::Paused => "The engine is paused. Press p on Home to start it again.",
        Pass::Off => "Vision is off, so nothing starts. Press K on Home to turn it on.",
    };
    let mut now = Now::new(
        Kind::Next,
        Tone::Wait,
        what.clone(),
        what,
        vec![
            pass.to_string(),
            "No agent works, so this costs nothing.".into(),
        ],
    );
    now.spinner = matches!(input.pass, Pass::Runs | Pass::Waits);
    now.ready = ready;
    now
}

/// An entry in words: `job:proof` is `the job proof`, a verb is `the write <verb>`.
fn entry_words(label: &str) -> String {
    match label.strip_prefix("job:") {
        Some(job) => format!("the job {job}"),
        None => format!("the write {label}"),
    }
}

/// `five_hour` is `5-hour`, `seven_day` is `7-day`.
pub fn window_name(window: &str) -> &str {
    match window {
        "five_hour" => "5-hour",
        "seven_day" => "7-day",
        other => other,
    }
}

fn percent(part: f64) -> String {
    format!("{:.0}%", part * 100.0)
}

/// The seconds from an RFC 3339 time to `wall`, or `None`.
pub fn seconds_since(at: Option<&str>, wall: OffsetDateTime) -> Option<i64> {
    let at = vtime::parse_rfc3339(at?).ok()?;
    Some(vtime::seconds_between(at, wall).max(0))
}

/// `m:ss`, such as `2:10`.
pub fn mmss(seconds: i64) -> String {
    let s = seconds.max(0);
    format!("{}:{:02}", s / 60, s % 60)
}

/// An age such as `5 min` or `2 h 10 min`.
fn age(seconds: i64) -> String {
    let m = seconds / 60;
    if m < 60 {
        format!("{m} min")
    } else {
        format!("{} h {} min", m / 60, m % 60)
    }
}

/// A time of the ledger in the local time: `at 18:00` today, else `on 2026-11-20 at 18:00`.
pub fn local_time(at: &str, wall: OffsetDateTime, offset_seconds: i32) -> Option<String> {
    let at = vtime::parse_rfc3339(at).ok()?;
    let shift = time::Duration::seconds(i64::from(offset_seconds));
    let local = at + shift;
    let today = (wall + shift).date();
    let hm = format!("{:02}:{:02}", local.hour(), local.minute());
    if local.date() == today {
        Some(format!("at {hm}"))
    } else {
        Some(format!("on {} at {hm}", local.date()))
    }
}

/// A text cut at a word to at most `max` characters, with `...` when it was cut.
fn cut_words(text: &str, max: usize) -> String {
    let text = text.trim();
    if text.chars().count() <= max {
        return text.to_string();
    }
    let mut out: String = text.chars().take(max).collect();
    if let Some(space) = out.rfind(' ') {
        out.truncate(space);
    }
    out.push_str("...");
    out
}
