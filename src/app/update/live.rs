//! The keys and the data of the live view of a ticket.
//!
//! Esc goes back to Home. `n` opens the card of the first thing of the ticket that waits for the
//! person, `t` opens the line to tell the agent something,
//! `o` opens the session, `l` the log, `p` the plan, `d` the commits that the forge does not
//! have, `x` stops the ticket, and `r` retries it when it waits for the person with a flag or
//! is paused. Enter on a stopped or paused ticket opens the continue card. The arrows, `k`, `j`, PageUp,
//! and PageDown scroll the activity; `G` and End go back to its end. `f` shows the next kind of
//! activity, `F` the kind before, and the digits 1 to 7 pick a tab; a change of the tab goes to
//! the end of its list. `?` shows the keys, and `q` quits as on Home.
//!
//! While the view is open, its data loads again each second, and a thread follows the stream
//! files of the ticket.

use std::time::Duration;

use super::super::feed::{Filter, FILTERS, KEEP_LEDGER};
use super::super::live::{Live, TicketData};
use super::super::model::{App, Card, OpenCard, RetryCard, Run, Screen, StopCard, Tone};
use super::super::moments::{in_feed, moment};
use super::super::msg::{Action, Effect, Key};
use super::super::now::Kind;
use super::super::pager::{Pager, PagerKind, PagerState};
use super::super::view::live::layout::Layout;
use super::{ask_quit, notice, pager, resume, tell};
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

/// The lines of the activity of the tab that shows, at the size of the window.
pub fn feed_lines(app: &App) -> usize {
    let Some(live) = &app.live else {
        return 0;
    };
    let layout = Layout::of(i32::from(app.size.0), i32::from(app.size.1));
    live.feed
        .shared_lines(layout.feed_text_w(), app.clock.offset_seconds, app.activity)
        .len()
}

/// Shows another tab of the activity, from the end of its list.
fn filter(app: &mut App, to: Filter) {
    app.activity = to;
    if let Some(live) = app.live.as_mut() {
        live.feed.end();
    }
}

/// A click on a tab of the activity shows that tab, as its digit does. The line of `t` stays
/// open.
pub fn click_tab(app: &mut App, to: Filter) {
    if app.card.is_none() && app.live.as_ref().is_some_and(|l| l.pagers.is_empty()) {
        filter(app, to);
    }
}

/// The lines of the activity before a change, when the feed does not follow: the place of the
/// view needs them. A feed that follows keeps no place, so it needs no count.
fn lines_before(app: &App) -> Option<usize> {
    let follows = app.live.as_ref().is_none_or(|l| l.feed.follow);
    (!follows).then(|| feed_lines(app))
}

/// Keeps the same lines in view after a change, when the feed does not follow.
fn keep_place(app: &mut App, before: Option<usize>) {
    let Some(before) = before else {
        return;
    };
    let added = feed_lines(app).saturating_sub(before);
    if let Some(live) = app.live.as_mut() {
        live.feed.keep_place(added);
    }
}

pub fn on_ticket(app: &mut App, key: &str, data: Result<TicketData, String>) {
    let before = lines_before(app);
    let Some(live) = app.live.as_mut().filter(|l| l.key == key) else {
        return;
    };
    live.data.since = None;
    live.data.at = Some(app.clock.elapsed);
    match data {
        Ok(data) => {
            // The feed keeps only the last lines of the ledger, and `Feed::set_moments` drops
            // the others. The cut comes before the words of each line, so a long timeline costs
            // no work for the lines that it drops.
            let mut moments: Vec<_> = data
                .timeline
                .iter()
                .rev()
                .filter(|entry| in_feed(entry))
                .take(KEEP_LEDGER)
                .map(|entry| (vtime::parse_rfc3339(&entry.at).ok(), moment(entry)))
                .collect();
            moments.reverse();
            let changed = live.feed.set_moments(&moments);
            if live.data.data.as_ref() != Some(&data) || live.data.error.is_some() {
                app.dirty = true;
            }
            live.data.data = Some(data);
            live.data.error = None;
            if changed {
                app.dirty = true;
                keep_place(app, before);
            }
            fill_waiting_logs(app);
        }
        Err(error) => {
            if live.data.error.as_deref() != Some(error.as_str()) {
                app.dirty = true;
            }
            // A log that waits for the first data says why it did not load. The next data
            // fills it.
            if live.data.data.is_none() {
                for pager in live.pagers.iter_mut().filter(|p| p.kind == PagerKind::Log) {
                    pager.state = PagerState::Failed(error.clone());
                }
            }
            live.data.error = Some(error);
        }
    }
}

/// The loader found no change: the data of the view stays.
pub fn on_ticket_same(app: &mut App, key: &str) {
    let Some(live) = app.live.as_mut().filter(|l| l.key == key) else {
        return;
    };
    live.data.since = None;
    live.data.at = Some(app.clock.elapsed);
}

pub fn on_stream(app: &mut App, key: &str, batch: &super::super::feed::Batch) {
    let before = lines_before(app);
    let Some(live) = app.live.as_mut().filter(|l| l.key == key) else {
        return;
    };
    if live.feed.add(batch) == 0 {
        return;
    }
    app.dirty = true;
    keep_place(app, before);
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
        Key::Char('f') => filter(app, app.activity.next()),
        Key::Char('F') => filter(app, app.activity.back()),
        Key::Char(c @ '1'..='7') => {
            let n = c.to_digit(10).map_or(1, |n| n as usize);
            filter(app, FILTERS[n - 1]);
        }
        Key::Char('n') => {
            let key = live.key.clone();
            super::needs::open(app, Some(key), fx);
        }
        Key::Char('t') => tell::open(app),
        Key::Char('o') => open_card(app),
        Key::Char('l') => log(app),
        Key::Char('p') => plan(app, fx),
        Key::Char('d') => diff(app, fx),
        Key::Char('x') => stop_card(app),
        Key::Char('r') => retry_card(app),
        Key::Enter => continue_card(app, fx),
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

/// `l`: the log of the ticket from the data that the view already has. While the data still
/// loads, the log waits for it.
fn log(app: &mut App) {
    let mut log = Pager::loading(PagerKind::Log, "log");
    if let Some(data) = data(app) {
        fill_log(app, data, &mut log);
    }
    push(app, log);
}

/// Fills the pager of the log with the timeline of the data.
fn fill_log(app: &App, data: &TicketData, log: &mut Pager) {
    let (head, lines) = super::super::pages::log_lines(&data.timeline, app.clock.offset_seconds);
    let rows = super::super::pager::body_rows(app.size.1, true);
    log.fill(Some(head), lines, rows);
}

/// Fills each log that waits for the data of the view, such as a log of a place of the history
/// that opened with the view, and a log whose wait failed.
fn fill_waiting_logs(app: &mut App) {
    let Some(live) = app.live.as_ref() else {
        return;
    };
    let waits = |p: &Pager| p.kind == PagerKind::Log && p.state != PagerState::Ready;
    let (Some(data), true) = (live.data.data.as_ref(), live.pagers.iter().any(waits)) else {
        return;
    };
    let mut pagers = live.pagers.clone();
    for pager in pagers.iter_mut().filter(|p| waits(p)) {
        fill_log(app, data, pager);
    }
    if let Some(live) = app.live.as_mut() {
        live.pagers = pagers;
    }
    app.dirty = true;
}

/// `p`: the plan of the ticket, which loads.
fn plan(app: &mut App, fx: &mut Vec<Effect>) {
    let Some(key) = app.live.as_ref().map(|l| l.key.clone()) else {
        return;
    };
    push(app, Pager::loading(PagerKind::Plan, "plan"));
    fx.push(Effect::LoadPlan(key));
}

/// Opens a pager of this kind over the view again, for a place of the history. The commits
/// load without the check of `d`: a failed load says what is wrong in the pager.
pub fn reopen(app: &mut App, kind: &PagerKind, fx: &mut Vec<Effect>) {
    let Some(key) = app.live.as_ref().map(|l| l.key.clone()) else {
        return;
    };
    match kind {
        PagerKind::Log => log(app),
        PagerKind::Plan => plan(app, fx),
        PagerKind::Diff => {
            push(app, Pager::loading(PagerKind::Diff, "commits"));
            fx.push(Effect::LoadDiff(key));
        }
        PagerKind::Turn { id } => pager::open_turn(app, id, fx),
    }
}

/// True while the view shows no card, no pager, and no line of `t`: then a part of the view
/// itself can act.
fn view_takes_clicks(app: &App) -> bool {
    app.card.is_none()
        && app
            .live
            .as_ref()
            .is_some_and(|l| l.tell.is_none() && l.pagers.is_empty())
}

/// A click on a step of the path: the log, with the cursor on the last line that names the
/// step.
pub fn click_step(app: &mut App, step: &str) {
    if !view_takes_clicks(app) {
        return;
    }
    let at = data(app).and_then(|d| {
        d.timeline
            .iter()
            .rposition(|e| super::super::moments::names_step(e, step))
    });
    log(app);
    let rows = super::super::pager::body_rows(app.size.1, true);
    if let (Some(at), Some(pager)) = (at, app.live.as_mut().and_then(|l| l.pagers.last_mut())) {
        if pager.state == PagerState::Ready && at < pager.lines.len() {
            pager.cursor = Some(at);
            pager.step(0, rows);
        }
    }
}

/// A click on a line of the activity: the events of its turn.
pub fn click_turn(app: &mut App, turn: &str, fx: &mut Vec<Effect>) {
    if view_takes_clicks(app) {
        pager::open_turn(app, turn, fx);
    }
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
            format!("{key} is already stopped. Press Enter to continue it.")
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
        act: data.facts.act,
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
    // A turn in a herdr pane opens in Herdr.app, and it goes on (section 7.7).
    if data.facts.work.is_some()
        && app
            .local
            .data
            .as_ref()
            .is_some_and(|l| l.panes.contains(&data.view.key))
    {
        let key = data.view.key.clone();
        super::engine::queue_action(app, Action::OpenInHerdr { key });
        return;
    }
    let card = OpenCard {
        key: data.view.key.clone(),
        running: data.facts.work.as_ref().map(|w| w.agent.clone()),
        last,
    };
    app.card = Some(Card::Open(card));
}

/// Enter on a stopped or paused ticket: the continue card, as on Home.
fn continue_card(app: &mut App, fx: &mut Vec<Effect>) {
    let Some(data) = data(app) else {
        return;
    };
    if data.facts.finished || !resume::continues(&data.view.flags) {
        return;
    }
    let key = data.view.key.clone();
    let title = data.view.title.clone().unwrap_or_default();
    let stored = data.view.step.clone().unwrap_or_default();
    let stopped = has_flag(data, STOPPED);
    resume::open(app, &key, &title, &stored, stopped, fx);
}

/// The state of the ticket of the live view, when its data has loaded.
pub fn state(app: &App) -> Option<Kind> {
    let data = data(app)?;
    Some(app.now_of(&data.view, &data.facts).kind)
}
