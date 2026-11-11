//! The question card (`mAsk` of the design): the question; the proposal of the Judge first,
//! with its reason and its sources, when it proposed one; why the agent asks; the example;
//! each choice with its letter, its label, and what it means; the recommended choice with its
//! reason; the evidence; and why the Judge was not sure. The foot holds the keys, the line of
//! an answer in the words of the person, the line of the reason, or the question of a needless
//! card.

use super::canvas::ink;
use super::cards::Rows;
use super::need_card::{Parts, TEXT};
use super::theme::{agent_color, T};
use crate::app::line::Line;
use crate::app::model::App;
use crate::app::needs::{enter_reply, Input, NeedsCard, Reply};
use crate::app::text::{cut, width, wrap};
use crate::questions::CardView;

/// The column of a value after its name, such as the text after "Why it asks".
const VALUE: i32 = 17;

pub(super) fn parts(app: &App, card: &NeedsCard, view: &CardView) -> Parts {
    let mut body = Rows::default();
    body.para(3, &view.question, TEXT, ink(T.fg).bold());
    body.blank();
    let judge = agent_color("judge");
    if let Some(p) = &view.proposal {
        let letter = p
            .choice
            .and_then(|n| view.choices.get(n.saturating_sub(1)))
            .and_then(|c| c.letter)
            .map(|l| format!("{l}  "))
            .unwrap_or_default();
        named(
            &mut body,
            "The Judge",
            &format!("proposes {letter}{}", p.answer),
            ink(judge).bold(),
        );
        if !p.reason.is_empty() {
            named(&mut body, "", &p.reason, ink(judge));
        }
        if !p.sources.is_empty() {
            named(&mut body, "Its sources", &p.sources.join(", "), ink(T.fg));
        }
        body.blank();
    }
    if !view.context.is_empty() {
        named(&mut body, "Why it asks", &view.context, ink(T.fg));
    }
    if !view.example.is_empty() {
        named(&mut body, "Example", &view.example, ink(T.fg));
    }
    if !view.context.is_empty() || !view.example.is_empty() {
        body.blank();
    }
    choices(&mut body, card, view);
    body.blank();
    if let Some(rec) = view
        .recommended
        .and_then(|n| view.choices.get(n.saturating_sub(1)))
    {
        let letter = rec.letter.map(|l| format!("{l}  ")).unwrap_or_default();
        let text = if view.why.is_empty() {
            format!("{letter}{}", rec.label)
        } else {
            format!("{letter}{}: {}", rec.label, view.why)
        };
        named(&mut body, "Recommended", &text, ink(T.fg));
    }
    if !view.evidence.is_empty() {
        named(&mut body, "Evidence", &view.evidence, ink(T.dim));
    }
    if view.proposal.is_none() {
        let reason = match &view.judge_reason {
            Some(reason) => format!("was not sure: {reason}"),
            None => "was not sure, so the question came to you.".to_string(),
        };
        named(&mut body, "The Judge", &reason, ink(judge));
    }
    Parts {
        title: format!("question · {} asks at the step {}", view.agent, step(view)),
        body,
        foot: foot(app, card, view),
    }
}

fn step(view: &CardView) -> &str {
    if view.step.is_empty() {
        "-"
    } else {
        &view.step
    }
}

/// A dim name and its text, wrapped from the column of the values.
fn named(rows: &mut Rows, name: &str, text: &str, style: super::canvas::Ink) {
    let mut first = true;
    for line in wrap(text, TEXT - VALUE as usize + 3) {
        let mut row = Vec::new();
        if first && !name.is_empty() {
            row.push((3, name.to_string(), ink(T.dim)));
        }
        first = false;
        row.push((VALUE, line, style));
        rows.rows.push(row);
    }
}

/// The choices: the letter, the label, what it means, and the mark of the recommended one. The
/// arrow marks the choice that Enter takes, or the choice that waits for its reason.
fn choices(rows: &mut Rows, card: &NeedsCard, view: &CardView) {
    let marked = match &card.input {
        Input::Why {
            reply: Reply::Choice(i),
            ..
        } => Some(*i),
        Input::None => enter_reply(view).and_then(|r| match r {
            Reply::Choice(i) => Some(i),
            Reply::Text(_) => None,
        }),
        _ => None,
    };
    let label_w = view
        .choices
        .iter()
        .map(|c| width(&c.label))
        .max()
        .unwrap_or(0)
        .clamp(8, 30);
    let means_x = 8 + label_w as i32 + 2;
    let rec_w = 12;
    let means_w = (TEXT as i32 + 3 - means_x - rec_w).max(10) as usize;
    for (i, choice) in view.choices.iter().enumerate() {
        let selected = marked == Some(i);
        let letter = choice.letter.map(String::from).unwrap_or_default();
        let lines = wrap(&choice.means, means_w);
        let mut row = vec![
            (
                3,
                if selected { "▸" } else { " " }.to_string(),
                ink(T.accent).bold(),
            ),
            (5, letter, ink(T.accent).bold()),
            (8, cut(&choice.label, label_w), {
                let style = ink(T.fg);
                if selected {
                    style.bold()
                } else {
                    style
                }
            }),
        ];
        if let Some(first) = lines.first() {
            row.push((means_x, first.clone(), ink(T.dim)));
        }
        if view.recommended == Some(choice.n) {
            row.push((TEXT as i32 + 3 - 11, "recommended".into(), ink(T.ok)));
        }
        rows.rows.push(row);
        for line in lines.iter().skip(1) {
            rows.rows.push(vec![(means_x, line.clone(), ink(T.dim))]);
        }
    }
}

/// The foot: the keys, or the line that the person types in.
fn foot(_app: &App, card: &NeedsCard, view: &CardView) -> Rows {
    let mut rows = Rows::default();
    match &card.input {
        Input::None => {
            let letters: Vec<String> = view
                .choices
                .iter()
                .filter_map(|c| c.letter.map(String::from))
                .collect();
            let letters = match letters.as_slice() {
                list if list.len() <= 4 => list.join(" "),
                [first, .., last] => format!("{first} to {last}"),
                _ => String::new(),
            };
            let taken = |c: char| view.choices.iter().any(|ch| ch.letter == Some(c));
            let words = if taken('e') { "t" } else { "e" };
            let needless = if taken('k') { "-" } else { "k" };
            let enter = if view.proposal.is_some() {
                Some("take the proposal")
            } else if view.recommended.is_some() {
                Some("take the recommended")
            } else {
                None
            };
            let mut keys: Vec<(&str, &str)> = Vec::new();
            if !letters.is_empty() {
                keys.push((&letters, "answer"));
            }
            if let Some(enter) = enter {
                keys.push(("enter", enter));
            }
            keys.push((words, "your words"));
            keys.push((needless, "needless"));
            if card.len() > 1 {
                keys.push(("←→", "the others"));
            }
            keys.push(("esc", "later"));
            rows.keys(&keys);
        }
        Input::Words(line) => {
            rows.text(3, "Your answer in your own words:", ink(T.accent).bold());
            line_row(&mut rows, line);
            rows.blank();
            rows.keys(&[("enter", "next"), ("esc", "back")]);
        }
        Input::Why { reply, line } => {
            let answer = match reply {
                Reply::Choice(i) => view
                    .choices
                    .get(*i)
                    .map(|c| {
                        let letter = c.letter.map(|l| format!("{l}  ")).unwrap_or_default();
                        format!("{letter}{}", c.label)
                    })
                    .unwrap_or_default(),
                Reply::Text(text) => text.clone(),
            };
            rows.rows.push(vec![
                (3, "Your answer".to_string(), ink(T.dim)),
                (VALUE, answer, ink(T.fg).bold()),
            ]);
            rows.rows.push(vec![
                (3, "Why?".to_string(), ink(T.accent).bold()),
                (
                    8,
                    "Vision keeps the answer with its reason.".to_string(),
                    ink(T.dim),
                ),
            ]);
            line_row(&mut rows, line);
            rows.blank();
            rows.keys(&[
                ("enter", "save the answer with its why"),
                ("tab", "save with no why"),
                ("esc", "back"),
            ]);
        }
        Input::Needless => {
            rows.text(
                3,
                "The question was needless. Where was the answer already?",
                ink(T.accent).bold(),
            );
            rows.blank();
            rows.keys(&[
                ("c", "the code"),
                ("t", "the ticket"),
                ("m", "the memory"),
                ("n", "no matter which choice"),
                ("esc", "back"),
            ]);
        }
    }
    rows
}

/// One line of input with its cursor. A long text shows the part around the cursor.
fn line_row(rows: &mut Rows, line: &Line) {
    let room = TEXT - 2;
    let chars: Vec<char> = line.text().chars().collect();
    let cursor = line.cursor().min(chars.len());
    let mut start = 0;
    while width(&chars[start..cursor].iter().collect::<String>()) + 1 > room {
        start += 1;
    }
    let mut shown = String::new();
    let mut used = 0;
    for ch in &chars[start..] {
        let w = crate::app::text::char_width(*ch);
        if used + w + 1 > room {
            break;
        }
        shown.push(*ch);
        used += w;
    }
    let before = width(&chars[start..cursor].iter().collect::<String>()) as i32;
    let under = chars
        .get(cursor)
        .map_or(" ".to_string(), |ch| ch.to_string());
    rows.rows.push(vec![
        (3, "›".to_string(), ink(T.accent).bold()),
        (5, shown, ink(T.fg)),
        (5 + before, under, ink(T.bg).on(T.accent)),
    ]);
}
