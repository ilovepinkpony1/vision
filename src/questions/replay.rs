//! The replay of the Judge (sections 9.4 and 10), for the Curator and for `vision model eval`.
//!
//! [`replay`] takes the last N cards that the person answered (not marked as needless), starts
//! one Judge turn for them against a memory folder that the caller names, with a model and an
//! effort that the caller can override, waits for that turn to end, applies the rules in code,
//! and reports how many answers of the Judge agree with the person.
//!
//! The Judge reads a copy of that folder without the decision files of the replayed answers,
//! because a file that holds the answer itself would make each replay agree. A replay writes
//! no decision file, changes no row of `questions`, and counts no use. Its turn runs under a
//! reserved key (`eval` or `curator`), and the next tick collects its row as for any turn.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::judge::{self, JudgeInput};
use super::rules::{self, Outcome};
use super::{Agents, Question, JUDGE};
use crate::config::Effort;
use crate::ctx::Ctx;
use crate::ledger::Ledger;
use crate::memory::{DecisionId, MemoryFile, MemoryId, Status};
use crate::paths::{TicketKey, TurnId};
use crate::turn::{self, TurnState};
use crate::util::fs::ensure_dir;
use crate::util::time;

/// The step of a replay turn.
pub const REPLAY_STEP: &str = "replay";

/// What a replay replays, and with what.
#[derive(Clone, Debug, PartialEq)]
pub struct ReplayOptions {
    /// The number of answers of the person to replay, newest first.
    pub n: u32,
    /// The memory folder that the Judge reads, such as a checkout of a proposal branch. It
    /// needs `decisions/` and `principles/`, and it need not be a git repo.
    pub memory: PathBuf,
    /// A model that replaces the model of the Judge, such as `opus`.
    pub model: Option<String>,
    pub effort: Option<Effort>,
    /// The reserved key of the turn: `eval` or `curator`.
    pub key: TicketKey,
    /// How long to wait for the turn. `None` waits `caps.judge_minutes`.
    pub wait: Option<Duration>,
}

/// One replayed card.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct ReplayItem {
    pub id: String,
    pub ticket: String,
    pub question: String,
    /// The answer of the person.
    pub person: String,
    /// The number of the choice of the person, from 1.
    pub person_choice: Option<usize>,
    /// The final outcome of the Judge after the rules in code.
    pub outcome: Outcome,
    /// The answer or the proposal of the Judge.
    pub judge: Option<String>,
    /// The number of the choice of the Judge, from 1.
    pub judge_choice: Option<usize>,
    /// True when the answer or the proposal of the Judge is the answer of the person: the same
    /// choice, or the same text after trimming and case folding.
    pub agree: bool,
    /// Why the rules in code changed the outcome of the Judge.
    pub rule: Option<String>,
}

/// The result of a replay.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct ReplayReport {
    pub turn: String,
    pub model: String,
    pub effort: Effort,
    /// The copy of the memory that the Judge read.
    pub memory: PathBuf,
    pub items: Vec<ReplayItem>,
    /// Final answers of the Judge that agree with the person.
    pub agree: usize,
    /// Final answers of the Judge that do not agree with the person.
    pub disagree: usize,
    /// Cards that went to the person: a proposal or an ask.
    pub to_person: usize,
    /// The proposals among them that agree with the person.
    pub proposals_agree: usize,
    pub cost_usd: f64,
    pub seconds: f64,
    /// Why the turn ended, such as `result` or `timeout`.
    pub reason: Option<String>,
    /// True when the replay stopped waiting and killed the turn.
    pub timed_out: bool,
}

/// A replay turn that runs: what [`replay_end`] needs to read its answers.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ReplayStarted {
    pub turn: String,
    /// The reserved key of the turn.
    pub key: String,
    /// The cards that the turn answers, by id: the same cards when the turn ends.
    pub cards: Vec<String>,
    /// The copy of the memory that the Judge reads.
    pub memory: PathBuf,
    pub model: String,
    pub effort: Effort,
    /// When the turn started, RFC 3339 UTC.
    pub started: String,
}

/// Replays the last answers of the person, and waits for the end. See the module text.
pub fn replay(ctx: &Ctx, ledger: &Ledger, options: &ReplayOptions) -> Result<ReplayReport> {
    let config = ctx.config()?;
    let started = replay_start(ctx, ledger, options, None, |_, _| Ok(()))?;
    let limit = options
        .wait
        .unwrap_or_else(|| Duration::from_secs(u64::from(config.caps.judge_minutes) * 60));
    let clock = Instant::now();
    let mut timed_out = false;
    loop {
        if replay_ended(ctx, &started)? {
            break;
        }
        if clock.elapsed() >= limit {
            timed_out = true;
            replay_kill(ctx, &started)?;
            let until = Instant::now() + Duration::from_secs(10);
            while Instant::now() < until && !replay_ended(ctx, &started)? {
                std::thread::sleep(Duration::from_millis(50));
            }
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    replay_end(ctx, ledger, &started, timed_out)
}

/// Starts the replay turn and gives what [`replay_end`] needs. `cards` names the cards to
/// replay; `None` takes the last `options.n` answers of the person. `claim` runs in the
/// transaction that writes the turn row, so that a crash never leaves a replay turn that its
/// caller did not record, or a record of a turn that does not exist.
pub fn replay_start(
    ctx: &Ctx,
    ledger: &Ledger,
    options: &ReplayOptions,
    cards: Option<&[String]>,
    claim: impl FnOnce(&Ledger, &ReplayStarted) -> Result<()>,
) -> Result<ReplayStarted> {
    ctx.require_setup()?;
    if !options.key.is_reserved() {
        bail!(
            "a replay runs under a reserved key such as eval or curator, not {}",
            options.key
        );
    }
    if !options.memory.join("decisions").is_dir() || !options.memory.join("principles").is_dir() {
        bail!(
            "{} is not a memory folder: it needs decisions/ and principles/",
            options.memory.display()
        );
    }
    let config = ctx.config()?;
    let mut agents = Agents::default();
    let rows = match cards {
        None => ledger.questions_answered_by_person(options.n)?,
        Some(ids) => {
            let mut rows = Vec::new();
            for id in ids {
                rows.extend(ledger.question(id)?);
            }
            rows
        }
    };
    let questions = rows
        .into_iter()
        .map(|row| Question::load(ledger, row, &mut agents))
        .collect::<Result<Vec<_>>>()?;
    if questions.is_empty() {
        bail!("the person answered no card yet, so there is nothing to replay");
    }

    let now = time::now();
    let stamp = format!(
        "{}-{}",
        time::compact(now),
        &uuid::Uuid::new_v4().simple().to_string()[..8]
    );
    let base = ctx
        .paths
        .ticket_dir(&options.key)
        .join("replay")
        .join(stamp);
    let copy = base.join("memory");
    let work = base.join("work");
    ensure_dir(&work)?;
    let hidden = hidden_decisions(&options.memory, &questions);
    copy_memory(&options.memory, &copy, &hidden)?;

    let input = JudgeInput {
        key: options.key.clone(),
        step: REPLAY_STEP.into(),
        workdir: work,
        staging: base.clone(),
        cards: questions
            .iter()
            .map(|q| judge::card_for_judge(&q.row))
            .collect(),
        asking: Value::Null,
        documents: Vec::new(),
        memory: copy.clone(),
    };
    let mut request = judge::request(config, &input)?;
    request.model = options.model.clone();
    request.effort = options.effort;
    request.no_retry = true;
    let cards: Vec<String> = questions.iter().map(|q| q.row.id.clone()).collect();
    let mut started = None;
    let launched = super::launch_claimed(ctx, ledger, &request, |ledger, launched| {
        let info = ReplayStarted {
            turn: launched.id.to_string(),
            key: options.key.to_string(),
            cards: cards.clone(),
            memory: copy.clone(),
            model: launched.model.clone(),
            effort: launched.effort,
            started: time::rfc3339(now),
        };
        claim(ledger, &info)?;
        started = Some(info);
        Ok(())
    });
    if launched.is_err() {
        let _ = fs::remove_dir_all(&base);
    }
    launched?;
    started.context("the replay turn was written with no record")
}

/// True when the replay turn ended: its `turn.json` says so.
pub fn replay_ended(ctx: &Ctx, started: &ReplayStarted) -> Result<bool> {
    let key = TicketKey::parse(&started.key)?;
    let id = TurnId::parse(&started.turn)?;
    Ok(TurnState::read(&ctx.paths.turn(&key, &id).status)
        .ok()
        .flatten()
        .is_some_and(|s| s.is_ended()))
}

/// Kills the replay turn after its time limit.
pub fn replay_kill(ctx: &Ctx, started: &ReplayStarted) -> Result<()> {
    let key = TicketKey::parse(&started.key)?;
    let id = TurnId::parse(&started.turn)?;
    turn::kill_turn(ctx, &key, &id)?;
    Ok(())
}

/// Reads the answers of an ended replay turn, applies the rules in code, and reports how many
/// agree with the person. `timed_out` says that the caller killed the turn at its time limit.
pub fn replay_end(
    ctx: &Ctx,
    ledger: &Ledger,
    started: &ReplayStarted,
    timed_out: bool,
) -> Result<ReplayReport> {
    let config = ctx.config()?;
    let id = TurnId::parse(&started.turn)?;
    let mut agents = Agents::default();
    let mut questions = Vec::new();
    for card in &started.cards {
        if let Some(row) = ledger.question(card)? {
            questions.push(Question::load(ledger, row, &mut agents)?);
        }
    }
    let outcome = turn::outcome(ctx, ledger, &id)?;
    let result = if timed_out {
        None
    } else {
        valid_result(ctx, outcome.result.as_ref())
    };
    let entries = judge::entries(ctx, &started.turn, result.as_ref());
    let copy = started.memory.clone();
    let active = |id: &str| active_in(&copy, id);

    let mut items = Vec::new();
    for q in &questions {
        let entry = entries.get(&q.row.id);
        let ruling = rules::rule(&q.card, entry, &config.judge, &active);
        let person = q.answer.clone().unwrap_or_default();
        let judge_reply = entry.and_then(|e| e.reply(&q.card));
        let agree = match (&judge_reply, ruling.outcome) {
            (Some(reply), Outcome::Answer | Outcome::Propose) => {
                reply.same(&person.reply(), &q.card)
            }
            _ => false,
        };
        let shown = matches!(ruling.outcome, Outcome::Answer | Outcome::Propose);
        items.push(ReplayItem {
            id: q.row.id.clone(),
            ticket: q.row.ticket.clone(),
            question: q.card.text.trim().to_string(),
            person: person.text.clone(),
            person_choice: person.choice.map(|i| i + 1),
            outcome: ruling.outcome,
            judge: judge_reply
                .as_ref()
                .filter(|_| shown)
                .map(|r| r.text(&q.card)),
            judge_choice: judge_reply
                .as_ref()
                .filter(|_| shown)
                .and_then(|r| r.choice(&q.card))
                .map(|i| i + 1),
            agree,
            rule: ruling.reason,
        });
    }
    let agree = items
        .iter()
        .filter(|i| i.outcome == Outcome::Answer && i.agree)
        .count();
    let disagree = items
        .iter()
        .filter(|i| i.outcome == Outcome::Answer && !i.agree)
        .count();
    let to_person = items.len() - agree - disagree;
    let proposals_agree = items
        .iter()
        .filter(|i| i.outcome == Outcome::Propose && i.agree)
        .count();
    let key = TicketKey::parse(&started.key)?;
    let dir = ctx.paths.turn(&key, &id).dir;
    let seconds = seconds_of(started, &outcome);
    ctx.log(&format!(
        "questions: replay {} of {} answer(s): {agree} agree, {disagree} disagree, {to_person} to the person",
        started.turn,
        items.len()
    ));
    Ok(ReplayReport {
        turn: started.turn.clone(),
        model: started.model.clone(),
        effort: started.effort,
        memory: copy,
        items,
        agree,
        disagree,
        to_person,
        proposals_agree,
        cost_usd: stream_cost(&dir.join("stream.jsonl")),
        seconds,
        reason: outcome.reason.map(|r| r.to_string()),
        timed_out,
    })
}

/// The seconds of a replay turn: from its start to its end, or to now.
fn seconds_of(started: &ReplayStarted, outcome: &turn::TurnOutcome) -> f64 {
    let Ok(from) = time::parse_rfc3339(&started.started) else {
        return 0.0;
    };
    let to = outcome
        .ended
        .as_deref()
        .and_then(|e| time::parse_rfc3339(e).ok())
        .unwrap_or_else(time::now);
    (to - from).as_seconds_f64().max(0.0)
}

/// The result object when it fits the schema of the Judge.
fn valid_result(ctx: &Ctx, result: Option<&Value>) -> Option<Value> {
    let result = result?;
    let schema = turn::agents_file(&ctx.paths, turn::schema_name(JUDGE))
        .ok()
        .and_then(|text| serde_json::from_str::<Value>(&text).ok())?;
    let problems = turn::schema_problems(&schema, result);
    if problems.is_empty() {
        Some(result.clone())
    } else {
        ctx.log(&format!(
            "questions: the replay result does not fit the schema of the Judge: {}",
            problems.join("; ")
        ));
        None
    }
}

/// The cost of the last result event of a stream, or 0.
fn stream_cost(path: &Path) -> f64 {
    let Ok(text) = fs::read_to_string(path) else {
        return 0.0;
    };
    text.lines()
        .rev()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .filter(|event| event.get("type").and_then(Value::as_str) == Some("result"))
        .find_map(|event| event.get("total_cost_usd").and_then(Value::as_f64))
        .unwrap_or(0.0)
}

/// The decision files of the replayed answers, and the files that they supersede.
fn hidden_decisions(memory: &Path, questions: &[Question]) -> BTreeSet<DecisionId> {
    let mut hidden = BTreeSet::new();
    for q in questions {
        let Some(id) = q
            .row
            .decision
            .as_deref()
            .and_then(|d| DecisionId::parse(d).ok())
        else {
            continue;
        };
        hidden.insert(id);
        let path = id.path();
        if let Ok(text) = fs::read_to_string(memory.join(&path)) {
            if let Ok(MemoryFile::Decision(d)) = MemoryFile::parse(&path, &text) {
                hidden.extend(d.supersedes);
            }
        }
    }
    hidden
}

/// Copies the memory files of `from` into `to`, without the hidden decisions.
fn copy_memory(from: &Path, to: &Path, hidden: &BTreeSet<DecisionId>) -> Result<()> {
    for folder in ["decisions", "principles"] {
        let target = to.join(folder);
        ensure_dir(&target)?;
        let source = from.join(folder);
        let entries = fs::read_dir(&source)
            .with_context(|| format!("could not read {}", source.display()))?;
        for entry in entries {
            let entry = entry.with_context(|| format!("could not read {}", source.display()))?;
            let name = entry.file_name();
            let Some(name) = name.to_str() else { continue };
            if name.starts_with('.') || !name.ends_with(".md") || !entry.path().is_file() {
                continue;
            }
            let id = name
                .strip_suffix(".md")
                .and_then(|stem| DecisionId::parse(stem).ok());
            if folder == "decisions" && id.is_some_and(|id| hidden.contains(&id)) {
                continue;
            }
            fs::copy(entry.path(), target.join(name))
                .with_context(|| format!("could not copy {}", entry.path().display()))?;
        }
    }
    let archive = from.join("ARCHIVE.md");
    if archive.is_file() {
        fs::copy(&archive, to.join("ARCHIVE.md"))
            .with_context(|| format!("could not copy {}", archive.display()))?;
    }
    Ok(())
}

/// True when the id names an active, valid file in the memory folder `dir`.
pub fn active_in(dir: &Path, id: &str) -> bool {
    let Ok(id) = MemoryId::parse(id) else {
        return false;
    };
    let path = id.path();
    let Ok(text) = fs::read_to_string(dir.join(&path)) else {
        return false;
    };
    MemoryFile::parse(&path, &text).is_ok_and(|f| f.status() == Status::Active)
}
