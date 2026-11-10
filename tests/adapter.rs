//! The adapter loader, the demo adapter, `observe`, and the doctor test of the adapter.

mod common;
mod executor_support;

use common::Home;
use executor_support::{demo_dir, key, Demo, BRANCH};
use vision::adapter::{self, EntryKind, Expect, Origin, StepKind};
use vision::config::{Config, Mode};
use vision::executor;
use vision::turn::TurnKind;

#[test]
fn the_demo_adapter_loads_with_the_full_path_of_a_ticket() {
    let home = Home::init();
    let ctx = vision::ctx::Ctx::new(home.paths());
    let adapter = adapter::load(&ctx).unwrap();
    assert_eq!(adapter.origin, Some(Origin::SourceTree));
    assert_eq!(adapter.dir, demo_dir());
    assert_eq!(adapter.scripts, demo_dir());
    let ids: Vec<&str> = adapter.steps.iter().map(|s| s.id.as_str()).collect();
    assert_eq!(
        ids,
        [
            "refine",
            "plan",
            "plan-ready",
            "build",
            "review",
            "ci",
            "fix-ci",
            "ready",
            "comments",
            "ready-to-merge",
            "done"
        ]
    );
    assert_eq!(adapter.entry().id, "refine");
    assert_eq!(adapter.end().id, "done");
    assert_eq!(adapter.branch_for(&key()), BRANCH);
    assert_eq!(adapter.base, "origin/main");
    for verb in [
        "tracker.describe",
        "git.push",
        "pr.open",
        "ci.run",
        "review.reply",
    ] {
        assert!(!adapter.verbs[verb].read, "{verb}");
    }
    assert!(adapter.verbs["tracker.issue"].read);
    assert!(adapter.verbs["git.push"].leaves);
    assert_eq!(adapter.verbs["review.reply"].mode, Mode::Ask);
    for job in ["proof", "second-review"] {
        assert!(adapter.jobs.contains_key(job), "{job}");
    }
    let build = adapter.step("build").unwrap();
    let StepKind::Turn(turn) = &build.kind else {
        panic!("build is a turn step")
    };
    assert_eq!(
        (turn.agent.as_str(), turn.turn),
        ("builder", TurnKind::Worker)
    );
    assert!(build.expects.contains(&Expect::Job("proof".into())));
    let after: Vec<String> = build.after.iter().map(|e| e.label()).collect();
    assert_eq!(after, ["git.push", "pr.open"]);
    let review = adapter.step("review").unwrap();
    assert_eq!(review.before[0].kind, EntryKind::Job);
    assert_eq!(review.cap.as_deref(), Some("review_rounds"));
    let ci = adapter.step("ci").unwrap();
    assert_eq!(ci.on["green"], "ready");
    assert_eq!(ci.on["red"], "fix-ci");
    assert_eq!(ci.start[0].name, "ci.run");
    assert_eq!(adapter.step("ready").unwrap().on["false"], "comments");
}

#[test]
fn a_private_adapter_wins_and_can_use_the_scripts_of_the_demo() {
    let home = Home::init();
    let private = home.path("adapters/demo");
    std::fs::create_dir_all(&private).unwrap();
    let text = std::fs::read_to_string(demo_dir().join("adapter.toml"))
        .unwrap()
        .replacen(
            "base = \"origin/main\"",
            &format!(
                "base = \"origin/main\"\nscripts = \"{}\"",
                demo_dir().display()
            ),
            1,
        )
        .replacen("branch = \"vision/{key}\"", "branch = \"work/{key}\"", 1);
    std::fs::write(private.join("adapter.toml"), text).unwrap();
    let ctx = vision::ctx::Ctx::new(home.paths());
    let adapter = adapter::load(&ctx).unwrap();
    assert_eq!(adapter.origin, Some(Origin::Home));
    assert_eq!(adapter.dir, private);
    assert_eq!(adapter.scripts, demo_dir());
    assert_eq!(adapter.script_path("act"), demo_dir().join("bin/act"));
    assert_eq!(adapter.branch_for(&key()), "work/DEMO-12");
    let run = home.run(&["doctor"]);
    run.expect_code(0);
    assert!(
        run.stdout
            .contains(&format!("ok    adapter    demo from {}", private.display())),
        "{run:?}"
    );

    std::fs::write(
        private.join("adapter.toml"),
        "name = \"demo\"\nscripts = \"no-such-folder\"\n[[step]]\nid = \"done\"\nkind = \"end\"\n",
    )
    .unwrap();
    let err = format!(
        "{:#}",
        adapter::load(&vision::ctx::Ctx::new(home.paths())).unwrap_err()
    );
    assert!(err.contains("the first step is the entry"), "{err}");
}

#[test]
fn adapter_dir_names_a_different_folder() {
    let home = Home::init();
    let elsewhere = home.user_home.join("my adapter");
    std::fs::create_dir_all(&elsewhere).unwrap();
    std::fs::copy(
        demo_dir().join("adapter.toml"),
        elsewhere.join("adapter.toml"),
    )
    .unwrap();
    let config = Config::parse(&format!("[adapter]\ndir = \"{}\"\n", elsewhere.display())).unwrap();
    let adapter = adapter::load_with(&config, &home.paths()).unwrap();
    assert_eq!(adapter.origin, Some(Origin::Configured));
    assert_eq!(adapter.dir, elsewhere);
}

#[test]
fn doctor_tests_that_the_adapter_of_the_project_loads() {
    let home = Home::init();
    let run = home.run(&["doctor"]);
    run.expect_code(0);
    let line = run
        .stdout
        .lines()
        .find(|l| l.contains("adapter"))
        .unwrap_or_else(|| panic!("no adapter line: {run:?}"));
    assert!(line.starts_with("ok"), "{line}");
    assert!(line.contains("demo from"), "{line}");
    assert!(line.contains("11 steps, 6 verbs, 4 jobs"), "{line}");

    let broken = home.user_home.join("broken");
    std::fs::create_dir_all(&broken).unwrap();
    std::fs::write(
        broken.join("adapter.toml"),
        "[[step]]\nid = \"a\"\nkind = \"turn\"\nnext = \"b\"\n",
    )
    .unwrap();
    let config = home.path("config.toml");
    let text = std::fs::read_to_string(&config).unwrap().replacen(
        "[adapter]",
        &format!("[adapter]\ndir = \"{}\"\n", broken.display()),
        1,
    );
    std::fs::write(&config, text).unwrap();
    let run = home.run(&["doctor"]);
    run.expect_code(1);
    assert!(run.stdout.contains("fail  adapter"), "{run:?}");
    assert!(
        run.stdout.contains("step `a`: a turn step names its agent"),
        "{run:?}"
    );
    assert!(
        run.stdout
            .contains("next names the step `b`, which does not exist"),
        "{run:?}"
    );

    let missing = Home::init();
    let config = missing.path("config.toml");
    let text = std::fs::read_to_string(&config).unwrap().replacen(
        "project = \"demo\"",
        "project = \"no-such-adapter\"",
        1,
    );
    std::fs::write(&config, text).unwrap();
    let run = missing.run(&["doctor"]);
    run.expect_code(1);
    assert!(
        run.stdout.contains("no adapter `no-such-adapter`"),
        "{run:?}"
    );
}

#[test]
fn observe_answers_the_queue_and_one_ticket() {
    let demo = Demo::new();
    let ctx = demo.ctx();
    demo.world_cmd(&["status", demo.root(), "DEMO-13", "In Progress"]);
    let queue = executor::observe_queue(&ctx).unwrap();
    assert_eq!(queue.len(), 2);
    assert_eq!(queue[0].key.as_str(), "DEMO-12");
    assert_eq!(queue[0].title, "Add subtract to the calculator");
    assert!(queue[0].eligible);
    assert_eq!(queue[0].reason, "");
    assert!(!queue[1].eligible);
    assert_eq!(queue[1].reason, "the status is In Progress, not To Do");

    let ledger = demo.ledger();
    let facts = executor::observe_ticket(&ctx, &ledger, &key()).unwrap();
    assert_eq!(facts.issue["key"], "DEMO-12");
    assert_eq!(facts.tracker_status.as_deref(), Some("To Do"));
    assert_eq!(facts.pr, None);
    assert_eq!(facts.ci.as_ref().unwrap().state, "none");
    assert_eq!(facts.watch_text("ci.state").as_deref(), Some("none"));
    assert!(facts.envs.is_empty());
    let keys: Vec<&String> = facts.answer.as_object().unwrap().keys().collect();
    assert_eq!(
        keys,
        ["ci", "envs", "issue", "pr", "tracker_status"],
        "the shape of section 11.1"
    );
}

#[test]
fn observe_follows_the_pull_request_and_the_ci() {
    let demo = Demo::new();
    let ctx = demo.ctx();
    let head = demo.commit_subtract();
    let ledger = demo.ledger();
    for verb in ["git.push", "pr.open", "ci.run"] {
        let result = executor::request(
            &ctx,
            &ledger,
            &executor_support::op(verb, serde_json::json!({})),
        )
        .unwrap();
        assert_eq!(
            result.outcome,
            executor::Outcome::Confirmed,
            "{verb}: {result:?}"
        );
    }
    demo.world_cmd(&[
        "comment",
        demo.root(),
        "1",
        "Name the test file after the function",
        "--path",
        "test_subtract.py",
    ]);
    let facts = executor::observe_ticket(&ctx, &ledger, &key()).unwrap();
    let pr = facts.pr.clone().unwrap();
    assert_eq!(pr.number, 1);
    assert!(pr.draft);
    assert_eq!(pr.head.as_deref(), Some(head.as_str()));
    assert!(!pr.merged && !pr.closed && !pr.conflict);
    assert_eq!(pr.comments_waiting, 1);
    assert_eq!(facts.ci.as_ref().unwrap().state, "green");
    assert_eq!(
        facts.ci.as_ref().unwrap().head.as_deref(),
        Some(head.as_str())
    );
    assert_eq!(facts.watch_text("pr.draft").as_deref(), Some("true"));
    assert_eq!(facts.watch_text("pr.number").as_deref(), Some("1"));
    assert_eq!(facts.watch_text("pr.approved_by"), None);
    assert_eq!(facts.watch_text("pr.nothing"), None);

    demo.world_cmd(&["ready", demo.root(), "1"]);
    demo.world_cmd(&["approve", demo.root(), "1"]);
    demo.world_cmd(&["merge", demo.root(), "1"]);
    let facts = executor::observe_ticket(&ctx, &ledger, &key()).unwrap();
    let pr = facts.pr.clone().unwrap();
    assert!(pr.merged && !pr.draft);
    assert_eq!(pr.approved_by, ["reviewer"]);
    assert_eq!(facts.watch_text("pr.merged").as_deref(), Some("true"));
    assert_eq!(facts.tracker_status.as_deref(), Some("Done"));
    assert!(demo.remote_branch("main").is_some());
}

#[test]
fn a_missing_world_is_a_clear_error() {
    let home = Home::init();
    let ctx = vision::ctx::Ctx::new(home.paths());
    let err = format!("{:#}", executor::observe_queue(&ctx).unwrap_err());
    assert!(err.contains("bin/observe failed with exit code 1"), "{err}");
    assert!(err.contains("set root in the [adapter] table"), "{err}");
}
