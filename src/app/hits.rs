//! The parts of a frame that the mouse can hit. While a screen draws, it records each part that
//! a click can hit, with what the click does, and each part that the wheel scrolls. The runtime
//! keeps the parts of the last frame in the model, so that the update maps a click at a column
//! and a row to what the person saw there.
//!
//! A key hint, such as `n needs` or `enter open`, is a part that acts as its key. A hint of more
//! keys, such as `↑↓ move` or `g G start end`, has a part for each key, and its label is no
//! part. [`hint_parts`] gives the parts of a hint, for the footer and for the keys of a card.
//!
//! A part that sends something or records a decision acts only on a second click. The first
//! click arms it, its hint says `click again`, and a second click on it within [`ARM_FOR`]
//! acts. A click elsewhere, a key, and the end of that time take the arm away. [`hint`] gives
//! the label and the parts of a hint with such keys.

use std::time::Duration;

use ratatui::layout::{Position, Rect};

use super::feed::Filter;
use super::model::{Card, Screen};
use super::msg::Key;
use super::needs::Target;
use super::text::width;

/// How long a part stays armed after its first click.
pub const ARM_FOR: Duration = Duration::from_secs(3);

/// The label of an armed hint of one key.
pub const AGAIN: &str = "click again";

/// What a click on a part does.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Act {
    /// The same as this key.
    Key(Key),
    /// The same as this key, which sends something or records a decision: the first click arms
    /// the part, and a second click acts.
    Decide(Key),
    /// A row of Home, by its ticket key: the first click selects it, and a click on the
    /// selected row opens it as Enter does.
    Ticket(String),
    /// A row of Models, by its index, in the same way.
    Agent(usize),
    /// A row of the log, by its index, in the same way.
    Line(usize),
    /// A step of the continue card, by its index: the first click selects it. The selected
    /// step is a part that continues the ticket, as Enter does, so it arms first.
    Pick(usize),
    /// A part of the question form.
    Form(Target),
    /// A tab of the activity.
    Tab(Filter),
}

impl Act {
    /// True for a part that sends something or records a decision: [`Act::Decide`], and the
    /// send of the question form.
    pub fn decides(&self) -> bool {
        matches!(self, Act::Decide(_) | Act::Form(Target::Send))
    }
}

/// A part that a first click armed: what it does, the screen and the card that showed it, and
/// the time until which a second click on it acts.
#[derive(Clone, Debug, PartialEq)]
pub struct Armed {
    pub act: Act,
    pub until: Duration,
    pub screen: Screen,
    /// The card that showed the part, as [`card_place`] names it.
    pub card: Option<String>,
}

/// The name of a card for an arm: its kind, its ticket, and for the card of the things that
/// wait, the item and the page that show. New data for the same card, such as a load of the
/// items or the end of a check, keeps the name, so it keeps the arm. Another item takes it away.
pub fn card_place(card: &Card) -> String {
    match card {
        Card::Start(c) => format!("start {}", c.key),
        Card::Continue(c) => format!("continue {}", c.key),
        Card::Kill(c) => format!("kill {}", c.turn_on),
        Card::Quit => "quit".into(),
        Card::Help => "help".into(),
        Card::Need(c) => {
            let item = c.items.as_ref().and_then(|items| items.get(c.at));
            let id = item.map(|item| item.id()).unwrap_or_default();
            format!("need {id} {}", c.page)
        }
        Card::Error { title, .. } => format!("error {title}"),
        Card::Stop(c) => format!("stop {} {}", c.key, c.step),
        Card::Retry(c) => format!("retry {} {}", c.key, c.step),
        Card::Open(c) => format!("open {}", c.key),
        Card::Opened(c) => format!("opened {}", c.key),
    }
}

/// The parts of one frame. A later part wins over an earlier part at the same cell, so that a
/// small part on a row, such as an arrow, wins over the row.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct HitMap {
    clicks: Vec<(Rect, Act)>,
    /// Each part that the wheel scrolls, and the presses of Up or Down that one step of the
    /// wheel gives there.
    wheels: Vec<(Rect, usize)>,
}

impl HitMap {
    /// Records a part that a click hits.
    pub fn click(&mut self, area: Rect, act: Act) {
        if !area.is_empty() {
            self.clicks.push((area, act));
        }
    }

    /// Records a part that the wheel scrolls: one step of the wheel is `by` presses of Up or
    /// Down.
    pub fn wheel(&mut self, area: Rect, by: usize) {
        if !area.is_empty() && by > 0 {
            self.wheels.push((area, by));
        }
    }

    /// What a click at this cell does, or `None` outside each part.
    pub fn click_at(&self, column: u16, row: u16) -> Option<&Act> {
        let at = Position::new(column, row);
        self.clicks
            .iter()
            .rev()
            .find(|(area, _)| area.contains(at))
            .map(|(_, act)| act)
    }

    /// The presses of Up or Down that one step of the wheel gives at this cell, or `None`
    /// outside each part that scrolls.
    pub fn wheel_at(&self, column: u16, row: u16) -> Option<usize> {
        let at = Position::new(column, row);
        self.wheels
            .iter()
            .rev()
            .find(|(area, _)| area.contains(at))
            .map(|(_, by)| *by)
    }

    /// Forgets each part, such as the parts of a screen under a card.
    pub fn clear(&mut self) {
        self.clicks.clear();
        self.wheels.clear();
    }

    pub fn is_empty(&self) -> bool {
        self.clicks.is_empty() && self.wheels.is_empty()
    }

    /// Each part that a click hits, in the order of the draw.
    pub fn clicks(&self) -> &[(Rect, Act)] {
        &self.clicks
    }

    /// Each part that the wheel scrolls, in the order of the draw.
    pub fn wheels(&self) -> &[(Rect, usize)] {
        &self.wheels
    }
}

/// The key of one word of a hint, such as `enter` or `pgup`.
fn named(word: &str) -> Option<Key> {
    Some(match word {
        "enter" => Key::Enter,
        "esc" => Key::Esc,
        "tab" => Key::Tab,
        "pgup" => Key::PageUp,
        "pgdn" => Key::PageDown,
        "home" => Key::Home,
        "end" => Key::End,
        _ => return None,
    })
}

/// The key of an arrow.
fn arrow(c: char) -> Option<Key> {
    Some(match c {
        '↑' => Key::Up,
        '↓' => Key::Down,
        '←' => Key::Left,
        '→' => Key::Right,
        _ => return None,
    })
}

/// The keys that the key text of a hint names, each with its column in the text and its width:
/// a named key, an arrow, one character, the two ends of a range such as `1-3`, or the letters
/// of a list such as `y a n`. A word that is no key, such as `to` in `a to f`, names none.
pub fn keys_of(text: &str) -> Vec<(i32, i32, Key)> {
    let mut out = Vec::new();
    let mut x = 0;
    for (i, word) in text.split(' ').enumerate() {
        if i > 0 {
            x += 1;
        }
        let w = width(word) as i32;
        let chars: Vec<char> = word.chars().collect();
        if let Some(key) = named(word) {
            out.push((x, w, key));
        } else if let [c] = chars.as_slice() {
            out.push((x, w, arrow(*c).unwrap_or(Key::Char(*c))));
        } else if !chars.is_empty() && chars.iter().all(|c| arrow(*c).is_some()) {
            let mut cx = x;
            for c in &chars {
                let cw = width(&c.to_string()) as i32;
                if let Some(key) = arrow(*c) {
                    out.push((cx, cw, key));
                }
                cx += cw;
            }
        } else if let [a, '-', b] = chars.as_slice() {
            if a.is_ascii_digit() && b.is_ascii_digit() {
                out.push((x, 1, Key::Char(*a)));
                out.push((x + 2, 1, Key::Char(*b)));
            }
        }
        x += w;
    }
    out
}

/// The parts of a key hint whose key text starts at the column `x`, and whose label follows
/// after one space: the column, the width, and the key of each part. A hint of one key is one
/// part with its label. A hint of more keys has a part for each key, and its label is no part.
pub fn hint_parts(x: i32, key: &str, label: &str) -> Vec<(i32, i32, Key)> {
    let keys = keys_of(key);
    let key_w = width(key) as i32;
    match keys.as_slice() {
        [(0, w, one)] if *w == key_w => {
            let label_w = if label.is_empty() {
                0
            } else {
                1 + width(label) as i32
            };
            vec![(x, key_w + label_w, *one)]
        }
        _ => keys.into_iter().map(|(dx, w, k)| (x + dx, w, k)).collect(),
    }
}

/// The text of a key in a hint, such as `enter` or `y`.
fn key_name(key: Key) -> String {
    match key {
        Key::Enter => "enter".into(),
        Key::Esc => "esc".into(),
        Key::Tab => "tab".into(),
        Key::Char(c) => c.to_string(),
        other => format!("{other:?}").to_lowercase(),
    }
}

/// A key hint as it shows: its label, true while a part of it is armed, and each part with its
/// column, its width, and what a click does.
#[derive(Clone, Debug, PartialEq)]
pub struct Hint {
    pub label: String,
    pub armed: bool,
    pub parts: Vec<(i32, i32, Act)>,
}

/// A key hint whose key text starts at the column `x`. A key of `decide` sends something or
/// records a decision, so its part is [`Act::Decide`]. While `armed` is the part of such a key,
/// the label says to click again: [`AGAIN`] for a hint of one key, and `click y again` for the
/// key `y` of a hint of more keys. Only the label changes, so the key keeps its place, and a
/// second click on it hits the same part.
pub fn hint(x: i32, key: &str, label: &str, decide: &[Key], armed: Option<&Act>) -> Hint {
    let act = |k: Key| {
        if decide.contains(&k) {
            Act::Decide(k)
        } else {
            Act::Key(k)
        }
    };
    let keys = keys_of(key);
    let one = matches!(keys.as_slice(), [(0, w, _)] if *w == width(key) as i32);
    let hit = keys
        .iter()
        .find(|(_, _, k)| decide.contains(k) && armed == Some(&Act::Decide(*k)));
    let label = match hit {
        None => label.to_string(),
        Some(_) if one => AGAIN.to_string(),
        Some((_, _, k)) => format!("click {} again", key_name(*k)),
    };
    let parts = hint_parts(x, key, &label)
        .into_iter()
        .map(|(hx, hw, k)| (hx, hw, act(k)))
        .collect();
    Hint {
        label,
        armed: hit.is_some(),
        parts,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rect(x: u16, y: u16, w: u16, h: u16) -> Rect {
        Rect::new(x, y, w, h)
    }

    #[test]
    fn a_key_that_decides_is_a_part_that_arms_and_its_label_says_to_click_again() {
        let enter = Act::Decide(Key::Enter);
        let shown = hint(3, "enter", "start", &[Key::Enter], None);
        assert_eq!((shown.label.as_str(), shown.armed), ("start", false));
        assert_eq!(shown.parts, [(3, 11, enter.clone())]);
        let shown = hint(3, "enter", "start", &[Key::Enter], Some(&enter));
        assert_eq!((shown.label.as_str(), shown.armed), ("click again", true));
        assert_eq!(
            shown.parts,
            [(3, 17, enter.clone())],
            "the part grows with its label"
        );
        // A key that does not decide keeps its label, also while another part is armed.
        let shown = hint(20, "esc", "cancel", &[Key::Enter], Some(&enter));
        assert_eq!((shown.label.as_str(), shown.armed), ("cancel", false));
        assert_eq!(shown.parts, [(20, 10, Act::Key(Key::Esc))]);
        // A hint of more keys: each deciding key is a part that arms, and the label names it.
        let y = Act::Decide(Key::Char('y'));
        let decide = [Key::Char('y'), Key::Char('a')];
        let shown = hint(3, "y a n", "choose", &decide, Some(&y));
        assert_eq!((shown.label.as_str(), shown.armed), ("click y again", true));
        assert_eq!(
            shown.parts,
            [
                (3, 1, y.clone()),
                (5, 1, Act::Decide(Key::Char('a'))),
                (7, 1, Act::Key(Key::Char('n')))
            ]
        );
        // An armed part of another key leaves the label as it is.
        let shown = hint(3, "a", "approve", &[Key::Char('a')], Some(&y));
        assert_eq!((shown.label.as_str(), shown.armed), ("approve", false));
        // The label that shows gives the same hint again, so a list can fit its labels first.
        let again = hint(3, "enter", AGAIN, &[Key::Enter], Some(&enter));
        assert_eq!(again.label, AGAIN);
        assert!(Act::Decide(Key::Enter).decides());
        assert!(Act::Form(Target::Send).decides());
        assert!(!Act::Key(Key::Enter).decides());
        assert!(!Act::Form(Target::Row(0)).decides());
    }

    #[test]
    fn a_click_hits_a_part_inside_and_on_its_edges_and_nothing_outside() {
        let mut map = HitMap::default();
        map.click(rect(10, 5, 4, 2), Act::Key(Key::Char('n')));
        let n = Some(&Act::Key(Key::Char('n')));
        for (x, y) in [(10, 5), (13, 5), (10, 6), (13, 6), (11, 6)] {
            assert_eq!(map.click_at(x, y), n, "({x}, {y}) is inside");
        }
        for (x, y) in [(9, 5), (14, 5), (10, 4), (10, 7), (14, 7), (0, 0)] {
            assert_eq!(map.click_at(x, y), None, "({x}, {y}) is outside");
        }
        assert_eq!(
            map.wheel_at(11, 5),
            None,
            "a part of a click does not scroll"
        );
    }

    #[test]
    fn a_later_part_wins_and_the_wheel_has_its_own_parts() {
        let mut map = HitMap::default();
        map.click(rect(0, 3, 40, 2), Act::Agent(1));
        map.click(rect(15, 3, 2, 1), Act::Key(Key::Left));
        map.wheel(rect(0, 3, 40, 10), 1);
        map.wheel(rect(0, 3, 40, 10), 3);
        assert_eq!(map.click_at(15, 3), Some(&Act::Key(Key::Left)));
        assert_eq!(map.click_at(17, 3), Some(&Act::Agent(1)));
        assert_eq!(map.click_at(15, 4), Some(&Act::Agent(1)));
        assert_eq!(map.wheel_at(0, 12), Some(3));
        assert_eq!(map.wheel_at(0, 13), None);
        // An empty part and a wheel of no step are not kept.
        map.click(rect(50, 0, 0, 1), Act::Key(Key::Esc));
        map.wheel(rect(50, 0, 5, 1), 0);
        assert_eq!(map.clicks().len(), 2);
        assert_eq!(map.wheels().len(), 2);
        map.clear();
        assert!(map.is_empty());
        assert_eq!(map.click_at(15, 3), None);
    }

    #[test]
    fn the_key_text_of_a_hint_names_its_keys() {
        assert_eq!(keys_of("enter"), [(0, 5, Key::Enter)]);
        assert_eq!(keys_of("?"), [(0, 1, Key::Char('?'))]);
        assert_eq!(keys_of("↑↓"), [(0, 1, Key::Up), (1, 1, Key::Down)]);
        assert_eq!(keys_of("←→"), [(0, 1, Key::Left), (1, 1, Key::Right)]);
        assert_eq!(
            keys_of("pgup pgdn"),
            [(0, 4, Key::PageUp), (5, 4, Key::PageDown)]
        );
        assert_eq!(
            keys_of("g G"),
            [(0, 1, Key::Char('g')), (2, 1, Key::Char('G'))]
        );
        assert_eq!(
            keys_of("y a n"),
            [
                (0, 1, Key::Char('y')),
                (2, 1, Key::Char('a')),
                (4, 1, Key::Char('n'))
            ]
        );
        assert_eq!(
            keys_of("a to f"),
            [(0, 1, Key::Char('a')), (5, 1, Key::Char('f'))]
        );
        assert_eq!(
            keys_of("1-3"),
            [(0, 1, Key::Char('1')), (2, 1, Key::Char('3'))]
        );
        assert_eq!(keys_of("log"), [], "a word that is no key names none");
    }

    #[test]
    fn a_hint_of_one_key_is_one_part_with_its_label() {
        assert_eq!(hint_parts(1, "n", "needs"), [(1, 7, Key::Char('n'))]);
        assert_eq!(
            hint_parts(12, "enter", "open or start"),
            [(12, 19, Key::Enter)]
        );
        assert_eq!(hint_parts(3, "esc", ""), [(3, 3, Key::Esc)]);
        // A hint of more keys: a part for each key, and none for the label.
        assert_eq!(
            hint_parts(1, "↑↓", "move"),
            [(1, 1, Key::Up), (2, 1, Key::Down)]
        );
        assert_eq!(
            hint_parts(40, "a b c", "answer"),
            [
                (40, 1, Key::Char('a')),
                (42, 1, Key::Char('b')),
                (44, 1, Key::Char('c'))
            ]
        );
    }
}
