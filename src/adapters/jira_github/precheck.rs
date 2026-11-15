//! `precheck`: the job that runs the lint, the specs, and the migrations of the project on the
//! machine before a push, so that a failure shows here and not in CI (design section 11.5).
//!
//! Its table in adapter.toml holds the commands. `{files}` in a command is the changed files of
//! the branch against its base, deleted files left out, filtered by the globs of the command,
//! each one quoted for the shell. `{base}` is the base commit, and `{version}` the version of a
//! migration. A command runs with `sh -c` in the clone, with the environment of the call: the
//! executor gives this job the environment of a work session, with no secret.
//!
//! - The lint: an offense counts only on a changed line, read as `path:line[:column]: message`.
//!   A lint that exits with an error and names no offense in that form is broken: the job fails.
//! - The specs: the changed spec files, and with `specs_map = "rails"` the spec file of each
//!   changed file of `app/` and `lib/`. A spec that fails at the head and whose file the branch
//!   did not change runs again in a worktree of the base commit. A spec that fails there too is
//!   not ours: it passes, and the document names it.
//! - The migrations: each added file under a folder `db/<...migrate...>/` whose name starts with
//!   its version runs the commands of `migrations`. Afterwards, each tracked file that they
//!   changed goes back to the state of the branch.
//! - The cap: `cap + cap_per_file x the checked files`, at most `cap_max`, for the lint, the
//!   specs, and jest together. A lint that the cap stops fails as a hang; specs or jest that the
//!   cap stops pass with a note, because CI is then their only run. The red specs on the base and
//!   the migrations always run to their end, each command bounded by `cap` alone.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::process::Command;
use std::time::{Duration, Instant};

use serde_json::{json, Map, Value};

use super::core::{clean, fail, short, splitlines, Config, Error, Res};
use super::git::Git;
use super::jobs::{
    canaries_of, last_lines, need_commits, real_path, strip_ansi, tail_chars, with_worktree, Glob,
};
use super::proc::{self, How};
use crate::adapter::JobEnv;
use crate::adapters::JobSettings;
use crate::util::regex::Regex;

/// The keys of the table of the job in adapter.toml.
pub const KEYS: [&str; 9] = [
    "lint",
    "specs",
    "specs_map",
    "jest",
    "migrations",
    "migrations_env",
    "cap",
    "cap_per_file",
    "cap_max",
];

/// What the loader of adapter.toml knows of the job.
pub static SETTINGS: JobSettings = JobSettings {
    keys: &KEYS,
    check: check_settings,
    env: JobEnv::Session,
    no_secrets: true,
    timeout: timeout_of,
};

/// The document of the job.
pub const DOC: &str = "precheck.md";
/// The characters of the output of one command that the document keeps.
const OUTPUT_KEEP: usize = 6000;
/// The offenses that the document lists.
const OFFENSES_SHOWN: usize = 50;

fn check_settings(table: &Map<String, Value>) -> Vec<String> {
    match Precheck::parse(table) {
        Ok(_) => Vec::new(),
        Err(problems) => problems,
    }
}

fn timeout_of(table: &Map<String, Value>) -> (u64, u64) {
    let precheck = Precheck::parse(table).unwrap_or_default();
    (precheck.default_timeout(), precheck.cap_max)
}

/// A command with the globs of its files. No glob: each changed file.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Run {
    pub run: String,
    pub globs: Vec<String>,
}

/// How the changed files map to spec files.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SpecsMap {
    /// Only the changed spec files.
    #[default]
    Changed,
    /// A changed spec file runs itself; `app/<dir>/<path>.rb` maps to
    /// `spec/<dir>/<path>_spec.rb`, and `lib/<path>.rb` to `spec/lib/<path>_spec.rb`, when that
    /// file exists.
    Rails,
}

/// The table of the job.
#[derive(Clone, Debug, PartialEq)]
pub struct Precheck {
    pub lint: Vec<Run>,
    /// The command of the specs, with `{files}` for the spec files to run.
    pub specs: Option<String>,
    pub specs_map: SpecsMap,
    pub jest: Option<Run>,
    /// The commands of each added migration, in order, with `{version}`.
    pub migrations: Vec<String>,
    pub migrations_env: Vec<(String, String)>,
    /// Seconds.
    pub cap: u64,
    pub cap_per_file: u64,
    pub cap_max: u64,
}

impl Default for Precheck {
    fn default() -> Precheck {
        Precheck {
            lint: Vec::new(),
            specs: None,
            specs_map: SpecsMap::Changed,
            jest: None,
            migrations: Vec::new(),
            migrations_env: Vec::new(),
            cap: 600,
            cap_per_file: 15,
            cap_max: 1800,
        }
    }
}

/// Reads a time: a number of seconds, or a text such as `90`, `15s`, `10m`, `1h`, or `1h30m`.
pub fn duration(value: &Value) -> Option<u64> {
    match value {
        Value::Number(n) => n.as_u64(),
        Value::String(text) => {
            let text = text.trim();
            if text.is_empty() {
                return None;
            }
            if text.bytes().all(|b| b.is_ascii_digit()) {
                return text.parse().ok();
            }
            let mut total: u64 = 0;
            let mut digits = String::new();
            for c in text.chars() {
                if c.is_ascii_digit() {
                    digits.push(c);
                    continue;
                }
                let unit = match c {
                    's' => 1,
                    'm' => 60,
                    'h' => 3600,
                    _ => return None,
                };
                let n: u64 = digits.parse().ok()?;
                total = total.checked_add(n.checked_mul(unit)?)?;
                digits.clear();
            }
            digits.is_empty().then_some(total)
        }
        _ => None,
    }
}

/// The time as people read it, such as `10m`, `15s`, or `1h30m`.
pub fn shown(seconds: u64) -> String {
    let (h, m, s) = (seconds / 3600, seconds % 3600 / 60, seconds % 60);
    let mut out = String::new();
    if h > 0 {
        out.push_str(&format!("{h}h"));
    }
    if m > 0 {
        out.push_str(&format!("{m}m"));
    }
    if s > 0 || out.is_empty() {
        out.push_str(&format!("{s}s"));
    }
    out
}

fn command_problem(key: &str, command: &str) -> Option<String> {
    if command.trim().is_empty() || command.contains('\0') {
        return Some(format!("{key} holds an empty command"));
    }
    crate::executor::gates::denied_command(command)
        .map(|why| format!("{key} runs `{command}`, which the executor refuses: {why}"))
}

impl Precheck {
    /// Reads the table of the job. Gives each problem.
    pub fn parse(table: &Map<String, Value>) -> Result<Precheck, Vec<String>> {
        let mut out = Precheck::default();
        let mut problems = Vec::new();
        for key in table.keys() {
            if !KEYS.contains(&key.as_str()) {
                problems.push(if key == "lint_globs" {
                    "lint_globs is not a key: give each lint its globs, such as lint = [{ run = \"bundle exec rubocop --format emacs {files}\", globs = [\"**/*.rb\"] }]".to_string()
                } else {
                    format!("`{key}` is not a key of the precheck. Its keys: {}", KEYS.join(", "))
                });
            }
        }
        let run_of = |value: &Value, key: &str, problems: &mut Vec<String>| -> Option<Run> {
            let run = match value {
                Value::String(text) => Run {
                    run: text.trim().to_string(),
                    globs: Vec::new(),
                },
                Value::Object(map) => {
                    for k in map.keys().filter(|k| *k != "run" && *k != "globs") {
                        problems.push(format!(
                            "{key}: `{k}` is not a key of a command. Its keys: run, globs"
                        ));
                    }
                    let globs = match map.get("globs") {
                        None => Vec::new(),
                        Some(Value::Array(items)) if items.iter().all(Value::is_string) => items
                            .iter()
                            .filter_map(Value::as_str)
                            .map(|g| g.trim().to_string())
                            .filter(|g| !g.is_empty())
                            .collect(),
                        Some(_) => {
                            problems.push(format!(
                                "{key}: globs is a list of globs, such as [\"**/*.rb\"]"
                            ));
                            Vec::new()
                        }
                    };
                    match map.get("run").and_then(Value::as_str) {
                        Some(run) => Run {
                            run: run.trim().to_string(),
                            globs,
                        },
                        None => {
                            problems.push(format!("{key}: a command table names its command in run, such as {{ run = \"npx eslint {{files}}\", globs = [\"**/*.js\"] }}"));
                            return None;
                        }
                    }
                }
                _ => {
                    problems.push(format!(
                        "{key} is a command, or a table such as {{ run = \"...\", globs = [...] }}"
                    ));
                    return None;
                }
            };
            if let Some(problem) = command_problem(key, &run.run) {
                problems.push(problem);
            }
            Some(run)
        };
        match table.get("lint") {
            None => {}
            Some(Value::Array(items)) => {
                for item in items {
                    if let Some(run) = run_of(item, "lint", &mut problems) {
                        out.lint.push(run);
                    }
                }
            }
            Some(_) => problems.push(
                "lint is a list of commands, such as [\"npx eslint --format unix {files}\"]".into(),
            ),
        }
        match table.get("specs") {
            None => {}
            Some(Value::String(text)) => {
                if let Some(problem) = command_problem("specs", text) {
                    problems.push(problem);
                }
                out.specs = Some(text.trim().to_string());
            }
            Some(_) => problems.push("specs is one command, such as \"bin/rspec {files}\"".into()),
        }
        match table.get("specs_map") {
            None => {}
            Some(Value::String(text)) if text.is_empty() => out.specs_map = SpecsMap::Changed,
            Some(Value::String(text)) if text == "rails" => out.specs_map = SpecsMap::Rails,
            Some(other) => problems.push(format!(
                "specs_map is \"rails\" or \"\" (only the changed spec files), not {other}"
            )),
        }
        if let Some(value) = table.get("jest") {
            out.jest = run_of(value, "jest", &mut problems);
        }
        match table.get("migrations") {
            None => {}
            Some(Value::Array(items)) if items.iter().all(Value::is_string) => {
                for item in items.iter().filter_map(Value::as_str) {
                    if let Some(problem) = command_problem("migrations", item) {
                        problems.push(problem);
                    }
                    out.migrations.push(item.trim().to_string());
                }
            }
            Some(_) => problems.push("migrations is a list of commands, such as [\"bin/rails db:migrate:up VERSION={version}\"]".into()),
        }
        match table.get("migrations_env") {
            None => {}
            Some(Value::Object(map)) => {
                for (name, value) in map {
                    let valid = !name.is_empty()
                        && name.chars().enumerate().all(|(at, c)| {
                            c == '_' || c.is_ascii_alphabetic() || (at > 0 && c.is_ascii_digit())
                        });
                    match value.as_str() {
                        Some(text) if valid && !text.contains(['\0', '\n', '\r']) => {
                            out.migrations_env.push((name.clone(), text.to_string()));
                        }
                        _ => problems.push(format!(
                            "migrations_env.{name} must be a variable name with a value of one line, such as RAILS_ENV = \"test\""
                        )),
                    }
                }
            }
            Some(_) => {
                problems.push("migrations_env is a table, such as { RAILS_ENV = \"test\" }".into())
            }
        }
        for (key, slot) in [
            ("cap", &mut out.cap),
            ("cap_per_file", &mut out.cap_per_file),
            ("cap_max", &mut out.cap_max),
        ] {
            if let Some(value) = table.get(key) {
                match duration(value) {
                    Some(seconds) => *slot = seconds,
                    None => problems.push(format!(
                        "{key} is a time such as \"10m\", \"15s\", or a number of seconds, not {value}"
                    )),
                }
            }
        }
        if out.cap == 0 {
            problems.push("cap must be above 0 seconds".into());
        }
        if out.cap_max < out.cap {
            problems.push(format!(
                "cap_max ({}) is below cap ({})",
                shown(out.cap_max),
                shown(out.cap)
            ));
        }
        if problems.is_empty() {
            Ok(out)
        } else {
            Err(problems)
        }
    }

    /// The time limit of the job when its table has none: the cap of the lint, the specs, and
    /// jest, and room for the worktree of the base with its canary, the red specs on the base,
    /// and the three commands of one migration, each one bounded by `cap`.
    pub fn default_timeout(&self) -> u64 {
        self.cap_max.saturating_add(self.cap.saturating_mul(6))
    }

    /// The cap of the lint, the specs, and jest for this many checked files.
    pub fn cap_for(&self, files: usize) -> u64 {
        self.cap
            .saturating_add(self.cap_per_file.saturating_mul(files as u64))
            .min(self.cap_max)
    }
}

/// The changed lines of each file of a diff with no context (`git diff -U0`): the lines of the
/// new side of each hunk.
pub fn changed_lines(diff: &str) -> BTreeMap<String, BTreeSet<u32>> {
    let mut out: BTreeMap<String, BTreeSet<u32>> = BTreeMap::new();
    let mut file: Option<String> = None;
    for line in diff.lines() {
        if let Some(rest) = line.strip_prefix("+++ ") {
            let rest = rest.trim_end();
            let rest = unquote(rest);
            file = rest.strip_prefix("b/").map(str::to_string);
            continue;
        }
        let (Some(name), Some(rest)) = (&file, line.strip_prefix("@@ ")) else {
            continue;
        };
        let Some(new) = rest.split_whitespace().find(|p| p.starts_with('+')) else {
            continue;
        };
        let (start, count) = match new[1..].split_once(',') {
            Some((a, b)) => (a.parse::<u32>().ok(), b.parse::<u32>().ok()),
            None => (new[1..].parse::<u32>().ok(), Some(1)),
        };
        if let (Some(start), Some(count)) = (start, count) {
            let set = out.entry(name.clone()).or_default();
            for n in start..start.saturating_add(count) {
                set.insert(n);
            }
        }
    }
    out
}

/// A path that git quoted, such as `"b/a\tb.rb"`, as plain text.
fn unquote(text: &str) -> String {
    let Some(inner) = text.strip_prefix('"').and_then(|t| t.strip_suffix('"')) else {
        return text.to_string();
    };
    let mut out = String::new();
    let mut chars = inner.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('t') => out.push('\t'),
            Some('n') => out.push('\n'),
            Some(other) => out.push(other),
            None => {}
        }
    }
    out
}

/// One offense of a lint.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Offense {
    pub path: String,
    pub line: u32,
    pub message: String,
}

/// The offenses of the output of a lint, in the form `path:line[:column]: message` (rubocop
/// with `--format emacs`, eslint with `--format unix`). A path of the clone loses the clone in
/// front of it.
pub fn offenses(output: &str, clones: &[String]) -> Vec<Offense> {
    let Ok(pattern) = Regex::new(r"^(\S[^:]*):(\d+)(?::\d+)?: (.+)$") else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for line in output.lines() {
        let line = strip_ansi(line);
        let Some(caps) = pattern.captures(line.trim_end()) else {
            continue;
        };
        let (Some(path), Some(number), Some(message)) = (
            caps.get(1).cloned().flatten(),
            caps.get(2).cloned().flatten(),
            caps.get(3).cloned().flatten(),
        ) else {
            continue;
        };
        let Ok(number) = number.parse::<u32>() else {
            continue;
        };
        out.push(Offense {
            path: relative(&path, clones),
            line: number,
            message: message.trim().to_string(),
        });
    }
    out
}

/// A path relative to the clone: no clone and no `./` in front.
fn relative(path: &str, clones: &[String]) -> String {
    let mut path = path.trim();
    for clone in clones {
        let clone = clone.trim_end_matches('/');
        if let Some(rest) = path.strip_prefix(clone).and_then(|r| r.strip_prefix('/')) {
            path = rest;
            break;
        }
    }
    path.strip_prefix("./").unwrap_or(path).to_string()
}

/// The examples that failed in the output of rspec: the lines `rspec ./spec/x_spec.rb:12 #
/// ...`, as `spec/x_spec.rb:12`, each one once, in their order.
pub fn red_specs(output: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for line in output.lines() {
        let line = strip_ansi(line);
        let Some(rest) = line.trim_start().strip_prefix("rspec ") else {
            continue;
        };
        let id = rest.split_whitespace().next().unwrap_or("");
        let id = id.strip_prefix("./").unwrap_or(id);
        if spec_file(id).ends_with("_spec.rb") && !out.iter().any(|o| o == id) {
            out.push(id.to_string());
        }
    }
    out
}

/// The file of an example id, such as `spec/x_spec.rb` of `spec/x_spec.rb:12` or of
/// `spec/x_spec.rb[1:2]`.
pub fn spec_file(id: &str) -> &str {
    let end = id.find([':', '[']).unwrap_or(id.len());
    &id[..end]
}

/// True for a spec file of rspec.
pub fn is_spec(path: &str) -> bool {
    path.ends_with("_spec.rb")
}

/// The spec file of a changed file, or `None`. `exists` tells whether a file of the clone
/// exists.
pub fn spec_of(path: &str, map: SpecsMap, exists: &dyn Fn(&str) -> bool) -> Option<String> {
    if is_spec(path) {
        return Some(path.to_string());
    }
    if map != SpecsMap::Rails {
        return None;
    }
    let stem = path.strip_suffix(".rb")?;
    let candidate = match stem.strip_prefix("app/") {
        Some(rest) => format!("spec/{rest}_spec.rb"),
        None => format!("spec/lib/{}_spec.rb", stem.strip_prefix("lib/")?),
    };
    exists(&candidate).then_some(candidate)
}

/// The version of an added migration: a file under a folder `db/<...migrate...>/` whose name
/// starts with digits, outside `spec/` and `test/`. `None` for each other file.
pub fn migration_version(path: &str) -> Option<String> {
    let parts: Vec<&str> = path.split('/').collect();
    if parts.iter().any(|p| *p == "spec" || *p == "test") {
        return None;
    }
    let (name, folders) = parts.split_last()?;
    let under = folders
        .windows(2)
        .any(|w| w[0] == "db" && w[1].contains("migrate"));
    let digits: String = name.chars().take_while(char::is_ascii_digit).collect();
    (under && !digits.is_empty()).then_some(digits)
}

/// A text for `sh`, in single quotes. A file name that starts with `-` gets `./` in front, so
/// that a command never reads it as an option.
pub fn quote(text: &str) -> String {
    let text = if text.starts_with('-') {
        format!("./{text}")
    } else {
        text.to_string()
    };
    format!("'{}'", text.replace('\'', "'\\''"))
}

/// A command with `{files}`, `{base}`, and `{version}` in place.
pub fn fill(command: &str, files: &[String], base: &str, version: &str) -> String {
    let files: Vec<String> = files.iter().map(|f| quote(f)).collect();
    command
        .replace("{files}", &files.join(" "))
        .replace("{base}", base)
        .replace("{version}", version)
}

/// What one command gave.
struct Ran {
    code: Option<i32>,
    stdout: String,
    stderr: String,
    seconds: u64,
    timed_out: bool,
}

impl Ran {
    fn passed(&self) -> bool {
        self.code == Some(0) && !self.timed_out
    }

    fn word(&self) -> String {
        if self.timed_out {
            format!("stopped by its time limit after {}", shown(self.seconds))
        } else if self.passed() {
            format!("pass in {}", shown(self.seconds))
        } else {
            match self.code {
                Some(code) if code < 0 => format!("fail by the signal {}", -code),
                Some(code) => format!("fail with the exit code {code}"),
                None => "fail with no exit code".to_string(),
            }
        }
    }

    fn output(&self) -> String {
        let mut text = self.stdout.clone();
        if !self.stderr.trim().is_empty() {
            if !text.is_empty() && !text.ends_with('\n') {
                text.push('\n');
            }
            text.push_str(&self.stderr);
        }
        text
    }
}

/// Runs the shell command `text` in `cwd` with the environment of this call and `env` more, in
/// its own process group, with a time limit. A command whose `template` the executor refuses
/// never runs. The test reads the template, as the table names it: a file name of `{files}`,
/// such as `bin/ssh`, is no program.
fn sh(
    template: &str,
    text: &str,
    cwd: &Path,
    timeout: Duration,
    env: &[(String, String)],
) -> Res<Ran> {
    if let Some(why) = crate::executor::gates::denied_command(template) {
        return fail(format!(
            "the precheck refuses to run `{}`: {why}",
            clean(template, 200)
        ));
    }
    let mut command = Command::new("sh");
    command.arg("-c").arg(text).current_dir(cwd);
    for (name, value) in env {
        command.env(name, value);
    }
    let output = proc::run(
        command,
        None,
        Some(timeout.max(Duration::from_secs(1))),
        How {
            session: true,
            merge: false,
        },
    )
    .map_err(|e| Error::Unexpected(format!("OSError: {e}")))?;
    Ok(Ran {
        code: output.code,
        stdout: output.stdout_text(),
        stderr: output.stderr_text(),
        seconds: output.seconds,
        timed_out: output.timed_out,
    })
}

/// The document and the checks of one run of the job.
#[derive(Default)]
struct Report {
    ran: Vec<String>,
    offenses: Vec<String>,
    specs: Vec<String>,
    migrations: Vec<String>,
    unfinished: Vec<String>,
    problems: Vec<String>,
    notes: Vec<String>,
    outputs: Vec<String>,
    checks: Vec<Value>,
}

impl Report {
    fn command(
        &mut self,
        kind: &str,
        label: &str,
        command: &str,
        ran: &Ran,
        holds: bool,
        cause: &str,
    ) {
        self.ran.push(format!(
            "- {label}: `{}`: {}",
            clean(command, 300),
            ran.word()
        ));
        self.checks.push(json!({
            "kind": kind,
            "result": if holds { "pass" } else { "fail" },
            "command": clean(command, 500),
            "seconds": ran.seconds,
            "cause": if holds { "" } else { cause },
        }));
        if !holds || ran.timed_out {
            self.outputs.push(format!(
                "### {label}\n\n```\n{}\n```",
                tail_chars(ran.output().trim_end(), OUTPUT_KEEP)
            ));
        }
    }
}

/// The paths of the clone as a lint can print them: as given, and with its links followed.
fn clone_names(clone: &Path) -> Vec<String> {
    let mut names = vec![clone.display().to_string()];
    let real = real_path(clone).display().to_string();
    if !names.contains(&real) {
        names.push(real);
    }
    names
}

fn git_list(clone: &Path, args: &[&str]) -> Res<Vec<String>> {
    let raw = Git::new(args, Some(clone)).run()?.stdout_text();
    Ok(raw
        .split('\0')
        .filter(|p| !p.is_empty())
        .map(str::to_string)
        .collect())
}

/// The lines of `git status --porcelain` of the clone, untracked files too.
fn status_lines(clone: &Path) -> Res<BTreeSet<String>> {
    let text = Git::new(
        &["status", "--porcelain", "--untracked-files=all"],
        Some(clone),
    )
    .run()?
    .stdout_text();
    Ok(splitlines(&text)
        .into_iter()
        .filter(|l| !l.trim().is_empty())
        .map(str::to_string)
        .collect())
}

/// Runs the precheck at the head of the ticket (see the module text).
pub fn precheck(config: &Config, call: &Map<String, Value>) -> Res<Value> {
    let what = "the precheck";
    // The executor adds the time limit of the job to its table.
    let mut table = call
        .get("table")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    table.remove("timeout");
    let pre = match Precheck::parse(&table) {
        Ok(pre) => pre,
        Err(problems) => {
            return fail(format!(
                "the table of the precheck is not valid: {}",
                problems.join("; ")
            ))
        }
    };
    let (clone, base, head) = need_commits(call, what)?;
    let at = Git::new(&["rev-parse", "HEAD"], Some(&clone)).text()?;
    if at != head {
        return fail(format!(
            "the clone is at {}, not at the head {}",
            short(&at),
            short(&head)
        ));
    }
    let changed = git_list(
        &clone,
        &[
            "diff",
            "--name-only",
            "--no-renames",
            "--diff-filter=d",
            "-z",
            &base,
            &head,
        ],
    )?;
    let added = git_list(
        &clone,
        &[
            "diff",
            "--name-only",
            "--no-renames",
            "--diff-filter=A",
            "-z",
            &base,
            &head,
        ],
    )?;
    let diff = Git::new(
        &[
            "diff",
            "-U0",
            "--no-renames",
            "--no-color",
            "--no-ext-diff",
            "--src-prefix=a/",
            "--dst-prefix=b/",
            &base,
            &head,
        ],
        Some(&clone),
    )
    .run()?
    .stdout_text();
    let lines = changed_lines(&diff);
    let clones = clone_names(&clone);
    let filtered = |globs: &[String]| -> Vec<String> {
        if globs.is_empty() {
            return changed.clone();
        }
        let globs: Vec<Glob> = globs.iter().map(|g| Glob::new(g)).collect();
        changed
            .iter()
            .filter(|p| globs.iter().any(|g| g.matches(p)))
            .cloned()
            .collect()
    };
    let lint_files: Vec<Vec<String>> = pre.lint.iter().map(|l| filtered(&l.globs)).collect();
    let jest_files = pre
        .jest
        .as_ref()
        .map(|j| filtered(&j.globs))
        .unwrap_or_default();
    let exists = |p: &str| clone.join(p).is_file();
    let mut spec_files: Vec<String> = Vec::new();
    if pre.specs.is_some() {
        for path in &changed {
            if let Some(spec) = spec_of(path, pre.specs_map, &exists) {
                if exists(&spec) && !spec_files.contains(&spec) {
                    spec_files.push(spec);
                }
            }
        }
    }
    let checked: BTreeSet<&String> = lint_files
        .iter()
        .flatten()
        .chain(&jest_files)
        .chain(&spec_files)
        .collect();
    let total = pre.cap_for(checked.len());
    let mut report = Report::default();
    report.notes.push(format!(
        "The cap of the lint, the specs, and jest: {} = cap {} + cap_per_file {} x {} checked file(s), at most cap_max {}. The red specs on the base and the migrations are bounded by cap alone.",
        shown(total),
        shown(pre.cap),
        shown(pre.cap_per_file),
        checked.len(),
        shown(pre.cap_max)
    ));
    let started = Instant::now();
    let deadline = started + Duration::from_secs(total);
    let left = || deadline.saturating_duration_since(Instant::now());
    let cap = Duration::from_secs(pre.cap);
    let mut hung = false;

    // The lint.
    for (lint, files) in pre.lint.iter().zip(&lint_files) {
        if files.is_empty() {
            report.ran.push(format!(
                "- lint: `{}`: not run, because no changed file matches its globs",
                clean(&lint.run, 300)
            ));
            continue;
        }
        if hung || left().is_zero() {
            report.unfinished.push(format!(
                "the lint `{}` did not run: the cap was spent",
                clean(&lint.run, 200)
            ));
            hung = true;
            continue;
        }
        let command = fill(&lint.run, files, &base, "");
        let ran = sh(&lint.run, &command, &clone, left(), &[])?;
        if ran.timed_out {
            hung = true;
            report.command("precheck-lint", "lint", &command, &ran, false, "infra");
            report.problems.push(format!(
                "the lint `{}` did not end within the cap of {}, so it counts as a hang",
                clean(&lint.run, 200),
                shown(total)
            ));
            continue;
        }
        let found = offenses(&ran.output(), &clones);
        let on_changed: Vec<&Offense> = found
            .iter()
            .filter(|o| lines.get(&o.path).is_some_and(|set| set.contains(&o.line)))
            .collect();
        let broken = !ran.passed() && found.is_empty();
        let holds = ran.passed() || (!broken && on_changed.is_empty());
        report.command(
            "precheck-lint",
            "lint",
            &command,
            &ran,
            holds,
            if broken { "env" } else { "code" },
        );
        if broken {
            let tail = last_lines(&ran.stderr, 10);
            report.problems.push(format!(
                "the lint `{}` failed and named no offense in the form path:line: message, so it is broken{}",
                clean(&lint.run, 200),
                if tail.is_empty() { String::new() } else { format!(". The end of its standard error:\n{tail}") }
            ));
        } else if !ran.passed() {
            if on_changed.is_empty() {
                report.notes.push(format!(
                    "the lint `{}` found {} offense(s), none on a changed line",
                    clean(&lint.run, 200),
                    found.len()
                ));
            } else {
                report.problems.push(format!(
                    "the lint `{}` found {} offense(s) on changed lines",
                    clean(&lint.run, 200),
                    on_changed.len()
                ));
                for o in on_changed.iter().take(OFFENSES_SHOWN) {
                    report.offenses.push(format!(
                        "- {}:{}: {}",
                        o.path,
                        o.line,
                        clean(&o.message, 300)
                    ));
                }
                if on_changed.len() > OFFENSES_SHOWN {
                    report
                        .offenses
                        .push(format!("- and {} more", on_changed.len() - OFFENSES_SHOWN));
                }
            }
        }
    }

    // The specs.
    let mut red: Vec<String> = Vec::new();
    if let Some(specs) = &pre.specs {
        if spec_files.is_empty() {
            report
                .ran
                .push("- specs: not run, because no changed file has a spec file".into());
        } else if hung || left().is_zero() {
            report
                .unfinished
                .push("the specs did not run: the cap was spent. CI is their only run".into());
        } else {
            let command = fill(specs, &spec_files, &base, "");
            let ran = sh(specs, &command, &clone, left(), &[])?;
            if ran.timed_out {
                report.command("precheck-specs", "specs", &command, &ran, true, "infra");
                report.unfinished.push(format!(
                    "unfinished: the specs did not end within the cap of {}. CI is their only run",
                    shown(total)
                ));
            } else if ran.passed() {
                report.command("precheck-specs", "specs", &command, &ran, true, "");
            } else {
                red = red_specs(&ran.output());
                report.command("precheck-specs", "specs", &command, &ran, false, "code");
                if red.is_empty() {
                    let tail = last_lines(&ran.stderr, 10);
                    report.problems.push(format!(
                        "the specs failed and named no failing example (no line `rspec ./spec/...`){}",
                        if tail.is_empty() { String::new() } else { format!(". The end of their standard error:\n{tail}") }
                    ));
                }
            }
        }
    }

    // jest.
    if let Some(jest) = &pre.jest {
        if jest_files.is_empty() {
            report.ran.push(format!(
                "- jest: `{}`: not run, because no changed file matches its globs",
                clean(&jest.run, 300)
            ));
        } else if hung || left().is_zero() {
            report
                .unfinished
                .push("jest did not run: the cap was spent. CI is its only run".into());
        } else {
            let command = fill(&jest.run, &jest_files, &base, "");
            let ran = sh(&jest.run, &command, &clone, left(), &[])?;
            if ran.timed_out {
                report.command("precheck-jest", "jest", &command, &ran, true, "infra");
                report.unfinished.push(format!(
                    "unfinished: jest did not end within the cap of {}. CI is its only run",
                    shown(total)
                ));
            } else {
                report.command(
                    "precheck-jest",
                    "jest",
                    &command,
                    &ran,
                    ran.passed(),
                    "code",
                );
                if !ran.passed() {
                    report
                        .problems
                        .push("jest failed on the changed files".into());
                }
            }
        }
    }

    // The red specs: each one in a file that the branch did not change runs again on the base.
    if !red.is_empty() {
        let changed_set: BTreeSet<&str> = changed.iter().map(String::as_str).collect();
        let (ours_first, again): (Vec<String>, Vec<String>) = red
            .iter()
            .cloned()
            .partition(|id| changed_set.contains(spec_file(id)));
        let mut ours = ours_first;
        let mut not_ours: Vec<String> = Vec::new();
        if !again.is_empty() {
            let specs = pre.specs.clone().unwrap_or_default();
            let canaries: Vec<String> = canaries_of(config.get("test_canary"))?
                .into_iter()
                .filter(|c| is_spec(c) && !changed_set.contains(c.as_str()))
                .collect();
            let setup = config.text("setup_command")?;
            let clone_text = clone.display().to_string();
            let base_red = with_worktree(&clone, &base, |tree| -> Res<Option<Vec<String>>> {
                if let Some(setup) = &setup {
                    let ran = sh(
                        setup,
                        setup,
                        tree,
                        cap,
                        &[("VISION_CLONE".to_string(), clone_text.clone())],
                    )?;
                    report.command(
                        "precheck-base",
                        "setup_command in the worktree of the base",
                        setup,
                        &ran,
                        ran.passed(),
                        "env",
                    );
                    if !ran.passed() {
                        report.notes.push("setup_command failed in the worktree of the base, so no red spec ran there".into());
                        return Ok(None);
                    }
                }
                for canary in &canaries {
                    if !tree.join(canary).is_file() {
                        report.notes.push(format!("the canary {canary} is missing in the worktree of the base, so it cannot run specs"));
                        return Ok(None);
                    }
                    let command = fill(&specs, std::slice::from_ref(canary), &base, "");
                    let ran = sh(&specs, &command, tree, cap, &[])?;
                    report.command(
                        "precheck-base",
                        &format!("the canary {canary} on the base"),
                        &command,
                        &ran,
                        ran.passed(),
                        "env",
                    );
                    if !ran.passed() {
                        report.notes.push(format!("the canary {canary} fails in the worktree of the base, so it cannot run specs"));
                        return Ok(None);
                    }
                }
                if canaries.is_empty() {
                    report.notes.push(
                        "no canary guards the run on the base (set test_canary to a spec file)"
                            .into(),
                    );
                }
                let command = fill(&specs, &again, &base, "");
                let ran = sh(&specs, &command, tree, cap, &[])?;
                report.command(
                    "precheck-base",
                    "the red specs on the base",
                    &command,
                    &ran,
                    true,
                    "",
                );
                if ran.timed_out {
                    report.notes.push(format!(
                        "the red specs on the base did not end within cap ({})",
                        shown(pre.cap)
                    ));
                    return Ok(None);
                }
                Ok(Some(if ran.passed() {
                    Vec::new()
                } else {
                    red_specs(&ran.output())
                }))
            })?;
            match base_red {
                Some(on_base) => {
                    for id in again {
                        if on_base.contains(&id) {
                            not_ours.push(id);
                        } else {
                            ours.push(id);
                        }
                    }
                }
                None => ours.extend(again),
            }
        }
        for id in &ours {
            report
                .specs
                .push(format!("- {id}: red at the head {}", short(&head)));
        }
        for id in &not_ours {
            report.specs.push(format!(
                "- {id}: red on the base {} too, so it is not ours: it passes",
                short(&base)
            ));
        }
        if !ours.is_empty() {
            report.problems.push(format!(
                "{} spec(s) fail at the head: {}",
                ours.len(),
                ours.join(", ")
            ));
        }
    }

    // The migrations.
    let mut migrations: Vec<(String, String)> = added
        .iter()
        .filter_map(|p| migration_version(p).map(|v| (v, p.clone())))
        .collect();
    migrations.sort();
    if !migrations.is_empty() && pre.migrations.is_empty() {
        report.notes.push(format!(
            "the branch adds {} migration(s), but the table has no migrations commands, so none ran",
            migrations.len()
        ));
    } else if !migrations.is_empty() {
        let before = status_lines(&clone)?;
        'migrations: for (version, path) in &migrations {
            for template in &pre.migrations {
                let command = fill(template, &[], &base, version);
                let ran = sh(template, &command, &clone, cap, &pre.migrations_env)?;
                report.command(
                    "precheck-migration",
                    &format!("the migration {path}"),
                    &command,
                    &ran,
                    ran.passed(),
                    "code",
                );
                if !ran.passed() {
                    report.migrations.push(format!(
                        "- {path} (version {version}): `{}`: {}",
                        clean(&command, 200),
                        ran.word()
                    ));
                    report.problems.push(format!(
                        "the migration {path} failed: `{}`",
                        clean(&command, 200)
                    ));
                    break 'migrations;
                }
            }
            report
                .migrations
                .push(format!("- {path} (version {version}): each command passed"));
        }
        let after = status_lines(&clone)?;
        let mut restored = Vec::new();
        let mut left_over = Vec::new();
        for line in after.difference(&before) {
            let (state, file) = (
                line.get(..2).unwrap_or(""),
                line.get(3..).unwrap_or("").trim(),
            );
            if file.is_empty() {
                continue;
            }
            if state == "??" {
                left_over.push(file.to_string());
            } else {
                restored.push(unquote(file));
            }
        }
        if !restored.is_empty() {
            let mut args: Vec<&str> = vec!["checkout", "-q", "HEAD", "--"];
            args.extend(restored.iter().map(String::as_str));
            Git::new(&args, Some(&clone)).run()?;
            report.migrations.push(format!(
                "The commands changed {}, which went back to the state of the branch.",
                restored.join(", ")
            ));
        }
        if !left_over.is_empty() {
            report.migrations.push(format!(
                "The commands left new files that git does not track: {}. They stay.",
                left_over.join(", ")
            ));
        }
    }

    let ok = report.problems.is_empty();
    let document = render(&report, &base, &head, ok);
    let detail = if ok {
        let mut text = format!("the precheck passed at {}", short(&head));
        if !report.unfinished.is_empty() {
            text.push_str(&format!(" with a note: {}", report.unfinished.join("; ")));
        }
        format!("{text}. See {DOC}")
    } else {
        format!(
            "the precheck failed at {}: {}. {DOC} names each one",
            short(&head),
            report
                .problems
                .iter()
                .map(|p| first_line(p))
                .collect::<Vec<_>>()
                .join("; ")
        )
    };
    Ok(json!({
        "ok": ok,
        "detail": detail,
        "documents": [{"name": DOC, "content": document}],
        "checks": report.checks,
    }))
}

fn first_line(text: &str) -> &str {
    text.lines().next().unwrap_or("")
}

/// The text of precheck.md.
fn render(report: &Report, base: &str, head: &str, ok: bool) -> String {
    let mut out = format!(
        "# Precheck at {}\n\nThe base is {}. Result: {}.\n",
        short(head),
        short(base),
        if ok { "pass" } else { "fail" }
    );
    let mut section = |title: &str, items: &[String]| {
        if !items.is_empty() {
            out.push_str(&format!("\n## {title}\n\n{}\n", items.join("\n")));
        }
    };
    let problems: Vec<String> = report.problems.iter().map(|p| format!("- {p}")).collect();
    section("Problems", &problems);
    section("What ran", &report.ran);
    section("Lint offenses on changed lines", &report.offenses);
    section("Red specs", &report.specs);
    section("Migrations", &report.migrations);
    let unfinished: Vec<String> = report.unfinished.iter().map(|p| format!("- {p}")).collect();
    section("Not finished", &unfinished);
    let notes: Vec<String> = report.notes.iter().map(|p| format!("- {p}")).collect();
    section("Notes", &notes);
    section("Output", &report.outputs);
    if report.ran.is_empty() && report.problems.is_empty() {
        out.push_str("\nNothing to check: no command of the table matches a changed file.\n");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn table(text: &str) -> Map<String, Value> {
        let value: toml::Table = toml::from_str(text).unwrap();
        crate::adapter::table_to_json(&value)
    }

    #[test]
    fn a_time_reads_as_seconds_or_with_a_unit() {
        for (text, seconds) in [
            ("90", 90),
            ("15s", 15),
            ("10m", 600),
            ("1h", 3600),
            ("1h30m", 5400),
        ] {
            assert_eq!(duration(&json!(text)), Some(seconds), "{text}");
        }
        assert_eq!(duration(&json!(45)), Some(45));
        for bad in ["", "10x", "m", "1.5m", "-3"] {
            assert_eq!(duration(&json!(bad)), None, "{bad}");
        }
        assert_eq!(shown(5400), "1h30m");
        assert_eq!(shown(15), "15s");
        assert_eq!(shown(0), "0s");
    }

    #[test]
    fn the_table_reads_each_key_and_names_each_problem() {
        let pre = Precheck::parse(&table(
            r#"
lint = ["npx eslint --format unix {files}", { run = "bundle exec rubocop --format emacs {files}", globs = ["**/*.rb", "Gemfile"] }]
specs = "bin/rspec {files}"
specs_map = "rails"
jest = { run = "npx jest --findRelatedTests {files}", globs = ["**/*.{js,ts}"] }
migrations = ["bin/rails db:migrate:up VERSION={version}", "bin/rails db:migrate:down VERSION={version}"]
migrations_env = { RAILS_ENV = "test" }
cap = "5m"
cap_per_file = "10s"
cap_max = "20m"
"#,
        ))
        .unwrap();
        assert_eq!(pre.lint.len(), 2);
        assert_eq!(pre.lint[1].globs, ["**/*.rb", "Gemfile"]);
        assert_eq!(pre.specs_map, SpecsMap::Rails);
        assert_eq!(pre.jest.as_ref().unwrap().globs, ["**/*.{js,ts}"]);
        assert_eq!(
            pre.migrations_env,
            [("RAILS_ENV".to_string(), "test".to_string())]
        );
        assert_eq!((pre.cap, pre.cap_per_file, pre.cap_max), (300, 10, 1200));
        assert_eq!(pre.cap_for(3), 330);
        assert_eq!(pre.cap_for(1000), 1200, "at most cap_max");
        assert_eq!(pre.default_timeout(), 1200 + 6 * 300);
        assert_eq!(Precheck::parse(&Map::new()).unwrap(), Precheck::default());
        let err = Precheck::parse(&table(
            r#"
lint_globs = { a = ["x"] }
specs = 3
specs_map = "django"
cap = "soon"
cap_max = "1m"
migrations = ["kubectl exec -it web -- rails db:migrate"]
lint = [{ globs = ["*.rb"] }]
"#,
        ))
        .unwrap_err()
        .join("\n");
        for part in [
            "lint_globs is not a key: give each lint its globs",
            "specs is one command",
            "specs_map is \"rails\" or \"\"",
            "cap is a time such as",
            "migrations runs `kubectl exec -it web -- rails db:migrate`, which the executor refuses",
            "a command table names its command in run",
        ] {
            assert!(err.contains(part), "{part}:\n{err}");
        }
        let err = Precheck::parse(&table("cap = \"20m\"\ncap_max = \"10m\"")).unwrap_err();
        assert!(
            err[0].contains("cap_max (10m) is below cap (20m)"),
            "{err:?}"
        );
    }

    #[test]
    fn the_changed_lines_come_from_the_hunks_of_the_new_side() {
        let diff = "diff --git a/app/a.rb b/app/a.rb\n--- a/app/a.rb\n+++ b/app/a.rb\n@@ -3,0 +4,2 @@ def x\n+a\n+b\n@@ -10 +12 @@\n-c\n+d\n@@ -20,3 +23,0 @@\n-e\ndiff --git a/new.rb b/new.rb\n--- /dev/null\n+++ b/new.rb\n@@ -0,0 +1,3 @@\n+x\n+y\n+z\n--- a/gone.rb\n+++ /dev/null\n@@ -1,2 +0,0 @@\n";
        let lines = changed_lines(diff);
        assert_eq!(lines["app/a.rb"], BTreeSet::from([4, 5, 12]));
        assert_eq!(lines["new.rb"], BTreeSet::from([1, 2, 3]));
        assert!(!lines.contains_key("gone.rb"));
    }

    #[test]
    fn an_offense_reads_in_the_forms_of_rubocop_and_eslint() {
        let clones = vec!["/work/clone".to_string()];
        let output = "/work/clone/app/a.rb:12:3: C: Layout/Indent: Use two spaces.\n./lib/b.js:4:1: Missing semicolon. [Error/semi]\nspec/c_spec.rb:9: W: something\n/gems/x.rb:5:in `block'\n1 file inspected, 2 offenses detected\n";
        let found = offenses(output, &clones);
        let got: Vec<(&str, u32)> = found.iter().map(|o| (o.path.as_str(), o.line)).collect();
        assert_eq!(
            got,
            [("app/a.rb", 12), ("lib/b.js", 4), ("spec/c_spec.rb", 9)]
        );
        assert_eq!(found[0].message, "C: Layout/Indent: Use two spaces.");
    }

    #[test]
    fn red_specs_read_the_lines_of_rspec() {
        let output = "Failures:\n...\nFailed examples:\n\n\u{1b}[31mrspec ./spec/models/user_spec.rb:12\u{1b}[0m \u{1b}[36m# User saves\u{1b}[0m\nrspec ./spec/a_spec.rb[1:2] # A works\nrspec ./spec/models/user_spec.rb:12 # again\nrspec --seed 4\n";
        assert_eq!(
            red_specs(output),
            ["spec/models/user_spec.rb:12", "spec/a_spec.rb[1:2]"]
        );
        assert_eq!(spec_file("spec/a_spec.rb[1:2]"), "spec/a_spec.rb");
        assert_eq!(spec_file("spec/x_spec.rb:12"), "spec/x_spec.rb");
    }

    #[test]
    fn the_rails_map_gives_the_spec_file_when_it_exists() {
        let exists =
            |p: &str| ["spec/models/user_spec.rb", "spec/lib/tools/csv_spec.rb"].contains(&p);
        let rails = |p: &str| spec_of(p, SpecsMap::Rails, &exists);
        assert_eq!(
            rails("app/models/user.rb").as_deref(),
            Some("spec/models/user_spec.rb")
        );
        assert_eq!(
            rails("lib/tools/csv.rb").as_deref(),
            Some("spec/lib/tools/csv_spec.rb")
        );
        assert_eq!(rails("app/models/order.rb"), None, "no such spec file");
        assert_eq!(
            rails("spec/models/order_spec.rb").as_deref(),
            Some("spec/models/order_spec.rb")
        );
        assert_eq!(rails("config/routes.rb"), None);
        assert_eq!(rails("app/views/a.html.erb"), None);
        let changed = |p: &str| spec_of(p, SpecsMap::Changed, &exists);
        assert_eq!(changed("app/models/user.rb"), None);
        assert_eq!(changed("spec/x_spec.rb").as_deref(), Some("spec/x_spec.rb"));
    }

    #[test]
    fn a_migration_is_an_added_file_under_a_migrate_folder() {
        assert_eq!(
            migration_version("db/migrate/20261101120000_add_x.rb").as_deref(),
            Some("20261101120000")
        );
        assert_eq!(
            migration_version("db/data_migrate/2026_fill.rb").as_deref(),
            Some("2026")
        );
        assert_eq!(
            migration_version("engines/shop/db/migrate/7_a.rb").as_deref(),
            Some("7")
        );
        assert_eq!(migration_version("db/migrate/add_x.rb"), None);
        assert_eq!(migration_version("db/seeds/2026_a.rb"), None);
        assert_eq!(migration_version("spec/db/migrate/2026_a_spec.rb"), None);
        assert_eq!(migration_version("test/db/migrate/2026_a.rb"), None);
    }

    #[test]
    fn a_command_gets_its_files_quoted() {
        let files = vec!["app/a b.rb".to_string(), "-x.rb".into(), "it's.rb".into()];
        assert_eq!(
            fill("rubocop {files} --base {base}", &files, "abc", ""),
            "rubocop 'app/a b.rb' './-x.rb' 'it'\\''s.rb' --base abc"
        );
        assert_eq!(
            fill("up VERSION={version}", &[], "", "2026"),
            "up VERSION=2026"
        );
    }
}
