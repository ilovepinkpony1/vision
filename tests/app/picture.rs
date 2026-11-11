//! The picture of the loader, `loader.ans`: the colors of each kind, the escape sequences that
//! go away and leave clean cells, the limits of its size, the files that count as no file, the
//! loader that shows the picture in place of the android, and no picture with no color.

use vision::app::model::App;
use vision::app::msg::{Effect, Msg};
use vision::app::picture::{
    index_color, parse, read, Cell, Picture, MAX_BYTES, MAX_COLS, MAX_ROWS,
};
use vision::app::update::start;
use vision::app::view::theme::{ColorMode, Rgb};

use super::support::*;

/// The characters of each row.
fn text(picture: &Picture) -> Vec<String> {
    picture
        .rows
        .iter()
        .map(|row| row.iter().filter_map(|c| c.ch).collect())
        .collect()
}

fn cell(picture: &Picture, row: usize, col: usize) -> Cell {
    picture.rows[row][col]
}

#[test]
fn the_colors_of_each_kind() {
    let p = parse(
        "\x1b[31mA\x1b[1;92mB\x1b[0m\x1b[44mC\x1b[103mD\x1b[0m\x1b[38;5;196mE\x1b[48;5;21mF\x1b[0m\x1b[38;2;10;20;30mG\x1b[48;2;200;100;50mH\x1b[0m\x1b[38:2::1:2:3mI\x1b[38:5:244mJ\x1b[39;49mK",
    )
    .unwrap();
    assert_eq!(text(&p), ["ABCDEFGHIJK"]);
    let a = cell(&p, 0, 0);
    assert_eq!((a.fg, a.bg, a.bold), (Some(Rgb(205, 0, 0)), None, false));
    let b = cell(&p, 0, 1);
    assert_eq!((b.fg, b.bold), (Some(Rgb(0, 255, 0)), true));
    let c = cell(&p, 0, 2);
    assert_eq!((c.fg, c.bg, c.bold), (None, Some(Rgb(0, 0, 238)), false));
    assert_eq!(cell(&p, 0, 3).bg, Some(Rgb(255, 255, 0)));
    assert_eq!(cell(&p, 0, 4).fg, Some(Rgb(255, 0, 0)));
    assert_eq!(cell(&p, 0, 5).bg, Some(Rgb(0, 0, 255)));
    let g = cell(&p, 0, 6);
    assert_eq!(g.fg, Some(Rgb(10, 20, 30)));
    let h = cell(&p, 0, 7);
    assert_eq!(
        (h.fg, h.bg),
        (Some(Rgb(10, 20, 30)), Some(Rgb(200, 100, 50)))
    );
    assert_eq!(cell(&p, 0, 8).fg, Some(Rgb(1, 2, 3)));
    assert_eq!(cell(&p, 0, 9).fg, Some(Rgb(128, 128, 128)));
    let k = cell(&p, 0, 10);
    assert_eq!((k.fg, k.bg), (None, None));
    // A reset with no parameter, and bold off.
    let p = parse("\x1b[1;33mA\x1b[22mB\x1b[mC").unwrap();
    assert!(cell(&p, 0, 0).bold);
    assert!(!cell(&p, 0, 1).bold);
    assert_eq!(cell(&p, 0, 1).fg, Some(Rgb(205, 205, 0)));
    assert_eq!(cell(&p, 0, 2).fg, None);
}

#[test]
fn the_table_of_the_256_colors() {
    assert_eq!(index_color(0), Some(Rgb(0, 0, 0)));
    assert_eq!(index_color(15), Some(Rgb(255, 255, 255)));
    assert_eq!(index_color(16), Some(Rgb(0, 0, 0)));
    assert_eq!(index_color(196), Some(Rgb(255, 0, 0)));
    assert_eq!(index_color(231), Some(Rgb(255, 255, 255)));
    assert_eq!(index_color(232), Some(Rgb(8, 8, 8)));
    assert_eq!(index_color(255), Some(Rgb(238, 238, 238)));
    assert_eq!(index_color(256), None);
}

#[test]
fn other_escape_sequences_and_control_characters_give_clean_cells() {
    let raw = concat!(
        "\x1b]0;a title\x07",     // a title change, ended by BEL
        "\x1b]2;another\x1b\\",   // a title change, ended by ESC backslash
        "\x1b[2J\x1b[H",          // a clear screen and a cursor home
        "\x1b[?25l",              // hide the cursor
        "\x1b[10;20H",            // a cursor move
        "\x1b(B",                 // a character set
        "\x1bPq#0;2;0;0;0\x1b\\", // a sixel string
        "\u{9b}31m",              // the 8-bit form of CSI
        "A\tB\rC\x07D\x08E",
        "\x1b[5A",             // a cursor move up
        "\x1b[38;2;300;0;0mF", // a color out of range does not apply
        "\x1b[31;",            // a sequence that the file cut
    );
    let p = parse(raw).unwrap();
    assert_eq!(text(&p), ["ABCDEF"]);
    assert_eq!(cell(&p, 0, 0).fg, Some(Rgb(205, 0, 0)));
    assert_eq!(cell(&p, 0, 5).fg, Some(Rgb(205, 0, 0)));
    // No escape and no control character is left in a cell.
    assert!(p
        .rows
        .iter()
        .flatten()
        .filter_map(|c| c.ch)
        .all(|ch| !ch.is_control()));
}

#[test]
fn the_picture_keeps_at_most_64_columns_and_18_rows() {
    let wide = "x".repeat(100);
    let rows = vec![wide.as_str(); 30].join("\n");
    let p = parse(&rows).unwrap();
    assert_eq!(p.height(), MAX_ROWS);
    assert_eq!(p.width(), MAX_COLS);
    // A wide character takes two columns, and one that does not fit goes away.
    let p = parse(&format!("{}界界", "y".repeat(61))).unwrap();
    assert_eq!(p.width(), 63);
    assert_eq!(p.rows[0][61].ch, Some('界'));
    assert_eq!(p.rows[0][62].ch, None);
    // Empty rows at the end go away; rows in the middle stay.
    let p = parse("a\n\nb\n\n\n").unwrap();
    assert_eq!(text(&p), ["a", "", "b"]);
}

#[test]
fn a_text_with_no_visible_cell_is_no_picture() {
    assert_eq!(parse(""), None);
    assert_eq!(parse("   \n \x1b[31m  \x1b[0m\n"), None);
    assert_eq!(parse("\x1b[2J\x1b[H"), None);
    // A colored background counts as visible.
    assert!(parse("\x1b[41m  ").is_some());
}

#[test]
fn the_files_that_count_as_no_file() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path();
    let file = home.join("loader.ans");
    assert_eq!(read(home), None, "no file");
    std::fs::write(&file, "").unwrap();
    assert_eq!(read(home), None, "an empty file");
    std::fs::write(&file, [b'A', 0xff, 0xfe, b'B']).unwrap();
    assert_eq!(read(home), None, "a file that is not UTF-8");
    std::fs::write(&file, "x".repeat(MAX_BYTES as usize + 1)).unwrap();
    assert_eq!(read(home), None, "a file over 256 KB");
    std::fs::write(
        &file,
        format!("\x1b[32m{}", "x".repeat(MAX_BYTES as usize - 10)),
    )
    .unwrap();
    assert!(read(home).is_some(), "a file just under the limit");
    std::fs::remove_file(&file).unwrap();
    std::fs::create_dir(&file).unwrap();
    assert_eq!(read(home), None, "a folder with the name");
}

/// A small picture: a green square with a red mark.
fn sample() -> Picture {
    let mut text = String::new();
    for row in 0..6 {
        text.push_str("\x1b[42m");
        for col in 0..12 {
            if row == 2 && (4..8).contains(&col) {
                text.push_str("\x1b[1;31m#\x1b[22;39m");
            } else {
                text.push(' ');
            }
        }
        text.push_str("\x1b[0m\n");
    }
    parse(&text).unwrap()
}

fn loader_with(picture: Option<Picture>) -> App {
    let mut app = booting((110, 32));
    send(&mut app, 0.05, Msg::Picture(Box::new(picture)));
    send(&mut app, 0.3, Msg::Setup(Ok(setup())));
    send(&mut app, 0.6, Msg::Tick);
    app
}

#[test]
fn the_loader_shows_the_picture_in_place_of_the_android() {
    let app = loader_with(Some(sample()));
    assert_snapshot("loader_picture", &screen(&app));
    let shown = screen(&app);
    assert!(shown.contains("####"), "{shown}");
    // No usable file: the android, as before.
    assert_snapshot("loader_middle", &screen(&loader_with(None)));
}

#[test]
fn a_tall_picture_is_cut_to_the_rows_that_fit() {
    let tall = parse(&vec!["\x1b[44m          \x1b[0m"; MAX_ROWS].join("\n")).unwrap();
    let mut app = booting((100, 28));
    send(&mut app, 0.05, Msg::Picture(Box::new(Some(tall))));
    send(&mut app, 0.3, Msg::Setup(Ok(setup())));
    send(&mut app, 0.6, Msg::Tick);
    assert_snapshot("loader_picture_100x28", &screen(&app));
}

#[test]
fn with_no_color_the_loader_reads_no_picture() {
    let mut app = App::new(clock(0.0), (110, 32), ColorMode::Plain);
    let fx = start(&mut app);
    assert_eq!(fx, vec![Effect::LoadSetup]);
    let mut app = App::new(clock(0.0), (110, 32), ColorMode::True);
    let fx = start(&mut app);
    assert_eq!(fx, vec![Effect::LoadSetup, Effect::LoadPicture]);
}
