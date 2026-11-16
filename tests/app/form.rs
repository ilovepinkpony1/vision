//! The question form of a set of questions: the moves between its pages (forward, back, a jump
//! by number, and the two ends), the review page, the send of each answer in one action, the
//! drafts and their file, a question that another path answered meanwhile, and the places of
//! the parts that a click can hit. The worker and the reads are played by [`drive`].

use std::collections::BTreeMap;

use vision::app::chain::{Check, CheckFacts};
use vision::app::model::App;
use vision::app::msg::{ChildResult, Effect, JobDone, Key, Msg};
use vision::app::needs::{Draft, NeedItem, Reply, Target};
use vision::app::view::need_card::hits;

use super::need_support::*;
use super::support::*;

const SIZE: (u16, u16) = (110, 32);

fn key(app: &mut App, at: f64, k: Key) -> Vec<Effect> {
    send(app, at, Msg::Key(k))
}

fn none(_: &Check) -> CheckFacts {
    CheckFacts::default()
}

fn notice(app: &App) -> String {
    app.notice
        .as_ref()
        .map(|n| n.text.clone())
        .unwrap_or_default()
}

fn page(app: &App) -> usize {
    need_card(app).page
}

fn choice(i: usize, why: &str) -> Draft {
    Draft::Answer {
        reply: Reply::Choice(i),
        why: why.into(),
    }
}

/// Answers the question that shows with the letter of a choice and a reason.
fn answer(app: &mut App, at: f64, letter: char, why: &str) -> Vec<Effect> {
    key(app, at, Key::Char(letter));
    type_text(app, at, why);
    key(app, at, Key::Enter)
}

/// The form of the three sample questions, with these saved drafts.
fn saved(drafts: &[(&str, Draft)]) -> NeedItem {
    let NeedItem::Questions(mut set) = form(three()) else {
        unreachable!()
    };
    set.saved = drafts
        .iter()
        .map(|(id, d)| (id.to_string(), d.clone()))
        .collect();
    NeedItem::Questions(set)
}

/// A new load of the card with these items.
fn reload(app: &mut App, at: f64, items: Vec<NeedItem>) -> Vec<Effect> {
    let load = need_card(app).load;
    send(
        app,
        at,
        Msg::Needs {
            load,
            items: Box::new(Ok(items)),
        },
    )
}

#[test]
fn the_arrows_and_the_digits_move_through_the_pages_and_stop_at_the_two_ends() {
    let mut app = open_with(SIZE, vec![form(three())]);
    assert_eq!(page(&app), 0);
    key(&mut app, 3.0, Key::Left);
    assert_eq!(page(&app), 0, "no page and no card before the first one");
    key(&mut app, 3.1, Key::Right);
    assert_eq!(page(&app), 1);
    key(&mut app, 3.2, Key::Right);
    key(&mut app, 3.3, Key::Right);
    assert!(need_card(&app).reviewing());
    key(&mut app, 3.4, Key::Right);
    assert!(need_card(&app).reviewing(), "no card after the review page");
    key(&mut app, 3.5, Key::Left);
    assert_eq!(page(&app), 2);
    key(&mut app, 3.6, Key::Char('1'));
    assert_eq!(page(&app), 0);
    key(&mut app, 3.7, Key::Char('3'));
    assert_eq!(page(&app), 2);
    key(&mut app, 3.8, Key::Char('4'));
    assert_eq!(page(&app), 2, "no question 4");
    key(&mut app, 3.9, Key::Char('r'));
    assert!(need_card(&app).reviewing());
    // The left and right arrows move the cursor while a line is open.
    key(&mut app, 4.0, Key::Char('1'));
    key(&mut app, 4.1, Key::Char('a'));
    type_text(&mut app, 4.2, "ab");
    key(&mut app, 4.3, Key::Left);
    assert_eq!(page(&app), 0);
    key(&mut app, 4.4, Key::Char('X'));
    match &need_card(&app).input {
        vision::app::needs::Input::Why { line, .. } => assert_eq!(line.text(), "aXb"),
        other => panic!("{other:?}"),
    }
}

#[test]
fn each_draft_goes_to_the_file_and_the_form_goes_to_the_next_question_with_no_draft() {
    let mut app = open_with(SIZE, vec![form(three())]);
    let fx = answer(&mut app, 3.0, 'b', "People compare");
    assert!(jobs(&fx).is_empty(), "nothing goes to Vision");
    assert_eq!(page(&app), 1);
    let saves: Vec<_> = fx
        .iter()
        .filter_map(|e| match e {
            Effect::SaveDrafts { ticket, drafts } => Some((ticket.clone(), drafts.clone())),
            _ => None,
        })
        .collect();
    assert_eq!(
        saves,
        [(
            "DEMO-12".to_string(),
            vec![("Q-0002".to_string(), choice(1, "People compare"))]
        )]
    );
    // A jump to the third question, then a draft: the review page comes, with the row of the
    // question that has no draft.
    key(&mut app, 3.1, Key::Char('3'));
    let fx = answer(&mut app, 3.2, 'a', "Keep it");
    assert!(need_card(&app).reviewing());
    assert_eq!(need_card(&app).row, 1);
    let Some(Effect::SaveDrafts { drafts, .. }) = fx.last() else {
        panic!("no write of the drafts: {fx:?}");
    };
    assert_eq!(drafts.len(), 2, "the file holds each draft of the ticket");
    // A change of the first draft goes to the question with no draft.
    key(&mut app, 3.3, Key::Char('1'));
    answer(&mut app, 3.4, 'a', " again");
    assert_eq!(page(&app), 1);
    assert_eq!(
        need_card(&app).drafts.get("Q-0002"),
        Some(&choice(0, "People compare again"))
    );
}

#[test]
fn the_review_page_moves_its_row_opens_a_question_and_sends_only_a_whole_set() {
    let mut app = open_with(SIZE, vec![form(three())]);
    key(&mut app, 3.0, Key::Char('r'));
    assert_eq!(need_card(&app).row, 0, "the first question with no draft");
    key(&mut app, 3.1, Key::Down);
    key(&mut app, 3.2, Key::Down);
    key(&mut app, 3.3, Key::Down);
    assert_eq!(need_card(&app).row, 2, "the row stops at the last question");
    key(&mut app, 3.4, Key::Up);
    assert_eq!(need_card(&app).row, 1);
    key(&mut app, 3.5, Key::Enter);
    assert_eq!(page(&app), 1);
    answer(&mut app, 3.6, 'c', "Each team chooses");
    // The send needs a draft for each question.
    key(&mut app, 3.7, Key::Char('r'));
    let fx = key(&mut app, 3.8, Key::Char('s'));
    assert!(jobs(&fx).is_empty());
    assert_eq!(
        notice(&app),
        "Questions 1 and 3 have no draft yet. Answer each one, then press s."
    );
    assert_eq!(need_card(&app).row, 0);
    // `s` on a question page shows the review page first.
    key(&mut app, 3.9, Key::Char('1'));
    answer(&mut app, 4.0, 'a', "");
    key(&mut app, 4.1, Key::Char('3'));
    key(&mut app, 4.2, Key::Char('s'));
    assert!(need_card(&app).reviewing());
    assert_eq!(
        notice(&app),
        "These are your answers. Each question needs one before the send."
    );
    key(&mut app, 4.3, Key::Char('3'));
    key(&mut app, 4.4, Key::Char('k'));
    key(&mut app, 4.5, Key::Char('c'));
    assert!(need_card(&app).reviewing());
    let fx = key(&mut app, 4.6, Key::Char('s'));
    let done = drive(&mut app, 4.7, fx, None, none);
    assert_eq!(
        done.actions,
        [vec![
            "answer".to_string(),
            "--file".into(),
            "tickets/DEMO-12/answer-send.json".into()
        ]]
    );
    let entries = vision::questions::set::parse_entries(&done.inputs[0]).unwrap();
    let ids: Vec<&str> = entries.keys().map(String::as_str).collect();
    assert_eq!(ids, ["Q-0002", "Q-0003", "Q-0004"]);
    assert_eq!(entries["Q-0003"].choice, Some(3));
    assert_eq!(entries["Q-0003"].why, "Each team chooses");
    assert_eq!(entries["Q-0004"].needless.as_deref(), Some("code"));
    assert_eq!(app.card, None);
    assert_eq!(
        notice(&app),
        "Sent your 3 answers. builder gets them at the next pass."
    );
}

#[test]
fn a_refused_send_keeps_the_drafts_and_names_the_question() {
    let mut app = open_with(SIZE, vec![one(false)]);
    answer(&mut app, 3.0, 'a', "Keep it");
    let fx = key(&mut app, 3.1, Key::Char('s'));
    let job = jobs(&fx).pop().expect("the send runs");
    send(
        &mut app,
        3.2,
        Msg::JobDone(JobDone {
            id: job.id,
            kind: job.kind.clone(),
            result: ChildResult {
                code: Some(1),
                stderr: "error: no answer was recorded, because the answer to Q-0002 was refused: the Judge already answered Q-0002\n".into(),
                ..ChildResult::default()
            },
        }),
    );
    assert_eq!(
        need_card(&app).drafts.get("Q-0002"),
        Some(&choice(0, "Keep it"))
    );
    let text = screen(&app);
    assert!(
        text.contains("refused: the Judge already answered Q-0002"),
        "{text}"
    );
    // A new draft drops the failure from the card.
    key(&mut app, 3.3, Key::Char('1'));
    answer(&mut app, 3.4, 'b', "");
    assert_eq!(app.chain, None);
}

#[test]
fn a_set_of_one_question_costs_one_key_more_than_one_card() {
    let mut app = open_with(SIZE, vec![one(false)]);
    let mut keys = 0;
    let mut fx = Vec::new();
    for k in [Key::Char('a'), Key::Tab, Key::Char('s')] {
        keys += 1;
        fx = key(&mut app, 3.0, k);
    }
    assert_eq!(jobs(&fx).len(), 1, "the third key sends");
    assert_eq!(
        keys, 3,
        "a letter, Tab, and the send: one key more than before"
    );
}

#[test]
fn the_first_load_takes_the_saved_drafts_and_later_loads_keep_the_drafts_of_the_card() {
    let item = saved(&[("Q-0002", choice(1, "From the file"))]);
    let mut app = open_with(SIZE, vec![item.clone()]);
    assert_eq!(page(&app), 1, "the first question with no draft");
    assert_eq!(
        need_card(&app).drafts.get("Q-0002"),
        Some(&choice(1, "From the file"))
    );
    key(&mut app, 3.0, Key::Char('1'));
    key(&mut app, 3.1, Key::Char('a'));
    key(&mut app, 3.15, Key::Tab);
    // A later load brings the drafts that the file had when it read them: the card keeps its own.
    reload(&mut app, 3.2, vec![item]);
    assert_eq!(need_card(&app).drafts.get("Q-0002"), Some(&choice(0, "")));
}

#[test]
fn a_question_that_another_path_answered_leaves_the_form_with_a_note_and_its_draft_goes() {
    let mut app = open_with(SIZE, vec![form(three())]);
    answer(&mut app, 3.0, 'a', "First");
    answer(&mut app, 3.1, 'b', "Second");
    key(&mut app, 3.2, Key::Char('3'));
    // The Judge answered Q-0003 meanwhile: the next load has two questions.
    let mut cards = three();
    cards.remove(1);
    let fx = reload(&mut app, 3.3, vec![form(cards)]);
    let card = need_card(&app);
    assert_eq!(
        card.note.as_deref(),
        Some("Q-0003 left the form: the Judge or another command answered it.")
    );
    assert_eq!(card.set().map(|s| s.len()), Some(2));
    assert_eq!(page(&app), 1, "the same question still shows");
    assert_eq!(card.drafts.get("Q-0003"), None);
    assert!(fx.iter().any(|e| matches!(
        e,
        Effect::DropDrafts { ticket, ids } if ticket == "DEMO-12" && ids == &["Q-0003".to_string()]
    )));
}

#[test]
fn drafts_of_questions_that_no_longer_wait_go_at_the_first_load_with_a_note() {
    let NeedItem::Questions(mut set) = form(three()) else {
        unreachable!()
    };
    set.gone = vec!["Q-0001".into()];
    let mut app = home_with(
        SIZE,
        local(two_tickets(), vec![card_need("DEMO-12")]),
        Some(queue()),
    );
    let load = press_n(&mut app);
    let fx = send(
        &mut app,
        2.1,
        Msg::Needs {
            load,
            items: Box::new(Ok(vec![NeedItem::Questions(set)])),
        },
    );
    assert!(fx.iter().any(|e| matches!(
        e,
        Effect::DropDrafts { ticket, ids } if ticket == "DEMO-12" && ids == &["Q-0001".to_string()]
    )));
    assert_eq!(
        need_card(&app).note.as_deref(),
        Some("Q-0001 got an answer from the Judge or another command, so its draft is gone.")
    );
}

#[test]
fn the_parts_that_a_click_can_hit_are_where_the_view_draws_them() {
    let mut app = open_with(SIZE, vec![form(three())]);
    let buffer = render(&app);
    let cell = |x: i32, y: i32, w: i32| -> String {
        (x..x + w)
            .map(|x| buffer[(x as u16, y as u16)].symbol().to_string())
            .collect()
    };
    let found = hits(&app);
    let tab = |t: Target| found.iter().find(|h| h.0 == t).copied().unwrap();
    let (_, x, y, w) = tab(Target::Page(2));
    assert_eq!(cell(x, y, w), "3 ◇ proposed");
    let (_, x, y, w) = tab(Target::Page(3));
    assert_eq!(cell(x, y, w), "review");
    let (_, x, y, _) = tab(Target::Choice(1));
    assert!(cell(x, y, 8).contains('b'), "{}", cell(x, y, 8));
    key(&mut app, 3.0, Key::Char('r'));
    let buffer = render(&app);
    let found = hits(&app);
    let cell = |x: i32, y: i32, w: i32| -> String {
        (x..x + w)
            .map(|x| buffer[(x as u16, y as u16)].symbol().to_string())
            .collect()
    };
    let (_, x, y, w) = found.iter().find(|h| h.0 == Target::Send).copied().unwrap();
    assert_eq!(cell(x, y, w), "s send the 3 answers");
    let rows: BTreeMap<usize, i32> = found
        .iter()
        .filter_map(|h| match h.0 {
            Target::Row(i) => Some((i, h.2)),
            _ => None,
        })
        .collect();
    assert_eq!(rows.len(), 3);
    let (_, x, y, _) = found
        .iter()
        .find(|h| h.0 == Target::Row(1))
        .copied()
        .unwrap();
    assert!(cell(x, y, 40).contains("Must the file have a header row?"));
}
