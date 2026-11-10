//! Untrusted values from a script: each string is cut to a length and loses its control
//! characters, so that it cannot break a log line or the terminal of the person.

use serde_json::{Map, Value};

use crate::adapter::valid_document_name;

/// The longest `detail` that the executor keeps.
pub const DETAIL_MAX: usize = 4000;
/// The longest document that the executor writes.
pub const DOCUMENT_MAX: usize = 4 * 1024 * 1024;

/// The documents of the ticket folder that a script can never write, because the person or the
/// pipeline owns them.
pub const RESERVED_DOCUMENTS: [&str; 9] = [
    "refine.md",
    "plan.md",
    "approved-plan.md",
    "answers.md",
    "pr.md",
    "review.md",
    "notes.md",
    "description.md",
    "issue.json",
];

/// A text of many lines: a string, or the JSON of another value. Empty for nothing.
pub fn text(value: Option<&Value>, max: usize) -> String {
    match value {
        None | Some(Value::Null) => String::new(),
        Some(Value::String(s)) => strip(s, true, max),
        Some(other) => strip(&other.to_string(), true, max),
    }
}

/// A text of one line: a string or a number.
pub fn line(value: Option<&Value>, max: usize) -> Option<String> {
    let text = match value? {
        Value::String(s) => strip(s, false, max),
        Value::Number(n) => n.to_string(),
        _ => return None,
    };
    let text = text.trim().to_string();
    (!text.is_empty()).then_some(text)
}

/// The text without control characters (a line break and a tab stay when `lines` is true),
/// cut to `max` characters.
pub fn strip(text: &str, lines: bool, max: usize) -> String {
    let mut out: String = text
        .chars()
        .filter(|c| !c.is_control() || (lines && matches!(c, '\n' | '\t')))
        .take(max)
        .collect();
    if text.chars().count() > max {
        out.push_str(" [cut]");
    }
    out
}

/// One document of an answer.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub struct Document {
    pub name: String,
    pub content: String,
}

/// The documents of an answer: `[{"name": "...", "content": "..."}]`. Gives the valid ones and
/// one note for each one that was dropped.
pub fn documents(value: Option<&Value>) -> (Vec<Document>, Vec<String>) {
    let mut docs = Vec::new();
    let mut dropped = Vec::new();
    let Some(value) = value.filter(|v| !v.is_null()) else {
        return (docs, dropped);
    };
    let Some(items) = value.as_array() else {
        dropped.push("`documents` is not a list".to_string());
        return (docs, dropped);
    };
    for item in items {
        let name = item.get("name").and_then(Value::as_str).unwrap_or("");
        let shown = strip(name, false, 60);
        let Some(content) = item.get("content").and_then(Value::as_str) else {
            dropped.push(format!("the document `{shown}` has no text content"));
            continue;
        };
        if !valid_document_name(name) {
            dropped.push(format!(
                "the document name `{shown}` is not a plain file name"
            ));
        } else if RESERVED_DOCUMENTS.contains(&name) {
            dropped.push(format!(
                "the document `{name}` belongs to the person or the pipeline"
            ));
        } else if content.len() > DOCUMENT_MAX {
            dropped.push(format!("the document `{name}` is larger than 4 MB"));
        } else {
            docs.push(Document {
                name: name.to_string(),
                content: content.to_string(),
            });
        }
    }
    (docs, dropped)
}

/// The arguments of a call: a JSON object. `null` is an empty object.
pub fn object_args(args: &Value) -> anyhow::Result<Value> {
    match args {
        Value::Null => Ok(Value::Object(Map::new())),
        Value::Object(_) => Ok(args.clone()),
        other => anyhow::bail!("the arguments must be a JSON object, not `{other}`"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn control_characters_go_and_long_text_is_cut() {
        assert_eq!(strip("a\u{1b}[31mb\nc", true, 100), "a[31mb\nc");
        assert_eq!(strip("a\nb", false, 100), "ab");
        assert_eq!(strip("abcdef", true, 3), "abc [cut]");
        assert_eq!(line(Some(&json!(42)), 10).as_deref(), Some("42"));
        assert_eq!(line(Some(&json!(["x"])), 10), None);
        assert_eq!(text(Some(&json!({"a": 1})), 100), "{\"a\":1}");
    }

    #[test]
    fn bad_documents_are_dropped_with_a_note() {
        let value = json!([
            {"name": "ci-log.txt", "content": "ok"},
            {"name": "../escape", "content": "x"},
            {"name": "approved-plan.md", "content": "x"},
            {"name": "empty.txt"}
        ]);
        let (docs, dropped) = documents(Some(&value));
        assert_eq!(
            docs,
            vec![Document {
                name: "ci-log.txt".into(),
                content: "ok".into()
            }]
        );
        assert_eq!(dropped.len(), 3, "{dropped:?}");
        assert!(dropped[1].contains("belongs to the person"), "{dropped:?}");
    }

    #[test]
    fn arguments_are_an_object() {
        assert_eq!(object_args(&Value::Null).unwrap(), json!({}));
        assert!(object_args(&json!([1])).is_err());
    }
}
