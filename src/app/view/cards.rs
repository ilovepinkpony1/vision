//! The cards over a screen (`modal`, `mStart`, `mQuit`, and `mHelp` of the design): the start
//! card, the resume card, the kill switch card, the quit card, the keys card, the list of needs,
//! and the card of a long error. A card keeps its width and stays in the center; the screen
//! behind it dims. Each card is a list of rows, so that its height follows its text.

use super::canvas::{ink, Canvas, Ink};
use super::header::spinner;
use super::theme::{agent_color, Rgb, T};
use crate::app::model::{App, Card, KillCard, ResumeCard, Run, StartCard};
use crate::app::text::{cut, wrap};
use crate::pipeline::state::clear_command;
use crate::snapshot::{Need, NeedKind};

/// The rows of a card: each row is a list of (column in the card, text, ink).
#[derive(Default)]
struct Rows {
    rows: Vec<Vec<(i32, String, Ink)>>,
}

impl Rows {
    fn text(&mut self, dx: i32, text: impl Into<String>, ink: Ink) {
        self.rows.push(vec![(dx, text.into(), ink)]);
    }

    fn blank(&mut self) {
        self.rows.push(Vec::new());
    }

    /// A text in lines of at most `w` cells.
    fn para(&mut self, dx: i32, text: &str, w: usize, ink: Ink) {
        for line in wrap(text, w) {
            self.text(dx, line, ink);
        }
    }

    /// A name and a value, as `kv` of the start card.
    fn kv(&mut self, key: &str, value: &str, w: usize, color: Rgb) {
        let mut first = true;
        for line in wrap(value, w) {
            let mut row = Vec::new();
            if first {
                row.push((3, key.to_string(), ink(T.dim)));
                first = false;
            }
            row.push((13, line, ink(color)));
            self.rows.push(row);
        }
    }

    /// The keys of a card: each key in bold, its label dim.
    fn keys(&mut self, list: &[(&str, &str)]) {
        let mut row = Vec::new();
        let mut x = 3;
        for (key, label) in list {
            row.push((x, key.to_string(), ink(T.fg).bold()));
            x += crate::app::text::width(key) as i32 + 1;
            row.push((x, label.to_string(), ink(T.dim)));
            x += crate::app::text::width(label) as i32 + 4;
        }
        self.rows.push(row);
    }

    /// A spinner and a text, while an action waits or runs.
    fn busy(&mut self, app: &App, text: &str) {
        self.rows.push(vec![
            (3, spinner(app).to_string(), ink(T.accent)),
            (5, text.to_string(), ink(T.fg)),
        ]);
    }
}

/// A text of many lines, each line wrapped to `w` cells.
fn lines_of(text: &str, w: usize) -> Vec<String> {
    text.lines().flat_map(|line| wrap(line, w)).collect()
}

/// Dims the screen, and draws an empty card of width `w` in the center with its rows.
fn show(c: &mut Canvas, w: i32, title: &str, color: Rgb, rows: Rows) {
    let w = w.min(c.w - 2);
    let h = (rows.rows.len() as i32 + 4).min(c.h - 2);
    let x = (c.w - w) / 2;
    let y = (c.h - h) / 2;
    c.dim_all();
    c.wipe(x, y, w, h, T.panel);
    c.frame(x, y, w, h, color);
    c.put(
        x + 2,
        y,
        &format!(" {} ", cut(title, (w - 6) as usize)),
        ink(T.fg).bold(),
    );
    for (i, row) in rows.rows.iter().enumerate() {
        let yy = y + 2 + i as i32;
        if yy >= y + h - 1 {
            break;
        }
        for (dx, text, style) in row {
            let room = (w - dx - 2).max(0) as usize;
            c.put(x + dx, yy, &cut(text, room), *style);
        }
    }
}

pub fn draw(app: &App, card: &Card, c: &mut Canvas) {
    match card {
        Card::Start(card) => start(app, card, c),
        Card::Resume(card) => resume(app, card, c),
        Card::Kill(card) => kill(app, card, c),
        Card::Quit => quit(app, c),
        Card::Help => help(c),
        Card::Needs => needs(app, c),
        Card::Error { title, text } => error(c, title, text),
    }
}

/// The last rows of a card with an action: its keys, or what the action does now.
fn action_rows(app: &App, rows: &mut Rows, run: &Run, go: &str, running: &str) {
    match run {
        Run::Idle => rows.keys(&[("enter", go), ("esc", "cancel")]),
        Run::Waiting { .. } => {
            rows.busy(app, "waits for the job of the engine that runs now");
            rows.blank();
            rows.keys(&[("esc", "close this card; the action still runs")]);
        }
        Run::Running { .. } => rows.busy(app, running),
        Run::Failed(text) => {
            rows.text(3, "It did not work:", ink(T.bad).bold());
            let lines = lines_of(text, 72);
            let shown = lines.len().min(6);
            for line in &lines[..shown] {
                rows.text(3, line.clone(), ink(T.bad));
            }
            if lines.len() > shown {
                rows.text(3, "The rest of the text is cut.", ink(T.faint));
            }
            rows.blank();
            rows.keys(&[("enter", "try again"), ("esc", "close")]);
        }
    }
}

fn start(app: &App, card: &StartCard, c: &mut Canvas) {
    let mut rows = Rows::default();
    let title = if card.title.is_empty() {
        card.key.clone()
    } else {
        card.title.clone()
    };
    rows.text(3, title, ink(T.fg).bold());
    rows.blank();
    match &card.info {
        None => rows.busy(app, "reads the adapter and looks for a free clone"),
        Some(Err(error)) => {
            rows.text(3, "Vision could not read what the start does:", ink(T.bad));
            rows.para(3, error, 72, ink(T.bad));
        }
        Some(Ok(info)) => {
            match &info.clone {
                Ok(clone) => rows.kv("clone", &format!("{clone} is free"), 64, T.fg),
                Err(why) => rows.kv("clone", why, 64, T.warn),
            }
            rows.kv(
                "branch",
                &format!("{} from {}", info.branch, info.base),
                64,
                T.fg,
            );
            let mut path = format!("{}: ", crate::app::text::count(info.steps.len(), "step"));
            path.push_str(&info.steps.join(", "));
            rows.rows.push(vec![
                (3, "path".into(), ink(T.dim)),
                (13, cut(&path, 64), ink(T.fg)),
            ]);
            let first = &info.first;
            let (text, color) = match (&first.agent, &first.model, &first.effort) {
                (Some(agent), Some(model), Some(effort)) => (
                    format!("{}: {agent} · {model} · {effort}", first.step),
                    agent_color(agent),
                ),
                (Some(agent), _, _) => (format!("{}: {agent}", first.step), agent_color(agent)),
                _ => (
                    format!("{}: a step of the kind {}", first.step, first.kind),
                    T.fg,
                ),
            };
            rows.kv("first", &text, 64, color);
            rows.kv("writes", &info.writes, 64, T.warn);
            rows.blank();
            rows.text(
                3,
                "Nothing goes to the tracker or the forge at the start.",
                ink(T.dim),
            );
        }
    }
    rows.blank();
    action_rows(
        app,
        &mut rows,
        &card.run,
        "start",
        &format!("runs vision take {}", card.key),
    );
    show(c, 80, &format!("Start {}", card.key), T.primary, rows);
}

fn resume(app: &App, card: &ResumeCard, c: &mut Canvas) {
    let mut rows = Rows::default();
    if !card.title.is_empty() {
        rows.text(3, card.title.clone(), ink(T.fg).bold());
        rows.blank();
    }
    rows.para(
        3,
        &format!("{} is stopped at the step {}.", card.key, card.step),
        72,
        ink(T.fg),
    );
    let place = match (&card.branch, &card.clone) {
        (Some(branch), Some(clone)) => {
            format!("Its branch {branch} stays in the clone {clone}.")
        }
        (Some(branch), None) => format!("Its branch {branch} stays in its clone."),
        _ => "Its branch stays in its clone.".to_string(),
    };
    rows.para(3, &place, 72, ink(T.dim));
    rows.blank();
    rows.para(
        3,
        &format!(
            "Enter runs vision take {}, and the ticket starts again at the same step.",
            card.key
        ),
        72,
        ink(T.dim),
    );
    rows.blank();
    action_rows(
        app,
        &mut rows,
        &card.run,
        "resume",
        &format!("runs vision take {}", card.key),
    );
    show(c, 80, &format!("Resume {}", card.key), T.primary, rows);
}

fn kill(app: &App, card: &KillCard, c: &mut Canvas) {
    let mut rows = Rows::default();
    if card.turn_on {
        rows.text(3, "Vision is off.", ink(T.fg).bold());
        rows.blank();
        rows.para(
            3,
            "No action runs now. Enter runs vision on, and the next pass acts again.",
            68,
            ink(T.dim),
        );
        rows.blank();
        action_rows(app, &mut rows, &card.run, "turn on", "runs vision on");
        show(c, 74, "The kill switch", T.primary, rows);
    } else {
        rows.text(3, "Turn Vision off?", ink(T.fg).bold());
        rows.blank();
        rows.para(
            3,
            "The kill switch stops each running turn now, and no action runs until you turn Vision on again. Each ticket keeps its step.",
            68,
            ink(T.dim),
        );
        rows.blank();
        action_rows(app, &mut rows, &card.run, "turn off", "runs vision off");
        show(c, 74, "The kill switch", T.bad, rows);
    }
}

fn quit(app: &App, c: &mut Canvas) {
    let moving: Vec<_> = app.held().iter().filter(|h| !h.stopped).collect();
    let local = app.local.data.as_ref();
    let timer = local.is_some_and(|l| l.timer);
    let agents = local.map_or(&[][..], |l| l.agents.as_slice());
    let mut rows = Rows::default();
    let head = match moving.len() {
        1 => "1 ticket is in progress.".to_string(),
        n => format!("{n} tickets are in progress."),
    };
    rows.text(3, head, ink(T.fg).bold());
    rows.blank();
    let running = match agents {
        [] => String::new(),
        [agent] => format!("The running turn of {agent} finishes by itself. "),
        many => format!(
            "The running turns of {} finish by themselves. ",
            join_and(many)
        ),
    };
    if timer {
        rows.para(
            3,
            &format!("{running}The system timer is installed, so it keeps the work moving while this window is closed."),
            68,
            ink(T.dim),
        );
        rows.blank();
        rows.text(3, "Open Vision again to see the work.", ink(T.faint));
    } else {
        rows.para(
            3,
            &format!("{running}The next step of each ticket waits until you open Vision again."),
            68,
            ink(T.dim),
        );
        rows.blank();
        rows.text(
            3,
            "To let the work continue without this window, run vision install.",
            ink(T.faint),
        );
    }
    rows.blank();
    rows.keys(&[("enter", "quit"), ("esc", "stay")]);
    show(c, 74, "Quit Vision?", T.primary, rows);
}

/// `a`, `a and b`, or `a, b, and c`.
fn join_and(items: &[String]) -> String {
    match items {
        [] => String::new(),
        [one] => one.clone(),
        [a, b] => format!("{a} and {b}"),
        [rest @ .., last] => format!("{}, and {last}", rest.join(", ")),
    }
}

fn help(c: &mut Canvas) {
    let list = [
        ("↑↓  j k", "move through the tickets"),
        ("enter", "open a ticket, or start one"),
        ("n", "what waits for you"),
        ("p", "pause the engine, or start it again"),
        ("r", "load the queue of the tracker again"),
        ("K", "the kill switch: stop each agent now"),
        ("?", "this list of keys"),
        ("esc", "go back, or close a card"),
        ("q", "quit"),
    ];
    let mut rows = Rows::default();
    for (key, label) in list {
        rows.rows.push(vec![
            (3, key.to_string(), ink(T.fg).bold()),
            (16, label.to_string(), ink(T.dim)),
        ]);
    }
    rows.blank();
    rows.text(3, "esc closes this list", ink(T.faint));
    show(c, 70, "Keys", T.primary, rows);
}

/// The command of today that answers a need.
pub fn need_command(need: &Need) -> String {
    let ticket = need.ticket.as_deref().unwrap_or("KEY");
    match need.kind {
        NeedKind::Card => format!("vision answer {} --choice N", need.id),
        NeedKind::Plan => format!("vision approve-plan {ticket} {}", need.id),
        NeedKind::Operation => format!("vision approve {ticket} {}", need.id),
        NeedKind::Flag => clear_command(&need.id, ticket),
        NeedKind::Proposal => "vision memory review".to_string(),
    }
}

fn need_word(kind: NeedKind) -> &'static str {
    match kind {
        NeedKind::Card => "question",
        NeedKind::Plan => "plan",
        NeedKind::Operation => "write",
        NeedKind::Flag => "blocked",
        NeedKind::Proposal => "memory",
    }
}

fn needs(app: &App, c: &mut Canvas) {
    let needs = app.needs();
    let mut rows = Rows::default();
    let fit = ((c.h - 12) / 3).max(1) as usize;
    for need in needs.iter().take(fit) {
        rows.rows.push(vec![
            (
                3,
                cut(need.ticket.as_deref().unwrap_or("-"), 10),
                ink(T.fg).bold(),
            ),
            (14, need_word(need.kind).to_string(), ink(T.warn)),
            (25, need.text.clone(), ink(T.fg)),
        ]);
        rows.rows.push(vec![
            (14, "run".to_string(), ink(T.faint)),
            (25, need_command(need), ink(T.accent)),
        ]);
        rows.blank();
    }
    if needs.len() > fit {
        rows.text(
            3,
            format!(
                "and {} more. vision needs lists each one.",
                needs.len() - fit
            ),
            ink(T.dim),
        );
        rows.blank();
    }
    rows.text(
        3,
        "Run the command in a shell. vision needs shows each question with its choices.",
        ink(T.dim),
    );
    rows.blank();
    rows.keys(&[("esc", "close")]);
    let title = match needs.len() {
        1 => "1 thing waits for you".to_string(),
        n => format!("{n} things wait for you"),
    };
    show(c, 92, &title, T.warn, rows);
}

fn error(c: &mut Canvas, title: &str, text: &str) {
    let mut rows = Rows::default();
    let lines = lines_of(text, 74);
    let fit = (c.h - 10).max(1) as usize;
    for line in lines.iter().take(fit) {
        rows.text(3, line.clone(), ink(T.fg));
    }
    if lines.len() > fit {
        rows.text(3, "The rest of the text is cut.", ink(T.faint));
    }
    rows.blank();
    rows.keys(&[("esc", "close")]);
    show(c, 80, title, T.bad, rows);
}
