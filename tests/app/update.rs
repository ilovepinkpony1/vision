//! The update: each key of each screen gives the right change of the model and the right
//! effects, a refresh keeps the selection on the same ticket, and the engine starts a pass at
//! the right moments.

use vision::app::model::{Card, Local, PassEnd, Run, Screen, Tone};
use vision::app::msg::{Action, ChildResult, Effect, JobDone, JobKind, Key, Msg};
use vision::app::terminal::key_of;
use vision::app::update::engine::LOCK_HELD;

use super::support::*;

const SIZE: (u16, u16) = (110, 32);

/// A message to the update, with the refresh of the local data left out of the effects: the
/// refresh runs each second, and its own test checks it.
fn send(app: &mut vision::app::model::App, at: f64, msg: Msg) -> Vec<Effect> {
    let mut fx = super::support::send(app, at, msg);
    fx.retain(|e| *e != Effect::LoadLocal);
    fx
}

fn key(app: &mut vision::app::model::App, at: f64, k: Key) -> Vec<Effect> {
    send(app, at, Msg::Key(k))
}

fn runs(effects: &[Effect]) -> Vec<JobKind> {
    effects
        .iter()
        .filter_map(|e| match e {
            Effect::Run(job) => Some(job.kind.clone()),
            _ => None,
        })
        .collect()
}

fn done(job_id: u64, kind: JobKind, result: ChildResult) -> Msg {
    Msg::JobDone(JobDone {
        id: job_id,
        kind,
        result,
    })
}

fn ok(stdout: &str) -> ChildResult {
    ChildResult {
        code: Some(0),
        stdout: stdout.into(),
        ..ChildResult::default()
    }
}

#[test]
fn the_arrows_and_j_k_move_through_both_boxes() {
    let mut app = home(SIZE);
    assert_eq!(app.home.selected.as_deref(), Some("DEMO-12"));
    key(&mut app, 2.0, Key::Down);
    assert_eq!(app.home.selected.as_deref(), Some("DEMO-7"));
    key(&mut app, 2.1, Key::Char('j'));
    assert_eq!(app.home.selected.as_deref(), Some("DEMO-15"));
    key(&mut app, 2.2, Key::Char('j'));
    key(&mut app, 2.3, Key::Down);
    assert_eq!(
        app.home.selected.as_deref(),
        Some("DEMO-9"),
        "the last row stays"
    );
    key(&mut app, 2.4, Key::Char('k'));
    key(&mut app, 2.5, Key::Up);
    key(&mut app, 2.6, Key::Up);
    key(&mut app, 2.7, Key::Up);
    assert_eq!(
        app.home.selected.as_deref(),
        Some("DEMO-12"),
        "the first row stays"
    );
}

#[test]
fn enter_opens_the_live_view_of_the_ticket_in_progress_and_esc_goes_back() {
    let mut app = home(SIZE);
    let fx = key(&mut app, 2.0, Key::Enter);
    assert_eq!(
        fx,
        vec![
            Effect::LoadTicket("DEMO-12".into()),
            Effect::Follow(Some("DEMO-12".into()))
        ]
    );
    assert_eq!(
        app.screen,
        Screen::Live {
            key: "DEMO-12".into()
        }
    );
    let fx = key(&mut app, 2.1, Key::Esc);
    assert_eq!(app.screen, Screen::Home);
    assert_eq!(fx, vec![Effect::Follow(None)]);
    assert_eq!(app.live, None);
}

#[test]
fn enter_on_a_ticket_that_can_start_opens_the_start_card_and_reads_its_facts() {
    let mut app = home(SIZE);
    key(&mut app, 2.0, Key::Down);
    key(&mut app, 2.1, Key::Down);
    let fx = key(&mut app, 2.2, Key::Enter);
    assert_eq!(fx, vec![Effect::LoadStartInfo("DEMO-15".into())]);
    let Some(Card::Start(card)) = &app.card else {
        panic!("no start card: {:?}", app.card);
    };
    assert_eq!(card.key, "DEMO-15");
    assert_eq!(card.info, None);
    assert_eq!(card.run, Run::Idle);
    // A late answer for another ticket changes nothing.
    send(
        &mut app,
        2.3,
        Msg::StartInfo {
            key: "DEMO-9".into(),
            info: Err("x".into()),
        },
    );
    let Some(Card::Start(card)) = &app.card else {
        panic!()
    };
    assert_eq!(card.info, None);
    key(&mut app, 2.4, Key::Esc);
    assert_eq!(app.card, None);
}

#[test]
fn enter_on_a_ticket_that_cannot_start_shows_its_reason() {
    let mut app = home(SIZE);
    for i in 0..3 {
        key(&mut app, 2.0 + f64::from(i) / 10.0, Key::Down);
    }
    let fx = key(&mut app, 2.5, Key::Enter);
    assert!(fx.is_empty());
    assert_eq!(app.card, None);
    let notice = app.notice.clone().unwrap();
    assert_eq!(
        notice.text,
        "DEMO-9 cannot start: it already has an open pull request"
    );
    assert_eq!(notice.tone, Tone::Info);
    // The notice goes away after a few seconds.
    send(&mut app, 10.0, Msg::Tick);
    assert_eq!(app.notice, None);
}

#[test]
fn enter_on_a_stopped_ticket_asks_to_resume_it_with_take() {
    let tickets = vec![stopped("DEMO-3", "Rename a setting", "review")];
    let mut app = home_with(SIZE, local(tickets, Vec::new()), Some(queue()));
    key(&mut app, 2.0, Key::Enter);
    let Some(Card::Resume(card)) = &app.card else {
        panic!("no resume card: {:?}", app.card);
    };
    assert_eq!(
        (card.key.as_str(), card.step.as_str()),
        ("DEMO-3", "review")
    );
    let fx = key(&mut app, 2.1, Key::Enter);
    assert_eq!(
        runs(&fx),
        vec![JobKind::Action(Action::Take {
            key: "DEMO-3".into()
        })]
    );
    let Some(Card::Resume(card)) = &app.card else {
        panic!()
    };
    assert!(matches!(card.run, Run::Running { .. }));
}

#[test]
fn n_opens_the_card_of_the_needs_or_says_that_nothing_waits() {
    let mut app = home(SIZE);
    key(&mut app, 2.0, Key::Char('n'));
    assert_eq!(app.card, None);
    assert_eq!(app.notice.as_ref().unwrap().text, "Nothing waits for you.");
    let mut app = home_with(
        SIZE,
        local(two_tickets(), vec![card_need("DEMO-12")]),
        Some(queue()),
    );
    let fx = key(&mut app, 2.0, Key::Char('n'));
    assert!(matches!(app.card, Some(Card::Need(_))));
    assert!(fx.iter().any(|e| matches!(e, Effect::LoadNeeds(_))));
    key(&mut app, 2.1, Key::Esc);
    assert_eq!(app.card, None);
}

#[test]
fn r_loads_the_queue_again_and_a_second_r_waits_for_it() {
    let mut app = home(SIZE);
    let fx = key(&mut app, 2.0, Key::Char('r'));
    assert_eq!(fx, vec![Effect::LoadQueue]);
    assert!(app.queue.loading());
    let fx = key(&mut app, 2.1, Key::Char('r'));
    assert!(fx.is_empty(), "{fx:?}");
    // The load that ran ends, and the second one starts.
    let fx = send(&mut app, 2.5, Msg::Queue(Ok(queue())));
    assert_eq!(fx, vec![Effect::LoadQueue]);
}

#[test]
fn k_opens_the_kill_switch_card_and_enter_runs_off_or_on() {
    let mut app = home(SIZE);
    key(&mut app, 2.0, Key::Char('K'));
    let Some(Card::Kill(card)) = &app.card else {
        panic!()
    };
    assert!(!card.turn_on);
    let fx = key(&mut app, 2.1, Key::Enter);
    assert_eq!(runs(&fx), vec![JobKind::Action(Action::Off)]);
    // A second Enter while it runs does nothing.
    let fx = key(&mut app, 2.2, Key::Enter);
    assert!(runs(&fx).is_empty());

    let mut data = local(two_tickets(), Vec::new());
    data.off = true;
    let mut app = home_with(SIZE, data, Some(queue()));
    key(&mut app, 2.0, Key::Char('K'));
    let fx = key(&mut app, 2.1, Key::Enter);
    assert_eq!(runs(&fx), vec![JobKind::Action(Action::On)]);
}

#[test]
fn the_keys_card_opens_with_a_question_mark_and_closes_with_esc() {
    let mut app = home(SIZE);
    key(&mut app, 2.0, Key::Char('?'));
    assert_eq!(app.card, Some(Card::Help));
    key(&mut app, 2.1, Key::Enter);
    assert_eq!(app.card, Some(Card::Help));
    key(&mut app, 2.2, Key::Esc);
    assert_eq!(app.card, None);
}

#[test]
fn q_leaves_at_once_when_no_ticket_moves() {
    let mut app = home_with(SIZE, local(Vec::new(), Vec::new()), Some(queue()));
    let fx = key(&mut app, 2.0, Key::Char('q'));
    assert_eq!(fx, vec![Effect::Quit]);
    assert!(app.quit);
    // A stopped ticket does not move either.
    let tickets = vec![stopped("DEMO-3", "Rename a setting", "review")];
    let mut app = home_with(SIZE, local(tickets, Vec::new()), Some(queue()));
    assert_eq!(key(&mut app, 2.0, Key::Char('q')), vec![Effect::Quit]);
}

#[test]
fn q_asks_first_when_tickets_are_in_progress() {
    let mut app = home(SIZE);
    let fx = key(&mut app, 2.0, Key::Char('q'));
    assert!(fx.is_empty());
    assert_eq!(app.card, Some(Card::Quit));
    key(&mut app, 2.1, Key::Esc);
    assert_eq!(app.card, None);
    assert!(!app.quit);
    key(&mut app, 2.2, Key::Char('q'));
    let fx = key(&mut app, 2.3, Key::Enter);
    assert_eq!(fx, vec![Effect::Quit]);
    assert!(app.quit);
}

#[test]
fn ctrl_c_is_the_key_q() {
    use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    let ctrl_c = KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL);
    // Ctrl-C arrives as Cancel, which acts as q (and as Esc in the line of `t`).
    assert_eq!(key_of(ctrl_c), Some(Key::Cancel));
    let mut app = home(SIZE);
    key(&mut app, 2.0, Key::Cancel);
    assert_eq!(app.card, Some(Card::Quit));
    let plain = KeyEvent::new(KeyCode::Char('c'), KeyModifiers::NONE);
    assert_eq!(key_of(plain), Some(Key::Char('c')));
    assert_eq!(
        key_of(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE)),
        Some(Key::Up)
    );
}

#[test]
fn a_refresh_keeps_the_selection_on_the_same_ticket() {
    let mut app = home(SIZE);
    key(&mut app, 2.0, Key::Down);
    assert_eq!(app.home.selected.as_deref(), Some("DEMO-7"));
    // A new ticket comes before it in the list.
    let mut tickets = two_tickets();
    tickets.insert(
        0,
        waiting("DEMO-1", "A new ticket", "plan", "next: a turn of builder"),
    );
    send(
        &mut app,
        3.0,
        Msg::Local(Ok(local(tickets.clone(), Vec::new()))),
    );
    assert_eq!(app.home.selected.as_deref(), Some("DEMO-7"));
    // The queue changes too.
    let mut rows = queue();
    rows.reverse();
    send(&mut app, 3.1, Msg::Queue(Ok(rows)));
    assert_eq!(app.home.selected.as_deref(), Some("DEMO-7"));
    // The selected ticket leaves: the row at its place gets the selection.
    tickets.retain(|t| t.view.key != "DEMO-7");
    send(&mut app, 4.0, Msg::Local(Ok(local(tickets, Vec::new()))));
    assert_eq!(app.home.selected.as_deref(), Some("DEMO-9"));
}

#[test]
fn a_take_that_worked_closes_the_card_and_selects_the_ticket() {
    let mut app = super::snapshots::start_card();
    let fx = key(&mut app, 3.0, Key::Enter);
    let Some(Effect::Run(job)) = fx.first().cloned() else {
        panic!("{fx:?}")
    };
    let fx = send(
        &mut app,
        4.0,
        done(job.id, job.kind, ok("Took DEMO-15 ...")),
    );
    assert_eq!(app.card, None);
    assert_eq!(
        app.notice.as_ref().unwrap().text,
        "Took DEMO-15. A pass starts its first step now."
    );
    assert!(fx.contains(&Effect::LoadQueue), "{fx:?}");
    assert!(
        app.local.loading(),
        "the local data loads again after each job"
    );
    assert_eq!(
        runs(&fx),
        vec![JobKind::Pass],
        "a pass runs after an action"
    );
    // The next local data has the ticket in progress, and the selection moves to it.
    let mut tickets = two_tickets();
    tickets.push(waiting(
        "DEMO-15",
        "Keep the filter",
        "refine",
        "next: a turn of scout",
    ));
    send(&mut app, 4.5, Msg::Local(Ok(local(tickets, Vec::new()))));
    assert_eq!(app.home.selected.as_deref(), Some("DEMO-15"));
}

#[test]
fn a_failed_take_keeps_the_card_with_the_error() {
    let mut app = super::snapshots::start_card();
    let fx = key(&mut app, 3.0, Key::Enter);
    let Some(Effect::Run(job)) = fx.first().cloned() else {
        panic!()
    };
    send(
        &mut app,
        4.0,
        done(
            job.id,
            job.kind,
            ChildResult {
                code: Some(1),
                stderr: "error: DEMO-15: the queue of the tracker has no ticket DEMO-15\n".into(),
                ..ChildResult::default()
            },
        ),
    );
    let Some(Card::Start(card)) = &app.card else {
        panic!()
    };
    assert_eq!(
        card.run,
        Run::Failed("DEMO-15: the queue of the tracker has no ticket DEMO-15".into())
    );
    // Enter tries again.
    let fx = key(&mut app, 5.0, Key::Enter);
    assert_eq!(runs(&fx).len(), 1);
}

// ----- the engine ---------------------------------------------------------------------------

#[test]
fn the_engine_starts_with_a_pass_when_the_ledger_stage_settles() {
    let mut app = booting(SIZE);
    let fx = super::support::send(&mut app, 0.1, Msg::Setup(Ok(setup())));
    assert_eq!(fx, vec![Effect::LoadLocal, Effect::LoadQueue]);
    assert!(!app.engine.started);
    let fx = send(&mut app, 0.2, Msg::Local(Ok(local(Vec::new(), Vec::new()))));
    assert!(fx.contains(&Effect::StartWorker));
    assert_eq!(runs(&fx), vec![JobKind::Pass]);
    assert!(app.engine.started);
}

#[test]
fn a_pass_starts_each_minute_and_never_twice_at_once() {
    let mut app = home(SIZE);
    assert!(runs(&send(&mut app, 30.0, Msg::Tick)).is_empty());
    let fx = send(&mut app, 60.3, Msg::Tick);
    assert_eq!(runs(&fx), vec![JobKind::Pass]);
    // While it runs, no other pass starts.
    assert!(runs(&send(&mut app, 200.0, Msg::Tick)).is_empty());
    assert_eq!(app.engine.queue.len(), 0);
}

#[test]
fn a_pass_starts_when_a_running_turn_ends() {
    let mut app = home(SIZE);
    // DEMO-12 has a working builder. In the next data, it works no more.
    let mut tickets = two_tickets();
    tickets[0].view.works = None;
    tickets[0].view.now = "the next tick tests the end facts of the turn of builder".into();
    let fx = send(&mut app, 5.0, Msg::Local(Ok(local(tickets, Vec::new()))));
    assert_eq!(runs(&fx), vec![JobKind::Pass]);
}

#[test]
fn a_pass_starts_when_a_turn_of_the_judge_ends() {
    let mut app = home(SIZE);
    // A turn of the Judge runs too: no ticket shows it as its work, and the count has it.
    let mut judging = local(two_tickets(), Vec::new());
    judging.turns += 1;
    let first = app.engine.queue.len();
    send(&mut app, 2.0, Msg::Local(Ok(judging.clone())));
    assert_eq!(
        app.engine.queue.len(),
        first,
        "one more turn starts no pass"
    );
    // The same count again starts nothing.
    assert!(runs(&send(&mut app, 3.0, Msg::Local(Ok(judging)))).is_empty());
    // The turn of the Judge ended: a pass takes its answers in at once.
    let fx = send(
        &mut app,
        4.0,
        Msg::Local(Ok(local(two_tickets(), Vec::new()))),
    );
    assert_eq!(runs(&fx), vec![JobKind::Pass]);
}

/// The id of the pass that the effects started.
fn pass_id(effects: &[Effect]) -> Option<u64> {
    effects.iter().find_map(|e| match e {
        Effect::Run(job) if job.kind == JobKind::Pass => Some(job.id),
        _ => None,
    })
}

/// The sample tickets after the turn of DEMO-12 ended and held its end facts, at a step.
fn turn_held(step: &str) -> Local {
    let mut tickets = two_tickets();
    tickets[0].view.works = None;
    tickets[0].view.step = Some(step.into());
    tickets[0].facts.work = None;
    tickets[0].facts.held = true;
    local(tickets, Vec::new())
}

/// Gives the local data to the update, also for a load that the update asked for again.
fn load(app: &mut vision::app::model::App, at: f64, data: &Local) -> Vec<Effect> {
    let mut fx = send(app, at, Msg::Local(Ok(data.clone())));
    while app.local.loading() {
        fx.extend(send(app, at, Msg::Local(Ok(data.clone()))));
    }
    fx
}

#[test]
fn a_pass_that_left_work_ready_starts_the_next_pass_at_once() {
    let mut app = home(SIZE);
    // The turn of DEMO-12 ended, so a pass collects it.
    let mut ended = two_tickets();
    ended[0].view.works = None;
    ended[0].facts.work = None;
    ended[0].facts.testing = true;
    let fx = send(&mut app, 5.0, Msg::Local(Ok(local(ended, Vec::new()))));
    let first = pass_id(&fx).expect("the pass that collects the turn");
    // The pass ends. The data after it shows a ticket that the next pass moves on.
    let fx = send(
        &mut app,
        6.0,
        done(first, JobKind::Pass, ok("tick done in 12 ms\n")),
    );
    assert!(runs(&fx).is_empty());
    let fx = load(&mut app, 6.1, &turn_held("build"));
    let second = pass_id(&fx).expect("a pass at once, and no wait of a minute");
    // That pass changed nothing, so no other pass starts before the minute.
    let fx = send(
        &mut app,
        7.0,
        done(second, JobKind::Pass, ok("tick done in 12 ms\n")),
    );
    assert!(runs(&fx).is_empty());
    let fx = load(&mut app, 7.1, &turn_held("build"));
    assert!(runs(&fx).is_empty());
    assert!(runs(&send(&mut app, 30.0, Msg::Tick)).is_empty());
}

#[test]
fn a_paused_engine_and_a_skipped_pass_start_no_quick_pass() {
    // Another tick held the lock: this pass did nothing, so it says nothing about the tickets.
    let mut app = home_with(SIZE, turn_held("plan"), Some(queue()));
    let fx = send(&mut app, 60.3, Msg::Tick);
    let id = pass_id(&fx).expect("the pass of the minute");
    let skipped = ok("tick skipped: another tick holds the lock\n");
    send(&mut app, 61.0, done(id, JobKind::Pass, skipped));
    let fx = load(&mut app, 61.1, &turn_held("build"));
    assert!(runs(&fx).is_empty());

    // A paused engine starts no pass at all.
    let mut app = home_with(SIZE, turn_held("plan"), Some(queue()));
    let fx = send(&mut app, 60.3, Msg::Tick);
    let id = pass_id(&fx).expect("the pass of the minute");
    key(&mut app, 60.5, Key::Char('p'));
    send(
        &mut app,
        61.0,
        done(id, JobKind::Pass, ok("tick done in 12 ms\n")),
    );
    let fx = load(&mut app, 61.1, &turn_held("build"));
    assert!(runs(&fx).is_empty());
    assert!(app.engine.queue.is_empty());
}

#[test]
fn the_quick_passes_in_a_row_have_a_limit() {
    use vision::app::update::engine::QUICK_MAX;
    let mut app = home_with(SIZE, turn_held("plan"), Some(queue()));
    let mut at = 60.3;
    let mut fx = send(&mut app, at, Msg::Tick);
    let mut quick = 0;
    // Each pass moves the ticket to a new step and leaves work ready, without an end.
    for n in 0..40 {
        let Some(id) = pass_id(&fx) else {
            break;
        };
        at += 0.5;
        send(
            &mut app,
            at,
            done(id, JobKind::Pass, ok("tick done in 12 ms\n")),
        );
        at += 0.1;
        fx = load(&mut app, at, &turn_held(&format!("step-{n}")));
        if pass_id(&fx).is_some() {
            quick += 1;
        }
    }
    assert_eq!(quick, QUICK_MAX);
    // The pass of the next minute starts the count again.
    let fx = send(&mut app, 200.0, Msg::Tick);
    let id = pass_id(&fx).expect("the pass of the minute");
    send(
        &mut app,
        200.5,
        done(id, JobKind::Pass, ok("tick done in 12 ms\n")),
    );
    let fx = load(&mut app, 200.6, &turn_held("a-new-step"));
    assert!(pass_id(&fx).is_some());
}

#[test]
fn a_paused_engine_starts_no_pass_and_still_runs_actions() {
    let mut app = home(SIZE);
    key(&mut app, 2.0, Key::Char('p'));
    assert!(app.engine.paused);
    assert!(runs(&send(&mut app, 120.0, Msg::Tick)).is_empty());
    key(&mut app, 121.0, Key::Char('K'));
    let fx = key(&mut app, 121.1, Key::Enter);
    assert_eq!(runs(&fx), vec![JobKind::Action(Action::Off)]);
    let Some(Effect::Run(job)) = fx.into_iter().find(|e| matches!(e, Effect::Run(_))) else {
        panic!()
    };
    // After the action, a pass waits in the queue but does not start.
    let fx = send(
        &mut app,
        122.0,
        done(job.id, job.kind, ok("Vision is off.")),
    );
    assert!(runs(&fx).is_empty());
    assert_eq!(app.engine.queue.len(), 1);
    // p starts the engine again, and the pass runs.
    let fx = key(&mut app, 123.0, Key::Char('p'));
    assert_eq!(runs(&fx), vec![JobKind::Pass]);
}

#[test]
fn an_action_waits_behind_a_running_pass_and_goes_first_after_it() {
    let mut app = home(SIZE);
    let fx = send(&mut app, 61.0, Msg::Tick);
    let Some(Effect::Run(pass)) = fx.into_iter().find(|e| matches!(e, Effect::Run(_))) else {
        panic!()
    };
    key(&mut app, 62.0, Key::Char('K'));
    let fx = key(&mut app, 62.1, Key::Enter);
    assert!(runs(&fx).is_empty(), "the worker is busy: {fx:?}");
    let Some(Card::Kill(card)) = &app.card else {
        panic!()
    };
    assert!(matches!(card.run, Run::Waiting { .. }));
    assert_eq!(
        app.notice.as_ref().unwrap().text,
        "The kill switch waits for the pass that runs now. It starts next."
    );
    // The turn of DEMO-12 ends while the pass runs, so one more pass waits too.
    let mut tickets = two_tickets();
    tickets[0].view.works = None;
    send(&mut app, 63.0, Msg::Local(Ok(local(tickets, Vec::new()))));
    let fx = send(
        &mut app,
        90.0,
        done(pass.id, pass.kind, ok("tick done in 5 ms")),
    );
    assert_eq!(runs(&fx), vec![JobKind::Action(Action::Off)]);
    let Some(Card::Kill(card)) = &app.card else {
        panic!()
    };
    assert!(matches!(card.run, Run::Running { .. }));
}

#[test]
fn another_tick_that_holds_the_lock_is_not_an_error() {
    let mut app = home(SIZE);
    let fx = send(&mut app, 61.0, Msg::Tick);
    let Some(Effect::Run(pass)) = fx.into_iter().find(|e| matches!(e, Effect::Run(_))) else {
        panic!()
    };
    send(
        &mut app,
        62.0,
        done(
            pass.id,
            pass.kind,
            ok(&format!("tick skipped: {LOCK_HELD}\n")),
        ),
    );
    assert_eq!(app.engine.last_pass, Some(PassEnd::Skipped));
    assert_eq!(app.notice, None);
}

#[test]
fn a_failed_pass_shows_its_error_text() {
    let mut app = home(SIZE);
    let fx = send(&mut app, 61.0, Msg::Tick);
    let Some(Effect::Run(pass)) = fx.into_iter().find(|e| matches!(e, Effect::Run(_))) else {
        panic!()
    };
    send(
        &mut app,
        62.0,
        done(
            pass.id,
            pass.kind,
            ChildResult {
                code: Some(1),
                stderr: "error: step 3, reconcile the operations: the ledger is locked\n".into(),
                ..ChildResult::default()
            },
        ),
    );
    let notice = app.notice.clone().unwrap();
    assert_eq!(notice.tone, Tone::Bad);
    assert_eq!(
        notice.text,
        "The pass failed: step 3, reconcile the operations: the ledger is locked. Vision tries again in a minute."
    );
    assert!(matches!(app.engine.last_pass, Some(PassEnd::Failed(_))));
}

#[test]
fn the_local_data_loads_again_each_second_and_after_each_job() {
    let mut app = home(SIZE);
    let at = |fx: &[Effect]| fx.contains(&Effect::LoadLocal);
    // The last load ended at 0.35 s.
    assert!(!at(&super::support::send(&mut app, 1.2, Msg::Tick)));
    assert!(at(&super::support::send(&mut app, 1.4, Msg::Tick)));
    // While it loads, no second load starts.
    assert!(!at(&super::support::send(&mut app, 3.0, Msg::Tick)));
    super::support::send(
        &mut app,
        3.1,
        Msg::Local(Ok(local(two_tickets(), Vec::new()))),
    );
    assert!(!at(&super::support::send(&mut app, 3.5, Msg::Tick)));
    // A job ends: the data loads again at once.
    let fx = super::support::send(&mut app, 61.0, Msg::Tick);
    let Some(Effect::Run(pass)) = fx.into_iter().find(|e| matches!(e, Effect::Run(_))) else {
        panic!()
    };
    super::support::send(
        &mut app,
        61.1,
        Msg::Local(Ok(local(two_tickets(), Vec::new()))),
    );
    let fx = super::support::send(&mut app, 61.2, done(pass.id, pass.kind, ok("tick done")));
    assert!(at(&fx));
}

#[test]
fn the_queue_loads_again_each_five_minutes() {
    let mut app = home(SIZE);
    assert!(!send(&mut app, 200.0, Msg::Tick).contains(&Effect::LoadQueue));
    assert!(send(&mut app, 301.0, Msg::Tick).contains(&Effect::LoadQueue));
}

#[test]
fn a_ticket_of_before_takes_its_title_from_the_queue() {
    let mut tickets = two_tickets();
    tickets[0].view.title = None;
    let mut rows = queue();
    rows.push(vision::pipeline::commands::PickRow {
        key: "DEMO-12".into(),
        title: "The title from the tracker".into(),
        status: "In Progress".into(),
        eligible: false,
        reason: String::new(),
        taken: Some("build".into()),
    });
    let app = home_with(SIZE, local(tickets, Vec::new()), Some(rows));
    assert_eq!(app.title_of(&app.held()[0]), "The title from the tracker");
    assert!(screen(&app).contains("DEMO-12   The title from the tracker"));
    // The queue box does not list it, because Vision holds it.
    assert_eq!(app.queue_rows().len(), 2);
}
