//! The tick hooks of the pipeline (section 11.3): they gather the facts of each ticket, ask
//! [`decide`] what it needs, and apply the answer. Each state change is one ledger transaction
//! with an event row. A crash between two lines leaves a state that the next tick repairs:
//! the turns, the operations, the checks, and the events prove the progress, so nothing runs
//! two times and no document is lost.

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

use ::time::OffsetDateTime;
use anyhow::{bail, Context, Result};
use serde_json::{json, Map, Value};

use super::decide::{
    check_end_facts, ci_result, decide, findings_branch, wait_target, Branch, Check, FactInputs,
    Facts, JobFact, KindFacts, LastTurn, List, Next, OpFact, Refused,
};
use super::docs::{self, result_documents};
use super::gates;
use super::state::{flags_of, flags_text, waits_on, Counts, FlagReason, Seen, WAITS_FOR_LIMIT};
use crate::adapter::{self, Adapter, Entry, EntryKind, Expect, GateRule, Step, StepKind};
use crate::ctx::Ctx;
use crate::executor::{self, Outcome, Secrets};
use crate::ledger::memory_use::MemoryUse;
use crate::ledger::notices::{notice_name, NewNotice, NoticeIssue};
use crate::ledger::operations::CheckRow;
use crate::ledger::pipeline::{FindingRow, ReviewRowNew, SpanRow};
use crate::ledger::turns::TurnRow;
use crate::ledger::{Event, Ledger, TicketRow};
use crate::paths::{expand_tilde, TicketKey, TurnId};
use crate::tick::{Action, FlagSet, JobRun, OperationRun, Pass, TurnStart};
use crate::turn::{self, TurnStatus};
use crate::util::fs::{atomic_write, read_optional};
use crate::util::{git, time};

/// The variable that makes a tick stop at a named point, as a crash would. Only the tests set
/// it, to prove that the next tick repairs the state.
pub const CRASH_ENV: &str = "VISION_TEST_CRASH";
/// The exit code of such a stop.
pub const CRASH_CODE: i32 = 86;

/// Stops the process at the point `name` when [`CRASH_ENV`] names it.
pub fn crash_point(name: &str) {
    if std::env::var(CRASH_ENV).is_ok_and(|v| v == name) {
        std::process::exit(CRASH_CODE);
    }
}

/// One ticket that the pipeline moves.
#[derive(Clone, Debug)]
pub(crate) struct Ticket {
    pub key: TicketKey,
    pub row: TicketRow,
    pub counts: Counts,
    pub flags: Vec<String>,
}

impl Ticket {
    pub fn of(row: TicketRow) -> Result<Ticket> {
        Ok(Ticket {
            key: TicketKey::parse(&row.key)?,
            counts: Counts::of(&row),
            flags: flags_of(&row),
            row,
        })
    }

    pub fn state(&self) -> &str {
        self.row.state.as_deref().unwrap_or("")
    }

    pub fn visit(&self) -> super::state::Visit {
        self.counts.visit.clone().unwrap_or_default()
    }

    pub fn clone_dir(&self) -> Result<PathBuf> {
        let text = self
            .row
            .clone
            .as_deref()
            .filter(|c| !c.trim().is_empty())
            .with_context(|| {
                format!("{} has no clone. Run `vision take {}`", self.key, self.key)
            })?;
        expand_tilde(text)
    }
}

/// Runs `f` in one transaction of the ledger.
pub(crate) fn in_tx<T>(ledger: &Ledger, f: impl FnOnce(&Ledger) -> Result<T>) -> Result<T> {
    ledger
        .conn()
        .execute_batch("BEGIN IMMEDIATE")
        .context("could not start a transaction of the ledger")?;
    match f(ledger) {
        Ok(value) => {
            if let Err(e) = ledger.conn().execute_batch("COMMIT") {
                let _ = ledger.conn().execute_batch("ROLLBACK");
                return Err(anyhow::Error::from(e).context("could not commit to the ledger"));
            }
            Ok(value)
        }
        Err(e) => {
            let _ = ledger.conn().execute_batch("ROLLBACK");
            Err(e)
        }
    }
}

/// The tickets that the pipeline still moves: a step, and not finished at the end.
pub(crate) fn active_tickets(ledger: &Ledger) -> Result<Vec<Ticket>> {
    let mut out = Vec::new();
    for row in ledger.tickets()? {
        if row.state.as_deref().is_none_or(str::is_empty) {
            continue;
        }
        let Ok(ticket) = Ticket::of(row) else {
            continue;
        };
        if ticket.key.is_reserved() || ticket.counts.finished.is_some() {
            continue;
        }
        out.push(ticket);
    }
    Ok(out)
}

/// The last commit of a clone.
pub(crate) fn head_of(clone: &std::path::Path) -> Result<String> {
    git::run(clone, &["rev-parse", "HEAD"]).with_context(|| {
        format!(
            "could not read the last commit of the clone {}",
            clone.display()
        )
    })
}

/// The lines of `git status --porcelain` of a clone.
pub(crate) fn dirty_lines(clone: &std::path::Path) -> Result<Vec<String>> {
    let text = git::run(
        clone,
        &["status", "--porcelain", "--untracked-files=normal"],
    )?;
    Ok(text
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(str::to_string)
        .collect())
}

/// Writes the row of a ticket and its event in one transaction.
fn save(ledger: &Ledger, ticket: &Ticket, now: &str, event: Option<Event>) -> Result<()> {
    in_tx(ledger, |ledger| {
        ledger.set_ticket_state(
            ticket.key.as_str(),
            ticket.state(),
            &flags_text(&ticket.flags),
            &ticket.counts.text(),
            now,
        )?;
        if let Some(event) = &event {
            ledger.add_event(event)?;
        }
        Ok(())
    })
}

/// The visit of the current step. A row with no visit, or a visit of a different step, gets a
/// new one: the repair of a row that an older writer or a crash left.
fn ensure_visit(ctx: &Ctx, ledger: &Ledger, ticket: &mut Ticket, step: &Step) -> Result<()> {
    if ticket
        .counts
        .visit
        .as_ref()
        .is_some_and(|v| v.step == step.id)
    {
        return Ok(());
    }
    let now = time::now_rfc3339();
    let turns = u32::try_from(ledger.step_turn_ids(ticket.key.as_str(), &step.id)?.len())?;
    let base = ticket.clone_dir().ok().and_then(|c| head_of(&c).ok());
    let visit = ticket
        .counts
        .enter(step, "repair", &now, turns, base, false);
    in_tx(ledger, |ledger| {
        ledger.close_spans(ticket.key.as_str(), &now)?;
        ledger.open_span(&span(ticket, step, "repair", &now))?;
        ledger.set_ticket_state(
            ticket.key.as_str(),
            ticket.state(),
            &flags_text(&ticket.flags),
            &ticket.counts.text(),
            &now,
        )?;
        ledger.add_event(
            &Event::now("tick", "visit")
                .ticket(ticket.key.as_str())
                .detail(json!({"step": step.id, "seq": visit.seq, "how": "repair"})),
        )?;
        Ok(())
    })?;
    ctx.log(&format!(
        "pipeline: {} had no visit of the step {}, so it got one",
        ticket.key, step.id
    ));
    Ok(())
}

pub(crate) fn span(ticket: &Ticket, step: &Step, how: &str, now: &str) -> SpanRow {
    SpanRow {
        ticket: ticket.key.to_string(),
        state: step.id.clone(),
        waits_on: Some(waits_on(step)),
        reason: Some(how.to_string()),
        started: now.to_string(),
        ended: None,
    }
}

/// The turns of the visit: the work turns of its step after the turns of the earlier visits.
pub(crate) fn visit_turns(
    ledger: &Ledger,
    key: &TicketKey,
    step: &str,
    before: u32,
) -> Result<Vec<TurnRow>> {
    let mut out = Vec::new();
    for id in ledger
        .step_turn_ids(key.as_str(), step)?
        .into_iter()
        .skip(before as usize)
    {
        if let Some(row) = ledger.turn(&id)? {
            let alone = row
                .agent
                .as_deref()
                .and_then(crate::models::agent)
                .is_some_and(|spec| spec.alone);
            if !alone {
                out.push(row);
            }
        }
    }
    Ok(out)
}

// ----- Tick step 5 -----------------------------------------------------------------------

/// Tick step 5: for each ticket, read the facts of its step and test its end facts. A turn
/// result gets its documents written once and its end facts tested; a wait step asks
/// `observe` each `every` seconds.
pub fn test_end_facts(pass: &mut Pass) -> Result<()> {
    let ctx = pass.ctx;
    let tickets = active_tickets(&pass.ledger)?;
    if tickets.is_empty() {
        return Ok(());
    }
    let adapter = adapter::load(ctx)?;
    let mut failed = Vec::new();
    for mut ticket in tickets {
        if !ticket.flags.is_empty() {
            continue;
        }
        let Some(step) = adapter.step(ticket.state()).cloned() else {
            continue;
        };
        let result =
            ensure_visit(ctx, &pass.ledger, &mut ticket, &step).and_then(|()| match &step.kind {
                StepKind::Turn(_) => test_turn(ctx, &pass.ledger, &mut ticket, &step),
                StepKind::Wait(_) => observe_wait(ctx, &pass.ledger, &mut ticket, &step, pass.now),
                _ => Ok(()),
            });
        if let Err(e) = result {
            ctx.log(&format!(
                "pipeline: the facts of {} could not be tested: {e:#}",
                ticket.key
            ));
            failed.push(ticket.key.to_string());
        }
    }
    if !failed.is_empty() {
        // The other tickets still move: steps 6 and 7 run for them.
        ctx.log(&format!(
            "pipeline: the facts of {} wait for the next tick",
            failed.join(", ")
        ));
    }
    Ok(())
}

/// The documents of the step: the names of its `doc:` end facts and the documents that its
/// gates read, each one once.
pub(crate) fn expected_docs(step: &Step) -> Vec<String> {
    let mut names: Vec<String> = Vec::new();
    for expect in &step.expects {
        let name = match expect {
            Expect::Doc(name) => name,
            Expect::Gate(gate) => &gate.doc,
            _ => continue,
        };
        if !names.contains(name) {
            names.push(name.clone());
        }
    }
    names
}

/// The marker of the documents of a turn that the pipeline wrote.
fn docs_mark(turn: &str) -> String {
    format!("docs-written:{turn}")
}

/// The marker of the end facts of a turn.
fn fact_mark(turn: &str) -> String {
    format!("end-fact:{turn}")
}

fn test_turn(ctx: &Ctx, ledger: &Ledger, ticket: &mut Ticket, step: &Step) -> Result<()> {
    let visit = ticket.visit();
    let turns = visit_turns(ledger, &ticket.key, &step.id, visit.turns)?;
    let Some(last) = turns.last() else {
        return Ok(());
    };
    if last.status.as_deref() != Some(TurnStatus::Done.as_str()) || last.end_fact.is_some() {
        return Ok(());
    }
    let outcome = turn::outcome(ctx, ledger, &TurnId::parse(&last.id)?)?;
    let Some(result) = outcome.result else {
        return Ok(());
    };
    if result.get("kind").and_then(Value::as_str) != Some("result") {
        return Ok(());
    }
    let expected = expected_docs(step);
    let found = result_documents(&result, &expected);
    if ledger.event_detail(&docs_mark(&last.id))?.is_none() {
        write_result_docs(ctx, ledger, ticket, step, last, &result, &found)?;
        crash_point("documents-written");
    }

    let clone = ticket.clone_dir()?;
    let head = head_of(&clone)?;
    let mut inputs = FactInputs {
        docs: found.docs.clone(),
        pointers: found.pointers.clone(),
        head: Some(head.clone()),
        base: visit.base.clone(),
        dirty: dirty_lines(&clone)?,
        jobs: Default::default(),
        human: human_actions(ledger, &ticket.key)?,
    };
    let mut job_args = BTreeMap::new();
    for expect in &step.expects {
        if let Expect::Job(job) = expect {
            let args = end_fact_args(ctx, &ticket.key, step, job);
            // A job that did not pass at this commit with these arguments runs again: a result
            // is tested once, and a job can fail for a reason outside the commit, such as a
            // slow machine.
            let fact =
                match executor::job_passed_with(ledger, ticket.key.as_str(), job, &head, &args)? {
                    Some(true) => JobFact::Passed,
                    None | Some(false) => JobFact::NotRun,
                };
            inputs.jobs.insert(job.clone(), fact);
            job_args.insert(job.clone(), args);
        }
    }
    let mut job_docs = Vec::new();
    let check = loop {
        match check_end_facts(&step.expects, &inputs) {
            Check::RunJob(job) => {
                let run = JobRun {
                    ticket: ticket.key.clone(),
                    step: step.id.clone(),
                    job: job.clone(),
                    args: job_args
                        .get(&job)
                        .cloned()
                        .unwrap_or_else(|| Value::Object(Map::new())),
                };
                let result = executor::job(ctx, ledger, &run)?;
                let fact = match (&result.failure, result.ok) {
                    (None, true) => JobFact::Passed,
                    (Some(failure), _) => JobFact::Failed(failure.clone()),
                    (None, false) => JobFact::Failed(result.detail.clone()),
                };
                if fact != JobFact::Passed {
                    job_docs.extend(result.documents.clone());
                }
                inputs.jobs.insert(job, fact);
            }
            other => break other,
        }
    };
    let now = time::now_rfc3339();
    let (value, detail) = match &check {
        Check::Held => ("held".to_string(), json!({"turn": last.id, "held": true})),
        Check::Failed { fact, found } => (
            format!("failed:{fact}"),
            json!({"turn": last.id, "fact": fact, "found": found, "documents": job_docs}),
        ),
        Check::RunJob(_) => return Ok(()),
    };
    if let Check::Failed { .. } = &check {
        if let Some(visit) = ticket.counts.visit.as_mut() {
            for doc in &job_docs {
                if !visit.extra.contains(doc) {
                    visit.extra.push(doc.clone());
                }
            }
        }
    }
    for note in gate_notes(step, &found.docs) {
        ctx.log(&format!("pipeline: a gate of {} notes: {note}", last.id));
    }
    let notices = match &check {
        Check::Held => gate_notices(ctx, step, &found.docs)?,
        _ => Vec::new(),
    };
    in_tx(ledger, |ledger| {
        ledger.set_end_fact(&last.id, &value)?;
        ledger.set_ticket_state(
            ticket.key.as_str(),
            ticket.state(),
            &flags_text(&ticket.flags),
            &ticket.counts.text(),
            &now,
        )?;
        ledger.add_event(
            &Event::now("tick", "end-fact")
                .ticket(ticket.key.as_str())
                .external_id(&fact_mark(&last.id))
                .detail(detail.clone()),
        )?;
        for notice in &notices {
            let added = ledger.add_notice(&NewNotice {
                ticket: ticket.key.as_str(),
                step: &step.id,
                turn: &last.id,
                gate: &notice.gate,
                doc: &notice.doc,
                issues: &notice.issues,
                note: notice.note.as_deref(),
                created: &now,
            })?;
            if let Some(id) = added {
                ledger.add_event(
                    &Event::now("tick", "notice")
                        .ticket(ticket.key.as_str())
                        .external_id(&format!("notice:{}:{}", last.id, notice.doc))
                        .detail(json!({
                            "notice": notice_name(id), "turn": last.id, "step": step.id,
                            "doc": notice.doc, "issues": notice.issues.len(),
                        })),
                )?;
            }
        }
        Ok(())
    })?;
    ctx.log(&format!(
        "pipeline: the end facts of {} at the step {}: {value}",
        last.id, step.id
    ));
    Ok(())
}

/// A notice that a passing gate qa with notify = true gives: the issues of its report that do
/// not block.
struct GateNotice {
    gate: String,
    doc: String,
    issues: Vec<NoticeIssue>,
    note: Option<String>,
}

/// The notices of the gates of a step whose end facts held. Each secret value in a title or a
/// note is masked, as in the documents of the ticket folder.
fn gate_notices(
    ctx: &Ctx,
    step: &Step,
    docs: &BTreeMap<String, String>,
) -> Result<Vec<GateNotice>> {
    let mut out = Vec::new();
    for expect in &step.expects {
        let Expect::Gate(gate) = expect else {
            continue;
        };
        let GateRule::Qa {
            block,
            notify: true,
        } = &gate.rule
        else {
            continue;
        };
        let Some(text) = docs.get(&gate.doc) else {
            continue;
        };
        let issues = gates::notice_issues(block, text);
        if issues.is_empty() {
            continue;
        }
        let secrets = Secrets::read(ctx.config()?)?;
        out.push(GateNotice {
            gate: gate.kind().to_string(),
            doc: gate.doc.clone(),
            issues: issues
                .into_iter()
                .map(|i| NoticeIssue {
                    id: i.id,
                    severity: i.severity.map(|s| s.to_string()).unwrap_or_default(),
                    title: secrets.mask(&i.title),
                })
                .collect(),
            note: gates::qa_report(text)
                .disagreement()
                .map(|n| secrets.mask(&n)),
        });
    }
    Ok(out)
}

/// The notes of the gates of a step that read their document, such as a count table that
/// disagrees with the issues, for the log.
fn gate_notes(step: &Step, docs: &BTreeMap<String, String>) -> Vec<String> {
    step.expects
        .iter()
        .filter_map(|e| match e {
            Expect::Gate(gate) => docs
                .get(&gate.doc)
                .and_then(|text| gates::judge(gate, text).ok()),
            _ => None,
        })
        .flatten()
        .collect()
}

/// Writes the expected documents of a result to the ticket folder once, with the evidence
/// that they carry: the findings of the step, the verdicts of review comments, and the
/// principles that the turn got.
fn write_result_docs(
    ctx: &Ctx,
    ledger: &Ledger,
    ticket: &Ticket,
    step: &Step,
    last: &TurnRow,
    result: &Value,
    found: &docs::ResultDocs,
) -> Result<()> {
    let key = &ticket.key;
    let secrets = Secrets::read(ctx.config()?)?;
    std::fs::create_dir_all(ctx.paths.ticket_dir(key))
        .with_context(|| format!("could not make {}", ctx.paths.ticket_dir(key).display()))?;
    let mut written = Vec::new();
    for (name, text) in &found.docs {
        let text = secrets.mask(text);
        atomic_write(&ctx.paths.ticket_doc(key, name), text.as_bytes())?;
        written.push(name.clone());
        // The first text of the agent stays, so that the Curator sees the edits of the person.
        if name == "plan.md" {
            let agent = ctx.paths.ticket_doc(key, "plan.agent.md");
            if !agent.exists() {
                atomic_write(&agent, text.as_bytes())?;
                written.push("plan.agent.md".into());
            }
        }
    }
    for note in &found.ignored {
        ctx.log(&format!(
            "pipeline: a document of {} is left out: {note}",
            last.id
        ));
    }
    let round = i64::from(ticket.visit().n.max(1));
    let mut findings = Vec::new();
    if let StepKind::Turn(turn) = &step.kind {
        if let Some(source) = &turn.findings {
            let earlier: BTreeSet<String> = ledger
                .findings_of(key.as_str())?
                .into_iter()
                .filter(|f| f.round < round || f.source != *source)
                .map(|f| f.fingerprint)
                .collect();
            for text in found.docs.values() {
                for f in docs::findings(text) {
                    let fingerprint = f.fingerprint();
                    findings.push(FindingRow {
                        ticket: key.to_string(),
                        round,
                        came_back: earlier.contains(&fingerprint),
                        fingerprint,
                        priority: f.priority,
                        source: source.clone(),
                        verdict: f.verdict,
                        reason_code: f.reason_code,
                        fix_commit: f.fix_commit,
                    });
                }
            }
        }
    }
    let mut reviews = Vec::new();
    if let Some(text) = found.docs.get("verdicts.json") {
        match docs::verdicts(text) {
            Ok(rows) => {
                // The comments come from a document that the step reads: a JSON list of
                // comments with `id` and `author`, whatever its name.
                let mut authors = std::collections::BTreeMap::new();
                if let StepKind::Turn(turn) = &step.kind {
                    for name in &turn.reads {
                        if let Some(text) = read_optional(&ctx.paths.ticket_doc(key, name))? {
                            authors.extend(docs::comment_authors(&text));
                        }
                    }
                }
                for row in rows {
                    reviews.push(ReviewRowNew {
                        round: format!("{key}:{round}"),
                        comment_id: row.comment_id,
                        reviewer: authors.get(&row.comment_id).cloned(),
                        verdict: row.verdict,
                        fix: i64::from(row.fix_commit.is_some()),
                    });
                }
            }
            Err(e) => ctx.log(&format!(
                "pipeline: verdicts.json of {} is not valid, so no review row was written: {e}",
                last.id
            )),
        }
    }
    let principles = principles_of(ctx, ticket, last, result);
    in_tx(ledger, |ledger| {
        for row in &findings {
            ledger.add_finding(row)?;
        }
        for row in &reviews {
            ledger.add_review_row(row)?;
        }
        if let Some((list, uses)) = &principles {
            ledger.set_turn_principles(&last.id, list)?;
            for row in uses {
                ledger.add_memory_use(row)?;
            }
        }
        ledger.add_event(
            &Event::now("tick", "documents")
                .ticket(key.as_str())
                .external_id(&docs_mark(&last.id))
                .detail(json!({
                    "turn": last.id, "written": written, "ignored": found.ignored,
                    "findings": findings.len(), "reviews": reviews.len(),
                })),
        )?;
        Ok(())
    })?;
    Ok(())
}

/// The principles that a turn got (from its facts file) and the ones that its result applied.
fn principles_of(
    ctx: &Ctx,
    ticket: &Ticket,
    last: &TurnRow,
    result: &Value,
) -> Option<(String, Vec<MemoryUse>)> {
    let id = TurnId::parse(&last.id).ok()?;
    let facts = read_optional(&ctx.paths.turn(&ticket.key, &id).facts).ok()??;
    let facts: Value = serde_json::from_str(&facts).ok()?;
    let loaded = facts.get("principles_loaded")?.as_array()?.clone();
    let applied: Vec<(String, String)> = result
        .get("applied")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(|a| {
                    Some((
                        a.get("id")?.as_str()?.trim().to_string(),
                        a.get("how")
                            .and_then(Value::as_str)
                            .unwrap_or("")
                            .to_string(),
                    ))
                })
                .collect()
        })
        .unwrap_or_default();
    let mut uses = Vec::new();
    for item in &loaded {
        let Some(file) = item.get("id").and_then(Value::as_str) else {
            continue;
        };
        let how = applied
            .iter()
            .find(|(id, _)| id == file)
            .map(|(_, h)| h.clone());
        uses.push(MemoryUse {
            turn: last.id.clone(),
            file: file.to_string(),
            version: item
                .get("version")
                .and_then(Value::as_str)
                .map(str::to_string),
            applied: how.is_some(),
            how,
        });
    }
    Some((Value::Array(loaded).to_string(), uses))
}

/// The human actions that the person did for a ticket, from the events `human`.
pub(crate) fn human_actions(ledger: &Ledger, key: &TicketKey) -> Result<BTreeSet<String>> {
    Ok(ledger
        .events_of_ticket(key.as_str())?
        .into_iter()
        .filter(|e| e.kind == "human")
        .filter_map(|e| {
            let detail: Value = serde_json::from_str(e.detail.as_deref()?).ok()?;
            detail.get("action")?.as_str().map(str::to_string)
        })
        .collect())
}

/// The external id of the event that records a human action of one visit.
pub(crate) fn human_mark(key: &TicketKey, seq: u32, action: &str) -> String {
    format!("human:{key}:{seq}:{action}")
}

fn observe_wait(
    ctx: &Ctx,
    ledger: &Ledger,
    ticket: &mut Ticket,
    step: &Step,
    now: OffsetDateTime,
) -> Result<()> {
    let StepKind::Wait(wait) = &step.kind else {
        return Ok(());
    };
    let start = op_facts(ctx, ledger, ticket, &step.start, None)?;
    if start.iter().any(|(_, f)| {
        !matches!(
            f,
            OpFact::Known {
                outcome: Outcome::Confirmed,
                ..
            }
        )
    }) {
        return Ok(());
    }
    let visit = ticket.visit();
    let due = visit
        .observed
        .as_deref()
        .and_then(|t| time::parse_rfc3339(t).ok())
        .is_none_or(|at| {
            time::seconds_between(at, now) >= i64::try_from(wait.every).unwrap_or(i64::MAX)
        });
    if !due {
        return Ok(());
    }
    let at = time::rfc3339(now);
    // The clock of `limit` starts at the first look of the visit, also when that look fails.
    let first = visit.since.is_none();
    if first {
        if let Some(visit) = ticket.counts.visit.as_mut() {
            visit.since = Some(at.clone());
        }
    }
    let facts = match executor::observe_ticket(ctx, ledger, &ticket.key) {
        Ok(facts) => facts,
        Err(e) => {
            if first {
                save(ledger, ticket, &at, None)?;
            }
            return Err(e);
        }
    };
    // A list or a table is no value of `on`: its JSON text names it in the reason of a flag.
    let watched = facts.watch_text(&wait.watch).or_else(|| {
        facts.watch(&wait.watch).map(|v| {
            let text = v.to_string();
            text.chars().take(100).collect::<String>()
        })
    });
    ticket.counts.seen = Some(Seen::of(&facts, &at));
    if let Some(visit) = ticket.counts.visit.as_mut() {
        visit.observed = Some(at.clone());
        visit.watched = watched.clone();
    }
    // The CI facts of each observe answer, whatever the step watches: a run that ended is one
    // check row for its commit and its result.
    let check = facts.ci.as_ref().and_then(|ci| {
        let result = ci_result(&ci.state)?;
        let head = ci.head.clone()?;
        Some((head, result))
    });
    let check = match check {
        Some((head, result)) if !ledger.has_check(ticket.key.as_str(), "ci", &head, result)? => {
            Some(CheckRow {
                ticket: ticket.key.to_string(),
                head,
                kind: "ci".into(),
                result: result.into(),
                command: wait.watch.clone(),
                seconds: 0,
                cause: String::new(),
                source: format!("step:{}", step.id),
                at: at.clone(),
            })
        }
        _ => None,
    };
    in_tx(ledger, |ledger| {
        ledger.set_ticket_state(
            ticket.key.as_str(),
            ticket.state(),
            &flags_text(&ticket.flags),
            &ticket.counts.text(),
            &at,
        )?;
        if let Some(row) = &check {
            ledger.add_check(row)?;
        }
        Ok(())
    })?;
    Ok(())
}

// ----- Tick step 6 -----------------------------------------------------------------------

/// Tick step 6: the next action of each ticket. A move to the next step, the end of a ticket,
/// and the clearing of the limit flag happen here, each in one transaction; the turns, the jobs,
/// the operations, and the flags are actions for step 7.
pub fn select_actions(pass: &mut Pass) -> Result<Vec<Action>> {
    let ctx = pass.ctx;
    let tickets = active_tickets(&pass.ledger)?;
    if tickets.is_empty() {
        return Ok(Vec::new());
    }
    let adapter = adapter::load(ctx)?;
    let mut actions = Vec::new();
    let mut failed = Vec::new();
    for mut ticket in tickets {
        match select_one(ctx, &pass.ledger, &adapter, &mut ticket, pass.now) {
            Ok(Some(action)) => actions.push(action),
            Ok(None) => {}
            Err(e) => {
                ctx.log(&format!(
                    "pipeline: could not select the next action of {}: {e:#}",
                    ticket.key
                ));
                failed.push(ticket.key.to_string());
            }
        }
    }
    if !failed.is_empty() {
        // The actions of the other tickets still run.
        ctx.log(&format!(
            "pipeline: no action for {} in this tick",
            failed.join(", ")
        ));
    }
    Ok(actions)
}

fn select_one(
    ctx: &Ctx,
    ledger: &Ledger,
    adapter: &Adapter,
    ticket: &mut Ticket,
    now: OffsetDateTime,
) -> Result<Option<Action>> {
    let Some(step) = adapter.step(ticket.state()).cloned() else {
        if ticket.flags.is_empty() {
            return Ok(Some(flag_action(
                ticket,
                super::state::NEEDS_YOU,
                &format!(
                    "the step {} is not in the adapter {}. Fix adapter.toml, or run `vision stop {}`",
                    ticket.state(),
                    adapter.name,
                    ticket.key
                ),
            )));
        }
        return Ok(None);
    };
    // A flag other than the limit stops each action, and `decide` gives nothing for it, so its
    // facts are not gathered: a stopped ticket may have lost its clone.
    if ticket.flags.iter().any(|f| f != WAITS_FOR_LIMIT) {
        return Ok(None);
    }
    ensure_visit(ctx, ledger, ticket, &step)?;
    for _ in 0..2 {
        let facts = gather(ctx, ledger, ticket, &step, now)?;
        let next = decide(&facts);
        match next {
            Next::Nothing(_) => return Ok(None),
            Next::ClearLimit => {
                clear_limit(ctx, ledger, ticket)?;
                continue;
            }
            Next::Flag { flag, reason } => return Ok(Some(flag_action(ticket, flag, &reason))),
            Next::Start { resume, message } => {
                return enter_turn(ctx, ledger, ticket, &step, resume, message)
            }
            Next::Resume { session, message } => {
                return Ok(Some(start_action(
                    ticket,
                    &step,
                    Some(session),
                    Some(message),
                )))
            }
            Next::Serve { turn, session } => {
                return serve(ctx, ledger, adapter, ticket, &step, &turn, &session)
            }
            Next::Request { list, index } => {
                let entries = match list {
                    List::After => &step.after,
                    List::Start => &step.start,
                };
                let Some(entry) = entries.get(index) else {
                    return Ok(None);
                };
                let turn = match list {
                    List::After => last_turn_id(ledger, ticket, &step)?,
                    List::Start => None,
                };
                return request_action(ctx, ticket, entry, turn).map(Some);
            }
            Next::RunJob => {
                let StepKind::Job(job) = &step.kind else {
                    return Ok(None);
                };
                return Ok(Some(Action::RunJob(JobRun {
                    ticket: ticket.key.clone(),
                    step: step.id.clone(),
                    job: job.job.clone(),
                    args: Value::Object(job.args.clone()),
                })));
            }
            Next::Move { to, how } => {
                move_to(ctx, ledger, adapter, ticket, &step, &to, &how)?;
                return Ok(None);
            }
            Next::Finish => {
                finish(ctx, ledger, ticket, now)?;
                return Ok(None);
            }
        }
    }
    Ok(None)
}

fn flag_action(ticket: &Ticket, flag: &str, reason: &str) -> Action {
    Action::SetFlag(FlagSet {
        ticket: ticket.key.clone(),
        flag: flag.to_string(),
        reason: reason.to_string(),
    })
}

fn start_action(
    ticket: &Ticket,
    step: &Step,
    resumes: Option<String>,
    message: Option<String>,
) -> Action {
    let (agent, kind) = match &step.kind {
        StepKind::Turn(turn) => (turn.agent.clone(), turn.turn),
        _ => (String::new(), turn::TurnKind::Reader),
    };
    Action::StartTurn(TurnStart {
        ticket: ticket.key.clone(),
        step: step.id.clone(),
        agent,
        kind,
        resumes,
        message,
    })
}

fn last_turn_id(ledger: &Ledger, ticket: &Ticket, step: &Step) -> Result<Option<String>> {
    Ok(
        visit_turns(ledger, &ticket.key, &step.id, ticket.visit().turns)?
            .last()
            .map(|t| t.id.clone()),
    )
}

/// The arguments of the end-fact job `job` of a step, with each `{doc:NAME}` replaced by the
/// document of the ticket. A missing document gives an empty text: the job decides what that
/// means, such as a proof with no plan.
pub(crate) fn end_fact_args(ctx: &Ctx, key: &TicketKey, step: &Step, job: &str) -> Value {
    let read = |name: &str| {
        Some(
            read_optional(&ctx.paths.ticket_doc(key, name))
                .ok()
                .flatten()
                .unwrap_or_default(),
        )
    };
    let args = step.expect_args_of(job);
    // The loader tested each `{doc:NAME}`, so the expansion cannot fail; the plain arguments
    // are the fallback.
    docs::expand_args(&args, &read).unwrap_or(Value::Object(args))
}

/// The arguments of an entry, with each `{doc:NAME}` replaced by the document of the ticket.
pub(crate) fn entry_args(ctx: &Ctx, key: &TicketKey, entry: &Entry) -> Result<Value, String> {
    let read = |name: &str| {
        read_optional(&ctx.paths.ticket_doc(key, name))
            .ok()
            .flatten()
    };
    docs::expand_args(&entry.args, &read).map_err(|e| format!("{}: {e}", entry.label()))
}

fn request_action(
    ctx: &Ctx,
    ticket: &Ticket,
    entry: &Entry,
    turn: Option<String>,
) -> Result<Action> {
    let args = match entry_args(ctx, &ticket.key, entry) {
        Ok(args) => args,
        Err(text) => return Ok(flag_action(ticket, super::state::NEEDS_YOU, &text)),
    };
    Ok(match entry.kind {
        EntryKind::Verb => Action::RunOperation(OperationRun {
            ticket: ticket.key.clone(),
            verb: entry.name.clone(),
            args,
            turn,
        }),
        EntryKind::Job => Action::RunJob(JobRun {
            ticket: ticket.key.clone(),
            step: ticket.state().to_string(),
            job: entry.name.clone(),
            args,
        }),
    })
}

/// The state of each entry of a list: the stored operation of a verb at the commit of now, or
/// the result of a job there.
pub(crate) fn op_facts(
    ctx: &Ctx,
    ledger: &Ledger,
    ticket: &Ticket,
    entries: &[Entry],
    turn: Option<&str>,
) -> Result<Vec<(String, OpFact)>> {
    let mut out = Vec::new();
    for entry in entries {
        let args = match entry_args(ctx, &ticket.key, entry) {
            Ok(args) => args,
            Err(text) => {
                out.push((entry.label(), OpFact::BadArgs(text)));
                continue;
            }
        };
        let fact = match entry.kind {
            EntryKind::Verb => {
                let run = OperationRun {
                    ticket: ticket.key.clone(),
                    verb: entry.name.clone(),
                    args,
                    turn: turn.map(str::to_string),
                };
                match executor::lookup(ctx, ledger, &run)? {
                    None => OpFact::NotAsked,
                    Some(op) => OpFact::Known {
                        outcome: op.outcome,
                        detail: op.detail,
                    },
                }
            }
            EntryKind::Job => {
                let head = ticket.clone_dir().ok().and_then(|c| head_of(&c).ok());
                match head
                    .map(|h| executor::job_passed(ledger, ticket.key.as_str(), &entry.name, &h))
                {
                    Some(Ok(Some(true))) => OpFact::Known {
                        outcome: Outcome::Confirmed,
                        detail: String::new(),
                    },
                    Some(Ok(Some(false))) => OpFact::Known {
                        outcome: Outcome::Failed,
                        detail: format!("the job {} did not pass", entry.name),
                    },
                    Some(Err(e)) => return Err(e),
                    _ => OpFact::NotAsked,
                }
            }
        };
        out.push((entry.label(), fact));
    }
    Ok(out)
}

/// The value of a cap of the configuration by its name.
fn cap_value(ctx: &Ctx, name: &str) -> Result<Option<u32>> {
    let caps = serde_json::to_value(&ctx.config()?.caps)?;
    Ok(caps
        .get(name)
        .and_then(Value::as_u64)
        .and_then(|v| u32::try_from(v).ok()))
}

/// What is open at a step, for the message of its cap: the open items of its documents, and a
/// red CI run.
pub(crate) fn open_items(
    ctx: &Ctx,
    ledger: &Ledger,
    key: &TicketKey,
    step: &Step,
) -> Result<Vec<String>> {
    let mut open = Vec::new();
    for name in expected_docs(step) {
        if let Some(text) = read_optional(&ctx.paths.ticket_doc(key, &name))? {
            for item in docs::open_items(&text) {
                open.push(format!("{name}: {item}"));
            }
        }
    }
    if let Some(ci) = ledger
        .latest_checks(key.as_str())?
        .into_iter()
        .find(|c| c.kind == "ci")
    {
        if ci.result == "red" {
            open.push(format!("the CI is red at {}", executor::short(&ci.head)));
        }
    }
    Ok(open)
}

/// The facts of the step of one ticket, for [`decide`].
fn gather(
    ctx: &Ctx,
    ledger: &Ledger,
    ticket: &Ticket,
    step: &Step,
    now: OffsetDateTime,
) -> Result<Facts> {
    let visit = ticket.visit();
    let key = &ticket.key;
    let mut refusal = None;
    let mut branch = None;
    let kind = match &step.kind {
        StepKind::Turn(turn) => {
            refusal = turn::refusal(ctx, ledger, key, turn.turn)?.map(|r| {
                if r.is_limit() {
                    Refused::Limit(r.to_string())
                } else {
                    Refused::Other(r.to_string())
                }
            });
            let turns = visit_turns(ledger, key, &step.id, visit.turns)?;
            let failures = u32::try_from(
                turns
                    .iter()
                    .filter(|t| {
                        t.end_fact
                            .as_deref()
                            .is_some_and(|f| f.starts_with("failed:"))
                    })
                    .count(),
            )?;
            let last = match turns.last() {
                None => None,
                Some(row) => Some(last_turn(ctx, ledger, row)?),
            };
            let failure = match &last {
                Some(t)
                    if t.end_fact
                        .as_deref()
                        .is_some_and(|f| f.starts_with("failed:")) =>
                {
                    ledger
                        .event_detail(&fact_mark(&t.id))?
                        .flatten()
                        .and_then(|d| serde_json::from_str::<Value>(&d).ok())
                        .and_then(|d| d.get("found").and_then(Value::as_str).map(str::to_string))
                }
                _ => None,
            };
            let after = match &last {
                Some(t) if t.end_fact.as_deref() == Some("held") => {
                    op_facts(ctx, ledger, ticket, &step.after, Some(&t.id))?
                }
                _ => Vec::new(),
            };
            if let Some(t) = last
                .as_ref()
                .filter(|t| t.end_fact.as_deref() == Some("held"))
            {
                if !step.on.is_empty() {
                    branch = Some(turn_branch(ctx, ledger, step, &t.id)?);
                }
            }
            KindFacts::Turn {
                last,
                failures,
                failure,
                after,
                resume: visit.resume.clone(),
                message: visit.message.clone(),
            }
        }
        StepKind::Job(job) => {
            let head = head_of(&ticket.clone_dir()?)?;
            let passed = executor::job_passed(ledger, key.as_str(), &job.job, &head)?;
            let after = if passed == Some(true) {
                op_facts(ctx, ledger, ticket, &step.after, None)?
            } else {
                Vec::new()
            };
            KindFacts::Job {
                passed,
                detail: format!(
                    "see the documents of the job {} in the ticket folder",
                    job.job
                ),
                after,
            }
        }
        StepKind::Human(human) => KindFacts::Human {
            action: human.action.clone(),
            done: ledger
                .event_detail(&human_mark(key, visit.seq, &human.action))?
                .is_some(),
        },
        StepKind::Wait(wait) => KindFacts::Wait {
            start: op_facts(ctx, ledger, ticket, &step.start, None)?,
            watch: wait.watch.clone(),
            watched: visit.watched.clone(),
            target: wait_target(&step.on, visit.watched.as_deref()),
            waits: wait.wait.clone(),
            limit: wait.limit,
            waited: visit
                .since
                .as_deref()
                .and_then(|t| time::parse_rfc3339(t).ok())
                .map(|since| time::seconds_between(since, now)),
        },
        StepKind::End => KindFacts::End {
            finished: ticket.counts.finished.is_some(),
        },
    };
    let cap = match &step.cap {
        Some(name) => cap_value(ctx, name)?.map(|v| (name.clone(), v)),
        None => None,
    };
    let cap_hit = super::decide::cap_exceeded(visit.n, cap.as_ref().map(|c| c.1), visit.skip_cap);
    Ok(Facts {
        step: step.id.clone(),
        kind,
        next: step.next.clone(),
        branch,
        flags: ticket.flags.clone(),
        running: ledger.running_turns_of_ticket(key.as_str())? > 0,
        open_cards: ledger.open_cards(key.as_str())?,
        refusal,
        cap,
        n: visit.n,
        skip_cap: visit.skip_cap,
        open: if cap_hit {
            open_items(ctx, ledger, key, step)?
        } else {
            Vec::new()
        },
    })
}

/// Where a turn step with `on` moves after its held turn: `findings` when the documents of the
/// turn hold one or more kept findings, else `none`.
fn turn_branch(ctx: &Ctx, ledger: &Ledger, step: &Step, turn_id: &str) -> Result<Branch> {
    let outcome = turn::outcome(ctx, ledger, &TurnId::parse(turn_id)?)?;
    let kept = outcome
        .result
        .as_ref()
        .map(|result| {
            result_documents(result, &expected_docs(step))
                .docs
                .values()
                .flat_map(|text| docs::findings(text))
                .filter(|f| f.verdict == "kept")
                .count()
        })
        .unwrap_or(0);
    findings_branch(&step.on, kept).with_context(|| {
        format!(
            "the step {} has on with no target for its findings. Fix adapter.toml",
            step.id
        )
    })
}

fn last_turn(ctx: &Ctx, ledger: &Ledger, row: &TurnRow) -> Result<LastTurn> {
    let outcome = turn::outcome(ctx, ledger, &TurnId::parse(&row.id)?)?;
    let reason = match (&outcome.reason, &outcome.detail) {
        (Some(r), Some(d)) => format!("{r}: {d}"),
        (Some(r), None) => r.to_string(),
        (None, Some(d)) => d.clone(),
        (None, None) => "no reason was recorded".into(),
    };
    Ok(LastTurn {
        id: row.id.clone(),
        status: outcome.status,
        reason,
        kind: outcome
            .result
            .as_ref()
            .and_then(|r| r.get("kind"))
            .and_then(Value::as_str)
            .map(str::to_string),
        session: row.session.clone(),
        end_fact: row.end_fact.clone(),
    })
}

fn clear_limit(ctx: &Ctx, ledger: &Ledger, ticket: &mut Ticket) -> Result<()> {
    ticket.flags.retain(|f| f != WAITS_FOR_LIMIT);
    ticket.counts.reasons.remove(WAITS_FOR_LIMIT);
    let now = time::now_rfc3339();
    save(
        ledger,
        ticket,
        &now,
        Some(
            Event::now("tick", "flag-cleared")
                .ticket(ticket.key.as_str())
                .detail(json!({"flag": WAITS_FOR_LIMIT, "why": "the limit allows a turn again"})),
        ),
    )?;
    ctx.log(&format!(
        "pipeline: {} no longer waits for the limit",
        ticket.key
    ));
    Ok(())
}

/// The entry of a turn step: each `before` entry, `observe` with `issue.json`, and the action
/// that starts the first turn of the visit.
fn enter_turn(
    ctx: &Ctx,
    ledger: &Ledger,
    ticket: &mut Ticket,
    step: &Step,
    resume: Option<String>,
    message: Option<String>,
) -> Result<Option<Action>> {
    for (index, entry) in step.before.iter().enumerate() {
        let label = entry.label();
        // An entry that is done in this visit does not run again while a later one waits.
        if ticket.visit().before_is_done(index, &label) {
            continue;
        }
        let args = match entry_args(ctx, &ticket.key, entry) {
            Ok(args) => args,
            Err(text) => return Ok(Some(flag_action(ticket, super::state::NEEDS_YOU, &text))),
        };
        match entry.kind {
            EntryKind::Job => {
                let run = JobRun {
                    ticket: ticket.key.clone(),
                    step: step.id.clone(),
                    job: entry.name.clone(),
                    args,
                };
                let result = executor::job(ctx, ledger, &run)?;
                if result.failure.is_some() || !result.ok {
                    let why = result.failure.unwrap_or(result.detail);
                    return Ok(Some(flag_action(
                        ticket,
                        super::state::NEEDS_YOU,
                        &format!(
                            "the job {} before the step {} did not pass: {why}",
                            entry.name, step.id
                        ),
                    )));
                }
            }
            EntryKind::Verb => {
                let run = OperationRun {
                    ticket: ticket.key.clone(),
                    verb: entry.name.clone(),
                    args,
                    turn: None,
                };
                let op = executor::request(ctx, ledger, &run)?;
                if op.outcome.needs_you()
                    && !matches!(op.outcome, Outcome::Waiting { .. } | Outcome::Uncertain)
                {
                    return Ok(Some(flag_action(
                        ticket,
                        super::state::NEEDS_YOU,
                        &format!(
                            "{} before the step {}: {}: {}",
                            entry.name, step.id, op.outcome, op.detail
                        ),
                    )));
                }
                if op.outcome != Outcome::Confirmed {
                    return Ok(None);
                }
            }
        }
        if let Some(visit) = ticket.counts.visit.as_mut() {
            visit
                .before_done
                .push(super::state::Visit::before_mark(index, &label));
        }
        save(ledger, ticket, &time::now_rfc3339(), None)?;
        crash_point("before-entry-done");
    }
    write_issue(ctx, ledger, ticket)?;
    Ok(Some(start_action(ticket, step, resume, message)))
}

/// Asks `observe` for the ticket, writes `issue.json` to the ticket folder, and keeps the facts
/// for the status view.
pub(crate) fn write_issue(ctx: &Ctx, ledger: &Ledger, ticket: &mut Ticket) -> Result<()> {
    let facts = executor::observe_ticket(ctx, ledger, &ticket.key)?;
    let issue = json!({
        "key": ticket.key.as_str(),
        "tracker_status": facts.tracker_status,
        "issue": {"untrusted": facts.issue},
    });
    let dir = ctx.paths.ticket_dir(&ticket.key);
    std::fs::create_dir_all(&dir).with_context(|| format!("could not make {}", dir.display()))?;
    atomic_write(
        &ctx.paths.ticket_doc(&ticket.key, "issue.json"),
        format!("{}\n", serde_json::to_string_pretty(&issue)?).as_bytes(),
    )?;
    let now = time::now_rfc3339();
    ticket.counts.seen = Some(Seen::of(&facts, &now));
    save(ledger, ticket, &now, None)
}

/// Serves the read verbs of a turn that ended with requests, writes each answer as a document
/// of the ticket folder, and gives the action that resumes the session with the files.
fn serve(
    ctx: &Ctx,
    ledger: &Ledger,
    adapter: &Adapter,
    ticket: &mut Ticket,
    step: &Step,
    turn_id: &str,
    session: &str,
) -> Result<Option<Action>> {
    let outcome = turn::outcome(ctx, ledger, &TurnId::parse(turn_id)?)?;
    let requests = outcome
        .result
        .as_ref()
        .and_then(|r| r.get("requests"))
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let secrets = Secrets::read(ctx.config()?)?;
    let reads: Vec<&str> = adapter
        .verbs
        .values()
        .filter(|v| v.read)
        .map(|v| v.name.as_str())
        .collect();
    let mut lines = Vec::new();
    let mut files = Vec::new();
    if requests.is_empty() {
        lines.push("Your result had the kind requests, but it named no request.".to_string());
    }
    for request in &requests {
        let verb = request
            .get("verb")
            .and_then(Value::as_str)
            .unwrap_or("")
            .trim();
        let args = request.get("args").cloned().unwrap_or(Value::Null);
        match adapter.verbs.get(verb) {
            Some(spec) if spec.read => match executor::read(ctx, ledger, &ticket.key, verb, &args) {
                Ok(answer) => {
                    let names =
                        executor::write_documents(&ctx.paths, &ticket.key, &answer.documents, &secrets)?;
                    let shown: Vec<String> = names.iter().map(|n| format!(".vision/{n}")).collect();
                    lines.push(format!(
                        "- {verb}: {}{}",
                        if shown.is_empty() {
                            "no file".to_string()
                        } else {
                            shown.join(", ")
                        },
                        if answer.detail.is_empty() {
                            String::new()
                        } else {
                            format!(" ({})", answer.detail)
                        }
                    ));
                    files.extend(names);
                }
                Err(e) => lines.push(format!("- {verb}: it failed, so it has no file: {e:#}")),
            },
            Some(_) => lines.push(format!(
                "- {verb}: refused. It is not a read verb, so a turn cannot ask for it. The read verbs: {}",
                if reads.is_empty() { "none".to_string() } else { reads.join(", ") }
            )),
            None => lines.push(format!(
                "- {}: refused. The adapter has no such verb. The read verbs: {}",
                if verb.is_empty() { "(no verb)" } else { verb },
                if reads.is_empty() { "none".to_string() } else { reads.join(", ") }
            )),
        }
    }
    if let Some(visit) = ticket.counts.visit.as_mut() {
        for name in &files {
            if !visit.extra.contains(name) {
                visit.extra.push(name.clone());
            }
        }
    }
    let now = time::now_rfc3339();
    save(
        ledger,
        ticket,
        &now,
        Some(
            Event::now("tick", "requests-served")
                .ticket(ticket.key.as_str())
                .detail(json!({"turn": turn_id, "files": files, "requests": requests.len()})),
        ),
    )?;
    let message = format!(
        "The executor answered your requests:\n{}\nContinue the same step with these files.",
        lines.join("\n")
    );
    Ok(Some(start_action(
        ticket,
        step,
        Some(session.to_string()),
        Some(message),
    )))
}

/// Moves a ticket to the step `to`: the span of the old step closes, the span of the new one
/// opens, and the new visit starts, in one transaction with the event `step`.
pub(crate) fn move_to(
    ctx: &Ctx,
    ledger: &Ledger,
    adapter: &Adapter,
    ticket: &mut Ticket,
    from: &Step,
    to: &str,
    how: &str,
) -> Result<()> {
    let Some(step) = adapter.step(to).cloned() else {
        bail!("the step {to} is not in the adapter");
    };
    let now = time::now_rfc3339();
    let turns = u32::try_from(ledger.step_turn_ids(ticket.key.as_str(), &step.id)?.len())?;
    let base = ticket.clone_dir().ok().and_then(|c| head_of(&c).ok());
    let visit = ticket.counts.enter(&step, how, &now, turns, base, true);
    ticket.row.state = Some(step.id.clone());
    in_tx(ledger, |ledger| {
        ledger.close_spans(ticket.key.as_str(), &now)?;
        ledger.open_span(&span(ticket, &step, how, &now))?;
        ledger.set_ticket_state(
            ticket.key.as_str(),
            &step.id,
            &flags_text(&ticket.flags),
            &ticket.counts.text(),
            &now,
        )?;
        ledger.add_event(
            &Event::now("tick", "step")
                .ticket(ticket.key.as_str())
                .detail(json!({"from": from.id, "to": step.id, "how": how, "n": visit.n, "seq": visit.seq})),
        )?;
        Ok(())
    })?;
    ctx.log(&format!(
        "pipeline: {} moved from {} to {} ({how}, entry {})",
        ticket.key, from.id, step.id, visit.n
    ));
    Ok(())
}

/// The end step: records a merged pull request for the Curator, frees the clone, and closes
/// the span, in one transaction with the event `ended`.
fn finish(ctx: &Ctx, ledger: &Ledger, ticket: &mut Ticket, now: OffsetDateTime) -> Result<()> {
    let facts = executor::observe_ticket(ctx, ledger, &ticket.key)?;
    let at = time::rfc3339(now);
    let merged = facts.pr.as_ref().filter(|pr| pr.merged).cloned();
    if let Some(pr) = &merged {
        crate::curator::record_merged(ledger, ticket.key.as_str(), pr.number, pr.head.as_deref())?;
    }
    ticket.counts.seen = Some(Seen::of(&facts, &at));
    ticket.counts.finished = Some(at.clone());
    in_tx(ledger, |ledger| {
        ledger.close_spans(ticket.key.as_str(), &at)?;
        ledger.set_ticket_clone(ticket.key.as_str(), None, &at)?;
        ledger.set_ticket_state(
            ticket.key.as_str(),
            ticket.state(),
            &flags_text(&ticket.flags),
            &ticket.counts.text(),
            &at,
        )?;
        ledger.add_event(
            &Event::now("tick", "ended")
                .ticket(ticket.key.as_str())
                .detail(
                    json!({"merged": merged.is_some(), "pr": merged.as_ref().map(|p| p.number)}),
                ),
        )?;
        Ok(())
    })?;
    ctx.log(&format!(
        "pipeline: {} reached its end{}, and its clone is free",
        ticket.key,
        merged
            .map(|pr| format!(" with the merged pull request #{}", pr.number))
            .unwrap_or_default()
    ));
    Ok(())
}

// ----- Tick step 7 -----------------------------------------------------------------------

/// Tick step 7 for [`Action::SetFlag`]: sets the flag with its reason, in one transaction with
/// the event `flag`.
pub fn set_flag(pass: &mut Pass, set: &FlagSet) -> Result<()> {
    let ctx = pass.ctx;
    let Some(row) = pass.ledger.ticket(set.ticket.as_str())? else {
        bail!("the ticket {} has no row", set.ticket);
    };
    let mut ticket = Ticket::of(row)?;
    let now = time::now_rfc3339();
    if !ticket.flags.contains(&set.flag) {
        ticket.flags.push(set.flag.clone());
    }
    ticket.counts.reasons.insert(
        set.flag.clone(),
        FlagReason {
            reason: set.reason.clone(),
            at: now.clone(),
        },
    );
    save(
        &pass.ledger,
        &ticket,
        &now,
        Some(
            Event::now("tick", "flag")
                .ticket(ticket.key.as_str())
                .detail(json!({"flag": set.flag, "reason": set.reason, "step": ticket.state()})),
        ),
    )?;
    ctx.log(&format!(
        "pipeline: {} got the flag {}: {}",
        ticket.key, set.flag, set.reason
    ));
    Ok(())
}
