//! The results of the loads: the local data and the queue of the tracker. A refresh never moves
//! the selection to a different ticket: the selection is a ticket key. A ticket whose turn just
//! ended puts a pass into the queue of the worker.

use super::super::model::{App, StageState, STAGE_LEDGER, STAGE_TRACKER};
use super::super::msg::Effect;
use super::super::text::{count, error_line};
use super::{boot, engine};
use crate::app::model::Local;
use crate::pipeline::commands::PickRow;

/// Starts a load of the local data, or asks for one more after the load that runs.
pub fn load_local(app: &mut App, fx: &mut Vec<Effect>) {
    if app.local.loading() {
        app.local.again = true;
        return;
    }
    app.local.since = Some(app.clock.elapsed);
    app.local.again = false;
    fx.push(Effect::LoadLocal);
}

/// Starts a load of the queue, or asks for one more after the load that runs.
pub fn load_queue(app: &mut App, fx: &mut Vec<Effect>) {
    if app.queue.loading() {
        app.queue.again = true;
        return;
    }
    app.queue.since = Some(app.clock.elapsed);
    app.queue.again = false;
    app.dirty = true;
    fx.push(Effect::LoadQueue);
}

pub fn on_local(app: &mut App, result: Result<Local, String>, fx: &mut Vec<Effect>) {
    let before: Vec<String> = app.items().iter().map(|i| i.key().to_string()).collect();
    let needs_before = app.needs().to_vec();
    let working: Vec<String> = app
        .held()
        .iter()
        .filter(|h| h.view.works.is_some())
        .map(|h| h.view.key.clone())
        .collect();
    // The turns that ran at the last load: the work turns, and the turns of the Judge and the
    // Curator, which no ticket shows as its work.
    let turns_before = app.local.data.as_ref().map_or(0, |l| l.turns);
    app.local.since = None;
    app.local.at = Some(app.clock.elapsed);
    match result {
        Ok(local) => {
            if app.local.data.as_ref() != Some(&local) || app.local.error.is_some() {
                app.dirty = true;
            }
            let text = format!(
                "{} in progress · {} · {}",
                local.tickets.len(),
                match local.turns {
                    0 => "no turn runs".to_string(),
                    1 => "1 turn runs".to_string(),
                    n => format!("{n} turns run"),
                },
                match local.needs.len() {
                    0 => "nothing waits for you".to_string(),
                    1 => "1 waits for you".to_string(),
                    n => format!("{n} wait for you"),
                }
            );
            // A turn that ran before and runs no more: a pass collects it now.
            let ended = local.turns < turns_before
                || working.iter().any(|key| {
                    local
                        .tickets
                        .iter()
                        .find(|h| &h.view.key == key)
                        .is_none_or(|h| h.view.works.is_none())
                });
            app.local.data = Some(local);
            app.local.error = None;
            if app.needs() != needs_before.as_slice() {
                super::needs::on_local_changed(app, fx);
            }
            boot::settle(app, STAGE_LEDGER, StageState::Done(text));
            if ended {
                engine::queue_pass(app);
            }
            // The data of a load that started before the pass ended says nothing about it.
            if !app.local.again {
                engine::after_pass(app);
            }
        }
        Err(error) => {
            if app.local.error.as_deref() != Some(error.as_str()) {
                app.dirty = true;
            }
            boot::settle(app, STAGE_LEDGER, StageState::Failed(short(&error)));
            app.local.error = Some(error);
        }
    }
    boot::ledger_settled(app, fx);
    keep_selection(app, &before);
    if app.local.again {
        load_local(app, fx);
    }
}

pub fn on_queue(app: &mut App, result: Result<Vec<PickRow>, String>, fx: &mut Vec<Effect>) {
    let before: Vec<String> = app.items().iter().map(|i| i.key().to_string()).collect();
    app.queue.since = None;
    app.queue.at = Some(app.clock.elapsed);
    app.dirty = true;
    match result {
        Ok(rows) => {
            let ready = rows
                .iter()
                .filter(|r| r.eligible && r.taken.is_none())
                .count();
            let text = match (rows.len(), ready) {
                (0, _) => "the queue is empty".to_string(),
                (_, 0) => "no ticket can start now".to_string(),
                (_, n) => format!("{} can start", count(n, "ticket")),
            };
            boot::settle(app, STAGE_TRACKER, StageState::Done(text));
            app.queue.data = Some(rows);
            app.queue.error = None;
        }
        Err(error) => {
            boot::settle(app, STAGE_TRACKER, StageState::Failed(short(&error)));
            app.queue.error = Some(error);
        }
    }
    keep_selection(app, &before);
    if app.queue.again {
        load_queue(app, fx);
    }
}

/// A short reason for a line of the loader.
fn short(error: &str) -> String {
    super::super::text::cut(&error_line(error), 52)
}

/// Keeps the selection on the same ticket key. A key that was asked for wins; a key that left
/// gives its place to the row that is now at the same place.
fn keep_selection(app: &mut App, before: &[String]) {
    let keys: Vec<String> = app.items().iter().map(|i| i.key().to_string()).collect();
    if let Some(next) = app.home.select_next.clone() {
        if keys.contains(&next) {
            app.home.selected = Some(next);
            app.home.select_next = None;
            return;
        }
    }
    if keys.is_empty() {
        app.home.selected = None;
        return;
    }
    match app.home.selected.clone() {
        Some(key) if keys.contains(&key) => {}
        Some(key) => {
            let at = before.iter().position(|k| *k == key).unwrap_or(0);
            app.home.selected = Some(keys[at.min(keys.len() - 1)].clone());
        }
        None => app.home.selected = Some(keys[0].clone()),
    }
}
