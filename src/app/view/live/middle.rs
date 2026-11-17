//! The middle of the live view: NOW, the state of the ticket in the words of the app, and
//! ACTIVITY, the feed with the newest line at the bottom, under a row of tabs that show one
//! kind of line with the count of each kind.
//!
//! NOW draws an agent or the Judge with its small head in its color and moving eyes, the model
//! and the effort, a spinner, the running time, the tool calls, and the output tokens. A ticket
//! that needs the person gets a box in the warning color with a badge and the key. Each other
//! state gets the resting head and its lines.
//!
//! A click on the box of a ticket that needs the person acts as its key, and a click on a tab
//! shows that tab. The marks `‹` and `›` show the tab before and after, as `F` and `f` do, the
//! mark of the lines below acts as `G`, and the wheel over the activity scrolls it as the arrows
//! do. While the line of `t` is open, the box and the mark of the lines below are no parts,
//! because their keys go into the line.

use super::layout::{Layout, ACTIVITY_Y, FEED_TEXT_DX, FEED_Y, TABS_Y};
use crate::app::hits::{keys_of, Act};
use crate::app::live::{Live, TicketData};
use crate::app::model::App;
use crate::app::msg::Key;
use crate::app::now::{mmss, Kind, Now};
use crate::app::text::{count, cut, width, wrap};
use crate::app::view::canvas::{ink, Canvas};
use crate::app::view::face;
use crate::app::view::header::spinner;
use crate::app::view::home::tone_color;
use crate::app::view::theme::{agent_color, hue, ColorMode, T};

/// The column of the text of NOW, after the head.
const TEXT_DX: i32 = 11;
/// The lines that one step of the wheel scrolls the activity.
const WHEEL_LINES: usize = 3;

pub fn now(app: &App, c: &mut Canvas, l: &Layout, data: &TicketData, now: &Now, live: &Live) {
    let x = l.mid_x();
    let w = l.mid_w();
    c.put(x, 2, "NOW", ink(T.dim).bold());
    let room = (w - TEXT_DX) as usize;
    let tx = x + TEXT_DX;
    let plain = app.colors == ColorMode::Plain;
    let t = app.clock.seconds();
    match now.kind {
        Kind::Needs | Kind::Paused => {
            // While the line of `t` is open, a key goes into the line: then the box is no part.
            if let Some((key, _)) = now.key.filter(|_| live.tell.is_none()) {
                if let [(_, _, k)] = keys_of(key).as_slice() {
                    c.click(x, 3, w, 5, Act::Key(*k));
                }
            }
            c.wipe(x, 3, w, 5, T.warn_bg);
            c.frame(x, 3, w, 5, T.warn);
            let badge = if now.kind == Kind::Paused {
                " paused "
            } else {
                " needs you "
            };
            let bx = c.put(x + 2, 4, badge, ink(T.bg).on(T.warn).bold());
            let mut r = x + w - 3;
            if let Some((key, label)) = now.key {
                r = c.right(r, 4, &format!("{key} {label}"), ink(T.warn).bold()) - 2;
            }
            c.put(
                bx + 1,
                4,
                &cut(&now.title, (r - bx - 1).max(1) as usize),
                ink(T.fg).bold(),
            );
            for (i, line) in now.lines.iter().take(2).enumerate() {
                c.put(
                    x + 2,
                    5 + i as i32,
                    &cut(line, (w - 4) as usize),
                    ink(T.dim),
                );
            }
        }
        Kind::Agent | Kind::Judge => {
            let agent = now.agent.as_deref().unwrap_or("agent");
            let color = agent_color(agent);
            if !plain {
                face::draw(c, x + 1, 3, t, color, true);
            }
            c.put(tx, 3, &cut(&now.title, room), ink(color).bold());
            if let Some(line) = now.lines.first() {
                c.put(tx, 4, &cut(line, room), ink(T.dim));
            }
            let turn = if now.kind == Kind::Judge {
                data.facts.judge.as_ref()
            } else {
                data.facts.work.as_ref()
            };
            if let Some(turn) = turn {
                let model = match (&turn.model, &turn.effort) {
                    (Some(m), Some(e)) => format!("{m} · {e}"),
                    (Some(m), None) => m.clone(),
                    _ => String::new(),
                };
                c.put(tx, 5, &cut(&model, room), ink(T.fg));
            }
            let mut sx = c.put(tx, 6, spinner(app), ink(color));
            sx = c.put(sx + 1, 6, &mmss(now.seconds.unwrap_or(0)), ink(T.fg).bold());
            let stats = turn.and_then(|t| live.feed.stats.get(&t.id));
            let mut tail = format!("· {}", count(stats.map_or(0, |s| s.tools), "tool call"));
            if let Some(tokens) = stats.and_then(|s| s.output_tokens()) {
                tail.push_str(&format!(" · {} tokens out", thousands(tokens)));
            }
            c.put(
                sx + 1,
                6,
                &cut(&tail, (x + w - sx - 1).max(1) as usize),
                ink(T.dim),
            );
        }
        _ => {
            if !plain {
                face::draw(c, x + 1, 3, t, face::REST, false);
            }
            c.put(
                tx,
                3,
                &cut(&now.title, room),
                ink(tone_color(now.tone)).bold(),
            );
            // The lines wrap into the three rows under the title; a spinner leads the first
            // line while a pass runs or waits. The state Runs keeps the last row for its time.
            let spin = now.spinner && now.kind == Kind::Next;
            let rows = if now.kind == Kind::Runs { 2 } else { 3 };
            let mut parts: Vec<(String, bool)> = Vec::new();
            for (i, line) in now.lines.iter().enumerate() {
                let width = if i == 0 && spin {
                    room.saturating_sub(2)
                } else {
                    room
                };
                for part in wrap(line, width.max(1)) {
                    parts.push((part, i == 0));
                }
            }
            for (y, (k, (part, first))) in (4..).zip(parts.iter().take(rows).enumerate()) {
                let color = if *first { T.fg } else { T.dim };
                if k == 0 && spin {
                    let sx = c.put(tx, y, spinner(app), ink(T.accent));
                    c.put(sx + 1, y, part, ink(color));
                } else {
                    c.put(tx, y, part, ink(color));
                }
            }
            if now.kind == Kind::Runs {
                let sx = c.put(tx, 6, spinner(app), ink(T.accent));
                c.put(sx + 1, 6, &mmss(now.seconds.unwrap_or(0)), ink(T.fg).bold());
            }
        }
    }
}

/// `12.3k` for 12345, and the number itself below 1000.
fn thousands(n: i64) -> String {
    if n < 1000 {
        n.to_string()
    } else {
        format!("{:.1}k", n as f64 / 1000.0)
    }
}

pub fn feed(app: &App, c: &mut Canvas, l: &Layout, live: &Live, now: &Now) {
    let x = l.mid_x();
    let w = l.mid_w();
    c.put(x, ACTIVITY_Y, "ACTIVITY", ink(T.dim).bold());
    c.put(
        x + 9,
        ACTIVITY_Y,
        &"─".repeat((w - 9).max(0) as usize),
        ink(T.line),
    );
    let feed = &live.feed;
    c.wheel(x, ACTIVITY_Y, w, l.bottom() - ACTIVITY_Y + 1, WHEEL_LINES);
    if !feed.follow {
        let mark = format!(" ▲ {} below · G goes to the end ", feed.back);
        let mx = c.right(x + w - 1, ACTIVITY_Y, &mark, ink(T.accent)) + 1;
        // While the line of `t` is open, `G` goes into the line: then the mark is no part.
        if live.tell.is_none() {
            let act = Act::Key(Key::Char('G'));
            c.click(mx, ACTIVITY_Y, width(&mark) as i32, 1, act);
        }
    }
    // The marks show the tab before and the tab after, as `F` and `f` do.
    let tabs = l.tabs(&feed.counts(), app.activity);
    if tabs.left {
        c.put(x, TABS_Y, "‹", ink(T.dim));
        c.click(x, TABS_Y, 1, 1, Act::Tab(app.activity.back()));
    }
    for (filter, tx, text) in &tabs.tabs {
        let style = if *filter == app.activity {
            ink(T.bg).on(T.accent).bold()
        } else {
            ink(T.dim)
        };
        c.put(*tx, TABS_Y, text, style);
        c.click(*tx, TABS_Y, width(text) as i32, 1, Act::Tab(*filter));
    }
    if tabs.right {
        c.put(x + w - 1, TABS_Y, "›", ink(T.dim));
        c.click(x + w - 1, TABS_Y, 1, 1, Act::Tab(app.activity.next()));
    }
    // The lines come from the cache of the feed: only a change wraps them again.
    let lines = feed.shared_lines(l.feed_text_w(), app.clock.offset_seconds, app.activity);
    let height = l.feed_h() as usize;
    if lines.is_empty() {
        let text = if feed.is_empty() {
            "No event yet. Each step of the work shows here.".to_string()
        } else {
            format!(
                "{}: no line yet. f shows the next tab.",
                app.activity.label()
            )
        };
        c.put(x, FEED_Y, &cut(&text, w as usize), ink(T.dim));
        return;
    }
    let end = lines.len().saturating_sub(feed.back.min(lines.len()));
    let start = end.saturating_sub(height);
    let shown = &lines[start..end];
    for (i, line) in shown.iter().enumerate() {
        let y = FEED_Y + i as i32;
        c.put(x, y, &line.time, ink(T.faint));
        c.put(x + 9, y, &cut(&line.word, 5), ink(hue(&line.hue)).bold());
        c.put(x + FEED_TEXT_DX, y, &line.text, ink(hue(&line.text_hue)));
    }
    // A cursor blinks under the last line while the agent works and the feed follows.
    if feed.follow && now.kind == Kind::Agent && shown.len() < height {
        let on = (app.clock.elapsed.as_millis() / 500) % 2 == 1;
        if on {
            c.put(
                x + FEED_TEXT_DX,
                FEED_Y + shown.len() as i32,
                "▌",
                ink(T.faint),
            );
        }
    }
}
