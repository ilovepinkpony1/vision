//! The question form (`mAsk` of the design): the questions of one asking turn, or of one ask
//! step, in one card. The head names who asks and the page, such as "Question 2 of 3", and a
//! row under it shows each question with its mark: a draft, a proposal of the Judge, or open.
//!
//! A question page shows the question; the draft of the person; the proposal of the Judge, with
//! its reason and its sources; why the agent asks; the example; each choice with its letter, its
//! label, and what it means; the recommended choice with its reason; the evidence; and why the
//! Judge was not sure. The foot holds the keys, the line of an answer in the words of the
//! person, the line of the reason, or the question of a needless card.
//!
//! The review page lists each question in two lines, with its draft and its reason, and marks
//! each question with no draft. Its foot holds the send.
//!
//! Each part that a click can hit (a tab of the question row, a choice, a row of the review
//! page, the send) is placed here, in the same code that draws it.

use super::canvas::{ink, Ink};
use super::cards::Rows;
use super::need_card::{Parts, DISCUSS, TEXT};
use super::theme::{agent_color, T};
use crate::app::chain::{End, Owner};
use crate::app::hits::{Act, AGAIN};
use crate::app::line::Line;
use crate::app::model::App;
use crate::app::msg::Key;
use crate::app::needs::{enter_reply, Draft, Input, Mark, NeedsCard, QuestionSet, Reply, Target};
use crate::app::recipes::needless_words;
use crate::app::text::{count, cut, width, wrap};
use crate::questions::CardView;

/// The column of a value after its name, such as the text after "Why it asks".
const VALUE: i32 = 17;

pub(super) fn parts(app: &App, card: &NeedsCard, set: &QuestionSet) -> Parts {
    let mut parts = match card.question() {
        Some(i) => question(app, card, set, i),
        None => review(app, card, set),
    };
    tabs(card, set, &mut parts);
    parts
}

/// The question row: a tab for each question with its mark, and the tab of the review page.
/// The tab that shows has the color of the accent.
fn tabs(card: &NeedsCard, set: &QuestionSet, parts: &mut Parts) {
    let review = set.review();
    let tab = |i: usize, words: bool| -> (String, Ink) {
        if i == review {
            let ready = set.missing(&card.drafts).is_empty();
            let color = if ready { T.ok } else { T.dim };
            return ("review".to_string(), ink(color));
        }
        let (symbol, word, color) = match set.mark(i, &card.drafts) {
            Mark::Drafted => ("✓", "draft", T.ok),
            Mark::Proposed => ("◇", "proposed", agent_color("judge")),
            Mark::Open => ("·", "open", T.dim),
        };
        let text = if words {
            format!("{} {symbol} {word}", i + 1)
        } else {
            format!("{} {symbol}", i + 1)
        };
        (text, ink(color))
    };
    let all = |words: bool| -> Vec<(String, Ink)> { (0..=review).map(|i| tab(i, words)).collect() };
    let wide =
        |list: &[(String, Ink)]| -> usize { list.iter().map(|(t, _)| width(t) + 4).sum::<usize>() };
    let mut list = all(true);
    if wide(&list) > TEXT {
        list = all(false);
    }
    let mut row = Vec::new();
    let mut x = 3;
    for (i, (text, style)) in list.into_iter().enumerate() {
        let shown = i == card.page.min(review);
        let text = if shown { format!(" {text} ") } else { text };
        let style = if shown {
            ink(T.bg).on(T.accent).bold()
        } else {
            style
        };
        let w = width(&text) as i32;
        let at = parts.head.rows.len();
        parts.head.hit(at, x, w, Act::Form(Target::Page(i)));
        row.push((x, text, style));
        x += w + 3;
    }
    parts.head.rows.push(row);
}

/// The title of a page: who asks, and the page.
fn title(set: &QuestionSet, page: &str) -> String {
    format!("{} · {page}", set.asker())
}

fn question(app: &App, card: &NeedsCard, set: &QuestionSet, i: usize) -> Parts {
    let view = &set.cards[i];
    let draft = card.drafts.get(&view.id);
    let mut parts = Parts::default();
    let body = &mut parts.body;
    body.para(3, &view.question, TEXT, ink(T.fg).bold());
    body.blank();
    if let Some(draft) = draft {
        let (answer, why) = draft_words(view, draft);
        named(body, "Your draft", &answer, ink(T.ok).bold());
        if !why.is_empty() {
            named(body, "Its reason", &why, ink(T.fg));
        }
        body.blank();
    }
    let judge = agent_color("judge");
    if let Some(p) = &view.proposal {
        let letter = p
            .choice
            .and_then(|n| view.choices.get(n.saturating_sub(1)))
            .and_then(|c| c.letter)
            .map(|l| format!("{l}  "))
            .unwrap_or_default();
        named(
            body,
            "The Judge",
            &format!("proposes {letter}{}", p.answer),
            ink(judge).bold(),
        );
        if !p.reason.is_empty() {
            named(body, "", &p.reason, ink(judge));
        }
        if !p.sources.is_empty() {
            named(body, "Its sources", &p.sources.join(", "), ink(T.fg));
        }
        body.blank();
    }
    if !view.context.is_empty() {
        named(body, "Why it asks", &view.context, ink(T.fg));
    }
    if !view.example.is_empty() {
        named(body, "Example", &view.example, ink(T.fg));
    }
    if !view.context.is_empty() || !view.example.is_empty() {
        body.blank();
    }
    choices(&mut parts, card, view, draft);
    let body = &mut parts.body;
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
        named(body, "Recommended", &text, ink(T.fg));
    }
    if !view.evidence.is_empty() {
        named(body, "Evidence", &view.evidence, ink(T.dim));
    }
    if view.proposal.is_none() {
        let reason = match &view.judge_reason {
            Some(reason) => format!("was not sure: {reason}"),
            None => "was not sure, so the question came to you.".to_string(),
        };
        named(body, "The Judge", &reason, ink(judge));
    }
    parts.title = title(set, &format!("Question {} of {}", i + 1, set.len()));
    parts.foot = foot(app, card, set, view, draft);
    parts
}

/// The answer and the reason of a draft, in words.
fn draft_words(view: &CardView, draft: &Draft) -> (String, String) {
    match draft {
        Draft::Answer { reply, why } => (reply_words(view, reply), why.clone()),
        Draft::Needless(place) => (
            format!("needless: {}", needless_words(place)),
            String::new(),
        ),
    }
}

/// A reply in words: the letter and the label of a choice, or the text.
fn reply_words(view: &CardView, reply: &Reply) -> String {
    match reply {
        Reply::Choice(i) => view
            .choices
            .get(*i)
            .map(|c| {
                let letter = c.letter.map(|l| format!("{l}  ")).unwrap_or_default();
                format!("{letter}{}", c.label)
            })
            .unwrap_or_default(),
        Reply::Text(text) => text.clone(),
    }
}

/// A dim name and its text, wrapped from the column of the values.
fn named(rows: &mut Rows, name: &str, text: &str, style: Ink) {
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
/// arrow marks the choice that Enter takes (the draft, else the proposal or the recommended
/// choice), or the choice that waits for its reason.
fn choices(parts: &mut Parts, card: &NeedsCard, view: &CardView, draft: Option<&Draft>) {
    let marked = match (&card.input, draft) {
        (
            Input::Why {
                reply: Reply::Choice(i),
                ..
            },
            _,
        ) => Some(*i),
        (
            Input::None,
            Some(Draft::Answer {
                reply: Reply::Choice(i),
                ..
            }),
        ) => Some(*i),
        (Input::None, None) => enter_reply(view).and_then(|r| match r {
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
        let rows = 1 + lines.len().saturating_sub(1);
        for line in 0..rows {
            let at = parts.body.rows.len() + line;
            parts
                .body
                .hit(at, 3, TEXT as i32, Act::Form(Target::Choice(i)));
        }
        parts.body.rows.push(row);
        for line in lines.iter().skip(1) {
            parts
                .body
                .rows
                .push(vec![(means_x, line.clone(), ink(T.dim))]);
        }
    }
}

/// The foot of a question page: the keys, or the line that the person types in.
fn foot(
    _app: &App,
    card: &NeedsCard,
    set: &QuestionSet,
    view: &CardView,
    draft: Option<&Draft>,
) -> Rows {
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
            let review = if taken('r') { "s" } else { "r" };
            let enter = if let Some(Draft::Needless(_)) = draft {
                Some("change the place")
            } else if draft.is_some() {
                Some("change the reason")
            } else if view.proposal.is_some() {
                Some("take the proposal")
            } else if view.recommended.is_some() {
                Some("take the recommended")
            } else {
                None
            };
            let numbers = match set.len() {
                1 => None,
                2 => Some("1 2".to_string()),
                n => Some(format!("1-{}", n.min(9))),
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
            keys.push(("←→", "the pages"));
            if let Some(numbers) = &numbers {
                keys.push((numbers, "a question"));
            }
            keys.push((review, "review"));
            keys.push(DISCUSS);
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
            rows.rows.push(vec![
                (3, "Your answer".to_string(), ink(T.dim)),
                (VALUE, reply_words(view, reply), ink(T.fg).bold()),
            ]);
            rows.rows.push(vec![
                (3, "Why?".to_string(), ink(T.accent).bold()),
                (
                    8,
                    "The reason goes with the answer when you send the form.".to_string(),
                    ink(T.dim),
                ),
            ]);
            line_row(&mut rows, line);
            rows.blank();
            rows.keys(&[
                ("enter", "keep it with this why"),
                ("tab", "keep it with no why"),
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

/// The review page: each question in two lines with its draft and its reason, a mark for each
/// question with no draft, and the send in the foot.
fn review(app: &App, card: &NeedsCard, set: &QuestionSet) -> Parts {
    let mut parts = Parts::default();
    let whom = if set.ask_step().is_some() {
        format!("answers.md of {}", set.ticket)
    } else {
        set.cards
            .first()
            .map_or("the agent".to_string(), |c| c.agent.clone())
    };
    let intro = if set.len() == 1 {
        format!("This answer goes to {whom} when you send it. Nothing went to Vision yet.")
    } else {
        format!("These answers go to {whom} when you send them. Nothing went to Vision yet.")
    };
    parts.body.para(3, &intro, TEXT, ink(T.dim));
    parts.body.blank();
    let refused = refusal(app, set);
    let row_w = TEXT as i32 - 3;
    for (i, view) in set.cards.iter().enumerate() {
        let selected = i == card.row;
        let first = parts.body.rows.len();
        parts.body.rows.push(vec![
            (
                1,
                if selected { "▸" } else { " " }.to_string(),
                ink(T.accent).bold(),
            ),
            (3, (i + 1).to_string(), ink(T.accent).bold()),
            (6, cut(&view.question, row_w as usize), {
                let style = ink(T.fg);
                if selected {
                    style.bold()
                } else {
                    style
                }
            }),
        ]);
        let (text, style) = match card.drafts.get(&view.id) {
            Some(draft) => {
                let (answer, why) = draft_words(view, draft);
                let text = if why.is_empty() {
                    answer
                } else {
                    format!("{answer} · {why}")
                };
                (text, ink(T.ok))
            }
            None => (
                format!("no draft yet · press {} to answer it", i + 1),
                ink(T.warn).bold(),
            ),
        };
        parts
            .body
            .rows
            .push(vec![(6, cut(&text, row_w as usize), style)]);
        if let Some((n, reason)) = &refused {
            if *n == i {
                parts.body.rows.push(vec![(
                    6,
                    cut(&format!("refused: {reason}"), row_w as usize),
                    ink(T.bad).bold(),
                )]);
            }
        }
        for row in first..parts.body.rows.len() {
            parts
                .body
                .hit(row, 1, TEXT as i32 + 2, Act::Form(Target::Row(i)));
        }
    }
    let missing = set.missing(&card.drafts);
    let (state, style) = match missing.as_slice() {
        [] => (
            match set.len() {
                1 => "The question has a draft. s sends it.".to_string(),
                n => format!("Each of the {n} questions has a draft. s sends them in one action."),
            },
            ink(T.ok),
        ),
        list => {
            let numbers: Vec<String> = list.iter().map(|i| (i + 1).to_string()).collect();
            (
                format!(
                    "{} no draft yet: {}. The send needs a draft for each question.",
                    match list.len() {
                        1 => "1 question has".to_string(),
                        n => format!("{n} questions have"),
                    },
                    numbers.join(", ")
                ),
                ink(T.warn),
            )
        }
    };
    parts.foot.para(3, &state, TEXT, style);
    let send = match set.len() {
        1 => "send the answer".to_string(),
        n => format!("send the {n} answers"),
    };
    // The send sends the answers, so a click on it arms first, and its label says to click
    // again while it is armed.
    let armed = app.armed_act() == Some(&Act::Form(Target::Send));
    let s = Act::Decide(Key::Char('s'));
    let label = if armed {
        AGAIN.to_string()
    } else {
        send.clone()
    };
    let at = parts.foot.rows.len();
    parts.foot.keys_with(
        &[
            ("s", &send),
            ("enter", "open the question"),
            ("↑↓", "choose"),
            ("←→", "the pages"),
            DISCUSS,
            ("esc", "later"),
        ],
        &[Key::Char('s')],
        armed.then_some(&s),
    );
    // The send is the first key of its row. It comes after the keys, so that it wins over the
    // part of the key `s` at the same place.
    let w = (width("s") + 1 + width(&label)) as i32;
    parts.foot.hit(at, 3, w, Act::Form(Target::Send));
    parts.title = title(set, &format!("your {}", count(set.len(), "answer")));
    parts
}

/// The question that a failed send names, with the reason, from the end of the chain of the
/// form.
fn refusal(app: &App, set: &QuestionSet) -> Option<(usize, String)> {
    let chain = app.chain_of(&Owner::Need(format!("questions:{}", set.turn)))?;
    let Some(End::Failed { text, .. }) = &chain.end else {
        return None;
    };
    let i = set.named_in(text)?;
    let reason = text
        .split_once("was refused: ")
        .map_or(text.as_str(), |(_, reason)| reason);
    Some((i, reason.to_string()))
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
