//! The screens Models and Stats: a text snapshot of each state at 110 by 32 and at 100 by 28,
//! and each key in the update: for Models the arrows, the effort, the pin, the reset, the mark
//! of a change that is not pinned yet, and Esc; for Stats the switch of the time window, a
//! result of the other window, and Esc.

use vision::app::chain::{Check, CheckFacts};
use vision::app::model::{App, Card, Screen};
use vision::app::model_table::{Edit, Evidence, ModelLine};
use vision::app::msg::{Effect, Key, Msg};
use vision::app::stats::{StatsData, StepBar, TicketLine};

use super::need_support::*;
use super::support::*;

const SIZE: (u16, u16) = (110, 32);
const SMALL: (u16, u16) = (100, 28);

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

fn line(agent: &str, model: &str, effort: &str, reason: &str) -> ModelLine {
    ModelLine {
        agent: agent.into(),
        model: model.into(),
        effort: effort.into(),
        pinned: false,
        since: None,
        default_model: model.into(),
        default_effort: effort.into(),
        reason: reason.into(),
        evidence: Vec::new(),
        recommendation: "fewer than 20 measured turns for a second model, so the default stays"
            .into(),
        recommended: None,
    }
}

/// The model table of the samples: builder is pinned to another model, and the table
/// recommends a cheaper one.
pub fn model_lines() -> Vec<ModelLine> {
    let mut builder = line(
        "builder",
        "claude-sonnet-5-5",
        "high",
        "It writes the code and uses the most tokens.",
    );
    builder.pinned = true;
    builder.since = Some("2026-11-02T10:00:00Z".into());
    builder.default_model = "claude-opus-5-5".into();
    builder.default_effort = "xhigh".into();
    builder.evidence = vec![
        Evidence {
            model: "claude-opus-5-5".into(),
            turns: 24,
            cost_per_turn: 0.84,
        },
        Evidence {
            model: "claude-sonnet-5-5".into(),
            turns: 21,
            cost_per_turn: 0.41,
        },
    ];
    builder.recommendation = "claude-sonnet-5-5: $0.41 for each turn, first try 81% against the best 83%. Run `vision model set builder claude-sonnet-5-5` to use it".into();
    builder.recommended = Some("claude-sonnet-5-5".into());
    let mut scout = line(
        "scout",
        "claude-fable-5-2",
        "high",
        "A wrong reading of the ticket costs the full build.",
    );
    scout.evidence = vec![Evidence {
        model: "claude-fable-5-2".into(),
        turns: 9,
        cost_per_turn: 0.12,
    }];
    vec![
        scout,
        builder,
        line(
            "reviewer",
            "claude-opus-5-5",
            "max",
            "The review is the last gate before a person reads the code.",
        ),
        line(
            "tester",
            "claude-opus-5-5",
            "high",
            "A false pass sends a bug to a person.",
        ),
        line(
            "shepherd",
            "claude-opus-5-5",
            "high",
            "It gives verdicts on review comments and fixes the valid ones.",
        ),
        line(
            "judge",
            "claude-fable-5-2",
            "high",
            "It can answer alone, so a wrong answer costs the most.",
        ),
        line(
            "curator",
            "claude-fable-5-2",
            "high",
            "It runs seldom, and its proposals change each later answer.",
        ),
    ]
}

/// Home, then `m`, then the model table.
fn models_open(size: (u16, u16)) -> App {
    let mut app = home(size);
    let fx = key(&mut app, 2.0, Key::Char('m'));
    assert_eq!(app.screen, Screen::Models);
    assert!(fx.contains(&Effect::LoadModels), "{fx:?}");
    send(&mut app, 2.1, Msg::Models(Box::new(Ok(model_lines()))));
    app
}

pub fn stats_data(week: bool) -> StatsData {
    let scale = if week { 1.0 } else { 3.0 };
    StatsData {
        week,
        cost_usd: 23.84 * scale,
        finished: if week { 2 } else { 7 },
        without_you: if week { 11 } else { 30 },
        questions: if week { 14 } else { 41 },
        minutes: 18.5 * scale,
        tickets: vec![
            TicketLine {
                key: "DEMO-12".into(),
                to_you: 2,
                by_memory: 3,
                by_judge: 4,
                minutes: 9.5,
                cost_usd: 12.40,
            },
            TicketLine {
                key: "DEMO-7".into(),
                to_you: 1,
                by_memory: 2,
                by_judge: 2,
                minutes: 9.0,
                cost_usd: 11.44,
            },
        ],
        steps: vec![
            StepBar {
                step: "refine".into(),
                seconds: 540,
                cost_usd: 1.10,
            },
            StepBar {
                step: "plan".into(),
                seconds: 900,
                cost_usd: 3.20,
            },
            StepBar {
                step: "build".into(),
                seconds: 3_600,
                cost_usd: 14.80,
            },
            StepBar {
                step: "review".into(),
                seconds: 1_200,
                cost_usd: 4.74,
            },
        ],
        agents: vec![
            ("scout".into(), 1.10),
            ("builder".into(), 18.00),
            ("reviewer".into(), 4.74),
        ],
        outside: vec![("curator".into(), 0.4 * scale)],
    }
}

fn stats_open(size: (u16, u16)) -> App {
    let mut app = home(size);
    let fx = key(&mut app, 2.0, Key::Char('s'));
    assert_eq!(app.screen, Screen::Stats);
    assert!(fx.contains(&Effect::LoadStats { week: true }), "{fx:?}");
    send(
        &mut app,
        2.1,
        Msg::Stats {
            week: true,
            data: Box::new(Ok(stats_data(true))),
        },
    );
    app
}

#[test]
fn the_screen_models() {
    assert_snapshot("models", &screen(&models_open(SIZE)));
    assert_snapshot("models_100x28", &screen(&models_open(SMALL)));
    let mut app = home(SIZE);
    key(&mut app, 2.0, Key::Char('m'));
    assert_snapshot("models_loading", &screen(&app));
    send(
        &mut app,
        2.1,
        Msg::Models(Box::new(Err("the configuration is not valid TOML".into()))),
    );
    assert_snapshot("models_error", &screen(&app));
}

#[test]
fn the_screen_models_with_a_change_and_a_pin_that_runs() {
    let mut app = models_open(SIZE);
    key(&mut app, 3.0, Key::Right);
    key(&mut app, 3.1, Key::Char('e'));
    assert_snapshot("models_changed", &screen(&app));
    let fx = key(&mut app, 3.2, Key::Enter);
    assert_eq!(jobs(&fx).len(), 1);
    send(&mut app, 3.3, Msg::Tick);
    assert_snapshot("models_pin_runs", &screen(&app));
    let mut app = models_open(SIZE);
    key(&mut app, 3.0, Key::Down);
    let fx = key(&mut app, 3.1, Key::Char('u'));
    drive(&mut app, 3.2, fx, Some("model"), none);
    assert_snapshot("models_reset_failed", &screen(&app));
    key(&mut app, 3.3, Key::Char('?'));
    assert_snapshot("models_keys", &screen(&app));
}

#[test]
fn the_arrows_change_the_model_and_e_the_effort_with_a_mark() {
    let mut app = models_open(SIZE);
    assert_eq!(app.models.selected, 0);
    key(&mut app, 3.0, Key::Up);
    assert_eq!(app.models.selected, 0);
    key(&mut app, 3.1, Key::Right);
    assert_eq!(
        app.models.edits.get("scout"),
        Some(&Edit {
            model: "claude-opus-5-5".into(),
            effort: "high".into()
        })
    );
    key(&mut app, 3.2, Key::Left);
    assert!(
        app.models.edits.is_empty(),
        "back to the values of the table"
    );
    key(&mut app, 3.3, Key::Left);
    assert_eq!(app.models.edits["scout"].model, "claude-haiku-5-5");
    key(&mut app, 3.4, Key::Char('e'));
    assert_eq!(app.models.edits["scout"].effort, "xhigh");
    for _ in 0..4 {
        key(&mut app, 3.5, Key::Char('e'));
    }
    assert_eq!(app.models.edits["scout"].effort, "high");
    // The arrows down and up, and j and k, select the agent.
    key(&mut app, 3.6, Key::Down);
    key(&mut app, 3.7, Key::Char('j'));
    assert_eq!(app.models.selected, 2);
    key(&mut app, 3.8, Key::Char('k'));
    assert_eq!(app.models.selected, 1);
    for _ in 0..10 {
        key(&mut app, 3.9, Key::Down);
    }
    assert_eq!(app.models.selected, 6);
    // Esc drops each change that is not pinned, and a second Esc goes home.
    key(&mut app, 4.0, Key::Esc);
    assert!(app.models.edits.is_empty());
    assert_eq!(app.screen, Screen::Models);
    assert_eq!(notice(&app), "Dropped each change that was not pinned.");
    key(&mut app, 4.1, Key::Esc);
    assert_eq!(app.screen, Screen::Home);
}

#[test]
fn enter_pins_the_values_that_show() {
    let mut app = models_open(SIZE);
    key(&mut app, 3.0, Key::Right);
    key(&mut app, 3.1, Key::Char('e'));
    let fx = key(&mut app, 3.2, Key::Enter);
    let done = drive(&mut app, 3.3, fx, None, none);
    assert_eq!(
        done.actions,
        [vec![
            "model".to_string(),
            "set".into(),
            "scout".into(),
            "claude-opus-5-5".into(),
            "--effort".into(),
            "xhigh".into()
        ]]
    );
    assert!(app.models.edits.is_empty());
    assert_eq!(
        notice(&app),
        "scout uses claude-opus-5-5 xhigh from its next turn."
    );
    assert!(
        done.other.contains(&Effect::LoadModels),
        "the table loads again: {:?}",
        done.other
    );

    // Enter with no change pins the values of the table.
    let mut app = models_open(SIZE);
    let fx = key(&mut app, 3.0, Key::Enter);
    let done = drive(&mut app, 3.1, fx, None, none);
    assert_eq!(
        done.actions[0],
        [
            "model",
            "set",
            "scout",
            "claude-fable-5-2",
            "--effort",
            "high"
        ]
    );
}

#[test]
fn u_goes_back_to_the_default_of_a_pinned_agent() {
    let mut app = models_open(SIZE);
    key(&mut app, 3.0, Key::Down);
    let fx = key(&mut app, 3.1, Key::Char('u'));
    let done = drive(&mut app, 3.2, fx, None, none);
    assert_eq!(done.actions, [["model", "reset", "builder"]]);
    assert_eq!(
        notice(&app),
        "builder uses the default again from its next turn: claude-opus-5-5 xhigh."
    );

    // An agent on its default needs no reset.
    let mut app = models_open(SIZE);
    let fx = key(&mut app, 3.0, Key::Char('u'));
    assert!(jobs(&fx).is_empty());
    assert_eq!(
        notice(&app),
        "scout already uses the default: claude-fable-5-2 high."
    );
}

#[test]
fn a_pin_that_fails_keeps_the_change_and_says_why() {
    let mut app = models_open(SIZE);
    key(&mut app, 3.0, Key::Right);
    let fx = key(&mut app, 3.1, Key::Enter);
    // A second Enter while the pin runs does nothing.
    assert!(jobs(&key(&mut app, 3.15, Key::Enter)).is_empty());
    drive(&mut app, 3.2, fx, Some("model"), none);
    assert!(app.models.edits.contains_key("scout"));
    assert!(matches!(
        app.chain.as_ref().and_then(|c| c.end.clone()),
        Some(vision::app::chain::End::Failed { .. })
    ));
}

#[test]
fn the_keys_card_of_models_and_q() {
    let mut app = models_open(SIZE);
    key(&mut app, 3.0, Key::Char('?'));
    assert_eq!(app.card, Some(Card::Help));
    key(&mut app, 3.1, Key::Esc);
    assert_eq!(app.card, None);
    assert_eq!(app.screen, Screen::Models);
    key(&mut app, 3.2, Key::Char('q'));
    assert_eq!(app.card, Some(Card::Quit));
}

#[test]
fn the_screen_stats() {
    assert_snapshot("stats_week", &screen(&stats_open(SIZE)));
    assert_snapshot("stats_week_100x28", &screen(&stats_open(SMALL)));
    let mut app = stats_open(SIZE);
    key(&mut app, 3.0, Key::Char('w'));
    assert_snapshot("stats_loading", &screen(&app));
    send(
        &mut app,
        3.1,
        Msg::Stats {
            week: false,
            data: Box::new(Ok(stats_data(false))),
        },
    );
    assert_snapshot("stats_all", &screen(&app));
    let empty = StatsData {
        week: false,
        ..StatsData::default()
    };
    send(
        &mut app,
        3.2,
        Msg::Stats {
            week: false,
            data: Box::new(Ok(empty)),
        },
    );
    assert_snapshot("stats_empty", &screen(&app));
    send(
        &mut app,
        3.3,
        Msg::Stats {
            week: false,
            data: Box::new(Err("the ledger is locked".into())),
        },
    );
    key(&mut app, 3.4, Key::Char('w'));
    send(
        &mut app,
        3.5,
        Msg::Stats {
            week: true,
            data: Box::new(Err("the ledger is locked".into())),
        },
    );
    assert_snapshot("stats_error", &screen(&app));
}

#[test]
fn w_switches_the_window_and_a_result_of_the_other_window_counts_for_nothing() {
    let mut app = stats_open(SIZE);
    assert!(app.stats.week);
    let fx = key(&mut app, 3.0, Key::Char('w'));
    assert_eq!(fx, vec![Effect::LoadStats { week: false }]);
    assert!(!app.stats.week);
    // A second w while the load runs waits for it, then loads the window of now.
    let fx = key(&mut app, 3.1, Key::Char('w'));
    assert!(fx.is_empty(), "{fx:?}");
    assert!(app.stats.week);
    let fx = send(
        &mut app,
        3.2,
        Msg::Stats {
            week: false,
            data: Box::new(Ok(stats_data(false))),
        },
    );
    assert_eq!(fx, vec![Effect::LoadStats { week: true }]);
    assert_eq!(app.stats.data.data.as_ref().map(|d| d.week), Some(true));
    let fx = send(
        &mut app,
        3.3,
        Msg::Stats {
            week: true,
            data: Box::new(Ok(stats_data(true))),
        },
    );
    assert!(fx.is_empty(), "{fx:?}");
    assert_eq!(app.stats.data.data.as_ref().unwrap().finished, 2);
    // A load that fails does not load again by itself.
    key(&mut app, 3.4, Key::Char('w'));
    let fx = send(
        &mut app,
        3.5,
        Msg::Stats {
            week: false,
            data: Box::new(Err("the ledger is locked".into())),
        },
    );
    assert!(fx.is_empty(), "{fx:?}");
    assert_eq!(
        app.stats.data.error.as_deref(),
        Some("the ledger is locked")
    );
    key(&mut app, 3.6, Key::Esc);
    assert_eq!(app.screen, Screen::Home);
}
