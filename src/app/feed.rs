//! The activity of the live view: a feed of human words with the newest line at the bottom. It
//! joins two sources by time: the stream files of the turns of the ticket (what the agent says,
//! and the end of a session with an error), and the timeline of the ticket from the ledger (a
//! turn that started or ended, a question and its answer, the Judge, an operation, a move
//! between steps, an end fact, a flag, a notice, an action of the person).
//!
//! A tool call of the agent gets no line: only its count stays, in NOW and in the numbers of its
//! turn. What the turn gets, the start of the session, and its end with a result get no line
//! either, because the lines of the turn in the ledger say the same in words. The events of one
//! turn, with all of these, stay in the log of the ticket.
//!
//! Each item has one kind, a [`Filter`], that [`filter_of`] gives. The tabs above the feed show
//! one kind, or all of them.
//!
//! The stream has no time in its lines. A line that the reader sees while the view is open
//! gets the time of that read. A line that was in the file before has no known time: it sorts
//! after the start of its turn, in its order, and it shows no time. Only a line of the last
//! event of the file shows the time of the file.
//!
//! The feed keeps a bounded number of items, and it counts only the items that it shows. While
//! it follows, the newest line is at the bottom; a scroll stops the follow, and `back` counts the
//! lines of the filtered list under the view.

use std::collections::BTreeMap;

use serde_json::Value;
use time::OffsetDateTime;

use super::model::Hue;
use super::moments::Moment;
use super::text::{cut, wrap};
use crate::pipeline::commands::LogEntry;
use crate::turn::{tool_summary, StreamEvent};

/// The items of the stream files that the feed keeps.
pub const KEEP_STREAM: usize = 400;
/// The items of the timeline of the ledger that the feed keeps.
pub const KEEP_LEDGER: usize = 200;
/// The lines that one text of an agent can take.
pub const ITEM_LINES: usize = 30;

/// One event of a stream file, with the output tokens of its message when the stream gives
/// them: the id of the message and its count.
#[derive(Clone, Debug, PartialEq)]
pub struct StreamLine {
    pub event: StreamEvent,
    pub output: Option<(String, i64)>,
}

/// The new events of one stream file, as the reader thread sends them.
#[derive(Clone, Debug, PartialEq)]
pub struct Batch {
    /// The id of the turn.
    pub turn: String,
    pub events: Vec<StreamLine>,
    /// True for the events that were in the file when the reader began to follow it.
    pub old: bool,
    /// When the reader read the events. For old events: the time of the last change of the
    /// file.
    pub at: OffsetDateTime,
    /// When the turn started, from its id.
    pub started: Option<OffsetDateTime>,
}

/// The kind of an item of the activity, for the tabs above the feed. Each item has exactly one
/// kind; `All` shows each item.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum Filter {
    #[default]
    All,
    /// What an agent says, the start and the end of its turns, and the reads that Vision serves
    /// for a turn.
    Agents,
    /// The questions of the agents, the Judge, and the answers.
    Questions,
    /// The moves between the steps, the end facts that held, the documents, and the end of
    /// the ticket.
    Steps,
    /// The operations: what Vision sends to the tracker and the forge.
    Writes,
    /// A flag, a session that ended with an error, a turn or an operation that failed, an end
    /// fact that did not hold, a card that was not valid, and a notice of a gate.
    Problems,
    /// The actions of the person.
    You,
}

/// The filters in the order of the tabs.
pub const FILTERS: [Filter; 7] = [
    Filter::All,
    Filter::Agents,
    Filter::Questions,
    Filter::Steps,
    Filter::Writes,
    Filter::Problems,
    Filter::You,
];

impl Filter {
    /// The name of the tab.
    pub fn label(self) -> &'static str {
        match self {
            Filter::All => "All",
            Filter::Agents => "Agents",
            Filter::Questions => "Questions",
            Filter::Steps => "Steps",
            Filter::Writes => "Writes",
            Filter::Problems => "Problems",
            Filter::You => "You",
        }
    }

    /// The place of the tab, from 0.
    pub fn index(self) -> usize {
        FILTERS.iter().position(|f| *f == self).unwrap_or(0)
    }

    /// The next tab, after the last one the first.
    pub fn next(self) -> Filter {
        FILTERS[(self.index() + 1) % FILTERS.len()]
    }

    /// The tab before, before the first one the last.
    pub fn back(self) -> Filter {
        FILTERS[(self.index() + FILTERS.len() - 1) % FILTERS.len()]
    }

    /// True when an item of the kind `kind` shows under this filter.
    pub fn shows(self, kind: Filter) -> bool {
        self == Filter::All || self == kind
    }
}

/// What an item of the feed comes from, for [`filter_of`].
#[derive(Clone, Copy, Debug)]
pub enum Origin<'a> {
    /// A text of a stream file by its word: `says`, `gets`, `start`, or `ends`, and true for
    /// an end with an error.
    Stream { word: &'a str, error: bool },
    /// A line of the timeline of the ledger.
    Ledger(&'a LogEntry),
}

/// The kind of an item: each item of the feed has exactly one.
pub fn filter_of(origin: Origin<'_>) -> Filter {
    let entry = match origin {
        Origin::Stream { error: true, .. } => return Filter::Problems,
        Origin::Stream { .. } => return Filter::Agents,
        Origin::Ledger(entry) => entry,
    };
    let text = |key: &str| entry.detail.get(key).and_then(Value::as_str);
    match entry.source.as_str() {
        "turn" => {
            let failed = entry.kind == "ended"
                && (text("status") == Some("failed")
                    || text("end_fact").is_some_and(|f| f.starts_with("failed:")));
            if failed {
                Filter::Problems
            } else if entry.who == "judge" {
                Filter::Questions
            } else {
                Filter::Agents
            }
        }
        "card" => Filter::Questions,
        "operation" => match entry.kind.as_str() {
            "failed" | "uncertain" => Filter::Problems,
            _ => Filter::Writes,
        },
        _ if entry.who == "user" => Filter::You,
        _ => match entry.kind.as_str() {
            "judge-started" | "judge-applied" | "judge-uses" | "questions-intake"
            | "questions-resumed" | "ask" | "ask-answered" => Filter::Questions,
            "questions-invalid" | "flag" | "flag-cleared" | "notice" => Filter::Problems,
            "end-fact" if entry.detail.get("held").and_then(Value::as_bool) != Some(true) => {
                Filter::Problems
            }
            "requests-served" | "read" | "fallback" => Filter::Agents,
            _ => Filter::Steps,
        },
    }
}

/// One item of the feed.
#[derive(Clone, Debug, PartialEq)]
pub struct Item {
    /// The order of the join: the time in nanoseconds, the source (the ledger first), and the
    /// number of the item in its source.
    pub order: (i128, u8, u64),
    /// The time that the feed shows, when it is known.
    pub at: Option<OffsetDateTime>,
    pub word: String,
    pub hue: Hue,
    pub text: String,
    /// True for a text of the agent, which wraps. Each other item is cut to one line.
    pub wraps: bool,
    /// The kind of the item, for the tabs.
    pub filter: Filter,
}

/// The numbers of one turn that the stream gives.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct TurnStats {
    /// The tool calls of the agent.
    pub tools: usize,
    /// The output tokens of each message, by its id.
    outputs: BTreeMap<String, i64>,
}

impl TurnStats {
    /// The output tokens so far, or `None` when the stream gave none.
    pub fn output_tokens(&self) -> Option<i64> {
        (!self.outputs.is_empty()).then(|| self.outputs.values().sum())
    }
}

/// One line of the feed on the screen.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FeedLine {
    /// The local time as `hh:mm:ss`, on the first line of an item with a known time.
    pub time: String,
    /// The word, on the first line of an item.
    pub word: String,
    pub hue: Hue,
    pub text: String,
    /// The color of the text: the words of the agent are bright, each other text is dim.
    pub text_hue: Hue,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Feed {
    stream: Vec<Item>,
    ledger: Vec<Item>,
    next: u64,
    /// True while the newest line shows at the bottom.
    pub follow: bool,
    /// The lines under the view, while it does not follow.
    pub back: usize,
    /// The numbers of each turn, by its id.
    pub stats: BTreeMap<String, TurnStats>,
}

impl Default for Feed {
    fn default() -> Feed {
        Feed {
            stream: Vec::new(),
            ledger: Vec::new(),
            next: 0,
            follow: true,
            back: 0,
            stats: BTreeMap::new(),
        }
    }
}

fn nanos(at: OffsetDateTime) -> i128 {
    at.unix_timestamp_nanos()
}

impl Feed {
    /// The items of both sources, in the order of the join.
    pub fn items(&self) -> Vec<&Item> {
        let mut out: Vec<&Item> = self.stream.iter().chain(&self.ledger).collect();
        out.sort_by_key(|a| a.order);
        out
    }

    /// The number of items that the feed holds now.
    pub fn len(&self) -> usize {
        self.stream.len() + self.ledger.len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// The number of items of each kind, in the order of [`FILTERS`]: `All` counts each item.
    pub fn counts(&self) -> [usize; 7] {
        let mut out = [0; 7];
        for item in self.stream.iter().chain(&self.ledger) {
            out[0] += 1;
            out[item.filter.index()] += 1;
        }
        out
    }

    /// Replaces the items of the ledger. Gives true when they changed.
    pub fn set_moments(&mut self, moments: &[(Option<OffsetDateTime>, Moment)]) -> bool {
        let start = moments.len().saturating_sub(KEEP_LEDGER);
        let items: Vec<Item> = moments[start..]
            .iter()
            .enumerate()
            .map(|(i, (at, m))| Item {
                order: (at.map_or(0, nanos), 0, i as u64),
                at: *at,
                word: m.word.clone(),
                hue: m.hue.clone(),
                text: m.text.clone(),
                wraps: false,
                filter: m.filter,
            })
            .collect();
        if items == self.ledger {
            return false;
        }
        self.ledger = items;
        true
    }

    /// Adds the events of one read of a stream file. Gives the number of items that it added.
    /// Only the words of the agent and the end of a session with an error add an item. A tool
    /// call only counts in the numbers of its turn. Each other event goes before the bound, so
    /// that a long turn does not push the words of the agent out.
    pub fn add(&mut self, batch: &Batch) -> usize {
        let stats = self.stats.entry(batch.turn.clone()).or_default();
        let base = batch.started.map_or(nanos(batch.at), nanos);
        let last = batch.events.len().saturating_sub(1);
        let mut added = Vec::new();
        for (index, line) in batch.events.iter().enumerate() {
            if let StreamEvent::Assistant { tools, .. } = &line.event {
                stats.tools += tools.iter().filter(|t| counts(&t.name)).count();
            }
            if let Some((id, n)) = &line.output {
                stats.outputs.insert(id.clone(), *n);
            }
            let error = matches!(&line.event, StreamEvent::Result(r) if r.is_error);
            for (word, hue, text, wraps) in feed_texts(&line.event) {
                let filter = filter_of(Origin::Stream { word: &word, error });
                added.push((word, hue, text, wraps, index == last, filter));
            }
        }
        let count = added.len();
        for (i, (word, hue, text, wraps, of_last, filter)) in added.into_iter().enumerate() {
            let (order, at) = if batch.old {
                // The time of the file is the time of its last event only.
                let shown = (of_last && i + 1 == count).then_some(batch.at);
                (base + (i as i128 + 1) * 1_000_000, shown)
            } else {
                (nanos(batch.at), Some(batch.at))
            };
            self.next += 1;
            self.stream.push(Item {
                order: (order, 1, self.next),
                at,
                word,
                hue,
                text,
                wraps,
                filter,
            });
        }
        if self.stream.windows(2).any(|w| w[0].order > w[1].order) {
            self.stream.sort_by_key(|a| a.order);
        }
        let over = self.stream.len().saturating_sub(KEEP_STREAM);
        self.stream.drain(..over);
        count
    }

    /// The lines of the items of the filter for a text column of `width` cells, oldest first.
    pub fn lines(&self, width: usize, offset_seconds: i32, filter: Filter) -> Vec<FeedLine> {
        let mut out = Vec::new();
        for item in self.items().into_iter().filter(|i| filter.shows(i.filter)) {
            let time = item.at.map_or(String::new(), |at| {
                let local = at + time::Duration::seconds(i64::from(offset_seconds));
                format!(
                    "{:02}:{:02}:{:02}",
                    local.hour(),
                    local.minute(),
                    local.second()
                )
            });
            let text_hue = if item.word == "says" {
                Hue::Fg
            } else {
                Hue::Dim
            };
            let parts = if item.wraps {
                let mut parts = wrap(&item.text, width);
                if parts.len() > ITEM_LINES {
                    parts.truncate(ITEM_LINES);
                    if let Some(last) = parts.last_mut() {
                        *last = cut(&format!("{last} …"), width);
                    }
                }
                parts
            } else {
                vec![cut(&item.text, width)]
            };
            let parts = if parts.is_empty() {
                vec![String::new()]
            } else {
                parts
            };
            for (i, text) in parts.into_iter().enumerate() {
                out.push(FeedLine {
                    time: if i == 0 { time.clone() } else { String::new() },
                    word: if i == 0 {
                        item.word.clone()
                    } else {
                        String::new()
                    },
                    hue: item.hue.clone(),
                    text,
                    text_hue: text_hue.clone(),
                });
            }
        }
        out
    }

    /// Scrolls up by `n` lines, out of `total` lines in a view of `height` rows. The feed
    /// stops following.
    pub fn up(&mut self, n: usize, total: usize, height: usize) {
        let most = total.saturating_sub(height);
        if most == 0 {
            return;
        }
        self.back = (self.back + n).min(most);
        self.follow = false;
    }

    /// Scrolls down by `n` lines. At the end, the feed follows again.
    pub fn down(&mut self, n: usize) {
        self.back = self.back.saturating_sub(n);
        if self.back == 0 {
            self.follow = true;
        }
    }

    /// Goes back to the end, and follows.
    pub fn end(&mut self) {
        self.back = 0;
        self.follow = true;
    }

    /// Keeps the same lines in view after `added` new lines came at the end.
    pub fn keep_place(&mut self, added: usize) {
        if !self.follow {
            self.back += added;
        }
    }
}

/// True for a tool call that counts as work of the agent. The tool of the result object does
/// not count.
fn counts(tool: &str) -> bool {
    tool != "StructuredOutput"
}

/// The word of a tool of a session, or `None` for a tool that the feed has no word for.
pub fn tool_word(name: &str) -> Option<(&'static str, Hue)> {
    Some(match name {
        "Read" | "NotebookRead" => ("read", Hue::Info),
        "Grep" | "Glob" | "LS" => ("find", Hue::Info),
        "Edit" | "Write" | "MultiEdit" | "NotebookEdit" => ("edit", Hue::Accent),
        "Bash" | "BashOutput" | "KillShell" | "KillBash" => ("run", Hue::Ok),
        "WebFetch" | "WebSearch" => ("web", Hue::Info),
        "Task" | "Agent" => ("task", Hue::Primary),
        "TodoWrite" => ("todo", Hue::Dim),
        _ => return None,
    })
}

/// The name of a tool with no word of its own, without the name of its server:
/// `mcp__notes__lookup` is `lookup`.
fn short_name(name: &str) -> &str {
    name.rsplit("__")
        .next()
        .filter(|s| !s.is_empty())
        .unwrap_or(name)
}

/// The most telling part of a tool call: a search names its pattern and its folder, each other
/// call the part that `vision logs` shows.
fn call_summary(call: &crate::turn::ToolCall) -> String {
    let text = |key: &str| call.input.get(key).and_then(serde_json::Value::as_str);
    match (call.name.as_str(), text("pattern")) {
        ("Grep" | "Glob", Some(pattern)) => match text("path") {
            Some(path) => format!("\"{pattern}\" in {path}"),
            None => format!("\"{pattern}\""),
        },
        _ => tool_summary(&call.input),
    }
}

/// The items of one event that the activity shows: what the agent says, and the end of a
/// session with an error. What the turn gets, the start of the session, and its end with a
/// result show only in the events of the turn: the lines of the turn in the ledger say the
/// same in words.
fn feed_texts(event: &StreamEvent) -> Vec<(String, Hue, String, bool)> {
    match event {
        StreamEvent::Assistant { .. } => texts_of(event),
        StreamEvent::Result(result) if result.is_error => texts_of(event),
        _ => Vec::new(),
    }
}

/// The texts of one event for the events of a turn, each as its word, its color, its text, and
/// whether it wraps: what the agent says or gets, the start of the session, and its end.
pub fn texts_of(event: &StreamEvent) -> Vec<(String, Hue, String, bool)> {
    match event {
        StreamEvent::Init { model, .. } => vec![(
            "start".into(),
            Hue::Dim,
            match model {
                Some(model) => format!("the session starts with {model}"),
                None => "the session starts".into(),
            },
            false,
        )],
        StreamEvent::RateLimit { .. } => Vec::new(),
        StreamEvent::Assistant { texts, .. } => texts
            .iter()
            .filter(|t| !t.trim().is_empty())
            .map(|t| ("says".to_string(), Hue::Fg, t.clone(), true))
            .collect(),
        StreamEvent::User { texts, .. } => texts
            .iter()
            .filter(|t| !t.trim().is_empty())
            .map(|t| ("gets".to_string(), Hue::Primary, t.clone(), true))
            .collect(),
        StreamEvent::Result(result) => {
            let text = if result.is_error {
                format!("the session ended with an error ({})", result.subtype)
            } else {
                match &result.structured_output {
                    Some(object) => {
                        let kind = object
                            .get("kind")
                            .and_then(serde_json::Value::as_str)
                            .unwrap_or("result");
                        let summary = object
                            .get("summary")
                            .and_then(serde_json::Value::as_str)
                            .unwrap_or("");
                        format!("{kind}: {summary}")
                    }
                    None => "the session ended with no result object".into(),
                }
            };
            vec![("ends".into(), Hue::Primary, text, true)]
        }
    }
}

/// The tool calls of one event, each as its word (`read`, `find`, `edit`, `run`, `web`,
/// `task`, `todo`, or `tool`), its color, and its text.
pub fn tools_of(event: &StreamEvent) -> Vec<(String, Hue, String, bool)> {
    let StreamEvent::Assistant { tools, .. } = event else {
        return Vec::new();
    };
    tools
        .iter()
        .filter(|t| counts(&t.name))
        .map(|call| {
            let summary = call_summary(call);
            match tool_word(&call.name) {
                Some((word, hue)) => {
                    let text = if summary.is_empty() {
                        call.name.clone()
                    } else {
                        summary
                    };
                    (word.to_string(), hue, text, false)
                }
                // A tool with no word of its own shows its name in the text.
                None => {
                    let name = short_name(&call.name);
                    let text = if summary.is_empty() {
                        name.to_string()
                    } else {
                        format!("{name}: {summary}")
                    };
                    ("tool".to_string(), Hue::Dim, text, false)
                }
            }
        })
        .collect()
}

/// Each item of one event with its tool calls after its texts, for the events of one turn.
pub fn words_of(event: &StreamEvent) -> Vec<(String, Hue, String, bool)> {
    let mut out = texts_of(event);
    out.extend(tools_of(event));
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn entry(source: &str, kind: &str, who: &str, detail: Value) -> LogEntry {
        LogEntry {
            at: "2026-11-16T14:00:00Z".into(),
            source: source.into(),
            kind: kind.into(),
            id: None,
            text: String::new(),
            who: who.into(),
            detail,
        }
    }

    fn of(source: &str, kind: &str, who: &str, detail: Value) -> Filter {
        filter_of(Origin::Ledger(&entry(source, kind, who, detail)))
    }

    #[test]
    fn each_kind_of_item_has_one_tab() {
        for word in ["says", "gets", "start", "ends"] {
            assert_eq!(
                filter_of(Origin::Stream { word, error: false }),
                Filter::Agents,
                "{word}"
            );
        }
        assert_eq!(
            filter_of(Origin::Stream {
                word: "ends",
                error: true
            }),
            Filter::Problems
        );
        let null = Value::Null;
        let cases = [
            ("turn", "started", "builder", null.clone(), Filter::Agents),
            ("turn", "started", "judge", null.clone(), Filter::Questions),
            (
                "turn",
                "ended",
                "builder",
                json!({"status": "done", "end_fact": "held"}),
                Filter::Agents,
            ),
            (
                "turn",
                "ended",
                "builder",
                json!({"status": "failed"}),
                Filter::Problems,
            ),
            (
                "turn",
                "ended",
                "builder",
                json!({"status": "done", "end_fact": "failed:commits"}),
                Filter::Problems,
            ),
            (
                "turn",
                "ended",
                "judge",
                json!({"status": "done"}),
                Filter::Questions,
            ),
            ("card", "asked", "builder", null.clone(), Filter::Questions),
            (
                "card",
                "answered",
                "builder",
                null.clone(),
                Filter::Questions,
            ),
            (
                "card",
                "needless",
                "builder",
                null.clone(),
                Filter::Questions,
            ),
            (
                "operation",
                "confirmed",
                "vision",
                null.clone(),
                Filter::Writes,
            ),
            (
                "operation",
                "pending",
                "vision",
                null.clone(),
                Filter::Writes,
            ),
            (
                "operation",
                "failed",
                "vision",
                null.clone(),
                Filter::Problems,
            ),
            (
                "operation",
                "uncertain",
                "vision",
                null.clone(),
                Filter::Problems,
            ),
            ("event", "take", "user", null.clone(), Filter::You),
            ("event", "stop", "user", null.clone(), Filter::You),
            ("event", "retry", "user", null.clone(), Filter::You),
            ("event", "pause", "user", null.clone(), Filter::You),
            ("event", "human", "user", null.clone(), Filter::You),
            ("event", "card-answered", "user", null.clone(), Filter::You),
            ("event", "card-needless", "user", null.clone(), Filter::You),
            ("event", "told", "user", null.clone(), Filter::You),
            ("event", "correct", "user", null.clone(), Filter::You),
            ("event", "notice-read", "user", null.clone(), Filter::You),
            (
                "event",
                "judge-started",
                "tick",
                null.clone(),
                Filter::Questions,
            ),
            (
                "event",
                "judge-applied",
                "tick",
                null.clone(),
                Filter::Questions,
            ),
            (
                "event",
                "judge-uses",
                "tick",
                null.clone(),
                Filter::Questions,
            ),
            (
                "event",
                "questions-intake",
                "tick",
                null.clone(),
                Filter::Questions,
            ),
            (
                "event",
                "questions-resumed",
                "tick",
                null.clone(),
                Filter::Questions,
            ),
            ("event", "ask", "tick", null.clone(), Filter::Questions),
            (
                "event",
                "ask-answered",
                "tick",
                null.clone(),
                Filter::Questions,
            ),
            (
                "event",
                "questions-invalid",
                "tick",
                null.clone(),
                Filter::Problems,
            ),
            ("event", "flag", "tick", null.clone(), Filter::Problems),
            (
                "event",
                "flag-cleared",
                "tick",
                null.clone(),
                Filter::Problems,
            ),
            ("event", "notice", "tick", null.clone(), Filter::Problems),
            (
                "event",
                "end-fact",
                "tick",
                json!({"held": true}),
                Filter::Steps,
            ),
            (
                "event",
                "end-fact",
                "tick",
                json!({"fact": "gate:cr:cr.md"}),
                Filter::Problems,
            ),
            (
                "event",
                "requests-served",
                "tick",
                null.clone(),
                Filter::Agents,
            ),
            ("event", "read", "executor", null.clone(), Filter::Agents),
            ("event", "fallback", "tick", null.clone(), Filter::Agents),
            ("event", "step", "tick", null.clone(), Filter::Steps),
            ("event", "visit", "tick", null.clone(), Filter::Steps),
            ("event", "skipped", "tick", null.clone(), Filter::Steps),
            ("event", "documents", "tick", null.clone(), Filter::Steps),
            ("event", "check", "tick", null.clone(), Filter::Steps),
            ("event", "kind", "tick", null.clone(), Filter::Steps),
            ("event", "extra-round", "tick", null.clone(), Filter::Steps),
            ("event", "escape-check", "tick", null.clone(), Filter::Steps),
            ("event", "ended", "tick", null.clone(), Filter::Steps),
        ];
        for (source, kind, who, detail, want) in cases {
            assert_eq!(of(source, kind, who, detail), want, "{source} {kind} {who}");
        }
    }

    #[test]
    fn the_tabs_go_round_and_all_shows_each_kind() {
        assert_eq!(Filter::All.next(), Filter::Agents);
        assert_eq!(Filter::You.next(), Filter::All);
        assert_eq!(Filter::All.back(), Filter::You);
        assert_eq!(Filter::Steps.index(), 3);
        for kind in FILTERS {
            assert!(Filter::All.shows(kind));
            assert_eq!(Filter::Writes.shows(kind), kind == Filter::Writes);
        }
    }
}
