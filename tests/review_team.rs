//! A review team (sections 10, 11.2, and 11.3), driven through `vision tick` on the demo adapter
//! with the review team (`adapters/demo-review-team`): three agents that the adapter defines
//! (two readers on Claude and one reader on Codex) review the branch in parallel, challenge the
//! findings of each other, and the lead Reviewer fixes the kept findings. A commit of the lead
//! sends the ticket back to the lanes, no commit sends it to the act step open-pr, which pushes
//! the branch and opens the pull request before CI, and the cap of the lanes stops a third
//! round. Nothing calls the real `claude` or `codex`.

mod pipeline_support;

use std::path::{Path, PathBuf};
use std::time::Duration;

use pipeline_support::{
    demo_dir, ends_with, git, result, runs_then, simple_scenarios, take_and_approve, World, KEY,
};
use serde_json::{json, Value};
use vision::paths::{TicketKey, TurnId};

const LONG: Duration = Duration::from_secs(120);
const LANES: [&str; 3] = ["cr-correctness", "cr-design", "cr-codex"];

/// The folder of the review team adapter in this repo.
fn team_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("adapters/demo-review-team")
}

/// Writes the review team adapter and its role files in the home as the adapter `demo`, with
/// the scripts of the demo adapter of the repo.
fn write_review_team(world: &World) {
    let text = std::fs::read_to_string(team_dir().join("adapter.toml"))
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
    std::fs::create_dir_all(dir.join("roles")).unwrap();
    std::fs::write(dir.join("adapter.toml"), text).unwrap();
    for entry in std::fs::read_dir(team_dir().join("roles")).unwrap() {
        let path = entry.unwrap().path();
        std::fs::copy(&path, dir.join("roles").join(path.file_name().unwrap())).unwrap();
    }
}

/// A plugin with one skill in the hidden folder `.claude` of the home folder of the runs, as
/// a person keeps the plugins of Claude Code. Gives the plugin folder and the skill file.
fn review_plugin(world: &World) -> (PathBuf, PathBuf) {
    let plugin = world.user_home.join(".claude/plugins/review-kit");
    std::fs::create_dir_all(plugin.join(".claude-plugin")).unwrap();
    std::fs::write(
        plugin.join(".claude-plugin/plugin.json"),
        r#"{"name": "review-kit"}"#,
    )
    .unwrap();
    let skill = plugin.join("skills/lane/SKILL.md");
    std::fs::create_dir_all(skill.parent().unwrap()).unwrap();
    std::fs::write(
        &skill,
        "---\nname: lane\n---\nRead each changed file twice. lane-skill-marker\n",
    )
    .unwrap();
    world.edit_config(|doc| {
        let mut list = toml_edit::Array::new();
        list.push(plugin.display().to_string());
        doc["claude"]["plugins"] = toml_edit::value(list);
    });
    (plugin, skill)
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

/// The findings of a lane in the form of the gate cr.
fn finding(path_line: &str, title: &str) -> String {
    format!("1. [MAJOR][BLOCKING] {path_line} {title}\n   Trigger: subtract(1.5, 1).\n   Net: +6\n")
}

/// The scenarios of the lanes and the challenge for two rounds. The lane on Codex reads the
/// skill of the plugin in its first round, and fails at a usage limit in its second round, so
/// that it falls back to Claude.
fn lane_scenarios(world: &World, skill: &Path) {
    world.scenario(
        "cr-correctness",
        "review-lanes",
        json!([
            ends_with(result(
                "Reviewed.",
                &[(
                    "findings-cr-correctness.md",
                    &finding("calc.py:5", "subtract drops the fraction"),
                )]
            )),
            ends_with(result(
                "Reviewed again.",
                &[("findings-cr-correctness.md", "none")]
            ))
        ]),
    );
    world.scenario(
        "cr-design",
        "review-lanes",
        json!([ends_with(result(
            "Reviewed.",
            &[("findings-cr-design.md", "none")]
        ))]),
    );
    codex_scenario(
        world,
        "cr-codex",
        "review-lanes",
        json!([
            [
                {"do": "message", "text": "I read the skill first."},
                {"do": "read", "path": skill.display().to_string()},
                {"do": "result", "result": {"kind": "result", "summary": "Reviewed.", "documents": [
                    {"name": "findings-cr-codex.md", "content": "none\n\nThe skill said: {read0}"}
                ]}}
            ],
            [{"do": "fail", "message": "You've hit your usage limit. Try again later."}]
        ]),
    );
    // The fallback of the lane on Codex runs on Claude with the same agent.
    world.scenario(
        "cr-codex",
        "review-lanes",
        json!([ends_with(result(
            "Reviewed on the fallback.",
            &[("findings-cr-codex.md", "none")]
        ))]),
    );
    for lane in ["cr-correctness", "cr-design"] {
        world.scenario(
            lane,
            "review-challenge",
            json!([ends_with(result(
                "Challenged.",
                &[(
                    &format!("challenge-{lane}.md"),
                    "cr-correctness F1: agree: calc.py:5 converts with int().",
                )]
            ))]),
        );
    }
    codex_scenario(
        world,
        "cr-codex",
        "review-challenge",
        json!([[{"do": "result", "result": {"kind": "result", "summary": "Challenged.", "documents": [
            {"name": "challenge-cr-codex.md", "content": "cr-correctness F1: agree: calc.py:5 converts with int()."}
        ]}}]]),
    );
}

/// The turn of the lead that fixes calc.py and commits, with its two documents.
fn lead_fixes(text: &str) -> Value {
    runs_then(
        &format!("printf '{text}\\n' >> calc.py && git add calc.py && git commit -q -m 'fix: keep the fraction'"),
        result(
            "Fixed one finding.",
            &[
                ("cr.md", "No open finding."),
                ("cr-fixed.md", "- cr-correctness F1: fixed (subtract keeps the fraction)."),
            ],
        ),
    )
}

/// The turn of the lead that changes nothing.
fn lead_keeps() -> Value {
    ends_with(result(
        "Nothing to fix.",
        &[("cr.md", "No open finding."), ("cr-fixed.md", "none")],
    ))
}

/// A world with the review team adapter, a plugin, and the scenarios up to the review.
fn team_world() -> (World, PathBuf, PathBuf) {
    let world = World::new();
    write_review_team(&world);
    let (plugin, skill) = review_plugin(&world);
    simple_scenarios(&world);
    lane_scenarios(&world, &skill);
    (world, plugin, skill)
}

/// The turn rows of one step of DEMO-12, oldest first.
fn step_turns(world: &World, step: &str) -> Vec<vision::ledger::turns::TurnRow> {
    world
        .ledger()
        .turns_of_ticket(KEY)
        .unwrap()
        .into_iter()
        .filter(|t| t.step.as_deref() == Some(step))
        .collect()
}

/// The folder of a turn of DEMO-12.
fn turn_dir(world: &World, turn: &str) -> PathBuf {
    world
        .ctx()
        .paths
        .turn(
            &TicketKey::parse(KEY).unwrap(),
            &TurnId::parse(turn).unwrap(),
        )
        .dir
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

#[test]
fn a_review_team_reviews_challenges_fixes_and_looks_again_at_the_new_head() {
    let (world, plugin, skill) = team_world();
    world.scenario(
        "reviewer",
        "review",
        json!([lead_fixes("# fixed in round 1"), lead_keeps()]),
    );
    take_and_approve(&world);
    world.tick_until("the lanes start", LONG, |w| {
        !step_turns(w, "review-lanes").is_empty()
    });
    // The three lanes started in one tick, each in its own session.
    let lanes = step_turns(&world, "review-lanes");
    let mut agents: Vec<&str> = lanes.iter().filter_map(|t| t.agent.as_deref()).collect();
    agents.sort_unstable();
    assert_eq!(
        agents,
        ["cr-codex", "cr-correctness", "cr-design"],
        "{lanes:?}"
    );
    let sessions: std::collections::BTreeSet<&str> =
        lanes.iter().filter_map(|t| t.session.as_deref()).collect();
    assert_eq!(sessions.len(), 3, "each lane has its own session");
    let models: Vec<(&str, &str, &str)> = lanes
        .iter()
        .map(|t| {
            (
                t.agent.as_deref().unwrap(),
                t.model.as_deref().unwrap(),
                t.effort.as_deref().unwrap(),
            )
        })
        .collect();
    for expected in [
        ("cr-correctness", "claude-opus-5-5", "high"),
        ("cr-design", "claude-sonnet-5-5", "high"),
        ("cr-codex", "gpt-6.1-sol", "high"),
    ] {
        assert!(models.contains(&expected), "{models:?}");
    }

    world.tick_until("the lead commits and the lanes run again", LONG, |w| {
        w.counts()["entries"]["review-lanes"] == 2
    });
    // The lane on Codex ran with the profile vision_reader. The profile keeps the hidden
    // folders, and it reads the plugin folder, so the lane read the file of its skill.
    let codex = lanes
        .iter()
        .find(|t| t.agent.as_deref() == Some("cr-codex"))
        .unwrap();
    let dir = turn_dir(&world, &codex.id);
    let args: Value =
        serde_json::from_str(&std::fs::read_to_string(dir.join("codex-args.json")).unwrap())
            .unwrap();
    let line: Vec<&str> = args["args"]
        .as_array()
        .unwrap()
        .iter()
        .map(|a| a.as_str().unwrap())
        .collect();
    assert!(
        line.contains(&"default_permissions=\"vision_reader\""),
        "{line:?}"
    );
    let filesystem = line
        .iter()
        .find(|a| a.starts_with("permissions.vision_reader.filesystem="))
        .unwrap();
    let hidden = world.user_home.join(".claude").display().to_string();
    assert!(
        filesystem.contains(&format!("{hidden:?} = \"none\"")),
        "{filesystem}"
    );
    let home = world.vision.display().to_string();
    assert!(
        filesystem.contains(&format!("{home:?} = \"none\"")),
        "{filesystem}"
    );
    let plugin_text = plugin.display().to_string();
    assert!(
        filesystem.contains(&format!("{plugin_text:?} = \"read\"")),
        "{filesystem}"
    );
    // The role file of an agent of the adapter comes after the common rules, as for an agent
    // of Vision.
    let role = std::fs::read_to_string(dir.join("role.md")).unwrap();
    assert!(role.starts_with("# Rules for each work turn"), "{role}");
    assert!(role.contains("\n# Second-model lane\n"), "{role}");
    let prompt = std::fs::read_to_string(dir.join("prompt.md")).unwrap();
    assert!(
        prompt.contains(&format!("- review-kit:lane: {}", skill.display())),
        "{prompt}"
    );
    let stream = std::fs::read_to_string(dir.join("stream.jsonl")).unwrap();
    assert!(stream.contains("lane-skill-marker"), "{stream}");

    // Each member of the challenge read only the findings of the two other lanes.
    let challenge = step_turns(&world, "review-challenge");
    assert_eq!(challenge.len(), 3, "{challenge:?}");
    for turn in &challenge {
        let agent = turn.agent.clone().unwrap();
        let facts: Value = serde_json::from_str(
            &std::fs::read_to_string(turn_dir(&world, &turn.id).join("facts.json")).unwrap(),
        )
        .unwrap();
        let mut docs: Vec<String> = facts["documents"]
            .as_object()
            .unwrap()
            .keys()
            .filter(|k| k.starts_with("findings-") || k.starts_with("challenge-"))
            .cloned()
            .collect();
        docs.sort();
        let mut others: Vec<String> = LANES
            .iter()
            .filter(|l| **l != agent)
            .map(|l| format!("findings-{l}.md"))
            .collect();
        others.sort();
        assert_eq!(docs, others, "{agent}: {facts}");
        assert_eq!(
            facts["expected_documents"],
            json!([format!("challenge-{agent}.md")])
        );
    }
    // The lead read each findings file and each challenge file, and fixed one finding.
    let lead = step_turns(&world, "review");
    assert_eq!(lead.len(), 1);
    let facts: Value = serde_json::from_str(
        &std::fs::read_to_string(turn_dir(&world, &lead[0].id).join("facts.json")).unwrap(),
    )
    .unwrap();
    for lane in LANES {
        for doc in [
            format!("findings-{lane}.md"),
            format!("challenge-{lane}.md"),
        ] {
            assert!(facts["documents"][&doc].is_string(), "{doc}: {facts}");
        }
    }
    assert!(world
        .doc("findings-cr-codex.md")
        .unwrap_or_default()
        .contains("lane-skill-marker"));
    assert!(git(world.clone(), &["log", "--format=%s"]).contains("fix: keep the fraction"));
    // The move by commits says the way and the two heads.
    let moves: Vec<Value> = events(&world, "step")
        .into_iter()
        .filter(|e| e["from"] == "review")
        .collect();
    assert_eq!(moves.len(), 1, "{moves:?}");
    assert_eq!(moves[0]["to"], "review-lanes");
    assert_eq!(moves[0]["how"], "on commits");
    assert_eq!(moves[0]["way"], "commits");
    let head = git(world.clone(), &["rev-parse", "HEAD"]);
    assert_eq!(moves[0]["head"], head.as_str());
    assert_ne!(moves[0]["entry_head"], moves[0]["head"]);
    assert_eq!(
        moves[0]["entry_head"],
        git(world.clone(), &["rev-parse", "HEAD~1"]).as_str()
    );
    // Nothing left the machine: the build and the fix of the lead stay in the clone.
    for verb in ["git.push", "pr.open"] {
        assert!(
            world
                .ledger()
                .operations_of(KEY, Some(verb))
                .unwrap()
                .is_empty(),
            "no {verb} before the review ends with no commit"
        );
    }
    assert!(world.world_json("prs.json")["prs"]
        .as_array()
        .unwrap()
        .is_empty());

    // The second round: the lane on Codex fails at its limit and falls back to Claude, and the
    // lead makes no commit, so the ticket goes on to open-pr, which pushes and opens the pull
    // request, and then to CI.
    world.tick_until("the ticket reaches ci", LONG, |w| {
        w.step().as_deref() == Some("ci")
    });
    assert!(world.flags().is_empty(), "{:?}", world.flags());
    let moves: Vec<Value> = events(&world, "step")
        .into_iter()
        .filter(|e| e["from"] == "review")
        .collect();
    assert_eq!(moves.len(), 2, "{moves:?}");
    assert_eq!(moves[1]["to"], "open-pr");
    assert_eq!(moves[1]["how"], "on none");
    assert_eq!(moves[1]["way"], "none");
    assert_eq!(moves[1]["head"], head.as_str());
    assert_eq!(moves[1]["entry_head"], head.as_str());
    let ops: Vec<(String, String)> = world
        .ledger()
        .operations_of(KEY, None)
        .unwrap()
        .into_iter()
        .filter(|o| o.verb != "tracker.describe")
        .map(|o| (o.verb, o.state))
        .collect();
    assert_eq!(
        ops,
        [
            ("git.push".to_string(), "confirmed".to_string()),
            ("pr.open".to_string(), "confirmed".to_string())
        ],
        "one push of the last head, then the pull request"
    );
    assert_eq!(world.acts_of("git.push"), 1);
    assert_eq!(
        git(
            &world.world.join("remote.git"),
            &["rev-parse", "refs/heads/vision/DEMO-12"]
        ),
        head,
        "the fix of the lead is on the forge"
    );
    assert_eq!(world.world_json("prs.json")["prs"][0]["number"], 1);
    assert_eq!(world.counts()["entries"]["review-lanes"], 2);
    assert_eq!(step_turns(&world, "review").len(), 2);
    let fallback = events(&world, "fallback");
    assert_eq!(fallback.len(), 1, "{fallback:?}");
    assert_eq!(fallback[0]["agent"], "cr-codex");
    assert_eq!(fallback[0]["to"], "claude-opus-5-5 high");
    let codex_turns: Vec<(String, String)> = step_turns(&world, "review-lanes")
        .into_iter()
        .filter(|t| t.agent.as_deref() == Some("cr-codex"))
        .map(|t| (t.status.unwrap_or_default(), t.model.unwrap_or_default()))
        .collect();
    assert_eq!(
        codex_turns,
        [
            ("done".to_string(), "gpt-6.1-sol".to_string()),
            ("failed".to_string(), "gpt-6.1-sol".to_string()),
            ("done".to_string(), "claude-opus-5-5".to_string()),
        ]
    );

    // The report lists the agents of the adapter with the source adapter.
    let report = world.run(&["report", "--json"]);
    report.expect_code(0);
    let agents = report.json()["agents"].as_array().unwrap().clone();
    let source = |name: &str| {
        agents
            .iter()
            .find(|a| a["agent"] == name)
            .map(|a| a["source"].as_str().unwrap().to_string())
    };
    assert_eq!(source("builder").as_deref(), Some("vision"));
    for lane in LANES {
        assert_eq!(source(lane).as_deref(), Some("adapter"), "{agents:?}");
    }
    let text = world.run(&["report"]).stdout;
    assert!(text.contains("By agent:"), "{text}");
    assert!(
        text.lines()
            .any(|l| l.split_whitespace().take(2).eq(["cr-codex", "adapter"])),
        "{text}"
    );
}

/// The review step with `fixes`: commits that fix only minor findings go on to open-pr.
fn with_minor_route(world: &World) {
    let path = world.vision.join("adapters/demo/adapter.toml");
    let text = std::fs::read_to_string(&path).unwrap();
    let old = "on = { commits = \"review-lanes\", none = \"open-pr\" }";
    assert!(text.contains(old), "the demo review team changed");
    let text = text.replace(
        old,
        "fixes = \"cr-fixed.md\"\non = { commits = \"review-lanes\", minor = \"open-pr\", none = \"open-pr\" }",
    );
    std::fs::write(&path, text).unwrap();
}

/// The turn of the lead that fixes calc.py and commits, with a list of fixed findings.
fn lead_fixes_listed(text: &str, fixed: &str) -> Value {
    runs_then(
        &format!("printf '{text}\\n' >> calc.py && git add calc.py && git commit -q -m 'fix: keep the fraction'"),
        result(
            "Fixed the findings.",
            &[("cr.md", "No open finding."), ("cr-fixed.md", fixed)],
        ),
    )
}

#[test]
fn a_fix_of_minor_findings_only_goes_on_without_a_new_round() {
    let (world, _, _) = team_world();
    with_minor_route(&world);
    world.scenario(
        "reviewer",
        "review",
        json!([lead_fixes_listed(
            "# a nit",
            "## Fixed\n\n1. [MINOR][FOLLOW-UP] calc.py:5\n   The fraction.\n\n2. [NIT][FOLLOW-UP] calc.py:7\n   A name."
        )]),
    );
    take_and_approve(&world);
    world.tick_until("the move after the review", LONG, |w| {
        events(w, "step").iter().any(|e| e["from"] == "review")
    });
    let moves: Vec<Value> = events(&world, "step")
        .into_iter()
        .filter(|e| e["from"] == "review")
        .collect();
    assert_eq!(moves[0]["to"], "open-pr", "{moves:?}");
    assert_eq!(moves[0]["how"], "on minor");
    assert_eq!(moves[0]["way"], "minor");
}

#[test]
fn a_fix_of_a_major_finding_starts_a_new_round_of_review() {
    let (world, _, _) = team_world();
    with_minor_route(&world);
    world.scenario(
        "reviewer",
        "review",
        json!([lead_fixes_listed(
            "# a major fix",
            "## Fixed\n\n1. [MAJOR][FOLLOW-UP] calc.py:5\n   The fraction.\n\n2. [NIT][FOLLOW-UP] calc.py:7\n   A name."
        )]),
    );
    take_and_approve(&world);
    world.tick_until("the move after the review", LONG, |w| {
        events(w, "step").iter().any(|e| e["from"] == "review")
    });
    let moves: Vec<Value> = events(&world, "step")
        .into_iter()
        .filter(|e| e["from"] == "review")
        .collect();
    assert_eq!(moves[0]["to"], "review-lanes", "{moves:?}");
    assert_eq!(moves[0]["way"], "commits");
}

#[test]
fn the_cap_of_the_lanes_stops_the_third_round() {
    let (world, _, _) = team_world();
    world.scenario(
        "reviewer",
        "review",
        json!([
            lead_fixes("# fixed in round 1"),
            lead_fixes("# fixed in round 2")
        ]),
    );
    take_and_approve(&world);
    world.tick_until("the cap of the lanes", LONG, |w| {
        w.flags().iter().any(|f| f == "needs-you")
    });
    assert_eq!(world.step().as_deref(), Some("review-lanes"));
    assert_eq!(world.counts()["entries"]["review-lanes"], 3);
    let reason = world.reason("needs-you");
    assert!(reason.contains("review-lanes"), "{reason}");
    assert!(reason.contains("cap"), "{reason}");
    // The third entry starts no lane.
    assert_eq!(
        step_turns(&world, "review-lanes")
            .iter()
            .filter(|t| t.agent.as_deref() == Some("cr-correctness"))
            .count(),
        2
    );
}

#[test]
fn vision_model_lists_the_agents_of_the_adapter_and_pins_one() {
    let world = World::new();
    write_review_team(&world);
    // The line codex of doctor names the lane of the adapter on Codex.
    let doctor = world.run(&["doctor"]);
    let line = doctor
        .stdout
        .lines()
        .find(|l| l.contains("codex"))
        .unwrap_or_default()
        .to_string();
    assert!(line.contains("for cr-codex"), "{}", doctor.stdout);
    let run = world.run(&["model", "--json"]);
    run.expect_code(0);
    let rows = run.json();
    let row = |name: &str| {
        rows.as_array()
            .unwrap()
            .iter()
            .find(|r| r["agent"] == name)
            .cloned()
            .unwrap_or(Value::Null)
    };
    for lane in LANES {
        assert_eq!(row(lane)["source"], "adapter", "{lane}: {rows}");
        assert_eq!(row(lane)["defined_by"], "adapter", "{lane}");
    }
    assert_eq!(row("builder")["source"], "default");
    assert_eq!(row("builder")["defined_by"], "vision");
    assert_eq!(row("cr-codex")["model"], "gpt-6.1-sol");
    assert_eq!(row("cr-codex")["runner"], "codex");
    assert_eq!(row("cr-codex")["fallback"], "claude-opus-5-5 high");
    let names: Vec<&str> = rows
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["agent"].as_str().unwrap())
        .collect();
    let first_lane = names.iter().position(|n| *n == "cr-codex").unwrap();
    assert!(
        names[..first_lane].contains(&"critic-narrow"),
        "the agents of Vision come first: {names:?}"
    );
    let text = world.run(&["model"]).stdout;
    let at = text
        .lines()
        .position(|l| l.starts_with("cr-codex "))
        .unwrap();
    let lines: Vec<&str> = text.lines().collect();
    assert!(lines[at].contains("gpt-6.1-sol on codex"), "{text}");
    assert!(lines[at].trim_end().ends_with("adapter"), "{text}");
    assert!(
        lines[at + 1].starts_with("  reason of the adapter: A second model reads the diff"),
        "{text}"
    );

    // A pin of the person wins over the values of the adapter.
    let set = world.run(&[
        "model",
        "set",
        "cr-codex",
        "gpt-6.1-sol",
        "--effort",
        "xhigh",
    ]);
    set.expect_code(0);
    assert!(
        set.stdout
            .contains("cr-codex now uses gpt-6.1-sol on codex with the effort xhigh"),
        "{set:?}"
    );
    let config = std::fs::read_to_string(world.vision.join("config.toml")).unwrap();
    assert!(config.contains("[agents.cr-codex]"), "{config}");
    let rows = world.run(&["model", "--json"]).json();
    let codex = rows
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["agent"] == "cr-codex")
        .unwrap()
        .clone();
    assert_eq!(codex["effort"], "xhigh");
    assert!(
        codex["source"]
            .as_str()
            .unwrap()
            .starts_with("yours since "),
        "{codex}"
    );
    assert_eq!(codex["default_effort"], "high");
    let text = world.run(&["model"]).stdout;
    assert!(
        text.contains("  reason of the default of the adapter gpt-6.1-sol high: A second model"),
        "{text}"
    );
    // A model with Claude moves the lane to Claude.
    let set = world.run(&["model", "set", "cr-design", "haiku"]);
    set.expect_code(0);
    // A reset goes back to the values of the adapter.
    let reset = world.run(&["model", "reset", "cr-codex"]);
    reset.expect_code(0);
    assert!(
        reset
            .stdout
            .contains("cr-codex uses the default of the adapter again: gpt-6.1-sol high"),
        "{reset:?}"
    );
    let rows = world.run(&["model", "--json"]).json();
    let codex = rows
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["agent"] == "cr-codex")
        .unwrap()
        .clone();
    assert_eq!(codex["source"], "adapter");
    assert_eq!(codex["effort"], "high");
    let ledger = world.ledger();
    let changes: Vec<(String, String)> = ledger
        .conn()
        .prepare("SELECT effort, source FROM model_changes WHERE agent = 'cr-codex' ORDER BY id")
        .unwrap()
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
        .unwrap()
        .map(|r| r.unwrap())
        .collect();
    assert_eq!(
        changes,
        [
            ("xhigh".to_string(), "set".to_string()),
            ("high".to_string(), "reset".to_string())
        ]
    );
    // An agent that neither Vision nor the adapter knows is refused.
    let unknown = world.run(&["model", "set", "cr-nobody", "opus"]);
    unknown.expect_code(1);
    assert!(
        unknown
            .stderr
            .contains("`cr-nobody` is not an agent of Vision or of the adapter"),
        "{unknown:?}"
    );

    // A missing role file stops the adapter. The agents of Vision still show, and a command
    // for an agent of the adapter names the problem.
    std::fs::remove_file(world.vision.join("adapters/demo/roles/cr-design.md")).unwrap();
    let run = world.run(&["model", "--json"]);
    run.expect_code(0);
    assert!(
        run.stderr
            .contains("[agents.cr-design]: the role file roles/cr-design.md does not exist"),
        "{run:?}"
    );
    assert!(!run.stdout.contains("cr-codex"), "{run:?}");
    let set = world.run(&["model", "set", "cr-codex", "opus"]);
    set.expect_code(1);
    assert!(set.stderr.contains("the adapter does not load"), "{set:?}");
    world
        .run(&["model", "set", "builder", "opus"])
        .expect_code(0);
}
