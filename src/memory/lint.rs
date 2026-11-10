//! The lint of the memory (section 9.2): each rule of a file, and the rules between files.
//!
//! The rules: the required keys and their values, the id equals the file name, the required
//! sections exist, each id in `evidence`, `sources`, and `supersedes` exists, a principle with
//! the status `candidate` or `active` names only decisions of a person as evidence (an imported
//! decision counts, because an import holds past answers of the person), and no decision of
//! the Judge is evidence for a principle.
//!
//! [`lint`] is pure: it reads a snapshot of the files (path -> text), so that the store can lint
//! the memory as it would be after a change, before it writes anything.

use std::collections::{BTreeMap, BTreeSet};

use serde::Serialize;

use super::doc::Doc;
use super::front::Value;
use super::model::{MemoryFile, MemoryId, Problem, Status, Writer, DECISIONS, PRINCIPLES};

/// The result of a lint of the whole memory.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct LintReport {
    /// The number of files that the lint read.
    pub files: usize,
    pub problems: Vec<Problem>,
}

impl LintReport {
    pub fn is_clean(&self) -> bool {
        self.problems.is_empty()
    }
}

/// What the rules between files know of one file.
struct Known {
    by: Option<Writer>,
}

/// Lints the files of a snapshot (path in the repo -> text). With `only`, it lints only those
/// paths, but it still finds the ids of each file of the snapshot. Problems come sorted by
/// path and line.
pub fn lint(files: &BTreeMap<String, String>, only: Option<&BTreeSet<String>>) -> Vec<Problem> {
    let docs: BTreeMap<&str, Option<Doc>> = files
        .iter()
        .map(|(path, text)| (path.as_str(), Doc::parse(text).ok()))
        .collect();
    let mut known: BTreeMap<MemoryId, Known> = BTreeMap::new();
    for (path, doc) in &docs {
        if let Some(id) = MemoryId::from_path(path) {
            let by = doc
                .as_ref()
                .and_then(|d| d.get("by"))
                .and_then(Value::as_text)
                .and_then(Writer::parse);
            known.insert(id, Known { by });
        }
    }
    let mut problems = Vec::new();
    for (path, text) in files {
        if only.is_some_and(|only| !only.contains(path)) {
            continue;
        }
        if let Err(name_problem) = check_name(path) {
            problems.push(name_problem);
            continue;
        }
        if let Err(file_problems) = MemoryFile::parse(path, text) {
            problems.extend(file_problems);
        }
        if let Some(Some(doc)) = docs.get(path.as_str()) {
            problems.extend(check_refs(path, doc, &known));
        }
    }
    problems.sort();
    problems.dedup();
    problems
}

/// The file must sit in its folder with an id as its name.
fn check_name(path: &str) -> Result<(), Problem> {
    if MemoryId::from_path(path).is_some() {
        return Ok(());
    }
    let example = if path.starts_with(&format!("{PRINCIPLES}/")) {
        "P-007.md"
    } else {
        "D-0142.md"
    };
    let reason = if path.starts_with(&format!("{DECISIONS}/"))
        || path.starts_with(&format!("{PRINCIPLES}/"))
    {
        format!(
            "the file name is not an id such as {example}. Rename it, or move it out of the memory"
        )
    } else {
        format!("a memory file belongs in {DECISIONS}/ or {PRINCIPLES}/")
    };
    Err(Problem {
        path: path.to_string(),
        line: 1,
        reason,
    })
}

fn check_refs(path: &str, doc: &Doc, known: &BTreeMap<MemoryId, Known>) -> Vec<Problem> {
    let mut problems = Vec::new();
    let mut problem = |key: &str, reason: String| {
        problems.push(Problem {
            path: path.to_string(),
            line: doc.key_line(key).unwrap_or(1),
            reason,
        });
    };
    let is_principle = path.starts_with(&format!("{PRINCIPLES}/"));
    let keys: &[&str] = if is_principle {
        &["evidence"]
    } else {
        &["sources", "supersedes"]
    };
    let status = doc
        .get("status")
        .and_then(Value::as_text)
        .and_then(Status::parse);
    for key in keys {
        for text in ids_of(doc.get(key)) {
            let Ok(id) = MemoryId::parse(&text) else {
                continue; // the rules of the file already name a bad id
            };
            let Some(target) = known.get(&id) else {
                problem(
                    key,
                    format!(
                        "`{key}` names {id}, but the memory has no file {}",
                        id.path()
                    ),
                );
                continue;
            };
            if *key != "evidence" || !id.is_decision() {
                continue;
            }
            match &target.by {
                Some(Writer::Judge) => problem(
                    key,
                    format!("`evidence` names {id}, an answer of the Judge. A decision of the Judge is never evidence for a principle"),
                ),
                // An import holds past answers of the person, so it counts as the person.
                Some(Writer::Person(_)) | Some(Writer::Import) => {}
                other if matches!(status, Some(Status::Candidate | Status::Active)) => {
                    let writer = other.as_ref().map_or("no valid writer".to_string(), |w| format!("the writer {w}"));
                    problem(
                        key,
                        format!("`evidence` names {id}, which has {writer}. A candidate or active principle needs decisions of a person, or imported answers of the person, as evidence"),
                    );
                }
                _ => {}
            }
        }
    }
    problems
}

/// The texts of a list value, or of a single text value.
fn ids_of(value: Option<&Value>) -> Vec<String> {
    match value {
        Some(Value::List(items)) => items
            .iter()
            .filter_map(|i| i.as_text())
            .map(|t| t.trim().to_string())
            .collect(),
        Some(v) => v
            .as_text()
            .map(|t| vec![t.trim().to_string()])
            .unwrap_or_default(),
        None => Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn decision(id: &str, by: &str) -> String {
        format!(
            "---\nid: {id}\nat: 2026-11-16T14:32:00Z\nby: {by}\nbasis: {}\nkind: product\narea: []\nstatus: active\nsources: []\nsupersedes: null\n---\n## Question\nQ?\n\n## Answer\nA\n",
            if by == "judge" { "inference" } else { "answer" }
        )
    }

    fn principle(id: &str, status: &str, evidence: &str) -> String {
        format!(
            "---\nid: {id}\nagent: judge\nkind: product\narea: []\nstatus: {status}\nby: user\nevidence: [{evidence}]\n---\n## Rule\nR\n"
        )
    }

    fn snapshot(files: &[(&str, String)]) -> BTreeMap<String, String> {
        files
            .iter()
            .map(|(p, t)| (p.to_string(), t.clone()))
            .collect()
    }

    fn reasons(problems: &[Problem]) -> Vec<String> {
        problems.iter().map(|p| p.to_string()).collect()
    }

    #[test]
    fn a_clean_memory_has_no_problem() {
        let files = snapshot(&[
            ("decisions/D-0001.md", decision("D-0001", "user")),
            (
                "principles/P-001.md",
                principle("P-001", "active", "D-0001"),
            ),
        ]);
        assert_eq!(lint(&files, None), Vec::<Problem>::new());
    }

    #[test]
    fn a_missing_id_is_named() {
        let mut d = decision("D-0002", "judge");
        d = d.replace("sources: []", "sources: [P-009, D-0001]");
        d = d.replace("supersedes: null", "supersedes: D-0007");
        let files = snapshot(&[
            ("decisions/D-0001.md", decision("D-0001", "user")),
            ("decisions/D-0002.md", d),
            (
                "principles/P-001.md",
                principle("P-001", "active", "D-0001, D-0042"),
            ),
        ]);
        let got = reasons(&lint(&files, None));
        assert_eq!(
            got,
            [
                "decisions/D-0002.md:9: `sources` names P-009, but the memory has no file principles/P-009.md",
                "decisions/D-0002.md:10: `supersedes` names D-0007, but the memory has no file decisions/D-0007.md",
                "principles/P-001.md:8: `evidence` names D-0042, but the memory has no file decisions/D-0042.md",
            ]
        );
    }

    #[test]
    fn evidence_must_come_from_a_person_or_an_import() {
        let files = snapshot(&[
            ("decisions/D-0001.md", decision("D-0001", "judge")),
            ("decisions/D-0002.md", decision("D-0002", "import")),
            ("decisions/D-0003.md", decision("D-0003", "user")),
            (
                "principles/P-001.md",
                principle("P-001", "candidate", "D-0001, D-0003"),
            ),
            (
                "principles/P-002.md",
                principle("P-002", "active", "D-0002"),
            ),
            (
                "principles/P-003.md",
                principle("P-003", "retired", "D-0001, D-0002")
                    .replace("## Rule\nR\n", "## Rule\nR\n\n## Retired\nOld.\n"),
            ),
            ("principles/P-004.md", principle("P-004", "active", "P-001")),
        ]);
        let got = reasons(&lint(&files, None));
        // P-002 is fine: an imported decision is a past answer of the person.
        assert_eq!(got.len(), 3, "{got:#?}");
        assert!(
            got[0].starts_with(
                "principles/P-001.md:8: `evidence` names D-0001, an answer of the Judge"
            ),
            "{got:#?}"
        );
        // A retired principle also never keeps an answer of the Judge as evidence.
        assert!(
            got[1].starts_with(
                "principles/P-003.md:8: `evidence` names D-0001, an answer of the Judge"
            ),
            "{got:#?}"
        );
        assert!(
            got[2].starts_with(
                "principles/P-004.md:8: `evidence` names `P-001`, which is not a decision id"
            ),
            "{got:#?}"
        );
    }

    #[test]
    fn names_must_be_ids_and_only_limits_the_files() {
        let files = snapshot(&[
            ("decisions/notes.md", "anything".into()),
            ("decisions/D-0001.md", decision("D-0001", "user")),
            (
                "principles/P-001.md",
                principle("P-001", "active", "D-0009"),
            ),
        ]);
        let got = reasons(&lint(&files, None));
        assert_eq!(got.len(), 2, "{got:#?}");
        assert!(got[0].contains("not an id such as D-0142.md"), "{got:#?}");
        let only: BTreeSet<String> = ["decisions/D-0001.md".to_string()].into();
        assert!(lint(&files, Some(&only)).is_empty());
    }

    #[test]
    fn a_broken_file_still_counts_as_present() {
        let files = snapshot(&[
            ("decisions/D-0001.md", "---\nid: [\n---\n".into()),
            (
                "principles/P-001.md",
                principle("P-001", "active", "D-0001"),
            ),
        ]);
        let got = reasons(&lint(&files, None));
        assert_eq!(got.len(), 2, "{got:#?}");
        assert!(got[0].starts_with("decisions/D-0001.md:2:"), "{got:#?}");
        assert!(got[1].contains("which has no valid writer"), "{got:#?}");
    }
}
