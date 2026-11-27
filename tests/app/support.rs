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
use vision::app::hits::HitMap;
use vision::app::model::{App, Clock, ContinueView, Held, Local, Setup, Week};
use vision::app::msg::{ChildResult, Effect, JobDone, Msg};
use vision::app::update::{start, update};
use vision::app::view::{self, theme::ColorMode};
use vision::ledger::LimitReading;
use vision::pipeline::commands::PickRow;
use vision::pipeline::facts::{StateFacts, TurnFact, WaitFacts};
use vision::pipeline::resume::{Found, Head};
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

pub const STEPS: [&str; 13] = [
    "refine",
    "plan",
    "plan-ready",
    "build",
    "review",
    "open-pr",
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
        sections: Vec::new(),
        merge_steps: vec!["ready-to-merge".to_string()],
    }
}

/// A row of the status view.
pub fn view(key: &str, title: &str, step: &str, now: &str) -> TicketView {
    TicketView {
        key: key.into(),
        title: (!title.is_empty()).then(|| title.to_string()),
        step: Some(step.into()),
        kind: Some("turn".into()),
        ticket_kind: None,
        now: now.into(),
        works: None,
        flags: Vec::new(),
        needs: BTreeMap::new(),
        marks: Marks::default(),
        cost_usd: 0.0,
        pr: None,
        pr_url: None,
        issue_url: Some(format!("https://jira.example/browse/{key}")),
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

/// The reason of the suggestion of [`continue_view`].
pub const REASON: &str = "the pull request #7 is open and ready, and 2 review comments wait";

/// What the read of the continue card gives for a ticket at the step `stored` of the demo:
/// the steps with no end step, and the suggestion `suggest` of the rule 3, or no rule that
/// holds. The ticket is stopped, else paused.
pub fn continue_view(
    key: &str,
    stored: &str,
    suggest: Option<&str>,
    stopped: bool,
) -> ContinueView {
    ContinueView {
        key: key.into(),
        stored: stored.into(),
        steps: STEPS
            .iter()
            .filter(|s| **s != "done")
            .map(|s| s.to_string())
            .collect(),
        rules: true,
        found: Some(match suggest {
            Some(step) => Found::Step {
                rule: 3,
                step: step.into(),
                reason: REASON.into(),
                back: None,
            },
            None => Found::Nothing("no rule holds".into()),
        }),
        head: Head::None,
        pr: Some(7),
        clone: Some("~/clones/demo-1".into()),
        stopped,
        paused: !stopped,
        needs_you: false,
        facts: FACTS.iter().map(|f| f.to_string()).collect(),
        back: None,
    }
}

/// The facts of [`continue_view`] that Vision read, in words.
pub const FACTS: [&str; 5] = [
    "the tracker status is In Review",
    "the pull request #7 is open and ready",
    "the review state is comments and 2 review comments wait",
    "CI is green at 3f2a1b4c5d6e",
    "the branch of the clone is at the head of the pull request",
];

/// Hands the read of the continue card of `key` to the update.
pub fn read_resume(
    app: &mut App,
    at: f64,
    key: &str,
    view: Result<ContinueView, String>,
) -> Vec<Effect> {
    send(
        app,
        at,
        Msg::Resume {
            key: key.into(),
            view: Box::new(view),
        },
    )
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
        jobs: Vec::new(),
        jobs_ended: 0,
        panes: Vec::new(),
        resets: [Some("2026-11-16T18:00:00Z".into()), None],
    }
}

/// The two tickets in progress of the samples.
pub fn two_tickets() -> Vec<Held> {
    let mut ci = waiting(
        "DEMO-7",
        "Show the totals under the table of orders",
        "ci",
        "waits: the watched value is none",
    );
    // DEMO-7 has a pull request with a page, so Home shows its GitHub mark. DEMO-12 has none.
    ci.view.pr = Some(7);
    ci.view.pr_url = Some(PR_URL.into());
    vec![
        working(
            "DEMO-12",
            "Export the report of the month as a CSV file",
            "build",
            "builder",
            6,
        ),
        ci,
    ]
}

/// Opens the tab "To take", and the tracker answers the load of the visit with the same queue.
pub fn open_take(app: &mut App, at: f64) {
    send(app, at, Msg::Key(vision::app::msg::Key::Char('2')));
    let rows = app.queue.data.clone().unwrap_or_default();
    send(app, at, Msg::Queue(Ok(rows)));
}

/// The page of the pull request of DEMO-7.
pub const PR_URL: &str = "https://github.com/demo/calc/pull/7";

pub fn queue() -> Vec<PickRow> {
    vec![
        PickRow {
            key: "DEMO-15".into(),
            title: "Keep the filter of the list when the page loads again".into(),
            status: "To Do".into(),
            eligible: true,
            reason: String::new(),
            taken: None,
            branch: None,
            pr: None,
        },
        PickRow {
            key: "DEMO-9".into(),
            title: "Fix the date format in the weekly email".into(),
            status: "To Do".into(),
            eligible: false,
            reason: "it already has an open pull request".into(),
            taken: None,
            branch: None,
            pr: None,
        },
    ]
}

/// Hands a message to the update at a time.
/// A ticket of the queue that is in progress outside Vision: a branch with its key, and an
/// open pull request.
pub fn outside_row() -> PickRow {
    PickRow {
        key: "DEMO-21".into(),
        title: "Export the report as a CSV file".into(),
        status: "In Review".into(),
        eligible: false,
        reason: "the branch feature/demo-21-export already has DEMO-21 in its name".into(),
        taken: None,
        branch: Some("feature/demo-21-export".into()),
        pr: Some(31),
    }
}

/// The queue of [`queue`] with [`outside_row`] at its end.
pub fn queue_with_outside() -> Vec<PickRow> {
    let mut rows = queue();
    rows.push(outside_row());
    rows
}

/// The read of the start card of [`outside_row`]: no stored step, no clone, and the suggestion
/// `suggest` of the rule 4, or no rule that holds.
pub fn outside_view(suggest: Option<&str>) -> ContinueView {
    let mut view = continue_view("DEMO-21", "", None, false);
    view.paused = false;
    view.clone = None;
    view.pr = Some(31);
    view.found = Some(match suggest {
        Some(step) => Found::Step {
            rule: 4,
            step: step.into(),
            reason: "the pull request #31 is open and ready".into(),
            back: None,
        },
        None => Found::Nothing("no rule holds".into()),
    });
    view.facts = [
        "the tracker status is In Review",
        "the pull request #31 is open and ready",
        "the review state is waiting and no review comment waits",
        "CI is green at 3f2a1b4c5d6e",
    ]
    .map(String::from)
    .to_vec();
    view
}

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
    home_from(size, setup(), local, queue)
}

/// Home after the loader, with the setup `setup`.
pub fn home_from(size: (u16, u16), setup: Setup, local: Local, queue: Option<Vec<PickRow>>) -> App {
    let mut app = booting(size);
    send(&mut app, 0.1, Msg::Setup(Ok(setup)));
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

/// The statuses of `app.sections` of [`sectioned_home`].
pub const SECTIONS: [&str; 3] = ["In Progress", "In Review", "In Testing"];

/// A queue of twelve tickets with five statuses, in the order of the tracker: three statuses
/// of [`SECTIONS`], and `To Do` and `Backlog`, which are not in it.
pub fn sectioned_queue() -> Vec<PickRow> {
    [
        (
            "DEMO-30",
            "To Do",
            "Show the name of the shop in the subject of the email",
        ),
        (
            "DEMO-31",
            "In Testing",
            "Count the visits of each page once a day",
        ),
        (
            "DEMO-32",
            "In Review",
            "Sort the orders by the date of the payment",
        ),
        (
            "DEMO-33",
            "In Progress",
            "Keep the last search of the person",
        ),
        (
            "DEMO-34",
            "In Testing",
            "Hide the button of the export for a guest",
        ),
        (
            "DEMO-35",
            "Backlog",
            "Let the person pick the first day of the week",
        ),
        (
            "DEMO-36",
            "In Review",
            "Show the totals of the month in the footer",
        ),
        (
            "DEMO-37",
            "In Progress",
            "Send the weekly email on the day that the person picks",
        ),
        (
            "DEMO-38",
            "To Do",
            "Add a link to the help page in the menu",
        ),
        (
            "DEMO-39",
            "In Review",
            "Fix the width of the table on a small screen",
        ),
        (
            "DEMO-40",
            "In Testing",
            "Load the next page of the list when the person scrolls",
        ),
        (
            "DEMO-41",
            "In Progress",
            "Mark the orders that came from a coupon",
        ),
    ]
    .into_iter()
    .map(|(key, status, title)| PickRow {
        key: key.into(),
        title: title.into(),
        status: status.into(),
        eligible: true,
        reason: String::new(),
        taken: None,
        branch: None,
        pr: None,
    })
    .collect()
}

/// Home with the two tickets in progress, `app.sections` of [`SECTIONS`], and
/// [`sectioned_queue`].
pub fn sectioned_home(size: (u16, u16)) -> App {
    let setup = Setup {
        sections: SECTIONS.iter().map(|s| s.to_string()).collect(),
        ..setup()
    };
    home_from(
        size,
        setup,
        local(two_tickets(), Vec::new()),
        Some(sectioned_queue()),
    )
}

/// [`sectioned_home`] with eight more tickets in the section Backlog, DEMO-50 to DEMO-57, so
/// that the queue is longer than the box of a window of the minimum size.
pub fn long_sectioned_home(size: (u16, u16)) -> App {
    let setup = Setup {
        sections: SECTIONS.iter().map(|s| s.to_string()).collect(),
        ..setup()
    };
    let mut queue = sectioned_queue();
    for n in 50..58 {
        queue.push(PickRow {
            key: format!("DEMO-{n}"),
            title: format!("Clean up the old report number {n}"),
            status: "Backlog".into(),
            eligible: true,
            reason: String::new(),
            taken: None,
            branch: None,
            pr: None,
        });
    }
    home_from(size, setup, local(two_tickets(), Vec::new()), Some(queue))
}

/// Draws the model into a test backend of its size.
pub fn render(app: &App) -> Buffer {
    let mut terminal = Terminal::new(TestBackend::new(app.size.0, app.size.1)).unwrap();
    terminal
        .draw(|frame| {
            view::draw(app, frame);
        })
        .unwrap();
    terminal.backend().buffer().clone()
}

/// Draws the model as the runtime does: the model keeps the parts of the frame that the mouse
/// can hit, and the change is drawn.
pub fn draw(app: &mut App) -> Buffer {
    let mut terminal = Terminal::new(TestBackend::new(app.size.0, app.size.1)).unwrap();
    let mut hits = HitMap::default();
    terminal
        .draw(|frame| hits = view::draw(app, frame))
        .unwrap();
    app.hits = hits;
    app.dirty = false;
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
