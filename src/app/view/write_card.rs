//! The write cards: an operation that waits for an approval, and the first write of a verb in
//! the mode shadow or hold (`mWrite` of the design). Each one shows the verb, what it does in
//! plain words when the adapter says so, the commit, and what it sends: its arguments as
//! readable rows in a box, the long texts in full. The card of an approval names the paths
//! outside the plan. Then the choices, each with the change of mode that it makes.

use serde_json::Value;

use super::canvas::ink;
use super::cards::Rows;
use super::need_card::{Parts, TEXT};
use super::theme::T;
use crate::app::chain::commit_text;
use crate::app::model::App;
use crate::app::needs::{NeedsCard, WriteItem};
use crate::app::recipes::{auto_words, can_ask, climb};
use crate::app::text::{width, wrap};

/// The card of an operation that waits for `vision approve`.
pub(super) fn approval(_app: &App, _card: &NeedsCard, item: &WriteItem) -> Parts {
    let mut body = Rows::default();
    head(&mut body, item);
    if item.held.is_empty() {
        body.para(
            3,
            &format!(
                "The verb {} is in the mode {}, so it waits for your approval.",
                item.verb, item.mode
            ),
            TEXT,
            ink(T.dim),
        );
    } else {
        body.para(
            3,
            &format!(
                "The diff touches {}, which the approved plan does not name, so it waits for your approval.",
                item.held.join(", ")
            ),
            TEXT,
            ink(T.warn),
        );
    }
    body.blank();
    body.text(3, "What it sends", ink(T.dim));
    args_box(&mut body, &item.args);
    body.blank();
    let mut choices: Vec<(&str, String, String)> = vec![(
        "y",
        "Approve it. Vision sends it now.".into(),
        String::new(),
    )];
    let auto = item.mode == "ask";
    if auto {
        choices.push((
            "a",
            format!(
                "Approve it, and move {} to the mode auto, so that Vision never asks again for {}.",
                item.verb, item.verb
            ),
            String::new(),
        ));
    }
    options(&mut body, &choices);
    if auto && item.leaves {
        body.blank();
        body.para(
            3,
            &format!("In the mode auto: {}", auto_words(item)),
            TEXT,
            ink(T.faint),
        );
    }
    let mut foot = Rows::default();
    let mut keys = vec![("y", "approve")];
    if auto {
        keys.push(("a", "approve, then auto"));
    }
    keys.push(("esc", "later"));
    foot.keys(&keys);
    Parts {
        title: format!("the write {} waits for your approval", item.verb),
        body,
        foot,
    }
}

/// The card of a write that the mode shadow or hold kept from running.
pub(super) fn first_write(_app: &App, _card: &NeedsCard, item: &WriteItem) -> Parts {
    let mut body = Rows::default();
    head(&mut body, item);
    body.para(
        3,
        &format!(
            "The verb {} is in the mode {}, so nothing was sent.",
            item.verb, item.mode
        ),
        TEXT,
        ink(T.dim),
    );
    body.blank();
    body.text(3, "The write that waits", ink(T.dim));
    let intent = item.intent.as_ref();
    let args = intent.and_then(|i| i.get("args")).unwrap_or(&item.args);
    args_box(&mut body, args);
    if let Some(gates) = intent.and_then(|i| i.get("gates")).and_then(Value::as_str) {
        body.para(3, &format!("The gates: {gates}."), TEXT, ink(T.dim));
    }
    body.blank();
    let mut choices: Vec<(&str, String, String)> = Vec::new();
    if can_ask(item) {
        choices.push((
            "y",
            "Send it. Ask me each time for this verb".into(),
            climb(item, false).unwrap_or_default(),
        ));
    }
    choices.push((
        "a",
        "Send it. Never ask again for this verb".into(),
        climb(item, true).unwrap_or_default(),
    ));
    choices.push((
        "n",
        "Do not send. The ticket stays as it is".into(),
        String::new(),
    ));
    options(&mut body, &choices);
    body.blank();
    let mut notes = String::from(
        "y and a change the mode of the verb for each later write of it, one step of the ladder at a time.",
    );
    if item.leaves {
        notes.push_str(&format!(" In the mode auto: {}", auto_words(item)));
    }
    body.para(3, &notes, TEXT, ink(T.faint));
    let mut foot = Rows::default();
    let keys: &str = if can_ask(item) { "y a n" } else { "a n" };
    foot.keys(&[(keys, "choose"), ("esc", "later")]);
    Parts {
        title: format!("the first write of {}", item.verb),
        body,
        foot,
    }
}

/// What the verb does, and the commit.
fn head(body: &mut Rows, item: &WriteItem) {
    if let Some(about) = &item.about {
        body.para(3, about, TEXT, ink(T.fg));
    }
    body.para(
        3,
        &format!(
            "Vision wants to run {} for {} at the commit {}.",
            item.verb,
            item.ticket,
            commit_text(&item.head)
        ),
        TEXT,
        ink(T.fg).bold(),
    );
}

/// The choices of a card: the key, the text, and the change of mode at the right.
fn options(body: &mut Rows, choices: &[(&str, String, String)]) {
    let how_w = choices.iter().map(|c| width(&c.2)).max().unwrap_or(0);
    let text_w = TEXT - 5 - if how_w > 0 { how_w + 2 } else { 0 };
    for (key, text, how) in choices {
        for (i, line) in wrap(text, text_w).into_iter().enumerate() {
            let mut row = Vec::new();
            if i == 0 {
                row.push((5, key.to_string(), ink(T.accent).bold()));
                if !how.is_empty() {
                    row.push((3 + TEXT as i32 - how_w as i32, how.clone(), ink(T.dim)));
                }
            }
            row.push((8, line, ink(T.fg)));
            body.rows.push(row);
        }
    }
}

/// The arguments as rows in a box: each name, and its value in full. A text keeps its lines.
pub(super) fn args_box(body: &mut Rows, args: &Value) {
    let line = T.line;
    let inner = TEXT - 4;
    body.text(3, format!("╭{}╮", "─".repeat(TEXT - 2)), ink(line));
    let mut rows: Vec<(String, String)> = Vec::new();
    match args {
        Value::Object(map) if map.is_empty() => rows.push((
            String::new(),
            "No arguments: the verb acts on the ticket and its commit.".into(),
        )),
        Value::Object(map) => {
            for (key, value) in map {
                rows.push((key.clone(), value_text(value)));
            }
        }
        Value::Null => rows.push((String::new(), "No arguments.".into())),
        other => rows.push((String::new(), value_text(other))),
    }
    let key_w = rows
        .iter()
        .map(|(k, _)| width(k))
        .max()
        .unwrap_or(0)
        .min(16);
    let value_x = if key_w > 0 { 5 + key_w as i32 + 2 } else { 5 };
    let value_w = (inner as i32 - (value_x - 5)).max(10) as usize;
    for (key, value) in rows {
        let mut first = true;
        let lines: Vec<String> = value
            .split('\n')
            .flat_map(|part| {
                let wrapped = wrap(part, value_w);
                if wrapped.is_empty() {
                    vec![String::new()]
                } else {
                    wrapped
                }
            })
            .collect();
        for text in lines {
            let mut row = vec![(3, "│".to_string(), ink(line))];
            if first {
                row.push((5, crate::app::text::cut(&key, key_w), ink(T.dim)));
                first = false;
            }
            row.push((value_x, text, ink(T.fg)));
            row.push((3 + TEXT as i32 - 1, "│".to_string(), ink(line)));
            body.rows.push(row);
        }
    }
    body.text(3, format!("╰{}╯", "─".repeat(TEXT - 2)), ink(line));
}

/// A value in words: a text as it is, `none` for null, and other values as compact JSON.
fn value_text(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        Value::Null => "none".into(),
        other => other.to_string(),
    }
}
