//! A text snapshot of each screen and each state of the app, drawn into the test backend of
//! ratatui: 110 by 32, and Home and the loader also at 100 by 28 and at 160 by 45.

use vision::app::model::{App, Card, SetupError};
use vision::app::msg::{ChildResult, Effect, JobDone, JobKind, Key, Msg};

use super::support::*;

const SIZE: (u16, u16) = (110, 32);

fn check(name: &str, app: &App) {
    assert_snapshot(name, &screen(app));
}

/// The loader with the home folder read at 0.3 s, and the ledger and the tracker running.
fn loader_middle(size: (u16, u16)) -> App {
    let mut app = booting(size);
    send(&mut app, 0.3, Msg::Setup(Ok(setup())));
    send(&mut app, 0.6, Msg::Tick);
    app
}

/// The loader with each stage done, a moment before it leaves.
fn loader_done(size: (u16, u16)) -> App {
    let mut app = loader_middle(size);
    send(
        &mut app,
        0.7,
        Msg::Local(Ok(local(two_tickets(), Vec::new()))),
    );
    send(&mut app, 0.8, Msg::Queue(Ok(queue())));
    send(&mut app, 0.85, Msg::Tick);
    app
}

#[test]
fn the_loader_at_the_start() {
    check("loader_start", &booting(SIZE));
}

#[test]
fn the_loader_in_the_middle() {
    check("loader_middle", &loader_middle(SIZE));
}

#[test]
fn the_loader_when_each_stage_is_done() {
    let app = loader_done(SIZE);
    assert_eq!(app.screen, vision::app::model::Screen::Boot);
    check("loader_done", &app);
}

#[test]
fn the_loader_with_a_failed_tracker_stage() {
    let mut app = loader_middle(SIZE);
    send(
        &mut app,
        0.7,
        Msg::Local(Ok(local(two_tickets(), Vec::new()))),
    );
    send(
        &mut app,
        0.8,
        Msg::Queue(Err(
            "bin/observe failed with the exit code 1: the tracker answered 503".into(),
        )),
    );
    send(&mut app, 0.85, Msg::Tick);
    check("loader_tracker_failed", &app);
}

#[test]
fn the_loader_of_a_home_that_is_not_set_up() {
    let mut app = booting(SIZE);
    send(
        &mut app,
        0.2,
        Msg::Setup(Err(SetupError {
            reason: "not set up".into(),
            what: "Vision is not set up at ~/.vision: config.toml is missing.".into(),
            fix: "Run vision init to make the home folder, then open Vision again.".into(),
        })),
    );
    send(&mut app, 1.5, Msg::Tick);
    check("loader_not_set_up", &app);
}

#[test]
fn the_loader_at_other_sizes() {
    check("loader_100x28", &loader_done((100, 28)));
    check("loader_160x45", &loader_done((160, 45)));
}

#[test]
fn home_with_nothing_in_progress() {
    check(
        "home_empty",
        &home_with(SIZE, local(Vec::new(), Vec::new()), Some(Vec::new())),
    );
}

#[test]
fn home_with_tickets() {
    check("home_tickets", &home(SIZE));
}

#[test]
fn home_with_the_bar_needs_you() {
    let tickets = vec![
        needing(
            "DEMO-12",
            "Export the report of the month as a CSV file",
            "plan",
        ),
        stopped("DEMO-3", "Rename the setting of the weekly email", "review"),
        two_tickets().remove(1),
    ];
    let needs = vec![card_need("DEMO-12"), plan_need("DEMO-20")];
    check(
        "home_needs",
        &home_with(SIZE, local(tickets, needs), Some(queue())),
    );
}

#[test]
fn home_while_the_queue_loads() {
    check(
        "home_queue_loading",
        &home_with(SIZE, local(two_tickets(), Vec::new()), None),
    );
}

#[test]
fn home_with_a_queue_that_failed() {
    let mut app = home_with(SIZE, local(two_tickets(), Vec::new()), None);
    send(
        &mut app,
        1.5,
        Msg::Queue(Err(
            "bin/observe failed with the exit code 1: the tracker answered 503 Service Unavailable, so the queue is unknown for now"
                .into(),
        )),
    );
    check("home_queue_failed", &app);
}

#[test]
fn home_with_the_engine_paused() {
    let mut app = home(SIZE);
    send(&mut app, 2.0, Msg::Key(Key::Char('p')));
    send(&mut app, 9.0, Msg::Tick);
    assert!(app.engine.paused);
    check("home_paused", &app);
}

#[test]
fn home_while_a_pass_runs() {
    let mut app = home(SIZE);
    // The minute passes, so the next pass starts at 61 s and runs for 75 s.
    let fx = send(&mut app, 61.0, Msg::Tick);
    assert!(fx
        .iter()
        .any(|e| matches!(e, Effect::Run(j) if j.kind == JobKind::Pass)));
    send(&mut app, 136.0, Msg::Tick);
    check("home_pass_runs", &app);
}

#[test]
fn home_when_vision_is_off() {
    let mut data = local(two_tickets(), Vec::new());
    data.off = true;
    check("home_off", &home_with(SIZE, data, Some(queue())));
}

#[test]
fn home_at_other_sizes() {
    check("home_100x28", &home((100, 28)));
    let tickets = vec![
        needing(
            "DEMO-12",
            "Export the report of the month as a CSV file",
            "plan",
        ),
        two_tickets().remove(0),
        two_tickets().remove(1),
    ];
    let mut tickets = tickets;
    tickets[1].view.key = "DEMO-14".into();
    check(
        "home_160x45",
        &home_with(
            (160, 45),
            local(tickets, vec![card_need("DEMO-12")]),
            Some(queue()),
        ),
    );
}

/// Home with the start card of DEMO-15 open and its facts read.
pub fn start_card() -> App {
    let mut app = home(SIZE);
    send(&mut app, 2.0, Msg::Key(Key::Down));
    send(&mut app, 2.1, Msg::Key(Key::Down));
    let fx = send(&mut app, 2.2, Msg::Key(Key::Enter));
    assert_eq!(fx, vec![Effect::LoadStartInfo("DEMO-15".into())]);
    send(
        &mut app,
        2.3,
        Msg::StartInfo {
            key: "DEMO-15".into(),
            info: Ok(vision::app::model::StartInfo {
                clone: Ok("~/clones/demo-2".into()),
                branch: "vision/DEMO-15".into(),
                base: "origin/main".into(),
                steps: STEPS.iter().map(|s| s.to_string()).collect(),
                first: vision::app::model::FirstStep {
                    step: "refine".into(),
                    kind: "turn".into(),
                    agent: Some("scout".into()),
                    model: Some("claude-fable-5-2".into()),
                    effort: Some("high".into()),
                },
                writes: "Of 5 write verbs, 4 in the mode auto and 1 in the mode ask. Vision asks you before each write that is not in the mode auto.".into(),
            }),
        },
    );
    app
}

#[test]
fn the_start_card_while_it_reads() {
    let mut app = home(SIZE);
    send(&mut app, 2.0, Msg::Key(Key::Down));
    send(&mut app, 2.1, Msg::Key(Key::Down));
    send(&mut app, 2.2, Msg::Key(Key::Enter));
    check("start_loading", &app);
}

#[test]
fn the_start_card() {
    check("start_idle", &start_card());
}

#[test]
fn the_start_card_while_the_take_runs() {
    let mut app = start_card();
    let fx = send(&mut app, 3.0, Msg::Key(Key::Enter));
    assert!(matches!(fx.as_slice(), [Effect::Run(_)]));
    check("start_running", &app);
}

#[test]
fn the_start_card_after_a_failed_take() {
    let mut app = start_card();
    let fx = send(&mut app, 3.0, Msg::Key(Key::Enter));
    let Some(Effect::Run(job)) = fx.first() else {
        panic!("no job: {fx:?}");
    };
    send(
        &mut app,
        4.0,
        Msg::JobDone(JobDone {
            id: job.id,
            kind: job.kind.clone(),
            result: ChildResult {
                code: Some(1),
                stderr: "error: DEMO-15: no clone is free:\n  /work/clones/demo-1: DEMO-12 holds it (step build)\nAdd a clone to `clones` in config.toml, or wait until a ticket ends\n".into(),
                ..ChildResult::default()
            },
        }),
    );
    check("start_failed", &app);
}

#[test]
fn the_resume_card() {
    let tickets = vec![stopped(
        "DEMO-3",
        "Rename the setting of the weekly email",
        "review",
    )];
    let mut app = home_with(SIZE, local(tickets, Vec::new()), Some(queue()));
    send(&mut app, 2.0, Msg::Key(Key::Enter));
    assert!(matches!(app.card, Some(Card::Resume(_))));
    check("resume", &app);
}

#[test]
fn the_quit_card_with_and_without_the_timer() {
    let mut app = home(SIZE);
    send(&mut app, 2.0, Msg::Key(Key::Char('q')));
    assert_eq!(app.card, Some(Card::Quit));
    check("quit", &app);
    let mut data = local(two_tickets(), Vec::new());
    data.timer = true;
    let mut app = home_with(SIZE, data, Some(queue()));
    send(&mut app, 2.0, Msg::Key(Key::Char('q')));
    check("quit_timer", &app);
}

#[test]
fn the_keys_card() {
    let mut app = home(SIZE);
    send(&mut app, 2.0, Msg::Key(Key::Char('?')));
    check("keys", &app);
}

#[test]
fn the_kill_switch_card() {
    let mut app = home(SIZE);
    send(&mut app, 2.0, Msg::Key(Key::Char('K')));
    check("kill", &app);
    let mut data = local(two_tickets(), Vec::new());
    data.off = true;
    let mut app = home_with(SIZE, data, Some(queue()));
    send(&mut app, 2.0, Msg::Key(Key::Char('K')));
    check("kill_off", &app);
}

#[test]
fn the_needs_card() {
    let needs = vec![card_need("DEMO-12"), plan_need("DEMO-20")];
    let mut app = home_with(SIZE, local(two_tickets(), needs), Some(queue()));
    send(&mut app, 2.0, Msg::Key(Key::Char('n')));
    check("needs", &app);
}

#[test]
fn the_card_of_a_long_error() {
    let mut app = home(SIZE);
    send(&mut app, 2.0, Msg::Key(Key::Char('K')));
    let fx = send(&mut app, 2.1, Msg::Key(Key::Enter));
    let Some(Effect::Run(job)) = fx.first() else {
        panic!("no job: {fx:?}");
    };
    send(&mut app, 2.2, Msg::Key(Key::Esc));
    send(
        &mut app,
        3.0,
        Msg::JobDone(JobDone {
            id: job.id,
            kind: job.kind.clone(),
            result: ChildResult {
                code: Some(1),
                stderr: "error: Vision is not set up at /work/home. Run `vision init` first\nwarning: a second line of text\n".into(),
                ..ChildResult::default()
            },
        }),
    );
    assert!(matches!(app.card, Some(Card::Error { .. })));
    check("error", &app);
}

#[test]
fn the_message_of_a_window_that_is_too_small() {
    let mut app = home((80, 24));
    send(&mut app, 2.0, Msg::Tick);
    check("too_small", &app);
}
