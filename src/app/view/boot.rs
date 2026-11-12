//! The loader (`drawBoot` of the design): the android, the name, the line of the tool, the four
//! real stages, the bar, and the hint. When the home folder has `loader.ans`, its picture shows
//! in place of the android. A picture that is larger than the room above the title shrinks to
//! fit it whole, with the same shape; a picture that fits shows cell for cell. With no color,
//! no picture and no android show, because a picture needs color. When the home folder is not
//! set up, the loader says what is missing and which command makes it.

use std::borrow::Cow;

use super::android;
use super::canvas::{ink, Canvas};
use super::header::spinner;
use super::theme::T;
use crate::app::model::{App, StageState, STAGE_ENGINE, STAGE_HOME, STAGE_LEDGER, STAGE_TRACKER};
use crate::app::picture::Picture;
use crate::app::text::{cut, wrap};
use crate::app::view::theme::ColorMode;

const NAMES: [&str; 4] = ["home folder", "ledger", "tracker", "engine"];
const RUNS: [&str; 4] = [
    "reads the configuration and the adapter",
    "reads the tickets, the turns, and the needs",
    "asks the tracker for your queue",
    "starts",
];

/// The width of the bar, and of the block of the stages.
const BAR: i32 = 42;

/// The rows below the art in a window of `h` rows: the title, the stages, the bar, and the
/// hint.
fn below(h: i32) -> i32 {
    if h >= 31 {
        13
    } else {
        12
    }
}

/// The room for the picture in a window of `w` columns and `h` rows: its columns and its rows.
pub fn room(w: i32, h: i32) -> (usize, usize) {
    ((w - 4).max(0) as usize, (h - below(h)).max(0) as usize)
}

/// The picture fit to the room: the copy that the update made for this room, or a new one
/// when the window changed size and the update has not heard of it yet.
fn fitted<'a>(app: &'a App, picture: &Picture, room: (usize, usize)) -> Cow<'a, Picture> {
    match &app.picture_fit {
        Some(fit) if fit.room == room => Cow::Borrowed(&fit.picture),
        _ => Cow::Owned(picture.fit(room.0, room.1)),
    }
}

pub fn draw(app: &App, c: &mut Canvas) {
    let t = app.clock.seconds();
    let plain = app.colors == ColorMode::Plain;
    let picture = match &app.picture {
        Some(Some(picture)) if !plain => Some(fitted(app, picture, room(c.w, c.h))),
        _ => None,
    };
    let art = if plain {
        0
    } else if let Some(picture) = &picture {
        picture.height() as i32
    } else {
        android::ROWS
    };
    // The rows of each part, from the top of the block.
    let (title, stages) = if plain { (0, 3) } else { (art + 1, art + 4) };
    let bar = stages + 5;
    let hint = if plain || c.h >= 31 { bar + 3 } else { bar + 2 };
    let height = hint + 1;
    let top = ((c.h - height + 1) / 2).max(0);
    match picture {
        Some(picture) => super::picture::draw(c, &picture, top),
        // While the file is read, the android starts to appear: the read takes a few
        // milliseconds, and the android shows almost no pixel in that time.
        None if !plain => {
            let x = (c.w - android::WIDTH + 1) / 2;
            android::draw(c, x, top, t);
        }
        None => {}
    }
    if plain || t > 0.5 {
        c.center(top + title, "V  I  S  I  O  N", ink(T.accent).bold());
        c.center(
            top + title + 1,
            "from a ticket to a pull request that is ready to merge",
            ink(T.dim),
        );
    }
    let x = (c.w - BAR) / 2;
    let text_room = (c.w - (x + 16) - 2).max(1) as usize;
    let mut settled = 0;
    for (i, stage) in app.boot.stages.iter().enumerate() {
        let y = top + stages + i as i32;
        let name = format!("{:<13}", NAMES[i]);
        match &stage.state {
            StageState::Waiting => {
                c.put(x, y, "·", ink(T.faint));
                c.put(x + 2, y, &name, ink(T.faint));
            }
            StageState::Running => {
                c.put(x, y, spinner(app), ink(T.accent));
                c.put(x + 2, y, &name, ink(T.fg).bold());
                c.put(x + 16, y, &cut(RUNS[i], text_room), ink(T.dim));
            }
            StageState::Done(text) => {
                settled += 1;
                c.put(x, y, "✓", ink(T.ok));
                c.put(x + 2, y, &name, ink(T.fg));
                c.put(x + 16, y, &cut(text, text_room), ink(T.dim));
            }
            StageState::Failed(reason) => {
                settled += 1;
                c.put(x, y, "✗", ink(T.bad));
                c.put(x + 2, y, &name, ink(T.fg));
                c.put(x + 16, y, &cut(reason, text_room), ink(T.bad));
            }
        }
    }
    if let Some(blocked) = &app.boot.blocked {
        let room = (c.w - 8).max(1) as usize;
        let mut y = top + bar;
        for line in wrap(&blocked.what, room) {
            c.center(y, &line, ink(T.warn).bold());
            y += 1;
        }
        for line in wrap(&blocked.fix, room) {
            c.center(y, &line, ink(T.fg));
            y += 1;
        }
        c.center((top + hint).max(y + 1), "q leaves", ink(T.faint));
        return;
    }
    let filled = (f64::from(settled) / 4.0 * f64::from(BAR)).round() as i32;
    let mut bx = c.put(x, top + bar, &"━".repeat(filled as usize), ink(T.accent));
    bx = c.put(
        bx,
        top + bar,
        &"━".repeat((BAR - filled) as usize),
        ink(T.line),
    );
    c.put(bx + 2, top + bar, &format!("{settled} of 4"), ink(T.dim));
    let all = [STAGE_HOME, STAGE_LEDGER, STAGE_TRACKER, STAGE_ENGINE]
        .iter()
        .all(|s| app.boot.stages[*s].settled());
    if all {
        c.center(top + hint, "ready", ink(T.ok));
    } else {
        c.center(top + hint, "any key skips", ink(T.faint));
    }
}
