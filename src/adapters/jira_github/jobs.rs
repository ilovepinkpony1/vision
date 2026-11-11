//! `job`: one job, such as the proof (design section 11.1).
//!
//! A job gets no secret, unless its table in adapter.toml sets `secrets = true`. Only `ci-log`
//! and `comments` do, because they only read GitHub and run no code of the project. Each command
//! of the project (the tests, the linter, the second reviewer, the app) runs with a plain
//! environment, in its own session and process group, with a time limit. A file name from the
//! diff reaches it only as one item of an argument list, never inside the text of a shell
//! command.

use std::collections::BTreeSet;
use std::io::{Read, Seek, SeekFrom};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::{Component, Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::{json, Map, Value};

use super::core::{
    args_of, check_key, clean, fail, is_commit, now, plain_env, pretty, py_opt, py_repr_str, short,
    short_opt, splitlines, stamp, text_of, truthy, whole, Config, Error, Res,
};
use super::forge::{self, GitHub, Ignore};
use super::git::Git;
use super::http::{check_base, send, split, Call};
use super::observe::pr_number;
use super::proc::{self, code_of, How, TempDir};

const OUTPUT_KEEP: usize = 20000;
const RED_JOBS: [&str; 4] = ["failure", "timed_out", "cancelled", "startup_failure"];
const USER_AGENT: &str = "vision-jira-github-adapter";

type Job = fn(&Config, &Map<String, Value>) -> Res<Value>;

const JOBS: [(&str, Job); 5] = [
    ("app-start", app_start),
    ("ci-log", ci_log),
    ("comments", comments),
    ("proof", proof),
    ("second-review", second_review),
];

pub fn job(call: &Map<String, Value>) -> Res<Value> {
    let name = call.get("job");
    let wanted = name.and_then(Value::as_str);
    match JOBS.iter().find(|(n, _)| Some(*n) == wanted) {
        Some((_, run)) => run(&Config::of(call), call),
        None => {
            let names: Vec<&str> = JOBS.iter().map(|(n, _)| *n).collect();
            fail(format!(
                "the adapter jira-github has no job {}. Its jobs: {}",
                py_repr_str(&clean(&py_opt(name), 40)),
                names.join(", ")
            ))
        }
    }
}

fn unexpected(e: std::io::Error) -> Error {
    Error::Unexpected(format!("OSError: {e}"))
}

/// The last `keep` characters of a text.
fn tail_chars(text: &str, keep: usize) -> &str {
    let count = text.chars().count();
    if count <= keep {
        return text;
    }
    let start = text
        .char_indices()
        .nth(count - keep)
        .map(|(i, _)| i)
        .unwrap_or(0);
    &text[start..]
}

// Commands of the project -----------------------------------------------------------------------

/// What a command of the project gave.
pub struct Ran {
    pub code: i32,
    pub output: String,
    pub seconds: u64,
    pub timed_out: bool,
    pub stderr: String,
}

impl Ran {
    pub fn passed(&self) -> bool {
        self.code == 0 && !self.timed_out
    }

    /// The cause of a failure (design 11.1): a missing command is the env, a time limit infra.
    pub fn cause(&self) -> &'static str {
        if self.timed_out {
            "infra"
        } else if self.code == 126 || self.code == 127 {
            "env"
        } else {
            "code"
        }
    }

    fn word(&self) -> &'static str {
        if self.passed() {
            "pass"
        } else {
            "fail"
        }
    }
}

/// Runs a command in its own session with a plain environment. At the time limit the whole
/// group is killed.
fn run(
    argv: &[String],
    cwd: &Path,
    timeout: f64,
    extra_env: &[(&str, &str)],
    stdout_only: bool,
) -> Res<Ran> {
    let mut command = Command::new(&argv[0]);
    command
        .args(&argv[1..])
        .current_dir(cwd)
        .env_clear()
        .envs(plain_env())
        .env("PYTHONDONTWRITEBYTECODE", "1");
    for (name, value) in extra_env {
        command.env(name, value);
    }
    let output = proc::run(
        command,
        None,
        Some(Duration::from_secs_f64(timeout)),
        How {
            session: true,
            merge: !stdout_only,
        },
    )
    .map_err(unexpected)?;
    let mut text = output.stdout_text();
    if output.timed_out {
        text.push_str(&format!("\n(killed after {} s)", whole(timeout)));
    }
    Ok(Ran {
        code: output.code.unwrap_or(-9),
        output: text,
        seconds: output.seconds,
        timed_out: output.timed_out,
        stderr: output.stderr_text(),
    })
}

/// `sh -c '<command> "$@"' sh args...`: the command of the configuration can use the shell, and
/// each argument stays one argument.
fn shell(command: &str, args: &[String]) -> Vec<String> {
    let mut argv = vec![
        "sh".to_string(),
        "-c".to_string(),
        format!("{command} \"$@\""),
        "sh".to_string(),
    ];
    argv.extend(args.iter().map(|a| {
        if a.starts_with('-') {
            format!("./{a}")
        } else {
            a.clone()
        }
    }));
    argv
}

fn need_commits(call: &Map<String, Value>, what: &str) -> Res<(PathBuf, String, String)> {
    let clone = match call.get("clone") {
        Some(Value::String(c)) if Path::new(c).is_dir() => PathBuf::from(c),
        _ => return fail(format!("{what} needs the clone of the ticket")),
    };
    let mut commits = Vec::new();
    for name in ["base", "head"] {
        match call.get(name) {
            Some(Value::String(c)) if is_commit(c) => commits.push(c.clone()),
            _ => return fail(format!("{what} needs the {name} commit")),
        }
    }
    let head = commits.pop().unwrap_or_default();
    let base = commits.pop().unwrap_or_default();
    Ok((clone, base, head))
}

/// One part of a glob.
#[derive(Clone, Debug, PartialEq)]
enum Part {
    /// `**/`: any number of folders.
    Folders,
    /// `**`: any text.
    Any,
    /// `*`: any text inside one folder.
    Star,
    /// `?`: one character inside one folder.
    One,
    /// `{a,b}`: one of the texts.
    Choice(Vec<Vec<char>>),
    Char(char),
}

/// A glob of `test_glob`: `**/` is any number of folders, `*` and `?` stay inside one folder,
/// and `{a,b}` is one of the texts.
#[derive(Clone, Debug, PartialEq)]
pub struct Glob {
    parts: Vec<Part>,
}

impl Glob {
    pub fn new(pattern: &str) -> Glob {
        let chars: Vec<char> = pattern.chars().collect();
        let mut parts = Vec::new();
        let mut i = 0;
        while i < chars.len() {
            let rest = &chars[i..];
            if rest.starts_with(&['*', '*', '/']) {
                parts.push(Part::Folders);
                i += 3;
            } else if rest.starts_with(&['*', '*']) {
                parts.push(Part::Any);
                i += 2;
            } else if chars[i] == '*' {
                parts.push(Part::Star);
                i += 1;
            } else if chars[i] == '?' {
                parts.push(Part::One);
                i += 1;
            } else if let Some(end) = (chars[i] == '{')
                .then(|| rest.iter().position(|c| *c == '}'))
                .flatten()
            {
                let inner: String = rest[1..end].iter().collect();
                parts.push(Part::Choice(
                    inner.split(',').map(|p| p.chars().collect()).collect(),
                ));
                i += end + 1;
            } else {
                parts.push(Part::Char(chars[i]));
                i += 1;
            }
        }
        Glob { parts }
    }

    pub fn matches(&self, path: &str) -> bool {
        let text: Vec<char> = super::core::line_end(path).chars().collect();
        match_parts(&self.parts, &text)
    }
}

fn match_parts(parts: &[Part], text: &[char]) -> bool {
    let Some((first, rest)) = parts.split_first() else {
        return text.is_empty();
    };
    match first {
        Part::Char(c) => text.first() == Some(c) && match_parts(rest, &text[1..]),
        Part::One => text.first().is_some_and(|c| *c != '/') && match_parts(rest, &text[1..]),
        Part::Star => {
            for j in 0..=text.len() {
                if match_parts(rest, &text[j..]) {
                    return true;
                }
                if j < text.len() && text[j] == '/' {
                    return false;
                }
            }
            false
        }
        Part::Any => (0..=text.len()).any(|j| match_parts(rest, &text[j..])),
        Part::Folders => {
            match_parts(rest, text)
                || (1..=text.len()).any(|j| text[j - 1] == '/' && match_parts(rest, &text[j..]))
        }
        Part::Choice(options) => options.iter().any(|o| {
            text.len() >= o.len() && text[..o.len()] == o[..] && match_parts(rest, &text[o.len()..])
        }),
    }
}

fn globs_of(value: Option<&Value>) -> Res<Vec<Glob>> {
    let items: Vec<String> = match value {
        Some(Value::String(s)) => s
            .trim()
            .split(|c: char| c.is_whitespace() || c == ',')
            .filter(|p| !p.is_empty())
            .map(str::to_string)
            .collect::<Vec<_>>(),
        Some(Value::Array(items)) if items.iter().all(Value::is_string) => items
            .iter()
            .filter_map(Value::as_str)
            .map(str::to_string)
            .collect(),
        _ => Vec::new(),
    };
    if !items.iter().any(|i| !i.is_empty()) {
        return fail(
            "the proof needs the key `test_glob` in the [adapter] table: a glob or a list of globs",
        );
    }
    Ok(items
        .iter()
        .filter(|i| !i.is_empty())
        .map(|i| Glob::new(i))
        .collect())
}

/// `test_canary`: one test file, or a list of test files, that exist on the base commit and
/// pass there. An empty list when the key is missing.
fn canaries_of(value: Option<&Value>) -> Res<Vec<String>> {
    let items: Vec<&str> = match value {
        None => return Ok(Vec::new()),
        Some(Value::String(s)) => vec![s.as_str()],
        Some(Value::Array(items)) if items.iter().all(Value::is_string) => {
            items.iter().filter_map(Value::as_str).collect()
        }
        Some(_) => {
            return fail("`test_canary` in the [adapter] table must be a test file or a list of test files, such as \"tests/test_smoke.py\"")
        }
    };
    let mut out: Vec<String> = Vec::new();
    for item in items {
        let path = item.trim();
        let path = path.strip_prefix("./").unwrap_or(path);
        if !path.is_empty() && !out.iter().any(|p| p == path) {
            out.push(path.to_string());
        }
    }
    Ok(out)
}

/// The files that the plan declares green: a list, or a text with one path on each line.
fn paths_of(value: Option<&Value>) -> BTreeSet<String> {
    let items: Vec<String> = match value {
        Some(Value::String(s)) => s.split(['\n', ',']).map(str::to_string).collect(),
        Some(Value::Array(items)) => items
            .iter()
            .filter_map(Value::as_str)
            .map(str::to_string)
            .collect(),
        _ => return BTreeSet::new(),
    };
    items
        .iter()
        .map(|i| i.trim())
        .filter(|i| !i.is_empty())
        .map(|p| p.strip_prefix("./").unwrap_or(p).to_string())
        .collect()
}

/// The rest of a line that declares the proof green: `Proof: green` for the whole proof, or
/// `Proof: green <path>` for one test file. Any case; a list mark, a quote mark, and emphasis
/// marks can come first, such as `- **Proof:** green tests/test_export.py`.
fn proof_line(line: &str) -> Option<&str> {
    let skip = |s: &str, f: fn(char) -> bool| -> usize { s.len() - s.trim_start_matches(f).len() };
    let mut rest = line.trim_start();
    while let Some(r) = rest.strip_prefix('>') {
        rest = r.trim_start();
    }
    // A list mark with white space after it.
    let mark = if rest.starts_with(['-', '*', '+']) {
        1
    } else {
        let n = rest.bytes().take_while(u8::is_ascii_digit).count();
        if n > 0 && rest[n..].starts_with(['.', ')']) {
            n + 1
        } else {
            0
        }
    };
    if mark > 0 && rest[mark..].starts_with(char::is_whitespace) {
        rest = rest[mark..].trim_start();
    }
    let emphasis = |c: char| c == '*' || c == '_';
    rest = &rest[skip(rest, emphasis)..];
    rest = rest.trim_start();
    let word = rest.get(..5)?;
    if !word.eq_ignore_ascii_case("proof") {
        return None;
    }
    rest = rest[5..].trim_start();
    rest = &rest[skip(rest, emphasis)..];
    rest = rest.trim_start().strip_prefix(':')?;
    rest = rest.trim_start();
    rest = &rest[skip(rest, emphasis)..];
    rest = rest.trim_start();
    let word = rest.get(..5)?;
    if !word.eq_ignore_ascii_case("green") {
        return None;
    }
    rest = &rest[5..];
    let mut next = rest.chars();
    match (next.next(), next.next()) {
        (Some(c), _) if c.is_ascii_alphanumeric() => None,
        (Some('_'), Some(c)) if c.is_ascii_alphanumeric() => None,
        _ => Some(rest),
    }
}

/// What the plan text declares green: (true, none) for the whole proof, or (false, the test
/// files). A path has a `/` or a file ending, or it stands in backticks. Each other text after
/// `green`, such as `- no new behavior`, `(a refactor)`, or `because ...`, is a comment: the
/// whole proof is green.
pub fn plan_green(text: Option<&str>) -> (bool, BTreeSet<String>) {
    let mut whole = false;
    let mut paths = BTreeSet::new();
    let Some(text) = text else {
        return (whole, paths);
    };
    for line in splitlines(text) {
        let Some(rest) = proof_line(line) else {
            continue;
        };
        let rest = rest.trim().trim_matches(['*', '_']).trim();
        let path = if let Some(inner) = rest.strip_prefix('`') {
            inner.split('`').next().unwrap_or("").trim().to_string()
        } else if rest
            .chars()
            .next()
            .is_some_and(|c| c.is_alphanumeric() || "./_".contains(c))
        {
            // A path has a folder part or a file ending. A plain word, such as `because`,
            // starts a comment.
            let word = rest
                .split_whitespace()
                .next()
                .unwrap_or("")
                .trim_end_matches(['.', ',', ';', ':', '*', '_']);
            if word.contains('/') || word.contains('.') {
                word.to_string()
            } else {
                String::new()
            }
        } else {
            String::new()
        };
        if path.is_empty() {
            whole = true;
        } else {
            paths.insert(path.strip_prefix("./").unwrap_or(&path).to_string());
        }
    }
    (whole, paths)
}

/// The real path of a path whose end can be missing: the links of the part that exists are
/// followed, and the rest is read as it is.
fn real_path(path: &Path) -> PathBuf {
    let mut existing = path.to_path_buf();
    let mut rest: Vec<std::ffi::OsString> = Vec::new();
    loop {
        if let Ok(real) = std::fs::canonicalize(&existing) {
            let mut out = real;
            for part in rest.iter().rev() {
                match Path::new(part).components().next() {
                    Some(Component::ParentDir) => {
                        out.pop();
                    }
                    Some(Component::CurDir) | None => {}
                    _ => out.push(part),
                }
            }
            return out;
        }
        match (existing.file_name(), existing.parent()) {
            (Some(name), Some(parent)) => {
                rest.push(name.to_os_string());
                existing = parent.to_path_buf();
            }
            _ => return path.to_path_buf(),
        }
    }
}

/// Runs `body` with a temporary worktree of the clone at the commit. The worktree is always
/// removed.
fn with_worktree<T>(clone: &Path, commit: &str, body: impl FnOnce(&Path) -> Res<T>) -> Res<T> {
    let tmp = TempDir::new("vision-proof-").map_err(unexpected)?;
    let path = tmp.path.join("base");
    let path_text = path.display().to_string();
    let result = Git::new(
        &["worktree", "add", "--detach", "--quiet", &path_text, commit],
        Some(clone),
    )
    .timeout(600)
    .run()
    .and_then(|_| body(&path));
    let _ = Git::new(
        &["worktree", "remove", "--force", "--force", &path_text],
        Some(clone),
    )
    .unchecked()
    .run();
    drop(tmp);
    let _ = Git::new(&["worktree", "prune"], Some(clone))
        .unchecked()
        .run();
    result
}

// proof -----------------------------------------------------------------------------------------

fn check_row(kind: &str, command: &str, ran: &Ran, holds: bool, cause: &str) -> Value {
    json!({
        "kind": kind,
        "result": if holds { "pass" } else { "fail" },
        "command": command,
        "seconds": ran.seconds,
        "cause": if holds { "" } else { cause },
    })
}

/// The changed tests pass at the head commit and fail on the base commit (design 11.3).
fn proof(config: &Config, call: &Map<String, Value>) -> Res<Value> {
    let what = "the proof";
    let command = config.text_need("test_command", what)?;
    let globs = globs_of(config.get("test_glob"))?;
    let (clone, base, head) = need_commits(call, what)?;
    let timeout = config.number("test_timeout", 600.0)?;
    let canaries = canaries_of(config.get("test_canary"))?;
    let args = args_of(call);
    // The plan (args.plan, the text of approved-plan.md) declares the proof green, and the
    // older form args.green does too: true, or the test files.
    let (plan_all, plan_paths) = plan_green(args.get("plan").and_then(Value::as_str));
    let green_all = args.get("green") == Some(&Value::Bool(true)) || plan_all;
    let mut green = paths_of(args.get("green"));
    green.extend(plan_paths);
    let at = Git::new(&["rev-parse", "HEAD"], Some(&clone)).text()?;
    if at != head {
        return fail(format!(
            "the clone is at {}, not at the head {}",
            short(&at),
            short(&head)
        ));
    }
    let raw = Git::new(
        &[
            "diff",
            "--name-only",
            "--no-renames",
            "--diff-filter=d",
            "-z",
            &base,
            &head,
        ],
        Some(&clone),
    )
    .run()?
    .stdout_text();
    let changed: Vec<String> = raw
        .split('\0')
        .filter(|p| !p.is_empty())
        .map(str::to_string)
        .collect();
    let tests: Vec<String> = changed
        .iter()
        .filter(|p| globs.iter().any(|g| g.matches(p)))
        .cloned()
        .collect();
    let mut checks: Vec<Value> = Vec::new();
    let mut log: Vec<String> = Vec::new();
    let mut problems: Vec<String> = Vec::new();
    if tests.is_empty() && !green_all {
        return Ok(json!({
            "ok": false,
            "detail": format!(
                "no test changed between the base {} and the head {}, so nothing proves the change. Add a test that fails on the base, or declare the proof green in the plan",
                short(&base), short(&head)
            ),
            "documents": [{"name": "proof.txt", "content": "No changed file matches test_glob.\n"}],
            "checks": [],
        }));
    }
    for path in &tests {
        let ran = run(
            &shell(&command, std::slice::from_ref(path)),
            &clone,
            timeout,
            &[],
            false,
        )?;
        checks.push(check_row(
            "proof-head",
            &format!("{command} {path}"),
            &ran,
            ran.passed(),
            ran.cause(),
        ));
        log.push(format!(
            "== {path} at the head {}: {} ({} s) ==\n{}",
            short(&head),
            ran.word(),
            ran.seconds,
            tail_chars(&ran.output, OUTPUT_KEEP)
        ));
        if !ran.passed() {
            problems.push(format!("{path} fails at the head"));
        }
    }
    let mut on_base: Vec<String> = tests
        .iter()
        .filter(|p| !green_all && !green.contains(*p))
        .cloned()
        .collect();
    let mut canary_note = "";
    if !on_base.is_empty() {
        with_worktree(&clone, &base, |tree| {
            if let Some(setup) = config.text("setup_command")? {
                let clone_text = clone.display().to_string();
                let ran = run(
                    &["sh".to_string(), "-c".to_string(), setup.clone()],
                    tree,
                    timeout,
                    &[("VISION_CLONE", &clone_text)],
                    false,
                )?;
                log.push(format!(
                    "== setup_command in the base worktree: {} ==\n{}",
                    ran.word(),
                    tail_chars(&ran.output, OUTPUT_KEEP)
                ));
                if !ran.passed() {
                    checks.push(
                        json!({"kind": "proof-setup", "result": "fail", "command": setup,
                                       "seconds": ran.seconds, "cause": "env"}),
                    );
                    problems.push("setup_command failed in the base worktree".into());
                    on_base.clear();
                }
            }
            if !on_base.is_empty() {
                // A canary proves that the base worktree can run tests at all, so that a
                // broken setup never passes the base half of the proof for the wrong reason.
                let mut guards = Vec::new();
                for canary in &canaries {
                    if tests.contains(canary) {
                        log.push(format!(
                            "== the canary {canary} is a changed test, so it is no canary: skipped =="
                        ));
                    } else {
                        guards.push(canary.clone());
                    }
                }
                if guards.is_empty() && !canaries.is_empty() {
                    canary_note =
                        "; no canary guards the run on the base, because each one is a changed test";
                } else if guards.is_empty() {
                    canary_note = "; no canary guards the run on the base (set test_canary)";
                }
                for canary in &guards {
                    if let Some(failure) = run_canary(
                        &command,
                        canary,
                        tree,
                        &base,
                        timeout,
                        &mut checks,
                        &mut log,
                    )? {
                        problems.push(failure);
                        on_base.clear();
                        break;
                    }
                }
            }
            let inside = real_path(tree);
            for path in &on_base {
                let target = tree.join(path);
                if !real_path(&target).starts_with(&inside) || real_path(&target) == inside {
                    return fail(format!(
                        "the test file {} is outside of the worktree",
                        py_repr_str(&clean(path, 100))
                    ));
                }
                if let Some(parent) = target.parent() {
                    std::fs::create_dir_all(parent).map_err(unexpected)?;
                }
                let spec = format!("{head}:{path}");
                let content = Git::new(&["show", &spec], Some(&clone)).run()?.stdout;
                std::fs::write(&target, content).map_err(unexpected)?;
                let ran = run(
                    &shell(&command, std::slice::from_ref(path)),
                    tree,
                    timeout,
                    &[],
                    false,
                )?;
                let fails = !ran.passed();
                let cause = if ran.passed() { "code" } else { ran.cause() };
                checks.push(check_row(
                    "proof-base",
                    &format!("{command} {path}"),
                    &ran,
                    fails,
                    cause,
                ));
                log.push(format!(
                    "== {path} on the base {}: {} ({} s) ==\n{}",
                    short(&base),
                    if fails { "fail, as it must" } else { "pass" },
                    ran.seconds,
                    tail_chars(&ran.output, OUTPUT_KEEP)
                ));
                if !fails {
                    problems.push(format!(
                        "{path} also passes on the base, so it does not prove the change"
                    ));
                }
            }
            Ok(())
        })?;
    }
    if let Some(lint) = config.text("lint_command")? {
        if !changed.is_empty() {
            let ran = run(&shell(&lint, &changed), &clone, timeout, &[], false)?;
            checks.push(check_row(
                "proof-lint",
                &lint,
                &ran,
                ran.passed(),
                ran.cause(),
            ));
            log.push(format!(
                "== lint_command: {} ==\n{}",
                ran.word(),
                tail_chars(&ran.output, OUTPUT_KEEP)
            ));
            if !ran.passed() {
                problems.push("lint_command fails on the changed files".into());
            }
        }
    }
    let declared: Vec<&String> = tests.iter().filter(|t| green.contains(*t)).collect();
    let mut declared: Vec<&str> = declared.iter().map(|s| s.as_str()).collect();
    declared.sort();
    declared.dedup();
    let green_note = if green_all {
        "; the plan declares the whole proof green".to_string()
    } else if !declared.is_empty() {
        format!("; the plan declares {} green", declared.join(", "))
    } else {
        String::new()
    };
    let detail = if problems.is_empty() {
        format!(
            "the proof holds: {} changed test file(s) pass at {}, {} of them fail on the base {}{green_note}{canary_note}",
            tests.len(),
            short(&head),
            on_base.len(),
            short(&base)
        )
    } else {
        format!(
            "the proof does not hold: {}{canary_note}",
            problems.join("; ")
        )
    };
    Ok(json!({
        "ok": problems.is_empty(),
        "detail": detail,
        "documents": [{"name": "proof.txt", "content": format!("{}\n", log.join("\n\n"))}],
        "checks": checks,
    }))
}

/// Runs one canary of `test_canary` in the worktree of the base commit, with a check row of the
/// kind proof-canary. Gives the problem when the worktree cannot run it, else `None`.
fn run_canary(
    command: &str,
    canary: &str,
    tree: &Path,
    base: &str,
    timeout: f64,
    checks: &mut Vec<Value>,
    log: &mut Vec<String>,
) -> Res<Option<String>> {
    let label = format!("{command} {canary}");
    let target = tree.join(canary);
    let inside = real_path(tree);
    let real = real_path(&target);
    if !real.starts_with(&inside) || real == inside || !target.is_file() {
        checks.push(json!({"kind": "proof-canary", "result": "fail", "command": label, "seconds": 0, "cause": "env"}));
        log.push(format!(
            "== the canary {canary} is missing in the base worktree {} ==",
            short(base)
        ));
        return Ok(Some(format!(
            "the worktree of the base commit {} cannot run tests: the canary {canary} is missing there. Fix setup_command (or the path in test_canary), then run vision retry",
            short(base)
        )));
    }
    let ran = run(
        &shell(command, &[canary.to_string()]),
        tree,
        timeout,
        &[],
        false,
    )?;
    checks.push(check_row("proof-canary", &label, &ran, ran.passed(), "env"));
    log.push(format!(
        "== the canary {canary} on the base {}: {} ({} s) ==\n{}",
        short(base),
        ran.word(),
        ran.seconds,
        tail_chars(&ran.output, OUTPUT_KEEP)
    ));
    if ran.passed() {
        return Ok(None);
    }
    Ok(Some(format!(
        "the worktree of the base commit {} cannot run tests: the canary {canary} fails there, so no changed test ran on the base. Fix setup_command so that the tests run in that worktree, then run vision retry",
        short(base)
    )))
}

// second-review ---------------------------------------------------------------------------------

fn second_review(config: &Config, call: &Map<String, Value>) -> Res<Value> {
    let Some(command) = config.text("second_review_command")? else {
        // No document: the facts file of the review turn then gives null for
        // second-review.md, which the reviewer reads as "no second review ran".
        return Ok(json!({
            "ok": true,
            "detail": "no second reviewer is set: second_review_command is empty, so no second review ran",
            "documents": [],
            "checks": [],
        }));
    };
    let (clone, base, head) = need_commits(call, "the second review")?;
    let timeout = config.number("second_review_timeout", 1100.0)?;
    let span = format!("{base}..{head}");
    let ran = run(
        &shell(&command, &[span]),
        &clone,
        timeout,
        &[("VISION_BASE", &base), ("VISION_HEAD", &head)],
        true,
    )?;
    let check = check_row("second-review", &command, &ran, ran.passed(), ran.cause());
    if !ran.passed() {
        let tail = {
            let err = tail_chars(&ran.stderr, 1000);
            if err.is_empty() {
                tail_chars(&ran.output, 1000)
            } else {
                err
            }
        };
        return Ok(json!({
            "ok": false,
            "detail": format!(
                "second_review_command failed with exit code {}: {}",
                ran.code,
                clean(tail, 600)
            ),
            "documents": [{"name": "second-review.md", "content": ran.output}],
            "checks": [check],
        }));
    }
    Ok(json!({
        "ok": true,
        "detail": format!("the second reviewer looked at {}..{}", short(&base), short(&head)),
        "documents": [{"name": "second-review.md", "content": ran.output}],
        "checks": [check],
    }))
}

// comments --------------------------------------------------------------------------------------

/// Each comment that waits for an answer, as comments.json: the inline threads, the reviews
/// that ask for changes or comment with a body, and the conversation comments, one row each with
/// its kind. The text is untrusted data.
fn comments(config: &Config, call: &Map<String, Value>) -> Res<Value> {
    let why = "the job comments";
    let own = config.logins("own_logins")?;
    if own.is_empty() {
        return fail(format!(
            "{why} needs the key `own_logins` in the [adapter] table"
        ));
    }
    let ignore = Ignore::new(&config.logins("ignore_logins")?)?;
    let gh = GitHub::new(config, why, None)?;
    let branch = text_of(call.get("branch"));
    let Some(pull) = gh.find_pr(pr_number(call.get("pr")), &branch)? else {
        return Ok(json!({
            "ok": true,
            "detail": "no pull request, so no comment waits",
            "documents": [{"name": "comments.json", "content": "[]\n"}],
        }));
    };
    let number = forge::int(&pull, "number")
        .ok_or_else(|| Error::Unexpected("KeyError: 'number'".into()))?;
    let rows = forge::waiting_rows(
        &gh.reviews(number)?,
        &gh.review_comments(number)?,
        &gh.issue_comments(number)?,
        &own,
        &ignore,
    );
    let kinds: Vec<String> = forge::KINDS
        .iter()
        .map(|k| {
            let count = rows.iter().filter(|r| r["kind"] == *k).count();
            format!("{count} {k}")
        })
        .collect();
    Ok(json!({
        "ok": true,
        "detail": format!(
            "{} comment(s) of #{number} wait for a reply ({})",
            rows.len(),
            kinds.join(", ")
        ),
        "documents": [{"name": "comments.json", "content": format!("{}\n", pretty(&Value::Array(rows)))}],
    }))
}

// ci-log ----------------------------------------------------------------------------------------

/// The end of the log of each failed job of the last run at the head, as ci-log.txt.
fn ci_log(config: &Config, call: &Map<String, Value>) -> Res<Value> {
    let why = "the job ci-log";
    let head = match call.get("head") {
        Some(Value::String(h)) if is_commit(h) => h.clone(),
        _ => return fail(format!("{why} needs the head commit")),
    };
    let workflow = config.text_need("ci_workflow", why)?;
    let keep = (config.number("log_tail_kb", 64.0)? * 1024.0).trunc() as usize;
    let gh = GitHub::new(config, why, None)?;
    let (content, detail) = ci_log_text(&gh, config, &head, &workflow, keep, None)?;
    Ok(
        json!({"ok": true, "detail": detail, "documents": [{"name": "ci-log.txt", "content": content}]}),
    )
}

/// The text of ci-log.txt and the detail: the end of the log of each failed job of a run.
pub fn ci_log_text(
    gh: &GitHub,
    config: &Config,
    head: &str,
    workflow: &str,
    keep: usize,
    run_id: Option<i64>,
) -> Res<(String, String)> {
    let run = match run_id.filter(|r| *r != 0) {
        Some(id) => {
            let run = gh.client.get(
                &gh.path(&format!("actions/runs/{id}")),
                &format!("read the run {id}"),
            )?;
            truthy(Some(&run)).cloned()
        }
        None => gh.last_run(Some(head), workflow)?,
    };
    let Some(run) = run else {
        return Ok((
            format!("No run of {workflow} at {}.\n", short(head)),
            format!("no run of {workflow} at {}", short(head)),
        ));
    };
    let id = forge::int(&run, "id")
        .ok_or_else(|| Error::Unexpected("TypeError: the run has no id".into()))?;
    let jobs: Vec<Value> = gh
        .client
        .pages(
            &gh.path(&format!("actions/runs/{id}/jobs")),
            &format!("list the jobs of the run {id}"),
            &[("filter".to_string(), "latest".to_string())],
            100,
            5,
        )?
        .into_iter()
        .filter(Value::is_object)
        .collect();
    let red = |v: Option<&Value>| {
        v.and_then(Value::as_str)
            .is_some_and(|c| RED_JOBS.contains(&c))
    };
    let failed: Vec<&Value> = jobs.iter().filter(|j| red(j.get("conclusion"))).collect();
    let run_head = text_of(run.get("head_sha"));
    let run_head = if run_head.is_empty() {
        head.to_string()
    } else {
        run_head
    };
    let names: Vec<String> = failed.iter().map(|j| py_opt(j.get("name"))).collect();
    let mut lines = vec![
        format!(
            "The run {} of {workflow} at {}: {} (attempt {})",
            py_opt(run.get("id")),
            short(&run_head),
            forge::run_state(Some(&run)),
            truthy(run.get("run_attempt"))
                .map(super::core::py_str)
                .unwrap_or_else(|| "1".into())
        ),
        format!(
            "Failed jobs: {}",
            if names.is_empty() {
                "none".to_string()
            } else {
                names.join(", ")
            }
        ),
        String::new(),
    ];
    for j in &failed {
        let steps: Vec<String> = match truthy(j.get("steps")) {
            Some(Value::Array(steps)) => steps
                .iter()
                .filter(|s| s.is_object() && red(s.get("conclusion")))
                .map(|s| py_opt(s.get("name")))
                .collect(),
            _ => Vec::new(),
        };
        lines.push(format!(
            "== the job {}: {}; failed steps: {} ==",
            py_opt(j.get("name")),
            py_opt(j.get("conclusion")),
            if steps.is_empty() {
                "-".to_string()
            } else {
                steps.join(", ")
            }
        ));
        lines.push(job_log(gh, config, j.get("id"), keep)?);
        lines.push(String::new());
    }
    Ok((
        lines.join("\n"),
        format!(
            "the logs of {} failed job(s) of the run {}",
            failed.len(),
            py_opt(run.get("id"))
        ),
    ))
}

/// Removes the color codes of a terminal from a text.
fn strip_ansi(text: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    let mut out = String::with_capacity(text.len());
    let mut i = 0;
    while i < chars.len() {
        if chars[i] == '\u{1b}' && chars.get(i + 1) == Some(&'[') {
            let mut j = i + 2;
            while j < chars.len()
                && (chars[j].is_ascii_digit() || chars[j] == ';' || chars[j] == '?')
            {
                j += 1;
            }
            while j < chars.len() && (' '..='/').contains(&chars[j]) {
                j += 1;
            }
            if j < chars.len() && ('@'..='~').contains(&chars[j]) {
                i = j + 1;
                continue;
            }
        }
        out.push(chars[i]);
        i += 1;
    }
    out
}

/// The last `keep` bytes of a job log. GitHub answers with a redirect to a storage address; the
/// token never goes there.
fn job_log(gh: &GitHub, config: &Config, job_id: Option<&Value>, keep: usize) -> Res<String> {
    let shown = py_opt(job_id);
    let Some(id) = job_id.and_then(Value::as_i64) else {
        return Err(Error::Unexpected(format!(
            "TypeError: the job id {shown} is not a number"
        )));
    };
    let mut response = match gh.client.call(
        "GET",
        &gh.path(&format!("actions/jobs/{id}/logs")),
        &format!("read the log of the job {shown}"),
        Call {
            ok: &[200, 301, 302, 303, 307],
            limit: Some(keep),
            ..Call::default()
        },
    ) {
        Ok(response) => response,
        Err(Error::Http { text, .. }) => return Ok(format!("(the log could not be read: {text})")),
        Err(e) => return Err(e),
    };
    if response.status != 200 {
        let location = response.header("Location").unwrap_or("").to_string();
        let parts = split(&location);
        let base = if !parts.netloc.is_empty() {
            format!("{}://{}", parts.scheme, parts.netloc)
        } else if !parts.scheme.is_empty() {
            format!("{}:", parts.scheme)
        } else {
            String::new()
        };
        if check_base(&base, "the log address").is_err() {
            return Ok("(the log is at an address that is not https, so it was not read)".into());
        }
        let headers = vec![("User-Agent".to_string(), USER_AGENT.to_string())];
        response = match send(
            "GET",
            &location,
            &headers,
            None,
            config.http_timeout()?,
            Some(keep),
        ) {
            Ok(response) => response,
            Err(Error::Unreachable(text)) => {
                return Ok(format!("(the log could not be read: {text})"))
            }
            Err(e) => return Err(e),
        };
        if response.status != 200 {
            return Ok(format!(
                "(the log could not be read: the storage answered HTTP {})",
                response.status
            ));
        }
    }
    let mut text = strip_ansi(&String::from_utf8_lossy(&response.body));
    if response.body.len() >= keep {
        if let Some((_, rest)) = text.split_once('\n') {
            text = format!("(the start is cut)\n{rest}");
        }
    }
    Ok(text)
}

// app-start -------------------------------------------------------------------------------------

/// True when the address answers with a status below 400.
fn answers(url: &str, timeout: f64) -> Res<bool> {
    let headers = vec![("User-Agent".to_string(), USER_AGENT.to_string())];
    match send("GET", url, &headers, None, timeout, Some(1024)) {
        Ok(response) => Ok(response.status < 400),
        Err(Error::Unreachable(_)) => Ok(false),
        Err(e) => Err(e),
    }
}

fn valid_env_name(name: &str) -> bool {
    (1..=40).contains(&name.len())
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

/// Starts the app of the clone for QA, and waits until its health address answers. The app runs
/// in its own session with a plain environment, so it outlives the job and gets no secret.
///
/// The job remembers the app that it started in a state file of the temporary folder (see
/// [`app_state_path`]): the process id, the start time of that process as the system reports
/// it, the head commit, and the log. At the next call for the same clone and env:
///
/// - The app of the state file still runs, and the head commit changed: the job stops its
///   process group and starts the app again at the new head (unless `app_restart` is false, for
///   an app that loads new code by itself).
/// - The address answers, and no app of the job runs: the person started it. The job leaves it
///   as it is and says that it can run older code.
///
/// The job never stops a process that it did not start: it compares the start time of the
/// process with the start time of the state file first, so that a process id that the system
/// gave to a different process is never taken for the app.
fn app_start(config: &Config, call: &Map<String, Value>) -> Res<Value> {
    let what = "app-start";
    let command = config.text_need("app_start_command", what)?;
    let health = config.text_need("app_health_url", what)?;
    let parts = split(&health);
    if !matches!(parts.scheme.as_str(), "http" | "https") || parts.netloc.is_empty() {
        return fail("`app_health_url` must be an http or https address, such as http://localhost:3000/health");
    }
    let app_url = match config.text("app_url")? {
        Some(url) => url,
        None => format!("{}://{}/", parts.scheme, parts.netloc),
    };
    let timeout = config.number("app_start_timeout", 120.0)?;
    let restart = config.flag("app_restart", true)?;
    let env_name = truthy(args_of(call).get("env"))
        .map(super::core::py_str)
        .unwrap_or_else(|| "local".into());
    if !valid_env_name(super::core::line_end(&env_name)) {
        return fail("args.env of app-start must be a plain name, such as local");
    }
    let clone = match call.get("clone") {
        Some(Value::String(c)) if Path::new(c).is_dir() => PathBuf::from(c),
        _ => return fail("app-start needs the clone of the ticket"),
    };
    let head = clone_head(&clone)?;
    let head_shown = short_opt(head.as_deref());
    let mut info = json!({"env": env_name, "url": app_url, "health": health, "started": false,
                          "pid": null, "log": null, "head": null, "restarted": false});
    let name = format!("app-{env_name}.json");
    let answer = |ok: bool, detail: String, info: &Value| -> Res<Value> {
        Ok(json!({"ok": ok, "detail": detail,
                  "documents": [{"name": name, "content": format!("{}\n", pretty(info))}]}))
    };
    let state_path = app_state_path(&std::env::temp_dir(), &clone, &env_name);
    let mut state = read_app_state(&state_path);
    let ours = state
        .as_ref()
        .is_some_and(|s| running_since(s.pid).as_deref() == Some(s.started.as_str()));
    if state.is_some() && !ours {
        // The app of the state file ended, or its process id now names a different process.
        remove_file(&state_path);
        state = None;
    }
    let mut restarted_from: Option<String> = None;
    if let (true, Some(state)) = (ours, &state) {
        info["pid"] = json!(state.pid);
        info["log"] = json!(state.log);
        info["head"] = json!(state.head);
        let same = state.head == head;
        if (same || !restart) && answers(&health, 2.0)? {
            if same {
                return answer(
                    true,
                    format!(
                        "the app that Vision started (process id {}) already answers at {health}, at the head {head_shown}, so nothing was started",
                        state.pid
                    ),
                    &info,
                );
            }
            return answer(
                true,
                format!(
                    "the app that Vision started (process id {}) already answers at {health}. The head is {head_shown} now, and app_restart is false, so it runs on: it loads the new code by itself",
                    state.pid
                ),
                &info,
            );
        }
        // A new head, or an app of the job that no longer answers: stop it, then start again.
        if running_since(state.pid).as_deref() != Some(state.started.as_str()) {
            remove_file(&state_path);
        } else {
            stop_pid(state.pid, &state.started, 3.0);
            remove_file(&state_path);
            restarted_from = state.head.clone();
            if !wait_silent(&health, 10.0)? {
                info["pid"] = Value::Null;
                info["log"] = Value::Null;
                info["head"] = Value::Null;
                return answer(
                    false,
                    format!(
                        "Vision stopped its app (process id {}), but {health} still answers, so a different process answers there. Stop that process, then run vision retry",
                        state.pid
                    ),
                    &info,
                );
            }
        }
        info["pid"] = Value::Null;
        info["log"] = Value::Null;
        info["head"] = Value::Null;
    } else if answers(&health, 2.0)? {
        return answer(
            true,
            format!(
                "the app already answers at {health}, but Vision did not start it, so it can run older code than the head {head_shown}. Stop it to let Vision start the app at the head, or restart it yourself"
            ),
            &info,
        );
    }
    let key = check_key(call.get("key"), "the ticket key")?;
    let (log, log_path) =
        proc::temp_file(&format!("vision-app-{key}-{}-", stamp()), ".log").map_err(unexpected)?;
    let log_text = log_path.display().to_string();
    let started = Instant::now();
    let deadline = started + Duration::from_secs_f64(timeout);
    let mut app = Command::new("sh");
    app.arg("-c")
        .arg(&command)
        .current_dir(&clone)
        .env_clear()
        .envs(plain_env())
        // A Python app writes its output at once, so that its log shows how far it came even
        // when it is stopped: a stopped process loses the output that waits in its buffer.
        .env("PYTHONUNBUFFERED", "1")
        .stdin(Stdio::null())
        .stdout(log.try_clone().map_err(unexpected)?)
        .stderr(log);
    proc::new_session(&mut app);
    let mut child = app.spawn().map_err(unexpected)?;
    drop(app);
    let pid = child.id();
    info["started"] = json!(true);
    info["pid"] = json!(pid);
    info["log"] = json!(log_text);
    info["head"] = json!(head);
    info["restarted"] = json!(restarted_from.is_some());
    if let Some(since) = running_since(i64::from(pid)) {
        write_app_state(
            &state_path,
            &json!({"clone": clone.display().to_string(), "env": env_name, "pid": pid, "started": since,
                    "head": head, "log": log_text, "at": now()}),
        );
    }
    let again = match &restarted_from {
        Some(old) => format!(
            " It stopped the app of the head {} first, so QA tests the code of the head {head_shown}.",
            short(old)
        ),
        None => String::new(),
    };
    loop {
        // One probe never runs past the time limit, and the last probe comes at the limit.
        let left = deadline
            .saturating_duration_since(Instant::now())
            .as_secs_f64();
        if answers(&health, left.clamp(0.2, 2.0))? {
            return answer(
                true,
                format!(
                    "started the app (process id {pid}) in {} s; it answers at {health}. Its log is {log_text}.{again}",
                    started.elapsed().as_secs()
                ),
                &info,
            );
        }
        if let Some(status) = child.try_wait().map_err(unexpected)? {
            remove_file(&state_path);
            return answer(
                false,
                format!(
                    "the app ended with exit code {} before it answered at {health}. The end of its log: {}",
                    code_of(status),
                    clean(&tail_of(&log_path), 1500)
                ),
                &info,
            );
        }
        if Instant::now() >= deadline {
            stop_child(&mut child, 3.0);
            remove_file(&state_path);
            let tail = clean(&tail_of(&log_path), 1500);
            return answer(
                false,
                format!(
                    "the app did not answer at {health} in {} s, so it was stopped. The end of its log: {}",
                    whole(timeout),
                    if tail.is_empty() { "(empty)".to_string() } else { tail }
                ),
                &info,
            );
        }
        let left = deadline.saturating_duration_since(Instant::now());
        std::thread::sleep(Duration::from_millis(500).min(left));
    }
}

/// The head commit of the clone, or `None` when it has none.
fn clone_head(clone: &Path) -> Res<Option<String>> {
    let output = Git::new(&["rev-parse", "--verify", "--quiet", "HEAD"], Some(clone))
        .unchecked()
        .run()?;
    let text = output.stdout_text().trim().to_string();
    Ok((output.code == Some(0) && is_commit(&text)).then_some(text))
}

/// The state file of the app of one clone and env: `vision-app-<hash>.json` in the temporary
/// folder `tmp`, where the hash is of the real path of the clone and the name of the env.
pub fn app_state_path(tmp: &Path, clone: &Path, env_name: &str) -> PathBuf {
    let real = std::fs::canonicalize(clone).unwrap_or_else(|_| clone.to_path_buf());
    let text = format!("{}\0{env_name}", real.display());
    let digest = crate::util::ids::sha256_hex(text.as_bytes());
    tmp.join(format!("vision-app-{}.json", &digest[..24]))
}

/// The state of an app that an earlier call started.
#[derive(Clone, Debug, PartialEq)]
pub struct AppState {
    pub pid: i64,
    pub started: String,
    pub head: Option<String>,
    pub log: Option<String>,
}

/// The state of an app that an earlier call started, or `None` for a missing or broken file.
///
/// The job stops the process that this file names, and other users can write into a shared
/// temporary folder. So the file counts only when it is a plain file of this user that no other
/// user can write. The job never follows a link at that name.
pub fn read_app_state(path: &Path) -> Option<AppState> {
    let mut file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NONBLOCK | libc::O_NOFOLLOW)
        .open(path)
        .ok()?;
    let meta = file.metadata().ok()?;
    // SAFETY: getuid has no failure and no side effect.
    let uid = unsafe { libc::getuid() };
    if !meta.file_type().is_file() || meta.uid() != uid || meta.mode() & 0o022 != 0 {
        return None;
    }
    let mut text = String::new();
    file.read_to_string(&mut text).ok()?;
    let state: Value = serde_json::from_str(&text).ok()?;
    let pid = state
        .get("pid")
        .and_then(Value::as_i64)
        .filter(|p| *p > 1)?;
    let started = state
        .get("started")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())?
        .to_string();
    Some(AppState {
        pid,
        started,
        head: state
            .get("head")
            .and_then(Value::as_str)
            .map(str::to_string),
        log: state.get("log").and_then(Value::as_str).map(str::to_string),
    })
}

/// Writes the state file for this user only: to a new temporary name first, and then a
/// rename. Gives false when the folder does not let it, such as a file of a different user at
/// that name. The job then has no state, so it never stops that app at a later call.
fn write_app_state(path: &Path, state: &Value) -> bool {
    let temp = PathBuf::from(format!("{}.{}.tmp", path.display(), std::process::id()));
    remove_file(&temp);
    let written = (|| -> std::io::Result<()> {
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW)
            .open(&temp)?;
        std::io::Write::write_all(&mut file, format!("{}\n", pretty(state)).as_bytes())?;
        std::fs::rename(&temp, path)
    })();
    if written.is_err() {
        remove_file(&temp);
        return false;
    }
    true
}

fn remove_file(path: &Path) {
    let _ = std::fs::remove_file(path);
}

/// The start time of a running process as the system reports it (`ps -o lstart=`, on macOS and
/// on Linux), or `None` when no such process runs. A zombie counts as gone.
pub fn running_since(pid: i64) -> Option<String> {
    let mut command = Command::new("ps");
    command
        .args(["-o", "stat=", "-o", "lstart=", "-p", &pid.to_string()])
        .env_clear()
        .envs(plain_env())
        .env("LC_ALL", "C")
        .env("LANG", "C");
    let output = proc::run(command, None, Some(Duration::from_secs(30)), How::default()).ok()?;
    if output.code != Some(0) {
        return None;
    }
    let text = output.stdout_text();
    let mut parts = text.trim_start().splitn(2, char::is_whitespace);
    let stat = parts.next().filter(|s| !s.is_empty())?;
    let rest = parts.next()?.trim();
    if rest.is_empty() || stat.starts_with('Z') {
        return None;
    }
    Some(rest.split_whitespace().collect::<Vec<_>>().join(" "))
}

/// Waits until the address no longer answers. Gives false when it still answers after
/// `seconds`.
fn wait_silent(url: &str, seconds: f64) -> Res<bool> {
    let deadline = Instant::now() + Duration::from_secs_f64(seconds);
    while answers(url, 1.0)? {
        if Instant::now() >= deadline {
            return Ok(false);
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    Ok(true)
}

/// Stops the process group of the app that this job started: SIGTERM, then SIGKILL after
/// `grace` seconds. It waits for the end of the app, so that the log holds each line that the
/// app wrote.
fn stop_child(child: &mut std::process::Child, grace: f64) {
    let pid = child.id();
    proc::kill_group(pid, libc::SIGTERM);
    if !wait_child(child, grace) {
        proc::kill_group(pid, libc::SIGKILL);
        wait_child(child, grace);
    }
}

fn wait_child(child: &mut std::process::Child, seconds: f64) -> bool {
    let deadline = Instant::now() + Duration::from_secs_f64(seconds);
    loop {
        if let Ok(Some(_)) = child.try_wait() {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// Stops the process group of an app that an earlier call started. The group gets a signal only
/// while the process with that id still has the start time of the state file.
fn stop_pid(pid: i64, started: &str, grace: f64) {
    let Ok(id) = u32::try_from(pid) else {
        return;
    };
    for signal in [libc::SIGTERM, libc::SIGKILL] {
        if started.is_empty() || running_since(pid).as_deref() != Some(started) {
            return;
        }
        proc::kill_group(id, signal);
        let deadline = Instant::now() + Duration::from_secs_f64(grace);
        while running_since(pid).as_deref() == Some(started) && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(100));
        }
    }
}

fn tail_of(path: &Path) -> String {
    let size = 4000u64;
    let Ok(mut file) = std::fs::File::open(path) else {
        return String::new();
    };
    let Ok(end) = file.seek(SeekFrom::End(0)) else {
        return String::new();
    };
    if file
        .seek(SeekFrom::Start(end.saturating_sub(size)))
        .is_err()
    {
        return String::new();
    }
    let mut data = Vec::new();
    let _ = file.read_to_end(&mut data);
    String::from_utf8_lossy(&data).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn globs_follow_the_rules_of_test_glob() {
        let cases = [
            ("tests/**/test_*.py", "tests/test_a.py", true),
            ("tests/**/test_*.py", "tests/unit/deep/test_a.py", true),
            ("tests/**/test_*.py", "src/test_a.py", false),
            ("**/*_spec.rb", "spec_helper.rb", false),
            ("**/*_spec.rb", "a_spec.rb", true),
            ("**/*_spec.rb", "spec/models/user_spec.rb", true),
            ("src/*.test.{ts,tsx}", "src/a.test.tsx", true),
            ("src/*.test.{ts,tsx}", "src/x/a.test.ts", false),
            ("a?c", "abc", true),
            ("a?c", "a/c", false),
            ("docs/**", "docs/a/b.md", true),
        ];
        for (pattern, path, expected) in cases {
            assert_eq!(
                Glob::new(pattern).matches(path),
                expected,
                "{pattern} {path}"
            );
        }
    }

    #[test]
    fn color_codes_go() {
        assert_eq!(strip_ansi("a \u{1b}[31mred\u{1b}[0m b"), "a red b");
        assert_eq!(strip_ansi("\u{1b}[?25l"), "");
        assert_eq!(strip_ansi("keep \u{1b}[ this"), "keep his");
        assert_eq!(strip_ansi("keep \u{1b}["), "keep \u{1b}[");
    }
}
