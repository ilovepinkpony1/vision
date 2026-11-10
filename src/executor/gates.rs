//! The gates (section 12): pure functions that test what leaves the machine before a verb with
//! `leaves` runs. The executor gives them the diff between the base and the last commit of the
//! clone, and each text of the call, such as the title and the body of a pull request.
//!
//! - [`secrets_gate`] refuses the value of a secret of `[secrets]`, a line that looks like a
//!   private key, and the common forms of access keys and tokens. Its error names the file and
//!   the line, never the value. It also reads the patch of each commit, because a push sends
//!   each commit: a secret that one commit adds and a later commit removes still leaves.
//! - [`size_gate`] refuses a diff with more changed lines than `gates.max_diff_lines`.
//! - [`sensitive_gate`] gives the paths of `gates.sensitive` that the diff touches and that the
//!   approved plan does not name. The executor holds such a diff for the person.

use std::collections::BTreeSet;
use std::fmt;

/// The secrets shorter than this are not tested, because they would match almost any text.
pub const MIN_SECRET_LEN: usize = 4;

/// The findings that one error names before it says "and N more".
const MAX_FINDINGS: usize = 10;

/// The three gates.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Gate {
    Secrets,
    Size,
    Sensitive,
}

impl Gate {
    pub fn as_str(self) -> &'static str {
        match self {
            Gate::Secrets => "secrets",
            Gate::Size => "size",
            Gate::Sensitive => "sensitive",
        }
    }
}

impl fmt::Display for Gate {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// The change of one file in a diff.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FileDiff {
    /// The path after the change. For a deleted file: the path before.
    pub path: String,
    /// The path before a rename.
    pub old_path: Option<String>,
    /// Each added line with its number in the new file.
    pub added: Vec<(usize, String)>,
    /// The number of removed lines.
    pub removed: usize,
    /// True for a binary file. The size gate does not count its lines.
    pub binary: bool,
    /// The commit of this change, in the patches of `git log`. `None` in a plain diff.
    pub commit: Option<String>,
}

/// One text that leaves the machine, such as the body of a pull request.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Text<'a> {
    /// What the text is, for the error, such as `args.body`.
    pub source: &'a str,
    pub text: &'a str,
}

/// Reads the output of `git diff` (with the prefixes `a/` and `b/`) into one entry per file.
/// It also reads the patches of `git log --format='commit %H' --patch`: each file after a line
/// `commit <hash>` gets that commit. The diff must be made with `--text`, so that an attribute
/// of the repo cannot hide a file. A file with a NUL character in a changed line is binary.
pub fn parse_diff(text: &str) -> Vec<FileDiff> {
    let mut files: Vec<FileDiff> = Vec::new();
    let mut in_hunk = false;
    let mut line_no = 0usize;
    let mut commit: Option<String> = None;
    for line in text.lines() {
        // A line of a hunk starts with '+', '-', ' ', '\\', or '@', so this is never content.
        if let Some(hash) = commit_line(line) {
            commit = Some(hash.to_string());
            in_hunk = false;
            continue;
        }
        if let Some(rest) = line.strip_prefix("diff --git ") {
            files.push(FileDiff {
                path: same_path(rest).unwrap_or_default(),
                commit: commit.clone(),
                ..FileDiff::default()
            });
            in_hunk = false;
            continue;
        }
        let Some(file) = files.last_mut() else {
            continue;
        };
        if in_hunk {
            match line.as_bytes().first() {
                Some(b'+') => {
                    file.binary |= line.contains('\0');
                    file.added.push((line_no, line[1..].to_string()));
                    line_no += 1;
                }
                Some(b'-') => {
                    file.binary |= line.contains('\0');
                    file.removed += 1;
                }
                Some(b' ') => line_no += 1,
                Some(b'@') => line_no = hunk_start(line).unwrap_or(line_no),
                _ => {}
            }
            continue;
        }
        if line.starts_with("@@") {
            in_hunk = true;
            line_no = hunk_start(line).unwrap_or(1);
        } else if let Some(path) = line.strip_prefix("rename from ") {
            file.old_path = Some(unquote(path));
        } else if let Some(path) = line.strip_prefix("rename to ") {
            file.path = unquote(path);
        } else if let Some(path) = line.strip_prefix("+++ ") {
            if let Some(path) = strip_side(path, "b/") {
                file.path = path;
            }
        } else if let Some(path) = line.strip_prefix("--- ") {
            if let Some(path) = strip_side(path, "a/") {
                if file.path.is_empty() {
                    file.path = path;
                }
            }
        } else if line.starts_with("Binary files ") || line == "GIT binary patch" {
            file.binary = true;
        }
    }
    files
}

/// The hash of a line `commit <hash>`, which starts the patch of one commit.
fn commit_line(line: &str) -> Option<&str> {
    let hash = line.strip_prefix("commit ")?;
    (hash.len() >= 40 && hash.bytes().all(|b| b.is_ascii_hexdigit())).then_some(hash)
}

/// The number of changed lines: each added line and each removed line of a file that is not
/// binary.
pub fn changed_lines(files: &[FileDiff]) -> u64 {
    files
        .iter()
        .filter(|f| !f.binary)
        .map(|f| (f.added.len() + f.removed) as u64)
        .sum()
}

/// Refuses a diff or a text that holds the value of a secret, a private key, or a common form
/// of an access key or a token. `secrets` gives each secret as (name, value). The error names
/// the secret, the file or the text, and the line, never the value.
///
/// `files` can hold the diff and the patches of the commits. A line of the diff is named one
/// time, with no commit. A line that only the patch of a commit has is named with its commit:
/// a later commit removed it, but the push still sends it.
pub fn secrets_gate(
    files: &[FileDiff],
    texts: &[Text<'_>],
    secrets: &[(&str, &str)],
) -> Result<(), String> {
    let mut findings = Vec::new();
    // The lines with a finding, by file. The patch of a commit repeats each line of the diff,
    // and two commits can hold the same line, so a line of the history is named one time.
    let mut seen: BTreeSet<(&str, &str)> = BTreeSet::new();
    for file in files.iter().filter(|f| f.commit.is_none()) {
        for (line_no, line) in &file.added {
            if let Some(what) = finding(line, secrets) {
                seen.insert((file.path.as_str(), line.as_str()));
                findings.push(format!("{what} in {} line {line_no}", file.path));
            }
        }
    }
    for file in files.iter().filter(|f| f.commit.is_some()) {
        let commit = file.commit.as_deref().unwrap_or_default();
        for (line_no, line) in &file.added {
            if seen.contains(&(file.path.as_str(), line.as_str())) {
                continue;
            }
            if let Some(what) = finding(line, secrets) {
                seen.insert((file.path.as_str(), line.as_str()));
                findings.push(format!(
                    "{what} in {} line {line_no} of the commit {}",
                    file.path,
                    commit.get(..12).unwrap_or(commit)
                ));
            }
        }
    }
    for text in texts {
        for (index, line) in text.text.lines().enumerate() {
            if let Some(what) = finding(line, secrets) {
                findings.push(format!("{what} in {} line {}", text.source, index + 1));
            }
        }
    }
    if findings.is_empty() {
        return Ok(());
    }
    let more = findings.len().saturating_sub(MAX_FINDINGS);
    findings.truncate(MAX_FINDINGS);
    let mut list = findings.join("; ");
    if more > 0 {
        list.push_str(&format!("; and {more} more"));
    }
    Err(format!(
        "what leaves the machine holds a secret: {list}. Remove it from the commits or the text (rewrite the commits that add it), then try again"
    ))
}

/// What a line holds that must not leave, if anything.
fn finding(line: &str, secrets: &[(&str, &str)]) -> Option<String> {
    for (name, value) in secrets {
        if value.len() >= MIN_SECRET_LEN && line.contains(value) {
            return Some(format!("the value of the secret {name}"));
        }
    }
    token_form(line).map(|what| format!("a line that looks like {what}"))
}

/// The kind of secret that a line looks like: a private key, an access key, or a token.
pub fn token_form(line: &str) -> Option<&'static str> {
    if line.contains("-----BEGIN") && line.contains("PRIVATE KEY") {
        return Some("a private key");
    }
    for form in FORMS {
        if has_form(line, form) {
            return Some(form.what);
        }
    }
    None
}

/// One common form of an access key or a token: a fixed prefix and a body of some characters.
struct Form {
    prefix: &'static str,
    /// The shortest body after the prefix.
    min: usize,
    /// The longest body, or `None` for no limit.
    max: Option<usize>,
    class: fn(u8) -> bool,
    what: &'static str,
}

fn upper_digit(b: u8) -> bool {
    b.is_ascii_uppercase() || b.is_ascii_digit()
}

fn alnum(b: u8) -> bool {
    b.is_ascii_alphanumeric()
}

fn word(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_' || b == b'-'
}

const fn form(
    prefix: &'static str,
    min: usize,
    max: Option<usize>,
    class: fn(u8) -> bool,
    what: &'static str,
) -> Form {
    Form {
        prefix,
        min,
        max,
        class,
        what,
    }
}

/// The forms of common access key ids, personal tokens, bot tokens, and live API keys.
const FORMS: &[Form] = &[
    form("AKIA", 16, Some(16), upper_digit, "an access key id"),
    form("ASIA", 16, Some(16), upper_digit, "an access key id"),
    form("ghp_", 36, None, alnum, "a personal access token"),
    form("gho_", 36, None, alnum, "an access token"),
    form("ghu_", 36, None, alnum, "an access token"),
    form("ghs_", 36, None, alnum, "an access token"),
    form("ghr_", 36, None, alnum, "a refresh token"),
    form("glpat-", 20, None, word, "a personal access token"),
    form("xoxb-", 10, None, word, "a bot token"),
    form("xoxp-", 10, None, word, "a user token"),
    form("xoxa-", 10, None, word, "an app token"),
    form("xoxr-", 10, None, word, "a refresh token"),
    form("xoxs-", 10, None, word, "a session token"),
    form("AIza", 35, None, word, "an API key"),
    form("sk_live_", 24, None, alnum, "a live API key"),
    form("rk_live_", 24, None, alnum, "a live API key"),
];

fn has_form(line: &str, form: &Form) -> bool {
    let bytes = line.as_bytes();
    for (start, _) in line.match_indices(form.prefix) {
        if start > 0 && bytes[start - 1].is_ascii_alphanumeric() {
            continue;
        }
        let body = &bytes[start + form.prefix.len()..];
        let len = body.iter().take_while(|b| (form.class)(**b)).count();
        let fits = len >= form.min && form.max.is_none_or(|max| len == max);
        let ends = body.get(len).is_none_or(|b| !b.is_ascii_alphanumeric());
        if fits && ends {
            return true;
        }
    }
    false
}

/// Refuses a diff with more changed lines than `max`.
pub fn size_gate(files: &[FileDiff], max: u64) -> Result<(), String> {
    let changed = changed_lines(files);
    if changed > max {
        return Err(format!(
            "the diff changes {changed} lines, more than gates.max_diff_lines ({max}). Split the change into smaller tickets, or raise gates.max_diff_lines in config.toml"
        ));
    }
    Ok(())
}

/// The paths of the diff that touch an entry of `sensitive` and that the approved plan does not
/// name, sorted. The plan names a path when it holds the path of the file or the entry itself,
/// such as `.github/`. With no approved plan, each sensitive path is held.
pub fn sensitive_gate(files: &[FileDiff], sensitive: &[String], plan: Option<&str>) -> Vec<String> {
    let mut held = Vec::new();
    for file in files {
        let paths = std::iter::once(file.path.as_str()).chain(file.old_path.as_deref());
        for path in paths {
            for entry in sensitive {
                let entry = entry.trim();
                if !touches(path, entry) {
                    continue;
                }
                let named = plan.is_some_and(|plan| plan.contains(path) || plan.contains(entry));
                if !named && !held.iter().any(|p| p == path) {
                    held.push(path.to_string());
                }
            }
        }
    }
    held.sort();
    held
}

/// True when `path` is the entry, or is in the folder that the entry names.
fn touches(path: &str, entry: &str) -> bool {
    if entry.is_empty() || path.is_empty() {
        return false;
    }
    if entry.ends_with('/') {
        return path.starts_with(entry);
    }
    path == entry || path.starts_with(&format!("{entry}/"))
}

/// The path of `a/P b/P`, the first line of a file in a diff, when both sides are the same.
fn same_path(rest: &str) -> Option<String> {
    let len = rest.len();
    if len < 5 || !(len - 5).is_multiple_of(2) {
        return None;
    }
    let half = (len - 5) / 2;
    let left = rest.get(..half + 2)?.strip_prefix("a/")?;
    let right = rest.get(half + 2..)?.strip_prefix(" b/")?;
    (left == right).then(|| left.to_string())
}

/// The number of the first new line of a hunk: `@@ -1,3 +4,5 @@` gives 4.
fn hunk_start(line: &str) -> Option<usize> {
    let plus = line.split_whitespace().find(|w| w.starts_with('+'))?;
    let number = plus[1..].split(',').next()?;
    number.parse().ok()
}

fn strip_side(path: &str, side: &str) -> Option<String> {
    let path = unquote(path.trim_end_matches('\t'));
    path.strip_prefix(side).map(str::to_string)
}

/// A path that git put in quotes because of a special character.
fn unquote(path: &str) -> String {
    let Some(inner) = path.strip_prefix('"').and_then(|p| p.strip_suffix('"')) else {
        return path.to_string();
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

#[cfg(test)]
mod tests {
    use super::*;

    const DIFF: &str = "\
diff --git a/src/app.py b/src/app.py
index 1111111..2222222 100644
--- a/src/app.py
+++ b/src/app.py
@@ -1,0 +2,2 @@ def main():
+TOKEN = \"s3cr3t-value\"
+print(TOKEN)
@@ -10 +12 @@
-old = 1
+new = 2
diff --git a/.github/workflows/ci.yml b/.github/workflows/ci.yml
new file mode 100644
--- /dev/null
+++ b/.github/workflows/ci.yml
@@ -0,0 +1 @@
+on: push
diff --git a/old name.txt b/new name.txt
similarity index 90%
rename from old name.txt
rename to new name.txt
diff --git a/gone.txt b/gone.txt
deleted file mode 100644
--- a/gone.txt
+++ /dev/null
@@ -1,2 +0,0 @@
-a
-b
diff --git a/logo.png b/logo.png
Binary files a/logo.png and b/logo.png differ
";

    #[test]
    fn the_diff_parser_finds_paths_lines_and_numbers() {
        let files = parse_diff(DIFF);
        let paths: Vec<&str> = files.iter().map(|f| f.path.as_str()).collect();
        assert_eq!(
            paths,
            [
                "src/app.py",
                ".github/workflows/ci.yml",
                "new name.txt",
                "gone.txt",
                "logo.png"
            ]
        );
        assert_eq!(
            files[0].added,
            vec![
                (2, "TOKEN = \"s3cr3t-value\"".to_string()),
                (3, "print(TOKEN)".to_string()),
                (12, "new = 2".to_string())
            ]
        );
        assert_eq!(files[0].removed, 1);
        assert_eq!(files[2].old_path.as_deref(), Some("old name.txt"));
        assert_eq!(files[3].removed, 2);
        assert!(files[4].binary);
        assert_eq!(changed_lines(&files), 7);
    }

    #[test]
    fn the_secrets_gate_names_the_file_and_the_line_never_the_value() {
        let files = parse_diff(DIFF);
        let secrets = [("FORGE_TOKEN", "s3cr3t-value")];
        let err = secrets_gate(&files, &[], &secrets).unwrap_err();
        assert!(
            err.contains("the value of the secret FORGE_TOKEN in src/app.py line 2"),
            "{err}"
        );
        assert!(!err.contains("s3cr3t-value"), "{err}");
        assert!(secrets_gate(&files, &[], &[("OTHER", "not-in-the-diff")]).is_ok());
        assert!(
            secrets_gate(&files, &[], &[("SHORT", "TOK")]).is_ok(),
            "too short to test"
        );
    }

    #[test]
    fn a_secret_that_a_later_commit_removed_is_named_with_its_commit() {
        let first = "1".repeat(40);
        let second = "2".repeat(40);
        let history = format!(
            "commit {first}\n\ndiff --git a/deploy.cfg b/deploy.cfg\nnew file mode 100644\n--- /dev/null\n+++ b/deploy.cfg\n@@ -0,0 +1,2 @@\n+user = demo\n+token = s3cr3t-value\n\
             commit {second}\n\ndiff --git a/deploy.cfg b/deploy.cfg\n--- a/deploy.cfg\n+++ b/deploy.cfg\n@@ -2 +1,0 @@\n-token = s3cr3t-value\n"
        );
        let files = parse_diff(&history);
        assert_eq!(files.len(), 2);
        assert_eq!(files[0].commit.as_deref(), Some(first.as_str()));
        assert_eq!(files[1].commit.as_deref(), Some(second.as_str()));
        assert_eq!(files[1].removed, 1);
        let secrets = [("FORGE_TOKEN", "s3cr3t-value")];
        let err = secrets_gate(&files, &[], &secrets).unwrap_err();
        assert!(
            err.contains(
                "the value of the secret FORGE_TOKEN in deploy.cfg line 2 of the commit 111111111111"
            ),
            "{err}"
        );
        // A line that the diff also has is named one time, with no commit.
        let mut both = parse_diff(DIFF);
        let mut again = parse_diff(DIFF);
        for file in &mut again {
            file.commit = Some(first.clone());
        }
        both.extend(again);
        let err = secrets_gate(&both, &[], &secrets).unwrap_err();
        assert_eq!(err.matches("FORGE_TOKEN").count(), 1, "{err}");
        assert!(!err.contains("of the commit"), "{err}");
    }

    #[test]
    fn a_file_with_a_nul_character_is_binary_and_the_size_gate_skips_it() {
        let diff = "diff --git a/logo.bin b/logo.bin\nnew file mode 100644\n--- /dev/null\n+++ b/logo.bin\n@@ -0,0 +1,3 @@\n+\u{0}\u{1}abc\n+token = s3cr3t-value\n+x\n\
                    diff --git a/a.txt b/a.txt\n--- a/a.txt\n+++ b/a.txt\n@@ -1 +1 @@\n-a\n+b\n";
        let files = parse_diff(diff);
        assert!(files[0].binary);
        assert!(!files[1].binary);
        assert_eq!(changed_lines(&files), 2, "only the text file counts");
        // The secrets gate still reads the lines of a binary file.
        let err = secrets_gate(&files, &[], &[("FORGE_TOKEN", "s3cr3t-value")]).unwrap_err();
        assert!(err.contains("logo.bin line 2"), "{err}");
    }

    #[test]
    fn the_secrets_gate_reads_the_texts_too() {
        let body = "Fixes the export.\nThe key is s3cr3t-value, keep it safe.";
        let texts = [Text {
            source: "args.body",
            text: body,
        }];
        let err = secrets_gate(&[], &texts, &[("FORGE_TOKEN", "s3cr3t-value")]).unwrap_err();
        assert!(err.contains("FORGE_TOKEN in args.body line 2"), "{err}");
    }

    #[test]
    fn private_keys_and_token_forms_are_refused() {
        let key = format!("{}{}", "-----BEGIN OPENSSH ", "PRIVATE KEY-----");
        assert_eq!(token_form(&key), Some("a private key"));
        let access = format!("access_key = {}{}", "AKIA", "ABCDEFGHIJKLMNOP");
        assert_eq!(token_form(&access), Some("an access key id"));
        let personal = format!("token: {}{}", "ghp_", "a".repeat(36));
        assert_eq!(token_form(&personal), Some("a personal access token"));
        let bot = format!("{}{}", "xoxb-", "1234567890-abcdef");
        assert_eq!(token_form(&bot), Some("a bot token"));
        for fine in [
            "AKIA is a prefix",
            "XAKIAABCDEFGHIJKLMNOP",
            "ghp_short",
            "the value of sk_live_ is",
            "-----BEGIN PUBLIC KEY-----",
        ] {
            assert_eq!(token_form(fine), None, "{fine}");
        }
        let files = vec![FileDiff {
            path: "deploy/id".into(),
            added: vec![(1, key)],
            ..FileDiff::default()
        }];
        let err = secrets_gate(&files, &[], &[]).unwrap_err();
        assert!(
            err.contains("a line that looks like a private key in deploy/id line 1"),
            "{err}"
        );
    }

    #[test]
    fn many_findings_are_cut_short() {
        let files = vec![FileDiff {
            path: "keys.txt".into(),
            added: (1..=15)
                .map(|n| (n, "-----BEGIN RSA PRIVATE KEY-----".to_string()))
                .collect(),
            ..FileDiff::default()
        }];
        let err = secrets_gate(&files, &[], &[]).unwrap_err();
        assert!(err.contains("line 10; and 5 more"), "{err}");
    }

    #[test]
    fn the_size_gate_counts_added_and_removed_lines() {
        let files = parse_diff(DIFF);
        assert!(size_gate(&files, 7).is_ok());
        let err = size_gate(&files, 6).unwrap_err();
        assert!(
            err.contains("the diff changes 7 lines, more than gates.max_diff_lines (6)"),
            "{err}"
        );
    }

    #[test]
    fn the_sensitive_gate_holds_unless_the_plan_names_the_path() {
        let files = parse_diff(DIFF);
        let sensitive = vec![".github/".to_string(), "db/migrate/".to_string()];
        assert_eq!(
            sensitive_gate(&files, &sensitive, None),
            [".github/workflows/ci.yml"]
        );
        let plan = "Change the CI: .github/workflows/ci.yml runs on push.";
        assert!(sensitive_gate(&files, &sensitive, Some(plan)).is_empty());
        assert!(sensitive_gate(&files, &sensitive, Some("Touch .github/ too.")).is_empty());
        assert_eq!(
            sensitive_gate(&files, &sensitive, Some("Only the app.")),
            [".github/workflows/ci.yml"]
        );
        let exact = vec!["gone.txt".to_string(), "src".to_string()];
        assert_eq!(
            sensitive_gate(&files, &exact, None),
            ["gone.txt", "src/app.py"]
        );
        assert!(sensitive_gate(&files, &["sr".to_string()], None).is_empty());
    }

    #[test]
    fn quoted_paths_are_read() {
        assert_eq!(unquote("\"b/a\\tb\""), "b/a\tb");
        assert_eq!(same_path("a/x y b/x y").as_deref(), Some("x y"));
        assert_eq!(same_path("a/x b/y"), None);
        assert_eq!(hunk_start("@@ -1,3 +4,5 @@ fn"), Some(4));
    }
}
