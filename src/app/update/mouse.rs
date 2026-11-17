//! A click and a step of the wheel. The view records each part of a frame that the mouse can
//! hit, and the runtime keeps the parts of the last frame in the model ([`App::hits`]).
//!
//! A click acts on the part under it. Most parts act as a key, so a click and its key run the
//! same code. A row selects on the first click, and a click on the selected row opens it as
//! Enter does. A part of the question form, and a tab of the activity, act as their keys do
//! when no line is open. A step of the wheel over a part that scrolls acts as the up or the
//! down arrow, as many times as the part says.
//!
//! A part that sends something or records a decision acts only on a second click: the first
//! click arms it, and a second click on it within [`ARM_FOR`] acts as its key. A click elsewhere
//! and a step of the wheel take the arm away. A typed key and a paste take it away too, and the
//! key acts at once, as always.
//!
//! A click outside each part does nothing. A step of the wheel that moves nothing marks no
//! change, so it makes no redraw.

use super::super::hits::{card_place, Act, Armed, ARM_FOR};
use super::super::model::{App, Card};
use super::super::msg::{Effect, Key, Mouse, MouseKind};

pub fn on_mouse(app: &mut App, event: Mouse, fx: &mut Vec<Effect>) {
    match event.kind {
        MouseKind::Click => {
            let Some(act) = app.hits.click_at(event.column, event.row).cloned() else {
                disarm(app);
                return;
            };
            // A click on a part is an act of the person, as a key is.
            app.dirty = true;
            // A part that decides acts only when the first click armed it.
            if act.decides() && app.armed_act() != Some(&act) {
                app.armed = Some(Armed {
                    act,
                    until: app.clock.elapsed + ARM_FOR,
                    screen: app.screen.clone(),
                    card: app.card.as_ref().map(card_place),
                });
            } else {
                app.armed = None;
                run(app, act, fx);
            }
        }
        MouseKind::WheelUp | MouseKind::WheelDown => {
            let Some(by) = app.hits.wheel_at(event.column, event.row) else {
                return;
            };
            disarm(app);
            let key = if event.kind == MouseKind::WheelUp {
                Key::Up
            } else {
                Key::Down
            };
            let before = place(app);
            for _ in 0..by {
                super::on_key(app, key, fx);
            }
            if place(app) != before {
                app.dirty = true;
            }
        }
    }
}

/// What a part does.
fn run(app: &mut App, act: Act, fx: &mut Vec<Effect>) {
    match act {
        Act::Key(key) | Act::Decide(key) => super::on_key(app, key, fx),
        Act::Ticket(key) => super::home::click_row(app, &key, fx),
        Act::Agent(i) => super::models::click_row(app, i),
        Act::Line(i) => super::pager::click_line(app, i, fx),
        Act::Form(target) => super::needs::click(app, target),
        Act::Tab(filter) => super::live::click_tab(app, filter),
    }
}

/// Takes the arm away. Only a part that was armed makes a redraw, so that its hint shows its
/// label again.
pub fn disarm(app: &mut App) {
    if app.armed.take().is_some() {
        app.dirty = true;
    }
}

/// What the up and down arrows move under a part that scrolls: the selection of Home and of
/// Models, the end of a chain that Models shows, the scroll of the activity and of the pager on
/// top, and the scroll and the row of the card of the things that wait.
#[derive(PartialEq)]
struct Place {
    home: Option<String>,
    models: usize,
    chain: bool,
    feed: Option<(usize, bool)>,
    pager: Option<(usize, Option<usize>)>,
    card: Option<(usize, usize)>,
}

fn place(app: &App) -> Place {
    let live = app.live.as_ref();
    Place {
        home: app.home.selected.clone(),
        models: app.models.selected,
        chain: app.chain.is_some(),
        feed: live.map(|l| (l.feed.back, l.feed.follow)),
        pager: live
            .and_then(|l| l.pagers.last())
            .map(|p| (p.top, p.cursor)),
        card: match &app.card {
            Some(Card::Need(card)) => Some((card.scroll, card.row)),
            _ => None,
        },
    }
}
