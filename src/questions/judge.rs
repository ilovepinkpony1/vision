//! The Judge turn (section 8.2): its request, its start for the new cards of one asking turn,
//! the application of its result through the rules in code, and its time limit (rule 5).

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Path, PathBuf};

use ::time::OffsetDateTime;
use anyhow::{bail, Context, Result};
use serde_json::{json, Map, Value};

use super::card::Card;
use super::record;
use super::rules::{self, Entry, Outcome};
use super::{launch_claimed, Agents, Answer, JudgeView, Question, JUDGE, JUDGE_STEP};
use crate::config::Config;
use crate::ctx::Ctx;
use crate::ledger::questions::{
    QuestionRow, RulingRow, STATE_ASKED, STATE_JUDGING, STATE_NEW, STATE_PROPOSED, STATE_RECORDING,
};
use crate::ledger::{Event, Ledger};
use crate::memory::{Memory, PrincipleId, UseKind, Writer};
use crate::paths::{TicketKey, TurnId};
use crate::turn::{self, LaunchRequest, Refusal, TurnKind, TurnStatus, ViewFile};
use crate::util::fs::{atomic_write, ensure_dir};
use crate::util::time;

/// The documents of the ticket folder that a Judge turn gets, with their key in the facts.
pub const DOCUMENTS: [(&str, &str); 5] = [
    ("issue", "issue.json"),
    ("refine", "refine.md"),
    ("plan", "plan.md"),
    ("approved_plan", "approved-plan.md"),
    ("answers", "answers.md"),
];

/// What a Judge turn gets.
#[derive(Clone, Debug)]
pub(crate) struct JudgeInput {
    pub key: TicketKey,
    pub step: String,
    /// The working folder: the clone of the ticket, or a folder of the turn.
    pub workdir: PathBuf,
    /// Where `cards.json` is written before the turn copies it into its view.
    pub staging: PathBuf,
    /// Each card as JSON, with its `id`.
    pub cards: Vec<Value>,
    /// `{"turn", "agent", "step"}` of the asking turn, or null.
    pub asking: Value,
    /// The documents that exist: the key in the facts and the file.
    pub documents: Vec<(&'static str, PathBuf)>,
    /// The memory folder that the session reads.
    pub memory: PathBuf,
}

/// The card of a row as the Judge reads it: the card of the agent with its `id`.
pub(crate) fn card_for_judge(row: &QuestionRow) -> Value {
    let mut card = match serde_json::from_str::<Value>(&row.card) {
        Ok(Value::Object(map)) => map,
        _ => Map::new(),
    };
    card.insert("id".into(), json!(row.id));
    Value::Object(card)
}

/// The documents of the ticket folder that exist.
pub(crate) fn ticket_documents(ctx: &Ctx, key: &TicketKey) -> Vec<(&'static str, PathBuf)> {
    DOCUMENTS
        .iter()
        .map(|(name, file)| (*name, ctx.paths.ticket_doc(key, file)))
        .filter(|(_, path)| path.is_file())
        .collect()
}

/// The launch request of a Judge turn: the agent `judge`, a reader, the role file
/// `judge.md`, the schema `judge.schema.json`, the memory as an extra folder, and the time
/// limit `caps.judge_minutes`. The facts name the cards, the documents (null when missing),
/// the memory, `always_ask`, and the writer name of the person.
pub(crate) fn request(config: &Config, input: &JudgeInput) -> Result<LaunchRequest> {
    ensure_dir(&input.staging)?;
    let cards_file = input.staging.join("cards.json");
    atomic_write(
        &cards_file,
        format!("{}\n", serde_json::to_string_pretty(&input.cards)?).as_bytes(),
    )?;
    let mut view = vec![ViewFile {
        from: cards_file,
        to: "turns/{turn}/cards.json".into(),
    }];
    let mut documents = Map::new();
    for (name, file) in DOCUMENTS {
        documents.insert(name.into(), Value::Null);
        if let Some((_, path)) = input.documents.iter().find(|(n, _)| *n == name) {
            view.push(ViewFile {
                from: path.clone(),
                to: format!("turns/{{turn}}/{file}"),
            });
            documents.insert(name.into(), json!(format!(".vision/turns/{{turn}}/{file}")));
        }
    }
    let mut request = LaunchRequest::new(
        input.key.clone(),
        JUDGE,
        &input.step,
        TurnKind::Reader,
        &input.workdir,
    );
    request.facts = json!({
        "asking": input.asking,
        "cards": ".vision/turns/{turn}/cards.json",
        "documents": documents,
        "memory": input.memory.display().to_string(),
        "always_ask": config.judge.always_ask,
        "person": config.user,
    });
    request.view = view;
    request.add_dirs = vec![input.memory.clone()];
    request.time_limit_secs = Some(u64::from(config.caps.judge_minutes).saturating_mul(60));
    Ok(request)
}

/// The entries of the output of a Judge turn, by card id. The first entry of a card wins. Each
/// entry that does not parse, and each repeated one, is logged.
pub(crate) fn entries(ctx: &Ctx, turn: &str, result: Option<&Value>) -> HashMap<String, Entry> {
    let mut out = HashMap::new();
    let items = result
        .and_then(|r| r.get("answers"))
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    for item in items {
        match serde_json::from_value::<Entry>(item.clone()) {
            Ok(entry) => {
                let id = entry.card.trim().to_string();
                if out.contains_key(&id) {
                    ctx.log(&format!(
                        "questions: the Judge turn {turn} gave a second entry for {id}, so it is ignored"
                    ));
                    continue;
                }
                out.insert(id, entry);
            }
            Err(e) => ctx.log(&format!(
                "questions: the Judge turn {turn} gave an entry that is not valid, so it is ignored: {e}"
            )),
        }
    }
    out
}

// ----- The start -------------------------------------------------------------------------

/// Starts one Judge turn for the new cards of each asking turn.
pub(crate) fn start_all(ctx: &Ctx, ledger: &Ledger, memory_dir: &Path) -> Result<()> {
    let mut batches: BTreeMap<(String, String), Vec<QuestionRow>> = BTreeMap::new();
    let mut order = Vec::new();
    for row in ledger.questions_in_states(&[STATE_NEW])? {
        let key = (row.created.clone().unwrap_or_default(), row.turn.clone());
        if !batches.contains_key(&key) {
            order.push(key.clone());
        }
        batches.entry(key).or_default().push(row);
    }
    let mut failed = Vec::new();
    for key in order {
        let rows = &batches[&key];
        let asking = &key.1;
        if let Err(e) = start(ctx, ledger, memory_dir, asking, rows) {
            if let Some(refused) = e.downcast_ref::<Refusal>() {
                ctx.log(&format!(
                    "questions: the Judge for the cards of {asking} waits: {refused}"
                ));
                continue;
            }
            ctx.log(&format!(
                "questions: could not start the Judge for the cards of {asking}: {e:#}"
            ));
            failed.push(asking.clone());
        }
    }
    if !failed.is_empty() {
        bail!(
            "could not start the Judge for the cards of {}",
            failed.join(", ")
        );
    }
    Ok(())
}

fn start(
    ctx: &Ctx,
    ledger: &Ledger,
    memory_dir: &Path,
    asking: &str,
    rows: &[QuestionRow],
) -> Result<()> {
    let Some(first) = rows.first() else {
        return Ok(());
    };
    let key = TicketKey::parse(&first.ticket)?;
    let flags = ledger.ticket_flags(key.as_str())?;
    if !flags.is_empty() {
        ctx.log(&format!(
            "questions: {key} has the flag(s) {}, so the Judge for the cards of {asking} waits",
            flags.join(", ")
        ));
        return Ok(());
    }
    let config = ctx.config()?;
    let asking_row = ledger.turn(asking)?;
    let staging = ctx.paths.ticket_dir(&key).join("judge").join(asking);
    let workdir = match ledger.ticket_clone(key.as_str())? {
        Some(clone) if Path::new(&clone).is_dir() => PathBuf::from(clone),
        _ => staging.clone(),
    };
    let input = JudgeInput {
        key: key.clone(),
        step: JUDGE_STEP.into(),
        workdir,
        staging,
        cards: rows.iter().map(card_for_judge).collect(),
        asking: json!({
            "turn": asking,
            "agent": asking_row.as_ref().and_then(|r| r.agent.clone()),
            "step": asking_row.as_ref().and_then(|r| r.step.clone()),
        }),
        documents: ticket_documents(ctx, &key),
        memory: memory_dir.to_path_buf(),
    };
    let request = request(config, &input)?;
    let ids: Vec<String> = rows.iter().map(|r| r.id.clone()).collect();
    let launched = launch_claimed(ctx, ledger, &request, |ledger, launched| {
        let claimed = ledger.set_questions_judging(&ids, &launched.id.to_string())?;
        if claimed == 0 {
            bail!("no card of {asking} was still new");
        }
        Ok(())
    })?;
    ledger.add_event(
        &Event::now("tick", "judge-started")
            .ticket(key.as_str())
            .external_id(&format!("judge-started:{}", launched.id))
            .detail(json!({"turn": launched.id.to_string(), "asking": asking, "cards": ids})),
    )?;
    ctx.log(&format!(
        "questions: started the Judge turn {} for {}",
        launched.id,
        ids.join(", ")
    ));
    Ok(())
}

// ----- The result ------------------------------------------------------------------------

/// Applies the result of each Judge turn that ended.
pub(crate) fn apply_all(
    ctx: &Ctx,
    ledger: &Ledger,
    memory: &Memory,
    now: OffsetDateTime,
) -> Result<()> {
    let turns: BTreeSet<String> = ledger
        .questions_in_states(&[STATE_JUDGING])?
        .into_iter()
        .filter_map(|r| r.judge_turn)
        .collect();
    let mut failed = Vec::new();
    for judge_turn in turns {
        if let Err(e) = apply(ctx, ledger, memory, &judge_turn, now) {
            ctx.log(&format!(
                "questions: could not apply the Judge turn {judge_turn}: {e:#}"
            ));
            failed.push(judge_turn);
        }
    }
    if !failed.is_empty() {
        bail!("could not apply the Judge turn(s) {}", failed.join(", "));
    }
    Ok(())
}

/// The last turn of the chain of automatic resumes that starts at `id`, with its outcome.
/// The rows that `id` judges move to that turn.
fn last_of_chain(
    ctx: &Ctx,
    ledger: &Ledger,
    id: &str,
    move_rows: bool,
) -> Result<(String, turn::TurnOutcome)> {
    let mut id = id.to_string();
    for _ in 0..10 {
        let outcome = turn::outcome(ctx, ledger, &TurnId::parse(&id)?)?;
        match (&outcome.status, &outcome.retried_by) {
            (TurnStatus::Retried, Some(next)) => {
                if move_rows {
                    ledger.move_judge_turn(&id, next)?;
                }
                id = next.clone();
            }
            _ => return Ok((id, outcome)),
        }
    }
    bail!("the Judge turn {id} has too many automatic resumes")
}

fn apply(
    ctx: &Ctx,
    ledger: &Ledger,
    memory: &Memory,
    judge_turn: &str,
    now: OffsetDateTime,
) -> Result<()> {
    let (judge_turn, outcome) = last_of_chain(ctx, ledger, judge_turn, true)?;
    let rows: Vec<QuestionRow> = ledger
        .questions_in_states(&[STATE_JUDGING])?
        .into_iter()
        .filter(|r| r.judge_turn.as_deref() == Some(judge_turn.as_str()))
        .collect();
    if rows.is_empty() {
        return Ok(());
    }
    match outcome.status {
        TurnStatus::Running | TurnStatus::Retried => Ok(()),
        TurnStatus::Failed => {
            let reason = outcome.reason.map_or("unknown".into(), |r| r.to_string());
            let rule = format!(
                "the Judge turn {judge_turn} failed with the reason {reason}, so the person answers"
            );
            for row in &rows {
                to_person(ledger, &row.id, &[STATE_JUDGING], &rule, Some(&judge_turn))?;
            }
            ctx.log(&format!("questions: {rule}"));
            Ok(())
        }
        TurnStatus::Done => apply_result(ctx, ledger, memory, &judge_turn, &outcome, &rows, now),
    }
}

/// Sends a card to the person, with the reason of the rule.
pub(crate) fn to_person(
    ledger: &Ledger,
    id: &str,
    from: &[&str],
    rule: &str,
    judge_turn: Option<&str>,
) -> Result<bool> {
    let view = serde_json::to_string(&JudgeView {
        judge: None,
        rule: Some(rule.to_string()),
    })?;
    ledger.set_question_ruling(
        id,
        from,
        &RulingRow {
            state: STATE_ASKED,
            outcome: Outcome::Ask.as_str(),
            basis: Some("none"),
            sure: None,
            proposal: &view,
            sources: None,
            answer: None,
            answered: None,
        },
        judge_turn,
    )
}

fn apply_result(
    ctx: &Ctx,
    ledger: &Ledger,
    memory: &Memory,
    judge_turn: &str,
    outcome: &turn::TurnOutcome,
    rows: &[QuestionRow],
    now: OffsetDateTime,
) -> Result<()> {
    let config = ctx.config()?;
    let entries = entries(ctx, judge_turn, outcome.result.as_ref());
    let known: BTreeSet<&str> = rows.iter().map(|r| r.id.as_str()).collect();
    let ignored: Vec<&String> = entries
        .keys()
        .filter(|id| !known.contains(id.as_str()))
        .collect();
    if !ignored.is_empty() {
        ctx.log(&format!(
            "questions: the Judge turn {judge_turn} answered the unknown card(s) {}, so those entries are ignored",
            ignored.iter().map(|s| s.as_str()).collect::<Vec<_>>().join(", ")
        ));
    }
    let answered = time::rfc3339(now);
    let active = |id: &str| memory.is_active(id);
    let mut counts: BTreeMap<&'static str, usize> = BTreeMap::new();
    let mut agents = Agents::default();
    let mut recording = Vec::new();
    for row in rows {
        let card = serde_json::from_str::<Card>(&row.card).unwrap_or_default();
        let entry = entries.get(&row.id);
        let ruling = rules::rule(&card, entry, &config.judge, &active);
        let view = serde_json::to_string(&JudgeView {
            judge: entry.cloned(),
            rule: ruling.reason.clone(),
        })?;
        let sources = entry
            .map(|e| serde_json::to_string(&e.sources))
            .transpose()?;
        let (state, answer) = match ruling.outcome {
            Outcome::Answer => {
                let entry = entry.context("an answer always has an entry")?;
                let reply = entry
                    .reply(&card)
                    .context("rule 4 makes each answer name a choice or a text")?;
                let mut answer = Answer::new(&reply, &card, JUDGE);
                answer.why = entry.reason.trim().to_string();
                (STATE_RECORDING, Some(serde_json::to_string(&answer)?))
            }
            Outcome::Propose => (STATE_PROPOSED, None),
            Outcome::Ask => (STATE_ASKED, None),
        };
        let written = ledger.set_question_ruling(
            &row.id,
            &[STATE_JUDGING],
            &RulingRow {
                state,
                outcome: ruling.outcome.as_str(),
                basis: Some(entry.map_or("none", |e| e.basis.as_str())),
                sure: entry.map(|e| e.sure),
                proposal: &view,
                sources: sources.as_deref(),
                answer: answer.as_deref(),
                answered: answer.is_some().then_some(answered.as_str()),
            },
            Some(judge_turn),
        )?;
        if !written {
            continue;
        }
        *counts.entry(ruling.outcome.as_str()).or_default() += 1;
        if state == STATE_RECORDING {
            recording.push(row.id.clone());
        }
    }

    // Each principle that a final answer of this turn names as a source counts as used. The
    // event makes the count happen once for each turn, also after a crash in the loop above.
    let mut used: Vec<PrincipleId> = Vec::new();
    let mut asking: Vec<&str> = rows.iter().map(|r| r.turn.as_str()).collect();
    asking.dedup();
    for turn in asking {
        for row in ledger.questions_of_turn(turn)? {
            if row.judge_turn.as_deref() == Some(judge_turn)
                && row.outcome.as_deref() == Some(Outcome::Answer.as_str())
            {
                let sources: Vec<String> = row
                    .sources
                    .as_deref()
                    .and_then(|s| serde_json::from_str(s).ok())
                    .unwrap_or_default();
                used.extend(record::principles(memory, &sources));
            }
        }
    }
    let first_count = ledger
        .add_event(
            &Event::now("tick", "judge-uses")
                .ticket(&outcome.ticket)
                .external_id(&format!("questions-uses:{judge_turn}"))
                .detail(json!({"principles": used})),
        )?
        .is_some();
    if first_count {
        record::count_ids(ctx, memory, &used, UseKind::Used, &Writer::Judge);
    }

    let mut failed = 0;
    for id in &recording {
        let Some(row) = ledger.question(id)? else {
            continue;
        };
        let question = Question::load(ledger, row, &mut agents)?;
        let Some(answer) = question.answer.clone() else {
            continue;
        };
        if let Err(e) = record::record(ctx, ledger, memory, &question, &answer, false) {
            failed += 1;
            ctx.log(&format!("questions: {e:#}. A later tick tries again"));
        }
    }
    ledger.add_event(
        &Event::now("tick", "judge-applied")
            .ticket(&outcome.ticket)
            .external_id(&format!("judge-applied:{judge_turn}"))
            .detail(json!({"turn": judge_turn, "outcomes": counts, "ignored": ignored})),
    )?;
    ctx.log(&format!(
        "questions: applied the Judge turn {judge_turn}: {} answer, {} propose, {} ask",
        counts.get("answer").unwrap_or(&0),
        counts.get("propose").unwrap_or(&0),
        counts.get("ask").unwrap_or(&0)
    ));
    if failed > 0 {
        bail!("{failed} decision file(s) of the Judge turn {judge_turn} could not be written");
    }
    Ok(())
}

// ----- Rule 5 ----------------------------------------------------------------------------

/// Sends each card that has no outcome `caps.judge_minutes` after it came in to the person,
/// and kills a Judge turn that still runs for it.
pub(crate) fn timeouts(ctx: &Ctx, ledger: &Ledger, now: OffsetDateTime) -> Result<()> {
    let minutes = ctx.config()?.caps.judge_minutes;
    let limit = i64::from(minutes) * 60;
    let late: Vec<QuestionRow> = ledger
        .questions_in_states(&[STATE_NEW, STATE_JUDGING])?
        .into_iter()
        .filter(|row| {
            row.created
                .as_deref()
                .and_then(|c| time::parse_rfc3339(c).ok())
                .is_none_or(|at| time::seconds_between(at, now) >= limit)
        })
        .collect();
    if late.is_empty() {
        return Ok(());
    }
    let turns: BTreeSet<String> = late.iter().filter_map(|r| r.judge_turn.clone()).collect();
    for judge_turn in turns {
        let (last, outcome) = match last_of_chain(ctx, ledger, &judge_turn, false) {
            Ok(found) => found,
            Err(e) => {
                ctx.log(&format!(
                    "questions: could not read the Judge turn {judge_turn}: {e:#}"
                ));
                continue;
            }
        };
        if outcome.status == TurnStatus::Running {
            let key = TicketKey::parse(&outcome.ticket)?;
            match turn::kill_turn(ctx, &key, &TurnId::parse(&last)?) {
                Ok(true) => ctx.log(&format!(
                    "questions: killed the Judge turn {last}, because it gave no outcome in {minutes} minutes"
                )),
                Ok(false) => {}
                Err(e) => ctx.log(&format!(
                    "questions: could not kill the Judge turn {last}: {e:#}"
                )),
            }
        }
    }
    let rule =
        format!("rule 5: the Judge gave no outcome in {minutes} minutes, so the person answers");
    for row in &late {
        to_person(ledger, &row.id, &[STATE_NEW, STATE_JUDGING], &rule, None)?;
    }
    ctx.log(&format!(
        "questions: {} card(s) went to the person: {rule}",
        late.len()
    ));
    Ok(())
}
