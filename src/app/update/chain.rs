//! The chains in the update: one chain runs at a time, one step after the other. An action goes
//! into the queue of the worker; a pass is the next pass that the engine hands out after the
//! step before it, even while the engine is paused; a check is a read that starts only when no
//! job of the worker runs, and while it runs, the engine hands out no job, so that nothing
//! changes the state between the check and the step after it; a session gets the terminal.
//!
//! A step that fails or a check that does not hold stops the chain, and the card says why and
//! what is true for the ticket now. If the person quits the app, the rest of the chain does not
//! run.

use super::super::chain::{judge, Chain, CheckFacts, End, Mark, Owner, Verdict, Work, MAX_PASSES};
use super::super::model::{App, Card, OpenedCard, Run, Screen, Tone};
use super::super::msg::{ChildResult, Effect, Handoff, JobDone, JobKind};
use super::super::text::{error_line, tidy};
use super::{engine, models, needs, notice};

/// Starts a chain, unless one still runs. Gives true when it started.
pub fn start(app: &mut App, chain: Chain) -> bool {
    if app.chain.as_ref().is_some_and(Chain::running) {
        notice(
            app,
            "The action of another card still runs. Try again when it ends.",
            Tone::Info,
        );
        return false;
    }
    let passes = chain.steps.iter().any(|s| s.work == Work::Pass);
    if passes && app.off() {
        notice(
            app,
            "Vision is off, so no pass runs. Press K on Home to turn it on, then try again.",
            Tone::Info,
        );
        return false;
    }
    app.chain = Some(chain);
    app.dirty = true;
    true
}

fn current(app: &App) -> Option<&super::super::chain::Step> {
    app.chain.as_ref()?.step()
}

/// True while the step of the chain is a check: the engine then hands out no job.
pub fn holds(app: &App) -> bool {
    current(app).is_some_and(|s| matches!(s.work, Work::Check(_)))
}

/// True while the chain waits for a pass that has not started yet.
pub fn wants_pass(app: &App) -> bool {
    let Some(chain) = app.chain.as_ref() else {
        return false;
    };
    chain.started && chain.job.is_none() && current(app).is_some_and(|s| s.work == Work::Pass)
}

/// The engine handed out a pass: the chain takes it when it waits for one.
pub fn pass_started(app: &mut App, job: u64) {
    if wants_pass(app) {
        if let Some(chain) = app.chain.as_mut() {
            chain.job = Some(job);
        }
    }
}

/// True when the job is the action or the pass of the step of the chain.
pub fn owns(app: &App, job: u64) -> bool {
    app.chain
        .as_ref()
        .is_some_and(|c| c.running() && c.job == Some(job))
}

/// Starts the step that comes next. The update calls it after each message, before the engine
/// hands out the next job.
pub fn advance(app: &mut App, fx: &mut Vec<Effect>) {
    let Some(chain) = app.chain.as_ref() else {
        return;
    };
    if !chain.running() || chain.started {
        return;
    }
    let Some(step) = chain.step() else {
        return;
    };
    let work = step.work.clone();
    let id = chain.id;
    match work {
        Work::Act(action) => {
            let job = engine::queue_quiet(app, action);
            set_started(app, Some(job));
        }
        Work::Pass => {
            engine::queue_pass(app);
            set_started(app, None);
        }
        Work::Check(check) => {
            if app.engine.running.is_some() {
                return;
            }
            fx.push(Effect::LoadCheck { chain: id, check });
            set_started(app, None);
        }
        Work::Session(handoff) => {
            fx.push(match handoff {
                Handoff::Session(key) => Effect::OpenSession(key),
                Handoff::Review(name) => Effect::OpenReview(name),
            });
            set_started(app, None);
        }
    }
}

fn set_started(app: &mut App, job: Option<u64>) {
    if let Some(chain) = app.chain.as_mut() {
        chain.started = true;
        chain.job = job;
        if let Some(step) = chain.steps.get_mut(chain.at) {
            step.mark = Mark::Runs;
        }
    }
    app.dirty = true;
}

/// The end of a job of the chain.
pub fn on_done(app: &mut App, done: &JobDone, fx: &mut Vec<Effect>) {
    if !done.result.ok() {
        let text = match done.kind {
            JobKind::Pass => format!("The pass failed: {}", error_line(&done.result.failure())),
            JobKind::Action(_) => tidy(&done.result.failure()),
        };
        fail(app, text, fx);
        return;
    }
    next(app, fx);
}

/// The result of the read of a check.
pub fn on_checked(app: &mut App, id: u64, facts: Result<CheckFacts, String>, fx: &mut Vec<Effect>) {
    let Some(chain) = app.chain.as_ref().filter(|c| c.id == id && c.running()) else {
        return;
    };
    let Some(Work::Check(check)) = chain.step().map(|s| s.work.clone()) else {
        return;
    };
    let facts = match facts {
        Ok(facts) => facts,
        Err(error) => {
            fail(app, format!("Vision could not read the state: {error}"), fx);
            return;
        }
    };
    match judge(&check, &facts) {
        Verdict::Go => next(app, fx),
        Verdict::Again => again(app, &check, &facts, fx),
        Verdict::Stop(text) => fail(app, text, fx),
        Verdict::Changed { select, note } => {
            app.chain = None;
            needs::changed(app, select, note, fx);
        }
    }
}

/// One more pass, then the same check, up to [`MAX_PASSES`].
fn again(
    app: &mut App,
    check: &super::super::chain::Check,
    facts: &CheckFacts,
    fx: &mut Vec<Effect>,
) {
    let Some(chain) = app.chain.as_mut() else {
        return;
    };
    let pass_before = chain.at > 0 && chain.steps[chain.at - 1].work == Work::Pass;
    if !pass_before || chain.passes + 1 >= MAX_PASSES {
        let verb = check.verb().unwrap_or("the write");
        let text = format!(
            "{} passes ran, and none of them asked for {verb} again. {} is at the step {} now.",
            chain.passes + 1,
            check.ticket(),
            facts.step.as_deref().unwrap_or("-")
        );
        fail(app, text, fx);
        return;
    }
    chain.passes += 1;
    chain.steps[chain.at].mark = Mark::Todo;
    chain.at -= 1;
    chain.steps[chain.at].mark = Mark::Todo;
    chain.started = false;
    chain.job = None;
    app.dirty = true;
}

/// The step is done: the next one comes, or the chain ends.
fn next(app: &mut App, fx: &mut Vec<Effect>) {
    let Some(chain) = app.chain.as_mut() else {
        return;
    };
    if let Some(step) = chain.steps.get_mut(chain.at) {
        step.mark = Mark::Done;
    }
    chain.at += 1;
    chain.started = false;
    chain.job = None;
    app.dirty = true;
    if chain.at >= chain.steps.len() {
        finish(app, fx);
    }
}

/// The last step is done.
fn finish(app: &mut App, fx: &mut Vec<Effect>) {
    let Some(mut chain) = app.chain.take() else {
        return;
    };
    chain.end = Some(End::Done);
    match &chain.owner {
        Owner::Need(item) => needs::done(app, item, &chain.done, fx),
        Owner::TakeOver { .. } => {}
        Owner::Continue(key) => {
            if matches!(&app.card, Some(Card::Opened(card)) if card.key == *key) {
                app.card = None;
            }
            notice(app, chain.done.clone(), Tone::Ok);
        }
        Owner::Models(agent) => models::changed(app, agent, &chain.done, fx),
    }
}

/// A step failed: the chain stops. Its card shows the reason and the state of the ticket; with
/// no such card, a card of the error says it.
fn fail(app: &mut App, text: String, fx: &mut Vec<Effect>) {
    let Some(chain) = app.chain.as_mut() else {
        return;
    };
    if let Some(step) = chain.steps.get_mut(chain.at) {
        step.mark = Mark::Failed;
    }
    let state = chain.state();
    chain.end = Some(End::Failed {
        text: text.clone(),
        state: state.clone(),
    });
    chain.job = None;
    app.dirty = true;
    let owner = chain.owner.clone();
    if !shown(app, &owner) {
        app.chain = None;
        engine::show_error(app, "It did not work", &format!("{text}\n\nNow: {state}"));
    }
    if let Owner::Need(_) = owner {
        needs::reload_if_open(app, fx);
    }
}

/// True when the card or the screen of the owner of a chain shows now.
pub fn shown(app: &App, owner: &Owner) -> bool {
    match (owner, &app.card) {
        (Owner::Need(item), Some(Card::Need(card))) => card.item().is_some_and(|i| i.id() == *item),
        (Owner::TakeOver { key, .. }, Some(Card::Open(card))) => card.key == *key,
        (Owner::Continue(key), Some(Card::Opened(card))) => card.key == *key,
        (Owner::Models(_), None) => app.screen == Screen::Models,
        _ => false,
    }
}

/// The chain of the owner, while it runs or after it failed.
pub fn of<'a>(app: &'a App, owner: &Owner) -> Option<&'a Chain> {
    app.chain_of(owner)
}

/// Drops a chain that ended, when its card closes or moves on.
pub fn drop_ended(app: &mut App) {
    if app.chain.as_ref().is_some_and(|c| !c.running()) {
        app.chain = None;
    }
}

/// A session ended. Gives true when the chain owned it: after a take over, the card after the
/// session says that the ticket is stopped and paused.
pub fn on_session_ended(app: &mut App, key: &str, result: &ChildResult) -> bool {
    let Some(chain) = app.chain.as_ref() else {
        return false;
    };
    let waits = matches!(
        chain.step().map(|s| &s.work),
        Some(Work::Session(Handoff::Session(k))) if k == key
    );
    let Owner::TakeOver { agent, .. } = &chain.owner else {
        return false;
    };
    if !waits || !chain.started {
        return false;
    }
    let agent = agent.clone();
    app.chain = None;
    let error = (!result.ok())
        .then(|| error_line(&result.failure()))
        .filter(|e| !e.is_empty());
    app.card = Some(Card::Opened(OpenedCard {
        key: key.to_string(),
        code: result.code,
        error,
        run: Run::Idle,
        took_over: Some(agent),
    }));
    app.dirty = true;
    true
}

/// The review of a proposal ended.
pub fn on_review_ended(app: &mut App, name: &str, result: &ChildResult, fx: &mut Vec<Effect>) {
    let waits = app.chain.as_ref().is_some_and(|c| {
        c.started
            && matches!(
                c.step().map(|s| &s.work),
                Some(Work::Session(Handoff::Review(n))) if n == name
            )
    });
    if !waits {
        return;
    }
    if result.ok() {
        next(app, fx);
    } else {
        fail(
            app,
            format!(
                "The review ended with an error: {}",
                error_line(&result.failure())
            ),
            fx,
        );
    }
}
