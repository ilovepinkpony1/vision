//! The keys of Home.
//!
//! `Tab` opens the next tab, and `1`, `2`, and `3` open the tabs "In progress", "To take", and
//! "Ready to merge". The arrows and `k`/`j` move through the rows of the open tab; Enter opens
//! the live view of a ticket in progress,
//! opens the start card of a ticket that can start, opens the start card of a ticket that is in
//! progress outside Vision (the continue card with [`Outside`]), says why another ticket cannot
//! start, and opens the continue card of a stopped or paused ticket; `n` opens the card of the
//! first thing that waits; `m` opens
//! Models and `s` opens Stats; `p` pauses the engine; `r` loads the queue again; `K` opens the
//! kill switch card; `?` shows the keys; `q` and Ctrl-C leave.
//!
//! A click on a ticket selects it, and a click on the selected ticket opens it as Enter does.

use super::super::model::{
    App, Card, HomeTab, Item, KillCard, Outside, Run, Screen, StartCard, Tone,
};
use super::super::msg::{Effect, Key};
use super::{ask_quit, data, engine, models, needs, notice, resume, stats};

pub fn on_key(app: &mut App, key: Key, fx: &mut Vec<Effect>) {
    match key {
        Key::Up | Key::Char('k') => step(app, -1),
        Key::Down | Key::Char('j') => step(app, 1),
        Key::Enter => open(app, fx),
        Key::Tab => open_tab(app, app.home.tab.next()),
        Key::Char(c @ '1'..='3') => open_tab(app, HomeTab::ALL[(c as u8 - b'1') as usize]),
        _ => common(app, key, fx),
    }
}

/// Opens a tab of Home. The selection stays when its ticket is in that tab; else the first row
/// of the tab is selected.
pub fn open_tab(app: &mut App, tab: HomeTab) {
    app.home.tab = tab;
    app.home.chosen = true;
    let keys: Vec<String> = app.items().iter().map(|i| i.key().to_string()).collect();
    let stays = app
        .home
        .selected
        .as_ref()
        .is_some_and(|key| keys.contains(key));
    if !stays {
        app.home.selected = keys.first().cloned();
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
        Outside(String, String, Outside),
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
        Item::Queue(row) if row.outside() => Choice::Outside(
            row.key.clone(),
            row.title.clone(),
            Outside {
                branch: row.branch.clone().unwrap_or_default(),
                pr: row.pr,
                status: row.status.clone(),
            },
        ),
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
        Choice::Outside(key, title, outside) => {
            resume::open_outside(app, &key, &title, outside, fx)
        }
        Choice::Cannot(text) => notice(app, text, Tone::Info),
    }
}
