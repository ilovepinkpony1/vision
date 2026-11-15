//! The Codex runner (section 7.3): a reader turn of an agent whose runner is `codex` runs
//! `codex exec` in place of Claude Code.
//!
//! - The start line: `codex exec --json -C <folder> -c default_permissions="vision_reader"
//!   ... -m <model> -c model_reasoning_effort=<effort> --output-last-message <file> -`, and for
//!   a turn that continues a thread `codex exec resume ... <thread id> -`. The prompt goes to
//!   standard input: the role file, the first line of a Claude turn, and the form of the result.
//! - The permission profile `vision_reader` extends the built-in `:read-only` of Codex: each
//!   command reads files, writes nothing, and has no network. `--sandbox read-only` alone lets
//!   a command read each file of the machine, so the profile also denies each read of the home
//!   of Vision and of the private folders of the home folder ([`PRIVATE`]). The working folder
//!   stays readable. On macOS, Codex enforces the profile with Seatbelt.
//! - `--ignore-user-config` keeps the configuration of the person out (its MCP servers, its
//!   profiles), and Codex still uses its own login. Vision never reads the folder of that login.
//! - Codex has no result schema of its own that fits the contract: its structured output needs
//!   each property in `required`. So the runner reads the last message as the result object,
//!   and the contract judges it as it judges a Claude turn.
//! - The supervisor writes each event of Codex to `codex.jsonl`, and the same facts as events
//!   of Claude Code to `stream.jsonl`: the thread id as the session, each message of the agent,
//!   each command and its output as a Bash call, and the tokens in the last `result`. So the
//!   collector, the journal, and the live view read a turn on Codex as they read any turn.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde_json::{json, Map, Value};

use crate::config::{CodexPrice, Effort};
use crate::ctx::Ctx;
use crate::ledger::Ledger;
use crate::paths::TurnId;

/// The raw events of Codex in the turn folder.
pub const CODEX_STREAM: &str = "codex.jsonl";
/// The last message of the agent, as `--output-last-message` writes it.
pub const LAST_MESSAGE: &str = "last-message.txt";
/// The prompt of the turn, which goes to standard input.
pub const PROMPT_FILE: &str = "prompt.md";
/// The most characters of the output of one command in `stream.jsonl`.
const OUTPUT_MAX: usize = 4000;

/// The effort of Codex for an effort of Vision. Codex has no `max`, so `max` is `xhigh`.
pub fn effort_word(effort: Effort) -> &'static str {
    match effort {
        Effort::Max => "xhigh",
        other => other.as_str(),
    }
}

/// The name of the permission profile of a reader turn on Codex.
pub const PROFILE: &str = "vision_reader";

/// The private folders and files of the home folder, which a turn on Codex never reads.
pub const PRIVATE: [&str; 10] = [
    ".ssh",
    ".aws",
    ".config/gh",
    ".claude",
    ".codex",
    ".gnupg",
    "Library/Keychains",
    ".netrc",
    ".docker",
    ".kube",
];

/// The paths that a turn on Codex never reads: the home of Vision, and each path of
/// [`PRIVATE`] in the home folder of the person.
pub fn hidden_paths(home: Option<&Path>, vision_home: &Path) -> Vec<PathBuf> {
    let mut out = vec![vision_home.to_path_buf()];
    if let Some(home) = home {
        out.extend(PRIVATE.iter().map(|p| home.join(p)));
    }
    out
}

/// The folder of the test of the sandbox in a turn folder.
pub const SANDBOX_TEST: &str = "sandbox-test";

/// The keys of the permission profile in the arguments of a turn: each `-c` with its value.
pub fn permission_keys(args: &[String]) -> Vec<String> {
    let mut out = Vec::new();
    for pair in args.windows(2) {
        let key = &pair[1];
        if pair[0] == "-c"
            && (key.starts_with("default_permissions=") || key.starts_with("permissions."))
        {
            out.extend(["-c".to_string(), key.clone()]);
        }
    }
    out
}

/// Tests the sandbox of Codex with the keys of the profile, with no model call. `codex sandbox`
/// runs `cat` of a system file and of a new canary file in `dir`, a folder in the home of
/// Vision. Gives `None` when the system file is read and the canary is not. Else it gives the
/// problem, and no turn runs on Codex. So each machine and each version of Codex proves that
/// it hides the home of Vision before a turn starts.
pub fn sandbox_problem(program: &str, dir: &Path, permissions: &[String]) -> Option<String> {
    let found = sandbox_test(program, dir, permissions);
    let _ = std::fs::remove_dir_all(dir);
    found
}

fn sandbox_test(program: &str, dir: &Path, permissions: &[String]) -> Option<String> {
    let problem = |why: String| {
        Some(format!(
            "codex cannot hide private folders on this machine: {why}"
        ))
    };
    let home = dir.join("codex-home");
    let canary = dir.join("canary.txt");
    let output = dir.join("output.txt");
    let token = format!("vision-canary-{}", crate::util::ids::new_session_id());
    let prepared = std::fs::create_dir_all(&home)
        .and_then(|()| std::fs::write(&canary, &token))
        .and_then(|()| std::fs::File::create(&output));
    let out = match prepared {
        Ok(out) => out,
        Err(e) => {
            return problem(format!(
                "could not prepare its test in {}: {e}",
                dir.display()
            ))
        }
    };
    let err = match out.try_clone() {
        Ok(err) => err,
        Err(e) => return problem(format!("could not prepare its test: {e}")),
    };
    let script = "cat /etc/hosts >/dev/null; echo \"read=$?\"; cat \"$1\"; echo \"hidden=$?\"";
    let spawned = std::process::Command::new(program)
        .arg("sandbox")
        .args(["-C", "/", "-P", PROFILE])
        .args(permissions)
        .args(["--", "/bin/sh", "-c", script, "sh"])
        .arg(&canary)
        .env("CODEX_HOME", &home)
        .stdin(std::process::Stdio::null())
        .stdout(out)
        .stderr(err)
        .spawn();
    let mut child = match spawned {
        Ok(child) => child,
        Err(e) => return problem(format!("could not start `{program} sandbox`: {e}")),
    };
    let until = std::time::Instant::now() + std::time::Duration::from_secs(30);
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if std::time::Instant::now() < until => {
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return problem(format!("`{program} sandbox` did not end in 30 seconds"));
            }
        }
    }
    let text = std::fs::read_to_string(&output).unwrap_or_default();
    if text.contains(&token) {
        return problem("a command in its sandbox read a file of the home of Vision".into());
    }
    let ran = text.lines().any(|l| l.trim() == "read=0")
        && text
            .lines()
            .any(|l| l.trim().starts_with("hidden=") && l.trim() != "hidden=0");
    if !ran {
        let first = text
            .lines()
            .find(|l| !l.trim().is_empty())
            .unwrap_or("no output");
        return problem(format!(
            "the test of its sandbox (`{program} sandbox -P {PROFILE}`) did not run: {first}"
        ));
    }
    None
}

/// The configuration keys of the permission profile [`PROFILE`]: the built-in `:read-only`,
/// `none` for each hidden path, and `read` for each readable folder, such as the working
/// folder. A path also comes as its real path when that differs, such as /private/var for
/// /var, because the sandbox sees real paths. A hidden path wins over the same readable one.
pub fn permission_args(hidden: &[PathBuf], readable: &[PathBuf]) -> Vec<String> {
    let forms = |path: &PathBuf| {
        let mut out = vec![path.display().to_string()];
        if let Ok(real) = std::fs::canonicalize(path) {
            let real = real.display().to_string();
            if !out.contains(&real) {
                out.push(real);
            }
        }
        out
    };
    let mut entries: BTreeMap<String, &str> = BTreeMap::new();
    for path in readable {
        for form in forms(path) {
            entries.insert(form, "read");
        }
    }
    for path in hidden {
        for form in forms(path) {
            entries.insert(form, "none");
        }
    }
    // A JSON string is a basic string of TOML too.
    let table: Vec<String> = entries
        .iter()
        .map(|(path, access)| {
            format!(
                "{} = \"{access}\"",
                serde_json::to_string(path).unwrap_or_default()
            )
        })
        .collect();
    vec![
        "-c".into(),
        format!("default_permissions=\"{PROFILE}\""),
        "-c".into(),
        format!("permissions.{PROFILE}.extends=\":read-only\""),
        "-c".into(),
        format!(
            "permissions.{PROFILE}.filesystem={{ {} }}",
            table.join(", ")
        ),
    ]
}

/// The arguments of `codex` for one turn. `thread` is the thread id that the turn continues.
/// `permissions` are the keys of [`permission_args`].
pub fn args(
    workdir: &str,
    model: &str,
    effort: Effort,
    last_message: &str,
    thread: Option<&str>,
    permissions: &[String],
) -> Vec<String> {
    let mut out: Vec<String> = vec!["exec".into()];
    if thread.is_some() {
        out.push("resume".into());
    }
    out.extend([
        "--json".into(),
        "--skip-git-repo-check".into(),
        "--ignore-user-config".into(),
    ]);
    // `exec resume` has no -C: the supervisor starts it in the working folder.
    if thread.is_none() {
        out.extend(["-C".into(), workdir.to_string()]);
    }
    out.extend(permissions.iter().cloned());
    out.extend([
        "-m".into(),
        model.to_string(),
        "-c".into(),
        format!("model_reasoning_effort=\"{}\"", effort_word(effort)),
        "--output-last-message".into(),
        last_message.to_string(),
    ]);
    if let Some(thread) = thread {
        out.push(thread.to_string());
    }
    out.push("-".into());
    out
}

/// The prompt of a turn on Codex: the role file, the first line that a Claude turn gets, and
/// the form of the result object.
pub fn prompt(role: &str, first_message: &str, schema: &Value) -> String {
    format!(
        "{}\n\n# The end of your turn\n\nYou run as a reader: you can read files and run read-only commands, and you change nothing. End your turn with one JSON object as your last message, with no other text and no code fence. It is the result object of the turn, and it fits this JSON schema:\n\n{}\n\n{first_message}\n",
        role.trim_end(),
        serde_json::to_string(schema).unwrap_or_default()
    )
}

/// True when the turn ran on Codex, from its `launch.json`.
pub fn ran_on_codex(ctx: &Ctx, ledger: &Ledger, turn: &str) -> bool {
    let Ok(id) = TurnId::parse(turn) else {
        return false;
    };
    super::launch::turn_paths(ctx, ledger, &id)
        .ok()
        .and_then(|paths| super::launch::LaunchFile::read(&paths.dir).ok())
        .is_some_and(|file| file.plan.runner == crate::config::Runner::Codex)
}

/// The result object in the last message of the agent: the whole text as JSON, else the text
/// in a code fence, else the text from the first `{` to the last `}`.
pub fn parse_result(text: &str) -> Option<Value> {
    let text = text.trim();
    let object = |t: &str| {
        serde_json::from_str::<Value>(t.trim())
            .ok()
            .filter(Value::is_object)
    };
    if let Some(found) = object(text) {
        return Some(found);
    }
    if let Some(inner) = text
        .strip_prefix("```json")
        .or_else(|| text.strip_prefix("```"))
        .and_then(|t| t.trim_end().strip_suffix("```"))
    {
        if let Some(found) = object(inner) {
            return Some(found);
        }
    }
    let (start, end) = (text.find('{')?, text.rfind('}')?);
    (start < end).then(|| object(&text[start..=end])).flatten()
}

/// What an error text of Codex says, for the reason of a fallback.
pub fn classify(text: &str) -> &'static str {
    let lower = text.to_lowercase();
    if ["usage limit", "rate limit", "quota", "too many requests"]
        .iter()
        .any(|w| lower.contains(w))
    {
        "a usage limit"
    } else if [
        "log in",
        "login",
        "logged in",
        "unauthorized",
        "authentication",
        "401",
    ]
    .iter()
    .any(|w| lower.contains(w))
    {
        "a login problem"
    } else if ["cyber", "flagged", "usage policy", "refus", "safety"]
        .iter()
        .any(|w| lower.contains(w))
    {
        "a refusal"
    } else {
        "an error"
    }
}

/// The tokens of a turn on Codex, from `turn.completed`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CodexUsage {
    pub input: i64,
    pub cached: i64,
    pub cache_write: i64,
    pub output: i64,
}

/// Reads the events of Codex and gives the same facts as events of Claude Code.
#[derive(Clone, Debug, Default)]
pub struct Translator {
    model: String,
    pub thread: Option<String>,
    /// The text of the last message of the agent.
    pub last_message: Option<String>,
    /// The tokens of each `turn.completed`, added up.
    pub usage: CodexUsage,
    /// The last error text of an `error` or a `turn.failed` event.
    pub error: Option<String>,
    /// True after `turn.completed`.
    pub completed: bool,
    /// The commands that started and did not complete yet.
    started: Vec<String>,
}

impl Translator {
    pub fn new(model: &str) -> Translator {
        Translator {
            model: model.to_string(),
            ..Translator::default()
        }
    }

    /// The lines for `stream.jsonl` of one event of Codex.
    pub fn lines(&mut self, event: &Value) -> Vec<Value> {
        let text = |v: Option<&Value>| v.and_then(Value::as_str).unwrap_or("").to_string();
        match event.get("type").and_then(Value::as_str).unwrap_or("") {
            "thread.started" => {
                let thread = text(event.get("thread_id"));
                if thread.is_empty() {
                    return Vec::new();
                }
                self.thread = Some(thread.clone());
                vec![json!({
                    "type": "system", "subtype": "init", "session_id": thread, "tools": [],
                    "permissionMode": "read-only", "mcp_servers": [], "model": self.model,
                    "runner": "codex",
                })]
            }
            "turn.completed" => {
                self.completed = true;
                let usage = event.get("usage");
                let count = |key: &str| {
                    usage
                        .and_then(|u| u.get(key))
                        .and_then(Value::as_i64)
                        .unwrap_or(0)
                };
                self.usage.input += count("input_tokens");
                self.usage.cached += count("cached_input_tokens");
                self.usage.cache_write += count("cache_write_input_tokens");
                self.usage.output += count("output_tokens");
                Vec::new()
            }
            "turn.failed" => {
                let message = text(event.get("error").and_then(|e| e.get("message")));
                self.error = Some(if message.is_empty() {
                    "the turn failed".to_string()
                } else {
                    message
                });
                Vec::new()
            }
            "error" => {
                let message = text(event.get("message"));
                if !message.is_empty() {
                    self.error = Some(message);
                }
                Vec::new()
            }
            "item.started" | "item.completed" => {
                let completed = event.get("type").and_then(Value::as_str) == Some("item.completed");
                let Some(item) = event.get("item") else {
                    return Vec::new();
                };
                self.item(item, completed)
            }
            _ => Vec::new(),
        }
    }

    fn item(&mut self, item: &Value, completed: bool) -> Vec<Value> {
        let text = |key: &str| {
            item.get(key)
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string()
        };
        let id = text("id");
        match item.get("type").and_then(Value::as_str).unwrap_or("") {
            "agent_message" if completed => {
                let said = text("text");
                self.last_message = Some(said.clone());
                vec![
                    json!({"type": "assistant", "message": {"content": [{"type": "text", "text": said}]}}),
                ]
            }
            "command_execution" => {
                let mut out = Vec::new();
                let call = json!({"type": "assistant", "message": {"content": [{
                    "type": "tool_use", "id": id, "name": "Bash", "input": {"command": text("command")},
                }]}});
                if !completed {
                    self.started.push(id);
                    return vec![call];
                }
                if let Some(at) = self.started.iter().position(|s| *s == id) {
                    self.started.remove(at);
                } else {
                    out.push(call);
                }
                let code = item.get("exit_code").and_then(Value::as_i64);
                let output: String = text("aggregated_output").chars().take(OUTPUT_MAX).collect();
                let failed = code.is_some_and(|c| c != 0);
                let content = if failed {
                    format!("Exit code {}\n{output}", code.unwrap_or(1))
                } else {
                    output
                };
                out.push(json!({"type": "user", "message": {"content": [{
                    "type": "tool_result", "tool_use_id": id, "content": content, "is_error": failed,
                }]}}));
                out
            }
            other if completed && !matches!(other, "reasoning" | "error" | "agent_message") => {
                let mut input = Map::new();
                for key in ["path", "query", "server", "tool"] {
                    if let Some(v) = item.get(key) {
                        input.insert(key.into(), v.clone());
                    }
                }
                vec![json!({"type": "assistant", "message": {"content": [{
                    "type": "tool_use", "id": id, "name": other, "input": Value::Object(input),
                }]}})]
            }
            _ => Vec::new(),
        }
    }

    /// The last line for `stream.jsonl`: a `result` with the result object of the last
    /// message, the thread id, the tokens, and the cost of `price` (0 without it).
    pub fn result(
        &self,
        last: Option<&str>,
        ok: bool,
        price: Option<CodexPrice>,
    ) -> (Value, Option<Value>) {
        let text = last
            .map(str::to_string)
            .or_else(|| self.last_message.clone())
            .unwrap_or_default();
        let object = if ok { parse_result(&text) } else { None };
        let cost = price.map_or(0.0, |p| p.cost(self.usage.input, self.usage.output));
        let line = json!({
            "type": "result",
            "subtype": if ok { "success" } else { "error" },
            "is_error": !ok,
            "result": text,
            "structured_output": object,
            "session_id": self.thread,
            "total_cost_usd": cost,
            "num_turns": 1,
            "permission_denials": [],
            "usage": {
                "input_tokens": (self.usage.input - self.usage.cached).max(0),
                "cache_creation_input_tokens": self.usage.cache_write,
                "cache_read_input_tokens": self.usage.cached,
                "output_tokens": self.usage.output,
            },
            "runner": "codex",
        });
        (line, object)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_start_line_of_a_new_turn_and_of_a_resume() {
        let keys = vec![
            "-c".to_string(),
            "default_permissions=\"vision_reader\"".to_string(),
        ];
        let new = args(
            "/clone",
            "gpt-6.1-sol",
            Effort::Max,
            "/t/last-message.txt",
            None,
            &keys,
        );
        assert_eq!(
            new.join(" "),
            "exec --json --skip-git-repo-check --ignore-user-config -C /clone -c default_permissions=\"vision_reader\" -m gpt-6.1-sol -c model_reasoning_effort=\"xhigh\" --output-last-message /t/last-message.txt -"
        );
        let resume = args(
            "/clone",
            "gpt-6.1-sol",
            Effort::High,
            "/t/last.txt",
            Some("th-1"),
            &keys,
        );
        assert_eq!(
            resume.join(" "),
            "exec resume --json --skip-git-repo-check --ignore-user-config -c default_permissions=\"vision_reader\" -m gpt-6.1-sol -c model_reasoning_effort=\"high\" --output-last-message /t/last.txt th-1 -"
        );
        assert!(!new.iter().chain(&resume).any(|a| a.contains("sandbox")));
        assert_eq!(permission_keys(&new), keys);
        assert_eq!(permission_keys(&resume), keys);
    }

    #[test]
    fn the_profile_hides_the_home_of_vision_and_the_private_folders() {
        let hidden = hidden_paths(Some(Path::new("/nowhere/home")), Path::new("/nowhere/v"));
        assert_eq!(hidden.len(), 1 + PRIVATE.len());
        for private in [
            ".ssh",
            ".aws",
            ".config/gh",
            ".claude",
            ".codex",
            "Library/Keychains",
        ] {
            assert!(
                hidden.contains(&PathBuf::from("/nowhere/home").join(private)),
                "{private}"
            );
        }
        assert_eq!(
            hidden_paths(None, Path::new("/nowhere/v")),
            [PathBuf::from("/nowhere/v")]
        );
        let keys = permission_args(
            &[
                PathBuf::from("/nowhere/v"),
                PathBuf::from("/nowhere/home/.ssh"),
                PathBuf::from("/nowhere/a \"b\""),
            ],
            &[PathBuf::from("/nowhere/clone"), PathBuf::from("/nowhere/v")],
        );
        assert_eq!(
            keys,
            [
                "-c",
                "default_permissions=\"vision_reader\"",
                "-c",
                "permissions.vision_reader.extends=\":read-only\"",
                "-c",
                "permissions.vision_reader.filesystem={ \"/nowhere/a \\\"b\\\"\" = \"none\", \"/nowhere/clone\" = \"read\", \"/nowhere/home/.ssh\" = \"none\", \"/nowhere/v\" = \"none\" }",
            ]
        );
        // The value of the last key is TOML that Codex parses.
        let value = keys[5].split_once('=').unwrap().1;
        let parsed: toml::Table = toml::from_str(&format!("x = {value}")).unwrap();
        assert_eq!(parsed["x"]["/nowhere/a \"b\""].as_str(), Some("none"));
        assert_eq!(parsed["x"]["/nowhere/clone"].as_str(), Some("read"));
        // A path with another real path comes in both forms.
        let dir = tempfile::tempdir().unwrap();
        let real = std::fs::canonicalize(dir.path()).unwrap();
        let link = dir.path().join("link");
        std::os::unix::fs::symlink(&real, &link).unwrap();
        let keys = permission_args(std::slice::from_ref(&link), &[]);
        assert!(
            keys[5].contains(&format!("{:?} = \"none\"", link.display().to_string())),
            "{}",
            keys[5]
        );
        assert!(
            keys[5].contains(&format!("{:?} = \"none\"", real.display().to_string())),
            "{}",
            keys[5]
        );
    }

    #[test]
    fn the_events_of_codex_become_events_of_claude_code() {
        let mut t = Translator::new("gpt-6.1-sol");
        let lines = |t: &mut Translator, text: &str| t.lines(&serde_json::from_str(text).unwrap());
        let init = lines(&mut t, r#"{"type":"thread.started","thread_id":"th-1"}"#);
        assert_eq!(init[0]["session_id"], "th-1");
        assert!(matches!(
            crate::turn::parse_stream_value(&init[0]),
            Some(crate::turn::StreamEvent::Init { .. })
        ));
        let started = lines(
            &mut t,
            r#"{"type":"item.started","item":{"id":"i1","type":"command_execution","command":"/bin/zsh -lc 'ls'","status":"in_progress"}}"#,
        );
        assert_eq!(started.len(), 1);
        assert_eq!(started[0]["message"]["content"][0]["name"], "Bash");
        let done = lines(
            &mut t,
            r#"{"type":"item.completed","item":{"id":"i1","type":"command_execution","command":"/bin/zsh -lc 'ls'","aggregated_output":"no such file\n","exit_code":2,"status":"failed"}}"#,
        );
        assert_eq!(done.len(), 1, "the call showed at its start");
        assert_eq!(done[0]["message"]["content"][0]["is_error"], true);
        assert!(done[0]["message"]["content"][0]["content"]
            .as_str()
            .unwrap()
            .starts_with("Exit code 2"));
        let said = lines(
            &mut t,
            r#"{"type":"item.completed","item":{"id":"i2","type":"agent_message","text":"{\"kind\":\"result\",\"summary\":\"ok\"}"}}"#,
        );
        assert_eq!(said[0]["type"], "assistant");
        lines(
            &mut t,
            r#"{"type":"turn.completed","usage":{"input_tokens":1000,"cached_input_tokens":800,"cache_write_input_tokens":0,"output_tokens":50}}"#,
        );
        assert!(t.completed);
        let price = CodexPrice {
            input: 1.0,
            output: 10.0,
        };
        let (line, object) = t.result(None, true, Some(price));
        assert_eq!(object.unwrap()["summary"], "ok");
        assert_eq!(line["session_id"], "th-1");
        assert_eq!(line["usage"]["input_tokens"], 200);
        assert_eq!(line["usage"]["cache_read_input_tokens"], 800);
        assert!((line["total_cost_usd"].as_f64().unwrap() - 0.0015).abs() < 1e-9);
        let (free, _) = t.result(None, true, None);
        assert_eq!(free["total_cost_usd"], 0.0);
    }

    #[test]
    fn an_error_of_codex_is_kept_and_named() {
        let mut t = Translator::new("m");
        t.lines(&json!({"type": "error", "message": "You've hit your usage limit."}));
        t.lines(
            &json!({"type": "turn.failed", "error": {"message": "You've hit your usage limit."}}),
        );
        assert_eq!(classify(t.error.as_deref().unwrap()), "a usage limit");
        assert_eq!(
            classify("Please log in again: 401 Unauthorized"),
            "a login problem"
        );
        assert_eq!(
            classify("This content was flagged for possible cybersecurity risk"),
            "a refusal"
        );
        assert_eq!(classify("The model is not supported"), "an error");
    }

    #[test]
    fn the_result_object_comes_from_the_last_message() {
        assert_eq!(
            parse_result(r#"{"kind":"result"}"#).unwrap()["kind"],
            "result"
        );
        assert_eq!(
            parse_result("```json\n{\"kind\":\"result\"}\n```").unwrap()["kind"],
            "result"
        );
        assert_eq!(
            parse_result("Here it is: {\"kind\":\"result\",\"summary\":\"x\"} done").unwrap()
                ["summary"],
            "x"
        );
        assert!(parse_result("no object").is_none());
        assert!(parse_result("[1, 2]").is_none());
    }
}
