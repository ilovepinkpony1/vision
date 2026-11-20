//! The turns in herdr panes (section 7.7). With `[herdr] enabled`, a Claude turn of a work agent
//! runs as an interactive Claude Code session in a tab of the named herdr session, with one
//! workspace for each ticket. The person watches the session in Herdr.app and types into it.
//!
//! The supervisor still owns the turn:
//!
//! 1. It makes sure that the herdr session answers. If not, it opens a window of Herdr.app on
//!    the session, which starts its server.
//! 2. It opens a tab with the name of the agent in the workspace of the ticket, and it closes
//!    the tabs of the ended turns of the ticket. It runs `pane.sh` in the tab: the script writes
//!    its pid, and replaces itself with `claude` and the environment of the allow list.
//! 3. The hooks of the session run `vision turn-hook`, which adds each hook event to
//!    `hooks.jsonl`. The status line writes the cost of the session to `status-line.json`.
//! 4. The supervisor copies each message of the transcript to `stream.jsonl`, in the form of
//!    the headless stream, so that the live view and the collection read it as before.
//! 5. At each `Stop`, it reads the last message of the agent. A result object that fits the
//!    schema ends the turn. Else it reminds the agent, unless the person wrote in the pane in
//!    the last 15 minutes.
//! 6. Each message of the inbox goes to the session with `herdr agent prompt`.
//! 7. At the time limit, on a signal, or when the session does not start, it stops `claude`.
//!    A pane that closes before the result ends the turn with the reason `killed`.

use std::collections::{BTreeSet, HashSet};
use std::ffi::OsString;
use std::fs::OpenOptions;
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::Ordering;
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};

use crate::ctx::Ctx;
use crate::ledger::Ledger;
use crate::paths::TurnPaths;
use crate::turn::launch::Plan;
use crate::turn::state::{self, Phase, Reason, TurnState};
use crate::turn::{inbox, roles, schema, settings};
use crate::util::fs::atomic_write;
use crate::util::time;

/// The hook events of a session, one JSON object for each line.
pub const HOOKS_FILE: &str = "hooks.jsonl";
/// The last input of the status line of the session: its cost and its model.
pub const STATUS_LINE_FILE: &str = "status-line.json";
/// The script that the pane runs.
pub const SCRIPT_FILE: &str = "pane.sh";
/// The pid that the script writes before it becomes `claude`.
pub const PID_FILE: &str = "pane.pid";

/// The hook events that the supervisor reads.
pub const HOOK_EVENTS: [&str; 6] = [
    "SessionStart",
    "UserPromptSubmit",
    "Stop",
    "StopFailure",
    "Notification",
    "SessionEnd",
];

/// How long the session can take to start, with the folder trust dialog.
const START_LIMIT: Duration = Duration::from_secs(120);
/// The reminders to end with the result object, before the turn fails.
const REMINDERS: u32 = 2;
/// How long the agent can talk with the person before the next reminder.
const PERSON_QUIET: Duration = Duration::from_secs(15 * 60);
/// The times that the supervisor types `continue` after a short API failure.
const CONTINUES: u32 = 2;
/// How long `claude` can run after the result before the supervisor stops it.
const EXIT_GRACE: Duration = Duration::from_secs(10);
/// The wait between TERM and KILL.
const TERM_GRACE: Duration = Duration::from_secs(1);
/// How often the supervisor reads the hooks, the transcript, and the inbox.
const POLL: Duration = Duration::from_millis(200);
/// The time limit of one call of the herdr CLI.
const HERDR_TIMEOUT: Duration = Duration::from_secs(20);
/// How long the server of the session can take to start after Herdr.app opens a window.
const SERVER_WAIT: Duration = Duration::from_secs(20);
/// The most keys for the folder dialogs of one start: two for each dialog.
const DIALOG_ANSWERS: u32 = 60;
/// How often the screen is read for a folder dialog before the session starts.
const DIALOG_EVERY: Duration = Duration::from_millis(700);

/// The herdr session, the CLI, and the app of a turn in a pane, resolved at launch.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PanePlan {
    pub session: String,
    pub bin: String,
    pub app: String,
}

impl PanePlan {
    pub fn of(config: &crate::config::Herdr) -> PanePlan {
        PanePlan {
            session: config.session.clone(),
            bin: config.bin.clone(),
            app: config.app.clone(),
        }
    }
}

/// Where a turn runs in herdr, in `turn.json`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PaneRef {
    pub session: String,
    pub workspace: String,
    pub tab: String,
    pub pane: String,
}

// The hook command ------------------------------------------------------------------------------

/// `vision turn-hook <turn folder> [--status-line]`: one hook event of a session in a pane. It
/// adds the event to `hooks.jsonl` with the time, and prints nothing, because Claude Code adds
/// the output of some hooks to the conversation. With `--status-line`, it keeps the input in
/// `status-line.json` and prints the line of the status bar.
pub fn hook(dir: &Path, status_line: bool) -> Result<()> {
    if !dir.join(crate::turn::launch::LAUNCH_FILE).is_file() {
        bail!("{} is not the folder of a turn", dir.display());
    }
    let mut raw = String::new();
    std::io::stdin()
        .take(4 << 20)
        .read_to_string(&mut raw)
        .context("could not read the hook input")?;
    let mut value: Value = serde_json::from_str(raw.trim()).unwrap_or(Value::Null);
    if status_line {
        atomic_write(&dir.join(STATUS_LINE_FILE), format!("{value}\n").as_bytes())?;
        println!("Vision runs this turn. Type here to talk to the agent.");
        return Ok(());
    }
    if let Value::Object(map) = &mut value {
        map.insert("vision_at".into(), Value::String(time::now_rfc3339()));
    }
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(dir.join(HOOKS_FILE))
        .with_context(|| format!("could not open {}", dir.join(HOOKS_FILE).display()))?;
    file.write_all(format!("{value}\n").as_bytes())?;
    Ok(())
}

/// The hooks and the status line of a session in a pane, added to its settings. A custom
/// settings file can hold hooks of its own: they stay, and these come after them.
pub fn with_hooks(settings_text: &str, program: &Path, dir: &Path) -> Result<String> {
    let mut value: Value =
        serde_json::from_str(settings_text).context("the settings of the turn are not JSON")?;
    let Value::Object(root) = &mut value else {
        bail!("the settings of the turn are not a JSON object");
    };
    let command = format!(
        "{} turn-hook {}",
        shell_quote(&program.display().to_string()),
        shell_quote(&dir.display().to_string())
    );
    let hooks = root
        .entry("hooks")
        .or_insert_with(|| Value::Object(Map::new()));
    let Value::Object(hooks) = hooks else {
        bail!("`hooks` of the settings of the turn is not a JSON object");
    };
    for event in HOOK_EVENTS {
        let list = hooks
            .entry(event)
            .or_insert_with(|| Value::Array(Vec::new()));
        let Value::Array(list) = list else {
            bail!("`hooks.{event}` of the settings of the turn is not a list");
        };
        list.push(json!({"hooks": [{"type": "command", "command": command, "timeout": 10}]}));
    }
    root.insert(
        "statusLine".into(),
        json!({"type": "command", "command": format!("{command} --status-line")}),
    );
    Ok(format!("{}\n", serde_json::to_string_pretty(&value)?))
}

/// The end of the role of an agent in a pane: how the turn ends, with the schema of its result.
pub fn result_section(schema: &Value) -> String {
    format!(
        "\n\n# The end of your turn\n\nYou run in a terminal pane, and the person can type to you there. When the person writes to you, answer, and then go on with the step. When the step is done, or when you need the person, end your turn with one JSON object as your whole last message, with no other text and no code fence. It is the result object of the turn, and it fits this JSON schema:\n\n{}\n",
        serde_json::to_string(schema).unwrap_or_default()
    )
}

/// The arguments of an interactive session: the arguments of the headless session, without
/// the ones that work only with `-p`.
pub fn interactive_args(args: &[String]) -> Vec<String> {
    const ALONE: [&str; 3] = ["-p", "--verbose", "--replay-user-messages"];
    const WITH_VALUE: [&str; 5] = [
        "--input-format",
        "--output-format",
        "--permission-prompts",
        "--json-schema",
        "--max-budget-usd",
    ];
    let mut out = Vec::new();
    let mut skip = false;
    for arg in args {
        if skip {
            skip = false;
            continue;
        }
        if ALONE.contains(&arg.as_str()) {
            continue;
        }
        if WITH_VALUE.contains(&arg.as_str()) {
            skip = true;
            continue;
        }
        out.push(arg.clone());
    }
    out
}

/// A text as one word of `sh`.
pub fn shell_quote(text: &str) -> String {
    format!("'{}'", text.replace('\'', r"'\''"))
}

/// The script that the pane runs: it writes its pid, goes to the working folder, and replaces
/// itself with `claude`, with only the environment of the allow list. `TERM` and `COLORTERM`
/// come from the pane, because the session draws in it.
pub fn script_text(
    plan: &Plan,
    env: &[(OsString, OsString)],
    pid_file: &Path,
    first_message: &str,
) -> String {
    let mut words = vec![
        "exec".to_string(),
        "env".to_string(),
        "-i".to_string(),
        r#"TERM="${TERM:-xterm-256color}""#.to_string(),
        r#"COLORTERM="${COLORTERM:-}""#.to_string(),
    ];
    let mut names = BTreeSet::new();
    let set: Vec<(String, String)> = plan
        .env_set
        .iter()
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();
    let passed = env
        .iter()
        .map(|(k, v)| {
            (
                k.to_string_lossy().into_owned(),
                v.to_string_lossy().into_owned(),
            )
        })
        .filter(|(k, _)| !plan.env_set.contains_key(k));
    for (name, value) in passed.chain(set) {
        if name == "TERM"
            || name == "COLORTERM"
            || !plain_name(&name)
            || !names.insert(name.clone())
        {
            continue;
        }
        words.push(shell_quote(&format!("{name}={value}")));
    }
    words.push(shell_quote(&plan.program));
    words.extend(interactive_args(&plan.args).iter().map(|a| shell_quote(a)));
    words.push(shell_quote(first_message));
    format!(
        "#!/bin/sh\n# Written by vision for the turn {turn}. It runs in a pane of herdr.\necho $$ > {pid}\ncd {dir} || exit 1\n{line}\n",
        turn = plan.turn,
        pid = shell_quote(&pid_file.display().to_string()),
        dir = shell_quote(&plan.workdir.display().to_string()),
        line = words.join(" "),
    )
}

fn plain_name(name: &str) -> bool {
    !name.is_empty()
        && !name.starts_with(|c: char| c.is_ascii_digit())
        && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
}

// The herdr CLI ---------------------------------------------------------------------------------

/// The herdr CLI on the named session of the turns. Contract: herdr 0.9.5.
pub struct Herdr {
    pub plan: PanePlan,
}

impl Herdr {
    pub fn new(plan: PanePlan) -> Herdr {
        Herdr { plan }
    }

    /// Runs one command on the session and gives `result` of its JSON answer.
    pub fn call(&self, args: &[&str]) -> Result<Value> {
        let what = args.iter().take(2).copied().collect::<Vec<_>>().join(" ");
        let output = run_with_timeout(
            Command::new(&self.plan.bin)
                .arg("--session")
                .arg(&self.plan.session)
                .args(args),
            HERDR_TIMEOUT,
        )
        .with_context(|| format!("could not run `{} {what}`", self.plan.bin))?;
        let text = String::from_utf8_lossy(&output.stdout);
        let value: Option<Value> = serde_json::from_str(text.trim()).ok();
        if let Some(error) = value.as_ref().and_then(|v| v.get("error")) {
            let code = error.get("code").and_then(Value::as_str).unwrap_or("error");
            let message = error.get("message").and_then(Value::as_str).unwrap_or("");
            bail!("herdr {what} failed ({code}): {message}");
        }
        if !output.status.success() {
            let err = String::from_utf8_lossy(&output.stderr);
            bail!(
                "herdr {what} failed with {}: {}",
                output.status,
                err.trim().chars().take(300).collect::<String>()
            );
        }
        Ok(value
            .and_then(|v| v.get("result").cloned())
            .unwrap_or(Value::Null))
    }

    /// True when the server of the session answers.
    pub fn answers(&self) -> bool {
        self.call(&["workspace", "list"]).is_ok()
    }

    /// Makes sure that the server of the session runs. A window of Herdr.app on the session
    /// starts it.
    pub fn ensure_running(&self) -> Result<()> {
        if self.answers() {
            return Ok(());
        }
        self.open_window()?;
        let until = Instant::now() + SERVER_WAIT;
        while Instant::now() < until {
            if self.answers() {
                return Ok(());
            }
            thread::sleep(Duration::from_millis(500));
        }
        bail!(
            "the herdr session `{}` did not start. Open it in Herdr.app, or set [herdr] enabled = false in config.toml",
            self.plan.session
        )
    }

    /// Opens a window of Herdr.app on the session.
    pub fn open_window(&self) -> Result<()> {
        let output = run_with_timeout(
            Command::new(&self.plan.app).args([
                "window",
                "new",
                "--session",
                &self.plan.session,
                "--json",
            ]),
            HERDR_TIMEOUT,
        )
        .with_context(|| format!("could not run {}", self.plan.app))?;
        if !output.status.success() {
            bail!(
                "Herdr.app did not open a window on the session `{}`: {}",
                self.plan.session,
                String::from_utf8_lossy(&output.stderr).trim()
            );
        }
        Ok(())
    }

    /// The workspace of a ticket, by its label.
    pub fn workspace_of(&self, label: &str) -> Result<Option<String>> {
        let list = self.call(&["workspace", "list"])?;
        Ok(list
            .get("workspaces")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .find(|w| w.get("label").and_then(Value::as_str) == Some(label))
            .and_then(|w| w.get("workspace_id").and_then(Value::as_str))
            .map(str::to_string))
    }

    /// Opens a tab with `label` in the workspace of the ticket, and makes the workspace when
    /// it does not exist. Gives the workspace, the tab, and the pane.
    pub fn open_tab(
        &self,
        ticket: &str,
        label: &str,
        cwd: &Path,
    ) -> Result<(String, String, String)> {
        let cwd = cwd.display().to_string();
        if let Some(workspace) = self.workspace_of(ticket)? {
            let made = self.call(&[
                "tab",
                "create",
                "--workspace",
                &workspace,
                "--cwd",
                &cwd,
                "--label",
                label,
                "--no-focus",
            ])?;
            let tab = id_at(&made, &["tab", "tab_id"])?;
            let pane = id_at(&made, &["root_pane", "pane_id"])?;
            return Ok((workspace, tab, pane));
        }
        let made = self.call(&[
            "workspace",
            "create",
            "--cwd",
            &cwd,
            "--label",
            ticket,
            "--no-focus",
        ])?;
        let workspace = id_at(&made, &["workspace", "workspace_id"])?;
        let tab = id_at(&made, &["tab", "tab_id"])?;
        let pane = id_at(&made, &["root_pane", "pane_id"])?;
        // The first tab of a new workspace has the label "1".
        let _ = self.call(&["tab", "rename", &tab, label]);
        Ok((workspace, tab, pane))
    }

    pub fn run(&self, pane: &str, line: &str) -> Result<()> {
        self.call(&["pane", "run", pane, line]).map(|_| ())
    }

    /// Types a prompt into the session of the pane. herdr refuses it while Claude shows a
    /// dialog.
    pub fn prompt(&self, pane: &str, text: &str) -> Result<()> {
        self.call(&["agent", "prompt", pane, text]).map(|_| ())
    }

    pub fn send_keys(&self, pane: &str, keys: &[&str]) -> Result<()> {
        let mut args = vec!["agent", "send-keys", pane];
        args.extend_from_slice(keys);
        self.call(&args).map(|_| ())
    }

    /// The text on the screen of the pane.
    pub fn read(&self, pane: &str) -> Result<String> {
        let output = run_with_timeout(
            Command::new(&self.plan.bin)
                .arg("--session")
                .arg(&self.plan.session)
                .args(["pane", "read", pane]),
            HERDR_TIMEOUT,
        )?;
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    }

    pub fn close_tab(&self, tab: &str) -> Result<()> {
        self.call(&["tab", "close", tab]).map(|_| ())
    }

    /// Shows the tab of a turn: its workspace and its tab get the focus in the session.
    pub fn focus(&self, workspace: &str, tab: &str) -> Result<()> {
        self.call(&["workspace", "focus", workspace])?;
        self.call(&["tab", "focus", tab]).map(|_| ())
    }

    /// Gives the focus to the window of Herdr.app that shows the workspace of the ticket: its
    /// title ends with the label of that workspace. With no such window, Herdr.app opens one
    /// on the session.
    pub fn show_window(&self, ticket: &str) -> Result<()> {
        let listed = run_with_timeout(
            Command::new(&self.plan.app).args(["window", "list", "--json"]),
            HERDR_TIMEOUT,
        );
        let windows: Vec<Value> = listed
            .ok()
            .and_then(|o| serde_json::from_slice(&o.stdout).ok())
            .unwrap_or_default();
        let suffix = format!(": {ticket}");
        let found = windows
            .iter()
            .find(|w| {
                w.get("title")
                    .and_then(Value::as_str)
                    .is_some_and(|t| t.ends_with(&suffix))
            })
            .and_then(|w| w.get("id").and_then(Value::as_u64));
        match found {
            Some(id) => {
                let output = run_with_timeout(
                    Command::new(&self.plan.app).args(["window", "focus", &id.to_string()]),
                    HERDR_TIMEOUT,
                )?;
                if !output.status.success() {
                    bail!("Herdr.app did not focus its window {id}");
                }
                Ok(())
            }
            None => self.open_window(),
        }
    }
}

/// Shows the tab of a running turn in its window of Herdr.app.
pub fn show(plan: &PanePlan, pane: &PaneRef, ticket: &str) -> Result<()> {
    let herdr = Herdr::new(plan.clone());
    herdr.ensure_running()?;
    herdr.focus(&pane.workspace, &pane.tab)?;
    herdr.show_window(ticket)
}

/// Opens a tab "you" in the workspace of the ticket, runs `line` there, and shows it: the
/// session of a paused ticket that the person takes over.
pub fn take_over(plan: &PanePlan, ticket: &str, workdir: &Path, line: &str) -> Result<PaneRef> {
    let herdr = Herdr::new(plan.clone());
    herdr.ensure_running()?;
    let (workspace, tab, pane) = herdr.open_tab(ticket, "you", workdir)?;
    herdr.run(&pane, line)?;
    herdr.focus(&workspace, &tab)?;
    herdr.show_window(ticket)?;
    Ok(PaneRef {
        session: plan.session.clone(),
        workspace,
        tab,
        pane,
    })
}

/// The id at a path of a JSON answer.
fn id_at(value: &Value, path: &[&str]) -> Result<String> {
    let mut at = value;
    for key in path {
        at = at.get(*key).unwrap_or(&Value::Null);
    }
    at.as_str()
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .with_context(|| format!("the answer of herdr has no {}", path.join(".")))
}

/// Runs a command with a time limit, and gives its output.
fn run_with_timeout(command: &mut Command, limit: Duration) -> Result<std::process::Output> {
    let mut child = command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    let (stdout, stderr) = (child.stdout.take(), child.stderr.take());
    let out = thread::spawn(move || {
        let mut bytes = Vec::new();
        if let Some(mut s) = stdout {
            let _ = s.read_to_end(&mut bytes);
        }
        bytes
    });
    let err = thread::spawn(move || {
        let mut bytes = Vec::new();
        if let Some(mut s) = stderr {
            let _ = s.read_to_end(&mut bytes);
        }
        bytes
    });
    let until = Instant::now() + limit;
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break status;
        }
        if Instant::now() >= until {
            let _ = child.kill();
            let _ = child.wait();
            bail!("it did not end in {} seconds", limit.as_secs());
        }
        thread::sleep(Duration::from_millis(20));
    };
    Ok(std::process::Output {
        status,
        stdout: out.join().unwrap_or_default(),
        stderr: err.join().unwrap_or_default(),
    })
}

/// The state of a folder dialog of Claude Code on the screen of a pane.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Dialog {
    /// No folder dialog waits at the bottom of the screen.
    None,
    /// A folder dialog waits with "No, exit" selected: Down selects "Yes".
    NoSelected,
    /// A folder dialog waits with "Yes" selected: Enter confirms it.
    YesSelected,
}

/// The folder dialog at the bottom of the screen, if one waits. Claude Code asks before an
/// interactive session works in a folder that it does not trust: the working folder ("Yes, I
/// trust this folder") and each folder of `--add-dir` ("Yes, add it"). A headless session never
/// asks. Only a dialog whose "Enter to confirm" line is among the last lines counts, so an old
/// dialog above the prompt of the shell is no dialog.
pub fn dialog(screen: &str) -> Dialog {
    let lines: Vec<&str> = screen
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect();
    let Some(confirm) = lines.iter().rposition(|l| l.contains("Enter to confirm")) else {
        return Dialog::None;
    };
    if confirm + 3 < lines.len() {
        return Dialog::None;
    }
    let above = &lines[..confirm];
    let known = above.iter().any(|l| {
        l.contains("Is this a project you created or one you trust")
            || l.contains("Adding a new directory")
    });
    let Some(no) = above.iter().rposition(|l| l.contains("No, exit")) else {
        return Dialog::None;
    };
    let Some(yes) = above.get(no + 1) else {
        return Dialog::None;
    };
    if !known || !(yes.contains("Yes, add it") || yes.contains("Yes, I trust this folder")) {
        return Dialog::None;
    }
    if above[no].starts_with('❯') {
        Dialog::NoSelected
    } else if yes.starts_with('❯') {
        Dialog::YesSelected
    } else {
        Dialog::None
    }
}

/// The last lines of a screen, for the detail of a session that did not start.
pub fn screen_tail(screen: &str) -> String {
    let lines: Vec<&str> = screen
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect();
    lines[lines.len().saturating_sub(3)..].join(" ")
}

// The transcript --------------------------------------------------------------------------------

/// Copies the messages of the transcript of a session as lines of the headless stream, and
/// counts its tokens.
pub struct Transcript {
    path: Option<PathBuf>,
    offset: u64,
    partial: String,
    session: String,
    seen: HashSet<String>,
    pub usage: crate::turn::stream::Usage,
    pub turns: i64,
}

impl Transcript {
    pub fn new(session: &str) -> Transcript {
        Transcript {
            path: None,
            offset: 0,
            partial: String::new(),
            session: session.to_string(),
            seen: HashSet::new(),
            usage: Default::default(),
            turns: 0,
        }
    }

    pub fn set_path(&mut self, path: PathBuf) {
        if self.path.as_ref() != Some(&path) {
            self.path = Some(path);
            self.offset = 0;
            self.partial.clear();
        }
    }

    /// The stream lines of the new lines of the transcript.
    pub fn poll(&mut self) -> Vec<Value> {
        let Some(path) = &self.path else {
            return Vec::new();
        };
        let Ok(mut file) = std::fs::File::open(path) else {
            return Vec::new();
        };
        if file.seek(SeekFrom::Start(self.offset)).is_err() {
            return Vec::new();
        }
        let mut text = String::new();
        let mut reader = BufReader::new(&mut file);
        let mut read = 0u64;
        loop {
            let mut line = String::new();
            match reader.read_line(&mut line) {
                Ok(0) => break,
                Ok(n) => {
                    read += n as u64;
                    text.push_str(&line);
                }
                Err(_) => break,
            }
        }
        self.offset += read;
        let mut all = std::mem::take(&mut self.partial);
        all.push_str(&text);
        let mut out = Vec::new();
        let mut rest = all.as_str();
        while let Some(end) = rest.find('\n') {
            let line = &rest[..end];
            rest = &rest[end + 1..];
            if let Ok(value) = serde_json::from_str::<Value>(line) {
                out.extend(self.line(&value));
            }
        }
        self.partial = rest.to_string();
        out
    }

    /// The stream line of one line of the transcript, or none.
    pub fn line(&mut self, value: &Value) -> Option<Value> {
        let kind = value.get("type").and_then(Value::as_str)?;
        if !matches!(kind, "user" | "assistant")
            || value.get("isSidechain").and_then(Value::as_bool) == Some(true)
        {
            return None;
        }
        let message = value.get("message")?;
        if kind == "user" && local_command(message) {
            return None;
        }
        if kind == "assistant" {
            let id = message.get("id").and_then(Value::as_str).unwrap_or("");
            if !id.is_empty() && self.seen.insert(id.to_string()) {
                self.turns += 1;
                if let Some(usage) = message.get("usage") {
                    let count = |key: &str| usage.get(key).and_then(Value::as_i64).unwrap_or(0);
                    self.usage.input += count("input_tokens");
                    self.usage.cache_creation += count("cache_creation_input_tokens");
                    self.usage.cache_read += count("cache_read_input_tokens");
                    self.usage.output += count("output_tokens");
                }
            }
        }
        Some(json!({"type": kind, "message": message, "session_id": self.session}))
    }
}

/// True for a user message of a local command, such as `/exit`, which is not text for the
/// agent.
fn local_command(message: &Value) -> bool {
    let text = match message.get("content") {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Array(blocks)) => blocks
            .iter()
            .filter_map(|b| b.get("text").and_then(Value::as_str))
            .collect::<Vec<_>>()
            .join(""),
        _ => return false,
    };
    let text = text.trim_start();
    text.starts_with("<local-command") || text.starts_with("<command-name>")
}

// The end of a turn -----------------------------------------------------------------------------

/// What a `Stop` with this last message means.
#[derive(Debug, PartialEq)]
pub enum Stop {
    /// A result object that fits the schema.
    Result(Value),
    /// No such object: the problem, for the reminder.
    NoResult(String),
}

pub fn judge_stop(message: Option<&str>, schema: &Value) -> Stop {
    let Some(text) = message.map(str::trim).filter(|t| !t.is_empty()) else {
        return Stop::NoResult("your last message is empty".into());
    };
    let Some(object) = crate::turn::codex::parse_result(text) else {
        return Stop::NoResult("your last message is not one JSON object".into());
    };
    let problems = schema::problems(schema, &object);
    if problems.is_empty() {
        Stop::Result(object)
    } else {
        Stop::NoResult(format!(
            "the JSON object of your last message does not fit the schema: {}",
            problems.join("; ")
        ))
    }
}

/// The reminder to end the turn with the result object.
pub fn reminder(problem: &str) -> String {
    format!(
        "Vision: {problem}. If the step is done, or if you need the person, end your turn now with the result object as your whole last message: one JSON object that fits the schema of your role, with no other text."
    )
}

/// What a `StopFailure` with this error means: `Some(text)` ends the turn, `None` lets the
/// supervisor type `continue`.
pub fn stop_failure(error: &str, message: Option<&str>) -> Option<String> {
    let said = message
        .map(str::trim)
        .filter(|m| !m.is_empty())
        .map(|m| format!(": {}", m.chars().take(300).collect::<String>()))
        .unwrap_or_default();
    match error {
        "server_error" | "overloaded" => None,
        "rate_limit" => Some(format!("Claude hit a usage limit{said}")),
        other => Some(format!("Claude stopped with the API error `{other}`{said}")),
    }
}

// The supervisor --------------------------------------------------------------------------------

/// The supervisor of a turn in a pane.
pub fn supervise(
    ctx: &Ctx,
    ledger: &Ledger,
    turn: &TurnPaths,
    plan: &Plan,
    pane_plan: &PanePlan,
    mut status: TurnState,
) -> Result<()> {
    let herdr = Herdr::new(pane_plan.clone());
    let end = |status: TurnState, reason: Reason, detail: String| {
        super::supervisor::finish(ctx, turn, status.ended(reason, Some(detail)), None)
    };
    if let Err(e) = herdr.ensure_running() {
        return end(status, Reason::Error, format!("{e:#}"));
    }
    close_ended_tabs(ctx, ledger, &herdr, &plan.ticket, &plan.turn);
    let (workspace, tab, pane) = match herdr.open_tab(&plan.ticket, &plan.agent, &plan.workdir) {
        Ok(ids) => ids,
        Err(e) => {
            return end(
                status,
                Reason::Error,
                format!(
                    "could not open a tab for the turn in the herdr session `{}`: {e:#}",
                    pane_plan.session
                ),
            )
        }
    };
    status.pane = Some(PaneRef {
        session: pane_plan.session.clone(),
        workspace,
        tab,
        pane: pane.clone(),
    });
    let pid_file = turn.dir.join(PID_FILE);
    let script = turn.dir.join(SCRIPT_FILE);
    let env = settings::session_env_of_process(&plan.env_pass);
    atomic_write(
        &script,
        script_text(plan, &env, &pid_file, &plan.first_message).as_bytes(),
    )?;
    let mut perms = std::fs::metadata(&script)?.permissions();
    std::os::unix::fs::PermissionsExt::set_mode(&mut perms, 0o700);
    std::fs::set_permissions(&script, perms)?;
    if let Err(e) = herdr.run(&pane, &shell_quote(&script.display().to_string())) {
        return end(
            status,
            Reason::Error,
            format!("could not start the session in its pane: {e:#}"),
        );
    }
    status.state = Phase::Running;
    status.at = time::now_rfc3339();
    status.write(&turn.status)?;
    ctx.log(&format!(
        "turn {}: runs in the pane {pane} of the herdr session {}",
        plan.turn, pane_plan.session
    ));

    let schema = roles::schema(&ctx.paths, &plan.agent)?;
    let mut watch = Watch::new(plan, turn);
    watch.sent.insert(plan.first_message.trim().to_string());
    let outcome = watch.run(ctx, ledger, &herdr, &pane, &schema, &mut status);
    let pid = watch.pid;
    if let Some(pid) = pid {
        // Stop what is left of the session, such as a command in the background.
        state::signal_group(pid, libc::SIGKILL);
        let _ = unsafe { libc::kill(pid as i32, libc::SIGKILL) };
    }
    for line in watch.transcript.poll() {
        watch.write(&line);
    }
    let (reason, detail, object) = match outcome {
        Outcome::Result(object) => (Reason::Result, None, Some(object)),
        Outcome::Forced(reason, detail) => (reason, Some(detail), None),
    };
    let cost = read_cost(&turn.dir.join(STATUS_LINE_FILE));
    let result_line = json!({
        "type": "result",
        "subtype": if object.is_some() { "success" } else { "error_during_execution" },
        "is_error": object.is_none(),
        "result": watch.last_message.clone(),
        "structured_output": object.clone(),
        "session_id": plan.session,
        "total_cost_usd": cost,
        "num_turns": watch.transcript.turns,
        "usage": {
            "input_tokens": watch.transcript.usage.input,
            "cache_creation_input_tokens": watch.transcript.usage.cache_creation,
            "cache_read_input_tokens": watch.transcript.usage.cache_read,
            "output_tokens": watch.transcript.usage.output,
        },
        "permission_denials": [],
    });
    watch.write(&result_line);
    status.session = Some(plan.session.clone());
    super::supervisor::finish(ctx, turn, status.ended(reason, detail), object)
}

/// The cost of the session from its status line, or none.
fn read_cost(path: &Path) -> Option<f64> {
    let text = std::fs::read_to_string(path).ok()?;
    let value: Value = serde_json::from_str(text.trim()).ok()?;
    value
        .get("cost")
        .and_then(|c| c.get("total_cost_usd"))
        .and_then(Value::as_f64)
}

/// Closes the tab of each ended turn of the ticket, so that the workspace shows the new turn
/// and the turns that run. A tab that is gone already counts as closed.
fn close_ended_tabs(ctx: &Ctx, ledger: &Ledger, herdr: &Herdr, ticket: &str, current: &str) {
    let Ok(key) = crate::paths::TicketKey::parse(ticket) else {
        return;
    };
    let Ok(turns) = ledger.turns_of_ticket(ticket) else {
        return;
    };
    for row in turns {
        if row.id == current {
            continue;
        }
        let Ok(id) = crate::paths::TurnId::parse(&row.id) else {
            continue;
        };
        let paths = ctx.paths.turn(&key, &id);
        let Ok(Some(state)) = TurnState::read(&paths.status) else {
            continue;
        };
        let Some(pane) = state.pane.as_ref() else {
            continue;
        };
        if state.is_ended()
            && pane.session == herdr.plan.session
            && !paths.dir.join(".tab-closed").exists()
        {
            let _ = herdr.close_tab(&pane.tab);
            let _ = std::fs::write(paths.dir.join(".tab-closed"), "");
        }
    }
}

enum Outcome {
    Result(Value),
    Forced(Reason, String),
}

/// The state of the watch of a session in a pane.
struct Watch {
    stream: Option<std::fs::File>,
    hooks_path: PathBuf,
    hooks_offset: u64,
    inbox: PathBuf,
    inbox_sent: PathBuf,
    pid_file: PathBuf,
    pid: Option<u32>,
    transcript: Transcript,
    /// Each prompt that Vision typed: a prompt that is not here came from the person.
    sent: HashSet<String>,
    last_message: Option<String>,
    deadline: Instant,
    time_limit: u64,
    contract_mode: String,
    model: String,
    session: String,
}

impl Watch {
    fn new(plan: &Plan, turn: &TurnPaths) -> Watch {
        Watch {
            stream: OpenOptions::new()
                .create(true)
                .append(true)
                .open(&turn.stream)
                .ok(),
            hooks_path: turn.dir.join(HOOKS_FILE),
            hooks_offset: 0,
            inbox: turn.inbox.clone(),
            inbox_sent: turn.inbox_sent.clone(),
            pid_file: turn.dir.join(PID_FILE),
            pid: None,
            transcript: Transcript::new(&plan.session),
            sent: HashSet::new(),
            last_message: None,
            deadline: Instant::now() + Duration::from_secs(plan.time_limit_secs),
            time_limit: plan.time_limit_secs,
            contract_mode: plan.contract.permission_mode.clone(),
            model: plan.model.clone(),
            session: plan.session.clone(),
        }
    }

    fn write(&mut self, line: &Value) {
        if let Some(file) = self.stream.as_mut() {
            if writeln!(file, "{line}")
                .and_then(|()| file.flush())
                .is_err()
            {
                self.stream = None;
            }
        }
    }

    /// The new hook events.
    fn hooks(&mut self) -> Vec<Value> {
        let Ok(mut file) = std::fs::File::open(&self.hooks_path) else {
            return Vec::new();
        };
        if file.seek(SeekFrom::Start(self.hooks_offset)).is_err() {
            return Vec::new();
        }
        let mut text = String::new();
        if file.read_to_string(&mut text).is_err() {
            return Vec::new();
        }
        // Only whole lines: a line that the hook still writes waits for the next read.
        let whole = text.rfind('\n').map_or(0, |i| i + 1);
        self.hooks_offset += whole as u64;
        text[..whole]
            .lines()
            .filter_map(|l| serde_json::from_str::<Value>(l).ok())
            .filter(|v| v.get("session_id").and_then(Value::as_str) == Some(self.session.as_str()))
            .collect()
    }

    fn read_pid(&mut self) -> Option<u32> {
        if self.pid.is_none() {
            self.pid = std::fs::read_to_string(&self.pid_file)
                .ok()
                .and_then(|t| t.trim().parse().ok());
        }
        self.pid
    }

    fn stop(&mut self) {
        if let Some(pid) = self.read_pid() {
            state::signal_group(pid, libc::SIGTERM);
            let _ = unsafe { libc::kill(pid as i32, libc::SIGTERM) };
            let until = Instant::now() + TERM_GRACE;
            while Instant::now() < until && crate::util::process::pid_alive(pid) {
                thread::sleep(Duration::from_millis(20));
            }
            state::signal_group(pid, libc::SIGKILL);
            let _ = unsafe { libc::kill(pid as i32, libc::SIGKILL) };
        }
    }

    fn prompt(&mut self, herdr: &Herdr, pane: &str, text: &str) -> bool {
        self.sent.insert(text.trim().to_string());
        herdr.prompt(pane, text).is_ok()
    }

    #[allow(clippy::too_many_lines)]
    fn run(
        &mut self,
        ctx: &Ctx,
        ledger: &Ledger,
        herdr: &Herdr,
        pane: &str,
        schema: &Value,
        status: &mut TurnState,
    ) -> Outcome {
        let started = Instant::now();
        let mut session_started = false;
        let mut answers = 0u32;
        let mut last_screen: Option<Instant> = None;
        let mut last_inbox: Option<Instant> = None;
        let mut person_at: Option<Instant> = None;
        let mut reminders = 0u32;
        let mut continues = 0u32;
        let mut waits_for_person = false;
        let mut result: Option<Value> = None;
        let mut result_at: Option<Instant> = None;
        let mut checked_mode = false;
        let force = |reason: Reason, detail: String| Outcome::Forced(reason, detail);
        loop {
            let signal = super::supervisor::SIGNAL.load(Ordering::SeqCst);
            if signal != 0 {
                self.stop();
                return force(
                    Reason::Killed,
                    format!("the supervisor got the signal {signal}"),
                );
            }
            if Instant::now() >= self.deadline {
                self.stop();
                return force(
                    Reason::Timeout,
                    format!("the time limit of {} seconds passed", self.time_limit),
                );
            }
            for event in self.hooks() {
                let name = event
                    .get("hook_event_name")
                    .and_then(Value::as_str)
                    .unwrap_or("");
                match name {
                    "SessionStart" => {
                        session_started = true;
                        if let Some(path) = event.get("transcript_path").and_then(Value::as_str) {
                            self.transcript.set_path(PathBuf::from(path));
                        }
                        let model = event
                            .get("model")
                            .and_then(Value::as_str)
                            .unwrap_or(&self.model)
                            .to_string();
                        let init = json!({"type": "system", "subtype": "init", "session_id": self.session, "model": model, "tools": [], "mcp_servers": []});
                        self.write(&init);
                    }
                    "UserPromptSubmit" => {
                        if !checked_mode {
                            checked_mode = true;
                            if let Some(mode) = event.get("permission_mode").and_then(Value::as_str)
                            {
                                if mode != self.contract_mode {
                                    self.stop();
                                    return force(
                                        Reason::Contract,
                                        format!(
                                            "the session runs in the permission mode {mode}, and the contract is {}",
                                            self.contract_mode
                                        ),
                                    );
                                }
                            }
                        }
                        let text = event.get("prompt").and_then(Value::as_str).unwrap_or("");
                        if !self.sent.contains(text.trim()) {
                            person_at = Some(Instant::now());
                            waits_for_person = false;
                            ctx.log(&format!(
                                "turn {}: the person wrote in the pane",
                                status.turn
                            ));
                        }
                    }
                    "Stop" => {
                        let message = event.get("last_assistant_message").and_then(Value::as_str);
                        self.last_message = message.map(str::to_string);
                        match judge_stop(message, schema) {
                            Stop::Result(object) => {
                                result = Some(object);
                                result_at = Some(Instant::now());
                                let _ = herdr.prompt(pane, "/exit");
                            }
                            Stop::NoResult(problem) => {
                                if person_at.is_some_and(|at| at.elapsed() < PERSON_QUIET) {
                                    waits_for_person = true;
                                } else if reminders < REMINDERS {
                                    reminders += 1;
                                    self.prompt(herdr, pane, &reminder(&problem));
                                } else {
                                    self.stop();
                                    return force(
                                        Reason::Error,
                                        format!("the session gave no result object: {problem}"),
                                    );
                                }
                            }
                        }
                    }
                    "StopFailure" => {
                        let error = event
                            .get("error")
                            .and_then(Value::as_str)
                            .unwrap_or("unknown");
                        let message = event.get("last_assistant_message").and_then(Value::as_str);
                        match stop_failure(error, message) {
                            None if continues < CONTINUES => {
                                continues += 1;
                                self.prompt(herdr, pane, "continue");
                            }
                            None => {
                                self.stop();
                                return force(
                                    Reason::Error,
                                    format!("Claude stopped with the API error `{error}` again after {CONTINUES} times continue"),
                                );
                            }
                            Some(detail) => {
                                self.stop();
                                return force(Reason::Error, detail);
                            }
                        }
                    }
                    "Notification" => {
                        let kind = event
                            .get("notification_type")
                            .and_then(Value::as_str)
                            .unwrap_or("");
                        if kind == "permission_prompt" {
                            ctx.log(&format!(
                                "turn {}: Claude waits for your approval in its pane. Answer it in Herdr.app",
                                status.turn
                            ));
                        }
                    }
                    "SessionEnd" => {
                        return match result.take() {
                            Some(object) => Outcome::Result(object),
                            None => {
                                let why = event
                                    .get("reason")
                                    .and_then(Value::as_str)
                                    .unwrap_or("other");
                                force(
                                    Reason::Killed,
                                    format!(
                                        "the session in the pane ended before its result ({why})"
                                    ),
                                )
                            }
                        };
                    }
                    _ => {}
                }
            }
            for line in self.transcript.poll() {
                self.write(&line);
            }
            if let Some(object) = &result {
                let pid_gone = self
                    .read_pid()
                    .is_some_and(|pid| !crate::util::process::pid_alive(pid));
                if pid_gone || result_at.is_some_and(|at| at.elapsed() >= EXIT_GRACE) {
                    let object = object.clone();
                    self.stop();
                    return Outcome::Result(object);
                }
            } else {
                if last_inbox.is_none_or(|at| at.elapsed() >= super::supervisor::INBOX_EVERY) {
                    last_inbox = Some(Instant::now());
                    // A message of the inbox is from the person.
                    if self.deliver(ledger, herdr, pane) > 0 {
                        person_at = Some(Instant::now());
                        waits_for_person = false;
                    }
                }
                if waits_for_person && person_at.is_some_and(|at| at.elapsed() >= PERSON_QUIET) {
                    waits_for_person = false;
                    person_at = None;
                    if reminders < REMINDERS {
                        reminders += 1;
                        self.prompt(
                            herdr,
                            pane,
                            &reminder("the person wrote nothing for 15 minutes"),
                        );
                    }
                }
                if !session_started {
                    if started.elapsed() >= START_LIMIT {
                        self.stop();
                        return force(
                            Reason::Error,
                            format!(
                                "the session did not start in its pane in {} seconds. Look at the pane in Herdr.app",
                                START_LIMIT.as_secs()
                            ),
                        );
                    }
                    // A folder dialog: Down selects "Yes", and Enter goes only after a new read
                    // of the screen shows "Yes" selected, so a late key never picks "No, exit".
                    if answers < DIALOG_ANSWERS
                        && last_screen.is_none_or(|at| at.elapsed() >= DIALOG_EVERY)
                    {
                        last_screen = Some(Instant::now());
                        let screen = herdr.read(pane).unwrap_or_default();
                        match dialog(&screen) {
                            Dialog::NoSelected => {
                                answers += 1;
                                let _ = herdr.send_keys(pane, &["down"]);
                            }
                            Dialog::YesSelected => {
                                answers += 1;
                                let _ = herdr.send_keys(pane, &["enter"]);
                                ctx.log(&format!(
                                    "turn {}: trusted a folder in the dialog of Claude Code",
                                    status.turn
                                ));
                            }
                            Dialog::None => {}
                        }
                        if self
                            .read_pid()
                            .is_some_and(|pid| !crate::util::process::pid_alive(pid))
                        {
                            return force(
                                Reason::Error,
                                format!(
                                    "the session in the pane exited before it started: {}",
                                    screen_tail(&screen)
                                ),
                            );
                        }
                    }
                }
                if session_started
                    && self
                        .read_pid()
                        .is_some_and(|pid| !crate::util::process::pid_alive(pid))
                {
                    return force(
                        Reason::Killed,
                        "the pane closed, or herdr restarted, before the result".into(),
                    );
                }
            }
            thread::sleep(POLL);
        }
    }

    /// Types each waiting message of the inbox into the session. Gives how many it typed.
    fn deliver(&mut self, ledger: &Ledger, herdr: &Herdr, pane: &str) -> usize {
        let Ok(waiting) = inbox::waiting(&self.inbox) else {
            return 0;
        };
        let mut typed = 0;
        for (path, message) in waiting {
            if !self.prompt(herdr, pane, &message.text) {
                break;
            }
            typed += 1;
            let _ = inbox::move_to(&path, &self.inbox_sent);
            if let Some(id) = message.id {
                let _ = ledger.set_message_delivered(id, &time::now_rfc3339());
            }
        }
        typed
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_interactive_arguments_leave_out_each_flag_of_print_mode() {
        let args: Vec<String> = [
            "-p",
            "--model",
            "m",
            "--effort",
            "high",
            "--input-format",
            "stream-json",
            "--output-format",
            "stream-json",
            "--verbose",
            "--replay-user-messages",
            "--restricted",
            "--tools",
            "Read,Bash",
            "--permission-prompts",
            "none",
            "--json-schema",
            "{}",
            "--max-budget-usd",
            "25",
            "--session-id",
            "s",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        assert_eq!(
            interactive_args(&args),
            [
                "--model",
                "m",
                "--effort",
                "high",
                "--restricted",
                "--tools",
                "Read,Bash",
                "--session-id",
                "s"
            ]
        );
    }

    #[test]
    fn the_hooks_and_the_status_line_join_the_settings_of_the_turn() {
        let text = r#"{"sandbox": {"enabled": true}, "hooks": {"Stop": [{"hooks": [{"type": "command", "command": "own"}]}]}}"#;
        let out = with_hooks(text, Path::new("/bin/vision x"), Path::new("/t/it's")).unwrap();
        let value: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(value["sandbox"]["enabled"], true);
        let command = r"'/bin/vision x' turn-hook '/t/it'\''s'";
        for event in HOOK_EVENTS {
            let list = value["hooks"][event].as_array().unwrap();
            assert_eq!(
                list.last().unwrap()["hooks"][0]["command"],
                command,
                "{event}"
            );
        }
        assert_eq!(
            value["hooks"]["Stop"].as_array().unwrap().len(),
            2,
            "the own hook stays"
        );
        assert_eq!(
            value["statusLine"]["command"],
            format!("{command} --status-line")
        );
    }

    #[test]
    fn a_stop_ends_the_turn_only_with_a_result_object_that_fits_the_schema() {
        let schema = json!({"type": "object", "required": ["kind"], "properties": {"kind": {"type": "string", "enum": ["result", "questions"]}}});
        assert_eq!(
            judge_stop(Some(r#"{"kind":"result"}"#), &schema),
            Stop::Result(json!({"kind": "result"}))
        );
        assert_eq!(
            judge_stop(
                Some("Done.\n```json\n{\"kind\":\"questions\"}\n```"),
                &schema
            ),
            Stop::Result(json!({"kind": "questions"}))
        );
        assert!(
            matches!(judge_stop(Some("I fixed it."), &schema), Stop::NoResult(p) if p.contains("not one JSON object"))
        );
        assert!(matches!(judge_stop(None, &schema), Stop::NoResult(p) if p.contains("empty")));
        assert!(
            matches!(judge_stop(Some(r#"{"kind":"maybe"}"#), &schema), Stop::NoResult(p) if p.contains("does not fit the schema"))
        );
    }

    #[test]
    fn a_short_api_failure_gets_continue_and_a_limit_ends_the_turn() {
        assert_eq!(stop_failure("overloaded", None), None);
        assert_eq!(stop_failure("server_error", Some("x")), None);
        assert_eq!(
            stop_failure("rate_limit", Some("You've hit your limit · resets 3pm")).as_deref(),
            Some("Claude hit a usage limit: You've hit your limit · resets 3pm")
        );
        assert_eq!(
            stop_failure("invalid_request", None).as_deref(),
            Some("Claude stopped with the API error `invalid_request`")
        );
    }

    #[test]
    fn a_folder_dialog_at_the_bottom_of_the_screen_gets_down_then_enter() {
        let add = " Adding a new directory:\n\n /home/me/.claude/skills/qa\n\n This session hasn't worked here before. Is this a directory you created or one you trust?\n\n Security guide\n\n ❯ No, exit\n   Yes, add it\n\n Enter to confirm · Esc to cancel\n";
        assert_eq!(dialog(add), Dialog::NoSelected);
        let picked = add.replace("❯ No, exit\n   Yes", "  No, exit\n ❯ Yes");
        assert_eq!(dialog(&picked), Dialog::YesSelected);
        let trust = " Quick safety check: Is this a project you created or one you trust? (Like your own code)\n\n ❯ No, exit\n   Yes, I trust this folder\n\n Enter to confirm · Esc to cancel";
        assert_eq!(dialog(trust), Dialog::NoSelected);
        // An old dialog above the prompt of the shell is no dialog.
        let old = format!("{add}To go on without it, start again with the same flags.\nAdd --resume to pick up an earlier conversation.\n➜  clone git:(main)\n");
        assert_eq!(dialog(&old), Dialog::None);
        assert_eq!(
            dialog("❯ No, exit\n  Yes, add it\nEnter to confirm"),
            Dialog::None,
            "another text"
        );
        assert_eq!(screen_tail("a\n\nb\nc\nd\n"), "b c d");
    }

    #[test]
    fn the_transcript_gives_the_messages_once_and_counts_each_answer_once() {
        let mut t = Transcript::new("s-1");
        let user = json!({"type": "user", "message": {"role": "user", "content": "Do the step."}});
        assert_eq!(
            t.line(&user),
            Some(
                json!({"type": "user", "message": {"role": "user", "content": "Do the step."}, "session_id": "s-1"})
            )
        );
        let usage = json!({"input_tokens": 2, "cache_creation_input_tokens": 10, "cache_read_input_tokens": 100, "output_tokens": 5});
        let first = json!({"type": "assistant", "message": {"id": "m1", "content": [{"type": "text", "text": "a"}], "usage": usage}});
        let second = json!({"type": "assistant", "message": {"id": "m1", "content": [{"type": "tool_use", "name": "Read", "input": {}}], "usage": usage}});
        assert!(t.line(&first).is_some());
        assert!(
            t.line(&second).is_some(),
            "each block of an answer is a line of the stream"
        );
        assert_eq!(
            (t.turns, t.usage.input, t.usage.cache_read, t.usage.output),
            (1, 2, 100, 5)
        );
        let exit =
            json!({"type": "user", "message": {"content": "<command-name>/exit</command-name>"}});
        assert_eq!(
            t.line(&exit),
            None,
            "a local command is not text for the agent"
        );
        let side = json!({"type": "assistant", "isSidechain": true, "message": {"id": "m2"}});
        assert_eq!(t.line(&side), None);
        assert_eq!(t.line(&json!({"type": "attachment"})), None);
    }

    #[test]
    fn the_script_passes_only_the_allow_list_and_quotes_each_word() {
        let plan: Plan = serde_json::from_value(json!({
            "turn": "T-1", "ticket": "DEMO-1", "agent": "builder", "kind": "worker",
            "program": "/bin/claude", "args": ["-p", "--model", "m", "--json-schema", "{}"],
            "workdir": "/w/it's", "env_pass": [], "env_set": {"GIT_CONFIG_GLOBAL": "/g"},
            "first_message": "Turn T-1.", "time_limit_secs": 60,
            "contract": {"tools": [], "permission_mode": "acceptEdits", "mcp_servers": []},
            "session": "s", "model": "m", "effort": "high", "schema": "turn.schema.json"
        }))
        .unwrap();
        let env = vec![
            (OsString::from("PATH"), OsString::from("/bin")),
            (OsString::from("TERM"), OsString::from("dumb")),
            (
                OsString::from("GIT_CONFIG_GLOBAL"),
                OsString::from("/other"),
            ),
        ];
        let text = script_text(&plan, &env, Path::new("/t/pane.pid"), "Turn T-1.");
        assert!(text.contains("echo $$ > '/t/pane.pid'\n"), "{text}");
        assert!(text.contains(r"cd '/w/it'\''s' || exit 1"), "{text}");
        assert!(text.contains(r#"exec env -i TERM="${TERM:-xterm-256color}" COLORTERM="${COLORTERM:-}" 'PATH=/bin' 'GIT_CONFIG_GLOBAL=/g' '/bin/claude' '--model' 'm' 'Turn T-1.'"#), "{text}");
        assert!(!text.contains("dumb"), "TERM comes from the pane");
        assert!(
            !text.contains("/other"),
            "a value that the runner sets wins"
        );
    }
}
