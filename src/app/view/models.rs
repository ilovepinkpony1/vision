//! The screen Models (`drawModels` of the design): one row for each agent with its model, its
//! effort, pinned or the default, and the measured turns of the model with their mean cost;
//! under it the reason of the default. A change that is not pinned yet has a mark. The box at
//! the bottom holds the recommendation for the selected agent.
//!
//! With more agents than fit above the box, the rows show a window that follows the selection,
//! and a mark above and below the rows says how many agents are out of view.
//!
//! A click on an agent selects it. A click on the selected agent with a change that is not
//! pinned acts as Enter, so it pins the change: it arms on the first click and pins on the
//! second. A click on the selected agent with no change only says how to make one. On the
//! selected row, `‹` and `›` act as the left and right arrows, and the effort acts as `e`. The
//! wheel over the rows moves the selection as the arrows do, and a click on the mark of the
//! agents above or below selects the next agent out of view.

use super::canvas::{ink, Canvas};
use super::cards::Rows;
use super::chain_rows;
use super::header::{header, keys_with, spinner};
use super::home::{more_marks, window};
use super::theme::{agent_color, T};
use crate::app::chain::Owner;
use crate::app::hits::Act;
use crate::app::model::App;
use crate::app::model_table::ModelLine;
use crate::app::msg::Key;
use crate::app::text::{cut, error_line, width, wrap};
use crate::pipeline::money;

const AGENT_X: i32 = 4;
const MODEL_X: i32 = 15;
const EFFORT_X: i32 = 40;
const SOURCE_X: i32 = 48;
/// The first row of the header of the table.
const TABLE_Y: i32 = 5;
/// The rows of the box of the recommendation, and the rows under it: the line over the footer
/// and the footer.
const BOX_H: i32 = 5;
const UNDER_BOX: i32 = 2;

pub fn draw(app: &App, c: &mut Canvas) {
    header(app, c, "");
    let intro = "Each agent keeps its model until you change it. A change holds for the next turn of the agent, not for a turn that runs now.";
    for (i, text) in wrap(intro, (c.w - 4) as usize).iter().take(2).enumerate() {
        c.put(2, 3 + i as i32, text, ink(T.dim));
    }
    let state = &app.models;
    let lines = state.lines();
    if lines.is_empty() {
        let text = match &state.data.error {
            Some(error) => format!("The model table did not load: {}", error_line(error)),
            None => format!("{} reads the model table", spinner(app)),
        };
        c.put(4, 5, &cut(&text, (c.w - 8) as usize), ink(T.dim));
        footer(app, c);
        return;
    }
    let y0 = TABLE_Y;
    c.put(AGENT_X, y0, "AGENT", ink(T.dim).bold());
    c.put(MODEL_X + 2, y0, "MODEL", ink(T.dim).bold());
    c.put(EFFORT_X, y0, "EFFORT", ink(T.dim).bold());
    c.put(SOURCE_X, y0, "SOURCE", ink(T.dim).bold());
    c.right(c.w - 3, y0, "MEASURED", ink(T.dim).bold());
    c.put(2, y0 + 1, &"─".repeat((c.w - 4) as usize), ink(T.line));
    let (top, shown) = (y0 + 2, shown_agents(c.h).min(lines.len()));
    let offset = window(Some(state.selected), lines.len(), shown);
    c.wheel(2, top, c.w - 4, shown as i32 * 2, 1);
    for (i, line) in lines.iter().enumerate().skip(offset).take(shown) {
        let y = top + (i - offset) as i32 * 2;
        let selected = i == state.selected;
        // A click on the selected agent pins its change, so it arms first.
        let act = if selected && state.edits.contains_key(&line.agent) {
            Act::Decide(Key::Enter)
        } else {
            Act::Agent(i)
        };
        c.click(2, y, c.w - 4, 2, act);
        row(app, c, line, y, selected);
    }
    let box_y = c.h - BOX_H - UNDER_BOX;
    if let Some(line) = lines.get(state.selected) {
        advice_box(app, c, line, top + shown as i32 * 2);
    }
    // The marks go on the line over the rows and on the top of the box.
    more_marks(c, y0 + 1, box_y - y0, (offset, shown, lines.len()), |i| {
        (i < lines.len()).then_some(Act::Agent(i))
    });
    footer(app, c);
}

/// The agents whose rows fit between the header of the table and the box of the
/// recommendation in a window of `h` rows, two rows for each agent.
fn shown_agents(h: i32) -> usize {
    ((h - BOX_H - UNDER_BOX - (TABLE_Y + 2)) / 2).max(1) as usize
}

fn row(app: &App, c: &mut Canvas, line: &ModelLine, y: i32, selected: bool) {
    let state = &app.models;
    if selected {
        c.tint(2, y, c.w - 4, T.sel);
        c.tint(2, y + 1, c.w - 4, T.sel);
        c.put(2, y, "▸", ink(T.accent).bold());
    }
    let (model, effort) = state.shown(line);
    let changed = state.edits.contains_key(&line.agent);
    c.put(
        AGENT_X,
        y,
        &line.agent,
        ink(agent_color(&line.agent)).bold(),
    );
    let shown = if selected {
        format!("‹ {model} ›")
    } else {
        format!("  {model}")
    };
    let model_ink = if changed && model != line.model {
        ink(T.warn).bold()
    } else {
        ink(T.fg)
    };
    let shown = cut(&shown, (EFFORT_X - MODEL_X - 1) as usize);
    let end = c.put(MODEL_X, y, &shown, model_ink);
    let effort_ink = if changed && effort != line.effort {
        ink(T.warn).bold()
    } else {
        ink(T.fg)
    };
    c.put(EFFORT_X, y, &effort, effort_ink);
    // The arrows and the effort of the selected row act as their keys. They come after the row,
    // so that they win over it.
    if selected {
        c.click(MODEL_X, y, 2, 1, Act::Key(Key::Left));
        if shown.ends_with('›') {
            c.click(end - 2, y, 2, 1, Act::Key(Key::Right));
        }
        c.click(
            EFFORT_X,
            y,
            width(&effort) as i32,
            1,
            Act::Key(Key::Char('e')),
        );
    }
    let (source, color) = if changed {
        ("• not pinned".to_string(), T.warn)
    } else if line.pinned {
        let since = line
            .since
            .as_deref()
            .map(|s| format!(" {}", s.get(..10).unwrap_or(s)))
            .unwrap_or_default();
        (format!("pinned{since}"), T.fg)
    } else if line.adapter {
        ("adapter".to_string(), T.dim)
    } else {
        ("default".to_string(), T.dim)
    };
    c.put(SOURCE_X, y, &source, ink(color));
    let measured = match line.measured(&model) {
        Some(e) if e.turns > 0 => format!(
            "{} · {} a turn",
            crate::app::text::count(e.turns as usize, "turn"),
            money(e.cost_per_turn)
        ),
        _ => "no turn yet".to_string(),
    };
    c.right(c.w - 3, y, &measured, ink(T.faint));
    let armed = selected && changed && app.armed_act() == Some(&Act::Decide(Key::Enter));
    let reason = if armed {
        "Click again to pin the change.".to_string()
    } else if changed {
        "The change is not pinned yet. Enter pins it, and Esc drops it.".to_string()
    } else if line.pinned && line.adapter {
        format!(
            "The default of the adapter {} {} was: {}",
            line.default_model, line.default_effort, line.reason
        )
    } else if line.pinned {
        format!(
            "The default {} {} was: {}",
            line.default_model, line.default_effort, line.reason
        )
    } else if line.adapter && line.reason.is_empty() {
        "The adapter defines this agent and gives no reason.".to_string()
    } else {
        line.reason.clone()
    };
    let color = if changed { T.warn } else { T.dim };
    c.put(
        MODEL_X + 2,
        y + 1,
        &cut(&reason, (c.w - 3 - MODEL_X - 2) as usize),
        ink(color),
    );
}

/// The recommendation of the model table, with the key of the app in place of a command.
pub fn advice(line: &ModelLine) -> String {
    let text = line
        .recommendation
        .split(". Run `vision model set")
        .next()
        .unwrap_or("")
        .trim_end_matches('.');
    // The model table writes its advice in lower case. A sentence starts with a capital
    // letter, unless it starts with the id of a model.
    let model_first = text
        .split_whitespace()
        .next()
        .is_some_and(|w| w.contains('-'));
    let mut chars = text.chars();
    let text: String = match chars.next() {
        Some(first) if !model_first => first.to_uppercase().chain(chars).collect(),
        _ => text.to_string(),
    };
    match &line.recommended {
        Some(model) if *model != line.model => {
            format!("{text}. Choose it with the arrows, then press Enter to pin it.")
        }
        _ => format!("{text}."),
    }
}

fn advice_box(app: &App, c: &mut Canvas, line: &ModelLine, below: i32) {
    let y = (c.h - BOX_H - UNDER_BOX).max(below);
    let h = 5;
    c.frame(2, y, c.w - 4, h, T.line);
    c.put(
        4,
        y,
        &format!(" recommendation for {} ", line.agent),
        ink(T.dim).bold(),
    );
    let room = (c.w - 10) as usize;
    let owner = Owner::Models(line.agent.clone());
    if let Some(chain) = app.chain_of(&owner) {
        let mut rows = Rows::default();
        chain_rows::rows(app, chain, &mut rows, room, usize::MAX);
        // The box has three rows: the step, then the reason and the state after a failure.
        rows.rows.retain(|row| {
            !row.is_empty() && !row.iter().any(|(_, text, _)| text == "It did not work:")
        });
        for (i, row) in rows.rows.iter().take(3).enumerate() {
            for (dx, text, style) in row {
                c.put(2 + dx, y + 1 + i as i32, &cut(text, room), *style);
            }
        }
        return;
    }
    let mut lines = wrap(&advice(line), room);
    lines.truncate(2);
    for (i, text) in lines.iter().enumerate() {
        c.put(5, y + 1 + i as i32, text, ink(T.fg));
    }
    let evidence: Vec<String> = line
        .evidence
        .iter()
        .map(|e| {
            format!(
                "{}: {}, {} a turn",
                e.model,
                crate::app::text::count(e.turns as usize, "turn"),
                money(e.cost_per_turn)
            )
        })
        .collect();
    let text = if evidence.is_empty() {
        "No turn of this agent ended yet, so nothing is measured.".to_string()
    } else {
        format!("Measured: {}", evidence.join(" · "))
    };
    c.put(5, y + 3, &cut(&text, room), ink(T.dim));
}

fn footer(app: &App, c: &mut Canvas) {
    let esc = if app.models.edits.is_empty() {
        "home"
    } else {
        "drop the change"
    };
    // Enter pins and `u` goes back to the default: each one records a change of the model, so a
    // click on it arms first.
    keys_with(
        c,
        &[
            ("↑↓", "agent"),
            ("←→", "model"),
            ("e", "effort"),
            ("enter", "pin"),
            ("u", "back to the default"),
            ("esc", esc),
            ("?", "keys"),
        ],
        &[Key::Enter, Key::Char('u')],
        app.armed_act(),
    );
}

/// The keys of Models.
pub fn help(c: &mut Canvas) {
    let list = [
        ("↑↓  j k", "select an agent"),
        ("←→", "change its model"),
        ("e", "change its effort"),
        ("enter", "pin the values that show"),
        ("u", "go back to the default of Vision"),
        ("esc", "drop the changes that are not pinned, then go home"),
        ("?", "this list of keys"),
        ("q", "quit"),
    ];
    // Enter pins and `u` goes back to the default: their rows are no parts, and their hints in
    // the footer arm on the first click.
    super::cards::keys_card(c, &list, &[Key::Enter, Key::Char('u')]);
}

/// A keys card with this list, in which no key decides.
pub(super) fn help_rows(c: &mut Canvas, list: &[(&str, &str)]) {
    super::cards::keys_card(c, list, &[]);
}
