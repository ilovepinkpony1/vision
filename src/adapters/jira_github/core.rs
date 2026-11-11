//! The call loop of a script, the errors, the configuration, the secrets, and the text helpers.
//!
//! Rules that each module follows:
//!
//! - A string from Jira or GitHub is data. It never reaches a shell, and it reaches a command line
//!   only as one item of an argument list.
//! - A secret comes from the environment of the script, and it goes only into one HTTP header or
//!   into the environment of one git call. It never goes into a file, a command line, an answer,
//!   or an error text. Each user of a machine can read a command line in the process list.
//! - A child process that runs code of the project gets a plain environment with no secret.

use std::fmt;
use std::io::{Read, Write};

use serde_json::{json, Map, Value};

/// The secrets that the scripts read, by name. Each value is masked in each text that leaves.
pub const SECRET_NAMES: [&str; 3] = ["JIRA_TOKEN", "FORGE_TOKEN", "PR_TOKEN"];

/// The executor gives a script these names and the secrets. A child process that runs code of
/// the project gets only these.
pub const PLAIN_ENV: [&str; 4] = ["PATH", "HOME", "LANG", "TMPDIR"];

pub const DEFAULT_GITHUB_API: &str = "https://api.github.com";
pub const DEFAULT_HTTP_TIMEOUT: f64 = 30.0;

/// Why a call did not give its answer.
#[derive(Clone, Debug, PartialEq)]
pub enum Error {
    /// The call failed: exit code 1.
    Failure(String),
    /// A gate of the script refused the call: exit code 2.
    Refusal(String),
    /// A server answered with a status that the call does not accept. A failure.
    Http { text: String, status: u16 },
    /// No answer: the address, the network, or the time limit. A failure.
    Unreachable(String),
    /// A fault that no rule of the script expects: exit code 1, and no handler catches it.
    Unexpected(String),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Failure(text)
            | Error::Refusal(text)
            | Error::Unreachable(text)
            | Error::Unexpected(text)
            | Error::Http { text, .. } => f.write_str(text),
        }
    }
}

pub type Res<T> = Result<T, Error>;

/// A failure with this text.
pub fn fail<T>(text: impl Into<String>) -> Res<T> {
    Err(Error::Failure(text.into()))
}

/// A refusal with this text.
pub fn refuse<T>(text: impl Into<String>) -> Res<T> {
    Err(Error::Refusal(text.into()))
}

// The call loop ---------------------------------------------------------------------------------

/// Reads the call from standard input, runs the handler, and prints its answer. Gives the exit
/// code: 0 done, 1 a failure, 2 a refusal. A panic of the handler is a failure with one masked
/// line, never a trace.
pub fn main(handler: fn(&Map<String, Value>) -> Res<Value>) -> u8 {
    std::panic::set_hook(Box::new(|_| {}));
    let outcome = std::panic::catch_unwind(|| read_call().and_then(|call| handler(&call)));
    let _ = std::panic::take_hook();
    let mut stdout = std::io::stdout().lock();
    match outcome {
        Ok(Ok(answer)) => {
            let text = serde_json::to_string(&mask_json(answer)).unwrap_or_default();
            let _ = stdout.write_all(text.as_bytes());
            let _ = stdout.flush();
            0
        }
        Ok(Err(Error::Refusal(text))) => {
            let text = mask(&text);
            eprintln!("{text}");
            let _ = stdout.write_all(json!({ "detail": text }).to_string().as_bytes());
            let _ = stdout.flush();
            2
        }
        Ok(Err(Error::Unexpected(text))) => {
            eprintln!("unexpected error: {}", mask(&text));
            1
        }
        Ok(Err(e)) => {
            eprintln!("{}", mask(&e.to_string()));
            1
        }
        Err(panic) => {
            let text = panic
                .downcast_ref::<String>()
                .cloned()
                .or_else(|| panic.downcast_ref::<&str>().map(|s| s.to_string()))
                .unwrap_or_default();
            eprintln!("unexpected error: panic: {}", mask(&text));
            1
        }
    }
}

fn read_call() -> Res<Map<String, Value>> {
    let mut data = Vec::new();
    std::io::stdin()
        .read_to_end(&mut data)
        .map_err(|e| Error::Unexpected(format!("OSError: {e}")))?;
    let text = String::from_utf8(data)
        .map_err(|_| Error::Unexpected("UnicodeDecodeError: the call is not UTF-8".into()))?;
    if text.trim().is_empty() {
        return Ok(Map::new());
    }
    match serde_json::from_str::<Value>(&text) {
        Ok(Value::Object(map)) => Ok(map),
        Ok(_) => fail("the call is not a JSON object"),
        Err(e) => Err(Error::Unexpected(format!("JSONDecodeError: {e}"))),
    }
}

/// A part of a later stage: the core stops with a clear need and never acts wrongly.
pub fn not_built<T>(what: &str) -> Res<T> {
    refuse(format!(
        "{what} is not built yet in the adapter jira-github"
    ))
}

// Masks and texts -------------------------------------------------------------------------------

/// The text with each secret value of the environment replaced by its name.
pub fn mask(text: &str) -> String {
    let mut text = text.to_string();
    for name in SECRET_NAMES {
        if let Ok(value) = std::env::var(name) {
            if value.chars().count() >= 4 && text.contains(&value) {
                text = text.replace(&value, &format!("[secret {name}]"));
            }
        }
    }
    text
}

/// Masks each string of a JSON value, keys included.
pub fn mask_json(value: Value) -> Value {
    match value {
        Value::String(text) => Value::String(mask(&text)),
        Value::Array(items) => Value::Array(items.into_iter().map(mask_json).collect()),
        Value::Object(map) => Value::Object(
            map.into_iter()
                .map(|(key, item)| (mask(&key), mask_json(item)))
                .collect(),
        ),
        other => other,
    }
}

/// Untrusted text for an error or a detail: one line, no control character, cut to `limit`
/// characters.
pub fn clean(text: &str, limit: usize) -> String {
    let text: String = text
        .chars()
        .map(|c| if printable(c) { c } else { ' ' })
        .collect();
    let text = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if text.chars().count() <= limit {
        text
    } else {
        let cut: String = text.chars().take(limit).collect();
        format!("{cut} [cut]")
    }
}

/// True for a character that a text can show as it is: not a control character, not a format
/// character, not a private character, and not a separator other than the space.
pub fn printable(c: char) -> bool {
    if c == ' ' {
        return true;
    }
    if c.is_control() || c.is_whitespace() {
        return false;
    }
    let n = u32::from(c);
    let format = matches!(
        n,
        0xAD | 0x600..=0x605
            | 0x61C
            | 0x6DD
            | 0x70F
            | 0x180E
            | 0x200B..=0x200F
            | 0x2028..=0x202E
            | 0x2060..=0x2064
            | 0x2066..=0x206F
            | 0xFEFF
            | 0xFFF9..=0xFFFB
            | 0x110BD
            | 0x1BCA0..=0x1BCA3
            | 0x1D173..=0x1D17A
            | 0xE0001
            | 0xE0020..=0xE007F
    );
    let private = matches!(n, 0xE000..=0xF8FF | 0xF0000..=0xFFFFD | 0x100000..=0x10FFFD);
    !format && !private
}

/// The text of a value as `str()` gives it in the first version of the scripts: a string as it
/// is, `None` for null, `True` and `False`, and the list form for a list.
pub fn py_str(value: &Value) -> String {
    match value {
        Value::Null => "None".into(),
        Value::Bool(true) => "True".into(),
        Value::Bool(false) => "False".into(),
        Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                i.to_string()
            } else if let Some(u) = n.as_u64() {
                u.to_string()
            } else {
                py_float(n.as_f64().unwrap_or(0.0))
            }
        }
        Value::String(s) => s.clone(),
        Value::Array(items) => format!(
            "[{}]",
            items.iter().map(py_repr).collect::<Vec<_>>().join(", ")
        ),
        Value::Object(map) => format!(
            "{{{}}}",
            map.iter()
                .map(|(k, v)| format!("{}: {}", py_repr_str(k), py_repr(v)))
                .collect::<Vec<_>>()
                .join(", ")
        ),
    }
}

/// `str(value or "")` of the first version of the scripts: the text of a truthy value, or "".
pub fn text_of(value: Option<&Value>) -> String {
    truthy(value).map(py_str).unwrap_or_default()
}

/// The text of a value of an optional field: [`py_str`] of the value, or `None`.
pub fn py_opt(value: Option<&Value>) -> String {
    py_str(value.unwrap_or(&Value::Null))
}

fn py_float(f: f64) -> String {
    if f.is_finite() && f.fract() == 0.0 && f.abs() < 1e16 {
        format!("{f:.1}")
    } else {
        format!("{f}")
    }
}

/// The quoted form of a value, as in a list: a string in quotes, each other value as [`py_str`].
pub fn py_repr(value: &Value) -> String {
    match value {
        Value::String(s) => py_repr_str(s),
        other => py_str(other),
    }
}

/// A string in quotes, with the escapes of the first version of the scripts, such as `'main'`.
pub fn py_repr_str(text: &str) -> String {
    let quote = if text.contains('\'') && !text.contains('"') {
        '"'
    } else {
        '\''
    };
    let mut out = String::with_capacity(text.len() + 2);
    out.push(quote);
    for c in text.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c == quote => {
                out.push('\\');
                out.push(c);
            }
            c if !printable(c) => {
                let n = u32::from(c);
                if n < 0x100 {
                    out.push_str(&format!("\\x{n:02x}"));
                } else if n < 0x10000 {
                    out.push_str(&format!("\\u{n:04x}"));
                } else {
                    out.push_str(&format!("\\U{n:08x}"));
                }
            }
            c => out.push(c),
        }
    }
    out.push(quote);
    out
}

/// A list of numbers in the list form, such as `[10, 21]`.
pub fn py_list(items: &[i64]) -> String {
    format!(
        "[{}]",
        items
            .iter()
            .map(i64::to_string)
            .collect::<Vec<_>>()
            .join(", ")
    )
}

/// The lines of a text at each kind of line end, with no empty line after a last line end.
pub fn splitlines(text: &str) -> Vec<&str> {
    let mut lines = Vec::new();
    let mut start = 0;
    let mut chars = text.char_indices().peekable();
    while let Some((i, c)) = chars.next() {
        let end = matches!(
            c,
            '\n' | '\r'
                | '\u{0b}'
                | '\u{0c}'
                | '\u{1c}'
                | '\u{1d}'
                | '\u{1e}'
                | '\u{85}'
                | '\u{2028}'
                | '\u{2029}'
        );
        if !end {
            continue;
        }
        lines.push(&text[start..i]);
        start = i + c.len_utf8();
        if c == '\r' {
            if let Some(&(j, '\n')) = chars.peek() {
                chars.next();
                start = j + 1;
            }
        }
    }
    if start < text.len() {
        lines.push(&text[start..]);
    }
    lines
}

/// The first 12 characters of a commit, or `-`.
pub fn short(commit: &str) -> String {
    let commit = if commit.is_empty() { "-" } else { commit };
    commit.chars().take(12).collect()
}

/// [`short`] of an optional commit.
pub fn short_opt(commit: Option<&str>) -> String {
    short(commit.unwrap_or(""))
}

/// The time now as RFC 3339 UTC text, such as `2026-11-16T14:30:00Z`.
pub fn now() -> String {
    crate::util::time::now_rfc3339()
}

/// The time now in the compact form, such as `20261116T143000Z`.
pub fn stamp() -> String {
    crate::util::time::compact(crate::util::time::now())
}

/// Seconds as a whole number for a text, as `%d` gives them: the part after the point is cut.
pub fn whole(seconds: f64) -> i64 {
    seconds.trunc() as i64
}

// Patterns --------------------------------------------------------------------------------------

/// The text that a pattern with `$` at its end tests: one line end at the end does not count.
pub fn line_end(text: &str) -> &str {
    text.strip_suffix('\n').unwrap_or(text)
}

/// A Jira key such as `DEMO-12`: a capital letter, up to 31 capitals, digits, or `_`, a dash,
/// and a number with no leading zero of up to 10 digits.
pub fn is_key(text: &str) -> bool {
    let text = line_end(text);
    let Some((project, number)) = text.rsplit_once('-') else {
        return false;
    };
    let mut chars = project.chars();
    let first_ok = chars.next().is_some_and(|c| c.is_ascii_uppercase());
    first_ok
        && project.len() <= 32
        && chars.all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_')
        && (1..=10).contains(&number.len())
        && number.bytes().all(|b| b.is_ascii_digit())
        && !number.starts_with('0')
}

/// A commit id: 40 or 64 lowercase hex characters.
pub fn is_commit(text: &str) -> bool {
    let text = line_end(text);
    (text.len() == 40 || text.len() == 64)
        && text
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// A text value of a call field that must be a commit.
pub fn commit_of(value: Option<&Value>) -> Option<&str> {
    value.and_then(Value::as_str).filter(|s| is_commit(s))
}

// The call --------------------------------------------------------------------------------------

/// A field of the call as text: the string, or "" for a missing value, null, "", or false.
pub fn text_field<'a>(call: &'a Map<String, Value>, key: &str) -> &'a str {
    call.get(key).and_then(Value::as_str).unwrap_or("")
}

/// True when a value counts as false: missing, null, false, 0, "", [], or {}.
pub fn falsy(value: Option<&Value>) -> bool {
    match value {
        None | Some(Value::Null) => true,
        Some(Value::Bool(b)) => !b,
        Some(Value::Number(n)) => n.as_f64() == Some(0.0),
        Some(Value::String(s)) => s.is_empty(),
        Some(Value::Array(a)) => a.is_empty(),
        Some(Value::Object(o)) => o.is_empty(),
    }
}

/// The value, or `None` when it counts as false (see [`falsy`]).
pub fn truthy(value: Option<&Value>) -> Option<&Value> {
    if falsy(value) {
        None
    } else {
        value
    }
}

/// `args` of the call: its object, or an empty one.
pub fn args_of(call: &Map<String, Value>) -> Map<String, Value> {
    match truthy(call.get("args")) {
        Some(Value::Object(map)) => map.clone(),
        _ => Map::new(),
    }
}

/// A JSON object from pairs, in one expression.
pub fn object(pairs: Vec<(&str, Value)>) -> Value {
    Value::Object(pairs.into_iter().map(|(k, v)| (k.to_string(), v)).collect())
}

// Configuration ---------------------------------------------------------------------------------

/// The [adapter] table of the configuration, with the defaults of adapter.toml, and `base_ref`
/// of the call: the `base` of adapter.toml, such as origin/main.
#[derive(Clone, Debug, Default)]
pub struct Config {
    pub table: Map<String, Value>,
    pub base_ref: Option<String>,
}

/// One staging env of the key `envs`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Env {
    pub name: String,
    pub branch: String,
    /// The GitHub environment whose deployments tell which commit the env runs.
    pub deployment: Option<String>,
}

impl Config {
    pub fn of(call: &Map<String, Value>) -> Config {
        Config {
            table: match call.get("config") {
                Some(Value::Object(map)) => map.clone(),
                _ => Map::new(),
            },
            base_ref: call
                .get("base_ref")
                .and_then(Value::as_str)
                .map(str::to_string),
        }
    }

    /// The value of a key, or `None` for a missing key, null, "", [], or {}.
    pub fn get(&self, key: &str) -> Option<&Value> {
        match self.table.get(key) {
            None | Some(Value::Null) => None,
            Some(Value::String(s)) if s.is_empty() => None,
            Some(Value::Array(a)) if a.is_empty() => None,
            Some(Value::Object(o)) if o.is_empty() => None,
            Some(value) => Some(value),
        }
    }

    pub fn need(&self, key: &str, why: &str) -> Res<&Value> {
        match self.get(key) {
            Some(value) => Ok(value),
            None => fail(format!(
                "{why} needs the key `{key}` in the [adapter] table of config.toml"
            )),
        }
    }

    fn as_text(key: &str, value: &Value) -> Res<String> {
        match value {
            Value::String(s) => Ok(s.clone()),
            _ => fail(format!("`{key}` in the [adapter] table must be a text")),
        }
    }

    /// A text key that the call needs.
    pub fn text_need(&self, key: &str, why: &str) -> Res<String> {
        Config::as_text(key, self.need(key, why)?)
    }

    /// A text key, or `None`.
    pub fn text(&self, key: &str) -> Res<Option<String>> {
        self.get(key).map(|v| Config::as_text(key, v)).transpose()
    }

    /// A text key, or its default.
    pub fn text_or(&self, key: &str, default: &str) -> Res<String> {
        Ok(self.text(key)?.unwrap_or_else(|| default.to_string()))
    }

    /// A number above 0, or its default.
    pub fn number(&self, key: &str, default: f64) -> Res<f64> {
        let value = match self.get(key) {
            None => return Ok(default),
            Some(value) => value,
        };
        match value.as_f64() {
            Some(n) if value.is_number() && n > 0.0 => Ok(n),
            _ => fail(format!(
                "`{key}` in the [adapter] table must be a number above 0"
            )),
        }
    }

    /// A key that is true or false.
    pub fn flag(&self, key: &str, default: bool) -> Res<bool> {
        match self.table.get(key) {
            None | Some(Value::Null) => Ok(default),
            Some(Value::Bool(b)) => Ok(*b),
            Some(_) => fail(format!(
                "`{key}` in the [adapter] table must be true or false"
            )),
        }
    }

    /// A list of GitHub logins, lowercase.
    pub fn logins(&self, key: &str) -> Res<Vec<String>> {
        let items: Vec<Value> = match self.get(key) {
            None => Vec::new(),
            Some(Value::String(s)) => vec![Value::String(s.clone())],
            Some(Value::Array(items)) => items.clone(),
            Some(_) => {
                return fail(format!(
                    "`{key}` in the [adapter] table must be a list of GitHub logins"
                ))
            }
        };
        let mut out = Vec::new();
        for item in &items {
            let Some(text) = item.as_str() else {
                return fail(format!(
                    "`{key}` in the [adapter] table must be a list of GitHub logins"
                ));
            };
            if !text.trim().is_empty() {
                out.push(text.trim().to_lowercase());
            }
        }
        Ok(out)
    }

    pub fn repo(&self, why: &str) -> Res<String> {
        let repo = self.text_need("github_repo", why)?;
        let part = |p: &str| {
            !p.is_empty()
                && p.chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '-'))
        };
        let valid = line_end(&repo)
            .split_once('/')
            .is_some_and(|(owner, name)| part(owner) && part(name));
        if !valid {
            return fail("`github_repo` must be owner/name, such as acme/widgets");
        }
        Ok(repo)
    }

    /// The branch that pull requests go into. `base_ref` of the call gives it: the text after
    /// its first `/`, such as main of origin/main. The key `base_branch` of the [adapter] table
    /// overrides it, for a project whose remote branch has a different name. With neither, it
    /// is main.
    pub fn base_branch(&self) -> Res<String> {
        if let Some(name) = self.text("base_branch")? {
            return Ok(name);
        }
        let reference = self.base_ref.as_deref().unwrap_or("").trim();
        if reference.is_empty() {
            return Ok("main".into());
        }
        Ok(match reference.split_once('/') {
            Some((_, rest)) => rest.to_string(),
            None => reference.to_string(),
        })
    }

    pub fn http_timeout(&self) -> Res<f64> {
        self.number("http_timeout", DEFAULT_HTTP_TIMEOUT)
    }

    /// The staging envs: a list of tables with `name` and `branch`, and the optional
    /// `deployment`: the name of a GitHub environment whose deployments tell which commit the
    /// env runs.
    pub fn envs(&self) -> Res<Vec<Env>> {
        let items: Vec<Value> =
            match self.get("envs") {
                None => Vec::new(),
                Some(Value::Object(map)) => map
                    .iter()
                    .filter_map(|(name, value)| match value {
                        Value::Object(table) => {
                            let mut table = table.clone();
                            table.insert("name".into(), Value::String(name.clone()));
                            Some(Value::Object(table))
                        }
                        _ => None,
                    })
                    .collect(),
                Some(Value::Array(items)) => items.clone(),
                Some(_) => return fail(
                    "`envs` in the [adapter] table must be a list of tables with name and branch",
                ),
            };
        let mut out = Vec::new();
        for item in &items {
            let (Some(name), Some(branch)) = (
                item.get("name").and_then(Value::as_str),
                item.get("branch").and_then(Value::as_str),
            ) else {
                return fail(
                    "each item of `envs` needs `name` and `branch`, such as { name = \"staging\", branch = \"staging\" }",
                );
            };
            let deployment = match item.get("deployment") {
                None | Some(Value::Null) => None,
                Some(Value::String(d)) if !d.trim().is_empty() => Some(d.trim().to_string()),
                Some(_) => {
                    return fail(format!(
                        "`deployment` of the env {} must be the name of a GitHub environment, such as \"staging\"",
                        clean(name, 40)
                    ))
                }
            };
            out.push(Env {
                name: name.to_string(),
                branch: branch.to_string(),
                deployment,
            });
        }
        Ok(out)
    }
}

/// The value of a secret of the environment, or a failure that names it.
pub fn secret(name: &str, why: &str) -> Res<String> {
    match std::env::var(name) {
        Ok(value) if !value.is_empty() => Ok(value),
        _ => fail(format!(
            "{why} needs the secret {name}. Add it to [secrets] of config.toml, such as {name} = {{ env = \"...\" }}"
        )),
    }
}

/// The value of an optional secret, or `None`.
pub fn optional_secret(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|v| !v.is_empty())
}

/// The plain environment: only the names of [`PLAIN_ENV`] that this process has.
pub fn plain_env() -> Vec<(String, String)> {
    PLAIN_ENV
        .iter()
        .filter_map(|name| std::env::var(name).ok().map(|v| (name.to_string(), v)))
        .collect()
}

/// The ticket key of the call, or a failure.
pub fn check_key(key: Option<&Value>, what: &str) -> Res<String> {
    match key {
        Some(Value::String(text)) if is_key(text) => Ok(text.clone()),
        other => fail(format!(
            "{what} must be a Jira key such as DEMO-12, not {}",
            py_repr_str(&clean(&py_opt(other), 40))
        )),
    }
}

/// JSON with an indent of two spaces, and each character above ASCII as `\uXXXX`, as the
/// documents of the first version of the scripts have it.
pub fn pretty(value: &Value) -> String {
    let text = serde_json::to_string_pretty(value).unwrap_or_default();
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        if c.is_ascii() {
            out.push(c);
        } else {
            let mut units = [0u16; 2];
            for unit in c.encode_utf16(&mut units) {
                out.push_str(&format!("\\u{unit:04x}"));
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_and_commits() {
        for key in ["DEMO-12", "A-1", "AB_2-1234567890", "DEMO-12\n"] {
            assert!(is_key(key), "{key}");
        }
        for key in [
            "demo-12",
            "DEMO-012",
            "DEMO-1/../x",
            "DEMO",
            "-1",
            "DEMO-12345678901",
            "1A-2",
        ] {
            assert!(!is_key(key), "{key}");
        }
        assert!(is_commit(&"a".repeat(40)));
        assert!(is_commit(&"0".repeat(64)));
        assert!(!is_commit(&"A".repeat(40)));
        assert!(!is_commit(&"a".repeat(41)));
    }

    #[test]
    fn texts_as_the_first_version_gives_them() {
        assert_eq!(clean("a\tb\n\nc  d", 300), "a b c d");
        assert_eq!(clean("abcdef", 3), "abc [cut]");
        assert_eq!(py_repr_str("main"), "'main'");
        assert_eq!(py_repr_str("it's"), "\"it's\"");
        assert_eq!(py_str(&Value::Null), "None");
        assert_eq!(py_str(&json!([1, "a"])), "[1, 'a']");
        assert_eq!(py_str(&json!(0.5)), "0.5");
        assert_eq!(py_list(&[10, 21]), "[10, 21]");
        assert_eq!(short(""), "-");
        assert_eq!(short(&"a".repeat(40)), "a".repeat(12));
        assert_eq!(
            check_key(None, "the ticket key").unwrap_err().to_string(),
            "the ticket key must be a Jira key such as DEMO-12, not 'None'"
        );
    }

    #[test]
    fn the_configuration_keys() {
        let config = Config {
            table: json!({"a": "", "n": 0.5, "bad": "x", "flag": "yes", "logins": [" Lead ", ""],
                          "envs": {"staging": {"branch": "st"}}})
            .as_object()
            .unwrap()
            .clone(),
            base_ref: Some("origin/release/2.0".into()),
        };
        assert_eq!(config.get("a"), None);
        assert_eq!(config.number("n", 1.0).unwrap(), 0.5);
        assert_eq!(config.number("a", 7.0).unwrap(), 7.0);
        assert!(config.number("bad", 1.0).is_err());
        assert!(config.flag("flag", true).is_err());
        assert_eq!(config.logins("logins").unwrap(), ["lead"]);
        assert_eq!(config.base_branch().unwrap(), "release/2.0");
        assert_eq!(
            config.envs().unwrap(),
            [Env {
                name: "staging".into(),
                branch: "st".into(),
                deployment: None
            }]
        );
    }
}
