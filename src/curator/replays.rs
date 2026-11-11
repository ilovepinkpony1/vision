//! The replays of a proposal (section 9.4): the Judge answers the last `curator.replay` cards
//! that the person answered, one time with the proposed memory and one time with the memory of
//! today, so that the review can show how many answers agree with the person on each side.
//!
//! The proposed memory is `main` with the files of the proposal, as it would be after the
//! person accepted each change: a candidate principle that the proposal writes is active there.
//! The replay against `main` is stored for one commit, one model, and one set of cards, so that
//! the next proposal of the same day reuses it.
//!
//! The replays run over several ticks ([`begin`], then [`advance`] at each tick): a tick starts
//! one replay turn, and a later tick reads it and starts the next one. No tick holds its lock
//! while a replay turn runs.

use std::fs;
use std::path::{Path, PathBuf};

use ::time::OffsetDateTime;
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use super::records::{self, ProposalRecord, EVENT_BASELINE, EVENT_PROPOSAL};
use crate::ctx::Ctx;
use crate::ledger::Ledger;
use crate::memory::doc::Doc;
use crate::memory::front::Value;
use crate::memory::{ChangeState, Memory, Op, ProposalView, Status};
use crate::models;
use crate::questions::{
    replay_end, replay_ended, replay_kill, replay_start, ReplayOptions, ReplayReport,
    ReplayStarted, JUDGE,
};
use crate::turn::Refusal;
use crate::util::fs::{atomic_write, ensure_dir};
use crate::util::{git, ids, time};

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

/// Which replay of a proposal runs or starts next. The tick moves a proposal through the
/// stages, one replay turn at a time, and never waits for a turn while it holds its lock.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Stage {
    /// The replay with the proposed memory runs, or starts at the next tick.
    Proposed,
    /// The replay with the memory of today runs, or starts at the next tick.
    Today,
    /// Each replay ended or could not run: the review shows the numbers. A record of an older
    /// Vision has no stage, and its replays are done.
    #[default]
    Done,
}

impl Stage {
    /// The memory of the replay of this stage, for people.
    pub fn memory_text(self) -> &'static str {
        match self {
            Stage::Proposed => "the proposed memory",
            Stage::Today => "the memory of today",
            Stage::Done => "no memory",
        }
    }

    fn next(self) -> Stage {
        match self {
            Stage::Proposed => Stage::Today,
            Stage::Today | Stage::Done => Stage::Done,
        }
    }
}

/// The replay turn of a proposal that runs now.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RunningReplay {
    pub started: ReplayStarted,
    /// For the replay of today: the key of its stored baseline.
    #[serde(default)]
    pub baseline: Option<String>,
    /// True when the tick killed the turn at the time limit `caps.judge_minutes`.
    #[serde(default)]
    pub killed: bool,
}

/// Both replays of a proposal.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Replays {
    /// The number of cards that the replays answer.
    pub cards: usize,
    /// The ids of those cards: both replays answer the same cards.
    pub card_ids: Vec<String>,
    /// The replay with the memory of today (`main`).
    pub today: Option<StoredReplay>,
    /// The replay with the proposed memory.
    pub proposed: Option<StoredReplay>,
    /// True when the replay of today came from a stored baseline.
    pub today_stored: bool,
    /// Why a replay did not run, or what went wrong.
    pub notes: Vec<String>,
    /// The replay that runs or starts next.
    pub stage: Stage,
    /// The replay turn that runs now.
    pub running: Option<RunningReplay>,
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
    /// True when each replay ended or could not run.
    pub fn is_done(&self) -> bool {
        self.stage == Stage::Done
    }

    /// One line for people while the replays run, or `None` when they are done.
    pub fn running_text(&self) -> Option<String> {
        if self.is_done() {
            return None;
        }
        let what = match &self.running {
            Some(running) => format!(
                "the replay with {} runs (turn {})",
                self.stage.memory_text(),
                running.started.turn
            ),
            None => format!(
                "the replay with {} starts at the next tick",
                self.stage.memory_text()
            ),
        };
        Some(format!(
            "the replays of the Judge still run: {what}. The numbers come when they end; you can accept or reject each change now"
        ))
    }

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

/// The replays of a new proposal, before the first one starts: the cards to replay, or a note
/// that says why no replay runs. A replay turn starts in [`advance`].
pub fn begin(ctx: &Ctx, ledger: &Ledger, view: &ProposalView) -> Result<Replays> {
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
    replays.card_ids = cards;
    replays.stage = Stage::Proposed;
    Ok(replays)
}

/// Moves the replays of an open proposal one step on, and stores the record after each step:
/// a replay turn that ended gets its numbers, and the next replay turn starts. A turn that runs
/// longer than `caps.judge_minutes` is killed, and the next tick reads it. The call never waits
/// for a turn. A crash between two steps leaves a record that the next call goes on from: a
/// running turn is in the record from the transaction that wrote its row, and an ended turn is
/// read again until its numbers are stored.
pub fn advance(
    ctx: &Ctx,
    ledger: &Ledger,
    memory: &Memory,
    record: &mut ProposalRecord,
    now: OffsetDateTime,
) -> Result<()> {
    let config = ctx.config()?;
    let limit = i64::from(config.caps.judge_minutes) * 60;
    loop {
        let Some(replays) = record.replays.as_mut() else {
            return Ok(());
        };
        if replays.is_done() {
            return Ok(());
        }
        if let Some(running) = replays.running.clone() {
            let ended = replay_ended(ctx, &running.started)?;
            if ended {
                crate::pipeline::crash_point("curator-replay-ended");
            } else {
                let since = time::parse_rfc3339(&running.started.started).ok();
                let late = since.is_some_and(|at| time::seconds_between(at, now) >= limit);
                if late && !running.killed {
                    if let Err(e) = replay_kill(ctx, &running.started) {
                        ctx.log(&format!(
                            "curator: could not kill the late replay {}: {e:#}",
                            running.started.turn
                        ));
                    }
                    if let Some(r) = replays.running.as_mut() {
                        r.killed = true;
                    }
                    save(ledger, record)?;
                }
                return Ok(());
            }
            let stage = replays.stage;
            match replay_end(ctx, ledger, &running.started, running.killed) {
                Ok(report) => {
                    let stored = StoredReplay::from_report(&report);
                    if stage == Stage::Today && !stored.timed_out {
                        if let Some(baseline) = &running.baseline {
                            records::add(ledger, "tick", EVENT_BASELINE, baseline, &stored)?;
                        }
                    }
                    match stage {
                        Stage::Proposed => replays.proposed = Some(stored),
                        _ => replays.today = Some(stored),
                    }
                }
                Err(e) => replays.notes.push(format!(
                    "the replay with {} could not be read: {e:#}",
                    stage.memory_text()
                )),
            }
            replays.running = None;
            replays.stage = stage.next();
            save(ledger, record)?;
            ctx.log(&format!(
                "curator: {}: the replay with {} ended",
                record.proposal,
                stage.memory_text()
            ));
            crate::pipeline::crash_point("curator-replay-read");
            continue;
        }
        let stage = replays.stage;
        let cards = replays.card_ids.clone();
        let started = match stage {
            Stage::Proposed => {
                let view = memory.proposal(&record.proposal)?;
                let dir = ctx
                    .paths
                    .ticket_dir(&super::runs::key()?)
                    .join("proposals")
                    .join(record.proposal.replace('/', "-"));
                let started = export(memory, &view, &dir.join("memory"))
                    .context("the proposed memory could not be written for the replay")
                    .and_then(|folder| start_side(ctx, ledger, record, folder, &cards, None));
                let _ = fs::remove_dir_all(&dir);
                started
            }
            Stage::Today => {
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
                )[..16]
                    .to_string();
                if let Some(stored) =
                    records::read::<StoredReplay>(ledger, EVENT_BASELINE, &baseline)?
                {
                    if let Some(replays) = record.replays.as_mut() {
                        replays.today = Some(stored);
                        replays.today_stored = true;
                        replays.stage = Stage::Done;
                    }
                    save(ledger, record)?;
                    continue;
                }
                let folder = memory.dir().to_path_buf();
                start_side(ctx, ledger, record, folder, &cards, Some(baseline))
            }
            Stage::Done => return Ok(()),
        };
        match started {
            Ok(()) => {
                crate::pipeline::crash_point("curator-replay-started");
                return Ok(());
            }
            Err(e) if e.downcast_ref::<Refusal>().is_some() => {
                // A limit or a budget: the next tick tries again.
                ctx.log(&format!(
                    "curator: the replay with {} of {} waits: {e:#}",
                    stage.memory_text(),
                    record.proposal
                ));
                return Ok(());
            }
            Err(e) => {
                if let Some(replays) = record.replays.as_mut() {
                    replays.notes.push(format!(
                        "the replay with {} did not run: {e:#}",
                        stage.memory_text()
                    ));
                    replays.stage = stage.next();
                }
                save(ledger, record)?;
            }
        }
    }
}

/// Starts the replay turn of the stage of `record` against the memory folder `folder`. The
/// record with the running turn is stored in the transaction that writes the turn row.
fn start_side(
    ctx: &Ctx,
    ledger: &Ledger,
    record: &mut ProposalRecord,
    folder: PathBuf,
    cards: &[String],
    baseline: Option<String>,
) -> Result<()> {
    let config = ctx.config()?;
    let options = ReplayOptions {
        n: config.curator.replay.max(1),
        memory: folder,
        model: None,
        effort: None,
        key: super::runs::key()?,
        wait: None,
    };
    let mut claimed = record.clone();
    let started = replay_start(ctx, ledger, &options, Some(cards), |ledger, started| {
        if let Some(replays) = claimed.replays.as_mut() {
            replays.running = Some(RunningReplay {
                started: started.clone(),
                baseline: baseline.clone(),
                killed: false,
            });
        }
        save(ledger, &claimed)
    })?;
    *record = claimed;
    ctx.log(&format!(
        "curator: {}: started the replay {} with {}",
        record.proposal,
        started.turn,
        record
            .replays
            .as_ref()
            .map_or("a memory", |r| r.stage.memory_text())
    ));
    Ok(())
}

fn save(ledger: &Ledger, record: &ProposalRecord) -> Result<()> {
    records::put(ledger, "tick", EVENT_PROPOSAL, &record.proposal, record)
}

/// Runs the replays of a proposal to their end, and waits for each turn: the form of a caller
/// that holds no lock of the tick, such as a test. The tick uses [`advance`].
pub fn run_to_end(
    ctx: &Ctx,
    ledger: &Ledger,
    memory: &Memory,
    record: &mut ProposalRecord,
) -> Result<()> {
    loop {
        advance(ctx, ledger, memory, record, time::now())?;
        if record.replays.as_ref().is_none_or(Replays::is_done) {
            return Ok(());
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
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
