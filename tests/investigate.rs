//! The investigation before the plan (sections 7.3, 8, 11.2, and 11.3), driven through `vision
//! tick` on the demo adapter with the investigation path (`adapters/demo-investigate`): the kind
//! of a ticket, the steps that `when` skips, a team of members in parallel, the Codex runner
//! through the fake `codex` with a fallback to Claude, the ask step with one batch of cards, the
//! amend, the reproduction gate, and `vision kind`. A chore goes on through the build and the
//! review to the act step open-pr, which pushes and opens the pull request only then. Nothing
//! calls the real `claude` or `codex`.

mod pipeline_support;

use std::time::Duration;

use pipeline_support::{demo_dir, ends_with, plan_hash_of, result, runs_then, World, KEY};
use serde_json::{json, Value};

/// Writes the private copy of the investigation path in the home, as the adapter `demo`.
fn write_investigate(world: &World) {
    let text = std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("adapters/demo-investigate/adapter.toml"),
    )
    .unwrap()
    .replace(
        "scripts = \"../demo\"",
        &format!(
            "scripts = {}",
            serde_json::to_string(&demo_dir().display().to_string()).unwrap()
        ),
    )
    .replace("every = 60", "every = 1");
    let dir = world.vision.join("adapters/demo");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("adapter.toml"), text).unwrap();
}

/// A world with the investigation path, and DEMO-12 of this issue type.
fn world_of(issue_type: &str) -> World {
    let world = World::new();
    write_investigate(&world);
    world.world_cmd(&["type", world.root(), KEY, issue_type]);
    world.scenario(
        "scout",
        "refine",
        json!([ends_with(result(
            "Refined.",
            &[("refine.md", "Research."), ("description.md", "Fix it.")]
        ))]),
    );
    world
}

/// Writes the scenario of the fake codex for one agent at one step.
fn codex_scenario(world: &World, agent: &str, step: &str, turns: Value) {
    let dir = world.vision.join("claude/fake-scenarios");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join(format!("codex-{agent}-{step}.json")),
        json!({ "turns": turns }).to_string(),
    )
    .unwrap();
}

/// A valid card with one recommended choice.
fn card(text: &str, recommended: usize) -> Value {
    json!({
        "text": text,
        "context": "The plan needs it.",
        "example": "A user types 5 - 3.",
        "options": [
            {"label": "Yes", "means": "do it"},
            {"label": "No", "means": "do not do it"}
        ],
        "recommended": recommended,
        "why": "The ticket says so.",
        "evidence": "findings-points.md",
        "kind": "product",
        "area": ["calculator"],
        "topics": []
    })
}

/// A plan with these files and this proof line.
fn plan(files: &[&str], proof: &str) -> String {
    let list: Vec<String> = files.iter().map(|f| format!("- {f}")).collect();
    format!(
        "# Plan\n\n## Architecture decisions\n\nnone\n\n## Cause\n\nThe sum is wrong.\n\n## Proof\n\n{proof}\n\n## Plan\n\n1. Test first.\n\n## Helpers\n\nnone\n\n## Doubts\n\nnone\n\n## Files\n\n{}\n",
        list.join("\n")
    )
}

const ACCEPTANCE: &str = r#"{"checks": [{"id": "A1", "point": "subtract", "given": "5 and 3", "then": "2", "how": "test_subtract.py"}]}"#;

/// The analyst at the step `analyze`, with this plan and these cards.
fn analyst(world: &World, step: &str, plan_text: &str, cards: Value) {
    let mut docs = vec![
        ("plan.md".to_string(), plan_text.to_string()),
        ("acceptance.json".to_string(), ACCEPTANCE.to_string()),
    ];
    if step == "analyze" {
        docs.push(("questions-analyst.json".into(), cards.to_string()));
    }
    let refs: Vec<(&str, &str)> = docs.iter().map(|(n, c)| (n.as_str(), c.as_str())).collect();
    world.scenario(
        "analyst",
        step,
        json!([ends_with(result("Analyzed.", &refs))]),
    );
}

fn status(world: &World) -> Value {
    let run = world.run(&["status", "--json"]);
    run.expect_code(0);
    run.json()
}

/// The turns of one step: (agent, status, model), oldest first.
fn step_turns(world: &World, step: &str) -> Vec<(String, String, String)> {
    world
        .ledger()
        .turns_of_ticket(KEY)
        .unwrap()
        .into_iter()
        .filter(|t| t.step.as_deref() == Some(step))
        .map(|t| {
            (
                t.agent.unwrap_or_default(),
                t.status.unwrap_or_default(),
                t.model.unwrap_or_default(),
            )
        })
        .collect()
}

/// The detail of each event of this kind, oldest first.
fn events(world: &World, kind: &str) -> Vec<Value> {
    world
        .ledger()
        .events_of_ticket(KEY)
        .unwrap()
        .into_iter()
        .filter(|e| e.kind == kind)
        .map(|e| {
            e.detail
                .as_deref()
                .and_then(|d| serde_json::from_str(d).ok())
                .unwrap_or(Value::Null)
        })
        .collect()
}

fn tick_to(world: &World, step: &str) {
    world.tick_until(&format!("the step {step}"), Duration::from_secs(90), |w| {
        w.step().as_deref() == Some(step)
    });
}

#[test]
fn a_bug_goes_through_a_team_codex_one_batch_of_questions_and_the_gate() {
    let world = world_of("Bug");
    // The team: repro reproduces with a failing spec in the clone, cause fails on Codex and
    // falls back to Claude, and history asks for the history of git first.
    world.scenario(
        "repro",
        "investigate",
        json!([[
            {"do": "init"},
            {"do": "sleep", "seconds": 2},
            {"do": "run", "command": "printf 'import unittest\\n\\n\\nclass BugTest(unittest.TestCase):\\n    def test_bug(self):\\n        self.assertEqual(1, 2)\\n' > test_bug.py && git add test_bug.py && git commit -q -m 'test: reproduce DEMO-12'"},
            {"do": "result", "structured_output": result("Reproduced.", &[("findings-repro.md", "Reproduced: yes\n\npython3 -m unittest test_bug fails.\n")]), "cost": 0.25}
        ]]),
    );
    codex_scenario(
        &world,
        "cause",
        "investigate",
        json!([[{"do": "fail", "message": "You've hit your usage limit. Try again later."}]]),
    );
    world.scenario(
        "cause",
        "investigate",
        json!([ends_with(result(
            "Found the cause.",
            &[("findings-cause.md", "The cause: calc.py:4 adds.")]
        ))]),
    );
    world.scenario(
        "history",
        "investigate",
        json!([
            [{"do": "init"}, {"do": "sleep", "seconds": 2}, {"do": "result", "structured_output":
                {"kind": "requests", "summary": "I need the history.",
                 "requests": [{"verb": "git.log", "args": {"path": "calc.py"}}]}}],
            ends_with(result("Read the history.", &[("findings-history.md", "- calc.py:4 why: the start breaks: nothing\n\n## Open questions\n\nnone\n")]))
        ]),
    );
    let first = card("Must subtract accept floats?", 0);
    let second = card("Must subtract round its result?", 0);
    analyst(
        &world,
        "analyze",
        &plan(&["calc.py", "test_calc.py"], "Reproduced: yes"),
        json!([first.clone(), second.clone()]),
    );
    // The critic of a narrow plan runs on Codex. One of its cards repeats a card of the
    // analyst in other spacing and case, so the batch drops it.
    let repeat = card("must subtract   accept FLOATS?", 1);
    let third = card("Must subtract warn on a negative result?", 0);
    codex_scenario(
        &world,
        "critic-narrow",
        "critic-narrow",
        json!([[
            {"do": "message", "text": "I read the plan."},
            {"do": "command", "command": "rg subtract", "output": "calc.py:4\n"},
            {"do": "result", "result": {"kind": "result", "summary": "Criticized.", "documents": [
                {"name": "critic.md", "content": "none"},
                {"name": "questions-critic.json", "content": json!([repeat, third]).to_string()}
            ]}}
        ]]),
    );
    analyst(
        &world,
        "amend",
        &format!(
            "{}\nThe person answered.\n",
            plan(&["calc.py", "test_calc.py"], "Reproduced: yes")
        ),
        Value::Null,
    );

    world.run(&["take", KEY]).expect_code(0);
    world.tick_until("the team starts", Duration::from_secs(60), |w| {
        w.turns_of("investigate") > 0
    });
    // Each member whose `when` holds starts in the same tick, each in its own session.
    let started = step_turns(&world, "investigate");
    let agents: Vec<&str> = started.iter().map(|t| t.0.as_str()).collect();
    assert_eq!(agents.len(), 3, "{started:?}");
    for agent in ["repro", "cause", "history"] {
        assert!(agents.contains(&agent), "{started:?}");
    }
    let sessions: std::collections::BTreeSet<String> = world
        .ledger()
        .turns_of_ticket(KEY)
        .unwrap()
        .into_iter()
        .filter(|t| t.step.as_deref() == Some("investigate"))
        .filter_map(|t| t.session)
        .collect();
    assert_eq!(sessions.len(), 3, "each member has its own session");
    let now = status(&world)["tickets"][0]["now"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(now.starts_with("team: "), "{now}");
    assert!(
        now.contains("repro runs") && now.contains("history runs"),
        "{now}"
    );
    assert_eq!(status(&world)["tickets"][0]["ticket_kind"], "bug");
    assert_eq!(events(&world, "kind")[0]["kind"], "bug");
    assert_eq!(events(&world, "kind")[0]["issue_type"], "Bug");

    tick_to(&world, "questions");
    // The Codex turn of cause failed at its usage limit, so cause ran again with Claude.
    let fallback = events(&world, "fallback");
    assert_eq!(fallback.len(), 1, "{fallback:?}");
    assert_eq!(fallback[0]["agent"], "cause");
    assert!(
        fallback[0]["why"]
            .as_str()
            .unwrap()
            .contains("a usage limit"),
        "{fallback:?}"
    );
    assert!(world
        .run(&["log", KEY])
        .stdout
        .contains("fallback cause: the turn"));
    let cause: Vec<(String, String, String)> = step_turns(&world, "investigate")
        .into_iter()
        .filter(|t| t.0 == "cause")
        .collect();
    assert_eq!(cause.len(), 2, "{cause:?}");
    assert_eq!(
        (cause[0].1.as_str(), cause[0].2.as_str()),
        ("failed", "gpt-6.1-sol")
    );
    assert_eq!(
        (cause[1].1.as_str(), cause[1].2.as_str()),
        ("done", "claude-opus-5-5")
    );
    // The repro committed its spec, and history got the log of git through a request.
    assert!(
        pipeline_support::git(world.clone(), &["log", "--format=%s"])
            .contains("test: reproduce DEMO-12")
    );
    let logs: Vec<String> = std::fs::read_dir(world.vision.join("tickets").join(KEY))
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.starts_with("git-log-"))
        .collect();
    assert_eq!(logs.len(), 1, "{logs:?}");
    assert!(world
        .doc(&logs[0])
        .unwrap()
        .contains("Start the toy calculator"));
    for doc in [
        "findings-repro.md",
        "findings-cause.md",
        "findings-history.md",
        "plan.md",
        "acceptance.json",
    ] {
        assert!(world.doc(doc).is_some(), "{doc}");
    }
    // The plan is narrow, so the critic of a wide plan was skipped, and the critic of a narrow
    // plan ran on Codex with its start line.
    let skipped = events(&world, "skipped");
    assert!(
        skipped
            .iter()
            .any(|s| s["step"] == "critic"
                && s["why"].as_str().unwrap().contains("the plan is narrow")),
        "{skipped:?}"
    );
    let spans = world.ledger().spans_of(Some(KEY)).unwrap();
    assert!(spans
        .iter()
        .any(|s| s.state == "critic" && s.waits_on.as_deref() == Some("skipped")));
    let critic = world
        .ledger()
        .turns_of_ticket(KEY)
        .unwrap()
        .into_iter()
        .find(|t| t.step.as_deref() == Some("critic-narrow"))
        .unwrap();
    assert_eq!(critic.model.as_deref(), Some("gpt-6.1-sol"));
    assert_eq!(critic.effort.as_deref(), Some("high"));
    assert_eq!(
        critic.cost_usd,
        Some(0.0),
        "Codex has no price in the configuration"
    );
    assert_eq!(
        (critic.input, critic.cache_read, critic.output),
        (Some(600), Some(400), Some(100))
    );
    let dir = world
        .vision
        .join("tickets")
        .join(KEY)
        .join("turns")
        .join(&critic.id);
    let args: Value =
        serde_json::from_str(&std::fs::read_to_string(dir.join("codex-args.json")).unwrap())
            .unwrap();
    let line: Vec<&str> = args["args"]
        .as_array()
        .unwrap()
        .iter()
        .map(|a| a.as_str().unwrap())
        .collect();
    assert_eq!(&line[..2], ["exec", "--json"]);
    for part in [
        "default_permissions=\"vision_reader\"",
        "permissions.vision_reader.extends=\":read-only\"",
        "-m",
        "gpt-6.1-sol",
        "model_reasoning_effort=\"high\"",
        "--ignore-user-config",
        "-",
    ] {
        assert!(line.contains(&part), "{line:?}");
    }
    // The commands of the turn never read the home of Vision or a private folder of the home
    // folder, and they read the working folder.
    let filesystem = line
        .iter()
        .find(|a| a.starts_with("permissions.vision_reader.filesystem="))
        .unwrap();
    let home = world.vision.display().to_string();
    assert!(
        filesystem.contains(&format!("{home:?} = \"none\"")),
        "{filesystem}"
    );
    assert!(filesystem.contains("/.ssh\" = \"none\""), "{filesystem}");
    assert!(filesystem.contains("/.codex\" = \"none\""), "{filesystem}");
    let workdir = args["cwd"].as_str().unwrap();
    assert!(
        filesystem.contains(&format!("{workdir:?} = \"read\"")),
        "{filesystem} {workdir}"
    );
    assert!(!line.contains(&"--sandbox"), "{line:?}");
    let env: Vec<&str> = args["env"]
        .as_array()
        .unwrap()
        .iter()
        .map(|a| a.as_str().unwrap())
        .collect();
    assert!(!env.contains(&"CLAUDE_CONFIG_DIR"), "{env:?}");
    assert!(!env.iter().any(|n| n.starts_with("VISION_")), "{env:?}");
    let stream = std::fs::read_to_string(dir.join("stream.jsonl")).unwrap();
    assert!(stream.contains("\"rg subtract\""), "{stream}");
    assert!(std::fs::read_to_string(dir.join("codex.jsonl"))
        .unwrap()
        .contains("thread.started"));

    // One batch: three cards, the repeated one dropped. The Judge answers the first, and the
    // person answers the two others.
    world.tick_until("the cards", Duration::from_secs(30), |w| {
        w.ledger()
            .questions_in_states(&["new", "judging", "asked", "proposed"])
            .unwrap()
            .len()
            == 3
    });
    let batch = format!("ask:{KEY}:questions:");
    let rows = world
        .ledger()
        .questions_in_states(&["new", "judging", "asked", "proposed"])
        .unwrap();
    assert!(rows.iter().all(|r| r.turn.starts_with(&batch)), "{rows:?}");
    let ids: Vec<String> = rows.iter().map(|r| r.id.clone()).collect();
    let dir = world.vision.join("claude/fake-scenarios");
    std::fs::write(
        dir.join("judge.json"),
        json!({"turns": [[{"do": "init"}, {"do": "result", "structured_output": {"answers": [
            {"card": ids[0], "outcome": "answer", "choice": 0, "basis": "inference", "sure": true, "reason": "The code shows it."},
            {"card": ids[1], "outcome": "ask", "basis": "none", "sure": false, "reason": "A product choice."},
            {"card": ids[2], "outcome": "ask", "basis": "none", "sure": false, "reason": "A product choice."}
        ]}}]]})
        .to_string(),
    )
    .unwrap();
    world.tick_until("the Judge", Duration::from_secs(30), |w| {
        w.ledger().questions_in_states(&["asked"]).unwrap().len() == 2
    });
    // `vision needs` names the agent that wrote each card of the batch.
    let needs = world.run(&["needs", "--json"]);
    needs.expect_code(0);
    let cards: Vec<(String, String)> = needs.json()["cards"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| {
            (
                c["agent"].as_str().unwrap().to_string(),
                c["question"].as_str().unwrap().to_string(),
            )
        })
        .collect();
    assert_eq!(
        cards,
        [
            (
                "analyst".to_string(),
                "Must subtract round its result?".to_string()
            ),
            (
                "critic-narrow".to_string(),
                "Must subtract warn on a negative result?".to_string()
            )
        ],
        "{}",
        needs.stdout
    );
    world
        .run(&["answer", &ids[1], "--choice", "1"])
        .expect_code(0);
    world
        .run(&["answer", &ids[2], "--choice", "2"])
        .expect_code(0);
    // An answer that is not the recommended choice changed the plan, so the analyst amends it.
    tick_to(&world, "plan-ready");
    let answered = events(&world, "ask-answered");
    assert_eq!(answered[0]["changed"], json!([ids[2]]));
    assert_eq!(world.turns_of("amend"), 1);
    let answers = world.doc("answers.md").unwrap();
    for id in &ids {
        assert!(answers.contains(&format!("## {id}:")), "{answers}");
    }
    assert!(
        answers.contains("- Asked by: critic-narrow, step questions, batch ask:"),
        "{answers}"
    );
    assert!(world
        .doc("plan.md")
        .unwrap()
        .contains("The person answered."));
    let checks = events(&world, "check");
    assert_eq!(checks[0]["holds"], true);
    assert!(world.flags().is_empty(), "{:?}", world.flags());
}

#[test]
fn a_feature_runs_points_a_wide_critic_and_skips_the_gate() {
    let world = world_of("Story");
    // A member that ends with questions gets them refused, and writes them into its findings.
    world.scenario(
        "points",
        "investigate",
        json!([
            [{"do": "init"}, {"do": "result", "structured_output":
                {"kind": "questions", "summary": "A question.", "questions": [card("Floats?", 0)]}}],
            [{"do": "init"}, {"do": "facts"}, {"do": "result", "structured_output":
                result("Points.", &[("findings-points.md", "1. subtract: missing.\n\n## Open questions\n\n- Floats?\n")])}]
        ]),
    );
    analyst(
        &world,
        "analyze",
        &plan(&["calc.py", "test_calc.py", "README.md"], "Proof: green"),
        json!([]),
    );
    codex_scenario(
        &world,
        "critic",
        "critic",
        json!([[{"do": "result", "result": {"kind": "result", "summary": "Fine.", "documents": [
            {"name": "critic.md", "content": "none"},
            {"name": "questions-critic.json", "content": "[]"}
        ]}}]]),
    );
    world.run(&["take", KEY]).expect_code(0);
    tick_to(&world, "plan-ready");
    let investigate = step_turns(&world, "investigate");
    let agents: Vec<&str> = investigate.iter().map(|t| t.0.as_str()).collect();
    assert_eq!(agents, ["points", "points"], "{investigate:?}");
    assert!(
        world
            .ledger()
            .questions_in_states(&["new", "judging", "asked", "proposed", "answered"])
            .unwrap()
            .is_empty(),
        "a member never asks the person"
    );
    let refused = events(&world, "end-fact");
    assert!(
        refused.iter().any(|e| e["fact"] == "questions"
            && e["found"].as_str().unwrap().contains("## Open questions")),
        "{refused:?}"
    );
    // A wide plan: the critic ran at xhigh, the critic of a narrow plan was skipped, the batch
    // had no card, and the gate of a bug was skipped.
    let critic = step_turns(&world, "critic");
    assert_eq!(critic.len(), 1);
    let row = world
        .ledger()
        .turns_of_ticket(KEY)
        .unwrap()
        .into_iter()
        .find(|t| t.step.as_deref() == Some("critic"))
        .unwrap();
    assert_eq!(row.effort.as_deref(), Some("xhigh"));
    let skipped: Vec<String> = events(&world, "skipped")
        .iter()
        .map(|s| s["step"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(skipped, ["critic-narrow", "reproduced"], "{skipped:?}");
    let ask = events(&world, "ask");
    assert_eq!(ask[0]["cards"], 0);
    assert_eq!(world.turns_of("amend"), 0);
    // The kind shows in `vision status` and in `vision report`.
    let text = world.run(&["status"]).stdout;
    assert!(text.contains("DEMO-12 (feature)"), "{text}");
    let report = world.run(&["report"]).stdout;
    assert!(report.contains("DEMO-12 (feature)"), "{report}");
    let json = world.run(&["report", "--json"]).json();
    assert_eq!(json["tickets"][KEY]["kind"], "feature");
}

#[test]
fn a_bug_that_did_not_reproduce_goes_back_and_a_drop_stops_for_the_person() {
    let world = world_of("Bug");
    for (agent, doc) in [
        ("repro", "findings-repro.md"),
        ("cause", "findings-cause.md"),
        ("history", "findings-history.md"),
    ] {
        world.scenario(
            agent,
            "investigate",
            json!([[
                {"do": "init"},
                {"do": "facts"},
                {"do": "result", "structured_output": result("Done.", &[(doc, "Reproduced: no\n")])}
            ]]),
        );
    }
    codex_scenario(
        &world,
        "cause",
        "investigate",
        json!([[{"do": "result", "result": result("Found.", &[("findings-cause.md", "calc.py:4")])}]]),
    );
    world.scenario(
        "analyst",
        "analyze",
        json!([
            ends_with(result(
                "Analyzed.",
                &[
                    ("plan.md", &plan(&["calc.py"], "Reproduced: no")),
                    ("acceptance.json", ACCEPTANCE),
                    (
                        "questions-analyst.json",
                        &json!([card("Does the bug show in the release?", 0)]).to_string()
                    ),
                ]
            )),
            ends_with(result(
                "Analyzed.",
                &[
                    (
                        "plan.md",
                        &format!("{}\nDecision: drop\n", plan(&["calc.py"], "Reproduced: no"))
                    ),
                    ("acceptance.json", ACCEPTANCE),
                    ("questions-analyst.json", "[]"),
                ]
            )),
        ]),
    );
    codex_scenario(
        &world,
        "critic-narrow",
        "critic-narrow",
        json!([[{"do": "result", "result": result("Fine.", &[("critic.md", "none"), ("questions-critic.json", "[]")])}]]),
    );
    let dir = world.vision.join("claude/fake-scenarios");
    std::fs::write(
        dir.join("judge.json"),
        json!({"turns": [[{"do": "init"}, {"do": "result", "structured_output": {"answers": [
            {"card": "Q-0001", "outcome": "ask", "basis": "none", "sure": false, "reason": "Only the person knows."}
        ]}}]]})
        .to_string(),
    )
    .unwrap();
    world.run(&["take", KEY]).expect_code(0);
    world.tick_until("the card", Duration::from_secs(90), |w| {
        w.ledger().questions_in_states(&["asked"]).unwrap().len() == 1
    });
    // The person answers with the recommended choice: the plan stays the same.
    world
        .run(&["answer", "Q-0001", "--choice", "1"])
        .expect_code(0);
    world.tick_until("the drop", Duration::from_secs(120), |w| {
        !w.flags().is_empty()
    });
    assert_eq!(events(&world, "ask-answered")[0]["to"], "reproduced");
    // The first plan did not say `Reproduced: yes`, so the team investigated again.
    let checks = events(&world, "check");
    assert_eq!(checks[0]["holds"], false);
    assert_eq!(checks[0]["to"], "investigate");
    assert_eq!(world.counts()["entries"]["investigate"], 2);
    // The second plan says `Decision: drop`: Vision never stops a ticket by itself.
    assert_eq!(world.step().as_deref(), Some("reproduced"));
    assert_eq!(world.flags(), ["needs-you"]);
    assert_eq!(
        world.reason("needs-you"),
        "the step reproduced found the stop line `Decision: drop` in plan.md, so the person decides: run `vision retry DEMO-12 --step STEP` or `vision stop DEMO-12`"
    );
    // The app learns that a retry reads plan.md again, and that no turn starts.
    let facts = vision::pipeline::facts::state_facts_of(&world.ctx(), &world.ledger(), KEY)
        .unwrap()
        .unwrap();
    assert_eq!(facts.check.as_deref(), Some("plan.md"));
    // The second entry of the team gives each member the answers of the person.
    let second = world
        .ledger()
        .turns_of_ticket(KEY)
        .unwrap()
        .into_iter()
        .filter(|t| t.step.as_deref() == Some("investigate") && t.agent.as_deref() == Some("repro"))
        .nth(1)
        .unwrap();
    let id = vision::paths::TurnId::parse(&second.id).unwrap();
    let facts: Value = serde_json::from_str(
        &std::fs::read_to_string(world.ctx().paths.turn(&pipeline_support::key(), &id).facts)
            .unwrap(),
    )
    .unwrap();
    assert!(facts["documents"].get("answers.md").is_some(), "{facts}");
    assert_eq!(facts["ticket"]["kind"], "bug");
}

#[test]
fn a_chore_from_vision_kind_runs_scope_and_opens_the_pull_request_after_the_review() {
    let world = world_of("Story");
    world.scenario(
        "scope",
        "investigate",
        json!([ends_with(result(
            "Scoped.",
            &[("findings-scope.md", "- calc.py: rename\n")]
        ))]),
    );
    analyst(
        &world,
        "analyze",
        &plan(&["calc.py"], "Proof: green"),
        json!([]),
    );
    codex_scenario(
        &world,
        "critic-narrow",
        "critic-narrow",
        json!([[{"do": "result", "result": result("Fine.", &[("critic.md", "none"), ("questions-critic.json", "[]")])}]]),
    );
    world.run(&["take", KEY]).expect_code(0);
    let wrong = world.run(&["kind", KEY, "epic"]);
    wrong.expect_code(1);
    assert!(
        wrong.stderr.contains("`epic` is not a kind"),
        "{}",
        wrong.stderr
    );
    assert!(
        wrong.stderr.contains("bug, chore, feature"),
        "{}",
        wrong.stderr
    );
    let set = world.run(&["kind", KEY, "chore"]);
    set.expect_code(0);
    assert!(
        set.stdout.contains("DEMO-12 is now a chore ticket"),
        "{}",
        set.stdout
    );
    world.tick_until("the team", Duration::from_secs(60), |w| {
        w.turns_of("investigate") > 0
    });
    let agents: Vec<String> = step_turns(&world, "investigate")
        .into_iter()
        .map(|t| t.0)
        .collect();
    assert_eq!(agents, ["scope"]);
    assert_eq!(events(&world, "kind").last().unwrap()["how"], "vision kind");
    // A narrow chore: the critic of a wide plan and the gate of a bug are skipped.
    tick_to(&world, "plan-ready");
    let skipped: Vec<String> = events(&world, "skipped")
        .iter()
        .map(|s| s["step"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(skipped, ["critic", "reproduced"], "{skipped:?}");
    assert_eq!(world.turns_of("critic-narrow"), 1);
    assert!(world.flags().is_empty(), "{:?}", world.flags());

    // From the plan on, the path is that of the demo: the build and the review keep their
    // commits in the clone, and the act step open-pr pushes the branch and opens the pull
    // request after the review.
    world.scenario(
        "builder",
        "build",
        json!([runs_then(
            "printf '# The functions of the toy calculator.\\n' >> calc.py && git commit -q -am 'Comment calc.py'",
            result("Built.", &[("pr.md", "Comment calc.py\n\nA chore.")])
        )]),
    );
    world.scenario(
        "reviewer",
        "review",
        json!([ends_with(result(
            "Reviewed.",
            &[("review.md", "# Review\n\n## Findings\n\nNone.\n")]
        ))]),
    );
    let shown = world.run(&["plan", KEY]);
    shown.expect_code(0);
    world
        .run(&["approve-plan", KEY, &plan_hash_of(&shown.stdout)])
        .expect_code(0);
    tick_to(&world, "open-pr");
    assert_eq!(world.turns_of("review"), 1);
    for verb in ["git.push", "pr.open"] {
        assert!(
            world
                .ledger()
                .operations_of(KEY, Some(verb))
                .unwrap()
                .is_empty(),
            "no {verb} before the review held"
        );
        assert_eq!(world.acts_of(verb), 0, "{verb}");
    }
    tick_to(&world, "ci");
    let ops: Vec<String> = world
        .ledger()
        .operations_of(KEY, None)
        .unwrap()
        .into_iter()
        .filter(|o| o.state == "confirmed")
        .map(|o| o.verb)
        .filter(|v| v == "git.push" || v == "pr.open")
        .collect();
    assert_eq!(
        ops,
        ["git.push", "pr.open"],
        "the push, then the pull request"
    );
    assert_eq!(
        world.world_json("prs.json")["prs"][0]["title"],
        "Comment calc.py"
    );
    assert!(world.flags().is_empty(), "{:?}", world.flags());
}

#[test]
fn doctor_names_the_codex_cli_and_model_set_takes_a_runner() {
    let world = world_of("Bug");
    let doctor = world.run(&["doctor"]);
    let line = doctor
        .stdout
        .lines()
        .find(|l| l.contains("codex"))
        .unwrap_or_default()
        .to_string();
    assert!(line.contains("codex-cli 0.0.0-fake"), "{}", doctor.stdout);
    assert!(line.contains("for cause, critic, critic-narrow"), "{line}");
    assert!(
        line.contains("Its sandbox hides the home of Vision and the private folders"),
        "{line}"
    );
    // A sandbox that shows the home of Vision is a warning, and no test file stays.
    let marker = world
        .vision
        .join("claude/fake-scenarios/codex-open-sandbox");
    std::fs::create_dir_all(marker.parent().unwrap()).unwrap();
    std::fs::write(&marker, "").unwrap();
    let doctor = world.run(&["doctor"]);
    let line = doctor
        .stdout
        .lines()
        .find(|l| l.contains("codex"))
        .unwrap_or_default()
        .to_string();
    assert!(line.starts_with("warn"), "{line}");
    assert!(
        line.contains("codex cannot hide private folders on this machine: a command in its sandbox read a file of the home of Vision"),
        "{line}"
    );
    let left: Vec<String> = std::fs::read_dir(&world.vision)
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.starts_with(".doctor-"))
        .collect();
    assert!(left.is_empty(), "{left:?}");
    std::fs::remove_file(&marker).unwrap();
    // A Claude model runs on Claude; a Codex model keeps the runner.
    let set = world.run(&["model", "set", "cause", "opus"]);
    set.expect_code(0);
    let config = std::fs::read_to_string(world.vision.join("config.toml")).unwrap();
    assert!(
        config.contains("[agents.cause]") && config.contains("runner = \"claude\""),
        "{config}"
    );
    let table = world.run(&["model", "--json"]).json();
    let cause = table
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["agent"] == "cause")
        .unwrap();
    assert_eq!(cause["runner"], "claude");
    assert_eq!(cause["fallback"], Value::Null);
    world
        .run(&[
            "model",
            "set",
            "cause",
            "gpt-6.1-sol",
            "--runner",
            "codex",
            "--effort",
            "xhigh",
        ])
        .expect_code(0);
    let table = world.run(&["model"]).stdout;
    assert!(table.contains("gpt-6.1-sol on codex"), "{table}");
    assert!(
        table.contains("fallback on claude: claude-opus-5-5 high"),
        "{table}"
    );
    let judge = world.run(&["model", "set", "judge", "gpt-6.1-sol", "--runner", "codex"]);
    judge.expect_code(1);
    assert!(
        judge.stderr.contains("cannot run on Codex"),
        "{}",
        judge.stderr
    );
}

#[test]
fn retry_of_a_team_starts_again_only_the_member_that_failed() {
    let world = world_of("Bug");
    world.scenario(
        "repro",
        "investigate",
        json!([ends_with(result(
            "Reproduced.",
            &[("findings-repro.md", "Reproduced: yes\n")]
        ))]),
    );
    codex_scenario(
        &world,
        "cause",
        "investigate",
        json!([[{"do": "result", "result": result("Found.", &[("findings-cause.md", "calc.py:4")])}]]),
    );
    // history forgets its findings twice: one resume, then the person.
    world.scenario(
        "history",
        "investigate",
        json!([
            ends_with(result("Nothing.", &[])),
            ends_with(result("Nothing.", &[])),
            ends_with(result(
                "Read the history.",
                &[("findings-history.md", "- calc.py:4 why: none\n")]
            ))
        ]),
    );
    world.run(&["take", KEY]).expect_code(0);
    world.tick_until("needs-you", Duration::from_secs(90), |w| {
        !w.flags().is_empty()
    });
    assert_eq!(world.flags(), ["needs-you"]);
    let reason = world.reason("needs-you");
    assert!(
        reason.starts_with("the member history of the step investigate: the end fact doc"),
        "{reason}"
    );
    world.run(&["retry", KEY]).expect_code(0);
    tick_to(&world, "analyze");
    let count = |agent: &str| {
        step_turns(&world, "investigate")
            .iter()
            .filter(|t| t.0 == agent)
            .count()
    };
    assert_eq!(
        (count("repro"), count("cause"), count("history")),
        (1, 1, 3)
    );
}

#[test]
fn a_sandbox_that_shows_the_home_of_vision_sends_the_turn_to_claude() {
    let world = world_of("Story");
    world.scenario(
        "points",
        "investigate",
        json!([ends_with(result(
            "Points.",
            &[("findings-points.md", "1. subtract: missing.\n")]
        ))]),
    );
    analyst(
        &world,
        "analyze",
        &plan(&["calc.py", "test_calc.py", "README.md"], "Proof: green"),
        json!([]),
    );
    // The fake sandbox of this test lets a command read each file. So the test before the
    // turn finds the canary, and the critic never starts on Codex.
    let marker = world
        .vision
        .join("claude/fake-scenarios/codex-open-sandbox");
    std::fs::write(&marker, "").unwrap();
    world.scenario(
        "critic",
        "critic",
        json!([ends_with(result(
            "Fine.",
            &[("critic.md", "none"), ("questions-critic.json", "[]")]
        ))]),
    );
    world.run(&["take", KEY]).expect_code(0);
    tick_to(&world, "plan-ready");
    let fallback = events(&world, "fallback");
    assert_eq!(fallback.len(), 1, "{fallback:?}");
    assert_eq!(fallback[0]["agent"], "critic");
    assert!(
        fallback[0]["why"].as_str().unwrap().contains(
            "codex cannot hide private folders on this machine: a command in its sandbox read a file of the home of Vision"
        ),
        "{fallback:?}"
    );
    let rows: Vec<_> = world
        .ledger()
        .turns_of_ticket(KEY)
        .unwrap()
        .into_iter()
        .filter(|t| t.step.as_deref() == Some("critic"))
        .collect();
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].status.as_deref(), Some("failed"));
    assert_eq!(rows[1].status.as_deref(), Some("done"));
    assert_eq!(rows[1].model.as_deref(), Some("claude-opus-5-5"));
    // `codex exec` never ran, and the test folder is gone.
    let dir = world
        .vision
        .join("tickets")
        .join(KEY)
        .join("turns")
        .join(&rows[0].id);
    assert!(!dir.join("codex-args.json").exists());
    assert!(!dir.join("sandbox-test").exists());
}
