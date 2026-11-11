//! The texts of the pagers as lines of colored parts: the log of a ticket (time, who, what,
//! detail), the events of one turn, the plan with its hash, and the commits that the forge does
//! not have. Pure functions of data that a loader read.

use super::feed::{words_of, StreamLine};
use super::model::Hue;
use super::moments::{moment, who_hue};
use super::pager::{PagerLine, Span};
use crate::pipeline::commands::LogEntry;
use crate::pipeline::{DiffView, PlanView, SHORT_HASH};
use crate::util::time as vtime;

/// The local time of an RFC 3339 time as `hh:mm:ss`, or the text itself when it does not
/// parse.
pub fn clock_of(at: &str, offset_seconds: i32) -> String {
    match vtime::parse_rfc3339(at) {
        Ok(at) => {
            let local = at + time::Duration::seconds(i64::from(offset_seconds));
            format!(
                "{:02}:{:02}:{:02}",
                local.hour(),
                local.minute(),
                local.second()
            )
        }
        Err(_) => at.to_string(),
    }
}

/// The log of a ticket: the names of the columns and one line for each line of the timeline.
/// A line of a turn names its turn, so that Enter opens its events.
pub fn log_lines(timeline: &[LogEntry], offset_seconds: i32) -> (PagerLine, Vec<PagerLine>) {
    let head = PagerLine::of(vec![
        Span::new("TIME", Hue::Dim).bold().width(10),
        Span::new("WHO", Hue::Dim).bold().width(10),
        Span::new("WHAT", Hue::Dim).bold().width(10),
        Span::new("DETAIL", Hue::Dim).bold(),
    ]);
    let lines = timeline
        .iter()
        .map(|entry| {
            let m = moment(entry);
            PagerLine {
                spans: vec![
                    Span::new(clock_of(&entry.at, offset_seconds), Hue::Faint).width(10),
                    Span::new(m.who.clone(), who_hue(&m.who)).bold().width(10),
                    Span::new(m.what.clone(), Hue::Fg).width(10),
                    Span::new(m.text.clone(), Hue::Dim),
                ],
                turn: m.turn,
            }
        })
        .collect();
    (head, lines)
}

/// The width at which the text of an event wraps in the pager: it fits a window of 100
/// columns.
const TURN_TEXT: usize = 88;

/// The events of one turn, each on its own line with its word. A text wraps.
pub fn turn_lines(events: &[StreamLine]) -> Vec<PagerLine> {
    let mut out = Vec::new();
    for line in events {
        for (word, hue, text, _) in words_of(&line.event) {
            let text_hue = if word == "says" { Hue::Fg } else { Hue::Dim };
            let parts: Vec<String> = text
                .lines()
                .filter(|l| !l.trim().is_empty())
                .flat_map(|l| super::text::wrap(l, TURN_TEXT))
                .collect();
            for (i, part) in parts.into_iter().enumerate() {
                let word = if i == 0 { word.clone() } else { String::new() };
                out.push(PagerLine::of(vec![
                    Span::new(word, hue.clone()).bold().width(7),
                    Span::new(part, text_hue.clone()),
                ]));
            }
        }
    }
    if out.is_empty() {
        out.push(PagerLine::of(vec![Span::new(
            "The stream file of this turn has no event yet.",
            Hue::Dim,
        )]));
    }
    out
}

/// The plan: a line with its hash and its state, then its text.
pub fn plan_lines(plan: &PlanView) -> Vec<PagerLine> {
    let state = if plan.waits {
        "It waits for your approval."
    } else {
        "It does not wait for an approval now."
    };
    let mut out = vec![
        PagerLine::of(vec![
            Span::new("hash ", Hue::Dim),
            Span::new(&plan.hash[..SHORT_HASH.min(plan.hash.len())], Hue::Accent).bold(),
            Span::new(format!("  {state}"), Hue::Dim),
        ]),
        PagerLine::default(),
    ];
    for line in plan.text.lines() {
        let hue = if line.starts_with('#') {
            Hue::Accent
        } else {
            Hue::Fg
        };
        out.push(PagerLine::of(vec![Span::new(line, hue)]));
    }
    if plan.edited {
        out.push(PagerLine::default());
        out.push(PagerLine::of(vec![Span::new(
            "You edited the plan after the agent wrote it. The first text of the agent stays in plan.agent.md.",
            Hue::Dim,
        )]));
    }
    out
}

/// The commits of the branch that the forge does not have.
pub fn diff_lines(diff: &DiffView) -> Vec<PagerLine> {
    let mut out = vec![PagerLine::of(vec![Span::new(
        if diff.commits.is_empty() {
            format!(
                "The forge has each commit of {} ({} is up to date).",
                diff.branch, diff.compared_with
            )
        } else {
            format!(
                "{} of {} that {} does not have yet:",
                crate::app::text::count(diff.commits.len(), "commit"),
                diff.branch,
                diff.compared_with
            )
        },
        Hue::Dim,
    )])];
    out.push(PagerLine::default());
    for commit in &diff.commits {
        out.push(PagerLine::of(vec![
            Span::new(crate::executor::short(&commit.commit), Hue::Accent).width(14),
            Span::new(&commit.subject, Hue::Fg),
        ]));
    }
    out
}
