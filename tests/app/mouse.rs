//! The mouse: the parts of a frame that a click can hit, a click on a key hint that acts as its
//! key on each screen, the rows that a click selects and opens, a choice and a tab, the wheel,
//! and a click that hits nothing. Each test draws the model as the runtime does, so that the
//! model keeps the parts of the last frame.

use ratatui::buffer::Buffer;
use vision::app::chain::Owner;
use vision::app::feed::Filter;
use vision::app::hits::Act;
use vision::app::model::{App, Card, Screen, SetupError};
use vision::app::msg::{Action, Effect, JobKind, Key, Mouse, MouseKind, Msg};
use vision::app::nav::Place;
use vision::app::needs::{Input, Reply, Target};
use vision::app::pager::{PagerKind, PagerState};
use vision::app::view::live::layout::Layout;
use vision::app::view::need_card::{body_size, hits};

use super::live_support::{events, live, says, timeline, working_live, TURN};
use super::need_support::*;
use super::screens::{models_many, models_open, stats_open};
use super::support::*;

const SIZE: (u16, u16) = (110, 32);

fn mouse(app: &mut App, at: f64, kind: MouseKind, (column, row): (u16, u16)) -> Vec<Effect> {
    send(app, at, Msg::Mouse(Mouse { kind, column, row }))
}

fn click(app: &mut App, at: f64, cell: (u16, u16)) -> Vec<Effect> {
    mouse(app, at, MouseKind::Click, cell)
}

/// Each row of the frame as its text, with the column of each byte of the text.
fn lines(buf: &Buffer) -> Vec<(String, Vec<u16>)> {
    (0..buf.area.height)
        .map(|y| {
            let mut line = String::new();
            let mut columns = Vec::new();
            for x in 0..buf.area.width {
                let symbol = buf[(x, y)].symbol();
                columns.extend(std::iter::repeat_n(x, symbol.len()));
                line.push_str(symbol);
            }
            (line, columns)
        })
        .collect()
}

/// The cell of the first character of `text` on the row `y`, if the row has it.
fn find_in(buf: &Buffer, text: &str, y: u16) -> Option<(u16, u16)> {
    let (line, columns) = &lines(buf)[usize::from(y)];
    line.find(text).map(|at| (columns[at], y))
}

/// The cell of the first character of `text` on the frame, from the top.
fn find(buf: &Buffer, text: &str) -> (u16, u16) {
    (0..buf.area.height)
        .find_map(|y| find_in(buf, text, y))
        .unwrap_or_else(|| panic!("no {text:?} on the frame:\n{}", super::support::text(buf)))
}

/// The cell of the first character of `text` on the frame, from the bottom.
fn find_last(buf: &Buffer, text: &str) -> (u16, u16) {
    (0..buf.area.height)
        .rev()
        .find_map(|y| find_in(buf, text, y))
        .unwrap_or_else(|| panic!("no {text:?} on the frame:\n{}", super::support::text(buf)))
}

/// The cell of the first character of `text` from the top, in the columns from `from` to
/// `to`.
fn find_between(buf: &Buffer, text: &str, from: u16, to: u16) -> (u16, u16) {
    let all = lines(buf);
    (0..buf.area.height)
        .find_map(|y| {
            let (line, columns) = &all[usize::from(y)];
            line.match_indices(text)
                .map(|(at, _)| (columns[at], y))
                .find(|(x, _)| (from..to).contains(x))
        })
        .unwrap_or_else(|| panic!("no {text:?} on the frame:\n{}", super::support::text(buf)))
}

/// The kinds of the pagers of the live view, the bottom one first.
fn pager_kinds(app: &App) -> Vec<PagerKind> {
    app.live
        .as_ref()
        .map(|l| l.pagers.iter().map(|p| p.kind.clone()).collect())
        .unwrap_or_default()
}

fn live_place(key: &str, pagers: &[PagerKind]) -> Place {
    Place::Live {
        key: key.into(),
        pagers: pagers.to_vec(),
    }
}

/// A click on the hint `hint` nearest the bottom of the frame, and the key `key`, on two copies
/// of the model after a draw. Both give the same model and the same effects; gives the model
/// and the effects of the click.
fn click_is_key(app: &App, hint: &str, key: Key) -> (App, Vec<Effect>) {
    let mut clicked = app.clone();
    let buf = draw(&mut clicked);
    let cell = find_last(&buf, hint);
    let mut pressed = clicked.clone();
    let at = clicked.clock.seconds();
    let by_click = click(&mut clicked, at, cell);
    let by_key = send(&mut pressed, at, Msg::Key(key));
    assert_eq!(by_click, by_key, "the effects of {hint:?}");
    assert_eq!(clicked, pressed, "the model after {hint:?}");
    (clicked, by_click)
}

/// Two clicks on the hint `hint` nearest the bottom of the frame, a part that sends something
/// or records a decision, and the key `key`, on two copies of the model after a draw. The first
/// click arms the part: it gives no effect, it changes nothing else, and the next frame shows
/// `again` on the row of the hint. The second click acts as the key: both give the same model
/// and the same effects; gives the model and the effects of the clicks.
fn two_clicks_are_key(app: &App, hint: &str, again: &str, key: Key) -> (App, Vec<Effect>) {
    let mut clicked = app.clone();
    let buf = draw(&mut clicked);
    let cell = find_last(&buf, hint);
    let mut pressed = clicked.clone();
    let at = clicked.clock.seconds();
    let first = click(&mut clicked, at, cell);
    // A tick at the same time runs the same timers, so the first click equals a tick and an arm.
    let mut tick = pressed.clone();
    let by_tick = send(&mut tick, at, Msg::Tick);
    assert_eq!(
        first, by_tick,
        "the first click on {hint:?} gives no effect"
    );
    let mut unarmed = clicked.clone();
    assert!(
        unarmed.armed.take().is_some(),
        "the first click on {hint:?} arms it"
    );
    assert!(unarmed.dirty, "an arm makes a redraw");
    unarmed.dirty = tick.dirty;
    assert_eq!(
        unarmed, tick,
        "the first click on {hint:?} changes only the arm"
    );
    let buf = draw(&mut clicked);
    assert!(
        find_in(&buf, again, cell.1).is_some(),
        "the armed {hint:?} says {again:?}:\n{}",
        super::support::text(&buf)
    );
    let by_click = click(&mut clicked, at, cell);
    let by_key = send(&mut pressed, at, Msg::Key(key));
    assert_eq!(by_click, by_key, "the effects of {hint:?}");
    assert_eq!(clicked.armed, None, "the second click takes the arm away");
    // The second draw recorded the parts of the armed frame.
    pressed.hits = clicked.hits.clone();
    assert_eq!(clicked, pressed, "the model after {hint:?}");
    (clicked, by_click)
}

/// The model after a tick at the same time: what a message that does nothing leaves.
fn ticked(app: &App) -> App {
    let mut ticked = app.clone();
    let at = app.clock.seconds();
    send(&mut ticked, at, Msg::Tick);
    ticked
}

#[test]
fn a_click_on_a_key_of_the_footer_of_home_acts_as_that_key() {
    let app = home(SIZE);
    let (models, fx) = click_is_key(&app, "m models", Key::Char('m'));
    assert_eq!(models.screen, Screen::Models);
    assert!(fx.contains(&Effect::LoadModels));
    let (paused, _) = click_is_key(&app, "p pause", Key::Char('p'));
    assert!(paused.engine.paused);
    let (live, fx) = click_is_key(&app, "enter open or start", Key::Enter);
    assert_eq!(
        live.screen,
        Screen::Live {
            key: "DEMO-12".into()
        }
    );
    assert!(fx.contains(&Effect::LoadTicket("DEMO-12".into())));
    let (keys, _) = click_is_key(&app, "? keys", Key::Char('?'));
    assert_eq!(keys.card, Some(Card::Help));
    // The label of a hint acts as its key too.
    let mut clicked = app.clone();
    let buf = draw(&mut clicked);
    let (x, y) = find_last(&buf, "s stats");
    click(&mut clicked, 1.0, (x + 4, y));
    assert_eq!(clicked.screen, Screen::Stats);
}

#[test]
fn a_click_on_a_key_of_the_footer_of_the_live_view_acts_as_that_key() {
    let app = working_live(SIZE);
    let (stop, _) = click_is_key(&app, "x stop", Key::Char('x'));
    assert!(matches!(&stop.card, Some(Card::Stop(card)) if card.key == "DEMO-12"));
    let (log, _) = click_is_key(&app, "l log and tool calls", Key::Char('l'));
    let pager = log.live.as_ref().unwrap().pagers.last().unwrap();
    assert_eq!(pager.kind, PagerKind::Log);
    let (next, _) = click_is_key(&app, "f filter", Key::Char('f'));
    assert_eq!(next.activity, Filter::Agents);
    let (home, fx) = click_is_key(&app, "esc home", Key::Esc);
    assert_eq!(home.screen, Screen::Home);
    assert!(fx.contains(&Effect::Follow(None)));
    // In the pager, `g start` and `G end` are hints of their own, each with its label.
    let (start, _) = click_is_key(&log, "g start", Key::Char('g'));
    assert_eq!(
        start.live.as_ref().unwrap().pagers.last().unwrap().cursor,
        Some(0)
    );
    let mut end = start.clone();
    let buf = draw(&mut end);
    let (x, y) = find_last(&buf, "G end");
    click(&mut end, 3.0, (x + 3, y));
    let pager = end.live.as_ref().unwrap().pagers.last().unwrap();
    assert_eq!(
        pager.cursor,
        Some(pager.lines.len() - 1),
        "G goes to the end"
    );
}

#[test]
fn a_click_on_a_key_of_a_card_acts_as_that_key() {
    let mut app = home(SIZE);
    for k in [Key::Down, Key::Down, Key::Enter] {
        send(&mut app, 1.5, Msg::Key(k));
    }
    assert!(matches!(&app.card, Some(Card::Start(card)) if card.key == "DEMO-15"));
    let (closed, _) = click_is_key(&app, "esc cancel", Key::Esc);
    assert_eq!(closed.card, None);
    // The start sends something, so it acts on the second click.
    let (started, fx) = two_clicks_are_key(&app, "enter start", "enter click again", Key::Enter);
    assert!(
        jobs(&fx).iter().any(|job| job.kind
            == JobKind::Action(Action::Take {
                key: "DEMO-15".into()
            })),
        "{fx:?}"
    );
    assert!(matches!(&started.card, Some(Card::Start(card)) if card.run.job().is_some()));
    // The keys card closes with a click on its last line.
    let mut keys = home(SIZE);
    send(&mut keys, 1.5, Msg::Key(Key::Char('?')));
    let (closed, _) = click_is_key(&keys, "esc closes this list", Key::Esc);
    assert_eq!(closed.card, None);
}

#[test]
fn a_click_on_a_key_of_the_question_form_acts_as_that_key() {
    let app = open_with(SIZE, vec![form(three())]);
    let (words, _) = click_is_key(&app, "e your words", Key::Char('e'));
    assert!(matches!(need_card(&words).input, Input::Words(_)));
    let (review, _) = click_is_key(&app, "r review", Key::Char('r'));
    assert!(need_card(&review).reviewing());
    let (later, _) = click_is_key(&app, "esc later", Key::Esc);
    assert_eq!(later.card, None);
    let (opened, _) = click_is_key(&review, "enter open the question", Key::Enter);
    assert_eq!(need_card(&opened).page, need_card(&review).row);
    // The send of the review page acts as `s` on the second click: with a question that has no
    // draft, a notice says which one.
    let (sent, _) = two_clicks_are_key(
        &review,
        "s send the 3 answers",
        "s click again",
        Key::Char('s'),
    );
    let text = sent
        .notice
        .as_ref()
        .map(|n| n.text.clone())
        .unwrap_or_default();
    assert!(text.contains("no draft yet"), "{text}");
}

#[test]
fn a_click_on_a_key_of_models_and_of_stats_acts_as_that_key() {
    let app = models_open(SIZE);
    let (effort, _) = click_is_key(&app, "e effort", Key::Char('e'));
    assert!(effort.models.edits.contains_key("scout"));
    let (home, _) = click_is_key(&app, "esc home", Key::Esc);
    assert_eq!(home.screen, Screen::Home);
    let stats = stats_open(SIZE);
    let (all, fx) = click_is_key(&stats, "w all time", Key::Char('w'));
    assert!(!all.stats.week);
    assert!(fx.contains(&Effect::LoadStats { week: false }));
    let (home, _) = click_is_key(&stats, "esc home", Key::Esc);
    assert_eq!(home.screen, Screen::Home);
}

#[test]
fn each_key_of_the_things_that_wait_that_decides_arms_on_the_first_click_and_acts_on_the_second() {
    let started = |app: &App| app.chain.as_ref().map(|c| c.owner.clone());
    // The plan: `a` approves it. `x` only opens the stop card, so it acts on the first click.
    let plan = open_with(SIZE, vec![plan_item(false)]);
    let (approved, _) = two_clicks_are_key(&plan, "a approve", "a click again", Key::Char('a'));
    assert_eq!(started(&approved), Some(Owner::Need("plan:DEMO-12".into())));
    let (stop, _) = click_is_key(&plan, "x stop the ticket", Key::Char('x'));
    assert!(matches!(stop.card, Some(Card::Stop(_))));
    // An operation that waits for an approval: `y` and `a` send it.
    let write = open_with(SIZE, vec![approval("ask", &[])]);
    for (hint, key) in [("y approve", 'y'), ("a approve, then auto", 'a')] {
        let (sent, _) = two_clicks_are_key(&write, hint, "click again", Key::Char(key));
        assert!(started(&sent).is_some(), "{hint}");
    }
    // The first write of a verb: `y` and `a` of the keys, and the rows of the choices of `y`
    // and `a`. The row of `n` only closes the card.
    let first = open_with(SIZE, vec![first_write("shadow")]);
    let (sent, _) = two_clicks_are_key(&first, "y a n", "click y again", Key::Char('y'));
    assert!(started(&sent).is_some());
    let (sent, _) = two_clicks_are_key(
        &first,
        "Send it. Never ask again",
        "a  click again",
        Key::Char('a'),
    );
    assert!(started(&sent).is_some());
    let (closed, _) = click_is_key(&first, "Do not send", Key::Char('n'));
    assert_eq!(closed.card, None);
    // A rule that stopped the ticket: `r` starts the step again. `o` only opens a card.
    let flag = open_with(SIZE, vec![super::need_support::flag(false)]);
    let (retried, _) = two_clicks_are_key(
        &flag,
        "r try the step again",
        "r click again",
        Key::Char('r'),
    );
    assert!(started(&retried).is_some());
    let (open, _) = click_is_key(&flag, "o open the session", Key::Char('o'));
    assert!(matches!(&open.card, Some(Card::Open(card)) if card.running.is_none()));
    // The open card pauses the ticket and gives the terminal to its session. The footer of Home
    // under the card also says `enter open`, so the hint goes with its neighbor.
    let (_, fx) = two_clicks_are_key(
        &open,
        "enter open    esc cancel",
        "enter click again",
        Key::Enter,
    );
    assert!(
        fx.contains(&Effect::OpenSession("DEMO-12".into())),
        "{fx:?}"
    );
    // A notice gets its mark of read.
    let app = open_with(SIZE, vec![notice_item(false)]);
    let (done, _) = two_clicks_are_key(&app, "enter mark it read", "enter click again", Key::Enter);
    assert!(started(&done).is_some());
    // Enter on a paused ticket only opens the continue card, so it acts on the first click.
    let app = open_with(SIZE, vec![paused(false)]);
    let (opened, fx) = click_is_key(&app, "enter continue", Key::Enter);
    assert!(matches!(&opened.card, Some(Card::Continue(c)) if c.key == "DEMO-12"));
    assert!(fx.contains(&Effect::LoadResume("DEMO-12".into())), "{fx:?}");
    // The review of a proposal only opens its session, so it acts on the first click.
    let review = open_with(SIZE, vec![proposal(false)]);
    let (reviewed, _) = click_is_key(&review, "enter review", Key::Enter);
    assert!(started(&reviewed).is_some());
}

#[test]
fn each_other_key_that_decides_arms_on_the_first_click_and_acts_on_the_second() {
    // The kill switch and the quit card while a ticket moves.
    let mut kill = home(SIZE);
    send(&mut kill, 1.5, Msg::Key(Key::Char('K')));
    let (_, fx) = two_clicks_are_key(&kill, "enter turn off", "enter click again", Key::Enter);
    assert!(
        jobs(&fx)
            .iter()
            .any(|job| job.kind == JobKind::Action(Action::Off)),
        "{fx:?}"
    );
    let mut quit = home(SIZE);
    send(&mut quit, 1.5, Msg::Key(Key::Char('q')));
    assert_eq!(quit.card, Some(Card::Quit));
    let (left, fx) = two_clicks_are_key(&quit, "enter quit", "enter click again", Key::Enter);
    assert!(left.quit && fx.contains(&Effect::Quit));
    // The stop card, and the open card of a running turn, whose `o` stops the turn.
    let live = working_live(SIZE);
    let mut stop = live.clone();
    send(&mut stop, 3.0, Msg::Key(Key::Char('x')));
    let (_, fx) = two_clicks_are_key(&stop, "enter stop", "enter click again", Key::Enter);
    assert!(
        jobs(&fx).iter().any(|job| job.kind
            == JobKind::Action(Action::Stop {
                key: "DEMO-12".into()
            })),
        "{fx:?}"
    );
    let mut open = live.clone();
    send(&mut open, 3.0, Msg::Key(Key::Char('o')));
    assert!(matches!(&open.card, Some(Card::Open(card)) if card.running.is_some()));
    let (took, _) = two_clicks_are_key(&open, "o stop the turn", "o click again", Key::Char('o'));
    assert!(matches!(
        took.chain.as_ref().map(|c| &c.owner),
        Some(Owner::TakeOver { .. })
    ));
    // Models: Enter pins, and `u` goes back to the default.
    let models = models_open(SIZE);
    let (pinned, _) = two_clicks_are_key(&models, "enter pin", "enter click again", Key::Enter);
    assert_eq!(
        pinned.chain.as_ref().map(|c| c.owner.clone()),
        Some(Owner::Models("scout".into()))
    );
    let (reset, _) = two_clicks_are_key(
        &models,
        "u back to the default",
        "u click again",
        Key::Char('u'),
    );
    let text = reset
        .notice
        .as_ref()
        .map(|n| n.text.clone())
        .unwrap_or_default();
    assert!(text.contains("already uses the default"), "{text}");
}

#[test]
fn a_click_elsewhere_a_key_a_paste_a_step_of_the_wheel_and_three_seconds_take_the_arm_away() {
    let mut app = open_with(SIZE, vec![first_write("shadow")]);
    let buf = draw(&mut app);
    let y = find_last(&buf, "y a n");
    let a = (y.0 + 2, y.1);
    let shows = |app: &mut App, text: &str| {
        let buf = draw(app);
        find_in(&buf, text, y.1).is_some()
    };
    // The first click arms `y`. A click on `a` arms `a` instead, and acts on nothing.
    click(&mut app, 2.5, y);
    assert_eq!(app.armed_act(), Some(&Act::Decide(Key::Char('y'))));
    assert!(shows(&mut app, "y a n click y again"));
    let fx = click(&mut app, 2.5, a);
    assert_eq!(fx, Vec::new());
    assert_eq!(app.armed_act(), Some(&Act::Decide(Key::Char('a'))));
    assert_eq!(app.chain, None);
    assert!(shows(&mut app, "y a n click a again"));
    // A click outside each part takes the arm away, with a redraw that shows the label again.
    click(&mut app, 2.5, (0, 0));
    assert_eq!(app.armed, None);
    assert!(app.dirty);
    assert!(shows(&mut app, "y a n choose"));
    let fx = click(&mut app, 2.5, y);
    assert_eq!(
        fx,
        Vec::new(),
        "after the arm went away, a click arms again"
    );
    // A typed key takes the arm away, and so does a paste.
    send(&mut app, 2.5, Msg::Key(Key::Down));
    assert_eq!(app.armed, None);
    assert!(shows(&mut app, "y a n choose"));
    click(&mut app, 2.5, y);
    send(&mut app, 2.5, Msg::Paste("text".into()));
    assert_eq!(app.armed, None);
    // A step of the wheel takes it away.
    assert!(shows(&mut app, "y a n choose"));
    click(&mut app, 2.5, y);
    assert!(app.armed.is_some());
    mouse(&mut app, 2.5, MouseKind::WheelDown, y);
    assert_eq!(app.armed, None);
    // The arm holds for three seconds. The first message after them, such as the tick of each
    // second at rest, takes it away with a redraw.
    assert!(shows(&mut app, "y a n choose"));
    click(&mut app, 3.0, y);
    assert!(shows(&mut app, "y a n click y again"));
    send(&mut app, 5.9, Msg::Tick);
    assert!(app.armed_act().is_some());
    assert!(!app.dirty, "the arm makes no redraw until its end");
    send(&mut app, 6.0, Msg::Tick);
    assert_eq!(app.armed, None);
    assert!(app.dirty, "the end of the arm makes a redraw");
    assert!(shows(&mut app, "y a n choose"));
    let fx = click(&mut app, 6.0, y);
    assert_eq!(fx, Vec::new());
    assert_eq!(app.chain, None);
}

#[test]
fn a_step_of_the_wheel_outside_each_part_takes_the_arm_away() {
    let mut app = open_with(SIZE, vec![plan_item(false)]);
    let buf = draw(&mut app);
    let a = find_last(&buf, "a approve");
    click(&mut app, 2.5, a);
    assert_eq!(app.armed_act(), Some(&Act::Decide(Key::Char('a'))));
    // The first cell of the header is outside the card, and no part of it scrolls.
    assert_eq!(app.hits.wheel_at(0, 0), None);
    mouse(&mut app, 2.6, MouseKind::WheelDown, (0, 0));
    assert_eq!(app.armed, None, "the step of the wheel takes the arm away");
    assert!(app.dirty, "the label of the part shows again");
    // So the next click on `a` within 3 seconds arms it again, and approves nothing.
    draw(&mut app);
    let fx = click(&mut app, 2.7, a);
    assert_eq!(fx, Vec::new());
    assert_eq!(app.chain, None);
    assert_eq!(app.armed_act(), Some(&Act::Decide(Key::Char('a'))));
}

#[test]
fn a_typed_key_acts_at_once_also_while_a_click_armed_its_part() {
    let mut app = open_with(SIZE, vec![first_write("shadow")]);
    let buf = draw(&mut app);
    click(&mut app, 2.5, find_last(&buf, "y a n"));
    assert!(app.armed.is_some());
    send(&mut app, 2.5, Msg::Key(Key::Char('a')));
    assert_eq!(app.armed, None);
    assert!(app.chain.is_some(), "the key `a` sends the write at once");
    // On the start card, Enter starts at once, as before.
    let mut start = home(SIZE);
    for k in [Key::Down, Key::Down] {
        send(&mut start, 1.5, Msg::Key(k));
    }
    let fx = send(&mut start, 1.5, Msg::Key(Key::Enter));
    assert!(matches!(&start.card, Some(Card::Start(_))), "{fx:?}");
    let fx = send(&mut start, 1.5, Msg::Key(Key::Enter));
    assert!(
        jobs(&fx).iter().any(|job| job.kind
            == JobKind::Action(Action::Take {
                key: "DEMO-15".into()
            })),
        "{fx:?}"
    );
}

#[test]
fn new_data_for_the_same_item_keeps_the_arm_and_another_item_takes_it_away() {
    let load = |app: &mut App, items: Vec<vision::app::needs::NeedItem>| {
        let load = need_card(app).load;
        send(
            app,
            2.6,
            Msg::Needs {
                load,
                items: Box::new(Ok(items)),
            },
        );
    };
    let mut app = open_with(SIZE, vec![plan_item(false)]);
    let buf = draw(&mut app);
    let a = find_last(&buf, "a approve");
    click(&mut app, 2.5, a);
    // A load of the same items keeps the arm, so the second click approves.
    load(&mut app, vec![plan_item(false)]);
    assert!(app.armed_act().is_some());
    draw(&mut app);
    click(&mut app, 2.7, a);
    assert_eq!(
        app.chain.as_ref().map(|c| c.owner.clone()),
        Some(Owner::Need("plan:DEMO-12".into()))
    );
    // A load that puts another item in its place takes the arm away: a click on the same key
    // of the other item arms it, and approves nothing.
    let mut app = open_with(SIZE, vec![plan_item(false)]);
    let buf = draw(&mut app);
    click(&mut app, 2.5, a);
    assert_eq!(find_last(&buf, "a approve"), a);
    let mut other = plan_item(false);
    if let vision::app::needs::NeedItem::Plan(plan) = &mut other {
        plan.ticket = "DEMO-7".into();
    }
    load(&mut app, vec![other]);
    assert_eq!(app.armed_act(), None);
    let buf = draw(&mut app);
    assert_eq!(find_last(&buf, "a approve"), a);
    let fx = click(&mut app, 2.7, a);
    assert_eq!(fx, Vec::new());
    assert_eq!(app.chain, None);
    assert!(app.armed_act().is_some());
}

#[test]
fn the_wheel_and_a_click_on_models_with_more_agents_than_rows_follow_the_window() {
    let mut app = models_many((100, 28));
    let buf = draw(&mut app);
    let rows = find(&buf, "scout");
    // Ten steps of the wheel over the rows move the selection ten agents, and the window
    // follows the selection.
    for _ in 0..10 {
        mouse(&mut app, 3.0, MouseKind::WheelDown, rows);
        draw(&mut app);
    }
    assert_eq!(app.models.selected, 10);
    // A part of a row holds the index of its agent in the table, not the row in the window:
    // `cause` is the fifth row of the window and the ninth agent.
    let buf = draw(&mut app);
    click(&mut app, 3.0, find(&buf, "cause"));
    assert_eq!(app.models.selected, 8);
    assert_eq!(app.models.lines()[8].agent, "cause");
    // The box of the recommendation does not scroll.
    let buf = draw(&mut app);
    let before = ticked(&app);
    mouse(
        &mut app,
        3.0,
        MouseKind::WheelDown,
        find(&buf, "recommendation for"),
    );
    assert_eq!(app, before);
}

#[test]
fn a_click_outside_each_part_does_nothing_and_makes_no_redraw() {
    for app in [
        home(SIZE),
        working_live(SIZE),
        models_open(SIZE),
        stats_open(SIZE),
    ] {
        let mut clicked = app.clone();
        draw(&mut clicked);
        let before = ticked(&clicked);
        let at = clicked.clock.seconds();
        // The line under the header holds no part on any screen.
        let fx = click(&mut clicked, at, (0, 1));
        assert!(fx.is_empty(), "{fx:?}");
        assert_eq!(clicked, before, "{:?}", app.screen);
        assert!(!clicked.dirty, "{:?}", app.screen);
    }
}

#[test]
fn a_card_takes_each_click_and_the_screen_under_it_has_no_part() {
    let mut app = home(SIZE);
    for k in [Key::Down, Key::Down, Key::Enter] {
        send(&mut app, 1.5, Msg::Key(k));
    }
    let buf = draw(&mut app);
    // The footer of Home still shows under the card, dim.
    let cell = find_last(&buf, "m models");
    assert_eq!(app.hits.click_at(cell.0, cell.1), None);
    let before = ticked(&app);
    click(&mut app, 1.5, cell);
    assert_eq!(app, before);
}

#[test]
fn only_the_parts_of_the_last_frame_count_and_a_resize_clears_them() {
    let mut app = home(SIZE);
    let buf = draw(&mut app);
    let row = find(&buf, "DEMO-15");
    assert_eq!(
        app.hits.click_at(row.0, row.1),
        Some(&Act::Ticket("DEMO-15".into()))
    );
    // Models draws over the same cells: the row of DEMO-15 is gone with its frame.
    let mut models = app.clone();
    send(&mut models, 1.5, Msg::Key(Key::Char('m')));
    send(
        &mut models,
        1.5,
        Msg::Models(Box::new(Ok(super::screens::model_lines()))),
    );
    draw(&mut models);
    assert!(!models
        .hits
        .clicks()
        .iter()
        .any(|(_, act)| matches!(act, Act::Ticket(_))));
    click(&mut models, 1.5, row);
    assert_eq!(models.home.selected.as_deref(), Some("DEMO-12"));
    // A change of the window size clears the parts until the next frame.
    let cell = find_last(&buf, "m models");
    send(
        &mut app,
        1.5,
        Msg::Resize {
            width: 120,
            height: 34,
        },
    );
    assert!(app.hits.is_empty());
    let before = ticked(&app);
    click(&mut app, 1.5, cell);
    assert_eq!(app, before);
    draw(&mut app);
    assert!(!app.hits.is_empty());
}

#[test]
fn a_click_selects_a_ticket_of_home_and_a_second_click_opens_it() {
    let mut app = home(SIZE);
    let buf = draw(&mut app);
    let queue = find(&buf, "DEMO-15");
    click(&mut app, 1.5, queue);
    assert_eq!(app.home.selected.as_deref(), Some("DEMO-15"));
    assert_eq!(app.card, None);
    assert!(app.dirty);
    draw(&mut app);
    let fx = click(&mut app, 1.5, queue);
    assert!(matches!(&app.card, Some(Card::Start(card)) if card.key == "DEMO-15"));
    assert!(fx.contains(&Effect::LoadStartInfo("DEMO-15".into())));
    // A ticket in progress has two lines: a click on the second line selects it too.
    let mut app = home(SIZE);
    let buf = draw(&mut app);
    let (x, y) = find(&buf, "DEMO-7");
    click(&mut app, 1.5, (x, y + 1));
    assert_eq!(app.home.selected.as_deref(), Some("DEMO-7"));
    draw(&mut app);
    let fx = click(&mut app, 1.5, (x, y));
    assert_eq!(
        app.screen,
        Screen::Live {
            key: "DEMO-7".into()
        }
    );
    assert!(fx.contains(&Effect::LoadTicket("DEMO-7".into())));
}

#[test]
fn a_click_and_the_wheel_select_a_step_of_the_continue_card_and_two_clicks_on_it_continue() {
    let tickets = vec![stopped(
        "DEMO-3",
        "Rename the setting of the weekly email",
        "review",
    )];
    let mut app = home_with(SIZE, local(tickets, Vec::new()), Some(queue()));
    send(&mut app, 2.0, Msg::Key(Key::Enter));
    read_resume(
        &mut app,
        2.1,
        "DEMO-3",
        Ok(continue_view("DEMO-3", "review", Some("comments"), true)),
    );
    let chosen = |app: &App| match &app.card {
        Some(Card::Continue(card)) => card.chosen().map(str::to_string),
        other => panic!("no continue card: {other:?}"),
    };
    assert_eq!(chosen(&app).as_deref(), Some("comments"));
    // A click on another step selects it and starts nothing.
    let buf = draw(&mut app);
    let fx = click(&mut app, 2.2, find(&buf, "ready-to-merge"));
    assert!(jobs(&fx).is_empty(), "{fx:?}");
    assert_eq!(chosen(&app).as_deref(), Some("ready-to-merge"));
    assert_eq!(app.chain, None);
    // A step of the wheel over the card moves the selection as an arrow does.
    let buf = draw(&mut app);
    mouse(&mut app, 2.3, MouseKind::WheelUp, find(&buf, "ci-review"));
    assert_eq!(chosen(&app).as_deref(), Some("ci-review"));
    // A click on the selected step acts as Enter: the first click arms, the second continues.
    let buf = draw(&mut app);
    let row = find(&buf, "ci-review");
    let first = click(&mut app, 2.4, row);
    assert!(jobs(&first).is_empty(), "{first:?}");
    assert!(app.armed.is_some());
    let buf = draw(&mut app);
    assert!(
        super::support::text(&buf).contains("enter click again"),
        "{}",
        super::support::text(&buf)
    );
    let second = click(&mut app, 2.5, row);
    assert_eq!(
        jobs(&second)
            .into_iter()
            .map(|job| job.kind)
            .collect::<Vec<_>>(),
        vec![JobKind::Action(Action::TakeAt {
            key: "DEMO-3".into(),
            step: "ci-review".into()
        })]
    );
}

/// A ticket that is in progress outside Vision: two clicks on its row open its start card, a
/// click picks a step, and two clicks on the selected step take the ticket there. The card has
/// no part for the live view.
#[test]
fn two_clicks_open_the_start_card_of_a_ticket_outside_vision_and_two_clicks_on_a_step_take_it() {
    let mut app = home_with(
        SIZE,
        local(two_tickets(), Vec::new()),
        Some(queue_with_outside()),
    );
    let buf = draw(&mut app);
    let row = find(&buf, "DEMO-21");
    click(&mut app, 1.5, row);
    assert_eq!(app.home.selected.as_deref(), Some("DEMO-21"));
    draw(&mut app);
    let fx = click(&mut app, 1.6, row);
    assert!(
        fx.contains(&Effect::LoadOutside {
            key: "DEMO-21".into(),
            branch: "feature/demo-21-export".into(),
            pr: Some(31),
        }),
        "{fx:?}"
    );
    read_resume(&mut app, 1.7, "DEMO-21", Ok(outside_view(Some("ready"))));
    let buf = draw(&mut app);
    assert!(!super::support::text(&buf).contains("live view"));
    assert!(app
        .hits
        .clicks()
        .iter()
        .all(|(_, act)| *act != Act::Key(Key::Char('v'))));
    // A click on another step selects it, and two clicks on it take the ticket there.
    let step = find(&buf, "fix-ci");
    assert!(jobs(&click(&mut app, 1.8, step)).is_empty());
    draw(&mut app);
    assert!(jobs(&click(&mut app, 1.9, step)).is_empty());
    assert!(app.armed.is_some());
    draw(&mut app);
    let fx = click(&mut app, 2.0, step);
    assert_eq!(
        jobs(&fx).into_iter().map(|job| job.kind).collect::<Vec<_>>(),
        vec![JobKind::Action(Action::TakeAt {
            key: "DEMO-21".into(),
            step: "fix-ci".into()
        })]
    );
}

#[test]
fn a_click_on_v_of_the_continue_card_opens_the_live_view_as_the_key_does() {
    let tickets = vec![stopped(
        "DEMO-3",
        "Rename the setting of the weekly email",
        "review",
    )];
    let mut app = home_with(SIZE, local(tickets, Vec::new()), Some(queue()));
    send(&mut app, 2.0, Msg::Key(Key::Enter));
    read_resume(
        &mut app,
        2.1,
        "DEMO-3",
        Ok(continue_view("DEMO-3", "review", Some("comments"), true)),
    );
    let (opened, fx) = click_is_key(&app, "v live view", Key::Char('v'));
    assert_eq!(opened.card, None);
    assert!(matches!(&opened.screen, Screen::Live { key } if key == "DEMO-3"));
    assert!(fx.contains(&Effect::LoadTicket("DEMO-3".into())), "{fx:?}");
}

#[test]
fn a_click_selects_an_agent_of_models_and_two_clicks_on_its_change_pin_it() {
    let mut app = models_open(SIZE);
    let buf = draw(&mut app);
    let builder = find(&buf, "builder");
    click(&mut app, 2.5, builder);
    assert_eq!(app.models.selected, 1);
    // The selected agent with no change has nothing to pin: a click only says how to make one.
    draw(&mut app);
    let fx = click(&mut app, 2.5, builder);
    assert_eq!(fx, Vec::new());
    assert_eq!(app.chain, None);
    let notice = app
        .notice
        .as_ref()
        .map(|n| n.text.clone())
        .unwrap_or_default();
    assert!(notice.starts_with("No change to pin."), "{notice}");
    // With a change, a click on the selected agent pins it as Enter does: the first click arms,
    // and the second click pins.
    send(&mut app, 2.6, Msg::Key(Key::Right));
    assert!(app.models.edits.contains_key("builder"));
    let (pinned, fx) = two_clicks_are_key(
        &app,
        "The change is not pinned yet",
        "Click again to pin the change.",
        Key::Enter,
    );
    assert_eq!(
        pinned.chain.as_ref().map(|c| c.owner.clone()),
        Some(Owner::Models("builder".into()))
    );
    assert!(
        jobs(&fx).iter().any(|job| matches!(
            &job.kind,
            JobKind::Action(Action::ModelSet { agent, .. }) if agent == "builder"
        )),
        "{fx:?}"
    );
}

#[test]
fn the_arrows_and_the_effort_of_the_selected_agent_act_as_their_keys() {
    let app = models_open(SIZE);
    let mut clicked = app.clone();
    let buf = draw(&mut clicked);
    let (x, y) = find(&buf, "‹ claude-fable-5-2 ›");
    let right = (x + "‹ claude-fable-5-2 ".chars().count() as u16, y);
    let mut pressed = clicked.clone();
    click(&mut clicked, 2.5, right);
    send(&mut pressed, 2.5, Msg::Key(Key::Right));
    assert_eq!(clicked, pressed);
    assert!(clicked.models.edits.contains_key("scout"));
    // The left arrow brings the model of the table back: no change is left.
    draw(&mut clicked);
    click(&mut clicked, 2.5, (x, y));
    assert!(
        clicked.models.edits.is_empty(),
        "{:?}",
        clicked.models.edits
    );
    assert_eq!(clicked.models.selected, 0, "the click did not pin the row");
    let effort = find_in(&buf, "high", y).unwrap();
    let mut clicked = app.clone();
    draw(&mut clicked);
    click(&mut clicked, 2.5, effort);
    assert_eq!(clicked.models.edits["scout"].effort, "xhigh");
    assert!(clicked.chain.is_none(), "the click did not pin the row");
}

#[test]
fn a_click_chooses_a_row_of_the_review_page_and_a_second_click_opens_it() {
    let mut app = open_with(SIZE, vec![form(three())]);
    send(&mut app, 2.5, Msg::Key(Key::Char('r')));
    assert!(need_card(&app).reviewing());
    assert_eq!(need_card(&app).row, 0);
    let buf = draw(&mut app);
    let row = find(&buf, "Must the file have a header row?");
    click(&mut app, 2.5, row);
    assert_eq!(need_card(&app).row, 1);
    assert!(need_card(&app).reviewing());
    draw(&mut app);
    click(&mut app, 2.5, row);
    assert_eq!(need_card(&app).page, 1);
    assert!(!need_card(&app).reviewing());
}

#[test]
fn a_click_puts_the_cursor_of_the_log_on_a_row_and_a_second_click_opens_its_turn() {
    let mut app = working_live(SIZE);
    send(&mut app, 3.0, Msg::Key(Key::Char('l')));
    let buf = draw(&mut app);
    let row = find(&buf, "builder starts a turn at the step plan");
    click(&mut app, 3.0, row);
    let pager = app.live.as_ref().unwrap().pagers.last().unwrap();
    let cursor = pager.cursor.unwrap();
    assert_eq!(
        pager.selected_turn(),
        Some("20261116T135630Z-DEMO-12-plan-1")
    );
    draw(&mut app);
    let fx = click(&mut app, 3.0, row);
    assert!(fx.contains(&Effect::LoadTurn {
        key: "DEMO-12".into(),
        turn: "20261116T135630Z-DEMO-12-plan-1".into()
    }));
    let live = app.live.as_ref().unwrap();
    assert_eq!(live.pagers.len(), 2);
    assert_eq!(live.pagers[0].cursor, Some(cursor));
}

#[test]
fn a_click_on_a_choice_takes_it_as_its_letter_does_and_keeps_the_reason() {
    let app = open_with(SIZE, vec![form(three())]);
    let mut clicked = app.clone();
    draw(&mut clicked);
    let place = |app: &App, target: Target| -> (u16, u16) {
        let (_, x, y, _) = hits(app).into_iter().find(|h| h.0 == target).unwrap();
        (x as u16, y as u16)
    };
    let b = place(&clicked, Target::Choice(1));
    let mut pressed = clicked.clone();
    click(&mut clicked, 3.0, b);
    send(&mut pressed, 3.0, Msg::Key(Key::Char('b')));
    assert_eq!(clicked, pressed);
    assert!(matches!(
        &need_card(&clicked).input,
        Input::Why {
            reply: Reply::Choice(1),
            ..
        }
    ));
    // With the line of the reason open, a click on another choice keeps the text of the line.
    type_text(&mut clicked, 3.0, "People compare");
    draw(&mut clicked);
    let a = place(&clicked, Target::Choice(0));
    click(&mut clicked, 3.0, a);
    match &need_card(&clicked).input {
        Input::Why { reply, line } => {
            assert_eq!(*reply, Reply::Choice(0));
            assert_eq!(line.text(), "People compare");
        }
        other => panic!("no line of the reason: {other:?}"),
    }
    // A tab of the question row opens its page, as its digit does.
    draw(&mut clicked);
    let third = place(&clicked, Target::Page(2));
    click(&mut clicked, 3.0, third);
    assert_eq!(need_card(&clicked).page, 2);
    assert_eq!(need_card(&clicked).input, Input::None);
}

#[test]
fn a_click_on_a_tab_of_the_activity_shows_it_and_the_marks_act_as_f_and_capital_f() {
    let mut app = working_live(SIZE);
    let buf = draw(&mut app);
    click(&mut app, 3.0, find(&buf, "Agents 7"));
    assert_eq!(app.activity, Filter::Agents);
    // The mark on the right acts as `f`.
    let mut clicked = app.clone();
    let buf = draw(&mut clicked);
    let mark = find_in(&buf, "›", 9).expect("more tabs on the right");
    let mut pressed = clicked.clone();
    click(&mut clicked, 3.0, mark);
    send(&mut pressed, 3.0, Msg::Key(Key::Char('f')));
    assert_eq!(clicked, pressed);
    assert_eq!(clicked.activity, Filter::Questions);
    // At the last tab, the mark on the left acts as `F`.
    for _ in 0..4 {
        send(&mut clicked, 3.0, Msg::Key(Key::Char('f')));
    }
    assert_eq!(clicked.activity, Filter::You);
    let buf = draw(&mut clicked);
    let mark = find_in(&buf, "‹", 9).expect("more tabs on the left");
    click(&mut clicked, 3.0, mark);
    assert_eq!(clicked.activity, Filter::Problems);
}

/// The live view of [`working_live`] with an activity that has more lines than its rows.
fn long_live() -> App {
    let mut list = events();
    for i in 0..30 {
        list.push((1520 + i, says(&format!("Line {i} of a long turn."))));
    }
    let tickets = working_live(SIZE).held().to_vec();
    live(SIZE, tickets, Vec::new(), list)
}

#[test]
fn the_wheel_scrolls_the_activity_under_the_pointer_as_the_arrows_do() {
    let mut app = long_live();
    let buf = draw(&mut app);
    let at = app.clock.seconds();
    let feed = find(&buf, "Line 29 of a long turn.");
    assert!(app.live.as_ref().unwrap().feed.follow);
    // At the end, a step down moves nothing: no change and no redraw.
    let mut wheeled = app.clone();
    mouse(&mut wheeled, at, MouseKind::WheelDown, feed);
    assert_eq!(wheeled, ticked(&app));
    assert!(!wheeled.dirty);
    // A step up scrolls three lines, as three presses of the up arrow do.
    let mut wheeled = app.clone();
    let mut pressed = app.clone();
    mouse(&mut wheeled, at, MouseKind::WheelUp, feed);
    for _ in 0..3 {
        send(&mut pressed, at, Msg::Key(Key::Up));
    }
    assert_eq!(wheeled, pressed);
    let feed_state = &wheeled.live.as_ref().unwrap().feed;
    assert_eq!((feed_state.back, feed_state.follow), (3, false));
    assert!(wheeled.dirty);
    // The wheel over the path does nothing.
    let path = find(&buf, "refine");
    let mut wheeled = app.clone();
    mouse(&mut wheeled, at, MouseKind::WheelUp, path);
    assert_eq!(wheeled, ticked(&app));
}

#[test]
fn the_wheel_moves_the_selection_of_home_scrolls_the_log_and_scrolls_a_card() {
    // Home: the wheel over a box acts as the arrows.
    let mut app = home(SIZE);
    let buf = draw(&mut app);
    mouse(&mut app, 1.5, MouseKind::WheelDown, find(&buf, "DEMO-9"));
    assert_eq!(app.home.selected.as_deref(), Some("DEMO-7"));
    // The log: one step moves the cursor three rows.
    let mut app = working_live(SIZE);
    send(&mut app, 3.0, Msg::Key(Key::Char('l')));
    let buf = draw(&mut app);
    let last = app.live.as_ref().unwrap().pagers[0].cursor.unwrap();
    mouse(
        &mut app,
        3.0,
        MouseKind::WheelUp,
        find(&buf, "took the ticket"),
    );
    assert_eq!(app.live.as_ref().unwrap().pagers[0].cursor, Some(last - 3));
    // A long card: one step scrolls its body three rows.
    let mut app = open_with((100, 28), vec![plan_item(false)]);
    let (total, shown) = body_size(&app);
    assert!(total >= shown + 3, "{total} {shown}");
    let buf = draw(&mut app);
    mouse(
        &mut app,
        2.5,
        MouseKind::WheelDown,
        find(&buf, "## Decisions"),
    );
    assert_eq!(need_card(&app).scroll, 3);
}

#[test]
fn a_click_on_the_box_of_a_ticket_that_needs_the_person_acts_as_its_key() {
    let mut held = needing(
        "DEMO-12",
        "Export the report of the month as a CSV file",
        "plan",
    );
    held.facts.agents = vec!["scout".into(), "builder".into(), "judge".into()];
    let app = live(SIZE, vec![held], vec![card_need("DEMO-12")], Vec::new());
    let mut clicked = app.clone();
    let buf = draw(&mut clicked);
    let cell = find(&buf, "The Judge could not answer it");
    let mut pressed = clicked.clone();
    let by_click = click(&mut clicked, 3.0, cell);
    let by_key = send(&mut pressed, 3.0, Msg::Key(Key::Char('n')));
    assert_eq!(by_click, by_key);
    assert_eq!(clicked, pressed);
    assert!(matches!(clicked.card, Some(Card::Need(_))));
}

#[test]
fn a_click_on_the_bar_that_needs_you_acts_as_n() {
    let app = home_with(
        SIZE,
        local(two_tickets(), vec![card_need("DEMO-12")]),
        Some(queue()),
    );
    let mut clicked = app.clone();
    let buf = draw(&mut clicked);
    let cell = find(&buf, "needs you");
    let mut pressed = clicked.clone();
    let by_click = click(&mut clicked, 1.5, cell);
    let by_key = send(&mut pressed, 1.5, Msg::Key(Key::Char('n')));
    assert_eq!(by_click, by_key);
    assert_eq!(clicked, pressed);
    assert!(matches!(clicked.card, Some(Card::Need(_))));
}

#[test]
fn a_click_skips_the_loader_and_a_blocked_loader_takes_only_q() {
    let mut app = booting(SIZE);
    draw(&mut app);
    click(&mut app, 0.2, (40, 10));
    assert!(app.boot.skip);
    let mut blocked = booting(SIZE);
    send(
        &mut blocked,
        0.1,
        Msg::Setup(Err(SetupError {
            reason: "not set up".into(),
            what: "The home folder has no config.toml.".into(),
            fix: "Run vision init.".into(),
        })),
    );
    let buf = draw(&mut blocked);
    click(&mut blocked, 3.0, (40, 10));
    assert!(!blocked.boot.skip && !blocked.quit);
    let fx = click(&mut blocked, 3.0, find(&buf, "q leaves"));
    assert!(blocked.quit);
    assert!(fx.contains(&Effect::Quit));
}

#[test]
fn while_the_line_of_t_is_open_a_click_types_nothing_into_it() {
    let mut held = needing(
        "DEMO-12",
        "Export the report of the month as a CSV file",
        "plan",
    );
    held.facts.agents = vec!["scout".into(), "builder".into(), "judge".into()];
    let mut app = live(SIZE, vec![held], vec![card_need("DEMO-12")], Vec::new());
    send(&mut app, 3.0, Msg::Key(Key::Char('t')));
    type_text(&mut app, 3.0, "keep it");
    let buf = draw(&mut app);
    // The box of NOW acts as `n`, and `n` goes into the line now: the box is no part.
    let before = ticked(&app);
    click(&mut app, 3.0, find(&buf, "The Judge could not answer it"));
    assert_eq!(app, before);
    // A tab shows its tab, and the line keeps its text.
    click(&mut app, 3.0, find(&buf, "Agents"));
    assert_eq!(app.activity, Filter::Agents);
    let line = app.live.as_ref().unwrap().tell.as_ref().unwrap();
    assert_eq!(line.text(), "keep it");
    // `enter send` sends the line on the second click. The first click arms it, and the line
    // keeps its text.
    let buf = draw(&mut app);
    let send_at = find(&buf, "enter send");
    let fx = click(&mut app, 3.0, send_at);
    assert_eq!(fx, Vec::new());
    let line = app.live.as_ref().unwrap().tell.as_ref().unwrap();
    assert_eq!(line.text(), "keep it");
    let buf = draw(&mut app);
    assert!(find_in(&buf, "enter click again · esc cancel", send_at.1).is_some());
    let fx = click(&mut app, 3.0, send_at);
    assert!(app.live.as_ref().unwrap().tell.is_none());
    let tell = |kind: &JobKind| matches!(kind, JobKind::Action(Action::Tell { text, .. }) if text == "keep it");
    assert!(
        jobs(&fx).iter().any(|job| tell(&job.kind))
            || app.engine.queue.iter().any(|job| tell(&job.kind)),
        "{fx:?}"
    );
}

#[test]
fn the_back_and_the_forward_parts_go_through_the_places_that_the_person_visited() {
    let mut app = working_live(SIZE);
    draw(&mut app);
    assert_eq!(app.hits.click_at(1, 0), Some(&Act::Back));
    assert_eq!(
        app.hits.click_at(4, 0),
        None,
        "nothing to go forward to yet"
    );
    let fx = click(&mut app, 3.0, (1, 0));
    assert_eq!(app.screen, Screen::Home);
    assert_eq!(app.live, None);
    assert!(fx.contains(&Effect::Follow(None)));
    draw(&mut app);
    assert_eq!(app.hits.click_at(1, 0), None, "Home was the first place");
    assert_eq!(app.hits.click_at(4, 0), Some(&Act::Forward));
    let fx = click(&mut app, 3.0, (4, 0));
    assert_eq!(
        app.screen,
        Screen::Live {
            key: "DEMO-12".into()
        }
    );
    assert!(fx.contains(&Effect::LoadTicket("DEMO-12".into())));
    assert!(fx.contains(&Effect::Follow(Some("DEMO-12".into()))));
    // A key that goes to another place is a visit too, and a visit empties the forward list.
    draw(&mut app);
    click(&mut app, 3.0, (1, 0));
    assert_eq!(app.nav.forward, [live_place("DEMO-12", &[])]);
    send(&mut app, 3.0, Msg::Key(Key::Char('m')));
    assert_eq!(app.screen, Screen::Models);
    assert!(app.nav.forward.is_empty());
    assert_eq!(app.nav.back, [Place::Home]);
    send(&mut app, 3.0, Msg::Key(Key::Esc));
    assert_eq!(app.nav.back, [Place::Home, Place::Models]);
}

#[test]
fn a_click_on_a_name_of_the_header_goes_to_its_place() {
    let mut app = working_live(SIZE);
    send(&mut app, 3.0, Msg::Key(Key::Char('l')));
    let fx = send(&mut app, 3.0, Msg::Key(Key::Enter));
    assert!(fx.contains(&Effect::LoadTurn {
        key: "DEMO-12".into(),
        turn: TURN.into()
    }));
    let buf = draw(&mut app);
    let head = super::support::text(&buf);
    assert!(
        head.lines()
            .next()
            .unwrap()
            .contains("‹  ›  ◆ VISION ▸ DEMO-12 ▸ log ▸ turn build-1 of 14:24"),
        "{head}"
    );
    // The name of the place that shows is no part.
    let current = find_in(&buf, "turn build-1", 0).unwrap();
    assert_eq!(app.hits.click_at(current.0, current.1), None);
    let fx = click(&mut app, 3.0, find_in(&buf, "log", 0).unwrap());
    assert!(fx.is_empty(), "the log stays open: {fx:?}");
    assert_eq!(pager_kinds(&app), [PagerKind::Log]);
    let buf = draw(&mut app);
    click(&mut app, 3.0, find_in(&buf, "DEMO-12", 0).unwrap());
    assert_eq!(pager_kinds(&app), []);
    assert_eq!(
        app.screen,
        Screen::Live {
            key: "DEMO-12".into()
        }
    );
    let buf = draw(&mut app);
    let fx = click(&mut app, 3.0, find_in(&buf, "VISION", 0).unwrap());
    assert_eq!(app.screen, Screen::Home);
    assert!(fx.contains(&Effect::Follow(None)));
    // On Home, the name of Home is no part.
    let buf = draw(&mut app);
    let before = ticked(&app);
    click(&mut app, 3.0, find_in(&buf, "VISION", 0).unwrap());
    assert_eq!(app, before);
}

#[test]
fn back_to_a_live_view_opens_its_pagers_again_and_its_log_waits_for_the_data() {
    let mut app = working_live(SIZE);
    let data = app.live.as_ref().unwrap().data.data.clone().unwrap();
    send(&mut app, 3.0, Msg::Key(Key::Char('l')));
    send(&mut app, 3.0, Msg::Key(Key::Enter));
    let buf = draw(&mut app);
    click(&mut app, 3.0, find_in(&buf, "VISION", 0).unwrap());
    assert_eq!(app.screen, Screen::Home);
    draw(&mut app);
    let fx = click(&mut app, 3.0, (1, 0));
    let turn = PagerKind::Turn { id: TURN.into() };
    assert_eq!(pager_kinds(&app), [PagerKind::Log, turn.clone()]);
    let live = app.live.as_ref().unwrap();
    assert!(live.pagers.iter().all(|p| p.state == PagerState::Loading));
    assert!(fx.contains(&Effect::LoadTicket("DEMO-12".into())));
    assert!(fx.contains(&Effect::LoadTurn {
        key: "DEMO-12".into(),
        turn: TURN.into()
    }));
    // A failed load of the data says why in the log, and the next data fills it.
    send(
        &mut app,
        3.05,
        Msg::Ticket {
            key: "DEMO-12".into(),
            data: Box::new(Err("the ledger is busy".into())),
        },
    );
    let log = &app.live.as_ref().unwrap().pagers[0];
    assert_eq!(log.state, PagerState::Failed("the ledger is busy".into()));
    // The data fills the log, and the place stays the same.
    send(
        &mut app,
        3.1,
        Msg::Ticket {
            key: "DEMO-12".into(),
            data: Box::new(Ok(data)),
        },
    );
    let log = &app.live.as_ref().unwrap().pagers[0];
    assert_eq!(log.state, PagerState::Ready);
    assert_eq!(log.lines.len(), timeline().len());
    assert_eq!(
        app.nav.at,
        Some(live_place("DEMO-12", &[PagerKind::Log, turn]))
    );
    assert_eq!(app.nav.forward, [Place::Home]);
}

#[test]
fn a_pager_that_closes_after_its_load_leaves_no_place_in_the_history() {
    let mut app = working_live(SIZE);
    send(&mut app, 3.0, Msg::Key(Key::Char('p')));
    assert_eq!(app.nav.back, [Place::Home, live_place("DEMO-12", &[])]);
    // The ticket has no plan, so the pager closes.
    send(
        &mut app,
        3.1,
        Msg::Plan {
            key: "DEMO-12".into(),
            plan: Ok(None),
        },
    );
    assert_eq!(pager_kinds(&app), []);
    assert_eq!(app.nav.back, [Place::Home]);
    assert_eq!(app.nav.at, Some(live_place("DEMO-12", &[])));
}

#[test]
fn a_click_on_a_key_of_the_keys_card_closes_it_and_acts_as_that_key() {
    let mut app = home(SIZE);
    send(&mut app, 1.5, Msg::Key(Key::Char('?')));
    let buf = draw(&mut app);
    let fx = click(&mut app, 1.5, find(&buf, "the model of each agent"));
    assert_eq!(app.card, None);
    assert_eq!(app.screen, Screen::Models);
    assert!(fx.contains(&Effect::LoadModels));
    // A row of more keys has a part for each key: `p` of `l  p  d` opens the plan.
    let mut live = working_live(SIZE);
    send(&mut live, 3.0, Msg::Key(Key::Char('?')));
    let buf = draw(&mut live);
    let (x, y) = find(&buf, "l  p  d");
    assert_eq!(live.hits.click_at(x + 1, y), None, "the space is no part");
    let fx = click(&mut live, 3.0, (x + 3, y));
    assert_eq!(live.card, None);
    assert_eq!(pager_kinds(&live), [PagerKind::Plan]);
    assert!(fx.contains(&Effect::LoadPlan("DEMO-12".into())));
    // In Models, Enter and `u` record a decision: their rows are no parts.
    let mut models = models_open(SIZE);
    let at = models.clock.seconds();
    send(&mut models, at, Msg::Key(Key::Char('?')));
    let buf = draw(&mut models);
    let (x, y) = find(&buf, "pin the values that show");
    assert_eq!(models.hits.click_at(x, y), None);
    let (x, y) = find(&buf, "go back to the default");
    assert_eq!(models.hits.click_at(x, y), None);
    let (x, y) = find(&buf, "change its effort");
    assert_eq!(models.hits.click_at(x, y), Some(&Act::Run(Key::Char('e'))));
    // A typed key keeps the card open, as before.
    send(&mut models, at, Msg::Key(Key::Char('e')));
    assert_eq!(models.card, Some(Card::Help));
}

#[test]
fn a_click_on_the_turns_the_commits_or_the_plan_opens_them_as_their_keys_do() {
    let mut app = working_live(SIZE);
    let buf = draw(&mut app);
    let right = Layout::of(110, 32).right_x() as u16;
    for (name, key) in [("turns", 'l'), ("commits", 'd'), ("plan", 'p')] {
        let cell = find_between(&buf, name, right, 110);
        let mut clicked = app.clone();
        let mut pressed = app.clone();
        let at = app.clock.seconds();
        let by_click = click(&mut clicked, at, cell);
        let by_key = send(&mut pressed, at, Msg::Key(Key::Char(key)));
        assert_eq!(by_click, by_key, "the effects of {name}");
        assert_eq!(clicked, pressed, "the model after {name}");
        assert_eq!(pager_kinds(&clicked).len(), 1, "{name} opens a pager");
    }
    // A mark that does not exist yet is no part: no CI ran.
    let ci = find_between(&buf, "CI", right, 110);
    assert_eq!(app.hits.click_at(ci.0, ci.1), None);
}

#[test]
fn a_click_on_a_visited_step_of_the_path_opens_the_log_at_that_step() {
    let mut app = working_live(SIZE);
    let buf = draw(&mut app);
    let left = Layout::of(110, 32).left_line() as u16;
    let review = find_between(&buf, "review", 0, left);
    assert_eq!(
        app.hits.click_at(review.0, review.1),
        None,
        "the ticket did not visit review"
    );
    let fx = click(&mut app, 3.0, find_between(&buf, "plan ", 0, left));
    assert!(fx.is_empty(), "{fx:?}");
    let pager = app.live.as_ref().unwrap().pagers.last().unwrap();
    assert_eq!(pager.kind, PagerKind::Log);
    // The last line that names plan is the move from plan to plan-ready.
    assert_eq!(pager.cursor, Some(7));
    // The current step opens the log too.
    let mut current = working_live(SIZE);
    let buf = draw(&mut current);
    click(&mut current, 3.0, find_between(&buf, "build", 0, left));
    let pager = current.live.as_ref().unwrap().pagers.last().unwrap();
    assert_eq!(pager.cursor, Some(timeline().len() - 1));
}

#[test]
fn a_click_on_a_line_of_a_turn_in_the_activity_opens_the_events_of_the_turn() {
    let mut app = working_live(SIZE);
    let buf = draw(&mut app);
    // A line of the ledger with no turn is no part.
    let (x, y) = find(&buf, "moves from plan-ready");
    assert_eq!(app.hits.click_at(x, y), None);
    // A line of the stream file of the turn that runs.
    let fx = click(&mut app, 3.0, find(&buf, "3 tests pass"));
    assert_eq!(pager_kinds(&app), [PagerKind::Turn { id: TURN.into() }]);
    assert!(fx.contains(&Effect::LoadTurn {
        key: "DEMO-12".into(),
        turn: TURN.into()
    }));
    // A line of a turn in the ledger opens its turn too.
    let mut ledger = working_live(SIZE);
    let buf = draw(&mut ledger);
    click(&mut ledger, 3.0, find(&buf, "the turn of scout ended"));
    assert_eq!(
        pager_kinds(&ledger),
        [PagerKind::Turn {
            id: "20261116T135204Z-DEMO-12-refine-1".into()
        }]
    );
}

#[test]
fn a_click_on_a_mark_of_more_rows_selects_the_next_row_out_of_view() {
    let mut app = models_many((100, 28));
    let buf = draw(&mut app);
    let shown = app.models.selected;
    assert_eq!(shown, 0);
    click(&mut app, 3.0, find(&buf, "▼ 8 more"));
    let next = app.models.selected;
    assert!(next > 0, "an agent below the window is selected");
    let buf = draw(&mut app);
    let (x, y) = find(&buf, "▲");
    click(&mut app, 3.0, (x, y));
    assert_eq!(
        app.models.selected, 0,
        "the agent above the window is selected"
    );
}

#[test]
fn while_the_line_of_t_is_open_the_header_and_the_links_of_the_view_are_no_parts() {
    let mut app = working_live(SIZE);
    send(&mut app, 3.0, Msg::Key(Key::Char('t')));
    type_text(&mut app, 3.0, "hi");
    draw(&mut app);
    let parts: Vec<&Act> = app.hits.clicks().iter().map(|(_, act)| act).collect();
    for act in &parts {
        assert!(
            !matches!(
                act,
                Act::Back | Act::Forward | Act::Go(_) | Act::Step(_) | Act::Turn(_)
            ),
            "{act:?} in {parts:?}"
        );
        assert!(
            !matches!(act, Act::Key(Key::Char('l' | 'd' | 'p'))),
            "{act:?} in {parts:?}"
        );
    }
}
