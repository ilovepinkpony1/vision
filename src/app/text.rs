//! Text from outside on the screen. A ticket title, a status of the tracker, a reason, and the
//! error text of a child are data, and they must never control the terminal: each control
//! character (ESC, CR, LF, TAB, and each other C0 and C1 character) becomes a space before the
//! text reaches a cell. A cut goes by the display width, never by bytes or chars, and it never
//! splits a wide character.

use ratatui::buffer::CellWidth;

/// The text with each control character as a space.
pub fn clean(text: &str) -> String {
    text.chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect()
}

/// The number of cells that one character takes: 0, 1, or 2. A control character counts as
/// one cell, because it becomes a space.
pub fn char_width(c: char) -> usize {
    if c.is_control() {
        return 1;
    }
    let mut bytes = [0u8; 4];
    usize::from(c.encode_utf8(&mut bytes).cell_width())
}

/// The number of cells that a text takes on the screen.
pub fn width(text: &str) -> usize {
    text.chars().map(char_width).sum()
}

/// The clean text cut to `max` cells. A text that is too wide ends with `…`, and the cut never
/// splits a wide character: such a character goes away whole.
pub fn cut(text: &str, max: usize) -> String {
    let text = clean(text);
    if width(&text) <= max {
        return text;
    }
    if max == 0 {
        return String::new();
    }
    let mut out = String::new();
    let mut used = 0;
    for c in text.chars() {
        let w = char_width(c);
        if used + w > max - 1 {
            break;
        }
        out.push(c);
        used += w;
    }
    out.push('…');
    out
}

/// The clean text cut to `max` cells with no mark at the end.
pub fn clip(text: &str, max: usize) -> String {
    let mut out = String::new();
    let mut used = 0;
    for c in clean(text).chars() {
        let w = char_width(c);
        if used + w > max {
            break;
        }
        out.push(c);
        used += w;
    }
    out
}

/// The clean text in lines of at most `max` cells, broken at spaces. A word that is wider than
/// a line is cut into parts.
pub fn wrap(text: &str, max: usize) -> Vec<String> {
    let max = max.max(1);
    let mut lines = Vec::new();
    let mut line = String::new();
    let mut used = 0;
    for word in clean(text).split(' ').filter(|w| !w.is_empty()) {
        let mut word = word.to_string();
        loop {
            let w = width(&word);
            let gap = usize::from(used > 0);
            if used + gap + w <= max {
                if gap == 1 {
                    line.push(' ');
                }
                line.push_str(&word);
                used += gap + w;
                break;
            }
            if used > 0 {
                lines.push(std::mem::take(&mut line));
                used = 0;
                continue;
            }
            // The word alone is wider than a line: put a part of it on this line.
            let part = clip(&word, max);
            if part.is_empty() {
                // A wide character on a line of one cell: drop it, so the loop ends.
                word = word.chars().skip(1).collect();
                if word.is_empty() {
                    break;
                }
                continue;
            }
            word = word[part.len()..].to_string();
            lines.push(part);
            if word.is_empty() {
                break;
            }
        }
    }
    if !line.is_empty() {
        lines.push(line);
    }
    lines
}

/// A count with its noun, such as `1 ticket` or `3 tickets`.
pub fn count(n: usize, noun: &str) -> String {
    if n == 1 {
        format!("1 {noun}")
    } else {
        format!("{n} {noun}s")
    }
}

/// The text of a failed command for people: each line without the word `error:` that a
/// command puts in front, and with no empty line at the ends.
pub fn tidy(text: &str) -> String {
    text.trim()
        .lines()
        .map(|line| {
            let line = line.trim_end();
            line.strip_prefix("error: ").unwrap_or(line)
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// The first line of an error text without the word `error:` that a command puts in front.
pub fn error_line(text: &str) -> String {
    let first = text.lines().map(str::trim).find(|l| !l.is_empty());
    let first = first.unwrap_or("").trim();
    first.strip_prefix("error: ").unwrap_or(first).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn control_characters_become_spaces() {
        assert_eq!(clean("a\x1b[31mb\r\nc\td\u{9b}e"), "a [31mb  c d e");
    }

    #[test]
    fn a_cut_goes_by_display_width_and_keeps_wide_characters_whole() {
        assert_eq!(cut("abcdef", 6), "abcdef");
        assert_eq!(cut("abcdefg", 6), "abcde…");
        // Each of these characters takes two cells.
        assert_eq!(width("日本語"), 6);
        assert_eq!(cut("日本語です", 6), "日本…");
        assert_eq!(cut("a日本", 4), "a日…");
        assert_eq!(cut("ab日本", 4), "ab…");
        assert_eq!(clip("ab日本", 3), "ab");
    }

    #[test]
    fn wrap_breaks_at_spaces_and_cuts_long_words() {
        assert_eq!(wrap("one two three", 7), ["one two", "three"]);
        assert_eq!(wrap("abcdefghij", 4), ["abcd", "efgh", "ij"]);
        assert_eq!(wrap("a\nb", 10), ["a b"]);
    }
}
