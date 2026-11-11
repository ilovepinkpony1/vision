//! The keys of the cards: the start card, the resume card, the kill switch card, the quit card,
//! the keys card, the card of a long error, and the cards of the live view (stop, retry, open
//! the session, and the card after the session). Enter runs the action of a card through the
//! queue of the worker; while the action waits or runs, a second Enter does nothing. Esc closes
//! a card. The card of the things that wait has its own module.
//!
//! The open card of a running turn also offers `o`: stop the turn of its agent and open its
//! session, as one chain. The card after such a session continues the ticket with the take
//! and then the retry, or keeps it stopped.

use super::super::chain::Owner;
use super::super::model::{App, Card, OpenedCard, Run, StartInfo};
use super::super::msg::{Action, ChildResult, Effect, Key};
use super::super::recipes;
use super::super::text::error_line;
use super::{ask_quit, chain, engine, leave, needs, tell};

/// The session of `vision open` ended: the card asks whether the ticket goes on.
pub fn on_session_ended(app: &mut App, key: &str, result: &ChildResult) {
    app.dirty = true;
    let error = if result.ok() {
        None
    } else {
        Some(error_line(&result.failure()))
    };
    app.card = Some(Card::Opened(OpenedCard {
        key: key.to_string(),
        code: result.code,
        error: error.filter(|e| !e.is_empty()),
        run: Run::Idle,
        took_over: None,
    }));
}

pub fn on_start_info(app: &mut App, key: &str, info: Result<StartInfo, String>) {
    if let Some(Card::Start(card)) = app.card.as_mut() {
        if card.key == key {
            card.info = Some(info);
            app.dirty = true;
        }
    }
}

pub fn on_key(app: &mut App, key: Key, fx: &mut Vec<Effect>) {
    let Some(card) = app.card.as_mut() else {
        return;
    };
    match card {
        Card::Help => match key {
            Key::Esc | Key::Char('?') => app.card = None,
            Key::Char('q') => {
                app.card = None;
                ask_quit(app, fx);
            }
            _ => {}
        },
        Card::Need(_) => needs::on_key(app, key, fx),
        Card::Error { .. } => {
            if matches!(key, Key::Esc | Key::Enter) {
                app.card = None;
            }
        }
        Card::Quit => match key {
            Key::Enter | Key::Char('q') => leave(app, fx),
            Key::Esc => app.card = None,
            _ => {}
        },
        Card::Start(card) => match key {
            Key::Enter if card.run.job().is_none() => {
                let action = Action::Take {
                    key: card.key.clone(),
                };
                run(app, action);
            }
            Key::Esc => app.card = None,
            _ => {}
        },
        Card::Resume(card) => match key {
            Key::Enter if card.run.job().is_none() => {
                let action = Action::Take {
                    key: card.key.clone(),
                };
                run(app, action);
            }
            Key::Esc => app.card = None,
            _ => {}
        },
        Card::Stop(card) => match key {
            Key::Enter if card.run.job().is_none() => {
                let action = Action::Stop {
                    key: card.key.clone(),
                };
                run(app, action);
            }
            Key::Esc => app.card = None,
            _ => {}
        },
        Card::Retry(card) => match key {
            Key::Enter if card.run.job().is_none() => {
                let action = Action::Retry {
                    key: card.key.clone(),
                };
                run(app, action);
            }
            Key::Esc => app.card = None,
            _ => {}
        },
        Card::Opened(card) if card.took_over.is_some() => match key {
            Key::Enter => {
                let key = card.key.clone();
                let opened = card.error.is_none();
                // One chain for this card: after a failure, its card says what is true now.
                if chain::of(app, &Owner::Continue(key.clone())).is_none() {
                    let id = app.next_seq();
                    let owner = Owner::Continue(key.clone());
                    chain::start(app, recipes::continue_after(id, &key, opened, owner));
                }
            }
            Key::Esc => {
                app.card = None;
                chain::drop_ended(app);
            }
            _ => {}
        },
        Card::Opened(card) => match key {
            Key::Enter if card.run.job().is_none() => {
                let action = Action::Retry {
                    key: card.key.clone(),
                };
                run(app, action);
            }
            Key::Esc => app.card = None,
            _ => {}
        },
        Card::Open(card) => match (key, card.running.is_some()) {
            (Key::Enter, false) => {
                let key = card.key.clone();
                app.card = None;
                fx.push(Effect::OpenSession(key));
            }
            (Key::Char('t'), true) => {
                app.card = None;
                tell::open(app);
            }
            (Key::Char('x'), true) => {
                app.card = None;
                super::live::on_key(app, Key::Char('x'), fx);
            }
            (Key::Char('o'), true) => {
                let key = card.key.clone();
                let agent = card.running.clone().unwrap_or_default();
                let owner = Owner::TakeOver {
                    key: key.clone(),
                    agent: agent.clone(),
                };
                if chain::of(app, &owner).is_none_or(|c| !c.running()) {
                    let id = app.next_seq();
                    chain::start(app, recipes::take_over(id, &key, &agent));
                }
            }
            (Key::Esc, _) => {
                app.card = None;
                chain::drop_ended(app);
            }
            _ => {}
        },
        Card::Kill(card) => match key {
            Key::Enter if card.run.job().is_none() => {
                let action = if card.turn_on {
                    Action::On
                } else {
                    Action::Off
                };
                run(app, action);
            }
            Key::Esc => app.card = None,
            _ => {}
        },
    }
}

/// Queues the action of the open card and marks the card as waiting for it.
fn run(app: &mut App, action: Action) {
    let (job, _) = engine::queue_action(app, action);
    let state = Run::Waiting { job };
    match app.card.as_mut() {
        Some(Card::Start(card)) => card.run = state,
        Some(Card::Resume(card)) => card.run = state,
        Some(Card::Kill(card)) => card.run = state,
        Some(Card::Stop(card)) => card.run = state,
        Some(Card::Retry(card)) => card.run = state,
        Some(Card::Opened(card)) => card.run = state,
        _ => {}
    }
}
