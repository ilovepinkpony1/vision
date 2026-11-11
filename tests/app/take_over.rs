//! The flow that takes over the session of a running turn, in the update: `o` on the open card
//! of a running turn runs `vision stop`, then gives the terminal to `vision open`; the card
//! after the session offers to continue, which runs `vision take` and then `vision retry`. A
//! failed step stops the flow and says what is true now. With text snapshots of each state.

use vision::app::chain::{Check, CheckFacts, End, Owner};
use vision::app::model::{App, Card};
use vision::app::msg::{ChildResult, Effect, Key, Msg};

use super::live_support::working_live;
use super::need_support::*;
use super::support::*;

const SIZE: (u16, u16) = (110, 32);

fn key(app: &mut App, at: f64, k: Key) -> Vec<Effect> {
    send(app, at, Msg::Key(k))
}

fn none(_: &Check) -> CheckFacts {
    CheckFacts::default()
}

fn args(list: &[&str]) -> Vec<String> {
    list.iter().map(|s| s.to_string()).collect()
}

fn session_ended(app: &mut App, at: f64, code: i32) -> Vec<Effect> {
    send(
        app,
        at,
        Msg::SessionEnded {
            key: "DEMO-12".into(),
            result: ChildResult {
                code: Some(code),
                stderr: if code == 0 {
                    String::new()
                } else {
                    "error: the session of DEMO-12 has no id\n".into()
                },
                ..ChildResult::default()
            },
        },
    )
}

#[test]
fn o_stops_the_turn_then_opens_its_session_and_enter_continues_the_ticket() {
    let mut app = working_live(SIZE);
    key(&mut app, 3.0, Key::Char('o'));
    let Some(Card::Open(card)) = &app.card else {
        panic!("no open card: {:?}", app.card)
    };
    assert_eq!(card.running.as_deref(), Some("builder"));
    let fx = key(&mut app, 3.1, Key::Char('o'));
    let started = jobs(&fx);
    assert_eq!(started.len(), 1, "{fx:?}");
    send(&mut app, 3.2, Msg::Tick);
    assert_snapshot("take_over_runs", &screen(&app));
    // A second o while the chain runs starts nothing.
    assert!(jobs(&key(&mut app, 3.3, Key::Char('o'))).is_empty());

    let done = drive(&mut app, 3.4, fx, None, none);
    assert_eq!(done.actions, [args(&["stop", "DEMO-12"])]);
    assert_eq!(done.other, [Effect::OpenSession("DEMO-12".into())]);

    // The person leaves the session: the card says that the ticket is stopped and paused.
    session_ended(&mut app, 5.0, 0);
    let Some(Card::Opened(card)) = &app.card else {
        panic!("no card after the session: {:?}", app.card)
    };
    assert_eq!(card.took_over.as_deref(), Some("builder"));
    assert_eq!(app.chain, None);
    assert_snapshot("take_over_back", &screen(&app));

    let fx = key(&mut app, 5.1, Key::Enter);
    send(&mut app, 5.2, Msg::Tick);
    assert_snapshot("take_over_continue_runs", &screen(&app));
    let done = drive(&mut app, 5.3, fx, None, none);
    assert_eq!(
        done.actions,
        [args(&["take", "DEMO-12"]), args(&["retry", "DEMO-12"])]
    );
    assert_eq!(app.card, None);
    assert_eq!(
        app.notice.as_ref().unwrap().text,
        "DEMO-12 goes on. Its next turn resumes the session that you worked in."
    );
}

#[test]
fn esc_after_the_session_keeps_the_ticket_stopped() {
    let mut app = working_live(SIZE);
    key(&mut app, 3.0, Key::Char('o'));
    let fx = key(&mut app, 3.1, Key::Char('o'));
    drive(&mut app, 3.2, fx, None, none);
    session_ended(&mut app, 5.0, 0);
    let fx = key(&mut app, 5.1, Key::Esc);
    assert!(jobs(&fx).is_empty());
    assert_eq!(app.card, None);
}

#[test]
fn a_failed_stop_stops_the_flow_and_says_that_the_turn_still_runs() {
    let mut app = working_live(SIZE);
    key(&mut app, 3.0, Key::Char('o'));
    let fx = key(&mut app, 3.1, Key::Char('o'));
    let done = drive(&mut app, 3.2, fx, Some("stop"), none);
    assert_eq!(done.actions, [args(&["stop", "DEMO-12"])]);
    assert!(done.other.is_empty(), "no session opens: {:?}", done.other);
    match app.chain.as_ref().map(|c| (&c.owner, &c.end)) {
        Some((Owner::TakeOver { key, .. }, Some(End::Failed { state, .. }))) => {
            assert_eq!(key, "DEMO-12");
            assert_eq!(state, "The turn of builder still runs.");
        }
        other => panic!("the flow did not fail: {other:?}"),
    }
    send(&mut app, 3.5, Msg::Tick);
    assert_snapshot("take_over_failed", &screen(&app));
}

#[test]
fn a_failed_retry_after_the_session_names_the_state_between_the_steps() {
    let mut app = working_live(SIZE);
    key(&mut app, 3.0, Key::Char('o'));
    let fx = key(&mut app, 3.1, Key::Char('o'));
    drive(&mut app, 3.2, fx, None, none);
    session_ended(&mut app, 5.0, 0);
    let fx = key(&mut app, 5.1, Key::Enter);
    let done = drive(&mut app, 5.2, fx, Some("retry"), none);
    assert_eq!(
        done.actions,
        [args(&["take", "DEMO-12"]), args(&["retry", "DEMO-12"])]
    );
    match app.chain.as_ref().and_then(|c| c.end.clone()) {
        Some(End::Failed { state, .. }) => assert_eq!(
            state,
            "DEMO-12 is no longer stopped, and it is still paused. Press n to continue it."
        ),
        other => panic!("the flow did not fail: {other:?}"),
    }
    // A second Enter does not start the flow again: the card shows its end.
    assert!(jobs(&key(&mut app, 5.3, Key::Enter)).is_empty());
    send(&mut app, 5.4, Msg::Tick);
    assert_snapshot("take_over_continue_failed", &screen(&app));
}

#[test]
fn a_session_that_did_not_open_continues_with_no_pause() {
    let mut app = working_live(SIZE);
    key(&mut app, 3.0, Key::Char('o'));
    let fx = key(&mut app, 3.1, Key::Char('o'));
    drive(&mut app, 3.2, fx, None, none);
    session_ended(&mut app, 5.0, 1);
    let Some(Card::Opened(card)) = &app.card else {
        panic!("no card after the session: {:?}", app.card)
    };
    assert!(card.error.is_some());
    let fx = key(&mut app, 5.1, Key::Enter);
    let done = drive(&mut app, 5.2, fx, None, none);
    assert_eq!(
        done.actions,
        [args(&["take", "DEMO-12"]), args(&["retry", "DEMO-12"])]
    );
    assert_eq!(
        app.notice.as_ref().unwrap().text,
        "DEMO-12 goes on at its step."
    );
}
