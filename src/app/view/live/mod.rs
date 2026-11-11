//! The live view of one ticket (`drawTicket` of the design): the header with the crumb, the
//! left column with the path and the team, the middle with NOW and the activity, the right
//! column with the facts of the ticket, the limits, and the engine, and the keys that can act in
//! the present state. A pager of the view draws in its place.

pub mod layout;
mod left;
mod middle;
mod right;

use super::canvas::{ink, Canvas};
use super::header::{header, keys, spinner};
use super::theme::T;
use crate::app::live::{Live, TicketData};
use crate::app::model::App;
use crate::app::now::{Kind, Now};
use crate::app::text::{cut, error_line, width};
use crate::pipeline::state::{NEEDS_YOU, PAUSED};
use layout::Layout;

pub fn draw(app: &App, c: &mut Canvas, key: &str) {
    let Some(live) = app.live.as_ref().filter(|l| l.key == key) else {
        header(app, c, Some((key, "")));
        return;
    };
    if let Some(pager) = live.pagers.last() {
        super::pager::draw(app, c, key, pager);
        return;
    }
    let data = live.data.data.as_ref();
    let title = data
        .and_then(|d| d.view.title.clone())
        .or_else(|| app.held_ticket(key).map(|h| app.title_of(h).to_string()))
        .unwrap_or_default();
    header(app, c, Some((key, &title)));
    let l = Layout::of(c.w, c.h);
    for y in 2..=l.bottom() {
        c.put(l.left_line(), y, "│", ink(T.line));
        c.put(l.right_line(), y, "│", ink(T.line));
    }
    c.put(0, c.h - 3, &"─".repeat(c.w as usize), ink(T.line));
    let Some(data) = data else {
        let text = match &live.data.error {
            Some(error) => format!("The ticket did not load: {}", error_line(error)),
            None => format!("{} reads the ticket", spinner(app)),
        };
        let color = if live.data.error.is_some() {
            T.bad
        } else {
            T.dim
        };
        c.put(l.mid_x(), 3, &cut(&text, l.mid_w() as usize), ink(color));
        keys(c, &[("esc", "home"), ("?", "keys")]);
        return;
    };
    let now = app.now_of(&data.view, &data.facts);
    left::draw(app, c, &l, data, &now);
    middle::now(app, c, &l, data, &now, live);
    middle::feed(app, c, &l, live, &now);
    right::draw(app, c, &l, data);
    match &live.tell {
        Some(line) => tell_line(c, data, line),
        None => {
            let list = footer(data, &now);
            let refs: Vec<(&str, &str)> = list.iter().map(|(k, l)| (*k, l.as_str())).collect();
            keys(c, &refs);
        }
    }
}

/// The keys that can act in the present state: the key of the state first.
fn footer(data: &TicketData, now: &Now) -> Vec<(&'static str, String)> {
    let has = |flag: &str| data.view.flags.iter().any(|f| f.flag == flag);
    let done = now.kind == Kind::Done;
    let stopped = now.kind == Kind::Stopped;
    let mut list: Vec<(&'static str, String)> = Vec::new();
    let mut add = |key: &'static str, label: &str| list.push((key, label.to_string()));
    if stopped {
        add("enter", "resume");
    }
    if has(NEEDS_YOU) {
        add("r", "retry");
    } else if has(PAUSED) {
        add("r", "continue");
    }
    if !data.needs.is_empty() {
        add("n", &format!("needs ({})", data.needs.len()));
    }
    if !done {
        match &data.facts.work {
            Some(work) => add("t", &format!("tell {}", work.agent)),
            None => add("t", "tell the next turn"),
        }
    }
    if !done && !stopped && (data.facts.turns > 0 || data.facts.work.is_some()) {
        add("o", "open session");
    }
    if !done && !stopped {
        add("x", "stop");
    }
    add("esc", "home");
    add("l", "log");
    add("p", "plan");
    if data.view.clone.is_some() && !done {
        add("d", "commits");
    }
    add("?", "keys");
    list
}

/// The line of `t` at the bottom: the prompt, the text, and the cursor. A long text shows the
/// part around the cursor.
fn tell_line(c: &mut Canvas, data: &TicketData, line: &crate::app::line::Line) {
    let y = c.h - 2;
    let who = match &data.facts.work {
        Some(work) => format!("tell {}:", work.agent),
        None => "tell the next turn:".to_string(),
    };
    let x = c.put(1, y, &cut(&who, 30), ink(T.accent).bold()) + 1;
    let hint = "enter send · esc cancel";
    let room = (c.w - 3 - width(hint) as i32 - 2 - x).max(4) as usize;
    let chars: Vec<char> = line.text().chars().collect();
    let cursor = line.cursor().min(chars.len());
    // The window of characters that fits, with the cursor in it.
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
    c.put(x, y, &shown, ink(T.fg));
    let before: String = chars[start..cursor].iter().collect();
    let cx = x + width(&before) as i32;
    let under = chars
        .get(cursor)
        .map_or(" ".to_string(), |ch| ch.to_string());
    c.put(cx, y, &under, ink(T.bg).on(T.accent));
    c.right(c.w - 2, y, hint, ink(T.dim));
}

/// The data of the live view, for the other parts of the view.
pub fn data(live: &Live) -> Option<&TicketData> {
    live.data.data.as_ref()
}
