//! The colors of the live view that a text snapshot cannot hold: the agent that works has its
//! own color in NOW, in its head, and in TEAM; a ticket that needs the person gets a box in the
//! warning color; each word of the activity has its color; and with no color the head stays
//! away.

use ratatui::buffer::Buffer;
use ratatui::style::{Color, Modifier};
use vision::app::model::App;
use vision::app::view::face;
use vision::app::view::theme::{agent_color, ColorMode, T};

use super::live_support::*;
use super::support::*;

const SIZE: (u16, u16) = (110, 32);
/// The first column of the middle, and the column of the head in NOW.
const MID_X: u16 = 29;
const HEAD_X: u16 = MID_X + 1;

/// The row and the column of the first cell of a text on the screen.
fn find(buf: &Buffer, needle: &str) -> (u16, u16) {
    for (y, line) in text(buf).lines().enumerate() {
        if let Some(at) = line.find(needle) {
            return (line[..at].chars().count() as u16, y as u16);
        }
    }
    panic!("no {needle:?} on the screen:\n{}", text(buf));
}

fn fg(buf: &Buffer, (x, y): (u16, u16)) -> Color {
    buf[(x, y)].fg
}

fn bold(buf: &Buffer, (x, y): (u16, u16)) -> bool {
    buf[(x, y)].modifier.contains(Modifier::BOLD)
}

#[test]
fn the_agent_that_works_has_its_color_in_now_and_in_team() {
    let buf = render(&working_live(SIZE));
    let builder = agent_color("builder").color();
    let title = find(&buf, "builder works");
    assert_eq!(fg(&buf, title), builder);
    assert!(bold(&buf, title));
    // TEAM: a filled dot and the name in the color of the agent; an agent that did a turn is
    // dim with a check mark; the others are faint.
    let dot = find(&buf, "● builder");
    assert_eq!(fg(&buf, dot), builder);
    assert_eq!(fg(&buf, (dot.0 + 2, dot.1)), builder);
    assert!(bold(&buf, (dot.0 + 2, dot.1)));
    let scout = find(&buf, "✓ scout");
    assert_eq!(fg(&buf, scout), T.dim.color());
    let reviewer = find(&buf, "· reviewer");
    assert_eq!(fg(&buf, reviewer), T.faint.color());
    // The path: the current step has the spinner in the color of the work.
    let (x, y) = find(&buf, " build ");
    assert_ne!(fg(&buf, (x - 1, y)), T.faint.color());
}

#[test]
fn the_head_has_the_color_of_the_agent_while_it_works_and_rests_dim() {
    let buf = render(&working_live(SIZE));
    // The third pixel column of the first row is the edge of the head: `K` over `K`.
    let cell = &buf[(HEAD_X + 2, 3)];
    assert_eq!(cell.symbol(), "▀");
    assert_eq!(cell.fg, agent_color("builder").mix(T.bg, 0.62).color());

    let held = resting("build");
    let app = live(SIZE, vec![held], Vec::new(), events());
    let buf = render(&app);
    let cell = &buf[(HEAD_X + 2, 3)];
    assert_eq!(cell.symbol(), "▀");
    assert_eq!(cell.fg, face::REST.mix(T.bg, 0.62).color());
}

#[test]
fn the_head_moves_while_the_agent_works() {
    let mut app = working_live(SIZE);
    let at = |app: &mut App, t: f64| {
        app.clock = clock(t);
        render(app)
    };
    // The eyes are in the second row of cells: the light point moves from the outer pixel to
    // the inner one.
    let a = at(&mut app, 10.1);
    let b = at(&mut app, 11.4);
    let eye = |buf: &Buffer| (buf[(HEAD_X + 2, 4)].bg, buf[(HEAD_X + 3, 4)].bg);
    assert_ne!(eye(&a), eye(&b));
    assert!(app.animating(), "a working agent keeps the screen moving");
    // A ticket that rests does not move, so the screen draws only when something changed.
    let mut app = live(SIZE, vec![resting("build")], Vec::new(), events());
    let a = at(&mut app, 10.1);
    let b = at(&mut app, 11.4);
    assert_eq!(eye(&a), eye(&b));
}

#[test]
fn a_ticket_that_needs_the_person_gets_a_box_in_the_warning_color() {
    let held = needing(
        "DEMO-12",
        "Export the report of the month as a CSV file",
        "plan",
    );
    let app = live(SIZE, vec![held], vec![card_need("DEMO-12")], events());
    let buf = render(&app);
    let badge = find(&buf, " needs you ");
    assert_eq!(buf[badge].bg, T.warn.color());
    let corner = find(&buf, "╭");
    assert_eq!(fg(&buf, corner), T.warn.color());
    let key = find(&buf, "n answer");
    assert_eq!(fg(&buf, key), T.warn.color());
    // The path marks the step that waits for the person in the warning color.
    let (x, y) = find(&buf, "◉ plan");
    assert_eq!(fg(&buf, (x, y)), T.warn.color());
}

#[test]
fn each_word_of_the_activity_has_its_color() {
    let buf = render(&working_live(SIZE));
    let word = |needle: &str| {
        let (x, y) = find(&buf, needle);
        (fg(&buf, (x, y)), bold(&buf, (x, y)))
    };
    assert_eq!(word("read  src/report"), (T.info.color(), true));
    assert_eq!(word("edit  src/report"), (T.accent.color(), true));
    assert_eq!(word("run   python3"), (T.ok.color(), true));
    assert_eq!(word("you   approved"), (T.primary.color(), true));
    // The words of the agent are bright; the text of a tool call is dim; the time is faint.
    let (x, y) = find(&buf, "The export writes");
    assert_eq!(fg(&buf, (x, y)), T.fg.color());
    let (x, y) = find(&buf, "src/report/export.py");
    assert_eq!(fg(&buf, (x, y)), T.dim.color());
    let (x, y) = find(&buf, "14:24:05");
    assert_eq!(fg(&buf, (x, y)), T.faint.color());
}

#[test]
fn with_no_color_the_live_view_has_no_color_and_no_head() {
    let mut app = working_live(SIZE);
    app.colors = ColorMode::Plain;
    let buf = render(&app);
    for y in 0..SIZE.1 {
        for x in 0..SIZE.0 {
            let cell = &buf[(x, y)];
            assert!(
                matches!(cell.fg, Color::Reset) && matches!(cell.bg, Color::Reset),
                "a color at {x},{y}: {cell:?}"
            );
        }
    }
    assert_eq!(buf[(HEAD_X + 2, 3)].symbol(), " ");
    assert!(text(&buf).contains("builder works"));
}
