//! The picture of the loader: `loader.ans` of the home folder, a text with color codes as a
//! tool such as `chafa` writes it for a terminal. The file is data. Only the codes of color and
//! style (SGR: reset, bold, the 16 colors, the 256 colors, and 24-bit colors, for the
//! foreground and the background) count; each other escape sequence and each control
//! character go away, and no byte of the file reaches the terminal as it is. The picture keeps
//! at most [`MAX_COLS`] columns and [`MAX_ROWS`] rows. A file that is empty, larger than
//! [`MAX_BYTES`] (1 MiB), or not valid UTF-8 counts as no file.
//!
//! [`Picture::fit`] shrinks a picture that is larger than the room of the loader, with the same
//! shape. A picture of only spaces and blocks ([`BLOCKS`]) shrinks as pixels with mean colors;
//! any other picture keeps the nearest cell.

use std::path::Path;

use super::text::char_width;
use super::view::theme::{Rgb, T};

/// The name of the file in the home folder.
pub const FILE: &str = "loader.ans";
pub const MAX_COLS: usize = 240;
pub const MAX_ROWS: usize = 120;
pub const MAX_BYTES: u64 = 1024 * 1024;

/// The space and the 15 characters of quadrants and halves, by the quadrants that each one
/// covers: bit 0 is the top left quadrant, bit 1 the top right, bit 2 the bottom left, and
/// bit 3 the bottom right.
pub const BLOCKS: [char; 16] = [
    ' ', '▘', '▝', '▀', '▖', '▌', '▞', '▛', '▗', '▚', '▐', '▜', '▄', '▙', '▟', '█',
];

/// A cell that shows nothing: the background of the app shows through it.
pub const BLANK: Cell = Cell {
    ch: Some(' '),
    fg: None,
    bg: None,
    bold: false,
};

/// One cell of the picture. A wide character takes two columns, and the cell after it is a
/// cell with no character.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Cell {
    /// The character, or `None` for the second column of a wide character.
    pub ch: Option<char>,
    pub fg: Option<Rgb>,
    pub bg: Option<Rgb>,
    pub bold: bool,
}

/// A still picture of rows of cells.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Picture {
    pub rows: Vec<Vec<Cell>>,
}

impl Picture {
    /// The width in columns: the widest row.
    pub fn width(&self) -> usize {
        self.rows.iter().map(Vec::len).max().unwrap_or(0)
    }

    pub fn height(&self) -> usize {
        self.rows.len()
    }

    /// The picture shrunk to fit in `max_w` columns and `max_h` rows. The columns and the rows
    /// shrink by the same factor, so the shape stays: a cell is 1 wide and 2 tall. A picture
    /// that fits stays as it is, and a picture never grows. A picture of only spaces and blocks
    /// shrinks as pixels, two by two in each cell; any other picture keeps the nearest cell.
    pub fn fit(&self, max_w: usize, max_h: usize) -> Picture {
        let (w, h) = (self.width(), self.height());
        if w <= max_w && h <= max_h {
            return self.clone();
        }
        if w == 0 || h == 0 || max_w == 0 || max_h == 0 {
            return Picture::default();
        }
        // The side that hits the room takes the size of the room, and the other side shrinks
        // by the same factor, rounded.
        let (tw, th) = if max_w * h <= max_h * w {
            (max_w, ((2 * h * max_w + w) / (2 * w)).max(1))
        } else {
            (((2 * w * max_h + h) / (2 * h)).max(1), max_h)
        };
        if self.is_blocks() {
            self.shrink_pixels(tw, th)
        } else {
            self.shrink_nearest(tw, th)
        }
    }

    /// True when each cell is a space or a block of quadrants.
    fn is_blocks(&self) -> bool {
        self.rows
            .iter()
            .flatten()
            .all(|cell| cell.ch.is_some_and(|c| BLOCKS.contains(&c)))
    }

    /// The nearest cell for each cell of the new size. The first and the last row and column
    /// stay, so a frame around the picture stays whole. A wide character keeps its two
    /// columns; at the right edge, and for a lone second column, a space with its colors comes
    /// in its place.
    fn shrink_nearest(&self, tw: usize, th: usize) -> Picture {
        let (w, h) = (self.width(), self.height());
        let rows = (0..th)
            .map(|ty| {
                let source = &self.rows[nearest(ty, th, h)];
                let mut row = Vec::with_capacity(tw);
                while row.len() < tw {
                    let tx = row.len();
                    let cell = source.get(nearest(tx, tw, w)).copied().unwrap_or(BLANK);
                    match cell.ch {
                        Some(c) if char_width(c) == 2 && tx + 1 < tw => {
                            row.push(cell);
                            row.push(Cell { ch: None, ..cell });
                        }
                        Some(c) if char_width(c) == 2 => row.push(Cell {
                            ch: Some(' '),
                            ..cell
                        }),
                        None => row.push(Cell {
                            ch: Some(' '),
                            ..cell
                        }),
                        Some(_) => row.push(cell),
                    }
                }
                row
            })
            .collect();
        Picture { rows }
    }

    /// Shrinks a picture of blocks as pixels: each cell holds two by two pixels, each new
    /// pixel is the mean of the pixels that it covers, and each new cell gets the block and the
    /// two colors that show its four pixels best.
    fn shrink_pixels(&self, tw: usize, th: usize) -> Picture {
        let (w, h) = (2 * self.width(), 2 * self.height());
        let mut pixels = vec![Px::CLEAR; w * h];
        for (r, row) in self.rows.iter().enumerate() {
            for (c, cell) in row.iter().enumerate() {
                let mask = BLOCKS.iter().position(|b| Some(*b) == cell.ch).unwrap_or(0);
                for q in 0..4 {
                    // The view shows a cell with no color of its own in the color of the text.
                    let color = if mask & (1 << q) != 0 {
                        Some(cell.fg.unwrap_or(T.fg))
                    } else {
                        cell.bg
                    };
                    if let Some(color) = color {
                        pixels[(2 * r + q / 2) * w + 2 * c + q % 2] = Px::of(color);
                    }
                }
            }
        }
        let cols = spans(w, 2 * tw);
        let lines = spans(h, 2 * th);
        let pixel = |x: usize, y: usize| -> Px {
            let mut sum = Px::default();
            for &(sy, wy) in &lines[y] {
                for &(sx, wx) in &cols[x] {
                    sum.add(&pixels[sy * w + sx], wx * wy);
                }
            }
            sum.rounded()
        };
        let rows = (0..th)
            .map(|ty| {
                (0..tw)
                    .map(|tx| {
                        let (x, y) = (2 * tx, 2 * ty);
                        encode(&[
                            pixel(x, y),
                            pixel(x + 1, y),
                            pixel(x, y + 1),
                            pixel(x + 1, y + 1),
                        ])
                    })
                    .collect()
            })
            .collect();
        Picture { rows }
    }
}

/// A picture fit to a room, with that room: its columns and its rows.
#[derive(Clone, Debug, PartialEq)]
pub struct Fitted {
    pub room: (usize, usize),
    pub picture: Picture,
}

impl Fitted {
    pub fn new(picture: &Picture, room: (usize, usize)) -> Fitted {
        Fitted {
            room,
            picture: picture.fit(room.0, room.1),
        }
    }
}

/// The index in a line of `len` for the index `i` of `n`. The first gives the first, and the
/// last gives the last; a line of one takes the middle.
fn nearest(i: usize, n: usize, len: usize) -> usize {
    if n <= 1 {
        return (len - 1) / 2;
    }
    (i * (len - 1) + (n - 1) / 2) / (n - 1)
}

/// For each of `parts` equal parts of a line of `len` pixels, the pixels that the part covers
/// and their weights. The weights of each part add up to 1.
fn spans(len: usize, parts: usize) -> Vec<Vec<(usize, f32)>> {
    (0..parts)
        .map(|i| {
            // In units of 1 / parts of a pixel: the part covers [a, b), pixel j covers
            // [j * parts, (j + 1) * parts).
            let (a, b) = (i * len, (i + 1) * len);
            (a / parts..b.div_ceil(parts))
                .map(|j| {
                    let overlap = b.min((j + 1) * parts) - a.max(j * parts);
                    (j, overlap as f32 / len as f32)
                })
                .collect()
        })
        .collect()
}

/// A pixel while the picture shrinks: its color, with the background of the app for the part
/// that is transparent, and its cover, from 0 (transparent) to 1 (a color of the picture).
#[derive(Clone, Copy, Debug, Default)]
struct Px {
    rgb: [f32; 3],
    cover: f32,
}

impl Px {
    const CLEAR: Px = Px {
        rgb: [T.bg.0 as f32, T.bg.1 as f32, T.bg.2 as f32],
        cover: 0.0,
    };

    fn of(color: Rgb) -> Px {
        Px {
            rgb: [f32::from(color.0), f32::from(color.1), f32::from(color.2)],
            cover: 1.0,
        }
    }

    fn add(&mut self, other: &Px, weight: f32) {
        for k in 0..3 {
            self.rgb[k] += other.rgb[k] * weight;
        }
        self.cover += other.cover * weight;
    }

    /// The pixel on whole steps of color and of cover, so that the noise of the sums leaves an
    /// area of one color with one color.
    fn rounded(self) -> Px {
        Px {
            rgb: self.rgb.map(f32::round),
            cover: (self.cover * 255.0).round() / 255.0,
        }
    }

    /// The squared distance, with the cover on the scale of a channel.
    fn distance(&self, other: &Px) -> f32 {
        let d = |a: f32, b: f32| (a - b) * (a - b);
        (0..3).map(|k| d(self.rgb[k], other.rgb[k])).sum::<f32>()
            + d(self.cover * 255.0, other.cover * 255.0)
    }

    /// The color, or `None` for a pixel that is mostly transparent.
    fn color(&self) -> Option<Rgb> {
        let channel = |v: f32| v.round().clamp(0.0, 255.0) as u8;
        (self.cover >= 0.5).then(|| {
            Rgb(
                channel(self.rgb[0]),
                channel(self.rgb[1]),
                channel(self.rgb[2]),
            )
        })
    }
}

/// The mean of the pixels of a mask, or `None` for an empty mask.
fn mean(pixels: &[Px; 4], mask: usize) -> Option<Px> {
    let count = (mask & 15).count_ones();
    if count == 0 {
        return None;
    }
    let mut sum = Px::default();
    for (q, px) in pixels.iter().enumerate() {
        if mask & (1 << q) != 0 {
            sum.add(px, 1.0 / count as f32);
        }
    }
    Some(sum)
}

/// A mask of two colors replaces the one before it only when its squared error is lower by
/// more than this, so a difference that no eye sees gives no pattern.
const SPLIT: f32 = 1.0;

/// The cell that shows four pixels best: of the 15 masks of quadrants, the one whose two mean
/// colors give the least squared error. The full block comes first, so a cell of one color
/// stays one color. A part that is mostly transparent gets no color, and a cell with no color
/// at all is [`BLANK`]. A cell of one color is a space on that color.
fn encode(pixels: &[Px; 4]) -> Cell {
    let mut best: Option<(f32, usize, Option<Px>, Option<Px>)> = None;
    for mask in [15, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14] {
        let on = mean(pixels, mask);
        let off = mean(pixels, !mask & 15);
        let error: f32 = (0..4)
            .map(|q| {
                let group = if mask & (1 << q) != 0 { on } else { off };
                group.map_or(0.0, |g| pixels[q].distance(&g))
            })
            .sum();
        if best.is_none_or(|b| error + SPLIT < b.0) {
            best = Some((error, mask, on, off));
        }
    }
    let Some((_, mask, on, off)) = best else {
        return BLANK;
    };
    let on = on.and_then(|p| p.color());
    let off = off.and_then(|p| p.color());
    let block = |mask: usize, fg: Rgb, bg: Option<Rgb>| Cell {
        ch: Some(BLOCKS[mask]),
        fg: Some(fg),
        bg,
        bold: false,
    };
    match (on, off) {
        (None, None) => BLANK,
        (Some(color), None) if mask == 15 => Cell {
            bg: Some(color),
            ..BLANK
        },
        (Some(fg), bg) => block(mask, fg, bg),
        (None, Some(fg)) => block(!mask & 15, fg, None),
    }
}

/// Reads the picture of a home folder, or gives `None` for no usable file.
pub fn read(home: &Path) -> Option<Picture> {
    let path = home.join(FILE);
    let meta = std::fs::metadata(&path).ok()?;
    if !meta.is_file() || meta.len() == 0 || meta.len() > MAX_BYTES {
        return None;
    }
    let bytes = std::fs::read(&path).ok()?;
    if bytes.len() as u64 > MAX_BYTES {
        return None;
    }
    parse(std::str::from_utf8(&bytes).ok()?)
}

/// The style that the codes set so far.
#[derive(Clone, Copy, Debug, Default)]
struct Style {
    fg: Option<Rgb>,
    bg: Option<Rgb>,
    bold: bool,
}

/// The picture of a text, or `None` when it has no visible cell.
pub fn parse(text: &str) -> Option<Picture> {
    let mut rows: Vec<Vec<Cell>> = vec![Vec::new()];
    let mut style = Style::default();
    let chars: Vec<char> = text.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        i += 1;
        match c {
            '\n' => {
                if rows.len() >= MAX_ROWS {
                    break;
                }
                rows.push(Vec::new());
            }
            '\u{1b}' => i = escape(&chars, i, &mut style),
            '\u{9b}' => i = csi(&chars, i, &mut style),
            '\u{9d}' | '\u{90}' | '\u{98}' | '\u{9e}' | '\u{9f}' => i = string_end(&chars, i),
            c if c.is_control() => {}
            c => {
                let row = rows.last_mut().expect("one row at least");
                let w = char_width(c);
                if w == 0 || row.len() + w > MAX_COLS {
                    continue;
                }
                row.push(Cell {
                    ch: Some(c),
                    fg: style.fg,
                    bg: style.bg,
                    bold: style.bold,
                });
                if w == 2 {
                    row.push(Cell {
                        ch: None,
                        fg: style.fg,
                        bg: style.bg,
                        bold: style.bold,
                    });
                }
            }
        }
    }
    while rows.last().is_some_and(Vec::is_empty) {
        rows.pop();
    }
    let visible = rows
        .iter()
        .flatten()
        .any(|cell| cell.bg.is_some() || cell.ch.is_some_and(|c| !c.is_whitespace()));
    visible.then_some(Picture { rows })
}

/// The rest of a sequence after ESC at `i`. Gives the index after it.
fn escape(chars: &[char], i: usize, style: &mut Style) -> usize {
    match chars.get(i) {
        None => i,
        Some('[') => csi(chars, i + 1, style),
        Some(']' | 'P' | 'X' | '^' | '_') => string_end(chars, i + 1),
        Some(c) if ('\u{20}'..='\u{2f}').contains(c) => {
            // Intermediate bytes, then one final byte, such as ESC ( B.
            let mut j = i;
            while chars
                .get(j)
                .is_some_and(|c| ('\u{20}'..='\u{2f}').contains(c))
            {
                j += 1;
            }
            (j + 1).min(chars.len())
        }
        Some(_) => i + 1,
    }
}

/// A control sequence after its introducer at `i`: its parameters, its intermediate bytes, and
/// its final byte. Only `m` with plain parameters changes the style. Gives the index after it.
fn csi(chars: &[char], i: usize, style: &mut Style) -> usize {
    let mut j = i;
    let mut params = String::new();
    while let Some(&c) = chars.get(j) {
        if ('\u{30}'..='\u{3f}').contains(&c) {
            params.push(c);
            j += 1;
        } else {
            break;
        }
    }
    let mut middle = false;
    while chars
        .get(j)
        .is_some_and(|c| ('\u{20}'..='\u{2f}').contains(c))
    {
        middle = true;
        j += 1;
    }
    match chars.get(j) {
        Some('m')
            if !middle
                && params
                    .chars()
                    .all(|c| c.is_ascii_digit() || c == ';' || c == ':') =>
        {
            sgr(&params, style);
            j + 1
        }
        Some(c) if ('\u{40}'..='\u{7e}').contains(c) => j + 1,
        // A broken sequence ends at the first byte that does not belong to it.
        _ => j,
    }
}

/// The end of a string sequence (OSC, DCS, SOS, PM, APC) that starts at `i`: after BEL, after
/// ESC \, or after the string terminator U+009C.
fn string_end(chars: &[char], i: usize) -> usize {
    let mut j = i;
    while let Some(&c) = chars.get(j) {
        match c {
            '\u{7}' | '\u{9c}' => return j + 1,
            '\u{1b}' if chars.get(j + 1) == Some(&'\\') => return j + 2,
            _ => j += 1,
        }
    }
    j
}

/// Applies the parameters of one SGR sequence.
fn sgr(params: &str, style: &mut Style) {
    let groups: Vec<&str> = params.split(';').collect();
    let num = |s: &str| -> Option<u32> {
        if s.is_empty() {
            Some(0)
        } else {
            s.parse().ok()
        }
    };
    let mut k = 0;
    while k < groups.len() {
        let group = groups[k];
        k += 1;
        if group.contains(':') {
            // The form with colons: 38:5:N, 38:2:R:G:B, or 38:2::R:G:B.
            let parts: Vec<&str> = group.split(':').collect();
            let target = num(parts[0]);
            let color = match parts.get(1).and_then(|p| num(p)) {
                Some(5) => parts.get(2).and_then(|p| num(p)).and_then(index_color),
                Some(2) => {
                    let rgb: Vec<&str> = if parts.len() >= 6 {
                        parts[3..6].to_vec()
                    } else {
                        parts.get(2..5).map(<[&str]>::to_vec).unwrap_or_default()
                    };
                    rgb_of(&rgb)
                }
                _ => None,
            };
            set(style, target, color);
            continue;
        }
        let Some(code) = num(group) else {
            continue;
        };
        match code {
            0 => *style = Style::default(),
            1 => style.bold = true,
            22 => style.bold = false,
            30..=37 => style.fg = index_color(code - 30),
            90..=97 => style.fg = index_color(code - 90 + 8),
            39 => style.fg = None,
            40..=47 => style.bg = index_color(code - 40),
            100..=107 => style.bg = index_color(code - 100 + 8),
            49 => style.bg = None,
            38 | 48 => {
                let color = match groups.get(k).and_then(|g| num(g)) {
                    Some(5) => {
                        let color = groups.get(k + 1).and_then(|g| num(g)).and_then(index_color);
                        k += 2;
                        color
                    }
                    Some(2) => {
                        let color = groups.get(k + 1..k + 4).and_then(rgb_of);
                        k += 4;
                        color
                    }
                    _ => None,
                };
                set(style, Some(code), color);
            }
            _ => {}
        }
    }
}

fn set(style: &mut Style, target: Option<u32>, color: Option<Rgb>) {
    let Some(color) = color else {
        return;
    };
    match target {
        Some(38) => style.fg = Some(color),
        Some(48) => style.bg = Some(color),
        _ => {}
    }
}

fn rgb_of(parts: &[&str]) -> Option<Rgb> {
    let value = |s: &str| s.parse::<u8>().ok();
    match parts {
        [r, g, b] => Some(Rgb(value(r)?, value(g)?, value(b)?)),
        _ => None,
    }
}

/// The 16 colors of the terminal, with the values of xterm.
const SIXTEEN: [Rgb; 16] = [
    Rgb(0, 0, 0),
    Rgb(205, 0, 0),
    Rgb(0, 205, 0),
    Rgb(205, 205, 0),
    Rgb(0, 0, 238),
    Rgb(205, 0, 205),
    Rgb(0, 205, 205),
    Rgb(229, 229, 229),
    Rgb(127, 127, 127),
    Rgb(255, 0, 0),
    Rgb(0, 255, 0),
    Rgb(255, 255, 0),
    Rgb(92, 92, 255),
    Rgb(255, 0, 255),
    Rgb(0, 255, 255),
    Rgb(255, 255, 255),
];

/// The color of an index of the 256 colors: the 16 colors, the color cube, and the gray ramp.
pub fn index_color(index: u32) -> Option<Rgb> {
    const CUBE: [u8; 6] = [0, 95, 135, 175, 215, 255];
    match index {
        0..=15 => Some(SIXTEEN[index as usize]),
        16..=231 => {
            let i = (index - 16) as usize;
            Some(Rgb(CUBE[i / 36], CUBE[i / 6 % 6], CUBE[i % 6]))
        }
        232..=255 => {
            let v = (8 + 10 * (index - 232)) as u8;
            Some(Rgb(v, v, v))
        }
        _ => None,
    }
}
