//! The small android head of an agent in the panel NOW (`FACE` and `faceColor` of the design):
//! 9 by 8 pixels, two pixels in each cell, in the color of the agent. While the agent works, the
//! gem on its head pulses, a light point moves between the two pixels of each eye, and the
//! eyes blink now and then. At rest, the head is dim and its eyes are closed.

use super::canvas::Canvas;
use super::theme::{Rgb, T, WHITE};

const FACE: [&str; 8] = [
    "..KKKKK..",
    ".KKBGBKK.",
    ".KBBBBBK.",
    ".KEEBEEK.",
    ".KBBBBBK.",
    "..KBbBK..",
    "...KKK...",
    ".SSSSSSS.",
];

/// The width of the head in cells, and its height in rows.
pub const WIDTH: i32 = 9;
pub const ROWS: i32 = 4;

/// The color of the resting head.
pub const REST: Rgb = Rgb::hex(0x6b7385);

/// The color of one pixel of the head, or `None` for no pixel. `x` is the column from 0 to 8.
pub fn color(ch: u8, x: usize, t: f64, base: Rgb, working: bool) -> Option<Rgb> {
    let dark = base.mix(T.bg, 0.45);
    let blink = t % 2.9 > 2.75;
    Some(match ch {
        b'K' => base.mix(T.bg, 0.62),
        b'S' => base.mix(T.bg, 0.72),
        b'B' => base.mix(T.bg, if working { 0.18 } else { 0.55 }),
        b'b' => dark,
        b'G' if working => WHITE.mix(base, 0.5 + 0.5 * (t * 5.0).sin()),
        b'G' => dark,
        b'E' if !working || blink => dark,
        b'E' => {
            // The light point looks left, then right: the outer or the inner pixel of each eye.
            let left = (t * 0.8).floor() as i64 % 2 == 0;
            let outer = x == 2 || x == 6;
            if outer == left {
                WHITE
            } else {
                WHITE.mix(base, 0.55)
            }
        }
        _ => return None,
    })
}

/// Draws the head with its top left cell at `x`, `y`.
pub fn draw(c: &mut Canvas, x: i32, y: i32, t: f64, base: Rgb, working: bool) {
    for row in 0..ROWS {
        let top = FACE[(row * 2) as usize].as_bytes();
        let bottom = FACE[(row * 2 + 1) as usize].as_bytes();
        for col in 0..WIDTH {
            let i = col as usize;
            let up = color(top[i], i, t, base, working);
            let down = color(bottom[i], i, t, base, working);
            c.pixel(x + col, y + row, up, down);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_eyes_move_while_it_works_and_close_at_rest() {
        let base = Rgb::hex(0xf5c542);
        let at = |t: f64, x: usize| color(b'E', x, t, base, true);
        assert_eq!(at(0.1, 2), Some(WHITE));
        assert_ne!(at(0.1, 3), Some(WHITE));
        assert_eq!(at(1.3, 3), Some(WHITE));
        assert_ne!(at(1.3, 2), Some(WHITE));
        assert_eq!(color(b'E', 2, 0.1, base, false), Some(base.mix(T.bg, 0.45)));
        assert_eq!(color(b'.', 0, 0.1, base, true), None);
    }
}
