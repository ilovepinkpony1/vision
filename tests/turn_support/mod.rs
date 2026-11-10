//! Helpers for the tests of turns: a temporary `VISION_HOME`, `HOME`, and working folder, all
//! on paths with spaces, the fake `claude` of `tests/bin/fake-claude`, and waits for files.
//! A test never runs the real `claude` and never touches the real home folder.

// Each test file uses a different part of these helpers.
#![allow(dead_code)]

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use tempfile::TempDir;
use vision::ctx::Ctx;
use vision::ledger::Ledger;
use vision::paths::{Paths, TicketKey, TurnId};
use vision::turn::{self, LaunchRequest, Launched, TurnKind, TurnState};

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
}

/// A home where `vision init` ran, with `claude.bin` set to the fake.
pub struct TestHome {
    _dir: TempDir,
    pub vision: PathBuf,
    pub user_home: PathBuf,
    /// A working folder for the sessions.
    pub work: PathBuf,
    /// A folder for scenario files.
    pub scenarios: PathBuf,
}

/// The fake Claude Code binary.
pub fn fake_claude() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/bin/fake-claude")
}

pub fn key(text: &str) -> TicketKey {
    TicketKey::parse(text).unwrap()
}

impl TestHome {
    pub fn new() -> TestHome {
        if let Some(bin) = std::env::var_os("VISION_CLAUDE_BIN") {
            assert_eq!(
                PathBuf::from(&bin),
                fake_claude(),
                "unset VISION_CLAUDE_BIN to run these tests: a test never runs the real claude"
            );
        }
        turn::set_vision_bin(env!("CARGO_BIN_EXE_vision"));
        let dir = tempfile::tempdir().expect("make a temporary folder");
        let root = dir.path().join("with space");
        let home = TestHome {
            vision: root.join("vision home"),
            user_home: root.join("user home"),
            work: root.join("work dir"),
            scenarios: root.join("scenarios"),
            _dir: dir,
        };
        for path in [&home.user_home, &home.work, &home.scenarios] {
            std::fs::create_dir_all(path).unwrap();
        }
        home.run(&["init"]).expect_code(0);
        home.set_config("claude", "bin", fake_claude().to_str().unwrap());
        home
    }

    /// Sets one string value of the configuration, such as `[claude] bin`.
    pub fn set_config(&self, table: &str, key: &str, value: &str) {
        self.edit_config(|doc| doc[table][key] = toml_edit::value(value));
    }

    /// Changes the configuration with `toml_edit`.
    pub fn edit_config(&self, edit: impl FnOnce(&mut toml_edit::DocumentMut)) {
        let path = self.vision.join("config.toml");
        let mut doc: toml_edit::DocumentMut =
            std::fs::read_to_string(&path).unwrap().parse().unwrap();
        edit(&mut doc);
        std::fs::write(&path, doc.to_string()).unwrap();
    }

    /// A context for the library API.
    pub fn ctx(&self) -> Ctx {
        Ctx::new(Paths::new(&self.vision))
    }

    pub fn ledger(&self) -> Ledger {
        Ledger::open(&self.vision.join("ledger.sqlite")).unwrap()
    }

    /// Runs `vision <args>` with the fake as `VISION_CLAUDE_BIN`.
    pub fn run(&self, args: &[&str]) -> Run {
        self.run_with(args, &[], None)
    }

    pub fn run_with(&self, args: &[&str], env: &[(&str, &str)], stdin: Option<&str>) -> Run {
        let mut command = Command::new(env!("CARGO_BIN_EXE_vision"));
        command
            .args(args)
            .env_clear()
            .env("PATH", std::env::var_os("PATH").unwrap_or_default())
            .env("HOME", &self.user_home)
            .env("VISION_HOME", &self.vision)
            .env("VISION_CLAUDE_BIN", fake_claude())
            .stdin(if stdin.is_some() {
                Stdio::piped()
            } else {
                Stdio::null()
            })
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        if let Some(tmp) = std::env::var_os("TMPDIR") {
            command.env("TMPDIR", tmp);
        }
        for (name, value) in env {
            command.env(name, value);
        }
        let mut child = command.spawn().expect("run the vision binary");
        if let Some(text) = stdin {
            let mut input = child.stdin.take().unwrap();
            input.write_all(text.as_bytes()).unwrap();
        }
        let output = child
            .wait_with_output()
            .expect("wait for the vision binary");
        Run {
            code: output.status.code().unwrap_or(-1),
            stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        }
    }

    /// Writes a scenario file of the fake and gives its path.
    pub fn scenario(&self, name: &str, turns: Value) -> PathBuf {
        let path = self.scenarios.join(format!("{name}.json"));
        std::fs::write(&path, json!({ "turns": turns }).to_string()).unwrap();
        path
    }

    /// A request whose facts name the scenario of the fake.
    pub fn request(
        &self,
        ticket: &str,
        agent: &str,
        step: &str,
        kind: TurnKind,
        scenario: &Path,
    ) -> LaunchRequest {
        let mut request = LaunchRequest::new(key(ticket), agent, step, kind, &self.work);
        request.facts = json!({"fake": {"scenario": scenario}});
        request
    }

    /// Launches a turn through the library.
    pub fn launch(&self, request: &LaunchRequest) -> Launched {
        let ctx = self.ctx();
        let ledger = self.ledger();
        turn::launch(&ctx, &ledger, request).expect("launch the turn")
    }

    /// The `turn.json` of a turn.
    pub fn state(&self, ticket: &str, id: &str) -> Option<TurnState> {
        let id = TurnId::parse(id).unwrap();
        TurnState::read(&self.ctx().paths.turn(&key(ticket), &id).status).unwrap()
    }

    /// Waits until `turn.json` says ended, and gives it.
    pub fn wait_ended(&self, launched: &Launched) -> TurnState {
        let ticket = launched.ticket.to_string();
        let id = launched.id.to_string();
        wait_for(Duration::from_secs(30), "the turn to end", || {
            self.state(&ticket, &id).filter(TurnState::is_ended)
        })
    }

    /// Waits until the stream of a turn holds `needle`.
    pub fn wait_stream(&self, launched: &Launched, needle: &str) -> String {
        let path = launched.dir.join("stream.jsonl");
        wait_for(
            Duration::from_secs(20),
            &format!("{needle:?} in the stream"),
            || {
                std::fs::read_to_string(&path)
                    .ok()
                    .filter(|text| text.contains(needle))
            },
        )
    }

    /// Collects the ended turns now, as tick step 4 does.
    pub fn collect(&self) -> Vec<turn::Collected> {
        let ctx = self.ctx();
        let ledger = self.ledger();
        turn::collect(&ctx, &ledger, vision::util::time::now()).expect("collect")
    }

    /// The text of a file of a turn folder.
    pub fn turn_file(&self, launched: &Launched, name: &str) -> String {
        std::fs::read_to_string(launched.dir.join(name)).unwrap_or_default()
    }

    /// The parsed events of a turn with this `type` and `subtype`.
    pub fn events(&self, launched: &Launched, kind: &str, subtype: Option<&str>) -> Vec<Value> {
        self.turn_file(launched, "stream.jsonl")
            .lines()
            .filter_map(|l| serde_json::from_str::<Value>(l).ok())
            .filter(|v| v["type"] == kind && subtype.is_none_or(|s| v["subtype"] == s))
            .collect()
    }
}

/// Polls `check` every 50 ms until it gives a value, or panics after `limit`.
pub fn wait_for<T>(limit: Duration, what: &str, mut check: impl FnMut() -> Option<T>) -> T {
    let until = Instant::now() + limit;
    loop {
        if let Some(value) = check() {
            return value;
        }
        assert!(Instant::now() < until, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// True when a process with this pid exists and is not a zombie.
pub fn alive(pid: u32) -> bool {
    if !vision::util::process::pid_alive(pid) {
        return false;
    }
    // A zombie still answers signal 0; `ps` shows its state as Z.
    let output = Command::new("ps")
        .args(["-o", "stat=", "-p", &pid.to_string()])
        .output();
    match output {
        Ok(out) => {
            let stat = String::from_utf8_lossy(&out.stdout);
            let stat = stat.trim();
            !stat.is_empty() && !stat.starts_with('Z')
        }
        Err(_) => true,
    }
}

/// Waits until a process is gone.
pub fn wait_gone(pid: u32) {
    wait_for(
        Duration::from_secs(10),
        &format!("process {pid} to end"),
        || (!alive(pid)).then_some(()),
    );
}

/// A result object that fits `agents/turn.schema.json`.
pub fn result_object(summary: &str) -> Value {
    json!({"kind": "result", "summary": summary})
}
