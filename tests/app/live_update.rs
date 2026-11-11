//! The update of the live view: each key gives the right change of the model and the right
//! effects, the cards run their actions through the queue of the worker, the line of `t` edits
//! and sends its text, the pager moves and opens the events of a turn, and the data loads again
//! each second.

use vision::app::model::{App, Card, Run, Screen};
use vision::app::msg::{Action, ChildResult, Effect, JobDone, JobKind, Key, Msg};
use vision::app::pager::{PagerKind, PagerState};

use super::live_support::*;
use super::support::*;

const SIZE: (u16, u16) = (110, 32);

/// A message to the update, with the refresh of the local data left out of the effects.
fn send(app: &mut App, at: f64, msg: Msg) -> Vec<Effect> {
    let mut fx = super::support::send(app, at, msg);
    fx.retain(|e| *e != Effect::LoadLocal);
    fx
}

/// A key, with the load of the data of each second left out of the effects: its own test
/// checks it.
fn key(app: &mut App, at: f64, k: Key) -> Vec<Effect> {
    let mut fx = send(app, at, Msg::Key(k));
    fx.retain(|e| !matches!(e, Effect::LoadTicket(_)));
    fx
}

/// The actions that the effects start.
fn actions(effects: &[Effect]) -> Vec<(u64, Action)> {
    effects
        .iter()
        .filter_map(|e| match e {
            Effect::Run(job) => match &job.kind {
                JobKind::Action(action) => Some((job.id, action.clone())),
                JobKind::Pass => None,
            },
            _ => None,
        })
        .collect()
}

/// Ends a job with this output.
fn done(app: &mut App, at: f64, id: u64, action: Action, stdout: &str) -> Vec<Effect> {
    send(
        app,
        at,
        Msg::JobDone(JobDone {
            id,
            kind: JobKind::Action(action),
            result: ChildResult {
                code: Some(0),
                stdout: stdout.into(),
                ..ChildResult::default()
            },
        }),
    )
}

fn notice(app: &App) -> String {
    app.notice
        .as_ref()
        .map(|n| n.text.clone())
        .unwrap_or_default()
}

/// A model with the live view of a resting ticket at this step, with these flags.
fn resting_live(step: &str, flags: &[(&str, &str)]) -> App {
    let mut held = resting(step);
    for (name, reason) in flags {
        held.view.flags.push(flag(name, reason));
    }
    live(SIZE, vec![held], Vec::new(), events())
}

#[test]
fn the_arrows_scroll_the_activity_and_g_goes_back_to_its_end() {
    let mut app = working_live((100, 20));
    let feed = |app: &App| app.live.as_ref().unwrap().feed.clone();
    assert!(feed(&app).follow);
    key(&mut app, 3.0, Key::Up);
    key(&mut app, 3.1, Key::Char('k'));
    assert!(!feed(&app).follow);
    assert_eq!(feed(&app).back, 2);
    key(&mut app, 3.2, Key::Char('j'));
    assert_eq!(feed(&app).back, 1);
    key(&mut app, 3.3, Key::Down);
    assert!(feed(&app).follow, "the end follows again");
    key(&mut app, 3.4, Key::PageUp);
    let back = feed(&app).back;
    assert!(back > 1, "a page moves more than one line: {back}");
    key(&mut app, 3.5, Key::PageDown);
    assert_eq!(feed(&app).back, 0);
    key(&mut app, 3.6, Key::Up);
    key(&mut app, 3.7, Key::Char('G'));
    assert!(feed(&app).follow);
    key(&mut app, 3.8, Key::Up);
    key(&mut app, 3.9, Key::End);
    assert!(feed(&app).follow);
}

#[test]
fn new_lines_keep_a_scrolled_feed_in_place() {
    let mut app = working_live((100, 20));
    key(&mut app, 3.0, Key::Up);
    let back = app.live.as_ref().unwrap().feed.back;
    let mut batch = batches(vec![(1520, events().remove(1).1)]).remove(0);
    batch.at = at(1520);
    send(
        &mut app,
        3.1,
        Msg::Stream {
            key: "DEMO-12".into(),
            batch,
        },
    );
    assert_eq!(app.live.as_ref().unwrap().feed.back, back + 1);
    // The events of another ticket do not reach the feed.
    let before = app.live.as_ref().unwrap().feed.len();
    send(
        &mut app,
        3.2,
        Msg::Stream {
            key: "DEMO-7".into(),
            batch: batches(events()).remove(2),
        },
    );
    assert_eq!(app.live.as_ref().unwrap().feed.len(), before);
}

#[test]
fn the_data_loads_again_each_second_while_the_view_is_open() {
    let mut app = working_live(SIZE);
    // The data came at 2.1: no load before 3.1.
    let fx = send(&mut app, 3.0, Msg::Tick);
    assert!(!fx.contains(&Effect::LoadTicket("DEMO-12".into())));
    let fx = send(&mut app, 3.2, Msg::Tick);
    assert!(fx.contains(&Effect::LoadTicket("DEMO-12".into())));
    // While it loads, no second load starts.
    let fx = send(&mut app, 4.5, Msg::Tick);
    assert!(!fx.contains(&Effect::LoadTicket("DEMO-12".into())));
    // The data of another ticket changes nothing.
    let other = data_of(&resting("done"), &[]);
    send(
        &mut app,
        4.6,
        Msg::Ticket {
            key: "DEMO-7".into(),
            data: Box::new(Ok(other)),
        },
    );
    assert!(app.live.as_ref().unwrap().data.loading());
    // A failed load keeps the data and shows the error.
    send(
        &mut app,
        4.7,
        Msg::Ticket {
            key: "DEMO-12".into(),
            data: Box::new(Err("the ledger is busy".into())),
        },
    );
    let live = app.live.as_ref().unwrap();
    assert!(live.data.data.is_some());
    assert_eq!(live.data.error.as_deref(), Some("the ledger is busy"));
    // Home does not load the ticket.
    key(&mut app, 4.8, Key::Esc);
    let fx = send(&mut app, 9.0, Msg::Tick);
    assert!(!fx.iter().any(|e| matches!(e, Effect::LoadTicket(_))));
}

#[test]
fn n_lists_the_needs_or_says_that_nothing_waits() {
    let mut app = working_live(SIZE);
    key(&mut app, 3.0, Key::Char('n'));
    assert_eq!(app.card, None);
    assert_eq!(notice(&app), "Nothing waits for you.");

    let held = needing(
        "DEMO-12",
        "Export the report of the month as a CSV file",
        "build",
    );
    let mut app = live(SIZE, vec![held], vec![card_need("DEMO-12")], events());
    key(&mut app, 3.0, Key::Char('n'));
    assert_eq!(app.card, Some(Card::Needs));
    key(&mut app, 3.1, Key::Esc);
    assert_eq!(app.card, None);
    assert!(matches!(app.screen, Screen::Live { .. }));
}

#[test]
fn x_opens_the_stop_card_and_enter_runs_stop() {
    let mut app = working_live(SIZE);
    key(&mut app, 3.0, Key::Char('x'));
    let Some(Card::Stop(card)) = &app.card else {
        panic!("no stop card: {:?}", app.card);
    };
    assert_eq!(card.key, "DEMO-12");
    assert_eq!(card.agent.as_deref(), Some("builder"));
    let fx = key(&mut app, 3.1, Key::Enter);
    let started = actions(&fx);
    assert_eq!(
        started.iter().map(|a| a.1.clone()).collect::<Vec<_>>(),
        vec![Action::Stop {
            key: "DEMO-12".into()
        }]
    );
    assert_eq!(started[0].1.args(), ["stop", "DEMO-12"]);
    // A second Enter while it runs does nothing.
    assert!(actions(&key(&mut app, 3.2, Key::Enter)).is_empty());
    let (id, action) = started[0].clone();
    done(&mut app, 3.3, id, action, "");
    assert_eq!(app.card, None);
    assert_eq!(
        notice(&app),
        "Stopped DEMO-12. Its branch stays in its clone. Press Enter to start it again."
    );
    assert!(matches!(app.screen, Screen::Live { .. }), "the view stays");

    // Esc closes the card with no action.
    let mut app = working_live(SIZE);
    key(&mut app, 3.0, Key::Char('x'));
    assert!(actions(&key(&mut app, 3.1, Key::Esc)).is_empty());
    assert_eq!(app.card, None);
}

#[test]
fn x_on_a_stopped_or_done_ticket_only_says_why() {
    let mut app = resting_live("review", &[("stopped", "you stopped it")]);
    key(&mut app, 3.0, Key::Char('x'));
    assert_eq!(app.card, None);
    assert_eq!(
        notice(&app),
        "DEMO-12 is already stopped. Press Enter to start it again."
    );

    let held = resting("done");
    let mut app = live(SIZE, vec![held.clone()], Vec::new(), Vec::new());
    let mut data = data_of(&held, &[]);
    data.facts.finished = true;
    send(
        &mut app,
        2.6,
        Msg::Ticket {
            key: "DEMO-12".into(),
            data: Box::new(Ok(data)),
        },
    );
    key(&mut app, 3.0, Key::Char('x'));
    assert_eq!(
        notice(&app),
        "DEMO-12 is done, so there is nothing to stop."
    );
    key(&mut app, 3.1, Key::Char('t'));
    assert_eq!(app.live.as_ref().unwrap().tell, None);
    assert_eq!(notice(&app), "DEMO-12 is done, so no turn reads a message.");
    key(&mut app, 3.2, Key::Char('o'));
    assert_eq!(
        notice(&app),
        "DEMO-12 is not in progress, so its session does not open here."
    );
}

#[test]
fn r_retries_a_ticket_that_waits_for_the_person() {
    let mut app = working_live(SIZE);
    key(&mut app, 3.0, Key::Char('r'));
    assert_eq!(app.card, None);
    assert_eq!(
        notice(&app),
        "DEMO-12 does not wait for you, so there is nothing to retry."
    );

    let mut app = resting_live("build", &[("needs-you", "the proof did not pass")]);
    key(&mut app, 3.0, Key::Char('r'));
    let Some(Card::Retry(card)) = &app.card else {
        panic!("no retry card: {:?}", app.card);
    };
    assert!(!card.paused);
    assert_eq!(card.reason.as_deref(), Some("the proof did not pass"));
    let fx = key(&mut app, 3.1, Key::Enter);
    let started = actions(&fx);
    assert_eq!(
        started[0].1,
        Action::Retry {
            key: "DEMO-12".into()
        }
    );
    let (id, action) = started[0].clone();
    let fx = done(&mut app, 3.2, id, action, "");
    assert_eq!(notice(&app), "DEMO-12 goes on. A pass runs now.");
    assert!(
        fx.iter()
            .any(|e| matches!(e, Effect::Run(job) if job.kind == JobKind::Pass)),
        "a pass runs after the action: {fx:?}"
    );

    let mut app = resting_live("build", &[("paused", "you took over the session")]);
    key(&mut app, 3.0, Key::Char('r'));
    let Some(Card::Retry(card)) = &app.card else {
        panic!("no retry card: {:?}", app.card);
    };
    assert!(card.paused);
}

#[test]
fn enter_on_a_stopped_ticket_opens_the_resume_card() {
    let mut app = working_live(SIZE);
    assert!(actions(&key(&mut app, 3.0, Key::Enter)).is_empty());
    assert_eq!(app.card, None, "Enter does nothing while a turn runs");

    let mut app = resting_live("review", &[("stopped", "you stopped it")]);
    key(&mut app, 3.0, Key::Enter);
    let Some(Card::Resume(card)) = &app.card else {
        panic!("no resume card: {:?}", app.card);
    };
    assert_eq!(card.key, "DEMO-12");
    let fx = key(&mut app, 3.1, Key::Enter);
    assert_eq!(
        actions(&fx)[0].1,
        Action::Take {
            key: "DEMO-12".into()
        }
    );
}

#[test]
fn o_on_a_running_turn_offers_tell_and_stop() {
    let mut app = working_live(SIZE);
    key(&mut app, 3.0, Key::Char('o'));
    let Some(Card::Open(card)) = &app.card else {
        panic!("no open card: {:?}", app.card);
    };
    assert_eq!(card.running.as_deref(), Some("builder"));
    // Enter does not open a session while a turn runs in it.
    assert!(key(&mut app, 3.1, Key::Enter).is_empty());
    assert!(matches!(app.card, Some(Card::Open(_))));
    key(&mut app, 3.2, Key::Char('t'));
    assert_eq!(app.card, None);
    assert!(app.live.as_ref().unwrap().tell.is_some());

    let mut app = working_live(SIZE);
    key(&mut app, 3.0, Key::Char('o'));
    key(&mut app, 3.1, Key::Char('x'));
    assert!(matches!(app.card, Some(Card::Stop(_))));
}

#[test]
fn o_on_a_resting_ticket_opens_the_session_and_the_card_after_it_retries() {
    let mut app = resting_live("build", &[]);
    key(&mut app, 3.0, Key::Char('o'));
    let Some(Card::Open(card)) = &app.card else {
        panic!("no open card: {:?}", app.card);
    };
    assert_eq!(card.running, None);
    assert_eq!(card.last.as_deref(), Some("builder"));
    let fx = key(&mut app, 3.1, Key::Enter);
    assert_eq!(fx, vec![Effect::OpenSession("DEMO-12".into())]);
    assert_eq!(app.card, None);

    send(
        &mut app,
        9.0,
        Msg::SessionEnded {
            key: "DEMO-12".into(),
            result: ChildResult {
                code: Some(0),
                ..ChildResult::default()
            },
        },
    );
    let Some(Card::Opened(card)) = &app.card else {
        panic!("no card after the session: {:?}", app.card);
    };
    assert_eq!(card.code, Some(0));
    assert_eq!(card.error, None);
    let fx = key(&mut app, 9.1, Key::Enter);
    assert_eq!(
        actions(&fx)[0].1,
        Action::Retry {
            key: "DEMO-12".into()
        }
    );

    // A session that failed names its error, and Esc keeps the ticket paused.
    let mut app = resting_live("build", &[]);
    send(
        &mut app,
        9.0,
        Msg::SessionEnded {
            key: "DEMO-12".into(),
            result: ChildResult {
                code: Some(1),
                stderr: "vision: DEMO-12 has no session to open.\n".into(),
                ..ChildResult::default()
            },
        },
    );
    let Some(Card::Opened(card)) = &app.card else {
        panic!()
    };
    assert_eq!(card.code, Some(1));
    assert!(card
        .error
        .as_deref()
        .unwrap()
        .contains("no session to open"));
    assert!(actions(&key(&mut app, 9.1, Key::Esc)).is_empty());
    assert_eq!(app.card, None);
}

#[test]
fn o_on_a_ticket_with_no_turn_says_so() {
    let mut held = resting("refine");
    held.facts.turns = 0;
    let mut app = live(SIZE, vec![held.clone()], Vec::new(), Vec::new());
    let mut data = data_of(&held, &[]);
    data.timeline.retain(|e| e.source != "turn");
    send(
        &mut app,
        2.6,
        Msg::Ticket {
            key: "DEMO-12".into(),
            data: Box::new(Ok(data)),
        },
    );
    key(&mut app, 3.0, Key::Char('o'));
    assert_eq!(app.card, None);
    assert_eq!(
        notice(&app),
        "DEMO-12 has no turn yet, so there is no session to open."
    );
}

#[test]
fn the_line_of_t_edits_and_sends_its_text() {
    let mut app = working_live(SIZE);
    key(&mut app, 3.0, Key::Char('t'));
    for c in "keep  old".chars() {
        key(&mut app, 3.1, Key::Char(c));
    }
    let line = |app: &App| app.live.as_ref().unwrap().tell.clone().unwrap();
    assert_eq!(line(&app).text(), "keep  old");
    key(&mut app, 3.2, Key::Left);
    key(&mut app, 3.2, Key::Left);
    key(&mut app, 3.2, Key::Left);
    key(&mut app, 3.3, Key::Backspace);
    assert_eq!(line(&app).text(), "keep old");
    key(&mut app, 3.4, Key::Home);
    key(&mut app, 3.4, Key::Delete);
    key(&mut app, 3.4, Key::Char('K'));
    key(&mut app, 3.5, Key::End);
    key(&mut app, 3.5, Key::Right);
    send(&mut app, 3.6, Msg::Paste(" export\nas it is".into()));
    assert_eq!(line(&app).text(), "Keep old export as it is");
    // q and the other keys of the view are letters of the line.
    key(&mut app, 3.7, Key::Char('q'));
    assert_eq!(app.card, None);
    key(&mut app, 3.7, Key::Backspace);
    let fx = key(&mut app, 3.8, Key::Enter);
    let started = actions(&fx);
    let action = Action::Tell {
        key: "DEMO-12".into(),
        text: "Keep old export as it is".into(),
    };
    assert_eq!(started[0].1, action);
    assert_eq!(
        action.args(),
        ["tell", "DEMO-12", "--", "Keep old export as it is"]
    );
    assert_eq!(app.live.as_ref().unwrap().tell, None);
    done(
        &mut app,
        3.9,
        started[0].0,
        action,
        "Sent to the running turn of DEMO-12.\n",
    );
    assert_eq!(
        notice(&app),
        "Sent to the running turn of DEMO-12. The agent reads it at its next tool step."
    );
}

#[test]
fn a_text_for_a_ticket_with_no_running_turn_waits_for_its_next_turn() {
    let mut app = resting_live("build", &[]);
    key(&mut app, 3.0, Key::Char('t'));
    key(&mut app, 3.1, Key::Char('a'));
    let fx = key(&mut app, 3.2, Key::Enter);
    let (id, action) = actions(&fx).remove(0);
    done(
        &mut app,
        3.3,
        id,
        action,
        "Kept for the next turn of DEMO-12.\n",
    );
    assert_eq!(
        notice(&app),
        "No turn of DEMO-12 runs now, so your text waits for its next turn."
    );
}

#[test]
fn an_empty_line_sends_nothing_and_esc_or_ctrl_c_cancel_it() {
    let mut app = working_live(SIZE);
    key(&mut app, 3.0, Key::Char('t'));
    key(&mut app, 3.1, Key::Char(' '));
    let fx = key(&mut app, 3.2, Key::Enter);
    assert!(actions(&fx).is_empty());
    assert_eq!(notice(&app), "The line was empty, so nothing was sent.");

    key(&mut app, 3.3, Key::Char('t'));
    key(&mut app, 3.4, Key::Char('a'));
    key(&mut app, 3.5, Key::Esc);
    assert_eq!(app.live.as_ref().unwrap().tell, None);
    assert!(
        matches!(app.screen, Screen::Live { .. }),
        "Esc closes only the line"
    );

    key(&mut app, 3.6, Key::Char('t'));
    key(&mut app, 3.7, Key::Cancel);
    assert_eq!(app.live.as_ref().unwrap().tell, None);
    assert_eq!(app.card, None, "Ctrl-C in the line does not ask to quit");
    // A paste outside the line changes nothing.
    send(&mut app, 3.8, Msg::Paste("text".into()));
    assert_eq!(app.live.as_ref().unwrap().tell, None);
}

#[test]
fn the_pager_of_the_log_moves_and_opens_the_events_of_a_turn() {
    let mut app = working_live(SIZE);
    key(&mut app, 3.0, Key::Char('l'));
    let pager = |app: &App| app.live.as_ref().unwrap().pagers.last().cloned().unwrap();
    assert_eq!(pager(&app).kind, PagerKind::Log);
    let last = pager(&app).lines.len() - 1;
    assert_eq!(
        pager(&app).cursor,
        Some(last),
        "the cursor starts at the end"
    );
    key(&mut app, 3.1, Key::Up);
    key(&mut app, 3.1, Key::Char('k'));
    assert_eq!(pager(&app).cursor, Some(last - 2));
    key(&mut app, 3.2, Key::Char('j'));
    assert_eq!(pager(&app).cursor, Some(last - 1));
    key(&mut app, 3.3, Key::Char('g'));
    assert_eq!(pager(&app).cursor, Some(0));
    key(&mut app, 3.3, Key::PageDown);
    assert!(pager(&app).cursor.unwrap() > 1);
    key(&mut app, 3.3, Key::PageUp);
    assert_eq!(pager(&app).cursor, Some(0));
    // Enter on a line that is not a turn does nothing.
    assert!(key(&mut app, 3.4, Key::Enter).is_empty());
    key(&mut app, 3.5, Key::Char('G'));
    assert_eq!(pager(&app).cursor, Some(last));
    let fx = key(&mut app, 3.6, Key::Enter);
    assert_eq!(
        fx,
        vec![Effect::LoadTurn {
            key: "DEMO-12".into(),
            turn: TURN.into()
        }]
    );
    assert_eq!(pager(&app).title, "turn build-1 of 14:24");
    assert_eq!(pager(&app).state, PagerState::Loading);
    send(
        &mut app,
        3.7,
        Msg::TurnEvents {
            key: "DEMO-12".into(),
            turn: TURN.into(),
            events: Err("the stream file of the turn is not there".into()),
        },
    );
    assert_eq!(
        pager(&app).state,
        PagerState::Failed("the stream file of the turn is not there".into())
    );
    // Esc closes one pager at a time, then the view.
    key(&mut app, 3.8, Key::Esc);
    assert_eq!(pager(&app).kind, PagerKind::Log);
    key(&mut app, 3.9, Key::Esc);
    assert!(app.live.as_ref().unwrap().pagers.is_empty());
    let fx = key(&mut app, 4.0, Key::Esc);
    assert_eq!(fx, vec![Effect::Follow(None)]);
    assert_eq!(app.screen, Screen::Home);
}

#[test]
fn p_and_d_load_their_text_and_q_in_a_pager_asks_to_quit() {
    let mut app = working_live(SIZE);
    let fx = key(&mut app, 3.0, Key::Char('p'));
    assert_eq!(fx, vec![Effect::LoadPlan("DEMO-12".into())]);
    send(
        &mut app,
        3.1,
        Msg::Plan {
            key: "DEMO-12".into(),
            plan: Err("the plan file is not there".into()),
        },
    );
    let pager = app.live.as_ref().unwrap().pagers.last().unwrap();
    assert_eq!(pager.kind, PagerKind::Plan);
    assert_eq!(
        pager.state,
        PagerState::Failed("the plan file is not there".into())
    );
    key(&mut app, 3.2, Key::Esc);
    let fx = key(&mut app, 3.3, Key::Char('d'));
    assert_eq!(fx, vec![Effect::LoadDiff("DEMO-12".into())]);
    key(&mut app, 3.4, Key::Char('q'));
    assert_eq!(app.card, Some(Card::Quit));

    // A ticket with no clone has no commits to compare.
    let mut held = resting("refine");
    held.view.clone = None;
    let mut app = live(SIZE, vec![held], Vec::new(), Vec::new());
    assert!(key(&mut app, 3.0, Key::Char('d')).is_empty());
    assert_eq!(
        notice(&app),
        "DEMO-12 holds no clone now, so it has no commits to compare."
    );
}

#[test]
fn the_keys_card_and_q_work_as_on_home() {
    let mut app = working_live(SIZE);
    key(&mut app, 3.0, Key::Char('?'));
    assert_eq!(app.card, Some(Card::Help));
    key(&mut app, 3.1, Key::Esc);
    assert_eq!(app.card, None);
    key(&mut app, 3.2, Key::Cancel);
    assert_eq!(app.card, Some(Card::Quit));
    key(&mut app, 3.3, Key::Esc);
    key(&mut app, 3.4, Key::Char('q'));
    let fx = key(&mut app, 3.5, Key::Enter);
    assert_eq!(fx, vec![Effect::Quit]);
}

#[test]
fn a_card_that_runs_shows_its_job_and_a_late_result_of_a_closed_card_changes_nothing() {
    let mut app = working_live(SIZE);
    key(&mut app, 3.0, Key::Char('x'));
    let fx = key(&mut app, 3.1, Key::Enter);
    let Some(Card::Stop(card)) = &app.card else {
        panic!()
    };
    assert!(matches!(
        card.run,
        Run::Running { .. } | Run::Waiting { .. }
    ));
    let (id, action) = actions(&fx).remove(0);
    key(&mut app, 3.2, Key::Esc);
    assert_eq!(app.card, None);
    done(&mut app, 3.3, id, action, "");
    assert_eq!(app.card, None);
}
