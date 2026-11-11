//! The reads of the live view of one ticket, each in a thread of the runtime, through the
//! functions of today that return data: the status rows, the facts of the state, the report,
//! the timeline of `vision log`, the plan of `vision plan`, the commits of `vision diff`, the
//! model table, and the adapter. None of them changes the state.

use std::io::Read;
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use super::feed::StreamLine;
use super::live::{Member, StepLine, TicketData};
use crate::ctx::Ctx;
use crate::paths::{Paths, TicketKey, TurnId};
use crate::pipeline::{self, DiffView, PlanView, SHORT_HASH};
use crate::snapshot::collect_needs;

/// The largest part of a stream file that the events of a turn read, from its end.
const TURN_READ_MAX: u64 = 8 * 1024 * 1024;

/// The data of the live view of one ticket.
pub fn ticket(paths: &Paths, key: &str) -> Result<TicketData, String> {
    read_ticket(paths, key).map_err(|e| format!("{e:#}"))
}

fn read_ticket(paths: &Paths, key: &str) -> anyhow::Result<TicketData> {
    let ctx = Ctx::new(paths.clone());
    ctx.require_setup()?;
    let ticket_key = TicketKey::parse(key)?;
    let ledger = ctx.ledger()?;
    let needs = collect_needs(&ctx, &ledger)?;
    let rows = pipeline::status_rows(&ctx, &ledger, &needs)?;
    let Some(view) = rows.into_iter().find(|r| r.key == key) else {
        anyhow::bail!("Vision has no ticket {key} now");
    };
    let facts = pipeline::facts::state_facts_of(&ctx, &ledger, key)?.unwrap_or_default();
    let row = ledger.ticket(key)?;
    let report = pipeline::report_of(&ledger, None)?;
    let measures = report.tickets.get(key).cloned().unwrap_or_default();
    let config = ctx.config()?;
    // A broken adapter leaves the path empty, and the rest of the view stays.
    let adapter = crate::adapter::load_with(config, paths).ok();
    let mut steps: Vec<StepLine> = adapter
        .iter()
        .flat_map(|a| &a.steps)
        .map(|step| StepLine {
            id: step.id.clone(),
            kind: step.kind.name().to_string(),
            ..StepLine::default()
        })
        .collect();
    let mut jobs: Vec<String> = Vec::new();
    for expect in adapter
        .iter()
        .flat_map(|a| &a.steps)
        .flat_map(|s| &s.expects)
    {
        if let crate::adapter::Expect::Job(name) = expect {
            if !jobs.contains(name) {
                jobs.push(name.clone());
            }
        }
    }
    for step in &mut steps {
        step.entries = facts.entries.get(&step.id).copied().unwrap_or(0);
        step.seconds = measures.step_seconds.get(&step.id).copied().unwrap_or(0);
        step.cost_usd = measures.step_cost_usd.get(&step.id).copied().unwrap_or(0.0);
    }
    let team = crate::models::AGENTS
        .iter()
        .map(|spec| match crate::models::choice(config, spec.name) {
            Ok(choice) => Member {
                agent: spec.name.to_string(),
                model: choice.model,
                effort: choice.effort.to_string(),
            },
            Err(_) => Member {
                agent: spec.name.to_string(),
                model: spec.model.to_string(),
                effort: spec.effort.to_string(),
            },
        })
        .collect();
    let timeline = pipeline::timeline(&ledger, &ticket_key)?;
    let unpushed = if view.clone.is_some() && !facts.finished {
        unpushed(&ctx, &ticket_key)
    } else {
        None
    };
    let plan = std::fs::read(paths.ticket_doc(&ticket_key, "plan.md"))
        .ok()
        .map(|bytes| pipeline::plan_hash(&bytes)[..SHORT_HASH].to_string());
    Ok(TicketData {
        taken: row.and_then(|r| r.taken),
        needs: needs
            .into_iter()
            .filter(|n| n.ticket.as_deref() == Some(key))
            .collect(),
        view,
        facts,
        steps,
        team,
        timeline,
        measures,
        unpushed,
        plan,
        jobs,
    })
}

/// The count of the commits that the forge does not have is read again only after this time.
/// The view loads each second, the count needs three git commands, and it changes only with a
/// commit or a push.
const COMMITS_EVERY: Duration = Duration::from_secs(10);

/// The last count of the commits: the home, the ticket, when it was read, and the count.
type CommitCount = (PathBuf, String, Instant, Option<usize>);

static COMMITS: Mutex<Option<CommitCount>> = Mutex::new(None);

/// The number of the commits of the branch that the forge does not have, by the refs of the
/// clone, with no fetch. `None` when the comparison fails.
fn unpushed(ctx: &Ctx, key: &TicketKey) -> Option<usize> {
    let mut last = COMMITS.lock().unwrap_or_else(|e| e.into_inner());
    if let Some((home, ticket, at, count)) = last.as_ref() {
        if home == ctx.paths.home() && ticket == key.as_str() && at.elapsed() < COMMITS_EVERY {
            return *count;
        }
    }
    let count = pipeline::diff_of(ctx, key, false)
        .ok()
        .map(|d| d.commits.len());
    *last = Some((
        ctx.paths.home().to_path_buf(),
        key.to_string(),
        Instant::now(),
        count,
    ));
    count
}

/// The plan of a ticket, or `None` when it has no plan yet.
pub fn plan(paths: &Paths, key: &str) -> Result<Option<PlanView>, String> {
    let read = || -> anyhow::Result<Option<PlanView>> {
        let ctx = Ctx::new(paths.clone());
        pipeline::plan_of(&ctx, &TicketKey::parse(key)?)
    };
    read().map_err(|e| format!("{e:#}"))
}

/// The commits of a ticket that the forge does not have, after a fetch.
pub fn diff(paths: &Paths, key: &str) -> Result<DiffView, String> {
    let read = || -> anyhow::Result<DiffView> {
        let ctx = Ctx::new(paths.clone());
        pipeline::diff_of(&ctx, &TicketKey::parse(key)?, true)
    };
    read().map_err(|e| format!("{e:#}"))
}

/// The events of one turn: the lines of its stream file, read as `vision logs` reads them.
/// A large file is read from its last 8 MB.
pub fn turn_events(paths: &Paths, key: &str, turn: &str) -> Result<Vec<StreamLine>, String> {
    let read = || -> anyhow::Result<Vec<StreamLine>> {
        let key = TicketKey::parse(key)?;
        let id = TurnId::parse(turn)?;
        let path = paths.turn(&key, &id).stream;
        let mut file = match std::fs::File::open(&path) {
            Ok(file) => file,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(e.into()),
        };
        let size = file.metadata()?.len();
        if size > TURN_READ_MAX {
            use std::io::Seek;
            file.seek(std::io::SeekFrom::Start(size - TURN_READ_MAX))?;
        }
        let mut bytes = Vec::new();
        file.read_to_end(&mut bytes)?;
        let text = String::from_utf8_lossy(&bytes);
        Ok(text.lines().filter_map(parse_line).collect())
    };
    read().map_err(|e| format!("{e:#}"))
}

/// One line of a stream file as an event, with the output tokens of its message when the line
/// gives them.
pub fn parse_line(line: &str) -> Option<StreamLine> {
    let value: serde_json::Value = serde_json::from_str(line.trim()).ok()?;
    let event = crate::turn::parse_stream_value(&value)?;
    let message = value.get("message");
    let output = message
        .and_then(|m| m.get("usage"))
        .and_then(|u| u.get("output_tokens"))
        .and_then(serde_json::Value::as_i64)
        .map(|n| {
            let id = message
                .and_then(|m| m.get("id"))
                .and_then(serde_json::Value::as_str)
                .unwrap_or("")
                .to_string();
            (id, n)
        });
    Some(StreamLine { event, output })
}
