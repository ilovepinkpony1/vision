//! The keys and the data of the screen Models, which `m` opens from Home. Up and down select an
//! agent, left and right change its model through the known models, `e` changes its effort,
//! Enter pins the values that show (`vision model set`), and `u` goes back to the default
//! (`vision model reset`). A change that is not pinned yet has a mark; Esc drops each such
//! change, and then goes back to Home.

use super::super::chain::{Chain, Owner};
use super::super::model::{App, Screen, Tone};
use super::super::model_table::{cycle, Edit, ModelLine, EFFORTS};
use super::super::msg::{Effect, Key};
use super::super::recipes;
use super::{ask_quit, chain, notice};

pub fn open(app: &mut App, fx: &mut Vec<Effect>) {
    app.screen = Screen::Models;
    app.models.selected = 0;
    app.models.edits.clear();
    load(app, fx);
}

fn load(app: &mut App, fx: &mut Vec<Effect>) {
    if app.models.data.loading() {
        app.models.data.again = true;
        return;
    }
    app.models.data.since = Some(app.clock.elapsed);
    app.models.data.again = false;
    fx.push(Effect::LoadModels);
}

pub fn on_models(app: &mut App, data: Result<Vec<ModelLine>, String>, fx: &mut Vec<Effect>) {
    let fetch = &mut app.models.data;
    fetch.since = None;
    fetch.at = Some(app.clock.elapsed);
    match data {
        Ok(lines) => {
            fetch.data = Some(lines);
            fetch.error = None;
        }
        Err(error) => fetch.error = Some(error),
    }
    let len = app.models.lines().len();
    app.models.selected = app.models.selected.min(len.saturating_sub(1));
    // A change that equals the values of the table is no change.
    let lines = app.models.lines().to_vec();
    app.models.edits.retain(|agent, edit| {
        lines
            .iter()
            .find(|l| l.agent == *agent)
            .is_some_and(|l| l.model != edit.model || l.effort != edit.effort)
    });
    app.dirty = true;
    if app.models.data.again {
        load(app, fx);
    }
}

/// The pin or the reset of an agent ended well.
pub fn changed(app: &mut App, agent: &str, text: &str, fx: &mut Vec<Effect>) {
    app.models.edits.remove(agent);
    notice(app, text.to_string(), Tone::Ok);
    if app.screen == Screen::Models {
        load(app, fx);
    }
}

pub fn on_key(app: &mut App, key: Key, fx: &mut Vec<Effect>) {
    let runs = chain::of(app, &owner_of(app)).is_some_and(Chain::running);
    let len = app.models.lines().len();
    match key {
        Key::Esc => {
            if app.models.edits.is_empty() {
                chain::drop_ended(app);
                app.screen = Screen::Home;
            } else {
                app.models.edits.clear();
                notice(app, "Dropped each change that was not pinned.", Tone::Info);
            }
        }
        Key::Up | Key::Char('k') => {
            app.models.selected = app.models.selected.saturating_sub(1);
            chain::drop_ended(app);
        }
        Key::Down | Key::Char('j') => {
            app.models.selected = (app.models.selected + 1).min(len.saturating_sub(1));
            chain::drop_ended(app);
        }
        Key::Left => change(app, |list, edit| edit.model = cycle(list, &edit.model, -1)),
        Key::Right => change(app, |list, edit| edit.model = cycle(list, &edit.model, 1)),
        Key::Char('e') => change(app, |_, edit| {
            let efforts: Vec<String> = EFFORTS.iter().map(|e| e.to_string()).collect();
            edit.effort = cycle(&efforts, &edit.effort, 1);
        }),
        Key::Enter if !runs => pin(app),
        Key::Char('u') if !runs => reset(app),
        Key::Char('?') => app.card = Some(super::super::model::Card::Help),
        Key::Char('q') => ask_quit(app, fx),
        _ => {}
    }
}

/// The owner of the chain of the selected agent.
fn owner_of(app: &App) -> Owner {
    let agent = app
        .models
        .lines()
        .get(app.models.selected)
        .map(|l| l.agent.clone())
        .unwrap_or_default();
    Owner::Models(agent)
}

/// Changes the values that show for the selected agent. A change back to the values of the
/// table is no change.
fn change(app: &mut App, f: impl FnOnce(&[String], &mut Edit)) {
    let Some(line) = app.models.lines().get(app.models.selected).cloned() else {
        return;
    };
    let (model, effort) = app.models.shown(&line);
    let mut edit = Edit { model, effort };
    let list = line.known_models();
    f(&list, &mut edit);
    if edit.model == line.model && edit.effort == line.effort {
        app.models.edits.remove(&line.agent);
    } else {
        app.models.edits.insert(line.agent.clone(), edit);
    }
}

/// Enter: pins the values that show for the selected agent.
fn pin(app: &mut App) {
    let Some(line) = app.models.lines().get(app.models.selected).cloned() else {
        return;
    };
    let (model, effort) = app.models.shown(&line);
    let id = app.next_seq();
    chain::start(app, recipes::model_set(id, &line.agent, &model, &effort));
}

/// `u`: the default of Vision again.
fn reset(app: &mut App) {
    let Some(line) = app.models.lines().get(app.models.selected).cloned() else {
        return;
    };
    if !line.pinned {
        app.models.edits.remove(&line.agent);
        notice(
            app,
            format!(
                "{} already uses the default: {} {}.",
                line.agent, line.default_model, line.default_effort
            ),
            Tone::Info,
        );
        return;
    }
    let id = app.next_seq();
    let chain = recipes::model_reset(id, &line.agent, &line.default_model, &line.default_effort);
    chain::start(app, chain);
}
