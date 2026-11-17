//! The reads of the live view of one ticket, through the functions of today that return data:
//! the status row, the facts of the state, the measures of the report, the timeline of `vision
//! log`, the plan of `vision plan`, the commits of `vision diff`, the model table, and the
//! adapter. The data of the view reads through the context and the connection that the loader
//! of the app keeps ([`super::loader`]); the plan, the commits, and the events of a turn each
//! read in a thread of the runtime. None of them changes the state.

use std::io::Read;

use super::feed::StreamLine;
use super::live::{Member, StepLine, TicketData};
use crate::ctx::Ctx;
use crate::ledger::Ledger;
use crate::paths::{Paths, TicketKey, TurnId};
use crate::pipeline::commands::{
    event_entry, other_entries, sort_timeline, timeline_rank, LogEntry,
};
use crate::pipeline::{self, DiffView, PlanView, SHORT_HASH};
use crate::snapshot::collect_needs_with;

/// The largest part of a stream file that the events of a turn read, from its end.
const TURN_READ_MAX: u64 = 8 * 1024 * 1024;

/// The data of the live view of one ticket, with its own context and connection, such as for a
/// test. The app reads through [`super::loader`], which keeps them.
pub fn ticket(paths: &Paths, key: &str) -> Result<TicketData, String> {
    let read = || -> anyhow::Result<TicketData> {
        let ctx = Ctx::new(paths.clone());
        ctx.require_setup()?;
        let ledger = ctx.ledger()?;
        let mut events = EventLines::new(key);
        let unpushed = unpushed(&ctx, &ledger, key);
        ticket_with(&ctx, &ledger, key, &mut events, false, unpushed)
    };
    read().map_err(|e| format!("{e:#}"))
}

/// The lines of the timeline of one ticket that come from its events, kept from one load to
/// the next. An event never changes after its write, so a load reads only the events after the
/// last one that it has.
#[derive(Clone, Debug, Default)]
pub struct EventLines {
    key: String,
    /// The id of the newest event that it read, and the number of the events that it read.
    last: i64,
    read: i64,
    /// The lines by time. Lines of one second keep the order of their events.
    lines: Vec<LogEntry>,
}

impl EventLines {
    pub fn new(key: &str) -> EventLines {
        EventLines {
            key: key.to_string(),
            ..EventLines::default()
        }
    }

    pub fn key(&self) -> &str {
        &self.key
    }

    /// The number of the events that it read.
    pub fn read(&self) -> i64 {
        self.read
    }

    /// Reads the events after the last one that it has. With `check`, it then compares the
    /// number of the events of the ticket and the newest id with what it read, and reads each
    /// event again when they differ.
    pub fn update(&mut self, ledger: &Ledger, check: bool) -> anyhow::Result<()> {
        self.read_after(ledger)?;
        if check && ledger.event_count_of_ticket(&self.key)? != (self.read, self.last) {
            *self = EventLines::new(&self.key);
            self.read_after(ledger)?;
        }
        Ok(())
    }

    fn read_after(&mut self, ledger: &Ledger) -> anyhow::Result<()> {
        for event in ledger.events_of_ticket_after(&self.key, self.last)? {
            self.last = event.id;
            self.read += 1;
            if let Some(line) = event_entry(&event) {
                // After each line of the same time or earlier: in the order of the events.
                let at = self.lines.partition_point(|l| l.at <= line.at);
                self.lines.insert(at, line);
            }
        }
        Ok(())
    }

    /// The timeline of the ticket: these lines with the lines of its turns, its cards, and its
    /// operations, which change after their write and so are read again. It is the same as
    /// [`pipeline::timeline`].
    pub fn timeline(&self, ledger: &Ledger) -> anyhow::Result<Vec<LogEntry>> {
        let mut others = other_entries(ledger, &TicketKey::parse(&self.key)?)?;
        sort_timeline(&mut others);
        let mut out = Vec::with_capacity(self.lines.len() + others.len());
        let mut events = self.lines.iter().peekable();
        for other in others {
            // An event goes first when it sorts before the other line or ties with it, as in
            // the stable sort of the whole timeline, where the events come first.
            while let Some(event) = events.next_if(|e| order(e) <= order(&other)) {
                out.push(event.clone());
            }
            out.push(other);
        }
        out.extend(events.cloned());
        Ok(out)
    }
}

/// The place of a line in the timeline: its time, then its rank in one second.
fn order(line: &LogEntry) -> (&str, u8) {
    (line.at.as_str(), timeline_rank(line))
}

/// The data of the live view of one ticket from a context and a connection that the caller
/// keeps, with the event lines of the ticket that it keeps, and the count of the commits that
/// the forge does not have. Each read runs in one read transaction, and each read is of this
/// ticket only.
pub fn ticket_with(
    ctx: &Ctx,
    ledger: &Ledger,
    key: &str,
    events: &mut EventLines,
    check_events: bool,
    unpushed: Option<usize>,
) -> anyhow::Result<TicketData> {
    ledger.in_read(|ledger| read_ticket(ctx, ledger, key, events, check_events, unpushed))
}

fn read_ticket(
    ctx: &Ctx,
    ledger: &Ledger,
    key: &str,
    events: &mut EventLines,
    check_events: bool,
    unpushed: Option<usize>,
) -> anyhow::Result<TicketData> {
    ctx.require_setup()?;
    let ticket_key = TicketKey::parse(key)?;
    // A proposal of the Curator names no ticket, so the needs of a ticket need no memory.
    let needs = collect_needs_with(ctx, ledger, Vec::new())?;
    let running = crate::turn::running_turns(ctx, ledger)?;
    let Some(view) = pipeline::view::row_of(ctx, ledger, key, &needs, &running)? else {
        anyhow::bail!("Vision has no ticket {key} now");
    };
    let facts =
        pipeline::facts::state_facts_of_with(ctx, ledger, key, &running)?.unwrap_or_default();
    let row = ledger.ticket(key)?;
    let measures = pipeline::commands::measures_of(ledger, key)?;
    let config = ctx.config()?;
    // A broken adapter leaves the path empty, and the rest of the view stays.
    let adapter = crate::adapter::load(ctx).ok();
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
    // The agents that the adapter names, and the Judge and the Curator. With no adapter, each
    // agent of Vision.
    let named: Option<Vec<String>> = adapter.as_ref().map(|a| {
        a.steps
            .iter()
            .filter_map(|s| match &s.kind {
                crate::adapter::StepKind::Turn(turn) => Some(turn.agents()),
                _ => None,
            })
            .flatten()
            .map(str::to_string)
            .collect()
    });
    // The agents of the adapter come after those of Vision.
    let defined = adapter
        .as_ref()
        .map(|a| a.agents.clone())
        .unwrap_or_default();
    let team = crate::models::names_in(&defined)
        .into_iter()
        .filter(|name| {
            crate::models::agent(name).is_some_and(|spec| spec.alone)
                || named
                    .as_ref()
                    .is_none_or(|names| names.iter().any(|n| n == name))
        })
        .filter_map(
            |name| match crate::models::choice_in(config, &defined, &name) {
                Ok(choice) => Some(Member {
                    agent: name,
                    model: choice.model,
                    effort: choice.effort.to_string(),
                }),
                Err(_) => crate::models::defaults(&defined, &name)
                    .ok()
                    .map(|base| Member {
                        agent: name,
                        model: base.model,
                        effort: base.effort.to_string(),
                    }),
            },
        )
        .collect();
    if events.key() != key {
        *events = EventLines::new(key);
    }
    events.update(ledger, check_events)?;
    let timeline = events.timeline(ledger)?;
    let plan = std::fs::read(ctx.paths.ticket_doc(&ticket_key, "plan.md"))
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

/// True when the count of the commits that the forge does not have means something for the
/// ticket: it holds a clone, and its end step did not run.
pub fn holds_clone(ledger: &Ledger, key: &str) -> anyhow::Result<bool> {
    Ok(ledger.ticket(key)?.is_some_and(|row| {
        row.clone.is_some() && crate::pipeline::state::Counts::of(&row).finished.is_none()
    }))
}

/// The number of the commits of the branch that the forge does not have, by the refs of the
/// clone, with no fetch. `None` when the ticket holds no clone, or when the comparison fails.
/// It runs three git commands, so the loader reads it again only each 10 seconds.
pub fn unpushed(ctx: &Ctx, ledger: &Ledger, key: &str) -> Option<usize> {
    if !holds_clone(ledger, key).unwrap_or(false) {
        return None;
    }
    let key = TicketKey::parse(key).ok()?;
    pipeline::commands::diff_with(ctx, ledger, &key, false)
        .ok()
        .map(|d| d.commits.len())
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
