//! The keys of Home.
//!
//! The arrows and `k`/`j` move through both boxes; Enter opens the live view of a ticket in progress,
//! opens the start card of a ticket that can start, says why another ticket cannot start, and
//! opens the continue card of a stopped or paused ticket; `n` opens the card of the first thing that waits; `m` opens
//! Models and `s` opens Stats; `p` pauses the engine; `r` loads the queue again; `K` opens the
//! kill switch card; `?` shows the keys; `q` and Ctrl-C leave.
//!
//! A click on a ticket selects it, and a click on the selected ticket opens it as Enter does.

use super::super::model::{App, Card, Item, KillCard, Run, Screen, StartCard, Tone};
use super::super::msg::{Effect, Key};
use super::{ask_quit, data, engine, models, needs, notice, resume, stats};

pub fn on_key(app: &mut App, key: Key, fx: &mut Vec<Effect>) {
    match key {
        Key::Up | Key::Char('k') => step(app, -1),
        Key::Down | Key::Char('j') => step(app, 1),
        Key::Enter => open(app, fx),
        _ => common(app, key, fx),
    }
}

/// A click on the ticket `key`: the first click selects it, and a click on the selected ticket
/// opens it as Enter does.
pub fn click_row(app: &mut App, key: &str, fx: &mut Vec<Effect>) {
    if app.screen != Screen::Home || app.card.is_some() {
        return;
    }
    if app.home.selected.as_deref() == Some(key) {
        open(app, fx);
    } else if app.items().iter().any(|i| i.key() == key) {
        app.home.selected = Some(key.to_string());
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
        Continue(String),
        Detail(String),
        Start(Box<StartCard>),
        Cannot(String),
    }
    let choice = match &app.items()[index] {
        Item::Held(held) if resume::continues(&held.view.flags) => {
            Choice::Continue(held.view.key.clone())
        }
        Item::Held(held) => Choice::Detail(held.view.key.clone()),
        Item::Queue(row) if row.eligible => Choice::Start(Box::new(StartCard {
            key: row.key.clone(),
            title: row.title.clone(),
            info: None,
            run: Run::Idle,
            since: app.clock.elapsed,
        })),
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
        Choice::Continue(key) => resume::open_held(app, &key, fx),
        Choice::Detail(key) => super::live::open(app, &key, fx),
        Choice::Start(card) => {
            fx.push(Effect::LoadStartInfo(card.key.clone()));
            app.card = Some(Card::Start(*card));
        }
        Choice::Cannot(text) => notice(app, text, Tone::Info),
    }
}
