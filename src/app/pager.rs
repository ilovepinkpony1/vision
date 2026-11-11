//! One pager for the long texts of a ticket: the log (`l`), the events of one turn (Enter on a
//! turn of the log), the plan (`p`), and the commits that the forge does not have (`d`). A
//! pager has a title, an optional row of column names, lines of colored parts, and a position.
//! The log also has a cursor on a row, so that Enter can open the events of a turn.
//!
//! The keys: up, down, `k`, `j`, PageUp, PageDown, Home, End, `g`, `G`, and Esc.

use super::model::Hue;

/// What a pager shows.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PagerKind {
    Log,
    /// The events of one turn.
    Turn {
        id: String,
    },
    Plan,
    Diff,
}

/// The state of the text of a pager.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PagerState {
    Loading,
    Ready,
    Failed(String),
}

/// One part of a line: a text in a color. With `width`, the part takes that many cells, cut or
/// filled with spaces, so that columns line up.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Span {
    pub text: String,
    pub hue: Hue,
    pub bold: bool,
    pub width: Option<usize>,
}

impl Span {
    pub fn new(text: impl Into<String>, hue: Hue) -> Span {
        Span {
            text: text.into(),
            hue,
            bold: false,
            width: None,
        }
    }

    pub fn bold(mut self) -> Span {
        self.bold = true;
        self
    }

    pub fn width(mut self, width: usize) -> Span {
        self.width = Some(width);
        self
    }
}

/// One line of a pager. `turn` names the turn that Enter opens, for a line of the log.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PagerLine {
    pub spans: Vec<Span>,
    pub turn: Option<String>,
}

impl PagerLine {
    pub fn of(spans: Vec<Span>) -> PagerLine {
        PagerLine { spans, turn: None }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Pager {
    pub kind: PagerKind,
    /// The words after the key in the header, such as `log`.
    pub title: String,
    /// The names of the columns, above the lines.
    pub head: Option<PagerLine>,
    pub lines: Vec<PagerLine>,
    /// The first line that shows.
    pub top: usize,
    /// The row of the cursor, for a pager whose rows open something.
    pub cursor: Option<usize>,
    pub state: PagerState,
}

impl Pager {
    /// A pager whose text loads now.
    pub fn loading(kind: PagerKind, title: impl Into<String>) -> Pager {
        Pager {
            kind,
            title: title.into(),
            head: None,
            lines: Vec::new(),
            top: 0,
            cursor: None,
            state: PagerState::Loading,
        }
    }

    /// Fills the pager with its text. The log puts its cursor on the last row and shows the
    /// end; each other pager shows its start.
    pub fn fill(&mut self, head: Option<PagerLine>, lines: Vec<PagerLine>, rows: usize) {
        self.head = head;
        self.lines = lines;
        self.state = PagerState::Ready;
        if self.kind == PagerKind::Log && !self.lines.is_empty() {
            self.cursor = Some(self.lines.len() - 1);
            self.top = self.lines.len().saturating_sub(rows.max(1));
        } else {
            self.top = 0;
        }
    }

    /// The last value of `top` that still fills the rows.
    fn max_top(&self, rows: usize) -> usize {
        self.lines.len().saturating_sub(rows.max(1))
    }

    /// Moves by `by` lines (or rows of the cursor), up when negative.
    pub fn step(&mut self, by: i64, rows: usize) {
        let rows = rows.max(1);
        if let Some(cursor) = self.cursor {
            let last = self.lines.len().saturating_sub(1) as i64;
            let next = (cursor as i64 + by).clamp(0, last) as usize;
            self.cursor = Some(next);
            if next < self.top {
                self.top = next;
            } else if next >= self.top + rows {
                self.top = next + 1 - rows;
            }
            return;
        }
        let next = (self.top as i64 + by).clamp(0, self.max_top(rows) as i64);
        self.top = next as usize;
    }

    /// The start of the text.
    pub fn first(&mut self) {
        self.top = 0;
        if self.cursor.is_some() {
            self.cursor = Some(0);
        }
    }

    /// The end of the text.
    pub fn last(&mut self, rows: usize) {
        self.top = self.max_top(rows);
        if self.cursor.is_some() {
            self.cursor = Some(self.lines.len().saturating_sub(1));
        }
    }

    /// The turn of the row of the cursor, if it names one.
    pub fn selected_turn(&self) -> Option<&str> {
        self.lines.get(self.cursor?)?.turn.as_deref()
    }
}

/// The rows of the text of a pager in a window `height` rows tall: under the header and the
/// line under it, the row of column names and its line, and above the line and the keys.
pub fn body_rows(height: u16, head: bool) -> usize {
    let top = if head { 5 } else { 3 };
    usize::from(height).saturating_sub(top + 3)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pager(kind: PagerKind, n: usize) -> Pager {
        let mut p = Pager::loading(kind, "x");
        let lines = (0..n)
            .map(|i| PagerLine::of(vec![Span::new(i.to_string(), Hue::Fg)]))
            .collect();
        p.fill(None, lines, 10);
        p
    }

    #[test]
    fn a_pager_scrolls_inside_its_text() {
        let mut p = pager(PagerKind::Plan, 25);
        assert_eq!(p.top, 0);
        p.step(-1, 10);
        assert_eq!(p.top, 0);
        p.step(10, 10);
        assert_eq!(p.top, 10);
        p.step(10, 10);
        assert_eq!(p.top, 15, "the last page stays full");
        p.first();
        assert_eq!(p.top, 0);
        p.last(10);
        assert_eq!(p.top, 15);
    }

    #[test]
    fn the_cursor_of_the_log_keeps_its_row_in_view() {
        let mut p = pager(PagerKind::Log, 25);
        assert_eq!((p.cursor, p.top), (Some(24), 15));
        p.step(-12, 10);
        assert_eq!((p.cursor, p.top), (Some(12), 12));
        p.step(1, 10);
        assert_eq!((p.cursor, p.top), (Some(13), 12));
        p.first();
        assert_eq!((p.cursor, p.top), (Some(0), 0));
        p.step(-3, 10);
        assert_eq!(p.cursor, Some(0));
    }
}
