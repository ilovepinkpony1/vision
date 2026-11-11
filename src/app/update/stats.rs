//! The keys and the data of the screen Stats, which `s` opens from Home. `w` switches between
//! the last 7 days and all time, and Esc goes back to Home. The numbers load when the screen
//! opens and when the window changes.

use super::super::model::{App, Card, Screen};
use super::super::msg::{Effect, Key};
use super::super::stats::StatsData;
use super::ask_quit;

pub fn open(app: &mut App, fx: &mut Vec<Effect>) {
    app.screen = Screen::Stats;
    load(app, fx);
}

fn load(app: &mut App, fx: &mut Vec<Effect>) {
    if app.stats.data.loading() {
        app.stats.data.again = true;
        return;
    }
    app.stats.data.since = Some(app.clock.elapsed);
    app.stats.data.again = false;
    fx.push(Effect::LoadStats {
        week: app.stats.week,
    });
}

pub fn on_stats(app: &mut App, week: bool, data: Result<StatsData, String>, fx: &mut Vec<Effect>) {
    let fetch = &mut app.stats.data;
    fetch.since = None;
    fetch.at = Some(app.clock.elapsed);
    app.dirty = true;
    // The numbers of the other window, from a load that ran before `w`, say nothing now.
    if week == app.stats.week {
        match data {
            Ok(data) => {
                fetch.data = Some(data);
                fetch.error = None;
            }
            Err(error) => fetch.error = Some(error),
        }
    }
    if app.stats.data.again || week != app.stats.week {
        load(app, fx);
    }
}

pub fn on_key(app: &mut App, key: Key, fx: &mut Vec<Effect>) {
    match key {
        Key::Esc => app.screen = Screen::Home,
        Key::Char('w') => {
            app.stats.week = !app.stats.week;
            load(app, fx);
        }
        Key::Char('?') => app.card = Some(Card::Help),
        Key::Char('q') => ask_quit(app, fx),
        _ => {}
    }
}
