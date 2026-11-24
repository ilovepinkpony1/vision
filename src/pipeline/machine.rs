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
    check_end_facts, ci_result, decide, findings_branch, gate_blocks, wait_target, Branch, Check,
    FactInputs, Facts, JobFact, KindFacts, LastTurn, List, Next, OpFact, Refused,
};
use super::docs::{self, result_documents};
use super::gates;
use super::refusals;
use super::state::{
    flags_of, flags_text, waits_on, Counts, FlagReason, Interrupt, Seen, NEEDS_YOU, STOPPED,
    WAITS_FOR_LIMIT,
};
use crate::adapter::when::Holds;
use crate::adapter::{self, Adapter, Entry, EntryKind, Expect, GateRule, Step, StepKind};
use crate::ctx::Ctx;
use crate::executor::runs::{self, RunState};
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

    /// True when the row names a clone: the ticket holds a slot, unless it is stopped.
    pub fn has_clone(&self) -> bool {
        self.row
            .clone
            .as_deref()
            .is_some_and(|c| !c.trim().is_empty())
    }

    /// The last commit of the ticket: `HEAD` of its clone, else the head of its row, as for a
    /// ticket in the background (section 11.3).
    pub fn head(&self) -> Option<String> {
        match self.clone_dir() {
            Ok(clone) => head_of(&clone).ok(),
            Err(_) => self.row.head.clone().filter(|h| !h.trim().is_empty()),
        }
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
pub(crate) fn save(
    ledger: &Ledger,
    ticket: &Ticket,
    now: &str,
    event: Option<Event>,
) -> Result<()> {
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
pub(crate) fn ensure_visit(
    ctx: &Ctx,
    ledger: &Ledger,
    ticket: &mut Ticket,
    step: &Step,
) -> Result<()> {
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
    let base = ticket.head();
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

/// The turns of one agent in the visit: the turns of the visit (only those of `agent` for a
/// member of a team), after the turns that a fallback from Codex left behind.
pub(crate) fn agent_turns(
    ledger: &Ledger,
    ticket: &Ticket,
    step: &str,
    agent: &str,
    member: bool,
) -> Result<Vec<TurnRow>> {
    let visit = ticket.visit();
    let skip = visit.fallback.get(agent).copied().unwrap_or(0) as usize;
    Ok(visit_turns(ledger, &ticket.key, step, visit.turns)?
        .into_iter()
        .filter(|t| !member || t.agent.as_deref() == Some(agent))
        .skip(skip)
        .collect())
}

/// The step of a team member as a step of its own, or the step itself.
pub(crate) fn turn_agent(step: &Step) -> Option<(String, crate::turn::TurnKind)> {
    match &step.kind {
        StepKind::Turn(turn) if turn.team.is_empty() => Some((turn.agent.clone(), turn.turn)),
        _ => None,
    }
}

/// The message of a refused result of a team member that ended with questions.
pub const MEMBER_QUESTIONS: &str = "a member of a team cannot end its turn with questions. Write each open question into your findings under `## Open questions`: the analyst reads the findings and asks them";

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
        let Some(step) = adapter.step(ticket.state()).cloned() else {
            continue;
        };
        match merged_outside(ctx, &pass.ledger, &adapter, &mut ticket, &step, pass.now) {
            Ok(true) => continue,
            Ok(false) => {}
            Err(e) => ctx.log(&format!(
                "pipeline: could not look for a merge of the pull request of {}: {e:#}",
                ticket.key
            )),
        }
        // The members of a team that still run when a rule stopped the ticket keep their
        // results: their end facts are tested also under the flag needs-you.
        let keeps = !step.team().is_empty()
            && ticket
                .flags
                .iter()
                .all(|f| f == super::state::NEEDS_YOU || f == WAITS_FOR_LIMIT);
        if !ticket.flags.is_empty() && !keeps {
            continue;
        }
        // A ticket in the background is watched at a wait step. At each other step it needs a
        // slot first, and the slots of the tick give it one (section 11.3).
        if !ticket.has_clone() && !matches!(step.kind, StepKind::Wait(_)) {
            continue;
        }
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

/// A ticket whose pull request was merged outside Vision ends, at whatever step it waits. A
/// wait step watches the pull request itself. At each other step with no running turn and no
/// job in the background, the ticket looks at its pull request at most once in
/// `caps.merge_look_minutes`,
/// also under the flag needs-you or stopped: a merged pull request leaves nothing to decide. A
/// paused ticket waits for the person, who works in its session. A merged pull request drops
/// each write of the ticket that waits, so that no approval sends it later, clears the flags,
/// and moves the ticket to its end step, which step 6 finishes. Gives true when the ticket
/// moved.
fn merged_outside(
    ctx: &Ctx,
    ledger: &Ledger,
    adapter: &Adapter,
    ticket: &mut Ticket,
    step: &Step,
    now: OffsetDateTime,
) -> Result<bool> {
    if ticket.row.pr.is_none()
        || matches!(step.kind, StepKind::Wait(_) | StepKind::End)
        || ticket.flags.iter().any(|f| f == turn::PAUSED)
        || ledger.running_turns_of_ticket(ticket.key.as_str())? > 0
        || !runs::of_ticket(&ctx.paths, ticket.key.as_str()).is_empty()
    {
        return Ok(false);
    }
    let every = i64::from(ctx.config()?.caps.merge_look_minutes) * 60;
    let due = ticket
        .counts
        .merge_look
        .as_deref()
        .and_then(|t| time::parse_rfc3339(t).ok())
        .is_none_or(|at| time::seconds_between(at, now) >= every);
    if !due {
        return Ok(false);
    }
    let at = time::rfc3339(now);
    ticket.counts.merge_look = Some(at.clone());
    ledger.set_ticket_state(
        ticket.key.as_str(),
        ticket.state(),
        &flags_text(&ticket.flags),
        &ticket.counts.text(),
        &at,
    )?;
    let facts = executor::observe_ticket(ctx, ledger, &ticket.key)?;
    let Some(pr) = facts.pr.as_ref().filter(|pr| pr.merged) else {
        return Ok(false);
    };
    let number = pr.number;
    let mut dropped = Vec::new();
    for op in ledger.operations_of(ticket.key.as_str(), None)? {
        if op.state != crate::ledger::operations::STATE_PENDING {
            continue;
        }
        let detail = format!(
            "dropped: the pull request #{number} was merged outside Vision, so this write did not run"
        );
        ledger.end_operation(
            &op.id,
            crate::ledger::operations::STATE_FAILED,
            None,
            Some(&at),
            &json!({"detail": detail, "dropped": "merged"}).to_string(),
        )?;
        dropped.push(format!("{} {}", op.verb, op.id));
    }
    let cleared = [NEEDS_YOU, WAITS_FOR_LIMIT, STOPPED];
    ticket.flags.retain(|f| !cleared.contains(&f.as_str()));
    for flag in cleared {
        ticket.counts.reasons.remove(flag);
    }
    ticket.counts.seen = Some(Seen::of(&facts, &at));
    let end = adapter.end().id.clone();
    move_to_with(
        ctx,
        ledger,
        adapter,
        ticket,
        step,
        &end,
        "merged outside",
        Some(&json!({"pr": number, "dropped": dropped})),
    )?;
    ctx.log(&format!(
        "pipeline: the pull request #{number} of {} was merged outside Vision at the step {}, so the ticket goes to its end{}",
        ticket.key,
        step.id,
        if dropped.is_empty() {
            String::new()
        } else {
            format!(" and drops the waiting writes {}", dropped.join(", "))
        }
    ));
    Ok(true)
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
    if let Some((agent, _)) = turn_agent(step) {
        return test_turn_of(ctx, ledger, ticket, step, &agent, false);
    }
    for member in step.team() {
        let view = step.member_view(member);
        test_turn_of(ctx, ledger, ticket, &view, &member.agent, true)?;
    }
    Ok(())
}

/// The end facts of the last turn of one agent: the agent of the step, or one member of a team
/// with `member` true. A member that ended with questions gets the failed end fact
/// `questions`, so that it resumes with the rule to write them into its findings.
fn test_turn_of(
    ctx: &Ctx,
    ledger: &Ledger,
    ticket: &mut Ticket,
    step: &Step,
    agent: &str,
    member: bool,
) -> Result<()> {
    let visit = ticket.visit();
    let turns = agent_turns(ledger, ticket, &step.id, agent, member)?;
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
    let kind = result.get("kind").and_then(Value::as_str);
    if member && kind == Some("questions") {
        let detail = json!({
            "turn": last.id, "fact": "questions", "found": MEMBER_QUESTIONS, "blocks": false,
            "documents": [],
        });
        in_tx(ledger, |ledger| {
            ledger.set_end_fact(&last.id, "failed:questions")?;
            ledger.add_event(
                &Event::now("tick", "end-fact")
                    .ticket(ticket.key.as_str())
                    .external_id(&fact_mark(&last.id))
                    .detail(detail.clone()),
            )?;
            Ok(())
        })?;
        ctx.log(&format!(
            "pipeline: the member {agent} of {} ended {} with questions, so the result is refused",
            ticket.key, last.id
        ));
        return Ok(());
    }
    if kind != Some("result") {
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
    let mut job_docs = Vec::new();
    for expect in &step.expects {
        if let Expect::Job(job) = expect {
            let args = end_fact_args(ctx, &ticket.key, step, job);
            // A job runs in the background (section 11.1). The run that this test asked for
            // gives its outcome once. Before it, a pass at this commit with these arguments
            // counts. A job that did not pass at this commit runs again for a new result,
            // because a job can fail for a reason outside the commit, such as a slow machine.
            let fact = match runs::state(&ctx.paths, ledger, &end_fact_mark(&last.id, job, &args))?
            {
                RunState::Ended(outcome) if outcome.ok => JobFact::Passed,
                RunState::Ended(outcome) => {
                    job_docs.extend(outcome.documents.clone());
                    JobFact::Failed(outcome.why())
                }
                RunState::InFlight(_) => JobFact::Running,
                RunState::None => {
                    match executor::job_passed_with(ledger, ticket.key.as_str(), job, &head, &args)?
                    {
                        Some(true) => JobFact::Passed,
                        None | Some(false) => JobFact::NotRun,
                    }
                }
            };
            inputs.jobs.insert(job.clone(), fact);
            job_args.insert(job.clone(), args);
        }
    }
    let check = match check_end_facts(&step.expects, &inputs) {
        Check::RunJob(job) => {
            let args = job_args
                .get(&job)
                .cloned()
                .unwrap_or_else(|| Value::Object(Map::new()));
            let run = JobRun {
                ticket: ticket.key.clone(),
                step: step.id.clone(),
                job: job.clone(),
                args: args.clone(),
            };
            // The next passes test the end facts again; this one does not wait for the job.
            runs::request(
                ctx,
                ledger,
                &run,
                Some(&end_fact_mark(&last.id, &job, &args)),
            )?;
            return Ok(());
        }
        Check::Waits(_) => return Ok(()),
        other => other,
    };
    // A requested change of a review that this round refuses once too often waits for the
    // decision of the person (section 11.3).
    let check = match check {
        Check::Held => match refusal_stops(ctx, ledger, &ticket.key, step, &found)? {
            Some(found) => Check::Failed {
                fact: REFUSALS_FACT.into(),
                found,
            },
            None => Check::Held,
        },
        other => other,
    };
    let now = time::now_rfc3339();
    let (value, detail) = match &check {
        Check::Held => ("held".to_string(), json!({"turn": last.id, "held": true})),
        // `blocks`: a failed gate found a blocking item in its document, which was written to
        // the ticket folder above. A `fail` route of the step goes on it.
        Check::Failed { fact, found: what } => {
            // The resume names the documents of a failed job, such as precheck.md, so that the
            // turn reads them in its view.
            let what = if fact.starts_with("job:") && !job_docs.is_empty() {
                let names: Vec<String> = job_docs.iter().map(|d| format!(".vision/{d}")).collect();
                format!("{what}. Read its documents: {}", names.join(", "))
            } else {
                what.clone()
            };
            (
                format!("failed:{fact}"),
                json!({
                    "turn": last.id, "fact": fact, "found": what, "documents": job_docs,
                    "blocks": gate_blocks(step, fact, &found.docs),
                }),
            )
        }
        Check::RunJob(_) | Check::Waits(_) => return Ok(()),
    };
    if let Check::Failed { .. } = &check {
        if let Some(visit) = ticket.counts.visit.as_mut() {
            let extra = if member {
                visit.member_extra.entry(agent.to_string()).or_default()
            } else {
                &mut visit.extra
            };
            for doc in &job_docs {
                if !extra.contains(doc) {
                    extra.push(doc.clone());
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
    if check == Check::Held {
        remember_seen_comments(ctx, &ticket.key, step, &found)?;
        count_refusals(ctx, ledger, &ticket.key, step, &found, &last.id, &now)?;
    }
    // The commands of the secrets can be slow, so only a step with a gate reads them.
    let secrets = if step.expects.iter().any(|e| matches!(e, Expect::Gate(_))) {
        Secrets::read(ctx.config()?)?
    } else {
        Secrets::from_values(Vec::new())
    };
    in_tx(ledger, |ledger| {
        // The rounds of the gates and the findings that the rounds follow, with the end fact,
        // so that a second pass over this turn records nothing new.
        let back = super::rounds::record(
            ledger,
            ticket.key.as_str(),
            step,
            &last.id,
            &head,
            &found.docs,
            &secrets,
            &now,
        )?;
        for notice in &back {
            if let Some(id) = ledger.add_notice(&NewNotice {
                ticket: ticket.key.as_str(),
                step: &step.id,
                turn: &last.id,
                gate: super::rounds::CR_SOURCE,
                doc: &notice.doc,
                issues: &notice.issues,
                note: Some(&notice.note),
                created: &now,
            })? {
                ledger.add_event(
                    &Event::now("tick", "notice")
                        .ticket(ticket.key.as_str())
                        .external_id(&format!("notice:{}:{}", last.id, notice.doc))
                        .detail(json!({
                            "notice": notice_name(id), "turn": last.id, "step": step.id,
                            "doc": notice.doc, "issues": notice.issues.len(), "why": notice.note,
                        })),
                )?;
            }
        }
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
                        turn: Some(last.id.clone()),
                        ..FindingRow::default()
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

/// Keeps the marks of the comments that a turn read, when its result gives verdicts on review
/// comments and its end facts held: each mark of a list that the step reads, such as
/// `comments.json`, joins the marks of the earlier rounds. `observe` gets them back in `seen`,
/// so a comment that the round left without a reply, such as a note of a bot, does not start
/// another round. The write comes before the end fact, and a second write gives the same file.
fn remember_seen_comments(
    ctx: &Ctx,
    key: &TicketKey,
    step: &Step,
    found: &docs::ResultDocs,
) -> Result<()> {
    let StepKind::Turn(turn) = &step.kind else {
        return Ok(());
    };
    if !found.docs.contains_key("verdicts.json") {
        return Ok(());
    }
    let mut marks: BTreeSet<String> = executor::seen_comments(ctx, key)?.into_iter().collect();
    let before = marks.len();
    for name in &turn.reads {
        if let Some(text) = read_optional(&ctx.paths.ticket_doc(key, name))? {
            marks.extend(docs::comment_marks(&text));
        }
    }
    if marks.len() == before {
        return Ok(());
    }
    let list: Vec<&String> = marks.iter().collect();
    let text = format!("{}\n", serde_json::to_string_pretty(&list)?);
    atomic_write(
        &ctx.paths.ticket_doc(key, executor::SEEN_COMMENTS),
        text.as_bytes(),
    )
}

/// The end fact of a round of review comments that refuses a request once too often.
const REFUSALS_FACT: &str = "refusals";

/// The counts so far, the verdicts of a round, and the comments that it read.
type RefusalInputs = (
    refusals::Refusals,
    Vec<docs::Verdict>,
    Vec<refusals::Comment>,
);

/// What a round needs to count refused requests of a review. `None` for a step that is not a
/// turn, or for a result with no `verdicts.json` that Vision can read.
fn refusal_inputs(
    ctx: &Ctx,
    key: &TicketKey,
    step: &Step,
    found: &docs::ResultDocs,
) -> Result<Option<RefusalInputs>> {
    let StepKind::Turn(turn) = &step.kind else {
        return Ok(None);
    };
    let Some(Ok(verdicts)) = found.docs.get("verdicts.json").map(|t| docs::verdicts(t)) else {
        return Ok(None);
    };
    let counts =
        refusals::parse(read_optional(&ctx.paths.ticket_doc(key, refusals::REFUSALS))?.as_deref());
    let mut comments = Vec::new();
    for name in turn.reads.iter().filter(|n| *n != refusals::REFUSALS) {
        if let Some(text) = read_optional(&ctx.paths.ticket_doc(key, name))? {
            comments.extend(refusals::comments(&text));
        }
    }
    Ok(Some((counts, verdicts, comments)))
}

/// The refused requests of a round that wait for the decision of the person, as one text for
/// the resume of the agent. `None` when the round can hold, or when `caps.refusals` is 0.
fn refusal_stops(
    ctx: &Ctx,
    ledger: &Ledger,
    key: &TicketKey,
    step: &Step,
    found: &docs::ResultDocs,
) -> Result<Option<String>> {
    let limit = ctx.config()?.caps.refusals;
    if limit == 0 {
        return Ok(None);
    }
    let Some((counts, verdicts, comments)) = refusal_inputs(ctx, key, step, found)? else {
        return Ok(None);
    };
    let decisions = refusals::decisions(&ledger.questions_of_ticket(key.as_str())?);
    let stops = refusals::check(&counts.requests, &verdicts, &comments, &decisions, limit);
    Ok((!stops.is_empty()).then(|| stops.join(" ")))
}

/// Counts the refused requests of a round whose end facts held, in `refusals.json`. The write
/// comes before the end fact, and a second write for the same turn changes nothing. With
/// `caps.refusals` 0, it counts nothing.
fn count_refusals(
    ctx: &Ctx,
    ledger: &Ledger,
    key: &TicketKey,
    step: &Step,
    found: &docs::ResultDocs,
    turn: &str,
    now: &str,
) -> Result<()> {
    if ctx.config()?.caps.refusals == 0 {
        return Ok(());
    }
    let Some((counts, verdicts, comments)) = refusal_inputs(ctx, key, step, found)? else {
        return Ok(());
    };
    let decisions = refusals::decisions(&ledger.questions_of_ticket(key.as_str())?);
    let next = refusals::count_round(&counts, &verdicts, &comments, &decisions, turn, now);
    if next == counts {
        return Ok(());
    }
    atomic_write(
        &ctx.paths.ticket_doc(key, refusals::REFUSALS),
        refusals::render(&next).as_bytes(),
    )
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
    // Before the time of `from`, the step does nothing: no start verb and no look.
    if opens_later(&ticket.visit(), now).is_some() {
        return Ok(());
    }
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
    // The interrupts of the step, from the same answer: one that fires moves the ticket before
    // the watched value counts.
    let interrupt = Interrupt::first(wait, &facts);
    if let Some(visit) = ticket.counts.visit.as_mut() {
        visit.observed = Some(at.clone());
        visit.watched = watched.clone();
        visit.interrupt = interrupt;
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

/// The moment of `from` of a wait step while it is still to come, else `None`.
pub(crate) fn opens_later(
    visit: &super::state::Visit,
    now: OffsetDateTime,
) -> Option<OffsetDateTime> {
    visit
        .opens
        .as_deref()
        .and_then(|t| time::parse_rfc3339(t).ok())
        .filter(|opens| *opens > now)
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
            Ok(list) => actions.extend(list),
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

/// The most steps that one tick skips for one ticket, so that a loop of skipped steps in a
/// broken adapter never spins.
const MAX_SKIPS: usize = 32;

fn select_one(
    ctx: &Ctx,
    ledger: &Ledger,
    adapter: &Adapter,
    ticket: &mut Ticket,
    now: OffsetDateTime,
) -> Result<Vec<Action>> {
    let Some(mut step) = adapter.step(ticket.state()).cloned() else {
        if ticket.flags.is_empty() {
            return Ok(vec![flag_action(
                ticket,
                super::state::NEEDS_YOU,
                &format!(
                    "the step {} is not in the adapter {}. Fix adapter.toml, or run `vision stop {}`",
                    ticket.state(),
                    adapter.name,
                    ticket.key
                ),
            )]);
        }
        return Ok(Vec::new());
    };
    // A flag other than the limit stops each action, and `decide` gives nothing for it, so its
    // facts are not gathered: a stopped ticket may have lost its clone.
    if ticket.flags.iter().any(|f| f != WAITS_FOR_LIMIT) {
        return Ok(Vec::new());
    }
    // A ticket with no clone acts only where it needs no work tree. At each other step it waits
    // until the slots of the tick give it a clone (section 11.3).
    if !ticket.has_clone() && !super::slots::needs_no_slot(ctx, ledger, ticket, &step)? {
        return Ok(Vec::new());
    }
    ensure_visit(ctx, ledger, ticket, &step)?;
    // A step whose `when` does not hold is skipped: the ticket moves through its next in the
    // same tick.
    let mut skipped = 0;
    loop {
        match super::kind::step_holds(ctx, ledger, adapter, ticket, &step)? {
            Holds::Yes => break,
            Holds::NoDoc(why) => {
                return Ok(vec![flag_action(
                    ticket,
                    super::state::NEEDS_YOU,
                    &format!("the step {}: {why}", step.id),
                )])
            }
            Holds::No(why) => {
                if skipped >= MAX_SKIPS {
                    return Ok(Vec::new());
                }
                skipped += 1;
                let to = super::kind::skip_target(&step)
                    .with_context(|| format!("the step {} has no next step to skip to", step.id))?;
                super::kind::skip(ctx, ledger, adapter, ticket, &step, &to, &why)?;
                step = adapter
                    .step(&to)
                    .cloned()
                    .with_context(|| format!("the step {to} is not in the adapter"))?;
            }
        }
    }
    match &step.kind {
        StepKind::Turn(turn) if !turn.team.is_empty() => {
            return super::team::select(ctx, ledger, adapter, ticket, &step, now)
        }
        StepKind::Ask(_) => return super::ask::select_ask(ctx, ledger, adapter, ticket, &step),
        StepKind::Check(_) => return super::ask::select_check(ctx, ledger, adapter, ticket, &step),
        _ => {}
    }
    let one = |action: Option<Action>| Ok(action.into_iter().collect());
    for _ in 0..2 {
        let facts = gather(ctx, ledger, adapter, ticket, &step, now, false)?;
        let next = decide(&facts);
        match next {
            Next::Nothing(_) => return Ok(Vec::new()),
            Next::ClearLimit => {
                clear_limit(ctx, ledger, ticket)?;
                continue;
            }
            Next::Flag { flag, reason } => return one(Some(flag_action(ticket, flag, &reason))),
            Next::Start { resume, message } => {
                return enter_turn(ctx, ledger, ticket, &step, resume, message).and_then(one)
            }
            Next::Resume {
                session,
                message,
                extra,
            } => {
                if let Some(why) = extra {
                    extra_round_event(ctx, ledger, ticket, &step, &why)?;
                }
                return one(Some(start_action(
                    ticket,
                    &step,
                    Some(session),
                    Some(message),
                )));
            }
            Next::Serve { turn, session } => {
                return serve(ctx, ledger, adapter, ticket, &step, &turn, &session, false)
                    .and_then(one)
            }
            Next::Fallback { why } => {
                return fall_back(ctx, ledger, adapter, ticket, &step, &why, false).and_then(one)
            }
            Next::Request { list, index } => {
                let entries: &[Entry] = match (list, &step.kind) {
                    (List::After, _) => &step.after,
                    (List::Start, _) => &step.start,
                    (List::Run, StepKind::Act(act)) => &act.run,
                    (List::Run, _) => &[],
                };
                let Some(entry) = entries.get(index) else {
                    return Ok(Vec::new());
                };
                // An operation of `after` names the turn whose end facts held. A `start` verb
                // and a verb of an act step follow a rule of the step, and no turn.
                let turn = match list {
                    List::After => last_turn_id(ledger, ticket, &step)?,
                    List::Start | List::Run => None,
                };
                return request_action(ctx, ticket, entry, turn).map(|a| vec![a]);
            }
            Next::RunJob => {
                let StepKind::Job(job) = &step.kind else {
                    return Ok(Vec::new());
                };
                return one(Some(Action::RunJob(JobRun {
                    ticket: ticket.key.clone(),
                    step: step.id.clone(),
                    job: job.job.clone(),
                    args: Value::Object(job.args.clone()),
                })));
            }
            Next::Move { to, how } => {
                // A move by `on` adds its detail to the event, such as the two heads of a
                // move by commits.
                let detail = facts
                    .branch
                    .as_ref()
                    .filter(|b| b.to == to && b.how == how)
                    .and_then(|b| b.detail.clone());
                move_to_with(
                    ctx,
                    ledger,
                    adapter,
                    ticket,
                    &step,
                    &to,
                    &how,
                    detail.as_ref(),
                )?;
                return Ok(Vec::new());
            }
            Next::Finish => {
                finish(ctx, ledger, ticket, now)?;
                return Ok(Vec::new());
            }
        }
    }
    Ok(Vec::new())
}

/// The event of an extra round of `extra_rounds`, with why it was granted. One event for each
/// failed turn, so a start that waits for a later tick records it once.
fn extra_round_event(
    ctx: &Ctx,
    ledger: &Ledger,
    ticket: &Ticket,
    step: &Step,
    why: &str,
) -> Result<()> {
    let Some(last) = last_turn_id(ledger, ticket, step)? else {
        return Ok(());
    };
    let added = ledger.add_event(
        &Event::now("tick", "extra-round")
            .ticket(ticket.key.as_str())
            .external_id(&format!("extra-round:{last}"))
            .detail(json!({"step": step.id, "turn": last, "why": why})),
    )?;
    if added.is_some() {
        ctx.log(&format!("pipeline: {} gets an {why}", ticket.key));
    }
    Ok(())
}

/// The fallback of an agent on Codex: the visit leaves its turns on Codex behind, records the
/// event `fallback`, and starts a fresh turn of the same agent on its fallback model with
/// Claude.
pub(crate) fn fall_back(
    ctx: &Ctx,
    ledger: &Ledger,
    adapter: &Adapter,
    ticket: &mut Ticket,
    step: &Step,
    why: &str,
    member: bool,
) -> Result<Option<Action>> {
    let Some((agent, _)) = turn_agent(step) else {
        return Ok(None);
    };
    let before = u32::try_from(
        visit_turns(ledger, &ticket.key, &step.id, ticket.visit().turns)?
            .iter()
            .filter(|t| !member || t.agent.as_deref() == Some(agent.as_str()))
            .count(),
    )?;
    if let Some(visit) = ticket.counts.visit.as_mut() {
        visit.fallback.insert(agent.clone(), before);
    }
    let fallback = crate::models::choice_in(ctx.config()?, &adapter.agents, &agent)?
        .fallback
        .map(|f| format!("{} {}", f.model, f.effort))
        .unwrap_or_default();
    save(
        ledger,
        ticket,
        &time::now_rfc3339(),
        Some(
            Event::now("tick", "fallback")
                .ticket(ticket.key.as_str())
                .detail(json!({"agent": agent, "step": step.id, "why": why, "to": fallback})),
        ),
    )?;
    ctx.log(&format!(
        "pipeline: fallback {agent} of {} to {fallback}: {why}",
        ticket.key
    ));
    let message = ticket.visit().message.clone();
    Ok(Some(start_action(ticket, step, None, message)))
}

pub(crate) fn flag_action(ticket: &Ticket, flag: &str, reason: &str) -> Action {
    Action::SetFlag(FlagSet {
        ticket: ticket.key.clone(),
        flag: flag.to_string(),
        reason: reason.to_string(),
    })
}

pub(crate) fn start_action(
    ticket: &Ticket,
    step: &Step,
    resumes: Option<String>,
    message: Option<String>,
) -> Action {
    let (agent, kind) = match &step.kind {
        StepKind::Turn(turn) => (turn.agent.clone(), turn.turn),
        _ => (String::new(), turn::TurnKind::Reader),
    };
    // An agent that fell back from Codex in this visit keeps its fallback for each later turn
    // of the visit.
    let fallback = ticket.visit().fallback.contains_key(&agent);
    Action::StartTurn(TurnStart {
        ticket: ticket.key.clone(),
        step: step.id.clone(),
        agent,
        kind,
        resumes,
        message,
        fallback,
    })
}

pub(crate) fn last_turn_id(
    ledger: &Ledger,
    ticket: &Ticket,
    step: &Step,
) -> Result<Option<String>> {
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
/// The mark of the run of a job that the end facts of a turn ask for: one outcome for each turn,
/// job, and arguments.
fn end_fact_mark(turn: &str, job: &str, args: &Value) -> String {
    format!("job-run:{turn}:{}", executor::job_command(job, args))
}

/// A run in the background in words, such as `the job proof runs since 10:37`.
fn run_words(run: &runs::RunRecord) -> String {
    let at = |text: &Option<String>| {
        text.as_deref()
            .and_then(|t| time::parse_rfc3339(t).ok())
            .map(time::local_hm)
            .unwrap_or_default()
    };
    match run.phase {
        runs::RunPhase::Queued => format!(
            "the job {} waits for a free place to run (since {})",
            run.job,
            at(&Some(run.queued.clone()))
        ),
        _ => format!(
            "the job {} runs since {}",
            run.job,
            at(&run.launched.clone().or_else(|| Some(run.queued.clone())))
        ),
    }
}

pub(crate) fn entry_args(ctx: &Ctx, key: &TicketKey, entry: &Entry) -> Result<Value, String> {
    let read = |name: &str| {
        read_optional(&ctx.paths.ticket_doc(key, name))
            .ok()
            .flatten()
    };
    docs::expand_args(&entry.args, &read).map_err(|e| format!("{}: {e}", entry.label()))
}

pub(crate) fn request_action(
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
                let head = ticket.head();
                if let Some(run) = runs::in_flight_of(&ctx.paths, ticket.key.as_str(), &entry.name)
                {
                    out.push((
                        entry.label(),
                        OpFact::Known {
                            outcome: Outcome::Executing,
                            detail: run_words(&run),
                        },
                    ));
                    continue;
                }
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

/// The first verb of `entries` whose operation at the commit of now failed on a merge
/// conflict: its label and the detail of its failure. `None` when no verb failed so.
fn conflict_of(
    ctx: &Ctx,
    ledger: &Ledger,
    ticket: &Ticket,
    entries: &[Entry],
) -> Result<Option<(String, String)>> {
    for entry in entries.iter().filter(|e| e.kind == EntryKind::Verb) {
        let Ok(args) = entry_args(ctx, &ticket.key, entry) else {
            continue;
        };
        let run = OperationRun {
            ticket: ticket.key.clone(),
            verb: entry.name.clone(),
            args,
            turn: None,
        };
        if let Some(op) = executor::lookup(ctx, ledger, &run)? {
            if op.state == executor::OpState::Failed && op.conflict {
                return Ok(Some((entry.label(), op.detail)));
            }
        }
    }
    Ok(None)
}

/// The value of a cap of the configuration by its name, or a number of entries of the step.
pub(crate) fn cap_value(ctx: &Ctx, name: &str) -> Result<Option<u32>> {
    if let Ok(n) = name.parse::<u32>() {
        return Ok(Some(n));
    }
    let caps = serde_json::to_value(&ctx.config()?.caps)?;
    Ok(caps
        .get(name)
        .and_then(Value::as_u64)
        .and_then(|v| u32::try_from(v).ok()))
}

/// What is open at a step, for the message of its cap: the open items of its documents, and the
/// newest CI run when it is red. Each step that reads the CI writes its checks under its own
/// source, so a red run of one step and a later green run of another step both stay.
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
        .rev()
        .find(|c| c.kind == "ci")
    {
        if ci.result == "red" {
            open.push(format!("the CI is red at {}", executor::short(&ci.head)));
        }
    }
    Ok(open)
}

/// The facts of the step of one ticket, for [`decide`]. With `member`, `step` is the view of one
/// member of a team: only the turns of its agent count, and the turns of the other members
/// that run do not stop it.
pub(crate) fn gather(
    ctx: &Ctx,
    ledger: &Ledger,
    adapter: &Adapter,
    ticket: &Ticket,
    step: &Step,
    now: OffsetDateTime,
    member: bool,
) -> Result<Facts> {
    let visit = ticket.visit();
    let key = &ticket.key;
    let mut refusal = None;
    let mut branch = None;
    let mut codex_fallback = None;
    let kind = match &step.kind {
        StepKind::Turn(turn) => {
            refusal = turn::refusal(ctx, ledger, key, turn.turn)?.map(|r| {
                if r.is_limit() {
                    Refused::Limit(r.to_string())
                } else {
                    Refused::Other(r.to_string())
                }
            });
            let turns = agent_turns(ledger, ticket, &step.id, &turn.agent, member)?;
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
            let mut last = match turns.last() {
                None => None,
                Some(row) => Some(last_turn(ctx, ledger, row)?),
            };
            // A member that ended with questions got the failed end fact `questions`: its
            // turn goes the way of a result whose end fact failed.
            if let Some(t) = last.as_mut() {
                if member
                    && t.kind.as_deref() == Some("questions")
                    && t.end_fact.as_deref() == Some("failed:questions")
                {
                    t.kind = Some("result".into());
                }
            }
            if let Some(row) = turns.last() {
                if turn::ran_on_codex(ctx, ledger, &row.id)
                    && !visit.fallback.contains_key(&turn.agent)
                {
                    codex_fallback = Some(
                        crate::models::choice_in(ctx.config()?, &adapter.agents, &turn.agent)
                            .map(|c| c.fallback.is_some())
                            .unwrap_or(false),
                    );
                }
            }
            let failed = match &last {
                Some(t)
                    if t.end_fact
                        .as_deref()
                        .is_some_and(|f| f.starts_with("failed:")) =>
                {
                    ledger
                        .event_detail(&fact_mark(&t.id))?
                        .flatten()
                        .and_then(|d| serde_json::from_str::<Value>(&d).ok())
                }
                _ => None,
            };
            let failure = failed
                .as_ref()
                .and_then(|d| d.get("found").and_then(Value::as_str).map(str::to_string));
            // A gate that failed on a blocking item goes by the `fail` route of the step, when
            // the step has one for it.
            let route = failed
                .as_ref()
                .filter(|d| d.get("blocks").and_then(Value::as_bool) == Some(true))
                .and_then(|d| d.get("fact").and_then(Value::as_str))
                .and_then(|fact| step.fail.get(fact).cloned());
            let converge = match failed
                .as_ref()
                .and_then(|d| d.get("fact").and_then(Value::as_str))
            {
                Some(fact) if step.extra_rounds > 0 => converge_of(ledger, step, &turns, fact)?,
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
                if step.on.contains_key(crate::adapter::COMMITS) {
                    branch = Some(commits_branch(ctx, ticket, step)?);
                } else if !step.on.is_empty() {
                    branch = Some(turn_branch(ctx, ledger, step, &t.id)?);
                }
            }
            KindFacts::Turn {
                last,
                failures,
                failure,
                route,
                converge,
                after,
                resume: visit.resume.clone(),
                message: visit.message.clone(),
            }
        }
        StepKind::Job(job) => {
            let head = head_of(&ticket.clone_dir()?)?;
            let passed = executor::job_passed(ledger, key.as_str(), &job.job, &head)?;
            let running =
                runs::in_flight_of(&ctx.paths, key.as_str(), &job.job).map(|run| run_words(&run));
            let after = if passed == Some(true) {
                op_facts(ctx, ledger, ticket, &step.after, None)?
            } else {
                Vec::new()
            };
            KindFacts::Job {
                passed,
                running,
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
            interrupt: visit.interrupt.clone().map(|hit| {
                // The event of the move names the interrupt.
                branch = Some(Branch {
                    to: hit.to.clone(),
                    how: hit.how(),
                    detail: Some(json!({"interrupt": hit.path, "value": hit.value})),
                });
                (hit.to.clone(), hit.how())
            }),
            // A merge conflict of a start verb wins over the interrupt, as it does in
            // `decide`, so the event of the move names the conflict.
            conflict: match &wait.conflict {
                Some(to) => {
                    conflict_of(ctx, ledger, ticket, &step.start)?.map(|(label, detail)| {
                        let how = format!("conflict {label}");
                        branch = Some(Branch {
                            to: to.clone(),
                            how: how.clone(),
                            detail: Some(json!({"conflict": label, "reason": detail})),
                        });
                        (to.clone(), how)
                    })
                }
                None => None,
            },
            target: wait_target(&step.on, visit.watched.as_deref()),
            waits: wait.wait.clone(),
            limit: wait.limit,
            waited: visit
                .since
                .as_deref()
                .and_then(|t| time::parse_rfc3339(t).ok())
                .map(|since| time::seconds_between(since, now)),
            until: opens_later(&visit, now).map(time::local_hm),
        },
        // The verbs of `run` at the commit of now: the same lookup as the `after` verbs of a
        // turn step, with no turn.
        StepKind::Act(act) => KindFacts::Act {
            run: op_facts(ctx, ledger, ticket, &act.run, None)?,
        },
        StepKind::End => KindFacts::End {
            finished: ticket.counts.finished.is_some(),
        },
        StepKind::Ask(_) | StepKind::Check(_) => {
            bail!(
                "the step {} has no facts of a turn, a job, a human action, or a wait",
                step.id
            )
        }
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
        // The other members of a team run at the same time: a member waits only for its own
        // turn, which its last turn says.
        running: !member && ledger.running_turns_of_ticket(key.as_str())? > 0,
        open_cards: if member {
            0
        } else {
            ledger.open_cards(key.as_str())?
        },
        refusal,
        cap,
        rounds: step.rounds,
        extra_rounds: step.extra_rounds,
        n: visit.n,
        skip_cap: visit.skip_cap,
        open: if cap_hit {
            open_items(ctx, ledger, key, step)?
        } else {
            Vec::new()
        },
        codex_fallback,
    })
}

/// Why the loop of a visit converges on the gate of the failed end fact `fact`: the counts of
/// that gate in the last turn and in the turn of the visit before it that has them. `None`
/// when the fact is no gate of the step, or when the loop does not converge.
fn converge_of(
    ledger: &Ledger,
    step: &Step,
    turns: &[TurnRow],
    fact: &str,
) -> Result<Option<String>> {
    let Some(gate) = step.expects.iter().find_map(|e| match e {
        Expect::Gate(gate) if gate.fact() == fact => Some(gate),
        _ => None,
    }) else {
        return Ok(None);
    };
    let mut counts = Vec::new();
    for row in turns.iter().rev() {
        if let Some(round) = ledger.gate_round(&row.id, &gate.doc)? {
            counts.push(round.count());
            if counts.len() == 2 {
                break;
            }
        }
    }
    // The last turn first: it must have its own count, or the loop gives no number.
    let last_has = match turns.last() {
        Some(row) => ledger.gate_round(&row.id, &gate.doc)?.is_some(),
        None => false,
    };
    if !last_has || counts.len() < 2 {
        return Ok(None);
    }
    Ok(super::follow::converges(
        gate.kind(),
        Some(counts[1]),
        Some(counts[0]),
    ))
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

/// Where a turn step with `on = { commits, none }` moves after its held turn: `commits` when
/// the head of the branch now differs from the head at the entry into the step, else `none`.
/// A visit with no head at its entry goes by `commits`, so the branch gets a fresh look. The
/// detail names the way and the two heads, for the event of the move.
fn commits_branch(ctx: &Ctx, ticket: &Ticket, step: &Step) -> Result<Branch> {
    let head = head_of(&ticket.clone_dir()?)?;
    let entry = ticket.visit().base;
    let fixes = match &step.kind {
        StepKind::Turn(turn) => turn.fixes.as_deref(),
        _ => None,
    };
    let way = if entry.as_deref() == Some(head.as_str()) {
        crate::adapter::NONE
    } else if step.on.contains_key(crate::adapter::MINOR)
        && fixes
            .and_then(|doc| {
                read_optional(&ctx.paths.ticket_doc(&ticket.key, doc))
                    .ok()
                    .flatten()
            })
            .is_some_and(|text| super::gates::only_minor_fixes(&text))
    {
        // The commits fix only findings that do not block: no new round of review for them.
        crate::adapter::MINOR
    } else {
        crate::adapter::COMMITS
    };
    let to = step.on.get(way).cloned().with_context(|| {
        format!(
            "the step {} has on with no target for {way}. Fix adapter.toml",
            step.id
        )
    })?;
    Ok(Branch {
        to,
        how: format!("on {way}"),
        detail: Some(json!({"way": way, "entry_head": entry, "head": head})),
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

pub(crate) fn clear_limit(ctx: &Ctx, ledger: &Ledger, ticket: &mut Ticket) -> Result<()> {
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
    match run_before(ctx, ledger, ticket, step)? {
        Before::Done => {}
        Before::Wait => return Ok(None),
        Before::Flag(action) => return Ok(Some(action)),
    }
    write_issue(ctx, ledger, ticket)?;
    Ok(Some(start_action(ticket, step, resume, message)))
}

/// Where the `before` entries of a turn step are.
pub(crate) enum Before {
    /// Each one is done in this visit.
    Done,
    /// One waits, such as for an approval or a reconcile.
    Wait,
    /// One failed: the action sets the flag.
    Flag(Action),
}

/// Runs each `before` entry of the step that is not done in this visit, in order.
pub(crate) fn run_before(
    ctx: &Ctx,
    ledger: &Ledger,
    ticket: &mut Ticket,
    step: &Step,
) -> Result<Before> {
    for (index, entry) in step.before.iter().enumerate() {
        let label = entry.label();
        // An entry that is done in this visit does not run again while a later one waits.
        if ticket.visit().before_is_done(index, &label) {
            continue;
        }
        let args = match entry_args(ctx, &ticket.key, entry) {
            Ok(args) => args,
            Err(text) => {
                return Ok(Before::Flag(flag_action(
                    ticket,
                    super::state::NEEDS_YOU,
                    &text,
                )))
            }
        };
        match entry.kind {
            EntryKind::Job => {
                let run = JobRun {
                    ticket: ticket.key.clone(),
                    step: step.id.clone(),
                    job: entry.name.clone(),
                    args,
                };
                // The job runs in the background; the visit waits for its outcome.
                let mark = format!(
                    "job-run:{}:{}:before:{index}:{}",
                    ticket.key,
                    ticket.visit().seq,
                    entry.name
                );
                let outcome = match runs::state(&ctx.paths, ledger, &mark)? {
                    RunState::Ended(outcome) => outcome,
                    RunState::InFlight(_) => return Ok(Before::Wait),
                    RunState::None => {
                        runs::request(ctx, ledger, &run, Some(&mark))?;
                        return Ok(Before::Wait);
                    }
                };
                if !outcome.ok {
                    let why = outcome.why();
                    return Ok(Before::Flag(flag_action(
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
                    return Ok(Before::Flag(flag_action(
                        ticket,
                        super::state::NEEDS_YOU,
                        &format!(
                            "{} before the step {}: {}: {}",
                            entry.name, step.id, op.outcome, op.detail
                        ),
                    )));
                }
                if op.outcome != Outcome::Confirmed {
                    return Ok(Before::Wait);
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
    Ok(Before::Done)
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
    let event = super::kind::decide_kind(ctx, ticket, &facts)?;
    save(ledger, ticket, &now, event)
}

/// Serves the read verbs of a turn that ended with requests, writes each answer as a document
/// of the ticket folder, and gives the action that resumes the session with the files.
#[allow(clippy::too_many_arguments)]
pub(crate) fn serve(
    ctx: &Ctx,
    ledger: &Ledger,
    adapter: &Adapter,
    ticket: &mut Ticket,
    step: &Step,
    turn_id: &str,
    session: &str,
    member: bool,
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
    let mut reads: Vec<&str> = adapter
        .verbs
        .values()
        .filter(|v| v.read)
        .map(|v| v.name.as_str())
        .collect();
    reads.extend(super::git_verbs::NAMES);
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
        // The read verbs of git: the core serves them in the clone, with no adapter call.
        if super::git_verbs::NAMES.contains(&verb) && !adapter.verbs.contains_key(verb) {
            match super::git_verbs::serve(ctx, ticket, verb, &args, &secrets) {
                Ok((name, detail)) => {
                    lines.push(format!("- {verb}: .vision/{name} ({detail})"));
                    files.push(name);
                }
                Err(e) => lines.push(format!("- {verb}: it failed, so it has no file: {e:#}")),
            }
            continue;
        }
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
        let extra = match (member, turn_agent(step)) {
            (true, Some((agent, _))) => visit.member_extra.entry(agent).or_default(),
            _ => &mut visit.extra,
        };
        for name in &files {
            if !extra.contains(name) {
                extra.push(name.clone());
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
    move_to_with(ctx, ledger, adapter, ticket, from, to, how, None)
}

/// [`move_to`] with more fields for the event `step`, such as the heads of a move by commits.
#[allow(clippy::too_many_arguments)]
pub(crate) fn move_to_with(
    ctx: &Ctx,
    ledger: &Ledger,
    adapter: &Adapter,
    ticket: &mut Ticket,
    from: &Step,
    to: &str,
    how: &str,
    extra: Option<&Value>,
) -> Result<()> {
    let Some(step) = adapter.step(to).cloned() else {
        bail!("the step {to} is not in the adapter");
    };
    let now = time::now_rfc3339();
    let turns = u32::try_from(ledger.step_turn_ids(ticket.key.as_str(), &step.id)?.len())?;
    let base = ticket.head();
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
        let mut detail =
            json!({"from": from.id, "to": step.id, "how": how, "n": visit.n, "seq": visit.seq});
        if let (Some(Value::Object(more)), Value::Object(fields)) = (extra, &mut detail) {
            for (key, value) in more {
                fields.insert(key.clone(), value.clone());
            }
        }
        ledger.add_event(
            &Event::now("tick", "step")
                .ticket(ticket.key.as_str())
                .detail(detail),
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
            by: None,
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
