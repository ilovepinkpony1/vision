//! The picture of the loader, `loader.ans`: the colors of each kind, the escape sequences that
//! go away and leave clean cells, the limits of its size, the files that count as no file, the
//! fit of a picture to a room, the loader that shows the picture in place of the android, and
//! no picture with no color.

use vision::app::model::App;
use vision::app::msg::{Effect, Msg};
use vision::app::picture::{
    index_color, parse, read, Cell, Picture, BLANK, BLOCKS, MAX_BYTES, MAX_COLS, MAX_ROWS,
};
use vision::app::update::start;
use vision::app::view::theme::{ColorMode, Rgb, T};

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
fn the_picture_keeps_at_most_240_columns_and_120_rows() {
    assert_eq!((MAX_COLS, MAX_ROWS), (240, 120));
    let wide = "x".repeat(100);
    let p = parse(&vec![wide.as_str(); 50].join("\n")).unwrap();
    assert_eq!(
        (p.width(), p.height()),
        (100, 50),
        "a picture of 100 x 50 stays whole"
    );
    let wide = "x".repeat(300);
    let p = parse(&vec![wide.as_str(); 150].join("\n")).unwrap();
    assert_eq!(p.height(), MAX_ROWS);
    assert_eq!(p.width(), MAX_COLS);
    // A wide character takes two columns, and one that does not fit goes away.
    let p = parse(&format!("{}界界", "y".repeat(237))).unwrap();
    assert_eq!(p.width(), 239);
    assert_eq!(p.rows[0][237].ch, Some('界'));
    assert_eq!(p.rows[0][238].ch, None);
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
    assert_eq!(MAX_BYTES, 1024 * 1024);
    assert_eq!(read(home), None, "a file over 1 MiB");
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

/// A picture of `w` x `h` cells, with the cell of each column and row.
fn picture_of(w: usize, h: usize, cell: impl Fn(usize, usize) -> Cell) -> Picture {
    Picture {
        rows: (0..h)
            .map(|r| (0..w).map(|c| cell(c, r)).collect())
            .collect(),
    }
}

fn block(ch: char, fg: Rgb, bg: Option<Rgb>) -> Cell {
    Cell {
        ch: Some(ch),
        fg: Some(fg),
        bg,
        bold: false,
    }
}

/// The colors that the four quadrants of a block show, in the order of the bits of
/// [`BLOCKS`], with `None` for the background of the app.
fn quadrants(cell: Cell) -> [Option<Rgb>; 4] {
    let mask = BLOCKS
        .iter()
        .position(|b| Some(*b) == cell.ch)
        .unwrap_or_else(|| panic!("not a block: {cell:?}"));
    std::array::from_fn(|q| {
        if mask & (1 << q) != 0 {
            Some(cell.fg.unwrap_or(T.fg))
        } else {
            cell.bg
        }
    })
}

fn size(picture: &Picture) -> (usize, usize) {
    (picture.width(), picture.height())
}

#[test]
fn fit_never_enlarges_a_picture() {
    let p = picture_of(30, 10, |c, r| {
        block(BLOCKS[(c + r) % 16], Rgb(c as u8, r as u8, 9), None)
    });
    assert_eq!(p.fit(30, 10), p, "a picture that fits stays as it is");
    assert_eq!(p.fit(200, 100), p, "a picture never grows");
    let text = parse("abc\ndef").unwrap();
    assert_eq!(text.fit(80, 40), text);
}

#[test]
fn fit_keeps_the_shape() {
    let p = picture_of(60, 30, |_, _| block('█', Rgb(1, 2, 3), None));
    assert_eq!(size(&p.fit(30, 100)), (30, 15));
    assert_eq!(size(&p.fit(100, 10)), (20, 10));
    assert_eq!(
        size(&p.fit(59, 30)),
        (59, 30),
        "the rounded side never passes the room"
    );
    // A portrait of 66 x 43 in the room of a window of 110 x 32.
    let p = picture_of(66, 43, |_, _| block('▀', Rgb(1, 2, 3), Some(Rgb(4, 5, 6))));
    assert_eq!(size(&p.fit(106, 19)), (29, 19));
    // A picture of text keeps its shape too.
    let p = picture_of(60, 30, |_, _| block('x', Rgb(1, 2, 3), None));
    assert_eq!(size(&p.fit(30, 100)), (30, 15));
    // No room gives no picture.
    assert_eq!(size(&p.fit(0, 10)), (0, 0));
}

#[test]
fn a_block_picture_shrinks_as_pixels_with_mean_colors() {
    // One color stays that color.
    let green = Rgb(20, 200, 60);
    let q = picture_of(4, 2, |_, _| block('█', green, None)).fit(2, 1);
    assert_eq!(size(&q), (2, 1));
    for cell in &q.rows[0] {
        assert_eq!(quadrants(*cell), [Some(green); 4]);
    }
    // A factor that is not whole leaves one color as one color.
    let q = picture_of(10, 18, |_, _| block('█', green, None)).fit(96, 16);
    assert_eq!(size(&q), (9, 16));
    assert!(q.rows.iter().flatten().all(|cell| *cell
        == Cell {
            bg: Some(green),
            ..BLANK
        }));
    // Fine stripes of red and blue mix to purple.
    let (red, blue) = (Rgb(200, 0, 0), Rgb(0, 0, 200));
    let q = picture_of(4, 2, |_, _| block('▌', red, Some(blue))).fit(2, 1);
    for cell in &q.rows[0] {
        assert_eq!(quadrants(*cell), [Some(Rgb(100, 0, 100)); 4]);
    }
    // A red top and a blue bottom give a half block with both colors.
    let p = picture_of(4, 4, |_, r| {
        block('█', if r < 2 { red } else { blue }, None)
    });
    assert_eq!(p.fit(2, 1).rows, vec![vec![block('▀', red, Some(blue))]]);
    // A larger picture of many blocks gives only blocks.
    let p = picture_of(40, 20, |c, r| {
        block(
            BLOCKS[(c * 3 + r * 5) % 16],
            Rgb(c as u8 * 6, 90, r as u8 * 12),
            Some(Rgb(9, r as u8, c as u8)),
        )
    });
    let q = p.fit(13, 13);
    assert_eq!(size(&q), (13, 7));
    assert!(q
        .rows
        .iter()
        .flatten()
        .all(|cell| cell.ch.is_some_and(|ch| BLOCKS.contains(&ch)) && !cell.bold));
}

#[test]
fn a_transparent_area_stays_transparent() {
    let red = Rgb(200, 0, 0);
    // The first column has no color; the other three are red.
    let p = picture_of(4, 2, |c, _| {
        if c == 0 {
            BLANK
        } else {
            block('█', red, None)
        }
    });
    let q = p.fit(2, 1);
    assert_eq!(
        q.rows[0][0],
        block('▐', red, None),
        "the left half shows the app"
    );
    assert_eq!(quadrants(q.rows[0][1]), [Some(red); 4]);
    // An area with no color at all gives a cell that shows nothing.
    let p = picture_of(4, 2, |c, _| {
        if c < 2 {
            BLANK
        } else {
            block('█', red, None)
        }
    });
    assert_eq!(p.fit(2, 1).rows[0][0], BLANK);
}

#[test]
fn a_picture_of_text_keeps_the_nearest_cells() {
    let p = parse("\x1b[1ma\x1b[22mbcdef\nghijkl\nmnopqr\nstuv\x1b[31mwx").unwrap();
    let q = p.fit(3, 2);
    // The first and the last row and column stay.
    assert_eq!(text(&q), ["adf", "svx"]);
    assert!(q.rows[0][0].bold, "a cell keeps its bold");
    assert!(!q.rows[0][1].bold);
    assert_eq!(q.rows[1][2].fg, Some(Rgb(205, 0, 0)));
    // One cell of text among blocks: the blocks keep their cells and mix no color.
    let (red, blue) = (Rgb(200, 0, 0), Rgb(0, 0, 200));
    let p = picture_of(4, 2, |c, r| {
        if (c, r) == (0, 0) {
            block('x', red, None)
        } else {
            block('▌', red, Some(blue))
        }
    });
    let q = p.fit(2, 1);
    assert_eq!(
        q.rows,
        vec![vec![block('x', red, None), block('▌', red, Some(blue))]]
    );
    // A wide character keeps its two columns, and a lone second column is a space.
    let q = parse("界界界界\n界界界界").unwrap().fit(3, 1);
    let chars: Vec<Option<char>> = q.rows[0].iter().map(|c| c.ch).collect();
    assert_eq!(chars, [Some('界'), None, Some(' ')]);
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
fn a_tall_picture_shrinks_to_the_rows_that_fit() {
    // 18 rows of a blue color; the window of 100 x 28 leaves 16 rows for the art.
    let tall = parse(&vec!["\x1b[44m          \x1b[0m"; 18].join("\n")).unwrap();
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
