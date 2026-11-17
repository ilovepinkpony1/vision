//! The continue card: Enter on a stopped or paused ticket, on Home, in the live view, on the
//! card of the things that wait, and on the card after a session. It names the stored step,
//! and a thread reads what the `[[resume]]` rules of the adapter find, with their one call of
//! observe. Then the card lists the steps of the path with the suggestion selected and the
//! stored step marked.
//!
//! Up and Down, `k` and `j`, the wheel, and a click select a step. Enter continues at the
//! selected step with the chain of [`recipes::resume`], and a click on the selected step acts
//! as Enter on its second click. Esc closes the card, and the ticket stays as it is. `v` closes
//! the card and opens the live view of the ticket.
//!
//! The same card starts a ticket that is in progress outside Vision: Enter on Home opens it
//! with the branch and the pull request of the queue, and its read applies the rules to the
//! facts of that branch. It has no stored step and no live view, and Enter takes the ticket
//! at the selected step with the chain of [`recipes::start_at`].

use super::super::chain::{Chain, Owner};
use super::super::model::{App, Card, ContinueCard, ContinueView, Outside, Screen};
use super::super::msg::{Effect, Key};
use super::super::recipes;
use super::chain;
use crate::pipeline::state::{PAUSED, STOPPED};

/// Opens the continue card of a ticket at the step `stored`, and starts its read.
pub fn open(
    app: &mut App,
    key: &str,
    title: &str,
    stored: &str,
    stopped: bool,
    fx: &mut Vec<Effect>,
) {
    chain::drop_ended(app);
    let since = app.clock.elapsed;
    app.card = Some(Card::Continue(ContinueCard::new(
        key, title, stored, stopped, since,
    )));
    app.dirty = true;
    fx.push(Effect::LoadResume(key.to_string()));
}

/// Opens the start card of a ticket that is in progress outside Vision, and starts its read.
pub fn open_outside(
    app: &mut App,
    key: &str,
    title: &str,
    outside: Outside,
    fx: &mut Vec<Effect>,
) {
    chain::drop_ended(app);
    let since = app.clock.elapsed;
    fx.push(load(key, Some(&outside)));
    app.card = Some(Card::Continue(ContinueCard::outside(
        key, title, outside, since,
    )));
    app.dirty = true;
}

/// The read of the card: the facts of a ticket that Vision holds, or of a ticket that is in
/// progress outside Vision.
fn load(key: &str, outside: Option<&Outside>) -> Effect {
    match outside {
        Some(outside) => Effect::LoadOutside {
            key: key.to_string(),
            branch: outside.branch.clone(),
            pr: outside.pr,
        },
        None => Effect::LoadResume(key.to_string()),
    }
}

/// Opens the card of a ticket in progress with what the local data knows of it. The read of
/// the card gives the flags of the ledger.
pub fn open_held(app: &mut App, key: &str, fx: &mut Vec<Effect>) {
    let (title, stored, stopped) = match app.held_ticket(key) {
        Some(held) => (
            app.title_of(held).to_string(),
            held.view.step.clone().unwrap_or_default(),
            held.stopped,
        ),
        None => (String::new(), String::new(), false),
    };
    open(app, key, &title, &stored, stopped, fx);
}

/// True for a ticket that the card continues: stopped or paused, by its flags.
pub fn continues(flags: &[crate::pipeline::view::FlagView]) -> bool {
    flags.iter().any(|f| f.flag == STOPPED || f.flag == PAUSED)
}

fn card_mut<'a>(app: &'a mut App, key: &str) -> Option<&'a mut ContinueCard> {
    match app.card.as_mut() {
        Some(Card::Continue(card)) if card.key == key => Some(card),
        _ => None,
    }
}

/// The chain of the card failed: the card reads the ticket again, because a step of the chain
/// can have changed its flags. Enter waits for that read.
pub fn reload(app: &mut App, key: &str, fx: &mut Vec<Effect>) {
    let now = app.clock.elapsed;
    let Some(card) = card_mut(app, key) else {
        return;
    };
    card.view = None;
    card.since = now;
    let effect = load(key, card.outside.as_ref());
    app.dirty = true;
    fx.push(effect);
}

/// The read of the card ended: the suggestion is selected, else the stored step.
pub fn on_view(app: &mut App, key: &str, view: Result<ContinueView, String>) {
    let Some(card) = card_mut(app, key) else {
        return;
    };
    if let Ok(view) = &view {
        let pick = view.suggested().unwrap_or(&view.stored);
        let at = |step: &str| view.steps.iter().position(|s| s == step);
        card.selected = at(pick).or_else(|| at(&view.stored)).unwrap_or(0);
        card.stored = view.stored.clone();
        card.stopped = view.stopped;
    }
    card.view = Some(view);
    app.dirty = true;
}

pub fn on_key(app: &mut App, key: Key, fx: &mut Vec<Effect>) {
    let Some(Card::Continue(card)) = app.card.as_ref() else {
        return;
    };
    let runs = chain::of(app, &Owner::Resume(card.key.clone())).is_some_and(Chain::running);
    match key {
        Key::Esc => {
            app.card = None;
            chain::drop_ended(app);
        }
        Key::Char('v') => live_view(app, fx),
        _ if runs => {}
        Key::Up | Key::Char('k') => select(app, -1),
        Key::Down | Key::Char('j') => select(app, 1),
        Key::Enter => go(app),
        _ => {}
    }
}

/// `v`: closes the card and opens the live view of its ticket, or only closes the card over
/// that live view. A chain of the card goes on, as after Esc. A ticket that is in progress
/// outside Vision has no live view.
fn live_view(app: &mut App, fx: &mut Vec<Effect>) {
    let Some(Card::Continue(card)) = app.card.as_ref().filter(|_| !outside_card(app)) else {
        return;
    };
    let key = card.key.clone();
    app.card = None;
    chain::drop_ended(app);
    if !matches!(&app.screen, Screen::Live { key: k } if *k == key) {
        super::live::open(app, &key, fx);
    }
}

/// Moves the selection by `by` rows. A chain that ended goes, because it was about the step
/// of the last Enter.
fn select(app: &mut App, by: i32) {
    let Some(Card::Continue(card)) = app.card.as_mut() else {
        return;
    };
    let Some(len) = card.loaded().map(|v| v.steps.len()).filter(|n| *n > 0) else {
        return;
    };
    let next = (card.selected as i32 + by).clamp(0, len as i32 - 1) as usize;
    if next != card.selected {
        card.selected = next;
        chain::drop_ended(app);
    }
}

/// A click on the step of the row `i`: the first click selects it, as the arrows do. The view
/// records the selected step as the part of Enter, so a click here never continues.
pub fn click_pick(app: &mut App, i: usize) {
    let Some(Card::Continue(card)) = app.card.as_mut() else {
        return;
    };
    let in_list = card.loaded().is_some_and(|v| i < v.steps.len());
    if in_list && card.selected != i {
        card.selected = i;
        chain::drop_ended(app);
    }
}

/// True when the open card starts a ticket that is in progress outside Vision.
pub fn outside_card(app: &App) -> bool {
    matches!(&app.card, Some(Card::Continue(card)) if card.outside.is_some())
}

/// Enter: continues at the selected step, once the read worked. A ticket that is in progress
/// outside Vision is taken at that step.
fn go(app: &mut App) {
    let Some(Card::Continue(card)) = app.card.as_ref() else {
        return;
    };
    let (Some(view), Some(step)) = (card.loaded().cloned(), card.chosen().map(str::to_string))
    else {
        return;
    };
    let outside = card.outside.is_some();
    let id = app.next_seq();
    let chain = if outside {
        recipes::start_at(id, &view.key, &step)
    } else {
        recipes::resume(id, &view, &step)
    };
    chain::start(app, chain);
}
