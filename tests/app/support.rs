//! Sample data, a model at each state, the text of a drawn frame, and the snapshot helper. The
//! snapshots are plain text files in `tests/snapshots/app/`. A failed comparison prints each
//! line that differs; `VISION_UPDATE_SNAPSHOTS=1` writes the files again.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::Duration;

use ratatui::backend::TestBackend;
use ratatui::buffer::{Buffer, CellWidth};
use ratatui::Terminal;
use time::macros::datetime;
use vision::app::model::{App, Clock, Held, Local, Setup, Week};
use vision::app::msg::{ChildResult, Effect, JobDone, Msg};
use vision::app::update::{start, update};
use vision::app::view::{self, theme::ColorMode};
use vision::ledger::LimitReading;
use vision::pipeline::commands::PickRow;
use vision::pipeline::facts::{StateFacts, TurnFact, WaitFacts};
use vision::pipeline::view::{FlagView, Marks, Works};
use vision::pipeline::TicketView;
use vision::snapshot::{LimitView, LimitsView, Need, NeedKind};

/// The wall clock of each test: 2026-11-16 14:30:00 UTC.
pub fn clock(seconds: f64) -> Clock {
    Clock::at(
        Duration::from_secs_f64(seconds),
        datetime!(2026-11-16 14:30:00 UTC),
    )
}

/// The time in RFC 3339 that is `minutes` before the wall clock of the tests.
pub fn minutes_ago(minutes: i64) -> String {
    let at = datetime!(2026-11-16 14:30:00 UTC) - time::Duration::minutes(minutes);
    vision::util::time::rfc3339(at)
}

pub const STEPS: [&str; 12] = [
    "refine",
    "plan",
    "plan-ready",
    "build",
    "review",
    "ci",
    "fix-ci",
    "ready",
    "comments",
    "ci-review",
    "ready-to-merge",
    "done",
];

pub fn setup() -> Setup {
    Setup {
        home: "~/.vision".into(),
        adapter: "demo".into(),
        steps: STEPS.iter().map(|s| s.to_string()).collect(),
    }
}

/// A row of the status view.
pub fn view(key: &str, title: &str, step: &str, now: &str) -> TicketView {
    TicketView {
        key: key.into(),
        title: (!title.is_empty()).then(|| title.to_string()),
        step: Some(step.into()),
        kind: Some("turn".into()),
        now: now.into(),
        works: None,
        flags: Vec::new(),
        needs: BTreeMap::new(),
        marks: Marks::default(),
        cost_usd: 0.0,
        pr: None,
        branch: Some(format!("vision/{key}")),
        clone: Some("~/clones/demo-1".into()),
        updated: None,
    }
}

/// A ticket in progress whose agent works.
pub fn working(key: &str, title: &str, step: &str, agent: &str, minutes: i64) -> Held {
    let mut v = view(key, title, step, &format!("{agent} works ({minutes} min)"));
    v.works = Some(Works {
        agent: agent.into(),
        turn: format!("20261116T140000Z-{key}-{step}-1"),
        minutes,
    });
    v.cost_usd = 1.03;
    let facts = StateFacts {
        key: key.into(),
        agent: Some(agent.into()),
        work: Some(TurnFact {
            id: format!("20261116T140000Z-{key}-{step}-1"),
            agent: agent.into(),
            step: step.into(),
            model: Some("claude-opus-5-5".into()),
            effort: Some("xhigh".into()),
            started: Some(minutes_ago(minutes)),
            n: 2,
        }),
        turns: 2,
        agents: vec!["scout".into(), agent.into()],
        ..StateFacts::default()
    };
    Held {
        view: v,
        facts,
        taken: Some(minutes_ago(38)),
        stopped: false,
        needs_you: false,
    }
}

/// A ticket in progress that waits.
pub fn waiting(key: &str, title: &str, step: &str, now: &str) -> Held {
    let mut v = view(key, title, step, now);
    v.kind = Some("wait".into());
    v.cost_usd = 6.12;
    v.pr = Some(7);
    let facts = StateFacts {
        key: key.into(),
        wait: Some(WaitFacts {
            watch: Some("ci.state".into()),
            value: Some("none".into()),
            since: Some(minutes_ago(9)),
            observed: Some(minutes_ago(0)),
            ..WaitFacts::default()
        }),
        turns: 5,
        agents: vec!["scout".into(), "builder".into(), "reviewer".into()],
        ..StateFacts::default()
    };
    Held {
        view: v,
        facts,
        taken: Some(minutes_ago(130)),
        stopped: false,
        needs_you: false,
    }
}

/// A ticket in progress that needs the person.
pub fn needing(key: &str, title: &str, step: &str) -> Held {
    let mut v = view(key, title, step, "needs you: 1 card. Run `vision needs`");
    v.needs.insert("card".into(), 1);
    v.cost_usd = 1.09;
    Held {
        view: v,
        facts: StateFacts {
            key: key.into(),
            open_cards: 1,
            turns: 2,
            agents: vec!["scout".into(), "builder".into(), "judge".into()],
            ..StateFacts::default()
        },
        taken: Some(minutes_ago(41)),
        stopped: false,
        needs_you: true,
    }
}

/// A stopped ticket.
pub fn stopped(key: &str, title: &str, step: &str) -> Held {
    let mut v = view(key, title, step, "stopped: you stopped it with vision stop");
    v.flags.push(FlagView {
        flag: "stopped".into(),
        reason: "you stopped it with vision stop".into(),
        since: Some(minutes_ago(5)),
        command: format!("vision take {key}"),
    });
    v.cost_usd = 2.5;
    Held {
        view: v,
        facts: StateFacts {
            key: key.into(),
            turns: 4,
            agents: vec!["scout".into(), "builder".into(), "reviewer".into()],
            ..StateFacts::default()
        },
        taken: Some(minutes_ago(300)),
        stopped: true,
        needs_you: false,
    }
}

pub fn limits() -> LimitsView {
    LimitsView {
        five_hour: LimitView {
            stop_at: 0.6,
            reading: Some(LimitReading {
                part: 0.31,
                at: minutes_ago(3),
                turn: "t".into(),
            }),
        },
        seven_day: LimitView {
            stop_at: 0.85,
            reading: Some(LimitReading {
                part: 0.71,
                at: minutes_ago(3),
                turn: "t".into(),
            }),
        },
    }
}

pub fn card_need(key: &str) -> Need {
    Need {
        kind: NeedKind::Card,
        ticket: Some(key.into()),
        id: "Q-0002".into(),
        text: "builder asks: Must the export keep the order of the columns?".into(),
    }
}

pub fn plan_need(key: &str) -> Need {
    Need {
        kind: NeedKind::Plan,
        ticket: Some(key.into()),
        id: "3f2a1b4c5d6e".into(),
        text: format!("the plan of {key} waits for your approval at the step plan-ready"),
    }
}

pub fn local(tickets: Vec<Held>, needs: Vec<Need>) -> Local {
    let agents = tickets
        .iter()
        .filter_map(|h| h.view.works.as_ref().map(|w| w.agent.clone()))
        .collect::<Vec<_>>();
    Local {
        off: false,
        limits: limits(),
        needs,
        turns: agents.len(),
        agents,
        tickets,
        week: Week {
            cost_usd: 23.84,
            finished: 2,
            without_you: 11,
            questions: 14,
        },
        timer: false,
        running: None,
        resets: [Some("2026-11-16T18:00:00Z".into()), None],
    }
}

/// The two tickets in progress of the samples.
pub fn two_tickets() -> Vec<Held> {
    vec![
        working(
            "DEMO-12",
            "Export the report of the month as a CSV file",
            "build",
            "builder",
            6,
        ),
        waiting(
            "DEMO-7",
            "Show the totals under the table of orders",
            "ci",
            "waits: the watched value is none",
        ),
    ]
}

pub fn queue() -> Vec<PickRow> {
    vec![
        PickRow {
            key: "DEMO-15".into(),
            title: "Keep the filter of the list when the page loads again".into(),
            status: "To Do".into(),
            eligible: true,
            reason: String::new(),
            taken: None,
        },
        PickRow {
            key: "DEMO-9".into(),
            title: "Fix the date format in the weekly email".into(),
            status: "To Do".into(),
            eligible: false,
            reason: "it already has an open pull request".into(),
            taken: None,
        },
    ]
}

/// Hands a message to the update at a time.
pub fn send(app: &mut App, at: f64, msg: Msg) -> Vec<Effect> {
    app.clock = clock(at);
    update(app, msg)
}

/// Ends each job that the effects started, with a result that worked.
pub fn finish_jobs(app: &mut App, at: f64, effects: &[Effect]) -> Vec<Effect> {
    let mut more = Vec::new();
    for effect in effects {
        if let Effect::Run(job) = effect {
            more.extend(send(
                app,
                at,
                Msg::JobDone(JobDone {
                    id: job.id,
                    kind: job.kind.clone(),
                    result: ChildResult {
                        code: Some(0),
                        stdout: "tick done in 12 ms\n".into(),
                        ..ChildResult::default()
                    },
                }),
            ));
        }
    }
    more
}

/// A model on the loader at time 0, with the home folder read started.
pub fn booting(size: (u16, u16)) -> App {
    let mut app = App::new(clock(0.0), size, ColorMode::True);
    start(&mut app);
    app
}

/// A model on Home after the loader, with the local data, the queue, and no job that runs.
pub fn home_with(size: (u16, u16), local: Local, queue: Option<Vec<PickRow>>) -> App {
    let mut app = booting(size);
    send(&mut app, 0.1, Msg::Setup(Ok(setup())));
    let fx = send(&mut app, 0.2, Msg::Local(Ok(local.clone())));
    finish_jobs(&mut app, 0.3, &fx);
    // The end of the first pass loads the local data again.
    send(&mut app, 0.35, Msg::Local(Ok(local)));
    if let Some(queue) = queue {
        send(&mut app, 0.4, Msg::Queue(Ok(queue)));
    }
    send(&mut app, 1.0, Msg::Tick);
    app
}

/// The sample Home: two tickets in progress and a queue of two.
pub fn home(size: (u16, u16)) -> App {
    home_with(size, local(two_tickets(), Vec::new()), Some(queue()))
}

/// Draws the model into a test backend of its size.
pub fn render(app: &App) -> Buffer {
    let mut terminal = Terminal::new(TestBackend::new(app.size.0, app.size.1)).unwrap();
    terminal.draw(|frame| view::draw(app, frame)).unwrap();
    terminal.backend().buffer().clone()
}

/// The text of a buffer: one line for each row, with the spaces at the end cut.
pub fn text(buf: &Buffer) -> String {
    let mut out = String::new();
    for y in 0..buf.area.height {
        let mut line = String::new();
        let mut x = 0;
        while x < buf.area.width {
            let cell = &buf[(x, y)];
            let symbol = cell.symbol();
            line.push_str(if symbol.is_empty() { " " } else { symbol });
            x += symbol.cell_width().max(1);
        }
        out.push_str(line.trim_end());
        out.push('\n');
    }
    out
}

/// The text of the model at its size.
pub fn screen(app: &App) -> String {
    text(&render(app))
}

fn snapshot_path(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/snapshots/app")
        .join(format!("{name}.txt"))
}

/// Compares a text with its stored snapshot, or writes the snapshot with
/// `VISION_UPDATE_SNAPSHOTS=1`.
pub fn assert_snapshot(name: &str, actual: &str) {
    let path = snapshot_path(name);
    if std::env::var("VISION_UPDATE_SNAPSHOTS").as_deref() == Ok("1") {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, actual).unwrap();
        return;
    }
    let expected = std::fs::read_to_string(&path).unwrap_or_else(|_| {
        panic!(
            "the snapshot {} is missing. Run the tests with VISION_UPDATE_SNAPSHOTS=1 to write it, and read it before you commit it",
            path.display()
        )
    });
    if expected != actual {
        panic!(
            "the screen {name} differs from {}:\n{}\nRun the tests with VISION_UPDATE_SNAPSHOTS=1 to write the new screen, if it is right.",
            path.display(),
            diff(&expected, actual)
        );
    }
}

/// The lines that differ, with their numbers.
pub fn diff(expected: &str, actual: &str) -> String {
    let a: Vec<&str> = expected.lines().collect();
    let b: Vec<&str> = actual.lines().collect();
    let mut out = String::new();
    for i in 0..a.len().max(b.len()) {
        let (x, y) = (a.get(i).copied(), b.get(i).copied());
        if x != y {
            out.push_str(&format!(
                "line {}:\n  stored: {}\n  now:    {}\n",
                i + 1,
                x.unwrap_or("(no line)"),
                y.unwrap_or("(no line)")
            ));
        }
    }
    out
}
