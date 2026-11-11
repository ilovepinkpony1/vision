//! The engine in the update: the queue of the worker. The worker runs one job at a time, so the
//! update hands it a job only when it is free. An action of the person goes before a pass. A
//! pass starts when the engine starts, after each action, when a running turn ends, and each
//! minute; while the engine is paused, no pass starts, and actions still run.
//!
//! One pass does one thing for a ticket: it collects a turn, or moves the ticket to its next
//! step, or starts the next turn. So a pass that changed a ticket and left work ready for the
//! next pass starts that pass at once, and the ticket does not wait a minute between two steps.
//! A pass that changed nothing starts no quick pass, and the number of quick passes in a row has
//! a limit, so a ticket that cannot move never makes the engine spin.

use std::time::Duration;

use super::super::model::{App, Card, PassEnd, Run, RunningJob, Tone};
use super::super::msg::{Action, ChildResult, Effect, Job, JobDone, JobKind};
use super::super::text::{cut, error_line, tidy};
use super::{data, notice};

/// A pass starts each this many seconds: the interval of the system timer.
pub const PASS_EVERY: Duration = Duration::from_secs(crate::install::INTERVAL_SECONDS as u64);

/// The most quick passes in a row. The pass of the minute starts the count again.
pub const QUICK_MAX: u32 = 12;

/// The text of `vision tick` when a different tick holds the lock.
pub const LOCK_HELD: &str = "another tick holds the lock";

/// Starts the worker and puts the first pass into its queue.
pub fn start(app: &mut App, fx: &mut Vec<Effect>) {
    app.engine.started = true;
    fx.push(Effect::StartWorker);
    queue_pass(app);
}

fn next_id(app: &mut App) -> u64 {
    app.engine.next_id += 1;
    app.engine.next_id
}

/// Puts a pass into the queue, unless one already waits there.
pub fn queue_pass(app: &mut App) {
    if !app.engine.started || app.engine.queue.iter().any(|j| j.kind == JobKind::Pass) {
        return;
    }
    let id = next_id(app);
    app.engine.queue.push_back(Job {
        id,
        kind: JobKind::Pass,
    });
}

/// Puts an action into the queue. Gives its job id, and whether it waits behind a job.
pub fn queue_action(app: &mut App, action: Action) -> (u64, bool) {
    let id = next_id(app);
    let busy = app.engine.running.is_some() || !app.engine.started;
    if busy {
        let waits_for = match app.engine.running.as_ref().map(|r| &r.job.kind) {
            Some(JobKind::Pass) => "the pass that runs now",
            Some(JobKind::Action(_)) => "the action that runs now",
            None => "the start of the engine",
        };
        notice(
            app,
            format!("{} waits for {waits_for}. It starts next.", action.label()),
            Tone::Info,
        );
    }
    app.engine.queue.push_back(Job {
        id,
        kind: JobKind::Action(action),
    });
    (id, busy)
}

fn pass_waits_or_runs(app: &App) -> bool {
    app.engine.queue.iter().any(|j| j.kind == JobKind::Pass)
        || app
            .engine
            .running
            .as_ref()
            .is_some_and(|r| r.job.kind == JobKind::Pass)
}

/// A pass each minute while the engine is on.
pub fn timer(app: &mut App) {
    if !app.engine.started || app.engine.paused || pass_waits_or_runs(app) {
        return;
    }
    let due = app
        .engine
        .last_pass_start
        .is_none_or(|at| app.clock.elapsed >= at + PASS_EVERY);
    if due {
        app.engine.quick = 0;
        queue_pass(app);
    }
}

/// After a pass and the load of the local data that follows it: starts the next pass at once
/// when the pass changed a ticket and the next pass has work.
pub fn after_pass(app: &mut App) {
    if !std::mem::take(&mut app.engine.pass_ended) {
        return;
    }
    let (states, ready) = app.ticket_states();
    let changed = states != app.engine.before_pass;
    if changed && ready && !app.engine.paused && !app.off() && app.engine.quick < QUICK_MAX {
        app.engine.quick += 1;
        queue_pass(app);
    }
}

/// Hands the next job to the worker when it is free: an action first, then a pass unless the
/// engine is paused.
pub fn dispatch(app: &mut App, fx: &mut Vec<Effect>) {
    if !app.engine.started || app.engine.running.is_some() {
        return;
    }
    let queue = &app.engine.queue;
    let at = queue
        .iter()
        .position(|j| matches!(j.kind, JobKind::Action(_)))
        .or_else(|| {
            if app.engine.paused {
                None
            } else {
                queue.iter().position(|j| j.kind == JobKind::Pass)
            }
        });
    let Some(at) = at else {
        return;
    };
    let Some(job) = app.engine.queue.remove(at) else {
        return;
    };
    let now = app.clock.elapsed;
    if job.kind == JobKind::Pass {
        app.engine.last_pass_start = Some(now);
        app.engine.before_pass = app.ticket_states().0;
        app.engine.pass_ended = false;
    }
    if let Some(run) = card_run(app, job.id) {
        *run = Run::Running { job: job.id };
    }
    app.engine.running = Some(RunningJob {
        job: job.clone(),
        since: now,
    });
    app.dirty = true;
    fx.push(Effect::Run(job));
}

/// The state of the action of the open card, when the card waits for this job.
fn card_run(app: &mut App, job: u64) -> Option<&mut Run> {
    let run = match app.card.as_mut()? {
        Card::Start(card) => &mut card.run,
        Card::Resume(card) => &mut card.run,
        Card::Kill(card) => &mut card.run,
        Card::Stop(card) => &mut card.run,
        Card::Retry(card) => &mut card.run,
        Card::Opened(card) => &mut card.run,
        _ => return None,
    };
    (run.job() == Some(job)).then_some(run)
}

/// `p`: pauses the engine, or starts it again.
pub fn toggle_pause(app: &mut App) {
    app.engine.paused = !app.engine.paused;
    if app.engine.paused {
        notice(
            app,
            "The engine is paused. No new pass starts, and your actions still run. Press p to start it again.",
            Tone::Info,
        );
    } else {
        queue_pass(app);
        notice(app, "The engine is on again. A pass runs now.", Tone::Ok);
    }
}

pub fn on_done(app: &mut App, done: JobDone, fx: &mut Vec<Effect>) {
    if app
        .engine
        .running
        .as_ref()
        .is_some_and(|r| r.job.id == done.id)
    {
        app.engine.running = None;
    }
    app.dirty = true;
    match &done.kind {
        JobKind::Pass => pass_done(app, &done.result),
        JobKind::Action(action) => action_done(app, action, done.id, &done.result, fx),
    }
    // The local data loads again after each job of the worker.
    data::load_local(app, fx);
}

fn pass_done(app: &mut App, result: &ChildResult) {
    if result.ok() {
        let skipped = result.stdout.contains(LOCK_HELD);
        // A pass that ran: the load of the local data that follows shows what it changed.
        app.engine.pass_ended = !skipped;
        app.engine.last_pass = Some(if skipped {
            PassEnd::Skipped
        } else {
            PassEnd::Done
        });
        return;
    }
    let text = result.failure();
    notice(
        app,
        format!(
            "The pass failed: {}. Vision tries again in a minute.",
            cut(error_line(&text).trim_end_matches('.'), 80)
        ),
        Tone::Bad,
    );
    app.engine.last_pass = Some(PassEnd::Failed(text));
}

fn action_done(
    app: &mut App,
    action: &Action,
    job: u64,
    result: &ChildResult,
    fx: &mut Vec<Effect>,
) {
    let card_waits = card_run(app, job).is_some();
    if !result.ok() {
        let text = tidy(&result.failure());
        if let Some(run) = card_run(app, job) {
            *run = Run::Failed(text);
        } else {
            show_error(app, &format!("{} failed", action.label()), &text);
        }
        return;
    }
    if card_waits {
        app.card = None;
    }
    match action {
        Action::Take { key } => {
            let resumed = app.held_ticket(key).is_some_and(|h| h.stopped);
            let text = match (resumed, app.engine.paused) {
                (true, false) => format!("{key} starts again at its step. A pass runs now."),
                (false, false) => format!("Took {key}. A pass starts its first step now."),
                (true, true) => format!(
                    "{key} starts again at its step when the engine runs. Press p to start it."
                ),
                (false, true) => {
                    format!("Took {key}. The engine is paused, so press p to start its first step.")
                }
            };
            notice(app, text, Tone::Ok);
            app.home.select_next = Some(key.clone());
            data::load_queue(app, fx);
        }
        Action::Off => notice(
            app,
            "Vision is off. Each running turn stopped. Press K to turn it on again.",
            Tone::Ok,
        ),
        Action::On => notice(app, "Vision is on. The next pass acts again.", Tone::Ok),
        Action::Stop { key } => notice(
            app,
            format!("Stopped {key}. Its branch stays in its clone. Press Enter to start it again."),
            Tone::Ok,
        ),
        Action::Retry { key } => {
            let text = if app.engine.paused {
                format!("{key} goes on when the engine runs. Press p on Home to start it.")
            } else {
                format!("{key} goes on. A pass runs now.")
            };
            notice(app, text, Tone::Ok);
        }
        Action::Tell { key, .. } => super::tell::told(app, key, result),
    }
    queue_pass(app);
}

/// Shows the reason of a failure: a notice when it is short, else a card of its own.
pub fn show_error(app: &mut App, title: &str, text: &str) {
    let line = error_line(text);
    let one_line = text.trim().lines().count() <= 1;
    if one_line && line.chars().count() <= 70 {
        notice(app, format!("{title}: {line}"), Tone::Bad);
    } else {
        app.card = Some(Card::Error {
            title: title.to_string(),
            text: text.trim().to_string(),
        });
        app.dirty = true;
    }
}
