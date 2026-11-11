//! The whole demo path with real sessions of Claude Code. The test is ignored, and it also does
//! nothing unless `VISION_REAL_CLAUDE=1` is set. Run it by hand:
//!
//! ```sh
//! VISION_REAL_CLAUDE=1 cargo test --test real_e2e -- --ignored --nocapture
//! ```
//!
//! It makes a temporary `VISION_HOME` and a demo world (`adapters/demo/bin/world`), and it uses
//! the `claude` of the `PATH` with the default login of the person (`claude.config_dir = ""`).
//! Each agent is pinned to the model `haiku` with the effort `low`, and `[limits]` of the home
//! is raised to 1.0, so that a full subscription window of the person does not stop the test.
//! It costs some dollars and takes many minutes.
//!
//! It takes `DEMO-12` and ticks until the ticket ends, with real sessions. It plays the person:
//! it answers each card with the choice that the card recommends, and it approves the plan and
//! each operation that waits with the exact command that `vision needs --json` gives. It plays
//! the team through `bin/world`: one review comment, ready, approve, merge. At the end it tests
//! the end state, the merged pull request, the commits of the branch, and the cost in
//! `vision report --json`. A failure prints the end of the log of Vision and of the stream of
//! the last turn, because the temporary home is gone after the test.

mod pipeline_support;

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use pipeline_support::{git, World, KEY};
use serde_json::Value;

/// The longest time that the whole path may take.
const LIMIT: Duration = Duration::from_secs(120 * 60);
/// The pause between two ticks.
const PAUSE: Duration = Duration::from_secs(5);
/// Each agent of Vision. Each one gets the model `haiku` with the effort `low`.
const AGENTS: [&str; 7] = [
    "scout", "builder", "reviewer", "tester", "shepherd", "judge", "curator",
];

fn enabled() -> bool {
    if std::env::var("VISION_REAL_CLAUDE").as_deref() == Ok("1") {
        return true;
    }
    eprintln!("skipped: set VISION_REAL_CLAUDE=1 to run real sessions");
    false
}

/// Runs `vision <args>` with the real home folder of the person and the real `claude`, because
/// the turns that a tick starts need the login of the person. `VISION_HOME` stays the
/// temporary home of the world.
fn real(world: &World, args: &[&str]) -> pipeline_support::Run {
    let home = std::env::var("HOME").expect("HOME is set");
    world.run_with(
        args,
        &[("HOME", home.as_str()), ("VISION_CLAUDE_BIN", "claude")],
    )
}

/// The last `lines` lines of a file, or a note when it cannot be read.
fn tail(path: &Path, lines: usize) -> String {
    match std::fs::read_to_string(path) {
        Ok(text) => {
            let all: Vec<&str> = text.lines().collect();
            all[all.len().saturating_sub(lines)..].join("\n")
        }
        Err(e) => format!("({} cannot be read: {e})", path.display()),
    }
}

/// The folder of the newest turn of any ticket of the home: a turn id starts with its UTC time.
fn last_turn_dir(world: &World) -> Option<PathBuf> {
    let mut dirs = Vec::new();
    for ticket in std::fs::read_dir(world.vision.join("tickets"))
        .ok()?
        .flatten()
    {
        if let Ok(turns) = std::fs::read_dir(ticket.path().join("turns")) {
            dirs.extend(turns.flatten().map(|t| t.path()));
        }
    }
    dirs.into_iter()
        .max_by_key(|d| d.file_name().map(|n| n.to_os_string()))
}

/// The evidence for a failure: the end of the log of Vision, and the end of the stream and the
/// error files of the last turn.
fn diagnose(world: &World) -> String {
    let mut out = format!(
        "--- the row: {:?}\n--- the end of log/vision.log\n{}\n",
        world.row(),
        tail(&world.vision.join("log/vision.log"), 80)
    );
    match last_turn_dir(world) {
        Some(dir) => {
            for (name, lines) in [
                ("turn.json", 20),
                ("stderr.log", 30),
                ("supervisor.log", 20),
                ("stream.jsonl", 40),
            ] {
                out.push_str(&format!(
                    "--- {name} of the last turn {}\n{}\n",
                    dir.display(),
                    tail(&dir.join(name), lines)
                ));
            }
        }
        None => out.push_str("--- no turn ran\n"),
    }
    out
}

fn fail(world: &World, what: &str) -> ! {
    panic!("{what}\n{}", diagnose(world))
}

macro_rules! check {
    ($world:expr, $cond:expr, $($msg:tt)+) => {
        let holds: bool = $cond;
        if !holds {
            fail($world, &format!($($msg)+));
        }
    };
}

/// Runs one command that `vision needs --json` gives, such as `vision approve DEMO-12 git.push
/// <commit>`, or each part of `vision plan DEMO-12, then vision approve-plan DEMO-12 <hash>`.
fn run_need_command(world: &World, command: &str) {
    for part in command.split(", then ") {
        let words: Vec<&str> = part.split_whitespace().collect();
        check!(
            world,
            words.first() == Some(&"vision") && words.len() > 1,
            "`vision needs --json` gave a command that is not one line of vision: {command}"
        );
        eprintln!("the person runs: {part}");
        let run = real(world, &words[1..]);
        check!(world, run.code == 0, "`{part}` failed: {run:#?}");
    }
}

/// Plays the person: each card gets the choice that the Judge proposes or that the card
/// recommends, and each plan and operation that waits gets its approval.
fn play_the_person(world: &World) {
    let run = world.run(&["needs", "--json"]);
    check!(world, run.code == 0, "vision needs failed: {run:#?}");
    let needs = run.json();
    for card in needs["cards"].as_array().cloned().unwrap_or_default() {
        let id = card["id"].as_str().unwrap_or_default().to_string();
        let choice = card["proposal"]["choice"]
            .as_u64()
            .or_else(|| card["recommended"].as_u64())
            .unwrap_or(1);
        eprintln!(
            "the person answers {id} ({}) with the choice {choice}",
            card["question"]
        );
        let run = world.run(&[
            "answer",
            &id,
            "--choice",
            &choice.to_string(),
            "--why",
            "The test of Vision takes the recommendation of the card.",
        ]);
        check!(world, run.code == 0, "vision answer {id} failed: {run:#?}");
    }
    for need in needs["others"].as_array().cloned().unwrap_or_default() {
        if need["ticket"].as_str() != Some(KEY) {
            continue;
        }
        match need["kind"].as_str() {
            Some("plan") | Some("operation") => {
                run_need_command(world, need["command"].as_str().unwrap_or_default())
            }
            // A flag stops the loop with the evidence; the Curator waits for no one here.
            _ => {}
        }
    }
}

#[test]
#[ignore = "runs the real claude: set VISION_REAL_CLAUDE=1 and pass --ignored"]
fn a_real_ticket_goes_through_the_whole_demo_path() {
    if !enabled() {
        return;
    }
    let world = World::new();
    world.edit_config(|doc| {
        doc["claude"]["bin"] = toml_edit::value("claude");
        doc["claude"]["config_dir"] = toml_edit::value("");
        doc["limits"]["five_hour"] = toml_edit::value(1.0);
        doc["limits"]["seven_day"] = toml_edit::value(1.0);
    });
    for agent in AGENTS {
        world
            .run(&["model", "set", agent, "haiku", "--effort", "low"])
            .expect_code(0);
    }
    let remote = world.world.join("remote.git");
    let base = git(&remote, &["rev-parse", "refs/heads/main"]);
    let take = world.run(&["take", KEY]);
    check!(&world, take.code == 0, "vision take failed: {take:#?}");
    eprintln!("the home: {}", world.vision.display());

    let started = Instant::now();
    let mut ready = false;
    let mut merged = false;
    let mut last_step = None;
    loop {
        let run = real(&world, &["tick"]);
        check!(&world, run.code == 0, "the tick failed: {run:#?}");
        if world.counts()["finished"].is_string() {
            break;
        }
        let flags = world.flags();
        check!(
            &world,
            flags.is_empty(),
            "the ticket got the flags {flags:?}: {}",
            world.counts()["reasons"]
        );
        play_the_person(&world);
        let step = world.step();
        if step != last_step {
            eprintln!(
                "the step {} after {} s",
                step.as_deref().unwrap_or("-"),
                started.elapsed().as_secs()
            );
            last_step = step.clone();
        }
        match step.as_deref() {
            // The team: one review comment, and the pull request is ready.
            Some("ready") if !ready => {
                world.world_cmd(&[
                    "comment",
                    world.root(),
                    "1",
                    "Please add a test for negative numbers too.",
                    "--author",
                    "someone",
                ]);
                world.world_cmd(&["ready", world.root(), "1"]);
                ready = true;
            }
            // The team approves and merges.
            Some("ready-to-merge") if !merged => {
                world.world_cmd(&["approve", world.root(), "1", "--author", "someone"]);
                world.world_cmd(&["merge", world.root(), "1"]);
                merged = true;
            }
            _ => {}
        }
        check!(
            &world,
            started.elapsed() < LIMIT,
            "the path did not end in {} minutes; it is at the step {:?}",
            LIMIT.as_secs() / 60,
            step
        );
        std::thread::sleep(PAUSE);
    }
    eprintln!("the path ended after {} s", started.elapsed().as_secs());
    // The merge makes the Curator due. The test ends here: `off` kills each turn that runs.
    let off = world.run(&["off"]);
    check!(&world, off.code == 0, "vision off failed: {off:#?}");

    // The end state.
    let row = world.row().expect("the row of the ticket");
    check!(
        &world,
        row.state.as_deref() == Some("done"),
        "the ticket ended at {:?}",
        row.state
    );
    check!(&world, row.clone.is_none(), "the end frees the clone");
    let steps: Vec<String> = world
        .ledger()
        .spans_of(Some(KEY))
        .unwrap()
        .into_iter()
        .map(|s| s.state)
        .collect();
    eprintln!("the steps: {steps:?}");
    for step in [
        "refine",
        "plan",
        "plan-ready",
        "build",
        "review",
        "ci",
        "ready",
        "comments",
        "ci-review",
        "ready-to-merge",
        "done",
    ] {
        check!(
            &world,
            steps.iter().any(|s| s == step),
            "the path has no step {step}: {steps:?}"
        );
    }

    // The merged pull request, and the commits of the branch on main.
    let prs = world.world_json("prs.json");
    let pr = &prs["prs"][0];
    check!(&world, pr["merged"] == true, "the pull request: {pr:#}");
    check!(
        &world,
        pr["comments"][0]["replies"]
            .as_array()
            .is_some_and(|r| !r.is_empty()),
        "the comment of the team has no reply: {pr:#}"
    );
    let branch = git(&remote, &["rev-parse", "refs/heads/vision/DEMO-12"]);
    let commits: usize = git(
        &remote,
        &["rev-list", "--count", &format!("{base}..{branch}")],
    )
    .parse()
    .unwrap_or(0);
    check!(
        &world,
        commits >= 1,
        "the branch has no commit after the base {base}"
    );
    let on_main = std::process::Command::new("git")
        .arg("-C")
        .arg(&remote)
        .args(["merge-base", "--is-ancestor", &branch, "refs/heads/main"])
        .status()
        .unwrap();
    check!(
        &world,
        on_main.success(),
        "main does not hold the branch {branch}"
    );
    eprintln!(
        "the commits of the branch:\n{}",
        git(
            &remote,
            &["log", "--format=%h %s", &format!("{base}..{branch}")]
        )
    );

    // The cost of the real sessions.
    let report = world.run(&["report", "--json"]);
    check!(
        &world,
        report.code == 0,
        "vision report failed: {report:#?}"
    );
    let report = report.json();
    let cost = report["total"]["cost_usd"].as_f64().unwrap_or(0.0);
    eprintln!("the cost of the path: ${cost:.4}");
    check!(&world, cost > 0.0, "the report has no cost: {report:#}");
    let measures: &Value = &report["tickets"][KEY];
    check!(
        &world,
        measures["cost_usd"].as_f64().unwrap_or(0.0) > 0.0,
        "the ticket has no cost: {report:#}"
    );
}
