//! The steps of a chain on its card: a mark for each step that the card lists (a dot for a
//! step that comes, a spinner for the step that runs, a check for a step that is done, and a
//! cross for the step that failed), and after a failure the reason and what is true now, in
//! texts of at most `width` columns.

use super::canvas::ink;
use super::cards::Rows;
use super::header::spinner;
use super::theme::T;
use crate::app::chain::{Chain, End, Mark};
use crate::app::model::App;

pub(super) fn rows(app: &App, chain: &Chain, rows: &mut Rows, width: usize) {
    for step in chain.steps.iter().filter(|s| s.shown) {
        let (mark, color) = match step.mark {
            Mark::Todo => ("·".to_string(), T.faint),
            Mark::Runs => (spinner(app).to_string(), T.accent),
            Mark::Done => ("✓".to_string(), T.ok),
            Mark::Failed => ("✗".to_string(), T.bad),
        };
        let text = if step.mark == Mark::Todo { T.dim } else { T.fg };
        rows.rows.push(vec![
            (3, mark, ink(color)),
            (5, step.label.clone(), ink(text)),
        ]);
    }
    if let Some(End::Failed { text, state }) = &chain.end {
        rows.blank();
        rows.text(3, "It did not work:", ink(T.bad).bold());
        rows.para(3, text, width, ink(T.bad));
        let mut first = true;
        for line in crate::app::text::wrap(state, width.saturating_sub(5).max(10)) {
            let mut row = Vec::new();
            if first {
                row.push((3, "Now".to_string(), ink(T.dim)));
                first = false;
            }
            row.push((8, line, ink(T.fg)));
            rows.rows.push(row);
        }
    }
}
