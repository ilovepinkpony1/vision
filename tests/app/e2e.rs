//! The app against a temporary home with the demo adapter and the demo world, as the end-to-end
//! tests make them, with the fake `claude`. The real loaders fill the model in their threads;
//! the take runs through the built binary and puts the ticket in progress with its title; one
//! pass through the worker moves the ticket on. The live view follows a running turn: its model
//! names the agent that works, its feed gets the events of the stream file, a text of `t`
//! reaches the turn, and the state moves on after the turn.

use std::process::{Command, Stdio};
use std::time::Duration;

use serde_json::json;
use vision::app::model::{App, Card, Screen};
use vision::app::msg::{Key, Msg};
use vision::app::now::Kind;
use vision::app::pager::{PagerKind, PagerState};
use vision::app::runtime::Runtime;
use vision::app::view::theme::ColorMode;
use vision::app::worker::WorkerConfig;

use super::pipeline_support::{ends_with, fake_claude, result, simple_scenarios, World, KEY, PLAN};

const LIMIT: Duration = Duration::from_secs(120);

fn runtime(world: &World) -> Runtime {
    let mut env = vec![
        ("PATH".into(), std::env::var_os("PATH").unwrap_or_default()),
        ("HOME".into(), world.user_home.clone().into_os_string()),
        ("VISION_CLAUDE_BIN".into(), fake_claude().into_os_string()),
    ];
    if let Some(tmp) = std::env::var_os("TMPDIR") {
        env.push(("TMPDIR".into(), tmp));
    }
    let worker = WorkerConfig {
        exe: env!("CARGO_BIN_EXE_vision").into(),
        home: world.vision.clone(),
        env: Some(env),
    };
    Runtime::new(
        world.ctx().paths.clone(),
        worker,
        (110, 32),
        ColorMode::True,
    )
}

fn idle(app: &App) -> bool {
    app.engine.running.is_none() && app.engine.queue.is_empty()
}

#[test]
fn the_app_takes_a_ticket_of_the_demo_world_and_moves_it_on() {
    let world = World::new();
    simple_scenarios(&world);
    let mut rt = runtime(&world);
    rt.start();

    // The real loaders fill the model, and the first pass of the engine ends.
    assert!(
        rt.run_until(LIMIT, |app| app.screen == Screen::Home
            && app.local.data.is_some()
            && app.queue.data.is_some()
            && idle(app)),
        "the loader never ended: {:?}",
        rt.app.boot
    );
    let row = rt
        .app
        .queue_rows()
        .into_iter()
        .find(|r| r.key == KEY)
        .cloned()
        .expect("the queue has the demo ticket");
    assert!(row.eligible, "{row:?}");
    assert!(!row.title.is_empty());
    assert!(rt.app.held().is_empty());

    // Select the demo ticket and open its start card.
    while rt.app.home.selected.as_deref() != Some(KEY) {
        let before = rt.app.home.selected.clone();
        rt.handle(Msg::Key(Key::Down));
        assert_ne!(
            before, rt.app.home.selected,
            "the ticket is not in the list"
        );
    }
    rt.handle(Msg::Key(Key::Enter));
    assert!(rt.run_until(LIMIT, |app| matches!(
        &app.card,
        Some(Card::Start(card)) if card.info.is_some()
    )));
    let Some(Card::Start(card)) = &rt.app.card else {
        panic!()
    };
    let info = card
        .info
        .clone()
        .unwrap()
        .expect("the start card reads its facts");
    assert_eq!(info.branch, "vision/DEMO-12");
    assert_eq!(info.first.step, "refine");
    assert_eq!(info.first.agent.as_deref(), Some("scout"));
    assert!(info.clone.is_ok(), "{info:?}");

    // Enter takes the ticket through the built binary.
    rt.handle(Msg::Key(Key::Enter));
    assert!(
        rt.run_until(LIMIT, |app| app.card.is_none()
            && app.held_ticket(KEY).is_some()),
        "the take did not end: {:?} {:?}",
        rt.app.card,
        rt.app.notice
    );
    let held = rt.app.held_ticket(KEY).unwrap().clone();
    assert_eq!(held.view.title.as_deref(), Some(row.title.as_str()));
    assert_eq!(
        world.row().unwrap().title.as_deref(),
        Some(row.title.as_str())
    );
    let status = world.run(&["status", "--json"]);
    status.expect_code(0);
    assert_eq!(status.json()["tickets"][0]["title"], row.title.as_str());
    assert_eq!(rt.app.home.selected.as_deref(), Some(KEY));
    assert!(rt
        .app
        .notice
        .as_ref()
        .unwrap()
        .text
        .starts_with("Took DEMO-12."));
    assert!(
        rt.run_until(LIMIT, |app| app.queue_rows().iter().all(|r| r.key != KEY)),
        "the queue box no longer lists the taken ticket"
    );

    // The pass after the take starts the first turn, and the model moves on.
    let before = held.view.now.clone();
    assert!(
        rt.run_until(LIMIT, |app| app
            .held_ticket(KEY)
            .is_some_and(|h| h.view.now != before)),
        "no pass moved the ticket on: {before}"
    );
    assert!(world.turns_of("refine") >= 1, "{:?}", world.turns());
    world.settle();
}

#[test]
fn vision_with_no_arguments_and_no_terminal_prints_the_help() {
    let home = tempfile::tempdir().unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_vision"))
        .env("VISION_HOME", home.path().join("vision"))
        .env("HOME", home.path())
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("Usage: vision <COMMAND>"), "{stderr}");
    assert!(!home.path().join("vision").exists());
}

/// The data of the live view, when it loaded.
fn live_data(app: &App) -> Option<&vision::app::live::TicketData> {
    app.live.as_ref()?.data.data.as_ref()
}

/// True when an item of the feed holds this text.
fn feed_has(app: &App, needle: &str) -> bool {
    app.live
        .as_ref()
        .is_some_and(|l| l.feed.items().iter().any(|i| i.text.contains(needle)))
}

#[test]
fn the_live_view_follows_a_running_turn_and_tells_it_something() {
    let world = World::new();
    // The turn of scout waits for a message, so the view can see it work.
    world.scenario(
        "scout",
        "refine",
        json!([[
            {"do": "init"},
            {"do": "text", "text": "I read the ticket now."},
            {"do": "tool", "name": "Read", "input": {"file_path": "README.md"}},
            {"do": "wait_input", "timeout": 60},
            {"do": "result", "cost": 0.25, "structured_output": result(
                "Refined.",
                &[("refine.md", "Research."), ("description.md", "Add subtract(a, b).")]
            )}
        ]]),
    );
    world.scenario(
        "builder",
        "plan",
        json!([ends_with(result("Planned.", &[("plan.md", PLAN)]))]),
    );
    world.run(&["take", KEY]).expect_code(0);
    let mut rt = runtime(&world);
    rt.start();

    // The first pass starts the turn of scout.
    assert!(
        rt.run_until(LIMIT, |app| app.screen == Screen::Home
            && app.held_ticket(KEY).is_some_and(|h| h.facts.work.is_some())),
        "no turn started: {:?}",
        rt.app.held_ticket(KEY).map(|h| &h.view.now)
    );
    while rt.app.home.selected.as_deref() != Some(KEY) {
        let before = rt.app.home.selected.clone();
        rt.handle(Msg::Key(Key::Down));
        assert_ne!(
            before, rt.app.home.selected,
            "the ticket is not in the list"
        );
    }

    // Enter opens the live view, its loader, and the reader of the stream files.
    rt.handle(Msg::Key(Key::Enter));
    assert_eq!(rt.app.screen, Screen::Live { key: KEY.into() });
    assert_eq!(rt.following(), Some(KEY));
    assert!(
        rt.run_until(LIMIT, |app| live_data(app)
            .and_then(|d| d.facts.work.as_ref())
            .is_some_and(|w| w.agent == "scout")),
        "the live view never saw scout work"
    );
    let data = live_data(&rt.app).unwrap().clone();
    let now = rt.app.now_of(&data.view, &data.facts);
    assert_eq!(now.kind, Kind::Agent);
    assert_eq!(now.agent.as_deref(), Some("scout"));
    assert!(now.title.starts_with("scout works"), "{}", now.title);
    assert_eq!(data.view.step.as_deref(), Some("refine"));
    assert!(data.team.iter().any(|m| m.agent == "scout"));

    // The feed gets the events of the stream file of the turn.
    assert!(
        rt.run_until(LIMIT, |app| feed_has(app, "I read the ticket now.")
            && feed_has(app, "README.md")),
        "the feed never got the stream: {:?}",
        rt.app.live.as_ref().map(|l| l.feed.items().len())
    );

    // `t` sends a text into the running turn.
    rt.handle(Msg::Key(Key::Char('t')));
    for c in "use the old export".chars() {
        rt.handle(Msg::Key(Key::Char(c)));
    }
    rt.handle(Msg::Key(Key::Enter));
    assert!(
        rt.run_until(LIMIT, |app| app.notice.as_ref().is_some_and(|n| n
            .text
            .starts_with("Sent to the running turn of DEMO-12."))),
        "the text did not reach the turn: {:?}",
        rt.app.notice
    );
    assert!(
        rt.run_until(LIMIT, |app| feed_has(app, "got: use the old export")),
        "the feed never showed the answer of the turn"
    );

    // The turn ends. The engine of the app collects it; the ticks of the test move the ticket
    // on at once, where the engine would wait for its next pass. The view sees each change.
    assert!(
        rt.run_until(LIMIT, |app| live_data(app)
            .is_some_and(|d| d.facts.work.is_none())),
        "the turn never ended"
    );
    world.tick_to("plan");
    assert!(
        rt.run_until(LIMIT, |app| live_data(app).is_some_and(|d| d
            .view
            .step
            .as_deref()
            == Some("plan"))),
        "the view did not see the step plan: {:?}",
        live_data(&rt.app).map(|d| &d.view.now)
    );
    assert!(
        rt.run_until(LIMIT, |app| feed_has(app, "the turn of scout ended")),
        "the feed never got the end of the turn from the ledger"
    );

    // `l` shows the log, with the turn of scout in it.
    rt.handle(Msg::Key(Key::Char('l')));
    let log = rt.app.live.as_ref().unwrap().pagers.last().unwrap().clone();
    assert_eq!(log.kind, PagerKind::Log);
    assert_eq!(log.state, PagerState::Ready);
    assert!(log.lines.iter().any(|l| l.turn.is_some()));
    rt.handle(Msg::Key(Key::Esc));

    // When the plan is there, `p` shows it.
    world.tick_to("plan-ready");
    assert!(
        rt.run_until(LIMIT, |app| live_data(app).is_some_and(|d| d
            .view
            .step
            .as_deref()
            == Some("plan-ready"))),
        "the view did not see the plan get ready"
    );
    rt.handle(Msg::Key(Key::Char('p')));
    assert!(
        rt.run_until(LIMIT, |app| app
            .live
            .as_ref()
            .and_then(|l| l.pagers.last())
            .is_some_and(|p| p.state == PagerState::Ready)),
        "the plan never loaded"
    );
    let plan = rt.app.live.as_ref().unwrap().pagers.last().unwrap().clone();
    assert_eq!(plan.kind, PagerKind::Plan);
    assert!(plan
        .lines
        .iter()
        .any(|l| l.spans.iter().any(|s| s.text.contains("Plan of DEMO-12"))));
    rt.handle(Msg::Key(Key::Esc));

    // Esc goes back to Home and stops the reader.
    rt.handle(Msg::Key(Key::Esc));
    assert_eq!(rt.app.screen, Screen::Home);
    assert_eq!(rt.following(), None);
    world.settle();
}
