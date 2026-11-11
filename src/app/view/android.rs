//! The android of the loader: pixel art of 31 by 32 pixels, drawn with the upper half block so
//! that one cell shows two pixels. Its colors follow `androidColor` of the design: the picture
//! appears pixel by pixel, the gem on the head pulses, a light point moves across each eye, and
//! the eyes close for a moment now and then.

use super::canvas::Canvas;
use super::theme::{Rgb, T, WHITE};

/// The left half of the picture and its center column: 16 pixels on each row. The right side
/// mirrors the first 15 pixels.
const LEFT: [&str; 32] = [
    "................",
    "...........HHHHH",
    ".........HHhhhhh",
    "........Hhhhhhhh",
    ".......Hhhhhhhhh",
    ".......HhhHHHHHH",
    "......HhhHFFFFFG",
    "......HhhHFFFFGG",
    "......HhhHFFFGGG",
    "......HhhHFFFFGG",
    "......HhhHFFFFFG",
    "......HhhHFFFFFF",
    "......HhhHEEEEFF",
    "......HhhHfeeeFF",
    "......HhhHFFFFFF",
    "......HhhHFFFFFL",
    "......HhhHfFFFFL",
    ".......HhHfFFFFF",
    ".......HhhHfFMMM",
    "........HhHFFFFF",
    "........HhhHFFFF",
    ".........HhhHHHH",
    "...........HNNNN",
    "............NNNN",
    ".........SSSNNNN",
    "......SSSsssSSSS",
    "....SSsssssssCss",
    "..SSssssssssssCs",
    ".SSssssssssssssC",
    "SSssssssssssssss",
    "SssssssssssssssS",
    "SssssssssssssssS",
];

/// The width of the picture in pixels and cells.
pub const WIDTH: i32 = 31;
/// The height of the picture in cells: two pixels for each cell.
pub const ROWS: i32 = 16;
/// The seconds that the picture takes to appear.
pub const APPEAR_SECONDS: f64 = 0.9;

/// The letter of the pixel at column `x` (0 to 30) and row `y` (0 to 31).
fn letter(x: usize, y: usize) -> u8 {
    let row = LEFT[y].as_bytes();
    if x < 16 {
        row[x]
    } else {
        row[30 - x]
    }
}

/// The noise of the design, from 0 to 1, for the order in which the pixels appear.
fn hash(x: u32, y: u32) -> f64 {
    let mut n = (x + 1).wrapping_mul(374_761_393) ^ (y + 1).wrapping_mul(668_265_263);
    n = (n ^ (n >> 13)).wrapping_mul(1_274_126_177);
    f64::from(n ^ (n >> 16)) / 4_294_967_296.0
}

/// The color of one pixel at `t` seconds after the start, or `None` for no pixel.
pub fn color(x: usize, y: usize, t: f64) -> Option<Rgb> {
    let p = &T.px;
    let ch = letter(x, y);
    let base = match ch {
        b'H' => p.head_dark,
        b'h' => p.head,
        b'F' => p.face,
        b'f' => p.face_dark,
        b'E' => p.eye,
        b'e' => p.eye_dark,
        b'G' => p.gem,
        b'L' => p.lip,
        b'M' => p.mouth,
        b'N' => p.neck,
        b'S' => p.suit_dark,
        b's' => p.suit,
        b'C' => p.chest,
        _ => return None,
    };
    let pulse = 0.5 + 0.5 * (t * 5.0).sin();
    let blink = t % 3.4 > 3.25;
    let scan = (t * 7.0) % 7.0;
    let mut c = base;
    match ch {
        b'G' => c = p.gem.mix(WHITE, pulse * 0.75),
        b'C' => c = p.chest.mix(p.gem, pulse * 0.6),
        b'E' | b'e' => {
            if blink {
                c = p.face_dark;
            } else {
                let ex = if x < 15 {
                    x as f64 - 10.0
                } else {
                    x as f64 - 17.0
                };
                if (ex - (scan - 1.5)).abs() < 0.9 {
                    c = WHITE;
                }
            }
        }
        b'F' => {
            let d = ((x as f64 - 15.0).powi(2) + (y as f64 - 8.0).powi(2)).sqrt();
            if d < 4.0 {
                c = c.mix(p.gem, pulse * 0.3 * (1.0 - d / 4.0));
            }
        }
        _ => {}
    }
    // The picture appears pixel by pixel; a pixel that just came shows faint first.
    let a = hash(x as u32, y as u32) * 0.55 + (y as f64 / 32.0) * 0.45;
    let progress = t * 1.1 / APPEAR_SECONDS;
    if progress < a {
        return None;
    }
    if progress < a + 0.1 {
        return Some(c.mix(T.bg, 0.6));
    }
    Some(c)
}

/// Draws the picture with its top left cell at `x`, `y`.
pub fn draw(c: &mut Canvas, x: i32, y: i32, t: f64) {
    for row in 0..ROWS {
        for col in 0..WIDTH {
            let top = color(col as usize, (row * 2) as usize, t);
            let bottom = color(col as usize, (row * 2 + 1) as usize, t);
            c.pixel(x + col, y + row, top, bottom);
        }
    }
}

/// The cells of the gem at the top of the head, as (column, row) from the top left cell of the
/// picture. A test reads their color.
pub fn gem_cells() -> Vec<(i32, i32)> {
    let mut out = Vec::new();
    for row in 0..ROWS {
        for col in 0..WIDTH {
            let top = letter(col as usize, (row * 2) as usize);
            if top == b'G' {
                out.push((col, row));
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_picture_mirrors_its_left_side() {
        for y in 0..32 {
            for x in 0..15 {
                assert_eq!(letter(x, y), letter(30 - x, y));
            }
        }
    }

    #[test]
    fn the_picture_is_whole_after_its_appearance() {
        assert!(color(15, 8, 0.0).is_none());
        assert!(color(15, 8, APPEAR_SECONDS).is_some());
        for y in 0..32 {
            for x in 0..31 {
                let pixel = letter(x, y) != b'.';
                assert_eq!(
                    color(x, y, APPEAR_SECONDS + 0.01).is_some(),
                    pixel,
                    "{x} {y}"
                );
            }
        }
    }
}
