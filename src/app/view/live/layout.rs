//! The places of the parts of the live view in a window of any allowed size, as `drawTicket` of
//! the design puts them at 110 by 32: the left column and the right column keep their widths,
//! the middle takes the rest, and the activity takes the rows that are left. The update uses
//! the same places, so that a scroll counts the lines that the view shows.

/// The width of the left column, before its line.
pub const LEFT_W: i32 = 27;
/// The width of the right column, after its line and a space.
pub const RIGHT_W: i32 = 23;
/// The first row of the lines of the activity.
pub const FEED_Y: i32 = 9;
/// The cells of the time and the word before the text of a line of the activity.
pub const FEED_TEXT_DX: i32 = 15;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Layout {
    pub w: i32,
    pub h: i32,
}

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
        assert_eq!((l.feed_h(), l.feed_text_w()), (20, 39));
        let wide = Layout::of(160, 45);
        assert_eq!((wide.mid_w(), wide.feed_h()), (104, 33));
    }
}
