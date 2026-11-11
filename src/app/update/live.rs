//! The keys and the data of the live view of a ticket.
//!
//! Esc goes back to Home. `n` lists the needs, `t` opens the line to tell the agent something,
//! `o` opens the session, `l` the log, `p` the plan, `d` the commits that the forge does not
//! have, `x` stops the ticket, and `r` retries it when it waits for the person with a flag or
//! is paused. Enter on a stopped ticket opens the resume card. The arrows, `k`, `j`, PageUp,
//! and PageDown scroll the activity; `G` and End go back to its end. `?` shows the keys, and
//! `q` quits as on Home.
//!
//! While the view is open, its data loads again each second, and a thread follows the stream
//! files of the ticket.

use std::time::Duration;

use super::super::live::{Live, TicketData};
use super::super::model::{
    App, Card, OpenCard, ResumeCard, RetryCard, Run, Screen, StopCard, Tone,
};
use super::super::moments::moment;
use super::super::msg::{Effect, Key};
use super::super::now::Kind;
use super::super::pager::{Pager, PagerKind};
use super::super::view::live::layout::Layout;
use super::{ask_quit, notice, pager, tell};
use crate::pipeline::state::{NEEDS_YOU, PAUSED, STOPPED};
use crate::util::time as vtime;

/// The data of the live view loads again after this time.
pub const LIVE_EVERY: Duration = Duration::from_secs(1);

/// Opens the live view of a ticket.
pub fn open(app: &mut App, key: &str, fx: &mut Vec<Effect>) {
    app.screen = Screen::Live {
        key: key.to_string(),
    };
    let mut live = Live::new(key);
    live.data.since = Some(app.clock.elapsed);
    app.live = Some(live);
    fx.push(Effect::LoadTicket(key.to_string()));
    fx.push(Effect::Follow(Some(key.to_string())));
}

/// Leaves the live view for Home, and stops the reader of its stream files.
pub fn close(app: &mut App, fx: &mut Vec<Effect>) {
    app.screen = Screen::Home;
    app.live = None;
    fx.push(Effect::Follow(None));
}

/// Loads the data again each second while the view is open.
pub fn timer(app: &mut App, fx: &mut Vec<Effect>) {
    let now = app.clock.elapsed;
    let Some(live) = app.live.as_mut() else {
        return;
    };
    if !live.data.loading() && live.data.at.is_some_and(|at| now >= at + LIVE_EVERY) {
        live.data.since = Some(now);
        fx.push(Effect::LoadTicket(live.key.clone()));
    }
}

/// The lines of the activity at the size of the window.
pub fn feed_lines(app: &App) -> usize {
    let Some(live) = &app.live else {
        return 0;
    };
    let layout = Layout::of(i32::from(app.size.0), i32::from(app.size.1));
    live.feed
        .lines(layout.feed_text_w(), app.clock.offset_seconds)
        .len()
}

pub fn on_ticket(app: &mut App, key: &str, data: Result<TicketData, String>) {
    let before = feed_lines(app);
    let Some(live) = app.live.as_mut().filter(|l| l.key == key) else {
        return;
    };
    live.data.since = None;
    live.data.at = Some(app.clock.elapsed);
    match data {
        Ok(data) => {
            let moments: Vec<_> = data
                .timeline
                .iter()
                .map(|entry| (vtime::parse_rfc3339(&entry.at).ok(), moment(entry)))
                .collect();
            let changed = live.feed.set_moments(&moments);
            if live.data.data.as_ref() != Some(&data) || live.data.error.is_some() {
                app.dirty = true;
            }
            live.data.data = Some(data);
            live.data.error = None;
            if changed {
                app.dirty = true;
                let added = feed_lines(app).saturating_sub(before);
                if let Some(live) = app.live.as_mut() {
                    live.feed.keep_place(added);
                }
            }
        }
        Err(error) => {
            if live.data.error.as_deref() != Some(error.as_str()) {
                app.dirty = true;
            }
            live.data.error = Some(error);
        }
    }
}

pub fn on_stream(app: &mut App, key: &str, batch: &super::super::feed::Batch) {
    let before = feed_lines(app);
    let Some(live) = app.live.as_mut().filter(|l| l.key == key) else {
        return;
    };
    if live.feed.add(batch) == 0 {
        return;
    }
    app.dirty = true;
    let added = feed_lines(app).saturating_sub(before);
    if let Some(live) = app.live.as_mut() {
        live.feed.keep_place(added);
    }
}

pub fn on_key(app: &mut App, key: Key, fx: &mut Vec<Effect>) {
    let Some(live) = app.live.as_ref() else {
        return;
    };
    if live.tell.is_some() {
        tell::on_key(app, key, fx);
        return;
    }
    if !live.pagers.is_empty() {
        pager::on_key(app, key, fx);
        return;
    }
    let layout = Layout::of(i32::from(app.size.0), i32::from(app.size.1));
    let height = layout.feed_h().max(1) as usize;
    match key {
        Key::Esc => close(app, fx),
        Key::Up | Key::Char('k') => scroll_up(app, 1, height),
        Key::Down | Key::Char('j') => scroll_down(app, 1),
        Key::PageUp => scroll_up(app, height.saturating_sub(1).max(1), height),
        Key::PageDown => scroll_down(app, height.saturating_sub(1).max(1)),
        Key::End | Key::Char('G') => {
            if let Some(live) = app.live.as_mut() {
                live.feed.end();
            }
        }
        Key::Char('n') => {
            if app.needs().is_empty() {
                notice(app, "Nothing waits for you.", Tone::Info);
            } else {
                app.card = Some(Card::Needs);
            }
        }
        Key::Char('t') => tell::open(app),
        Key::Char('o') => open_card(app),
        Key::Char('l') => log(app),
        Key::Char('p') => {
            let key = live.key.clone();
            push(app, Pager::loading(PagerKind::Plan, "plan"));
            fx.push(Effect::LoadPlan(key));
        }
        Key::Char('d') => diff(app, fx),
        Key::Char('x') => stop_card(app),
        Key::Char('r') => retry_card(app),
        Key::Enter => resume_card(app),
        Key::Char('?') => app.card = Some(Card::Help),
        Key::Char('q') => ask_quit(app, fx),
        _ => {}
    }
}

fn scroll_up(app: &mut App, n: usize, height: usize) {
    let total = feed_lines(app);
    if let Some(live) = app.live.as_mut() {
        live.feed.up(n, total, height);
    }
}

fn scroll_down(app: &mut App, n: usize) {
    if let Some(live) = app.live.as_mut() {
        live.feed.down(n);
    }
}

fn data(app: &App) -> Option<&TicketData> {
    app.live.as_ref()?.data.data.as_ref()
}

fn has_flag(data: &TicketData, flag: &str) -> bool {
    data.view.flags.iter().any(|f| f.flag == flag)
}

/// Puts a pager over the view.
pub fn push(app: &mut App, pager: Pager) {
    if let Some(live) = app.live.as_mut() {
        live.pagers.push(pager);
    }
}

/// `l`: the log of the ticket from the data that the view already has.
fn log(app: &mut App) {
    let Some(data) = data(app) else {
        notice(
            app,
            "The data of the ticket still loads. Try again in a moment.",
            Tone::Info,
        );
        return;
    };
    let mut log = Pager::loading(PagerKind::Log, "log");
    let (head, lines) = super::super::pages::log_lines(&data.timeline, app.clock.offset_seconds);
    let rows = super::super::pager::body_rows(app.size.1, true);
    log.fill(Some(head), lines, rows);
    push(app, log);
}

/// `d`: the commits that the forge does not have, for a ticket that holds a clone.
fn diff(app: &mut App, fx: &mut Vec<Effect>) {
    let Some(data) = data(app) else {
        return;
    };
    if data.view.clone.is_none() || data.facts.finished {
        let key = data.view.key.clone();
        notice(
            app,
            format!("{key} holds no clone now, so it has no commits to compare."),
            Tone::Info,
        );
        return;
    }
    let key = data.view.key.clone();
    push(app, Pager::loading(PagerKind::Diff, "commits"));
    fx.push(Effect::LoadDiff(key));
}

fn stop_card(app: &mut App) {
    let Some(data) = data(app) else {
        return;
    };
    if data.facts.finished || has_flag(data, STOPPED) {
        let key = data.view.key.clone();
        let text = if data.facts.finished {
            format!("{key} is done, so there is nothing to stop.")
        } else {
            format!("{key} is already stopped. Press Enter to start it again.")
        };
        notice(app, text, Tone::Info);
        return;
    }
    let card = StopCard {
        key: data.view.key.clone(),
        step: data.view.step.clone().unwrap_or_default(),
        branch: data.view.branch.clone(),
        agent: data.facts.work.as_ref().map(|w| w.agent.clone()),
        run: Run::Idle,
    };
    app.card = Some(Card::Stop(card));
}

fn retry_card(app: &mut App) {
    let Some(data) = data(app) else {
        return;
    };
    let paused = has_flag(data, PAUSED);
    let reason = data
        .view
        .flags
        .iter()
        .find(|f| f.flag == NEEDS_YOU)
        .map(|f| f.reason.clone());
    if !paused && reason.is_none() {
        let key = data.view.key.clone();
        notice(
            app,
            format!("{key} does not wait for you, so there is nothing to retry."),
            Tone::Info,
        );
        return;
    }
    let card = RetryCard {
        key: data.view.key.clone(),
        step: data.view.step.clone().unwrap_or_default(),
        agent: data.facts.agent.clone(),
        paused,
        held: data.facts.held,
        reason,
        run: Run::Idle,
    };
    app.card = Some(Card::Retry(card));
}

fn open_card(app: &mut App) {
    let Some(data) = data(app) else {
        return;
    };
    if data.facts.finished || has_flag(data, STOPPED) {
        let key = data.view.key.clone();
        notice(
            app,
            format!("{key} is not in progress, so its session does not open here."),
            Tone::Info,
        );
        return;
    }
    let last = data
        .timeline
        .iter()
        .rev()
        .find(|e| e.source == "turn" && !e.who.is_empty() && e.who != "judge")
        .map(|e| e.who.clone());
    if last.is_none() && data.facts.work.is_none() {
        let key = data.view.key.clone();
        notice(
            app,
            format!("{key} has no turn yet, so there is no session to open."),
            Tone::Info,
        );
        return;
    }
    let card = OpenCard {
        key: data.view.key.clone(),
        running: data.facts.work.as_ref().map(|w| w.agent.clone()),
        last,
    };
    app.card = Some(Card::Open(card));
}

/// Enter on a stopped ticket: the resume card of Home.
fn resume_card(app: &mut App) {
    let Some(data) = data(app) else {
        return;
    };
    if !has_flag(data, STOPPED) {
        return;
    }
    let card = ResumeCard {
        key: data.view.key.clone(),
        title: data.view.title.clone().unwrap_or_default(),
        step: data.view.step.clone().unwrap_or_default(),
        clone: data.view.clone.clone(),
        branch: data.view.branch.clone(),
        run: Run::Idle,
    };
    app.card = Some(Card::Resume(card));
}

/// The state of the ticket of the live view, when its data has loaded.
pub fn state(app: &App) -> Option<Kind> {
    let data = data(app)?;
    Some(app.now_of(&data.view, &data.facts).kind)
}
