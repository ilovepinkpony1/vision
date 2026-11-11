//! The keys of a pager over the live view, and the texts that its loads bring: the plan, the
//! commits, and the events of a turn. Up, down, `k`, and `j` move by one line (the cursor in
//! the log), PageUp and PageDown by a page, Home and `g` go to the start, End and `G` to the
//! end, Enter on a turn of the log opens its events, and Esc closes the pager.

use super::super::feed::StreamLine;
use super::super::model::{App, Tone};
use super::super::msg::{Effect, Key};
use super::super::pager::{body_rows, Pager, PagerKind, PagerState};
use super::super::pages::{diff_lines, plan_lines, turn_lines};
use super::{ask_quit, notice};
use crate::pipeline::{DiffView, PlanView};

fn top(app: &mut App) -> Option<&mut Pager> {
    app.live.as_mut()?.pagers.last_mut()
}

pub fn on_key(app: &mut App, key: Key, fx: &mut Vec<Effect>) {
    let height = app.size.1;
    let Some(pager) = top(app) else {
        return;
    };
    let rows = body_rows(height, pager.head.is_some());
    match key {
        Key::Esc => {
            if let Some(live) = app.live.as_mut() {
                live.pagers.pop();
            }
        }
        Key::Up | Key::Char('k') => pager.step(-1, rows),
        Key::Down | Key::Char('j') => pager.step(1, rows),
        Key::PageUp => pager.step(-(rows.max(2) as i64 - 1), rows),
        Key::PageDown => pager.step(rows.max(2) as i64 - 1, rows),
        Key::Home | Key::Char('g') => pager.first(),
        Key::End | Key::Char('G') => pager.last(rows),
        Key::Enter if pager.kind == PagerKind::Log => {
            let Some(turn) = pager.selected_turn().map(str::to_string) else {
                return;
            };
            let Some(key) = app.live.as_ref().map(|l| l.key.clone()) else {
                return;
            };
            let title = turn_title(&turn, &key, app.clock.offset_seconds);
            if let Some(live) = app.live.as_mut() {
                live.pagers
                    .push(Pager::loading(PagerKind::Turn { id: turn.clone() }, title));
            }
            fx.push(Effect::LoadTurn { key, turn });
        }
        Key::Char('q') => ask_quit(app, fx),
        _ => {}
    }
}

/// The title of the pager of a turn: its step and number, and its start in the local time,
/// such as `turn build-1 of 14:24`.
fn turn_title(turn: &str, key: &str, offset_seconds: i32) -> String {
    match crate::paths::TurnId::parse(turn) {
        Ok(id) => {
            let at = id.at() + time::Duration::seconds(i64::from(offset_seconds));
            let step = id
                .name()
                .strip_prefix(&format!("{key}-"))
                .unwrap_or(id.name());
            format!(
                "turn {step}-{} of {:02}:{:02}",
                id.n(),
                at.hour(),
                at.minute()
            )
        }
        Err(_) => format!("turn {turn}"),
    }
}

/// The pager of this kind, while it waits for its text.
fn waiting<'a>(app: &'a mut App, key: &str, kind: &PagerKind) -> Option<&'a mut Pager> {
    let live = app.live.as_mut().filter(|l| l.key == key)?;
    live.pagers
        .iter_mut()
        .rev()
        .find(|p| p.kind == *kind && p.state == PagerState::Loading)
}

/// Removes a pager that waits for a text that will not come.
fn drop_waiting(app: &mut App, kind: &PagerKind) {
    if let Some(live) = app.live.as_mut() {
        live.pagers
            .retain(|p| !(p.kind == *kind && p.state == PagerState::Loading));
    }
}

pub fn on_plan(app: &mut App, key: &str, plan: Result<Option<PlanView>, String>) {
    app.dirty = true;
    let rows = body_rows(app.size.1, false);
    match plan {
        Ok(Some(plan)) => {
            if let Some(pager) = waiting(app, key, &PagerKind::Plan) {
                pager.title = format!("plan {}", &plan.hash[..12.min(plan.hash.len())]);
                pager.fill(None, plan_lines(&plan), rows);
            }
        }
        Ok(None) => {
            drop_waiting(app, &PagerKind::Plan);
            notice(
                app,
                format!("{key} has no plan yet. Its plan step writes the plan."),
                Tone::Info,
            );
        }
        Err(error) => {
            if let Some(pager) = waiting(app, key, &PagerKind::Plan) {
                pager.state = PagerState::Failed(error);
            }
        }
    }
}

pub fn on_diff(app: &mut App, key: &str, diff: Result<DiffView, String>) {
    app.dirty = true;
    let rows = body_rows(app.size.1, false);
    if let Some(pager) = waiting(app, key, &PagerKind::Diff) {
        match diff {
            Ok(diff) => pager.fill(None, diff_lines(&diff), rows),
            Err(error) => pager.state = PagerState::Failed(error),
        }
    }
}

pub fn on_turn(app: &mut App, key: &str, turn: &str, events: Result<Vec<StreamLine>, String>) {
    app.dirty = true;
    let rows = body_rows(app.size.1, false);
    let kind = PagerKind::Turn {
        id: turn.to_string(),
    };
    if let Some(pager) = waiting(app, key, &kind) {
        match events {
            Ok(events) => pager.fill(None, turn_lines(&events), rows),
            Err(error) => pager.state = PagerState::Failed(error),
        }
    }
}
