//! The keys of Home.
//!
//! The arrows and `k`/`j` move through both boxes; Enter opens the live view of a ticket in progress,
//! opens the start card of a ticket that can start, says why another ticket cannot start, and
//! asks to resume a stopped ticket; `n` opens the card of the first thing that waits; `m` opens
//! Models and `s` opens Stats; `p` pauses the engine; `r` loads the queue again; `K` opens the
//! kill switch card; `?` shows the keys; `q` and Ctrl-C leave.

use super::super::model::{App, Card, Item, KillCard, ResumeCard, Run, StartCard, Tone};
use super::super::msg::{Effect, Key};
use super::{ask_quit, data, engine, models, needs, notice, stats};

pub fn on_key(app: &mut App, key: Key, fx: &mut Vec<Effect>) {
    match key {
        Key::Up | Key::Char('k') => step(app, -1),
        Key::Down | Key::Char('j') => step(app, 1),
        Key::Enter => open(app, fx),
        _ => common(app, key, fx),
    }
}

/// The keys that work on each screen.
fn common(app: &mut App, key: Key, fx: &mut Vec<Effect>) {
    match key {
        Key::Char('n') => needs::open(app, None, fx),
        Key::Char('m') => models::open(app, fx),
        Key::Char('s') => stats::open(app, fx),
        Key::Char('p') => engine::toggle_pause(app),
        Key::Char('r') => data::load_queue(app, fx),
        Key::Char('K') => {
            app.card = Some(Card::Kill(KillCard {
                turn_on: app.off(),
                run: Run::Idle,
            }))
        }
        Key::Char('?') => app.card = Some(Card::Help),
        Key::Char('q') => ask_quit(app, fx),
        _ => {}
    }
}

fn step(app: &mut App, by: i32) {
    let keys: Vec<String> = app.items().iter().map(|i| i.key().to_string()).collect();
    if keys.is_empty() {
        return;
    }
    let at = app.selected_index().map_or(-1, |i| i as i32);
    let next = (at + by).clamp(0, keys.len() as i32 - 1) as usize;
    app.home.selected = Some(keys[next].clone());
}

fn open(app: &mut App, fx: &mut Vec<Effect>) {
    let Some(index) = app.selected_index() else {
        return;
    };
    enum Choice {
        Resume(ResumeCard),
        Detail(String),
        Start(StartCard),
        Cannot(String),
    }
    let choice = match &app.items()[index] {
        Item::Held(held) if held.stopped => Choice::Resume(ResumeCard {
            key: held.view.key.clone(),
            title: app.title_of(held).to_string(),
            step: held.view.step.clone().unwrap_or_default(),
            clone: held.view.clone.clone(),
            branch: held.view.branch.clone(),
            run: Run::Idle,
        }),
        Item::Held(held) => Choice::Detail(held.view.key.clone()),
        Item::Queue(row) if row.eligible => Choice::Start(StartCard {
            key: row.key.clone(),
            title: row.title.clone(),
            info: None,
            run: Run::Idle,
        }),
        Item::Queue(row) => {
            let reason = if row.reason.trim().is_empty() {
                "the tracker gave no reason".to_string()
            } else {
                row.reason.trim().trim_start_matches("no: ").to_string()
            };
            Choice::Cannot(format!("{} cannot start: {reason}", row.key))
        }
    };
    match choice {
        Choice::Resume(card) => app.card = Some(Card::Resume(card)),
        Choice::Detail(key) => super::live::open(app, &key, fx),
        Choice::Start(card) => {
            fx.push(Effect::LoadStartInfo(card.key.clone()));
            app.card = Some(Card::Start(card));
        }
        Choice::Cannot(text) => notice(app, text, Tone::Info),
    }
}
