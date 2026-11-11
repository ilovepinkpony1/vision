//! The right column of the live view: TICKET (the cost, the time since the take, the turns, and
//! the commits that the forge does not have), QUESTIONS (to you, by the memory, by the Judge),
//! MARKS (the plan, each job, CI, the pull request, and each env; a dash for a mark that does not
//! exist yet), LIMITS (the two windows as bars with the part, the stop value, and the reset
//! time), and ENGINE (on, paused, or off, and the time since the last pass). When the window is
//! short, the free rows between the parts go first.

use super::layout::Layout;
use crate::app::live::TicketData;
use crate::app::model::App;
use crate::app::msg::JobKind;
use crate::app::now::{local_time, mmss};
use crate::app::text::{cut, width};
use crate::app::view::canvas::{ink, Canvas};
use crate::app::view::home::since;
use crate::app::view::theme::{Rgb, T};
use crate::pipeline::money;
use crate::snapshot::LimitView;

/// One row of the column: a name on the left, a value flush right, or a title.
enum Row {
    Title(&'static str),
    Pair(String, String, Rgb),
    Bar(&'static str, LimitView),
    Note(String),
    /// The state of the engine in its color, and a value.
    State(String, Rgb, String),
    Gap,
}

pub fn draw(app: &App, c: &mut Canvas, l: &Layout, data: &TicketData) {
    let rows = rows(app, data);
    let room = (l.bottom() - 2 + 1) as usize;
    let gaps = rows.iter().filter(|r| matches!(r, Row::Gap)).count();
    let drop_gaps = rows.len() > room && rows.len() - gaps <= room;
    let x = l.right_x();
    let end = l.right_end();
    let mut y = 2;
    for row in rows {
        if y > l.bottom() {
            break;
        }
        match row {
            Row::Gap if drop_gaps => continue,
            Row::Gap => {}
            Row::Title(title) => {
                c.put(x, y, title, ink(T.dim).bold());
            }
            Row::Pair(name, value, color) => {
                c.put(x, y, &name, ink(T.dim));
                let room = (end - x - width(&name) as i32 - 1).max(1) as usize;
                c.right(end, y, &cut(&value, room), ink(color));
            }
            Row::Bar(label, view) => bar(c, x, end, y, label, &view),
            Row::Note(text) => {
                c.put(x, y, &cut(&text, (end - x + 1) as usize), ink(T.faint));
            }
            Row::State(state, color, value) => {
                let after = c.put(x, y, &state, ink(color));
                let room = (end - after - 1).max(1) as usize;
                c.right(end, y, &cut(&value, room), ink(T.dim));
            }
        }
        y += 1;
    }
}

fn rows(app: &App, data: &TicketData) -> Vec<Row> {
    let view = &data.view;
    let m = &data.measures;
    let mut out = vec![Row::Title("TICKET")];
    out.push(Row::Pair("cost".into(), money(view.cost_usd), T.fg));
    out.push(Row::Pair(
        "time".into(),
        since(app, data.taken.as_deref()),
        T.fg,
    ));
    out.push(Row::Pair(
        "turns".into(),
        data.facts.turns.to_string(),
        T.fg,
    ));
    let commits = match data.unpushed {
        // A branch that the forge does not have yet is compared with the base, so zero means
        // only that no commit waits.
        Some(0) => "none to push".to_string(),
        Some(n) => format!("{n} not pushed"),
        None => "-".to_string(),
    };
    out.push(Row::Pair("commits".into(), commits, T.fg));
    out.push(Row::Gap);
    out.push(Row::Title("QUESTIONS"));
    out.push(Row::Pair(
        "to you".into(),
        m.cards_to_person.to_string(),
        if m.cards_to_person > 0 { T.warn } else { T.fg },
    ));
    out.push(Row::Pair(
        "by memory".into(),
        m.cards_by_memory.to_string(),
        T.ok,
    ));
    out.push(Row::Pair(
        "by the Judge".into(),
        m.cards_by_judge.to_string(),
        T.fg,
    ));
    out.push(Row::Gap);
    out.push(Row::Title("MARKS"));
    let short = |h: &str| h.chars().take(7).collect::<String>();
    let plan = match (&view.marks.plan, &data.plan) {
        (Some(hash), _) => (format!("{} ✓", short(hash)), T.ok),
        (None, Some(hash)) => (format!("{} waits", short(hash)), T.warn),
        (None, None) => ("-".to_string(), T.fg),
    };
    out.push(Row::Pair("plan".into(), plan.0, plan.1));
    let mut jobs = data.jobs.clone();
    for mark in &view.marks.jobs {
        if !jobs.contains(&mark.name) {
            jobs.push(mark.name.clone());
        }
    }
    for job in jobs {
        let (value, color) = match view.marks.jobs.iter().find(|m| m.name == job) {
            Some(mark) => (
                format!("{} @{}", mark.result, short(&mark.commit)),
                if mark.result == "pass" { T.ok } else { T.bad },
            ),
            None => ("-".to_string(), T.fg),
        };
        out.push(Row::Pair(job, value, color));
    }
    let ci = match &view.marks.ci {
        Some(mark) => (
            format!("{} @{}", mark.result, short(&mark.commit)),
            match mark.result.as_str() {
                "green" | "pass" => T.ok,
                "red" | "fail" => T.bad,
                _ => T.info,
            },
        ),
        None => ("-".to_string(), T.fg),
    };
    out.push(Row::Pair("CI".into(), ci.0, ci.1));
    let pr = match view.pr {
        Some(number) if view.marks.approved_by.is_empty() => (format!("#{number}"), T.fg),
        Some(number) => (
            format!("#{number} ✓ {}", view.marks.approved_by.join(", ")),
            T.ok,
        ),
        None => ("-".to_string(), T.fg),
    };
    out.push(Row::Pair("PR".into(), pr.0, pr.1));
    for (env, head) in &view.marks.envs {
        let value = head
            .as_deref()
            .map_or("-".to_string(), |h| format!("@{}", short(h)));
        out.push(Row::Pair(env.clone(), value, T.fg));
    }
    out.push(Row::Gap);
    out.push(Row::Title("LIMITS"));
    if let Some(local) = &app.local.data {
        let windows = [
            ("5h", &local.limits.five_hour, &local.resets[0]),
            ("7d", &local.limits.seven_day, &local.resets[1]),
        ];
        for (label, view, resets) in windows {
            out.push(Row::Bar(label, view.clone()));
            if let Some(when) = resets
                .as_deref()
                .and_then(|at| local_time(at, app.clock.wall, app.clock.offset_seconds))
            {
                out.push(Row::Note(format!("   resets {when}")));
            }
        }
    } else {
        out.push(Row::Note("no reading yet".into()));
    }
    out.push(Row::Gap);
    out.push(Row::Title("ENGINE"));
    out.push(engine(app));
    out
}

/// One window: its label, a bar of 10 cells, and the part of the stop value.
fn bar(c: &mut Canvas, x: i32, end: i32, y: i32, label: &str, view: &LimitView) {
    let mut cx = c.put(x, y, &format!("{label} "), ink(T.dim));
    let Some(reading) = &view.reading else {
        c.right(end, y, "no reading", ink(T.dim));
        return;
    };
    let part = reading.part.clamp(0.0, 1.0);
    let cells = 10;
    let filled = (part * f64::from(cells)).round() as i32;
    let color = if reading.part >= view.stop_at {
        T.bad
    } else if reading.part >= view.stop_at - 0.15 {
        T.warn
    } else {
        T.ok
    };
    cx = c.put(cx, y, &"━".repeat(filled as usize), ink(color));
    c.put(cx, y, &"━".repeat((cells - filled) as usize), ink(T.line));
    let text = format!(
        "{:.0}% of {:.0}",
        reading.part * 100.0,
        view.stop_at * 100.0
    );
    c.right(end, y, &text, ink(T.fg));
}

/// The engine: off, a pass that runs, paused, or on, and the time since the last pass.
fn engine(app: &App) -> Row {
    let now = app.clock.elapsed;
    if app.off() {
        return Row::State("■ off".into(), T.bad, "the kill switch is on".into());
    }
    if let Some(running) = &app.engine.running {
        if running.job.kind == JobKind::Pass {
            let ran = now.saturating_sub(running.since).as_secs() as i64;
            return Row::State("● on".into(), T.ok, format!("a pass runs {}", mmss(ran)));
        }
    }
    let last = match app.engine.last_pass_start {
        Some(at) => format!(
            "last pass {} ago",
            crate::util::time::short_age(now.saturating_sub(at).as_secs() as i64)
        ),
        None => "no pass yet".to_string(),
    };
    if app.engine.paused {
        Row::State("■ paused".into(), T.warn, last)
    } else {
        Row::State("● on".into(), T.ok, last)
    }
}
