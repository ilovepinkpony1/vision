//! Helpers for the tests of the adapter and the executor: a temporary home after `vision init`,
//! a demo world made by `adapters/demo/bin/world`, a clone of its toy project, and a ticket row
//! for `DEMO-12` on the branch `vision/DEMO-12`. Nothing here calls the network, the real
//! `claude`, or the real home of Vision.

// Each test file uses a different part of these helpers.
#![allow(dead_code)]

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::Value;
use toml_edit::DocumentMut;
use vision::ctx::Ctx;
use vision::ledger::Ledger;
use vision::paths::TicketKey;
use vision::tick::{JobRun, OperationRun};

use crate::common::{Home, Run};

pub const KEY: &str = "DEMO-12";
pub const BRANCH: &str = "vision/DEMO-12";
pub const TEST_COMMAND: &str = "python3 -B -m unittest discover -q";

/// The folder of the demo adapter in this repo.
pub fn demo_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("adapters/demo")
}

pub fn key() -> TicketKey {
    TicketKey::parse(KEY).unwrap()
}

/// A home, a demo world, a clone on the branch of the ticket, and the ticket row.
pub struct Demo {
    pub home: Home,
    pub world: PathBuf,
    pub clone: PathBuf,
}

impl Demo {
    pub fn new() -> Demo {
        let home = Home::init();
        let world = home.user_home.join("world");
        let clone = home.user_home.join("clone");
        let demo = Demo { home, world, clone };
        demo.world_cmd(&["new", demo.world.to_str().unwrap()]);
        demo.world_cmd(&[
            "clone",
            demo.world.to_str().unwrap(),
            demo.clone.to_str().unwrap(),
        ]);
        git(&demo.clone, &["checkout", "--quiet", "-b", BRANCH]);
        let world = demo.world.display().to_string();
        demo.edit_config(|doc| {
            doc["adapter"]["root"] = toml_edit::value(world);
            doc["adapter"]["test_command"] = toml_edit::value(TEST_COMMAND);
        });
        demo.ledger()
            .conn()
            .execute(
                "INSERT INTO tickets (key, state, clone, branch, taken) VALUES (?1, 'build', ?2, ?3, '2026-11-16T14:30:00Z')",
                rusqlite::params![KEY, demo.clone.to_str().unwrap(), BRANCH],
            )
            .unwrap();
        demo
    }

    /// The context of the library API in this home.
    pub fn ctx(&self) -> Ctx {
        Ctx::new(self.home.paths())
    }

    pub fn ledger(&self) -> Ledger {
        Ledger::open(&self.home.path("ledger.sqlite")).unwrap()
    }

    /// Runs `vision <args>` in this home.
    pub fn run(&self, args: &[&str]) -> Run {
        self.home.run(args)
    }

    /// Edits `config.toml`.
    pub fn edit_config(&self, change: impl FnOnce(&mut DocumentMut)) {
        let path = self.home.path("config.toml");
        let mut doc: DocumentMut = std::fs::read_to_string(&path).unwrap().parse().unwrap();
        change(&mut doc);
        std::fs::write(&path, doc.to_string()).unwrap();
    }

    pub fn set_mode(&self, verb: &str, mode: &str) {
        let (verb, mode) = (verb.to_string(), mode.to_string());
        self.edit_config(|doc| doc["modes"][verb.as_str()] = toml_edit::value(mode));
    }

    /// Runs `bin/world` with a temporary HOME, and gives its standard output.
    pub fn world_cmd(&self, args: &[&str]) -> String {
        let output = Command::new(demo_dir().join("bin/world"))
            .args(args)
            .env_clear()
            .env("PATH", std::env::var_os("PATH").unwrap_or_default())
            .env("HOME", &self.home.user_home)
            .output()
            .expect("run bin/world");
        assert!(
            output.status.success(),
            "world {args:?} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8_lossy(&output.stdout).into_owned()
    }

    /// The world argument of `bin/world`.
    pub fn root(&self) -> &str {
        self.world.to_str().unwrap()
    }

    /// Writes files in the clone and commits them. Gives the new commit.
    pub fn commit(&self, files: &[(&str, &str)], message: &str) -> String {
        for (name, text) in files {
            let path = self.clone.join(name);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, text).unwrap();
        }
        git(&self.clone, &["add", "--all"]);
        git(&self.clone, &["commit", "--quiet", "-m", message]);
        self.head()
    }

    /// The change of DEMO-12: subtract with its test.
    pub fn commit_subtract(&self) -> String {
        let calc = std::fs::read_to_string(self.clone.join("calc.py")).unwrap();
        self.commit(
            &[
                (
                    "calc.py",
                    &format!("{calc}\n\ndef subtract(a, b):\n    \"\"\"Gives a minus b.\"\"\"\n    return a - b\n"),
                ),
                (
                    "test_subtract.py",
                    "import unittest\n\nfrom calc import subtract\n\n\nclass SubtractTest(unittest.TestCase):\n    def test_subtract(self):\n        self.assertEqual(subtract(5, 3), 2)\n",
                ),
            ],
            "Add subtract",
        )
    }

    pub fn head(&self) -> String {
        git(&self.clone, &["rev-parse", "HEAD"])
    }

    /// The commit of a branch in the bare repo of the world, or `None`.
    pub fn remote_branch(&self, branch: &str) -> Option<String> {
        let output = Command::new("git")
            .arg("-C")
            .arg(self.world.join("remote.git"))
            .args([
                "rev-parse",
                "--verify",
                "--quiet",
                &format!("refs/heads/{branch}"),
            ])
            .output()
            .unwrap();
        output
            .status
            .success()
            .then(|| String::from_utf8_lossy(&output.stdout).trim().to_string())
    }

    /// A JSON file of the world, such as `prs.json`.
    pub fn world_json(&self, name: &str) -> Value {
        serde_json::from_str(&std::fs::read_to_string(self.world.join(name)).unwrap()).unwrap()
    }

    /// Each call of `bin/act` that changed something, from `act.log` of the world.
    pub fn act_log(&self) -> Vec<Value> {
        match std::fs::read_to_string(self.world.join("act.log")) {
            Ok(text) => text
                .lines()
                .map(|line| serde_json::from_str(line).unwrap())
                .collect(),
            Err(_) => Vec::new(),
        }
    }

    /// The `act.log` entries of one verb.
    pub fn acts_of(&self, verb: &str) -> usize {
        self.act_log().iter().filter(|e| e["verb"] == verb).count()
    }

    pub fn ticket_row(&self) -> vision::ledger::TicketRow {
        self.ledger().ticket(KEY).unwrap().unwrap()
    }

    /// The text of the log of Vision.
    pub fn log_text(&self) -> String {
        std::fs::read_to_string(self.home.path("log/vision.log")).unwrap_or_default()
    }

    /// Each row of `operations` as one text, to search for a secret.
    pub fn ledger_text(&self) -> String {
        let ledger = self.ledger();
        let mut stmt = ledger
            .conn()
            .prepare("SELECT coalesce(args, '') || coalesce(result, '') || coalesce(external_id, '') FROM operations")
            .unwrap();
        let rows: Vec<String> = stmt
            .query_map([], |row| row.get(0))
            .unwrap()
            .map(|r| r.unwrap())
            .collect();
        rows.join("\n")
    }
}

/// An operation of DEMO-12.
pub fn op(verb: &str, args: Value) -> OperationRun {
    OperationRun {
        ticket: key(),
        verb: verb.to_string(),
        args,
        turn: None,
    }
}

/// A job of DEMO-12.
pub fn job(name: &str, args: Value) -> JobRun {
    JobRun {
        ticket: key(),
        step: "build".into(),
        job: name.to_string(),
        args,
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

/// Writes an executable script.
pub fn write_script(path: &Path, text: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, text).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
}
