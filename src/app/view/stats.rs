//! The screen Stats (`drawStats` of the design): four tiles (the cost, the tickets that
//! finished, the questions that the memory and the Judge answered without the person, and the
//! minutes of the person), the table of tickets, the bars of time and cost of each step in the
//! order of the adapter, and the cost of each agent, for the last 7 days or for all time.

use super::canvas::{ink, Canvas};
use super::header::{header, keys, spinner};
use super::models::help_rows;
use super::theme::{agent_color, Rgb, T};
use crate::app::model::App;
use crate::app::stats::StatsData;
use crate::app::text::{count, cut, error_line};
use crate::pipeline::money;
use crate::util::time::short_age;

pub fn draw(app: &App, c: &mut Canvas) {
    let week = app.stats.week;
    header(
        app,
        c,
        Some(("stats", if week { "the last 7 days" } else { "all time" })),
    );
    let Some(data) = app.stats.data.data.as_ref().filter(|d| d.week == week) else {
        let text = match &app.stats.data.error {
            Some(error) => format!("The measures did not load: {}", error_line(error)),
            None => format!("{} reads the ledger", spinner(app)),
        };
        c.put(2, 3, &cut(&text, (c.w - 4) as usize), ink(T.dim));
        footer(c, week);
        return;
    };
    tiles(c, data);
    let bottom = c.h - 9;
    let mut y = 8;
    let avail = (bottom - y + 1).max(4);
    let table_rows = data.tickets.len() as i32;
    let step_rows = data.steps.len() as i32;
    let steps_shown = step_rows.min((avail - 3 - table_rows.min(2)).max(1));
    let tickets_shown = table_rows.min((avail - 3 - steps_shown).max(1));
    // The table of tickets.
    if data.tickets.is_empty() {
        let window = if week { "in the last 7 days" } else { "so far" };
        c.put(
            2,
            y,
            &format!("No ticket has measures {window}."),
            ink(T.dim),
        );
        y += 2;
    } else {
        let heads = [
            ("TICKET", 2),
            ("TO YOU", 13),
            ("BY MEMORY", 22),
            ("BY JUDGE", 34),
            ("YOUR MIN", 45),
            ("COST", 56),
        ];
        for (text, x) in heads {
            c.put(x, y, text, ink(T.dim).bold());
        }
        y += 1;
        for t in data.tickets.iter().take(tickets_shown as usize) {
            c.put(2, y, &cut(&t.key, 10), ink(T.fg).bold());
            c.put(13, y, &t.to_you.to_string(), ink(T.fg));
            c.put(22, y, &t.by_memory.to_string(), ink(T.fg));
            c.put(34, y, &t.by_judge.to_string(), ink(T.fg));
            c.put(45, y, &format!("{:.1}", t.minutes), ink(T.fg));
            c.put(56, y, &money(t.cost_usd), ink(T.fg));
            y += 1;
        }
        let more = table_rows - tickets_shown;
        if more > 0 {
            c.put(
                70,
                y - 1,
                &format!("and {} more", count(more as usize, "ticket")),
                ink(T.dim),
            );
        }
        y += 1;
    }
    // The bars of each step.
    c.put(2, y, "BY STEP, ALL TICKETS", ink(T.dim).bold());
    y += 1;
    if data.steps.is_empty() {
        c.put(2, y, "No step has measures yet.", ink(T.dim));
    }
    let longest = data
        .steps
        .iter()
        .map(|s| s.seconds)
        .max()
        .unwrap_or(0)
        .max(1);
    let bar_w = (c.w - 2 - 14 - 22).clamp(10, 60);
    for s in data.steps.iter().take(steps_shown as usize) {
        c.put(2, y, &cut(&s.step, 11), ink(T.fg));
        let n = if s.seconds > 0 {
            ((s.seconds as f64 / longest as f64) * f64::from(bar_w))
                .round()
                .max(1.0) as i32
        } else {
            0
        };
        // A heavy line for the part and a light line for the rest: the length shows with no
        // color too.
        let mut x = c.put(14, y, &"━".repeat(n as usize), ink(T.accent));
        x = c.put(x, y, &"─".repeat((bar_w - n) as usize), ink(T.line));
        c.right(x + 9, y, &short_age(s.seconds), ink(T.fg));
        c.right(x + 19, y, &money(s.cost_usd), ink(T.dim));
        y += 1;
    }
    if step_rows > steps_shown {
        c.put(
            2,
            y,
            &format!(
                "and {} more",
                count((step_rows - steps_shown) as usize, "step")
            ),
            ink(T.dim),
        );
    }
    // The cost of each agent.
    let ya = c.h - 7;
    c.put(2, ya, "BY AGENT", ink(T.dim).bold());
    let mut x = 2;
    if data.agents.is_empty() {
        c.put(2, ya + 1, "No turn cost anything yet.", ink(T.dim));
    }
    for (agent, cost) in &data.agents {
        let part = format!("{agent} {}", money(*cost));
        if x + 2 + part.chars().count() as i32 > c.w - 2 {
            break;
        }
        x = c.put(x, ya + 1, "● ", ink(agent_color(agent)));
        x = c.put(x, ya + 1, &format!("{agent} "), ink(T.fg));
        x = c.put(x, ya + 1, &money(*cost), ink(T.dim)) + 4;
    }
    // The turns that no ticket owns.
    if !data.outside.is_empty() {
        let mut x = c.put(
            2,
            ya + 2,
            "outside tickets, not in the cost above  ",
            ink(T.dim),
        );
        for (agent, cost) in &data.outside {
            let part = format!("{agent} {}", money(*cost));
            if x + 2 + part.chars().count() as i32 > c.w - 2 {
                break;
            }
            x = c.put(x, ya + 2, "● ", ink(agent_color(agent)));
            x = c.put(x, ya + 2, &format!("{agent} "), ink(T.fg));
            x = c.put(x, ya + 2, &money(*cost), ink(T.dim)) + 4;
        }
    }
    c.put(
        2,
        c.h - 4,
        "The measures come from the ledger. Vision keeps them from the first ticket.",
        ink(T.faint),
    );
    footer(c, week);
}

fn tiles(c: &mut Canvas, data: &StatsData) {
    let w = ((c.w - 4 - 3) / 4).max(16);
    let week = data.week;
    let cost_title = if week {
        "cost this week"
    } else {
        "cost of all time"
    };
    let answered = if data.questions == 0 {
        "no question yet".to_string()
    } else {
        format!(
            "{} of {} · {:.0}%",
            data.without_you,
            data.questions,
            100.0 * data.without_you as f64 / data.questions as f64
        )
    };
    let list: [(&str, String, Rgb); 4] = [
        (cost_title, money(data.cost_usd), T.fg),
        ("tickets finished", data.finished.to_string(), T.ok),
        ("answered without you", answered, T.ok),
        ("your minutes", format!("{:.1} min", data.minutes), T.fg),
    ];
    for (i, (title, value, color)) in list.iter().enumerate() {
        let x = 2 + i as i32 * (w + 1);
        c.frame(x, 3, w, 4, T.line);
        c.put(x + 2, 4, &cut(title, (w - 3) as usize), ink(T.dim));
        c.put(x + 2, 5, &cut(value, (w - 3) as usize), ink(*color).bold());
    }
}

fn footer(c: &mut Canvas, week: bool) {
    let other = if week { "all time" } else { "the last 7 days" };
    keys(c, &[("w", other), ("esc", "home"), ("?", "keys")]);
}

/// The keys of Stats.
pub fn help(c: &mut Canvas) {
    help_rows(
        c,
        &[
            ("w", "the last 7 days, or all time"),
            ("esc", "back to Home"),
            ("?", "this list of keys"),
            ("q", "quit"),
        ],
    );
}
