//! Sample data of the live view: the data that its loader reads, a timeline, the events of a
//! stream file, and a model with the live view of a ticket open in each state. The data is
//! invented, with neutral names.

use serde_json::json;
use time::macros::datetime;
use time::OffsetDateTime;
use vision::app::feed::{Batch, StreamLine};
use vision::app::live::{Member, StepLine, TicketData};
use vision::app::model::{App, Held};
use vision::app::msg::{Key, Msg};
use vision::pipeline::commands::{LogEntry, Measures};
use vision::pipeline::view::FlagView;
use vision::snapshot::Need;
use vision::turn::{ResultEvent, StreamEvent, ToolCall};

use super::support::*;

/// The turn of the samples that runs now.
pub const TURN: &str = "20261116T142400Z-DEMO-12-build-1";

/// A time of the wall clock of the tests: `seconds` after 14:00:00 UTC.
pub fn at(seconds: i64) -> OffsetDateTime {
    datetime!(2026-11-16 14:00:00 UTC) + time::Duration::seconds(seconds)
}

fn rfc(seconds: i64) -> String {
    vision::util::time::rfc3339(at(seconds))
}

pub fn team() -> Vec<Member> {
    [
        ("scout", "claude-fable-5-2", "high"),
        ("builder", "claude-opus-5-5", "xhigh"),
        ("reviewer", "claude-opus-5-5", "max"),
        ("tester", "claude-opus-5-5", "high"),
        ("shepherd", "claude-opus-5-5", "high"),
        ("judge", "claude-fable-5-2", "high"),
        ("curator", "claude-fable-5-2", "high"),
    ]
    .into_iter()
    .map(|(agent, model, effort)| Member {
        agent: agent.into(),
        model: model.into(),
        effort: effort.into(),
    })
    .collect()
}

/// The steps of the samples with the measures of a ticket at `step`.
pub fn steps(step: &str) -> Vec<StepLine> {
    let at = STEPS.iter().position(|s| *s == step).unwrap_or(0);
    let done = [
        (240, 0.21),
        (540, 0.88),
        (120, 0.0),
        (0, 0.0),
        (0, 0.0),
        (0, 0.0),
    ];
    STEPS
        .iter()
        .enumerate()
        .map(|(i, id)| {
            let (seconds, cost) = if i < at {
                done.get(i).copied().unwrap_or((60, 0.1))
            } else if i == at {
                (360, 0.0)
            } else {
                (0, 0.0)
            };
            let kind = match *id {
                "plan-ready" => "human",
                "ci" | "ready" | "ci-review" | "ready-to-merge" => "wait",
                "done" => "end",
                _ => "turn",
            };
            StepLine {
                id: id.to_string(),
                kind: kind.into(),
                entries: u32::from(i <= at),
                seconds,
                cost_usd: cost,
            }
        })
        .collect()
}

fn entry(
    seconds: i64,
    source: &str,
    kind: &str,
    who: &str,
    id: Option<&str>,
    detail: serde_json::Value,
) -> LogEntry {
    LogEntry {
        at: rfc(seconds),
        source: source.into(),
        kind: kind.into(),
        id: id.map(str::to_string),
        text: String::new(),
        who: who.into(),
        detail,
    }
}

/// The timeline of a ticket that went from the take to the step build.
pub fn timeline() -> Vec<LogEntry> {
    let scout = "20261116T135204Z-DEMO-12-refine-1";
    let plan = "20261116T135630Z-DEMO-12-plan-1";
    vec![
        entry(-480, "event", "take", "user", None, json!({})),
        entry(
            -476,
            "turn",
            "started",
            "scout",
            Some(scout),
            json!({"step": "refine", "model": "claude-fable-5-2", "effort": "high"}),
        ),
        entry(
            -226,
            "turn",
            "ended",
            "scout",
            Some(scout),
            json!({"step": "refine", "status": "done", "reason": "result", "cost": 0.21, "end_fact": "held"}),
        ),
        entry(
            -225,
            "operation",
            "confirmed",
            "vision",
            None,
            json!({"verb": "tracker.describe", "head": "", "outcome": "confirmed", "detail": ""}),
        ),
        entry(
            -224,
            "event",
            "step",
            "tick",
            None,
            json!({"from": "refine", "to": "plan"}),
        ),
        entry(
            -210,
            "turn",
            "started",
            "builder",
            Some(plan),
            json!({"step": "plan", "model": "claude-opus-5-5", "effort": "xhigh"}),
        ),
        entry(
            330,
            "turn",
            "ended",
            "builder",
            Some(plan),
            json!({"step": "plan", "status": "done", "reason": "result", "cost": 0.88, "end_fact": "held"}),
        ),
        entry(
            331,
            "event",
            "step",
            "tick",
            None,
            json!({"from": "plan", "to": "plan-ready"}),
        ),
        entry(
            1200,
            "event",
            "human",
            "user",
            None,
            json!({"action": "approve-plan"}),
        ),
        entry(
            1201,
            "event",
            "step",
            "tick",
            None,
            json!({"from": "plan-ready", "to": "build"}),
        ),
        entry(
            1440,
            "turn",
            "started",
            "builder",
            Some(TURN),
            json!({"step": "build", "model": "claude-opus-5-5", "effort": "xhigh"}),
        ),
    ]
}

/// The data of the live view of a ticket in progress.
pub fn data_of(held: &Held, needs: &[Need]) -> TicketData {
    let step = held.view.step.clone().unwrap_or_default();
    TicketData {
        view: held.view.clone(),
        facts: held.facts.clone(),
        taken: held.taken.clone(),
        steps: steps(&step),
        team: team(),
        timeline: timeline(),
        measures: Measures {
            cards_to_person: needs.len(),
            cards_by_memory: 1,
            cards_by_judge: 0,
            ..Measures::default()
        },
        unpushed: Some(0),
        plan: Some("3f2a1b4c5d6e".into()),
        jobs: vec!["proof".into()],
        needs: needs
            .iter()
            .filter(|n| n.ticket.as_deref() == Some(held.view.key.as_str()))
            .cloned()
            .collect(),
    }
}

fn line(event: StreamEvent) -> StreamLine {
    StreamLine {
        event,
        output: None,
    }
}

fn tool(name: &str, input: serde_json::Value) -> StreamEvent {
    StreamEvent::Assistant {
        texts: Vec::new(),
        tools: vec![ToolCall {
            name: name.into(),
            input,
        }],
    }
}

fn says(text: &str) -> StreamEvent {
    StreamEvent::Assistant {
        texts: vec![text.into()],
        tools: Vec::new(),
    }
}

/// The events of the running turn, as the reader saw them while the view was open.
pub fn events() -> Vec<(i64, StreamEvent)> {
    vec![
        (1441, StreamEvent::Init {
            session_id: Some("s".into()),
            tools: Vec::new(),
            permission_mode: None,
            mcp_servers: Vec::new(),
            model: Some("claude-opus-5-5".into()),
        }),
        (1445, tool("Read", json!({"file_path": "src/report/export.py"}))),
        (1452, tool("Grep", json!({"pattern": "def export_month", "path": "src"}))),
        (1460, says("The export writes each row by hand. I add a CSV writer and keep the order of the columns that the plan names.")),
        (1475, tool("Edit", json!({"file_path": "src/report/export.py"}))),
        (1490, tool("Bash", json!({"command": "python3 -m unittest tests/test_export.py"}))),
        (1503, says("3 tests pass. I add a test for an empty month next.")),
        (1510, tool("mcp__notes__lookup", json!({"query": "csv"}))),
    ]
}

/// The batches of the events, one for each event, each at its own time.
pub fn batches(events: Vec<(i64, StreamEvent)>) -> Vec<Batch> {
    events
        .into_iter()
        .map(|(seconds, event)| Batch {
            turn: TURN.into(),
            events: vec![line(event)],
            old: false,
            at: at(seconds),
            started: Some(at(1440)),
        })
        .collect()
}

/// A result event of a turn.
pub fn result_event(summary: &str) -> StreamEvent {
    StreamEvent::Result(ResultEvent {
        subtype: "success".into(),
        structured_output: Some(json!({"kind": "result", "summary": summary})),
        ..ResultEvent::default()
    })
}

/// A model with the live view of the first ticket open, its data, and the events of its
/// running turn. The clock is at 14:30:00 plus `t` seconds of the app.
pub fn live(
    size: (u16, u16),
    tickets: Vec<Held>,
    needs: Vec<Need>,
    events: Vec<(i64, StreamEvent)>,
) -> App {
    let data = data_of(&tickets[0], &needs);
    let key = tickets[0].view.key.clone();
    let mut app = home_with(size, local(tickets, needs), Some(queue()));
    send(&mut app, 2.0, Msg::Key(Key::Enter));
    assert!(matches!(&app.screen, vision::app::model::Screen::Live { key: k } if *k == key));
    send(
        &mut app,
        2.1,
        Msg::Ticket {
            key: key.clone(),
            data: Box::new(Ok(data)),
        },
    );
    for batch in batches(events) {
        send(
            &mut app,
            2.2,
            Msg::Stream {
                key: key.clone(),
                batch,
            },
        );
    }
    send(&mut app, 2.4, Msg::Tick);
    app
}

/// The live view of DEMO-12 while builder works at the step build.
pub fn working_live(size: (u16, u16)) -> App {
    let mut held = working(
        "DEMO-12",
        "Export the report of the month as a CSV file",
        "build",
        "builder",
        6,
    );
    if let Some(work) = held.facts.work.as_mut() {
        work.id = TURN.into();
        work.n = 3;
    }
    held.facts.turns = 3;
    held.facts.agents = vec!["scout".into(), "builder".into()];
    held.view.marks.plan = Some("3f2a1b4c5d6e".into());
    live(
        size,
        vec![held, two_tickets().remove(1)],
        Vec::new(),
        events(),
    )
}

/// A flag of a ticket, set 3 minutes ago.
pub fn flag(name: &str, reason: &str) -> FlagView {
    FlagView {
        flag: name.into(),
        reason: reason.into(),
        since: Some(minutes_ago(3)),
        command: String::new(),
    }
}

/// DEMO-12 at a step with no work turn: the base of the states that rest.
pub fn resting(step: &str) -> Held {
    let mut held = working(
        "DEMO-12",
        "Export the report of the month as a CSV file",
        step,
        "builder",
        6,
    );
    held.view.works = None;
    held.facts.work = None;
    held.facts.turns = 3;
    held.view.marks.plan = Some("3f2a1b4c5d6e".into());
    held
}
