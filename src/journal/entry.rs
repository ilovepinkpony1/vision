//! One journal entry (section 16): a Markdown file with a small front matter and the sections
//! Summary, Notes, Errors and fixes, Gate results, Answers, and Outcome. Each section shows only
//! when it has content.
//!
//! [`build`] gathers the facts of one turn from its folder and the ledger, [`render`] writes the
//! text, and [`signature`] gives the hash of the facts of the ledger that the text shows, so
//! that a tick builds an entry again only when one of them changed.

use std::path::{Path, PathBuf};

use anyhow::Result;
use serde_json::{json, Value};

use super::errors::{self, cut};
use crate::adapter::{Adapter, Expect, GateRule};
use crate::ctx::Ctx;
use crate::ledger::turns::TurnRow;
use crate::ledger::Ledger;
use crate::memory::front::{parse_entry, render_value, ParsedLine, Value as FrontValue};
use crate::paths::{TicketKey, TurnId};
use crate::pipeline::docs::result_documents;
use crate::pipeline::gates;
use crate::util::fs::read_optional;
use crate::util::ids;

/// The version of the form of an entry. A new version writes each recent entry again.
pub const FORMAT: &str = "1";
/// The kinds of a note.
pub const NOTE_KINDS: [&str; 6] = ["problem", "cause", "fix", "dead-end", "command", "fact"];
/// The kinds of the events that the outcome of a turn reads.
const OUTCOME_EVENTS: [&str; 4] = ["end-fact", "step", "flag", "ended"];
/// The most files that the front matter names.
const MAX_FILES: usize = 50;
/// The most items of one gate.
const MAX_GATE_ITEMS: usize = 10;
/// The most notes of one entry.
const MAX_NOTES: usize = 40;
/// The longest text of one note.
const MAX_NOTE_CHARS: usize = 600;

/// One note of a result (section 7.6).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Note {
    pub kind: String,
    pub text: String,
    pub files: Vec<String>,
    pub commit: Option<String>,
    pub cites: Vec<String>,
}

/// What one gate of the step found in the document of the turn.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct GateResult {
    /// `cr` or `qa`.
    pub kind: String,
    pub doc: String,
    /// `passed`, `failed`, or `missing`.
    pub verdict: String,
    /// The text of a failed gate, or the notes of a gate that passed.
    pub detail: String,
    /// The failing or low items.
    pub items: Vec<String>,
}

/// One card of the turn and its answer.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AnswerLine {
    pub question: String,
    pub text: String,
    pub answer: Option<String>,
    pub by: Option<String>,
    pub why: Option<String>,
    pub decision: Option<String>,
    pub state: String,
}

/// The facts of one entry.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Entry {
    pub id: String,
    pub ticket: String,
    pub step: String,
    pub agent: String,
    pub model: Option<String>,
    pub started: Option<String>,
    pub ended: Option<String>,
    /// One word or a short phrase, such as `held`, `failed:clean`, or `questions`.
    pub outcome: String,
    pub head: Option<String>,
    pub files: Vec<String>,
    pub cost: Option<f64>,
    pub summary: Option<String>,
    pub notes: Vec<Note>,
    /// The lines of the section Errors and fixes.
    pub errors: Vec<String>,
    pub gates: Vec<GateResult>,
    pub answers: Vec<AnswerLine>,
    /// The lines of the section Outcome.
    pub outcome_lines: Vec<String>,
}

/// The notes of a result object. A note with an unknown kind or no text is left out.
pub fn notes_of(result: &Value) -> Vec<Note> {
    let strings = |v: Option<&Value>| -> Vec<String> {
        v.and_then(Value::as_array)
            .map(|items| {
                items
                    .iter()
                    .filter_map(Value::as_str)
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
                    .collect()
            })
            .unwrap_or_default()
    };
    result
        .get("notes")
        .and_then(Value::as_array)
        .map(|notes| {
            notes
                .iter()
                .filter_map(|note| {
                    let kind = note.get("kind").and_then(Value::as_str)?.trim();
                    let text = note.get("text").and_then(Value::as_str)?.trim();
                    if !NOTE_KINDS.contains(&kind) || text.is_empty() {
                        return None;
                    }
                    Some(Note {
                        kind: kind.to_string(),
                        text: cut(&one_line(text), MAX_NOTE_CHARS),
                        files: strings(note.get("files")),
                        commit: note
                            .get("commit")
                            .and_then(Value::as_str)
                            .map(str::trim)
                            .filter(|c| !c.is_empty())
                            .map(str::to_string),
                        cites: strings(note.get("cites")),
                    })
                })
                .take(MAX_NOTES)
                .collect()
        })
        .unwrap_or_default()
}

/// A text with each line end as one space.
fn one_line(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// The text of an entry. The caller masks the secrets in it.
pub fn render(entry: &Entry) -> String {
    let mut out = String::from("---\n");
    let mut key = |name: &str, value: FrontValue| {
        out.push_str(&format!("{name}: {}\n", render_value(&value)));
    };
    key("id", FrontValue::str(&entry.id));
    key("ticket", FrontValue::str(&entry.ticket));
    key("step", FrontValue::str(&entry.step));
    key("agent", FrontValue::str(&entry.agent));
    key("model", FrontValue::opt_str(entry.model.as_deref()));
    key("started", FrontValue::opt_str(entry.started.as_deref()));
    key("ended", FrontValue::opt_str(entry.ended.as_deref()));
    key("outcome", FrontValue::str(&entry.outcome));
    key("head", FrontValue::opt_str(entry.head.as_deref()));
    key("files", FrontValue::list(entry.files.iter().cloned()));
    key(
        "cost",
        match entry.cost {
            Some(cost) => {
                FrontValue::Scalar(crate::memory::front::Scalar::Num(format!("{cost:.4}")))
            }
            None => FrontValue::null(),
        },
    );
    out.push_str("---\n");
    out.push_str(&format!(
        "# {} {}: a turn of the {}\n",
        entry.ticket, entry.step, entry.agent
    ));
    let mut section = |title: &str, body: &str| {
        if !body.trim().is_empty() {
            out.push_str(&format!("\n## {title}\n\n{}\n", body.trim_end()));
        }
    };
    // A line of the summary that starts with `#` gets a space in front, so that it never
    // starts a section of the entry.
    let summary: Vec<String> = entry
        .summary
        .as_deref()
        .unwrap_or("")
        .lines()
        .map(|line| {
            if line.starts_with('#') {
                format!(" {line}")
            } else {
                line.to_string()
            }
        })
        .collect();
    section("Summary", &summary.join("\n"));
    section(
        "Notes",
        &entry
            .notes
            .iter()
            .map(render_note)
            .collect::<Vec<_>>()
            .join("\n"),
    );
    section("Errors and fixes", &entry.errors.join("\n"));
    section(
        "Gate results",
        &entry
            .gates
            .iter()
            .map(render_gate)
            .collect::<Vec<_>>()
            .join("\n"),
    );
    section(
        "Answers",
        &entry
            .answers
            .iter()
            .map(render_answer)
            .collect::<Vec<_>>()
            .join("\n"),
    );
    section("Outcome", &entry.outcome_lines.join("\n"));
    out
}

fn render_note(note: &Note) -> String {
    let mut text = format!("- {}: {}", note.kind, note.text);
    let mut more = Vec::new();
    if !note.files.is_empty() {
        more.push(format!("files: {}", note.files.join(", ")));
    }
    if let Some(commit) = &note.commit {
        more.push(format!("commit: {commit}"));
    }
    if !note.cites.is_empty() {
        more.push(format!("cites: {}", note.cites.join(", ")));
    }
    if !more.is_empty() {
        text.push_str(&format!(" ({})", more.join("; ")));
    }
    text
}

fn render_gate(gate: &GateResult) -> String {
    let mut text = format!("- gate {} ({}): {}", gate.kind, gate.doc, gate.verdict);
    if !gate.detail.trim().is_empty() {
        text.push_str(&format!(". {}", cut(&one_line(&gate.detail), 600)));
    }
    for item in &gate.items {
        text.push_str(&format!("\n  - {item}"));
    }
    text
}

fn render_answer(answer: &AnswerLine) -> String {
    let mut text = format!("- {}", answer.question);
    if let Some(decision) = &answer.decision {
        text.push_str(&format!(" ({decision})"));
    }
    text.push_str(&format!(": {}", cut(&one_line(&answer.text), 300)));
    match &answer.answer {
        Some(words) => {
            text.push_str(&format!(" Answer: {}", cut(&one_line(words), 300)));
            if let Some(by) = &answer.by {
                text.push_str(&format!(" (by {by})"));
            }
            text.push('.');
            if let Some(why) = answer.why.as_deref().filter(|w| !w.trim().is_empty()) {
                text.push_str(&format!(" Why: {}", cut(&one_line(why), 300)));
            }
        }
        None => text.push_str(&format!(" No answer yet ({}).", answer.state)),
    }
    text
}

/// The questions of the turn, the events of its ticket after its end, and its own row, as
/// one hash. The entry shows these facts, so a new hash means a new text.
pub fn signature(ledger: &Ledger, row: &TurnRow) -> Result<String> {
    let questions: Vec<Value> = ledger
        .questions_of_turn(&row.id)?
        .into_iter()
        .map(|q| json!([q.id, q.state, q.answer, q.decision]))
        .collect();
    let (from, to) = window(ledger, row)?;
    let events: Vec<i64> = match &from {
        Some(from) => ledger
            .ticket_events_between(&row.ticket, &OUTCOME_EVENTS, from, to.as_deref())?
            .into_iter()
            .map(|e| e.id)
            .collect(),
        None => Vec::new(),
    };
    let resumed = resumed_by(ledger, row)?;
    let value = json!({
        "format": FORMAT, "status": row.status, "reason": row.reason, "ended": row.ended,
        "end_fact": row.end_fact, "cost": row.cost_usd, "questions": questions,
        "events": events, "next": to, "resumed": resumed,
    });
    Ok(ids::sha256_hex(value.to_string().as_bytes()))
}

/// The time span of the outcome of a turn: from its end to the start of the next turn of its
/// ticket, or with no end while no turn followed.
fn window(ledger: &Ledger, row: &TurnRow) -> Result<(Option<String>, Option<String>)> {
    let Some(from) = row.ended.clone().or_else(|| row.started.clone()) else {
        return Ok((None, None));
    };
    let after = row.started.clone().unwrap_or_else(|| from.clone());
    // A turn of the Judge or of the Curator does not end the outcome of a work turn.
    let alone: Vec<&str> = crate::models::AGENTS
        .iter()
        .filter(|a| a.alone)
        .map(|a| a.name)
        .collect();
    let next = ledger.next_turn_start(&row.ticket, &after, &row.id, &alone)?;
    Ok((Some(from), next))
}

/// The turn that resumed the session of this turn with `continue` or with a message.
fn resumed_by(ledger: &Ledger, row: &TurnRow) -> Result<Option<String>> {
    match (&row.session, &row.started) {
        (Some(session), Some(started)) => Ok(ledger
            .next_turn_of_session(session, started, &row.id)?
            .map(|r| r.id)),
        _ => Ok(None),
    }
}

/// The facts of one turn. `head` is the last commit of the turn, when the caller knows it.
pub fn build(
    ctx: &Ctx,
    ledger: &Ledger,
    row: &TurnRow,
    adapter: Option<&Adapter>,
    head: Option<String>,
) -> Result<Entry> {
    let key = TicketKey::parse(&row.ticket)?;
    let id = TurnId::parse(&row.id)?;
    let turn = ctx.paths.turn(&key, &id);
    let result = read_optional(&turn.result)?
        .and_then(|text| serde_json::from_str::<Value>(&text).ok())
        .filter(|v| !v.is_null());
    let step = row.step.clone().unwrap_or_default();
    let stream = errors::read_stream(&turn.stream)?;
    let (items, left_out) = errors::errors_and_fixes(&stream.calls);
    let mut error_lines: Vec<String> = items.iter().map(errors::render).collect();
    if left_out > 0 {
        error_lines.push(format!("- {left_out} more failed call(s) are left out."));
    }

    let workdir = workdir_of(&turn.dir);
    let mut files: Vec<String> = stream
        .files
        .iter()
        .map(|f| relative(f, workdir.as_deref()))
        .collect();
    let start_head = read_optional(&turn.facts)?
        .and_then(|text| serde_json::from_str::<Value>(&text).ok())
        .and_then(|facts| {
            facts
                .get("head")
                .and_then(Value::as_str)
                .map(str::to_string)
        });
    if let (Some(dir), Some(from), Some(to)) = (&workdir, &start_head, &head) {
        if from != to {
            if let Ok(names) = crate::util::git::run(dir, &["diff", "--name-only", from, to]) {
                for name in names.lines().map(str::trim).filter(|n| !n.is_empty()) {
                    if !files.iter().any(|f| f == name) {
                        files.push(name.to_string());
                    }
                }
            }
        }
    }
    files.truncate(MAX_FILES);

    let gates = match (adapter.and_then(|a| a.step(&step)), &result) {
        (Some(step), Some(result)) => gate_results(step, result),
        _ => Vec::new(),
    };
    let answers = ledger
        .questions_of_turn(&row.id)?
        .into_iter()
        .map(|q| {
            let card: Value = serde_json::from_str(&q.card).unwrap_or(Value::Null);
            let answer: Option<Value> = q
                .answer
                .as_deref()
                .and_then(|a| serde_json::from_str(a).ok());
            let field = |name: &str| {
                answer
                    .as_ref()
                    .and_then(|a| a.get(name))
                    .and_then(Value::as_str)
                    .map(str::to_string)
            };
            AnswerLine {
                question: q.id.clone(),
                text: card
                    .get("text")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string(),
                answer: field("text"),
                by: field("by"),
                why: field("why"),
                decision: q.decision.clone(),
                state: q.state.clone(),
            }
        })
        .collect();

    let kind = result
        .as_ref()
        .and_then(|r| r.get("kind"))
        .and_then(Value::as_str)
        .map(str::to_string);
    let status = row.status.clone().unwrap_or_default();
    let outcome = match (status.as_str(), kind.as_deref(), &row.end_fact) {
        ("done", Some("result"), Some(fact)) => fact.clone(),
        ("done", Some(kind), _) => kind.to_string(),
        (status, _, _) => match &row.reason {
            Some(reason) => format!("{status} ({reason})"),
            None => status.to_string(),
        },
    };
    let outcome_lines = outcome_lines(ledger, row, kind.as_deref())?;
    Ok(Entry {
        id: row.id.clone(),
        ticket: row.ticket.clone(),
        step,
        agent: row.agent.clone().unwrap_or_default(),
        model: row.model.clone(),
        started: row.started.clone(),
        ended: row.ended.clone(),
        outcome,
        head,
        files,
        cost: row.cost_usd,
        summary: result
            .as_ref()
            .and_then(|r| r.get("summary"))
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string),
        notes: result.as_ref().map(notes_of).unwrap_or_default(),
        errors: error_lines,
        gates,
        answers,
        outcome_lines,
    })
}

/// The working folder of a turn, from its `launch.json`.
fn workdir_of(turn_dir: &Path) -> Option<PathBuf> {
    crate::turn::LaunchFile::read(turn_dir)
        .ok()
        .map(|file| file.plan.workdir)
}

/// A path relative to the working folder, when it is in it.
fn relative(path: &str, workdir: Option<&Path>) -> String {
    match workdir.and_then(|dir| Path::new(path).strip_prefix(dir).ok()) {
        Some(rest) => rest.display().to_string(),
        None => path.to_string(),
    }
}

/// What each gate of the step found in the documents of the result.
fn gate_results(step: &crate::adapter::Step, result: &Value) -> Vec<GateResult> {
    let mut out = Vec::new();
    for expect in &step.expects {
        let Expect::Gate(gate) = expect else {
            continue;
        };
        let docs = result_documents(result, std::slice::from_ref(&gate.doc));
        let text = docs.docs.get(&gate.doc).filter(|t| !t.trim().is_empty());
        let Some(text) = text else {
            out.push(GateResult {
                kind: gate.kind().to_string(),
                doc: gate.doc.clone(),
                verdict: "missing".into(),
                detail: "the result has no such document".into(),
                items: Vec::new(),
            });
            continue;
        };
        let (verdict, detail) = match gates::judge(gate, text) {
            Ok(notes) => ("passed".to_string(), notes.join(". ")),
            Err(why) => ("failed".to_string(), why),
        };
        let items: Vec<String> = match &gate.rule {
            GateRule::Cr { net_max } => gates::cr_findings(text)
                .into_iter()
                .filter(|f| f.blocking == Some(true) || f.net.is_some_and(|n| n > *net_max))
                .map(|f| match f.net {
                    Some(net) => format!("{} (Net {net:+})", f.line),
                    None => format!("{} (no Net)", f.line),
                })
                .collect(),
            GateRule::Qa { block, .. } => gates::qa_report(text)
                .issues
                .into_iter()
                .filter_map(|i| {
                    let severity = i.severity?;
                    let mark = if block.contains(&severity) {
                        "blocks"
                    } else {
                        "low"
                    };
                    Some(format!("{} [{severity}, {mark}] {}", i.id, i.title))
                })
                .collect(),
        };
        let more = items.len().saturating_sub(MAX_GATE_ITEMS);
        let mut items: Vec<String> = items
            .into_iter()
            .take(MAX_GATE_ITEMS)
            .map(|i| cut(&one_line(&i), 300))
            .collect();
        if more > 0 {
            items.push(format!("{more} more item(s)"));
        }
        out.push(GateResult {
            kind: gate.kind().to_string(),
            doc: gate.doc.clone(),
            verdict,
            detail,
            items,
        });
    }
    out
}

/// The lines of the section Outcome: the status, the end facts, the steps, the flags, and the
/// turn that resumed the session.
fn outcome_lines(ledger: &Ledger, row: &TurnRow, kind: Option<&str>) -> Result<Vec<String>> {
    let mut lines = Vec::new();
    let status = row.status.as_deref().unwrap_or("unknown");
    let mut first = format!("- status {status}");
    if let Some(reason) = &row.reason {
        first.push_str(&format!(", reason {reason}"));
    }
    if let Some(kind) = kind {
        first.push_str(&format!(", result {kind}"));
    }
    lines.push(first);
    let (from, to) = window(ledger, row)?;
    let events = match &from {
        Some(from) => {
            ledger.ticket_events_between(&row.ticket, &OUTCOME_EVENTS, from, to.as_deref())?
        }
        None => Vec::new(),
    };
    let detail = |e: &crate::ledger::EventRow| -> Value {
        e.detail
            .as_deref()
            .and_then(|d| serde_json::from_str(d).ok())
            .unwrap_or(Value::Null)
    };
    match &row.end_fact {
        Some(fact) if fact == "held" => lines.push("- end facts: held".into()),
        Some(fact) => {
            let found = events
                .iter()
                .filter(|e| e.kind == "end-fact")
                .map(&detail)
                .find(|d| d.get("turn").and_then(Value::as_str) == Some(row.id.as_str()))
                .and_then(|d| d.get("found").and_then(Value::as_str).map(str::to_string));
            match found {
                Some(found) => lines.push(format!(
                    "- end facts: {fact}: {}",
                    cut(&one_line(&found), 400)
                )),
                None => lines.push(format!("- end facts: {fact}")),
            }
        }
        None => {}
    }
    for event in &events {
        let d = detail(event);
        let text = |key: &str| d.get(key).and_then(Value::as_str).unwrap_or("").to_string();
        match event.kind.as_str() {
            "step" => lines.push(format!(
                "- next: {} -> {} ({})",
                text("from"),
                text("to"),
                text("how")
            )),
            "flag" => lines.push(format!(
                "- flag {}: {}",
                text("flag"),
                cut(&one_line(&text("reason")), 300)
            )),
            "ended" => lines.push(format!(
                "- the ticket ended{}",
                if d.get("merged").and_then(Value::as_bool) == Some(true) {
                    " with a merged pull request"
                } else {
                    ""
                }
            )),
            _ => {}
        }
    }
    if let Some(next) = resumed_by(ledger, row)? {
        lines.push(format!("- resumed by {next}"));
    }
    Ok(lines)
}

/// The front matter and the text of an entry file.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct EntryFile {
    pub id: String,
    pub ticket: String,
    pub step: String,
    pub agent: String,
    pub ended: Option<String>,
    pub path: PathBuf,
    pub text: String,
}

/// Reads an entry file. `None` when it cannot be read or has no `id`.
pub fn read_entry(path: &Path) -> Option<EntryFile> {
    let text = std::fs::read_to_string(path).ok()?;
    parse_entry_text(&text, path)
}

/// Reads the text of an entry file.
pub fn parse_entry_text(text: &str, path: &Path) -> Option<EntryFile> {
    let mut lines = text.lines();
    if lines.next()? != "---" {
        return None;
    }
    let mut entry = EntryFile {
        path: path.to_path_buf(),
        text: text.to_string(),
        ..EntryFile::default()
    };
    for line in lines {
        if line == "---" {
            break;
        }
        let Ok(ParsedLine::Entry(item)) = parse_entry(line) else {
            continue;
        };
        let value = item.value.as_text().map(str::to_string);
        match item.key.as_str() {
            "id" => entry.id = value.unwrap_or_default(),
            "ticket" => entry.ticket = value.unwrap_or_default(),
            "step" => entry.step = value.unwrap_or_default(),
            "agent" => entry.agent = value.unwrap_or_default(),
            "ended" => entry.ended = value,
            _ => {}
        }
    }
    (!entry.id.is_empty()).then_some(entry)
}

/// The body of the section `## <name>` of an entry, trimmed, or `None` when it has none.
pub fn section<'a>(text: &'a str, name: &str) -> Option<&'a str> {
    let heading = format!("\n## {name}\n");
    let start = text.find(&heading)? + heading.len();
    let rest = &text[start..];
    let end = rest.find("\n## ").unwrap_or(rest.len());
    let body = rest[..end].trim();
    (!body.is_empty()).then_some(body)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry() -> Entry {
        Entry {
            id: "20261116T143000Z-DEMO-12-build-1".into(),
            ticket: "DEMO-12".into(),
            step: "build".into(),
            agent: "builder".into(),
            model: Some("claude-opus-5-5".into()),
            started: Some("2026-11-16T14:30:00Z".into()),
            ended: Some("2026-11-16T14:41:12Z".into()),
            outcome: "held".into(),
            head: Some("abc1234".into()),
            files: vec!["calc.py".into(), "test calc.py".into()],
            cost: Some(0.25),
            summary: Some("Built subtract.".into()),
            notes: vec![Note {
                kind: "fix".into(),
                text: "Import subtract in calc.py.".into(),
                files: vec!["calc.py".into()],
                commit: Some("abc1234".into()),
                cites: vec!["20261110T100000Z-DEMO-7-build-1".into()],
            }],
            errors: vec!["- `make` failed (exit 2): boom".into()],
            gates: Vec::new(),
            answers: Vec::new(),
            outcome_lines: vec!["- status done, reason result, result result".into()],
        }
    }

    #[test]
    fn an_entry_renders_only_the_sections_with_content_and_reads_back() {
        let text = render(&entry());
        assert!(
            text.starts_with("---\nid: 20261116T143000Z-DEMO-12-build-1\nticket: DEMO-12\n"),
            "{text}"
        );
        assert!(
            text.contains("\nfiles: [calc.py, test calc.py]\n"),
            "{text}"
        );
        assert!(text.contains("\ncost: 0.2500\n"), "{text}");
        assert!(text.contains("\n## Notes\n\n- fix: Import subtract in calc.py. (files: calc.py; commit: abc1234; cites: 20261110T100000Z-DEMO-7-build-1)\n"), "{text}");
        assert!(!text.contains("## Gate results"), "{text}");
        assert!(!text.contains("## Answers"), "{text}");
        let read = parse_entry_text(&text, Path::new("/j/x.md")).unwrap();
        assert_eq!(read.id, "20261116T143000Z-DEMO-12-build-1");
        assert_eq!(read.ticket, "DEMO-12");
        assert_eq!(read.step, "build");
        assert_eq!(read.ended.as_deref(), Some("2026-11-16T14:41:12Z"));
        assert_eq!(
            section(&text, "Errors and fixes"),
            Some("- `make` failed (exit 2): boom")
        );
        assert_eq!(section(&text, "Summary"), Some("Built subtract."));
        assert_eq!(section(&text, "Answers"), None);
    }

    #[test]
    fn a_heading_in_the_summary_starts_no_section() {
        let mut e = entry();
        e.summary = Some("Built.\n## Errors and fixes\nnone".into());
        let text = render(&e);
        assert_eq!(
            section(&text, "Errors and fixes"),
            Some("- `make` failed (exit 2): boom")
        );
        assert!(text.contains("\n ## Errors and fixes\nnone\n"), "{text}");
    }

    #[test]
    fn notes_keep_only_known_kinds_with_text() {
        let result = json!({"kind": "result", "summary": "s", "notes": [
            {"kind": "fix", "text": "  Two\nlines. ", "files": ["a.rs", " "], "commit": " "},
            {"kind": "guess", "text": "no"},
            {"kind": "fact", "text": ""},
            {"kind": "dead-end", "text": "Retry did not help.", "cites": ["T1"]}
        ]});
        let notes = notes_of(&result);
        assert_eq!(notes.len(), 2);
        assert_eq!(notes[0].text, "Two lines.");
        assert_eq!(notes[0].files, ["a.rs"]);
        assert_eq!(notes[0].commit, None);
        assert_eq!(notes[1].cites, ["T1"]);
        assert!(notes_of(&json!({"kind": "result"})).is_empty());
    }

    #[test]
    fn the_gates_of_the_step_give_their_verdict_and_their_items() {
        let adapter = Adapter::parse(
            r#"
[[step]]
id = "check"
kind = "turn"
agent = "reviewer"
turn = "reader"
expects = [{ gate = "cr" }, { gate = "qa", doc = "qa.md" }, { gate = "qa", doc = "smoke.md" }]
next = "done"

[[step]]
id = "done"
kind = "end"
"#,
            Path::new("/a/toy"),
        )
        .unwrap();
        let step = adapter.step("check").unwrap();
        let cr = include_str!("../../tests/fixtures/gates/cr-fails.md");
        let qa = include_str!("../../tests/fixtures/gates/qa-low.md");
        let result = json!({"kind": "result", "summary": "s", "documents": [
            {"name": "cr.md", "content": cr}, {"name": "qa.md", "content": qa}]});
        let gates = gate_results(step, &result);
        assert_eq!(gates.len(), 3);
        assert_eq!(gates[0].kind, "cr");
        assert_eq!(gates[0].verdict, "failed");
        assert_eq!(
            gates[0].items,
            [
                "1. [MAJOR][BLOCKING] app/models/report_export.rb:42 (Net +11)",
                "3. [MINOR][FOLLOW-UP] spec/models/report_export_spec.rb:8 (Net +5.5)"
            ]
        );
        assert_eq!(gates[1].verdict, "passed");
        assert_eq!(
            gates[1].items,
            [
                "ISSUE-001 [low, low] The export button has a typo",
                "ISSUE-002 [low, low] The hover color of the export button differs"
            ]
        );
        assert_eq!(gates[2].verdict, "missing");
        let text = render(&Entry { gates, ..entry() });
        assert!(
            text.contains("\n## Gate results\n\n- gate cr (cr.md): failed. "),
            "{text}"
        );
        assert!(
            text.contains("\n- gate qa (smoke.md): missing. the result has no such document"),
            "{text}"
        );
    }

    #[test]
    fn an_answer_and_a_gate_render_in_one_place() {
        let mut e = entry();
        e.answers = vec![
            AnswerLine {
                question: "Q-0001".into(),
                text: "CSV or XLSX?".into(),
                answer: Some("CSV".into()),
                by: Some("user".into()),
                why: Some("Finance imports CSV.".into()),
                decision: Some("D-0001".into()),
                state: "answered".into(),
            },
            AnswerLine {
                question: "Q-0002".into(),
                text: "Which name?".into(),
                state: "asked".into(),
                ..AnswerLine::default()
            },
        ];
        e.gates = vec![GateResult {
            kind: "qa".into(),
            doc: "qa.md".into(),
            verdict: "passed".into(),
            detail: String::new(),
            items: vec!["ISSUE-002 [low, low] The label wraps".into()],
        }];
        let text = render(&e);
        assert!(
            text.contains(
                "- Q-0001 (D-0001): CSV or XLSX? Answer: CSV (by user). Why: Finance imports CSV."
            ),
            "{text}"
        );
        assert!(
            text.contains("- Q-0002: Which name? No answer yet (asked)."),
            "{text}"
        );
        assert!(
            text.contains("- gate qa (qa.md): passed\n  - ISSUE-002 [low, low] The label wraps"),
            "{text}"
        );
    }
}
