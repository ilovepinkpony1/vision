//! The plain detail screen of a ticket in progress: the header with a crumb, and the facts of
//! the status view. Part A2 replaces it with the live view of the ticket.

use super::canvas::{ink, Canvas};
use super::header::{dots, header, keys};
use super::home::{since, tone};
use super::theme::T;
use crate::app::model::App;
use crate::app::text::{count, cut, wrap};
use crate::pipeline::money;
use crate::pipeline::view::marks_text;

pub fn draw(app: &App, c: &mut Canvas, key: &str) {
    let Some(ticket) = app.held_ticket(key) else {
        header(app, c, Some((key, "")));
        c.put(
            2,
            3,
            &format!("{key} is no longer in progress. Press esc to go back to Home."),
            ink(T.dim),
        );
        keys(c, &[("esc", "home"), ("?", "keys"), ("q", "quit")]);
        return;
    };
    let view = &ticket.view;
    let title = app.title_of(ticket);
    header(app, c, Some((key, title)));
    let room = (c.w - 18).max(10) as usize;
    let mut y = 3;
    if !title.is_empty() {
        c.put(2, y, &cut(title, (c.w - 4) as usize), ink(T.fg).bold());
        y += 2;
    }
    let steps = app.setup.as_ref().map_or(&[][..], |s| s.steps.as_slice());
    let step = view.step.as_deref().unwrap_or("-");
    let tone = tone(ticket);
    let mut x = dots(
        c,
        2,
        y,
        steps.len(),
        steps.iter().position(|s| s == step),
        tone,
        c.w - 4,
    );
    if !steps.is_empty() {
        x += 2;
    }
    c.put(x, y, step, ink(T.fg).bold());
    y += 2;
    // A name and its value in lines. The rows above the footer of keys are the limit.
    let line = |c: &mut Canvas, y: &mut i32, name: &str, value: &str, color| {
        let mut first = true;
        for part in wrap(value, room) {
            if *y >= c.h - 3 {
                return;
            }
            if first {
                c.put(2, *y, name, ink(T.dim));
                first = false;
            }
            c.put(16, *y, &part, ink(color));
            *y += 1;
        }
        if first && *y < c.h - 3 {
            c.put(2, *y, name, ink(T.dim));
            *y += 1;
        }
    };
    let kind = view.kind.as_deref().unwrap_or("-");
    line(
        c,
        &mut y,
        "step",
        &format!("{step} (a step of the kind {kind})"),
        T.fg,
    );
    line(c, &mut y, "now", &view.now, tone);
    if view.flags.is_empty() {
        line(c, &mut y, "flags", "none", T.dim);
    }
    for flag in &view.flags {
        line(
            c,
            &mut y,
            "flag",
            &format!("{}: {}", flag.flag, flag.reason),
            T.warn,
        );
        line(
            c,
            &mut y,
            "",
            &format!("Clears it: {}", flag.command),
            T.dim,
        );
    }
    let needs: usize = view.needs.values().sum();
    if needs > 0 {
        line(
            c,
            &mut y,
            "needs",
            &format!("{} for you. Press n to see them.", count(needs, "item")),
            T.warn,
        );
    }
    line(c, &mut y, "marks", &marks_text(&view.marks), T.fg);
    if let Some(plan) = &view.marks.plan {
        line(c, &mut y, "plan", &format!("approved, hash {plan}"), T.fg);
    }
    line(c, &mut y, "cost", &money(view.cost_usd), T.fg);
    let taken = match &ticket.taken {
        Some(at) => format!("{} ago, at {at}", since(app, Some(at))),
        None => "-".to_string(),
    };
    line(c, &mut y, "taken", &taken, T.fg);
    line(
        c,
        &mut y,
        "branch",
        view.branch.as_deref().unwrap_or("-"),
        T.fg,
    );
    line(
        c,
        &mut y,
        "clone",
        view.clone.as_deref().unwrap_or("none now"),
        T.fg,
    );
    let pr = view
        .pr
        .map_or_else(|| "none yet".to_string(), |pr| format!("#{pr}"));
    line(c, &mut y, "pull request", &pr, T.fg);
    keys(
        c,
        &[
            ("esc", "home"),
            ("n", "needs"),
            ("?", "keys"),
            ("q", "quit"),
        ],
    );
}
