//! The whole path of one ticket (design section 11.3), the most important test of this repo:
//! a temporary home, a demo world, a clone, and the fake `claude`. `vision take DEMO-12` and
//! then `vision tick` in a loop carry the ticket from the tracker to a merged pull request,
//! with `bin/world` of the demo adapter playing the team.

mod pipeline_support;

use std::time::Duration;

use pipeline_support::{
    ends_with, plan_hash_of, result, runs_then, World, BUILD_COMMAND, KEY, PLAN,
};
use serde_json::{json, Value};
use vision::memory::{Kind, Memory, NewDecision, Writer};

/// A valid card of the scout.
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

fn decision_files(world: &World) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(world.vision.join("memory/decisions"))
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.ends_with(".md"))
        .collect();
    names.sort();
    names
}

fn memory_file(world: &World, path: &str) -> String {
    std::fs::read_to_string(world.vision.join("memory").join(path)).unwrap()
}

fn status(world: &World) -> Value {
    let run = world.run(&["status", "--json"]);
    run.expect_code(0);
    run.json()
}

fn ticket_status(world: &World) -> Value {
    status(world)["tickets"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["key"] == KEY)
        .cloned()
        .expect("the status has DEMO-12")
}

fn needs(world: &World) -> Vec<Value> {
    status(world)["needs"].as_array().unwrap().clone()
}

#[test]
fn a_ticket_goes_from_the_tracker_to_a_merged_pull_request() {
    let world = World::new();

    // The memory has a decision of the person that answers the first card.
    let memory = Memory::open(&world.ctx().paths, "user").unwrap();
    let mut seeded = NewDecision::answer(
        Writer::Person("user".into()),
        "Must subtract take the same numbers as add?",
        "Yes",
    );
    seeded.kind = Some(Kind::Product);
    seeded.area = vec!["calculator".into()];
    seeded.why = "The calculator treats each function alike.".into();
    memory.add_decision(&seeded).unwrap();

    world.scenario(
        "scout",
        "refine",
        json!([
            ends_with(
                json!({"kind": "questions", "summary": "Two questions.", "questions": [
                    card("Must subtract take the same numbers as add?"),
                    card("Must the README name subtract?")
                ]})
            ),
            ends_with(result(
                "Refined.",
                &[
                    (
                        "refine.md",
                        "# Refine\n\nsubtract goes next to add in calc.py.\n"
                    ),
                    (
                        "description.md",
                        "Add subtract(a, b) to the calculator, with a test."
                    ),
                ]
            ))
        ]),
    );
    world.scenario(
        "judge",
        "",
        json!([[{"do": "init"}, {"do": "result", "structured_output": {"answers": [
            {"card": "Q-0001", "outcome": "answer", "basis": "decision", "sources": ["D-0001"],
             "sure": true, "choice": 0, "topics": [], "reason": "D-0001 answers it."},
            {"card": "Q-0002", "outcome": "ask", "basis": "none", "sources": [], "sure": false,
             "choice": null, "topics": [], "reason": "No decision covers the README."}
        ]}, "cost": 0.05}]]),
    );
    world.scenario(
        "builder",
        "plan",
        json!([ends_with(result("Planned.", &[("plan.md", PLAN)]))]),
    );
    world.scenario(
        "builder",
        "build",
        json!([runs_then(
            BUILD_COMMAND,
            result(
                "Built.",
                &[(
                    "pr.md",
                    "Add subtract to the calculator\n\nThe calculator can subtract now."
                )]
            )
        )]),
    );
    let review = "# Review\n\n## Findings\n\n### F1: subtract has no test for negative numbers (minor)\nKept: a test shows it.\n\n### F2: the docstring is short\nRejected: style_only.\n\n## Open findings\n\n- F1: subtract has no test for negative numbers\n";
    world.scenario(
        "reviewer",
        "review",
        json!([ends_with(result("Reviewed.", &[("review.md", review)]))]),
    );
    world.scenario(
        "builder",
        "fix-ci",
        json!([runs_then(
            "git rm -q TODO.txt && git commit -q -m 'Remove the TODO file'",
            result("Fixed the CI.", &[])
        )]),
    );
    let verdicts = r#"[{"comment_id": 1, "verdict": "incorrect", "fix_commit": "", "reply": "The other tests use the same names."}]"#;
    world.scenario(
        "shepherd",
        "comments",
        json!([ends_with(result(
            "Answered the comment.",
            &[("verdicts.json", verdicts)]
        ))]),
    );
    world.scenario(
        "curator",
        "",
        json!([[{"do": "init"}, {"do": "result", "structured_output": {"summary": "Nothing to change.", "changes": []}}]]),
    );

    // pick and take.
    let pick = world.run(&["pick", "--json"]);
    pick.expect_code(0);
    assert_eq!(pick.json()[0]["key"], KEY);
    assert_eq!(pick.json()[0]["eligible"], true);
    let take = world.run(&["take", KEY]);
    take.expect_code(0);
    assert!(
        take.stdout.contains("The next tick starts the step refine"),
        "{take:?}"
    );
    assert_eq!(world.step().as_deref(), Some("refine"));
    assert_eq!(
        pipeline_support::git(world.clone(), &["rev-parse", "--abbrev-ref", "HEAD"]),
        "vision/DEMO-12"
    );
    let pick = world.run(&["pick", "--json"]).json();
    assert_eq!(
        pick[0]["taken"], "refine",
        "a held ticket is shown as taken"
    );

    // refine: two cards; the Judge answers one from the memory and sends one to the person.
    world.tick_until("the card for the person", Duration::from_secs(60), |w| {
        needs(w).iter().any(|n| n["kind"] == "card")
    });
    assert!(world
        .doc("issue.json")
        .unwrap()
        .contains("Add subtract to the calculator"));
    let card_needs: Vec<Value> = needs(&world)
        .into_iter()
        .filter(|n| n["kind"] == "card")
        .collect();
    assert_eq!(card_needs.len(), 1, "{card_needs:?}");
    assert_eq!(card_needs[0]["id"], "Q-0002");
    assert!(ticket_status(&world)["now"]
        .as_str()
        .unwrap()
        .contains("needs you: 1 card"));
    assert_eq!(
        world.step().as_deref(),
        Some("refine"),
        "the cards hold the step"
    );
    assert_eq!(
        world.turns_of("refine"),
        1,
        "no second turn while a card is open"
    );
    world
        .run(&[
            "answer",
            "Q-0002",
            "--choice",
            "1",
            "--why",
            "People read the README.",
        ])
        .expect_code(0);
    let decisions = decision_files(&world);
    assert_eq!(decisions, ["D-0001.md", "D-0002.md", "D-0003.md"]);
    assert!(memory_file(&world, "decisions/D-0002.md").contains("by: judge"));
    assert!(memory_file(&world, "decisions/D-0003.md").contains("by: user"));

    // The answers resume the session; the description goes to the tracker.
    world.tick_to("plan");
    assert!(world
        .doc("refine.md")
        .unwrap()
        .contains("subtract goes next to add"));
    assert!(world.doc("answers.md").unwrap().contains("Q-0002"));
    let tracker = world.world_json("tracker.json");
    assert_eq!(
        tracker["tickets"][0]["description"],
        "Add subtract(a, b) to the calculator, with a test."
    );
    assert_eq!(world.acts_of("tracker.describe"), 1);
    let refine_turns: Vec<_> = world
        .turns()
        .into_iter()
        .filter(|t| t.0 == "refine")
        .collect();
    assert_eq!(refine_turns.len(), 2, "{refine_turns:?}");
    assert_eq!(refine_turns[1].3.as_deref(), Some("held"));

    // The plan, and its approval.
    world.tick_to("plan-ready");
    assert_eq!(world.doc("plan.md").as_deref(), Some(PLAN));
    assert_eq!(world.doc("plan.agent.md").as_deref(), Some(PLAN));
    let plan_need = needs(&world)
        .into_iter()
        .find(|n| n["kind"] == "plan")
        .expect("the plan waits for the approval");
    let plan = world.run(&["plan", KEY]);
    plan.expect_code(0);
    let hash = plan_hash_of(&plan.stdout);
    assert_eq!(plan_need["id"], hash.as_str());
    world.run(&["approve-plan", KEY, &hash]).expect_code(0);
    assert_eq!(world.doc("approved-plan.md").as_deref(), Some(PLAN));

    // The build: a commit, pr.md, the proof, the push, and the pull request.
    world.tick_to("review");
    let build_turns: Vec<_> = world
        .turns()
        .into_iter()
        .filter(|t| t.0 == "build")
        .collect();
    assert_eq!(build_turns.len(), 1);
    assert_eq!(build_turns[0].3.as_deref(), Some("held"));
    assert!(world.doc("pr.md").unwrap().starts_with("Add subtract"));
    assert!(
        world.doc("proof.txt").is_some(),
        "the proof job wrote its log"
    );
    let prs = world.world_json("prs.json");
    assert_eq!(prs["prs"][0]["number"], 1);
    assert_eq!(prs["prs"][0]["title"], "Add subtract to the calculator");
    assert_eq!(prs["prs"][0]["body"], "The calculator can subtract now.");
    assert_eq!(world.row().unwrap().pr, Some(1));
    let head = pipeline_support::git(world.clone(), &["rev-parse", "HEAD"]);
    let remote = pipeline_support::git(
        &world.world.join("remote.git"),
        &["rev-parse", "refs/heads/vision/DEMO-12"],
    );
    assert_eq!(head, remote, "the branch is on the forge");
    let diff = world.run(&["diff", KEY, "--json"]);
    diff.expect_code(0);
    assert_eq!(diff.json()["commits"], json!([]), "{diff:?}");

    // The review step, then a red CI run and the fix.
    world.tick_to("fix-ci");
    assert!(
        world.doc("second-review.md").is_some(),
        "the before job ran"
    );
    assert!(world.doc("review.md").is_some());
    let findings = world.ledger().findings_of(KEY).unwrap();
    assert_eq!(findings.len(), 2, "{findings:?}");
    assert!(findings.iter().all(|f| f.source == "review"));
    assert_eq!(findings[1].reason_code.as_deref(), Some("style_only"));
    assert_eq!(
        world.acts_of("git.push"),
        1,
        "the review made no commit, so no second push"
    );
    let checks = world.ledger().checks_of(KEY, Some(&head)).unwrap();
    assert!(
        checks.iter().any(|c| c.kind == "ci" && c.result == "red"),
        "{checks:?}"
    );

    world.tick_to("ready");
    assert!(world.doc("ci-log.txt").unwrap().contains("CI is red"));
    let fixed = pipeline_support::git(world.clone(), &["rev-parse", "HEAD"]);
    assert_ne!(fixed, head);
    let checks = world.ledger().checks_of(KEY, Some(&fixed)).unwrap();
    assert!(
        checks.iter().any(|c| c.kind == "ci" && c.result == "green"),
        "{checks:?}"
    );
    assert_eq!(world.acts_of("ci.run"), 2);
    let marks = &ticket_status(&world)["marks"];
    assert_eq!(marks["ci"]["result"], "green", "{marks}");

    // The team marks the pull request ready and comments on it.
    world.world_cmd(&["ready", world.root(), "1"]);
    world.world_cmd(&[
        "comment",
        world.root(),
        "1",
        "Name the test file test_calc_subtract.py",
        "--author",
        "someone",
    ]);
    world.tick_until("the approval of the reply", Duration::from_secs(60), |w| {
        needs(w).iter().any(|n| n["kind"] == "operation")
    });
    assert_eq!(world.step().as_deref(), Some("comments"));
    let reviews = world.ledger().review_rows_after(0, 10).unwrap();
    assert_eq!(reviews.len(), 1);
    assert_eq!(reviews[0].comment_id, Some(1));
    assert_eq!(reviews[0].verdict.as_deref(), Some("incorrect"));
    assert_eq!(reviews[0].reviewer.as_deref(), Some("someone"));
    let reply = needs(&world)
        .into_iter()
        .find(|n| n["kind"] == "operation")
        .unwrap();
    let id = reply["id"].as_str().unwrap().to_string();
    let (verb, commit) = id.split_once(' ').unwrap();
    assert_eq!(verb, "review.reply");
    assert_eq!(
        ticket_status(&world)["now"],
        "waits for your approval of review.reply. Run `vision needs`",
        "the turn ended well, so the column names the after verb"
    );
    world.run(&["approve", KEY, verb, commit]).expect_code(0);
    let prs = world.world_json("prs.json");
    assert_eq!(
        prs["prs"][0]["comments"][0]["replies"][0]["body"],
        "The other tests use the same names."
    );

    // The approval and the merge by the team, and the end.
    world.tick_to("ready-to-merge");
    world.world_cmd(&["approve", world.root(), "1", "--author", "someone"]);
    world.world_cmd(&["merge", world.root(), "1"]);
    world.tick_until("the end", Duration::from_secs(60), |w| {
        w.counts()["finished"].is_string()
    });
    let row = world.row().unwrap();
    assert_eq!(row.state.as_deref(), Some("done"));
    assert_eq!(row.clone, None, "the end frees the clone");
    let merged = world
        .ledger()
        .event_by_external_id("ticket-merged:DEMO-12:1")
        .unwrap();
    assert!(merged.is_some(), "the merged event for the Curator");
    let spans = world.ledger().spans_of(Some(KEY)).unwrap();
    assert!(spans.iter().all(|s| s.ended.is_some()), "{spans:?}");
    let steps: Vec<&str> = spans.iter().map(|s| s.state.as_str()).collect();
    assert_eq!(
        steps,
        [
            "refine",
            "plan",
            "plan-ready",
            "build",
            "review",
            "ci",
            "fix-ci",
            "ci",
            "ready",
            "comments",
            "ci-review",
            "ready-to-merge",
            "done"
        ]
    );
    assert_eq!(
        ticket_status(&world)["now"],
        "done with the merged pull request #1"
    );

    // Each operation ran once, and none waits.
    for (verb, n) in [
        ("tracker.describe", 1),
        ("git.push", 2),
        ("pr.open", 1),
        ("ci.run", 2),
        ("review.reply", 1),
    ] {
        assert_eq!(world.acts_of(verb), n, "{verb}");
    }
    let ops = world.ledger().operations_of(KEY, None).unwrap();
    assert!(ops.iter().all(|o| o.state == "confirmed"), "{ops:?}");
    let ci_rows: Vec<String> = world
        .ledger()
        .checks_of(KEY, None)
        .unwrap()
        .into_iter()
        .filter(|c| c.kind == "ci")
        .map(|c| c.result)
        .collect();
    assert_eq!(
        ci_rows,
        ["red", "green"],
        "one row for each commit and result"
    );

    // The evidence: the end facts, the human rows, and the report.
    let facts: Vec<Option<String>> = world
        .turns()
        .into_iter()
        .filter(|t| t.1 != "judge" && t.0 != "refine")
        .map(|t| t.3)
        .collect();
    assert!(
        facts.iter().all(|f| f.as_deref() == Some("held")),
        "{facts:?}"
    );
    let report = world.run(&["report", "--json"]);
    report.expect_code(0);
    let report = report.json();
    let measures = &report["tickets"][KEY];
    assert_eq!(measures["cards_to_person"], 1);
    assert_eq!(measures["cards_by_memory"], 1);
    assert_eq!(measures["cards_by_judge"], 0);
    let cost = measures["cost_usd"].as_f64().unwrap();
    assert!(cost > 2.0, "{measures}");
    assert!(
        measures["step_cost_usd"]["build"].as_f64().unwrap() >= 0.5,
        "{measures}"
    );
    assert!(
        measures["step_seconds"]["refine"].as_i64().is_some(),
        "{measures}"
    );
    assert_eq!(report["total"]["cost_usd"], measures["cost_usd"]);
    let log = world.run(&["log", KEY, "--json"]).json();
    let kinds: Vec<&str> = log
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["kind"].as_str().unwrap())
        .collect();
    assert!(
        kinds.contains(&"take") && kinds.contains(&"ended"),
        "{kinds:?}"
    );

    // The merged event makes the Curator run once; let its turn end before the home goes.
    world.tick_until(
        "the end of the Curator turn",
        Duration::from_secs(60),
        |w| w.ledger().turns_with_status("running").unwrap().is_empty(),
    );
}
