//! `vision model eval` (section 10) with the fake `claude`: one replay of the Judge for each
//! model, the mark of the cheapest model within 5 points of the best, and the event that
//! `vision model recommend` shows. Also the reason column of `vision model` for a pinned agent.

mod turn_support;

use serde_json::{json, Value};
use turn_support::TestHome;
use vision::memory::{Kind, Memory, NewDecision, Writer};

const ASKING: &str = "20261110T100000Z-DEMO-12-plan-1";

fn card(n: usize) -> Value {
    json!({"text": format!("Question {n}: CSV or XLSX?"), "context": "The export button.",
           "example": "The March report.", "evidence": "src/export.rs:12", "kind": "product",
           "area": ["exports"], "recommended": 0, "why": "Finance imports CSV.",
           "options": [{"label": "CSV", "means": "plain"}, {"label": "XLSX", "means": "sheet"}]})
}

/// Four cards that the person answered with CSV, each with its decision.
fn answered_cards(home: &TestHome) {
    let memory = Memory::open(&home.ctx().paths, "user").unwrap();
    let ledger = home.ledger();
    ledger
        .conn()
        .execute(
            "INSERT INTO turns (id, ticket, agent, step, status) VALUES (?1, 'DEMO-12', 'builder', 'plan', 'done')",
            [ASKING],
        )
        .unwrap();
    for n in 1..=4 {
        let mut new = NewDecision::answer(
            Writer::Person("user".into()),
            &format!("Question {n}: CSV or XLSX?"),
            "CSV",
        );
        new.kind = Some(Kind::Product);
        new.ticket = Some("DEMO-12".into());
        let decision = memory.add_decision(&new).unwrap().id.to_string();
        let answer = json!({"choice": 0, "text": "CSV", "by": "user", "why": "", "when": ""});
        ledger
            .conn()
            .execute(
                "INSERT INTO questions (id, ticket, turn, card, state, outcome, answer, decision, created, answered)
                 VALUES (?1, 'DEMO-12', ?2, ?3, 'answered', 'ask', ?4, ?5, '2026-01-10T10:00:00Z', ?6)",
                rusqlite::params![
                    format!("Q-000{n}"),
                    ASKING,
                    card(n).to_string(),
                    answer.to_string(),
                    decision,
                    format!("2026-01-10T10:0{n}:00Z")
                ],
            )
            .unwrap();
    }
}

fn answer(card: &str, choice: i64) -> Value {
    json!({"card": card, "outcome": "answer", "basis": "inference", "sources": [], "sure": true,
           "choice": choice, "reason": "From the code."})
}

/// The Judge: opus answers each card right for $0.30; sonnet answers three of four right and
/// sends one to the person, for $0.05.
fn judge_scenario(home: &TestHome) {
    let opus = json!({"answers": [answer("Q-0001", 0), answer("Q-0002", 0), answer("Q-0003", 0), answer("Q-0004", 0)]});
    let sonnet = json!({"answers": [answer("Q-0001", 0), answer("Q-0002", 0), answer("Q-0003", 0),
        {"card": "Q-0004", "outcome": "ask", "basis": "none", "sources": [], "sure": false, "reason": "Unsure."}]});
    let dir = home.vision.join("claude/fake-scenarios");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("judge.json"),
        json!({"turns": [[{"do": "init"}, {"do": "result", "cost": 0.30, "structured_output": opus,
            "when": [{"model": "claude-sonnet-5-5", "set": {"cost": 0.05, "structured_output": sonnet}}]},
            {"do": "exit", "code": 0}]]})
        .to_string(),
    )
    .unwrap();
}

#[test]
fn model_eval_replays_the_judge_with_each_model_and_changes_no_setting() {
    let home = TestHome::new();
    answered_cards(&home);
    judge_scenario(&home);
    let config_before = std::fs::read_to_string(home.vision.join("config.toml")).unwrap();

    let run = home.run(&[
        "model",
        "eval",
        "judge",
        "--models",
        "opus,sonnet",
        "--n",
        "4",
    ]);
    run.expect_code(0);
    let out = &run.stdout;
    assert!(
        out.contains("Replayed the last 4 answers of the person with the Judge (effort high)"),
        "{run:?}"
    );
    let opus = out
        .lines()
        .find(|l| l.starts_with("claude-opus-5-5"))
        .expect("an opus row");
    let sonnet = out
        .lines()
        .find(|l| l.starts_with("claude-sonnet-5-5"))
        .expect("a sonnet row");
    assert!(
        opus.contains("4/4 (100%)") && opus.contains("$0.30"),
        "{out}"
    );
    assert!(
        sonnet.contains("3/4 (75%)") && sonnet.contains("$0.05"),
        "{out}"
    );
    assert!(
        opus.ends_with("<- the cheapest within 5 points of the best") && !sonnet.contains("<-"),
        "75% is more than 5 points under 100%, so only opus fits: {out}"
    );
    assert!(out.contains("This command changes no setting."), "{out}");
    assert_eq!(
        std::fs::read_to_string(home.vision.join("config.toml")).unwrap(),
        config_before
    );

    // Each replay ran as a turn of the Judge under the key eval, with its model.
    let ledger = home.ledger();
    let models: Vec<String> = ledger
        .turns_of_ticket("eval")
        .unwrap()
        .into_iter()
        .map(|t| {
            format!(
                "{} {}",
                t.agent.unwrap_or_default(),
                t.model.unwrap_or_default()
            )
        })
        .collect();
    assert_eq!(models, ["judge claude-opus-5-5", "judge claude-sonnet-5-5"]);

    // The evaluation is an event, and `vision model recommend` shows its line.
    let event = ledger.latest_event("model-eval").unwrap().unwrap();
    let detail: Value = serde_json::from_str(event.detail.as_deref().unwrap()).unwrap();
    assert_eq!(detail["agent"], "judge");
    assert_eq!(detail["rows"].as_array().unwrap().len(), 2);
    assert_eq!(detail["recommended"], "claude-opus-5-5");
    let recommend = home.run(&["model", "recommend", "judge"]);
    recommend.expect_code(0);
    assert!(
        recommend.stdout.contains("last eval (")
            && recommend.stdout.contains("claude-sonnet-5-5 75% $0.05")
            && recommend
                .stdout
                .contains("the cheapest within 5 points: claude-opus-5-5"),
        "{recommend:?}"
    );
}

#[test]
fn model_eval_marks_the_cheapest_model_within_five_points() {
    let home = TestHome::new();
    answered_cards(&home);
    judge_scenario(&home);
    // Both models answer each card right, so the cheaper one wins.
    let dir = home.vision.join("claude/fake-scenarios");
    let text = std::fs::read_to_string(dir.join("judge.json")).unwrap();
    let mut scenario: Value = serde_json::from_str(&text).unwrap();
    let opus = scenario["turns"][0][1]["structured_output"].clone();
    scenario["turns"][0][1]["when"][0]["set"]["structured_output"] = opus;
    std::fs::write(dir.join("judge.json"), scenario.to_string()).unwrap();

    let run = home.run(&[
        "model",
        "eval",
        "judge",
        "--models",
        "opus,sonnet",
        "--json",
    ]);
    run.expect_code(0);
    let evaluation: Value = serde_json::from_str(&run.stdout).unwrap();
    assert_eq!(evaluation["recommended"], "claude-sonnet-5-5");
    assert_eq!(evaluation["n"], 20, "the default is curator.replay");
    assert_eq!(evaluation["rows"][1]["agree"], 4);
    let table = home.run(&["model", "eval", "judge", "--models", "opus,sonnet"]);
    let sonnet = table
        .stdout
        .lines()
        .find(|l| l.starts_with("claude-sonnet-5-5"))
        .unwrap()
        .to_string();
    assert!(
        sonnet.ends_with("<- the cheapest within 5 points of the best"),
        "{table:?}"
    );
}

#[test]
fn model_eval_of_another_agent_is_not_built_and_needs_answers_for_the_judge() {
    let home = TestHome::new();
    let run = home.run(&["model", "eval", "builder", "--models", "opus,sonnet"]);
    run.expect_code(3);
    assert!(
        run.stderr.starts_with("not built yet: model eval builder"),
        "{run:?}"
    );
    assert!(run.stderr.contains("replays only the Judge"), "{run:?}");
    assert!(
        run.stderr.contains("vision model recommend builder"),
        "{run:?}"
    );
    let run = home.run(&["model", "eval", "judge", "--models", "opus"]);
    run.expect_code(1);
    assert!(run.stderr.contains("nothing to replay"), "{run:?}");
    let run = home.run(&["model", "eval", "nobody", "--models", "opus"]);
    run.expect_code(1);
    assert!(run.stderr.contains("not an agent of Vision"), "{run:?}");
}

#[test]
fn a_pinned_agent_shows_the_default_that_it_left() {
    let home = TestHome::new();
    home.run(&["model", "set", "builder", "sonnet"])
        .expect_code(0);
    let run = home.run(&["model"]);
    run.expect_code(0);
    // The reason of the default has its own line under the row of the agent (with a second
    // line when it is long), so that the table fits 100 columns.
    let reason_of = |agent: &str| {
        let lines: Vec<&str> = run.stdout.lines().collect();
        let at = lines.iter().position(|l| l.starts_with(agent)).unwrap();
        let mut text = lines[at + 1].trim().to_string();
        for more in lines[at + 2..].iter().take_while(|l| l.starts_with("    ")) {
            text.push(' ');
            text.push_str(more.trim());
        }
        text
    };
    assert_eq!(
        reason_of("builder"),
        "reason of the default claude-opus-5-5 xhigh: It writes the code and uses the most tokens. This model costs half as much as the top model.",
        "{run:?}"
    );
    assert_eq!(
        reason_of("scout"),
        "reason of the default: A wrong reading of the ticket costs the full build. Its turns are short.",
        "{run:?}"
    );
}
