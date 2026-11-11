//! The script runner (section 11.1): the only code that starts `bin/observe`, `bin/act`, and
//! `bin/job` of an adapter. For a built-in adapter (`scripts = "builtin:<name>"`), it starts
//! this binary as `vision adapter-script <name> <observe|act|job>`, with the same contract.
//!
//! A call writes one JSON object to standard input (always with `config`, the `[adapter]` table
//! of the configuration, and `base_ref`, the `base` of `adapter.toml`) and reads one JSON object
//! from standard output. The environment holds
//! only `PATH`, `HOME`, `LANG`, `TMPDIR`, and each secret of `[secrets]`. Exit code 0 is done,
//! 2 is a refused gate, and each other code or the end of the time limit is a failure whose
//! text has the last lines of standard error. The script runs in its own process group, so that
//! the time limit also kills its children.
//!
//! Each string that comes back is untrusted, and each value of a secret in it is masked before
//! it reaches a log line, an error text, a document, or the ledger.

use std::fmt;
use std::io::{Read, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Stdio};
use std::sync::{mpsc, Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{anyhow, bail, Result};
use serde_json::{Map, Value};

use super::gates::MIN_SECRET_LEN;
use crate::adapter::{table_to_json, Adapter};
use crate::config::{Config, Secret};

/// The time limit of a call when the adapter gives none.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(300);

/// The time limit of the command of one secret.
const SECRET_TIMEOUT: Duration = Duration::from_secs(30);

/// The environment names that a script gets from the executor, next to the secrets.
pub const PASSED_ENV: [&str; 4] = ["PATH", "HOME", "LANG", "TMPDIR"];

/// The lines of standard error that an error text keeps.
const STDERR_LINES: usize = 20;
/// The bytes of standard error that the runner keeps while it reads.
const STDERR_KEEP: usize = 64 * 1024;
/// The largest answer that the runner reads.
const STDOUT_MAX: usize = 32 * 1024 * 1024;
/// The wait for the pipes after a kill.
const GRACE: Duration = Duration::from_secs(2);

/// The three scripts of an adapter.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Script {
    Observe,
    Act,
    Job,
}

impl Script {
    /// The file name in `bin/`.
    pub fn file_name(self) -> &'static str {
        match self {
            Script::Observe => "observe",
            Script::Act => "act",
            Script::Job => "job",
        }
    }
}

impl fmt::Display for Script {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "bin/{}", self.file_name())
    }
}

/// The values of the secrets of `[secrets]`, read at the moment of one call. Its `Debug` form
/// shows only the names.
#[derive(Clone, Default)]
pub struct Secrets {
    values: Vec<(String, String)>,
}

impl fmt::Debug for Secrets {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Secrets")
            .field("names", &self.names())
            .finish()
    }
}

impl Secrets {
    /// Reads each secret of `[secrets]` now: an environment variable of this process, or the
    /// standard output of a shell command. The error names the secret, never a value.
    pub fn read(config: &Config) -> Result<Secrets> {
        let mut values = Vec::new();
        for (name, secret) in &config.secrets {
            check_secret_name(name)?;
            let value = match secret {
                Secret::Env(var) => std::env::var(var).map_err(|_| {
                    anyhow!(
                        "the secret {name} comes from the environment variable {var}, which is not set. Set it for the timer and the shell, or change [secrets] in config.toml"
                    )
                })?,
                Secret::Cmd(cmd) => secret_command(name, cmd)?,
            };
            let value = value.trim_end_matches(['\n', '\r']).to_string();
            if value.is_empty() {
                bail!("the secret {name} is empty. Fix its source in [secrets] of config.toml");
            }
            values.push((name.clone(), value));
        }
        values.sort_by_key(|(_, value)| std::cmp::Reverse(value.len()));
        Ok(Secrets { values })
    }

    /// Secrets with known values, for tests.
    pub fn from_values(values: Vec<(String, String)>) -> Secrets {
        let mut values = values;
        values.sort_by_key(|(_, value)| std::cmp::Reverse(value.len()));
        Secrets { values }
    }

    pub fn names(&self) -> Vec<&str> {
        self.values.iter().map(|(name, _)| name.as_str()).collect()
    }

    /// Each secret as (name, value), for the gates.
    pub fn pairs(&self) -> Vec<(&str, &str)> {
        self.values
            .iter()
            .map(|(name, value)| (name.as_str(), value.as_str()))
            .collect()
    }

    /// The text with each value of a secret replaced by `[secret NAME]`.
    pub fn mask(&self, text: &str) -> String {
        let mut text = text.to_string();
        for (name, value) in &self.values {
            if value.len() >= MIN_SECRET_LEN && text.contains(value.as_str()) {
                text = text.replace(value.as_str(), &format!("[secret {name}]"));
            }
        }
        text
    }

    /// Masks each string of a JSON value, keys included.
    pub fn mask_json(&self, value: &mut Value) {
        if self.values.is_empty() {
            return;
        }
        match value {
            Value::String(text) => *text = self.mask(text),
            Value::Array(items) => items.iter_mut().for_each(|item| self.mask_json(item)),
            Value::Object(map) => {
                let entries = std::mem::take(map);
                for (key, mut item) in entries {
                    self.mask_json(&mut item);
                    map.insert(self.mask(&key), item);
                }
            }
            _ => {}
        }
    }

    /// The name of the first secret whose value is in a string of the JSON value.
    pub fn found_in(&self, value: &Value) -> Option<&str> {
        let mut masked = value.clone();
        self.mask_json(&mut masked);
        if masked == *value {
            return None;
        }
        let text = value.to_string();
        self.values
            .iter()
            .find(|(_, v)| v.len() >= MIN_SECRET_LEN && text.contains(v.as_str()))
            .map(|(name, _)| name.as_str())
            .or(Some("a secret"))
    }
}

fn check_secret_name(name: &str) -> Result<()> {
    let valid = name.starts_with(|c: char| c.is_ascii_alphabetic() || c == '_')
        && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_');
    if !valid {
        bail!("the secret name `{name}` in [secrets] is not a valid environment name. Use letters, digits, and '_', such as FORGE_TOKEN");
    }
    if PASSED_ENV.contains(&name) {
        bail!("the secret name {name} in [secrets] is taken by the environment of the scripts. Use a different name");
    }
    Ok(())
}

fn secret_command(name: &str, cmd: &str) -> Result<String> {
    let mut command = Command::new("sh");
    command.arg("-c").arg(cmd);
    let output = run_child(command, Vec::new(), SECRET_TIMEOUT)
        .map_err(|e| anyhow!("could not run the command of the secret {name}: {e}"))?;
    if output.timed_out {
        bail!(
            "the command of the secret {name} did not end in {} s. Fix it in [secrets] of config.toml",
            SECRET_TIMEOUT.as_secs()
        );
    }
    if !output.status.is_some_and(|s| s.success()) {
        bail!(
            "the command of the secret {name} failed ({}). Fix it in [secrets] of config.toml",
            describe_status(output.status)
        );
    }
    String::from_utf8(output.stdout)
        .map_err(|_| anyhow!("the command of the secret {name} printed text that is not UTF-8"))
}

/// The answer of a script.
#[derive(Clone, Debug, PartialEq)]
pub enum Answer {
    /// Exit code 0: the JSON object that it printed.
    Done(Map<String, Value>),
    /// Exit code 2: a gate of the script refused the call. The object that it printed, if any,
    /// with `detail` from standard error when it gave none.
    Refused(Map<String, Value>),
}

/// A call that failed. Each text in it is masked.
#[derive(Clone, Debug, PartialEq, thiserror::Error)]
pub enum ScriptError {
    #[error("{path} does not exist or is not executable. Make it executable with `chmod +x`, or fix the adapter")]
    Missing { path: String },
    #[error("could not start {path}: {why}")]
    Start { path: String, why: String },
    #[error("{script} failed with exit code {code}{tail}")]
    Exit {
        script: String,
        code: i32,
        tail: String,
    },
    #[error("{script} was killed by signal {signal}{tail}")]
    Signal {
        script: String,
        signal: i32,
        tail: String,
    },
    #[error("{script} did not end in {secs} s, so the executor killed it{tail}")]
    Timeout {
        script: String,
        secs: u64,
        tail: String,
    },
    #[error("{script} printed no single JSON object ({why}){tail}")]
    Output {
        script: String,
        why: String,
        tail: String,
    },
}

impl ScriptError {
    /// False when the script never started, so it cannot have changed anything.
    pub fn may_have_acted(&self) -> bool {
        !matches!(
            self,
            ScriptError::Missing { .. } | ScriptError::Start { .. }
        )
    }
}

/// A built-in adapter (`scripts = "builtin:<name>"`): this binary answers its calls as
/// `<exe> adapter-script <name> <observe|act|job>`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Builtin {
    /// The name after `builtin:`, such as `jira-github`.
    pub name: String,
    /// The `vision` binary, or why this program cannot name it.
    pub exe: Result<PathBuf, String>,
}

impl Builtin {
    /// The built-in adapter `name`, answered by the `vision` binary of
    /// [`crate::turn::vision_bin`].
    pub fn new(name: &str) -> Builtin {
        Builtin {
            name: name.to_string(),
            exe: crate::turn::vision_bin().map_err(|e| format!("{e:#}")),
        }
    }
}

/// Runs the scripts of one adapter with one set of secrets.
#[derive(Clone, Debug)]
pub struct Runner<'a> {
    /// The folder whose `bin/` holds the scripts. For a built-in adapter, the adapter folder.
    /// Each call runs in this folder.
    pub scripts: PathBuf,
    /// A built-in adapter: the calls go to this binary, and `bin/` takes no part.
    pub builtin: Option<Builtin>,
    /// The `[adapter]` table of the configuration, as JSON.
    pub config: Value,
    /// The `base` of `adapter.toml`, such as `origin/main`: the one place of the base branch.
    pub base_ref: String,
    pub secrets: &'a Secrets,
}

impl<'a> Runner<'a> {
    pub fn new(adapter: &Adapter, config: &Config, secrets: &'a Secrets) -> Runner<'a> {
        Runner {
            scripts: adapter.scripts.clone(),
            builtin: adapter.builtin.as_deref().map(Builtin::new),
            config: Value::Object(table_to_json(&config.adapter)),
            base_ref: adapter.base.clone(),
            secrets,
        }
    }

    /// The path of a script: `bin/<script>`, or the binary of a built-in adapter.
    pub fn path(&self, script: Script) -> PathBuf {
        match &self.builtin {
            Some(builtin) => builtin
                .exe
                .clone()
                .unwrap_or_else(|_| PathBuf::from("vision")),
            None => self.scripts.join("bin").join(script.file_name()),
        }
    }

    /// True when the script exists and is executable.
    pub fn has(&self, script: Script) -> bool {
        match &self.builtin {
            Some(builtin) => builtin.exe.as_ref().is_ok_and(|exe| is_executable(exe)),
            None => is_executable(&self.path(script)),
        }
    }

    /// `bin/observe` with the default time limit.
    pub fn observe(&self, input: Value) -> Result<Answer, ScriptError> {
        self.call(Script::Observe, input, DEFAULT_TIMEOUT, true)
    }

    /// `bin/act`.
    pub fn act(&self, input: Value, timeout: Duration) -> Result<Answer, ScriptError> {
        self.call(Script::Act, input, timeout, true)
    }

    /// `bin/job`. It gets the secrets only when `with_secrets` is true, which is the key
    /// `secrets` of the job in `adapter.toml`. A job can run code that an agent wrote, and that
    /// code must never see a credential.
    pub fn job(
        &self,
        input: Value,
        timeout: Duration,
        with_secrets: bool,
    ) -> Result<Answer, ScriptError> {
        self.call(Script::Job, input, timeout, with_secrets)
    }

    /// Starts a script with `input` plus `config` and `base_ref`, waits at most `timeout`, and
    /// reads its answer.
    /// The environment of the script holds the names of [`PASSED_ENV`], and the secrets when
    /// `with_secrets` is true. The answer is masked in both cases.
    pub fn call(
        &self,
        script: Script,
        input: Value,
        timeout: Duration,
        with_secrets: bool,
    ) -> Result<Answer, ScriptError> {
        if let Some(Builtin {
            name,
            exe: Err(why),
        }) = &self.builtin
        {
            return Err(ScriptError::Start {
                path: format!("the built-in adapter {name}"),
                why: why.clone(),
            });
        }
        let path = self.path(script);
        let path_text = path.display().to_string();
        if !is_executable(&path) {
            return Err(ScriptError::Missing { path: path_text });
        }
        let mut input = match input {
            Value::Object(map) => map,
            _ => Map::new(),
        };
        input.insert("config".into(), self.config.clone());
        input.insert("base_ref".into(), Value::String(self.base_ref.clone()));
        let bytes = serde_json::to_vec(&Value::Object(input)).unwrap_or_default();
        let mut command = Command::new(&path);
        if let Some(builtin) = &self.builtin {
            command.args(["adapter-script", &builtin.name, script.file_name()]);
        }
        command.current_dir(&self.scripts).env_clear();
        for name in PASSED_ENV {
            if let Some(value) = std::env::var_os(name) {
                command.env(name, value);
            }
        }
        if with_secrets {
            for (name, value) in &self.secrets.values {
                command.env(name, value);
            }
        }
        let output = run_child(command, bytes, timeout).map_err(|e| ScriptError::Start {
            path: path_text.clone(),
            why: self.secrets.mask(&e.to_string()),
        })?;
        let script_name = script.to_string();
        let tail = self.tail(&output.stderr);
        if output.timed_out {
            return Err(ScriptError::Timeout {
                script: script_name,
                secs: timeout.as_secs(),
                tail,
            });
        }
        let status = output.status.ok_or_else(|| ScriptError::Output {
            script: script_name.clone(),
            why: "its output did not end".into(),
            tail: tail.clone(),
        })?;
        match status.code() {
            Some(0) => self.parse(&script_name, &output, &tail).map(Answer::Done),
            Some(2) => {
                let mut answer = self.parse(&script_name, &output, &tail).unwrap_or_default();
                if !answer.get("detail").is_some_and(Value::is_string) {
                    let text = self.secrets.mask(&last_lines(&output.stderr));
                    let detail = if text.trim().is_empty() {
                        format!("{script_name} refused the call")
                    } else {
                        text
                    };
                    answer.insert("detail".into(), Value::String(detail));
                }
                Ok(Answer::Refused(answer))
            }
            Some(code) => Err(ScriptError::Exit {
                script: script_name,
                code,
                tail,
            }),
            None => Err(ScriptError::Signal {
                script: script_name,
                signal: status.signal().unwrap_or(0),
                tail,
            }),
        }
    }

    fn parse(
        &self,
        script: &str,
        output: &ChildOutput,
        tail: &str,
    ) -> Result<Map<String, Value>, ScriptError> {
        let bad = |why: String| ScriptError::Output {
            script: script.to_string(),
            why,
            tail: tail.to_string(),
        };
        if output.stdout_overflow {
            return Err(bad(format!("more than {} MB", STDOUT_MAX / (1024 * 1024))));
        }
        let mut value: Value = serde_json::from_slice(&output.stdout)
            .map_err(|e| bad(format!("standard output is not JSON: {e}")))?;
        self.secrets.mask_json(&mut value);
        match value {
            Value::Object(map) => Ok(map),
            _ => Err(bad("the answer is JSON, but not an object".into())),
        }
    }

    /// The last lines of standard error, masked, as the end of an error text.
    fn tail(&self, stderr: &[u8]) -> String {
        let text = self.secrets.mask(&last_lines(stderr));
        if text.trim().is_empty() {
            String::new()
        } else {
            format!(". The last lines of its standard error:\n{text}")
        }
    }
}

/// The last lines of standard error, each cut to 300 characters, with control characters removed.
fn last_lines(stderr: &[u8]) -> String {
    let text = String::from_utf8_lossy(stderr);
    let lines: Vec<&str> = text.lines().filter(|l| !l.trim().is_empty()).collect();
    let start = lines.len().saturating_sub(STDERR_LINES);
    lines[start..]
        .iter()
        .map(|line| {
            let clean: String = line
                .chars()
                .filter(|c| !c.is_control() || *c == '\t')
                .take(300)
                .collect();
            format!("  {clean}")
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn is_executable(path: &Path) -> bool {
    path.metadata()
        .map(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

fn describe_status(status: Option<ExitStatus>) -> String {
    match status {
        Some(s) => match s.code() {
            Some(code) => format!("exit code {code}"),
            None => format!("signal {}", s.signal().unwrap_or(0)),
        },
        None => "no exit status".into(),
    }
}

/// What a child process gave.
struct ChildOutput {
    /// `None` when it timed out, or when its pipes did not end.
    status: Option<ExitStatus>,
    timed_out: bool,
    stdout: Vec<u8>,
    stdout_overflow: bool,
    /// The last bytes of standard error.
    stderr: Vec<u8>,
}

/// Starts `command` in its own process group with `input` on standard input, and waits at most
/// `timeout` for its end and the end of its output. At the time limit it kills the group.
fn run_child(
    mut command: Command,
    input: Vec<u8>,
    timeout: Duration,
) -> std::io::Result<ChildOutput> {
    command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0);
    let mut child = command.spawn()?;
    let pgid = child.id();
    let deadline = Instant::now() + timeout;
    if let Some(mut stdin) = child.stdin.take() {
        thread::spawn(move || {
            // A script that does not read its input closes the pipe; that is not an error here.
            let _ = stdin.write_all(&input);
        });
    }
    let (out_tx, out_rx) = mpsc::channel();
    if let Some(stdout) = child.stdout.take() {
        thread::spawn(move || {
            let _ = out_tx.send(read_capped(stdout));
        });
    }
    // Standard error is shared while it is read, so that a pipe that never ends still gives
    // the lines that came.
    let err_buf = Arc::new(Mutex::new(Vec::new()));
    let (err_tx, err_rx) = mpsc::channel();
    if let Some(stderr) = child.stderr.take() {
        let buf = Arc::clone(&err_buf);
        thread::spawn(move || {
            read_tail(stderr, &buf);
            let _ = err_tx.send(());
        });
    }
    let mut timed_out = false;
    let mut pause = Duration::from_millis(2);
    let mut status = loop {
        if let Some(status) = child.try_wait()? {
            break Some(status);
        }
        let now = Instant::now();
        if now >= deadline {
            kill_group(pgid);
            let _ = child.kill();
            let _ = child.wait();
            timed_out = true;
            break None;
        }
        thread::sleep(pause.min(deadline - now));
        pause = (pause * 2).min(Duration::from_millis(50));
    };
    // The output ends when each process of the group closed it.
    let pipes_by = if timed_out {
        Instant::now() + GRACE
    } else {
        deadline.max(Instant::now() + Duration::from_millis(100))
    };
    let mut stdout = recv_until(&out_rx, pipes_by);
    if stdout.is_none() && !timed_out {
        kill_group(pgid);
        stdout = recv_until(&out_rx, Instant::now() + GRACE);
        status = None;
    }
    let _ = recv_until(&err_rx, Instant::now() + GRACE);
    let stderr = err_buf.lock().map(|b| b.clone()).unwrap_or_default();
    let (stdout, stdout_overflow) = stdout.unwrap_or_default();
    Ok(ChildOutput {
        status,
        timed_out,
        stdout,
        stdout_overflow,
        stderr,
    })
}

fn recv_until<T>(rx: &mpsc::Receiver<T>, until: Instant) -> Option<T> {
    rx.recv_timeout(until.saturating_duration_since(Instant::now()))
        .ok()
}

fn kill_group(pgid: u32) {
    if let Ok(pgid) = libc::pid_t::try_from(pgid) {
        if pgid > 1 {
            // SAFETY: killpg only sends a signal to the processes of this group, which the
            // runner made for the script.
            unsafe {
                libc::killpg(pgid, libc::SIGKILL);
            }
        }
    }
}

fn read_capped(mut pipe: impl Read) -> (Vec<u8>, bool) {
    let mut data = Vec::new();
    let mut overflow = false;
    let mut buf = [0u8; 8192];
    loop {
        match pipe.read(&mut buf) {
            Ok(0) | Err(_) => break,
            Ok(n) => {
                if data.len() + n > STDOUT_MAX {
                    overflow = true;
                } else {
                    data.extend_from_slice(&buf[..n]);
                }
            }
        }
    }
    (data, overflow)
}

fn read_tail(mut pipe: impl Read, data: &Mutex<Vec<u8>>) {
    let mut buf = [0u8; 8192];
    loop {
        match pipe.read(&mut buf) {
            Ok(0) | Err(_) => break,
            Ok(n) => {
                let Ok(mut data) = data.lock() else {
                    break;
                };
                data.extend_from_slice(&buf[..n]);
                if data.len() > STDERR_KEEP {
                    let cut = data.len() - STDERR_KEEP;
                    data.drain(..cut);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn script(dir: &Path, name: &str, body: &str) {
        let bin = dir.join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        let path = bin.join(name);
        std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    fn runner<'a>(dir: &Path, secrets: &'a Secrets) -> Runner<'a> {
        Runner {
            scripts: dir.to_path_buf(),
            builtin: None,
            config: json!({"root": "/world"}),
            base_ref: "origin/trunk".into(),
            secrets,
        }
    }

    #[test]
    fn a_call_sends_the_input_with_config_and_reads_the_answer() {
        let dir = tempfile::tempdir().unwrap();
        script(
            dir.path(),
            "observe",
            "read line; printf '{\"got\": %s}' \"$line\"",
        );
        let secrets = Secrets::default();
        let answer = runner(dir.path(), &secrets)
            .observe(json!({"what": "queue"}))
            .unwrap();
        assert_eq!(
            answer,
            Answer::Done(
                json!({"got": {"what": "queue", "config": {"root": "/world"}, "base_ref": "origin/trunk"}})
                    .as_object()
                    .unwrap()
                    .clone()
            )
        );
    }

    #[test]
    fn the_environment_holds_only_the_allowed_names_and_the_secrets() {
        let dir = tempfile::tempdir().unwrap();
        script(
            dir.path(),
            "act",
            "printf '{\"env\": \"%s\"}' \"$(env | cut -d= -f1 | sort | tr '\\n' ' ')\"",
        );
        let secrets = Secrets::from_values(vec![("FORGE_TOKEN".into(), "tok-123456".into())]);
        let Answer::Done(answer) = runner(dir.path(), &secrets)
            .act(json!({}), DEFAULT_TIMEOUT)
            .unwrap()
        else {
            panic!("done")
        };
        let names: Vec<&str> = answer["env"].as_str().unwrap().split_whitespace().collect();
        for name in &names {
            assert!(
                PASSED_ENV.contains(name)
                    || *name == "FORGE_TOKEN"
                    || *name == "PWD"
                    || *name == "SHLVL"
                    || *name == "_",
                "unexpected name {name} in {names:?}"
            );
        }
        assert!(names.contains(&"FORGE_TOKEN"), "{names:?}");
    }

    #[test]
    fn a_job_gets_the_secrets_only_when_it_asks_for_them() {
        let dir = tempfile::tempdir().unwrap();
        script(
            dir.path(),
            "job",
            "printf '{\"env\": \"%s\", \"said\": \"tok-123456\"}' \"$(env | cut -d= -f1 | sort | tr '\\n' ' ')\"",
        );
        let secrets = Secrets::from_values(vec![("FORGE_TOKEN".into(), "tok-123456".into())]);
        let names_of = |with_secrets: bool| -> (Vec<String>, String) {
            let Answer::Done(answer) = runner(dir.path(), &secrets)
                .job(json!({}), DEFAULT_TIMEOUT, with_secrets)
                .unwrap()
            else {
                panic!("done")
            };
            let names = answer["env"]
                .as_str()
                .unwrap()
                .split_whitespace()
                .map(str::to_string)
                .collect();
            (names, answer["said"].as_str().unwrap().to_string())
        };
        let (names, said) = names_of(false);
        assert!(!names.iter().any(|n| n == "FORGE_TOKEN"), "{names:?}");
        // The answer is masked also for a job with no secrets in its environment.
        assert_eq!(said, "[secret FORGE_TOKEN]");
        let (names, _) = names_of(true);
        assert!(names.iter().any(|n| n == "FORGE_TOKEN"), "{names:?}");
    }

    #[test]
    fn exit_codes_two_and_others_and_bad_output() {
        let dir = tempfile::tempdir().unwrap();
        let secrets = Secrets::from_values(vec![("FORGE_TOKEN".into(), "tok-123456".into())]);
        let r = runner(dir.path(), &secrets);
        script(dir.path(), "act", "echo 'the branch is main' >&2; exit 2");
        let Answer::Refused(answer) = r.act(json!({}), DEFAULT_TIMEOUT).unwrap() else {
            panic!("refused")
        };
        assert!(answer["detail"]
            .as_str()
            .unwrap()
            .contains("the branch is main"));

        script(
            dir.path(),
            "act",
            "echo one >&2; echo 'token tok-123456 failed' >&2; exit 7",
        );
        let err = r.act(json!({}), DEFAULT_TIMEOUT).unwrap_err();
        let text = err.to_string();
        assert!(matches!(err, ScriptError::Exit { code: 7, .. }), "{text}");
        assert!(
            text.contains("one") && text.contains("[secret FORGE_TOKEN] failed"),
            "{text}"
        );
        assert!(!text.contains("tok-123456"), "{text}");

        script(dir.path(), "act", "echo not json");
        let err = r.act(json!({}), DEFAULT_TIMEOUT).unwrap_err();
        assert!(matches!(err, ScriptError::Output { .. }), "{err}");
        script(dir.path(), "act", "echo '[1]'");
        assert!(r
            .act(json!({}), DEFAULT_TIMEOUT)
            .unwrap_err()
            .to_string()
            .contains("not an object"));

        script(dir.path(), "act", "kill -9 $$");
        assert!(matches!(
            r.act(json!({}), DEFAULT_TIMEOUT).unwrap_err(),
            ScriptError::Signal { signal: 9, .. }
        ));
    }

    #[test]
    fn the_answer_is_masked() {
        let dir = tempfile::tempdir().unwrap();
        script(
            dir.path(),
            "act",
            "echo '{\"detail\": \"pushed with tok-123456\", \"tok-123456\": [\"tok-123456\"]}'",
        );
        let secrets = Secrets::from_values(vec![("FORGE_TOKEN".into(), "tok-123456".into())]);
        let Answer::Done(answer) = runner(dir.path(), &secrets)
            .act(json!({}), DEFAULT_TIMEOUT)
            .unwrap()
        else {
            panic!("done")
        };
        let text = Value::Object(answer).to_string();
        assert!(!text.contains("tok-123456"), "{text}");
        assert!(text.contains("pushed with [secret FORGE_TOKEN]"), "{text}");
    }

    #[test]
    fn a_script_that_never_ends_is_killed_with_its_children() {
        let dir = tempfile::tempdir().unwrap();
        let marker = dir.path().join("child-ran");
        script(
            dir.path(),
            "job",
            &format!("(sleep 3; touch '{}') & sleep 30", marker.display()),
        );
        let secrets = Secrets::default();
        let started = Instant::now();
        let err = runner(dir.path(), &secrets)
            .job(json!({}), Duration::from_secs(1), false)
            .unwrap_err();
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "{:?}",
            started.elapsed()
        );
        assert!(matches!(err, ScriptError::Timeout { secs: 1, .. }), "{err}");
        assert!(err.to_string().contains("did not end in 1 s"), "{err}");
        std::thread::sleep(Duration::from_millis(2500));
        assert!(
            !marker.exists(),
            "the child of the script must be killed too"
        );
    }

    #[test]
    fn a_missing_script_is_named() {
        let dir = tempfile::tempdir().unwrap();
        let secrets = Secrets::default();
        let err = runner(dir.path(), &secrets).observe(json!({})).unwrap_err();
        assert!(!err.may_have_acted());
        assert!(
            err.to_string().contains("bin/observe does not exist"),
            "{err}"
        );
    }

    #[test]
    fn secrets_come_from_the_environment_or_a_command() {
        let mut config = Config::default();
        config.secrets.insert(
            "FROM_CMD".into(),
            Secret::Cmd("printf 'cmd-value-1\\n'".into()),
        );
        config
            .secrets
            .insert("FROM_ENV".into(), Secret::Env("PATH".into()));
        let secrets = Secrets::read(&config).unwrap();
        assert_eq!(secrets.names().len(), 2);
        assert!(secrets.pairs().contains(&("FROM_CMD", "cmd-value-1")));
        assert_eq!(secrets.mask("a cmd-value-1 b"), "a [secret FROM_CMD] b");
        assert!(!format!("{secrets:?}").contains("cmd-value-1"));
        assert_eq!(
            secrets.found_in(&json!({"body": "x cmd-value-1"})),
            Some("FROM_CMD")
        );
        assert_eq!(secrets.found_in(&json!({"body": "clean"})), None);

        let mut missing = Config::default();
        missing.secrets.insert(
            "TOKEN".into(),
            Secret::Env("VISION_TEST_NO_SUCH_VARIABLE".into()),
        );
        let err = Secrets::read(&missing).unwrap_err().to_string();
        assert!(
            err.contains("VISION_TEST_NO_SUCH_VARIABLE, which is not set"),
            "{err}"
        );
        let mut failing = Config::default();
        failing
            .secrets
            .insert("TOKEN".into(), Secret::Cmd("exit 3".into()));
        let err = Secrets::read(&failing).unwrap_err().to_string();
        assert!(
            err.contains("the command of the secret TOKEN failed (exit code 3)"),
            "{err}"
        );
        let mut bad = Config::default();
        bad.secrets
            .insert("PATH".into(), Secret::Cmd("echo x".into()));
        assert!(Secrets::read(&bad).is_err());
    }
}
