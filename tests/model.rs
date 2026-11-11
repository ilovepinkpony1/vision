//! `vision model`, `model set`, `model reset`, and `model recommend` (section 10).

mod turn_support;

use serde_json::{json, Value};
use turn_support::{result_object, TestHome};
use vision::turn::TurnKind;

fn rows(home: &TestHome) -> Vec<Value> {
    let run = home.run(&["model", "--json"]);
    run.expect_code(0);
    serde_json::from_str(&run.stdout).unwrap()
}

fn row<'a>(rows: &'a [Value], agent: &str) -> &'a Value {
    rows.iter().find(|r| r["agent"] == agent).unwrap()
}

fn comment_lines(text: &str) -> Vec<String> {
    text.lines()
        .filter(|l| l.contains('#'))
        .map(str::to_string)
        .collect()
}

#[test]
fn model_shows_each_agent_with_its_default_and_reason() {
    let home = TestHome::new();
    let run = home.run(&["model"]);
    run.expect_code(0);
    let lines: Vec<&str> = run.stdout.lines().collect();
    assert!(lines[0].starts_with("AGENT"), "{}", run.stdout);
    for header in ["MODEL", "EFFORT", "SOURCE"] {
        assert!(lines[0].contains(header), "{}", run.stdout);
    }
    // The table fits 100 columns: the reason and the recommendation of each agent have their
    // own lines under its row.
    for line in &lines {
        assert!(line.chars().count() <= 100, "{}", run.stdout);
    }
    let agents: Vec<&str> = lines
        .iter()
        .filter(|l| !l.starts_with(' '))
        .map(|l| l.split_whitespace().next().unwrap())
        .collect();
    assert_eq!(
        agents,
        ["AGENT", "scout", "builder", "reviewer", "tester", "shepherd", "judge", "curator"],
        "{}",
        run.stdout
    );
    let at = lines.iter().position(|l| l.starts_with("builder")).unwrap();
    let builder: Vec<&str> = lines[at].split_whitespace().take(4).collect();
    assert_eq!(
        builder,
        ["builder", "claude-opus-5-5", "xhigh", "default"],
        "{}",
        run.stdout
    );
    assert!(lines[at + 1]
        .starts_with("  reason of the default: It writes the code and uses the most tokens."));
    assert_eq!(
        lines
            .iter()
            .filter(|l| l.starts_with("  recommendation: "))
            .count(),
        7,
        "{}",
        run.stdout
    );

    let rows = rows(&home);
    assert_eq!(rows.len(), 7);
    let judge = row(&rows, "judge");
    assert_eq!(judge["model"], "claude-fable-5-2");
    assert_eq!(judge["effort"], "high");
    assert_eq!(judge["source"], "default");
    assert!(judge["recommendation"]
        .as_str()
        .unwrap()
        .ends_with("so the default stays"));
    assert_eq!(judge["recommended_model"], Value::Null);
    assert_eq!(row(&rows, "reviewer")["effort"], "max");
}

#[test]
fn set_and_reset_pin_a_model_and_record_each_change() {
    let home = TestHome::new();
    let before = std::fs::read_to_string(home.vision.join("config.toml")).unwrap();
    let run = home.run(&["model", "set", "judge", "opus", "--effort", "xhigh"]);
    run.expect_code(0);
    assert!(
        run.stdout
            .contains("judge now uses claude-opus-5-5 with the effort xhigh"),
        "{run:?}"
    );
    let text = std::fs::read_to_string(home.vision.join("config.toml")).unwrap();
    assert_eq!(
        comment_lines(&text),
        comment_lines(&before),
        "each comment stays"
    );
    let config = vision::config::Config::parse(&text).unwrap();
    let pin = &config.agents["judge"];
    assert_eq!(pin.model.as_deref(), Some("claude-opus-5-5"));
    assert_eq!(pin.effort, Some(vision::config::Effort::Xhigh));
    let pinned_at = pin.pinned_at.clone().unwrap();
    let judge = row(&rows(&home), "judge").clone();
    assert_eq!(judge["source"], format!("yours since {}", &pinned_at[..10]));
    assert_eq!(judge["pinned_at"], pinned_at.as_str());

    home.run(&["model", "set", "judge", "sonnet"])
        .expect_code(0);
    let judge = row(&rows(&home), "judge").clone();
    assert_eq!(judge["model"], "claude-sonnet-5-5");
    assert_eq!(
        judge["effort"], "xhigh",
        "a pinned effort stays without --effort"
    );

    let reset = home.run(&["model", "reset", "judge"]);
    reset.expect_code(0);
    assert!(
        reset
            .stdout
            .contains("uses the default of Vision again: claude-fable-5-2 high"),
        "{reset:?}"
    );
    let after = std::fs::read_to_string(home.vision.join("config.toml")).unwrap();
    assert!(
        vision::config::Config::parse(&after)
            .unwrap()
            .agents
            .is_empty(),
        "{after}"
    );
    assert_eq!(comment_lines(&after), comment_lines(&before));
    let again = home.run(&["model", "reset", "judge"]);
    again.expect_code(0);
    assert!(
        again.stdout.contains("already uses the default"),
        "{again:?}"
    );

    let ledger = home.ledger();
    let mut stmt = ledger
        .conn()
        .prepare("SELECT agent, model, effort, old_model, old_effort, source FROM model_changes ORDER BY id")
        .unwrap();
    let changes: Vec<[String; 6]> = stmt
        .query_map([], |r| {
            Ok([
                r.get(0)?,
                r.get(1)?,
                r.get(2)?,
                r.get(3)?,
                r.get(4)?,
                r.get(5)?,
            ])
        })
        .unwrap()
        .map(|r| r.unwrap())
        .collect();
    let expected = [
        [
            "judge",
            "claude-opus-5-5",
            "xhigh",
            "claude-fable-5-2",
            "high",
            "set",
        ],
        [
            "judge",
            "claude-sonnet-5-5",
            "xhigh",
            "claude-opus-5-5",
            "xhigh",
            "set",
        ],
        [
            "judge",
            "claude-fable-5-2",
            "high",
            "claude-sonnet-5-5",
            "xhigh",
            "reset",
        ],
    ]
    .map(|r| r.map(String::from));
    assert_eq!(changes, expected);
}

#[test]
fn bad_agents_and_models_are_refused() {
    let home = TestHome::new();
    let run = home.run(&["model", "set", "nobody", "opus"]);
    run.expect_code(1);
    assert!(run.stderr.contains("not an agent of Vision"), "{run:?}");
    assert!(run.stderr.contains("scout, builder, reviewer"), "{run:?}");
    let run = home.run(&["model", "set", "judge", "two words"]);
    run.expect_code(1);
    assert!(run.stderr.contains("not a model id"), "{run:?}");
    home.run(&["model", "reset", "nobody"]).expect_code(1);
    let eval = home.run(&["model", "eval", "scout", "--models", "opus,sonnet"]);
    eval.expect_code(3);
    assert!(
        eval.stderr.starts_with("not built yet: model eval scout"),
        "{eval:?}"
    );
}

#[test]
fn recommend_applies_the_rule_of_section_10() {
    let home = TestHome::new();
    let ledger = home.ledger();
    let insert = |prefix: &str, agent: &str, model: &str, count: usize, held: usize, cost: f64| {
        for i in 0..count {
            let end_fact = if i < held { "held" } else { "failed:commits" };
            ledger
                .conn()
                .execute(
                    "INSERT INTO turns (id, ticket, agent, model, started, ended, status, cost_usd, end_fact)
                     VALUES (?1, ?2, ?3, ?4, '2026-01-01T00:00:00Z', '2026-01-01T00:10:00Z', 'done', ?5, ?6)",
                    rusqlite::params![format!("{prefix}-{i}"), format!("DEMO-{}", i % 3), agent, model, cost, end_fact],
                )
                .unwrap();
        }
    };
    insert("a", "scout", "claude-fable-5-2", 12, 11, 1.0);
    insert("b", "scout", "claude-opus-5-5", 9, 9, 0.5);
    let run = home.run(&["model", "recommend", "scout"]);
    run.expect_code(0);
    assert!(
        run.stdout
            .contains("fewer than 10 turns: claude-opus-5-5 (9)"),
        "{run:?}"
    );
    assert!(run.stdout.contains("so the default stays"), "{run:?}");

    insert("c", "scout", "claude-opus-5-5", 1, 0, 0.5);
    let run = home.run(&["model", "recommend", "scout", "--json"]);
    run.expect_code(0);
    let advice: Vec<Value> = serde_json::from_str(&run.stdout).unwrap();
    assert_eq!(advice.len(), 1);
    assert_eq!(advice[0]["recommended"], "claude-opus-5-5", "{advice:?}");
    let opus = advice[0]["models"]
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["model"] == "claude-opus-5-5")
        .unwrap();
    assert_eq!(opus["turns"], 10);
    assert_eq!(opus["first_try"], 90.0);
    assert_eq!(opus["cost_per_turn"], 0.5);

    let text = home.run(&["model", "recommend", "scout"]);
    assert!(text.stdout.contains("FIRST TRY"), "{text:?}");
    assert!(
        text.stdout
            .contains("vision model set scout claude-opus-5-5"),
        "{text:?}"
    );
    let scout = row(&rows(&home), "scout").clone();
    assert_eq!(scout["recommended_model"], "claude-opus-5-5");

    // The Judge counts the corrections and the changed proposals of the person.
    insert("j", "judge", "claude-fable-5-2", 10, 10, 0.2);
    ledger
        .conn()
        .execute_batch(
            "INSERT INTO questions (id, judge_turn, outcome, took_proposal) VALUES ('Q-1', 'j-0', 'propose', 0);
             INSERT INTO questions (id, judge_turn, outcome, took_proposal) VALUES ('Q-2', 'j-1', 'answer', NULL);
             INSERT INTO human (at, kind, object) VALUES ('2026-01-02T00:00:00Z', 'correct', 'Q-2');",
        )
        .unwrap();
    let run = home.run(&["model", "recommend", "judge", "--json"]);
    let advice: Vec<Value> = serde_json::from_str(&run.stdout).unwrap();
    let extra = &advice[0]["models"][0]["extra"];
    assert_eq!(
        extra,
        &json!([["corrections", 1.0], ["changed proposals", 1.0]])
    );
}

#[test]
fn a_turn_uses_and_records_the_model_of_its_agent() {
    let home = TestHome::new();
    home.run(&["model", "set", "scout", "haiku", "--effort", "low"])
        .expect_code(0);
    let scenario = home.scenario(
        "model",
        json!([[{"do": "init"}, {"do": "args"}, {"do": "result", "structured_output": result_object("ok")}]]),
    );
    let launched =
        home.launch(&home.request("DEMO-12", "scout", "refine", TurnKind::Reader, &scenario));
    assert_eq!(launched.model, "claude-haiku-5-5");
    home.wait_ended(&launched);
    let row = home
        .ledger()
        .turn(&launched.id.to_string())
        .unwrap()
        .unwrap();
    assert_eq!(row.model.as_deref(), Some("claude-haiku-5-5"));
    assert_eq!(row.effort.as_deref(), Some("low"));
    let args = &home.events(&launched, "system", Some("fake_args"))[0];
    let argv: Vec<String> = serde_json::from_value(args["argv"].clone()).unwrap();
    let at = argv.iter().position(|a| a == "--model").unwrap();
    assert_eq!(
        &argv[at + 1..at + 4],
        ["claude-haiku-5-5", "--effort", "low"]
    );
}
