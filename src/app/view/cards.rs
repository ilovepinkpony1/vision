//! The cards over a screen (`modal`, `mStart`, `mQuit`, and `mHelp` of the design): the start
//! card, the kill switch card, the quit card, the keys card of each screen, and the card of a
//! long error. The continue card has its own module. A card keeps its width and stays in the center; the screen
//! behind it dims. Each card is a list of rows, so that its height follows its text. The card
//! of the things that wait has its own modules.
//!
//! The rows of a card also hold the parts that a click can hit, such as each key of its keys,
//! and the code that puts the rows in the window records them there.

use super::canvas::{ink, Canvas, Ink};
use super::header::spinner;
use super::theme::{agent_color, Rgb, T};
use crate::app::hits::{hint, keys_of, Act};
use crate::app::model::{App, Card, KillCard, Run, StartCard};
use crate::app::msg::Key;
use crate::app::text::{cut, width, wrap};

/// A part of a row of a card that a click can hit: its row, its column in the card, its
/// width, and what a click does.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct RowHit {
    pub(super) row: usize,
    pub(super) x: i32,
    pub(super) w: i32,
    pub(super) act: Act,
}

/// The rows of a card: each row is a list of (column in the card, text, ink), and the parts of
/// the rows that a click can hit.
#[derive(Clone, Default)]
pub(super) struct Rows {
    pub(super) rows: Vec<Vec<(i32, String, Ink)>>,
    pub(super) hits: Vec<RowHit>,
}

impl Rows {
    pub(super) fn text(&mut self, dx: i32, text: impl Into<String>, ink: Ink) {
        self.rows.push(vec![(dx, text.into(), ink)]);
    }

    /// Records a part of the row `row` that a click can hit.
    pub(super) fn hit(&mut self, row: usize, x: i32, w: i32, act: Act) {
        self.hits.push(RowHit { row, x, w, act });
    }

    /// Puts the rows of `other` after these rows, with their parts.
    pub(super) fn append(&mut self, other: Rows) {
        let offset = self.rows.len();
        self.rows.extend(other.rows);
        self.hits.extend(other.hits.into_iter().map(|mut hit| {
            hit.row += offset;
            hit
        }));
    }

    /// Puts the rows in the window, with the first row at `y` and the column 0 of the card at
    /// `x`, cut to a card of width `w`, and records their parts. Only `count` rows from the row
    /// `from` show.
    pub(super) fn put(&self, c: &mut Canvas, x: i32, y: i32, w: i32, from: usize, count: usize) {
        let end = (from + count).min(self.rows.len());
        for (i, row) in self.rows[from.min(end)..end].iter().enumerate() {
            for (dx, text, style) in row {
                let room = (w - dx - 2).max(0) as usize;
                c.put(x + dx, y + i as i32, &cut(text, room), *style);
            }
        }
        for (act, hx, hy, hw) in self.placed(x, y, w, from, count) {
            c.click(hx, hy, hw, 1, act);
        }
    }

    /// The parts of the rows that [`Rows::put`] puts with the same values: what a click does,
    /// the column, the row, and the width of each part in the window. A part is cut as its text
    /// is cut, and a part of a row that does not show is not in the list.
    pub(super) fn placed(
        &self,
        x: i32,
        y: i32,
        w: i32,
        from: usize,
        count: usize,
    ) -> Vec<(Act, i32, i32, i32)> {
        let end = (from + count).min(self.rows.len());
        self.hits
            .iter()
            .filter(|h| h.row >= from && h.row < end)
            .filter_map(|hit| {
                let hw = hit.w.min(w - hit.x - 2);
                let hy = y + (hit.row - from) as i32;
                (hw > 0).then(|| (hit.act.clone(), x + hit.x, hy, hw))
            })
            .collect()
    }

    pub(super) fn blank(&mut self) {
        self.rows.push(Vec::new());
    }

    /// A text in lines of at most `w` cells.
    pub(super) fn para(&mut self, dx: i32, text: &str, w: usize, ink: Ink) {
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

    /// The keys of a card: each key in bold, its label dim. A row that would pass the column
    /// 89, the end of the text of the widest card, goes on in the next row. A click on a key
    /// acts as that key.
    pub(super) fn keys(&mut self, list: &[(&str, &str)]) {
        self.keys_with(list, &[], None);
    }

    /// The keys of a card, with the keys of `decide`, which send something or record a
    /// decision: a click on such a key arms it, and while `armed` is its part, its label says
    /// to click again.
    pub(super) fn keys_with(&mut self, list: &[(&str, &str)], decide: &[Key], armed: Option<&Act>) {
        self.keys_within(list, decide, armed, 89);
    }

    /// The keys of [`Rows::keys_with`] in rows that end before the column `right`.
    pub(super) fn keys_within(
        &mut self,
        list: &[(&str, &str)],
        decide: &[Key],
        armed: Option<&Act>,
        right: i32,
    ) {
        let mut row = Vec::new();
        let mut x = 3;
        for (key, label) in list {
            let label = hint(0, key, label, decide, armed).label;
            let wide = (width(key) + 1 + width(&label)) as i32;
            if x > 3 && x + wide > right {
                self.rows.push(std::mem::take(&mut row));
                x = 3;
            }
            let at = self.rows.len();
            let shown = hint(x, key, &label, decide, armed);
            for (hx, hw, act) in shown.parts {
                self.hit(at, hx, hw, act);
            }
            let tone = if shown.armed { T.warn } else { T.dim };
            row.push((x, key.to_string(), ink(T.fg).bold()));
            x += width(key) as i32 + 1;
            row.push((x, label.clone(), ink(tone)));
            x += width(&label) as i32 + 4;
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

/// Where a card of width `w` with `rows` rows goes in the window: its column, its row, its
/// width, and its height. [`show`] puts each card there.
pub(super) fn card_rect(c: &Canvas, w: i32, rows: usize) -> (i32, i32, i32, i32) {
    let w = w.min(c.w - 2);
    let h = (rows as i32 + 4).min(c.h - 2);
    ((c.w - w) / 2, (c.h - h) / 2, w, h)
}

/// Dims the screen, and draws an empty card of width `w` in the center with its rows and their
/// parts.
pub(super) fn show(c: &mut Canvas, w: i32, title: &str, color: Rgb, rows: Rows) {
    let (x, y, w, h) = card_rect(c, w, rows.rows.len());
    c.dim_all();
    c.wipe(x, y, w, h, T.panel);
    c.frame(x, y, w, h, color);
    c.put(
        x + 2,
        y,
        &format!(" {} ", cut(title, (w - 6) as usize)),
        ink(T.fg).bold(),
    );
    // The rows that fit above the last row of the frame.
    let fit = (h - 3).max(0) as usize;
    rows.put(c, x, y + 2, w, 0, fit);
}

/// The keys card of a screen (`mHelp` of the design): each key with what it does, the mouse,
/// and how the card closes.
///
/// A click on the row of a key closes the card, and the screen acts as that key, so each key of
/// the screen works with the mouse, also a key that a narrow footer leaves out. A row of more
/// keys, such as `l  p  d`, has a part for each key. A key of `decide` sends something or
/// records a decision, so its row is no part: its hint in the footer arms on the first click.
pub(super) fn keys_card(c: &mut Canvas, list: &[(&str, &str)], decide: &[Key]) {
    let mut rows = Rows::default();
    for (key, label) in list {
        let at = rows.rows.len();
        let keys = keys_of(key);
        match keys.as_slice() {
            [(0, w, one)] if *w == width(key) as i32 => {
                if !decide.contains(one) {
                    let wide = KEYS_LABEL_X - 3 + width(label) as i32;
                    rows.hit(at, 3, wide, Act::Run(*one));
                }
            }
            _ => {
                for (dx, w, one) in keys.iter().filter(|(_, _, k)| !decide.contains(k)) {
                    rows.hit(at, 3 + dx, *w, Act::Run(*one));
                }
            }
        }
        rows.rows.push(vec![
            (3, key.to_string(), ink(T.fg).bold()),
            (KEYS_LABEL_X, label.to_string(), ink(T.dim)),
        ]);
    }
    let mouse = [
        ("click", "a key acts as that key; a row selects, then opens"),
        (
            "2 clicks",
            "a key that sends something or records a decision",
        ),
        ("‹  ›", "back and forward; a name at the top goes there"),
        ("shift drag", "selects text; some terminals use Option"),
    ];
    for (key, label) in mouse {
        rows.rows.push(vec![
            (3, key.to_string(), ink(T.fg).bold()),
            (KEYS_LABEL_X, label.to_string(), ink(T.dim)),
        ]);
    }
    rows.blank();
    let close = "esc closes this list";
    rows.hit(rows.rows.len(), 3, width(close) as i32, Act::Key(Key::Esc));
    rows.text(3, close, ink(T.faint));
    show(c, 70, "Keys", T.primary, rows);
}

/// The column of the labels of the keys card.
const KEYS_LABEL_X: i32 = 16;

pub fn draw(app: &App, card: &Card, c: &mut Canvas) {
    match card {
        Card::Start(card) => start(app, card, c),
        Card::Continue(card) => super::continue_card::draw(app, card, c),
        Card::Kill(card) => kill(app, card, c),
        Card::Quit => quit(app, c),
        Card::Help => match app.screen {
            crate::app::model::Screen::Live { .. } => super::live_cards::help(c),
            crate::app::model::Screen::Models => super::models::help(c),
            crate::app::model::Screen::Stats => super::stats::help(c),
            _ => help(c),
        },
        Card::Need(card) => super::need_card::draw(app, card, c),
        Card::Error { title, text } => error(c, title, text),
        Card::Stop(card) => super::live_cards::stop(app, card, c),
        Card::Retry(card) => super::live_cards::retry(app, card, c),
        Card::Open(card) => super::live_cards::open(app, card, c),
        Card::Opened(card) => super::live_cards::opened(card, c),
    }
}

/// The last rows of a card with an action: its keys, or what the action does now. Enter does
/// the action, so a click on it arms first.
pub(super) fn action_rows(app: &App, rows: &mut Rows, run: &Run, go: &str, running: &str) {
    let armed = app.armed_act();
    match run {
        Run::Idle => rows.keys_with(&[("enter", go), ("esc", "cancel")], &[Key::Enter], armed),
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
            rows.keys_with(
                &[("enter", "try again"), ("esc", "close")],
                &[Key::Enter],
                armed,
            );
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

fn kill(app: &App, card: &KillCard, c: &mut Canvas) {
    let mut rows = Rows::default();
    if card.turn_on {
        rows.text(3, "Vision is off.", ink(T.fg).bold());
        rows.blank();
        rows.para(
            3,
            "No action runs now. Press Enter to turn Vision on, and the next pass acts again.",
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
    // The quit card shows only while a ticket moves, so its Enter arms first.
    rows.keys_with(
        &[("enter", "quit"), ("esc", "stay")],
        &[Key::Enter],
        app.armed_act(),
    );
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
        ("n", "answer what waits for you"),
        ("m", "the model of each agent"),
        ("s", "the stats"),
        ("p", "pause the engine, or start it again"),
        ("r", "load the queue of the tracker again"),
        ("K", "the kill switch: stop each agent now"),
        ("?", "this list of keys"),
        ("esc", "go back, or close a card"),
        ("q", "quit"),
    ];
    keys_card(c, &list, &[]);
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
