//! The adapter jira-github: its `adapter.toml` loads through the loader of the core, and the
//! built-in adapter (`vision adapter-script jira-github`) passes the tests of each verb, read
//! verb, job, and observe call. The tests run against a fake HTTP server on a free localhost
//! port and against temporary git repos: no test reaches the network, and no test needs Python.

mod jira_github_support;

#[path = "adapter_jira_github/act.rs"]
mod act;
#[path = "adapter_jira_github/adf.rs"]
mod adf;
#[path = "adapter_jira_github/app_token.rs"]
mod app_token;
#[path = "adapter_jira_github/builtin.rs"]
mod builtin;
#[path = "adapter_jira_github/fixtures.rs"]
mod fixtures;
#[path = "adapter_jira_github/jobs.rs"]
mod jobs;
#[path = "adapter_jira_github/observe.rs"]
mod observe;
#[path = "adapter_jira_github/precheck.rs"]
mod precheck;
#[path = "adapter_jira_github/world.rs"]
mod world;

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use vision::adapter::{self, EntryKind, Expect, Origin, StepKind};
use vision::config::{Config, Mode};
use vision::paths::{Paths, TicketKey};
use vision::turn::TurnKind;

fn adapter_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("adapters/jira-github")
}

fn load() -> adapter::Adapter {
    let home = tempfile::tempdir().unwrap();
    let paths = Paths::new(home.path().join("vision"));
    let config = Config::parse("project = \"jira-github\"\n").unwrap();
    adapter::load_with(&config, &paths).unwrap()
}

/// The app of the tests of app-start: a small HTTP server on the port of
/// `VISION_TEST_APP_PORT`, which prints one line when it listens and `PORT-TAKEN` when its port
/// is taken. The test binary runs it as `<test binary> test_app --exact --ignored`. With no
/// port it does nothing.
#[test]
#[ignore]
fn test_app() {
    jobs::serve_test_app();
}

#[test]
fn the_adapter_loads_through_the_loader_of_the_core() {
    let adapter = load();
    assert_eq!(adapter.origin, Some(Origin::SourceTree));
    assert_eq!(adapter.name, "jira-github");
    assert_eq!(adapter.dir, adapter_dir());
    assert_eq!(adapter.scripts, adapter_dir());
    assert_eq!(adapter.builtin.as_deref(), Some("jira-github"));
    let key = TicketKey::parse("DEMO-12").unwrap();
    assert_eq!(adapter.branch_for(&key), "vision/DEMO-12");
    let ids: Vec<&str> = adapter.steps.iter().map(|s| s.id.as_str()).collect();
    assert_eq!(
        ids,
        [
            "refine",
            "plan",
            "plan-ready",
            "build",
            "review",
            "qa",
            "fix-qa",
            "open-pr",
            "ci",
            "ci-rerun",
            "fix-ci",
            "ready",
            "in-review",
            "comments",
            "ci-review",
            "fix-ci-review",
            "rerequest",
            "ready-to-merge",
            "done"
        ]
    );
    assert_eq!(adapter.entry().id, "refine");
    assert_eq!(adapter.end().id, "done");
    // The binary answers the calls: the folder holds only the documented template.
    let files: Vec<String> = std::fs::read_dir(adapter_dir())
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(files, ["adapter.toml"]);
}

#[test]
fn each_verb_starts_with_its_mode_and_the_reads_have_none() {
    let adapter = load();
    // Each write verb starts in the mode auto: no write waits for an approval.
    for verb in [
        "tracker.describe",
        "git.push",
        "pr.open",
        "ci.run",
        "ci.rerun",
        "tracker.move",
        "deploy",
        "deploy.merge",
        "review.reply",
        "review.rerequest",
        "review.resolve",
        "review.minimize",
    ] {
        let spec = &adapter.verbs[verb];
        assert_eq!(spec.mode, Mode::Auto, "{verb}");
        assert!(!spec.read, "{verb}");
    }
    let writes: BTreeSet<&str> = adapter
        .verbs
        .values()
        .filter(|v| !v.read)
        .map(|v| v.name.as_str())
        .collect();
    assert_eq!(writes.len(), 12, "{writes:?}");
    for verb in ["review.resolve", "review.minimize"] {
        assert!(
            !adapter.verbs[verb].leaves,
            "{verb} sends no text out, so no gate reads it"
        );
    }
    for verb in [
        "tracker.describe",
        "git.push",
        "pr.open",
        "review.reply",
        "deploy.merge",
    ] {
        assert!(adapter.verbs[verb].leaves, "{verb} sends text out");
    }
    for verb in ["jira.issue", "jira.search", "github.pr", "ci.log"] {
        assert!(adapter.verbs[verb].read, "{verb}");
        let about = adapter.verbs[verb].about.as_deref().unwrap_or("");
        assert!(
            about.contains("args:"),
            "{verb} says what it gives: {about}"
        );
    }
    assert!(
        adapter.verbs["tracker.move"].once,
        "one move for each status, whatever the commit"
    );
    let timeout = adapter.verbs["ci.run"].timeout.unwrap();
    assert!(
        timeout >= 60,
        "ci.run waits for the run well below its limit"
    );
}

#[test]
fn only_the_jobs_that_read_the_forge_get_the_secrets() {
    let adapter = load();
    let names: BTreeSet<&str> = adapter.jobs.keys().map(String::as_str).collect();
    assert_eq!(
        names,
        BTreeSet::from(["app-start", "ci-log", "comments", "proof", "second-review"])
    );
    for job in ["ci-log", "comments"] {
        assert!(adapter.jobs[job].secrets, "{job}");
    }
    for job in ["proof", "app-start", "second-review"] {
        assert!(!adapter.jobs[job].secrets, "{job} runs code of the project");
    }
}

#[test]
fn the_path_goes_through_ci_again_after_the_comments_and_each_loop_has_a_cap() {
    let adapter = load();
    let step = |id: &str| adapter.step(id).unwrap();
    // Each turn step that a loop can enter again has a cap.
    for (id, cap) in [
        ("review", "review_rounds"),
        ("fix-ci", "ci_fix"),
        ("fix-ci-review", "ci_fix"),
        ("qa", "qa_fix"),
        ("fix-qa", "qa_fix"),
        ("comments", "comments"),
    ] {
        assert_eq!(step(id).cap.as_deref(), Some(cap), "{id}");
    }
    // Each cycle of the step graph passes a turn step with a cap.
    let capped: BTreeSet<&str> = adapter
        .steps
        .iter()
        .filter(|s| matches!(s.kind, StepKind::Turn(_)) && s.cap.is_some())
        .map(|s| s.id.as_str())
        .collect();
    for start in &adapter.steps {
        let mut seen = BTreeSet::new();
        let mut todo = vec![start.id.as_str()];
        while let Some(id) = todo.pop() {
            let s = step(id);
            let targets: Vec<&str> = s
                .next
                .iter()
                .map(String::as_str)
                .chain(s.on.values().map(String::as_str))
                .collect();
            for target in targets {
                if capped.contains(target) {
                    continue;
                }
                assert_ne!(
                    target, start.id,
                    "the step {} is in a loop with no cap",
                    start.id
                );
                if seen.insert(target) {
                    todo.push(target);
                }
            }
        }
    }
    // The commit of the comments step goes through the CI wait before the merge. The round
    // pushes and replies, and it asks for the review again only after CI is green.
    let comments = step("comments");
    assert_eq!(comments.next.as_deref(), Some("ci-review"));
    let after: Vec<String> = comments.after.iter().map(|e| e.label()).collect();
    assert_eq!(after, ["git.push", "review.reply"]);
    assert_eq!(comments.after[1].args["verdicts"], "{doc:verdicts.json}");
    let ci_review = step("ci-review");
    assert_eq!(ci_review.start[0].name, "ci.run");
    assert_eq!(ci_review.on["green"], "rerequest");
    assert_eq!(ci_review.on["red"], "fix-ci-review");
    let rerequest = step("rerequest");
    let StepKind::Act(act) = &rerequest.kind else {
        panic!("rerequest is an act step: {rerequest:?}")
    };
    let run: Vec<String> = act.run.iter().map(|e| e.label()).collect();
    assert_eq!(run, ["review.rerequest"]);
    assert_eq!(rerequest.next.as_deref(), Some("in-review"));
    // A CI fix in code review is a turn like fix-ci, and its commit goes to CI again.
    let (fix, fix_ci) = (step("fix-ci-review"), step("fix-ci"));
    let (StepKind::Turn(turn), StepKind::Turn(fix_ci_turn)) = (&fix.kind, &fix_ci.kind) else {
        panic!("fix-ci-review and fix-ci are turn steps")
    };
    assert_eq!(turn.agent, "builder");
    assert_eq!(turn.turn, TurnKind::Worker);
    assert_eq!(turn.reads, fix_ci_turn.reads);
    assert_eq!(fix.before, fix_ci.before);
    assert_eq!(fix.expects, fix_ci.expects);
    assert_eq!(fix.expect_args, fix_ci.expect_args);
    assert_eq!(fix.after, fix_ci.after);
    assert_eq!(fix.next.as_deref(), Some("ci-review"));
    // From the comments on, no step goes back to the review, the QA, open-pr, or the CI of
    // the draft: each step that the ticket can reach is a step of the code review.
    let mut reached = BTreeSet::from(["comments"]);
    let mut todo = vec!["comments"];
    while let Some(id) = todo.pop() {
        let s = step(id);
        for target in s.next.iter().chain(s.on.values()) {
            if reached.insert(target.as_str()) {
                todo.push(target.as_str());
            }
        }
    }
    assert_eq!(
        reached,
        BTreeSet::from([
            "comments",
            "ci-review",
            "fix-ci-review",
            "rerequest",
            "in-review",
            "ready-to-merge",
            "done"
        ])
    );
    for id in ["in-review", "ready-to-merge"] {
        let StepKind::Wait(wait) = &step(id).kind else {
            panic!("{id} is a wait step")
        };
        assert_eq!(wait.watch, "review.state");
        assert_eq!(step(id).on["merged"], "done");
        assert_eq!(step(id).on["comments"], "comments");
    }
    assert_eq!(step("in-review").on["approved"], "ready-to-merge");
}

#[test]
fn the_steps_name_their_documents_and_their_evidence() {
    let adapter = load();
    let step = |id: &str| adapter.step(id).unwrap();
    let refine = step("refine");
    assert_eq!(refine.after[0].name, "tracker.describe");
    assert_eq!(refine.after[0].args["text"], "{doc:description.md}");
    assert_eq!(refine.after[0].args["seen"], "{doc:issue.json}");
    let build = step("build");
    assert!(build.expects.contains(&Expect::Job("proof".into())));
    // Nothing leaves the machine before the review and the local QA hold: the build, the
    // review, and the fix of QA push nothing, and the act step open-pr comes after QA.
    assert!(build.after.is_empty(), "{:?}", build.after);
    assert_eq!(build.next.as_deref(), Some("review"));
    let StepKind::Turn(review) = &step("review").kind else {
        panic!("review is a turn step")
    };
    assert_eq!(review.findings.as_deref(), Some("review"));
    assert!(step("review").after.is_empty());
    assert_eq!(step("review").next.as_deref(), Some("qa"));
    let qa = step("qa");
    let StepKind::Turn(turn) = &qa.kind else {
        panic!("qa is a turn step")
    };
    assert_eq!(turn.findings.as_deref(), Some("qa"));
    assert_eq!(turn.turn, TurnKind::Tester);
    assert_eq!(turn.with["env"], "local");
    assert_eq!(qa.before[0].kind, EntryKind::Job);
    assert_eq!(qa.before[0].name, "app-start");
    // QA moves on its findings; a fix goes through the review, and then through QA again.
    assert_eq!(qa.next, None);
    assert_eq!(qa.on["findings"], "fix-qa");
    assert_eq!(qa.on["none"], "open-pr");
    let fix = step("fix-qa");
    assert!(
        fix.after.is_empty(),
        "the fix of QA pushes nothing: {:?}",
        fix.after
    );
    assert_eq!(fix.next.as_deref(), Some("review"));
    // The act step pushes the branch and opens the pull request, and CI comes after it.
    let open = step("open-pr");
    let StepKind::Act(act) = &open.kind else {
        panic!("open-pr is an act step: {open:?}")
    };
    let run: Vec<String> = act.run.iter().map(|e| e.label()).collect();
    assert_eq!(run, ["git.push", "pr.open"]);
    assert_eq!(act.run[1].args["text"], "{doc:pr.md}");
    assert_eq!(open.next.as_deref(), Some("ci"));
    for id in ["ci", "ci-rerun"] {
        assert_eq!(step(id).on["green"], "ready", "{id}");
    }
    assert_eq!(step("ci").on["red"], "ci-rerun");
    assert_eq!(step("ci-rerun").on["red"], "fix-ci");
    // The steps after the pull request exists push their own commits.
    for id in ["fix-ci", "comments", "fix-ci-review"] {
        assert_eq!(step(id).after[0].name, "git.push", "{id}");
    }
    // Each wait step names the values that mean "not yet", and has a limit.
    for wait in adapter
        .steps
        .iter()
        .filter(|s| matches!(s.kind, StepKind::Wait(_)))
    {
        let StepKind::Wait(spec) = &wait.kind else {
            unreachable!()
        };
        assert!(spec.wait.is_some(), "{}", wait.id);
        assert!(spec.limit.is_some_and(|l| l >= 600), "{}", wait.id);
    }
    // The proof of each step reads the approved plan.
    for id in ["build", "review", "fix-ci", "fix-ci-review"] {
        assert_eq!(
            step(id).expect_args_of("proof")["plan"],
            "{doc:approved-plan.md}",
            "{id}"
        );
    }
    let StepKind::Wait(ci) = &step("ci").kind else {
        panic!("ci is a wait step")
    };
    assert_eq!(ci.watch, "ci.state");
    let StepKind::Wait(ready) = &step("ready").kind else {
        panic!("ready is a wait step")
    };
    assert_eq!(ready.watch, "pr.draft");
    assert_eq!(step("ready").on["false"], "in-review");
}
