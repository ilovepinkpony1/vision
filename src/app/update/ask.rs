//! The keys of the question card (`mAsk` of the design). A letter chooses that choice. Enter
//! takes the proposal of the Judge, or else the recommended choice. `e` (or `t`) opens a line
//! for an answer in the words of the person, and `k` (or `-`) marks the question as needless
//! and asks where the answer already was: `c` the code, `t` the ticket, `m` the memory, or `n`
//! no matter. After a choice or a text, the card asks for the reason in one line: Enter saves
//! the answer with its reason, and Tab saves it with no reason. Esc goes back one step.
//!
//! `e` and `k` are also letters of choices; on a card with that many choices, the letter
//! chooses, and `t` and `-` still open the line and the question of a needless card.

use super::super::line::Line;
use super::super::model::{App, Card, Tone};
use super::super::msg::{Effect, Given, Key};
use super::super::needs::{enter_reply, Input, NeedItem, Reply};
use super::super::recipes;
use super::{chain, notice};
use crate::questions::CardView;

fn input(app: &mut App) -> Option<&mut Input> {
    match app.card.as_mut() {
        Some(Card::Need(card)) => Some(&mut card.input),
        _ => None,
    }
}

fn set(app: &mut App, value: Input) {
    if let Some(input) = input(app) {
        *input = value;
    }
}

/// A key of the card while no line is open.
pub fn on_key(app: &mut App, card: &CardView, key: Key) {
    match key {
        Key::Enter => match enter_reply(card) {
            Some(reply) => set(
                app,
                Input::Why {
                    reply,
                    line: Line::default(),
                },
            ),
            None => notice(
                app,
                "This question has no recommended choice. Press the letter of a choice.",
                Tone::Info,
            ),
        },
        Key::Char(c) => {
            if let Some(choice) = card.choices.iter().find(|ch| ch.letter == Some(c)) {
                set(
                    app,
                    Input::Why {
                        reply: Reply::Choice(choice.n.saturating_sub(1)),
                        line: Line::default(),
                    },
                );
            } else if c == 'e' || c == 't' {
                set(app, Input::Words(Line::default()));
            } else if c == 'k' || c == '-' {
                set(app, Input::Needless);
            }
        }
        _ => {}
    }
}

/// A key while a line or the question of a needless card is open.
pub fn on_input(app: &mut App, key: Key, _fx: &mut [Effect]) {
    let Some(Card::Need(card)) = app.card.as_ref() else {
        return;
    };
    let Some(NeedItem::Question(view)) = card.item().cloned() else {
        return;
    };
    let Some(input) = input(app) else {
        return;
    };
    match input {
        Input::None => {}
        Input::Needless => {
            let place = match key {
                Key::Esc => {
                    *input = Input::None;
                    return;
                }
                Key::Char('c') => "code",
                Key::Char('t') => "ticket",
                Key::Char('m') => "memory",
                Key::Char('n') => "no-matter",
                _ => return,
            };
            submit(app, &view, Given::Needless(place.into()), "");
        }
        Input::Words(line) => match key {
            Key::Esc => *input = Input::None,
            Key::Enter => {
                if line.is_blank() {
                    notice(
                        app,
                        "The answer is empty. Type it, or press Esc.",
                        Tone::Info,
                    );
                } else {
                    let text = line.text().trim().to_string();
                    *input = Input::Why {
                        reply: Reply::Text(text),
                        line: Line::default(),
                    };
                }
            }
            other => edit(line, other),
        },
        Input::Why { reply, line } => match key {
            Key::Esc => *input = Input::None,
            Key::Enter | Key::Tab => {
                let why = if key == Key::Enter {
                    line.text()
                } else {
                    String::new()
                };
                let given = match reply.clone() {
                    Reply::Choice(i) => Given::Choice(i + 1),
                    Reply::Text(text) => Given::Text(text),
                };
                submit(app, &view, given, &why);
            }
            other => edit(line, other),
        },
    }
}

fn edit(line: &mut Line, key: Key) {
    match key {
        Key::Left => line.left(),
        Key::Right => line.right(),
        Key::Home => line.home(),
        Key::End => line.end(),
        Key::Backspace => line.backspace(),
        Key::Delete => line.delete(),
        Key::Char(c) => line.insert(c),
        _ => {}
    }
}

/// Starts the chain of the answer, and closes the line.
fn submit(app: &mut App, view: &CardView, given: Given, why: &str) {
    let id = app.next_seq();
    if chain::start(app, recipes::answer(id, view, given, why)) {
        set(app, Input::None);
    }
}
