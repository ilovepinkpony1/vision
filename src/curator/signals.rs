//! The signals of the Curator (section 9.4): what happened since the last Curator run that
//! ended well, from the ledger and the memory repo, as one JSON file.
//!
//! - The new decisions (the decision files that `main` gained), and those of them with no reason.
//! - Each proposal of the Judge that the person changed, and each correction of the person.
//! - The cards that the person marked as needless.
//! - The difference between the plan of the agent (`plan.md`) and the plan that the person
//!   approved (`approved-plan.md`) of each ticket folder.
//! - The rows of `review_rows`: the review comments of people with their verdicts.
//! - Two active files that name the same area and kind, as a hint: the Curator decides whether
//!   they disagree. A pair needs a principle, or two decisions with the same question, and at
//!   least one file that changed since the last run.
//! - The changes of earlier proposals that the person rejected, and the questions of the
//!   Curator that the person answered in the review.
//!
//! Text that came from outside (the name of a reviewer, the text of a plan) is wrapped as
//! `{"untrusted": "..."}`. A [`Mark`] holds where the next collection starts: the time, the
//! commit of `main`, and the last ids of `human` and `review_rows`.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use ::time::OffsetDateTime;
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use super::records::{HUMAN_ACCEPT, HUMAN_REJECT};
use crate::ctx::Ctx;
use crate::ledger::Ledger;
use crate::memory::archive::removed_lines;
use crate::memory::{Memory, MemoryFile, Status, Writer};
use crate::paths::TicketKey;
use crate::questions::{needless_text, Question};
use crate::util::git;
use crate::util::time;

/// The most entries of one list. The count of the others goes to `more`.
pub const LIMIT: usize = 100;

/// Where a collection of the signals starts: the end of the last good run.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Mark {
    /// When the signals were collected, RFC 3339 UTC.
    pub at: String,
    /// The commit of `main` of the memory then.
    pub memory_head: Option<String>,
    /// The highest id of `human` then.
    pub human_id: i64,
    /// The highest row id of `review_rows` then.
    pub review_rowid: i64,
}

/// The signals file of one Curator turn.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct Signals {
    /// The time of the last good run, or `None` before the first one.
    pub since: Option<String>,
    /// When the signals were collected.
    pub until: String,
    pub new_decisions: Vec<DecisionSignal>,
    pub no_reason: Vec<DecisionSignal>,
    pub changed_proposals: Vec<ProposalSignal>,
    pub corrections: Vec<CorrectionSignal>,
    pub needless: Vec<NeedlessSignal>,
    pub plans: Vec<PlanSignal>,
    pub reviews: Vec<ReviewSignal>,
    pub overlaps: Vec<Overlap>,
    pub rejected: Vec<RejectedSignal>,
    pub answered_questions: Vec<AnsweredSignal>,
    /// For each list that was cut at [`LIMIT`]: the number of entries left out.
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub more: BTreeMap<String, usize>,
}

/// A decision file.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct DecisionSignal {
    pub id: String,
    pub by: String,
    pub ticket: Option<String>,
    pub kind: Option<String>,
    pub area: Vec<String>,
    pub question: String,
    pub answer: String,
    /// Empty when the person gave no reason.
    pub why: String,
}

/// A proposal of the Judge that the person changed.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct ProposalSignal {
    pub card: String,
    pub ticket: String,
    pub agent: String,
    pub question: String,
    /// What the Judge proposed.
    pub proposal: String,
    /// What the person answered instead.
    pub answer: String,
    pub why: String,
    pub decision: Option<String>,
}

/// A correction of an answer of the Judge.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct CorrectionSignal {
    pub card: String,
    pub ticket: Option<String>,
    pub question: String,
    /// The answer of the Judge.
    pub judge: String,
    /// The right answer of the person.
    pub answer: String,
    pub why: String,
    /// The decision of the person that replaced the one of the Judge.
    pub decision: Option<String>,
}

/// A card that the person marked as needless.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct NeedlessSignal {
    pub card: String,
    pub ticket: String,
    pub agent: String,
    pub question: String,
    /// `code`, `ticket`, `memory`, or `no-matter`.
    pub reason: String,
    pub means: String,
}

/// The lines that the person changed in the plan of the agent.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct PlanSignal {
    pub ticket: String,
    /// The lines of `plan.md` that `approved-plan.md` does not have, as untrusted text.
    pub removed: Value,
    /// The lines of `approved-plan.md` that `plan.md` does not have, as untrusted text.
    pub added: Value,
}

/// One review comment of a person and its verdicts.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct ReviewSignal {
    pub round: Option<String>,
    pub comment: Option<i64>,
    /// The reviewer, as untrusted text.
    pub reviewer: Value,
    pub verdict: Option<String>,
    pub fix: Option<i64>,
    pub verdict_final: Option<String>,
    pub fix_final: Option<i64>,
}

/// Two active files that name the same area and kind.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Overlap {
    pub files: [String; 2],
    pub kind: String,
    /// The area nouns that both name.
    pub area: Vec<String>,
    /// The rule of a principle, or the question of a decision, of each file.
    pub texts: [String; 2],
}

/// A change of an earlier proposal that the person rejected.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct RejectedSignal {
    pub proposal: String,
    pub change: Value,
    /// Why the person rejected it. Empty when the person gave no reason.
    pub why: String,
    pub at: String,
}

/// A question of the Curator that the person answered in the review.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct AnsweredSignal {
    pub proposal: String,
    pub question: String,
    pub answer: String,
    pub at: String,
}

impl Signals {
    /// True when nothing happened, so that a Curator turn has nothing to read.
    pub fn is_empty(&self) -> bool {
        self.counts().is_empty()
    }

    /// The number of entries of each list that has some, with the left out ones.
    pub fn counts(&self) -> BTreeMap<String, usize> {
        let mut out = BTreeMap::new();
        let lists = [
            ("new_decisions", self.new_decisions.len()),
            ("no_reason", self.no_reason.len()),
            ("changed_proposals", self.changed_proposals.len()),
            ("corrections", self.corrections.len()),
            ("needless", self.needless.len()),
            ("plans", self.plans.len()),
            ("reviews", self.reviews.len()),
            ("overlaps", self.overlaps.len()),
            ("rejected", self.rejected.len()),
            ("answered_questions", self.answered_questions.len()),
        ];
        for (name, n) in lists {
            let n = n + self.more.get(name).copied().unwrap_or(0);
            if n > 0 {
                out.insert(name.to_string(), n);
            }
        }
        out
    }

    /// One line for people, such as `3 new decisions, 1 correction`.
    pub fn describe(&self) -> String {
        let counts = self.counts();
        if counts.is_empty() {
            return "no signal".into();
        }
        counts
            .iter()
            .map(|(name, n)| format!("{n} {}", name.replace('_', " ")))
            .collect::<Vec<_>>()
            .join(", ")
    }
}

/// Text that came from outside, for a session that must read it as data.
pub fn untrusted(text: &str) -> Value {
    json!({ "untrusted": text })
}

/// The signals and the mark where the next collection starts.
#[derive(Clone, Debug)]
pub struct Collected {
    pub signals: Signals,
    pub mark: Mark,
}

/// Collects the signals since `since` (the mark of the last good run), or since the start.
pub fn collect(
    ctx: &Ctx,
    ledger: &Ledger,
    memory: &Memory,
    since: Option<&Mark>,
    now: OffsetDateTime,
) -> Result<Collected> {
    let mark = Mark {
        at: time::rfc3339(now),
        memory_head: memory.head().ok(),
        human_id: ledger.max_human_id()?,
        review_rowid: ledger.max_review_rowid()?,
    };
    let since_at = since.map(|m| m.at.as_str()).filter(|a| !a.is_empty());
    let mut signals = Signals {
        since: since_at.map(str::to_string),
        until: mark.at.clone(),
        ..Signals::default()
    };

    let (texts, _) = memory.snapshot()?;
    let files: Vec<MemoryFile> = texts
        .iter()
        .filter_map(|(path, text)| MemoryFile::parse(path, text).ok())
        .collect();
    let changes = memory_changes(memory, since.and_then(|m| m.memory_head.as_deref()));
    let decisions: Vec<DecisionSignal> = files
        .iter()
        .filter_map(|f| match f {
            MemoryFile::Decision(d) => Some(d),
            MemoryFile::Principle(_) => None,
        })
        .filter(|d| changes.added(&d.path))
        .map(|d| DecisionSignal {
            id: d.id.to_string(),
            by: d.by.name().to_string(),
            ticket: d.ticket.clone(),
            kind: d.kind.map(|k| k.to_string()),
            area: d.area.clone(),
            question: d.question.trim().to_string(),
            answer: d.answer.trim().to_string(),
            why: d.why.trim().to_string(),
        })
        .collect();
    let without_reason = no_reason(&files, &changes);
    signals.new_decisions = cap(decisions, "new_decisions", &mut signals.more);
    signals.no_reason = cap(without_reason, "no_reason", &mut signals.more);

    let mut changed = Vec::new();
    for id in ledger.changed_proposals_since(since_at)? {
        let Ok(q) = Question::find(ledger, &id) else {
            continue;
        };
        let proposal = q
            .view
            .judge
            .as_ref()
            .and_then(|e| e.reply(&q.card))
            .map(|r| r.text(&q.card))
            .unwrap_or_default();
        let answer = q.answer.clone().unwrap_or_default();
        changed.push(ProposalSignal {
            card: q.row.id.clone(),
            ticket: q.row.ticket.clone(),
            agent: q.agent.clone(),
            question: q.card.text.trim().to_string(),
            proposal,
            answer: answer.text,
            why: answer.why,
            decision: q.row.decision.clone(),
        });
    }
    signals.changed_proposals = cap(changed, "changed_proposals", &mut signals.more);

    let human_after = since.map_or(0, |m| m.human_id);
    let mut corrections = Vec::new();
    for (_, row) in ledger.human_rows_after(human_after, "correct")? {
        let found = Question::find(ledger, &row.object).ok();
        let question = found
            .as_ref()
            .map(|q| q.card.text.trim().to_string())
            .unwrap_or_default();
        let judge = found
            .as_ref()
            .and_then(|q| {
                q.view
                    .judge
                    .as_ref()
                    .and_then(|e| e.reply(&q.card))
                    .map(|r| r.text(&q.card))
            })
            .unwrap_or_default();
        corrections.push(CorrectionSignal {
            card: row.object.clone(),
            ticket: row.ticket.clone(),
            question,
            judge,
            answer: row.value.clone().unwrap_or_default(),
            why: row.reason.clone().unwrap_or_default(),
            decision: found.and_then(|q| q.row.decision),
        });
    }
    signals.corrections = cap(corrections, "corrections", &mut signals.more);

    let mut needless = Vec::new();
    for id in ledger.needless_since(since_at)? {
        let Ok(q) = Question::find(ledger, &id) else {
            continue;
        };
        let reason = q.needless().unwrap_or("").to_string();
        needless.push(NeedlessSignal {
            card: q.row.id.clone(),
            ticket: q.row.ticket.clone(),
            agent: q.agent.clone(),
            question: q.card.text.trim().to_string(),
            means: needless_text(&reason).to_string(),
            reason,
        });
    }
    signals.needless = cap(needless, "needless", &mut signals.more);

    let since_time = since_at.and_then(|a| time::parse_rfc3339(a).ok());
    signals.plans = cap(
        plan_signals(&ctx.paths.tickets_dir(), since_time)?,
        "plans",
        &mut signals.more,
    );

    let review_after = since.map_or(0, |m| m.review_rowid);
    let limit = u32::try_from(LIMIT).unwrap_or(u32::MAX);
    signals.reviews = ledger
        .review_rows_after(review_after, limit)?
        .into_iter()
        .map(|r| ReviewSignal {
            round: r.round,
            comment: r.comment_id,
            reviewer: untrusted(r.reviewer.as_deref().unwrap_or("")),
            verdict: r.verdict,
            fix: r.fix,
            verdict_final: r.verdict_final,
            fix_final: r.fix_final,
        })
        .collect();
    let all_reviews = usize::try_from(ledger.count_review_rows_after(review_after)?).unwrap_or(0);
    if all_reviews > signals.reviews.len() {
        signals
            .more
            .insert("reviews".into(), all_reviews - signals.reviews.len());
    }

    signals.overlaps = cap(overlaps(&files, &changes), "overlaps", &mut signals.more);

    let mut rejected = Vec::new();
    for (_, row) in ledger.human_rows_after(human_after, HUMAN_REJECT)? {
        let change: Value = row
            .value
            .as_deref()
            .and_then(|v| serde_json::from_str(v).ok())
            .unwrap_or(Value::Null);
        rejected.push(RejectedSignal {
            proposal: proposal_of(&row.object),
            change,
            why: row.reason.clone().unwrap_or_default(),
            at: row.at.clone(),
        });
    }
    signals.rejected = cap(rejected, "rejected", &mut signals.more);
    let mut answered = Vec::new();
    for (_, row) in ledger.human_rows_after(human_after, HUMAN_ACCEPT)? {
        let value: Value = row
            .value
            .as_deref()
            .and_then(|v| serde_json::from_str(v).ok())
            .unwrap_or(Value::Null);
        let answer = row.reason.clone().unwrap_or_default();
        if value.get("op").and_then(Value::as_str) != Some("question") || answer.trim().is_empty() {
            continue;
        }
        answered.push(AnsweredSignal {
            proposal: proposal_of(&row.object),
            question: value
                .get("question")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string(),
            answer,
            at: row.at.clone(),
        });
    }
    signals.answered_questions = cap(answered, "answered_questions", &mut signals.more);

    Ok(Collected { signals, mark })
}

/// The proposal of a `human` object such as `proposal/2026-11-16-1 change 2`.
fn proposal_of(object: &str) -> String {
    object
        .split(" change ")
        .next()
        .unwrap_or(object)
        .to_string()
}

/// The files of the memory that changed since a commit of `main`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct MemoryChanges {
    /// `None` when each file counts as new: the first run, or a commit that `main` lost.
    pub since: Option<BTreeSet<String>>,
    /// The paths that `main` gained.
    pub added: BTreeSet<String>,
}

impl MemoryChanges {
    /// Each file counts as new.
    pub fn all() -> MemoryChanges {
        MemoryChanges::default()
    }

    pub fn added(&self, path: &str) -> bool {
        self.since.is_none() || self.added.contains(path)
    }

    pub fn changed(&self, path: &str) -> bool {
        self.since.as_ref().is_none_or(|s| s.contains(path))
    }
}

fn memory_changes(memory: &Memory, head: Option<&str>) -> MemoryChanges {
    let Some(head) = head else {
        return MemoryChanges::all();
    };
    let ancestor = git::run(memory.dir(), &["merge-base", "--is-ancestor", head, "main"]);
    let diff = ancestor.and_then(|_| {
        git::run(
            memory.dir(),
            &[
                "diff",
                "--name-status",
                "--no-renames",
                head,
                "main",
                "--",
                "decisions",
                "principles",
            ],
        )
    });
    let Ok(diff) = diff else {
        return MemoryChanges::all();
    };
    let mut changes = MemoryChanges {
        since: Some(BTreeSet::new()),
        added: BTreeSet::new(),
    };
    for line in diff.lines() {
        let mut parts = line.splitn(2, '\t');
        let (Some(status), Some(path)) = (parts.next(), parts.next()) else {
            continue;
        };
        if status.starts_with('A') {
            changes.added.insert(path.to_string());
        }
        if let Some(since) = &mut changes.since {
            since.insert(path.to_string());
        }
    }
    changes
}

/// The new active decisions of the person or of an import with no reason.
pub fn no_reason(files: &[MemoryFile], changes: &MemoryChanges) -> Vec<DecisionSignal> {
    files
        .iter()
        .filter_map(|f| match f {
            MemoryFile::Decision(d) => Some(d),
            MemoryFile::Principle(_) => None,
        })
        .filter(|d| {
            changes.added(&d.path)
                && d.status == Status::Active
                && matches!(d.by, Writer::Person(_) | Writer::Import)
                && d.why.trim().is_empty()
        })
        .map(|d| DecisionSignal {
            id: d.id.to_string(),
            by: d.by.name().to_string(),
            ticket: d.ticket.clone(),
            kind: d.kind.map(|k| k.to_string()),
            area: d.area.clone(),
            question: d.question.trim().to_string(),
            answer: d.answer.trim().to_string(),
            why: String::new(),
        })
        .collect()
}

/// The pairs of active files with the same kind and a common area noun: a principle and any
/// other file, or two decisions with the same question. At least one file of a pair changed.
pub fn overlaps(files: &[MemoryFile], changes: &MemoryChanges) -> Vec<Overlap> {
    let active: Vec<&MemoryFile> = files
        .iter()
        .filter(|f| f.status() == Status::Active && f.kind().is_some())
        .collect();
    let mut out = Vec::new();
    for (i, a) in active.iter().enumerate() {
        for b in &active[i + 1..] {
            if !changes.changed(a.path()) && !changes.changed(b.path()) {
                continue;
            }
            if a.kind() != b.kind() {
                continue;
            }
            let shared: Vec<String> = a
                .area()
                .iter()
                .filter(|x| b.area().iter().any(|y| y.eq_ignore_ascii_case(x)))
                .cloned()
                .collect();
            if shared.is_empty() {
                continue;
            }
            let both_decisions =
                matches!((a, b), (MemoryFile::Decision(_), MemoryFile::Decision(_)));
            if both_decisions && fold(a.headline()) != fold(b.headline()) {
                continue;
            }
            let (first, second) = if a.id() <= b.id() { (a, b) } else { (b, a) };
            out.push(Overlap {
                files: [first.id().to_string(), second.id().to_string()],
                kind: a.kind().map(|k| k.to_string()).unwrap_or_default(),
                area: shared,
                texts: [
                    first.headline().trim().to_string(),
                    second.headline().trim().to_string(),
                ],
            });
        }
    }
    out.sort_by(|a, b| a.files.cmp(&b.files));
    out
}

fn fold(text: &str) -> String {
    text.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

/// The plan difference of one ticket, or `None` when the person approved the plan unchanged.
pub fn plan_signal(ticket: &str, plan: &str, approved: &str) -> Option<PlanSignal> {
    let removed = removed_lines(plan, approved);
    let added = removed_lines(approved, plan);
    if removed.is_empty() && added.is_empty() {
        return None;
    }
    Some(PlanSignal {
        ticket: ticket.to_string(),
        removed: untrusted(&removed.join("\n")),
        added: untrusted(&added.join("\n")),
    })
}

/// The plan differences of the ticket folders whose approved plan changed after `since`.
fn plan_signals(tickets: &Path, since: Option<OffsetDateTime>) -> Result<Vec<PlanSignal>> {
    let entries = match std::fs::read_dir(tickets) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e).with_context(|| format!("could not read {}", tickets.display())),
    };
    let mut out = Vec::new();
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        let Ok(key) = TicketKey::parse(&name) else {
            continue;
        };
        if key.is_reserved() {
            continue;
        }
        let dir = entry.path();
        let approved_path = dir.join("approved-plan.md");
        let (Ok(plan), Ok(approved)) = (
            std::fs::read_to_string(dir.join("plan.md")),
            std::fs::read_to_string(&approved_path),
        ) else {
            continue;
        };
        if let Some(since) = since {
            let modified = std::fs::metadata(&approved_path)
                .and_then(|m| m.modified())
                .map(OffsetDateTime::from);
            if modified.is_ok_and(|m| m <= since) {
                continue;
            }
        }
        if let Some(signal) = plan_signal(key.as_str(), &plan, &approved) {
            out.push(signal);
        }
    }
    out.sort_by(|a, b| a.ticket.cmp(&b.ticket));
    Ok(out)
}

/// The last [`LIMIT`] entries of a list, with the count of the others in `more`.
fn cap<T>(mut items: Vec<T>, name: &str, more: &mut BTreeMap<String, usize>) -> Vec<T> {
    if items.len() > LIMIT {
        let cut = items.len() - LIMIT;
        more.insert(name.to_string(), cut);
        items.drain(..cut);
    }
    items
}

#[cfg(test)]
mod tests {
    use super::*;

    fn decision(id: &str, by: &str, area: &str, question: &str, why: &str) -> MemoryFile {
        let basis = if by == "judge" { "inference" } else { "answer" };
        let text = format!(
            "---\nid: {id}\nat: 2026-11-16T14:32:00Z\nby: {by}\nbasis: {basis}\nticket: DEMO-12\nkind: product\narea: [{area}]\nstatus: active\nsources: []\nsupersedes: null\n---\n## Question\n{question}\n\n## Answer\nCSV\n\n## Why\n{why}\n"
        );
        MemoryFile::parse(&format!("decisions/{id}.md"), &text).unwrap()
    }

    fn principle(id: &str, kind: &str, area: &str, status: &str) -> MemoryFile {
        let text = format!(
            "---\nid: {id}\nagent: judge\nkind: {kind}\narea: [{area}]\nstatus: {status}\nby: user\nevidence: []\n---\n## Rule\nWhen {id}, make sure that it holds.\n"
        );
        MemoryFile::parse(&format!("principles/{id}.md"), &text).unwrap()
    }

    fn changed(paths: &[&str]) -> MemoryChanges {
        MemoryChanges {
            since: Some(paths.iter().map(|p| p.to_string()).collect()),
            added: paths.iter().map(|p| p.to_string()).collect(),
        }
    }

    #[test]
    fn empty_signals_say_so() {
        let mut signals = Signals::default();
        assert!(signals.is_empty());
        assert_eq!(signals.describe(), "no signal");
        signals.more.insert("reviews".into(), 3);
        assert!(!signals.is_empty());
        assert_eq!(signals.describe(), "3 reviews");
        let value = serde_json::to_value(Signals::default()).unwrap();
        assert!(value.get("more").is_none());
        assert_eq!(value["new_decisions"], json!([]));
    }

    #[test]
    fn a_decision_with_no_reason_counts_only_for_the_person_or_an_import() {
        let files = vec![
            decision("D-0001", "user", "exports", "Q1?", ""),
            decision("D-0002", "import", "exports", "Q2?", ""),
            decision("D-0003", "judge", "exports", "Q3?", "Because."),
            decision("D-0004", "user", "exports", "Q4?", "The finance team."),
            decision("D-0005", "user", "exports", "Q5?", ""),
        ];
        let ids: Vec<String> = no_reason(&files, &MemoryChanges::all())
            .into_iter()
            .map(|d| d.id)
            .collect();
        assert_eq!(ids, ["D-0001", "D-0002", "D-0005"]);
        let ids: Vec<String> = no_reason(&files, &changed(&["decisions/D-0005.md"]))
            .into_iter()
            .map(|d| d.id)
            .collect();
        assert_eq!(ids, ["D-0005"], "only the new decisions");
    }

    #[test]
    fn overlaps_need_the_same_kind_a_common_area_and_a_changed_file() {
        let files = vec![
            principle("P-001", "product", "exports, reports", "active"),
            principle("P-002", "product", "Exports", "active"),
            principle("P-003", "code", "exports", "active"),
            principle("P-004", "product", "exports", "candidate"),
            principle("P-005", "product", "billing", "active"),
            decision("D-0001", "user", "exports", "Must it be CSV?", ""),
            decision("D-0002", "user", "exports", "Must  it be csv?", ""),
            decision("D-0003", "user", "exports", "Another question?", ""),
        ];
        let all = overlaps(&files, &MemoryChanges::all());
        let pairs: Vec<[String; 2]> = all.iter().map(|o| o.files.clone()).collect();
        let expected: Vec<[String; 2]> = [
            ["D-0001", "D-0002"],
            ["D-0001", "P-001"],
            ["D-0001", "P-002"],
            ["D-0002", "P-001"],
            ["D-0002", "P-002"],
            ["D-0003", "P-001"],
            ["D-0003", "P-002"],
            ["P-001", "P-002"],
        ]
        .map(|[a, b]| [a.to_string(), b.to_string()])
        .to_vec();
        assert_eq!(pairs, expected, "{all:#?}");
        let p12 = all.iter().find(|o| o.files == ["P-001", "P-002"]).unwrap();
        assert_eq!(p12.area, ["exports"]);
        assert_eq!(p12.kind, "product");

        let some = overlaps(&files, &changed(&["principles/P-002.md"]));
        let pairs: Vec<[String; 2]> = some.iter().map(|o| o.files.clone()).collect();
        assert!(
            pairs.iter().all(|p| p.contains(&"P-002".to_string())),
            "{pairs:?}"
        );
        assert_eq!(pairs.len(), 4);
        assert!(overlaps(&files, &changed(&[])).is_empty());
    }

    #[test]
    fn a_plan_difference_is_untrusted_text() {
        assert_eq!(plan_signal("DEMO-12", "a\nb\n", "a\nb\n"), None);
        let signal = plan_signal("DEMO-12", "a\nb\nc\n", "a\nc\nd\n").unwrap();
        assert_eq!(signal.removed, json!({"untrusted": "b"}));
        assert_eq!(signal.added, json!({"untrusted": "d"}));
    }

    #[test]
    fn a_list_keeps_its_last_entries() {
        let mut more = BTreeMap::new();
        let items: Vec<usize> = (0..LIMIT + 3).collect();
        let kept = cap(items, "x", &mut more);
        assert_eq!(kept.len(), LIMIT);
        assert_eq!(kept[0], 3);
        assert_eq!(more["x"], 3);
        assert_eq!(
            proposal_of("proposal/2026-11-16-1 change 2"),
            "proposal/2026-11-16-1"
        );
    }
}
