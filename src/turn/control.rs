//! The control of running turns: list them, kill one or all, and send a message to one
//! (section 7.5).

use std::fs;
use std::path::Path;
use std::time::{Duration, Instant};

use anyhow::{bail, Result};
use serde::Serialize;

use crate::ctx::Ctx;
use crate::ledger::turns::STATUS_RUNNING;
use crate::ledger::Ledger;
use crate::paths::{TicketKey, TurnId, TurnPaths};
use crate::turn::collect::START_GRACE_SECS;
use crate::turn::inbox::{self, MessageFile};
use crate::turn::state::{self, Phase, Reason, TurnState};
use crate::util::time;

/// How long a kill waits for the supervisor to stop the session and write the end.
const KILL_WAIT: Duration = Duration::from_secs(5);

/// A turn whose supervisor lives, or that waits for its supervisor to start.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct RunningTurn {
    pub id: String,
    pub ticket: String,
    pub agent: Option<String>,
    pub step: Option<String>,
    pub kind: Option<String>,
    pub started: Option<String>,
    pub phase: Phase,
    pub supervisor_pid: Option<u32>,
}

impl RunningTurn {
    pub fn turn_id(&self) -> Result<TurnId> {
        TurnId::parse(&self.id)
    }

    pub fn ticket_key(&self) -> Result<TicketKey> {
        TicketKey::parse(&self.ticket)
    }
}

/// The turns that run now, oldest first.
pub fn running_turns(ctx: &Ctx, ledger: &Ledger) -> Result<Vec<RunningTurn>> {
    let now = time::now();
    let mut running = Vec::new();
    for row in ledger.turns_with_status(STATUS_RUNNING)? {
        let (Ok(key), Ok(id)) = (TicketKey::parse(&row.ticket), TurnId::parse(&row.id)) else {
            continue;
        };
        let turn = ctx.paths.turn(&key, &id);
        let current = TurnState::read(&turn.status).unwrap_or(None);
        let phase = current.as_ref().map_or(Phase::Queued, |s| s.state);
        let alive = match phase {
            Phase::Ended => false,
            Phase::Starting | Phase::Running => state::supervisor_alive(&turn.dir),
            Phase::Queued => {
                let age = row
                    .started
                    .as_deref()
                    .and_then(|s| time::parse_rfc3339(s).ok())
                    .map_or(i64::MAX, |at| time::seconds_between(at, now));
                age < START_GRACE_SECS || state::supervisor_alive(&turn.dir)
            }
        };
        if alive {
            running.push(RunningTurn {
                id: row.id,
                ticket: row.ticket,
                agent: row.agent,
                step: row.step,
                kind: row.kind,
                started: row.started,
                phase,
                supervisor_pid: current.and_then(|s| s.pid),
            });
        }
    }
    Ok(running)
}

/// The newest running turn of a ticket.
pub fn running_turn_of(ctx: &Ctx, ledger: &Ledger, key: &TicketKey) -> Result<Option<RunningTurn>> {
    Ok(running_turns(ctx, ledger)?
        .into_iter()
        .rfind(|t| t.ticket == key.as_str()))
}

/// Kills one turn: TERM to its supervisor, which stops the session and ends with the reason
/// `killed`. When the supervisor does not end in 5 seconds, both get KILL. Gives false when
/// the turn did not run.
pub fn kill_turn(ctx: &Ctx, key: &TicketKey, id: &TurnId) -> Result<bool> {
    let turn = ctx.paths.turn(key, id);
    kill_in(&turn)
}

fn kill_in(turn: &TurnPaths) -> Result<bool> {
    let Some(current) = TurnState::read(&turn.status)? else {
        return Ok(false);
    };
    if current.is_ended() {
        return Ok(false);
    }
    if !state::supervisor_alive(&turn.dir) {
        if current.state == Phase::Queued {
            // The supervisor has not started yet. It refuses a turn that already ended.
            current
                .ended(Reason::Killed, Some("killed before it started".into()))
                .write(&turn.status)?;
            return Ok(true);
        }
        // The supervisor died: the next tick marks a crash. Stop what is left of the session.
        if let Some(child) = current.child_pid {
            state::kill_orphan_group(child);
        }
        return Ok(false);
    }
    // A supervisor that just took its lock writes its pid a moment later.
    let mut current = current;
    let until = Instant::now() + Duration::from_secs(1);
    while current.pid.is_none() && Instant::now() < until {
        std::thread::sleep(Duration::from_millis(20));
        if let Some(latest) = TurnState::read(&turn.status)? {
            current = latest;
        }
    }
    if current.is_ended() {
        return Ok(false);
    }
    let Some(pid) = current.pid else {
        bail!(
            "{} names no supervisor pid, so the turn cannot be killed",
            turn.status.display()
        );
    };
    state::signal_pid(pid, libc::SIGTERM);
    let until = Instant::now() + KILL_WAIT;
    while state::supervisor_alive(&turn.dir) && Instant::now() < until {
        std::thread::sleep(Duration::from_millis(50));
    }
    if state::supervisor_alive(&turn.dir) {
        state::signal_pid(pid, libc::SIGKILL);
        if let Some(child) = current.child_pid {
            state::signal_group(child, libc::SIGKILL);
        }
        let until = Instant::now() + Duration::from_secs(1);
        while state::supervisor_alive(&turn.dir) && Instant::now() < until {
            std::thread::sleep(Duration::from_millis(20));
        }
        let latest = TurnState::read(&turn.status)?.unwrap_or(current);
        if !latest.is_ended() {
            latest
                .ended(
                    Reason::Killed,
                    Some("the supervisor did not stop after TERM, so it got KILL".into()),
                )
                .write(&turn.status)?;
        }
    }
    Ok(true)
}

/// Kills each running turn. It reads the turn folders, not the ledger, so that the kill
/// switch works even when the ledger is broken. Gives the ids of the killed turns.
pub fn kill_all(ctx: &Ctx) -> Result<Vec<String>> {
    let mut killed = Vec::new();
    for (key, id) in turn_folders(&ctx.paths.tickets_dir()) {
        let turn = ctx.paths.turn(&key, &id);
        let Ok(Some(current)) = TurnState::read(&turn.status) else {
            continue;
        };
        if current.is_ended() {
            continue;
        }
        if kill_in(&turn)? {
            killed.push(id.to_string());
        }
    }
    Ok(killed)
}

/// The ticket and the id of each `tickets/<KEY>/turns/<id>/` folder.
fn turn_folders(tickets: &Path) -> Vec<(TicketKey, TurnId)> {
    let mut found = Vec::new();
    let Ok(tickets) = fs::read_dir(tickets) else {
        return found;
    };
    for ticket in tickets.flatten() {
        let Ok(key) = TicketKey::parse(&ticket.file_name().to_string_lossy()) else {
            continue;
        };
        let Ok(turns) = fs::read_dir(ticket.path().join("turns")) else {
            continue;
        };
        for turn in turns.flatten() {
            if let Ok(id) = TurnId::parse(&turn.file_name().to_string_lossy()) {
                found.push((key.clone(), id));
            }
        }
    }
    found.sort();
    found
}

/// Where a message went.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "to", content = "turn", rename_all = "lowercase")]
pub enum Told {
    /// Into the inbox of this running turn.
    Running(String),
    /// Stored for the next turn of the ticket.
    Stored,
}

/// Sends a message to the running turn of a ticket, or stores it for the next turn. The
/// ledger keeps each message in `messages`.
pub fn tell(ctx: &Ctx, ledger: &Ledger, key: &TicketKey, text: &str, source: &str) -> Result<Told> {
    if text.trim().is_empty() {
        bail!("the message is empty. Give the text to send, such as vision tell DEMO-12 \"use the old export\"");
    }
    let at = time::now_rfc3339();
    // A message of the person is for the work agent, never for the Judge or the Curator.
    let work = running_turns(ctx, ledger)?.into_iter().rfind(|t| {
        t.ticket == key.as_str()
            && !t
                .agent
                .as_deref()
                .and_then(crate::models::agent)
                .is_some_and(|spec| spec.alone)
    });
    match work {
        Some(running) => {
            let id = running.turn_id()?;
            let message_id = ledger.add_message(Some(&running.id), &at, source, text)?;
            let message = MessageFile {
                text: text.to_string(),
                source: source.to_string(),
                at,
                id: Some(message_id),
            };
            inbox::write(&ctx.paths.turn(key, &id).inbox, &message)?;
            Ok(Told::Running(running.id))
        }
        None => {
            let message_id = ledger.add_message(None, &at, source, text)?;
            let message = MessageFile {
                text: text.to_string(),
                source: source.to_string(),
                at,
                id: Some(message_id),
            };
            inbox::write(&inbox::ticket_inbox(&ctx.paths, key), &message)?;
            Ok(Told::Stored)
        }
    }
}

/// `vision tell KEY "TEXT"`.
pub fn tell_command(ctx: &Ctx, key: &TicketKey, text: &str) -> Result<()> {
    ctx.require_setup()?;
    let ledger = ctx.ledger()?;
    match tell(ctx, &ledger, key, text, "user")? {
        Told::Running(turn) => println!(
            "Sent to the running turn {turn} of {key}. The session gets it at its next step."
        ),
        Told::Stored => println!("No turn of {key} runs, so the message waits for its next turn."),
    }
    Ok(())
}
