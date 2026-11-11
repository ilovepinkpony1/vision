//! `t` of the live view: a one-line input at the bottom, "tell builder:" with a cursor. Left,
//! right, Home, End, Backspace, Delete, and a paste edit the line; Enter sends it with the
//! action `tell`; Esc cancels. An empty line sends nothing. The notice after the action says
//! where the text went: into the running turn, or kept for the next turn of the ticket.

use super::super::line::Line;
use super::super::model::{App, Tone};
use super::super::msg::{Action, ChildResult, Effect, Key};
use super::{engine, notice};

/// Opens the line.
pub fn open(app: &mut App) {
    let Some(live) = app.live.as_mut() else {
        return;
    };
    let done = live.data.data.as_ref().is_some_and(|d| d.facts.finished);
    if done {
        let key = live.key.clone();
        notice(
            app,
            format!("{key} is done, so no turn reads a message."),
            Tone::Info,
        );
        return;
    }
    live.tell = Some(Line::default());
}

pub fn on_key(app: &mut App, key: Key, _fx: &mut [Effect]) {
    let Some(live) = app.live.as_mut() else {
        return;
    };
    let Some(line) = live.tell.as_mut() else {
        return;
    };
    match key {
        Key::Esc => live.tell = None,
        Key::Enter => {
            let text = line.text().trim().to_string();
            let ticket = live.key.clone();
            live.tell = None;
            if text.is_empty() {
                notice(app, "The line was empty, so nothing was sent.", Tone::Info);
                return;
            }
            engine::queue_action(app, Action::Tell { key: ticket, text });
        }
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

pub fn on_paste(app: &mut App, text: &str) {
    if let Some(line) = app.live.as_mut().and_then(|l| l.tell.as_mut()) {
        line.paste(text);
        app.dirty = true;
    }
}

/// The notice after `vision tell`: where the text went.
pub fn told(app: &mut App, key: &str, result: &ChildResult) {
    let text = if result.stdout.contains("Sent to the running turn") {
        format!("Sent to the running turn of {key}. The agent reads it at its next tool step.")
    } else {
        format!("No turn of {key} runs now, so your text waits for its next turn.")
    };
    notice(app, text, Tone::Ok);
}
