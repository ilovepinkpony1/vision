//! Text snapshots of the live view of a ticket: each state of the state function at 110 by 32,
//! the working agent also at 100 by 28 and at 160 by 45, a path with more steps than rows, a
//! feed that does not follow, the line of `t`, each card of the view, and the pager with the
//! log, the events of a turn, the plan, and the commits.

use vision::app::live::StepLine;
use vision::app::model::App;
use vision::app::msg::{ChildResult, Key, Msg};
use vision::pipeline::commands::{DiffCommit, DiffView, PlanView};
use vision::pipeline::facts::{Checks, LimitFact, TurnFact, WaitFacts};
use vision::running::Running;

use super::live_support::*;
use super::support::*;

const SIZE: (u16, u16) = (110, 32);

fn check(name: &str, app: &App) {
    assert_snapshot(name, &screen(app));
}

fn key(app: &mut App, at: f64, k: Key) {
    send(app, at, Msg::Key(k));
}

#[test]
fn an_agent_works() {
    check("live_agent", &working_live(SIZE));
}

#[test]
fn an_agent_works_at_other_sizes() {
    check("live_agent_100x28", &working_live((100, 28)));
    check("live_agent_160x45", &working_live((160, 45)));
}

#[test]
fn the_judge_reads() {
    let mut held = resting("plan");
    held.facts.judge = Some(TurnFact {
        id: "20261116T142800Z-DEMO-12-plan-2".into(),
        agent: "judge".into(),
        step: "plan".into(),
        model: Some("claude-fable-5-2".into()),
        effort: Some("high".into()),
        started: Some(minutes_ago(1)),
        n: 3,
    });
    held.facts.judging_cards = 2;
    let app = live(SIZE, vec![held], Vec::new(), Vec::new());
    check("live_judge", &app);
}

#[test]
fn a_job_runs_inside_a_pass() {
    let held = resting("build");
    let mut data = local(vec![held.clone()], Vec::new());
    data.running = Some(Running {
        what: "job".into(),
        name: "proof".into(),
        ticket: "DEMO-12".into(),
        started: minutes_ago(2),
        pid: 1,
    });
    let mut app = live(SIZE, vec![held], Vec::new(), events());
    send(&mut app, 2.5, Msg::Local(Ok(data)));
    check("live_runs", &app);
}

#[test]
fn a_question_needs_the_person() {
    let mut held = needing(
        "DEMO-12",
        "Export the report of the month as a CSV file",
        "plan",
    );
    held.facts.agents = vec!["scout".into(), "builder".into(), "judge".into()];
    let app = live(SIZE, vec![held], vec![card_need("DEMO-12")], Vec::new());
    check("live_needs", &app);
}

#[test]
fn a_rule_stopped_the_ticket() {
    let mut held = resting("build");
    held.view
        .flags
        .push(flag("needs-you", "the proof did not pass at 1a2b3c4d5e6f"));
    held.needs_you = true;
    let app = live(SIZE, vec![held], Vec::new(), events());
    check("live_needs_flag", &app);
}

#[test]
fn the_ticket_waits_for_ci() {
    let mut held = waiting(
        "DEMO-7",
        "Show the totals under the table of orders",
        "ci",
        "",
    );
    if let Some(wait) = held.facts.wait.as_mut() {
        wait.checks = Checks {
            passed: 3,
            running: 2,
            failed: 0,
        };
        wait.ci = Some(vision::executor::CiFacts {
            head: Some("4f2a9c1".into()),
            state: "pending".into(),
        });
    }
    held.view.marks.plan = Some("3f2a1b4c5d6e".into());
    held.view.marks.jobs.push(vision::pipeline::view::Mark {
        name: "proof".into(),
        result: "pass".into(),
        commit: "4f2a9c1d2e3f".into(),
    });
    let app = live(SIZE, vec![held], Vec::new(), Vec::new());
    check("live_waits", &app);
}

#[test]
fn the_next_step_starts_at_the_next_pass() {
    let mut held = resting("review");
    held.facts.agent = Some("reviewer".into());
    let mut app = live(SIZE, vec![held], Vec::new(), Vec::new());
    // The minute passes, so a pass runs now and the spinner moves.
    send(&mut app, 61.0, Msg::Tick);
    check("live_next", &app);
}

#[test]
fn the_ticket_waits_for_a_limit() {
    let mut held = resting("build");
    held.view
        .flags
        .push(flag("waits-for-limit", "refused by limits.five_hour"));
    held.facts.limit = Some(LimitFact {
        window: "five_hour".into(),
        part: 0.64,
        stop: 0.6,
        resets: Some("2026-11-16T18:00:00Z".into()),
    });
    let app = live(SIZE, vec![held], Vec::new(), events());
    check("live_limit", &app);
}

#[test]
fn the_ticket_is_stopped() {
    // Home opens the resume card for a stopped ticket, so the view shows the stop after `x`.
    let held = resting("review");
    let mut app = live(SIZE, vec![held.clone()], Vec::new(), events());
    let mut stopped = held;
    stopped.view.flags.push(flag("stopped", "you stopped it"));
    stopped.stopped = true;
    let data = data_of(&stopped, &[]);
    send(
        &mut app,
        2.6,
        Msg::Ticket {
            key: "DEMO-12".into(),
            data: Box::new(Ok(data)),
        },
    );
    check("live_stopped", &app);
}

#[test]
fn the_ticket_is_paused() {
    let mut held = resting("build");
    held.view
        .flags
        .push(flag("paused", "you took over the session"));
    held.needs_you = true;
    let app = live(SIZE, vec![held], Vec::new(), events());
    check("live_paused", &app);
}

#[test]
fn the_ticket_is_done() {
    let held = resting("done");
    let mut app = live(SIZE, vec![held.clone()], Vec::new(), Vec::new());
    let mut data = data_of(&held, &[]);
    data.facts.finished = true;
    data.facts.merged_pr = Some(7);
    data.view.pr = Some(7);
    data.view.clone = None;
    data.view.kind = Some("end".into());
    send(
        &mut app,
        2.6,
        Msg::Ticket {
            key: "DEMO-12".into(),
            data: Box::new(Ok(data)),
        },
    );
    check("live_done", &app);
}

#[test]
fn a_path_with_more_steps_than_rows() {
    let mut app = working_live((100, 28));
    if let Some(live) = app.live.as_mut() {
        let data = live.data.data.as_mut().unwrap();
        let mut steps: Vec<StepLine> = (1..=10)
            .map(|i| StepLine {
                id: format!("early-{i}"),
                kind: "turn".into(),
                entries: 1,
                seconds: 60,
                cost_usd: 0.05,
            })
            .collect();
        steps.extend(data.steps.clone());
        steps.extend((1..=6).map(|i| StepLine {
            id: format!("late-{i}"),
            kind: "wait".into(),
            ..StepLine::default()
        }));
        data.steps = steps;
    }
    check("live_path_window", &app);
}

#[test]
fn a_feed_that_does_not_follow() {
    let mut app = working_live(SIZE);
    for i in 0..4 {
        key(&mut app, 3.0 + f64::from(i) / 10.0, Key::Up);
    }
    check("live_scrolled", &app);
}

#[test]
fn the_line_of_tell() {
    let mut app = working_live(SIZE);
    key(&mut app, 3.0, Key::Char('t'));
    for c in "keep the old export as it is".chars() {
        key(&mut app, 3.1, Key::Char(c));
    }
    key(&mut app, 3.2, Key::Left);
    key(&mut app, 3.3, Key::Left);
    check("live_tell", &app);
}

#[test]
fn the_cards_of_the_live_view() {
    let mut app = working_live(SIZE);
    key(&mut app, 3.0, Key::Char('x'));
    check("live_card_stop", &app);

    let mut app = working_live(SIZE);
    key(&mut app, 3.0, Key::Char('o'));
    check("live_card_open_running", &app);

    let mut held = resting("build");
    held.view
        .flags
        .push(flag("needs-you", "the proof did not pass at 1a2b3c4d5e6f"));
    let mut app = live(SIZE, vec![held.clone()], Vec::new(), events());
    key(&mut app, 3.0, Key::Char('r'));
    check("live_card_retry", &app);

    let mut app = live(SIZE, vec![resting("build")], Vec::new(), events());
    key(&mut app, 3.0, Key::Char('o'));
    check("live_card_open", &app);

    let mut app = live(SIZE, vec![resting("build")], Vec::new(), events());
    send(
        &mut app,
        3.0,
        Msg::SessionEnded {
            key: "DEMO-12".into(),
            result: ChildResult {
                code: Some(0),
                ..ChildResult::default()
            },
        },
    );
    check("live_card_opened", &app);

    let mut app = working_live(SIZE);
    key(&mut app, 3.0, Key::Char('?'));
    check("live_keys", &app);
}

#[test]
fn the_pager_with_the_log_and_the_events_of_a_turn() {
    let mut app = working_live(SIZE);
    key(&mut app, 3.0, Key::Char('l'));
    check("pager_log", &app);
    // The cursor starts on the last row: the start of the running turn.
    key(&mut app, 3.1, Key::Enter);
    send(
        &mut app,
        3.2,
        Msg::TurnEvents {
            key: "DEMO-12".into(),
            turn: TURN.into(),
            events: Ok(events()
                .into_iter()
                .map(|(_, event)| vision::app::feed::StreamLine {
                    event,
                    output: None,
                })
                .chain(std::iter::once(vision::app::feed::StreamLine {
                    event: result_event("Built the CSV export."),
                    output: None,
                }))
                .collect()),
        },
    );
    check("pager_turn", &app);
}

#[test]
fn the_pager_with_the_plan_and_the_commits() {
    let mut app = working_live(SIZE);
    key(&mut app, 3.0, Key::Char('p'));
    send(
        &mut app,
        3.1,
        Msg::Plan {
            key: "DEMO-12".into(),
            plan: Ok(Some(PlanView {
                text: "# Plan of DEMO-12\n\n## Steps\n\n1. Add a CSV writer to the export.\n2. Keep the order of the columns.\n3. Test an empty month.\n\n## Proof\n\ntests/test_export.py fails on the base and passes after the change.\n".into(),
                hash: "3f2a1b4c5d6e7f8091a2b3c4d5e6f708192a3b4c5d6e7f8091a2b3c4d5e6f7081".into(),
                edited: false,
                waits: false,
            })),
        },
    );
    check("pager_plan", &app);

    let mut app = working_live(SIZE);
    key(&mut app, 3.0, Key::Char('d'));
    send(
        &mut app,
        3.1,
        Msg::Diff {
            key: "DEMO-12".into(),
            diff: Ok(DiffView {
                ticket: "DEMO-12".into(),
                branch: "vision/DEMO-12".into(),
                clone: "/work/clones/demo-1".into(),
                compared_with: "origin/vision/DEMO-12".into(),
                commits: vec![
                    DiffCommit {
                        commit: "4f2a9c1d2e3f4a5b6c7d8e9f0a1b2c3d4e5f6a7b".into(),
                        subject: "Add a CSV writer to the export".into(),
                    },
                    DiffCommit {
                        commit: "9e8d7c6b5a4f3e2d1c0b9a8f7e6d5c4b3a2f1e0d".into(),
                        subject: "Test an empty month".into(),
                    },
                ],
            }),
        },
    );
    check("pager_diff", &app);
}

#[test]
fn a_ticket_with_no_plan_gives_a_notice_and_no_pager() {
    let mut app = working_live(SIZE);
    key(&mut app, 3.0, Key::Char('p'));
    send(
        &mut app,
        3.1,
        Msg::Plan {
            key: "DEMO-12".into(),
            plan: Ok(None),
        },
    );
    assert!(app.live.as_ref().unwrap().pagers.is_empty());
    assert_eq!(
        app.notice.as_ref().unwrap().text,
        "DEMO-12 has no plan yet. Its plan step writes the plan."
    );
}

#[test]
fn the_waits_of_a_review_and_a_draft() {
    let mut held = waiting("DEMO-7", "Show the totals", "ready", "");
    held.facts.wait = Some(WaitFacts {
        watch: Some("pr.draft".into()),
        value: Some("true".into()),
        observed: Some(minutes_ago(0)),
        ..WaitFacts::default()
    });
    let app = live(SIZE, vec![held], Vec::new(), Vec::new());
    assert!(screen(&app).contains("waits for the pull request #7"));
}
