//! The words of the timeline of a ticket: each line of the ledger (a turn that started or
//! ended, a card and its answer, an operation, a flag, an action of the person) as who did it,
//! a short word, and a plain text. The activity feed and the log pager both use them. The text
//! is data from the ledger: the view takes each control character out before it reaches a cell.

use serde_json::Value;

use super::model::Hue;
use crate::pipeline::commands::LogEntry;

/// One line of the timeline in words.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Moment {
    /// An agent, `you`, `judge`, `memory`, or `vision`.
    pub who: String,
    /// The word of the activity feed, such as `turn` or `you`.
    pub word: String,
    pub hue: Hue,
    /// The word of the column WHAT of the log, such as `started` or `approve`.
    pub what: String,
    pub text: String,
    /// The turn of the line, for Enter in the log.
    pub turn: Option<String>,
}

fn s<'a>(detail: &'a Value, key: &str) -> Option<&'a str> {
    detail.get(key).and_then(Value::as_str)
}

/// The color of who did it.
pub fn who_hue(who: &str) -> Hue {
    match who {
        "you" | "user" => Hue::Primary,
        "vision" | "tick" | "executor" => Hue::Dim,
        "memory" => Hue::Ok,
        agent => Hue::Agent(agent.to_string()),
    }
}

/// The words of one line of the timeline.
pub fn moment(entry: &LogEntry) -> Moment {
    let d = &entry.detail;
    let make = |who: &str, word: &str, hue: Hue, what: &str, text: String| Moment {
        who: who.to_string(),
        word: word.to_string(),
        hue,
        what: what.to_string(),
        text,
        turn: None,
    };
    match entry.source.as_str() {
        "turn" => {
            let agent = if entry.who.is_empty() {
                "agent"
            } else {
                entry.who.as_str()
            };
            let step = s(d, "step").unwrap_or("-");
            let mut m = if entry.kind == "started" {
                let model = match (s(d, "model"), s(d, "effort")) {
                    (Some(m), Some(e)) => format!(" · {m} {e}"),
                    (Some(m), None) => format!(" · {m}"),
                    _ => String::new(),
                };
                let resumes = if d.get("resumes").and_then(Value::as_bool) == Some(true) {
                    ", in its last session"
                } else {
                    ""
                };
                make(
                    agent,
                    if agent == "judge" { "judge" } else { "turn" },
                    if agent == "judge" {
                        Hue::Agent("judge".into())
                    } else {
                        Hue::Primary
                    },
                    "started",
                    format!("{agent} starts a turn at the step {step}{resumes}{model}"),
                )
            } else {
                make(
                    agent,
                    if agent == "judge" { "judge" } else { "turn" },
                    if agent == "judge" {
                        Hue::Agent("judge".into())
                    } else {
                        Hue::Primary
                    },
                    "ended",
                    format!("the turn of {agent} ended: {}", turn_end(d)),
                )
            };
            m.turn = entry.id.clone();
            m
        }
        "card" => card(entry, d),
        "operation" => {
            let verb = s(d, "verb").unwrap_or("?");
            let head = s(d, "head").unwrap_or("");
            let outcome = s(d, "outcome").unwrap_or(entry.kind.as_str());
            let detail = s(d, "detail").filter(|t| !t.is_empty());
            let at = if head.is_empty() {
                String::new()
            } else {
                format!(" at {head}")
            };
            make(
                "vision",
                "write",
                Hue::Accent,
                "write",
                match detail {
                    Some(text) => format!("{verb}{at} is {outcome}: {text}"),
                    None => format!("{verb}{at} is {outcome}"),
                },
            )
        }
        _ => event(entry, d),
    }
}

/// The end of a turn in words: its status, its reason, its cost, and its end facts.
fn turn_end(d: &Value) -> String {
    let status = s(d, "status").unwrap_or("?");
    let reason = s(d, "reason").unwrap_or("?");
    let mut text = match (status, reason) {
        ("done", "result") => "it gave its result".to_string(),
        ("retried", r) => format!("it stopped ({r}), so Vision resumes it"),
        ("failed", r) => format!("it failed ({r})"),
        (st, r) => format!("{st} ({r})"),
    };
    if let Some(cost) = d.get("cost").and_then(Value::as_f64) {
        text.push_str(&format!(" · {}", crate::pipeline::money(cost)));
    }
    match s(d, "end_fact") {
        Some("held") => text.push_str(" · its end facts held"),
        Some(fact) => {
            let fact = fact.strip_prefix("failed:").unwrap_or(fact);
            text.push_str(&format!(" · the end fact {fact} did not hold"));
        }
        None => {}
    }
    text
}

fn card(entry: &LogEntry, d: &Value) -> Moment {
    let asked_by = if entry.who.is_empty() {
        "agent".to_string()
    } else {
        entry.who.clone()
    };
    let question = s(d, "question").unwrap_or("").to_string();
    if entry.kind == "asked" {
        return Moment {
            who: asked_by,
            word: "asks".into(),
            hue: Hue::Warn,
            what: "asks".into(),
            text: question,
            turn: None,
        };
    }
    let answer = s(d, "answer").unwrap_or("").to_string();
    let (who, word, hue, text) = match (s(d, "outcome"), s(d, "basis"), entry.kind.as_str()) {
        (_, _, "needless") => (
            "you",
            "you",
            Hue::Primary,
            format!("marked a question as needless: {question}"),
        ),
        (Some("answer"), Some("decision" | "principle"), _) => (
            "memory",
            "memo",
            Hue::Ok,
            format!("answered from the memory: {answer}"),
        ),
        (Some("answer"), _, "answered") => (
            "judge",
            "judge",
            Hue::Agent("judge".into()),
            format!("answered alone: {answer}"),
        ),
        (Some("propose" | "ask"), _, "answered") => {
            ("you", "you", Hue::Primary, format!("answered: {answer}"))
        }
        (Some("propose"), _, _) => (
            "judge",
            "judge",
            Hue::Agent("judge".into()),
            format!("is not sure, so it proposes an answer to you: {question}"),
        ),
        (Some("ask"), _, _) => (
            "judge",
            "judge",
            Hue::Agent("judge".into()),
            format!("sends a question to you: {question}"),
        ),
        _ => ("judge", "judge", Hue::Agent("judge".into()), question),
    };
    Moment {
        who: who.into(),
        word: word.into(),
        hue,
        what: entry.kind.clone(),
        text,
        turn: None,
    }
}

fn event(entry: &LogEntry, d: &Value) -> Moment {
    let you = entry.who == "user";
    let who = if you { "you" } else { "vision" };
    let kind = entry.kind.as_str();
    let n = |key: &str| d.get(key).and_then(Value::as_u64).unwrap_or(0);
    let (word, hue, text) = match kind {
        "take" => ("you", Hue::Primary, "took the ticket".to_string()),
        "stop" => ("you", Hue::Primary, "stopped the ticket".to_string()),
        "retry" => (
            "you",
            Hue::Primary,
            format!(
                "started the step {} again",
                s(d, "step").unwrap_or("of the ticket")
            ),
        ),
        "pause" => (
            "you",
            Hue::Primary,
            "took over the session, so the ticket is paused".to_string(),
        ),
        "human" => match s(d, "action") {
            Some("approve-plan") => ("you", Hue::Primary, "approved the plan".to_string()),
            Some(action) => ("you", Hue::Primary, format!("did {action}")),
            None => ("you", Hue::Primary, "acted".to_string()),
        },
        "card-answered" => ("you", Hue::Primary, "answered a question".to_string()),
        "card-needless" => (
            "you",
            Hue::Primary,
            "marked a question as needless".to_string(),
        ),
        "correct" => ("you", Hue::Primary, "corrected an answer".to_string()),
        "step" => (
            "step",
            Hue::Info,
            format!(
                "moves from {} to {}",
                s(d, "from").unwrap_or("?"),
                s(d, "to").unwrap_or("?")
            ),
        ),
        "visit" => (
            "step",
            Hue::Info,
            format!("enters the step {}", s(d, "step").unwrap_or("?")),
        ),
        "flag" => {
            let reason = s(d, "reason").unwrap_or("");
            let text = match s(d, "flag") {
                Some("needs-you") => format!("stops and needs you: {reason}"),
                Some("waits-for-limit") => format!("waits for the limit: {reason}"),
                Some(other) => format!("sets the flag {other}: {reason}"),
                None => reason.to_string(),
            };
            ("flag", Hue::Warn, text)
        }
        "flag-cleared" => (
            "flag",
            Hue::Info,
            "the limit allows a turn again".to_string(),
        ),
        "end-fact" => (
            "check",
            Hue::Ok,
            "tested the end facts of the turn".to_string(),
        ),
        "documents" => (
            "docs",
            Hue::Dim,
            "kept the documents of the turn".to_string(),
        ),
        "judge-started" => (
            "judge",
            Hue::Agent("judge".into()),
            format!(
                "reads {}",
                crate::app::text::count(
                    d.get("cards").and_then(Value::as_array).map_or(0, Vec::len),
                    "question"
                )
            ),
        ),
        "judge-applied" => (
            "judge",
            Hue::Agent("judge".into()),
            "gave its answers".to_string(),
        ),
        "questions-intake" => (
            "asks",
            Hue::Warn,
            format!(
                "the turn asked {}",
                crate::app::text::count(n("cards") as usize, "question")
            ),
        ),
        "questions-resumed" => (
            "turn",
            Hue::Primary,
            "the answers go back to the agent".to_string(),
        ),
        "requests-served" => (
            "read",
            Hue::Info,
            format!(
                "served {} for the turn",
                crate::app::text::count(n("requests") as usize, "read")
            ),
        ),
        "read" => (
            "read",
            Hue::Info,
            format!("read {}", s(d, "verb").unwrap_or("a fact")),
        ),
        "ended" => {
            let text = match d.get("pr").and_then(Value::as_i64) {
                Some(pr) if d.get("merged").and_then(Value::as_bool) == Some(true) => {
                    format!("the ticket ended with the merged pull request #{pr}")
                }
                _ => "the ticket ended".to_string(),
            };
            ("done", Hue::Ok, text)
        }
        other => (
            if you { "you" } else { "note" },
            if you { Hue::Primary } else { Hue::Dim },
            other.replace('-', " "),
        ),
    };
    let what = match kind {
        "human" => "approve",
        "card-answered" => "answer",
        "card-needless" => "needless",
        "flag-cleared" => "flag",
        "end-fact" => "check",
        "judge-started" | "judge-applied" => "judge",
        "questions-intake" => "asks",
        "questions-resumed" => "resume",
        "requests-served" => "read",
        "ended" => "done",
        other => other,
    };
    Moment {
        who: who.into(),
        word: word.into(),
        hue,
        what: what.to_string(),
        text,
        turn: None,
    }
}
