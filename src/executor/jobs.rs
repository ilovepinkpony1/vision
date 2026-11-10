//! Jobs and read verbs (section 11.1): `bin/job` for a job such as the proof, and `bin/act` for
//! a read verb that a turn asks for.
//!
//! A job stores each check of its answer in `checks`, one more row of the kind `job` with its
//! result (`pass`, `fail`, or `error`), and each document of its answer in the ticket folder.
//! A read verb changes nothing outside: it needs no gate, no approval, and no idempotency key.
//! Its answer carries documents, which the caller gives to the turn.

use std::time::{Duration, Instant};

use anyhow::{bail, Result};
use serde::Serialize;
use serde_json::{json, Value};

use super::clean::{self, object_args, Document, DETAIL_MAX};
use super::context::{short, TicketContext};
use super::ops::args_hash;
use super::scripts::{Answer, Runner, Script, Secrets, DEFAULT_TIMEOUT};
use crate::adapter;
use crate::ctx::Ctx;
use crate::exit::Stop;
use crate::ledger::operations::CheckRow;
use crate::ledger::{Event, Ledger};
use crate::paths::{Paths, TicketKey};
use crate::tick::JobRun;
use crate::util::fs::{atomic_write, ensure_dir};
use crate::util::time;

/// The causes of a check (section 11.1).
pub const CAUSES: [&str; 6] = ["code", "infra", "flaky", "env", "not ours", "unknown"];

/// The `kind` of the check row that holds the result of a whole job.
pub const JOB_CHECK: &str = "job";

/// What one run of a job gave.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct JobResult {
    pub job: String,
    pub ticket: String,
    /// The last commit that the job ran at. Empty for a ticket with no commit.
    pub head: String,
    /// True when the job passed.
    pub ok: bool,
    /// What the job said, for people. Each secret is masked.
    pub detail: String,
    /// The names of the documents that it wrote to the ticket folder.
    pub documents: Vec<String>,
    /// The checks of its answer, as stored.
    pub checks: Vec<CheckRow>,
    /// Why the script failed or refused, when it did.
    pub failure: Option<String>,
}

/// What a read verb gave.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct ReadAnswer {
    pub verb: String,
    pub detail: String,
    /// The documents for the turn. Each secret is masked.
    pub documents: Vec<Document>,
}

/// The source of the checks of a job, such as `job:proof`.
pub fn job_source(job: &str) -> String {
    format!("job:{job}")
}

/// Runs a job at the last commit of the ticket, and stores its checks and its documents.
/// A script that fails or refuses gives a result with `ok` false and a `failure`; an error is
/// only for a call that could not be made.
pub fn job(ctx: &Ctx, ledger: &Ledger, run: &JobRun) -> Result<JobResult> {
    let config = ctx.config()?;
    let adapter = adapter::load_with(config, &ctx.paths)?;
    let spec = adapter.job(&run.job)?.clone();
    let args = object_args(&run.args)?;
    let ticket = TicketContext::load(ledger, &adapter, &run.ticket)?;
    let base = ticket.base(&adapter)?;
    let head = ticket.head_text();
    let secrets = Secrets::read(config)?;
    let runner = Runner::new(&adapter, config, &secrets);
    if !runner.has(Script::Job) {
        bail!(
            "{} does not exist or is not executable, so the job {} cannot run. Fix the adapter",
            runner.path(Script::Job).display(),
            run.job
        );
    }
    let input = json!({
        "job": run.job,
        "key": run.ticket.as_str(),
        "clone": ticket.clone_text(),
        "branch": ticket.branch,
        "base": base,
        "head": ticket.head,
        "pr": ticket.pr,
        "args": args,
    });
    let timeout = spec
        .timeout
        .map(Duration::from_secs)
        .unwrap_or(DEFAULT_TIMEOUT);
    let started = Instant::now();
    let answer = runner.job(input, timeout, spec.secrets);
    let seconds = i64::try_from(started.elapsed().as_secs()).unwrap_or(i64::MAX);
    let at = time::now_rfc3339();
    let source = job_source(&run.job);
    let mut result = JobResult {
        job: run.job.clone(),
        ticket: run.ticket.to_string(),
        head: head.clone(),
        ok: false,
        detail: String::new(),
        documents: Vec::new(),
        checks: Vec::new(),
        failure: None,
    };
    match answer {
        Ok(Answer::Done(map)) => {
            let mut notes = Vec::new();
            match map.get("ok") {
                Some(Value::Bool(ok)) => result.ok = *ok,
                _ => notes.push("bin/job gave no ok, so the job did not pass".to_string()),
            }
            let (docs, dropped) = clean::documents(map.get("documents"));
            notes.extend(dropped);
            result.documents = write_documents(&ctx.paths, &run.ticket, &docs, &secrets)?;
            let (checks, bad) = checks(map.get("checks"), &run.ticket, &head, &source, &at);
            notes.extend(bad);
            result.checks = checks;
            result.detail = clean::text(map.get("detail"), DETAIL_MAX);
            for note in notes {
                if !result.detail.is_empty() {
                    result.detail.push_str(". ");
                }
                result.detail.push_str(&note);
            }
        }
        Ok(Answer::Refused(map)) => {
            result.failure = Some(format!(
                "bin/job refused: {}",
                clean::text(map.get("detail"), DETAIL_MAX)
            ));
        }
        Err(e) => result.failure = Some(e.to_string()),
    }
    for check in &result.checks {
        ledger.add_check(check)?;
    }
    ledger.add_check(&CheckRow {
        ticket: run.ticket.to_string(),
        head: head.clone(),
        kind: JOB_CHECK.into(),
        result: match (&result.failure, result.ok) {
            (Some(_), _) => "error",
            (None, true) => "pass",
            (None, false) => "fail",
        }
        .into(),
        command: job_command(&run.job, &args),
        seconds,
        cause: if result.failure.is_some() {
            "infra"
        } else {
            ""
        }
        .into(),
        source,
        at,
    })?;
    ctx.log(&format!(
        "job {} of {} at {}: {}{}",
        run.job,
        run.ticket,
        short(&head),
        if result.ok { "pass" } else { "no pass" },
        match (&result.failure, result.detail.is_empty()) {
            (Some(failure), _) => format!(": {failure}"),
            (None, false) => format!(": {}", result.detail),
            (None, true) => String::new(),
        }
    ));
    Ok(result)
}

/// The `command` of the check row of a whole job: the job name, and a hash of its arguments
/// when it has some, such as `proof 1f2e3d4c5b6a7980`.
pub fn job_command(job: &str, args: &Value) -> String {
    let hash = args_hash(args);
    if hash.is_empty() {
        job.to_string()
    } else {
        format!("{job} {hash}")
    }
}

/// The result of the newest run of a job at a commit, with any arguments: `Some(true)` when it
/// passed, `None` when it did not run there. Any new commit voids a pass.
pub fn job_passed(ledger: &Ledger, ticket: &str, job: &str, head: &str) -> Result<Option<bool>> {
    let source = job_source(job);
    Ok(ledger
        .checks_of(ticket, Some(head))?
        .into_iter()
        .rev()
        .find(|row| row.kind == JOB_CHECK && row.source == source)
        .map(|row| row.result == "pass"))
}

/// The result of the newest run of a job at a commit with these arguments. A run with other
/// arguments does not count, so a job runs again when its arguments change at the same commit.
pub fn job_passed_with(
    ledger: &Ledger,
    ticket: &str,
    job: &str,
    head: &str,
    args: &Value,
) -> Result<Option<bool>> {
    let source = job_source(job);
    let command = job_command(job, args);
    Ok(ledger
        .checks_of(ticket, Some(head))?
        .into_iter()
        .rev()
        .find(|row| row.kind == JOB_CHECK && row.source == source && row.command == command)
        .map(|row| row.result == "pass"))
}

/// Serves a read verb that a turn asked for: `bin/act` with `read` set to true. It records one
/// event, and gives the documents of the answer.
pub fn read(
    ctx: &Ctx,
    ledger: &Ledger,
    key: &TicketKey,
    verb: &str,
    args: &Value,
) -> Result<ReadAnswer> {
    let config = ctx.config()?;
    let adapter = adapter::load_with(config, &ctx.paths)?;
    let spec = adapter.verb(verb)?.clone();
    if !spec.read {
        let reads: Vec<&str> = adapter
            .verbs
            .values()
            .filter(|v| v.read)
            .map(|v| v.name.as_str())
            .collect();
        bail!(
            "{verb} is not a read verb, so a turn cannot ask for it. The read verbs: {}",
            if reads.is_empty() {
                "none".to_string()
            } else {
                reads.join(", ")
            }
        );
    }
    let args = object_args(args)?;
    let ticket = TicketContext::load(ledger, &adapter, key)?;
    let secrets = Secrets::read(config)?;
    let runner = Runner::new(&adapter, config, &secrets);
    let input = json!({
        "verb": verb,
        "read": true,
        "key": key.as_str(),
        "clone": ticket.clone_text(),
        "branch": ticket.branch,
        "head": ticket.head,
        "pr": ticket.pr,
        "args": args,
    });
    let timeout = spec
        .timeout
        .map(Duration::from_secs)
        .unwrap_or(DEFAULT_TIMEOUT);
    let answer = runner.act(input, timeout);
    let mut masked_args = args.clone();
    secrets.mask_json(&mut masked_args);
    let (outcome, result) = match answer {
        Ok(Answer::Done(map)) => {
            let (documents, dropped) = clean::documents(map.get("documents"));
            let mut detail = clean::text(map.get("detail"), DETAIL_MAX);
            for note in dropped {
                if !detail.is_empty() {
                    detail.push_str(". ");
                }
                detail.push_str(&note);
            }
            let documents: Vec<Document> = documents
                .into_iter()
                .map(|d| Document {
                    content: secrets.mask(&d.content),
                    name: d.name,
                })
                .collect();
            (
                "done",
                Ok(ReadAnswer {
                    verb: verb.to_string(),
                    detail,
                    documents,
                }),
            )
        }
        Ok(Answer::Refused(map)) => (
            "refused",
            Err(Stop::GateRefused(format!(
                "bin/act refused the read verb {verb}: {}",
                clean::text(map.get("detail"), DETAIL_MAX)
            ))
            .into()),
        ),
        Err(e) => (
            "failed",
            Err(anyhow::anyhow!(e).context(format!("the read verb {verb} failed"))),
        ),
    };
    let names: Vec<&str> = match &result {
        Ok(answer) => answer.documents.iter().map(|d| d.name.as_str()).collect(),
        Err(_) => Vec::new(),
    };
    ledger.add_event(&Event::now("executor", "read").ticket(key.as_str()).detail(
        json!({"verb": verb, "args": masked_args, "outcome": outcome, "documents": names}),
    ))?;
    result
}

/// Writes documents to the ticket folder, each secret masked. Gives their names.
pub fn write_documents(
    paths: &Paths,
    key: &TicketKey,
    documents: &[Document],
    secrets: &Secrets,
) -> Result<Vec<String>> {
    if documents.is_empty() {
        return Ok(Vec::new());
    }
    ensure_dir(&paths.ticket_dir(key))?;
    let mut names = Vec::new();
    for doc in documents {
        atomic_write(
            &paths.ticket_doc(key, &doc.name),
            secrets.mask(&doc.content).as_bytes(),
        )?;
        names.push(doc.name.clone());
    }
    Ok(names)
}

/// The checks of a job answer, ready to store, and one note for each one that was dropped.
fn checks(
    value: Option<&Value>,
    ticket: &TicketKey,
    head: &str,
    source: &str,
    at: &str,
) -> (Vec<CheckRow>, Vec<String>) {
    let mut rows = Vec::new();
    let mut notes = Vec::new();
    let Some(items) = value.and_then(Value::as_array) else {
        if value.is_some_and(|v| !v.is_null()) {
            notes.push("`checks` is not a list".to_string());
        }
        return (rows, notes);
    };
    for item in items {
        let kind = clean::line(item.get("kind"), 64);
        let result = clean::line(item.get("result"), 32);
        let (Some(kind), Some(result)) = (kind, result) else {
            notes.push("a check has no kind or no result, so it was dropped".to_string());
            continue;
        };
        if kind == JOB_CHECK {
            notes
                .push("a check of the kind `job` was dropped: the executor writes that row".into());
            continue;
        }
        let cause = clean::line(item.get("cause"), 16).unwrap_or_default();
        let cause = if cause.is_empty() || CAUSES.contains(&cause.as_str()) {
            cause
        } else {
            "unknown".to_string()
        };
        rows.push(CheckRow {
            ticket: ticket.to_string(),
            head: head.to_string(),
            kind,
            result,
            command: clean::line(item.get("command"), 500).unwrap_or_default(),
            seconds: item
                .get("seconds")
                .and_then(Value::as_i64)
                .unwrap_or(0)
                .max(0),
            cause,
            source: source.to_string(),
            at: at.to_string(),
        });
    }
    (rows, notes)
}
