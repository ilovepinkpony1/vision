//! The parts that each screen shares: the header with the back and the forward parts, the names
//! of the places, the engine, and the limits; the footer of keys; the strip of dots of the path;
//! the spinner; and the notice at the bottom.

use std::time::Duration;

use super::canvas::{ink, Canvas};
use super::theme::{Rgb, T};
use crate::app::hits::{hint, Act};
use crate::app::model::{App, Tone};
use crate::app::msg::{JobKind, Key};
use crate::app::nav::Place;
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

/// The longest name of a ticket key in the header, and of a pager.
const KEY_MAX: usize = 24;
const PAGER_MAX: usize = 30;

/// The header: the back and the forward parts, the gem and the name, a name for each place on
/// the way from Home to the place that shows, a title, the engine, the two limits, and the
/// local time.
///
/// A click on `‹` goes back and a click on `›` goes forward, as in a browser. A click on a name
/// goes to its place: `◆ VISION` to Home, the key of a ticket to its live view, and the name of
/// a pager to that pager. The name of the place that shows is no part. While the line of `t` is
/// open, the header has no part, so that a click cannot drop the text of the line.
pub fn header(app: &App, c: &mut Canvas, title: &str) {
    let telling = app.live.as_ref().is_some_and(|l| l.tell.is_some());
    let place = app.place();
    let parts = !telling && app.card.is_none();
    let arrow = |on: bool| {
        if on && parts {
            ink(T.fg).bold()
        } else {
            ink(T.faint)
        }
    };
    let can_back = !app.nav.back.is_empty();
    let can_forward = !app.nav.forward.is_empty();
    c.put(1, 0, "‹", arrow(can_back));
    c.put(4, 0, "›", arrow(can_forward));
    if parts && can_back {
        c.click(0, 0, 3, 1, Act::Back);
    }
    if parts && can_forward {
        c.click(3, 0, 3, 1, Act::Forward);
    }
    let mut x = c.put(7, 0, "◆", ink(T.accent).bold());
    x = c.put(x + 1, 0, "VISION", ink(T.fg).bold());
    if parts && place.as_ref().is_some_and(|p| *p != Place::Home) {
        c.click(7, 0, x - 7, 1, Act::Go(Place::Home));
    }
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
    // The names end two cells before the engine.
    let end = left - 2;
    let names = place.as_ref().map(|p| names(app, p)).unwrap_or_default();
    let last = names.len();
    for (i, (name, max)) in names.iter().enumerate() {
        if end - (x + 3) < 2 {
            break;
        }
        x = c.put(x + 1, 0, "▸", ink(T.faint));
        let shown = cut(name, ((end - (x + 1)).max(0) as usize).min(*max));
        let current = i + 1 == last;
        let style = if current {
            ink(T.accent).bold()
        } else if i == 0 {
            ink(T.fg).bold()
        } else {
            ink(T.fg)
        };
        let start = x + 1;
        x = c.put(start, 0, &shown, style);
        if parts && !current {
            if let Some(target) = place.as_ref().map(|p| p.up_to(i + 1)) {
                c.click(start, 0, x - start, 1, Act::Go(target));
            }
        }
    }
    let room = (end - (x + 2)).clamp(0, 34) as usize;
    if !title.is_empty() && room > 1 {
        c.put(x + 2, 0, &cut(title, room), ink(T.dim));
    }
    c.put(0, 1, &"─".repeat(c.w as usize), ink(T.line));
}

/// The names on the way from Home to a place, each with its longest width: the screen, and
/// for the live view, the key of the ticket and the title of each pager.
fn names(app: &App, place: &Place) -> Vec<(String, usize)> {
    match place {
        Place::Home => Vec::new(),
        Place::Models => vec![("models".to_string(), PAGER_MAX)],
        Place::Stats => vec![("stats".to_string(), PAGER_MAX)],
        Place::Live { key, .. } => {
            let mut out = vec![(key.clone(), KEY_MAX)];
            if let Some(live) = app.live.as_ref().filter(|l| l.key == *key) {
                out.extend(live.pagers.iter().map(|p| (p.title.clone(), PAGER_MAX)));
            }
            out
        }
    }
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
/// narrow, the keys before the last one go away from the end, so that `q quit` stays. A click on
/// a key that shows acts as that key.
pub fn keys(c: &mut Canvas, list: &[(&str, &str)]) {
    keys_with(c, list, &[], None);
}

/// The footer, with the keys of `decide`, which send something or record a decision: a click
/// on such a key arms it, and while `armed` is its part, its label says to click again. The
/// row fits with the labels that show.
pub fn keys_with(c: &mut Canvas, list: &[(&str, &str)], decide: &[Key], armed: Option<&Act>) {
    let y = c.h - 2;
    let mut list: Vec<(&str, String)> = list
        .iter()
        .map(|(key, label)| (*key, hint(0, key, label, decide, armed).label))
        .collect();
    // The row fits when it is at most the width of the window less one.
    let fits = |list: &[(&str, String)]| {
        let refs: Vec<(&str, &str)> = list.iter().map(|(k, l)| (*k, l.as_str())).collect();
        keys_width(&refs) < c.w
    };
    while list.len() > 1 && !fits(&list) {
        list.remove(list.len() - 2);
    }
    let mut x = 1;
    for (key, label) in &list {
        let shown = hint(x, key, label, decide, armed);
        for (hx, hw, act) in shown.parts {
            c.click(hx, y, hw, 1, act);
        }
        let tone = if shown.armed { T.warn } else { T.dim };
        x = c.put(x, y, key, ink(T.fg).bold());
        x = c.put(x + 1, y, &shown.label, ink(tone)) + 3;
    }
}

/// The cells that [`keys`] needs for a list, from the first column of the row. The row fits
/// when it is at most the width of the window less one.
pub fn keys_width(list: &[(&str, &str)]) -> i32 {
    list.iter()
        .map(|(k, l)| width(k) as i32 + 1 + width(l) as i32 + 3)
        .sum::<i32>()
        - 3
        + 1
}

/// The strip of dots of the path: the passed steps, the current one, and the coming ones. Gives
/// the column after it. Each step takes two cells. When the steps need more than `max` cells,
/// the strip shows a window that holds the current step and up to [`AFTER`] steps after it, and
/// `…` stands for the steps that the window cuts off at each end.
pub fn dots(
    c: &mut Canvas,
    x: i32,
    y: i32,
    steps: usize,
    at: Option<usize>,
    tone: Rgb,
    max: i32,
) -> i32 {
    let room = usize::try_from((max + 1) / 2).unwrap_or(0);
    let (from, to) = shown(steps, at, room);
    // A place is a step, or `None` for the mark of a cut.
    let mut places: Vec<Option<usize>> = Vec::new();
    if from > 0 {
        places.push(None);
    }
    places.extend((from..to).map(Some));
    if to < steps {
        places.push(None);
    }
    let done = |i: usize| at.is_some_and(|a| i < a);
    let mut end = x;
    for (n, place) in places.iter().enumerate() {
        let cx = x + n as i32 * 2;
        if cx + 1 > x + max {
            break;
        }
        // A mark stands for the last cut step at the start, or the first cut step at the end.
        let step = match place {
            Some(i) => *i,
            None if n == 0 => from - 1,
            None => to,
        };
        let (symbol, color) = match place {
            None => ("…", if done(step) { T.ok } else { T.faint }),
            Some(i) if at == Some(*i) => ("◉", tone),
            Some(i) if done(*i) => ("●", T.ok),
            Some(_) => ("○", T.faint),
        };
        c.put(cx, y, symbol, ink(color));
        end = cx + 1;
        if n + 1 < places.len() && cx + 2 <= x + max {
            let (line, color) = if done(step) {
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

/// The steps after the current one that a cut strip keeps, so that the next steps show.
const AFTER: usize = 3;

/// The window of steps that a strip of `room` places shows: the first step and the step after
/// the last. All the steps fit, or the window holds the current step, up to [`AFTER`] steps
/// after it, and as many steps before it as fit. Each cut end takes one place for its mark.
fn shown(steps: usize, at: Option<usize>, room: usize) -> (usize, usize) {
    if steps <= room || room < 3 {
        return (0, steps);
    }
    let at = at.map_or(0, |a| a.min(steps - 1));
    let last = (at + AFTER.min(room - 3)).min(steps - 1);
    if last + 2 <= room {
        // A mark at the end only.
        (0, room - 1)
    } else if steps - at < room {
        // A mark at the start only.
        (steps + 1 - room, steps)
    } else {
        // A mark at each end.
        (last + 3 - room, last + 1)
    }
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

#[cfg(test)]
mod tests {
    use super::shown;

    #[test]
    fn a_path_that_fits_shows_each_step() {
        assert_eq!(shown(13, Some(12), 23), (0, 13));
    }

    #[test]
    fn a_long_path_keeps_the_current_step_and_the_next_steps_in_its_window() {
        // Early: a mark at the end only.
        assert_eq!(shown(52, Some(2), 23), (0, 22));
        // Late: a mark at the start only, and the window reaches the last step.
        assert_eq!(shown(52, Some(47), 23), (30, 52));
        // In the middle: a mark at each end, and three steps after the current one.
        let (from, to) = shown(52, Some(25), 23);
        assert_eq!((from, to), (8, 29));
        assert_eq!(to - from, 21, "21 steps and two marks fill the 23 places");
        // No current step: the start of the path.
        assert_eq!(shown(52, None, 23), (0, 22));
        // Each window holds its current step.
        for at in 0..52 {
            let (from, to) = shown(52, Some(at), 23);
            assert!(from <= at && at < to, "{at}: {from}..{to}");
            let places = (to - from) + usize::from(from > 0) + usize::from(to < 52);
            assert!(places <= 23, "{at}: {places} places");
        }
    }
}
