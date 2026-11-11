//! The update: a function that takes a message, changes the model, and gives the effects to
//! run. It does no input or output and reads no clock: the time is [`App::clock`], which the
//! runtime sets before each message.
//!
//! After each message, the update runs the timers (the refresh of the local data each second,
//! the queue each 5 minutes, a pass each minute) and hands the next job to the worker when the
//! worker is free.

mod act;
mod ask;
mod boot;
mod cards;
pub mod chain;
mod data;
pub mod engine;
mod home;
pub mod live;
mod models;
mod needs;
mod pager;
mod stats;
mod tell;

use std::time::Duration;

use super::model::{App, Notice, Tone};
use super::msg::{Effect, Key, Msg};

/// The local data loads again after this time.
pub const LOCAL_EVERY: Duration = Duration::from_secs(1);
/// The queue of the tracker loads again after this time.
pub const QUEUE_EVERY: Duration = Duration::from_secs(300);
/// How long a notice stays.
pub const NOTICE_FOR: Duration = Duration::from_secs(4);

/// The first effects of the app: the loader reads the home folder.
pub fn start(app: &mut App) -> Vec<Effect> {
    boot::start(app)
}

/// Changes the model by one message and gives the effects to run.
pub fn update(app: &mut App, msg: Msg) -> Vec<Effect> {
    let mut fx = Vec::new();
    match msg {
        Msg::Tick | Msg::Signal(_) => {}
        Msg::Resize { width, height } => {
            app.size = (width, height);
            app.dirty = true;
        }
        Msg::Key(key) => {
            app.dirty = true;
            on_key(app, key, &mut fx);
        }
        Msg::Paste(text) => {
            tell::on_paste(app, &text);
            needs::on_paste(app, &text);
        }
        Msg::Ticket { key, data } => live::on_ticket(app, &key, *data),
        Msg::Stream { key, batch } => live::on_stream(app, &key, &batch),
        Msg::Plan { key, plan } => pager::on_plan(app, &key, plan),
        Msg::Diff { key, diff } => pager::on_diff(app, &key, diff),
        Msg::TurnEvents { key, turn, events } => pager::on_turn(app, &key, &turn, events),
        Msg::SessionEnded { key, result } => {
            if !chain::on_session_ended(app, &key, &result) {
                cards::on_session_ended(app, &key, &result);
            }
        }
        Msg::ReviewEnded { proposal, result } => {
            app.dirty = true;
            chain::on_review_ended(app, &proposal, &result, &mut fx);
        }
        Msg::Needs { load, items } => needs::on_needs(app, load, *items),
        Msg::Checked { chain: id, facts } => chain::on_checked(app, id, *facts, &mut fx),
        Msg::Models(data) => models::on_models(app, *data, &mut fx),
        Msg::Stats { week, data } => stats::on_stats(app, week, *data, &mut fx),
        Msg::Picture(picture) => {
            app.picture = Some(*picture);
            app.dirty = true;
        }
        Msg::Setup(result) => boot::on_setup(app, result, &mut fx),
        Msg::Local(result) => data::on_local(app, result, &mut fx),
        Msg::Queue(result) => data::on_queue(app, result, &mut fx),
        Msg::StartInfo { key, info } => cards::on_start_info(app, &key, info),
        Msg::JobDone(done) => engine::on_done(app, done, &mut fx),
    }
    boot::leave_when_ready(app);
    timers(app, &mut fx);
    chain::advance(app, &mut fx);
    engine::dispatch(app, &mut fx);
    fx
}

fn on_key(app: &mut App, key: Key, fx: &mut Vec<Effect>) {
    // Ctrl-C cancels the line of `t` and the lines of a card, and is `q` everywhere else.
    let typing = matches!(&app.card, Some(super::model::Card::Need(card)) if card.input != super::needs::Input::None);
    let telling =
        typing || app.card.is_none() && app.live.as_ref().is_some_and(|l| l.tell.is_some());
    let key = match key {
        Key::Cancel if telling => Key::Esc,
        Key::Cancel => Key::Char('q'),
        other => other,
    };
    if app.card.is_some() {
        cards::on_key(app, key, fx);
        return;
    }
    match app.screen {
        super::model::Screen::Boot => boot::on_key(app, key, fx),
        super::model::Screen::Home => home::on_key(app, key, fx),
        super::model::Screen::Live { .. } => live::on_key(app, key, fx),
        super::model::Screen::Models => models::on_key(app, key, fx),
        super::model::Screen::Stats => stats::on_key(app, key, fx),
    }
}

fn timers(app: &mut App, fx: &mut Vec<Effect>) {
    let now = app.clock.elapsed;
    if app.notice.as_ref().is_some_and(|n| n.until <= now) {
        app.notice = None;
        app.dirty = true;
    }
    if app.setup.is_some() {
        if !app.local.loading() && app.local.at.is_some_and(|at| now >= at + LOCAL_EVERY) {
            data::load_local(app, fx);
        }
        if !app.queue.loading() && app.queue.at.is_some_and(|at| now >= at + QUEUE_EVERY) {
            data::load_queue(app, fx);
        }
    }
    live::timer(app, fx);
    engine::timer(app);
}

/// Shows a notice for a few seconds.
pub fn notice(app: &mut App, text: impl Into<String>, tone: Tone) {
    app.notice = Some(Notice {
        text: text.into(),
        tone,
        until: app.clock.elapsed + NOTICE_FOR,
    });
    app.dirty = true;
}

/// Leaves the app at once.
fn leave(app: &mut App, fx: &mut Vec<Effect>) {
    app.quit = true;
    fx.push(Effect::Quit);
}

/// `q`: leaves at once when no ticket moves, else opens the quit card.
fn ask_quit(app: &mut App, fx: &mut Vec<Effect>) {
    let moving = app.held().iter().filter(|h| !h.stopped).count();
    if moving == 0 {
        leave(app, fx);
    } else {
        app.card = Some(super::model::Card::Quit);
    }
}
