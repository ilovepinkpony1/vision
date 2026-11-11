//! The cards of the things that wait against a temporary home with the demo adapter and the
//! fake `claude`, through the real loaders and the worker with the built binary. The engine of
//! the app is paused, so only the ticks of the test and the passes of a chain move a ticket.
//!
//! - A question: its card answers it with a reason, the ledger keeps the reason, and the ticket
//!   goes on.
//! - The plan: the card approves the hash that it showed; a plan that changed on disk is not
//!   approved.
//! - The first write of a verb in the mode shadow: `y` and `a`, with the same rows in the
//!   ledger as the commands of today; a write whose arguments changed in the middle is not
//!   approved; a mode that the person changed stops the chain; a failed step stops it.
//! - The take over of a running turn: stop, the session, then take and retry; the next turn
//!   resumes the session. A failed step says that the ticket is stopped and paused, and the
//!   card of `n` continues it.
//!
//! A wrapper of the binary fails one command on purpose: the command whose marker file
//! `fail-<command>` is in the home.

use std::path::PathBuf;
use std::time::Duration;

use serde_json::{json, Value};
use vision::app::model::{App, Card, Screen};
use vision::app::msg::{ChildResult, Handoff, Key, Msg};
use vision::app::needs::NeedItem;
use vision::app::runtime::Runtime;
use vision::app::view::theme::ColorMode;
use vision::app::worker::WorkerConfig;

use super::pipeline_support::{ends_with, fake_claude, result, simple_scenarios, World, KEY, PLAN};

const LIMIT: Duration = Duration::from_secs(120);

/// The wrapper of the built binary in the folder of the world.
fn wrapper(world: &World) -> PathBuf {
    let path = world.vision.parent().unwrap().join("vision-wrapper");
    let script = format!(
        "#!/bin/sh\nif [ -f \"$VISION_HOME/fail-$1\" ]; then\n  echo \"error: the $1 failed in the test\" >&2\n  exit 1\nfi\nexec '{}' \"$@\"\n",
        env!("CARGO_BIN_EXE_vision")
    );
    std::fs::write(&path, script).unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    path
}

fn runtime(world: &World) -> Runtime {
    let mut env = vec![
        ("PATH".into(), std::env::var_os("PATH").unwrap_or_default()),
        ("HOME".into(), world.user_home.clone().into_os_string()),
        ("VISION_CLAUDE_BIN".into(), fake_claude().into_os_string()),
    ];
    if let Some(tmp) = std::env::var_os("TMPDIR") {
        env.push(("TMPDIR".into(), tmp));
    }
    let worker = WorkerConfig {
        exe: wrapper(world),
        home: world.vision.clone(),
        env: Some(env),
    };
    Runtime::new(
        world.ctx().paths.clone(),
        worker,
        (110, 32),
        ColorMode::True,
    )
}

/// The app on Home after the loader and its first pass, with the engine paused.
fn start(world: &World) -> Runtime {
    let mut rt = runtime(world);
    rt.start();
    assert!(
        rt.run_until(LIMIT, |app| app.screen == Screen::Home
            && app.local.data.is_some()
            && app.engine.running.is_none()
            && app.engine.queue.is_empty()),
        "the loader never ended: {:?}",
        rt.app.boot
    );
    rt.handle(Msg::Key(Key::Char('p')));
    assert!(rt.app.engine.paused);
    rt
}

/// `n`, and the first item that its card shows.
fn open_need(rt: &mut Runtime) -> NeedItem {
    assert!(
        rt.run_until(LIMIT, |app| !app.needs().is_empty()),
        "the local data has no need"
    );
    rt.handle(Msg::Key(Key::Char('n')));
    assert!(
        rt.run_until(LIMIT, |app| matches!(
            &app.card,
            Some(Card::Need(card)) if card.item().is_some()
        )),
        "the card never loaded: {:?}",
        rt.app.card
    );
    shown(&rt.app)
}

fn shown(app: &App) -> NeedItem {
    match &app.card {
        Some(Card::Need(card)) => card.item().cloned().expect("an item"),
        other => panic!("no card of the needs: {other:?}"),
    }
}

/// Waits until the chain ended, well or not.
fn wait_chain(rt: &mut Runtime) {
    assert!(
        rt.run_until(LIMIT, |app| app.chain.as_ref().is_none_or(|c| !c.running())),
        "the chain never ended: {:?}",
        rt.app.chain
    );
}

fn notice(app: &App) -> String {
    app.notice
        .as_ref()
        .map(|n| n.text.clone())
        .unwrap_or_default()
}

fn failure(app: &App) -> (String, String) {
    match app.chain.as_ref().and_then(|c| c.end.clone()) {
        Some(vision::app::chain::End::Failed { text, state }) => (text, state),
        other => panic!("the chain did not fail: {other:?}\n{:?}", app.notice),
    }
}

fn mode_of(world: &World, verb: &str) -> Option<String> {
    let text = std::fs::read_to_string(world.vision.join("config.toml")).unwrap();
    let doc: toml_edit::DocumentMut = text.parse().unwrap();
    doc.get("modes")
        .and_then(|m| m.get(verb))
        .and_then(|v| v.as_str())
        .map(str::to_string)
}

fn set_mode(world: &World, verb: &str, mode: &str) {
    world.edit_config(|doc| {
        doc["modes"][verb] = toml_edit::value(mode);
    });
}

fn has_flag(world: &World, flag: &str) -> bool {
    world.flags().iter().any(|f| f == flag)
}

/// One row of the table `human`: the ticket, the kind, the object, the value, the reason, and
/// whether it has seconds.
type Human = (
    Option<String>,
    String,
    String,
    Option<String>,
    Option<String>,
    bool,
);

/// The rows of the ledger that a person or a command made, with the parts that differ
/// between two worlds (the times, the ids, the commits, the folders) left out or replaced.
#[derive(Debug)]
struct Rows {
    human: Vec<Human>,
    /// actor and kind of each event of the ticket.
    events: Vec<(String, String)>,
    /// verb, mode, state, and arguments of each operation of the ticket.
    ops: Vec<(String, String, String, String)>,
    /// kind and state of each outcome of the operations of the ticket.
    outcomes: Vec<(String, String)>,
}

fn rows(world: &World) -> Rows {
    let head = world.row().and_then(|r| r.head).unwrap_or_default();
    let clone = world.clone().display().to_string();
    let tidy = |text: String| -> String {
        let mut out = text.replace(&clone, "<clone>");
        if !head.is_empty() {
            out = out.replace(&head, "<head>").replace(&head[..12], "<head>");
        }
        while let Some(at) = out.find("op-") {
            let end = out[at + 3..]
                .find(|c: char| !c.is_ascii_alphanumeric())
                .map_or(out.len(), |n| at + 3 + n);
            out.replace_range(at..end, "<op>");
        }
        out
    };
    let conn = rusqlite::Connection::open(world.vision.join("ledger.sqlite")).unwrap();
    let mut stmt = conn
        .prepare("SELECT ticket, kind, object, value, reason, seconds FROM human ORDER BY id")
        .unwrap();
    let human = stmt
        .query_map([], |r| {
            Ok((
                r.get::<_, Option<String>>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, Option<String>>(3)?,
                r.get::<_, Option<String>>(4)?,
                r.get::<_, Option<i64>>(5)?.is_some(),
            ))
        })
        .unwrap()
        .map(|r| {
            let (t, k, o, v, why, s) = r.unwrap();
            (t, k, tidy(o), v.map(tidy), why.map(tidy), s)
        })
        .collect();
    let mut stmt = conn
        .prepare("SELECT coalesce(actor, ''), coalesce(kind, '') FROM events WHERE ticket = ?1 ORDER BY id")
        .unwrap();
    let events = stmt
        .query_map([KEY], |r| Ok((r.get(0)?, r.get(1)?)))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    let mut stmt = conn
        .prepare("SELECT verb, coalesce(mode, ''), coalesce(state, ''), coalesce(args, '') FROM operations WHERE ticket = ?1 ORDER BY rowid")
        .unwrap();
    let ops = stmt
        .query_map([KEY], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))
        .unwrap()
        .map(|r| {
            let (v, m, s, a): (String, String, String, String) = r.unwrap();
            (v, m, s, tidy(a))
        })
        .collect();
    let mut stmt = conn
        .prepare("SELECT coalesce(o.kind, ''), coalesce(o.state, '') FROM outcomes o JOIN operations p ON o.op = p.id WHERE p.ticket = ?1 ORDER BY o.rowid")
        .unwrap();
    let outcomes = stmt
        .query_map([KEY], |r| Ok((r.get(0)?, r.get(1)?)))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    Rows {
        human,
        events,
        ops,
        outcomes,
    }
}

/// The rows of two worlds are the same, field by field.
fn same_rows(app: &World, commands: &World) {
    let (a, b) = (rows(app), rows(commands));
    assert_eq!(a.human, b.human, "the human rows");
    assert_eq!(a.events, b.events, "the events");
    assert_eq!(a.ops, b.ops, "the operations");
    assert_eq!(a.outcomes, b.outcomes, "the outcomes");
}

// ----- A question ------------------------------------------------------------------------

fn card(text: &str) -> Value {
    json!({
        "text": text,
        "context": "The ticket does not say it.",
        "example": "subtract(5, 3) gives 2.",
        "evidence": "calc.py:4",
        "kind": "product",
        "area": ["calculator"],
        "topics": [],
        "options": [{"label": "Yes", "means": "do it"}, {"label": "No", "means": "leave it"}],
        "recommended": 0,
        "why": "It matches add."
    })
}

/// A world where scout asks one question that the Judge sends to the person, and the
/// ticket waits for the answer.
fn question_world() -> World {
    let world = World::new();
    simple_scenarios(&world);
    world.scenario(
        "scout",
        "refine",
        json!([
            ends_with(
                json!({"kind": "questions", "summary": "One question.", "questions": [
                    card("Must the README name subtract?")
                ]})
            ),
            ends_with(result(
                "Refined.",
                &[
                    ("refine.md", "Research."),
                    ("description.md", "Add subtract(a, b).")
                ]
            ))
        ]),
    );
    world.scenario(
        "judge",
        "",
        json!([[{"do": "init"}, {"do": "result", "structured_output": {"answers": [
            {"card": "Q-0001", "outcome": "ask", "basis": "none", "sources": [], "sure": false,
             "choice": null, "topics": [], "reason": "No decision covers the README."}
        ]}, "cost": 0.05}]]),
    );
    world.run(&["take", KEY]).expect_code(0);
    world.tick_until("the card for the person", LIMIT, |w| {
        let status = w.run(&["status", "--json"]).json();
        status["needs"]
            .as_array()
            .is_some_and(|n| n.iter().any(|n| n["kind"] == "card"))
    });
    world
}

#[test]
fn a_question_answered_through_its_card_reaches_the_ledger_and_the_ticket_goes_on() {
    let world = question_world();
    let mut rt = start(&world);
    let NeedItem::Question(view) = open_need(&mut rt) else {
        panic!("not a question: {:?}", shown(&rt.app));
    };
    assert_eq!(view.id, "Q-0001");
    assert_eq!(view.question, "Must the README name subtract?");
    assert_eq!(
        view.judge_reason.as_deref(),
        Some("No decision covers the README.")
    );
    rt.handle(Msg::Key(Key::Char('a')));
    for c in "People read the README".chars() {
        rt.handle(Msg::Key(Key::Char(c)));
    }
    rt.handle(Msg::Key(Key::Enter));
    wait_chain(&mut rt);
    assert_eq!(rt.app.chain, None, "{:?}", rt.app.notice);
    assert!(
        notice(&rt.app).starts_with("Saved your answer to Q-0001 with its reason."),
        "{}",
        notice(&rt.app)
    );
    assert_eq!(rt.app.card, None);

    // The same answer with the command of today, in a second world.
    let other = question_world();
    other
        .run(&[
            "answer",
            "Q-0001",
            "--choice",
            "1",
            "--why",
            "People read the README",
        ])
        .expect_code(0);
    same_rows(&world, &other);
    let rows_app = rows(&world);
    assert!(
        rows_app
            .human
            .iter()
            .any(|h| h.4.as_deref() == Some("People read the README")),
        "{:?}",
        rows_app.human
    );

    // The ticket goes on: the turn gets the answer, and the plan step comes.
    world.tick_to("plan");
    assert_eq!(world.turns_of("refine"), 2);
    world.settle();
}

// ----- The plan --------------------------------------------------------------------------

#[test]
fn the_plan_card_approves_the_hash_that_it_showed_and_not_a_changed_plan() {
    let world = World::new();
    simple_scenarios(&world);
    world.run(&["take", KEY]).expect_code(0);
    world.tick_to("plan-ready");
    let mut rt = start(&world);
    let NeedItem::Plan(plan) = open_need(&mut rt) else {
        panic!("not the plan: {:?}", shown(&rt.app));
    };
    assert_eq!(plan.plan.text, PLAN);
    assert_eq!(plan.plan.hash.len(), 64);

    // The plan changes on disk after the card showed it: `a` approves nothing.
    let path = world.vision.join("tickets").join(KEY).join("plan.md");
    std::fs::write(&path, format!("{PLAN}\n- subtract also takes floats.\n")).unwrap();
    rt.handle(Msg::Key(Key::Char('a')));
    assert!(
        rt.run_until(LIMIT, |app| matches!(
            &app.card,
            Some(Card::Need(card)) if card.note.is_some()
                && matches!(card.item(), Some(NeedItem::Plan(p)) if p.plan.hash != plan.plan.hash)
        )),
        "the card did not load the new plan: {:?}",
        rt.app.card
    );
    assert_eq!(world.step().as_deref(), Some("plan-ready"));
    assert!(!rows(&world).human.iter().any(|h| h.1 == "approve-plan"));

    // `a` again approves the new plan with its full hash.
    let NeedItem::Plan(new) = shown(&rt.app) else {
        unreachable!()
    };
    rt.handle(Msg::Key(Key::Char('a')));
    wait_chain(&mut rt);
    assert!(
        notice(&rt.app).starts_with(&format!("Approved the plan {}.", &new.plan.hash[..12])),
        "{}",
        notice(&rt.app)
    );
    let human = rows(&world).human;
    assert!(
        human
            .iter()
            .any(|h| h.1 == "approve-plan" && h.3.as_deref() == Some(&new.plan.hash[..12])),
        "the ledger names the hash: {human:?}"
    );
    world.tick_to("build");
    world.settle();
}

// ----- The first write -------------------------------------------------------------------

/// A world where the after entry tracker.describe of refine is in the mode shadow, so the
/// ticket waits at refine with the flag `needs-you`.
fn shadow_world() -> World {
    let world = World::new();
    simple_scenarios(&world);
    set_mode(&world, "tracker.describe", "shadow");
    world.run(&["take", KEY]).expect_code(0);
    world.tick_until("the flag needs-you", LIMIT, |w| has_flag(w, "needs-you"));
    assert_eq!(world.step().as_deref(), Some("refine"));
    world
}

/// The first write card of the shadow world.
fn first_write(rt: &mut Runtime) -> vision::app::needs::WriteItem {
    match open_need(rt) {
        NeedItem::FirstWrite(write) => write,
        other => panic!("not a first write: {other:?}"),
    }
}

fn confirmed(world: &World, verb: &str) -> bool {
    rows(world)
        .ops
        .iter()
        .any(|o| o.0 == verb && o.2 == "confirmed")
}

#[test]
fn y_on_the_first_write_ends_with_the_write_sent_and_the_verb_in_the_mode_ask() {
    let world = shadow_world();
    let mut rt = start(&world);
    let write = first_write(&mut rt);
    assert_eq!(write.verb, "tracker.describe");
    assert_eq!(write.mode, "shadow");
    assert_eq!(write.step, "refine");
    assert!(write.turn_held);
    let intent = write.intent.clone().expect("the recorded intent");
    assert_eq!(intent["args"]["text"], "Add subtract(a, b).");
    rt.handle(Msg::Key(Key::Char('y')));
    wait_chain(&mut rt);
    assert_eq!(rt.app.chain, None, "{:?}", rt.app.chain);
    assert_eq!(
        notice(&rt.app),
        "Sent: tracker.describe of DEMO-12 ran. Vision asks you each time for tracker.describe."
    );
    assert_eq!(mode_of(&world, "tracker.describe").as_deref(), Some("ask"));
    assert!(confirmed(&world, "tracker.describe"));
    assert_eq!(world.acts_of("tracker.describe"), 1);

    // The three commands of today, in a second world, give the same rows.
    let other = shadow_world();
    other.run(&["promote", "tracker.describe"]).expect_code(0);
    other.run(&["retry", KEY]).expect_code(0);
    other.tick();
    let head = other.row().unwrap().head.unwrap();
    other
        .run(&["approve", KEY, "tracker.describe", &head])
        .expect_code(0);
    same_rows(&world, &other);

    world.tick_to("plan");
    world.settle();
    other.settle();
}

#[test]
fn a_on_the_first_write_ends_with_the_verb_in_the_mode_auto_and_the_write_sent() {
    let world = shadow_world();
    let mut rt = start(&world);
    first_write(&mut rt);
    rt.handle(Msg::Key(Key::Char('a')));
    wait_chain(&mut rt);
    assert_eq!(rt.app.chain, None, "{:?}", rt.app.chain);
    assert!(
        notice(&rt.app).starts_with(
            "Sent: tracker.describe of DEMO-12 ran. tracker.describe is in the mode auto now."
        ),
        "{}",
        notice(&rt.app)
    );
    assert_eq!(mode_of(&world, "tracker.describe").as_deref(), Some("auto"));
    assert!(confirmed(&world, "tracker.describe"));

    let other = shadow_world();
    other.run(&["promote", "tracker.describe"]).expect_code(0);
    other.run(&["promote", "tracker.describe"]).expect_code(0);
    other.run(&["retry", KEY]).expect_code(0);
    other.tick();
    same_rows(&world, &other);
    world.settle();
    other.settle();
}

#[test]
fn a_write_whose_arguments_changed_in_the_middle_is_not_approved() {
    let world = shadow_world();
    let mut rt = start(&world);
    let write = first_write(&mut rt);
    // The description changes after the card showed the write: the retry asks for a write
    // with other arguments.
    std::fs::write(
        world
            .vision
            .join("tickets")
            .join(KEY)
            .join("description.md"),
        "Add subtract(a, b) and divide(a, b).",
    )
    .unwrap();
    rt.handle(Msg::Key(Key::Char('y')));
    assert!(
        rt.run_until(LIMIT, |app| matches!(
            &app.card,
            Some(Card::Need(card)) if card.note.is_some()
                && matches!(card.item(), Some(NeedItem::Approval(_)))
        )),
        "the card did not show the new write: {:?} {:?}",
        rt.app.card,
        rt.app.chain
    );
    let NeedItem::Approval(now) = shown(&rt.app) else {
        unreachable!()
    };
    assert_ne!(now.op, write.op);
    assert_eq!(now.args["text"], "Add subtract(a, b) and divide(a, b).");
    assert!(!confirmed(&world, "tracker.describe"), "nothing was sent");
    assert_eq!(world.acts_of("tracker.describe"), 0);
    assert_eq!(mode_of(&world, "tracker.describe").as_deref(), Some("ask"));
    // The normal write card approves the write that it shows.
    rt.handle(Msg::Key(Key::Char('y')));
    wait_chain(&mut rt);
    assert!(confirmed(&world, "tracker.describe"));
    world.settle();
}

#[test]
fn a_mode_that_the_person_changed_stops_the_chain_before_it_acts() {
    let world = shadow_world();
    let mut rt = start(&world);
    first_write(&mut rt);
    world.run(&["promote", "tracker.describe"]).expect_code(0);
    rt.handle(Msg::Key(Key::Char('a')));
    wait_chain(&mut rt);
    let (text, state) = failure(&rt.app);
    assert_eq!(
        text,
        "tracker.describe is in the mode ask now, not in the mode shadow, so Vision does not change it."
    );
    assert_eq!(
        state,
        "Nothing was sent, and DEMO-12 still waits at the step refine."
    );
    assert_eq!(mode_of(&world, "tracker.describe").as_deref(), Some("ask"));
    assert!(!confirmed(&world, "tracker.describe"));
    world.settle();
}

#[test]
fn a_failed_step_stops_the_chain_and_the_card_says_what_is_true_now() {
    let world = shadow_world();
    let mut rt = start(&world);
    first_write(&mut rt);
    std::fs::write(world.vision.join("fail-retry"), "").unwrap();
    rt.handle(Msg::Key(Key::Char('y')));
    wait_chain(&mut rt);
    let (text, state) = failure(&rt.app);
    assert_eq!(text, "the retry failed in the test");
    assert_eq!(
        state,
        "tracker.describe is in the mode ask now. Nothing was sent, and DEMO-12 still waits at the step refine."
    );
    assert_eq!(mode_of(&world, "tracker.describe").as_deref(), Some("ask"));
    assert!(has_flag(&world, "needs-you"));
    assert!(!confirmed(&world, "tracker.describe"));
    // The person closes the app: nothing more runs, and the card of the next start shows
    // what waits. The verb is in the mode ask now, so the write is no longer kept back, and
    // the card of the flag offers the retry.
    drop(rt);
    std::fs::remove_file(world.vision.join("fail-retry")).unwrap();
    let mut rt = start(&world);
    let NeedItem::Flag(flag) = open_need(&mut rt) else {
        panic!("not the flag: {:?}", shown(&rt.app));
    };
    assert!(flag.held);
    rt.handle(Msg::Key(Key::Char('r')));
    wait_chain(&mut rt);
    assert_eq!(notice(&rt.app), "DEMO-12 goes on at the step refine.");
    world.tick();
    assert_eq!(
        rows(&world)
            .ops
            .last()
            .map(|o| (o.1.as_str(), o.2.as_str())),
        Some(("ask", "pending")),
        "the write waits for the approval in the mode ask"
    );
    world.settle();
}

// ----- The take over of a running turn ---------------------------------------------------

/// A world where the turn of scout at refine runs and waits, then a second turn ends.
fn running_world() -> World {
    let world = World::new();
    simple_scenarios(&world);
    world.scenario(
        "scout",
        "refine",
        json!([
            [
                {"do": "init"},
                {"do": "text", "text": "I read the ticket now."},
                {"do": "wait_input", "timeout": 60}
            ],
            ends_with(result(
                "Refined.",
                &[("refine.md", "Research."), ("description.md", "Add subtract(a, b).")]
            ))
        ]),
    );
    world.run(&["take", KEY]).expect_code(0);
    world.tick_until("the turn of scout", LIMIT, |w| {
        w.turns()
            .iter()
            .any(|t| t.0 == "refine" && t.2 == "running")
    });
    world
}

/// The live view of the ticket, then `o` and `o`: the stop runs, and the session waits for
/// the terminal. The test plays the session with `vision open` and gives its end to the app.
fn take_over(world: &World, rt: &mut Runtime) {
    while rt.app.home.selected.as_deref() != Some(KEY) {
        let before = rt.app.home.selected.clone();
        rt.handle(Msg::Key(Key::Down));
        assert_ne!(
            before, rt.app.home.selected,
            "the ticket is not in the list"
        );
    }
    rt.handle(Msg::Key(Key::Enter));
    assert!(
        rt.run_until(LIMIT, |app| app
            .live
            .as_ref()
            .and_then(|l| l.data.data.as_ref())
            .is_some_and(|d| d.facts.work.is_some())),
        "the live view never saw the turn"
    );
    rt.handle(Msg::Key(Key::Char('o')));
    assert!(matches!(&rt.app.card, Some(Card::Open(c)) if c.running.as_deref() == Some("scout")));
    rt.handle(Msg::Key(Key::Char('o')));
    assert!(
        rt.run_until(LIMIT, |app| app.chain.as_ref().is_some_and(|c| c.started
            && matches!(
                c.step().map(|s| &s.work),
                Some(vision::app::chain::Work::Session(_))
            ))),
        "the stop never ended: {:?}",
        rt.app.chain
    );
    assert!(has_flag(world, "stopped"));
    assert_eq!(rt.take_open(), Some(Handoff::Session(KEY.into())));
    let open = world.run(&["open", KEY]);
    open.expect_code(0);
    assert!(has_flag(world, "paused"));
    rt.handle(Msg::SessionEnded {
        key: KEY.into(),
        result: ChildResult {
            code: Some(0),
            ..ChildResult::default()
        },
    });
    assert!(
        matches!(&rt.app.card, Some(Card::Opened(c)) if c.took_over.as_deref() == Some("scout")),
        "{:?}",
        rt.app.card
    );
}

/// The turns of refine after the take over: the last one resumes the session of the first.
fn resumes_the_session(world: &World) {
    world.tick_until("a new turn of refine", LIMIT, |w| {
        w.ledger().turns_of_ticket(KEY).unwrap().len() >= 2
    });
    let turns = world.ledger().turns_of_ticket(KEY).unwrap();
    assert!(turns[0].session.is_some());
    assert_eq!(
        turns.last().unwrap().resumes,
        turns[0].session,
        "the new turn resumes the session that the person worked in"
    );
}

#[test]
fn the_take_over_of_a_running_turn_ends_with_a_turn_that_resumes_the_session() {
    let world = running_world();
    let mut rt = start(&world);
    take_over(&world, &mut rt);
    rt.handle(Msg::Key(Key::Enter));
    wait_chain(&mut rt);
    assert_eq!(rt.app.chain, None, "{:?}", rt.app.chain);
    assert_eq!(
        notice(&rt.app),
        "DEMO-12 goes on. Its next turn resumes the session that you worked in."
    );
    assert!(world.flags().is_empty(), "{:?}", world.flags());
    resumes_the_session(&world);
    world.settle();
}

#[test]
fn a_failed_take_after_the_session_says_the_state_and_n_continues_the_ticket() {
    let world = running_world();
    let mut rt = start(&world);
    take_over(&world, &mut rt);
    std::fs::write(world.vision.join("fail-take"), "").unwrap();
    rt.handle(Msg::Key(Key::Enter));
    wait_chain(&mut rt);
    let (text, state) = failure(&rt.app);
    assert_eq!(text, "the take failed in the test");
    assert_eq!(state, "DEMO-12 is stopped and paused.");
    assert!(has_flag(&world, "stopped") && has_flag(&world, "paused"));
    std::fs::remove_file(world.vision.join("fail-take")).unwrap();

    // Esc keeps it stopped; the card of `n` continues it.
    rt.handle(Msg::Key(Key::Esc));
    rt.handle(Msg::Key(Key::Esc));
    assert_eq!(rt.app.screen, Screen::Home);
    let NeedItem::Paused(flag) = open_need(&mut rt) else {
        panic!("not the paused ticket: {:?}", shown(&rt.app));
    };
    assert!(flag.stopped);
    rt.handle(Msg::Key(Key::Enter));
    wait_chain(&mut rt);
    assert_eq!(rt.app.chain, None, "{:?}", rt.app.chain);
    assert!(world.flags().is_empty(), "{:?}", world.flags());
    resumes_the_session(&world);
    world.settle();
}
