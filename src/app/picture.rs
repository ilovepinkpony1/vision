//! The picture of the loader: `loader.ans` of the home folder, a text with color codes as a
//! tool such as `chafa` writes it for a terminal. The file is data. Only the codes of color and
//! style (SGR: reset, bold, the 16 colors, the 256 colors, and 24-bit colors, for the
//! foreground and the background) count; each other escape sequence and each control
//! character go away, and no byte of the file reaches the terminal as it is. The picture keeps
//! at most [`MAX_COLS`] columns and [`MAX_ROWS`] rows. A file that is empty, larger than
//! [`MAX_BYTES`], or not valid UTF-8 counts as no file.

use std::path::Path;

use super::text::char_width;
use super::view::theme::Rgb;

/// The name of the file in the home folder.
pub const FILE: &str = "loader.ans";
pub const MAX_COLS: usize = 64;
pub const MAX_ROWS: usize = 18;
pub const MAX_BYTES: u64 = 256 * 1024;

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
