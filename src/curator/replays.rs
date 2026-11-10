//! The replays of a proposal (section 9.4): the Judge answers the last `curator.replay` cards
//! that the person answered, one time with the proposed memory and one time with the memory of
//! today, so that the review can show how many answers agree with the person on each side.
//!
//! The proposed memory is `main` with the files of the proposal, as it would be after the
//! person accepted each change: a candidate principle that the proposal writes is active there.
//! The replay against `main` is stored for one commit, one model, and one set of cards, so that
//! the next proposal of the same day reuses it.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use super::records::{self, EVENT_BASELINE};
use crate::ctx::Ctx;
use crate::ledger::Ledger;
use crate::memory::doc::Doc;
use crate::memory::front::Value;
use crate::memory::{ChangeState, Memory, Op, ProposalView, Status};
use crate::models;
use crate::questions::{replay, ReplayOptions, ReplayReport, JUDGE};
use crate::util::fs::{atomic_write, ensure_dir};
use crate::util::{git, ids};

/// One replayed card, as the ledger keeps it.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct StoredItem {
    pub id: String,
    pub ticket: String,
    pub question: String,
    /// The answer of the person.
    pub person: String,
    /// `answer`, `propose`, or `ask`: the final outcome of the Judge.
    pub outcome: String,
    /// The answer or the proposal of the Judge.
    pub judge: Option<String>,
    pub agree: bool,
    pub rule: Option<String>,
}

/// Where a replayed card lands: the Judge answers it right, answers it wrong, or sends it to
/// the person.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Side {
    Right,
    Wrong,
    Person,
}

impl Side {
    pub fn text(self) -> &'static str {
        match self {
            Side::Right => "right",
            Side::Wrong => "wrong",
            Side::Person => "to the person",
        }
    }
}

impl StoredItem {
    pub fn side(&self) -> Side {
        match (self.outcome.as_str(), self.agree) {
            ("answer", true) => Side::Right,
            ("answer", false) => Side::Wrong,
            _ => Side::Person,
        }
    }
}

/// One replay, as the ledger keeps it.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct StoredReplay {
    pub turn: String,
    pub model: String,
    pub effort: String,
    pub items: Vec<StoredItem>,
    /// Answers of the Judge that agree with the person.
    pub agree: usize,
    pub disagree: usize,
    /// Cards that went to the person: a proposal or an ask.
    pub to_person: usize,
    pub proposals_agree: usize,
    pub cost_usd: f64,
    pub seconds: f64,
    pub reason: Option<String>,
    pub timed_out: bool,
}

impl StoredReplay {
    pub fn from_report(report: &ReplayReport) -> StoredReplay {
        serde_json::to_value(report)
            .ok()
            .and_then(|v| serde_json::from_value(v).ok())
            .unwrap_or_default()
    }

    /// One line, such as `4 right, 1 wrong, 1 to the person`.
    pub fn line(&self) -> String {
        format!(
            "{} right, {} wrong, {} to the person",
            self.agree, self.disagree, self.to_person
        )
    }
}

/// Both replays of a proposal.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Replays {
    /// The number of cards that the replays answer.
    pub cards: usize,
    /// The replay with the memory of today (`main`).
    pub today: Option<StoredReplay>,
    /// The replay with the proposed memory.
    pub proposed: Option<StoredReplay>,
    /// True when the replay of today came from a stored baseline.
    pub today_stored: bool,
    /// Why a replay did not run, or what went wrong.
    pub notes: Vec<String>,
}

/// A card that changed side between the two replays.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Changed {
    pub id: String,
    pub question: String,
    pub person: String,
    pub today: Option<String>,
    pub today_side: Side,
    pub proposed: Option<String>,
    pub proposed_side: Side,
}

impl Replays {
    /// The cards whose side differs between today and the proposal.
    pub fn changed(&self) -> Vec<Changed> {
        let (Some(today), Some(proposed)) = (&self.today, &self.proposed) else {
            return Vec::new();
        };
        let mut out = Vec::new();
        for p in &proposed.items {
            let Some(t) = today.items.iter().find(|t| t.id == p.id) else {
                continue;
            };
            if t.side() != p.side() {
                out.push(Changed {
                    id: p.id.clone(),
                    question: p.question.clone(),
                    person: p.person.clone(),
                    today: t.judge.clone(),
                    today_side: t.side(),
                    proposed: p.judge.clone(),
                    proposed_side: p.side(),
                });
            }
        }
        out
    }
}

/// Runs the replays of a proposal: with its memory, and with `main` unless a stored baseline
/// fits. A replay that cannot run leaves a note, never an error.
pub fn run(ctx: &Ctx, ledger: &Ledger, memory: &Memory, view: &ProposalView) -> Result<Replays> {
    let config = ctx.config()?;
    let n = config.curator.replay.max(1);
    let mut replays = Replays::default();
    let cards: Vec<String> = ledger
        .questions_answered_by_person(n)?
        .into_iter()
        .map(|r| r.id)
        .collect();
    replays.cards = cards.len();
    if cards.is_empty() {
        replays
            .notes
            .push("the person answered no card yet, so there is nothing to replay".into());
        return Ok(replays);
    }
    let writes_files = view
        .file
        .changes
        .iter()
        .any(|c| c.state == ChangeState::Open && !c.files.is_empty());
    if !writes_files {
        replays
            .notes
            .push("the proposal changes no file, so the answers stay as they are".into());
        return Ok(replays);
    }
    let key = super::runs::key()?;
    let options = |dir: PathBuf| ReplayOptions {
        n,
        memory: dir,
        model: None,
        effort: None,
        key: key.clone(),
        wait: None,
    };

    let dir = ctx
        .paths
        .ticket_dir(&key)
        .join("proposals")
        .join(view.file.name.replace('/', "-"));
    let exported = export(memory, view, &dir.join("memory"));
    match exported {
        Ok(folder) => match replay(ctx, ledger, &options(folder)) {
            Ok(report) => replays.proposed = Some(StoredReplay::from_report(&report)),
            Err(e) => replays.notes.push(format!(
                "the replay with the proposed memory did not run: {e:#}"
            )),
        },
        Err(e) => replays.notes.push(format!(
            "the proposed memory could not be written for the replay: {e:#}"
        )),
    }
    let _ = fs::remove_dir_all(&dir);

    let choice = models::choice(config, JUDGE)?;
    let head = memory.head()?;
    let baseline = ids::sha256_hex(
        format!(
            "{head}\n{}\n{}\n{}",
            choice.model,
            choice.effort,
            cards.join(",")
        )
        .as_bytes(),
    );
    let baseline = &baseline[..16];
    if let Some(stored) = records::read::<StoredReplay>(ledger, EVENT_BASELINE, baseline)? {
        replays.today = Some(stored);
        replays.today_stored = true;
    } else {
        match replay(ctx, ledger, &options(memory.dir().to_path_buf())) {
            Ok(report) => {
                let stored = StoredReplay::from_report(&report);
                if !stored.timed_out {
                    records::add(ledger, "tick", EVENT_BASELINE, baseline, &stored)?;
                }
                replays.today = Some(stored);
            }
            Err(e) => replays.notes.push(format!(
                "the replay with the memory of today did not run: {e:#}"
            )),
        }
    }
    Ok(replays)
}

/// Writes the proposed memory into `to`: the files of `main`, the files that the proposal
/// changed, and each candidate principle of an open change as active, with the person as `by`.
pub fn export(memory: &Memory, view: &ProposalView, to: &Path) -> Result<PathBuf> {
    if to.exists() {
        fs::remove_dir_all(to).with_context(|| format!("could not clear {}", to.display()))?;
    }
    for folder in ["decisions", "principles"] {
        ensure_dir(&to.join(folder))?;
    }
    let (files, _) = memory.snapshot()?;
    for (path, text) in &files {
        atomic_write(&to.join(path), text.as_bytes())?;
    }
    let archive = memory.dir().join("ARCHIVE.md");
    if archive.is_file() {
        fs::copy(&archive, to.join("ARCHIVE.md"))
            .with_context(|| format!("could not copy {}", archive.display()))?;
    }
    let changed = git::run(
        memory.dir(),
        &["diff", "--name-only", "--no-renames", &view.base, &view.tip],
    )?;
    for path in changed.lines().filter(|p| p.ends_with(".md")) {
        let target = to.join(path);
        let present = git::run(
            memory.dir(),
            &["cat-file", "-e", &format!("{}:{path}", view.tip)],
        )
        .is_ok();
        if present {
            let bytes = git::run_bytes(memory.dir(), &["show", &format!("{}:{path}", view.tip)])?;
            if let Some(parent) = target.parent() {
                ensure_dir(parent)?;
            }
            atomic_write(&target, &bytes)?;
        } else {
            let _ = fs::remove_file(&target);
        }
    }
    for change in &view.file.changes {
        if change.state != ChangeState::Open || !matches!(change.op, Op::Add | Op::Edit | Op::Merge)
        {
            continue;
        }
        let Some(path) = change
            .path
            .as_deref()
            .filter(|p| p.starts_with("principles/"))
        else {
            continue;
        };
        let file = to.join(path);
        let Ok(text) = fs::read_to_string(&file) else {
            continue;
        };
        let Ok(mut doc) = Doc::parse(&text) else {
            continue;
        };
        let status = doc
            .get("status")
            .and_then(Value::as_text)
            .and_then(Status::parse);
        if status == Some(Status::Candidate) {
            doc.set("status", Value::str(Status::Active.as_str()));
            doc.set("by", Value::str(memory.person().name()));
            atomic_write(&file, doc.to_text().as_bytes())?;
        }
    }
    Ok(to.to_path_buf())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(id: &str, outcome: &str, agree: bool, judge: &str) -> StoredItem {
        StoredItem {
            id: id.into(),
            question: format!("Question {id}?"),
            person: "CSV".into(),
            outcome: outcome.into(),
            judge: Some(judge.into()),
            agree,
            ..StoredItem::default()
        }
    }

    #[test]
    fn cards_that_changed_side_are_listed() {
        let today = StoredReplay {
            items: vec![
                item("Q-0001", "answer", false, "XLSX"),
                item("Q-0002", "answer", true, "CSV"),
                item("Q-0003", "ask", false, "CSV"),
            ],
            ..StoredReplay::default()
        };
        let proposed = StoredReplay {
            items: vec![
                item("Q-0001", "answer", true, "CSV"),
                item("Q-0002", "answer", true, "CSV"),
                item("Q-0003", "propose", true, "CSV"),
            ],
            ..StoredReplay::default()
        };
        let replays = Replays {
            today: Some(today),
            proposed: Some(proposed),
            ..Replays::default()
        };
        let changed = replays.changed();
        assert_eq!(changed.len(), 1, "{changed:#?}");
        assert_eq!(changed[0].id, "Q-0001");
        assert_eq!(changed[0].today_side, Side::Wrong);
        assert_eq!(changed[0].proposed_side, Side::Right);
        assert_eq!(changed[0].today.as_deref(), Some("XLSX"));
        assert!(Replays::default().changed().is_empty());
    }
}
