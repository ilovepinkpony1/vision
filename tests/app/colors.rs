//! The colors that a text snapshot cannot hold: the selection, the warning color of a ticket
//! that needs the person, the gem of the android at a known time, the 256 colors, and a screen
//! with no color at all. Also the text from outside: a control character never reaches a cell,
//! and a cut never splits a wide character.

use std::f64::consts::PI;

use ratatui::buffer::Buffer;
use ratatui::style::{Color, Modifier};
use vision::app::model::App;
use vision::app::msg::Msg;
use vision::app::view::android;
use vision::app::view::theme::{ColorMode, T};

use super::support::*;

const SIZE: (u16, u16) = (110, 32);

/// The column of a text in a row of the screen, by cells.
fn column(buf: &Buffer, y: u16, needle: &str) -> u16 {
    let line = text(buf).lines().nth(y as usize).unwrap().to_string();
    let at = line
        .find(needle)
        .unwrap_or_else(|| panic!("no {needle:?} in row {y}: {line}"));
    line[..at].chars().count() as u16
}

#[test]
fn the_selected_row_has_the_selection_background() {
    let buf = render(&home(SIZE));
    // DEMO-12 is selected: both of its rows, and not the rows of DEMO-7.
    assert_eq!(buf[(10, 5)].bg, T.sel.color());
    assert_eq!(buf[(40, 6)].bg, T.sel.color());
    assert_eq!(buf[(10, 8)].bg, T.bg.color());
    let key = column(&buf, 5, "DEMO-12");
    assert_eq!(buf[(key, 5)].fg, T.accent.color());
    assert!(buf[(key, 5)].modifier.contains(Modifier::BOLD));
}

#[test]
fn a_ticket_that_needs_the_person_has_the_warning_color() {
    let tickets = vec![
        needing("DEMO-12", "Export the report", "plan"),
        two_tickets().remove(1),
    ];
    let app = home_with(
        SIZE,
        local(tickets, vec![card_need("DEMO-12")]),
        Some(queue()),
    );
    let buf = render(&app);
    // The bar "needs you" is on row 3, so the first ticket starts on row 7.
    let x = column(&buf, 8, "needs you: 1 card");
    assert_eq!(buf[(x, 8)].fg, T.warn.color());
    let x = column(&buf, 3, "needs you");
    assert_eq!(buf[(x, 3)].bg, T.warn.color());
    // The ticket that waits has the color of waiting.
    let x = column(&buf, 11, "waits: the watched value");
    assert_eq!(buf[(x, 11)].fg, T.info.color());
}

/// The loader at `t` seconds, with the home folder read.
fn loader_at(t: f64, mode: ColorMode) -> App {
    let mut app = booting(SIZE);
    app.colors = mode;
    send(&mut app, 0.1, Msg::Setup(Ok(setup())));
    app.clock = clock(t);
    app
}

#[test]
fn the_gem_of_the_android_has_the_gem_color_at_a_known_time() {
    // At 3π/10 s the pulse is at its low point, so the gem shows its own color.
    let buf = render(&loader_at(3.0 * PI / 10.0, ColorMode::True));
    let (x0, y0) = ((110 - android::WIDTH + 1) / 2, 2);
    let cells = android::gem_cells();
    assert!(!cells.is_empty());
    for (col, row) in &cells {
        let cell = &buf[((x0 + col) as u16, (y0 + row) as u16)];
        assert_eq!(cell.symbol(), "▀");
        assert_eq!(cell.fg, T.px.gem.color(), "the gem at {col},{row}");
    }
    // At the high point of the pulse, the gem is lighter.
    let buf = render(&loader_at(PI / 10.0 + 2.0 * PI / 5.0, ColorMode::True));
    let (col, row) = cells[0];
    assert_ne!(
        buf[((x0 + col) as u16, (y0 + row) as u16)].fg,
        T.px.gem.color()
    );
}

#[test]
fn with_no_color_no_cell_has_a_color_and_the_android_stays_away() {
    let mut app = home(SIZE);
    app.colors = ColorMode::Plain;
    let buf = render(&app);
    for cell in &buf.content {
        assert_eq!((cell.fg, cell.bg), (Color::Reset, Color::Reset));
    }
    assert!(buf
        .content
        .iter()
        .any(|c| c.modifier.contains(Modifier::BOLD)));
    assert!(buf
        .content
        .iter()
        .any(|c| c.modifier.contains(Modifier::DIM)));

    let buf = render(&loader_at(2.0, ColorMode::Plain));
    for cell in &buf.content {
        assert_eq!((cell.fg, cell.bg), (Color::Reset, Color::Reset));
        assert_ne!(cell.symbol(), "▀", "the android needs color");
    }
    let screen = text(&buf);
    assert!(screen.contains("V  I  S  I  O  N"), "{screen}");
    assert!(screen.contains("home folder"), "{screen}");
}

#[test]
fn without_truecolor_each_color_is_one_of_the_256() {
    let mut app = home(SIZE);
    app.colors = ColorMode::Ansi256;
    let buf = render(&app);
    for cell in &buf.content {
        for color in [cell.fg, cell.bg] {
            assert!(
                matches!(color, Color::Indexed(_) | Color::Reset),
                "{color:?}"
            );
        }
    }
}

#[test]
fn text_from_outside_never_controls_the_terminal_and_its_cut_keeps_wide_characters() {
    let title = format!("Fix \x1b[31mthe\r\nexport\tof the {}", "日本語".repeat(30));
    let mut tickets = two_tickets();
    tickets[0].view.title = Some(title.clone());
    tickets[0].view.now = "builder works\x1b]0;evil\x07 (6 min)".into();
    let app = home_with(SIZE, local(tickets, Vec::new()), Some(queue()));
    let buf = render(&app);
    for cell in &buf.content {
        assert!(
            !cell.symbol().chars().any(char::is_control),
            "a control character reached a cell: {:?}",
            cell.symbol()
        );
    }
    let line = text(&buf).lines().nth(5).unwrap().to_string();
    // The title starts at column 15 and has room for 76 cells. 28 cells of plain text and 23
    // wide characters take 74 cells; a 24th would need the cells 75 and 76, where … must go,
    // so the cut ends one cell early and keeps each wide character whole.
    let wide: String = "日本語".repeat(30).chars().take(23).collect();
    let expected = format!("Fix  [31mthe  export of the {wide}…");
    assert_eq!(vision::app::text::width(&expected), 75);
    assert!(line.contains(&format!(" {expected} ")), "{line}");
    let second = text(&buf).lines().nth(6).unwrap().to_string();
    assert!(
        second.contains("builder works ]0;evil  (6 min)"),
        "{second}"
    );
}
