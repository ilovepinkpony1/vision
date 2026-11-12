//! The picture of the loader on the canvas: each cell of [`crate::app::picture::Picture`] with
//! its colors and its bold, centered. The loader fits the picture to its room first.

use super::canvas::{ink, Canvas};
use super::theme::T;
use crate::app::picture::Picture;

/// Draws the picture, centered, with its top at `y`.
pub fn draw(c: &mut Canvas, picture: &Picture, y: i32) {
    let x0 = (c.w - picture.width() as i32) / 2;
    for (r, row) in picture.rows.iter().enumerate() {
        for (col, cell) in row.iter().enumerate() {
            let Some(ch) = cell.ch else {
                continue;
            };
            let mut style = ink(cell.fg.unwrap_or(T.fg));
            if let Some(bg) = cell.bg {
                style = style.on(bg);
            }
            if cell.bold {
                style = style.bold();
            }
            c.put(x0 + col as i32, y + r as i32, &ch.to_string(), style);
        }
    }
}
