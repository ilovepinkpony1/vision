//! `n`: the card of the things that wait for the person. It shows the first thing that waits,
//! or the first thing of the ticket of the live view. The left and right arrows go to the
//! previous and the next one, the up and down arrows scroll a long card, and Esc closes the card
//! while the thing keeps waiting. After an answer, the next thing shows, or the card closes with
//! a notice.
//!
//! The items load in a thread. Each load has a number, and only the result of the last load
//! counts, so that a load that started before an action never brings back what the action
//! ended. The card loads again when the needs of the local data change.

use super::super::model::{App, Card, Tone};
use super::super::msg::{Effect, Key};
use super::super::needs::{Input, NeedItem, NeedsCard};
use super::super::view::need_card::body_size;
use super::{act, ask, ask_quit, chain, notice};

/// Opens the card: of each thing that waits, or only of the things of one ticket.
pub fn open(app: &mut App, scope: Option<String>, fx: &mut Vec<Effect>) {
    let waits = app
        .needs()
        .iter()
        .any(|n| scope.is_none() || n.ticket == scope);
    if !waits {
        notice(app, "Nothing waits for you.", Tone::Info);
        return;
    }
    chain::drop_ended(app);
    let load = app.next_seq();
    app.card = Some(Card::Need(NeedsCard::new(scope, load)));
    fx.push(Effect::LoadNeeds(load));
}

fn card(app: &mut App) -> Option<&mut NeedsCard> {
    match app.card.as_mut() {
        Some(Card::Need(card)) => Some(card),
        _ => None,
    }
}

/// Loads the items again while the card is open.
pub fn reload_if_open(app: &mut App, fx: &mut Vec<Effect>) {
    if !matches!(app.card, Some(Card::Need(_))) {
        return;
    }
    let load = app.next_seq();
    if let Some(card) = card(app) {
        card.load = load;
    }
    fx.push(Effect::LoadNeeds(load));
}

/// The needs of the local data changed: the open card loads its items again, unless the person
/// types on it or its chain runs.
pub fn on_local_changed(app: &mut App, fx: &mut Vec<Effect>) {
    let busy = app.chain.as_ref().is_some_and(|c| c.running());
    let typing = matches!(&app.card, Some(Card::Need(card)) if card.input != Input::None);
    if !busy && !typing {
        reload_if_open(app, fx);
    }
}

pub fn on_needs(app: &mut App, load: u64, items: Result<Vec<NeedItem>, String>) {
    let Some(card) = card(app).filter(|c| c.load == load) else {
        return;
    };
    let items = match items {
        Ok(items) => items,
        Err(error) => {
            card.error = Some(error);
            app.dirty = true;
            return;
        }
    };
    card.error = None;
    let items: Vec<NeedItem> = items
        .into_iter()
        .filter(|i| card.scope.is_none() || i.ticket() == card.scope.as_deref())
        .collect();
    if items.is_empty() {
        let text = match &card.scope {
            Some(key) => format!("Nothing of {key} waits for you now."),
            None => "Nothing waits for you now.".to_string(),
        };
        let note = card.note.take();
        app.card = None;
        chain::drop_ended(app);
        notice(app, note.unwrap_or(text), Tone::Info);
        return;
    }
    let before = card.item().map(NeedItem::id);
    let plan_before = card.item().and_then(plan_hash_of);
    let want = card.select.take().or(before.clone());
    let at = want
        .and_then(|id| items.iter().position(|i| i.id() == id))
        .unwrap_or_else(|| card.at.min(items.len() - 1));
    card.items = Some(items);
    card.at = at;
    let now = card.item().map(NeedItem::id);
    if now != before {
        card.input = Input::None;
        card.scroll = 0;
    } else if plan_before.is_some() && card.item().and_then(plan_hash_of) != plan_before {
        // The same card now shows another plan: the person must not approve it unread.
        card.scroll = 0;
        if card.note.is_none() {
            card.note = Some(
                "The plan changed while this card was open. Read the new plan, then press a."
                    .into(),
            );
        }
    }
    app.dirty = true;
}

/// The hash of the plan of a plan card.
fn plan_hash_of(item: &NeedItem) -> Option<String> {
    match item {
        NeedItem::Plan(plan) => Some(plan.plan.hash.clone()),
        _ => None,
    }
}

/// A chain of an item ended well: the item goes away, and the next one shows, or the card
/// closes with the notice. A proposal stays until the next load says that no change of it
/// waits.
pub fn done(app: &mut App, item: &str, text: &str, fx: &mut Vec<Effect>) {
    notice(app, text.to_string(), Tone::Ok);
    let keep = item.starts_with("proposal:");
    let Some(card) = card(app) else {
        return;
    };
    if let Some(items) = card.items.as_mut() {
        if !keep {
            items.retain(|i| i.id() != item);
        }
        if items.is_empty() {
            app.card = None;
            return;
        }
        card.at = card.at.min(items.len() - 1);
    }
    card.input = Input::None;
    card.scroll = 0;
    card.note = None;
    reload_if_open(app, fx);
}

/// A check found that the thing changed after the card showed it: the card loads again, shows
/// the item `select`, and says what happened.
pub fn changed(app: &mut App, select: Option<String>, note: String, fx: &mut Vec<Effect>) {
    match card(app) {
        Some(card) => {
            card.select = select;
            card.note = Some(note);
            card.input = Input::None;
            card.scroll = 0;
            reload_if_open(app, fx);
        }
        None => notice(app, format!("{note} Press n to see it."), Tone::Info),
    }
}

pub fn on_paste(app: &mut App, text: &str) {
    if let Some(card) = card(app) {
        match &mut card.input {
            Input::Words(line) | Input::Why { line, .. } => line.paste(text),
            _ => return,
        }
        app.dirty = true;
    }
}

pub fn on_key(app: &mut App, key: Key, fx: &mut Vec<Effect>) {
    let Some(card) = card(app) else {
        return;
    };
    if card.input != Input::None {
        ask::on_input(app, key, fx);
        return;
    }
    let runs = card
        .item()
        .map(|i| i.id())
        .and_then(|id| chain::of(app, &super::super::chain::Owner::Need(id)))
        .is_some_and(|c| c.running());
    match key {
        Key::Esc => {
            app.card = None;
            chain::drop_ended(app);
        }
        Key::Left => step(app, -1),
        Key::Right => step(app, 1),
        Key::Up => scroll(app, -1),
        Key::Down => scroll(app, 1),
        Key::PageUp => scroll(app, -page(app)),
        Key::PageDown => scroll(app, page(app)),
        Key::Home => scroll(app, i64::MIN / 2),
        Key::End => scroll(app, i64::MAX / 2),
        Key::Char('q') => {
            app.card = None;
            ask_quit(app, fx);
        }
        _ if runs => {}
        _ => item_key(app, key),
    }
}

/// A key for the item that shows.
fn item_key(app: &mut App, key: Key) {
    let Some(item) = card(app).and_then(|c| c.item().cloned()) else {
        return;
    };
    match &item {
        NeedItem::Question(card) => ask::on_key(app, card, key),
        other => act::on_key(app, other, key),
    }
}

/// Moves to the previous or the next item.
fn step(app: &mut App, by: i64) {
    let Some(card) = card(app) else {
        return;
    };
    let len = card.len() as i64;
    if len == 0 {
        return;
    }
    let at = (card.at as i64 + by).clamp(0, len - 1) as usize;
    if at != card.at {
        card.at = at;
        card.input = Input::None;
        card.scroll = 0;
        card.note = None;
        chain::drop_ended(app);
    }
}

fn page(app: &App) -> i64 {
    (body_size(app).1 as i64 - 1).max(1)
}

/// Scrolls the body of a long card.
fn scroll(app: &mut App, by: i64) {
    let (total, visible) = body_size(app);
    let max = total.saturating_sub(visible) as i64;
    if let Some(card) = card(app) {
        card.scroll = (card.scroll as i64 + by).clamp(0, max) as usize;
    }
}
