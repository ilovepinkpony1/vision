//! The back and the forward parts of the header, and the names of the header that go to a
//! place. The update goes to a place of the history ([`crate::app::nav`]) as the keys of the
//! screens go there: leaving the live view stops the reader of its stream files, leaving Models
//! drops each change that is not pinned yet, and each pager of the place opens again with the
//! load of its text.

use super::super::model::{App, Screen, Tone};
use super::super::msg::Effect;
use super::super::nav::Place;
use super::super::pager::PagerKind;
use super::{chain, live, models, notice, stats};

/// Notes the place that shows after a message: after a key or a click, a new place is a visit,
/// and each other message only corrects the place.
pub fn note(app: &mut App, by_person: bool) {
    let now = app.place();
    if by_person {
        app.nav.visit(now);
    } else {
        app.nav.correct(now);
    }
}

/// The back part of the header: the place before this one.
pub fn back(app: &mut App, fx: &mut Vec<Effect>) {
    if let Some(place) = app.nav.back() {
        go(app, place, fx);
    }
}

/// The forward part of the header: the place that back left.
pub fn forward(app: &mut App, fx: &mut Vec<Effect>) {
    if let Some(place) = app.nav.forward() {
        go(app, place, fx);
    }
}

/// Goes to a place. A card takes each click, so no part of the header acts under a card.
pub fn go(app: &mut App, place: Place, fx: &mut Vec<Effect>) {
    if app.card.is_some() || app.place().as_ref() == Some(&place) {
        return;
    }
    if app.screen == Screen::Models && place != Place::Models {
        leave_models(app);
    }
    match place {
        Place::Home => {
            close_live(app, fx);
            app.screen = Screen::Home;
        }
        Place::Models => {
            close_live(app, fx);
            if app.screen != Screen::Models {
                models::open(app, fx);
            }
        }
        Place::Stats => {
            close_live(app, fx);
            if app.screen != Screen::Stats {
                stats::open(app, fx);
            }
        }
        Place::Live { key, pagers } => {
            let open = matches!(&app.screen, Screen::Live { key: k } if *k == key)
                && app.live.as_ref().is_some_and(|l| l.key == key);
            if !open {
                live::open(app, &key, fx);
            }
            // The pagers that the place shares with the view stay, with their positions.
            let shown: Vec<PagerKind> = app
                .live
                .as_ref()
                .map(|l| l.pagers.iter().map(|p| p.kind.clone()).collect())
                .unwrap_or_default();
            let same = shown
                .iter()
                .zip(&pagers)
                .take_while(|(a, b)| a == b)
                .count();
            if let Some(live) = app.live.as_mut() {
                live.pagers.truncate(same);
            }
            for kind in &pagers[same..] {
                live::reopen(app, kind, fx);
            }
        }
    }
}

/// Leaves the live view, and stops the reader of its stream files.
fn close_live(app: &mut App, fx: &mut Vec<Effect>) {
    if app.live.take().is_some() {
        fx.push(Effect::Follow(None));
    }
}

/// Leaves Models as Esc does: a change that is not pinned goes, with a notice.
fn leave_models(app: &mut App) {
    if !app.models.edits.is_empty() {
        app.models.edits.clear();
        notice(app, "Dropped each change that was not pinned.", Tone::Info);
    }
    chain::drop_ended(app);
}
