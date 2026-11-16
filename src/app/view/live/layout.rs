//! The places of the parts of the live view in a window of any allowed size, as `drawTicket` of
//! the design puts them at 110 by 32: the left column and the right column keep their widths,
//! the middle takes the rest, and the activity takes the rows that are left. The update uses
//! the same places, so that a scroll counts the lines that the view shows. The tabs of the
//! activity are placed here too, so that a click can find the tab that it hit.

use crate::app::feed::{Filter, FILTERS};
use crate::app::text::width;

/// The width of the left column, before its line.
pub const LEFT_W: i32 = 27;
/// The width of the right column, after its line and a space.
pub const RIGHT_W: i32 = 23;
/// The row of the name ACTIVITY.
pub const ACTIVITY_Y: i32 = 8;
/// The row of the tabs of the activity.
pub const TABS_Y: i32 = 9;
/// The first row of the lines of the activity.
pub const FEED_Y: i32 = 10;
/// The cells of the time and the word before the text of a line of the activity.
pub const FEED_TEXT_DX: i32 = 15;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Layout {
    pub w: i32,
    pub h: i32,
}

/// The tabs of the activity that fit the width of the middle, around the tab that shows: each
/// with its filter, its column, and its text, such as `Agents 12`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Tabs {
    pub tabs: Vec<(Filter, i32, String)>,
    /// True when the tabs before the first one do not fit: a mark shows on the left.
    pub left: bool,
    /// True when the tabs after the last one do not fit: a mark shows on the right.
    pub right: bool,
}

/// The cells between two tabs, and the cells of a mark with its space.
const TAB_GAP: i32 = 2;

impl Layout {
    pub fn of(w: i32, h: i32) -> Layout {
        Layout { w, h }
    }

    /// The column of the line after the left column.
    pub fn left_line(&self) -> i32 {
        LEFT_W
    }

    /// The first column of the middle.
    pub fn mid_x(&self) -> i32 {
        LEFT_W + 2
    }

    /// The width of the middle.
    pub fn mid_w(&self) -> i32 {
        (self.right_line() - 1 - self.mid_x()).max(10)
    }

    /// The column of the line before the right column.
    pub fn right_line(&self) -> i32 {
        self.w - RIGHT_W - 3
    }

    /// The first column of the right column.
    pub fn right_x(&self) -> i32 {
        self.w - RIGHT_W - 1
    }

    /// The last column of the right column.
    pub fn right_end(&self) -> i32 {
        self.w - 2
    }

    /// The last row of the parts, above the line over the keys.
    pub fn bottom(&self) -> i32 {
        self.h - 4
    }

    /// The rows of the lines of the activity.
    pub fn feed_h(&self) -> i32 {
        (self.bottom() - FEED_Y + 1).max(1)
    }

    /// The width of the text of a line of the activity.
    pub fn feed_text_w(&self) -> usize {
        (self.mid_w() - FEED_TEXT_DX).max(10) as usize
    }

    /// The tabs of the activity with the count of each kind, as many as fit, around the tab
    /// that shows. The tabs on its left come first, so that `All` shows when it can.
    pub fn tabs(&self, counts: &[usize; 7], shown: Filter) -> Tabs {
        let texts: Vec<String> = FILTERS
            .iter()
            .zip(counts)
            .map(|(f, n)| format!("{} {n}", f.label()))
            .collect();
        let last = texts.len() - 1;
        let wide = |a: usize, b: usize| -> i32 {
            let tabs: i32 = texts[a..=b].iter().map(|t| width(t) as i32).sum::<i32>()
                + TAB_GAP * (b - a) as i32;
            let marks = if a > 0 { TAB_GAP } else { 0 } + if b < last { TAB_GAP } else { 0 };
            tabs + marks
        };
        let room = self.mid_w();
        let (mut a, mut b) = (shown.index(), shown.index());
        loop {
            if a > 0 && wide(a - 1, b) <= room {
                a -= 1;
            } else if b < last && wide(a, b + 1) <= room {
                b += 1;
            } else {
                break;
            }
        }
        let mut x = self.mid_x() + if a > 0 { TAB_GAP } else { 0 };
        let mut tabs = Vec::new();
        for i in a..=b {
            tabs.push((FILTERS[i], x, texts[i].clone()));
            x += width(&texts[i]) as i32 + TAB_GAP;
        }
        Tabs {
            tabs,
            left: a > 0,
            right: b < last,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_columns_of_the_design_at_110_by_32() {
        let l = Layout::of(110, 32);
        assert_eq!(
            (l.left_line(), l.mid_x(), l.right_line(), l.right_x()),
            (27, 29, 84, 86)
        );
        assert_eq!((l.mid_w(), l.right_end(), l.bottom()), (54, 108, 28));
        assert_eq!((l.feed_h(), l.feed_text_w()), (19, 39));
        let wide = Layout::of(160, 45);
        assert_eq!((wide.mid_w(), wide.feed_h()), (104, 32));
    }

    #[test]
    fn the_tabs_fit_the_middle_around_the_tab_that_shows() {
        let counts = [87, 40, 6, 25, 3, 1, 4];
        let wide = Layout::of(160, 45).tabs(&counts, Filter::All);
        assert_eq!(wide.tabs.len(), 7);
        assert!(!wide.left && !wide.right);
        assert_eq!(wide.tabs[0], (Filter::All, 29, "All 87".to_string()));
        assert_eq!(wide.tabs[1].1, 29 + 6 + 2);
        let narrow = Layout::of(100, 28);
        let first = narrow.tabs(&counts, Filter::All);
        let names: Vec<Filter> = first.tabs.iter().map(|t| t.0).collect();
        assert_eq!(
            names,
            [
                Filter::All,
                Filter::Agents,
                Filter::Questions,
                Filter::Steps
            ]
        );
        assert!(!first.left && first.right);
        let last = narrow.tabs(&counts, Filter::You);
        assert!(last.left && !last.right);
        assert_eq!(last.tabs.last().map(|t| t.0), Some(Filter::You));
        let end = last.tabs.last().map(|t| t.1 + width(&t.2) as i32).unwrap();
        assert!(
            end <= narrow.mid_x() + narrow.mid_w(),
            "the tabs fit the middle"
        );
    }
}
