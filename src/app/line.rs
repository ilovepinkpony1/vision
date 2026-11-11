//! A one-line text input, as the line of `t` uses it: the text, a cursor, and the edit keys
//! (left, right, Home, End, Backspace, Delete) and a paste of plain text. A typed or pasted
//! control character never reaches the text: a line end or a tab of a paste becomes a space,
//! and each other control character goes away.

/// The longest text that the line takes, in characters.
pub const MAX: usize = 4000;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Line {
    chars: Vec<char>,
    /// The cursor, as a character index from 0 to the length.
    cursor: usize,
}

impl Line {
    pub fn text(&self) -> String {
        self.chars.iter().collect()
    }

    pub fn cursor(&self) -> usize {
        self.cursor
    }

    /// True when the line holds only spaces.
    pub fn is_blank(&self) -> bool {
        self.chars.iter().all(|c| c.is_whitespace())
    }

    /// Types one character at the cursor.
    pub fn insert(&mut self, c: char) {
        if c.is_control() || self.chars.len() >= MAX {
            return;
        }
        self.chars.insert(self.cursor, c);
        self.cursor += 1;
    }

    /// Puts a pasted text at the cursor, as one line.
    pub fn paste(&mut self, text: &str) {
        for c in text.chars() {
            match c {
                '\r' => {}
                '\n' | '\t' => self.insert(' '),
                c => self.insert(c),
            }
        }
    }

    pub fn left(&mut self) {
        self.cursor = self.cursor.saturating_sub(1);
    }

    pub fn right(&mut self) {
        self.cursor = (self.cursor + 1).min(self.chars.len());
    }

    pub fn home(&mut self) {
        self.cursor = 0;
    }

    pub fn end(&mut self) {
        self.cursor = self.chars.len();
    }

    /// Removes the character before the cursor.
    pub fn backspace(&mut self) {
        if self.cursor > 0 {
            self.cursor -= 1;
            self.chars.remove(self.cursor);
        }
    }

    /// Removes the character at the cursor.
    pub fn delete(&mut self) {
        if self.cursor < self.chars.len() {
            self.chars.remove(self.cursor);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_edit_key_changes_the_text_at_the_cursor() {
        let mut line = Line::default();
        line.paste("use the ld");
        line.home();
        line.delete();
        line.insert('U');
        assert_eq!((line.text().as_str(), line.cursor()), ("Use the ld", 1));
        line.end();
        line.left();
        line.left();
        line.insert('o');
        line.right();
        line.right();
        line.right();
        line.backspace();
        assert_eq!((line.text().as_str(), line.cursor()), ("Use the ol", 10));
        line.insert('d');
        line.insert('\x1b');
        line.paste(" a\r\nb\tc\x07");
        assert_eq!(line.text(), "Use the old a b c");
        assert!(!line.is_blank());
        assert!(Line::default().is_blank());
        line.home();
        line.left();
        line.backspace();
        assert_eq!(line.cursor(), 0);
    }
}
