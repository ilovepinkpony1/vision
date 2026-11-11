//! The keys of the cards: the start card, the resume card, the kill switch card, the quit card,
//! the keys card, the needs card, and the card of a long error. Enter runs the action of a card
//! through the queue of the worker; while the action waits or runs, a second Enter does
//! nothing. Esc closes a card.

use super::super::model::{App, Card, Run, StartInfo};
use super::super::msg::{Action, Effect, Key};
use super::{ask_quit, engine, leave};

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
        Card::Needs => match key {
            Key::Esc | Key::Char('n') => app.card = None,
            Key::Char('q') => {
                app.card = None;
                ask_quit(app, fx);
            }
            _ => {}
        },
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
        _ => {}
    }
}
