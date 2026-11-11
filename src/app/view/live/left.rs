//! The left column of the live view: PATH, one row for each step of the adapter with its mark,
//! its time, and its cost, and TEAM, one row for each agent of the model table. When the window
//! has fewer rows than the adapter has steps, the path shows the rows around the current step
//! and says how many are above and below.

use super::layout::Layout;
use crate::app::live::TicketData;
use crate::app::model::App;
use crate::app::now::{mmss, Kind, Now};
use crate::app::text::cut;
use crate::app::view::canvas::{ink, Canvas};
use crate::app::view::header::spinner;
use crate::app::view::theme::{agent_color, T};
use crate::pipeline::money;
use crate::util::time::short_age;

/// The rows of TEAM under the path: a free row, its title, and one row for each agent.
const TEAM_ROWS: i32 = 9;

pub fn draw(app: &App, c: &mut Canvas, l: &Layout, data: &TicketData, now: &Now) {
    c.put(1, 2, "PATH", ink(T.dim).bold());
    let steps = &data.steps;
    let step = data.view.step.as_deref().unwrap_or("");
    let current = steps.iter().position(|s| s.id == step);
    let room = (l.bottom() - 2 - TEAM_ROWS).max(3) as usize;
    let (start, end, above, below) = window(steps.len(), current, room);
    let mut y = 3;
    if above > 0 {
        c.put(3, y, &format!("↑ {above} more"), ink(T.faint));
        y += 1;
    }
    for (i, line) in steps.iter().enumerate().take(end).skip(start) {
        let is_current = Some(i) == current && now.kind != Kind::Done;
        let visited = line.entries > 0 || line.seconds > 0;
        let before = current.is_some_and(|cur| i < cur);
        let (mark, mark_color) = if is_current {
            let mark = if matches!(now.kind, Kind::Agent | Kind::Judge | Kind::Runs) {
                spinner(app)
            } else {
                "◉"
            };
            (mark, tone_color(now))
        } else if visited {
            ("✓", T.ok)
        } else if before {
            ("-", T.faint)
        } else {
            ("○", T.faint)
        };
        c.put(1, y, mark, ink(mark_color));
        let name_ink = if is_current {
            ink(T.fg).bold()
        } else if visited {
            ink(T.dim)
        } else {
            ink(T.faint)
        };
        let x = c.put(3, y, &cut(&line.id, 15), name_ink);
        if line.entries > 1 {
            c.put(x, y, &format!(" ×{}", line.entries), ink(T.faint));
        }
        if is_current {
            let (text, color) = match now.kind {
                Kind::Agent | Kind::Judge | Kind::Runs => {
                    (mmss(now.seconds.unwrap_or(0)), T.accent)
                }
                Kind::Needs | Kind::Paused => ("you".to_string(), T.warn),
                Kind::Stopped => ("stopped".to_string(), T.dim),
                _ => ("waits".to_string(), T.info),
            };
            c.right(l.left_line() - 1, y, &text, ink(color));
        } else if visited {
            if line.seconds > 0 {
                c.right(20, y, &short_age(line.seconds), ink(T.faint));
            }
            if line.cost_usd > 0.0 {
                c.right(l.left_line() - 1, y, &money(line.cost_usd), ink(T.dim));
            }
        }
        y += 1;
    }
    if below > 0 {
        c.put(3, y, &format!("↓ {below} more"), ink(T.faint));
        y += 1;
    }
    if steps.is_empty() {
        c.put(3, y, "the adapter did not load", ink(T.faint));
        y += 1;
    }
    team(c, y + 1, data);
}

fn tone_color(now: &Now) -> crate::app::view::theme::Rgb {
    crate::app::view::home::tone_color(now.tone)
}

/// The first and the end index of the steps that show, and how many are above and below, for
/// `rows` rows. The rows of the marks above and below count.
pub fn window(n: usize, current: Option<usize>, rows: usize) -> (usize, usize, usize, usize) {
    if n <= rows {
        return (0, n, 0, 0);
    }
    let rows = rows.max(3);
    let cur = current.unwrap_or(0);
    // With a mark at each end, `rows - 2` steps show.
    let k = rows - 2;
    let start = cur.saturating_sub(k / 2);
    if start == 0 {
        // No mark above: one more step shows.
        return (0, k + 1, 0, n - (k + 1));
    }
    if start + k >= n {
        let start = n - (k + 1);
        return (start, n, start, 0);
    }
    (start, start + k, start, n - (start + k))
}

/// TEAM: the agent that works now in its color with a filled dot, each agent that did a turn for
/// the ticket with a check mark, and the others dim.
fn team(c: &mut Canvas, y: i32, data: &TicketData) {
    c.put(1, y, "TEAM", ink(T.dim).bold());
    let working: Vec<&str> = data
        .facts
        .work
        .iter()
        .chain(&data.facts.judge)
        .map(|t| t.agent.as_str())
        .collect();
    for (i, member) in data.team.iter().enumerate() {
        let yy = y + 1 + i as i32;
        let color = agent_color(&member.agent);
        let works = working.contains(&member.agent.as_str());
        let did = data.facts.agents.contains(&member.agent);
        let (mark, mark_ink, name_ink, model_ink) = if works {
            ("●", ink(color), ink(color).bold(), ink(T.fg))
        } else if did {
            ("✓", ink(T.dim), ink(T.dim), ink(T.faint))
        } else {
            ("·", ink(T.faint), ink(T.faint), ink(T.faint))
        };
        c.put(1, yy, mark, mark_ink);
        c.put(3, yy, &cut(&member.agent, 9), name_ink);
        let model = member
            .model
            .strip_prefix("claude-")
            .unwrap_or(&member.model);
        c.put(
            13,
            yy,
            &cut(&format!("{model:<8} {}", member.effort), 14),
            model_ink,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::window;

    #[test]
    fn the_window_of_the_path_holds_the_current_step() {
        assert_eq!(window(12, Some(5), 15), (0, 12, 0, 0));
        assert_eq!(window(20, Some(1), 10), (0, 9, 0, 11));
        assert_eq!(window(20, Some(10), 10), (6, 14, 6, 6));
        assert_eq!(window(20, Some(19), 10), (11, 20, 11, 0));
    }
}
