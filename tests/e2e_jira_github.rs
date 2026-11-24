//! The whole path of one ticket with the adapter for Jira and GitHub (design section 11.3): a
//! temporary home, the fake `claude`, a clone of a toy project, and a fake Jira and GitHub with
//! state (tests/jira_github_support/world.rs) on a free localhost port, in this test process.
//! `vision take DEMO-12` and then `vision tick` in a loop carry the ticket from the tracker to a
//! merged pull request, through each step of the `adapter.toml` of the repo, whose calls the
//! built-in adapter of the binary answers (`scripts = "builtin:jira-github"`). The test plays
//! the person (it approves the plan; each write verb is in the mode auto, so no write waits for
//! an approval) and the team (it marks the pull request ready, reviews, comments, approves, and
//! merges with the control calls of the world). The toy project and its tests are shell
//! scripts, so no part of the path needs Python (the fake `claude` is a separate program).
//!
//! The order: the build, the review, and the local QA with its fix run first, and nothing
//! leaves the machine. Only then the act step open-pr pushes the branch and opens the pull
//! request, and CI runs.
//!
//! A round of comments pushes its fix and replies, and then waits for CI at the new head. Only
//! a green run asks for the review again, in the act step rerequest. A late round after the
//! approval takes the same path, and its red run goes to fix-ci-review and to CI again, never
//! to the review, the QA, or open-pr.
//!
//! Nothing here reaches the network: each address of `[adapter]` points at the fake server, and
//! git pushes to a local bare repo.
//!
//! To look into a run, set `VISION_E2E_DUMP` to a file: the test writes the step, the turns, and
//! the log there before each stage and at the end. A failed stage prints the same report.

mod jira_github_support;
mod pipeline_support;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use jira_github_support::server::call;
use jira_github_support::world::{install, World};
use jira_github_support::FakeServer;
use pipeline_support::{ends_with, fake_claude, git, plan_hash_of, result, runs_then, KEY};
use serde_json::{json, Value};
use tempfile::TempDir;
use toml_edit::DocumentMut;
use vision::ledger::Ledger;

const BRANCH: &str = "vision/DEMO-12";
/// The fake tokens. The world accepts only these, and they must never show up in a file, a
/// URL, or an answer.
const JIRA_TOKEN: &str = "jira-e2e-token-3f9a1c7b5d2e";
const FORGE_TOKEN: &str = "forge-e2e-token-8b4d2f6a9c1e";
const JIRA_USER: &str = "bot@example.invalid";
/// The limit of one stage of the path. It only guards against a hang, so it is far above the
/// time of a stage on a slow machine.
const STAGE: Duration = Duration::from_secs(180);

/// The toy project: shell functions, and tests that are shell scripts (`sh <file>` passes when
/// the script exits with 0).
const TOY_CALC: &str = "# A tiny calculator.\n\n# Gives a plus b.\nadd() { echo $(($1 + $2)); }\n";
const TOY_TEST: &str = ". ./calc.sh\n[ \"$(add 2 3)\" = 5 ]\n";
const TOY_CI: &str = "#!/bin/sh\n# The CI of the toy project: each test must pass.\nset -e\nfor test in test_*.sh; do sh \"$test\"; done\n";

/// The build: subtract with its test, one more test of add (it passes on the base too, so the
/// plan declares it green), and a ci.sh that fails while TODO.txt exists.
const BUILD: &str = "printf '\\n# Gives a minus b.\\nsubtract() { echo $(($1 - $2)); }\\n' >> calc.sh && printf '. ./calc.sh\\n[ \"$(subtract 5 3)\" = 2 ]\\n' > test_subtract.sh && printf '[ \"$(add 0 0)\" = 0 ]\\n' >> test_calc.sh && printf 'test ! -f TODO.txt\\n' >> ci.sh && echo 'Name subtract in the README.' > TODO.txt && git add -A && git commit -q -m 'Add subtract'";

const PLAN: &str = "# Plan of DEMO-12\n\n## Decisions\n\n- subtract goes next to add in calc.sh.\n\n## Proof\n\n- test_subtract.sh fails on the base and passes after the change.\n- **Proof:** green test_calc.sh\n";

/// The fake Jira and GitHub with state, served on a free localhost port by a thread of this
/// test process.
struct Server {
    fake: FakeServer,
    world: Arc<Mutex<World>>,
}

impl Server {
    fn start(remote: &Path) -> Server {
        let mut world = World::new(remote, Some(JIRA_TOKEN), Some(FORGE_TOKEN), Some(JIRA_USER));
        world.add_issue(
            KEY,
            "Add subtract to the calculator",
            "The calculator can only add. Add a function subtract(a, b) that gives a minus b, with a test.",
        );
        let world = Arc::new(Mutex::new(world));
        let fake = FakeServer::start();
        install(&fake, Arc::clone(&world));
        Server { fake, world }
    }

    fn url(&self) -> String {
        self.fake.url.clone()
    }

    /// One HTTP call to the world, for the control calls of the team.
    fn call(&self, method: &str, path: &str, body: Option<Value>) -> (u16, Value) {
        call(self.fake.port, method, path, body.as_ref(), &[])
    }

    /// A control call of the team, which must succeed.
    fn team(&self, action: &str, body: Value) -> Value {
        let (status, answer) = self.call("POST", &format!("/_control/{action}"), Some(body));
        assert!((200..300).contains(&status), "{action}: {status} {answer}");
        answer
    }

    fn state(&self) -> Value {
        self.call("GET", "/_control/state", None).1
    }

    /// Each call of the adapter that the world recorded, oldest first.
    fn requests(&self) -> Vec<Value> {
        self.world.lock().unwrap().log.clone()
    }
}

/// The home, the toy project with its bare remote and its clone, and the world.
struct Project {
    server: Server,
    vision: PathBuf,
    user_home: PathBuf,
    remote: PathBuf,
    clone: PathBuf,
    // The last field: the folder goes after the world has ended.
    _dir: TempDir,
}

impl Project {
    /// The project with a private copy of the adapter of the repo, with short waits.
    fn new() -> Project {
        Project::with_adapter(fast_adapter())
    }

    /// The project with this text as the private adapter.toml of `jira-github`.
    fn with_adapter(adapter: String) -> Project {
        if let Some(bin) = std::env::var_os("VISION_CLAUDE_BIN") {
            assert_eq!(
                PathBuf::from(&bin),
                fake_claude(),
                "unset VISION_CLAUDE_BIN: a test never runs the real claude"
            );
        }
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let vision = root.join("vision");
        let user_home = root.join("user");
        let remote = root.join("remote.git");
        let clone = root.join("clone-1");
        std::fs::create_dir_all(&user_home).unwrap();
        // The toy project on a bare remote, and a clone of it that only Vision uses.
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        for (name, text) in [
            ("calc.sh", TOY_CALC),
            ("test_calc.sh", TOY_TEST),
            ("ci.sh", TOY_CI),
        ] {
            std::fs::write(work.join(name), text).unwrap();
        }
        git(
            root,
            &["init", "--quiet", "--bare", remote.to_str().unwrap()],
        );
        git(&work, &["init", "--quiet"]);
        git(&work, &["add", "--all"]);
        git(
            &work,
            &["commit", "--quiet", "-m", "Start the toy calculator"],
        );
        git(
            &work,
            &[
                "push",
                "--quiet",
                remote.to_str().unwrap(),
                "HEAD:refs/heads/main",
            ],
        );
        git(&remote, &["symbolic-ref", "HEAD", "refs/heads/main"]);
        git(
            root,
            &[
                "clone",
                "--quiet",
                remote.to_str().unwrap(),
                clone.to_str().unwrap(),
            ],
        );
        git(&clone, &["config", "user.name", "vision"]);
        git(&clone, &["config", "user.email", "vision@example.invalid"]);
        let server = Server::start(&remote);
        let path = Project {
            server,
            vision,
            user_home,
            remote,
            clone,
            _dir: dir,
        };
        path.run(&["init"]).expect_code(0);
        path.configure(&adapter);
        path
    }

    /// The configuration: the project, the fake claude, the clone, the `[adapter]` keys that
    /// point at the world, and the secrets from the environment of the `vision` process. And
    /// the private adapter.toml.
    fn configure(&self, adapter_toml: &str) {
        let url = self.server.url();
        let clone = self.clone.display().to_string();
        let remote = self.remote.display().to_string();
        let fake = fake_claude().display().to_string();
        let path = self.vision.join("config.toml");
        let mut doc: DocumentMut = std::fs::read_to_string(&path).unwrap().parse().unwrap();
        doc["project"] = toml_edit::value("jira-github");
        doc["claude"]["bin"] = toml_edit::value(fake);
        let mut clones = toml_edit::Array::new();
        clones.push(clone.as_str());
        doc["clones"] = toml_edit::value(clones);
        let adapter: toml_edit::Table = format!(
            r#"
jira_url = "{url}"
jira_user = "{JIRA_USER}"
queue_jql = "project = DEMO AND status = \"To Do\""
github_repo = "acme/widgets"
github_api = "{url}"
git_remote_url = "{remote}"
ci_workflow = "ci.yml"
ci_find_seconds = 10
ci_poll_seconds = 0.2
approvers = ["lead"]
own_logins = ["vision-bot", "person"]
statuses = {{ in_progress = "In Progress", in_review = "In Review" }}
test_command = "sh"
test_glob = ["test_*.sh"]
app_start_command = "exit 1"
app_health_url = "{url}/health"
http_timeout = 20
"#
        )
        .parse::<DocumentMut>()
        .unwrap()
        .as_table()
        .clone();
        doc["adapter"] = toml_edit::Item::Table(adapter);
        let secrets: toml_edit::Table = "JIRA_TOKEN = { env = \"VISION_E2E_JIRA\" }\nFORGE_TOKEN = { env = \"VISION_E2E_FORGE\" }\n"
            .parse::<DocumentMut>()
            .unwrap()
            .as_table()
            .clone();
        doc["secrets"] = toml_edit::Item::Table(secrets);
        std::fs::write(&path, doc.to_string()).unwrap();

        // A private adapter folder holds only its adapter.toml: the binary answers its calls.
        let dir = self.vision.join("adapters/jira-github");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("adapter.toml"), adapter_toml).unwrap();
    }

    fn run(&self, args: &[&str]) -> pipeline_support::Run {
        let mut command = Command::new(env!("CARGO_BIN_EXE_vision"));
        command
            .args(args)
            .env_clear()
            .env("PATH", std::env::var_os("PATH").unwrap_or_default())
            .env("HOME", &self.user_home)
            .env("VISION_HOME", &self.vision)
            .env("VISION_CLAUDE_BIN", fake_claude())
            .env("VISION_E2E_JIRA", JIRA_TOKEN)
            .env("VISION_E2E_FORGE", FORGE_TOKEN)
            .stdin(Stdio::null());
        if let Some(tmp) = std::env::var_os("TMPDIR") {
            command.env("TMPDIR", tmp);
        }
        let output = command.output().expect("run the vision binary");
        pipeline_support::Run {
            code: output.status.code().unwrap_or(-1),
            stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        }
    }

    fn ledger(&self) -> Ledger {
        Ledger::open(&self.vision.join("ledger.sqlite")).unwrap()
    }

    fn row(&self) -> Option<vision::ledger::TicketRow> {
        self.ledger().ticket(KEY).unwrap()
    }

    fn step(&self) -> Option<String> {
        self.row().and_then(|r| r.state)
    }

    fn flags(&self) -> Vec<String> {
        self.ledger().ticket_flags(KEY).unwrap()
    }

    fn counts(&self) -> Value {
        self.row()
            .and_then(|r| r.counts)
            .and_then(|c| serde_json::from_str(&c).ok())
            .unwrap_or(Value::Null)
    }

    fn doc(&self, name: &str) -> Option<String> {
        std::fs::read_to_string(self.vision.join("tickets").join(KEY).join(name)).ok()
    }

    fn head(&self) -> String {
        git(&self.clone, &["rev-parse", "HEAD"])
    }

    fn needs(&self) -> Vec<Value> {
        let run = self.run(&["status", "--json"]);
        run.expect_code(0);
        run.json()["needs"].as_array().unwrap().clone()
    }

    /// One tick, which must end with code 0, and must not flag the ticket.
    fn tick(&self) {
        let run = self.run(&["tick"]);
        assert_eq!(
            run.code,
            0,
            "the tick failed: {run:#?}\n{}",
            self.log_text()
        );
        let flags = self.flags();
        assert!(
            flags.is_empty(),
            "the ticket got the flags {flags:?}: {}\nThe log:\n{}",
            self.counts()["reasons"],
            self.log_text()
        );
    }

    /// Ticks until the ticket gets the flag `flag`, for a stage where the person is needed.
    fn tick_until_flag(&self, flag: &str) {
        let until = Instant::now() + STAGE;
        loop {
            let run = self.run(&["tick"]);
            assert_eq!(run.code, 0, "the tick failed: {run:#?}");
            if self.flags().iter().any(|f| f == flag) {
                return;
            }
            assert!(
                Instant::now() < until,
                "timed out waiting for the flag {flag}. {}",
                self.report()
            );
            std::thread::sleep(Duration::from_millis(100));
        }
    }

    /// Ticks until `done` holds. Each stage of the path waits for a fact, never for a time.
    fn tick_until(&self, what: &str, mut done: impl FnMut(&Project) -> bool) {
        let until = Instant::now() + STAGE;
        if let Some(out) = std::env::var_os("VISION_E2E_DUMP") {
            let _ = std::fs::write(out, format!("waiting for {what}\n{}", self.report()));
        }
        loop {
            self.tick();
            if done(self) {
                return;
            }
            assert!(
                Instant::now() < until,
                "timed out waiting for {what}. {}",
                self.report()
            );
            std::thread::sleep(Duration::from_millis(100));
        }
    }

    fn tick_to(&self, step: &str) {
        let started = Instant::now();
        self.tick_until(&format!("the step {step}"), |p| {
            p.step().as_deref() == Some(step)
        });
        eprintln!("the step {step} after {:?}", started.elapsed());
    }

    /// The commit of the branch of the ticket on the remote, or `None` before the first push.
    fn remote_branch(&self) -> Option<String> {
        let output = Command::new("git")
            .arg("-C")
            .arg(&self.remote)
            .args([
                "rev-parse",
                "--verify",
                "--quiet",
                &format!("refs/heads/{BRANCH}"),
            ])
            .output()
            .unwrap();
        output
            .status
            .success()
            .then(|| String::from_utf8_lossy(&output.stdout).trim().to_string())
    }

    /// The operations of one verb of the ticket, as (state, commit), oldest first.
    fn operations(&self, verb: &str) -> Vec<(String, String)> {
        self.ledger()
            .operations_of(KEY, Some(verb))
            .unwrap()
            .into_iter()
            .map(|o| {
                let call: Value = serde_json::from_str(&o.args).unwrap();
                (o.state, call["head"].as_str().unwrap_or("").to_string())
            })
            .collect()
    }

    /// The verbs of the operations of the ticket at one commit, oldest first.
    fn verbs_at(&self, head: &str) -> Vec<String> {
        self.ledger()
            .operations_of(KEY, None)
            .unwrap()
            .into_iter()
            .filter(|o| {
                let call: Value = serde_json::from_str(&o.args).unwrap();
                call["head"] == head
            })
            .map(|o| o.verb)
            .collect()
    }

    /// The CI results of the ticket, as (commit, result), oldest first.
    fn ci_checks(&self) -> Vec<(String, String)> {
        self.ledger()
            .checks_of(KEY, None)
            .unwrap()
            .into_iter()
            .filter(|c| c.kind == "ci")
            .map(|c| (c.head, c.result))
            .collect()
    }

    /// The steps of the ticket in the order of its visits, from the spans.
    fn path(&self) -> Vec<String> {
        self.ledger()
            .spans_of(Some(KEY))
            .unwrap()
            .into_iter()
            .map(|s| s.state)
            .collect()
    }

    /// Asserts that nothing left the machine yet: no push, no pull request, and no operation
    /// of git.push or pr.open.
    fn assert_nothing_left(&self, when: &str) {
        assert_eq!(
            self.remote_branch(),
            None,
            "{when}: the branch is on the remote"
        );
        assert_eq!(
            self.server.state()["pulls"],
            json!({}),
            "{when}: a pull request is open"
        );
        for verb in ["git.push", "pr.open"] {
            assert!(
                self.operations(verb).is_empty(),
                "{when}: an operation of {verb}"
            );
        }
        assert!(
            self.needs().iter().all(|n| n["kind"] != "operation"),
            "{when}: a write waits for an approval"
        );
    }

    fn scenario(&self, agent: &str, step: &str, turns: Value) {
        let dir = self.vision.join("claude/fake-scenarios");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join(format!("{agent}-{step}.json")),
            json!({ "turns": turns }).to_string(),
        )
        .unwrap();
    }

    fn log_text(&self) -> String {
        std::fs::read_to_string(self.vision.join("log/vision.log")).unwrap_or_default()
    }

    /// What a failed stage prints: the step, each turn with its status, and the log.
    fn report(&self) -> String {
        let turns: Vec<String> = self
            .ledger()
            .turns_of_ticket(KEY)
            .unwrap()
            .into_iter()
            .map(|t| {
                format!(
                    "{} {:?} {:?} {:?} resumes={:?}",
                    t.id, t.status, t.reason, t.end_fact, t.resumes
                )
            })
            .collect();
        format!(
            "The step: {:?}\nThe turns:\n{}\nThe log:\n{}",
            self.step(),
            turns.join("\n"),
            self.log_text()
        )
    }

    /// The facts file of the last turn of a step.
    fn facts_of_step(&self, step: &str) -> Value {
        let turn = self
            .ledger()
            .turns_of_ticket(KEY)
            .unwrap()
            .into_iter()
            .rfind(|t| t.step.as_deref() == Some(step))
            .expect("a turn of the step");
        let paths = vision::paths::Paths::new(&self.vision);
        let dir = paths.turn(
            &vision::paths::TicketKey::parse(KEY).unwrap(),
            &vision::paths::TurnId::parse(&turn.id).unwrap(),
        );
        serde_json::from_str(&std::fs::read_to_string(dir.facts).unwrap()).unwrap()
    }

    /// The writes of the adapter to the world, as `METHOD path`, with their count.
    fn writes(&self) -> BTreeMap<String, usize> {
        let mut out = BTreeMap::new();
        for r in self.server.requests() {
            let method = r["method"].as_str().unwrap();
            if method != "GET" {
                *out.entry(format!("{method} {}", r["path"].as_str().unwrap()))
                    .or_default() += 1;
            }
        }
        out
    }
}

/// The adapter.toml of the repo with short waits. It keeps `scripts = "builtin:jira-github"`.
fn fast_adapter() -> String {
    let repo = Path::new(env!("CARGO_MANIFEST_DIR")).join("adapters/jira-github");
    let mut text = std::fs::read_to_string(repo.join("adapter.toml")).unwrap();
    for every in ["every = 30", "every = 60", "every = 120"] {
        text = text.replace(every, "every = 1");
    }
    assert!(!text.contains("every = 30") && text.contains("every = 1"));
    assert!(
        text.contains("\nscripts = \"builtin:jira-github\""),
        "the adapter of the repo uses the built-in scripts"
    );
    text
}

/// Each file under `dir`, as bytes, for the search of a token.
fn files_under(dir: &Path, out: &mut Vec<(PathBuf, Vec<u8>)>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(kind) = entry.file_type() else {
            continue;
        };
        if kind.is_dir() {
            files_under(&path, out);
        } else if kind.is_file() {
            if let Ok(bytes) = std::fs::read(&path) {
                out.push((path, bytes));
            }
        }
    }
}

fn holds(bytes: &[u8], text: &str) -> bool {
    bytes.windows(text.len()).any(|w| w == text.as_bytes())
}

fn scenarios(p: &Project) {
    p.scenario(
        "scout",
        "refine",
        json!([ends_with(result(
            "Refined.",
            &[
                ("refine.md", "# Refine\n\nsubtract goes next to add in calc.sh.\n"),
                (
                    "description.md",
                    "Add a function subtract(a, b) to the calculator that gives a minus b, with a test."
                )
            ]
        ))]),
    );
    p.scenario(
        "builder",
        "plan",
        json!([ends_with(result("Planned.", &[("plan.md", PLAN)]))]),
    );
    p.scenario(
        "builder",
        "build",
        json!([runs_then(
            BUILD,
            result(
                "Built.",
                &[(
                    "pr.md",
                    "Add subtract to the calculator\n\nThe calculator can subtract now.\n\nCloses DEMO-12."
                )]
            )
        )]),
    );
    p.scenario(
        "reviewer",
        "review",
        json!([ends_with(result(
            "Reviewed.",
            &[(
                "review.md",
                "# Review\n\n## Findings\n\n### F1: the docstring of subtract is short\nRejected: style_only.\n\n## Open findings\n\nNone.\n"
            )]
        ))]),
    );
    p.scenario(
        "builder",
        "fix-ci",
        json!([runs_then(
            "git rm -q TODO.txt && git commit -q -m 'Remove the TODO file'",
            result("Fixed the CI: the TODO file made ci.sh fail.", &[])
        )]),
    );
    p.scenario(
        "tester",
        "qa",
        json!([
            ends_with(result(
                "One bug.",
                &[(
                    "qa.md",
                    "## Tests\n\n| Requirement | Steps | Result | Evidence |\n| --- | --- | --- | --- |\n| subtract gives a minus b | subtract(2, 5) | fail | shots/sign.png |\n\n## Bugs\n\n### B1: subtract(2, 5) loses the sign (major)\nSteps: call subtract(2, 5).\nResult: 3.\n"
                )]
            )),
            ends_with(result(
                "No bug.",
                &[(
                    "qa.md",
                    "## Tests\n\n| Requirement | Steps | Result | Evidence |\n| --- | --- | --- | --- |\n| subtract gives a minus b | subtract(2, 5) | pass | shots/sign.png |\n\n## Bugs\n\nNone.\n"
                )]
            ))
        ]),
    );
    p.scenario(
        "builder",
        "fix-qa",
        json!([runs_then(
            "printf '\\n# subtract keeps the sign of a minus b.\\n' >> calc.sh && git commit -q -am 'Keep the sign of subtract'",
            result(
                "Fixed B1.",
                &[("fix-notes.md", "# Fix notes\n\nB1 is fixed in the last commit.\n")]
            )
        )]),
    );
    let verdicts = json!([
        {"comment_id": 501, "kind": "inline", "verdict": "valid", "fix_commit": "", "reply": "Renamed to test_calc_subtract.sh."},
        {"comment_id": 601, "kind": "review", "verdict": "valid", "fix_commit": "", "reply": "Done: the test file is test_calc_subtract.sh now."},
        {"comment_id": 701, "kind": "conversation", "verdict": "out-of-scope", "fix_commit": "", "reply": "Division belongs to a different ticket."}
    ])
    .to_string();
    // The late round: the fix names subtract in a README, and a note in TODO.txt makes ci.sh
    // fail at its head.
    let late_verdicts = json!([
        {"comment_id": 502, "kind": "inline", "verdict": "valid", "fix_commit": "", "reply": "README.md names subtract now."},
        {"comment_id": 603, "kind": "review", "verdict": "valid", "fix_commit": "", "reply": "Done: README.md names subtract."}
    ])
    .to_string();
    p.scenario(
        "shepherd",
        "comments",
        json!([
            runs_then(
                "git mv test_subtract.sh test_calc_subtract.sh && git commit -q -m 'Name the test of subtract like the others'",
                result("Answered three comments.", &[("verdicts.json", &verdicts)])
            ),
            runs_then(
                "printf 'subtract(a, b) gives a minus b.\\n' > README.md && echo 'Link README.md from the docs.' > TODO.txt && git add -A && git commit -q -m 'Name subtract in the README'",
                result("Answered two comments.", &[("verdicts.json", &late_verdicts)])
            )
        ]),
    );
    p.scenario(
        "builder",
        "fix-ci-review",
        json!([runs_then(
            "git rm -q TODO.txt && git commit -q -m 'Remove the TODO file of the README'",
            result("Fixed the CI: the TODO file made ci.sh fail.", &[])
        )]),
    );
    let dir = p.vision.join("claude/fake-scenarios");
    std::fs::write(
        dir.join("curator.json"),
        json!({"turns": [[{"do": "init"}, {"do": "result", "structured_output": {"summary": "Nothing to change.", "changes": []}}]]}).to_string(),
    )
    .unwrap();
}

#[test]
fn a_ticket_goes_from_jira_to_a_merged_pull_request_on_github() {
    let started = Instant::now();
    let p = Project::new();
    scenarios(&p);

    // pick and take: the queue comes from the search of Jira and the branches of GitHub.
    let pick = p.run(&["pick", "--json"]);
    pick.expect_code(0);
    assert_eq!(pick.json()[0]["key"], KEY, "{pick:?}");
    assert_eq!(pick.json()[0]["eligible"], true);
    p.run(&["take", KEY]).expect_code(0);
    let base = p.head();

    // refine: the new description goes to Jira, and the old one stays in a document.
    p.tick_to("plan");
    let issue = p.server.state()["issues"][KEY].clone();
    assert_eq!(
        issue["description"]["content"][0]["content"][0]["text"],
        "Add a function subtract(a, b) to the calculator that gives a minus b, with a test."
    );
    let backup = std::fs::read_dir(p.vision.join("tickets").join(KEY))
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .find(|n| n.starts_with("description-backup-DEMO-12-"))
        .expect("the backup of the old description, from the act answer");
    let backup: Value = serde_json::from_str(&p.doc(&backup).unwrap()).unwrap();
    assert_eq!(
        backup["markdown"],
        "The calculator can only add. Add a function subtract(a, b) that gives a minus b, with a test."
    );

    // The plan and its approval.
    p.tick_to("plan-ready");
    let plan = p.run(&["plan", KEY]);
    plan.expect_code(0);
    let hash = plan_hash_of(&plan.stdout);
    p.run(&["approve-plan", KEY, &hash]).expect_code(0);

    // build: tracker.move runs in the mode auto before the turn, then the build and the
    // proof. The commit stays in the clone.
    p.tick_to("review");
    assert_eq!(p.server.state()["issues"][KEY]["status"], "In Progress");
    assert_eq!(
        p.operations("tracker.move"),
        [("confirmed".to_string(), base.clone())],
        "the move ran with no approval, before the build"
    );
    let built = p.head();
    assert_ne!(built, base);
    p.assert_nothing_left("after the build");
    assert_eq!(p.row().unwrap().pr, None);
    assert!(
        p.doc("proof.txt").unwrap().contains("test_subtract.sh"),
        "the proof ran the new test"
    );
    let proof = p
        .ledger()
        .checks_of(KEY, Some(&built))
        .unwrap()
        .into_iter()
        .filter(|c| c.kind == "proof-base")
        .map(|c| c.command)
        .collect::<Vec<_>>();
    assert_eq!(
        proof,
        ["sh test_subtract.sh"],
        "the plan declares test_calc.sh green, so only test_subtract.sh runs on the base"
    );

    // review: no second reviewer is set, so the review turn gets null for its document.
    p.tick_to("qa");
    p.assert_nothing_left("after the review");
    assert!(p.doc("second-review.md").is_none());
    let review_facts = p.facts_of_step("review");
    assert_eq!(review_facts["documents"]["second-review.md"], Value::Null);
    let requests: Vec<&str> = review_facts["requests"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["verb"].as_str().unwrap())
        .collect();
    assert_eq!(
        requests,
        [
            "ci.log",
            "github.pr",
            "jira.issue",
            "jira.search",
            "review.read",
            "git.log",
            "git.blame"
        ]
    );
    assert!(review_facts["requests"][0]["about"]
        .as_str()
        .unwrap()
        .contains("args: run"));

    // qa: one bug on the local app. Its fix goes through the review again, then QA with no
    // bug. The fix stays in the clone too.
    p.tick_to("fix-qa");
    assert!(
        p.doc("app-local.json").unwrap().contains("/health"),
        "app-start ran before QA"
    );
    p.tick_to("review");
    let qa_fixed = p.head();
    assert_ne!(qa_fixed, built);
    p.assert_nothing_left("after the fix of QA");
    p.tick_to("qa");
    p.assert_nothing_left("before the second QA");

    // open-pr: the second QA holds with no bug, and only now the push and the pull request.
    p.tick_to("open-pr");
    p.assert_nothing_left("at the entry of open-pr");
    let qa_turns: Vec<Option<String>> = p
        .ledger()
        .turns_of_ticket(KEY)
        .unwrap()
        .into_iter()
        .filter(|t| t.step.as_deref() == Some("qa"))
        .map(|t| t.end_fact)
        .collect();
    assert_eq!(
        qa_turns,
        [Some("held".to_string()), Some("held".to_string())]
    );
    p.tick_to("ci");
    assert_eq!(
        p.operations("git.push"),
        [("confirmed".to_string(), qa_fixed.clone())]
    );
    assert_eq!(
        p.operations("pr.open"),
        [("confirmed".to_string(), qa_fixed.clone())]
    );
    let ops: Vec<String> = p
        .ledger()
        .operations_of(KEY, None)
        .unwrap()
        .into_iter()
        .map(|o| o.verb)
        .filter(|v| v == "git.push" || v == "pr.open")
        .collect();
    assert_eq!(
        ops,
        ["git.push", "pr.open"],
        "the push, then the pull request"
    );
    assert_eq!(
        p.remote_branch().as_deref(),
        Some(qa_fixed.as_str()),
        "the reviewed and tested head is on the remote"
    );
    let pull = p.server.state()["pulls"]["7"].clone();
    assert_eq!(pull["title"], "Add subtract to the calculator");
    assert_eq!(pull["draft"], true);
    assert_eq!(
        pull["base"]["ref"], "main",
        "the base branch comes from base_ref"
    );
    assert_eq!(p.row().unwrap().pr, Some(7));

    // ci: the first run is red; the failed jobs run once more, still red; fix-ci fixes it.
    p.tick_to("fix-ci");
    // The job ci-log runs before the first turn of the step.
    p.tick_until("the log of the red run", |p| p.doc("ci-log.txt").is_some());
    let log = p.doc("ci-log.txt").unwrap();
    assert!(log.contains("Failed jobs: test"), "{log}");
    assert!(
        log.contains("test ! -f TODO.txt") || log.contains("attempt 2"),
        "{log}"
    );
    let runs = p.server.state()["runs"].clone();
    assert_eq!(runs.as_array().unwrap().len(), 1);
    assert_eq!(
        runs[0]["run_attempt"], 2,
        "ci-rerun ran the failed jobs again"
    );
    assert_eq!(runs[0]["conclusion"], "failure");
    p.tick_to("ready");
    let fixed = p.head();
    assert_ne!(fixed, qa_fixed);
    assert_eq!(
        p.ci_checks(),
        [
            (qa_fixed.clone(), "red".to_string()),
            (fixed.clone(), "green".to_string())
        ]
    );
    assert_eq!(
        p.remote_branch().as_deref(),
        Some(fixed.as_str()),
        "fix-ci pushes its own commit"
    );
    let findings = p.ledger().findings_of(KEY).unwrap();
    let rows: Vec<(&str, &str)> = findings
        .iter()
        .map(|f| (f.source.as_str(), f.verdict.as_str()))
        .collect();
    assert_eq!(
        rows,
        [
            ("review", "rejected"),
            ("qa", "kept"),
            ("review", "rejected")
        ]
    );

    // ready: tracker.move runs in the mode auto; the person marks the pull request ready.
    p.tick_until("the move to In Review", |p| {
        p.server.state()["issues"][KEY]["status"] == "In Review"
    });
    p.tick();
    assert_eq!(p.step().as_deref(), Some("ready"), "the draft waits");
    p.server.team("ready", json!({"number": 7}));
    p.tick_to("in-review");

    // A review that asks for changes, with one inline comment and one conversation comment.
    p.tick();
    assert_eq!(p.step().as_deref(), Some("in-review"), "nothing waits yet");
    p.server.team(
        "comment",
        json!({"number": 7, "id": 501, "login": "lead", "path": "test_subtract.sh", "line": 1,
               "body": "Name this file test_calc_subtract.sh."}),
    );
    p.server.team(
        "review",
        json!({"number": 7, "id": 601, "login": "lead", "state": "CHANGES_REQUESTED",
               "body": "Please follow the names of the other tests.\nSee the inline comment."}),
    );
    p.server.team(
        "issue_comment",
        json!({"number": 7, "id": 701, "login": "other", "body": "Can the calculator also divide?"}),
    );
    // A note of a bot that needs no reply: the round reads it and answers nothing.
    p.server.team(
        "issue_comment",
        json!({"number": 7, "id": 702, "login": "preview-bot", "body": "The preview of #7 is ready."}),
    );
    p.tick_to("comments");
    p.tick_until("the comments that wait", |p| {
        p.doc("comments.json").is_some()
    });
    let comments: Value = serde_json::from_str(&p.doc("comments.json").unwrap()).unwrap();
    let kinds: Vec<(i64, &str)> = comments
        .as_array()
        .unwrap()
        .iter()
        .map(|c| (c["id"].as_i64().unwrap(), c["kind"].as_str().unwrap()))
        .collect();
    assert_eq!(
        kinds,
        [
            (501, "inline"),
            (601, "review"),
            (701, "conversation"),
            (702, "conversation")
        ]
    );

    // The Shepherd round: a fix, then its push and the replies, each in the mode auto. The
    // review is not asked again yet: CI runs at the new head first.
    p.tick_to("ci-review");
    let shepherd = p.head();
    assert_ne!(shepherd, fixed);
    assert_eq!(p.verbs_at(&shepherd), ["git.push", "review.reply"]);
    assert_eq!(
        p.operations("review.reply"),
        [("confirmed".to_string(), shepherd.clone())]
    );
    assert!(
        p.operations("review.rerequest").is_empty(),
        "no review request before CI holds"
    );
    assert_eq!(p.remote_branch().as_deref(), Some(shepherd.as_str()));
    let state = p.server.state();
    let inline: Vec<&Value> = state["comments"]["7"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|c| c["in_reply_to_id"] == 501)
        .collect();
    assert_eq!(inline.len(), 1);
    assert_eq!(inline[0]["body"], "Renamed to test_calc_subtract.sh.");
    let talk: Vec<String> = state["issue_comments"]["7"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|c| c["user"]["login"] == "vision-bot")
        .map(|c| c["body"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(
        talk,
        [
            "Re-review #601: Done: the test file is test_calc_subtract.sh now.\n\n<!-- vision-reply review 601 -->",
            "Re #701: Division belongs to a different ticket.\n\n<!-- vision-reply conversation 701 -->"
        ]
    );
    assert_eq!(
        state["pulls"]["7"]["requested_reviewers"],
        json!([]),
        "the review is not asked again before CI holds"
    );
    let reviews = p.ledger().review_rows_after(0, 10).unwrap();
    let reviews: Vec<(Option<i64>, Option<String>, Option<String>)> = reviews
        .into_iter()
        .map(|r| (r.comment_id, r.reviewer, r.verdict))
        .collect();
    assert_eq!(
        reviews,
        [
            (Some(501), Some("lead".into()), Some("valid".into())),
            (Some(601), Some("lead".into()), Some("valid".into())),
            (Some(701), Some("other".into()), Some("out-of-scope".into()))
        ]
    );

    // The round ended well, so Vision keeps the mark of each comment that it read, also of the
    // note that got no reply.
    let seen: Value = serde_json::from_str(&p.doc("comments-seen.json").unwrap()).unwrap();
    assert_eq!(
        seen,
        json!([
            "conversation:701:701",
            "conversation:702:702",
            "inline:501:501",
            "review:601:601"
        ])
    );

    // ci-review: CI runs at the head of the comments. Only its green run moves the ticket to
    // rerequest, and that act step asks for the review again.
    p.tick_to("rerequest");
    assert_eq!(
        p.verbs_at(&shepherd),
        ["git.push", "review.reply", "ci.run"]
    );
    assert_eq!(
        p.ci_checks().last(),
        Some(&(shepherd.clone(), "green".to_string()))
    );
    assert_eq!(
        p.server.state()["pulls"]["7"]["requested_reviewers"],
        json!([]),
        "the move to rerequest runs no verb"
    );
    p.tick_to("in-review");
    assert_eq!(
        p.verbs_at(&shepherd),
        ["git.push", "review.reply", "ci.run", "review.rerequest"],
        "the push, the replies, CI, and only then the review request"
    );
    assert_eq!(
        p.operations("review.rerequest"),
        [("confirmed".to_string(), shepherd.clone())]
    );
    assert_eq!(
        p.server.state()["pulls"]["7"]["requested_reviewers"],
        json!([{"login": "lead"}])
    );
    assert_eq!(p.server.state()["runs"].as_array().unwrap().len(), 3);
    // The round went back to no step before the pull request: not the review, the QA, or
    // open-pr.
    let path = p.path();
    let round = path.iter().rposition(|s| s == "comments").unwrap();
    assert_eq!(
        path[round..],
        ["comments", "ci-review", "rerequest", "in-review"]
    );
    p.tick();
    assert_eq!(
        p.step().as_deref(),
        Some("in-review"),
        "the review is asked again, and nothing waits: the note of the bot does not start a round"
    );
    p.server.team(
        "review",
        json!({"number": 7, "id": 602, "login": "lead", "state": "APPROVED", "body": ""}),
    );
    p.tick_to("ready-to-merge");

    // The base moved on: another ticket put multiply on main, at the place where this ticket
    // adds subtract. The pull request conflicts with its base, so ready-to-merge sends the
    // ticket to merge-base-ready. The job base-merge fetches main and starts the merge in the
    // clone. Merger keeps both functions and commits the merge, the proof holds, and the branch
    // goes out with a CI run.
    let team = TempDir::new().unwrap();
    let work = team.path().join("team");
    git(
        team.path(),
        &[
            "clone",
            "--quiet",
            p.remote.to_str().unwrap(),
            work.to_str().unwrap(),
        ],
    );
    let multiply = "\n# Gives a times b.\nmultiply() { echo $(($1 * $2)); }\n";
    std::fs::write(work.join("calc.sh"), format!("{TOY_CALC}{multiply}")).unwrap();
    git(&work, &["commit", "--quiet", "-am", "Add multiply"]);
    git(&work, &["push", "--quiet", "origin", "HEAD:main"]);
    let tip = git(&work, &["rev-parse", "HEAD"]);
    let approved = p.head();
    let addition = team.path().join("multiply.sh");
    std::fs::write(&addition, multiply).unwrap();
    p.scenario(
        "merger",
        "merge-base-ready",
        json!([runs_then(
            &format!(
                "git checkout --ours calc.sh && cat '{}' >> calc.sh && git add calc.sh && git commit -q --no-edit",
                addition.display()
            ),
            result(
                "Merged.",
                &[(
                    "merge.md",
                    "# Merge of main into the branch of DEMO-12\n\n## calc.sh\n\nmain added multiply, and the ticket added subtract: both stay.\n"
                )]
            )
        )]),
    );
    p.server
        .team("conflict", json!({"number": 7, "conflict": true}));
    p.tick_until("the merge of main", |p| {
        p.step().as_deref() == Some("merge-base-ready")
    });
    // GitHub sees the merge after the push, and the conflict is gone.
    p.server
        .team("conflict", json!({"number": 7, "conflict": false}));
    p.tick_until("the way back from the merge of main", |p| {
        p.step().as_deref() == Some("ready-to-merge")
    });
    assert!(p.flags().is_empty(), "{:?} {}", p.flags(), p.report());
    let merged = p.head();
    assert_eq!(
        git(&p.clone, &["rev-list", "--parents", "-n", "1", &merged]),
        format!("{merged} {approved} {tip}")
    );
    assert_eq!(
        git(&p.clone, &["log", "-1", "--format=%s", &merged]),
        format!("Merge main ({}) into {BRANCH}", &tip[..12])
    );
    assert_eq!(p.remote_branch().as_deref(), Some(merged.as_str()));
    assert_eq!(p.verbs_at(&merged), ["git.push", "ci.run"]);
    let doc = p
        .doc("base-merge.md")
        .expect("the document of the merge of main");
    assert!(doc.contains("- `calc.sh`") && doc.contains(&tip), "{doc}");
    assert!(p.doc("merge.md").is_some());

    // A late round: after the approval, the lead asks for one more change, so ready-to-merge
    // goes to comments, and the round takes the same path. Its fix leaves TODO.txt, so CI is
    // red at its head: fix-ci-review fixes it and pushes, CI runs again at the new head, and
    // only the green run asks for the review again.
    p.server.team(
        "comment",
        json!({"number": 7, "id": 502, "login": "lead", "path": "calc.sh", "line": 1,
               "body": "Name subtract in a README too."}),
    );
    p.server.team(
        "review",
        json!({"number": 7, "id": 603, "login": "lead", "state": "CHANGES_REQUESTED",
               "body": "One more thing before the merge.\nSee the inline comment."}),
    );
    p.tick_to("comments");
    p.tick_to("ci-review");
    let late = p.head();
    assert_ne!(late, shepherd);
    assert_eq!(p.verbs_at(&late), ["git.push", "review.reply"]);
    assert_eq!(p.remote_branch().as_deref(), Some(late.as_str()));
    let replies = p.server.state()["comments"]["7"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|c| c["in_reply_to_id"] == 502)
        .count();
    assert_eq!(replies, 1, "the late round replies before CI");
    p.tick_to("fix-ci-review");
    assert_eq!(p.verbs_at(&late), ["git.push", "review.reply", "ci.run"]);
    assert_eq!(
        p.ci_checks().last(),
        Some(&(late.clone(), "red".to_string()))
    );
    assert_eq!(
        p.operations("review.rerequest").len(),
        1,
        "a red run asks for no review"
    );
    p.tick_to("ci-review");
    let ci_fixed = p.head();
    assert_ne!(ci_fixed, late);
    assert_eq!(p.verbs_at(&ci_fixed), ["git.push"]);
    assert_eq!(
        p.remote_branch().as_deref(),
        Some(ci_fixed.as_str()),
        "fix-ci-review pushes its own commit"
    );
    p.tick_to("rerequest");
    assert_eq!(
        p.ci_checks().last(),
        Some(&(ci_fixed.clone(), "green".to_string()))
    );
    assert_eq!(p.operations("review.rerequest").len(), 1);
    p.tick_to("in-review");
    assert_eq!(
        p.verbs_at(&ci_fixed),
        ["git.push", "ci.run", "review.rerequest"]
    );
    assert_eq!(
        p.server.state()["pulls"]["7"]["requested_reviewers"],
        json!([{"login": "lead"}])
    );
    let path = p.path();
    let round = path.iter().rposition(|s| s == "comments").unwrap();
    assert_eq!(
        path[round - 1..],
        [
            "ready-to-merge",
            "comments",
            "ci-review",
            "fix-ci-review",
            "ci-review",
            "rerequest",
            "in-review"
        ]
    );
    p.tick();
    assert_eq!(
        p.step().as_deref(),
        Some("in-review"),
        "the review is asked again, and nothing waits"
    );
    p.server.team(
        "review",
        json!({"number": 7, "id": 604, "login": "lead", "state": "APPROVED", "body": ""}),
    );
    p.tick_to("ready-to-merge");
    p.server.team("merge", json!({"number": 7}));
    p.tick_until("the end", |p| p.counts()["finished"].is_string());
    let row = p.row().unwrap();
    assert_eq!(row.state.as_deref(), Some("done"));
    assert_eq!(row.clone, None, "the end frees the clone");
    assert!(p
        .ledger()
        .event_by_external_id("ticket-merged:DEMO-12:7")
        .unwrap()
        .is_some());
    assert!(git(&p.remote, &["log", "-1", "--format=%s", "main"]).contains("Merge pull request #7"));

    // The path through each step of adapter.toml. After the first entry into comments, each
    // step is a step of the code review.
    let spans = p.path();
    assert_eq!(
        spans,
        [
            "refine",
            "plan",
            "plan-ready",
            "build",
            "review",
            "qa",
            "fix-qa",
            "review",
            "qa",
            "open-pr",
            "ci",
            "ci-rerun",
            "fix-ci",
            "ci",
            "ready",
            "in-review",
            "comments",
            "ci-review",
            "rerequest",
            "in-review",
            "ready-to-merge",
            "merge-base-ready",
            "ready-to-merge",
            "comments",
            "ci-review",
            "fix-ci-review",
            "ci-review",
            "rerequest",
            "in-review",
            "ready-to-merge",
            "done"
        ]
    );
    let first = spans.iter().position(|s| s == "comments").unwrap();
    for step in &spans[first..] {
        assert!(
            [
                "comments",
                "ci-review",
                "fix-ci-review",
                "rerequest",
                "in-review",
                "ready-to-merge",
                "merge-base-ready",
                "done"
            ]
            .contains(&step.as_str()),
            "the code review went back to {step}: {spans:?}"
        );
    }

    // The operations: each one confirmed, and each verb as often as the path needs it.
    let ops = p.ledger().operations_of(KEY, None).unwrap();
    assert!(ops.iter().all(|o| o.state == "confirmed"), "{ops:#?}");
    let mut by_verb: BTreeMap<&str, usize> = BTreeMap::new();
    for op in &ops {
        *by_verb.entry(op.verb.as_str()).or_default() += 1;
    }
    assert_eq!(
        by_verb,
        BTreeMap::from([
            ("ci.rerun", 1),
            ("ci.run", 6),
            ("git.push", 6),
            ("pr.open", 1),
            ("review.reply", 2),
            ("review.rerequest", 2),
            ("tracker.describe", 1),
            ("tracker.move", 2),
        ])
    );

    // The requests that the world recorded: each write one time, each token in its header only.
    let writes = p.writes();
    assert_eq!(
        writes,
        BTreeMap::from([
            (
                "POST /repos/acme/widgets/actions/runs/41/rerun-failed-jobs".to_string(),
                1
            ),
            (
                "POST /repos/acme/widgets/actions/workflows/ci.yml/dispatches".to_string(),
                6
            ),
            ("POST /repos/acme/widgets/issues/7/comments".to_string(), 3),
            ("POST /repos/acme/widgets/pulls".to_string(), 1),
            (
                "POST /repos/acme/widgets/pulls/7/comments/501/replies".to_string(),
                1
            ),
            (
                "POST /repos/acme/widgets/pulls/7/comments/502/replies".to_string(),
                1
            ),
            (
                "POST /repos/acme/widgets/pulls/7/requested_reviewers".to_string(),
                2
            ),
            ("POST /rest/api/3/issue/DEMO-12/transitions".to_string(), 2),
            (
                "POST /rest/api/3/search/jql".to_string(),
                writes["POST /rest/api/3/search/jql"]
            ),
            ("PUT /rest/api/3/issue/DEMO-12".to_string(), 1),
        ]),
        "a search is a read; each other write happened one time"
    );
    let dispatched: std::collections::BTreeSet<String> = p.server.state()["runs"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["head_sha"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(dispatched.len(), 6, "one run for each commit");
    let requests = p.server.requests();
    for r in &requests {
        let path = r["path"].as_str().unwrap();
        let expected = if path.starts_with("/rest/api/3/") {
            "jira"
        } else if path.starts_with("/repos/") {
            "forge"
        } else {
            "none"
        };
        assert_eq!(r["auth"], expected, "{r}");
        let line = r.to_string();
        for token in [JIRA_TOKEN, FORGE_TOKEN] {
            assert!(!line.contains(token), "a token in a request: {line}");
        }
    }
    assert!(
        requests
            .iter()
            .any(|r| r["path"].as_str().unwrap().starts_with("/storage/logs/")),
        "the job log came from the storage, with no token"
    );

    // The documents of the ticket folder.
    for name in [
        "issue.json",
        "refine.md",
        "description.md",
        "plan.md",
        "approved-plan.md",
        "pr.md",
        "proof.txt",
        "review.md",
        "ci-log.txt",
        "app-local.json",
        "qa.md",
        "fix-notes.md",
        "comments.json",
        "verdicts.json",
    ] {
        assert!(p.doc(name).is_some(), "the document {name}");
    }

    // No token reached a file: the home, the ledger, the clone, and the remote.
    let mut files = Vec::new();
    for dir in [&p.vision, &p.clone, &p.remote, &p.user_home] {
        files_under(dir, &mut files);
    }
    assert!(files.len() > 20);
    for (path, bytes) in &files {
        for token in [JIRA_TOKEN, FORGE_TOKEN] {
            assert!(!holds(bytes, token), "a token in {}", path.display());
        }
    }

    // The merged event makes the Curator run once; let its turn end before the home goes.
    let until = Instant::now() + STAGE;
    while !p.ledger().turns_with_status("running").unwrap().is_empty() {
        assert!(Instant::now() < until, "a turn never ended");
        p.tick();
        std::thread::sleep(Duration::from_millis(100));
    }
    if let Some(out) = std::env::var_os("VISION_E2E_DUMP") {
        std::fs::write(out, p.report()).unwrap();
    }
    // The path takes about 60 s on a laptop. The time is printed, not asserted: a busy machine
    // makes each process slower, and that says nothing about the code.
    eprintln!("the whole path took {:?}", started.elapsed());
}

/// A private adapter as a project starts with one: the built-in scripts, a shorter path with no
/// qa step and no deploy verb, and each write verb in the mode shadow.
fn shadow_adapter() -> String {
    let mut doc: DocumentMut = fast_adapter().parse().unwrap();
    let steps = doc["step"].as_array_of_tables_mut().unwrap();
    steps.retain(|step| !matches!(step["id"].as_str(), Some("qa" | "fix-qa")));
    for step in steps.iter_mut() {
        if step["id"].as_str() == Some("review") {
            step["next"] = toml_edit::value("open-pr");
        }
    }
    let verbs = doc["verbs"].as_table_mut().unwrap();
    verbs.remove("deploy");
    for (_, verb) in verbs.iter_mut() {
        if verb.get("mode").is_some() {
            verb["mode"] = toml_edit::value("shadow");
        }
    }
    doc.to_string()
}

#[test]
fn a_private_adapter_with_the_built_in_scripts_keeps_each_write_in_shadow() {
    let adapter = shadow_adapter();
    assert!(adapter.contains("\nscripts = \"builtin:jira-github\""));
    assert!(!adapter.contains("id = \"qa\"") && !adapter.contains("[verbs.deploy]"));
    assert!(!adapter.contains("mode = \"auto\"") && !adapter.contains("mode = \"ask\""));
    let p = Project::with_adapter(adapter);
    scenarios(&p);

    // doctor finds the private adapter and names its built-in scripts.
    let doctor = p.run(&["doctor", "--json"]);
    let tests = doctor.json()["tests"].clone();
    let check = tests
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["test"] == "adapter")
        .expect("the adapter test of doctor")
        .clone();
    assert_eq!(check["result"], "ok", "{check}");
    let text = check.to_string();
    assert!(
        text.contains(&p.vision.join("adapters/jira-github").display().to_string())
            && text.contains("19 steps")
            && text.contains("with the built-in scripts builtin:jira-github"),
        "{text}"
    );

    // pick reads the queue through observe of the built-in adapter.
    let pick = p.run(&["pick", "--json"]);
    pick.expect_code(0);
    assert_eq!(pick.json()[0]["key"], KEY, "{pick:?}");
    assert_eq!(pick.json()[0]["eligible"], true);
    p.run(&["take", KEY]).expect_code(0);

    // refine: the turn ends, and tracker.describe in the mode shadow records its text, sends
    // nothing, and asks for the person.
    p.tick_until_flag("needs-you");
    assert_eq!(p.step().as_deref(), Some("refine"));
    let reason = p.counts()["reasons"]["needs-you"]["reason"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(
        reason.contains("tracker.describe is in the mode shadow"),
        "{reason}"
    );
    let ops = p.ledger().operations_of(KEY, None).unwrap();
    let states: Vec<(&str, &str)> = ops
        .iter()
        .map(|o| (o.verb.as_str(), o.state.as_str()))
        .collect();
    assert_eq!(
        states,
        [("tracker.describe", "pending")],
        "the mode shadow keeps the write back until a promote"
    );
    let issue = p.server.state()["issues"][KEY].clone();
    assert_eq!(
        issue["description"]["content"][0]["content"][0]["text"],
        "The calculator can only add. Add a function subtract(a, b) that gives a minus b, with a test.",
        "the description in Jira stays as it was"
    );
    let writes = p.writes();
    assert!(
        writes.keys().all(|w| w == "POST /rest/api/3/search/jql"),
        "only reads reached the world: {writes:?}"
    );
    for r in p.server.requests() {
        let line = r.to_string();
        for token in [JIRA_TOKEN, FORGE_TOKEN] {
            assert!(!line.contains(token), "a token in a request: {line}");
        }
    }
    let mut files = Vec::new();
    for dir in [&p.vision, &p.clone, &p.remote, &p.user_home] {
        files_under(dir, &mut files);
    }
    for (path, bytes) in &files {
        for token in [JIRA_TOKEN, FORGE_TOKEN] {
            assert!(!holds(bytes, token), "a token in {}", path.display());
        }
    }
}

#[test]
fn the_person_moves_a_ticket_from_a_turn_step_to_in_review_and_the_next_tick_follows_the_review() {
    let p = Project::new();
    // The scout writes no description, so after a resume the ticket needs the person at refine,
    // a turn step.
    p.scenario(
        "scout",
        "refine",
        json!([ends_with(result(
            "Refined.",
            &[("refine.md", "# Refine\n")]
        ))]),
    );
    p.run(&["take", KEY]).expect_code(0);
    p.tick_until_flag("needs-you");
    assert_eq!(p.step().as_deref(), Some("refine"));

    // The person did the work by hand: a commit, the push, and the pull request. Then the lead
    // asks for a change, so the review state is `comments`.
    std::fs::write(
        p.clone.join("calc.sh"),
        format!("{TOY_CALC}\n# Gives a minus b.\nsubtract() {{ echo $(($1 - $2)); }}\n"),
    )
    .unwrap();
    git(&p.clone, &["commit", "--quiet", "-am", "Add subtract"]);
    git(&p.clone, &["push", "--quiet", "origin", BRANCH]);
    let number = p.server.world.lock().unwrap().open_pull(BRANCH);
    p.server.team(
        "comment",
        json!({"number": number, "id": 501, "login": "lead", "path": "calc.sh", "line": 5,
               "body": "Add a test for subtract."}),
    );

    let moved = p.run(&["retry", KEY, "--step", "in-review"]);
    moved.expect_code(0);
    assert!(
        moved.stdout.contains(
            "Moved DEMO-12 from the step refine to the step in-review (cleared: needs-you). The next tick enters the step in-review."
        ),
        "{moved:?}"
    );
    assert!(p.flags().is_empty());
    assert_eq!(p.step().as_deref(), Some("in-review"));
    assert_eq!(p.counts()["visit"]["how"], "move");
    assert_eq!(
        p.counts()["visit"]["message"],
        Value::Null,
        "a wait step has no turn"
    );

    // The next tick observes in-review as after a move of the pipeline into it: the value
    // comments sends the ticket to the step comments.
    p.tick();
    assert_eq!(p.step().as_deref(), Some("comments"), "{}", p.report());
    assert_eq!(p.path(), ["refine", "in-review", "comments"]);
    let step = p
        .ledger()
        .events_of_ticket(KEY)
        .unwrap()
        .into_iter()
        .rfind(|e| e.kind == "step")
        .expect("the move of the pipeline");
    let detail: Value = serde_json::from_str(step.detail.as_deref().unwrap()).unwrap();
    assert_eq!(
        (&detail["from"], &detail["to"], &detail["how"]),
        (
            &json!("in-review"),
            &json!("comments"),
            &json!("on comments")
        )
    );
    assert_eq!(p.counts()["entries"]["comments"], 1);
    assert_eq!(
        p.server.state()["issues"][KEY]["status"],
        "To Do",
        "the move ran no verb of the steps it skipped"
    );
    let log = p.run(&["log", KEY]);
    log.expect_code(0);
    assert!(
        log.stdout
            .contains("user moved the ticket from refine to in-review"),
        "{log:?}"
    );
    let human = p.ledger().human_of(Some(KEY)).unwrap();
    assert_eq!(human.last().map(|h| h.kind.as_str()), Some("move"));
}

// ----- The rules of a continue: [[resume]] --------------------------------------------------

/// The scout writes no description, so the ticket needs the person at refine. Gives the
/// number of requests that the world recorded then.
fn needs_the_person_at_refine(p: &Project) -> usize {
    p.scenario(
        "scout",
        "refine",
        json!([ends_with(result(
            "Refined.",
            &[("refine.md", "# Refine\n")]
        ))]),
    );
    p.run(&["take", KEY]).expect_code(0);
    p.tick_until_flag("needs-you");
    assert_eq!(p.step().as_deref(), Some("refine"));
    p.server.requests().len()
}

/// The person does the rest by hand: a commit in the clone, the push, and an open pull request
/// that is ready. Gives its number.
fn pull_request_by_hand(p: &Project) -> i64 {
    std::fs::write(
        p.clone.join("calc.sh"),
        format!("{TOY_CALC}\n# Gives a minus b.\nsubtract() {{ echo $(($1 - $2)); }}\n"),
    )
    .unwrap();
    git(&p.clone, &["commit", "--quiet", "-am", "Add subtract"]);
    git(&p.clone, &["push", "--quiet", "origin", BRANCH]);
    p.server.world.lock().unwrap().open_pull(BRANCH)
}

/// A ticket that stopped at review, a turn step of the self-review, while the person opened
/// the pull request by hand. The person moves the ticket from refine to review, which runs no
/// rule because the ticket is not paused, and stops it there. Gives the number of the pull
/// request.
fn stopped_at_review_with_a_pull_request(p: &Project) -> i64 {
    let requests = needs_the_person_at_refine(p);
    p.run(&["retry", KEY, "--step", "review"]).expect_code(0);
    assert_eq!(
        p.server.requests().len(),
        requests,
        "a move of a ticket that is not paused calls no observe"
    );
    p.run(&["stop", KEY]).expect_code(0);
    assert_eq!(p.flags(), ["stopped"]);
    assert_eq!(p.step().as_deref(), Some("review"));
    pull_request_by_hand(p)
}

/// The detail of the last event `resumed` of the ticket, an event of the person.
fn last_resumed(p: &Project) -> Value {
    let event = p
        .ledger()
        .events_of_ticket(KEY)
        .unwrap()
        .into_iter()
        .rfind(|e| e.kind == "resumed")
        .expect("an event resumed");
    assert_eq!(event.actor, "user");
    serde_json::from_str(event.detail.as_deref().unwrap()).unwrap()
}

#[test]
fn a_stopped_ticket_goes_on_where_the_rules_find_it_and_takes_the_head_that_the_person_pushed() {
    let p = Project::new();
    let number = stopped_at_review_with_a_pull_request(&p);
    let events = p.ledger().events_of_ticket(KEY).unwrap().len();

    // A plain take does not continue: it prints the suggestion of the rules with its reason and
    // the two commands, exits with code 1, and changes nothing.
    let take = p.run(&["take", KEY]);
    take.expect_code(1);
    assert_eq!(
        take.stdout,
        format!(
            "Vision thinks that DEMO-12 is at the step in-review, not at the step review where it stopped: the pull request #{number} is open and ready (rule 4). It did not continue the ticket.\nVision read:\n  the tracker status is To Do\n  the pull request #{number} is open and ready\n  the review state is waiting and no review comment waits\n  CI has no run\n  the branch of the clone is at the head of the pull request\nTo continue at in-review:  vision take DEMO-12 --step in-review\nTo continue at review:     vision take DEMO-12 --step review\n"
        ),
        "{take:?}"
    );
    assert_eq!(p.flags(), ["stopped"]);
    assert_eq!(p.step().as_deref(), Some("review"));
    assert_eq!(
        p.ledger().events_of_ticket(KEY).unwrap().len(),
        events,
        "a take that does not continue writes no event"
    );

    // A copy of the remote as it is now, without the next commit of the person.
    let root = p._dir.path();
    let stale = root.join("stale.git");
    git(
        root,
        &[
            "clone",
            "--quiet",
            "--bare",
            p.remote.to_str().unwrap(),
            stale.to_str().unwrap(),
        ],
    );
    // The person pushes one more commit from another clone, so the clone of Vision does not
    // have the head of the pull request.
    let person = root.join("person");
    git(
        root,
        &[
            "clone",
            "--quiet",
            "--branch",
            BRANCH,
            p.remote.to_str().unwrap(),
            person.to_str().unwrap(),
        ],
    );
    git(&person, &["config", "user.name", "person"]);
    git(&person, &["config", "user.email", "person@example.invalid"]);
    std::fs::write(
        person.join("test_subtract.sh"),
        ". ./calc.sh\n[ \"$(subtract 5 3)\" = 2 ]\n",
    )
    .unwrap();
    git(&person, &["add", "--all"]);
    git(&person, &["commit", "--quiet", "-m", "Test subtract"]);
    git(&person, &["push", "--quiet", "origin", BRANCH]);
    let pushed = git(&person, &["rev-parse", "HEAD"]);
    let before = p.head();

    // The remote of the clone of Vision does not have the head: the take at the suggestion
    // fetches, the head is still not there, so nothing moves, and the ticket gets the flag
    // needs-you with the fix.
    let origin = git(&p.clone, &["remote", "get-url", "origin"]);
    git(
        &p.clone,
        &["remote", "set-url", "origin", stale.to_str().unwrap()],
    );
    let behind = p.run(&["take", KEY, "--step", "in-review"]);
    behind.expect_code(1);
    let clone = p.clone.display().to_string();
    assert!(
        behind.stderr.contains(&format!(
            "the clone is behind the remote: Vision fetched, and the head {} of the pull request #{number} is still not in the clone {clone}. Push that commit to the remote of the clone, or fetch it into the clone by hand, then run `vision take DEMO-12 --step in-review` again. Nothing moved, and DEMO-12 has the flag needs-you until a continue takes the head",
            &pushed[..12]
        )),
        "{behind:?}"
    );
    assert_eq!(p.step().as_deref(), Some("review"));
    assert_eq!(p.flags(), ["stopped", "needs-you"]);
    assert_eq!(p.counts()["reasons"]["needs-you"]["by"], "resume");
    assert_eq!(p.head(), before);

    // With the head on the remote of the clone, the take fetches it by itself, moves the
    // branch of the clone forward to it, and moves the ticket to in-review: a new visit that
    // does not count for the cap of its step. The person runs no fetch. Vision now reads the
    // push by hand and suggests ci-review, but the person picks in-review.
    git(&p.clone, &["remote", "set-url", "origin", &origin]);
    let moved = p.run(&["take", KEY, "--step", "in-review"]);
    moved.expect_code(0);
    assert_eq!(
        moved.stdout,
        format!(
            "Moved DEMO-12 from the step review to the step in-review (cleared: stopped, needs-you). Vision suggested the step ci-review (rule 3): the pull request #{number} is open and ready and the pull request has later commits than the clone, such as a push by hand. Vision read: the tracker status is To Do. The pull request #{number} is open and ready. The review state is waiting and no review comment waits. CI has no run. The pull request has later commits than the clone, such as a push by hand. It took the head {} of the pull request #{number} that someone pushed by hand: the branch of the clone moved forward to it. The next tick enters the step in-review.\n",
            &pushed[..12]
        ),
        "{moved:?}"
    );
    assert!(p.flags().is_empty());
    assert_eq!(p.step().as_deref(), Some("in-review"));
    assert_eq!(p.head(), pushed);
    assert_eq!(
        p.row().unwrap().head.as_deref(),
        Some(pushed.as_str()),
        "the ticket row takes the head of the pull request"
    );
    assert_eq!(
        p.row().unwrap().pr,
        Some(number),
        "the ticket row takes the number of the pull request"
    );
    let status = p.run(&["status", "--json"]);
    status.expect_code(0);
    let view = status.json()["tickets"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["key"] == KEY)
        .cloned()
        .unwrap();
    assert_eq!(
        view["pr"],
        json!(number),
        "vision status shows the pull request"
    );
    let counts = p.counts();
    assert_eq!(counts["visit"]["how"], "move");
    assert_eq!(counts["visit"]["skip_cap"], true);
    let detail = last_resumed(&p);
    assert_eq!(
        (
            &detail["from"],
            &detail["step"],
            &detail["suggestion"],
            &detail["took"],
            &detail["rule"],
            &detail["how"]
        ),
        (
            &json!("review"),
            &json!("in-review"),
            &json!("ci-review"),
            &json!(false),
            &json!(3),
            &json!("move")
        )
    );
    assert_eq!(detail["head"], json!({"from": before, "to": pushed}));
    assert_eq!(detail["pr_head"], json!(pushed));
    assert_eq!(detail["pr"], json!(number));
    assert_eq!(detail["pr_written"], json!(number));
    assert_eq!(detail["pr_kept"], Value::Null);
    let human = p.ledger().human_of(Some(KEY)).unwrap();
    let row = human.last().unwrap();
    assert_eq!(
        (row.kind.as_str(), row.value.as_deref()),
        ("resume", Some("in-review"))
    );

    // The next tick acts at in-review: it observes review.state, and a waiting comment of the
    // lead sends the ticket to comments.
    p.server.team(
        "comment",
        json!({"number": number, "id": 501, "login": "lead", "path": "calc.sh", "line": 5,
               "body": "Name subtract in the README."}),
    );
    p.tick();
    assert_eq!(p.step().as_deref(), Some("comments"), "{}", p.report());
    let step = p
        .ledger()
        .events_of_ticket(KEY)
        .unwrap()
        .into_iter()
        .rfind(|e| e.kind == "step")
        .expect("the move of the pipeline");
    let detail: Value = serde_json::from_str(step.detail.as_deref().unwrap()).unwrap();
    assert_eq!(
        (&detail["from"], &detail["to"], &detail["how"]),
        (
            &json!("in-review"),
            &json!("comments"),
            &json!("on comments")
        )
    );
    let log = p.run(&["log", KEY]);
    log.expect_code(0);
    assert!(
        log.stdout.contains(&format!(
            "user resumed the ticket at in-review; Vision suggested ci-review, and took the head {} of the pull request",
            &pushed[..12]
        )),
        "{log:?}"
    );
}

/// The person starts a ticket by hand, outside Vision: a branch with its key, from another
/// clone, and an open pull request that is ready. Gives the branch, its head, and the number of
/// the pull request.
fn started_outside_vision(p: &Project) -> (&'static str, String, i64) {
    let branch = "feature/demo-12-subtract";
    let root = p._dir.path();
    let person = root.join("person");
    git(
        root,
        &[
            "clone",
            "--quiet",
            p.remote.to_str().unwrap(),
            person.to_str().unwrap(),
        ],
    );
    git(&person, &["config", "user.name", "person"]);
    git(&person, &["config", "user.email", "person@example.invalid"]);
    git(&person, &["switch", "--quiet", "-c", branch]);
    std::fs::write(
        person.join("calc.sh"),
        format!("{TOY_CALC}\n# Gives a minus b.\nsubtract() {{ echo $(($1 - $2)); }}\n"),
    )
    .unwrap();
    git(&person, &["commit", "--quiet", "-am", "Add subtract"]);
    git(&person, &["push", "--quiet", "origin", branch]);
    let head = git(&person, &["rev-parse", "HEAD"]);
    let number = p.server.world.lock().unwrap().open_pull(branch);
    (branch, head, number)
}

#[test]
fn a_ticket_in_progress_outside_vision_is_taken_at_its_step() {
    let p = Project::new();
    let (branch, pushed, number) = started_outside_vision(&p);

    // The queue lists the ticket as not eligible, with its branch and its pull request.
    let pick = p.run(&["pick", "--json"]);
    pick.expect_code(0);
    let row = pick
        .json()
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["key"] == KEY)
        .cloned()
        .unwrap();
    assert_eq!(row["eligible"], false);
    assert_eq!(row["branch"], branch);
    assert_eq!(row["pr"], json!(number));
    let table = p.run(&["pick"]);
    table.expect_code(0);
    assert!(
        table.stdout.contains("yes, at the step where it is"),
        "{table:?}"
    );

    // A plain take reads the facts and the rules, prints the suggestion, and changes nothing.
    let plain = p.run(&["take", KEY]);
    plain.expect_code(1);
    assert_eq!(
        plain.stdout,
        format!(
            "DEMO-12 is in progress outside Vision, on the branch {branch}, with the pull request #{number}. Vision did not take it.\nVision read:\n  the tracker status is To Do\n  the pull request #{number} is open and ready\n  the review state is waiting and no review comment waits\n  CI has no run\nVision thinks that it is at the step in-review: the pull request #{number} is open and ready (rule 4).\nTo take it there:      vision take DEMO-12 --step in-review\n"
        ),
        "{plain:?}"
    );
    assert!(p.row().is_none(), "a plain take writes no row");
    assert_eq!(
        git(&p.clone, &["rev-parse", "--abbrev-ref", "HEAD"]),
        "main",
        "a plain take leaves the clone as it was"
    );

    // An end step is refused, and a step that the adapter does not have too.
    let done = p.run(&["take", KEY, "--step", "done"]);
    done.expect_code(1);
    assert!(
        done.stderr.contains(
            "`done` is the end step of the adapter jira-github, and Vision takes no ticket that is done. Name the step where DEMO-12 waits now"
        ),
        "{done:?}"
    );
    let unknown = p.run(&["take", KEY, "--step", "nowhere"]);
    unknown.expect_code(1);
    assert!(
        unknown
            .stderr
            .contains("the adapter jira-github has no step `nowhere`"),
        "{unknown:?}"
    );
    assert!(p.row().is_none());

    // The take at the step puts the clone on the branch of the pull request, at its head, and
    // holds the ticket there with the pull request.
    let taken = p.run(&["take", KEY, "--step", "in-review"]);
    taken.expect_code(0);
    let clone = p.clone.display().to_string();
    assert_eq!(
        taken.stdout,
        format!(
            "Took DEMO-12 in the clone {clone} on its branch {branch} ({}), with the pull request #{number}. The next tick enters the step in-review.\n",
            &pushed[..12]
        ),
        "{taken:?}"
    );
    assert_eq!(
        git(&p.clone, &["rev-parse", "--abbrev-ref", "HEAD"]),
        branch
    );
    assert_eq!(p.head(), pushed);
    let row = p.row().unwrap();
    assert_eq!(row.state.as_deref(), Some("in-review"));
    assert_eq!(row.branch.as_deref(), Some(branch));
    assert_eq!(row.pr, Some(number));
    assert_eq!(row.head.as_deref(), Some(pushed.as_str()));
    assert!(p.flags().is_empty());
    let take = p
        .ledger()
        .events_of_ticket(KEY)
        .unwrap()
        .into_iter()
        .rfind(|e| e.kind == "take")
        .expect("the event take");
    assert_eq!(take.actor, "user");
    let detail: Value = serde_json::from_str(take.detail.as_deref().unwrap()).unwrap();
    assert_eq!(detail["outside"], true);
    assert_eq!(detail["pr"], json!(number));
    assert_eq!(detail["suggestion"], "in-review");
    assert_eq!(detail["step"], "in-review");
    assert_eq!(
        detail["facts"][1],
        format!("the pull request #{number} is open and ready")
    );

    // The next tick follows the review of the pull request: the lead asks for a change, and
    // the ticket goes to the step comments.
    p.server.team(
        "comment",
        json!({"number": number, "id": 601, "login": "lead", "path": "calc.sh", "line": 5,
               "body": "Add a test for subtract."}),
    );
    p.tick();
    assert_eq!(p.step().as_deref(), Some("comments"), "{}", p.report());
    let again = p.run(&["take", KEY, "--step", "in-review"]);
    again.expect_code(1);
    assert!(
        again.stderr.contains("DEMO-12 is not stopped"),
        "Vision holds the ticket now: {again:?}"
    );
}

/// A ticket that the person started outside Vision never ran the step build, so its folder
/// has no pr.md. A later loop through open-pr, such as after a fix of a QA finding, still pushes,
/// and pr.open confirms the open pull request without a text.
#[test]
fn a_ticket_taken_from_outside_passes_open_pr_without_pr_md() {
    let p = Project::new();
    let (_, pushed, number) = started_outside_vision(&p);
    p.run(&["take", KEY, "--step", "open-pr"]).expect_code(0);
    assert!(p.doc("pr.md").is_none(), "the take writes no pr.md");

    p.tick_to("ci");
    let open = p
        .ledger()
        .operations_of(KEY, Some("pr.open"))
        .unwrap()
        .into_iter()
        .last()
        .expect("the operation pr.open");
    assert_eq!(
        open.external_id.as_deref(),
        Some(number.to_string().as_str())
    );
    assert_eq!(p.row().unwrap().pr, Some(number));
    assert_eq!(p.head(), pushed);
    assert_eq!(
        p.writes().get("POST /repos/acme/widgets/pulls"),
        None,
        "Vision opened no second pull request: {:?}",
        p.writes()
    );
}

#[test]
fn waiting_comments_suggest_comments_and_an_observe_that_fails_suggests_nothing() {
    let p = Project::new();
    let number = stopped_at_review_with_a_pull_request(&p);
    p.server.team(
        "comment",
        json!({"number": number, "id": 502, "login": "lead", "path": "calc.sh", "line": 5,
               "body": "Add a test for subtract."}),
    );
    let take = p.run(&["take", KEY]);
    take.expect_code(1);
    assert!(
        take.stdout.starts_with(&format!(
            "Vision thinks that DEMO-12 is at the step comments, not at the step review where it stopped: the pull request #{number} is open and ready, the review state is comments, and 1 review comment waits (rule 2). It did not continue the ticket.\nVision read:\n  the tracker status is To Do\n  the pull request #{number} is open and ready\n  the review state is comments and 1 review comment waits\n  CI has no run\n  the branch of the clone is at the head of the pull request\nTo continue at comments:  vision take DEMO-12 --step comments\n"
        )),
        "{take:?}"
    );
    assert_eq!(p.flags(), ["stopped"]);

    // The forge does not answer: the rules suggest nothing, so the take starts the stored step
    // again, as a take with no rule does, and says why it suggests nothing.
    let path = p.vision.join("config.toml");
    let mut doc: DocumentMut = std::fs::read_to_string(&path).unwrap().parse().unwrap();
    doc["adapter"]["github_api"] = toml_edit::value("http://127.0.0.1:9");
    std::fs::write(&path, doc.to_string()).unwrap();
    let take = p.run(&["take", KEY]);
    take.expect_code(0);
    assert!(
        take.stdout.starts_with("DEMO-12 starts again at the step review in the clone ")
            && take.stdout.contains(
                " Vision could not check the tracker and the pull request, so it suggested nothing: "
            ),
        "{take:?}"
    );
    assert!(p.flags().is_empty());
    assert_eq!(p.step().as_deref(), Some("review"));
    assert_eq!(p.counts()["visit"]["how"], "restart");
    let detail = last_resumed(&p);
    assert_eq!(
        (
            &detail["step"],
            &detail["how"],
            &detail["failed"],
            &detail["suggestion"],
            &detail["took"]
        ),
        (
            &json!("review"),
            &json!("restart"),
            &json!(true),
            &Value::Null,
            &json!(false)
        )
    );
}

#[test]
fn a_paused_ticket_goes_on_with_retry_step_after_the_rules() {
    let p = Project::new();
    needs_the_person_at_refine(&p);
    // The person takes over the session, then opens the pull request by hand.
    p.run(&["open", KEY]).expect_code(0);
    assert_eq!(p.flags(), ["needs-you", "paused"]);
    let number = pull_request_by_hand(&p);
    let requests = p.server.requests().len();

    let moved = p.run(&["retry", KEY, "--step", "in-review"]);
    moved.expect_code(0);
    assert!(
        moved.stdout.starts_with(&format!(
            "Moved DEMO-12 from the step refine to the step in-review (cleared: needs-you, paused). Vision suggested this step (rule 4): the pull request #{number} is open and ready. Vision read: the tracker status is To Do. The pull request #{number} is open and ready. The review state is waiting and no review comment waits. CI has no run. The branch of the clone is at the head of the pull request. The next tick enters the step in-review."
        )),
        "{moved:?}"
    );
    assert!(
        p.server.requests().len() > requests,
        "the rules called observe"
    );
    assert!(p.flags().is_empty());
    assert_eq!(p.step().as_deref(), Some("in-review"));
    let detail = last_resumed(&p);
    assert_eq!(
        (
            &detail["from"],
            &detail["step"],
            &detail["suggestion"],
            &detail["took"],
            &detail["pr_head"]
        ),
        (
            &json!("refine"),
            &json!("in-review"),
            &json!("in-review"),
            &json!(true),
            &json!(p.head())
        )
    );
    // The next tick acts at in-review: no comment waits, so it waits for the review.
    p.tick();
    assert_eq!(p.step().as_deref(), Some("in-review"), "{}", p.report());
}

#[test]
fn with_no_resume_table_a_take_of_a_stopped_ticket_calls_no_observe_and_starts_the_stored_step() {
    let p = Project::with_adapter(pipeline_support::without_resume(fast_adapter()));
    let requests = needs_the_person_at_refine(&p);
    p.run(&["stop", KEY]).expect_code(0);
    pull_request_by_hand(&p);
    let take = p.run(&["take", KEY]);
    take.expect_code(0);
    assert_eq!(
        take.stdout,
        format!(
            "DEMO-12 starts again at the step refine in the clone {} on the branch {BRANCH}.\n",
            p.clone.display()
        )
    );
    assert_eq!(p.server.requests().len(), requests, "no call of observe");
    assert_eq!(p.flags(), ["needs-you"]);
    let event = p
        .ledger()
        .events_of_ticket(KEY)
        .unwrap()
        .into_iter()
        .rfind(|e| e.kind == "take")
        .expect("the take");
    let detail: Value = serde_json::from_str(event.detail.as_deref().unwrap()).unwrap();
    assert_eq!(detail["restart"], true);
    assert!(!p
        .ledger()
        .events_of_ticket(KEY)
        .unwrap()
        .iter()
        .any(|e| e.kind == "resumed"));
}

#[test]
fn a_push_by_hand_and_a_late_comment_take_a_ticket_back_from_ready_to_merge() {
    let p = Project::new();
    needs_the_person_at_refine(&p);
    // The ticket waits at ready-to-merge after the review, and the person stops it there.
    p.run(&["retry", KEY, "--step", "ready-to-merge"])
        .expect_code(0);
    p.run(&["stop", KEY]).expect_code(0);
    let number = pull_request_by_hand(&p);
    p.server.team(
        "review",
        json!({"number": number, "id": 601, "login": "lead", "state": "APPROVED"}),
    );
    // The person pushes one more commit from another checkout: head.state is forward after
    // the fetch, so the rule of a push by hand takes the ticket back to ci-review.
    let root = p._dir.path();
    let person = root.join("person");
    git(
        root,
        &[
            "clone",
            "--quiet",
            "--branch",
            BRANCH,
            p.remote.to_str().unwrap(),
            person.to_str().unwrap(),
        ],
    );
    git(&person, &["config", "user.name", "person"]);
    git(&person, &["config", "user.email", "person@example.invalid"]);
    std::fs::write(person.join("NOTES.txt"), "A late change.\n").unwrap();
    git(&person, &["add", "--all"]);
    git(&person, &["commit", "--quiet", "-m", "A late change"]);
    git(&person, &["push", "--quiet", "origin", BRANCH]);
    let pushed = git(&person, &["rev-parse", "HEAD"]);
    let take = p.run(&["take", KEY]);
    take.expect_code(1);
    assert!(
        take.stdout.starts_with(
            "Vision thinks that DEMO-12 is at the step ci-review, not at the step ready-to-merge where it stopped (rule 3). This goes back from ready-to-merge: the pull request has later commits than the clone, such as a push by hand. It did not continue the ticket.\nVision read:\n"
        ),
        "{take:?}"
    );
    assert!(
        take.stdout.contains(&format!(
            "  the tracker status is To Do\n  the pull request #{number} is open and ready\n  the review state is approved and no review comment waits\n"
        )) && take.stdout.contains("  the pull request has later commits than the clone, such as a push by hand\n"),
        "{take:?}"
    );
    assert_eq!(p.flags(), ["stopped"]);
    assert_eq!(p.step().as_deref(), Some("ready-to-merge"));

    // The lead comments late: the comments rule comes first, and it takes the ticket back to
    // comments, with each fact that Vision read.
    p.server.team(
        "comment",
        json!({"number": number, "id": 503, "login": "lead", "path": "calc.sh", "line": 5,
               "body": "One more test, please."}),
    );
    let take = p.run(&["take", KEY]);
    take.expect_code(1);
    assert!(
        take.stdout.starts_with(
            "Vision thinks that DEMO-12 is at the step comments, not at the step ready-to-merge where it stopped (rule 2). This goes back from ready-to-merge: the review state is comments and 1 review comment waits. It did not continue the ticket.\nVision read:\n"
        ),
        "{take:?}"
    );
    assert!(
        take.stdout.ends_with(
            "To continue at comments:        vision take DEMO-12 --step comments\nTo continue at ready-to-merge:  vision take DEMO-12 --step ready-to-merge\n"
        ),
        "{take:?}"
    );
    let moved = p.run(&["take", KEY, "--step", "comments"]);
    moved.expect_code(0);
    assert!(
        moved.stdout.starts_with(
            "Moved DEMO-12 from the step ready-to-merge to the step comments (cleared: stopped). Vision suggested this step (rule 2). This goes back from ready-to-merge: the review state is comments and 1 review comment waits. Vision read: the tracker status is To Do. "
        ),
        "{moved:?}"
    );
    assert!(p.flags().is_empty());
    assert_eq!(p.step().as_deref(), Some("comments"));
    assert_eq!(p.head(), pushed, "the clone took the head of the person");
    let detail = last_resumed(&p);
    assert_eq!(
        (
            &detail["from"],
            &detail["step"],
            &detail["suggestion"],
            &detail["took"],
            &detail["back"],
            &detail["rule"]
        ),
        (
            &json!("ready-to-merge"),
            &json!("comments"),
            &json!("comments"),
            &json!(true),
            &json!(true),
            &json!(2)
        )
    );
    let facts: Vec<String> = serde_json::from_value(detail["facts"].clone()).unwrap();
    assert_eq!(facts[0], "the tracker status is To Do");
    assert!(
        facts.contains(&"the review state is comments and 1 review comment waits".to_string()),
        "{facts:?}"
    );
    let log = p.run(&["log", KEY]);
    log.expect_code(0);
    assert!(
        log.stdout.contains(
            "user resumed the ticket at comments, as Vision suggested, back from ready-to-merge"
        ),
        "{log:?}"
    );
}

#[test]
fn an_approval_takes_a_ticket_stopped_at_comments_on_to_ready_to_merge() {
    let p = Project::new();
    needs_the_person_at_refine(&p);
    p.run(&["retry", KEY, "--step", "comments"]).expect_code(0);
    p.run(&["stop", KEY]).expect_code(0);
    let number = pull_request_by_hand(&p);
    p.server.team(
        "review",
        json!({"number": number, "id": 601, "login": "lead", "state": "APPROVED"}),
    );
    // in-review comes before comments, so its rule does not apply: the rule of the approval
    // does, as in-review would route it.
    let take = p.run(&["take", KEY]);
    take.expect_code(1);
    assert!(
        take.stdout.starts_with(
            "Vision thinks that DEMO-12 is at the step ready-to-merge, not at the step comments where it stopped"
        ) && take.stdout.contains("(rule 5)"),
        "{take:?}"
    );
    assert_eq!(p.step().as_deref(), Some("comments"));
    assert_eq!(p.flags(), ["stopped"]);
}

#[test]
fn a_push_by_hand_wins_over_a_commit_of_vision_that_never_left_the_machine() {
    let p = Project::new();
    let path = p.vision.join("config.toml");
    let mut doc: DocumentMut = std::fs::read_to_string(&path).unwrap().parse().unwrap();
    doc["git"]["name"] = toml_edit::value("factory[bot]");
    doc["git"]["email"] = toml_edit::value("factory@example.invalid");
    std::fs::write(&path, doc.to_string()).unwrap();
    needs_the_person_at_refine(&p);
    p.run(&["retry", KEY, "--step", "ready-to-merge"])
        .expect_code(0);
    p.run(&["stop", KEY]).expect_code(0);
    let number = pull_request_by_hand(&p);
    p.server.team(
        "review",
        json!({"number": number, "id": 601, "login": "lead", "state": "APPROVED"}),
    );
    // A commit of Vision that no push sent, such as a fix whose push a gate refused.
    std::fs::write(p.clone.join("VISION.txt"), "A fix of Vision.\n").unwrap();
    git(&p.clone, &["add", "--all"]);
    let status = Command::new("git")
        .args(["commit", "--quiet", "-m", "A fix of Vision"])
        .current_dir(&p.clone)
        .env("GIT_AUTHOR_NAME", "factory[bot]")
        .env("GIT_AUTHOR_EMAIL", "factory@example.invalid")
        .env("GIT_COMMITTER_NAME", "factory[bot]")
        .env("GIT_COMMITTER_EMAIL", "factory@example.invalid")
        .status()
        .unwrap();
    assert!(status.success());
    let own = p.head();
    // A gate held the push of that commit for the person.
    let args = json!({"args": {}, "branch": BRANCH, "head": own}).to_string();
    assert!(p
        .ledger()
        .add_operation(&vision::ledger::operations::NewOperation {
            id: "op-held",
            idem: "idem-held",
            source: "rule",
            turn: None,
            ticket: KEY,
            verb: "git.push",
            args: &args,
            mode: "auto",
            result: r#"{"detail":"held by the sensitive gate"}"#,
        })
        .unwrap());
    // The person goes on from another checkout and pushes: the branches went apart.
    let root = p._dir.path();
    let person = root.join("person");
    git(
        root,
        &[
            "clone",
            "--quiet",
            "--branch",
            BRANCH,
            p.remote.to_str().unwrap(),
            person.to_str().unwrap(),
        ],
    );
    git(&person, &["config", "user.name", "person"]);
    git(&person, &["config", "user.email", "person@example.invalid"]);
    std::fs::write(person.join("NOTES.txt"), "A late change.\n").unwrap();
    git(&person, &["add", "--all"]);
    git(&person, &["commit", "--quiet", "-m", "A late change"]);
    git(&person, &["push", "--quiet", "origin", BRANCH]);
    let pushed = git(&person, &["rev-parse", "HEAD"]);

    // The pull request went on without the commit of Vision: the rule of a push by hand holds,
    // and nothing moves before the person continues.
    let take = p.run(&["take", KEY]);
    take.expect_code(1);
    assert!(
        take.stdout.starts_with(
            "Vision thinks that DEMO-12 is at the step ci-review, not at the step ready-to-merge where it stopped (rule 3)."
        ),
        "{take:?}"
    );
    assert!(
        take.stdout.contains(
            "  the pull request went on without 1 commit of Vision that never left the machine\n"
        ),
        "{take:?}"
    );
    assert_eq!(p.head(), own, "a plain take moves nothing");

    let moved = p.run(&["take", KEY, "--step", "ci-review"]);
    moved.expect_code(0);
    let kept = format!("refs/vision/kept/{BRANCH}/{}", &own[..12]);
    assert!(
        moved.stdout.contains(&format!(
            "The clone had 1 commit of Vision that never left the machine ({}). The pull request #{number} went on without it, so the branch of the clone moved to its head {}. The old tip stays in {kept}.",
            &own[..12],
            &pushed[..12]
        )),
        "{moved:?}"
    );
    assert!(p.flags().is_empty(), "{:?}", p.flags());
    assert_eq!(p.step().as_deref(), Some("ci-review"));
    assert_eq!(p.head(), pushed, "the push of the person wins");
    assert!(p.clone.join("NOTES.txt").is_file());
    assert!(!p.clone.join("VISION.txt").exists());
    assert_eq!(
        git(&p.clone, &["rev-parse", &kept]),
        own,
        "no commit goes away"
    );
    let detail = last_resumed(&p);
    assert_eq!(detail["dropped"], json!([own]));
    assert_eq!(detail["kept"], json!(kept));
    assert_eq!(detail["head"], json!({"from": own, "to": pushed}));
    // The held push of the dropped commit ends, so no approval sends it.
    let held = p
        .ledger()
        .operations_of(KEY, None)
        .unwrap()
        .into_iter()
        .find(|op| op.id == "op-held")
        .unwrap();
    assert_eq!(held.state, "failed");
    assert!(
        held.result
            .as_deref()
            .unwrap()
            .contains("\"dropped\":\"replaced\""),
        "{held:?}"
    );
}

/// The adapter of the repo with an interrupt on the wait step ready: a waiting comment or a
/// change request takes the ticket to comments before pr.draft counts.
fn interrupt_adapter() -> String {
    let watch = "on = { \"false\" = \"in-review\" }\n";
    let text = fast_adapter();
    assert_eq!(text.matches(watch).count(), 1);
    text.replace(
        watch,
        &format!("{watch}interrupt = {{ \"review.state\" = {{ comments = \"comments\", changes = \"comments\" }} }}\n"),
    )
}

#[test]
fn a_late_comment_interrupts_a_wait_step_before_its_watch() {
    // The pull request is ready, and a comment waits: the interrupt moves the ticket to
    // comments, not the watch of pr.draft to in-review.
    let p = Project::with_adapter(interrupt_adapter());
    needs_the_person_at_refine(&p);
    p.run(&["retry", KEY, "--step", "ready"]).expect_code(0);
    let number = pull_request_by_hand(&p);
    p.server.team(
        "comment",
        json!({"number": number, "id": 504, "login": "lead", "path": "calc.sh", "line": 5,
               "body": "A late comment."}),
    );
    p.tick_until("the move from ready", |p| {
        p.step().as_deref() != Some("ready")
    });
    assert_eq!(p.step().as_deref(), Some("comments"), "{}", p.report());
    let step = p
        .ledger()
        .events_of_ticket(KEY)
        .unwrap()
        .into_iter()
        .rfind(|e| e.kind == "step")
        .expect("the move of the interrupt");
    let detail: Value = serde_json::from_str(step.detail.as_deref().unwrap()).unwrap();
    assert_eq!(
        (
            &detail["from"],
            &detail["to"],
            &detail["how"],
            &detail["interrupt"],
            &detail["value"]
        ),
        (
            &json!("ready"),
            &json!("comments"),
            &json!("interrupt review.state = comments"),
            &json!("review.state"),
            &json!("comments")
        )
    );
    assert_eq!(p.counts()["entries"]["comments"], 1, "an entry of comments");

    // With no comment, no interrupt fires, and the watch moves the ticket as before.
    let p = Project::with_adapter(interrupt_adapter());
    needs_the_person_at_refine(&p);
    p.run(&["retry", KEY, "--step", "ready"]).expect_code(0);
    pull_request_by_hand(&p);
    p.tick_until("the move from ready", |p| {
        p.step().as_deref() != Some("ready")
    });
    assert_eq!(p.step().as_deref(), Some("in-review"), "{}", p.report());
    let step = p
        .ledger()
        .events_of_ticket(KEY)
        .unwrap()
        .into_iter()
        .rfind(|e| e.kind == "step")
        .unwrap();
    let detail: Value = serde_json::from_str(step.detail.as_deref().unwrap()).unwrap();
    assert_eq!(detail["how"], "on false");
    assert_eq!(detail["interrupt"], Value::Null);
}

/// The adapter of the repo with the steps of the env staging before ready-to-merge: a deploy
/// whose merge conflict goes to merge-staging, where Merger resolves it and deploy.merge pushes
/// the merge.
fn conflict_adapter() -> String {
    let anchor = "\n[[step]]\nid = \"ready-to-merge\"";
    let text = fast_adapter();
    assert_eq!(text.matches(anchor).count(), 1);
    text.replace(
        anchor,
        &format!(
            r#"
[[step]]
id = "deploy-staging"
kind = "wait"
start = [{{ verb = "deploy", args = {{ env = "staging" }} }}]
watch = "envs.staging.holds"
on = {{ "true" = "ready-to-merge" }}
wait = ["false", "none"]
conflict = "merge-staging"
every = 1

[[step]]
id = "merge-staging"
kind = "turn"
agent = "merger"
turn = "worker"
reads = ["deploy-conflict.md"]
expects = ["doc:merge.md"]
after = [{{ verb = "deploy.merge", args = {{ env = "staging" }} }}]
next = "ready-to-merge"
{anchor}"#
        ),
    )
}

#[test]
fn a_deploy_that_does_not_merge_goes_to_merger_and_deploy_merge_pushes_the_merge() {
    let p = Project::with_adapter(conflict_adapter());
    // The env staging, with the branch staging, in the [adapter] table of this test only.
    let path = p.vision.join("config.toml");
    let mut doc: DocumentMut = std::fs::read_to_string(&path).unwrap().parse().unwrap();
    let mut env = toml_edit::InlineTable::new();
    env.insert("name", "staging".into());
    env.insert("branch", "staging".into());
    let mut envs = toml_edit::Array::new();
    envs.push(env);
    doc["adapter"]["envs"] = toml_edit::value(envs);
    std::fs::write(&path, doc.to_string()).unwrap();

    needs_the_person_at_refine(&p);
    pull_request_by_hand(&p);
    let head = p.head();
    // Another ticket put multiply on staging, at the place where this ticket adds subtract.
    let team = TempDir::new().unwrap();
    let work = team.path().join("team");
    git(
        team.path(),
        &[
            "clone",
            "--quiet",
            p.remote.to_str().unwrap(),
            work.to_str().unwrap(),
        ],
    );
    git(
        &work,
        &["checkout", "--quiet", "-b", "staging", "origin/main"],
    );
    let multiply = "\n# Gives a times b.\nmultiply() { echo $(($1 * $2)); }\n";
    std::fs::write(work.join("calc.sh"), format!("{TOY_CALC}{multiply}")).unwrap();
    git(&work, &["commit", "--quiet", "-am", "Add multiply"]);
    git(&work, &["push", "--quiet", "origin", "staging"]);
    let tip = git(&work, &["rev-parse", "HEAD"]);

    // Merger keeps both functions in the file of the merge folder. It does not commit: the
    // sandbox of a real turn cannot write the git folder of a worktree.
    let both =
        format!("{TOY_CALC}{multiply}\n# Gives a minus b.\nsubtract() {{ echo $(($1 - $2)); }}\n");
    let resolved = team.path().join("calc.sh");
    std::fs::write(&resolved, &both).unwrap();
    let command = format!("cp '{}' .vision/merge/staging/calc.sh", resolved.display());
    p.scenario(
        "merger",
        "merge-staging",
        json!([runs_then(
            &command,
            result(
                "Merged.",
                &[(
                    "merge.md",
                    "# Merge of DEMO-12 into staging\n\n## calc.sh\n\nstaging added multiply, and the ticket added subtract: both stay.\n"
                )]
            )
        )]),
    );

    p.run(&["retry", KEY, "--step", "deploy-staging"])
        .expect_code(0);
    p.tick_until("the merge on staging", |p| {
        p.step().as_deref() == Some("ready-to-merge")
    });

    // The deploy failed on the conflict, and the ticket went to merge-staging with no flag.
    let moved = p
        .ledger()
        .events_of_ticket(KEY)
        .unwrap()
        .into_iter()
        .filter(|e| e.kind == "step")
        .find_map(|e| {
            let detail: Value = serde_json::from_str(e.detail.as_deref()?).ok()?;
            (detail["to"] == "merge-staging").then_some(detail)
        })
        .unwrap_or_else(|| panic!("no move to merge-staging. {}", p.report()));
    assert_eq!(
        (&moved["from"], &moved["how"], &moved["conflict"]),
        (
            &json!("deploy-staging"),
            &json!("conflict deploy"),
            &json!("deploy")
        )
    );
    assert!(
        moved["reason"]
            .as_str()
            .unwrap_or("")
            .contains("conflict in calc.sh"),
        "{moved}"
    );
    let conflict = p
        .doc("deploy-conflict.md")
        .expect("the document of the conflict");
    assert!(
        conflict.contains("- `calc.sh`") && conflict.contains(&tip),
        "{conflict}"
    );
    assert!(p.doc("merge.md").is_some());
    assert!(p.flags().is_empty(), "{:?}", p.flags());

    // deploy.merge committed the merge of the head into the tip, and pushed it as a
    // fast-forward of staging.
    let merge = git(&p.remote, &["rev-parse", "refs/heads/staging"]);
    assert_eq!(
        git(&p.remote, &["rev-list", "--parents", "-n", "1", &merge]),
        format!("{merge} {tip} {head}")
    );
    assert_eq!(
        git(&p.remote, &["log", "-1", "--format=%s", &merge]),
        format!("Merge {KEY} ({}) into staging", &head[..12])
    );
    assert_eq!(
        git(&p.remote, &["show", "refs/heads/staging:calc.sh"]),
        both.trim_end()
    );
    // The merge folder is gone, and the clone keeps its branch, its head, and a clean tree.
    assert!(!p.clone.join(".vision/merge/staging").exists());
    assert_eq!(git(&p.clone, &["symbolic-ref", "--short", "HEAD"]), BRANCH);
    assert_eq!(p.head(), head);
    assert_eq!(git(&p.clone, &["status", "--porcelain"]), "");
}

#[test]
fn a_round_of_comments_keeps_the_marks_only_after_its_end_facts_hold() {
    // A note of a bot waits, and the Shepherd answers nothing. Its first result leaves a file
    // outside a commit, so the end fact clean fails, and the note keeps waiting. The resume
    // commits the file, the end facts hold, and Vision keeps the mark of the note.
    let p = Project::new();
    needs_the_person_at_refine(&p);
    p.run(&["retry", KEY, "--step", "in-review"]).expect_code(0);
    let number = pull_request_by_hand(&p);
    p.server.team(
        "issue_comment",
        json!({"number": number, "id": 705, "login": "preview-bot", "body": "The preview is ready."}),
    );
    p.scenario(
        "shepherd",
        "comments",
        json!([
            runs_then(
                "echo 'a note' > notes.txt",
                result("Nothing needs a reply.", &[("verdicts.json", "[]")])
            ),
            runs_then(
                "git add notes.txt && git commit -q -m 'Keep the note'",
                result("Nothing needs a reply.", &[("verdicts.json", "[]")])
            )
        ]),
    );
    let failed = |p: &Project| {
        p.ledger().events_of_ticket(KEY).unwrap().iter().any(|e| {
            e.kind == "end-fact"
                && e.detail.as_deref().is_some_and(|d| {
                    d.contains("\"held\":false") || d.contains("\"fact\":\"clean\"")
                })
        })
    };
    p.tick_until("the failed end fact of the first result", failed);
    assert_eq!(p.step().as_deref(), Some("comments"), "{}", p.report());
    assert!(p
        .doc("comments.json")
        .unwrap()
        .contains("\"mark\": \"conversation:705:705\""));
    assert_eq!(
        p.doc("comments-seen.json"),
        None,
        "a result whose end facts failed keeps no mark"
    );
    p.tick_until("the end of the round", |p| {
        p.step().as_deref() != Some("comments")
    });
    let seen: Value = serde_json::from_str(&p.doc("comments-seen.json").unwrap()).unwrap();
    assert_eq!(seen, json!(["conversation:705:705"]));
}
