//! The loader with a clock that the test sets: the shortest stay, the key that skips, the limit
//! of the tracker stage, a failed stage, a home that is not set up, and a picture that fits the
//! room of the window.

use ratatui::buffer::Buffer;
use vision::app::model::{
    App, Screen, SetupError, StageState, STAGE_ENGINE, STAGE_LEDGER, STAGE_TRACKER,
};
use vision::app::msg::{Effect, Key, Msg};
use vision::app::picture::{Cell, Picture, BLOCKS};
use vision::app::view::theme::{Rgb, T};

use super::support::*;

const SIZE: (u16, u16) = (110, 32);

#[test]
fn the_loader_stays_until_the_android_has_appeared() {
    let mut app = booting(SIZE);
    send(&mut app, 0.1, Msg::Setup(Ok(setup())));
    send(&mut app, 0.2, Msg::Local(Ok(local(Vec::new(), Vec::new()))));
    send(&mut app, 0.3, Msg::Queue(Ok(queue())));
    assert!(app.boot.stages.iter().all(|s| s.settled()));
    send(&mut app, 0.8, Msg::Tick);
    assert_eq!(app.screen, Screen::Boot, "the picture still appears");
    send(&mut app, 0.95, Msg::Tick);
    assert_eq!(app.screen, Screen::Home);
}

#[test]
fn the_loader_waits_for_the_stages_after_the_android() {
    let mut app = booting(SIZE);
    send(&mut app, 0.1, Msg::Setup(Ok(setup())));
    send(&mut app, 2.0, Msg::Tick);
    assert_eq!(app.screen, Screen::Boot);
    send(&mut app, 2.1, Msg::Local(Ok(local(Vec::new(), Vec::new()))));
    assert_eq!(app.screen, Screen::Boot, "the tracker stage still runs");
    send(&mut app, 2.2, Msg::Queue(Ok(queue())));
    assert_eq!(app.screen, Screen::Home);
}

#[test]
fn a_key_skips_the_loader() {
    let mut app = booting(SIZE);
    // A key before the home folder is read: the loader leaves as soon as it is read.
    send(&mut app, 0.05, Msg::Key(Key::Char('x')));
    assert_eq!(app.screen, Screen::Boot);
    send(&mut app, 0.1, Msg::Setup(Ok(setup())));
    assert_eq!(app.screen, Screen::Home);
    // The other stages go on, and Home fills when they end.
    assert_eq!(app.boot.stages[STAGE_LEDGER].state, StageState::Running);
    send(
        &mut app,
        0.3,
        Msg::Local(Ok(local(two_tickets(), Vec::new()))),
    );
    assert_eq!(app.held().len(), 2);
    assert!(app.engine.started);
}

#[test]
fn the_loader_stops_waiting_for_the_tracker_after_10_seconds() {
    let mut app = booting(SIZE);
    send(&mut app, 0.1, Msg::Setup(Ok(setup())));
    send(&mut app, 0.2, Msg::Local(Ok(local(Vec::new(), Vec::new()))));
    assert!(app.boot.stages[STAGE_ENGINE].settled());
    send(&mut app, 9.9, Msg::Tick);
    assert_eq!(app.screen, Screen::Boot);
    send(&mut app, 10.2, Msg::Tick);
    assert_eq!(
        app.screen,
        Screen::Home,
        "the tracker stage started at 0.1 s"
    );
    assert!(app.queue.loading());
    // The queue arrives later, and Home shows it.
    send(&mut app, 15.0, Msg::Queue(Ok(queue())));
    assert_eq!(app.queue_rows().len(), 2);
    assert!(screen(&app).contains("DEMO-15"));
}

#[test]
fn a_failed_stage_shows_its_reason_and_the_loader_goes_on() {
    let mut app = booting(SIZE);
    send(&mut app, 0.1, Msg::Setup(Ok(setup())));
    let fx = send(
        &mut app,
        0.2,
        Msg::Local(Err("could not open the ledger: database is locked".into())),
    );
    assert_eq!(
        app.boot.stages[STAGE_LEDGER].state,
        StageState::Failed("could not open the ledger: database is locked".into())
    );
    assert!(fx.contains(&Effect::StartWorker), "the engine still starts");
    send(
        &mut app,
        0.3,
        Msg::Queue(Err("error: bin/observe failed".into())),
    );
    assert_eq!(
        app.boot.stages[STAGE_TRACKER].state,
        StageState::Failed("bin/observe failed".into())
    );
    send(&mut app, 1.0, Msg::Tick);
    assert_eq!(app.screen, Screen::Home);
}

#[test]
fn a_home_that_is_not_set_up_stops_the_loader_and_q_leaves() {
    let mut app = booting(SIZE);
    let fx = send(
        &mut app,
        0.1,
        Msg::Setup(Err(SetupError {
            reason: "not set up".into(),
            what: "Vision is not set up at ~/.vision: config.toml is missing.".into(),
            fix: "Run vision init to make the home folder, then open Vision again.".into(),
        })),
    );
    assert!(fx.is_empty(), "no other stage starts: {fx:?}");
    send(&mut app, 5.0, Msg::Key(Key::Enter));
    send(&mut app, 30.0, Msg::Tick);
    assert_eq!(app.screen, Screen::Boot, "no key skips it");
    assert!(!app.engine.started);
    let text = screen(&app);
    assert!(text.contains("config.toml is missing"), "{text}");
    assert!(text.contains("Run vision init"), "{text}");
    let fx = send(&mut app, 31.0, Msg::Key(Key::Char('q')));
    assert_eq!(fx, vec![Effect::Quit]);
}

#[test]
fn the_loader_animates_and_home_waits_quietly() {
    let app = booting(SIZE);
    assert!(app.animating());
    let app = home(SIZE);
    assert!(
        !app.animating(),
        "nothing moves on Home after the first pass"
    );
}

const TOP: Rgb = Rgb(250, 40, 40);
const BOTTOM: Rgb = Rgb(40, 250, 40);

/// A portrait of 66 x 43 cells of blocks: a band of one color in the first 3 rows, a band of
/// another color in the last 3 rows, and quadrants of many colors between them.
fn portrait() -> Picture {
    let cell = |c: usize, r: usize| -> Cell {
        let (ch, fg, bg) = match r {
            0..=2 => ('▀', Some(TOP), Some(TOP)),
            40.. => (' ', None, Some(BOTTOM)),
            _ => (
                BLOCKS[(r * 7 + c * 3) % 16],
                Some(Rgb(30, 60 + r as u8, 120 + c as u8)),
                Some(Rgb(10, 20 + c as u8, 40 + r as u8)),
            ),
        };
        Cell {
            ch: Some(ch),
            fg,
            bg,
            bold: false,
        }
    };
    Picture {
        rows: (0..43)
            .map(|r| (0..66).map(|c| cell(c, r)).collect())
            .collect(),
    }
}

fn loader_with_portrait(size: (u16, u16)) -> App {
    let mut app = booting(size);
    send(&mut app, 0.05, Msg::Picture(Box::new(Some(portrait()))));
    send(&mut app, 0.3, Msg::Setup(Ok(setup())));
    send(&mut app, 0.6, Msg::Tick);
    app
}

/// The rows of the screen with a cell in a color.
fn rows_with(buf: &Buffer, color: Rgb) -> Vec<u16> {
    (0..buf.area.height)
        .filter(|&y| {
            (0..buf.area.width).any(|x| {
                let cell = &buf[(x, y)];
                cell.bg == color.color() || cell.fg == color.color()
            })
        })
        .collect()
}

/// The row of the screen with the name of the tool.
fn title_row(buf: &Buffer) -> u16 {
    text(buf)
        .lines()
        .position(|line| line.contains("V  I  S  I  O  N"))
        .expect("the title shows") as u16
}

#[test]
fn a_portrait_larger_than_the_room_shows_whole_and_shrunk() {
    // The window of 110 x 32 leaves 19 rows above the title for the art.
    let buf = render(&loader_with_portrait((110, 32)));
    let (top, bottom) = (rows_with(&buf, TOP), rows_with(&buf, BOTTOM));
    assert_eq!(top.len(), 1, "the first rows of the art: {top:?}");
    assert_eq!(bottom.len(), 1, "the last rows of the art: {bottom:?}");
    assert_eq!(
        bottom[0] - top[0],
        18,
        "the art takes the 19 rows of the room"
    );
    assert_eq!(
        title_row(&buf),
        bottom[0] + 2,
        "the title shows below the art"
    );
    // The shape stays: 29 columns for 19 rows.
    let columns = (0..110)
        .filter(|&x| buf[(x, top[0])].bg == TOP.color())
        .count();
    assert_eq!(columns, 29);
    let shown = text(&buf);
    assert!(shown.contains("home folder"), "{shown}");
    assert!(shown.contains("any key skips"), "{shown}");
}

#[test]
fn a_portrait_that_fits_the_room_shows_cell_for_cell() {
    // The window of 186 x 57 leaves 44 rows above the title. The block of the loader takes 56
    // rows, so it starts at row 1.
    let buf = render(&loader_with_portrait((186, 57)));
    let (x0, y0) = ((186 - 66) / 2, 1);
    for (r, row) in portrait().rows.iter().enumerate() {
        for (col, cell) in row.iter().enumerate() {
            let shown = &buf[((x0 + col) as u16, (y0 + r) as u16)];
            let at = format!("column {col}, row {r}");
            assert_eq!(shown.symbol(), cell.ch.unwrap().to_string(), "{at}");
            assert_eq!(shown.fg, cell.fg.unwrap_or(T.fg).color(), "{at}");
            assert_eq!(shown.bg, cell.bg.unwrap_or(T.bg).color(), "{at}");
        }
    }
    assert_eq!(rows_with(&buf, TOP), [1, 2, 3]);
    assert_eq!(rows_with(&buf, BOTTOM), [41, 42, 43]);
    assert_eq!(title_row(&buf), 45);
}

#[test]
fn the_update_fits_the_picture_once_for_each_size_of_the_window() {
    let mut app = loader_with_portrait((110, 32));
    let fit = app.picture_fit.clone().expect("a copy for the window");
    assert_eq!(fit.room, (106, 19));
    assert_eq!((fit.picture.width(), fit.picture.height()), (29, 19));
    send(
        &mut app,
        0.7,
        Msg::Resize {
            width: 186,
            height: 57,
        },
    );
    let fit = app.picture_fit.clone().expect("a copy for the new size");
    assert_eq!(fit.room, (182, 44));
    assert_eq!(fit.picture, portrait());
    // A window that changed size before the update heard of it: the view fits on its own.
    let mut app = loader_with_portrait((110, 32));
    app.size = (186, 57);
    let buf = render(&app);
    assert_eq!(rows_with(&buf, TOP), [1, 2, 3]);
    // After the loader, a change of size drops the copy.
    let mut app = loader_with_portrait((110, 32));
    send(&mut app, 0.7, Msg::Key(Key::Char('x')));
    assert_eq!(app.screen, Screen::Home);
    send(
        &mut app,
        0.8,
        Msg::Resize {
            width: 120,
            height: 40,
        },
    );
    assert_eq!(app.picture_fit, None);
}
