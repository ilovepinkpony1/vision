//! The documents of the pipeline as pure functions: the documents of a result object, the
//! findings of a review document, the verdicts of review comments, and the arguments of an
//! entry that name a document.
//!
//! A real session writes looser text than a format asks for, so each reader makes one safe
//! repair where one exists, and each refusal says exactly what was wrong.

use std::collections::BTreeMap;

use serde_json::{Map, Value};

use crate::adapter::valid_document_name;
use crate::util::ids;

/// The longest document of a result that the pipeline writes.
pub const DOCUMENT_MAX: usize = 4 * 1024 * 1024;

/// The documents of a result object that the step expects, by name, with what was left out.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ResultDocs {
    pub docs: BTreeMap<String, String>,
    /// For the log: each document that was left out, with why.
    pub ignored: Vec<String>,
}

/// Reads `documents` of a result object. A name with a folder part, such as `.vision/plan.md`,
/// keeps only its file name when the step expects that name: the one safe repair. A name that
/// the step does not expect is left out and noted.
pub fn result_documents(result: &Value, expected: &[String]) -> ResultDocs {
    let mut out = ResultDocs::default();
    let items = result
        .get("documents")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    for item in items {
        let raw = item
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or("")
            .trim();
        let Some(content) = item.get("content").and_then(Value::as_str) else {
            out.ignored
                .push(format!("`{raw}`: the document has no text content"));
            continue;
        };
        let name = if expected.iter().any(|e| e == raw) {
            raw.to_string()
        } else {
            let file = raw.rsplit('/').next().unwrap_or(raw).to_string();
            if expected.contains(&file) {
                file
            } else {
                out.ignored.push(format!(
                    "`{raw}`: the step does not expect it. It expects {}",
                    if expected.is_empty() {
                        "no document".to_string()
                    } else {
                        expected.join(", ")
                    }
                ));
                continue;
            }
        };
        if !valid_document_name(&name) {
            out.ignored.push(format!("`{raw}`: not a plain file name"));
        } else if content.len() > DOCUMENT_MAX {
            out.ignored
                .push(format!("`{name}`: the document is larger than 4 MB"));
        } else {
            match out.docs.entry(name) {
                std::collections::btree_map::Entry::Occupied(e) => out.ignored.push(format!(
                    "`{}`: a second document with the same name",
                    e.key()
                )),
                std::collections::btree_map::Entry::Vacant(e) => {
                    e.insert(content.to_string());
                }
            }
        }
    }
    out
}

// ----- Findings --------------------------------------------------------------------------

/// The reason codes of a rejected finding (`agents/reviewer.md`).
pub const REASON_CODES: [&str; 7] = [
    "duplicate",
    "not_reproducible",
    "speculative",
    "outside_diff",
    "pre_existing",
    "style_only",
    "environment",
];

/// The words of a priority or a severity.
pub const PRIORITIES: [&str; 7] = [
    "critical", "major", "minor", "trivial", "high", "medium", "low",
];

/// One finding of a review document, or one bug of a QA document.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Finding {
    pub title: String,
    pub priority: Option<String>,
    /// `kept` or `rejected`.
    pub verdict: String,
    pub reason_code: Option<String>,
    pub fix_commit: Option<String>,
}

impl Finding {
    /// A short stable hash of the title, so that a finding that comes back is found again.
    pub fn fingerprint(&self) -> String {
        let words: Vec<String> = self
            .title
            .to_lowercase()
            .split(|c: char| !c.is_alphanumeric())
            .filter(|w| !w.is_empty())
            .map(str::to_string)
            .collect();
        ids::sha256_hex(words.join(" ").as_bytes())[..16].to_string()
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Section {
    Other,
    Findings,
    Open,
}

fn section_of(heading: &str) -> Section {
    let lower = heading.to_lowercase();
    if lower.contains("open") {
        Section::Open
    } else if ["finding", "bug", "issue"]
        .iter()
        .any(|w| lower.contains(w))
    {
        Section::Findings
    } else {
        Section::Other
    }
}

/// A heading line: its level and its text.
fn heading(line: &str) -> Option<(usize, &str)> {
    let level = line.chars().take_while(|c| *c == '#').count();
    if level == 0 || level > 6 {
        return None;
    }
    let rest = &line[level..];
    rest.starts_with(' ').then(|| (level, rest.trim()))
}

/// A list item at the start of a line: its text.
fn top_item(line: &str) -> Option<&str> {
    for mark in ["- ", "* ", "+ "] {
        if let Some(rest) = line.strip_prefix(mark) {
            return Some(rest.trim());
        }
    }
    let digits = line.chars().take_while(char::is_ascii_digit).count();
    if digits > 0 {
        let rest = &line[digits..];
        if let Some(rest) = rest.strip_prefix(". ").or_else(|| rest.strip_prefix(") ")) {
            return Some(rest.trim());
        }
    }
    None
}

fn is_none_item(text: &str) -> bool {
    let lower = text
        .trim()
        .trim_matches(|c: char| c == '.' || c == '*' || c == '_')
        .to_lowercase();
    matches!(
        lower.as_str(),
        "none" | "no findings" | "no open findings" | "nothing" | "-" | "n/a" | "no bugs"
    )
}

/// The findings of a review or QA document. The format follows the role files: a section whose
/// heading names findings, bugs, or issues holds one finding for each heading under it, or for
/// each list item at the start of a line when it has no such heading. A section whose heading
/// says "open" lists the open ones. A finding is `rejected` when it names a reason code (see
/// [`reason_code_of`]), and `kept` otherwise. Its priority is the first priority word of its
/// title, or of a line that starts with `Priority` or `Severity`. Its fix commit is the hash in
/// a line that names a fix or a commit.
pub fn findings(text: &str) -> Vec<Finding> {
    let mut blocks: Vec<(String, String)> = Vec::new();
    let mut section = Section::Other;
    let mut section_level = 0;
    let mut sub_headings = false;
    for line in text.lines() {
        if let Some((level, title)) = heading(line) {
            if section == Section::Findings
                && level > section_level
                && section_of(title) != Section::Open
            {
                sub_headings = true;
                blocks.push((title.to_string(), String::new()));
                continue;
            }
            section = section_of(title);
            section_level = level;
            sub_headings = false;
            continue;
        }
        if section != Section::Findings {
            continue;
        }
        if !sub_headings {
            if let Some(item) = top_item(line) {
                blocks.push((item.to_string(), String::new()));
                continue;
            }
        }
        if let Some((_, body)) = blocks.last_mut() {
            body.push_str(line);
            body.push('\n');
        }
    }
    blocks
        .into_iter()
        .filter(|(title, _)| !title.is_empty() && !is_none_item(title))
        .map(|(title, body)| finding(&title, &body))
        .collect()
}

fn finding(title: &str, body: &str) -> Finding {
    let lines = || std::iter::once(title).chain(body.lines());
    let reason_code = lines().find_map(reason_code_of).map(str::to_string);
    let priority = priority_of(title).or_else(|| {
        body.lines()
            .filter(|line| {
                let lower = line.to_lowercase();
                let label = lower.trim_start_matches(|c: char| !c.is_ascii_alphanumeric());
                label.starts_with("priority") || label.starts_with("severity")
            })
            .find_map(priority_of)
    });
    let fix_commit = lines().find_map(fix_commit_of);
    Finding {
        title: title.trim().to_string(),
        priority,
        verdict: if reason_code.is_some() {
            "rejected"
        } else {
            "kept"
        }
        .to_string(),
        reason_code,
        fix_commit,
    }
}

/// The words of a line in lower case: letters, digits, and `_`.
fn words_of(lower: &str) -> Vec<&str> {
    lower
        .split(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
        .filter(|w| !w.is_empty())
        .collect()
}

/// The reason code of a rejected finding in one line, or `None`.
///
/// A code with `_`, such as `style_only`, is not a word of plain text, so it counts in each
/// line. The codes `duplicate`, `speculative`, and `environment` are also plain words: a kept
/// finding can say "the CI environment". They count only in a line that says `rejected` or
/// `reason code`. `duplicate of` is the one other form of a duplicate. In such a marked line,
/// a code can also have a space or `-` in the place of `_`.
fn reason_code_of(line: &str) -> Option<&'static str> {
    let lower = line.to_lowercase();
    let marked = lower.contains("reject") || lower.contains("reason code");
    let joined = lower.replace(['-', ' '], "_");
    let words = words_of(&lower);
    REASON_CODES.into_iter().find(|code| {
        if code.contains('_') {
            words.contains(code) || (marked && joined.contains(code))
        } else {
            (marked && words.contains(code))
                || (*code == "duplicate" && lower.contains("duplicate of"))
        }
    })
}

/// The first priority word of a line.
fn priority_of(line: &str) -> Option<String> {
    let lower = line.to_lowercase();
    words_of(&lower)
        .into_iter()
        .find(|w| PRIORITIES.contains(w))
        .map(str::to_string)
}

/// The commit of a line that names a fix, such as `Fixed in 1a2b3c4d.`: the first word of 7 to
/// 40 hex characters with a digit and a letter. A line with no `fix` and no `commit` names no
/// fix, so that a color or a different hash in the text is not taken.
fn fix_commit_of(line: &str) -> Option<String> {
    let lower = line.to_lowercase();
    if !(lower.contains("fix") || lower.contains("commit")) {
        return None;
    }
    words_of(&lower)
        .into_iter()
        .find(|w| {
            (7..=40).contains(&w.len())
                && w.bytes().all(|b| b.is_ascii_hexdigit())
                && w.bytes().any(|b| b.is_ascii_digit())
                && w.bytes().any(|b| b.is_ascii_alphabetic())
        })
        .map(str::to_string)
}

/// The items of each section whose heading says "open", such as "Open findings".
pub fn open_items(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut open = false;
    for line in text.lines() {
        if let Some((_, title)) = heading(line) {
            open = section_of(title) == Section::Open;
            continue;
        }
        if !open {
            continue;
        }
        let item = top_item(line).or_else(|| top_item(line.trim_start()));
        if let Some(item) = item {
            if !is_none_item(item) {
                out.push(item.to_string());
            }
        }
    }
    out
}

// ----- Verdicts of review comments -------------------------------------------------------

/// The verdicts of `agents/shepherd.md`.
pub const VERDICTS: [&str; 6] = [
    "valid",
    "already-done",
    "incorrect",
    "out-of-scope",
    "low-priority",
    "question",
];

/// One row of `verdicts.json`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Verdict {
    pub comment_id: i64,
    pub verdict: String,
    pub fix_commit: Option<String>,
    pub reply: String,
}

/// Reads a JSON text that a session wrote. The one safe repair: a text inside a Markdown code
/// fence loses the fence.
pub fn loose_json(text: &str) -> Result<Value, String> {
    let trimmed = text.trim();
    let inner = trimmed
        .strip_prefix("```")
        .map(|rest| {
            let rest = rest.split_once('\n').map_or("", |(_, body)| body);
            rest.trim_end().strip_suffix("```").unwrap_or(rest)
        })
        .unwrap_or(trimmed);
    serde_json::from_str(inner).map_err(|e| format!("it is not valid JSON: {e}"))
}

/// The rows of `verdicts.json`: a JSON list, or an object with the list under `verdicts`. A
/// comment id may be a number or a text of digits, and a verdict any case with `_` or `-`.
pub fn verdicts(text: &str) -> Result<Vec<Verdict>, String> {
    let value = loose_json(text)?;
    let items = match &value {
        Value::Array(items) => items.clone(),
        Value::Object(map) => match map.get("verdicts") {
            Some(Value::Array(items)) => items.clone(),
            _ => return Err("it is an object with no list under `verdicts`".into()),
        },
        _ => return Err("it is not a list of rows".into()),
    };
    let mut rows = Vec::new();
    for (n, item) in items.iter().enumerate() {
        let id = match item.get("comment_id") {
            Some(Value::Number(n)) => n.as_i64(),
            Some(Value::String(s)) => s.trim().trim_start_matches('#').parse().ok(),
            _ => None,
        };
        let Some(comment_id) = id else {
            return Err(format!("row {} has no comment_id number", n + 1));
        };
        let verdict = item
            .get("verdict")
            .and_then(Value::as_str)
            .unwrap_or("")
            .trim()
            .to_lowercase()
            .replace(['_', ' '], "-");
        if !VERDICTS.contains(&verdict.as_str()) {
            return Err(format!(
                "row {} has the verdict `{verdict}`, which is not one of {}",
                n + 1,
                VERDICTS.join(", ")
            ));
        }
        let fix_commit = item
            .get("fix_commit")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|c| !c.is_empty())
            .map(str::to_string);
        let reply = item
            .get("reply")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        rows.push(Verdict {
            comment_id,
            verdict,
            fix_commit,
            reply,
        });
    }
    Ok(rows)
}

/// The author of each review comment of `comments.json`, by its id.
pub fn comment_authors(text: &str) -> BTreeMap<i64, String> {
    let Ok(Value::Array(items)) = loose_json(text) else {
        return BTreeMap::new();
    };
    items
        .iter()
        .filter_map(|c| {
            let id = c.get("id").and_then(Value::as_i64)?;
            let author = c.get("author").and_then(Value::as_str)?;
            Some((id, author.to_string()))
        })
        .collect()
}

// ----- Arguments that name a document ----------------------------------------------------

/// Replaces each `{doc:NAME}` in a string argument with the text of the document `NAME` of the
/// ticket folder. `read` gives the text, or `None` for a missing document. The error names the
/// document and what to do.
pub fn expand_args(
    args: &Map<String, Value>,
    read: &dyn Fn(&str) -> Option<String>,
) -> Result<Value, String> {
    let mut value = Value::Object(args.clone());
    expand(&mut value, read)?;
    Ok(value)
}

fn expand(value: &mut Value, read: &dyn Fn(&str) -> Option<String>) -> Result<(), String> {
    match value {
        Value::String(text) if text.contains("{doc:") => {
            let mut out = String::new();
            let mut rest = text.as_str();
            while let Some(start) = rest.find("{doc:") {
                out.push_str(&rest[..start]);
                let after = &rest[start + 5..];
                let Some(end) = after.find('}') else {
                    return Err(format!("`{text}` opens {{doc: and never closes it"));
                };
                let name = &after[..end];
                if !valid_document_name(name) {
                    return Err(format!(
                        "`{{doc:{name}}}` does not name a plain file of the ticket folder"
                    ));
                }
                let Some(doc) = read(name) else {
                    return Err(format!(
                        "an argument names the document {name}, which the ticket folder does not have"
                    ));
                };
                out.push_str(&doc);
                rest = &after[end + 1..];
            }
            out.push_str(rest);
            *text = out;
        }
        Value::Array(items) => {
            for item in items {
                expand(item, read)?;
            }
        }
        Value::Object(map) => {
            for item in map.values_mut() {
                expand(item, read)?;
            }
        }
        _ => {}
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn only_the_expected_documents_of_a_result_are_kept() {
        let result = json!({"kind": "result", "summary": "s", "documents": [
            {"name": "plan.md", "content": "Step 1."},
            {"name": ".vision/refine.md", "content": "Research."},
            {"name": "notes.txt", "content": "x"},
            {"name": "plan.md", "content": "again"},
            {"name": "empty.md"}
        ]});
        let docs = result_documents(&result, &["plan.md".into(), "refine.md".into()]);
        assert_eq!(docs.docs["plan.md"], "Step 1.");
        assert_eq!(docs.docs["refine.md"], "Research.", "the folder part goes");
        assert_eq!(docs.ignored.len(), 3, "{:?}", docs.ignored);
        assert!(
            docs.ignored[0].contains("notes.txt")
                && docs.ignored[0].contains("expects plan.md, refine.md")
        );
    }

    const REVIEW: &str = "# Review of DEMO-12\n\n## Findings\n\n### F1: subtract ignores floats (major)\nTrigger: subtract(1.5, 1).\nFixed in 1a2b3c4d.\n\n### F2: the docstring is short\nRejected: style_only.\n\n## Open findings\n\n- F3: the README does not name subtract\n";

    #[test]
    fn findings_come_from_the_headings_of_a_findings_section() {
        let found = findings(REVIEW);
        assert_eq!(found.len(), 2, "{found:?}");
        assert_eq!(found[0].title, "F1: subtract ignores floats (major)");
        assert_eq!(found[0].priority.as_deref(), Some("major"));
        assert_eq!(found[0].verdict, "kept");
        assert_eq!(found[0].fix_commit.as_deref(), Some("1a2b3c4d"));
        assert_eq!(found[1].verdict, "rejected");
        assert_eq!(found[1].reason_code.as_deref(), Some("style_only"));
        assert_eq!(
            open_items(REVIEW),
            ["F3: the README does not name subtract"]
        );
        assert_eq!(found[0].fingerprint().len(), 16);
        let again = Finding {
            title: "f1 subtract IGNORES floats major".into(),
            ..found[0].clone()
        };
        assert_eq!(again.fingerprint(), found[0].fingerprint());
    }

    #[test]
    fn findings_can_be_list_items_and_none_means_none() {
        let text = "## Bugs\n\n1. The total is wrong (critical)\n2. A typo, duplicate of 1\n\n## Open\n\n- none\n";
        let found = findings(text);
        assert_eq!(found.len(), 2);
        assert_eq!(found[0].priority.as_deref(), Some("critical"));
        assert_eq!(found[1].reason_code.as_deref(), Some("duplicate"));
        assert!(open_items(text).is_empty());
        assert!(findings("## Findings\n\n- None.\n").is_empty());
        assert!(findings("No sections here.\n- a list item\n").is_empty());
    }

    #[test]
    fn a_plain_word_of_a_kept_finding_is_not_a_reason_code_or_a_priority() {
        let text = "## Findings\n\n\
            ### F1: the test fails in the CI environment (major)\n\
            A low number of rows shows it. The color is #ff00aa80.\n\n\
            ### F2: the retry is speculative work\n\
            Severity: high\n\
            The commit 9f3e21c0 fixes it.\n\n\
            ### F3: flaky on one machine\n\
            Rejected: environment.\n\n\
            ### F4: cannot see it\n\
            Reason code: not reproducible\n\n\
            ### F5: old code\n\
            This is pre_existing.\n";
        let found = findings(text);
        assert_eq!(found.len(), 5, "{found:?}");
        assert_eq!(found[0].verdict, "kept", "{:?}", found[0]);
        assert_eq!(found[0].priority.as_deref(), Some("major"));
        assert_eq!(found[0].fix_commit, None, "a color is not a commit");
        assert_eq!(found[1].verdict, "kept", "{:?}", found[1]);
        assert_eq!(found[1].priority.as_deref(), Some("high"));
        assert_eq!(found[1].fix_commit.as_deref(), Some("9f3e21c0"));
        assert_eq!(found[2].reason_code.as_deref(), Some("environment"));
        assert_eq!(found[2].priority, None);
        assert_eq!(found[3].reason_code.as_deref(), Some("not_reproducible"));
        assert_eq!(found[4].reason_code.as_deref(), Some("pre_existing"));
    }

    #[test]
    fn verdicts_are_read_with_one_safe_repair() {
        let text = "```json\n[{\"comment_id\": \"3\", \"verdict\": \"Out_of_scope\", \"fix_commit\": \"\", \"reply\": \"Later.\"},\n {\"comment_id\": 4, \"verdict\": \"valid\", \"fix_commit\": \"abc1234\", \"reply\": \"Done.\"}]\n```";
        let rows = verdicts(text).unwrap();
        assert_eq!(rows[0].comment_id, 3);
        assert_eq!(rows[0].verdict, "out-of-scope");
        assert_eq!(rows[0].fix_commit, None);
        assert_eq!(rows[1].fix_commit.as_deref(), Some("abc1234"));
        let wrapped =
            verdicts("{\"verdicts\": [{\"comment_id\": 1, \"verdict\": \"question\"}]}").unwrap();
        assert_eq!(wrapped.len(), 1);
        let err = verdicts("[{\"comment_id\": 1, \"verdict\": \"maybe\"}]").unwrap_err();
        assert!(err.contains("`maybe`, which is not one of valid"), "{err}");
        assert!(verdicts("[{\"verdict\": \"valid\"}]")
            .unwrap_err()
            .contains("row 1 has no comment_id"));
        assert!(verdicts("not json").unwrap_err().contains("not valid JSON"));
        let authors = comment_authors("[{\"id\": 1, \"author\": \"someone\", \"text\": \"x\"}]");
        assert_eq!(authors[&1], "someone");
    }

    #[test]
    fn an_argument_takes_the_text_of_a_document() {
        let args =
            json!({"text": "{doc:description.md}", "nested": ["Title: {doc:pr.md}!"], "n": 1});
        let read = |name: &str| match name {
            "description.md" => Some("The new text.".to_string()),
            "pr.md" => Some("Add subtract".to_string()),
            _ => None,
        };
        let value = expand_args(args.as_object().unwrap(), &read).unwrap();
        assert_eq!(
            value,
            json!({"text": "The new text.", "nested": ["Title: Add subtract!"], "n": 1})
        );
        let missing = json!({"text": "{doc:plan.md}"});
        let err = expand_args(missing.as_object().unwrap(), &read).unwrap_err();
        assert!(
            err.contains("names the document plan.md, which the ticket folder does not have"),
            "{err}"
        );
        let bad = json!({"text": "{doc:../x}"});
        assert!(expand_args(bad.as_object().unwrap(), &read).is_err());
        let open = json!({"text": "{doc:pr.md"});
        assert!(expand_args(open.as_object().unwrap(), &read)
            .unwrap_err()
            .contains("never closes"));
    }
}
