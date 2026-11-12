//! A text snapshot of each card of the things that wait, in each of its states, at 110 by 32,
//! and the largest cards also at 100 by 28: the question (idle, with and without a proposal of
//! the Judge, the line of the words, the line of the reason, the question of a needless card,
//! its chain while it runs and after it failed), the plan (also after it changed), a write that
//! waits, the first write (also while its chain runs), a rule that stopped a ticket, a paused
//! ticket, a proposal of the Curator, a notice of a gate, and the card while it loads or after
//! its load failed.

use vision::app::chain::{CheckFacts, OpFacts};
use vision::app::model::App;
use vision::app::msg::{ChildResult, Effect, JobDone, Key, Msg};
use vision::app::needs::NeedItem;

use super::need_support::*;
use super::support::*;

const SIZE: (u16, u16) = (110, 32);
const SMALL: (u16, u16) = (100, 28);

fn check(name: &str, app: &App) {
    assert_snapshot(name, &screen(app));
}

/// Sends a key, and gives the effects.
fn key(app: &mut App, at: f64, key: Key) -> Vec<Effect> {
    send(app, at, Msg::Key(key))
}

/// The chain and the check of a read that the effects asked for.
fn check_of(effects: &[Effect]) -> u64 {
    effects
        .iter()
        .find_map(|e| match e {
            Effect::LoadCheck { chain, .. } => Some(*chain),
            _ => None,
        })
        .unwrap_or_else(|| panic!("no check: {effects:?}"))
}

#[test]
fn the_question_card_with_a_proposal_of_the_judge() {
    let items = vec![NeedItem::Question(question(true)), plan_item(false)];
    check("need_question", &open_with(SIZE, items.clone()));
    check("need_question_100x28", &open_with(SMALL, items));
}

#[test]
fn the_question_card_without_a_proposal() {
    let app = open_with(SIZE, vec![NeedItem::Question(question(false))]);
    check("need_question_no_judge", &app);
}

#[test]
fn the_question_card_with_its_lines() {
    let mut app = open_with(SIZE, vec![NeedItem::Question(question(false))]);
    key(&mut app, 3.0, Key::Char('e'));
    type_text(&mut app, 3.1, "Keep the order, and put the total last");
    check("need_question_words", &app);
    key(&mut app, 3.2, Key::Enter);
    type_text(&mut app, 3.3, "The team reads the file next to the table");
    check("need_question_why_text", &app);

    let mut app = open_with(SIZE, vec![NeedItem::Question(question(false))]);
    key(&mut app, 3.0, Key::Char('b'));
    check("need_question_why", &app);

    let mut app = open_with(SIZE, vec![NeedItem::Question(question(false))]);
    key(&mut app, 3.0, Key::Char('k'));
    check("need_question_needless", &app);
}

#[test]
fn the_question_card_while_its_answer_runs_and_after_it_failed() {
    let mut app = open_with(SIZE, vec![NeedItem::Question(question(true))]);
    key(&mut app, 3.0, Key::Enter);
    let fx = key(&mut app, 3.1, Key::Tab);
    let job = jobs(&fx).pop().expect("the answer runs");
    send(&mut app, 3.5, Msg::Tick);
    check("need_question_runs", &app);
    send(
        &mut app,
        4.0,
        Msg::JobDone(JobDone {
            id: job.id,
            kind: job.kind.clone(),
            result: ChildResult {
                code: Some(1),
                stderr: "error: the card Q-0002 is already answered\n".into(),
                ..ChildResult::default()
            },
        }),
    );
    check("need_question_failed", &app);
}

#[test]
fn the_plan_card() {
    let app = open_with(SIZE, vec![plan_item(true)]);
    check("need_plan", &app);
    check(
        "need_plan_100x28",
        &open_with(SMALL, vec![plan_item(false)]),
    );
    let mut app = open_with(SIZE, vec![plan_item(false)]);
    key(&mut app, 3.0, Key::End);
    check("need_plan_end", &app);
}

#[test]
fn the_plan_card_after_the_plan_changed() {
    let mut app = open_with(SIZE, vec![plan_item(false)]);
    let fx = key(&mut app, 3.0, Key::Char('a'));
    let chain = check_of(&fx);
    let fx = send(
        &mut app,
        3.2,
        Msg::Checked {
            chain,
            facts: Box::new(Ok(CheckFacts {
                plan: Some("9".repeat(64)),
                ..CheckFacts::default()
            })),
        },
    );
    assert!(jobs(&fx).is_empty(), "nothing runs: {fx:?}");
    let load = fx
        .iter()
        .find_map(|e| match e {
            Effect::LoadNeeds(load) => Some(*load),
            _ => None,
        })
        .expect("the card loads again");
    let NeedItem::Plan(mut plan) = plan_item(false) else {
        unreachable!()
    };
    plan.plan.hash = "9".repeat(64);
    plan.plan.text =
        "# Plan of DEMO-12\n\n## Decisions\n\n- The export sorts the columns by name.\n".into();
    send(
        &mut app,
        3.4,
        Msg::Needs {
            load,
            items: Box::new(Ok(vec![NeedItem::Plan(plan)])),
        },
    );
    check("need_plan_changed", &app);
}

#[test]
fn the_cards_of_a_write_that_waits() {
    check(
        "need_approval",
        &open_with(SIZE, vec![approval("ask", &[])]),
    );
    check(
        "need_approval_held",
        &open_with(
            SIZE,
            vec![approval("auto", &["db/migrate/0042_orders.sql"])],
        ),
    );
}

#[test]
fn the_card_of_the_first_write() {
    check(
        "need_first_write",
        &open_with(SIZE, vec![first_write("shadow")]),
    );
    check(
        "need_first_write_100x28",
        &open_with(SMALL, vec![first_write("shadow")]),
    );
    check(
        "need_first_write_hold",
        &open_with(SIZE, vec![first_write("hold")]),
    );
}

#[test]
fn the_card_of_the_first_write_while_its_chain_runs() {
    let mut app = open_with(SIZE, vec![first_write("shadow")]);
    let fx = key(&mut app, 3.0, Key::Char('y'));
    let chain = check_of(&fx);
    let fx = send(
        &mut app,
        3.2,
        Msg::Checked {
            chain,
            facts: Box::new(Ok(CheckFacts {
                mode: Some("shadow".into()),
                step: Some("refine".into()),
                flags: vec!["needs-you".into()],
                ..CheckFacts::default()
            })),
        },
    );
    let job = jobs(&fx).pop().expect("the promote runs");
    send(&mut app, 3.5, Msg::Tick);
    check("need_first_write_runs", &app);
    // The promote fails: the chain stops, and the card says what is true now.
    send(
        &mut app,
        4.0,
        Msg::JobDone(JobDone {
            id: job.id,
            kind: job.kind.clone(),
            result: ChildResult {
                code: Some(1),
                stderr: "error: the configuration is not valid TOML\n".into(),
                ..ChildResult::default()
            },
        }),
    );
    check("need_first_write_failed", &app);
}

#[test]
fn the_card_of_a_write_that_changed() {
    let mut app = open_with(SIZE, vec![approval("ask", &[])]);
    let fx = key(&mut app, 3.0, Key::Char('y'));
    let chain = check_of(&fx);
    let fx = send(
        &mut app,
        3.2,
        Msg::Checked {
            chain,
            facts: Box::new(Ok(CheckFacts {
                mode: Some("ask".into()),
                ops: vec![OpFacts {
                    id: "op-9b1c".into(),
                    state: "pending".into(),
                    head: HEAD.into(),
                    args: serde_json::json!({"text": "Another text."}),
                    waits: true,
                    detail: String::new(),
                }],
                ..CheckFacts::default()
            })),
        },
    );
    assert!(jobs(&fx).is_empty(), "nothing runs: {fx:?}");
    let load = fx
        .iter()
        .find_map(|e| match e {
            Effect::LoadNeeds(load) => Some(*load),
            _ => None,
        })
        .expect("the card loads again");
    let NeedItem::Approval(mut write) = approval("ask", &[]) else {
        unreachable!()
    };
    write.op = "op-9b1c".into();
    write.args = serde_json::json!({"text": "Another text."});
    send(
        &mut app,
        3.4,
        Msg::Needs {
            load,
            items: Box::new(Ok(vec![NeedItem::Approval(write)])),
        },
    );
    check("need_approval_changed", &app);
}

#[test]
fn the_cards_of_a_flag_and_of_a_pause() {
    check("need_flag", &open_with(SIZE, vec![flag(false)]));
    check("need_flag_stopped", &open_with(SIZE, vec![flag(true)]));
    check("need_paused", &open_with(SIZE, vec![paused(false)]));
    check("need_paused_stopped", &open_with(SIZE, vec![paused(true)]));
}

#[test]
fn the_card_of_a_proposal() {
    check("need_proposal", &open_with(SIZE, vec![proposal(true)]));
}

#[test]
fn the_card_of_a_notice_lists_its_issues() {
    check("need_notice", &open_with(SIZE, vec![notice_item(true)]));
    check(
        "need_notice_100x28",
        &open_with(SMALL, vec![notice_item(false), proposal(false)]),
    );
}

#[test]
fn the_card_while_it_loads_and_after_its_load_failed() {
    let mut app = home_with(
        SIZE,
        local(two_tickets(), vec![card_need("DEMO-12")]),
        Some(queue()),
    );
    let load = press_n(&mut app);
    check("need_loading", &app);
    send(
        &mut app,
        2.2,
        Msg::Needs {
            load,
            items: Box::new(Err("the ledger is locked".into())),
        },
    );
    check("need_load_error", &app);
}

#[test]
fn the_bar_of_home_names_the_first_write_and_not_a_rule() {
    use vision::snapshot::{Need, NeedKind};
    let mut held = needing("DEMO-12", "Export the report of the month", "refine");
    held.facts.first_write = Some("tracker.describe".into());
    let need = Need {
        kind: NeedKind::Flag,
        ticket: Some("DEMO-12".into()),
        id: "needs-you".into(),
        text: "DEMO-12 needs-you: tracker.describe is in the mode shadow: the executor recorded what it would do".into(),
    };
    let app = home_with(
        SIZE,
        local(vec![held.clone()], vec![need.clone()]),
        Some(queue()),
    );
    let text = screen(&app);
    assert!(
        text.contains("DEMO-12 · the first write tracker.describe waits for your decision"),
        "{text}"
    );
    // Another flag of the ticket keeps the words of a rule.
    held.facts.first_write = None;
    let app = home_with(SIZE, local(vec![held], vec![need]), Some(queue()));
    let text = screen(&app);
    assert!(
        text.contains("DEMO-12 · a rule stopped it: tracker.describe is in the mode shadow"),
        "{text}"
    );
}
