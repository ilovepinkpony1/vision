//! The pager of the live view (`drawLog` of the design): the header with the key and the title,
//! the names of the columns when the text has them, the lines with their colors, a mark of the
//! position, and the keys. The log shows its cursor row.

use super::canvas::{ink, Canvas};
use super::header::{header, keys, spinner};
use super::theme::{hue, T};
use crate::app::model::App;
use crate::app::pager::{body_rows, Pager, PagerKind, PagerLine, PagerState};
use crate::app::text::{clip, cut, width, wrap};

pub fn draw(app: &App, c: &mut Canvas, key: &str, pager: &Pager) {
    header(app, c, Some((key, &pager.title)));
    let mut y = 3;
    if let Some(head) = &pager.head {
        line(c, y, head, c.w - 4);
        c.put(
            2,
            y + 1,
            &"─".repeat((c.w - 4).max(0) as usize),
            ink(T.line),
        );
        y += 2;
    }
    c.put(0, c.h - 3, &"─".repeat(c.w as usize), ink(T.line));
    let rows = body_rows(c.h as u16, pager.head.is_some());
    match &pager.state {
        PagerState::Loading => {
            let what = match pager.kind {
                PagerKind::Plan => "reads the plan",
                PagerKind::Diff => "compares the branch with the forge",
                PagerKind::Turn { .. } => "reads the events of the turn",
                PagerKind::Log => "reads the log",
            };
            c.put(2, y, &format!("{} {what}", spinner(app)), ink(T.dim));
            keys(c, &[("esc", "back")]);
            return;
        }
        PagerState::Failed(error) => {
            c.put(2, y, "It did not load:", ink(T.bad).bold());
            for (i, part) in wrap(error, (c.w - 6).max(10) as usize)
                .into_iter()
                .take(rows.saturating_sub(2))
                .enumerate()
            {
                c.put(2, y + 1 + i as i32, &part, ink(T.bad));
            }
            keys(c, &[("esc", "back")]);
            return;
        }
        PagerState::Ready => {}
    }
    if pager.lines.is_empty() {
        c.put(2, y, "Nothing to show yet.", ink(T.dim));
    }
    for (i, item) in pager.lines.iter().enumerate().skip(pager.top).take(rows) {
        let yy = y + (i - pager.top) as i32;
        if pager.cursor == Some(i) {
            c.tint(1, yy, c.w - 2, T.sel);
            c.put(0, yy, "▸", ink(T.accent).bold());
        }
        line(c, yy, item, c.w - 4);
    }
    let total = pager.lines.len();
    if total > rows {
        let last = (pager.top + rows).min(total);
        let mark = format!("{} to {} of {}", pager.top + 1, last, total);
        c.right(c.w - 2, c.h - 2, &mark, ink(T.dim));
    }
    let list: Vec<(&str, &str)> = match &pager.kind {
        PagerKind::Log => vec![
            ("↑↓", "move"),
            ("enter", "the events of the turn"),
            ("pgup pgdn", "page"),
            ("g G", "start end"),
            ("esc", "back"),
        ],
        _ => vec![
            ("↑↓", "scroll"),
            ("pgup pgdn", "page"),
            ("g G", "start end"),
            ("esc", "back"),
        ],
    };
    keys(c, &list);
}

/// One line of parts from the column 2, at most `room` cells wide.
fn line(c: &mut Canvas, y: i32, item: &PagerLine, room: i32) {
    let mut x = 2;
    let end = 2 + room;
    for span in &item.spans {
        if x >= end {
            break;
        }
        let left = (end - x) as usize;
        let mut style = ink(hue(&span.hue));
        if span.bold {
            style = style.bold();
        }
        match span.width {
            Some(w) => {
                let w = w.min(left);
                let text = clip(&span.text, w.saturating_sub(1));
                c.put(x, y, &text, style);
                x += w as i32;
            }
            None => {
                let text = cut(&span.text, left);
                x += width(&text) as i32;
                c.put(x - width(&text) as i32, y, &text, style);
            }
        }
    }
}
