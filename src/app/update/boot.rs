//! The loader in the update: its four stages, and when it leaves for Home.
//!
//! The home folder and the picture of the loader load first. When the home folder is set up,
//! the ledger and the queue of the tracker load at the same time. When the ledger stage
//! settles, the engine starts and its first pass goes into the queue. The loader leaves when
//! the android has appeared and the stages have settled, when the person presses a key, or
//! when the tracker took more than 10 seconds: the queue then still loads, and Home shows it
//! when it arrives.
//!
//! The picture of the loader is fit to the room of the window when it arrives and when the
//! window changes size, so the view draws a copy that is ready.

use std::time::Duration;

use super::super::model::{
    App, Screen, Setup, SetupError, StageState, STAGE_ENGINE, STAGE_HOME, STAGE_LEDGER,
    STAGE_TRACKER,
};
use super::super::msg::{Effect, Key};
use super::super::picture::Fitted;
use super::super::text::count;
use super::super::view::android::APPEAR_SECONDS;
use super::super::view::boot::room;
use super::{data, engine, leave};

/// The loader stops waiting for the tracker after this time.
pub const TRACKER_LIMIT: Duration = Duration::from_secs(10);

pub fn start(app: &mut App) -> Vec<Effect> {
    run_stage(app, STAGE_HOME);
    // With no color, the loader shows no picture, so the file is not read.
    if app.colors == crate::app::view::theme::ColorMode::Plain {
        app.picture = Some(None);
        return vec![Effect::LoadSetup];
    }
    vec![Effect::LoadSetup, Effect::LoadPicture]
}

/// Fits the picture of the loader to the room of the window, once for each size. After the
/// loader, a change of size drops the copy.
pub fn fit_picture(app: &mut App) {
    let room = room(i32::from(app.size.0), i32::from(app.size.1));
    let fit = match &app.picture {
        Some(Some(picture)) if app.screen == Screen::Boot => {
            if app.picture_fit.as_ref().is_some_and(|f| f.room == room) {
                return;
            }
            Some(Fitted::new(picture, room))
        }
        _ => None,
    };
    app.picture_fit = fit;
}

pub fn run_stage(app: &mut App, stage: usize) {
    let now = app.clock.elapsed;
    let stage = &mut app.boot.stages[stage];
    stage.state = StageState::Running;
    stage.since = Some(now);
    app.dirty = true;
}

pub fn settle(app: &mut App, stage: usize, state: StageState) {
    if app.boot.stages[stage].state == StageState::Running {
        app.boot.stages[stage].state = state;
        app.dirty = true;
    }
}

pub fn on_setup(app: &mut App, result: Result<Setup, SetupError>, fx: &mut Vec<Effect>) {
    app.dirty = true;
    match result {
        Ok(setup) => {
            let text = format!(
                "{} · adapter {} · {}",
                setup.home,
                setup.adapter,
                count(setup.steps.len(), "step")
            );
            settle(app, STAGE_HOME, StageState::Done(text));
            app.setup = Some(setup);
            run_stage(app, STAGE_LEDGER);
            data::load_local(app, fx);
            run_stage(app, STAGE_TRACKER);
            data::load_queue(app, fx);
        }
        Err(error) => {
            settle(app, STAGE_HOME, StageState::Failed(error.reason.clone()));
            app.boot.blocked = Some(error);
            app.screen = Screen::Boot;
        }
    }
}

/// The ledger stage settled: the engine starts, and its first pass goes into the queue.
pub fn ledger_settled(app: &mut App, fx: &mut Vec<Effect>) {
    if app.engine.started {
        return;
    }
    run_stage(app, STAGE_ENGINE);
    engine::start(app, fx);
    let text = if app.off() {
        "off · the kill switch is on. K turns it on".to_string()
    } else {
        "on · the first pass runs".to_string()
    };
    settle(app, STAGE_ENGINE, StageState::Done(text));
}

pub fn on_key(app: &mut App, key: Key, fx: &mut Vec<Effect>) {
    if key == Key::Char('q') {
        leave(app, fx);
        return;
    }
    if app.boot.blocked.is_none() {
        app.boot.skip = true;
    }
}

/// Moves from the loader to Home when the loader is done.
pub fn leave_when_ready(app: &mut App) {
    if app.screen != Screen::Boot || app.boot.blocked.is_some() {
        return;
    }
    let stages = &app.boot.stages;
    if !matches!(stages[STAGE_HOME].state, StageState::Done(_)) {
        return;
    }
    let now = app.clock.elapsed;
    let tracker_done = stages[STAGE_TRACKER].settled()
        || stages[STAGE_TRACKER]
            .since
            .is_some_and(|since| now >= since + TRACKER_LIMIT);
    let ready = now.as_secs_f64() >= APPEAR_SECONDS
        && stages[STAGE_LEDGER].settled()
        && stages[STAGE_ENGINE].settled()
        && tracker_done;
    if app.boot.skip || ready {
        app.screen = Screen::Home;
        app.dirty = true;
    }
}
