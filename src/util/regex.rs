//! A small regular expression, for a pattern that the person writes in the configuration, such
//! as the priority of a comment of a review bot (`\*\*\[P([0-3])\]`). The crate list of the repo
//! has no regex crate, so this module reads the common part of the syntax:
//!
//! - a character, `.` (any character but a line end), and `\` before a mark that stands for
//!   itself, such as `\*` or `\[`;
//! - `\d`, `\w`, `\s` and their opposites `\D`, `\W`, `\S`, and `\t`, `\n`, `\r`;
//! - a class such as `[0-3]`, `[A-Za-z_]`, or `[^ ]`;
//! - a group `( )` that captures, and `(?: )` that does not;
//! - `|` between choices;
//! - `*`, `+`, `?`, `{n}`, `{n,}`, and `{n,m}`, each one greedy, or lazy with a `?` after it;
//! - `^` and `$` for the start and the end of the text;
//! - `(?i)` at the start for a match in any case.
//!
//! A search tries each start in the text and gives the first match. The matcher backtracks with
//! a budget of steps, so a pattern of the person never hangs a tick on an untrusted text: a
//! search above the budget finds nothing.

use std::cell::Cell;

/// The most steps of one search.
const BUDGET: u64 = 200_000;

#[derive(Clone, Debug, PartialEq)]
enum Node {
    Char(char),
    Any,
    Class {
        ranges: Vec<(char, char)>,
        negated: bool,
    },
    Start,
    End,
    Group(Box<Node>, Option<usize>),
    Concat(Vec<Node>),
    Alt(Vec<Node>),
    Repeat {
        node: Box<Node>,
        min: u32,
        max: Option<u32>,
        greedy: bool,
    },
}

/// A parsed pattern.
#[derive(Clone, Debug, PartialEq)]
pub struct Regex {
    root: Node,
    groups: usize,
    any_case: bool,
}

type Caps = Vec<Option<(usize, usize)>>;

struct Parser<'a> {
    chars: Vec<char>,
    at: usize,
    groups: usize,
    pattern: &'a str,
}

impl Parser<'_> {
    fn error(&self, what: &str) -> String {
        format!(
            "the pattern `{}` is not a regular expression this program reads: {what} (at character {})",
            self.pattern,
            self.at + 1
        )
    }

    fn peek(&self) -> Option<char> {
        self.chars.get(self.at).copied()
    }

    fn alt(&mut self) -> Result<Node, String> {
        let mut choices = vec![self.concat()?];
        while self.peek() == Some('|') {
            self.at += 1;
            choices.push(self.concat()?);
        }
        Ok(if choices.len() == 1 {
            choices.pop().unwrap_or(Node::Concat(Vec::new()))
        } else {
            Node::Alt(choices)
        })
    }

    fn concat(&mut self) -> Result<Node, String> {
        let mut items = Vec::new();
        while let Some(c) = self.peek() {
            if c == '|' || c == ')' {
                break;
            }
            let atom = self.atom()?;
            items.push(self.quantified(atom)?);
        }
        Ok(Node::Concat(items))
    }

    fn quantified(&mut self, atom: Node) -> Result<Node, String> {
        let (min, max) = match self.peek() {
            Some('*') => {
                self.at += 1;
                (0, None)
            }
            Some('+') => {
                self.at += 1;
                (1, None)
            }
            Some('?') => {
                self.at += 1;
                (0, Some(1))
            }
            Some('{') => match self.braces()? {
                Some(bounds) => bounds,
                None => return Ok(atom),
            },
            _ => return Ok(atom),
        };
        if matches!(atom, Node::Start | Node::End) {
            return Err(self.error("a quantifier follows an anchor"));
        }
        let greedy = if self.peek() == Some('?') {
            self.at += 1;
            false
        } else {
            true
        };
        if matches!(self.peek(), Some('*' | '+' | '{')) {
            return Err(self.error("two quantifiers follow each other"));
        }
        Ok(Node::Repeat {
            node: Box::new(atom),
            min,
            max,
            greedy,
        })
    }

    /// `{n}`, `{n,}`, or `{n,m}`. A `{` that starts no such form is a plain character.
    fn braces(&mut self) -> Result<Option<(u32, Option<u32>)>, String> {
        let start = self.at;
        let rest: String = self.chars[start..].iter().collect();
        let Some(end) = rest.find('}') else {
            return Ok(None);
        };
        let inner = &rest[1..end];
        let number = |t: &str| -> Option<u32> {
            (!t.is_empty() && t.len() <= 4 && t.bytes().all(|b| b.is_ascii_digit()))
                .then(|| t.parse().ok())
                .flatten()
        };
        let bounds = match inner.split_once(',') {
            None => number(inner).map(|n| (n, Some(n))),
            Some((a, "")) => number(a).map(|n| (n, None)),
            Some((a, b)) => match (number(a), number(b)) {
                (Some(a), Some(b)) if a <= b => Some((a, Some(b))),
                (Some(_), Some(_)) => return Err(self.error("a bound {n,m} has n above m")),
                _ => None,
            },
        };
        if bounds.is_some() {
            self.at = start + rest[..=end].chars().count();
        }
        Ok(bounds)
    }

    fn atom(&mut self) -> Result<Node, String> {
        let Some(c) = self.peek() else {
            return Err(self.error("the pattern ends too early"));
        };
        self.at += 1;
        match c {
            '.' => Ok(Node::Any),
            '^' => Ok(Node::Start),
            '$' => Ok(Node::End),
            '(' => {
                let index = if self.chars[self.at..].starts_with(&['?', ':']) {
                    self.at += 2;
                    None
                } else if self.peek() == Some('?') {
                    return Err(
                        self.error("a group with `?` is only `(?:...)`, or `(?i)` at the start")
                    );
                } else {
                    self.groups += 1;
                    Some(self.groups)
                };
                let inner = self.alt()?;
                if self.peek() != Some(')') {
                    return Err(self.error("a group does not close"));
                }
                self.at += 1;
                Ok(Node::Group(Box::new(inner), index))
            }
            ')' => Err(self.error("a `)` closes no group")),
            '[' => self.class(),
            '\\' => self
                .escape()
                .map(|(ranges, negated)| match ranges.as_slice() {
                    [(a, b)] if a == b && !negated => Node::Char(*a),
                    _ => Node::Class { ranges, negated },
                }),
            '*' | '+' | '?' => Err(self.error("a quantifier has nothing before it")),
            other => Ok(Node::Char(other)),
        }
    }

    /// The character or the set of an escape, after its `\`.
    fn escape(&mut self) -> Result<(Vec<(char, char)>, bool), String> {
        let Some(c) = self.peek() else {
            return Err(self.error("the pattern ends with `\\`"));
        };
        self.at += 1;
        let digit = vec![('0', '9')];
        let word = vec![('a', 'z'), ('A', 'Z'), ('0', '9'), ('_', '_')];
        let space = vec![
            (' ', ' '),
            ('\t', '\t'),
            ('\n', '\n'),
            ('\r', '\r'),
            ('\u{b}', '\u{c}'),
        ];
        Ok(match c {
            'd' => (digit, false),
            'D' => (digit, true),
            'w' => (word, false),
            'W' => (word, true),
            's' => (space, false),
            'S' => (space, true),
            't' => (vec![('\t', '\t')], false),
            'n' => (vec![('\n', '\n')], false),
            'r' => (vec![('\r', '\r')], false),
            c if c.is_ascii_alphanumeric() => {
                return Err(self.error(&format!("the escape `\\{c}` is not known")))
            }
            c => (vec![(c, c)], false),
        })
    }

    fn class(&mut self) -> Result<Node, String> {
        let negated = if self.peek() == Some('^') {
            self.at += 1;
            true
        } else {
            false
        };
        let mut ranges = Vec::new();
        let mut first = true;
        loop {
            let Some(c) = self.peek() else {
                return Err(self.error("a class `[` does not close"));
            };
            if c == ']' && !first {
                self.at += 1;
                break;
            }
            first = false;
            self.at += 1;
            let low = if c == '\\' {
                let (set, negated) = self.escape()?;
                if negated {
                    return Err(self.error("a class cannot hold \\D, \\W, or \\S"));
                }
                if set.len() != 1 || set[0].0 != set[0].1 {
                    ranges.extend(set);
                    continue;
                }
                set[0].0
            } else {
                c
            };
            if self.peek() == Some('-') && self.chars.get(self.at + 1).is_some_and(|n| *n != ']') {
                self.at += 1;
                let Some(mut high) = self.peek() else {
                    return Err(self.error("a range in a class does not end"));
                };
                self.at += 1;
                if high == '\\' {
                    let (set, _) = self.escape()?;
                    high = set[0].0;
                }
                if high < low {
                    return Err(self.error("a range in a class goes down"));
                }
                ranges.push((low, high));
            } else {
                ranges.push((low, low));
            }
        }
        Ok(Node::Class { ranges, negated })
    }
}

struct Matcher<'a> {
    text: &'a [char],
    any_case: bool,
    steps: Cell<u64>,
}

impl Matcher<'_> {
    fn spend(&self) -> bool {
        let steps = self.steps.get() + 1;
        self.steps.set(steps);
        steps <= BUDGET
    }

    fn same(&self, a: char, b: char) -> bool {
        a == b || (self.any_case && a.to_lowercase().eq(b.to_lowercase()))
    }

    fn in_class(&self, ranges: &[(char, char)], c: char) -> bool {
        let inside = |c: char| ranges.iter().any(|(lo, hi)| (*lo..=*hi).contains(&c));
        inside(c)
            || (self.any_case && (c.to_lowercase().any(inside) || c.to_uppercase().any(inside)))
    }

    fn m(
        &self,
        node: &Node,
        pos: usize,
        caps: &mut Caps,
        k: &mut dyn FnMut(usize, &mut Caps) -> bool,
    ) -> bool {
        if !self.spend() {
            return false;
        }
        match node {
            Node::Char(c) => {
                self.text.get(pos).is_some_and(|t| self.same(*t, *c)) && k(pos + 1, caps)
            }
            Node::Any => self.text.get(pos).is_some_and(|t| *t != '\n') && k(pos + 1, caps),
            Node::Class { ranges, negated } => {
                self.text
                    .get(pos)
                    .is_some_and(|t| self.in_class(ranges, *t) != *negated)
                    && k(pos + 1, caps)
            }
            Node::Start => pos == 0 && k(pos, caps),
            Node::End => pos == self.text.len() && k(pos, caps),
            Node::Group(inner, index) => match index {
                None => self.m(inner, pos, caps, k),
                Some(i) => {
                    let i = *i;
                    self.m(inner, pos, caps, &mut |end, caps: &mut Caps| {
                        let old = caps[i];
                        caps[i] = Some((pos, end));
                        if k(end, caps) {
                            return true;
                        }
                        caps[i] = old;
                        false
                    })
                }
            },
            Node::Concat(items) => self.seq(items, pos, caps, k),
            Node::Alt(choices) => choices.iter().any(|c| self.m(c, pos, caps, k)),
            Node::Repeat {
                node,
                min,
                max,
                greedy,
            } => self.rep(node, *min, *max, *greedy, 0, pos, caps, k),
        }
    }

    fn seq(
        &self,
        items: &[Node],
        pos: usize,
        caps: &mut Caps,
        k: &mut dyn FnMut(usize, &mut Caps) -> bool,
    ) -> bool {
        match items.split_first() {
            None => k(pos, caps),
            Some((first, rest)) => self.m(first, pos, caps, &mut |p, caps: &mut Caps| {
                self.seq(rest, p, caps, k)
            }),
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn rep(
        &self,
        node: &Node,
        min: u32,
        max: Option<u32>,
        greedy: bool,
        count: u32,
        pos: usize,
        caps: &mut Caps,
        k: &mut dyn FnMut(usize, &mut Caps) -> bool,
    ) -> bool {
        if !self.spend() {
            return false;
        }
        let can_more = max.is_none_or(|m| count < m);
        if !greedy && count >= min && k(pos, caps) {
            return true;
        }
        if can_more {
            let more = self.m(node, pos, caps, &mut |p, caps: &mut Caps| {
                // A loop that matched nothing stops, so that `(a*)*` ends.
                if p == pos && count >= min {
                    return false;
                }
                self.rep(node, min, max, greedy, count + 1, p, caps, k)
            });
            if more {
                return true;
            }
        }
        greedy && count >= min && k(pos, caps)
    }
}

impl Regex {
    /// Reads a pattern. The error names what this module cannot read.
    pub fn new(pattern: &str) -> Result<Regex, String> {
        let (any_case, body) = match pattern.strip_prefix("(?i)") {
            Some(rest) => (true, rest),
            None => (false, pattern),
        };
        let mut parser = Parser {
            chars: body.chars().collect(),
            at: 0,
            groups: 0,
            pattern,
        };
        let root = parser.alt()?;
        if parser.at < parser.chars.len() {
            return Err(parser.error("a `)` closes no group"));
        }
        Ok(Regex {
            root,
            groups: parser.groups,
            any_case,
        })
    }

    /// The number of groups that capture.
    pub fn groups(&self) -> usize {
        self.groups
    }

    /// The first match in the text: the whole match first, then each group, `None` for a group
    /// that took no part.
    pub fn captures(&self, text: &str) -> Option<Vec<Option<String>>> {
        let chars: Vec<char> = text.chars().collect();
        let matcher = Matcher {
            text: &chars,
            any_case: self.any_case,
            steps: Cell::new(0),
        };
        for start in 0..=chars.len() {
            let mut caps: Caps = vec![None; self.groups + 1];
            let mut found: Option<Caps> = None;
            let hit = matcher.m(&self.root, start, &mut caps, &mut |end, caps: &mut Caps| {
                let mut all = caps.clone();
                all[0] = Some((start, end));
                found = Some(all);
                true
            });
            if hit {
                let caps = found?;
                return Some(
                    caps.into_iter()
                        .map(|c| c.map(|(a, b)| chars[a..b].iter().collect()))
                        .collect(),
                );
            }
            if matcher.steps.get() > BUDGET {
                return None;
            }
        }
        None
    }

    /// True when the text holds a match.
    pub fn is_match(&self, text: &str) -> bool {
        self.captures(text).is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn group(pattern: &str, text: &str, n: usize) -> Option<String> {
        Regex::new(pattern).unwrap().captures(text)?.get(n)?.clone()
    }

    #[test]
    fn the_priority_of_a_review_bot_is_group_one() {
        let p = r"\*\*\[P([0-3])\]";
        assert_eq!(Regex::new(p).unwrap().groups(), 1);
        assert_eq!(
            group(p, "**[P1] Handle the nil user**\n\nMore text.", 1).as_deref(),
            Some("1")
        );
        assert_eq!(group(p, "text **[P0] a**", 0).as_deref(), Some("**[P0]"));
        assert_eq!(group(p, "**[P4] no**", 1), None);
        assert_eq!(group(p, "[P1] without bold", 1), None);
    }

    #[test]
    fn the_common_syntax_reads() {
        assert_eq!(group(r"a(b|c)+d", "xxabcbd", 1).as_deref(), Some("b"));
        assert_eq!(group(r"a(b|c)+d", "xxabcbd", 0).as_deref(), Some("abcbd"));
        assert_eq!(group(r"(\d{2,3})", "x12345", 1).as_deref(), Some("123"));
        assert_eq!(group(r"(\d{2,3}?)", "x12345", 1).as_deref(), Some("12"));
        assert_eq!(group(r"(\d{2})", "x1y", 1), None);
        assert_eq!(group(r"^(\w+)", "hello world", 1).as_deref(), Some("hello"));
        assert_eq!(group(r"(\w+)$", "hello world", 1).as_deref(), Some("world"));
        assert_eq!(group(r"(?:x)(y)", "xy", 1).as_deref(), Some("y"));
        assert_eq!(group(r"([^ ]+) ", "ab cd", 1).as_deref(), Some("ab"));
        assert_eq!(group(r"(.*)!", "hi!", 1).as_deref(), Some("hi"));
        assert_eq!(group(r"(.*?)!", "a!b!", 1).as_deref(), Some("a"));
        assert_eq!(group(r"p(r)?io", "pio", 1), None);
        assert_eq!(
            group(r"(?i)priority: (high|low)", "PRIORITY: High", 1).as_deref(),
            Some("High")
        );
        assert_eq!(group(r"[A-Z_]+", "ab CD_E f", 0).as_deref(), Some("CD_E"));
        assert_eq!(group(r"a{x", "a{x", 0).as_deref(), Some("a{x"));
        assert_eq!(group(r"[\]]", "a]", 0).as_deref(), Some("]"));
        assert_eq!(group(r"[a\-z]", "x-y", 0).as_deref(), Some("-"));
        assert_eq!(group(r"\s(\S+)", "a  b", 1).as_deref(), Some("b"));
        assert!(Regex::new("abc").unwrap().is_match("xabcx"));
        assert!(!Regex::new("^abc$").unwrap().is_match("xabc"));
    }

    #[test]
    fn a_bad_pattern_is_named() {
        for bad in [
            "(a", "a)", "[a", "*a", r"\q", "a**", "(?=x)", "a{3,2}", r"x\",
        ] {
            let err = Regex::new(bad).unwrap_err();
            assert!(err.contains(&format!("`{bad}`")), "{bad}: {err}");
        }
    }

    #[test]
    fn a_pattern_that_explodes_stops_at_its_budget() {
        let p = Regex::new(r"(a+)+b").unwrap();
        let text = "a".repeat(40);
        let started = std::time::Instant::now();
        assert!(!p.is_match(&text));
        assert!(started.elapsed().as_secs() < 5);
        assert!(Regex::new(r"(a*)*b").unwrap().is_match("aaab"));
    }
}
