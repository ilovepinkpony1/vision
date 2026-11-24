//! A text snapshot of each card of the things that wait, in each of its states, at 110 by 32,
//! and the largest cards also at 100 by 28: the question form (idle, with and without a
//! proposal of the Judge, the line of the words, the line of the reason, the question of a
//! needless card, a page with a draft, the review page, its send while it runs and after a
//! refusal), the plan (also after it changed), a write that
//! waits, the first write (also while its chain runs), a rule that stopped a ticket, a paused
//! ticket, a proposal of the Curator, a notice of a gate, and the card while it loads or after
//! its load failed.

use vision::app::chain::{CheckFacts, OpFacts};
use vision::app::hits::Act;
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

/// The card of the first write in a window of `size`, after its promote failed with the text
/// `stderr`.
fn first_write_failed(size: (u16, u16), stderr: String) -> App {
    let mut app = open_with(size, vec![first_write("shadow")]);
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
    send(
        &mut app,
        4.0,
        Msg::JobDone(JobDone {
            id: job.id,
            kind: job.kind.clone(),
            result: ChildResult {
                code: Some(1),
                stderr,
                ..ChildResult::default()
            },
        }),
    );
    app
}

#[test]
fn a_long_reason_of_a_failure_keeps_the_card_in_the_window() {
    // Thirty lines of standard error: the foot of the card takes the rows that leave one row of
    // the body, the reason shows its first lines, and the state and the keys stay in the card.
    let stderr: String = (1..=30)
        .map(|n| format!("error: line {n} of a long error that the command wrote\n"))
        .collect();
    for size in [SMALL, SIZE] {
        let app = first_write_failed(size, stderr.clone());
        let text = screen(&app);
        let rows: Vec<&str> = text.lines().collect();
        let h = usize::from(size.1);
        let bottom = rows
            .iter()
            .rposition(|r| r.contains('╰'))
            .unwrap_or_else(|| panic!("no bottom of the card:\n{text}"));
        assert!(bottom <= h - 2, "{text}");
        let keys = rows
            .iter()
            .position(|r| r.contains("y a n choose"))
            .unwrap();
        let now = rows
            .iter()
            .position(|r| r.contains("Now  Nothing was sent"))
            .unwrap();
        let cut = rows
            .iter()
            .position(|r| r.contains("The rest of the text is cut."))
            .unwrap_or_else(|| panic!("no cut:\n{text}"));
        assert!(cut < now && now < keys && keys < bottom, "{text}");
        assert!(text.contains("line 1 of a long error"), "{text}");
        assert!(!text.contains("line 30 of a long error"), "{text}");
        assert!(
            text.contains("▼"),
            "the body shows its first row and a mark:\n{text}"
        );
        // The keys of the foot are parts that a click can hit.
        let mut drawn = app.clone();
        draw(&mut drawn);
        let at = rows[keys].find("y a n").unwrap();
        let x = rows[keys][..at].chars().count() as u16;
        assert_eq!(
            drawn.hits.click_at(x, keys as u16),
            Some(&Act::Decide(Key::Char('y')))
        );
    }
    check(
        "need_first_write_long_failure_100x28",
        &first_write_failed(SMALL, stderr),
    );
    // A short reason shows whole, with no cut.
    let app = first_write_failed(SMALL, "error: the configuration is not valid TOML\n".into());
    assert!(!screen(&app).contains("The rest of the text is cut."));
}

#[test]
fn the_question_form_with_a_proposal_of_the_judge() {
    let items = vec![one(true), plan_item(false)];
    check("need_question", &open_with(SIZE, items.clone()));
    check("need_question_100x28", &open_with(SMALL, items));
}

#[test]
fn the_question_form_without_a_proposal() {
    let app = open_with(SIZE, vec![one(false)]);
    check("need_question_no_judge", &app);
}

#[test]
fn the_question_form_with_its_lines() {
    let mut app = open_with(SIZE, vec![one(false)]);
    key(&mut app, 3.0, Key::Char('e'));
    type_text(&mut app, 3.1, "Keep the order, and put the total last");
    check("need_question_words", &app);
    key(&mut app, 3.2, Key::Enter);
    type_text(&mut app, 3.3, "The team reads the file next to the table");
    check("need_question_why_text", &app);

    let mut app = open_with(SIZE, vec![one(false)]);
    key(&mut app, 3.0, Key::Char('b'));
    check("need_question_why", &app);

    let mut app = open_with(SIZE, vec![one(false)]);
    key(&mut app, 3.0, Key::Char('k'));
    check("need_question_needless", &app);
}

#[test]
fn the_form_of_three_questions_on_a_page_with_a_draft_and_on_its_review_page() {
    let mut app = open_with(SIZE, vec![form(three())]);
    check("need_form_question", &app);
    key(&mut app, 3.0, Key::Char('b'));
    type_text(&mut app, 3.1, "People compare the file with the table");
    key(&mut app, 3.2, Key::Enter);
    // The form goes to the next question with no draft; the first one shows its draft.
    key(&mut app, 3.3, Key::Left);
    check("need_form_draft", &app);
    key(&mut app, 3.4, Key::Char('3'));
    key(&mut app, 3.5, Key::Enter);
    key(&mut app, 3.6, Key::Tab);
    check("need_form_review", &app);
    let mut small = open_with(SMALL, vec![form(three())]);
    key(&mut small, 3.0, Key::Char('r'));
    check("need_form_review_100x28", &small);
}

#[test]
fn the_question_form_while_its_send_runs_and_after_a_refusal() {
    let mut app = open_with(SIZE, vec![one(true)]);
    key(&mut app, 3.0, Key::Enter);
    key(&mut app, 3.1, Key::Tab);
    let fx = key(&mut app, 3.2, Key::Char('s'));
    let job = jobs(&fx).pop().expect("the send runs");
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
                stderr: "error: no answer was recorded, because the answer to Q-0002 was refused: Q-0002 already has an answer\n".into(),
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
    let NeedItem::Approval(mut big) = approval("auto", &[]) else {
        unreachable!()
    };
    big.oversize =
        Some("the diff changes 4252 lines, more than gates.max_diff_lines (3000)".into());
    check(
        "need_approval_oversize",
        &open_with(SIZE, vec![NeedItem::Approval(big)]),
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
fn the_card_of_a_finding_that_keeps_coming_back() {
    check(
        "need_notice_back",
        &open_with(SIZE, vec![back_notice_item()]),
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
