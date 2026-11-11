//! The app against a temporary home with the demo adapter and the demo world, as the end-to-end
//! tests make them, with the fake `claude`. The real loaders fill the model in their threads;
//! the take runs through the built binary and puts the ticket in progress with its title; one
//! pass through the worker moves the ticket on.

use std::process::{Command, Stdio};
use std::time::Duration;

use vision::app::model::{App, Card, Screen};
use vision::app::msg::{Key, Msg};
use vision::app::runtime::Runtime;
use vision::app::view::theme::ColorMode;
use vision::app::worker::WorkerConfig;

use super::pipeline_support::{fake_claude, simple_scenarios, World, KEY};

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
