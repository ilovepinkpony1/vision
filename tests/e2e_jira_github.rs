//! The whole path of one ticket with the adapter for Jira and GitHub (design section 11.3): a
//! temporary home, the fake `claude`, a clone of a toy project, and a fake Jira and GitHub with
//! state (`adapters/jira-github/tests/serve_world.py`) on a free localhost port. `vision take
//! DEMO-12` and then `vision tick` in a loop carry the ticket from the tracker to a merged pull
//! request, through each step of the `adapter.toml` of the repo. The test plays the person (it
//! approves the plan and the operations in the mode ask) and the team (it marks the pull
//! request ready, reviews, comments, approves, and merges with the control calls of the world).
//!
//! Nothing here reaches the network: each address of `[adapter]` points at the fake server, and
//! git pushes to a local bare repo.
//!
//! To look into a run, set `VISION_E2E_DUMP` to a file: the test writes the step, the turns, and
//! the log there before each stage and at the end. A failed stage prints the same report.

mod pipeline_support;

use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::time::{Duration, Instant};

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

const TOY_CALC: &str = "\"\"\"A tiny calculator.\"\"\"\n\n\ndef add(a, b):\n    \"\"\"Gives a plus b.\"\"\"\n    return a + b\n";
const TOY_TEST: &str = "import unittest\n\nfrom calc import add\n\n\nclass AddTest(unittest.TestCase):\n    def test_add(self):\n        self.assertEqual(add(2, 3), 5)\n\n\nif __name__ == \"__main__\":\n    unittest.main()\n";
const TOY_CI: &str = "#!/bin/sh\n# The CI of the toy project: each test must pass.\nset -e\npython3 -B -m unittest discover -q\n";

/// The build: subtract with its test, one more test of add (it passes on the base too, so the
/// plan declares it green), and a ci.sh that fails while TODO.txt exists.
const BUILD: &str = "printf '\\n\\ndef subtract(a, b):\\n    \"\"\"Gives a minus b.\"\"\"\\n    return a - b\\n' >> calc.py && printf 'import unittest\\n\\nfrom calc import subtract\\n\\n\\nclass SubtractTest(unittest.TestCase):\\n    def test_subtract(self):\\n        self.assertEqual(subtract(5, 3), 2)\\n' > test_subtract.py && printf '\\n\\nclass ZeroTest(unittest.TestCase):\\n    def test_zero(self):\\n        self.assertEqual(add(0, 0), 0)\\n' >> test_calc.py && printf 'test ! -f TODO.txt\\n' >> ci.sh && echo 'Name subtract in the README.' > TODO.txt && git add -A && git commit -q -m 'Add subtract'";

const PLAN: &str = "# Plan of DEMO-12\n\n## Decisions\n\n- subtract goes next to add in calc.py.\n\n## Proof\n\n- test_subtract.py fails on the base and passes after the change.\n- **Proof:** green test_calc.py\n";

/// The fake Jira and GitHub as a child process. Its standard input stays open while the test
/// runs; the end of the test closes it, and the world ends.
struct Server {
    child: Child,
    stdin: Option<ChildStdin>,
    port: u16,
    dir: PathBuf,
}

impl Server {
    fn start(dir: &Path, remote: &Path) -> Server {
        std::fs::create_dir_all(dir).unwrap();
        let script =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("adapters/jira-github/tests/serve_world.py");
        let mut command = Command::new("python3");
        command
            .arg("-B")
            .arg(&script)
            .arg("--dir")
            .arg(dir)
            .arg("--remote")
            .arg(remote)
            .args([
                "--issue",
                KEY,
                "--title",
                "Add subtract to the calculator",
                "--description",
                "The calculator can only add. Add a function subtract(a, b) that gives a minus b, with a test.",
            ])
            .env_clear()
            .env("PATH", std::env::var_os("PATH").unwrap_or_default())
            .env("HOME", dir)
            .env("LANG", "C.UTF-8")
            .env("FAKE_JIRA_TOKEN", JIRA_TOKEN)
            .env("FAKE_FORGE_TOKEN", FORGE_TOKEN)
            .env("FAKE_JIRA_USER", JIRA_USER)
            .stdin(Stdio::piped())
            .stdout(std::fs::File::create(dir.join("stdout.log")).unwrap())
            .stderr(std::fs::File::create(dir.join("stderr.log")).unwrap());
        if let Some(tmp) = std::env::var_os("TMPDIR") {
            command.env("TMPDIR", tmp);
        }
        let mut child = command.spawn().expect("start python3 serve_world.py");
        let stdin = child.stdin.take();
        // Wait for a fact: the port file, which the world writes once it listens.
        let port_file = dir.join("port");
        let until = Instant::now() + Duration::from_secs(60);
        let port = loop {
            if let Ok(text) = std::fs::read_to_string(&port_file) {
                if let Ok(port) = text.trim().parse::<u16>() {
                    break port;
                }
            }
            if let Ok(Some(status)) = child.try_wait() {
                panic!(
                    "the world ended with {status} before it listened: {}",
                    std::fs::read_to_string(dir.join("stderr.log")).unwrap_or_default()
                );
            }
            assert!(Instant::now() < until, "the world never wrote its port");
            std::thread::sleep(Duration::from_millis(50));
        };
        Server {
            child,
            stdin,
            port,
            dir: dir.to_path_buf(),
        }
    }

    fn url(&self) -> String {
        format!("http://127.0.0.1:{}", self.port)
    }

    /// One HTTP call to the world, for the control calls of the team.
    fn call(&self, method: &str, path: &str, body: Option<Value>) -> (u16, Value) {
        let mut stream =
            TcpStream::connect(("127.0.0.1", self.port)).expect("connect to the world");
        stream
            .set_read_timeout(Some(Duration::from_secs(120)))
            .unwrap();
        let text = body.map(|b| b.to_string()).unwrap_or_default();
        write!(
            stream,
            "{method} {path} HTTP/1.1\r\nHost: 127.0.0.1:{}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{text}",
            self.port,
            text.len()
        )
        .unwrap();
        let mut response = String::new();
        stream.read_to_string(&mut response).unwrap();
        let status = response
            .split_whitespace()
            .nth(1)
            .and_then(|s| s.parse().ok())
            .unwrap_or(0);
        let body = response
            .split_once("\r\n\r\n")
            .map(|(_, b)| b)
            .unwrap_or("");
        (status, serde_json::from_str(body).unwrap_or(Value::Null))
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
        std::fs::read_to_string(self.dir.join("requests.jsonl"))
            .unwrap_or_default()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect()
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        drop(self.stdin.take());
        let until = Instant::now() + Duration::from_secs(10);
        while Instant::now() < until {
            if let Ok(Some(_)) = self.child.try_wait() {
                return;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
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
    fn new() -> Project {
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
            ("calc.py", TOY_CALC),
            ("test_calc.py", TOY_TEST),
            ("ci.sh", TOY_CI),
            (".gitignore", "__pycache__/\n*.pyc\n"),
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
        let server = Server::start(&root.join("world"), &remote);
        let path = Project {
            server,
            vision,
            user_home,
            remote,
            clone,
            _dir: dir,
        };
        path.run(&["init"]).expect_code(0);
        path.configure();
        path
    }

    /// The configuration: the project, the fake claude, the clone, the `[adapter]` keys that
    /// point at the world, and the secrets from the environment of the `vision` process. And
    /// a private copy of the adapter of the repo with short waits and the scripts of the repo.
    fn configure(&self) {
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
test_command = "python3 -B -m unittest -q"
test_glob = ["test_*.py"]
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

        let repo = Path::new(env!("CARGO_MANIFEST_DIR")).join("adapters/jira-github");
        let mut text = std::fs::read_to_string(repo.join("adapter.toml")).unwrap();
        for every in ["every = 30", "every = 60", "every = 120"] {
            text = text.replace(every, "every = 1");
        }
        assert!(!text.contains("every = 30") && text.contains("every = 1"));
        let text = text.replacen(
            "base = \"origin/main\"",
            &format!(
                "base = \"origin/main\"\nscripts = {}",
                serde_json::to_string(&repo.display().to_string()).unwrap()
            ),
            1,
        );
        let dir = self.vision.join("adapters/jira-github");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("adapter.toml"), text).unwrap();
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

    /// Ticks until an operation of `verb` waits for the person, then approves it with the
    /// exact command of `vision needs`. Gives the commit of the approval.
    fn approve(&self, verb: &str) -> String {
        self.tick_until(&format!("the approval of {verb}"), |p| {
            p.needs()
                .iter()
                .any(|n| n["kind"] == "operation" && n["id"].as_str().unwrap().starts_with(verb))
        });
        let need = self
            .needs()
            .into_iter()
            .find(|n| n["kind"] == "operation")
            .unwrap();
        let id = need["id"].as_str().unwrap().to_string();
        let (named, commit) = id.split_once(' ').unwrap();
        assert_eq!(named, verb, "{need}");
        let run = self.run(&["approve", KEY, verb, commit]);
        assert_eq!(run.code, 0, "{run:#?}");
        commit.to_string()
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
                ("refine.md", "# Refine\n\nsubtract goes next to add in calc.py.\n"),
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
            "printf '\\n# subtract keeps the sign of a minus b.\\n' >> calc.py && git commit -q -am 'Keep the sign of subtract'",
            result(
                "Fixed B1.",
                &[("fix-notes.md", "# Fix notes\n\nB1 is fixed in the last commit.\n")]
            )
        )]),
    );
    let verdicts = json!([
        {"comment_id": 501, "kind": "inline", "verdict": "valid", "fix_commit": "", "reply": "Renamed to test_calc_subtract.py."},
        {"comment_id": 601, "kind": "review", "verdict": "valid", "fix_commit": "", "reply": "Done: the test file is test_calc_subtract.py now."},
        {"comment_id": 701, "kind": "conversation", "verdict": "out-of-scope", "fix_commit": "", "reply": "Division belongs to a different ticket."}
    ])
    .to_string();
    p.scenario(
        "shepherd",
        "comments",
        json!([runs_then(
            "git mv test_subtract.py test_calc_subtract.py && git commit -q -m 'Name the test of subtract like the others'",
            result("Answered three comments.", &[("verdicts.json", &verdicts)])
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

    // build: tracker.move waits for the person, then the build, the proof, the push, and the
    // draft pull request.
    let moved_at = p.approve("tracker.move");
    assert_eq!(
        moved_at, base,
        "the move waits at the commit of its first request"
    );
    assert_eq!(p.server.state()["issues"][KEY]["status"], "In Progress");
    p.tick_to("review");
    let built = p.head();
    assert_ne!(built, base);
    assert_eq!(
        git(&p.remote, &["rev-parse", &format!("refs/heads/{BRANCH}")]),
        built,
        "the branch is on the remote"
    );
    let pull = p.server.state()["pulls"]["7"].clone();
    assert_eq!(pull["title"], "Add subtract to the calculator");
    assert_eq!(pull["draft"], true);
    assert_eq!(
        pull["base"]["ref"], "main",
        "the base branch comes from base_ref"
    );
    assert_eq!(p.row().unwrap().pr, Some(7));
    assert!(
        p.doc("proof.txt").unwrap().contains("test_subtract.py"),
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
        ["python3 -B -m unittest -q test_subtract.py"],
        "the plan declares test_calc.py green, so only test_subtract.py runs on the base"
    );

    // review: no second reviewer is set, so the review turn gets null for its document.
    p.tick_to("ci");
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
        ["ci.log", "github.pr", "jira.issue", "jira.search"]
    );
    assert!(review_facts["requests"][0]["about"]
        .as_str()
        .unwrap()
        .contains("args: run"));

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
    p.tick_to("qa");
    let fixed = p.head();
    assert_ne!(fixed, built);
    let checks: Vec<(String, String)> = p
        .ledger()
        .checks_of(KEY, None)
        .unwrap()
        .into_iter()
        .filter(|c| c.kind == "ci")
        .map(|c| (c.head, c.result))
        .collect();
    assert_eq!(
        checks,
        [
            (built.clone(), "red".to_string()),
            (fixed.clone(), "green".to_string())
        ]
    );

    // qa: one bug and its fix, which goes through CI, then QA with no bug.
    p.tick_to("fix-qa");
    assert!(
        p.doc("app-local.json").unwrap().contains("/health"),
        "app-start ran before QA"
    );
    p.tick_to("ci");
    p.tick_to("qa");
    let qa_fixed = p.head();
    assert_ne!(qa_fixed, fixed);
    p.tick_until("the step ready", |p| p.step().as_deref() == Some("ready"));
    let findings = p.ledger().findings_of(KEY).unwrap();
    let rows: Vec<(&str, &str)> = findings
        .iter()
        .map(|f| (f.source.as_str(), f.verdict.as_str()))
        .collect();
    assert_eq!(rows, [("review", "rejected"), ("qa", "kept")]);

    // ready: tracker.move waits for the person; the person marks the pull request ready.
    p.approve("tracker.move");
    assert_eq!(p.server.state()["issues"][KEY]["status"], "In Review");
    p.tick();
    assert_eq!(p.step().as_deref(), Some("ready"), "the draft waits");
    p.server.team("ready", json!({"number": 7}));
    p.tick_to("in-review");

    // A review that asks for changes, with one inline comment and one conversation comment.
    p.tick();
    assert_eq!(p.step().as_deref(), Some("in-review"), "nothing waits yet");
    p.server.team(
        "comment",
        json!({"number": 7, "id": 501, "login": "lead", "path": "test_subtract.py", "line": 1,
               "body": "Name this file test_calc_subtract.py."}),
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
        [(501, "inline"), (601, "review"), (701, "conversation")]
    );

    // The Shepherd round: a fix, then the replies and the new review request after approve.
    let reply_at = p.approve("review.reply");
    let shepherd = p.head();
    assert_eq!(reply_at, shepherd);
    assert_ne!(shepherd, qa_fixed);
    let state = p.server.state();
    let inline: Vec<&Value> = state["comments"]["7"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|c| c["in_reply_to_id"] == 501)
        .collect();
    assert_eq!(inline.len(), 1);
    assert_eq!(inline[0]["body"], "Renamed to test_calc_subtract.py.");
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
            "> Please follow the names of the other tests.\n\nDone: the test file is test_calc_subtract.py now.\n\n<!-- vision-reply review 601 -->",
            "> Can the calculator also divide?\n\nDivision belongs to a different ticket.\n\n<!-- vision-reply conversation 701 -->"
        ]
    );
    p.approve("review.rerequest");
    assert_eq!(
        p.server.state()["pulls"]["7"]["requested_reviewers"],
        json!([{"login": "lead"}])
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

    // A new CI run of the commit of the comments, then the approval and the merge.
    p.tick_to("ci-review");
    p.tick_to("in-review");
    assert_eq!(p.server.state()["runs"].as_array().unwrap().len(), 4);
    p.tick();
    assert_eq!(
        p.step().as_deref(),
        Some("in-review"),
        "the review is asked again, and nothing waits"
    );
    p.server.team(
        "review",
        json!({"number": 7, "id": 602, "login": "lead", "state": "APPROVED", "body": ""}),
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

    // The path through each step of adapter.toml.
    let spans: Vec<String> = p
        .ledger()
        .spans_of(Some(KEY))
        .unwrap()
        .into_iter()
        .map(|s| s.state)
        .collect();
    assert_eq!(
        spans,
        [
            "refine",
            "plan",
            "plan-ready",
            "build",
            "review",
            "ci",
            "ci-rerun",
            "fix-ci",
            "ci",
            "qa",
            "fix-qa",
            "ci",
            "qa",
            "ready",
            "in-review",
            "comments",
            "ci-review",
            "in-review",
            "ready-to-merge",
            "done"
        ]
    );

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
            ("ci.run", 4),
            ("git.push", 4),
            ("pr.open", 1),
            ("review.reply", 1),
            ("review.rerequest", 1),
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
                4
            ),
            ("POST /repos/acme/widgets/issues/7/comments".to_string(), 2),
            ("POST /repos/acme/widgets/pulls".to_string(), 1),
            (
                "POST /repos/acme/widgets/pulls/7/comments/501/replies".to_string(),
                1
            ),
            (
                "POST /repos/acme/widgets/pulls/7/requested_reviewers".to_string(),
                1
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
    assert_eq!(dispatched.len(), 4, "one run for each commit");
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
    // The path takes about 50 s on a laptop. The time is printed, not asserted: a busy machine
    // makes each process slower, and that says nothing about the code.
    eprintln!("the whole path took {:?}", started.elapsed());
}
