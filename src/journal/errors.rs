//! The errors and fixes of a turn, from its `stream.jsonl` (section 16): each Bash call that
//! failed, with its command and the first useful lines of its error. When a later call of the
//! same command in the same turn passed, the item says so, with the commit that a call between
//! them made.
//!
//! A failed Bash call of Claude Code has a tool result with `is_error: true` and a text that
//! starts with `Exit code N`. A real session showed this.

use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::Path;

use anyhow::{Context, Result};
use serde_json::Value;

/// The most items of one entry.
pub const MAX_ITEMS: usize = 20;
/// The longest text of one item.
pub const MAX_ITEM_CHARS: usize = 600;
/// The most lines of an error in one item.
const MAX_LINES: usize = 4;
/// The longest line of an error, and the longest command.
const MAX_LINE_CHARS: usize = 200;

/// The words that make a line of an error useful.
const ERROR_WORDS: [&str; 14] = [
    "error",
    "fail",
    "panic",
    "exception",
    "traceback",
    "not found",
    "no such",
    "denied",
    "cannot",
    "can't",
    "undefined",
    "refused",
    "fatal",
    "expected",
];

/// One Bash call of a turn and its result.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BashCall {
    pub command: String,
    pub ok: bool,
    /// The exit code that the result names.
    pub exit: Option<i32>,
    pub output: String,
}

/// One failed command of a turn.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ErrorFix {
    pub command: String,
    pub exit: Option<i32>,
    /// The first useful lines of the error.
    pub lines: Vec<String>,
    /// How many calls of the command failed with the same first line.
    pub times: usize,
    /// A later call of the same command passed.
    pub passed_later: bool,
    /// A call between the failure and the pass made a commit: its hash, when its output shows
    /// one, or an empty text.
    pub commit: Option<String>,
}

/// The tools that change a file, with the key of their input that names it.
const FILE_TOOLS: [(&str, &str); 4] = [
    ("Edit", "file_path"),
    ("Write", "file_path"),
    ("MultiEdit", "file_path"),
    ("NotebookEdit", "notebook_path"),
];

/// What the journal reads in a stream: the Bash calls in their order, and the files that a
/// file tool changed, each one once.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct StreamFacts {
    pub calls: Vec<BashCall>,
    pub files: Vec<String>,
}

/// Reads a stream file. A line that is not JSON is ignored, and a missing file has no facts.
pub fn read_stream(path: &Path) -> Result<StreamFacts> {
    let file = match File::open(path) {
        Ok(file) => file,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(StreamFacts::default()),
        Err(e) => return Err(e).with_context(|| format!("could not read {}", path.display())),
    };
    let mut calls = Calls::default();
    for line in BufReader::new(file).lines() {
        let Ok(line) = line else {
            continue;
        };
        if let Ok(value) = serde_json::from_str::<Value>(line.trim()) {
            calls.add(&value);
        }
    }
    Ok(calls.facts)
}

/// The facts of stream lines.
pub fn facts_of(text: &str) -> StreamFacts {
    let mut calls = Calls::default();
    for line in text.lines() {
        if let Ok(value) = serde_json::from_str::<Value>(line.trim()) {
            calls.add(&value);
        }
    }
    calls.facts
}

/// The Bash calls of stream lines.
pub fn calls_of(text: &str) -> Vec<BashCall> {
    facts_of(text).calls
}

/// The reader of a stream, one event at a time, so that a large session file never sits in
/// memory whole. A session file of Claude Code has the same events as `stream.jsonl`.
#[derive(Default)]
pub struct Calls {
    /// The Bash calls that wait for their result: tool use id -> command.
    open: Vec<(String, String)>,
    facts: StreamFacts,
}

impl Calls {
    /// The facts of the events so far.
    pub fn finish(self) -> StreamFacts {
        self.facts
    }

    /// Reads one event: a tool call of an assistant event, or a tool result of a user event.
    pub fn add(&mut self, value: &Value) {
        let blocks = value
            .pointer("/message/content")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        match value.get("type").and_then(Value::as_str) {
            Some("assistant") => {
                for block in &blocks {
                    if block.get("type").and_then(Value::as_str) != Some("tool_use") {
                        continue;
                    }
                    let name = block.get("name").and_then(Value::as_str).unwrap_or("");
                    if let Some((_, key)) = FILE_TOOLS.iter().find(|(tool, _)| *tool == name) {
                        if let Some(file) = block
                            .get("input")
                            .and_then(|i| i.get(*key))
                            .and_then(Value::as_str)
                        {
                            if !self.facts.files.iter().any(|f| f == file) {
                                self.facts.files.push(file.to_string());
                            }
                        }
                    }
                    if name == "Bash" {
                        let id = block.get("id").and_then(Value::as_str).unwrap_or("");
                        let command = block
                            .pointer("/input/command")
                            .and_then(Value::as_str)
                            .unwrap_or("");
                        self.open.push((id.to_string(), command.to_string()));
                    }
                }
            }
            Some("user") => {
                for block in &blocks {
                    if block.get("type").and_then(Value::as_str) != Some("tool_result") {
                        continue;
                    }
                    let id = block
                        .get("tool_use_id")
                        .and_then(Value::as_str)
                        .unwrap_or("");
                    let Some(at) = self.open.iter().position(|(open, _)| open == id) else {
                        continue;
                    };
                    let (_, command) = self.open.remove(at);
                    let output = result_text(block.get("content"));
                    let exit = exit_code(&output);
                    let is_error = block
                        .get("is_error")
                        .and_then(Value::as_bool)
                        .unwrap_or(false);
                    let ok = !is_error && exit.is_none_or(|code| code == 0);
                    self.facts.calls.push(BashCall {
                        command,
                        ok,
                        exit,
                        output,
                    });
                }
            }
            _ => {}
        }
    }
}

/// The text of a tool result: a string, or the texts of its blocks.
fn result_text(content: Option<&Value>) -> String {
    match content {
        Some(Value::String(text)) => text.clone(),
        Some(Value::Array(blocks)) => blocks
            .iter()
            .filter_map(|b| b.get("text").and_then(Value::as_str))
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    }
}

/// The code of a first line `Exit code N`.
fn exit_code(output: &str) -> Option<i32> {
    let first = output.lines().next()?.trim();
    first.strip_prefix("Exit code ")?.trim().parse().ok()
}

/// A command with each run of white space as one space.
fn normal(command: &str) -> String {
    command.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// True for a command that makes a commit.
pub fn commits(command: &str) -> bool {
    let words: Vec<&str> = command.split_whitespace().collect();
    words
        .iter()
        .position(|w| *w == "git")
        .is_some_and(|at| words[at..].contains(&"commit"))
}

/// The hash of the commit that the output of `git commit` names: `[branch abc1234] message`.
pub fn commit_of(output: &str) -> Option<String> {
    output.lines().find_map(|line| {
        let inside = line.trim().strip_prefix('[')?.split(']').next()?;
        let last = inside.split_whitespace().last()?;
        ((7..=40).contains(&last.len()) && last.chars().all(|c| c.is_ascii_hexdigit()))
            .then(|| last.to_string())
    })
}

/// Cuts a text to `max` characters, with `...` at a cut.
pub fn cut(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    let kept: String = text.chars().take(max.saturating_sub(3)).collect();
    format!("{kept}...")
}

/// The first useful lines of the output of a failed call: the lines that name an error, or
/// else the last lines. A line with the form of a token is left out.
fn useful_lines(output: &str) -> Vec<String> {
    let lines: Vec<&str> = output
        .lines()
        .enumerate()
        .filter(|(at, line)| !(*at == 0 && line.trim().starts_with("Exit code ")))
        .map(|(_, line)| line.trim())
        .filter(|line| line.chars().any(char::is_alphanumeric))
        .collect();
    let named: Vec<&str> = lines
        .iter()
        .copied()
        .filter(|line| {
            let lower = line.to_lowercase();
            ERROR_WORDS.iter().any(|w| lower.contains(w))
        })
        .take(MAX_LINES)
        .collect();
    let chosen: Vec<&str> = if named.is_empty() {
        let skip = lines.len().saturating_sub(MAX_LINES);
        lines[skip..].to_vec()
    } else {
        named
    };
    chosen
        .into_iter()
        .map(|line| match crate::executor::gates::token_form(line) {
            Some(form) => format!("(a line with {form} is left out)"),
            None => cut(line, MAX_LINE_CHARS),
        })
        .collect()
}

/// The failed commands of the calls, each one once for its first line, in their order, with
/// what came later in the same turn. At most [`MAX_ITEMS`]; the second value counts the items
/// that were left out.
pub fn errors_and_fixes(calls: &[BashCall]) -> (Vec<ErrorFix>, usize) {
    let mut items: Vec<ErrorFix> = Vec::new();
    let mut left_out = 0;
    for (at, call) in calls.iter().enumerate() {
        if call.ok {
            continue;
        }
        let lines = useful_lines(&call.output);
        let command = normal(&call.command);
        if let Some(same) = items
            .iter_mut()
            .find(|i| i.command == command && i.lines.first() == lines.first())
        {
            same.times += 1;
            continue;
        }
        if items.len() >= MAX_ITEMS {
            left_out += 1;
            continue;
        }
        let pass = calls[at + 1..]
            .iter()
            .position(|later| later.ok && normal(&later.command) == command)
            .map(|offset| at + 1 + offset);
        let commit = pass.and_then(|pass| {
            calls[at + 1..pass]
                .iter()
                .rev()
                .find(|c| c.ok && commits(&c.command))
                .map(|c| commit_of(&c.output).unwrap_or_default())
        });
        items.push(ErrorFix {
            command,
            exit: call.exit,
            lines,
            times: 1,
            passed_later: pass.is_some(),
            commit,
        });
    }
    (items, left_out)
}

/// One item as a line of the entry, at most [`MAX_ITEM_CHARS`] characters.
pub fn render(item: &ErrorFix) -> String {
    let command = match crate::executor::gates::token_form(&item.command) {
        Some(form) => format!("(a command with {form})"),
        None => cut(&item.command.replace('`', "'"), MAX_LINE_CHARS),
    };
    let mut text = format!("- `{command}` failed");
    if let Some(code) = item.exit {
        text.push_str(&format!(" (exit {code})"));
    }
    if item.times > 1 {
        text.push_str(&format!(", {} times", item.times));
    }
    if item.lines.is_empty() {
        text.push('.');
    } else {
        text.push_str(&format!(": {}", item.lines.join(" | ")));
    }
    if item.passed_later {
        match item.commit.as_deref() {
            Some("") => text.push_str(" Passed later, after a commit."),
            Some(hash) => text.push_str(&format!(" Passed later, after the commit {hash}.")),
            None => text.push_str(" Passed later."),
        }
    }
    cut(&text, MAX_ITEM_CHARS)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn call(id: &str, command: &str) -> String {
        json!({"type": "assistant", "message": {"content": [
            {"type": "tool_use", "id": id, "name": "Bash", "input": {"command": command}}]}})
        .to_string()
    }

    fn answer(id: &str, content: &str, is_error: bool) -> String {
        json!({"type": "user", "message": {"content": [
            {"type": "tool_result", "tool_use_id": id, "content": content, "is_error": is_error}]}})
        .to_string()
    }

    #[test]
    fn a_failure_that_passed_later_names_the_commit_between() {
        let stream = [
            call("a", "cargo test --test calc"),
            answer(
                "a",
                "Exit code 101\n   Compiling calc\nerror[E0425]: cannot find value `x`\n  --> src/lib.rs:3:5",
                true,
            ),
            call("b", "git add -A && git commit -m 'Fix x'"),
            answer("b", "[vision/DEMO-12 1a2b3c4] Fix x\n 1 file changed", false),
            call("c", "cargo  test --test calc"),
            answer("c", "test result: ok", false),
        ]
        .join("\n");
        let calls = calls_of(&stream);
        assert_eq!(calls.len(), 3);
        assert!(!calls[0].ok && calls[1].ok && calls[2].ok);
        let (items, left_out) = errors_and_fixes(&calls);
        assert_eq!(left_out, 0);
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].exit, Some(101));
        assert_eq!(items[0].commit.as_deref(), Some("1a2b3c4"));
        let line = render(&items[0]);
        assert!(
            line.starts_with(
                "- `cargo test --test calc` failed (exit 101): error[E0425]: cannot find value `x`"
            ),
            "{line}"
        );
        assert!(
            line.ends_with("Passed later, after the commit 1a2b3c4."),
            "{line}"
        );
    }

    #[test]
    fn an_exit_code_without_the_error_flag_still_fails_and_a_tool_of_another_name_does_not_count() {
        let stream = [
            call("a", "make lint"),
            answer(
                "a",
                "Exit code 2\nlint.sh: line 4: rubocop: command not found",
                false,
            ),
            json!({"type": "assistant", "message": {"content": [
                {"type": "tool_use", "id": "r", "name": "Read", "input": {"file_path": "/x"}}]}})
            .to_string(),
            answer(
                "r",
                "<tool_use_error>File does not exist.</tool_use_error>",
                true,
            ),
            "not json".to_string(),
        ]
        .join("\n");
        let (items, _) = errors_and_fixes(&calls_of(&stream));
        assert_eq!(items.len(), 1);
        assert!(!items[0].passed_later);
        assert_eq!(
            items[0].lines,
            ["lint.sh: line 4: rubocop: command not found"]
        );
    }

    #[test]
    fn repeats_count_once_the_items_have_a_limit_and_a_token_line_is_left_out() {
        let mut lines = Vec::new();
        for n in 0..3 {
            lines.push(call(&format!("r{n}"), "npm test"));
            lines.push(answer(&format!("r{n}"), "Exit code 1\nError: boom", true));
        }
        for n in 0..25 {
            lines.push(call(&format!("c{n}"), &format!("false {n}")));
            lines.push(answer(&format!("c{n}"), "Exit code 1", true));
        }
        let token = format!("{}{}", "ghp_", "a".repeat(36));
        lines.push(call("t", "curl api"));
        lines.push(answer(
            "t",
            &format!("Exit code 22\nerror: token {token} refused"),
            true,
        ));
        let (items, left_out) = errors_and_fixes(&calls_of(&lines.join("\n")));
        assert_eq!(items.len(), MAX_ITEMS);
        assert_eq!(items[0].times, 3);
        assert_eq!(left_out, 26 - (MAX_ITEMS - 1));
        let (items, _) = errors_and_fixes(&calls_of(
            &[
                call("t", "curl api"),
                answer(
                    "t",
                    &format!("Exit code 22\nerror: token {token} refused"),
                    true,
                ),
            ]
            .join("\n"),
        ));
        let line = render(&items[0]);
        assert!(!line.contains(&token), "{line}");
        assert!(line.contains("is left out"), "{line}");
    }

    #[test]
    fn a_long_error_is_cut_and_blocks_of_text_count() {
        let long = "error: ".to_string() + &"x".repeat(2000);
        let stream = [
            call("a", "run"),
            json!({"type": "user", "message": {"content": [{"type": "tool_result",
                "tool_use_id": "a", "is_error": true,
                "content": [{"type": "text", "text": format!("Exit code 3\n{long}\n{long}\n{long}\n{long}\nerror: five")}]}]}})
            .to_string(),
        ]
        .join("\n");
        let (items, _) = errors_and_fixes(&calls_of(&stream));
        assert_eq!(items[0].lines.len(), 4);
        let line = render(&items[0]);
        assert!(line.chars().count() <= MAX_ITEM_CHARS, "{}", line.len());
        assert!(line.ends_with("..."));
    }

    #[test]
    fn the_files_of_the_file_tools_count_once() {
        let edit = |id: &str, name: &str, key: &str, path: &str| {
            json!({"type": "assistant", "message": {"content": [
                {"type": "tool_use", "id": id, "name": name, "input": {key: path}}]}})
            .to_string()
        };
        let stream = [
            edit("1", "Edit", "file_path", "/clone/src/a.rs"),
            edit("2", "Write", "file_path", "/clone/src/b.rs"),
            edit("3", "Edit", "file_path", "/clone/src/a.rs"),
            edit("4", "Read", "file_path", "/clone/src/c.rs"),
            edit("5", "NotebookEdit", "notebook_path", "/clone/n.ipynb"),
        ]
        .join("\n");
        assert_eq!(
            facts_of(&stream).files,
            ["/clone/src/a.rs", "/clone/src/b.rs", "/clone/n.ipynb"]
        );
    }

    #[test]
    fn the_hash_of_a_commit_comes_from_its_first_line() {
        assert_eq!(
            commit_of("[main (root-commit) abc1234] First").as_deref(),
            Some("abc1234")
        );
        assert_eq!(commit_of("nothing to commit"), None);
        assert_eq!(commit_of("[main zz] x"), None);
        assert!(commits("git -c a=b commit -m x"));
        assert!(!commits("git status"));
    }
}
