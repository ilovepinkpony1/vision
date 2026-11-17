//! The view: a function from the model to a frame. It does no input or output and reads no
//! clock. Each screen is one module that draws on a [`canvas::Canvas`], as the functions of the
//! design draw on its grid: the loader, Home, and the live view of a ticket with its pagers,
//! then a card over the screen, then the notice at the bottom.
//!
//! Each part that the mouse can hit is recorded on the canvas by the code that draws it, and
//! the draw gives the parts of the frame back. A card takes each click: the parts of the screen
//! under it go.
//!
//! Models and Stats are screens of their own. The card of the things that wait has a frame
//! ([`need_card`]) and one module for each kind of item: the question, the writes, and the
//! others. The loader shows the picture of `loader.ans` in place of the android when the home
//! folder has one.

pub mod android;
mod ask_card;
pub mod boot;
pub mod canvas;
mod cards;
mod chain_rows;
pub mod face;
pub mod header;
pub mod home;
pub mod live;
mod live_cards;
pub mod models;
pub mod need_card;
mod other_cards;
pub mod pager;
pub mod picture;
pub mod stats;
pub mod theme;
mod write_card;

use ratatui::buffer::Buffer;
use ratatui::Frame;

use super::hits::HitMap;
use super::model::{App, Screen, MIN_HEIGHT, MIN_WIDTH};
use super::text::wrap;
use canvas::{ink, Canvas};
use theme::{ColorMode, T};

/// Draws the model into a frame, and gives the parts of the frame that the mouse can hit.
pub fn draw(app: &App, frame: &mut Frame) -> HitMap {
    render(app, frame.buffer_mut())
}

/// Draws the model into a buffer of any size, and gives the parts of the frame that the mouse
/// can hit.
pub fn render(app: &App, buf: &mut Buffer) -> HitMap {
    let hits = {
        let mut c = Canvas::new(buf, app.colors == ColorMode::Plain);
        c.fill();
        if c.w < i32::from(MIN_WIDTH) || c.h < i32::from(MIN_HEIGHT) {
            too_small(&mut c);
        } else {
            match &app.screen {
                Screen::Boot => boot::draw(app, &mut c),
                Screen::Home => home::draw(app, &mut c),
                Screen::Live { key } => live::draw(app, &mut c, key),
                Screen::Models => models::draw(app, &mut c),
                Screen::Stats => stats::draw(app, &mut c),
            }
            if let Some(card) = &app.card {
                c.clear_hits();
                cards::draw(app, card, &mut c);
            }
            header::notice(app, &mut c);
        }
        c.into_hits()
    };
    theme::finish(buf, app.colors);
    hits
}

/// The one message of a window that is too small.
fn too_small(c: &mut Canvas) {
    let lines = [
        format!("Vision needs a window of at least {MIN_WIDTH} columns and {MIN_HEIGHT} rows."),
        format!("This window has {} columns and {} rows.", c.w, c.h),
        "Make the window larger, or make the font smaller.".to_string(),
    ];
    let width = (c.w - 2).max(1) as usize;
    let mut rows: Vec<(String, bool)> = Vec::new();
    for (i, line) in lines.iter().enumerate() {
        if i > 0 {
            rows.push((String::new(), false));
        }
        for part in wrap(line, width) {
            rows.push((part, i == 0));
        }
    }
    let top = ((c.h - rows.len() as i32) / 2).max(0);
    for (i, (text, first)) in rows.iter().enumerate() {
        let color = if *first {
            ink(T.warn).bold()
        } else {
            ink(T.fg)
        };
        c.center(top + i as i32, text, color);
    }
}
