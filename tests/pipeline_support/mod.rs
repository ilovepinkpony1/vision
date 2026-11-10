//! Helpers for the tests of the pipeline: a temporary home after `vision init`, a demo world
//! made by `adapters/demo/bin/world`, clones of its toy project, a private copy of the demo
//! adapter with short `every` values, and the fake `claude` of `tests/bin/fake-claude`. Each
//! turn that the tick starts plays `claude/fake-scenarios/<agent>-<step>.json` of the home.
//! Nothing here calls the network, the real `claude`, or the real home of Vision.

// Each test file uses a different part of these helpers.
#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use tempfile::TempDir;
use toml_edit::DocumentMut;
use vision::ctx::Ctx;
use vision::ledger::{Ledger, TicketRow};
use vision::paths::{Paths, TicketKey};

pub const KEY: &str = "DEMO-12";
pub const BRANCH: &str = "vision/DEMO-12";
pub const TEST_COMMAND: &str = "python3 -B -m unittest discover -q";

/// The result of one run of the binary.
#[derive(Debug)]
pub struct Run {
    pub code: i32,
    pub stdout: String,
    pub stderr: String,
}

impl Run {
    pub fn expect_code(&self, code: i32) -> &Run {
        assert_eq!(self.code, code, "unexpected exit code: {self:#?}");
        self
    }

    pub fn json(&self) -> Value {
        serde_json::from_str(&self.stdout).unwrap_or_else(|e| panic!("not JSON ({e}): {self:#?}"))
    }
}

/// The fake Claude Code binary.
pub fn fake_claude() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/bin/fake-claude")
}

/// The folder of the demo adapter in this repo.
pub fn demo_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("adapters/demo")
}

pub fn key() -> TicketKey {
    TicketKey::parse(KEY).unwrap()
}

/// A home, a demo world, and clones.
pub struct World {
    _dir: TempDir,
    pub vision: PathBuf,
    pub user_home: PathBuf,
    pub world: PathBuf,
    pub clones: Vec<PathBuf>,
}

impl World {
    /// A home with one clone.
    pub fn new() -> World {
        World::with_clones(1)
    }

    /// A home with `n` clones in `clones` of the configuration.
    pub fn with_clones(n: usize) -> World {
        if let Some(bin) = std::env::var_os("VISION_CLAUDE_BIN") {
            assert_eq!(
                PathBuf::from(&bin),
                fake_claude(),
                "unset VISION_CLAUDE_BIN to run these tests: a test never runs the real claude"
            );
        }
        let dir = tempfile::tempdir().expect("make a temporary folder");
        let root = dir.path().to_path_buf();
        let world = World {
            vision: root.join("vision"),
            user_home: root.join("user"),
            world: root.join("world"),
            clones: (1..=n).map(|i| root.join(format!("clone-{i}"))).collect(),
            _dir: dir,
        };
        std::fs::create_dir_all(&world.user_home).unwrap();
        world.run(&["init"]).expect_code(0);
        world.world_cmd(&["new", world.root()]);
        for clone in &world.clones {
            world.world_cmd(&["clone", world.root(), clone.to_str().unwrap()]);
        }
        let clones: Vec<String> = world
            .clones
            .iter()
            .map(|c| c.display().to_string())
            .collect();
        let root_text = world.world.display().to_string();
        world.edit_config(|doc| {
            doc["claude"]["bin"] = toml_edit::value(fake_claude().display().to_string());
            let mut list = toml_edit::Array::new();
            for clone in &clones {
                list.push(clone.as_str());
            }
            doc["clones"] = toml_edit::value(list);
            doc["adapter"]["root"] = toml_edit::value(root_text);
            doc["adapter"]["test_command"] = toml_edit::value(TEST_COMMAND);
        });
        world.write_adapter(|text| text);
        world
    }

    /// Writes the private copy of the demo adapter in the home: the steps of the repo with
    /// `every = 1`, changed by `change`, and the scripts of the repo.
    pub fn write_adapter(&self, change: impl FnOnce(String) -> String) {
        let text = std::fs::read_to_string(demo_dir().join("adapter.toml")).unwrap();
        let text = text.replace("every = 60", "every = 1").replacen(
            "base = \"origin/main\"",
            &format!(
                "base = \"origin/main\"\nscripts = {}",
                serde_json::to_string(&demo_dir().display().to_string()).unwrap()
            ),
            1,
        );
        let dir = self.vision.join("adapters/demo");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("adapter.toml"), change(text)).unwrap();
    }

    pub fn root(&self) -> &str {
        self.world.to_str().unwrap()
    }

    pub fn clone(&self) -> &Path {
        &self.clones[0]
    }

    pub fn ctx(&self) -> Ctx {
        Ctx::new(Paths::new(&self.vision))
    }

    pub fn ledger(&self) -> Ledger {
        Ledger::open(&self.vision.join("ledger.sqlite")).unwrap()
    }

    pub fn edit_config(&self, change: impl FnOnce(&mut DocumentMut)) {
        let path = self.vision.join("config.toml");
        let mut doc: DocumentMut = std::fs::read_to_string(&path).unwrap().parse().unwrap();
        change(&mut doc);
        std::fs::write(&path, doc.to_string()).unwrap();
    }

    /// Runs `vision <args>` in this home.
    pub fn run(&self, args: &[&str]) -> Run {
        self.run_with(args, &[])
    }

    pub fn run_with(&self, args: &[&str], env: &[(&str, &str)]) -> Run {
        let mut command = Command::new(env!("CARGO_BIN_EXE_vision"));
        command
            .args(args)
            .env_clear()
            .env("PATH", std::env::var_os("PATH").unwrap_or_default())
            .env("HOME", &self.user_home)
            .env("VISION_HOME", &self.vision)
            .env("VISION_CLAUDE_BIN", fake_claude())
            .stdin(Stdio::null());
        if let Some(tmp) = std::env::var_os("TMPDIR") {
            command.env("TMPDIR", tmp);
        }
        for (name, value) in env {
            command.env(name, value);
        }
        let output = command.output().expect("run the vision binary");
        Run {
            code: output.status.code().unwrap_or(-1),
            stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        }
    }

    /// One tick, which must end with code 0.
    pub fn tick(&self) -> Run {
        let run = self.run(&["tick"]);
        run.expect_code(0);
        run
    }

    /// Ticks until `done` holds, with a short pause between two ticks. Panics with the log of
    /// Vision after `limit`.
    pub fn tick_until(&self, what: &str, limit: Duration, mut done: impl FnMut(&World) -> bool) {
        let until = Instant::now() + limit;
        loop {
            self.tick();
            if done(self) {
                return;
            }
            if Instant::now() >= until {
                panic!(
                    "timed out waiting for {what}. The row: {:?}\nThe log:\n{}",
                    self.row(),
                    self.log_text()
                );
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    }

    /// Ticks until the ticket is at `step`.
    pub fn tick_to(&self, step: &str) {
        self.tick_until(&format!("the step {step}"), Duration::from_secs(60), |w| {
            w.step().as_deref() == Some(step)
        });
    }

    /// Writes the scenario of the turns of one agent at one step.
    pub fn scenario(&self, agent: &str, step: &str, turns: Value) {
        let dir = self.vision.join("claude/fake-scenarios");
        std::fs::create_dir_all(&dir).unwrap();
        let name = if step.is_empty() {
            format!("{agent}.json")
        } else {
            format!("{agent}-{step}.json")
        };
        std::fs::write(dir.join(name), json!({ "turns": turns }).to_string()).unwrap();
    }

    /// How many runs of the fake session took a list of this scenario. The fake makes one
    /// marker file for each run at its start.
    pub fn scenario_runs(&self, agent: &str, step: &str) -> usize {
        let dir = self.vision.join("claude/fake-scenarios");
        let prefix = if step.is_empty() {
            format!("{agent}.json.run")
        } else {
            format!("{agent}-{step}.json.run")
        };
        std::fs::read_dir(dir)
            .map(|entries| {
                entries
                    .flatten()
                    .filter(|e| e.file_name().to_string_lossy().starts_with(&prefix))
                    .count()
            })
            .unwrap_or(0)
    }

    /// Runs `bin/world` and gives its standard output.
    pub fn world_cmd(&self, args: &[&str]) -> String {
        let output = Command::new(demo_dir().join("bin/world"))
            .args(args)
            .env_clear()
            .env("PATH", std::env::var_os("PATH").unwrap_or_default())
            .env("HOME", &self.user_home)
            .output()
            .expect("run bin/world");
        assert!(
            output.status.success(),
            "world {args:?} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8_lossy(&output.stdout).into_owned()
    }

    /// A JSON file of the world, such as `prs.json`.
    pub fn world_json(&self, name: &str) -> Value {
        serde_json::from_str(&std::fs::read_to_string(self.world.join(name)).unwrap()).unwrap()
    }

    /// The `act.log` entries of one verb.
    pub fn acts_of(&self, verb: &str) -> usize {
        match std::fs::read_to_string(self.world.join("act.log")) {
            Ok(text) => text
                .lines()
                .filter_map(|l| serde_json::from_str::<Value>(l).ok())
                .filter(|e| e["verb"] == verb)
                .count(),
            Err(_) => 0,
        }
    }

    pub fn row(&self) -> Option<TicketRow> {
        self.ledger().ticket(KEY).unwrap()
    }

    pub fn step(&self) -> Option<String> {
        self.row().and_then(|r| r.state)
    }

    pub fn flags(&self) -> Vec<String> {
        self.ledger().ticket_flags(KEY).unwrap()
    }

    /// The counters of the row as JSON.
    pub fn counts(&self) -> Value {
        self.row()
            .and_then(|r| r.counts)
            .and_then(|c| serde_json::from_str(&c).ok())
            .unwrap_or(Value::Null)
    }

    /// The reason of a flag.
    pub fn reason(&self, flag: &str) -> String {
        self.counts()["reasons"][flag]["reason"]
            .as_str()
            .unwrap_or("")
            .to_string()
    }

    /// The text of a document of the ticket folder, or `None`.
    pub fn doc(&self, name: &str) -> Option<String> {
        std::fs::read_to_string(self.vision.join("tickets").join(KEY).join(name)).ok()
    }

    pub fn log_text(&self) -> String {
        std::fs::read_to_string(self.vision.join("log/vision.log")).unwrap_or_default()
    }

    /// The turns of the ticket: (step, agent, status, end_fact), oldest first.
    pub fn turns(&self) -> Vec<(String, String, String, Option<String>)> {
        self.ledger()
            .turns_of_ticket(KEY)
            .unwrap()
            .into_iter()
            .map(|t| {
                (
                    t.step.unwrap_or_default(),
                    t.agent.unwrap_or_default(),
                    t.status.unwrap_or_default(),
                    t.end_fact,
                )
            })
            .collect()
    }

    /// The number of turns of one step of the ticket.
    pub fn turns_of(&self, step: &str) -> usize {
        self.turns().iter().filter(|t| t.0 == step).count()
    }

    /// Waits until no turn of the ticket runs, then ticks once to collect it.
    pub fn settle(&self) {
        let until = Instant::now() + Duration::from_secs(30);
        loop {
            let running = self
                .ledger()
                .turns_of_ticket(KEY)
                .unwrap()
                .iter()
                .any(|t| t.status.as_deref() == Some("running"));
            if !running {
                return;
            }
            assert!(
                Instant::now() < until,
                "a turn never ended:\n{}",
                self.log_text()
            );
            std::thread::sleep(Duration::from_millis(100));
            self.tick();
        }
    }

    /// The kinds of the events of the ticket, oldest first.
    pub fn event_kinds(&self) -> Vec<String> {
        self.ledger()
            .events_of_ticket(KEY)
            .unwrap()
            .into_iter()
            .map(|e| e.kind)
            .collect()
    }

    /// Writes files in a clone and commits them there. Gives the new commit.
    pub fn commit_in(&self, clone: &Path, files: &[(&str, &str)], message: &str) -> String {
        for (name, text) in files {
            let path = clone.join(name);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, text).unwrap();
        }
        git(clone, &["add", "--all"]);
        git(clone, &["commit", "--quiet", "-m", message]);
        git(clone, &["rev-parse", "HEAD"])
    }
}

/// Runs git in `dir` with no signing and no hooks, and gives its standard output.
pub fn git(dir: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args([
            "-c",
            "commit.gpgsign=false",
            "-c",
            "core.hooksPath=/dev/null",
            "-c",
            "user.name=demo",
            "-c",
            "user.email=demo@example.invalid",
        ])
        .args(args)
        .output()
        .expect("run git");
    assert!(
        output.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout)
        .trim_end()
        .to_string()
}

// ----- Scenarios -------------------------------------------------------------------------

/// The steps of one turn that ends with a result object.
pub fn ends_with(object: Value) -> Value {
    json!([{"do": "init"}, {"do": "result", "structured_output": object, "cost": 0.25}])
}

/// The steps of one turn that runs a shell command in the clone, then ends with a result.
pub fn runs_then(command: &str, object: Value) -> Value {
    json!([
        {"do": "init"},
        {"do": "run", "command": command},
        {"do": "result", "structured_output": object, "cost": 0.5}
    ])
}

/// A result object with documents.
pub fn result(summary: &str, docs: &[(&str, &str)]) -> Value {
    let documents: Vec<Value> = docs
        .iter()
        .map(|(n, c)| json!({"name": n, "content": c}))
        .collect();
    json!({"kind": "result", "summary": summary, "documents": documents})
}

/// The shell command of the build of DEMO-12: subtract, its test, and a ci.sh that fails while
/// TODO.txt exists, so that the first CI run is red and the proof still passes.
pub const BUILD_COMMAND: &str = "printf '\\n\\ndef subtract(a, b):\\n    \"\"\"Gives a minus b.\"\"\"\\n    return a - b\\n' >> calc.py && printf 'import unittest\\n\\nfrom calc import subtract\\n\\n\\nclass SubtractTest(unittest.TestCase):\\n    def test_subtract(self):\\n        self.assertEqual(subtract(5, 3), 2)\\n' > test_subtract.py && printf 'test ! -f TODO.txt\\n' >> ci.sh && echo 'Name subtract in the README.' > TODO.txt && git add -A && git commit -q -m 'Add subtract'";

/// The plan document of DEMO-12.
pub const PLAN: &str = "# Plan of DEMO-12\n\n## Decisions\n\n- subtract takes the same numbers as add.\n\n## Proof\n\ntest_subtract.py fails on the base and passes after the change.\n";

/// Writes the scenarios of the steps of the demo adapter up to the build: refine with no
/// question, the plan, and the build.
pub fn simple_scenarios(world: &World) {
    world.scenario(
        "scout",
        "refine",
        json!([ends_with(result(
            "Refined.",
            &[
                ("refine.md", "Research."),
                ("description.md", "Add subtract(a, b).")
            ]
        ))]),
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
                &[("pr.md", "Add subtract\n\nThe calculator can subtract.")]
            )
        )]),
    );
}

/// Takes DEMO-12 and drives it to the step `plan-ready`, then approves the plan.
pub fn take_and_approve(world: &World) {
    world.run(&["take", KEY]).expect_code(0);
    world.tick_to("plan-ready");
    let plan = world.run(&["plan", KEY]);
    plan.expect_code(0);
    let hash = plan_hash_of(&plan.stdout);
    world.run(&["approve-plan", KEY, &hash]).expect_code(0);
}

/// The short hash that `vision plan` prints.
pub fn plan_hash_of(stdout: &str) -> String {
    stdout
        .split("hash ")
        .nth(1)
        .and_then(|rest| rest.split_whitespace().next())
        .expect("vision plan prints the hash")
        .to_string()
}
