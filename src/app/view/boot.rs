//! The loader (`drawBoot` of the design): the android, the name, the line of the tool, the four
//! real stages, the bar, and the hint. With no color, the android stays away, because its
//! picture needs color. When the home folder is not set up, the loader says what is missing
//! and which command makes it.

use super::android;
use super::canvas::{ink, Canvas};
use super::header::spinner;
use super::theme::T;
use crate::app::model::{App, StageState, STAGE_ENGINE, STAGE_HOME, STAGE_LEDGER, STAGE_TRACKER};
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

pub fn draw(app: &App, c: &mut Canvas) {
    let t = app.clock.seconds();
    let plain = app.colors == ColorMode::Plain;
    // The rows of each part, from the top of the block.
    let (art, title, stages) = if plain {
        (0, 0, 3)
    } else {
        (android::ROWS, 17, 20)
    };
    let bar = stages + 5;
    let hint = if plain || c.h >= 31 { bar + 3 } else { bar + 2 };
    let height = hint + 1;
    let top = ((c.h - height + 1) / 2).max(0);
    if !plain && art > 0 {
        let x = (c.w - android::WIDTH + 1) / 2;
        android::draw(c, x, top, t);
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
