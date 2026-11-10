//! The adapter jira-github: its `adapter.toml` loads through the loader of the core, and the
//! Python tests of its scripts pass. Those tests run against a fake HTTP server on a free
//! localhost port and against temporary git repos: no test reaches the network.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::Command;

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

#[test]
fn the_adapter_loads_through_the_loader_of_the_core() {
    let adapter = load();
    assert_eq!(adapter.origin, Some(Origin::SourceTree));
    assert_eq!(adapter.name, "jira-github");
    assert_eq!(adapter.dir, adapter_dir());
    assert_eq!(adapter.scripts, adapter_dir());
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
            "ci",
            "ci-rerun",
            "fix-ci",
            "qa",
            "fix-qa",
            "ready",
            "in-review",
            "comments",
            "ci-review",
            "ready-to-merge",
            "done"
        ]
    );
    assert_eq!(adapter.entry().id, "refine");
    assert_eq!(adapter.end().id, "done");
    for script in ["observe", "act", "job", "app-token"] {
        let path = adapter.script_path(script);
        let mode = std::os::unix::fs::PermissionsExt::mode(&path.metadata().unwrap().permissions());
        assert!(mode & 0o111 != 0, "{} is executable", path.display());
    }
}

#[test]
fn each_verb_starts_with_its_mode_and_the_reads_have_none() {
    let adapter = load();
    for (verb, mode) in [
        ("tracker.describe", Mode::Auto),
        ("git.push", Mode::Auto),
        ("pr.open", Mode::Auto),
        ("ci.run", Mode::Auto),
        ("ci.rerun", Mode::Auto),
        ("tracker.move", Mode::Ask),
        ("deploy", Mode::Ask),
        ("review.reply", Mode::Ask),
        ("review.rerequest", Mode::Ask),
    ] {
        let spec = &adapter.verbs[verb];
        assert_eq!(spec.mode, mode, "{verb}");
        assert!(!spec.read, "{verb}");
    }
    for verb in ["tracker.describe", "git.push", "pr.open", "review.reply"] {
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
    // The commit of the comments step goes through the CI wait before the merge.
    let comments = step("comments");
    assert_eq!(comments.next.as_deref(), Some("ci-review"));
    let after: Vec<String> = comments.after.iter().map(|e| e.label()).collect();
    assert_eq!(after, ["git.push", "review.reply", "review.rerequest"]);
    assert_eq!(comments.after[1].args["verdicts"], "{doc:verdicts.json}");
    let ci_review = step("ci-review");
    assert_eq!(ci_review.start[0].name, "ci.run");
    assert_eq!(ci_review.on["green"], "in-review");
    assert_eq!(ci_review.on["red"], "fix-ci");
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
    assert_eq!(build.after[1].args["text"], "{doc:pr.md}");
    let StepKind::Turn(review) = &step("review").kind else {
        panic!("review is a turn step")
    };
    assert_eq!(review.findings.as_deref(), Some("review"));
    let qa = step("qa");
    let StepKind::Turn(turn) = &qa.kind else {
        panic!("qa is a turn step")
    };
    assert_eq!(turn.findings.as_deref(), Some("qa"));
    assert_eq!(turn.turn, TurnKind::Tester);
    assert_eq!(turn.with["env"], "local");
    assert_eq!(qa.before[0].kind, EntryKind::Job);
    assert_eq!(qa.before[0].name, "app-start");
    // QA moves on its findings; a fix goes through CI, and then through QA again.
    assert_eq!(qa.next, None);
    assert_eq!(qa.on["findings"], "fix-qa");
    assert_eq!(qa.on["none"], "ready");
    assert_eq!(step("fix-qa").next.as_deref(), Some("ci"));
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
    for id in ["build", "review", "fix-ci"] {
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

#[test]
fn the_python_tests_of_the_adapter_pass() {
    let dir = adapter_dir();
    let home = tempfile::tempdir().unwrap();
    // A plain environment: no token of this machine can reach the tests.
    let mut command = Command::new("python3");
    command
        .args([
            "-B", "-m", "unittest", "discover", "-s", "tests", "-t", "tests",
        ])
        .current_dir(&dir)
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("HOME", home.path())
        .env("LANG", "C.UTF-8")
        .env("PYTHONDONTWRITEBYTECODE", "1");
    if let Some(tmp) = std::env::var_os("TMPDIR") {
        command.env("TMPDIR", tmp);
    }
    let output = command.output().expect("run python3");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "the Python tests of adapters/jira-github failed:\n{}\n{stderr}",
        String::from_utf8_lossy(&output.stdout)
    );
    assert!(stderr.contains("OK"), "{stderr}");
}
