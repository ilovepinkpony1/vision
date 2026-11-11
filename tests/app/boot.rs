//! The loader with a clock that the test sets: the shortest stay, the key that skips, the limit
//! of the tracker stage, a failed stage, and a home that is not set up.

use vision::app::model::{
    Screen, SetupError, StageState, STAGE_ENGINE, STAGE_LEDGER, STAGE_TRACKER,
};
use vision::app::msg::{Effect, Key, Msg};

use super::support::*;

const SIZE: (u16, u16) = (110, 32);

#[test]
fn the_loader_stays_until_the_android_has_appeared() {
    let mut app = booting(SIZE);
    send(&mut app, 0.1, Msg::Setup(Ok(setup())));
    send(&mut app, 0.2, Msg::Local(Ok(local(Vec::new(), Vec::new()))));
    send(&mut app, 0.3, Msg::Queue(Ok(queue())));
    assert!(app.boot.stages.iter().all(|s| s.settled()));
    send(&mut app, 0.8, Msg::Tick);
    assert_eq!(app.screen, Screen::Boot, "the picture still appears");
    send(&mut app, 0.95, Msg::Tick);
    assert_eq!(app.screen, Screen::Home);
}

#[test]
fn the_loader_waits_for_the_stages_after_the_android() {
    let mut app = booting(SIZE);
    send(&mut app, 0.1, Msg::Setup(Ok(setup())));
    send(&mut app, 2.0, Msg::Tick);
    assert_eq!(app.screen, Screen::Boot);
    send(&mut app, 2.1, Msg::Local(Ok(local(Vec::new(), Vec::new()))));
    assert_eq!(app.screen, Screen::Boot, "the tracker stage still runs");
    send(&mut app, 2.2, Msg::Queue(Ok(queue())));
    assert_eq!(app.screen, Screen::Home);
}

#[test]
fn a_key_skips_the_loader() {
    let mut app = booting(SIZE);
    // A key before the home folder is read: the loader leaves as soon as it is read.
    send(&mut app, 0.05, Msg::Key(Key::Char('x')));
    assert_eq!(app.screen, Screen::Boot);
    send(&mut app, 0.1, Msg::Setup(Ok(setup())));
    assert_eq!(app.screen, Screen::Home);
    // The other stages go on, and Home fills when they end.
    assert_eq!(app.boot.stages[STAGE_LEDGER].state, StageState::Running);
    send(
        &mut app,
        0.3,
        Msg::Local(Ok(local(two_tickets(), Vec::new()))),
    );
    assert_eq!(app.held().len(), 2);
    assert!(app.engine.started);
}

#[test]
fn the_loader_stops_waiting_for_the_tracker_after_10_seconds() {
    let mut app = booting(SIZE);
    send(&mut app, 0.1, Msg::Setup(Ok(setup())));
    send(&mut app, 0.2, Msg::Local(Ok(local(Vec::new(), Vec::new()))));
    assert!(app.boot.stages[STAGE_ENGINE].settled());
    send(&mut app, 9.9, Msg::Tick);
    assert_eq!(app.screen, Screen::Boot);
    send(&mut app, 10.2, Msg::Tick);
    assert_eq!(
        app.screen,
        Screen::Home,
        "the tracker stage started at 0.1 s"
    );
    assert!(app.queue.loading());
    // The queue arrives later, and Home shows it.
    send(&mut app, 15.0, Msg::Queue(Ok(queue())));
    assert_eq!(app.queue_rows().len(), 2);
    assert!(screen(&app).contains("DEMO-15"));
}

#[test]
fn a_failed_stage_shows_its_reason_and_the_loader_goes_on() {
    let mut app = booting(SIZE);
    send(&mut app, 0.1, Msg::Setup(Ok(setup())));
    let fx = send(
        &mut app,
        0.2,
        Msg::Local(Err("could not open the ledger: database is locked".into())),
    );
    assert_eq!(
        app.boot.stages[STAGE_LEDGER].state,
        StageState::Failed("could not open the ledger: database is locked".into())
    );
    assert!(fx.contains(&Effect::StartWorker), "the engine still starts");
    send(
        &mut app,
        0.3,
        Msg::Queue(Err("error: bin/observe failed".into())),
    );
    assert_eq!(
        app.boot.stages[STAGE_TRACKER].state,
        StageState::Failed("bin/observe failed".into())
    );
    send(&mut app, 1.0, Msg::Tick);
    assert_eq!(app.screen, Screen::Home);
}

#[test]
fn a_home_that_is_not_set_up_stops_the_loader_and_q_leaves() {
    let mut app = booting(SIZE);
    let fx = send(
        &mut app,
        0.1,
        Msg::Setup(Err(SetupError {
            reason: "not set up".into(),
            what: "Vision is not set up at ~/.vision: config.toml is missing.".into(),
            fix: "Run vision init to make the home folder, then open Vision again.".into(),
        })),
    );
    assert!(fx.is_empty(), "no other stage starts: {fx:?}");
    send(&mut app, 5.0, Msg::Key(Key::Enter));
    send(&mut app, 30.0, Msg::Tick);
    assert_eq!(app.screen, Screen::Boot, "no key skips it");
    assert!(!app.engine.started);
    let text = screen(&app);
    assert!(text.contains("config.toml is missing"), "{text}");
    assert!(text.contains("Run vision init"), "{text}");
    let fx = send(&mut app, 31.0, Msg::Key(Key::Char('q')));
    assert_eq!(fx, vec![Effect::Quit]);
}

#[test]
fn the_loader_animates_and_home_waits_quietly() {
    let app = booting(SIZE);
    assert!(app.animating());
    let app = home(SIZE);
    assert!(
        !app.animating(),
        "nothing moves on Home after the first pass"
    );
}
