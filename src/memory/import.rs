//! `vision memory import FILE` (section 9.2): past answers from a JSONL file as decisions.
//!
//! Each line is one JSON object. The import reads these keys and ignores each other key:
//! `question` (text), `options` (a list of texts, or of objects with `label`), `answer` (a text
//! or a list of texts), `note` (the reason), `at` (a time), `ticket`, `kind`, `area` (a list),
//! and also `step` and `asked_by` when they are there. A line with no question or no answer is
//! skipped. Each decision has the writer `import` and keeps a stable hash of its answer in
//! `import_hash`, so that a second import of the same file adds nothing. The import is one
//! commit.

use std::collections::BTreeSet;

use anyhow::Result;
use serde::Serialize;
use serde_json::{json, Map, Value as Json};
use time::{Date, OffsetDateTime};

use super::doc::Doc;
use super::model::{Basis, DecisionId, Kind, NewDecision, Writer};
use super::store::{Change, Memory};
use crate::util::ids::sha256_hex;
use crate::util::time::parse_rfc3339;

/// One line that the import skipped, with its number (from 1) and the reason.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Skipped {
    pub line: usize,
    pub reason: String,
}

/// What one import did.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct ImportReport {
    /// The lines that are not blank.
    pub read: usize,
    pub added: Vec<DecisionId>,
    pub skipped: Vec<Skipped>,
    /// The lines whose answer the memory already has.
    pub already: usize,
    /// Values that the import could not use, such as an unknown kind.
    pub warnings: Vec<String>,
    /// The commit of the import, or `None` when it added nothing.
    pub commit: Option<String>,
}

/// One past answer that the import can write.
struct Past {
    hash: String,
    decision: NewDecision,
}

impl Memory {
    /// Imports the lines of a JSONL text. `source` names the file in the commit message.
    pub fn import_jsonl(&self, text: &str, source: &str) -> Result<ImportReport> {
        let mut report = ImportReport::default();
        let mut pasts = Vec::new();
        for (index, line) in text.lines().enumerate() {
            let number = index + 1;
            if line.trim().is_empty() {
                continue;
            }
            report.read += 1;
            match read_line(line, number, &mut report.warnings) {
                Ok(past) => pasts.push(past),
                Err(reason) => report.skipped.push(Skipped {
                    line: number,
                    reason,
                }),
            }
        }
        let _lock = self.lock()?;
        self.check_main()?;
        let mut known = self.import_hashes()?;
        let mut next = self.next_decision()?.number();
        let mut changes = Vec::new();
        for past in pasts {
            if !known.insert(past.hash.clone()) {
                report.already += 1;
                continue;
            }
            let Some(id) = DecisionId::new(next) else {
                continue;
            };
            next += 1;
            let text = past.decision.render(id, None, Some(&past.hash));
            changes.push(Change::write(id.path(), text));
            report.added.push(id);
        }
        if changes.is_empty() {
            return Ok(report);
        }
        self.check_changes(&changes)?;
        let noun = if changes.len() == 1 {
            "decision"
        } else {
            "decisions"
        };
        let message = format!("Import {} {noun} from {source}", changes.len());
        report.commit = self.write_commit(&changes, &message, &Writer::Import)?;
        Ok(report)
    }

    /// The import hashes of the decisions in the work tree.
    fn import_hashes(&self) -> Result<BTreeSet<String>> {
        let (files, _) = self.snapshot()?;
        Ok(files
            .values()
            .filter_map(|text| Doc::parse(text).ok())
            .filter_map(|doc| {
                doc.get("import_hash")
                    .and_then(|v| v.as_text())
                    .map(str::to_string)
            })
            .collect())
    }
}

fn read_line(line: &str, number: usize, warnings: &mut Vec<String>) -> Result<Past, String> {
    let value: Json =
        serde_json::from_str(line).map_err(|e| format!("the line is not JSON ({e})"))?;
    let Json::Object(object) = value else {
        return Err("the line is not a JSON object".into());
    };
    let question = text_of(object.get("question")).filter(|q| !q.is_empty());
    let Some(question) = question else {
        return Err("it has no question".into());
    };
    let answers = texts_of(object.get("answer"));
    if answers.is_empty() {
        return Err("it has no answer".into());
    }
    let choices: Vec<String> = match object.get("options") {
        Some(Json::Array(items)) => items
            .iter()
            .filter_map(|item| match item {
                Json::Object(o) => text_of(o.get("label")),
                other => text_of(Some(other)),
            })
            .filter(|c| !c.is_empty())
            .collect(),
        _ => Vec::new(),
    };
    let at = match object.get("at") {
        None | Some(Json::Null) => None,
        Some(value) => match time_of(value) {
            Some(at) => Some(at),
            None => {
                warnings.push(format!(
                    "line {number}: `at` is {value}, which is not a time, so the decision has the time of the import"
                ));
                None
            }
        },
    };
    let kind = match text_of(object.get("kind")) {
        None => None,
        Some(text) if text.is_empty() => None,
        Some(text) => {
            let kind = Kind::parse(&text);
            if kind.is_none() {
                warnings.push(format!(
                    "line {number}: `kind` is `{text}`, which is not one of {}, so the decision has no kind",
                    Kind::choices()
                ));
            }
            kind
        }
    };
    let area = match object.get("area") {
        Some(Json::String(text)) => text.split(',').map(|s| s.trim().to_string()).collect(),
        other => texts_of(other),
    };
    let hash = import_hash(&object, &question, &answers);
    let decision = NewDecision {
        at,
        by: Writer::Import,
        basis: Basis::Answer,
        ticket: text_of(object.get("ticket")).filter(|t| !t.is_empty()),
        step: text_of(object.get("step")).filter(|t| !t.is_empty()),
        asked_by: text_of(object.get("asked_by")).filter(|t| !t.is_empty()),
        kind,
        area,
        sources: Vec::new(),
        question,
        choices,
        answer: answers.join("\n"),
        why: text_of(object.get("note")).unwrap_or_default(),
        applies_when: String::new(),
    };
    Ok(Past { hash, decision })
}

/// The stable hash of a past answer: the question, the answers, the time, and the ticket, as
/// the line gave them. The same answer in a file with other spacing or key order has the same hash.
fn import_hash(object: &Map<String, Json>, question: &str, answers: &[String]) -> String {
    let key = json!([
        question,
        answers,
        object.get("at").unwrap_or(&Json::Null),
        object.get("ticket").unwrap_or(&Json::Null)
    ]);
    sha256_hex(key.to_string().as_bytes())[..16].to_string()
}

/// A text, a number, or a boolean as trimmed text.
fn text_of(value: Option<&Json>) -> Option<String> {
    match value? {
        Json::String(s) => Some(s.trim().to_string()),
        Json::Number(n) => Some(n.to_string()),
        Json::Bool(b) => Some(b.to_string()),
        _ => None,
    }
}

/// A text or a list of texts, without the empty ones.
fn texts_of(value: Option<&Json>) -> Vec<String> {
    let items: Vec<String> = match value {
        Some(Json::Array(items)) => items.iter().filter_map(|i| text_of(Some(i))).collect(),
        other => text_of(other).into_iter().collect(),
    };
    items.into_iter().filter(|s| !s.is_empty()).collect()
}

/// RFC 3339 text, a date such as `2026-11-16`, or Unix seconds (or milliseconds).
fn time_of(value: &Json) -> Option<OffsetDateTime> {
    match value {
        Json::String(text) => {
            let text = text.trim();
            if let Ok(at) = parse_rfc3339(text) {
                return Some(at);
            }
            let format = time::macros::format_description!("[year]-[month]-[day]");
            Date::parse(text, format)
                .ok()
                .map(|d| d.midnight().assume_utc())
        }
        Json::Number(n) => {
            let n = n.as_i64()?;
            let seconds = if n.abs() >= 100_000_000_000 {
                n / 1000
            } else {
                n
            };
            OffsetDateTime::from_unix_timestamp(seconds).ok()
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_line_with_every_key_becomes_a_decision() {
        let line = r#"{"question": " Which format? ", "options": [{"label": "CSV", "means": "plain"}, "XLSX"], "answer": "CSV", "note": "Finance imports CSV.", "at": "2026-11-16T16:30:00+02:00", "ticket": "DEMO-12", "kind": "Product", "area": ["reports", "exports"], "extra": 1}"#;
        let mut warnings = Vec::new();
        let past = read_line(line, 1, &mut warnings).unwrap();
        assert!(warnings.is_empty(), "{warnings:?}");
        let d = past.decision;
        assert_eq!(d.question, "Which format?");
        assert_eq!(d.choices, ["CSV", "XLSX"]);
        assert_eq!(d.answer, "CSV");
        assert_eq!(d.why, "Finance imports CSV.");
        assert_eq!(d.kind, Some(Kind::Product));
        assert_eq!(d.area, ["reports", "exports"]);
        assert_eq!(d.ticket.as_deref(), Some("DEMO-12"));
        assert_eq!(
            crate::util::time::rfc3339(d.at.unwrap()),
            "2026-11-16T14:30:00Z"
        );
        assert_eq!(past.hash.len(), 16);
    }

    #[test]
    fn the_hash_ignores_spacing_and_key_order_but_not_the_answer() {
        let mut w = Vec::new();
        let a = read_line(
            r#"{"question":"Q","answer":"A","ticket":"DEMO-1"}"#,
            1,
            &mut w,
        )
        .unwrap();
        let b = read_line(
            r#"{ "ticket" : "DEMO-1", "answer" : "A", "question" : "Q", "note": "x" }"#,
            1,
            &mut w,
        )
        .unwrap();
        let c = read_line(
            r#"{"question":"Q","answer":"B","ticket":"DEMO-1"}"#,
            1,
            &mut w,
        )
        .unwrap();
        assert_eq!(a.hash, b.hash);
        assert_ne!(a.hash, c.hash);
    }

    #[test]
    fn lines_without_a_question_or_an_answer_are_skipped() {
        let mut w = Vec::new();
        for (line, word) in [
            (r#"{"answer": "A"}"#, "no question"),
            (r#"{"question": "  ", "answer": "A"}"#, "no question"),
            (r#"{"question": "Q"}"#, "no answer"),
            (r#"{"question": "Q", "answer": []}"#, "no answer"),
            (r#"{"question": "Q", "answer": null}"#, "no answer"),
            ("[1, 2]", "not a JSON object"),
            ("{broken", "not JSON"),
        ] {
            let err = read_line(line, 3, &mut w).err().unwrap();
            assert!(err.contains(word), "{line}: {err}");
        }
    }

    #[test]
    fn odd_values_become_warnings() {
        let mut w = Vec::new();
        let past = read_line(
            r#"{"question": "Q", "answer": ["A", "B"], "at": "last week", "kind": "food", "area": "a, b"}"#,
            7,
            &mut w,
        )
        .unwrap();
        assert_eq!(past.decision.answer, "A\nB");
        assert_eq!(past.decision.area, ["a", "b"]);
        assert_eq!(past.decision.kind, None);
        assert_eq!(past.decision.at, None);
        assert_eq!(w.len(), 2, "{w:?}");
        assert!(w[0].starts_with("line 7: `at`"), "{w:?}");
        let day = read_line(
            r#"{"question": "Q", "answer": "A", "at": "2026-01-02"}"#,
            1,
            &mut w,
        )
        .unwrap();
        assert_eq!(
            crate::util::time::rfc3339(day.decision.at.unwrap()),
            "2026-01-02T00:00:00Z"
        );
        let unix = read_line(
            r#"{"question": "Q", "answer": "A", "at": 1794412800000}"#,
            1,
            &mut w,
        )
        .unwrap();
        assert_eq!(unix.decision.at.unwrap().unix_timestamp(), 1794412800);
    }
}
