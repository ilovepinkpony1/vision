//! The live view of one ticket (`drawTicket` of the design): the header with the crumb, the
//! left column with the path and the team, the middle with NOW and the activity, the right
//! column with the facts of the ticket, the limits, and the engine, and the keys that can act in
//! the present state. A pager of the view draws in its place.

pub mod layout;
mod left;
mod middle;
mod right;

use super::canvas::{ink, Canvas};
use super::header::{header, keys, keys_width, spinner};
use super::theme::T;
use crate::app::hits::hint;
use crate::app::live::{Live, TicketData};
use crate::app::model::App;
use crate::app::msg::Key;
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
        Some(line) => tell_line(app, c, data, line),
        None => {
            let list = footer(data, &now, c.w - 1);
            let refs: Vec<(&str, &str)> = list.iter().map(|(k, l)| (*k, l.as_str())).collect();
            keys(c, &refs);
        }
    }
}

/// The keys that can act in the present state: the key of the state first. The row fits
/// `room` cells, see [`fit`].
fn footer(data: &TicketData, now: &Now, room: i32) -> Vec<(&'static str, String)> {
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
    add("l", "log and tool calls");
    add("f", "filter");
    add("p", "plan");
    if data.view.clone.is_some() && !done {
        add("d", "commits");
    }
    add("?", "keys");
    fit(list, room)
}

/// Makes the keys of the footer fit `room` cells. The hints that the person needs least change
/// first, in this order, until the row fits: `d` and `p` go, the hint of `t` gets short, `o`
/// and `x` go, and the hint of `l` gets short. So `f filter` stays at each width that the app
/// allows, and `?` lists each key.
fn fit(mut list: Vec<(&'static str, String)>, room: i32) -> Vec<(&'static str, String)> {
    let wide = |list: &[(&'static str, String)]| {
        let refs: Vec<(&str, &str)> = list.iter().map(|(k, l)| (*k, l.as_str())).collect();
        keys_width(&refs)
    };
    let steps: [(&str, Option<&str>); 6] = [
        ("d", None),
        ("p", None),
        ("t", Some("tell")),
        ("o", None),
        ("x", None),
        ("l", Some("log")),
    ];
    for (key, short) in steps {
        if wide(&list) <= room {
            break;
        }
        match short {
            Some(label) => {
                for item in list.iter_mut().filter(|(k, _)| *k == key) {
                    item.1 = label.to_string();
                }
            }
            None => list.retain(|(k, _)| *k != key),
        }
    }
    list
}

/// The line of `t` at the bottom: the prompt, the text, and the cursor. A long text shows the
/// part around the cursor.
fn tell_line(app: &App, c: &mut Canvas, data: &TicketData, line: &crate::app::line::Line) {
    let y = c.h - 2;
    let who = match &data.facts.work {
        Some(work) => format!("tell {}:", work.agent),
        None => "tell the next turn:".to_string(),
    };
    let x = c.put(1, y, &cut(&who, 30), ink(T.accent).bold()) + 1;
    // Enter sends the text, so a click on `enter send` arms first, and its label says to click
    // again while it is armed. The hint ends at the same column, so `esc cancel` stays.
    let send = hint(0, "enter", "send", &[Key::Enter], app.armed_act());
    let before_esc = format!("enter {} · ", send.label);
    let hint_text = format!("{before_esc}esc cancel");
    let room = (c.w - 3 - width(&hint_text) as i32 - 2 - x).max(4) as usize;
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
    // A click on `esc cancel` acts as that key.
    let hx = c.right(c.w - 2, y, &hint_text, ink(T.dim)) + 1;
    if send.armed {
        c.put(hx + width("enter ") as i32, y, &send.label, ink(T.warn));
    }
    for (px, pw, act) in hint(hx, "enter", &send.label, &[Key::Enter], app.armed_act()).parts {
        c.click(px, y, pw, 1, act);
    }
    c.hint(hx + width(&before_esc) as i32, y, "esc", "cancel");
}

/// The data of the live view, for the other parts of the view.
pub fn data(live: &Live) -> Option<&TicketData> {
    live.data.data.as_ref()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn list(items: &[(&'static str, &str)]) -> Vec<(&'static str, String)> {
        items.iter().map(|(k, l)| (*k, l.to_string())).collect()
    }

    fn wide(list: &[(&'static str, String)]) -> i32 {
        let refs: Vec<(&str, &str)> = list.iter().map(|(k, l)| (*k, l.as_str())).collect();
        keys_width(&refs)
    }

    #[test]
    fn the_footer_keeps_f_at_each_width_that_the_app_allows() {
        let running = list(&[
            ("r", "continue"),
            ("n", "needs (12)"),
            ("t", "tell the next turn"),
            ("o", "open session"),
            ("x", "stop"),
            ("esc", "home"),
            ("l", "log and tool calls"),
            ("f", "filter"),
            ("p", "plan"),
            ("d", "commits"),
            ("?", "keys"),
        ]);
        let stopped = list(&[
            ("enter", "resume"),
            ("r", "continue"),
            ("n", "needs (12)"),
            ("t", "tell the next turn"),
            ("esc", "home"),
            ("l", "log and tool calls"),
            ("f", "filter"),
            ("p", "plan"),
            ("d", "commits"),
            ("?", "keys"),
        ]);
        for full in [running, stopped] {
            for w in 100..=200 {
                let room = w - 1;
                let fitted = fit(full.clone(), room);
                assert!(wide(&fitted) <= room, "{w}: {fitted:?}");
                assert!(
                    fitted.contains(&("f", "filter".to_string())),
                    "{w}: {fitted:?}"
                );
                assert_eq!(fitted.last(), full.last(), "{w}");
            }
            assert_eq!(
                fit(full.clone(), 199),
                full,
                "a wide window keeps each hint"
            );
        }
    }

    #[test]
    fn the_hints_that_the_person_needs_least_change_first() {
        let full = list(&[
            ("r", "retry"),
            ("n", "needs (1)"),
            ("t", "tell the next turn"),
            ("o", "open session"),
            ("x", "stop"),
            ("esc", "home"),
            ("l", "log and tool calls"),
            ("f", "filter"),
            ("p", "plan"),
            ("d", "commits"),
            ("?", "keys"),
        ]);
        // At 120 columns: `d` and `p` go, and the hint of `t` gets short.
        assert_eq!(
            fit(full.clone(), 119),
            list(&[
                ("r", "retry"),
                ("n", "needs (1)"),
                ("t", "tell"),
                ("o", "open session"),
                ("x", "stop"),
                ("esc", "home"),
                ("l", "log and tool calls"),
                ("f", "filter"),
                ("?", "keys"),
            ])
        );
        // At 130 columns, only `d` and `p` go.
        let at_130 = fit(full, 129);
        assert!(at_130.contains(&("t", "tell the next turn".to_string())));
        assert!(!at_130.iter().any(|(k, _)| *k == "p" || *k == "d"));
    }
}
