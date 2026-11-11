//! Helpers of the tests of the adapter jira-github: a fake HTTP server on a free localhost port,
//! a fake Jira and GitHub with state on it, temporary git repos, and a runner that calls the
//! built-in adapter the way the executor does (`vision adapter-script`, a plain environment,
//! and the secrets).
//!
//! No test reaches the network: each base address of the configuration points at the fake
//! server.

// Each test file uses a different part of these helpers.
#![allow(dead_code)]

pub mod server;
pub mod world;

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use serde_json::{json, Map, Value};
use tempfile::TempDir;

#[allow(unused_imports)]
pub use server::{FakeServer, Record, Reply};

/// Fake secrets. A test fails when one of them, or its base64 form, shows up where it must not.
pub const SECRETS: [(&str, &str); 3] = [
    ("JIRA_TOKEN", "jira-fake-token-7c1d2e3f4a5b"),
    ("FORGE_TOKEN", "forge-fake-token-9e8d7c6b5a41"),
    ("PR_TOKEN", "pr-fake-token-31a2b3c4d5e6"),
];

pub const KEY: &str = "DEMO-12";
pub const BRANCH: &str = "vision/DEMO-12";
pub const REPO: &str = "acme/widgets";
/// The core gives each call the `base` of adapter.toml as base_ref.
pub const BASE_REF: &str = "origin/main";

/// The value of a fake secret.
pub fn secret(name: &str) -> &'static str {
    SECRETS.iter().find(|(n, _)| *n == name).unwrap().1
}

/// The fake secrets without some of them.
pub fn secrets_without(names: &[&str]) -> Vec<(String, String)> {
    SECRETS
        .iter()
        .filter(|(n, _)| !names.contains(n))
        .map(|(n, v)| (n.to_string(), v.to_string()))
        .collect()
}

/// Each fake secret.
pub fn all_secrets() -> Vec<(String, String)> {
    secrets_without(&[])
}

/// Each secret, and the forms in which a client can leak it.
pub fn secret_forms() -> Vec<String> {
    let mut forms = Vec::new();
    for (_, value) in SECRETS {
        forms.push(value.to_string());
        forms.push(vision::util::base64::encode(
            format!("x-access-token:{value}").as_bytes(),
        ));
    }
    forms
}

/// The vision binary.
pub fn vision_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_vision"))
}

/// The result of one call.
#[derive(Debug)]
pub struct Outcome {
    pub code: i32,
    pub stdout: String,
    pub stderr: String,
    pub json: Value,
}

impl Outcome {
    /// A field of the answer as text.
    pub fn text(&self, key: &str) -> String {
        self.json
            .get(key)
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string()
    }

    /// `state` of an act answer.
    pub fn state(&self) -> String {
        self.text("state")
    }

    /// `detail` of the answer.
    pub fn detail(&self) -> String {
        self.text("detail")
    }
}

/// Runs git with no global configuration and gives its standard output, trimmed.
pub fn git(args: &[&str], cwd: &Path) -> String {
    let output = git_output(args, cwd);
    assert!(
        output.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}

pub fn git_output(args: &[&str], cwd: &Path) -> std::process::Output {
    Command::new("git")
        .args([
            "-c",
            "user.name=tester",
            "-c",
            "user.email=tester@example.invalid",
            "-c",
            "core.hooksPath=/dev/null",
            "-c",
            "init.defaultBranch=main",
            "-c",
            "commit.gpgsign=false",
        ])
        .args(args)
        .current_dir(cwd)
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("HOME", cwd)
        .env("LANG", "C")
        .output()
        .expect("run git")
}

/// A bare remote with `main`, and a clone on the branch of the ticket with one commit.
pub struct Repos {
    pub remote: PathBuf,
    pub clone: PathBuf,
    pub base: String,
    pub head: String,
}

impl Repos {
    pub fn new(root: &Path) -> Repos {
        std::fs::create_dir_all(root).unwrap();
        let remote = root.join("remote.git");
        let clone = root.join("clone");
        std::fs::create_dir_all(&clone).unwrap();
        git(
            &["init", "--bare", "--quiet", remote.to_str().unwrap()],
            root,
        );
        git(&["init", "--quiet"], &clone);
        let mut repos = Repos {
            remote,
            clone,
            base: String::new(),
            head: String::new(),
        };
        repos.write(&[("app.py", "def add(a, b):\n    return a + b\n")]);
        repos.base = repos.commit("the first commit");
        git(
            &[
                "push",
                "--quiet",
                repos.remote.to_str().unwrap(),
                "main:refs/heads/main",
            ],
            &repos.clone,
        );
        git(&["checkout", "--quiet", "-b", BRANCH], &repos.clone);
        repos.write(&[(
            "app.py",
            "def add(a, b):\n    return a + b\n\n\ndef sub(a, b):\n    return a - b\n",
        )]);
        repos.head = repos.commit("add sub");
        repos
    }

    pub fn write(&self, files: &[(&str, &str)]) {
        for (name, text) in files {
            let path = self.clone.join(name);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, text).unwrap();
        }
    }

    pub fn commit(&self, message: &str) -> String {
        git(&["add", "-A"], &self.clone);
        git(&["commit", "--quiet", "-m", message], &self.clone);
        git(&["rev-parse", "HEAD"], &self.clone)
    }

    pub fn remote_head(&self, branch: &str) -> Option<String> {
        let reference = format!("refs/heads/{branch}");
        let output = git_output(
            &["rev-parse", "--verify", "--quiet", &reference],
            &self.remote,
        );
        let text = String::from_utf8_lossy(&output.stdout).trim().to_string();
        (!text.is_empty()).then_some(text)
    }
}

/// The folder of one test: the fake server, a home, a scratch folder for TMPDIR, and the calls
/// of the adapter.
pub struct AdapterTest {
    pub server: FakeServer,
    pub tmp: TempDir,
    pub home: PathBuf,
    pub scratch: PathBuf,
    /// Folders that come first in PATH, such as the folder of a git spy.
    pub path_prefix: Vec<PathBuf>,
    pub base_ref: String,
}

impl AdapterTest {
    pub fn new() -> AdapterTest {
        let tmp = tempfile::Builder::new()
            .prefix("jg-test-")
            .tempdir()
            .unwrap();
        let home = tmp.path().join("home");
        let scratch = tmp.path().join("scratch");
        std::fs::create_dir_all(&home).unwrap();
        std::fs::create_dir_all(&scratch).unwrap();
        AdapterTest {
            server: FakeServer::start(),
            tmp,
            home,
            scratch,
            path_prefix: Vec::new(),
            base_ref: BASE_REF.into(),
        }
    }

    pub fn path(&self, name: &str) -> PathBuf {
        self.tmp.path().join(name)
    }

    /// The [adapter] table of the tests, with more keys. A null value removes a key.
    pub fn config(&self, more: Value) -> Value {
        let mut config = json!({
            "jira_url": self.server.url,
            "jira_user": "bot@example.invalid",
            "queue_jql": "project = DEMO AND status = \"To Do\"",
            "github_repo": REPO,
            "github_api": self.server.url,
            "ci_workflow": "ci.yml",
            "approvers": ["lead"],
            "own_logins": ["vision-bot", "person"],
            "statuses": {"in_progress": "In Progress", "in_review": "In Review"},
            "ci_find_seconds": 2,
            "ci_poll_seconds": 0.2,
            "http_timeout": 5,
        });
        if let Value::Object(more) = more {
            let map = config.as_object_mut().unwrap();
            for (key, value) in more {
                if value.is_null() {
                    map.remove(&key);
                } else {
                    map.insert(key, value);
                }
            }
        }
        config
    }

    fn env(&self, secrets: &[(String, String)]) -> Vec<(String, String)> {
        let mut parts: Vec<String> = self
            .path_prefix
            .iter()
            .map(|p| p.display().to_string())
            .collect();
        parts.push(std::env::var("PATH").unwrap_or_default());
        let mut env = vec![
            ("PATH".to_string(), parts.join(":")),
            ("HOME".to_string(), self.home.display().to_string()),
            ("LANG".to_string(), "C.UTF-8".to_string()),
            ("TMPDIR".to_string(), self.scratch.display().to_string()),
        ];
        env.extend(secrets.iter().cloned());
        env
    }

    /// Calls the built-in adapter the way the executor does, and tests that no secret is in
    /// its output.
    pub fn run_script(
        &self,
        script: &str,
        request: &Value,
        secrets: &[(String, String)],
    ) -> Outcome {
        let mut child = Command::new(vision_bin())
            .args(["adapter-script", "jira-github", script])
            .env_clear()
            .envs(self.env(secrets))
            .current_dir(self.tmp.path())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("start vision adapter-script");
        let input = request.to_string();
        let mut stdin = child.stdin.take().unwrap();
        let writer = std::thread::spawn(move || {
            let _ = stdin.write_all(input.as_bytes());
        });
        let output = child.wait_with_output().unwrap();
        let _ = writer.join();
        let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
        let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
        let outcome = Outcome {
            code: output.status.code().unwrap_or(-1),
            json: serde_json::from_str(&stdout).unwrap_or(Value::Null),
            stdout,
            stderr,
        };
        assert_no_secret(
            &format!("{}{}", outcome.stdout, outcome.stderr),
            &format!("the output of {script}"),
        );
        outcome
    }

    pub fn observe(&self, request: Value) -> Outcome {
        self.observe_with(request, &all_secrets())
    }

    pub fn observe_with(&self, request: Value, secrets: &[(String, String)]) -> Outcome {
        let mut request = request.as_object().cloned().unwrap_or_default();
        request
            .entry("config")
            .or_insert_with(|| self.config(json!({})));
        request
            .entry("base_ref")
            .or_insert_with(|| json!(self.base_ref));
        self.run_script("observe", &Value::Object(request), secrets)
    }

    /// A call of `act`.
    pub fn act(&self, verb: &str) -> Act<'_> {
        Act {
            test: self,
            verb: verb.to_string(),
            args: json!({}),
            reconcile: false,
            read: false,
            config: None,
            secrets: all_secrets(),
            fields: Map::new(),
        }
    }

    /// A call of `job`, with no secret unless the call gives them.
    pub fn job(&self, name: &str) -> Job<'_> {
        Job {
            test: self,
            name: name.to_string(),
            args: json!({}),
            config: None,
            secrets: Vec::new(),
            fields: Map::new(),
        }
    }

    /// A `git` first in PATH that logs its arguments, the names of its environment, and the
    /// configuration values that came through the environment (a header value as its git
    /// hash, never as text), and runs the real git. Gives the path of the log.
    pub fn git_spy(&mut self) -> PathBuf {
        let spy = self.path("spy");
        std::fs::create_dir_all(&spy).unwrap();
        let log = self.path("git-calls.log");
        let real = find_git();
        let script = format!(
            r#"#!/bin/sh
real='{real}'
log='{log}'
shown() {{
  case "$1" in
    *extraheader=*) printf '%s=<hash:%s>' "${{1%%=*}}" "$(printf %s "${{1#*=}}" | "$real" hash-object --stdin)" ;;
    *) printf '%s' "$1" ;;
  esac
}}
line=''
for a in "$@"; do line="$line$(shown "$a")$(printf '\t')"; done
line="$line$(printf '\036')"
i=0
count=${{GIT_CONFIG_COUNT:-0}}
while [ "$i" -lt "$count" ]; do
  eval "k=\${{GIT_CONFIG_KEY_$i}}"
  eval "v=\${{GIT_CONFIG_VALUE_$i}}"
  line="$line$(shown "$k=$v")$(printf '\t')"
  i=$((i + 1))
done
line="$line$(printf '\036')$(env | sed 's/=.*//' | tr '\n' '\t')"
printf '%s\n' "$line" >> "$log"
exec "$real" "$@"
"#,
            real = real.display(),
            log = log.display(),
        );
        let path = spy.join("git");
        std::fs::write(&path, script).unwrap();
        let mut perms = std::fs::metadata(&path).unwrap().permissions();
        std::os::unix::fs::PermissionsExt::set_mode(&mut perms, 0o755);
        std::fs::set_permissions(&path, perms).unwrap();
        self.path_prefix = vec![spy];
        log
    }
}

/// One logged git call of the spy.
#[derive(Debug)]
pub struct GitCall {
    pub args: Vec<String>,
    pub config: Vec<String>,
    pub env: Vec<String>,
}

/// The calls of the git spy, oldest first.
pub fn read_calls(log: &Path) -> Vec<GitCall> {
    let text = std::fs::read_to_string(log).unwrap_or_default();
    text.lines()
        .filter(|l| !l.is_empty())
        .map(|line| {
            let mut parts = line.split('\u{1e}');
            let list = |part: Option<&str>| -> Vec<String> {
                part.unwrap_or("")
                    .split('\t')
                    .filter(|x| !x.is_empty())
                    .map(str::to_string)
                    .collect()
            };
            GitCall {
                args: list(parts.next()),
                config: list(parts.next()),
                env: list(parts.next()),
            }
        })
        .collect()
}

fn find_git() -> PathBuf {
    let path = std::env::var_os("PATH").unwrap_or_default();
    std::env::split_paths(&path)
        .map(|dir| dir.join("git"))
        .find(|p| p.is_file())
        .expect("git in PATH")
}

/// The form in which the spy logs the header that carries `token`.
pub fn header_hash(token: &str) -> String {
    let value = format!(
        "Authorization: Basic {}",
        vision::util::base64::encode(format!("x-access-token:{token}").as_bytes())
    );
    let mut child = Command::new("git")
        .args(["hash-object", "--stdin"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(value.as_bytes())
        .unwrap();
    let output = child.wait_with_output().unwrap();
    format!("<hash:{}>", String::from_utf8_lossy(&output.stdout).trim())
}

pub fn assert_no_secret(text: &str, place: &str) {
    for form in secret_forms() {
        assert!(!text.contains(&form), "a secret is in {place}");
    }
}

/// Each file under the folders, as bytes: no secret form is allowed in one.
pub fn assert_no_secret_in_files(roots: &[&Path]) {
    fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let Ok(kind) = entry.file_type() else {
                continue;
            };
            if kind.is_dir() {
                walk(&path, out);
            } else if kind.is_file() {
                out.push(path);
            }
        }
    }
    let mut files = Vec::new();
    for root in roots {
        walk(root, &mut files);
    }
    for path in files {
        let data = String::from_utf8_lossy(&std::fs::read(&path).unwrap()).into_owned();
        for form in secret_forms() {
            assert!(
                !data.contains(&form),
                "a secret is in the file {}",
                path.display()
            );
        }
    }
}

/// A call of `act` in the making.
pub struct Act<'a> {
    test: &'a AdapterTest,
    verb: String,
    args: Value,
    reconcile: bool,
    read: bool,
    config: Option<Value>,
    secrets: Vec<(String, String)>,
    fields: Map<String, Value>,
}

impl Act<'_> {
    pub fn args(mut self, args: Value) -> Self {
        self.args = args;
        self
    }

    pub fn reconcile(mut self) -> Self {
        self.reconcile = true;
        self
    }

    pub fn read(mut self) -> Self {
        self.read = true;
        self
    }

    pub fn config(mut self, config: Value) -> Self {
        self.config = Some(config);
        self
    }

    pub fn secrets(mut self, secrets: Vec<(String, String)>) -> Self {
        self.secrets = secrets;
        self
    }

    /// One more field of the call, such as `head` or `pr`.
    pub fn field(mut self, key: &str, value: Value) -> Self {
        self.fields.insert(key.into(), value);
        self
    }

    pub fn run(self) -> Outcome {
        let mut request = json!({
            "verb": self.verb, "op": "op-test", "key": KEY, "branch": BRANCH, "args": self.args,
            "config": self.config.unwrap_or_else(|| self.test.config(json!({}))),
            "base_ref": self.test.base_ref,
        });
        let map = request.as_object_mut().unwrap();
        for (key, value) in self.fields {
            map.insert(key, value);
        }
        if self.reconcile {
            map.insert("reconcile".into(), json!(true));
        }
        if self.read {
            map.insert("read".into(), json!(true));
        }
        self.test.run_script("act", &request, &self.secrets)
    }
}

/// A call of `job` in the making.
pub struct Job<'a> {
    test: &'a AdapterTest,
    name: String,
    args: Value,
    config: Option<Value>,
    secrets: Vec<(String, String)>,
    fields: Map<String, Value>,
}

impl Job<'_> {
    pub fn args(mut self, args: Value) -> Self {
        self.args = args;
        self
    }

    pub fn config(mut self, config: Value) -> Self {
        self.config = Some(config);
        self
    }

    pub fn secrets(mut self, secrets: Vec<(String, String)>) -> Self {
        self.secrets = secrets;
        self
    }

    pub fn field(mut self, key: &str, value: Value) -> Self {
        self.fields.insert(key.into(), value);
        self
    }

    pub fn run(self) -> Outcome {
        let mut request = json!({
            "job": self.name, "key": KEY, "branch": BRANCH, "args": self.args,
            "config": self.config.unwrap_or_else(|| self.test.config(json!({}))),
            "base_ref": self.test.base_ref,
        });
        let map = request.as_object_mut().unwrap();
        for (key, value) in self.fields {
            map.insert(key, value);
        }
        self.test.run_script("job", &request, &self.secrets)
    }
}
