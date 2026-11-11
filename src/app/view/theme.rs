//! The colors of the app: the palette `synth` and the base table of the design. The view draws
//! with these colors as 24-bit values. At the end of each frame, [`finish`] turns them into what
//! the terminal can show: 24-bit color when `COLORTERM` says `truecolor` or `24bit`, else the
//! nearest of the 256 colors, and no color at all when `NO_COLOR` is set (then only bold and
//! dim remain).

use std::collections::HashMap;

use ratatui::buffer::Buffer;
use ratatui::style::{Color, Modifier};

/// A 24-bit color.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Rgb(pub u8, pub u8, pub u8);

impl Rgb {
    /// The color of a hex value such as `0x101216`.
    pub const fn hex(value: u32) -> Rgb {
        Rgb((value >> 16) as u8, (value >> 8) as u8, value as u8)
    }

    /// The color between `self` (k = 0) and `other` (k = 1), as `mix` of the design does.
    pub fn mix(self, other: Rgb, k: f64) -> Rgb {
        let one = |a: u8, b: u8| -> u8 {
            let value = f64::from(a) + (f64::from(b) - f64::from(a)) * k;
            value.round().clamp(0.0, 255.0) as u8
        };
        Rgb(
            one(self.0, other.0),
            one(self.1, other.1),
            one(self.2, other.2),
        )
    }

    pub fn color(self) -> Color {
        Color::Rgb(self.0, self.1, self.2)
    }

    /// The color of a cell, when it holds a 24-bit color.
    pub fn of(color: Color) -> Option<Rgb> {
        match color {
            Color::Rgb(r, g, b) => Some(Rgb(r, g, b)),
            _ => None,
        }
    }
}

/// The colors of the pixels of the android, by the letter of the matrix.
#[derive(Clone, Copy, Debug)]
pub struct Pixels {
    pub head_dark: Rgb,
    pub head: Rgb,
    pub face: Rgb,
    pub face_dark: Rgb,
    pub eye: Rgb,
    pub eye_dark: Rgb,
    pub gem: Rgb,
    pub lip: Rgb,
    pub mouth: Rgb,
    pub neck: Rgb,
    pub suit_dark: Rgb,
    pub suit: Rgb,
    pub chest: Rgb,
}

/// The palette of the app.
#[derive(Clone, Copy, Debug)]
pub struct Palette {
    pub bg: Rgb,
    pub panel: Rgb,
    pub fg: Rgb,
    pub dim: Rgb,
    pub faint: Rgb,
    pub line: Rgb,
    pub ok: Rgb,
    pub warn: Rgb,
    pub bad: Rgb,
    pub info: Rgb,
    pub warn_bg: Rgb,
    pub accent: Rgb,
    pub primary: Rgb,
    pub sel: Rgb,
    pub px: Pixels,
}

/// The palette `synth` on the base table of the design.
pub const T: Palette = Palette {
    bg: Rgb::hex(0x101216),
    panel: Rgb::hex(0x181b22),
    fg: Rgb::hex(0xd7dae0),
    dim: Rgb::hex(0x8a92a2),
    faint: Rgb::hex(0x565e6e),
    line: Rgb::hex(0x333a47),
    ok: Rgb::hex(0x5fd38d),
    warn: Rgb::hex(0xf0a35e),
    bad: Rgb::hex(0xf2777a),
    info: Rgb::hex(0x5cc8e8),
    warn_bg: Rgb::hex(0x2e2416),
    accent: Rgb::hex(0xf5c542),
    primary: Rgb::hex(0xa892f7),
    sel: Rgb::hex(0x262043),
    px: Pixels {
        head_dark: Rgb::hex(0x2b2154),
        head: Rgb::hex(0x4a3a8e),
        face: Rgb::hex(0x8672e0),
        face_dark: Rgb::hex(0x6a57c0),
        eye: Rgb::hex(0x9af6ff),
        eye_dark: Rgb::hex(0x46bfd0),
        gem: Rgb::hex(0xffd34d),
        lip: Rgb::hex(0x4a3a8e),
        mouth: Rgb::hex(0x3a2d73),
        neck: Rgb::hex(0x3a2d6e),
        suit_dark: Rgb::hex(0x2b2154),
        suit: Rgb::hex(0x43357f),
        chest: Rgb::hex(0xc9952a),
    },
};

pub const WHITE: Rgb = Rgb::hex(0xffffff);

/// The color of each agent of Vision, from the design. Another name gets the primary color.
pub fn agent_color(agent: &str) -> Rgb {
    match agent {
        "scout" => Rgb::hex(0x5cc8e8),
        "builder" => Rgb::hex(0xf5c542),
        "reviewer" => Rgb::hex(0xe58fd0),
        "tester" => Rgb::hex(0x5fd38d),
        "shepherd" => Rgb::hex(0x7aa2f7),
        "judge" => Rgb::hex(0xf4e6c0),
        "curator" => Rgb::hex(0xb79cf5),
        _ => T.primary,
    }
}

/// What the terminal can show.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ColorMode {
    /// 24-bit color.
    True,
    /// The 256 colors.
    Ansi256,
    /// No color: only bold and dim.
    Plain,
}

impl ColorMode {
    /// The mode of the values of `COLORTERM` and `NO_COLOR`.
    pub fn of(colorterm: Option<&str>, no_color: Option<&str>) -> ColorMode {
        if no_color.is_some_and(|v| !v.is_empty()) {
            return ColorMode::Plain;
        }
        match colorterm.map(str::trim) {
            Some(v) if v.eq_ignore_ascii_case("truecolor") || v.eq_ignore_ascii_case("24bit") => {
                ColorMode::True
            }
            _ => ColorMode::Ansi256,
        }
    }

    /// The mode of the environment of this process.
    pub fn from_env() -> ColorMode {
        let colorterm = std::env::var("COLORTERM").ok();
        let no_color = std::env::var("NO_COLOR").ok();
        ColorMode::of(colorterm.as_deref(), no_color.as_deref())
    }
}

/// Turns the 24-bit colors of a drawn frame into what the terminal of `mode` can show.
pub fn finish(buf: &mut Buffer, mode: ColorMode) {
    match mode {
        ColorMode::True => {}
        ColorMode::Ansi256 => {
            let mut cache: HashMap<Rgb, u8> = HashMap::new();
            let mut map = |color: Color| match Rgb::of(color) {
                Some(rgb) => Color::Indexed(*cache.entry(rgb).or_insert_with(|| nearest_256(rgb))),
                None => color,
            };
            for cell in buf.content.iter_mut() {
                cell.fg = map(cell.fg);
                cell.bg = map(cell.bg);
            }
        }
        ColorMode::Plain => {
            for cell in buf.content.iter_mut() {
                let soft =
                    Rgb::of(cell.fg).is_some_and(|c| c == T.dim || c == T.faint || c == T.line);
                if soft {
                    cell.modifier.insert(Modifier::DIM);
                }
                cell.fg = Color::Reset;
                cell.bg = Color::Reset;
            }
        }
    }
}

/// The value of each step of the color cube of the 256 colors.
const CUBE: [u8; 6] = [0, 95, 135, 175, 215, 255];

/// The color of an index of the 256 colors from 16 to 255: the color cube and the gray ramp.
/// The first 16 colors are left out, because each terminal gives them its own values.
fn color_of_index(index: u8) -> Rgb {
    if index >= 232 {
        let v = 8 + 10 * (index - 232);
        return Rgb(v, v, v);
    }
    let i = index - 16;
    Rgb(
        CUBE[usize::from(i / 36)],
        CUBE[usize::from(i / 6 % 6)],
        CUBE[usize::from(i % 6)],
    )
}

/// The nearest of the 256 colors, from the color cube and the gray ramp.
pub fn nearest_256(color: Rgb) -> u8 {
    let distance = |other: Rgb| {
        let d = |a: u8, b: u8| (i32::from(a) - i32::from(b)).pow(2);
        d(color.0, other.0) + d(color.1, other.1) + d(color.2, other.2)
    };
    (16..=255u8)
        .min_by_key(|index| distance(color_of_index(*index)))
        .unwrap_or(16)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_mode_follows_colorterm_and_no_color() {
        assert_eq!(ColorMode::of(Some("truecolor"), None), ColorMode::True);
        assert_eq!(ColorMode::of(Some("24bit"), Some("")), ColorMode::True);
        assert_eq!(ColorMode::of(None, None), ColorMode::Ansi256);
        assert_eq!(
            ColorMode::of(Some("truecolor"), Some("1")),
            ColorMode::Plain
        );
    }

    #[test]
    fn the_nearest_of_the_256_colors() {
        assert_eq!(nearest_256(Rgb(0, 0, 0)), 16);
        assert_eq!(nearest_256(Rgb(255, 255, 255)), 231);
        assert_eq!(nearest_256(Rgb(128, 128, 128)), 244);
        assert_eq!(nearest_256(Rgb(255, 0, 0)), 196);
    }

    #[test]
    fn mix_rounds_as_the_design_does() {
        assert_eq!(
            Rgb(0, 0, 0).mix(Rgb(255, 255, 255), 0.5),
            Rgb(128, 128, 128)
        );
        assert_eq!(T.px.gem.mix(WHITE, 0.0), T.px.gem);
    }
}
