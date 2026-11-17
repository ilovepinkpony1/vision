//! `n`: the card of the things that wait for the person. It shows the first thing that waits,
//! or the first thing of the ticket of the live view. The left and right arrows go to the
//! previous and the next one, the up and down arrows scroll a long card, and Esc closes the card
//! while the thing keeps waiting. After an answer, the next thing shows, or the card closes with
//! a notice.
//!
//! The questions of one asking id are one form. In a form, the left and right arrows first go
//! through its pages: each question, then the review page. From its first page they go to the
//! thing before it, and from its review page to the thing after it. On the review page, the up
//! and down arrows choose a row.
//!
//! The items load in a thread. Each load has a number, and only the result of the last load
//! counts, so that a load that started before an action never brings back what the action
//! ended. The card loads again when the needs of the local data change. The first load also
//! gives the drafts of the forms from their files; after it, the card keeps its own drafts. A
//! question that another path answered meanwhile leaves its form with a note, and its draft
//! goes.

use std::collections::BTreeMap;

use super::super::model::{App, Card, Tone};
use super::super::msg::{Effect, Key};
use super::super::needs::{Gone, Input, NeedItem, NeedsCard, Target};
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

pub fn on_needs(
    app: &mut App,
    load: u64,
    items: Result<Vec<NeedItem>, String>,
    fx: &mut Vec<Effect>,
) {
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
    let first = !card.drafts_read;
    if first {
        card.drafts_read = true;
        for set in items.iter().filter_map(NeedItem::set) {
            for (id, draft) in &set.saved {
                card.drafts
                    .entry(id.clone())
                    .or_insert_with(|| draft.clone());
            }
        }
    }
    let left = left_forms(card.items.as_deref(), &items);
    if !left.is_empty() {
        let mut by_ticket: BTreeMap<String, Vec<String>> = BTreeMap::new();
        for (ticket, id) in &left {
            if card.drafts.remove(id).is_some() {
                by_ticket
                    .entry(ticket.clone())
                    .or_default()
                    .push(id.clone());
            }
        }
        for (ticket, ids) in by_ticket {
            fx.push(Effect::DropDrafts { ticket, ids });
        }
        // The note names the questions that left the form that shows.
        let shown: Vec<&str> = card
            .set()
            .map(|set| set.cards.iter().map(|c| c.id.as_str()).collect())
            .unwrap_or_default();
        let ids: Vec<&str> = left
            .iter()
            .map(|(_, id)| id.as_str())
            .filter(|id| shown.contains(id))
            .collect();
        if !ids.is_empty() {
            card.note = Some(left_note(&ids));
        }
    }
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
    let question_before = card
        .question()
        .and_then(|i| card.set().map(|set| set.cards[i].id.clone()));
    let reviewing = card.reviewing();
    let want = card.select.take().or(before.clone());
    let at = want
        .and_then(|id| items.iter().position(|i| i.id() == id))
        .unwrap_or_else(|| card.at.min(items.len() - 1));
    card.items = Some(items);
    card.at = at;
    let now = card.item().map(NeedItem::id);
    if now != before || first {
        card.input = Input::None;
        card.scroll = 0;
        card.page = card.set().map_or(0, |set| set.first_page(&card.drafts));
        card.row = card
            .set()
            .and_then(|set| set.missing(&card.drafts).first().copied())
            .unwrap_or(0);
    } else if let Some(set) = card.set().cloned() {
        // The same form: the same question stays, or the same kind of page.
        let found = question_before.and_then(|id| set.cards.iter().position(|c| c.id == id));
        let page = match found {
            Some(i) => i,
            None if reviewing => set.review(),
            None => card.page.min(set.review()),
        };
        if page != card.page {
            card.input = Input::None;
            card.scroll = 0;
        }
        card.page = page;
        card.row = card.row.min(set.len().saturating_sub(1));
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
    if first {
        note_gone(app, fx);
    }
    app.dirty = true;
}

/// The questions of the forms of the old items that no new item has: the ticket and the id of
/// each one.
fn left_forms(old: Option<&[NeedItem]>, new: &[NeedItem]) -> Vec<(String, String)> {
    let ids_of = |items: &[NeedItem]| -> Vec<(String, String)> {
        items
            .iter()
            .filter_map(NeedItem::set)
            .flat_map(|set| set.cards.iter().map(|c| (set.ticket.clone(), c.id.clone())))
            .collect()
    };
    let now = ids_of(new);
    ids_of(old.unwrap_or_default())
        .into_iter()
        .filter(|old| !now.contains(old))
        .collect()
}

/// The note when questions leave a form.
fn left_note(ids: &[&str]) -> String {
    match ids {
        [one] => format!("{one} left the form: the Judge or another command answered it."),
        many => format!(
            "{} left the form: the Judge or another command answered them.",
            many.join(", ")
        ),
    }
}

/// At the first load: the drafts of the files whose questions have a final answer go, with a
/// note that says who answered them.
fn note_gone(app: &mut App, fx: &mut Vec<Effect>) {
    let Some(card) = card(app) else {
        return;
    };
    let mut gone: Vec<(String, Gone)> = Vec::new();
    for set in card.items.iter().flatten().filter_map(NeedItem::set) {
        if !set.gone.is_empty() {
            gone.extend(set.gone.iter().cloned());
            fx.push(Effect::DropDrafts {
                ticket: set.ticket.clone(),
                ids: set.gone.iter().map(|(id, _)| id.clone()).collect(),
            });
        }
    }
    if !gone.is_empty() && card.note.is_none() {
        let parts: Vec<String> = [Gone::Judge, Gone::Person, Gone::Other]
            .into_iter()
            .filter_map(|who| {
                let ids: Vec<&str> = gone
                    .iter()
                    .filter(|(_, by)| *by == who)
                    .map(|(id, _)| id.as_str())
                    .collect();
                (!ids.is_empty()).then(|| gone_note(&ids, who))
            })
            .collect();
        card.note = Some(parts.join(" "));
    }
}

/// The note about the drafts of the questions `ids` that `who` answered.
fn gone_note(ids: &[&str], who: Gone) -> String {
    match (ids, who) {
        ([one], Gone::Judge) => {
            format!("{one} got an answer from the Judge, so its draft is gone.")
        }
        (many, Gone::Judge) => format!(
            "{} got answers from the Judge, so their drafts are gone.",
            many.join(", ")
        ),
        ([one], Gone::Person) => format!("{one} got your answer, so its draft is gone."),
        (many, Gone::Person) => format!(
            "{} got your answers, so their drafts are gone.",
            many.join(", ")
        ),
        ([one], Gone::Other) => format!("{one} no longer waits, so its draft is gone."),
        (many, Gone::Other) => format!(
            "{} no longer wait, so their drafts are gone.",
            many.join(", ")
        ),
    }
}

/// The hash of the plan of a plan card.
fn plan_hash_of(item: &NeedItem) -> Option<String> {
    match item {
        NeedItem::Plan(plan) => Some(plan.plan.hash.clone()),
        _ => None,
    }
}

/// The send of a form ended well: its drafts go, from the card and from the drafts file of
/// the ticket, also when the card is closed.
pub fn sent(app: &mut App, ticket: &str, ids: Vec<String>, fx: &mut Vec<Effect>) {
    if let Some(card) = card(app) {
        for id in &ids {
            card.drafts.remove(id);
        }
    }
    fx.push(Effect::DropDrafts {
        ticket: ticket.to_string(),
        ids,
    });
}

/// A chain of an item ended well: the item goes away, and the next one shows, or the card
/// closes with the notice. A proposal stays until the next load says that no change of it
/// waits.
pub fn done(app: &mut App, item: &str, text: &str, tone: Tone, fx: &mut Vec<Effect>) {
    notice(app, text.to_string(), tone);
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
    card.page = card.set().map_or(0, |set| set.first_page(&card.drafts));
    card.row = 0;
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
    let review = match app.card.as_ref() {
        Some(Card::Need(card)) if card.reviewing() => card.set().cloned(),
        _ => None,
    };
    match key {
        Key::Esc => {
            app.card = None;
            chain::drop_ended(app);
        }
        Key::Left => step(app, -1),
        Key::Right => step(app, 1),
        Key::Up | Key::Down if review.is_some() => {
            if let Some(set) = &review {
                ask::move_row(app, set, if key == Key::Up { -1 } else { 1 });
            }
        }
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
        _ => item_key(app, key, fx),
    }
}

/// A click on a part of the question form that shows. While the chain of the form runs, it does
/// nothing, as the keys of the form do nothing then.
pub fn click(app: &mut App, target: Target) {
    let Some(card) = card(app) else {
        return;
    };
    let Some(set) = card.set().cloned() else {
        return;
    };
    let id = card.item().map(NeedItem::id);
    let runs = id
        .and_then(|id| chain::of(app, &super::super::chain::Owner::Need(id)))
        .is_some_and(|c| c.running());
    if !runs {
        ask::on_click(app, &set, target);
    }
}

/// A key for the item that shows.
fn item_key(app: &mut App, key: Key, fx: &mut Vec<Effect>) {
    let Some(item) = card(app).and_then(|c| c.item().cloned()) else {
        return;
    };
    match &item {
        NeedItem::Questions(set) => ask::on_key(app, set, key),
        other => act::on_key(app, other, key, fx),
    }
}

/// Moves to the previous or the next page of a form, or to the previous or the next item. A
/// form shows its first page when the arrows come from the item before it, and its review page
/// when they come from the item after it.
fn step(app: &mut App, by: i64) {
    let Some(card) = card(app) else {
        return;
    };
    if let Some(set) = card.set().cloned() {
        let page = card.page as i64 + by;
        if (0..=set.review() as i64).contains(&page) {
            ask::open_page(app, &set, page as usize);
            return;
        }
    }
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
        card.page = match card.set() {
            Some(set) if by < 0 => set.review(),
            _ => 0,
        };
        card.row = 0;
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
