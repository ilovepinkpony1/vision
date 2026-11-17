//! The continue card (section 15.3): Enter on a stopped or paused ticket. Its head names the
//! stored step and what the `[[resume]]` rules found, with the head of the pull request. Under
//! it, the steps of the path show in a window that follows the selection, with the suggestion
//! selected and the stored step marked. Its foot holds the keys, or the steps of the chain of
//! Enter. `v` opens the live view of the ticket, unless the card shows over it.
//!
//! [`rows`] makes the rows of the card in one place, with the window of the steps and the
//! parts that a click hits, so the draw and the mouse see the same card.

use super::canvas::{ink, Canvas};
use super::cards::{card_rect, show, Rows};
use super::chain_rows;
use super::header::{mmss, spinner};
use super::home::window;
use super::theme::T;
use crate::app::chain::{Chain, Owner};
use crate::app::hits::Act;
use crate::app::model::{App, ContinueCard, Screen};
use crate::app::msg::Key;
use crate::app::text::width;
use crate::pipeline::resume::Found;

/// The width of the card.
pub const WIDTH: i32 = 84;
/// The width of a text in the card, from its column 3.
const TEXT: usize = 78;
/// The most lines of the reason of a failed chain that the foot shows.
const FAILURE_LINES: usize = 4;
/// What the read of the card does, while it runs.
pub const READING: &str = "It asks the tracker for the status, and the forge for the pull request, the review, CI, and each env. It also compares the branch with the clone. The steps show when the read ends.";

pub fn draw(app: &App, card: &ContinueCard, c: &mut Canvas) {
    let rows = rows(app, card, c.h);
    let (x, y, w, h) = card_rect(c, WIDTH, rows.rows.len());
    show(c, WIDTH, &format!("Continue {}", card.key), T.primary, rows);
    // The wheel over the card acts as the up and down arrows: one step picks the next step.
    c.wheel(x, y, w, h, 1);
}

/// The rows of the card in a window of `h` rows: the head, the steps that fit, and the foot.
pub(super) fn rows(app: &App, card: &ContinueCard, h: i32) -> Rows {
    let head = head(app, card);
    let foot = foot(app, card);
    // The card keeps a row above and under itself, and its frame, the blank row under its
    // title, and the blank row at its end take four rows. The row above and the row under the
    // steps hold their marks.
    let room = (h - 2 - 4).max(0) as usize;
    let room = room.saturating_sub(head.rows.len() + foot.rows.len() + 2);
    let mut rows = head;
    rows.append(steps(app, card, room));
    rows.append(foot);
    rows
}

/// A text with a capital first letter.
fn capital(text: &str) -> String {
    let mut chars = text.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
}

fn head(app: &App, card: &ContinueCard) -> Rows {
    let mut rows = Rows::default();
    if !card.title.is_empty() {
        rows.text(3, card.title.clone(), ink(T.fg).bold());
        rows.blank();
    }
    // A read after a failed chain names no flag: a step of the chain can have changed them.
    let again = card.view.is_none()
        && app
            .chain_of(&Owner::Resume(card.key.clone()))
            .is_some_and(|c| !c.running());
    let state = match card.loaded() {
        Some(view) if view.stopped && view.paused => "is stopped and paused at",
        _ if again => "is at",
        _ if card.stopped => "is stopped at",
        _ => "is paused at",
    };
    rows.para(
        3,
        &format!("{} {state} the step {}.", card.key, card.stored),
        TEXT,
        ink(T.fg),
    );
    let Some(view) = &card.view else {
        // The read can take some seconds: the card says what it reads and for how long.
        let ran = app.clock.elapsed.saturating_sub(card.since).as_secs();
        rows.blank();
        rows.rows.push(vec![
            (3, spinner(app).to_string(), ink(T.accent)),
            (
                5,
                format!("Vision reads the facts of {} now · {}", card.key, mmss(ran)),
                ink(T.accent).bold(),
            ),
        ]);
        rows.para(5, READING, TEXT - 2, ink(T.dim));
        return rows;
    };
    let view = match view {
        Ok(view) => view,
        Err(error) => {
            rows.para(
                3,
                &format!("Vision could not read where the ticket is: {error}"),
                TEXT,
                ink(T.bad),
            );
            return rows;
        }
    };
    match &view.found {
        None => rows.para(
            3,
            "The adapter has no resume rules, so Vision checks nothing. Pick the step where the ticket goes on.",
            TEXT,
            ink(T.dim),
        ),
        Some(Found::Step {
            step,
            reason,
            back: None,
            ..
        }) => rows.para(
            3,
            &format!("Vision thinks it is at {step}: {reason}."),
            TEXT,
            ink(T.accent).bold(),
        ),
        Some(Found::Step {
            step,
            back: Some(why),
            ..
        }) => {
            let back = view
                .back
                .clone()
                .unwrap_or_else(|| crate::pipeline::resume::back_sentence(&view.stored, why));
            rows.para(
                3,
                &format!("Vision thinks it is at {step}. {back}."),
                TEXT,
                ink(T.warn).bold(),
            )
        }
        Some(Found::Nothing(why)) => rows.para(
            3,
            &format!("Vision found nothing further: {why}."),
            TEXT,
            ink(T.dim),
        ),
        Some(Found::Failed(why)) => rows.para(
            3,
            &format!(
                "Vision could not check the tracker and the pull request, so it suggests nothing: {why}"
            ),
            TEXT,
            ink(T.warn),
        ),
    }
    // Each fact that Vision read, in a fixed order, also the facts that no rule names.
    if !view.facts.is_empty() {
        rows.text(3, "Vision read:", ink(T.dim));
        for fact in &view.facts {
            rows.para(3, &format!("· {fact}"), TEXT, ink(T.dim));
        }
    }
    let clone = view.clone.as_deref().unwrap_or("its clone");
    if let Some(problem) = view.head.problem(view.pr, clone) {
        rows.para(
            3,
            &format!(
                "{}. {}, then press Enter. Nothing moves before that.",
                capital(&problem.what),
                problem.fix
            ),
            TEXT,
            ink(T.warn),
        );
    } else if let Some(note) = view.head.note(view.pr) {
        rows.para(3, &format!("{note}."), TEXT, ink(T.dim));
    }
    rows
}

/// The steps that fit in `room` rows, in a window that follows the selection, between a row
/// above and a row under them that hold the marks of the steps out of view.
fn steps(app: &App, card: &ContinueCard, room: usize) -> Rows {
    let mut rows = Rows::default();
    let mark = |text: String| match text.is_empty() {
        true => Vec::new(),
        false => vec![(WIDTH - 3 - width(&text) as i32, text, ink(T.dim))],
    };
    let Some(view) = card.loaded() else {
        rows.blank();
        return rows;
    };
    let total = view.steps.len();
    let shown = total.min(room.max(1));
    let stored = view.steps.iter().position(|s| *s == view.stored);
    let offset = with_stored(
        window(Some(card.selected), total, shown),
        shown,
        card.selected,
        stored,
    );
    rows.rows.push(mark(if offset > 0 {
        format!("▲ {offset} more")
    } else {
        String::new()
    }));
    let runs = app
        .chain_of(&Owner::Resume(card.key.clone()))
        .is_some_and(Chain::running);
    let name_w = view.steps.iter().map(|s| width(s)).max().unwrap_or(0) as i32;
    let here = if view.stopped {
        "stopped here"
    } else {
        "paused here"
    };
    for (i, step) in view.steps.iter().enumerate().skip(offset).take(shown) {
        let selected = i == card.selected;
        let mut row = Vec::new();
        if selected {
            row.push((3, "▸".to_string(), ink(T.accent).bold()));
        }
        let name = if selected {
            ink(T.fg).bold()
        } else {
            ink(T.dim)
        };
        row.push((5, step.clone(), name));
        let mut x = 5 + name_w + 3;
        let mut marks = Vec::new();
        if *step == view.stored {
            marks.push((here, T.warn));
        }
        if view.suggested() == Some(step.as_str()) {
            marks.push(("Vision suggests", T.accent));
        }
        for (text, color) in marks {
            row.push((x, text.to_string(), ink(color)));
            x += width(text) as i32 + 3;
        }
        let at = rows.rows.len();
        rows.rows.push(row);
        // A click on a step selects it. The selected step continues the ticket, as Enter does,
        // so its click arms first. While the chain runs, a step is no part.
        if !runs {
            let act = if selected {
                Act::Decide(Key::Enter)
            } else {
                Act::Pick(i)
            };
            rows.hit(at, 3, WIDTH - 6, act);
        }
    }
    let below = total.saturating_sub(offset + shown);
    rows.rows.push(mark(if below > 0 {
        format!("▼ {below} more · ↑↓ pick")
    } else {
        String::new()
    }));
    rows
}

/// The first step in view: the window `offset` of the selection, moved so that the stored step
/// shows too when both fit in `shown` rows. A suggestion that goes back selects a step above
/// the stored step, and the person sees both.
fn with_stored(offset: usize, shown: usize, selected: usize, stored: Option<usize>) -> usize {
    let Some(stored) = stored else {
        return offset;
    };
    let (low, high) = (selected.min(stored), selected.max(stored));
    if high - low >= shown {
        offset
    } else if high >= offset + shown {
        high + 1 - shown
    } else if low < offset {
        low
    } else {
        offset
    }
}

/// True when the card shows over the live view of its own ticket.
fn over_live(app: &App, card: &ContinueCard) -> bool {
    matches!(&app.screen, Screen::Live { key } if *key == card.key)
}

fn foot(app: &App, card: &ContinueCard) -> Rows {
    let mut rows = Rows::default();
    // The keys end before the frame of the card.
    let right = WIDTH - 2;
    let live = (!over_live(app, card)).then_some(("v", "live view"));
    if let Some(chain) = app.chain_of(&Owner::Resume(card.key.clone())) {
        chain_rows::rows(app, chain, &mut rows, TEXT, FAILURE_LINES);
        rows.blank();
        if chain.running() {
            let keys: Vec<(&str, &str)> =
                std::iter::once(("esc", "close this card; the action goes on"))
                    .chain(live)
                    .collect();
            rows.keys_within(&keys, &[], None, right);
            return rows;
        }
    }
    let close = if app.chain.is_some() {
        "close"
    } else {
        "cancel"
    };
    match card.chosen() {
        Some(step) => {
            let go = format!("continue at {step}");
            let keys: Vec<(&str, &str)> = [
                ("enter", go.as_str()),
                ("↑↓", "pick a step"),
                ("esc", close),
            ]
            .into_iter()
            .chain(live)
            .collect();
            rows.keys_within(&keys, &[Key::Enter], app.armed_act(), right);
        }
        None => {
            let keys: Vec<(&str, &str)> = std::iter::once(("esc", close)).chain(live).collect();
            rows.keys_within(&keys, &[], None, right);
        }
    }
    rows
}

#[cfg(test)]
mod tests {
    use super::with_stored;

    #[test]
    fn the_window_of_the_steps_keeps_the_stored_step_in_view_when_it_fits() {
        // The stored step under the window of the selection: the window moves down to it.
        assert_eq!(with_stored(0, 11, 9, Some(11)), 1);
        // The stored step above the window of the selection: the window moves up to it.
        assert_eq!(with_stored(5, 4, 8, Some(6)), 5);
        assert_eq!(with_stored(5, 4, 8, Some(5)), 5);
        assert_eq!(with_stored(6, 4, 9, Some(7)), 6);
        assert_eq!(with_stored(6, 4, 8, Some(5)), 5);
        // Both in view already, or too far apart: the window of the selection stays.
        assert_eq!(with_stored(0, 11, 9, Some(4)), 0);
        assert_eq!(with_stored(0, 4, 2, Some(10)), 0);
        assert_eq!(with_stored(3, 4, 6, None), 3);
    }
}
