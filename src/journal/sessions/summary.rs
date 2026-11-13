//! The summary of one session of Claude Code (section 16). Code builds it from the session
//! file, with no model call, so it costs nothing.
//!
//! A session file has one JSON event on each line. The summary reads these events:
//!
//! - `user`: a message that the person typed, or a line that the harness adds with the role of
//!   the user: a tool result (a block `tool_result`), a system reminder or a hook text
//!   (`isMeta`), the summary of a compaction (`isCompactSummary`), a notice of a background task
//!   (`origin.kind` = `task-notification`), and a local command such as `/clear` with its output
//!   (text in tags such as `<command-name>` and `<local-command-stdout>`). A version of Claude
//!   Code that marks the origin of its lines gives a typed message `origin.kind` = `human`; in a
//!   file with such marks, a user line with no mark is a line of the harness.
//! - `assistant`: text blocks, and tool calls (`tool_use` blocks). A line with
//!   `isApiErrorMessage` is an error text of the harness, not an answer.
//! - `custom-title`, `ai-title`, and the older `summary`: the title of the session.
//!
//! Each event has a `timestamp`, and most have `cwd` and `gitBranch`. A line that is not JSON
//! is skipped.

use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::Deserialize;
use serde_json::Value;

use crate::journal::errors::{self, cut};
use crate::memory::front::{parse_entry, render_value, ParsedLine, Value as FrontValue};

/// The kind of a summary entry in its front matter.
pub const KIND: &str = "claude-session";
/// The most asks of one summary.
pub const MAX_ASKS: usize = 30;
/// The longest ask.
pub const MAX_ASK_CHARS: usize = 200;
/// The most files of one summary.
pub const MAX_FILES: usize = 50;
/// The most commits of one summary.
pub const MAX_COMMITS: usize = 30;
/// The longest outcome.
pub const MAX_OUTCOME_CHARS: usize = 600;
/// The longest summary entry.
pub const MAX_ENTRY_CHARS: usize = 20_000;

/// The starts of a text that the harness adds with the role of the user.
const HARNESS_STARTS: [&str; 9] = [
    "<local-command-",
    "<command-",
    "<bash-",
    "<system-reminder>",
    "<task-notification>",
    "<user-prompt-submit-hook>",
    "[Request interrupted",
    "Caveat:",
    "This session is being continued",
];

/// The facts of one session.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Summary {
    pub session: String,
    pub project: String,
    pub cwd: Option<String>,
    pub branch: Option<String>,
    pub title: Option<String>,
    pub started: Option<String>,
    pub ended: Option<String>,
    /// The messages that the person typed: all of them, not only the asks that show.
    pub messages: usize,
    pub tool_calls: usize,
    /// The key of the session file in the archive.
    pub archive: String,
    /// The first line of each message that the person typed, at most [`MAX_ASKS`].
    pub asks: Vec<String>,
    pub files: Vec<String>,
    /// The lines of the section Errors and fixes.
    pub errors: Vec<String>,
    /// The lines of the section Commits.
    pub commits: Vec<String>,
    pub outcome: Option<String>,
}

/// The few fields of an event that the summary reads on each line. The rest of a line is
/// skipped, not kept, so that a large event costs little.
#[derive(Deserialize, Default)]
#[serde(default)]
struct Head {
    #[serde(rename = "type")]
    kind: Option<Value>,
    timestamp: Option<Value>,
    cwd: Option<Value>,
    #[serde(rename = "gitBranch")]
    git_branch: Option<Value>,
    #[serde(rename = "aiTitle")]
    ai_title: Option<Value>,
    #[serde(rename = "customTitle")]
    custom_title: Option<Value>,
    summary: Option<Value>,
}

/// The text of a JSON value that is a string with more than white space.
fn text_of(value: &Option<Value>) -> Option<&str> {
    value
        .as_ref()
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|t| !t.is_empty())
}

fn flag(value: &Value, name: &str) -> bool {
    value.get(name).and_then(Value::as_bool) == Some(true)
}

/// The reader of a session file, one event at a time.
#[derive(Default)]
pub struct Reader {
    summary: Summary,
    calls: errors::Calls,
    /// True when a user line of the file has a mark of its origin.
    marks_origin: bool,
    /// The asks of the lines with the mark `human`, and their count.
    marked: (Vec<String>, usize),
    /// The asks of the lines with no mark, and their count.
    unmarked: (Vec<String>, usize),
    ai_title: Option<String>,
    custom_title: Option<String>,
    old_title: Option<String>,
    last_text: Option<String>,
}

impl Reader {
    pub fn new(session: &str, project: &str, archive: &str) -> Reader {
        Reader {
            summary: Summary {
                session: session.to_string(),
                project: project.to_string(),
                archive: archive.to_string(),
                ..Summary::default()
            },
            ..Reader::default()
        }
    }

    /// Reads one line of the file.
    pub fn line(&mut self, line: &str) {
        let line = line.trim();
        if line.is_empty() {
            return;
        }
        let Ok(head) = serde_json::from_str::<Head>(line) else {
            return;
        };
        if let Some(at) = text_of(&head.timestamp) {
            if self.summary.started.is_none() {
                self.summary.started = Some(at.to_string());
            }
            self.summary.ended = Some(at.to_string());
        }
        if self.summary.cwd.is_none() {
            self.summary.cwd = text_of(&head.cwd).map(str::to_string);
        }
        if let Some(branch) = text_of(&head.git_branch).filter(|b| *b != "HEAD") {
            self.summary.branch = Some(branch.to_string());
        }
        match head.kind.as_ref().and_then(Value::as_str).unwrap_or("") {
            "ai-title" => self.ai_title = text_of(&head.ai_title).map(str::to_string),
            "custom-title" => self.custom_title = text_of(&head.custom_title).map(str::to_string),
            "summary" => self.old_title = text_of(&head.summary).map(str::to_string),
            "user" => {
                if let Ok(value) = serde_json::from_str::<Value>(line) {
                    self.user(&value);
                }
            }
            "assistant" => {
                if let Ok(value) = serde_json::from_str::<Value>(line) {
                    self.assistant(&value);
                }
            }
            _ => {}
        }
    }

    fn user(&mut self, value: &Value) {
        if flag(value, "isSidechain") {
            return;
        }
        self.calls.add(value);
        let marked = value.get("origin").is_some();
        self.marks_origin |= marked;
        if let Some(ask) = ask_of(value) {
            let (asks, count) = if marked {
                &mut self.marked
            } else {
                &mut self.unmarked
            };
            *count += 1;
            if asks.len() < MAX_ASKS {
                asks.push(ask);
            }
        }
    }

    fn assistant(&mut self, value: &Value) {
        if flag(value, "isSidechain") {
            return;
        }
        self.calls.add(value);
        let synthetic = flag(value, "isApiErrorMessage")
            || value.pointer("/message/model").and_then(Value::as_str) == Some("<synthetic>");
        for block in value
            .pointer("/message/content")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            match block.get("type").and_then(Value::as_str) {
                Some("tool_use") => self.summary.tool_calls += 1,
                Some("text") if !synthetic => {
                    if let Some(text) = block
                        .get("text")
                        .and_then(Value::as_str)
                        .map(str::trim)
                        .filter(|t| !t.is_empty())
                    {
                        self.last_text = Some(text.to_string());
                    }
                }
                _ => {}
            }
        }
    }

    /// The summary of the lines read.
    pub fn finish(self) -> Summary {
        let mut summary = self.summary;
        // In a file that marks the origin of its lines, a line with no mark is the harness.
        let (asks, count) = if self.marks_origin {
            self.marked
        } else {
            self.unmarked
        };
        summary.asks = asks;
        summary.messages = count;
        summary.title = self.custom_title.or(self.ai_title).or(self.old_title);
        let facts = self.calls.finish();
        let cwd = summary.cwd.clone();
        let mut files: Vec<String> = facts
            .files
            .iter()
            .map(|f| relative(f, cwd.as_deref()))
            .collect();
        if files.len() > MAX_FILES {
            let more = files.len() - MAX_FILES;
            files.truncate(MAX_FILES);
            files.push(format!("{more} more file(s)"));
        }
        summary.files = files;
        let (items, left_out) = errors::errors_and_fixes(&facts.calls);
        summary.errors = items.iter().map(errors::render).collect();
        if left_out > 0 {
            summary
                .errors
                .push(format!("- {left_out} more failed call(s) are left out."));
        }
        let commits: Vec<String> = facts
            .calls
            .iter()
            .filter(|c| c.ok && errors::commits(&c.command))
            .filter_map(commit_line)
            .collect();
        let more = commits.len().saturating_sub(MAX_COMMITS);
        summary.commits = commits.into_iter().take(MAX_COMMITS).collect();
        if more > 0 {
            summary.commits.push(format!("- {more} more commit(s)"));
        }
        summary.outcome = self.last_text.map(|t| cut(&t, MAX_OUTCOME_CHARS));
        summary
    }
}

/// Reads a whole session file.
pub fn read_file(path: &Path, session: &str, project: &str, archive: &str) -> Result<Summary> {
    let file = File::open(path).with_context(|| format!("could not read {}", path.display()))?;
    let mut reader = Reader::new(session, project, archive);
    for line in BufReader::new(file).lines() {
        match line {
            Ok(line) => reader.line(&line),
            // A line that is not UTF-8 is skipped, as a line that is not JSON is.
            Err(e) if e.kind() == std::io::ErrorKind::InvalidData => continue,
            Err(e) => return Err(e).with_context(|| format!("could not read {}", path.display())),
        }
    }
    Ok(reader.finish())
}

/// The first line of a message that the person typed, or `None` for a line of the harness. A
/// skill or command that the person typed, such as `/review 12`, shows as that command.
pub fn ask_of(value: &Value) -> Option<String> {
    if [
        "isMeta",
        "isCompactSummary",
        "isVisibleInTranscriptOnly",
        "isSidechain",
    ]
    .iter()
    .any(|name| flag(value, name))
    {
        return None;
    }
    let origin = value.get("origin").map(|o| {
        o.get("kind")
            .and_then(Value::as_str)
            .or_else(|| o.as_str())
            .unwrap_or("")
    });
    if origin.is_some_and(|kind| kind != "human") {
        return None;
    }
    let texts: Vec<&str> = match value.pointer("/message/content") {
        Some(Value::String(text)) => vec![text.as_str()],
        Some(Value::Array(blocks)) => {
            if blocks
                .iter()
                .any(|b| b.get("type").and_then(Value::as_str) == Some("tool_result"))
            {
                return None;
            }
            blocks
                .iter()
                .filter(|b| b.get("type").and_then(Value::as_str) == Some("text"))
                .filter_map(|b| b.get("text").and_then(Value::as_str))
                .collect()
        }
        _ => return None,
    };
    for text in texts {
        let text = text.trim();
        if text.is_empty() {
            continue;
        }
        if text.contains("<command-name>") {
            // A command of the person, or a local command of the harness such as /clear.
            return (origin == Some("human"))
                .then(|| command_ask(text))
                .flatten();
        }
        if HARNESS_STARTS.iter().any(|start| text.starts_with(start)) {
            continue;
        }
        let first = text.lines().map(str::trim).find(|l| !l.is_empty())?;
        return Some(cut(first, MAX_ASK_CHARS));
    }
    None
}

/// The text between `<tag>` and `</tag>`, trimmed.
fn inside<'a>(text: &'a str, tag: &str) -> Option<&'a str> {
    let open = format!("<{tag}>");
    let close = format!("</{tag}>");
    let start = text.find(&open)? + open.len();
    let end = text[start..].find(&close)? + start;
    Some(text[start..end].trim())
}

/// A command that the person typed, such as `/review 12`.
fn command_ask(text: &str) -> Option<String> {
    let name = inside(text, "command-name").filter(|n| !n.is_empty())?;
    let name = if name.starts_with('/') {
        name.to_string()
    } else {
        format!("/{name}")
    };
    let args = inside(text, "command-args").unwrap_or("");
    let line = format!("{name} {args}");
    let first = line.lines().next().unwrap_or("").trim();
    Some(cut(first, MAX_ASK_CHARS))
}

/// A path relative to the working folder, when it is in it.
fn relative(path: &str, cwd: Option<&str>) -> String {
    match cwd.and_then(|dir| Path::new(path).strip_prefix(dir).ok()) {
        Some(rest) if !rest.as_os_str().is_empty() => rest.display().to_string(),
        _ => path.to_string(),
    }
}

/// The line of one commit: its hash and its first message line. The output of `git commit`
/// gives both (`[branch abc1234] message`). With `-q`, the message comes from the command, and
/// the hash from a line of the output that holds it right before the start of the message, as
/// `git log --oneline` prints it.
fn commit_line(call: &errors::BashCall) -> Option<String> {
    let message = message_of(&call.command);
    let bracket = errors::commit_of(&call.output);
    let subject = call
        .output
        .lines()
        .find_map(|line| {
            let line = line.trim();
            let inside = line.strip_prefix('[')?;
            let (head, rest) = inside.split_once(']')?;
            let last = head.split_whitespace().last()?;
            (Some(last) == bracket.as_deref()).then(|| rest.trim().to_string())
        })
        .filter(|s| !s.is_empty())
        .or_else(|| message.clone());
    let hash = bracket.or_else(|| {
        message
            .as_deref()
            .and_then(|m| hash_before(&call.output, m))
    });
    match (hash, subject) {
        (Some(hash), Some(subject)) => Some(format!("- {hash} {}", cut(&subject, 200))),
        (Some(hash), None) => Some(format!("- {hash}")),
        (None, Some(subject)) => Some(format!("- {} (no hash shown)", cut(&subject, 200))),
        (None, None) => None,
    }
}

/// A hash of 7 to 40 hex digits right before the start of the message on a line of the
/// output, such as `98f1cb3 Add the export`.
fn hash_before(output: &str, message: &str) -> Option<String> {
    let start: String = message.chars().take(24).collect();
    if start.trim().is_empty() {
        return None;
    }
    let needle = format!(" {start}");
    output.lines().find_map(|line| {
        let at = line.find(&needle)?;
        let word = line[..at].split_whitespace().last()?;
        ((7..=40).contains(&word.len()) && word.chars().all(|c| c.is_ascii_hexdigit()))
            .then(|| word.to_string())
    })
}

/// The first non-empty line after the line that starts a here document (`<<'EOF'`) at or after
/// `from`.
fn here_document(command: &str, from: usize) -> Option<String> {
    let start = command[from..].find("<<")? + from;
    command[start..]
        .lines()
        .skip(1)
        .map(str::trim)
        .find(|l| !l.is_empty())
        .map(str::to_string)
}

/// The first line of the message of `git commit`: the text of `-m` (quoted or a bare word), or
/// the first line of a here document, such as `-m "$(cat <<'EOF' ... EOF)"` or
/// `-F - <<'EOF' ... EOF`.
pub fn message_of(command: &str) -> Option<String> {
    for form in ["-F - ", "-F- ", "--file=- ", "--file - "] {
        if let Some(at) = command.find(form) {
            if let Some(line) = here_document(command, at) {
                return Some(line);
            }
        }
    }
    let at = command
        .match_indices("-m")
        .find(|(at, _)| {
            command[..*at].ends_with(char::is_whitespace)
                && command[at + 2..].starts_with([' ', '"', '\''])
        })
        .map(|(at, _)| at + 2)?;
    let rest = command[at..].trim_start();
    if rest.starts_with("\"$(cat <<") {
        // The words of the message start on the line after `<<'EOF'`.
        return here_document(command, at);
    }
    let text = match rest.chars().next()? {
        quote @ ('"' | '\'') => {
            let body = &rest[1..];
            let mut out = String::new();
            let mut chars = body.chars();
            while let Some(c) = chars.next() {
                if c == quote {
                    break;
                }
                if c == '\\' && quote == '"' {
                    if let Some(next) = chars.next() {
                        out.push(next);
                    }
                    continue;
                }
                out.push(c);
            }
            out
        }
        _ => rest.split_whitespace().next()?.to_string(),
    };
    text.lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .map(str::to_string)
}

/// A line of text that starts with `#` gets a space in front, so that it never starts a section
/// of the entry.
fn no_heading(text: &str) -> String {
    text.lines()
        .map(|line| {
            if line.starts_with('#') {
                format!(" {line}")
            } else {
                line.to_string()
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// The text of a summary entry. The caller masks the secrets in it and cuts it.
pub fn render(summary: &Summary) -> String {
    let mut out = String::from("---\n");
    let mut key = |name: &str, value: FrontValue| {
        out.push_str(&format!("{name}: {}\n", render_value(&value)));
    };
    key("id", FrontValue::str(&summary.session));
    key("kind", FrontValue::str(KIND));
    key("project", FrontValue::str(&summary.project));
    key("cwd", FrontValue::opt_str(summary.cwd.as_deref()));
    key("branch", FrontValue::opt_str(summary.branch.as_deref()));
    key(
        "title",
        FrontValue::opt_str(summary.title.as_deref().map(one_line).as_deref()),
    );
    key("started", FrontValue::opt_str(summary.started.as_deref()));
    key("ended", FrontValue::opt_str(summary.ended.as_deref()));
    key("messages", FrontValue::num(summary.messages as u64));
    key("tool_calls", FrontValue::num(summary.tool_calls as u64));
    key("archive", FrontValue::str(&summary.archive));
    out.push_str("---\n");
    let place = summary.cwd.as_deref().unwrap_or(&summary.project);
    match &summary.title {
        Some(title) => out.push_str(&format!("# {}: a session in {place}\n", one_line(title))),
        None => out.push_str(&format!("# A session in {place}\n")),
    }
    let mut section = |title: &str, body: &str| {
        if !body.trim().is_empty() {
            out.push_str(&format!("\n## {title}\n\n{}\n", body.trim_end()));
        }
    };
    let list = |items: &[String]| -> String {
        items
            .iter()
            .map(|item| format!("- {}", one_line(item)))
            .collect::<Vec<_>>()
            .join("\n")
    };
    section("Asks", &list(&summary.asks));
    section("Files", &list(&summary.files));
    section("Errors and fixes", &summary.errors.join("\n"));
    section("Commits", &summary.commits.join("\n"));
    section(
        "Outcome",
        &no_heading(summary.outcome.as_deref().unwrap_or("")),
    );
    out
}

fn one_line(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// A summary entry read back: its front matter, its first ask, and its text.
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize)]
pub struct SessionEntry {
    pub session: String,
    pub project: String,
    pub started: Option<String>,
    pub title: Option<String>,
    pub archive: Option<String>,
    pub first_ask: Option<String>,
    pub path: PathBuf,
    #[serde(skip)]
    pub text: String,
}

/// Reads the text of a summary entry. `None` when it has no front matter, no `id`, or another
/// kind.
pub fn parse(text: &str, path: &Path) -> Option<SessionEntry> {
    let mut lines = text.lines();
    if lines.next()? != "---" {
        return None;
    }
    let mut entry = SessionEntry {
        path: path.to_path_buf(),
        text: text.to_string(),
        ..SessionEntry::default()
    };
    let mut kind = String::new();
    for line in lines {
        if line == "---" {
            break;
        }
        let Ok(ParsedLine::Entry(item)) = parse_entry(line) else {
            continue;
        };
        let value = item.value.as_text().map(str::to_string);
        match item.key.as_str() {
            "id" => entry.session = value.unwrap_or_default(),
            "kind" => kind = value.unwrap_or_default(),
            "project" => entry.project = value.unwrap_or_default(),
            "started" => entry.started = value,
            "title" => entry.title = value,
            "archive" => entry.archive = value,
            _ => {}
        }
    }
    entry.first_ask = crate::journal::entry::section(text, "Asks")
        .and_then(|asks| asks.lines().next())
        .map(|line| line.trim_start_matches("- ").trim().to_string());
    (!entry.session.is_empty() && kind == KIND).then_some(entry)
}

/// Reads a summary entry file.
pub fn read_entry(path: &Path) -> Option<SessionEntry> {
    parse(&std::fs::read_to_string(path).ok()?, path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn summary_of(lines: &[Value]) -> Summary {
        let mut reader = Reader::new(
            "s-1",
            "-home-me-work",
            "vision/sessions/-home-me-work/s-1.jsonl.gz",
        );
        for line in lines {
            reader.line(&line.to_string());
        }
        reader.finish()
    }

    #[test]
    fn a_typed_message_is_an_ask_and_each_line_of_the_harness_is_not() {
        let typed = |text: &str| json!({"type": "user", "origin": {"kind": "human"}, "message": {"role": "user", "content": text}});
        let lines = vec![
            typed("Export the report as CSV\nwith a header"),
            json!({"type": "user", "isMeta": true, "message": {"content": [{"type": "text", "text": "<system-reminder>be brief</system-reminder>"}]}}),
            json!({"type": "user", "origin": {"kind": "task-notification"}, "message": {"content": "<task-notification><task-id>1</task-id></task-notification>"}}),
            json!({"type": "user", "message": {"content": "<command-name>/clear</command-name>\n<command-message>clear</command-message>\n<command-args></command-args>"}}),
            json!({"type": "user", "message": {"content": "<local-command-stdout>done</local-command-stdout>"}}),
            json!({"type": "user", "isCompactSummary": true, "message": {"content": "This session is being continued from an earlier one."}}),
            json!({"type": "user", "message": {"content": [{"type": "tool_result", "tool_use_id": "t", "content": "secret output"}]}}),
            json!({"type": "user", "origin": {"kind": "human"}, "message": {"content": "<command-message>review</command-message>\n<command-name>/review</command-name>\n<command-args>12</command-args>"}}),
            json!({"type": "user", "message": {"content": [{"type": "text", "text": "[Request interrupted by user]"}]}}),
            json!({"type": "user", "message": {"content": "/compact"}}),
            typed("  \n  Then add a total row  "),
        ];
        let summary = summary_of(&lines);
        assert_eq!(
            summary.asks,
            [
                "Export the report as CSV",
                "/review 12",
                "Then add a total row"
            ]
        );
        assert_eq!(summary.messages, 3);
    }

    #[test]
    fn a_file_with_no_marks_of_origin_counts_each_plain_user_line() {
        let lines = vec![
            json!({"type": "user", "message": {"content": "Fix the build"}}),
            json!({"type": "user", "message": {"content": [{"type": "text", "text": "<system-reminder>x</system-reminder>"}, {"type": "text", "text": "and run the tests"}]}}),
            json!({"type": "user", "message": {"content": "<local-command-stdout>x</local-command-stdout>"}}),
        ];
        let summary = summary_of(&lines);
        assert_eq!(summary.asks, ["Fix the build", "and run the tests"]);
    }

    #[test]
    fn the_summary_reads_times_titles_files_commits_errors_and_the_outcome() {
        let tool = |id: &str, name: &str, input: Value| json!({"type": "assistant", "timestamp": "2026-11-02T10:05:00.000Z", "message": {"content": [{"type": "tool_use", "id": id, "name": name, "input": input}]}});
        let result = |id: &str, text: &str, error: bool| json!({"type": "user", "message": {"content": [{"type": "tool_result", "tool_use_id": id, "content": text, "is_error": error}]}});
        let lines = vec![
            json!({"type": "ai-title", "aiTitle": "Export report", "sessionId": "s-1"}),
            json!({"type": "user", "timestamp": "2026-11-02T10:00:00.000Z", "cwd": "/home/me/work", "gitBranch": "main", "message": {"content": "Add the export"}}),
            tool(
                "e1",
                "Edit",
                json!({"file_path": "/home/me/work/src/export.rs"}),
            ),
            tool("w1", "Write", json!({"file_path": "/elsewhere/notes.txt"})),
            tool("b1", "Bash", json!({"command": "cargo test"})),
            result(
                "b1",
                "Exit code 101\nerror[E0425]: cannot find value `rows`",
                true,
            ),
            tool(
                "b2",
                "Bash",
                json!({"command": "git commit -am \"Add the export\""}),
            ),
            result(
                "b2",
                "[feature/export 1a2b3c4] Add the export\n 1 file changed",
                false,
            ),
            tool("b3", "Bash", json!({"command": "cargo test"})),
            result("b3", "test result: ok", false),
            tool(
                "b4",
                "Bash",
                json!({"command": "git commit -q -m \"$(cat <<'EOF'\nAdd the total row\n\nBody.\nEOF\n)\""}),
            ),
            result("b4", "", false),
            json!({"type": "assistant", "gitBranch": "feature/export", "message": {"content": [{"type": "text", "text": "## Done\nThe export works."}]}}),
            json!({"type": "assistant", "isApiErrorMessage": true, "timestamp": "2026-11-02T11:00:00.000Z", "message": {"model": "<synthetic>", "content": [{"type": "text", "text": "API Error: overloaded"}]}}),
            json!({"type": "custom-title", "customTitle": "The CSV export"}),
            "not json at all".into(),
        ];
        let summary = summary_of(&lines);
        assert_eq!(summary.started.as_deref(), Some("2026-11-02T10:00:00.000Z"));
        assert_eq!(summary.ended.as_deref(), Some("2026-11-02T11:00:00.000Z"));
        assert_eq!(summary.cwd.as_deref(), Some("/home/me/work"));
        assert_eq!(summary.branch.as_deref(), Some("feature/export"));
        assert_eq!(summary.title.as_deref(), Some("The CSV export"));
        assert_eq!(summary.tool_calls, 6);
        assert_eq!(summary.files, ["src/export.rs", "/elsewhere/notes.txt"]);
        assert_eq!(
            summary.commits,
            [
                "- 1a2b3c4 Add the export",
                "- Add the total row (no hash shown)"
            ]
        );
        assert_eq!(summary.errors.len(), 1);
        assert!(
            summary.errors[0].ends_with("Passed later, after the commit 1a2b3c4."),
            "{:?}",
            summary.errors
        );
        assert_eq!(
            summary.outcome.as_deref(),
            Some("## Done\nThe export works.")
        );
        let text = render(&summary);
        assert!(text.starts_with("---\nid: s-1\nkind: claude-session\nproject: -home-me-work\ncwd: /home/me/work\nbranch: feature/export\n"), "{text}");
        assert!(text.contains("\nmessages: 1\ntool_calls: 6\narchive: vision/sessions/-home-me-work/s-1.jsonl.gz\n---\n# The CSV export: a session in /home/me/work\n"), "{text}");
        assert!(text.contains("\n## Asks\n\n- Add the export\n"), "{text}");
        assert!(
            text.contains(
                "\n## Commits\n\n- 1a2b3c4 Add the export\n- Add the total row (no hash shown)\n"
            ),
            "{text}"
        );
        assert!(
            text.contains("\n## Outcome\n\n ## Done\nThe export works.\n"),
            "{text}"
        );
        assert!(!text.contains("API Error"), "{text}");
        let back = parse(&text, Path::new("/j/s-1.md")).unwrap();
        assert_eq!(back.session, "s-1");
        assert_eq!(back.project, "-home-me-work");
        assert_eq!(back.first_ask.as_deref(), Some("Add the export"));
        assert_eq!(
            back.archive.as_deref(),
            Some("vision/sessions/-home-me-work/s-1.jsonl.gz")
        );
        assert!(parse("---\nid: T1\nticket: DEMO-1\n---\n", Path::new("/x")).is_none());
    }

    #[test]
    fn the_message_of_a_commit_comes_from_the_command() {
        assert_eq!(
            message_of("git commit -m 'Fix x'").as_deref(),
            Some("Fix x")
        );
        assert_eq!(
            message_of("git add . && git commit -m \"Say \\\"hi\\\"\"").as_deref(),
            Some("Say \"hi\"")
        );
        assert_eq!(message_of("git commit -m word").as_deref(), Some("word"));
        assert_eq!(message_of("git commit --amend"), None);
        assert_eq!(message_of("git commit -mx"), None);
        assert_eq!(
            message_of(
                "git add -A && git commit -q -F - <<'EOF'\nchore: save the work\n\nBody.\nEOF"
            )
            .as_deref(),
            Some("chore: save the work")
        );
    }

    #[test]
    fn a_quiet_commit_takes_its_hash_from_a_line_of_the_log() {
        let call = |command: &str, output: &str| errors::BashCall {
            command: command.into(),
            ok: true,
            exit: None,
            output: output.into(),
        };
        let quiet = call(
            "git commit -q -F - <<'EOF'\nchore: save the work\nEOF\ngit log --oneline -1",
            "Scanning 3 files...\n98f1cb3 chore: save the work",
        );
        assert_eq!(
            commit_line(&quiet).as_deref(),
            Some("- 98f1cb3 chore: save the work")
        );
        let in_a_loop = call(
            "for d in app web; do (cd $d && git commit -q -m \"chore: kit 0.5\" && git log --oneline -1); done",
            "app b379c10 chore: kit 0.5\nweb 7d509cd chore: kit 0.5",
        );
        assert_eq!(
            commit_line(&in_a_loop).as_deref(),
            Some("- b379c10 chore: kit 0.5")
        );
        let nothing = call(
            "git commit -q -m \"chore: x\"",
            "(Bash completed with no output)",
        );
        assert_eq!(
            commit_line(&nothing).as_deref(),
            Some("- chore: x (no hash shown)")
        );
    }

    #[test]
    fn many_asks_and_files_have_a_limit() {
        let mut lines = Vec::new();
        for n in 0..60 {
            lines.push(json!({"type": "user", "message": {"content": format!("ask {n} {}", "x".repeat(300))}}));
            lines.push(json!({"type": "assistant", "message": {"content": [{"type": "tool_use", "id": format!("e{n}"), "name": "Edit", "input": {"file_path": format!("/f{n}.rs")}}]}}));
        }
        let summary = summary_of(&lines);
        assert_eq!(summary.asks.len(), MAX_ASKS);
        assert_eq!(summary.messages, 60);
        assert!(summary.asks[0].chars().count() <= MAX_ASK_CHARS);
        assert_eq!(summary.files.len(), MAX_FILES + 1);
        assert_eq!(summary.files.last().unwrap(), "10 more file(s)");
    }
}
