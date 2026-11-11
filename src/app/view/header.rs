//! The parts that each screen shares: the header with the engine and the limits, the footer of
//! keys, the strip of dots of the path, the spinner, and the notice at the bottom.

use std::time::Duration;

use super::canvas::{ink, Canvas};
use super::theme::{Rgb, T};
use crate::app::model::{App, Tone};
use crate::app::msg::JobKind;
use crate::app::text::{cut, width};
use crate::snapshot::LimitView;

/// The frames of the spinner.
const SPIN: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

/// The header shows a pass only after it ran this long.
pub const PASS_SHOWN_AFTER: Duration = Duration::from_secs(2);

/// The frame of the spinner at the time of the model.
pub fn spinner(app: &App) -> &'static str {
    SPIN[(app.clock.elapsed.as_millis() / 90) as usize % SPIN.len()]
}

/// `m:ss`, such as `1:05`.
pub fn mmss(seconds: u64) -> String {
    format!("{}:{:02}", seconds / 60, seconds % 60)
}

/// The header: the gem and the name, a crumb, the engine, the two limits, and the local time.
pub fn header(app: &App, c: &mut Canvas, crumb: Option<(&str, &str)>) {
    let mut x = c.put(1, 0, "◆", ink(T.accent).bold());
    x = c.put(x + 1, 0, "VISION", ink(T.fg).bold());
    let (hour, minute) = app.clock.local_hm();
    let clock = format!("{hour:02}:{minute:02}");
    let mut r = c.w - 2;
    c.right(r, 0, &clock, ink(T.dim));
    r -= width(&clock) as i32 + 2;
    if let Some(local) = &app.local.data {
        r = limit(c, r, "7d", &local.limits.seven_day);
        r = limit(c, r - 2, "5h", &local.limits.five_hour);
    } else {
        r = c.right(r, 0, "7d -", ink(T.dim));
        r = c.right(r - 2, 0, "5h -", ink(T.dim));
    }
    let (state, color) = engine_state(app);
    r = c.right(r - 3, 0, &state, ink(color));
    let left = c.right(r - 1, 0, "engine", ink(T.dim));
    if let Some((key, title)) = crumb {
        x = c.put(x + 1, 0, "▸", ink(T.faint));
        x = c.put(x + 1, 0, &cut(key, 24), ink(T.accent).bold());
        let room = (left - 2 - (x + 2)).clamp(0, 34) as usize;
        if !title.is_empty() && room > 1 {
            c.put(x + 2, 0, &cut(title, room), ink(T.dim));
        }
    }
    c.put(0, 1, &"─".repeat(c.w as usize), ink(T.line));
}

/// The state of the engine: off, a pass that runs, paused, or on.
fn engine_state(app: &App) -> (String, Rgb) {
    if app.off() {
        return ("■ off".into(), T.bad);
    }
    if let Some(running) = &app.engine.running {
        let ran = app.clock.elapsed.saturating_sub(running.since);
        if running.job.kind == JobKind::Pass && ran >= PASS_SHOWN_AFTER {
            return (
                format!("{} pass {}", spinner(app), mmss(ran.as_secs())),
                T.accent,
            );
        }
    }
    if app.engine.paused {
        return ("■ paused".into(), T.warn);
    }
    ("● on".into(), T.ok)
}

/// One limit bar, flush right at `r`: the label, 6 cells of bar, and the percent. Its color
/// says how near the reading is to the stop value. A dim dash means no reading yet. Gives the
/// column before it.
fn limit(c: &mut Canvas, r: i32, label: &str, view: &LimitView) -> i32 {
    let Some(reading) = &view.reading else {
        return c.right(r, 0, &format!("{label} -"), ink(T.dim));
    };
    let part = reading.part.clamp(0.0, 1.0);
    let pct = format!("{:.0}%", part * 100.0);
    let bar = 6;
    let filled = (part * f64::from(bar)).round() as i32;
    let total = width(label) as i32 + 1 + bar + 1 + width(&pct) as i32;
    let x0 = r - total + 1;
    let color = if reading.part >= view.stop_at {
        T.bad
    } else if reading.part >= view.stop_at - 0.15 {
        T.warn
    } else {
        T.ok
    };
    let mut x = c.put(x0, 0, &format!("{label} "), ink(T.dim));
    x = c.put(x, 0, &"━".repeat(filled as usize), ink(color));
    x = c.put(x, 0, &"━".repeat((bar - filled) as usize), ink(T.line));
    c.put(x + 1, 0, &pct, ink(T.fg));
    x0 - 1
}

/// The footer: the keys of a screen, on the second row from the bottom. When the row is too
/// narrow, the keys before the last one go away from the end, so that `q quit` stays.
pub fn keys(c: &mut Canvas, list: &[(&str, &str)]) {
    let y = c.h - 2;
    let wide = |list: &[(&str, &str)]| -> i32 {
        list.iter()
            .map(|(k, l)| width(k) as i32 + 1 + width(l) as i32 + 3)
            .sum::<i32>()
            - 3
            + 1
    };
    let mut list = list.to_vec();
    while list.len() > 1 && wide(&list) > c.w - 1 {
        list.remove(list.len() - 2);
    }
    let mut x = 1;
    for (key, label) in &list {
        x = c.put(x, y, key, ink(T.fg).bold());
        x = c.put(x + 1, y, label, ink(T.dim)) + 3;
    }
}

/// The strip of dots of the path: the passed steps, the current one, and the coming ones. Gives
/// the column after it. A strip wider than `max` cells ends early.
pub fn dots(
    c: &mut Canvas,
    x: i32,
    y: i32,
    steps: usize,
    at: Option<usize>,
    tone: Rgb,
    max: i32,
) -> i32 {
    let mut end = x;
    for i in 0..steps {
        let cx = x + i as i32 * 2;
        if cx + 1 > x + max {
            break;
        }
        let done = at.is_some_and(|a| i < a);
        let current = at == Some(i);
        let (symbol, color) = if current {
            ("◉", tone)
        } else if done {
            ("●", T.ok)
        } else {
            ("○", T.faint)
        };
        c.put(cx, y, symbol, ink(color));
        end = cx + 1;
        if i + 1 < steps && cx + 2 <= x + max {
            let (line, color) = if done {
                ("━", T.ok)
            } else {
                ("─", T.faint)
            };
            c.put(cx + 1, y, line, ink(color));
            end = cx + 2;
        }
    }
    end
}

/// The notice: one line at the bottom while it lasts.
pub fn notice(app: &App, c: &mut Canvas) {
    let Some(notice) = &app.notice else {
        return;
    };
    if notice.until <= app.clock.elapsed {
        return;
    }
    let bg = match notice.tone {
        Tone::Ok => T.ok,
        Tone::Info => T.info,
        Tone::Bad => T.bad,
    };
    let text = format!(" {} ", cut(&notice.text, (c.w - 4).max(1) as usize));
    let x = (c.w - width(&text) as i32) / 2;
    c.put(x, c.h - 1, &text, ink(T.bg).on(bg).bold());
}
