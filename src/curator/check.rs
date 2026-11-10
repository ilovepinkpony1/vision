//! The checks of a change of the Curator before the build of its proposal (sections 9.3 and
//! 9.4). A change that fails a check is dropped, with a reason that the review shows:
//!
//! - It names an unknown file: an edit, a merge, or a retirement of a file that the memory does
//!   not have, an add whose path is not `principles/NEW-<n>.md`, or evidence that is not a
//!   decision of the memory.
//! - Its evidence names a decision with the writer `judge`, for a change that writes a principle.
//! - It adds a new principle with fewer than 2 active decisions of the person (or of an import)
//!   from 2 different tickets as evidence.
//! - The file that it writes fails the lint.
//!
//! [`check`] is pure: it reads a snapshot of the files of `main` (path -> text). The store tests
//! the change again when it builds the proposal.

use std::collections::{BTreeMap, BTreeSet};

use crate::memory::doc::Doc;
use crate::memory::front::Value;
use crate::memory::lint::lint;
use crate::memory::{
    CuratorChange, Decision, MemoryFile, MemoryId, Op, PrincipleId, Status, Writer,
};

/// What the checks know of the memory: its files and its decisions.
#[derive(Clone, Debug)]
pub struct Base<'a> {
    files: &'a BTreeMap<String, String>,
    decisions: BTreeMap<String, Decision>,
    next_principle: u32,
}

impl<'a> Base<'a> {
    /// The snapshot of the files of the memory (path in the repo -> text).
    pub fn new(files: &'a BTreeMap<String, String>) -> Base<'a> {
        let mut decisions = BTreeMap::new();
        let mut max = 0;
        for (path, text) in files {
            match MemoryId::from_path(path) {
                Some(MemoryId::Decision(id)) => {
                    if let Ok(MemoryFile::Decision(d)) = MemoryFile::parse(path, text) {
                        decisions.insert(id.to_string(), d);
                    }
                }
                Some(MemoryId::Principle(id)) => max = max.max(id.number()),
                None => {}
            }
        }
        Base {
            files,
            decisions,
            next_principle: max + 1,
        }
    }
}

/// The path of an existing file, from an id such as `P-007` or a path.
fn existing(base: &Base<'_>, target: Option<&str>) -> Result<String, String> {
    let target = target.map(str::trim).unwrap_or("");
    let path = match MemoryId::parse(target) {
        Ok(id) => id.path(),
        Err(_) => target.to_string(),
    };
    if MemoryId::from_path(&path).is_none() || !base.files.contains_key(&path) {
        return Err(format!(
            "it names `{target}`, which is not a file of the memory"
        ));
    }
    Ok(path)
}

fn is_new_path(path: &str) -> bool {
    path.strip_prefix("principles/NEW-")
        .and_then(|rest| rest.strip_suffix(".md"))
        .is_some_and(|n| {
            !n.is_empty()
                && n.chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
        })
}

fn content(change: &CuratorChange) -> Result<&str, String> {
    match change.content.as_deref() {
        Some(text) if !text.trim().is_empty() => Ok(text),
        _ => Err(format!(
            "an {} needs the full new file in `content`",
            change.op.as_str()
        )),
    }
}

fn parse(text: &str) -> Result<Doc, String> {
    Doc::parse(text).map_err(|e| {
        format!(
            "the content is not a memory file: line {}: {}",
            e.line, e.message
        )
    })
}

/// The ids of the `evidence` key of a file.
fn evidence_key(doc: &Doc) -> Vec<String> {
    match doc.get("evidence") {
        Some(Value::List(items)) => items
            .iter()
            .filter_map(|i| i.as_text())
            .map(|t| t.trim().to_string())
            .filter(|t| !t.is_empty())
            .collect(),
        Some(value) => value
            .as_text()
            .map(|t| vec![t.trim().to_string()])
            .unwrap_or_default(),
        None => Vec::new(),
    }
}

/// The change with each item of `evidence` cut down to the decision id that it starts with.
/// A real Curator session wrote `D-0001 (DEMO-7): the report export is CSV` for an item,
/// and the id is all that the check needs. An item that starts with no id stays as it is,
/// so that the check names it.
pub fn clean_evidence(change: &CuratorChange) -> CuratorChange {
    let mut out = change.clone();
    out.evidence = change
        .evidence
        .iter()
        .map(|item| leading_decision_id(item).unwrap_or_else(|| item.trim().to_string()))
        .collect();
    out
}

/// The change as the check and the store must see it. A real Curator session gives a looser
/// text than the format asks for, so this step repairs what has one safe repair: the last line
/// end of the file text, the evidence items, and the keys of a new principle.
pub fn tidy(change: &CuratorChange) -> CuratorChange {
    complete_new_principle(&clean_evidence(&end_with_newline(change)))
}

/// The change with a line end after the last line of its file text.
pub fn end_with_newline(change: &CuratorChange) -> CuratorChange {
    let mut out = change.clone();
    if let Some(content) = out.content.as_mut() {
        if !content.is_empty() && !content.ends_with('\n') {
            content.push('\n');
        }
    }
    out
}

/// The change with the two front matter keys that a real Curator session left out of a new
/// principle: `agent` gets the value `all`, and `kind` gets `null`. Each key that the Curator
/// wrote stays. Each other change, and content that does not parse, stays as it is.
pub fn complete_new_principle(change: &CuratorChange) -> CuratorChange {
    let mut out = change.clone();
    let new =
        matches!(change.op, Op::Add | Op::Merge) && change.path.as_deref().is_some_and(is_new_path);
    let Some(Ok(mut doc)) = change.content.as_deref().filter(|_| new).map(Doc::parse) else {
        return out;
    };
    let mut changed = false;
    if doc.get("agent").is_none() {
        doc.set("agent", Value::str("all"));
        changed = true;
    }
    if doc.get("kind").is_none() {
        doc.set("kind", Value::null());
        changed = true;
    }
    if changed {
        out.content = Some(doc.to_text());
    }
    out
}

/// `D-0001` from `D-0001 (DEMO-7): ...`, or `None` if the text does not start with a decision id.
fn leading_decision_id(item: &str) -> Option<String> {
    let text = item.trim_start().trim_start_matches(['`', '[', '(']);
    let rest = text.strip_prefix("D-")?;
    let digits = rest.chars().take_while(char::is_ascii_digit).count();
    if digits == 0 {
        return None;
    }
    // The id must end there: `D-0001x` is not an id with more text.
    match rest[digits..].chars().next() {
        Some(next) if next.is_alphanumeric() || next == '-' => None,
        _ => Some(format!("D-{}", &rest[..digits])),
    }
}

/// The change with each decision of its `evidence` also in the `evidence` key of the new
/// principle that it writes (an add, or a merge into a new file), so that the file keeps its
/// evidence. Each other change, and content that does not parse, stays as it is.
pub fn with_evidence(change: &CuratorChange) -> CuratorChange {
    let mut out = change.clone();
    let new =
        matches!(change.op, Op::Add | Op::Merge) && change.path.as_deref().is_some_and(is_new_path);
    let Some(Ok(mut doc)) = change.content.as_deref().filter(|_| new).map(Doc::parse) else {
        return out;
    };
    let mut ids = evidence_key(&doc);
    let before = ids.len();
    for id in &change.evidence {
        let id = id.trim().to_string();
        if !id.is_empty() && !ids.contains(&id) {
            ids.push(id);
        }
    }
    if ids.len() != before {
        doc.set("evidence", Value::list(ids));
        out.content = Some(doc.to_text());
    }
    out
}

/// Tests one change against the memory. The error is the reason to drop it.
pub fn check(change: &CuratorChange, base: &Base<'_>) -> Result<(), String> {
    if change.reason.trim().is_empty() {
        return Err("it has no reason".into());
    }
    // The file that the change writes, with its text as the store will write it, and whether
    // it is a new principle.
    let mut written: Option<(String, Doc, bool)> = None;
    match change.op {
        Op::Question => {
            if change
                .question
                .as_deref()
                .is_none_or(|q| q.trim().is_empty())
            {
                return Err("a question needs the text in `question`".into());
            }
        }
        Op::Add => {
            let path = change.path.as_deref().unwrap_or("");
            if !is_new_path(path) {
                return Err(format!(
                    "it names `{path}`, but an add names a new path such as principles/NEW-1.md"
                ));
            }
            let doc = parse(content(change)?)?;
            written = Some((path.to_string(), doc, true));
        }
        Op::Edit => {
            let path = existing(base, change.path.as_deref())?;
            let doc = parse(content(change)?)?;
            written = Some((path, doc, false));
        }
        Op::Merge => {
            let path = change.path.as_deref().unwrap_or("");
            let new = is_new_path(path);
            let target = if new {
                path.to_string()
            } else {
                existing(base, Some(path))?
            };
            if change.removes.is_empty() {
                return Err("a merge lists the files that it replaces in `removes`".into());
            }
            for remove in &change.removes {
                existing(base, Some(remove))?;
            }
            let doc = parse(content(change)?)?;
            written = Some((target, doc, new));
        }
        Op::Retire => {
            existing(base, change.target.as_deref().or(change.path.as_deref()))?;
        }
    }
    for id in &change.evidence {
        if !base.decisions.contains_key(id.trim()) {
            return Err(format!(
                "the evidence names `{}`, which is not a decision of the memory",
                id.trim()
            ));
        }
    }
    let Some((path, mut doc, new)) = written else {
        return Ok(());
    };
    if new || path.starts_with("principles/") {
        let mut evidence: Vec<String> = change
            .evidence
            .iter()
            .map(|e| e.trim().to_string())
            .collect();
        for id in evidence_key(&doc) {
            if !evidence.contains(&id) {
                evidence.push(id);
            }
        }
        if let Some(id) = evidence.iter().find(|id| {
            base.decisions
                .get(*id)
                .is_some_and(|d| d.by == Writer::Judge)
        }) {
            return Err(format!(
                "the evidence names {id}, an answer of the Judge. A decision of the Judge is never evidence for a principle"
            ));
        }
        if new {
            let people: Vec<&Decision> = evidence
                .iter()
                .filter_map(|id| base.decisions.get(id))
                .filter(|d| {
                    d.status == Status::Active && matches!(d.by, Writer::Person(_) | Writer::Import)
                })
                .collect();
            let tickets: BTreeSet<String> = people
                .iter()
                .filter_map(|d| d.ticket.as_deref())
                .map(|t| t.trim().to_uppercase())
                .filter(|t| !t.is_empty())
                .collect();
            if people.len() < 2 || tickets.len() < 2 {
                return Err(format!(
                    "a new principle needs at least 2 active decisions of the person from 2 different tickets as evidence, but it names {} from {} ticket(s)",
                    people.len(),
                    tickets.len()
                ));
            }
        }
    }
    let path = if new {
        let Some(id) = PrincipleId::new(base.next_principle) else {
            return Err("no free principle id".into());
        };
        doc.set("id", Value::str(id.to_string()));
        doc.set("status", Value::str(Status::Candidate.as_str()));
        doc.set("by", Value::str(Writer::Curator.name()));
        id.path()
    } else {
        path
    };
    let mut overlay = base.files.clone();
    overlay.insert(path.clone(), doc.to_text());
    let only: BTreeSet<String> = [path].into();
    let problems = lint(&overlay, Some(&only));
    if !problems.is_empty() {
        let lines: Vec<String> = problems.iter().map(|p| p.to_string()).collect();
        return Err(format!(
            "it would leave an invalid file: {}",
            lines.join("; ")
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn decision(id: &str, by: &str, ticket: &str, status: &str) -> String {
        let basis = if by == "judge" { "inference" } else { "answer" };
        format!(
            "---\nid: {id}\nat: 2026-11-16T14:32:00Z\nby: {by}\nbasis: {basis}\nticket: {ticket}\nkind: product\narea: [exports]\nstatus: {status}\nsources: []\nsupersedes: null\n---\n## Question\nMust the export be CSV?\n\n## Answer\nCSV\n\n## Why\nFinance.\n"
        )
    }

    #[test]
    fn an_evidence_item_is_cut_down_to_its_decision_id() {
        let change = CuratorChange {
            evidence: vec![
                "D-0001 (DEMO-7): the report export is CSV. Why: finance.".into(),
                " `D-0021`".into(),
                "D-0003".into(),
                "the decision about dates".into(),
                "D-12x".into(),
            ],
            ..add(&[])
        };
        assert_eq!(
            clean_evidence(&change).evidence,
            [
                "D-0001",
                "D-0021",
                "D-0003",
                "the decision about dates",
                "D-12x"
            ]
        );
    }

    #[test]
    fn a_file_text_gets_its_last_line_end() {
        let change = CuratorChange {
            content: Some(principle("NEW-1", "D-0001").trim_end().to_string()),
            ..add(&[])
        };
        assert!(!change.content.as_deref().unwrap().ends_with('\n'));
        let text = tidy(&change).content.unwrap();
        assert!(text.ends_with("## Not when\n"), "{text:?}");
        // A text that ends with a line end stays the same.
        let full = CuratorChange {
            content: Some(principle("NEW-1", "D-0001")),
            ..add(&[])
        };
        assert_eq!(end_with_newline(&full).content, full.content);
        assert_eq!(tidy(&full).content, full.content);
    }

    #[test]
    fn a_new_principle_gets_the_keys_that_the_curator_left_out() {
        let bare =
            "---\nid: NEW-1\nstatus: candidate\n---\n## Rule\nR\n\n## Because\nB\n\n## Not when\n";
        let change = CuratorChange {
            content: Some(bare.into()),
            ..add(&[])
        };
        let text = complete_new_principle(&change).content.unwrap();
        assert!(text.contains("agent: all\n"), "{text}");
        assert!(text.contains("kind: null\n"), "{text}");
        // A key that the Curator wrote stays.
        let full = CuratorChange {
            content: Some(principle("NEW-1", "D-0001")),
            ..add(&[])
        };
        assert_eq!(complete_new_principle(&full).content, full.content);
    }

    fn principle(id: &str, evidence: &str) -> String {
        format!(
            "---\nid: {id}\nagent: judge\nkind: product\narea: [exports]\nstatus: active\nby: user\nevidence: [{evidence}]\nuses: {{used: 0, agreed: 0, corrected: 0}}\n---\n## Rule\nWhen a report is exported, make sure that it is CSV.\n\n## Because\nFinance.\n\n## Not when\n"
        )
    }

    fn memory() -> BTreeMap<String, String> {
        [
            (
                "decisions/D-0001.md",
                decision("D-0001", "user", "DEMO-12", "active"),
            ),
            (
                "decisions/D-0002.md",
                decision("D-0002", "user", "DEMO-13", "active"),
            ),
            (
                "decisions/D-0003.md",
                decision("D-0003", "judge", "DEMO-14", "active"),
            ),
            (
                "decisions/D-0004.md",
                decision("D-0004", "user", "DEMO-12", "active"),
            ),
            (
                "decisions/D-0005.md",
                decision("D-0005", "import", "DEMO-15", "active"),
            ),
            (
                "decisions/D-0006.md",
                decision("D-0006", "user", "DEMO-16", "superseded"),
            ),
            ("principles/P-001.md", principle("P-001", "")),
            ("principles/P-002.md", principle("P-002", "")),
        ]
        .into_iter()
        .map(|(p, t)| (p.to_string(), t))
        .collect()
    }

    fn change(op: Op) -> CuratorChange {
        CuratorChange {
            op,
            path: None,
            content: None,
            removes: vec![],
            target: None,
            question: None,
            evidence: vec![],
            reason: "A reason.".into(),
        }
    }

    fn add(evidence: &[&str]) -> CuratorChange {
        CuratorChange {
            path: Some("principles/NEW-1.md".into()),
            content: Some(principle("NEW-1", &evidence.join(", "))),
            evidence: evidence.iter().map(|e| e.to_string()).collect(),
            ..change(Op::Add)
        }
    }

    fn reason(change: &CuratorChange) -> String {
        let files = memory();
        check(change, &Base::new(&files)).unwrap_err()
    }

    fn passes(change: &CuratorChange) {
        let files = memory();
        assert_eq!(check(change, &Base::new(&files)), Ok(()));
    }

    #[test]
    fn valid_changes_pass() {
        passes(&add(&["D-0001", "D-0002"]));
        passes(&add(&["D-0001", "D-0005"]));
        passes(&CuratorChange {
            path: Some("principles/P-001.md".into()),
            content: Some(principle("P-001", "D-0001")),
            ..change(Op::Edit)
        });
        passes(&CuratorChange {
            path: Some("principles/P-001.md".into()),
            content: Some(principle("P-001", "")),
            removes: vec!["P-002".into()],
            ..change(Op::Merge)
        });
        passes(&CuratorChange {
            target: Some("P-002".into()),
            evidence: vec!["D-0003".into()],
            ..change(Op::Retire)
        });
        passes(&CuratorChange {
            question: Some("Do internal users need XLSX?".into()),
            ..change(Op::Question)
        });
    }

    #[test]
    fn an_unknown_file_is_dropped() {
        let edit = CuratorChange {
            path: Some("principles/P-009.md".into()),
            content: Some(principle("P-009", "")),
            ..change(Op::Edit)
        };
        assert!(reason(&edit).contains("`principles/P-009.md`, which is not a file"));
        let retire = CuratorChange {
            target: Some("P-042".into()),
            ..change(Op::Retire)
        };
        assert!(reason(&retire).contains("`P-042`, which is not a file"));
        let merge = CuratorChange {
            path: Some("principles/P-001.md".into()),
            content: Some(principle("P-001", "")),
            removes: vec!["principles/P-077.md".into()],
            ..change(Op::Merge)
        };
        assert!(reason(&merge).contains("P-077"));
        let archive = CuratorChange {
            path: Some("ARCHIVE.md".into()),
            content: Some("x".into()),
            ..change(Op::Edit)
        };
        assert!(reason(&archive).contains("not a file of the memory"));
        let mut bad_path = add(&["D-0001", "D-0002"]);
        bad_path.path = Some("principles/P-003.md".into());
        assert!(reason(&bad_path).contains("principles/NEW-1.md"));
        let mut bad_evidence = add(&["D-0001", "D-0002"]);
        bad_evidence.evidence.push("D-0099".into());
        assert!(reason(&bad_evidence).contains("`D-0099`, which is not a decision"));
    }

    #[test]
    fn a_new_principle_needs_two_decisions_of_two_tickets() {
        let one = reason(&add(&["D-0001"]));
        assert!(one.contains("at least 2 active decisions"), "{one}");
        assert!(one.contains("names 1 from 1 ticket(s)"), "{one}");
        let same_ticket = reason(&add(&["D-0001", "D-0004"]));
        assert!(
            same_ticket.contains("names 2 from 1 ticket(s)"),
            "{same_ticket}"
        );
        let superseded = reason(&add(&["D-0001", "D-0006"]));
        assert!(
            superseded.contains("names 1 from 1 ticket(s)"),
            "{superseded}"
        );
        // The file's own evidence key counts too.
        let mut from_file = add(&["D-0001"]);
        from_file.content = Some(principle("NEW-1", "D-0001, D-0002"));
        passes(&from_file);
    }

    #[test]
    fn evidence_of_the_judge_is_dropped() {
        let judged = reason(&add(&["D-0001", "D-0002", "D-0003"]));
        assert!(
            judged.contains("D-0003, an answer of the Judge"),
            "{judged}"
        );
        let edit = CuratorChange {
            path: Some("principles/P-001.md".into()),
            content: Some(principle("P-001", "")),
            evidence: vec!["D-0003".into()],
            ..change(Op::Edit)
        };
        assert!(reason(&edit).contains("an answer of the Judge"));
    }

    #[test]
    fn a_file_that_fails_the_lint_is_dropped() {
        let edit = CuratorChange {
            path: Some("principles/P-001.md".into()),
            content: Some(principle("P-001", "").replace("status: active", "status: maybe")),
            ..change(Op::Edit)
        };
        let why = reason(&edit);
        assert!(
            why.starts_with("it would leave an invalid file: principles/P-001.md"),
            "{why}"
        );
        let mut no_rule = add(&["D-0001", "D-0002"]);
        no_rule.content = Some(principle("NEW-1", "D-0001, D-0002").replace(
            "## Rule\nWhen a report is exported, make sure that it is CSV.\n\n",
            "",
        ));
        let why = reason(&no_rule);
        assert!(why.contains("principles/P-003.md"), "{why}");
        let broken = CuratorChange {
            content: Some("---\nid: [\n---\n".into()),
            ..add(&["D-0001", "D-0002"])
        };
        assert!(reason(&broken).starts_with("the content is not a memory file"));
    }

    #[test]
    fn a_new_principle_keeps_the_evidence_of_its_change_in_its_file() {
        let mut short = add(&["D-0001", "D-0002"]);
        short.content = Some(principle("NEW-1", "D-0001"));
        let out = with_evidence(&short);
        assert!(
            out.content
                .as_deref()
                .unwrap()
                .contains("evidence: [D-0001, D-0002]"),
            "{:?}",
            out.content
        );
        let full = add(&["D-0001", "D-0002"]);
        assert_eq!(with_evidence(&full), full);
        let edit = CuratorChange {
            path: Some("principles/P-001.md".into()),
            content: Some(principle("P-001", "")),
            evidence: vec!["D-0001".into()],
            ..change(Op::Edit)
        };
        assert_eq!(with_evidence(&edit), edit);
    }

    #[test]
    fn a_change_needs_its_parts() {
        let mut no_reason = add(&["D-0001", "D-0002"]);
        no_reason.reason = "  ".into();
        assert_eq!(reason(&no_reason), "it has no reason");
        assert!(reason(&change(Op::Question)).contains("`question`"));
        let no_content = CuratorChange {
            path: Some("principles/P-001.md".into()),
            ..change(Op::Edit)
        };
        assert!(reason(&no_content).contains("`content`"));
        let no_removes = CuratorChange {
            path: Some("principles/P-001.md".into()),
            content: Some(principle("P-001", "")),
            ..change(Op::Merge)
        };
        assert!(reason(&no_removes).contains("`removes`"));
    }
}
