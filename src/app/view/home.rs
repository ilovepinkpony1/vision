//! Home (`drawHome` of the design): the bar "needs you", the box IN PROGRESS with two lines for
//! each ticket, the box MY TICKETS with the queue of the tracker, the line THIS WEEK, and the
//! keys. MY TICKETS has one section for each status of the tracker ([`crate::app::sections`]):
//! the top line of the box and each line between two sections show the status and its count.
//! A ticket of the queue that can start shows bright, and so does a ticket that is in progress
//! outside Vision, because its start card takes it at its step. Each other ticket of the queue
//! shows faint, with the reason that it cannot start. The boxes take the full width of the
//! window. When a box has more rows than fit, it shows the part around the selection and says
//! how many rows are above and below. The top line of MY TICKETS then shows the status of the
//! first row in view.
//!
//! A click on a ticket selects it, and a click on the selected ticket opens it as Enter does.
//! The wheel over a box moves the selection as the arrows do, a click on the mark of the rows
//! above or below a box selects the next row out of view, and a click on the bar "needs you"
//! acts as `n`.

use super::canvas::{ink, Canvas};
use super::header::{dots, header, keys, spinner};
use super::theme::{Rgb, T};
use crate::app::hits::Act;
use crate::app::model::{App, Held};
use crate::app::msg::Key;
use crate::app::now::{need_line, Tone};
use crate::app::sections::sections;
use crate::app::text::{count, cut, error_line, width, wrap};
use crate::pipeline::commands::PickRow;
use crate::pipeline::money;
use crate::util::time;

pub fn draw(app: &App, c: &mut Canvas) {
    header(app, c, "");
    let mut y = 3;
    if !app.needs().is_empty() {
        needs_bar(app, c, y);
        y += 2;
    }
    let held = app.held();
    let queue = app.queue_rows();
    let kx = key_column(held, &queue);
    let message = queue_message(app, c.w);
    let lines = queue_lines(&queue);
    // The title of the first section is on the top line of the box, so each other line is a
    // row of the box.
    let queue_rows = message
        .as_ref()
        .map_or(lines.len().saturating_sub(1), Vec::len)
        .max(1) as i32;
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
        c.wheel(1, y, c.w - 2, show_held * 3 + 1, 1);
        more_marks(
            c,
            y,
            show_held * 3 + 1,
            (offset, show_held as usize, held.len()),
            |i| held.get(i).map(|h| Act::Ticket(h.view.key.clone())),
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

    // MY TICKETS
    c.put(2, y, "MY TICKETS", ink(T.dim).bold());
    let tail = if app.queue.data.is_some() {
        format!("from the tracker · {} open", queue.len())
    } else {
        "from the tracker".to_string()
    };
    c.right(c.w - 3, y, &tail, ink(T.faint));
    y += 1;
    c.frame(1, y, c.w - 2, show_queue + 2, T.line);
    if let Some(message) = &message {
        for (i, (text, color)) in message.iter().enumerate().take(show_queue as usize) {
            c.put(5, y + 1 + i as i32, text, ink(*color));
        }
    } else {
        c.wheel(1, y, c.w - 2, show_queue + 2, 1);
        let sel = selected
            .filter(|i| *i >= held.len())
            .map(|i| i - held.len());
        queue_box(c, &queue, &lines, (y, show_queue as usize), kx, sel);
    }

    week(app, c);
    let pause = if app.engine.paused { "resume" } else { "pause" };
    keys(
        c,
        &[
            ("↑↓", "move"),
            ("enter", "open or start"),
            ("n", "needs"),
            ("m", "models"),
            ("s", "stats"),
            ("p", pause),
            ("?", "keys"),
            // A narrow window drops the keys from the end: the kill switch stays longer than
            // the reload.
            ("K", "kill switch"),
            ("r", "reload"),
            ("q", "quit"),
        ],
    );
}

/// The bar "needs you": the first need and the count of the others. A click on it acts as the
/// key that it names.
fn needs_bar(app: &App, c: &mut Canvas, y: i32) {
    let needs = app.needs();
    c.click(1, y, c.w - 2, 1, Act::Key(Key::Char('n')));
    c.wipe(1, y, c.w - 2, 1, T.warn_bg);
    c.put(3, y, " needs you ", ink(T.bg).on(T.warn).bold());
    let mut r = c.right(c.w - 4, y, "n needs", ink(T.warn).bold());
    if needs.len() > 1 {
        r = c.right(r - 2, y, &format!("+{} more", needs.len() - 1), ink(T.warn));
    }
    let first = &needs[0];
    // A flag that the mode shadow or hold set is the first write of a verb, not a rule.
    let first_write = (first.kind == crate::snapshot::NeedKind::Flag)
        .then(|| {
            let key = first.ticket.as_deref()?;
            app.held_ticket(key)?.facts.first_write.as_deref()
        })
        .flatten();
    let line = match first_write {
        Some(verb) => format!("the first write {verb} waits for your decision"),
        None => need_line(first),
    };
    let text = match &first.ticket {
        Some(ticket) => format!("{ticket} · {line}"),
        None => line,
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

/// One line of the box MY TICKETS: the title of a section with its count, or a row of the
/// queue by its index.
#[derive(Clone, Debug, PartialEq)]
enum Line {
    Title { status: String, count: usize },
    Row(usize),
}

/// The lines of the box MY TICKETS: the title of each section, then its rows.
fn queue_lines(queue: &[&PickRow]) -> Vec<Line> {
    let mut lines = Vec::new();
    for section in sections(queue) {
        lines.push(Line::Title {
            count: section.rows.len(),
            status: section.status,
        });
        lines.extend(section.rows.map(Line::Row));
    }
    lines
}

/// The inside of the box MY TICKETS from its top line at `y`, with `shown` rows: the rows of
/// the queue, and a line with the title of a section between two sections. The top line shows
/// the title of the section of the first row in view. When the box shows only a part of the
/// lines, the part holds the selected row, and a title at its top moves into the top line, so
/// that no title shows two times. The marks on the border count only the tickets above and
/// below.
fn queue_box(
    c: &mut Canvas,
    queue: &[&PickRow],
    lines: &[Line],
    (y, shown): (i32, usize),
    kx: i32,
    sel: Option<usize>,
) {
    let Some((first, body)) = lines.split_first() else {
        return;
    };
    let at = sel.and_then(|s| body.iter().position(|l| *l == Line::Row(s)));
    let mut offset = window(at, body.len(), shown);
    if matches!(body.get(offset), Some(Line::Title { .. })) {
        offset += 1;
    }
    // `lines[offset]` is the line just before `body[offset]`.
    let top = lines[..=offset]
        .iter()
        .rev()
        .find(|l| matches!(l, Line::Title { .. }))
        .unwrap_or(first);
    title_line(c, y, top, false);
    let end = (offset + shown).min(body.len());
    let ticket = |line: &Line| match line {
        Line::Row(i) => Some(Act::Ticket(queue[*i].key.clone())),
        Line::Title { .. } => None,
    };
    let tickets = |part: &[Line]| part.iter().filter(|l| matches!(l, Line::Row(_))).count();
    marks(
        c,
        y,
        shown as i32 + 2,
        (
            tickets(&body[..offset]),
            body[..offset].iter().rev().find_map(ticket),
        ),
        (tickets(&body[end..]), body[end..].iter().find_map(ticket)),
    );
    for (i, line) in body[offset..end].iter().enumerate() {
        let yy = y + 1 + i as i32;
        match line {
            Line::Title { .. } => title_line(c, yy, line, true),
            Line::Row(r) => queue_row(c, queue[*r], yy, kx, sel == Some(*r)),
        }
    }
}

/// A line of the box MY TICKETS with the title of a section on it, such as
/// `├─ In Review · 3 ───┤`: the top line of the box, or a line inside it between two sections.
fn title_line(c: &mut Canvas, y: i32, line: &Line, inside: bool) {
    let Line::Title { status, count } = line else {
        return;
    };
    if inside {
        let rule = "─".repeat((c.w - 4).max(0) as usize);
        c.put(1, y, &format!("├{rule}┤"), ink(T.line));
    }
    let room = ((c.w - 8) / 2).max(1) as usize;
    let x = c.put(3, y, " ", ink(T.line));
    let x = c.put(x, y, &cut(status, room), ink(T.dim).bold());
    c.put(x, y, &format!(" · {count} "), ink(T.faint));
}

/// How many rows of each box fit in `avail` rows.
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
pub(super) fn window(selected: Option<usize>, total: usize, shown: usize) -> usize {
    match selected {
        Some(sel) if shown > 0 && sel >= shown => (sel + 1 - shown).min(total - shown),
        _ => 0,
    }
}

/// The marks on the border of a box that does not show each row. A click on a mark acts as
/// `pick` gives for the index of a row: the mark above for the last row above the box, and the
/// mark below for the first row below it, so that the box moves to that row.
pub(super) fn more_marks(
    c: &mut Canvas,
    y: i32,
    h: i32,
    (offset, shown, total): (usize, usize, usize),
    pick: impl Fn(usize) -> Option<Act>,
) {
    let below = total.saturating_sub(offset + shown);
    marks(
        c,
        y,
        h,
        (offset, offset.checked_sub(1).and_then(&pick)),
        (below, (below > 0).then(|| pick(offset + shown)).flatten()),
    );
}

/// The marks on the top line and the bottom line of a box at `y` with the height `h`: how
/// many rows are above it and below it. A click on a mark acts as the act that comes with its
/// count.
fn marks(
    c: &mut Canvas,
    y: i32,
    h: i32,
    (above, up): (usize, Option<Act>),
    (below, down): (usize, Option<Act>),
) {
    for (n, arrow, yy, act) in [(above, "▲", y, up), (below, "▼", y + h - 1, down)] {
        if n == 0 {
            continue;
        }
        let mark = format!(" {arrow} {n} more ");
        let x = c.right(c.w - 4, yy, &mark, ink(T.dim)) + 1;
        if let Some(act) = act {
            c.click(x, yy, width(&mark) as i32, 1, act);
        }
    }
}

/// The color of a tone of the state of a ticket.
pub fn tone_color(tone: Tone) -> Rgb {
    match tone {
        Tone::Work => T.accent,
        Tone::Need => T.warn,
        Tone::Wait => T.info,
        Tone::Rest => T.dim,
        Tone::Done => T.ok,
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
    c.click(2, y, c.w - 4, 2, Act::Ticket(view.key.clone()));
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
    let now = app.now_of(&ticket.view, &ticket.facts);
    let tone = tone_color(now.tone);
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
        &cut(&format!("· {}", now.short), room),
        ink(tone),
    );
}

/// A row of the queue. Its section names its status. A ticket that can start and a ticket that
/// is in progress outside Vision show bright, because Enter starts both. Each other ticket
/// shows faint, with the reason that it cannot start.
fn queue_row(c: &mut Canvas, row: &PickRow, y: i32, kx: i32, selected: bool) {
    c.click(2, y, c.w - 4, 1, Act::Ticket(row.key.clone()));
    if selected {
        c.tint(2, y, c.w - 4, T.sel);
        c.put(3, y, "▸", ink(T.accent).bold());
    }
    let starts = row.eligible || row.outside();
    let key_color = if !starts {
        T.faint
    } else if selected {
        T.accent
    } else {
        T.fg
    };
    let mut key_ink = ink(key_color);
    if starts {
        key_ink = key_ink.bold();
    }
    c.put(5, y, &cut(&row.key, (kx - 6) as usize), key_ink);
    let end = c.w - 4;
    let room = if starts {
        end - kx + 1
    } else {
        let reason = cut(&row.reason, ((end - kx) / 2).max(1) as usize);
        c.right(end, y, &reason, ink(T.faint)) - 2 - kx
    };
    let title_color = if starts { T.fg } else { T.faint };
    c.put(
        kx,
        y,
        &cut(&row.title, room.max(1) as usize),
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
