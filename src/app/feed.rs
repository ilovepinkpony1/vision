//! The activity of the live view: a feed with the newest line at the bottom. It joins two
//! sources by time: the events of the stream files of the turns of the ticket (what the agent
//! says and each tool that it calls), and the timeline of the ticket from the ledger (a turn
//! that started or ended, an answer of the Judge, an operation, a flag, an action of the
//! person).
//!
//! The stream has no time in its lines. A line that the reader sees while the view is open
//! gets the time of that read. A line that was in the file before has no known time: it sorts
//! after the start of its turn, in its order, and it shows no time, except the first one (the
//! start of the turn) and the last one (the time of the file).
//!
//! The feed keeps a bounded number of items. While it follows, the newest line is at the
//! bottom; a scroll stops the follow, and `back` counts the lines under the view.

use std::collections::BTreeMap;

use time::OffsetDateTime;

use super::model::Hue;
use super::moments::Moment;
use super::text::{cut, wrap};
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
}

/// The numbers of one turn that the stream gives.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct TurnStats {
    /// The tool calls of the agent.
    pub tools: usize,
    /// The output tokens of each message, by its id.
    outputs: BTreeMap<String, i64>,
    /// True after the first message of the agent in the stream of the turn.
    spoke: bool,
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
            })
            .collect();
        if items == self.ledger {
            return false;
        }
        self.ledger = items;
        true
    }

    /// Adds the events of one read of a stream file. Gives the number of items that it added.
    pub fn add(&mut self, batch: &Batch) -> usize {
        let stats = self.stats.entry(batch.turn.clone()).or_default();
        let base = batch.started.map_or(nanos(batch.at), nanos);
        let mut added = Vec::new();
        for line in &batch.events {
            // What the turn gets before the agent says anything is the text that starts the
            // turn. It names files of the turn folder, so it takes one line. The pager of the
            // turn shows it in full.
            let prompt = matches!(line.event, StreamEvent::User { .. }) && !stats.spoke;
            if let StreamEvent::Assistant { tools, .. } = &line.event {
                stats.tools += tools.iter().filter(|t| counts(&t.name)).count();
                stats.spoke = true;
            }
            if let Some((id, n)) = &line.output {
                stats.outputs.insert(id.clone(), *n);
            }
            for (word, hue, text, wraps) in words_of(&line.event) {
                added.push((
                    word,
                    hue,
                    text,
                    wraps && !prompt,
                    matches!(line.event, StreamEvent::Init { .. }),
                ));
            }
        }
        let count = added.len();
        for (i, (word, hue, text, wraps, init)) in added.into_iter().enumerate() {
            let (order, at) = if batch.old {
                let shown = if init {
                    batch.started
                } else if i + 1 == count {
                    Some(batch.at)
                } else {
                    None
                };
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
            });
        }
        if self.stream.windows(2).any(|w| w[0].order > w[1].order) {
            self.stream.sort_by_key(|a| a.order);
        }
        let over = self.stream.len().saturating_sub(KEEP_STREAM);
        self.stream.drain(..over);
        count
    }

    /// The lines of the feed for a text column of `width` cells, oldest first.
    pub fn lines(&self, width: usize, offset_seconds: i32) -> Vec<FeedLine> {
        let mut out = Vec::new();
        for item in self.items() {
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

/// The items of one event: the word, its color, the text, and whether it wraps.
pub fn words_of(event: &StreamEvent) -> Vec<(String, Hue, String, bool)> {
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
        StreamEvent::Assistant { texts, tools } => {
            let mut out: Vec<(String, Hue, String, bool)> = texts
                .iter()
                .filter(|t| !t.trim().is_empty())
                .map(|t| ("says".to_string(), Hue::Fg, t.clone(), true))
                .collect();
            for call in tools.iter().filter(|t| counts(&t.name)) {
                let summary = call_summary(call);
                let item = match tool_word(&call.name) {
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
                };
                out.push(item);
            }
            out
        }
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
