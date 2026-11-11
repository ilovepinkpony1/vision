//! A character grid over the buffer of a frame, with the few moves that the screens of the
//! design use: put text at a cell, put it flush right or centered, wipe or tint a part, draw a
//! box, draw two pixels in one cell, and dim the whole screen behind a card. Each text goes
//! through [`super::super::text`] rules: a control character becomes a space, and a wide
//! character never splits at the right edge.

use ratatui::buffer::Buffer;
use ratatui::style::Modifier;

use super::theme::{Rgb, T};
use crate::app::text::{char_width, width};

/// How a text looks: its color, an optional background, and bold.
#[derive(Clone, Copy, Debug)]
pub struct Ink {
    pub fg: Rgb,
    pub bg: Option<Rgb>,
    pub bold: bool,
}

/// Plain text in a color.
pub fn ink(fg: Rgb) -> Ink {
    Ink {
        fg,
        bg: None,
        bold: false,
    }
}

impl Ink {
    pub fn bold(mut self) -> Ink {
        self.bold = true;
        self
    }

    pub fn on(mut self, bg: Rgb) -> Ink {
        self.bg = Some(bg);
        self
    }
}

/// The grid of one frame.
pub struct Canvas<'a> {
    buf: &'a mut Buffer,
    pub w: i32,
    pub h: i32,
    /// True when the terminal shows no color: dimming then uses the dim attribute.
    plain: bool,
}

impl<'a> Canvas<'a> {
    pub fn new(buf: &'a mut Buffer, plain: bool) -> Canvas<'a> {
        let area = buf.area;
        Canvas {
            w: i32::from(area.width),
            h: i32::from(area.height),
            buf,
            plain,
        }
    }

    fn cell(&mut self, x: i32, y: i32) -> Option<&mut ratatui::buffer::Cell> {
        if x < 0 || y < 0 || x >= self.w || y >= self.h {
            return None;
        }
        let area = self.buf.area;
        self.buf.cell_mut((area.x + x as u16, area.y + y as u16))
    }

    /// Paints each cell with the background of the app.
    pub fn fill(&mut self) {
        for cell in self.buf.content.iter_mut() {
            cell.reset();
            cell.set_symbol(" ");
            cell.fg = T.fg.color();
            cell.bg = T.bg.color();
        }
    }

    /// Puts a text at a cell. Gives the column after it.
    pub fn put(&mut self, x: i32, y: i32, text: &str, ink: Ink) -> i32 {
        let mut cx = x;
        let mut last: Option<i32> = None;
        for c in text.chars() {
            let c = if c.is_control() { ' ' } else { c };
            let w = char_width(c) as i32;
            if w == 0 {
                // A mark that joins the character before it, such as an accent.
                if let Some(lx) = last {
                    if let Some(cell) = self.cell(lx, y) {
                        let symbol = format!("{}{c}", cell.symbol());
                        cell.set_symbol(&symbol);
                    }
                }
                continue;
            }
            let fits = cx >= 0 && cx + w <= self.w;
            let symbol = if fits { c } else { ' ' };
            for i in 0..w {
                if let Some(cell) = self.cell(cx + i, y) {
                    if i == 0 {
                        cell.set_char(symbol);
                    } else {
                        cell.set_symbol(if fits { "" } else { " " });
                    }
                    cell.fg = ink.fg.color();
                    if let Some(bg) = ink.bg {
                        cell.bg = bg.color();
                    }
                    if ink.bold {
                        cell.modifier.insert(Modifier::BOLD);
                    } else {
                        cell.modifier.remove(Modifier::BOLD);
                    }
                }
                if !fits {
                    break;
                }
            }
            last = Some(cx);
            cx += w;
        }
        cx
    }

    /// Puts a text so that it ends at the column `x_end`. Gives the column before it.
    pub fn right(&mut self, x_end: i32, y: i32, text: &str, ink: Ink) -> i32 {
        let w = width(text) as i32;
        self.put(x_end - w + 1, y, text, ink);
        x_end - w
    }

    /// Puts a text in the middle of a row.
    pub fn center(&mut self, y: i32, text: &str, ink: Ink) {
        let w = width(text) as i32;
        self.put((self.w - w).div_euclid(2), y, text, ink);
    }

    /// Clears a part of the grid to spaces on a background.
    pub fn wipe(&mut self, x: i32, y: i32, w: i32, h: i32, bg: Rgb) {
        for yy in y..y + h {
            for xx in x..x + w {
                if let Some(cell) = self.cell(xx, yy) {
                    cell.set_symbol(" ");
                    cell.fg = T.fg.color();
                    cell.bg = bg.color();
                    cell.modifier = Modifier::empty();
                }
            }
        }
    }

    /// Gives a part of a row a background and keeps its text.
    pub fn tint(&mut self, x: i32, y: i32, w: i32, bg: Rgb) {
        for xx in x..x + w {
            if let Some(cell) = self.cell(xx, y) {
                cell.bg = bg.color();
            }
        }
    }

    /// A box with round corners, and an optional title and tail on its top line.
    pub fn frame(&mut self, x: i32, y: i32, w: i32, h: i32, color: Rgb) {
        if w < 2 || h < 2 {
            return;
        }
        let line = "─".repeat((w - 2) as usize);
        self.put(x, y, &format!("╭{line}╮"), ink(color));
        for i in 1..h - 1 {
            self.put(x, y + i, "│", ink(color));
            self.put(x + w - 1, y + i, "│", ink(color));
        }
        self.put(x, y + h - 1, &format!("╰{line}╯"), ink(color));
    }

    /// Two pixels in one cell: the upper one as the color of the upper half block, the lower
    /// one as the background. A missing pixel shows the background of the app.
    pub fn pixel(&mut self, x: i32, y: i32, top: Option<Rgb>, bottom: Option<Rgb>) {
        if top.is_none() && bottom.is_none() {
            return;
        }
        if let Some(cell) = self.cell(x, y) {
            cell.set_symbol("▀");
            cell.fg = top.unwrap_or(T.bg).color();
            cell.bg = bottom.unwrap_or(T.bg).color();
            cell.modifier = Modifier::empty();
        }
    }

    /// Dims each cell, as the design does behind a card. With no color, each cell gets the dim
    /// attribute.
    pub fn dim_all(&mut self) {
        for cell in self.buf.content.iter_mut() {
            if self.plain {
                cell.modifier.insert(Modifier::DIM);
            } else {
                let fg = Rgb::of(cell.fg).unwrap_or(T.fg);
                cell.fg = fg.mix(T.bg, 0.62).color();
                if let Some(bg) = Rgb::of(cell.bg) {
                    cell.bg = bg.mix(T.bg, 0.6).color();
                }
            }
            cell.modifier.remove(Modifier::BOLD);
        }
    }
}
