//! Home (`drawHome` of the design): the bar "needs you", the box IN PROGRESS with two lines for
//! each ticket, the box READY TO START with the queue of the tracker, the line THIS WEEK, and
//! the keys. The boxes take the full width of the window. When a box has more rows than fit, it
//! shows the part around the selection and says how many rows are above and below.

use super::canvas::{ink, Canvas};
use super::header::{dots, header, keys, spinner};
use super::theme::{Rgb, T};
use crate::app::model::{App, Held};
use crate::app::text::{count, cut, error_line, width, wrap};
use crate::pipeline::commands::PickRow;
use crate::pipeline::money;
use crate::util::time;

pub fn draw(app: &App, c: &mut Canvas) {
    header(app, c, None);
    let mut y = 3;
    if !app.needs().is_empty() {
        needs_bar(app, c, y);
        y += 2;
    }
    let held = app.held();
    let queue = app.queue_rows();
    let kx = key_column(held, &queue);
    let queue_lines = queue_message(app, c.w);
    let queue_rows = queue_lines.as_ref().map_or(queue.len(), Vec::len).max(1) as i32;
    let held_rows = held.len() as i32;
    let avail = c.h - 6 - y + 1;
    let (show_held, show_queue) = fit(held_rows, queue_rows, avail);

    // IN PROGRESS
    c.put(2, y, "IN PROGRESS", ink(T.dim).bold());
    if !held.is_empty() {
        // What moves the tickets now: the kill switch and the pause come before the engine.
        let what = if app.off() {
            "Vision is off, so no step runs"
        } else if app.engine.paused {
            "the engine is paused, so no new step starts"
        } else if held.len() == 1 {
            "Vision moves it while this window is open"
        } else {
            "Vision moves them while this window is open"
        };
        c.right(
            c.w - 3,
            y,
            &format!("{} · {what}", count(held.len(), "ticket")),
            ink(T.faint),
        );
    }
    y += 1;
    let selected = app.selected_index();
    if held.is_empty() {
        c.frame(1, y, c.w - 2, 3, T.line);
        let text = match (&app.local.data, &app.local.error) {
            (None, Some(error)) => format!("The ledger did not load: {}", error_line(error)),
            (None, None) => format!("{} reads the ledger", spinner(app)),
            _ => "Nothing is in progress. Select a ticket below and press Enter to start it."
                .to_string(),
        };
        c.put(5, y + 1, &cut(&text, (c.w - 10) as usize), ink(T.dim));
        y += 3;
    } else {
        let sel = selected.filter(|i| *i < held.len());
        let offset = window(sel, held.len(), show_held as usize);
        c.frame(1, y, c.w - 2, show_held * 3 + 1, T.line);
        more_marks(
            c,
            y,
            show_held * 3 + 1,
            offset,
            show_held as usize,
            held.len(),
        );
        for (i, ticket) in held
            .iter()
            .enumerate()
            .skip(offset)
            .take(show_held as usize)
        {
            let yy = y + 1 + (i - offset) as i32 * 3;
            held_row(app, c, ticket, yy, kx, sel == Some(i));
        }
        y += show_held * 3 + 1;
    }
    y += 1;

    // READY TO START
    c.put(2, y, "READY TO START", ink(T.dim).bold());
    let ready = queue.iter().filter(|r| r.eligible).count();
    let tail = if app.queue.data.is_some() {
        format!("from the tracker · {ready} can start")
    } else {
        "from the tracker".to_string()
    };
    c.right(c.w - 3, y, &tail, ink(T.faint));
    y += 1;
    c.frame(1, y, c.w - 2, show_queue + 2, T.line);
    if let Some(lines) = &queue_lines {
        for (i, (text, color)) in lines.iter().enumerate().take(show_queue as usize) {
            c.put(5, y + 1 + i as i32, text, ink(*color));
        }
    } else {
        let sel = selected
            .filter(|i| *i >= held.len())
            .map(|i| i - held.len());
        let offset = window(sel, queue.len(), show_queue as usize);
        more_marks(
            c,
            y,
            show_queue + 2,
            offset,
            show_queue as usize,
            queue.len(),
        );
        for (i, row) in queue
            .iter()
            .enumerate()
            .skip(offset)
            .take(show_queue as usize)
        {
            let yy = y + 1 + (i - offset) as i32;
            queue_row(c, row, yy, kx, sel == Some(i));
        }
    }

    week(app, c);
    let pause = if app.engine.paused { "resume" } else { "pause" };
    keys(
        c,
        &[
            ("↑↓", "move"),
            ("enter", "open or start"),
            ("n", "needs"),
            ("p", pause),
            ("r", "reload"),
            ("K", "kill switch"),
            ("?", "keys"),
            ("q", "quit"),
        ],
    );
}

/// The bar "needs you": the first need and the count of the others.
fn needs_bar(app: &App, c: &mut Canvas, y: i32) {
    let needs = app.needs();
    c.wipe(1, y, c.w - 2, 1, T.warn_bg);
    c.put(3, y, " needs you ", ink(T.bg).on(T.warn).bold());
    let mut r = c.right(c.w - 4, y, "n needs", ink(T.warn).bold());
    if needs.len() > 1 {
        r = c.right(r - 2, y, &format!("+{} more", needs.len() - 1), ink(T.warn));
    }
    let first = &needs[0];
    let text = match &first.ticket {
        Some(ticket) => format!("{ticket} · {}", first.text),
        None => first.text.clone(),
    };
    let room = (r - 2 - 16).max(1) as usize;
    c.put(16, y, &cut(&text, room), ink(T.fg));
}

/// The column of the titles: after the longest key, and at least at the column of the design.
fn key_column(held: &[Held], queue: &[&PickRow]) -> i32 {
    let longest = held
        .iter()
        .map(|h| width(&h.view.key))
        .chain(queue.iter().map(|r| width(&r.key)))
        .max()
        .unwrap_or(0) as i32;
    (5 + longest + 1).clamp(15, 30)
}

/// The rows of the queue box when it shows a message instead of tickets: while it loads, after
/// a failure, and when the queue is empty.
fn queue_message(app: &App, w: i32) -> Option<Vec<(String, Rgb)>> {
    let room = (w - 10).max(1) as usize;
    if app.queue.loading() && (app.queue.data.is_none() || app.queue.error.is_some()) {
        return Some(vec![(
            format!("{} asks the tracker for the queue", spinner(app)),
            T.dim,
        )]);
    }
    if let Some(error) = &app.queue.error {
        let mut lines: Vec<(String, Rgb)> = wrap(
            &format!("The queue did not load: {}", error_line(error)),
            room,
        )
        .into_iter()
        .take(2)
        .map(|l| (l, T.bad))
        .collect();
        lines.push(("Press r to try again.".to_string(), T.dim));
        return Some(lines);
    }
    let rows = app.queue.data.as_ref()?;
    if rows.is_empty() {
        return Some(vec![(
            "The queue of the tracker is empty.".to_string(),
            T.dim,
        )]);
    }
    if app.queue_rows().is_empty() {
        return Some(vec![(
            "Each ticket of the queue is in progress.".to_string(),
            T.dim,
        )]);
    }
    None
}

/// How many tickets of each box fit in `avail` rows.
fn fit(held: i32, queue: i32, avail: i32) -> (i32, i32) {
    let held_box = |n: i32| if n == 0 { 3 } else { n * 3 + 1 };
    let need = 1 + held_box(held) + 1 + 1 + queue + 2;
    if need <= avail || held == 0 && queue <= 1 {
        return (held, queue);
    }
    let queue_min = queue.min(3);
    let room = avail - (1 + 1 + 1 + queue_min + 2);
    let show_held = if held == 0 {
        0
    } else {
        ((room - 1) / 3).clamp(1, held)
    };
    let left = avail - (1 + held_box(show_held) + 1 + 1 + 2);
    (show_held, left.clamp(1, queue))
}

/// The first row to show so that the selected row is in a window of `shown` rows.
fn window(selected: Option<usize>, total: usize, shown: usize) -> usize {
    match selected {
        Some(sel) if shown > 0 && sel >= shown => (sel + 1 - shown).min(total - shown),
        _ => 0,
    }
}

/// The marks on the border of a box that does not show each row.
fn more_marks(c: &mut Canvas, y: i32, h: i32, offset: usize, shown: usize, total: usize) {
    if offset > 0 {
        c.right(c.w - 4, y, &format!(" ▲ {offset} more "), ink(T.dim));
    }
    let below = total.saturating_sub(offset + shown);
    if below > 0 {
        c.right(c.w - 4, y + h - 1, &format!(" ▼ {below} more "), ink(T.dim));
    }
}

/// The tone of a ticket: an agent works, it needs the person, it is stopped, or it waits.
pub fn tone(ticket: &Held) -> Rgb {
    if ticket.view.works.is_some() {
        T.accent
    } else if ticket.needs_you {
        T.warn
    } else if ticket.stopped {
        T.dim
    } else {
        T.info
    }
}

/// The time since the take, such as `38m` or `2h 10m`.
pub fn since(app: &App, at: Option<&str>) -> String {
    let Some(at) = at.and_then(|a| time::parse_rfc3339(a).ok()) else {
        return "-".to_string();
    };
    let s = time::seconds_between(at, app.clock.wall).max(0);
    match s {
        0..=59 => format!("{s}s"),
        60..=3599 => format!("{}m", s / 60),
        3600..=86399 => format!("{}h {}m", s / 3600, s % 3600 / 60),
        _ => format!("{}d {}h", s / 86400, s % 86400 / 3600),
    }
}

fn held_row(app: &App, c: &mut Canvas, ticket: &Held, y: i32, kx: i32, selected: bool) {
    let view = &ticket.view;
    if selected {
        c.tint(2, y, c.w - 4, T.sel);
        c.tint(2, y + 1, c.w - 4, T.sel);
        c.put(3, y, "▸", ink(T.accent).bold());
    }
    let key_color = if selected { T.accent } else { T.fg };
    c.put(
        5,
        y,
        &cut(&view.key, (kx - 6) as usize),
        ink(key_color).bold(),
    );
    let right = format!(
        "{}  {:>6}",
        money(view.cost_usd),
        since(app, ticket.taken.as_deref())
    );
    let r = c.right(c.w - 4, y, &right, ink(T.dim));
    let title = app.title_of(ticket);
    if !title.is_empty() {
        c.put(kx, y, &cut(title, (r - 2 - kx).max(1) as usize), ink(T.fg));
    }
    let tone = tone(ticket);
    let steps = app.setup.as_ref().map_or(&[][..], |s| s.steps.as_slice());
    let step = view.step.as_deref().unwrap_or("-");
    let at = steps.iter().position(|s| s == step);
    let end = c.w - 4;
    let mut x = dots(c, kx, y + 1, steps.len(), at, tone, (end - kx) / 2);
    if !steps.is_empty() {
        x += 2;
    }
    x = c.put(x, y + 1, &cut(step, 24), ink(T.fg).bold());
    let room = (end - (x + 1)).max(1) as usize;
    c.put(
        x + 1,
        y + 1,
        &cut(&format!("· {}", view.now), room),
        ink(tone),
    );
}

fn queue_row(c: &mut Canvas, row: &PickRow, y: i32, kx: i32, selected: bool) {
    if selected {
        c.tint(2, y, c.w - 4, T.sel);
        c.put(3, y, "▸", ink(T.accent).bold());
    }
    let key_color = if !row.eligible {
        T.faint
    } else if selected {
        T.accent
    } else {
        T.fg
    };
    let mut key_ink = ink(key_color);
    if row.eligible {
        key_ink = key_ink.bold();
    }
    c.put(5, y, &cut(&row.key, (kx - 6) as usize), key_ink);
    let tail = if row.eligible {
        row.status.clone()
    } else {
        row.reason.clone()
    };
    let tail = cut(&tail, ((c.w - 4 - kx) / 2).max(1) as usize);
    let tail_color = if row.eligible { T.dim } else { T.faint };
    let r = c.right(c.w - 4, y, &tail, ink(tail_color));
    let title_color = if row.eligible { T.fg } else { T.faint };
    c.put(
        kx,
        y,
        &cut(&row.title, (r - 2 - kx).max(1) as usize),
        ink(title_color),
    );
}

/// The line THIS WEEK: the cost, the tickets that finished, and the questions that the memory
/// and the Judge answered without the person.
fn week(app: &App, c: &mut Canvas) {
    let y = c.h - 5;
    c.put(2, y, "THIS WEEK", ink(T.dim).bold());
    let Some(local) = &app.local.data else {
        return;
    };
    let week = &local.week;
    let mut x = c.put(14, y, &money(week.cost_usd), ink(T.fg).bold());
    x = c.put(x + 1, y, "spent", ink(T.dim)) + 4;
    x = c.put(x, y, &week.finished.to_string(), ink(T.fg).bold());
    let noun = if week.finished == 1 {
        "ticket finished"
    } else {
        "tickets finished"
    };
    x = c.put(x + 1, y, noun, ink(T.dim)) + 4;
    if week.questions == 0 {
        c.put(x, y, "no question came", ink(T.dim));
    } else {
        x = c.put(
            x,
            y,
            &format!("{} of {}", week.without_you, week.questions),
            ink(T.ok).bold(),
        );
        c.put(x + 1, y, "questions answered without you", ink(T.dim));
    }
}
