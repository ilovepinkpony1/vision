//! The reads of the screens Models and Stats against a temporary home whose ledger a run of
//! the demo world fills: the numbers of Stats for the last 7 days and for all time, the same as
//! the report of today, with the cost of each agent; and the model table of Models with a pin
//! and a reset of the commands of today.

use std::time::Duration;

use time::OffsetDateTime;
use vision::app::load_screens::{models, stats};

use super::pipeline_support::{simple_scenarios, World, KEY};

fn close(a: f64, b: f64) -> bool {
    (a - b).abs() < 1e-9
}

#[test]
fn the_numbers_of_stats_come_from_the_ledger_for_both_windows() {
    let world = World::new();
    simple_scenarios(&world);
    world.run(&["take", KEY]).expect_code(0);
    world.tick_to("plan-ready");
    let paths = world.ctx().paths;
    let now = OffsetDateTime::now_utc();

    let week = stats(&paths, true, now).unwrap();
    let all = stats(&paths, false, now).unwrap();
    let report = world.run(&["report", "--json"]).json();
    let report_week = world.run(&["report", "--week", "--json"]).json();
    for (data, report) in [(&week, &report_week), (&all, &report)] {
        let total = report["total"]["cost_usd"].as_f64().unwrap();
        assert!(close(data.cost_usd, total), "{} {}", data.cost_usd, total);
        assert!(close(data.cost_usd, 0.5), "two turns of 0.25: {data:?}");
        assert_eq!(data.finished, 0);
        assert_eq!(data.questions, 0);
        assert_eq!(data.tickets.len(), 1);
        assert_eq!(data.tickets[0].key, KEY);
        assert!(close(
            data.tickets[0].cost_usd,
            report["tickets"][KEY]["cost_usd"].as_f64().unwrap()
        ));
        // The steps in the order of the adapter, with the cost of each one.
        let steps: Vec<&str> = data.steps.iter().map(|s| s.step.as_str()).collect();
        assert_eq!(&steps[..2], ["refine", "plan"], "{steps:?}");
        assert!(close(data.steps[0].cost_usd, 0.25));
        assert!(close(data.steps[1].cost_usd, 0.25));
        // The cost of each agent, in the order of the agents of Vision.
        assert_eq!(
            data.agents,
            vec![("scout".to_string(), 0.25), ("builder".to_string(), 0.25)]
        );
    }
    assert!(week.week && !all.week);
    assert!(all.outside.is_empty(), "{:?}", all.outside);

    // A turn of the Curator has no ticket: its cost shows apart, and the cost of the window
    // and the cost of each agent do not count it.
    let ledger = world.ctx().ledger().unwrap();
    let started = vision::util::time::rfc3339(now);
    ledger
        .insert_turn(&vision::ledger::turns::NewTurn {
            id: "20261111T120000Z-curator-curate-1",
            ticket: "curator",
            agent: "curator",
            step: "curate",
            kind: "curate",
            session: "s-curator",
            resumes: None,
            model: "claude-fable-5-2",
            effort: "high",
            started: &started,
            role_hash: "",
        })
        .unwrap();
    ledger
        .end_turn(
            "20261111T120000Z-curator-curate-1",
            &vision::ledger::turns::TurnEnd {
                status: "done".into(),
                reason: "result".into(),
                ended: started.clone(),
                cost_usd: Some(0.4),
                ..Default::default()
            },
        )
        .unwrap();
    let with_curator = stats(&paths, false, now + Duration::from_secs(60)).unwrap();
    assert_eq!(with_curator.outside, vec![("curator".to_string(), 0.4)]);
    assert_eq!(with_curator.agents, all.agents);
    assert!(close(with_curator.cost_usd, all.cost_usd));

    // Seen from 30 days later, the last 7 days hold nothing, and all time holds the same.
    let later = now + Duration::from_secs(30 * 24 * 3600);
    let empty = stats(&paths, true, later).unwrap();
    assert!(close(empty.cost_usd, 0.0));
    assert!(empty.tickets.is_empty());
    assert!(empty.agents.is_empty());
    assert!(empty.steps.is_empty(), "{:?}", empty.steps);
    let same = stats(&paths, false, later).unwrap();
    assert_eq!(same.agents, all.agents);
    assert_eq!(same.tickets, all.tickets);
    world.settle();
}

#[test]
fn the_model_table_shows_a_pin_and_a_reset_of_the_commands() {
    let world = World::new();
    let paths = world.ctx().paths;
    let lines = models(&paths).unwrap();
    let agents: Vec<&str> = lines.iter().map(|l| l.agent.as_str()).collect();
    assert_eq!(
        agents,
        ["scout", "builder", "reviewer", "tester", "shepherd", "judge", "curator"]
    );
    let scout = &lines[0];
    assert!(!scout.pinned);
    assert_eq!(scout.model, scout.default_model);
    assert!(!scout.reason.is_empty());
    assert!(!scout.recommendation.is_empty());

    world
        .run(&[
            "model",
            "set",
            "scout",
            "claude-opus-5-5",
            "--effort",
            "max",
        ])
        .expect_code(0);
    let scout = models(&paths).unwrap().remove(0);
    assert!(scout.pinned);
    assert!(scout.since.is_some());
    assert_eq!(
        (scout.model.as_str(), scout.effort.as_str()),
        ("claude-opus-5-5", "max")
    );
    assert_eq!(scout.default_model, "claude-fable-5-2");

    world.run(&["model", "reset", "scout"]).expect_code(0);
    let scout = models(&paths).unwrap().remove(0);
    assert!(!scout.pinned);
    assert_eq!(scout.model, "claude-fable-5-2");
}

#[test]
fn a_home_that_is_not_set_up_gives_an_error_and_no_numbers() {
    let dir = tempfile::tempdir().unwrap();
    let paths = vision::paths::Paths::new(dir.path().join("vision"));
    assert!(stats(&paths, true, OffsetDateTime::now_utc()).is_err());
    assert!(models(&paths).is_err());
}
