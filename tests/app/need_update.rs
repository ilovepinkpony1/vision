//! The update of the cards of the things that wait: each key of each card, the exact arguments
//! of each command that a key runs, the guards of the chains (the same plan, the same write,
//! the same mode), the passes that a chain waits for, a step that fails, and the move between
//! the cards. The worker and the reads are played by [`drive`].

use vision::app::chain::{Check, CheckFacts, End, Owner};
use vision::app::model::{App, Card, Screen};
use vision::app::msg::{Action, Effect, Given, Key, Msg};
use vision::app::needs::{Input, NeedItem, Reply};

use super::need_support::*;
use super::support::*;

const SIZE: (u16, u16) = (110, 32);

fn key(app: &mut App, at: f64, k: Key) -> Vec<Effect> {
    send(app, at, Msg::Key(k))
}

fn input(app: &App) -> Input {
    need_card(app).input.clone()
}

fn notice(app: &App) -> String {
    app.notice
        .as_ref()
        .map(|n| n.text.clone())
        .unwrap_or_default()
}

/// The end of the chain that shows, with its reason and the state that it names.
fn failed(app: &App) -> (String, String) {
    match app.chain.as_ref().and_then(|c| c.end.clone()) {
        Some(End::Failed { text, state }) => (text, state),
        other => panic!("the chain did not fail: {other:?}"),
    }
}

fn none(_: &Check) -> CheckFacts {
    CheckFacts::default()
}

fn args(list: &[&str]) -> Vec<String> {
    list.iter().map(|s| s.to_string()).collect()
}

#[test]
fn the_arguments_of_each_answer() {
    let answer = |given: Given, why: &str| {
        Action::Answer {
            id: "Q-0002".into(),
            given,
            why: why.into(),
        }
        .args()
    };
    assert_eq!(
        answer(Given::Choice(1), ""),
        ["answer", "Q-0002", "--choice", "1"]
    );
    assert_eq!(
        answer(Given::Choice(2), " People compare the file. "),
        [
            "answer",
            "Q-0002",
            "--choice",
            "2",
            "--why=People compare the file."
        ]
    );
    assert_eq!(
        answer(Given::Text("Keep it".into()), ""),
        ["answer", "Q-0002", "--text=Keep it"]
    );
    assert_eq!(
        answer(Given::Text("-1 is fine".into()), "-x reads as a text"),
        [
            "answer",
            "Q-0002",
            "--text=-1 is fine",
            "--why=-x reads as a text"
        ]
    );
    // A needless card takes no reason: the command refuses `--why` with `--needless`.
    assert_eq!(
        answer(Given::Needless("code".into()), "ignored"),
        ["answer", "Q-0002", "--needless", "code"]
    );
}

#[test]
fn a_letter_then_the_reason_runs_the_answer_with_its_why() {
    let mut app = open_with(SIZE, vec![NeedItem::Question(question(false))]);
    key(&mut app, 3.0, Key::Char('b'));
    assert!(matches!(
        input(&app),
        Input::Why {
            reply: Reply::Choice(1),
            ..
        }
    ));
    type_text(&mut app, 3.1, "People compare the file with the table");
    let fx = key(&mut app, 3.2, Key::Enter);
    let done = drive(&mut app, 3.3, fx, None, none);
    assert_eq!(
        done.actions,
        [args(&[
            "answer",
            "Q-0002",
            "--choice",
            "2",
            "--why=People compare the file with the table"
        ])]
    );
    // The only item is answered: the card closes with the notice.
    assert_eq!(app.card, None);
    assert_eq!(
        notice(&app),
        "Saved your answer to Q-0002 with its reason. builder gets it when each question of its turn has an answer."
    );
}

#[test]
fn tab_saves_the_answer_with_no_reason() {
    let mut app = open_with(SIZE, vec![NeedItem::Question(question(false))]);
    key(&mut app, 3.0, Key::Char('a'));
    type_text(&mut app, 3.1, "this text does not go");
    let fx = key(&mut app, 3.2, Key::Tab);
    let done = drive(&mut app, 3.3, fx, None, none);
    assert_eq!(done.actions, [args(&["answer", "Q-0002", "--choice", "1"])]);
}

#[test]
fn enter_takes_the_proposal_of_the_judge_or_the_recommended_choice() {
    let mut judged = question(true);
    judged.proposal.as_mut().unwrap().choice = Some(2);
    let mut app = open_with(SIZE, vec![NeedItem::Question(judged)]);
    key(&mut app, 3.0, Key::Enter);
    let fx = key(&mut app, 3.1, Key::Tab);
    let done = drive(&mut app, 3.2, fx, None, none);
    assert_eq!(done.actions, [args(&["answer", "Q-0002", "--choice", "2"])]);

    // A proposal in words, with no choice, goes as a text.
    let mut words = question(true);
    let proposal = words.proposal.as_mut().unwrap();
    proposal.choice = None;
    proposal.answer = "Keep the order, then sort the rest".into();
    let mut app = open_with(SIZE, vec![NeedItem::Question(words)]);
    key(&mut app, 3.0, Key::Enter);
    let fx = key(&mut app, 3.1, Key::Tab);
    let done = drive(&mut app, 3.2, fx, None, none);
    assert_eq!(
        done.actions,
        [args(&[
            "answer",
            "Q-0002",
            "--text=Keep the order, then sort the rest"
        ])]
    );

    let mut app = open_with(SIZE, vec![NeedItem::Question(question(false))]);
    key(&mut app, 3.0, Key::Enter);
    let fx = key(&mut app, 3.1, Key::Tab);
    let done = drive(&mut app, 3.2, fx, None, none);
    assert_eq!(done.actions, [args(&["answer", "Q-0002", "--choice", "1"])]);

    // No proposal and no recommended choice: Enter says what to press.
    let mut plain = question(false);
    plain.recommended = None;
    let mut app = open_with(SIZE, vec![NeedItem::Question(plain)]);
    let fx = key(&mut app, 3.0, Key::Enter);
    assert!(jobs(&fx).is_empty());
    assert_eq!(input(&app), Input::None);
    assert_eq!(
        notice(&app),
        "This question has no recommended choice. Press the letter of a choice."
    );
}

#[test]
fn e_takes_an_answer_in_the_words_of_the_person() {
    let mut app = open_with(SIZE, vec![NeedItem::Question(question(false))]);
    key(&mut app, 3.0, Key::Char('e'));
    assert!(matches!(input(&app), Input::Words(_)));
    // An empty answer does not go.
    key(&mut app, 3.1, Key::Enter);
    assert!(matches!(input(&app), Input::Words(_)));
    assert_eq!(notice(&app), "The answer is empty. Type it, or press Esc.");
    type_text(&mut app, 3.2, "Keep the order");
    send(&mut app, 3.3, Msg::Paste(", and put the total last".into()));
    key(&mut app, 3.4, Key::Enter);
    assert!(matches!(
        input(&app),
        Input::Why { reply: Reply::Text(ref t), .. } if t == "Keep the order, and put the total last"
    ));
    type_text(&mut app, 3.5, "The team reads it");
    let fx = key(&mut app, 3.6, Key::Enter);
    let done = drive(&mut app, 3.7, fx, None, none);
    assert_eq!(
        done.actions,
        [args(&[
            "answer",
            "Q-0002",
            "--text=Keep the order, and put the total last",
            "--why=The team reads it"
        ])]
    );
}

#[test]
fn k_marks_the_question_as_needless_with_its_place() {
    for (letter, place) in [
        ('c', "code"),
        ('t', "ticket"),
        ('m', "memory"),
        ('n', "no-matter"),
    ] {
        let mut app = open_with(SIZE, vec![NeedItem::Question(question(false))]);
        key(&mut app, 3.0, Key::Char('k'));
        assert_eq!(input(&app), Input::Needless);
        // Another key does nothing.
        assert!(jobs(&key(&mut app, 3.05, Key::Char('z'))).is_empty());
        let fx = key(&mut app, 3.1, Key::Char(letter));
        let done = drive(&mut app, 3.2, fx, None, none);
        assert_eq!(
            done.actions,
            [args(&["answer", "Q-0002", "--needless", place])]
        );
    }
}

#[test]
fn esc_and_ctrl_c_go_back_one_step_while_the_person_types() {
    let mut app = open_with(SIZE, vec![NeedItem::Question(question(false))]);
    key(&mut app, 3.0, Key::Char('a'));
    key(&mut app, 3.1, Key::Esc);
    assert_eq!(input(&app), Input::None);
    key(&mut app, 3.2, Key::Char('e'));
    key(&mut app, 3.3, Key::Cancel);
    assert_eq!(input(&app), Input::None);
    assert!(matches!(app.card, Some(Card::Need(_))));
    key(&mut app, 3.4, Key::Char('k'));
    key(&mut app, 3.5, Key::Esc);
    assert_eq!(input(&app), Input::None);
    // With no line open, Esc closes the card, and the question keeps waiting.
    key(&mut app, 3.6, Key::Esc);
    assert_eq!(app.card, None);
}

#[test]
fn a_letter_of_a_choice_wins_over_a_key_of_the_card() {
    let mut view = question(false);
    for (n, letter) in ['c', 'd', 'e', 'f', 'g', 'h', 'i', 'j', 'k']
        .into_iter()
        .enumerate()
    {
        view.choices.push(vision::questions::ChoiceView {
            n: n + 3,
            letter: Some(letter),
            label: format!("Choice {}", n + 3),
            means: String::new(),
        });
    }
    let mut app = open_with(SIZE, vec![NeedItem::Question(view)]);
    key(&mut app, 3.0, Key::Char('e'));
    assert!(matches!(
        input(&app),
        Input::Why {
            reply: Reply::Choice(4),
            ..
        }
    ));
    key(&mut app, 3.1, Key::Esc);
    key(&mut app, 3.2, Key::Char('t'));
    assert!(matches!(input(&app), Input::Words(_)));
    key(&mut app, 3.3, Key::Esc);
    key(&mut app, 3.4, Key::Char('-'));
    assert_eq!(input(&app), Input::Needless);
}

#[test]
fn after_an_answer_the_next_card_shows() {
    let mut app = open_with(
        SIZE,
        vec![NeedItem::Question(question(false)), plan_item(false)],
    );
    assert_eq!(need_card(&app).len(), 2);
    key(&mut app, 3.0, Key::Char('a'));
    let fx = key(&mut app, 3.1, Key::Tab);
    let done = drive(&mut app, 3.2, fx.clone(), None, none);
    assert_eq!(done.actions.len(), 1);
    let card = need_card(&app);
    assert_eq!(card.len(), 1);
    assert!(matches!(card.item(), Some(NeedItem::Plan(_))));
    assert!(notice(&app).starts_with("Saved your answer to Q-0002."));
}

#[test]
fn the_arrows_move_between_the_cards_and_scroll_a_long_one() {
    let mut app = open_with(
        (100, 28),
        vec![
            NeedItem::Question(question(false)),
            plan_item(false),
            flag(false),
        ],
    );
    key(&mut app, 3.0, Key::Left);
    assert_eq!(need_card(&app).at, 0);
    key(&mut app, 3.1, Key::Right);
    assert_eq!(need_card(&app).at, 1);
    key(&mut app, 3.2, Key::Down);
    key(&mut app, 3.3, Key::Down);
    assert_eq!(need_card(&app).scroll, 2);
    key(&mut app, 3.4, Key::Up);
    assert_eq!(need_card(&app).scroll, 1);
    key(&mut app, 3.5, Key::End);
    let end = need_card(&app).scroll;
    assert!(end > 1);
    key(&mut app, 3.6, Key::Down);
    assert_eq!(need_card(&app).scroll, end, "the scroll stops at the end");
    key(&mut app, 3.7, Key::Home);
    assert_eq!(need_card(&app).scroll, 0);
    key(&mut app, 3.8, Key::Right);
    key(&mut app, 3.9, Key::Right);
    assert_eq!(need_card(&app).at, 2);
    assert_eq!(need_card(&app).scroll, 0);
}

#[test]
fn q_on_a_card_asks_to_quit() {
    let mut app = open_with(SIZE, vec![plan_item(false)]);
    key(&mut app, 3.0, Key::Char('q'));
    assert_eq!(app.card, Some(Card::Quit));
}

#[test]
fn a_load_from_the_live_view_keeps_only_the_items_of_its_ticket() {
    let held = needing(
        "DEMO-12",
        "Export the report of the month as a CSV file",
        "build",
    );
    let mut app =
        super::live_support::live(SIZE, vec![held], vec![card_need("DEMO-12")], Vec::new());
    let fx = key(&mut app, 3.0, Key::Char('n'));
    let load = fx
        .iter()
        .find_map(|e| match e {
            Effect::LoadNeeds(load) => Some(*load),
            _ => None,
        })
        .unwrap();
    let mut other = flag_item(false);
    other.ticket = "DEMO-7".into();
    send(
        &mut app,
        3.1,
        Msg::Needs {
            load,
            items: Box::new(Ok(vec![NeedItem::Flag(other), plan_item(false)])),
        },
    );
    assert_eq!(need_card(&app).len(), 1);
    assert!(matches!(need_card(&app).item(), Some(NeedItem::Plan(_))));
    // An older load counts for nothing.
    send(
        &mut app,
        3.2,
        Msg::Needs {
            load: load - 1,
            items: Box::new(Ok(Vec::new())),
        },
    );
    assert_eq!(need_card(&app).len(), 1);
    // A load with nothing of the ticket closes the card.
    let needs = vec![card_need("DEMO-12"), plan_need("DEMO-12")];
    let fx = send(&mut app, 3.3, Msg::Local(Ok(local(two_tickets(), needs))));
    let load = fx
        .iter()
        .find_map(|e| match e {
            Effect::LoadNeeds(load) => Some(*load),
            _ => None,
        })
        .expect("a change of the needs loads the card again");
    send(
        &mut app,
        3.4,
        Msg::Needs {
            load,
            items: Box::new(Ok(Vec::new())),
        },
    );
    assert_eq!(app.card, None);
    assert_eq!(notice(&app), "Nothing of DEMO-12 waits for you now.");
}

#[test]
fn a_approves_the_plan_with_the_hash_that_the_card_showed() {
    let mut app = open_with(SIZE, vec![plan_item(false)]);
    let fx = key(&mut app, 3.0, Key::Char('a'));
    let done = drive(&mut app, 3.1, fx, None, |_| CheckFacts {
        plan: Some(PLAN_HASH.into()),
        ..CheckFacts::default()
    });
    assert_eq!(
        done.checks,
        [Check::Plan {
            ticket: "DEMO-12".into(),
            hash: PLAN_HASH.into()
        }]
    );
    assert_eq!(
        done.actions,
        [args(&["approve-plan", "DEMO-12", PLAN_HASH])]
    );
    assert_eq!(app.card, None);
    assert_eq!(
        notice(&app),
        "Approved the plan 3f2a1b4c5d6e. DEMO-12 moves on to its next step at the next pass."
    );
}

#[test]
fn a_plan_that_changed_is_not_approved() {
    let mut app = open_with(SIZE, vec![plan_item(false)]);
    let fx = key(&mut app, 3.0, Key::Char('a'));
    let done = drive(&mut app, 3.1, fx, None, |_| CheckFacts {
        plan: Some("9".repeat(64)),
        ..CheckFacts::default()
    });
    assert!(done.actions.is_empty(), "{done:?}");
    let card = need_card(&app);
    assert_eq!(card.select.as_deref(), Some("plan:DEMO-12"));
    assert!(card
        .note
        .as_deref()
        .unwrap()
        .starts_with("The plan changed after the card showed it, so Vision did not approve it."));
    assert_eq!(app.chain, None);

    // No plan at all stops the chain with its reason.
    let mut app = open_with(SIZE, vec![plan_item(false)]);
    let fx = key(&mut app, 3.0, Key::Char('a'));
    let done = drive(&mut app, 3.1, fx, None, none);
    assert!(done.actions.is_empty());
    assert_eq!(failed(&app).0, "DEMO-12 has no plan now.");
}

#[test]
fn a_plan_that_changes_under_an_open_card_says_so() {
    let mut app = open_with(SIZE, vec![plan_item(false)]);
    key(&mut app, 3.0, Key::Down);
    assert_eq!(need_card(&app).scroll, 1);
    let NeedItem::Plan(mut plan) = plan_item(true) else {
        unreachable!()
    };
    plan.plan.hash = "9".repeat(64);
    plan.plan.text.push_str("\n- One more step.");
    let load = need_card(&app).load;
    send(
        &mut app,
        3.1,
        Msg::Needs {
            load,
            items: Box::new(Ok(vec![NeedItem::Plan(plan)])),
        },
    );
    let card = need_card(&app);
    assert_eq!(card.scroll, 0, "the new plan shows from its start");
    assert_eq!(
        card.note.as_deref(),
        Some("The plan changed while this card was open. Read the new plan, then press a.")
    );
    // The same plan again changes nothing.
    key(&mut app, 3.2, Key::Down);
    let same = need_card(&app).item().cloned().unwrap();
    send(
        &mut app,
        3.3,
        Msg::Needs {
            load,
            items: Box::new(Ok(vec![same])),
        },
    );
    assert_eq!(need_card(&app).scroll, 1);
}

#[test]
fn x_on_the_plan_opens_the_stop_card() {
    let mut app = open_with(SIZE, vec![plan_item(false)]);
    key(&mut app, 3.0, Key::Char('x'));
    let Some(Card::Stop(card)) = &app.card else {
        panic!("no stop card: {:?}", app.card)
    };
    assert_eq!(card.key, "DEMO-12");
    assert_eq!(card.step, "plan-ready");
}

#[test]
fn y_approves_the_write_with_the_verb_and_the_commit_that_the_card_showed() {
    let mut app = open_with(SIZE, vec![approval("ask", &[])]);
    let fx = key(&mut app, 3.0, Key::Char('y'));
    let done = drive(&mut app, 3.1, fx, None, |_| {
        facts("comments", "ask", vec![op_facts("pending", true)])
    });
    assert_eq!(
        done.actions,
        [args(&[
            "approve",
            "DEMO-12",
            "tracker.describe",
            HEAD,
            "--op",
            OP
        ])]
    );
    assert_eq!(notice(&app), "Sent: tracker.describe of DEMO-12 ran.");
}

#[test]
fn a_approves_the_write_then_moves_its_verb_to_auto() {
    let mut app = open_with(SIZE, vec![approval("ask", &[])]);
    let fx = key(&mut app, 3.0, Key::Char('a'));
    let done = drive(&mut app, 3.1, fx, None, |_| {
        facts("comments", "ask", vec![op_facts("pending", true)])
    });
    assert_eq!(
        done.actions,
        [
            args(&["approve", "DEMO-12", "tracker.describe", HEAD, "--op", OP]),
            args(&["promote", "tracker.describe"])
        ]
    );
    assert!(notice(&app).contains("tracker.describe is in the mode auto now."));

    // A held write in the mode auto has no `a`.
    let mut app = open_with(SIZE, vec![approval("auto", &["db/schema.sql"])]);
    let fx = key(&mut app, 3.0, Key::Char('a'));
    assert!(jobs(&fx).is_empty());
    assert_eq!(app.chain, None);
}

#[test]
fn a_write_that_changed_is_not_approved_and_a_second_write_does_not_stop_the_approval() {
    let mut app = open_with(SIZE, vec![approval("ask", &[])]);
    let fx = key(&mut app, 3.0, Key::Char('y'));
    let mut changed = op_facts("pending", true);
    changed.args = serde_json::json!({"text": "Another text."});
    let done = drive(&mut app, 3.1, fx, None, |_| {
        facts("comments", "ask", vec![changed.clone()])
    });
    assert!(done.actions.is_empty(), "{done:?}");
    assert!(need_card(&app)
        .note
        .as_deref()
        .unwrap()
        .contains("Vision did not approve it."));

    let mut app = open_with(SIZE, vec![approval("ask", &[])]);
    let fx = key(&mut app, 3.0, Key::Char('y'));
    let mut other = op_facts("pending", true);
    other.id = "op-2".into();
    let done = drive(&mut app, 3.1, fx, None, |_| {
        facts(
            "comments",
            "ask",
            vec![op_facts("pending", true), other.clone()],
        )
    });
    // A second write of the verb at that commit does not stop the approval: it names the
    // operation of the card, and the other write keeps waiting.
    assert_eq!(
        done.actions,
        [args(&[
            "approve",
            "DEMO-12",
            "tracker.describe",
            HEAD,
            "--op",
            OP
        ])]
    );

    // A write that no longer waits stops the chain, and says so.
    let mut app = open_with(SIZE, vec![approval("ask", &[])]);
    let fx = key(&mut app, 3.0, Key::Char('y'));
    let done = drive(&mut app, 3.1, fx, None, |_| {
        facts("comments", "ask", vec![op_facts("confirmed", false)])
    });
    assert!(done.actions.is_empty());
    assert_eq!(failed(&app).0, "tracker.describe was already sent.");
}

/// The facts of the chain of the first write: the verb climbs one step at each promote.
fn ladder(modes: &'static [&'static str]) -> impl Fn(&Check) -> CheckFacts {
    move |check| match check {
        Check::Ready { .. } => facts("refine", modes[0], Vec::new()),
        Check::Mode { mode, .. } => facts("refine", mode, Vec::new()),
        Check::Waits { .. } => facts("refine", "ask", vec![op_facts("pending", true)]),
        Check::Sent { .. } => facts("plan", "auto", vec![op_facts("confirmed", false)]),
        Check::Plan { .. } => CheckFacts::default(),
    }
}

#[test]
fn y_on_the_first_write_promotes_retries_waits_for_the_write_and_approves_it() {
    let mut app = open_with(SIZE, vec![first_write("shadow")]);
    let fx = key(&mut app, 3.0, Key::Char('y'));
    let done = drive(&mut app, 3.1, fx, None, ladder(&["shadow"]));
    assert_eq!(
        done.actions,
        [
            args(&["promote", "tracker.describe"]),
            args(&["retry", "DEMO-12"]),
            args(&["approve", "DEMO-12", "tracker.describe", HEAD, "--op", OP])
        ]
    );
    assert!(done.passes >= 1);
    assert!(matches!(done.checks[0], Check::Ready { ref mode, .. } if mode == "shadow"));
    assert!(matches!(
        done.checks.last(),
        Some(Check::Waits { passes: true, ref head, .. }) if head == HEAD
    ));
    assert_eq!(app.card, None);
    assert_eq!(
        notice(&app),
        "Sent: tracker.describe of DEMO-12 ran. Vision asks you each time for tracker.describe."
    );
}

#[test]
fn y_on_a_verb_in_hold_climbs_two_steps_with_a_check_between_them() {
    let mut app = open_with(SIZE, vec![first_write("hold")]);
    let fx = key(&mut app, 3.0, Key::Char('y'));
    let done = drive(&mut app, 3.1, fx, None, ladder(&["hold"]));
    assert_eq!(
        done.actions,
        [
            args(&["promote", "tracker.describe"]),
            args(&["promote", "tracker.describe"]),
            args(&["retry", "DEMO-12"]),
            args(&["approve", "DEMO-12", "tracker.describe", HEAD, "--op", OP])
        ]
    );
    assert!(done
        .checks
        .iter()
        .any(|c| matches!(c, Check::Mode { mode, .. } if mode == "shadow")));
}

#[test]
fn a_on_the_first_write_moves_the_verb_to_auto_and_waits_until_it_ran() {
    let mut app = open_with(SIZE, vec![first_write("shadow")]);
    let fx = key(&mut app, 3.0, Key::Char('a'));
    let done = drive(&mut app, 3.1, fx, None, ladder(&["shadow"]));
    assert_eq!(
        done.actions,
        [
            args(&["promote", "tracker.describe"]),
            args(&["promote", "tracker.describe"]),
            args(&["retry", "DEMO-12"])
        ]
    );
    assert!(matches!(done.checks.last(), Some(Check::Sent { .. })));
    assert!(notice(&app).starts_with(
        "Sent: tracker.describe of DEMO-12 ran. tracker.describe is in the mode auto now."
    ));
}

#[test]
fn n_on_the_first_write_leaves_it_as_it_is() {
    let mut app = open_with(SIZE, vec![first_write("shadow")]);
    let fx = key(&mut app, 3.0, Key::Char('n'));
    assert!(jobs(&fx).is_empty());
    assert_eq!(app.card, None);
    assert_eq!(app.chain, None);
}

#[test]
fn a_mode_that_changed_stops_the_chain_before_it_acts() {
    let mut app = open_with(SIZE, vec![first_write("shadow")]);
    let fx = key(&mut app, 3.0, Key::Char('y'));
    let done = drive(&mut app, 3.1, fx, None, ladder(&["ask"]));
    assert!(done.actions.is_empty(), "{done:?}");
    let (text, state) = failed(&app);
    assert_eq!(
        text,
        "tracker.describe is in the mode ask now, not in the mode shadow, so Vision does not change it."
    );
    assert_eq!(
        state,
        "Nothing was sent, and DEMO-12 still waits at the step refine."
    );
}

#[test]
fn a_failed_step_stops_the_chain_and_names_the_state() {
    let mut app = open_with(SIZE, vec![first_write("shadow")]);
    let fx = key(&mut app, 3.0, Key::Char('y'));
    let done = drive(&mut app, 3.1, fx, Some("retry"), ladder(&["shadow"]));
    assert_eq!(
        done.actions,
        [
            args(&["promote", "tracker.describe"]),
            args(&["retry", "DEMO-12"])
        ]
    );
    let (text, state) = failed(&app);
    assert_eq!(text, "the command failed in the test");
    assert_eq!(
        state,
        "tracker.describe is in the mode ask now. Nothing was sent, and DEMO-12 still waits at the step refine."
    );
    // The card still shows, with the end of the chain, and its keys act again.
    assert!(matches!(
        need_card(&app).item(),
        Some(NeedItem::FirstWrite(_))
    ));
}

#[test]
fn the_chain_stops_after_three_passes_that_did_not_ask_for_the_write() {
    let mut app = open_with(SIZE, vec![first_write("shadow")]);
    let fx = key(&mut app, 3.0, Key::Char('y'));
    // The ticket goes on, and no pass asks for the write again.
    let done = drive(&mut app, 3.1, fx, None, |check| match check {
        Check::Waits { .. } => CheckFacts {
            mode: Some("ask".into()),
            step: Some("refine".into()),
            ..CheckFacts::default()
        },
        other => ladder(&["shadow"])(other),
    });
    assert_eq!(done.passes, 3);
    assert_eq!(
        done.actions,
        [
            args(&["promote", "tracker.describe"]),
            args(&["retry", "DEMO-12"])
        ]
    );
    assert!(failed(&app)
        .0
        .starts_with("3 passes ran, and none of them asked for tracker.describe again."));
}

#[test]
fn the_pass_of_a_chain_runs_while_the_engine_is_paused_and_a_check_holds_the_worker() {
    let mut app = home_with(
        SIZE,
        local(two_tickets(), vec![card_need("DEMO-12")]),
        Some(queue()),
    );
    // Pause the engine on Home, then open the card.
    key(&mut app, 1.5, Key::Char('p'));
    assert!(app.engine.paused);
    let load = press_n(&mut app);
    send(
        &mut app,
        2.1,
        Msg::Needs {
            load,
            items: Box::new(Ok(vec![first_write("shadow")])),
        },
    );
    let fx = key(&mut app, 3.0, Key::Char('y'));
    // While the read of the check runs, no job starts.
    assert!(jobs(&fx).is_empty());
    let done = drive(&mut app, 3.1, fx, None, ladder(&["shadow"]));
    assert_eq!(done.actions.len(), 3, "{done:?}");
    assert!(done.passes >= 1);
    assert!(app.engine.paused);
}

#[test]
fn the_keys_of_a_flag() {
    let mut app = open_with(SIZE, vec![flag(false)]);
    let fx = key(&mut app, 3.0, Key::Char('r'));
    let done = drive(&mut app, 3.1, fx, None, none);
    assert_eq!(done.actions, [args(&["retry", "DEMO-12"])]);
    assert_eq!(notice(&app), "DEMO-12 goes on at the step build.");

    let mut app = open_with(SIZE, vec![flag(false)]);
    key(&mut app, 3.0, Key::Char('o'));
    let Some(Card::Open(card)) = &app.card else {
        panic!("no open card: {:?}", app.card)
    };
    assert_eq!(card.last.as_deref(), Some("builder"));

    let mut app = open_with(SIZE, vec![flag(false)]);
    key(&mut app, 3.0, Key::Char('x'));
    assert!(matches!(&app.card, Some(Card::Stop(c)) if c.step == "build"));

    // A stopped ticket: Enter opens the card that starts it again, and r does nothing.
    let mut app = open_with(SIZE, vec![flag(true)]);
    assert!(jobs(&key(&mut app, 3.0, Key::Char('r'))).is_empty());
    assert!(matches!(app.card, Some(Card::Need(_))));
    key(&mut app, 3.1, Key::Enter);
    assert!(matches!(&app.card, Some(Card::Resume(c)) if c.key == "DEMO-12"));

    // With no turn yet, o says that there is no session.
    let mut item = flag_item(false);
    item.last = None;
    let mut app = open_with(SIZE, vec![NeedItem::Flag(item)]);
    key(&mut app, 3.0, Key::Char('o'));
    assert!(matches!(app.card, Some(Card::Need(_))));
    assert_eq!(
        notice(&app),
        "DEMO-12 has no turn yet, so there is no session to open."
    );
}

#[test]
fn the_keys_of_a_paused_ticket() {
    let mut app = open_with(SIZE, vec![paused(false)]);
    let fx = key(&mut app, 3.0, Key::Enter);
    let done = drive(&mut app, 3.1, fx, None, none);
    assert_eq!(done.actions, [args(&["retry", "DEMO-12"])]);

    let mut app = open_with(SIZE, vec![paused(true)]);
    let fx = key(&mut app, 3.0, Key::Char('r'));
    let done = drive(&mut app, 3.1, fx, None, none);
    assert_eq!(
        done.actions,
        [args(&["take", "DEMO-12"]), args(&["retry", "DEMO-12"])]
    );
    assert_eq!(
        notice(&app),
        "DEMO-12 goes on. Its next turn resumes the session that you worked in."
    );
}

#[test]
fn enter_marks_a_notice_read_and_the_next_thing_shows() {
    let mut app = open_with(SIZE, vec![notice_item(false), proposal(false)]);
    // Another key does nothing on a notice.
    assert!(jobs(&key(&mut app, 2.9, Key::Char('r'))).is_empty());
    let fx = key(&mut app, 3.0, Key::Enter);
    let done = drive(&mut app, 3.1, fx, None, none);
    assert_eq!(done.actions, [args(&["needs", "--read", "N-1"])]);
    assert_eq!(
        notice(&app),
        "Marked the notice N-1 as read. DEMO-12 goes on as before."
    );
    let card = need_card(&app);
    assert_eq!(card.len(), 1);
    assert!(matches!(card.item(), Some(NeedItem::Proposal(_))));

    // A failed read keeps the notice on the card.
    let mut app = open_with(SIZE, vec![notice_item(false)]);
    let fx = key(&mut app, 3.0, Key::Enter);
    drive(&mut app, 3.1, fx, Some("needs"), none);
    assert!(matches!(need_card(&app).item(), Some(NeedItem::Notice(_))));
    assert_eq!(
        Action::ReadNotice { id: "N-1".into() }.label(),
        "The read of the notice N-1"
    );
}

#[test]
fn enter_on_a_proposal_opens_its_review_and_the_card_comes_back() {
    let mut app = open_with(SIZE, vec![proposal(false)]);
    let fx = key(&mut app, 3.0, Key::Enter);
    assert!(fx.contains(&Effect::OpenReview("P-0003".into())), "{fx:?}");
    let fx = send(
        &mut app,
        3.5,
        Msg::ReviewEnded {
            proposal: "P-0003".into(),
            result: vision::app::msg::ChildResult {
                code: Some(0),
                ..Default::default()
            },
        },
    );
    assert_eq!(notice(&app), "You left the review of P-0003.");
    assert!(matches!(
        need_card(&app).item(),
        Some(NeedItem::Proposal(_))
    ));
    assert!(fx.iter().any(|e| matches!(e, Effect::LoadNeeds(_))));

    // A review that failed says so on the card.
    let mut app = open_with(SIZE, vec![proposal(false)]);
    key(&mut app, 3.0, Key::Enter);
    send(
        &mut app,
        3.5,
        Msg::ReviewEnded {
            proposal: "P-0003".into(),
            result: vision::app::msg::ChildResult {
                code: Some(2),
                stderr: "error: the proposal P-0003 is closed\n".into(),
                ..Default::default()
            },
        },
    );
    assert!(failed(&app)
        .0
        .starts_with("The review ended with an error:"));
}

#[test]
fn the_chain_of_a_card_that_closed_goes_on_and_its_end_is_a_notice() {
    let mut app = open_with(SIZE, vec![plan_item(false)]);
    let fx = key(&mut app, 3.0, Key::Char('a'));
    key(&mut app, 3.05, Key::Esc);
    assert_eq!(app.card, None);
    assert!(matches!(&app.chain, Some(c) if c.owner == Owner::Need("plan:DEMO-12".into())));
    let done = drive(&mut app, 3.1, fx, None, |_| CheckFacts {
        plan: Some(PLAN_HASH.into()),
        ..CheckFacts::default()
    });
    assert_eq!(done.actions.len(), 1);
    assert!(notice(&app).starts_with("Approved the plan 3f2a1b4c5d6e."));
    assert_eq!(app.screen, Screen::Home);
}
