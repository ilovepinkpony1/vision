//! The keys of the question form (`mAsk` of the design). The questions of one asking turn, or
//! of one ask step, show as one form: a page for each question, and a review page after the
//! last one. A key of a question page only stores a draft. Nothing goes to Vision before the
//! send, and the drafts stay in the drafts file of the ticket until then.
//!
//! On a question page, while no line is open: a letter chooses that choice. Enter takes the
//! draft of the question, else the proposal of the Judge, else the recommended choice. `e` (or
//! `t`) opens a line for an answer in the words of the person, and `k` (or `-`) marks the
//! question as needless and asks where the answer already was: `c` the code, `t` the ticket,
//! `m` the memory, or `n` no matter. After a choice or a text, a line asks for the reason:
//! Enter keeps the draft with its reason, and Tab keeps it with no reason. A line opens with
//! the old text of the draft. After a draft, the form goes to the next question with no draft,
//! or to the review page. Esc goes back one step.
//!
//! A digit opens the question of that number, and `r` or `s` opens the review page. On the
//! review page, the up and down arrows choose a row, Enter opens its question, and `s` sends
//! each answer of the form in one action, which needs a draft for each question. The left and
//! right arrows move between the pages; the module of the card handles them.
//!
//! `e`, `k`, and `r` are also letters of choices. On a card with that many choices, the letter
//! chooses, and `t`, `-`, and `s` still do their part.

use super::super::line::Line;
use super::super::model::{App, Card, Tone};
use super::super::msg::{Effect, Key};
use super::super::needs::{enter_reply, Draft, Input, NeedsCard, QuestionSet, Reply};
use super::super::recipes;
use super::super::text::join_and;
use super::{chain, notice};

fn card(app: &mut App) -> Option<&mut NeedsCard> {
    match app.card.as_mut() {
        Some(Card::Need(card)) => Some(card),
        _ => None,
    }
}

fn set_input(app: &mut App, value: Input) {
    if let Some(card) = card(app) {
        card.input = value;
    }
}

/// A key of the form while no line is open.
pub fn on_key(app: &mut App, set: &QuestionSet, key: Key) {
    let Some(page) = card(app).map(|c| c.page) else {
        return;
    };
    if let Key::Char(c) = key {
        if let Some(n) = c.to_digit(10).map(|n| n as usize) {
            if (1..=set.len()).contains(&n) {
                open_page(app, set, n - 1);
            }
            return;
        }
    }
    if page >= set.review() {
        review_key(app, set, key);
    } else {
        question_key(app, set, page, key);
    }
}

/// A key of a question page.
fn question_key(app: &mut App, set: &QuestionSet, i: usize, key: Key) {
    let Some(view) = set.cards.get(i) else {
        return;
    };
    let draft = card(app).and_then(|c| c.drafts.get(&view.id).cloned());
    let old_why = match &draft {
        Some(Draft::Answer { why, .. }) => why.clone(),
        _ => String::new(),
    };
    match key {
        Key::Enter => match draft {
            Some(Draft::Answer { reply, why }) => set_input(
                app,
                Input::Why {
                    reply,
                    line: Line::of(&why),
                },
            ),
            Some(Draft::Needless(_)) => set_input(app, Input::Needless),
            None => match enter_reply(view) {
                Some(reply) => set_input(
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
        },
        Key::Char(c) => {
            if let Some(choice) = view.choices.iter().find(|ch| ch.letter == Some(c)) {
                set_input(
                    app,
                    Input::Why {
                        reply: Reply::Choice(choice.n.saturating_sub(1)),
                        line: Line::of(&old_why),
                    },
                );
            } else if c == 'e' || c == 't' {
                let old = match &draft {
                    Some(Draft::Answer {
                        reply: Reply::Text(text),
                        ..
                    }) => text.clone(),
                    _ => String::new(),
                };
                set_input(app, Input::Words(Line::of(&old)));
            } else if c == 'k' || c == '-' {
                set_input(app, Input::Needless);
            } else if c == 'r' || c == 's' {
                open_page(app, set, set.review());
                if c == 's' {
                    let text = if set.missing(&drafts(app)).is_empty() {
                        "These are your answers. Press s to send them."
                    } else {
                        "These are your answers. Each question needs one before the send."
                    };
                    notice(app, text, Tone::Info);
                }
            }
        }
        _ => {}
    }
}

fn drafts(app: &App) -> std::collections::BTreeMap<String, Draft> {
    match &app.card {
        Some(Card::Need(card)) => card.drafts.clone(),
        _ => Default::default(),
    }
}

/// A key of the review page.
fn review_key(app: &mut App, set: &QuestionSet, key: Key) {
    let Some(row) = card(app).map(|c| c.row) else {
        return;
    };
    match key {
        Key::Enter => open_page(app, set, row.min(set.len().saturating_sub(1))),
        Key::Char('s') => send(app, set),
        _ => {}
    }
}

/// Moves the row of the review page.
pub fn move_row(app: &mut App, set: &QuestionSet, by: i64) {
    if let Some(card) = card(app) {
        let last = set.len().saturating_sub(1) as i64;
        card.row = (card.row as i64 + by).clamp(0, last) as usize;
    }
}

/// Shows a page of the form: a question, or the review page.
pub fn open_page(app: &mut App, set: &QuestionSet, page: usize) {
    let Some(card) = card(app) else {
        return;
    };
    card.page = page.min(set.review());
    card.input = Input::None;
    card.scroll = 0;
    card.note = None;
    if card.page >= set.review() {
        card.row = set
            .missing(&card.drafts)
            .first()
            .copied()
            .unwrap_or(card.row)
            .min(set.len().saturating_sub(1));
    } else {
        card.row = card.page;
    }
}

/// The send: one action with the draft of each question, or a notice that names the questions
/// with no draft.
fn send(app: &mut App, set: &QuestionSet) {
    let Some(card) = card(app) else {
        return;
    };
    match set.answers(&card.drafts) {
        Err(missing) => {
            card.row = missing.first().copied().unwrap_or(0);
            let numbers: Vec<String> = missing.iter().map(|i| (i + 1).to_string()).collect();
            let text = match numbers.as_slice() {
                [one] => format!("Question {one} has no draft yet. Press {one} to answer it."),
                many => format!(
                    "Questions {} have no draft yet. Answer each one, then press s.",
                    join_and(many)
                ),
            };
            notice(app, text, Tone::Info);
        }
        Ok(answers) => {
            let id = app.next_seq();
            chain::start(app, recipes::answer_set(id, set, answers));
        }
    }
}

/// A key while a line or the question of a needless card is open.
pub fn on_input(app: &mut App, key: Key, fx: &mut Vec<Effect>) {
    let Some(card) = card(app) else {
        return;
    };
    let Some(set) = card.set().cloned() else {
        return;
    };
    let Some(i) = card.question() else {
        return;
    };
    let old_why = match set.cards.get(i).and_then(|c| card.drafts.get(&c.id)) {
        Some(Draft::Answer { why, .. }) => why.clone(),
        _ => String::new(),
    };
    match &mut card.input {
        Input::None => {}
        Input::Needless => {
            let place = match key {
                Key::Esc => {
                    card.input = Input::None;
                    return;
                }
                Key::Char('c') => "code",
                Key::Char('t') => "ticket",
                Key::Char('m') => "memory",
                Key::Char('n') => "no-matter",
                _ => return,
            };
            store(app, &set, i, Draft::Needless(place.into()), fx);
        }
        Input::Words(line) => match key {
            Key::Esc => card.input = Input::None,
            Key::Enter => {
                if line.is_blank() {
                    notice(
                        app,
                        "The answer is empty. Type it, or press Esc.",
                        Tone::Info,
                    );
                } else {
                    let text = line.text().trim().to_string();
                    card.input = Input::Why {
                        reply: Reply::Text(text),
                        line: Line::of(&old_why),
                    };
                }
            }
            other => edit(line, other),
        },
        Input::Why { reply, line } => match key {
            Key::Esc => card.input = Input::None,
            Key::Enter | Key::Tab => {
                let why = if key == Key::Enter {
                    line.text().trim().to_string()
                } else {
                    String::new()
                };
                let draft = Draft::Answer {
                    reply: reply.clone(),
                    why,
                };
                store(app, &set, i, draft, fx);
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

/// Stores the draft of the question `i`, writes the drafts file, and goes to the next question
/// with no draft, or to the review page. A send that failed before no longer shows, because
/// the answers changed.
fn store(app: &mut App, set: &QuestionSet, i: usize, draft: Draft, fx: &mut Vec<Effect>) {
    chain::drop_ended(app);
    let Some(card) = card(app) else {
        return;
    };
    let Some(view) = set.cards.get(i) else {
        return;
    };
    card.drafts.insert(view.id.clone(), draft);
    card.input = Input::None;
    card.scroll = 0;
    card.note = None;
    card.page = set.after_draft(i, &card.drafts);
    card.row = if card.page >= set.review() {
        set.missing(&card.drafts).first().copied().unwrap_or(i)
    } else {
        card.page
    };
    fx.push(Effect::SaveDrafts {
        ticket: set.ticket.clone(),
        drafts: card.drafts_of(&set.ticket),
    });
}
