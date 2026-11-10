//! One memory file as text (section 9.1): the front matter between two `---` lines, and a body
//! of `##` sections.
//!
//! A [`Doc`] keeps each line as it was read: unknown keys, their order, comments, blank lines,
//! unknown sections, and the line ends (`\n` or `\r\n`). [`Doc::to_text`] of an unchanged doc
//! gives the same bytes back. A change rewrites only the lines that it touches.

use std::fmt;

use super::front::{parse_entry, render_value, ParsedLine, Value};

/// A line that the parser could not read, with its number (from 1).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ParseError {
    pub line: usize,
    pub message: String,
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "line {}: {}", self.line, self.message)
    }
}

impl std::error::Error for ParseError {}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Line {
    text: String,
    crlf: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum FrontLine {
    /// A blank line or a comment line.
    Other(Line),
    Entry(Entry),
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Entry {
    key: String,
    value: Value,
    comment: String,
    line: Line,
    /// True when the value changed, so that the line is written again.
    changed: bool,
}

impl Entry {
    fn text(&self) -> String {
        if self.changed {
            format!(
                "{}: {}{}",
                self.key,
                render_value(&self.value),
                self.comment
            )
        } else {
            self.line.text.clone()
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Section {
    heading: Line,
    name: String,
    lines: Vec<Line>,
}

/// A memory file. See the module text.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Doc {
    bom: bool,
    open: Line,
    front: Vec<FrontLine>,
    close: Line,
    preamble: Vec<Line>,
    sections: Vec<Section>,
    final_newline: bool,
    /// The line end of new lines: the end of the first line.
    crlf: bool,
}

impl Doc {
    /// Reads a memory file. The error names the line that is wrong.
    pub fn parse(text: &str) -> Result<Doc, ParseError> {
        let (bom, text) = match text.strip_prefix('\u{feff}') {
            Some(rest) => (true, rest),
            None => (false, text),
        };
        if text.is_empty() {
            return Err(err(
                1,
                "the file is empty. A memory file starts with a `---` line",
            ));
        }
        let mut pieces: Vec<&str> = text.split('\n').collect();
        let final_newline = pieces.last() == Some(&"");
        if final_newline {
            pieces.pop();
        }
        let mut lines = pieces
            .into_iter()
            .map(|piece| match piece.strip_suffix('\r') {
                Some(t) => Line {
                    text: t.to_string(),
                    crlf: true,
                },
                None => Line {
                    text: piece.to_string(),
                    crlf: false,
                },
            });
        let open = lines.next().ok_or_else(|| {
            err(
                1,
                "the file is empty. A memory file starts with a `---` line",
            )
        })?;
        if open.text.trim_end() != "---" {
            return Err(err(
                1,
                "a memory file starts with a `---` line, then the front matter, then `---`",
            ));
        }
        let crlf = open.crlf;
        let mut front = Vec::new();
        let mut close = None;
        let mut number = 1;
        for line in lines.by_ref() {
            number += 1;
            if line.text.trim_end() == "---" {
                close = Some(line);
                break;
            }
            let parsed = parse_entry(&line.text).map_err(|message| err(number, &message))?;
            match parsed {
                ParsedLine::Other => front.push(FrontLine::Other(line)),
                ParsedLine::Entry(e) => {
                    if let Some(first) = front_line_of(&front, &e.key) {
                        return Err(err(
                            number,
                            &format!(
                                "the key `{}` is here a second time (first on line {first})",
                                e.key
                            ),
                        ));
                    }
                    front.push(FrontLine::Entry(Entry {
                        key: e.key,
                        value: e.value,
                        comment: e.comment,
                        line,
                        changed: false,
                    }));
                }
            }
        }
        let Some(close) = close else {
            return Err(err(number, "the front matter has no closing `---` line"));
        };
        let mut preamble = Vec::new();
        let mut sections: Vec<Section> = Vec::new();
        for line in lines {
            if let Some(name) = heading_name(&line.text) {
                sections.push(Section {
                    name: name.to_string(),
                    heading: line,
                    lines: Vec::new(),
                });
            } else if let Some(section) = sections.last_mut() {
                section.lines.push(line);
            } else {
                preamble.push(line);
            }
        }
        Ok(Doc {
            bom,
            open,
            front,
            close,
            preamble,
            sections,
            final_newline,
            crlf,
        })
    }

    /// A new doc with these keys and sections, in this order.
    pub fn new(keys: &[(&str, Value)], sections: &[(&str, &str)]) -> Doc {
        let line = |text: String| Line { text, crlf: false };
        let front = keys
            .iter()
            .map(|(key, value)| {
                FrontLine::Entry(Entry {
                    key: key.to_string(),
                    value: value.clone(),
                    comment: String::new(),
                    line: line(String::new()),
                    changed: true,
                })
            })
            .collect();
        let mut doc = Doc {
            bom: false,
            open: line("---".into()),
            front,
            close: line("---".into()),
            preamble: Vec::new(),
            sections: Vec::new(),
            final_newline: true,
            crlf: false,
        };
        for (name, text) in sections {
            doc.set_section(name, text);
        }
        doc
    }

    /// The text of the file.
    pub fn to_text(&self) -> String {
        let mut lines: Vec<(String, bool)> = Vec::new();
        lines.push((self.open.text.clone(), self.open.crlf));
        for line in &self.front {
            match line {
                FrontLine::Other(l) => lines.push((l.text.clone(), l.crlf)),
                FrontLine::Entry(e) => lines.push((e.text(), e.line.crlf)),
            }
        }
        lines.push((self.close.text.clone(), self.close.crlf));
        for l in &self.preamble {
            lines.push((l.text.clone(), l.crlf));
        }
        for section in &self.sections {
            lines.push((section.heading.text.clone(), section.heading.crlf));
            for l in &section.lines {
                lines.push((l.text.clone(), l.crlf));
            }
        }
        let mut out = String::new();
        if self.bom {
            out.push('\u{feff}');
        }
        let count = lines.len();
        for (index, (text, crlf)) in lines.into_iter().enumerate() {
            out.push_str(&text);
            if index + 1 < count || self.final_newline {
                out.push_str(if crlf { "\r\n" } else { "\n" });
            }
        }
        out
    }

    /// The value of a key.
    pub fn get(&self, key: &str) -> Option<&Value> {
        self.entries().find(|e| e.key == key).map(|e| &e.value)
    }

    /// The keys in their order.
    pub fn keys(&self) -> Vec<&str> {
        self.entries().map(|e| e.key.as_str()).collect()
    }

    /// The line of a key in the file, from 1.
    pub fn key_line(&self, key: &str) -> Option<usize> {
        front_line_of(&self.front, key)
    }

    /// Sets a key. A new key goes after the last key. An unchanged value changes no byte.
    pub fn set(&mut self, key: &str, value: Value) {
        for line in &mut self.front {
            if let FrontLine::Entry(e) = line {
                if e.key == key {
                    if e.value != value {
                        e.value = value;
                        e.changed = true;
                    }
                    return;
                }
            }
        }
        let at = self
            .front
            .iter()
            .rposition(|l| matches!(l, FrontLine::Entry(_)))
            .map_or(self.front.len(), |i| i + 1);
        self.front.insert(
            at,
            FrontLine::Entry(Entry {
                key: key.to_string(),
                value,
                comment: String::new(),
                line: Line {
                    text: String::new(),
                    crlf: self.crlf,
                },
                changed: true,
            }),
        );
    }

    /// Removes a key. Gives true when it was there.
    pub fn remove(&mut self, key: &str) -> bool {
        let before = self.front.len();
        self.front
            .retain(|l| !matches!(l, FrontLine::Entry(e) if e.key == key));
        before != self.front.len()
    }

    /// The names of the sections, in their order.
    pub fn section_names(&self) -> Vec<&str> {
        self.sections.iter().map(|s| s.name.as_str()).collect()
    }

    /// The text of the first section with this name (case does not matter), without the blank
    /// lines at its start and end. `None` when the doc has no such section.
    pub fn section(&self, name: &str) -> Option<String> {
        let section = self
            .sections
            .iter()
            .find(|s| s.name.eq_ignore_ascii_case(name))?;
        let texts: Vec<&str> = section.lines.iter().map(|l| l.text.as_str()).collect();
        let start = texts.iter().position(|t| !t.trim().is_empty());
        let end = texts.iter().rposition(|t| !t.trim().is_empty());
        let (Some(start), Some(end)) = (start, end) else {
            return Some(String::new());
        };
        let body: Vec<String> = texts[start..=end].iter().map(|t| unescape(t)).collect();
        Some(body.join("\n"))
    }

    /// The line of the heading of a section, from 1.
    pub fn section_line(&self, name: &str) -> Option<usize> {
        let mut number = 2 + self.front.len() + self.preamble.len();
        for section in &self.sections {
            number += 1;
            if section.name.eq_ignore_ascii_case(name) {
                return Some(number);
            }
            number += section.lines.len();
        }
        None
    }

    /// The names that more than one section has.
    pub fn repeated_sections(&self) -> Vec<String> {
        let mut seen: Vec<String> = Vec::new();
        let mut repeated = Vec::new();
        for section in &self.sections {
            let lower = section.name.to_ascii_lowercase();
            if seen.contains(&lower) {
                if !repeated.contains(&section.name) {
                    repeated.push(section.name.clone());
                }
            } else {
                seen.push(lower);
            }
        }
        repeated
    }

    /// Sets the text of a section, or adds the section at the end. A line of the text that
    /// would start a section gets a `\` in front of it. An unchanged text changes no byte.
    pub fn set_section(&mut self, name: &str, text: &str) {
        let text = text.trim_matches('\n');
        let crlf = self.crlf;
        let new_lines = |text: &str| -> Vec<Line> {
            if text.trim().is_empty() {
                return Vec::new();
            }
            text.lines()
                .map(|t| Line {
                    text: escape(t.strip_suffix('\r').unwrap_or(t)),
                    crlf,
                })
                .collect()
        };
        if let Some(index) = self
            .sections
            .iter()
            .position(|s| s.name.eq_ignore_ascii_case(name))
        {
            if self.section(name).as_deref() == Some(normalized(text).as_str()) {
                return;
            }
            let mut lines = new_lines(text);
            if index + 1 < self.sections.len() {
                lines.push(Line {
                    text: String::new(),
                    crlf,
                });
            }
            self.sections[index].lines = lines;
            return;
        }
        let blank = Line {
            text: String::new(),
            crlf,
        };
        match self.sections.last_mut() {
            Some(last) => {
                if last.lines.last().is_none_or(|l| !l.text.trim().is_empty()) {
                    last.lines.push(blank);
                }
            }
            None => {
                if self
                    .preamble
                    .last()
                    .is_some_and(|l| !l.text.trim().is_empty())
                {
                    self.preamble.push(blank);
                }
            }
        }
        self.sections.push(Section {
            heading: Line {
                text: format!("## {name}"),
                crlf,
            },
            name: name.to_string(),
            lines: new_lines(text),
        });
        self.final_newline = true;
    }

    /// Removes each section with this name. Gives true when one was there.
    pub fn remove_section(&mut self, name: &str) -> bool {
        let before = self.sections.len();
        self.sections.retain(|s| !s.name.eq_ignore_ascii_case(name));
        if before == self.sections.len() {
            return false;
        }
        // The new last section needs no blank line at its end.
        if let Some(last) = self.sections.last_mut() {
            while last.lines.last().is_some_and(|l| l.text.trim().is_empty()) {
                last.lines.pop();
            }
        }
        true
    }

    fn entries(&self) -> impl Iterator<Item = &Entry> {
        self.front.iter().filter_map(|l| match l {
            FrontLine::Entry(e) => Some(e),
            FrontLine::Other(_) => None,
        })
    }
}

fn err(line: usize, message: &str) -> ParseError {
    ParseError {
        line,
        message: message.to_string(),
    }
}

fn front_line_of(front: &[FrontLine], key: &str) -> Option<usize> {
    front
        .iter()
        .position(|l| matches!(l, FrontLine::Entry(e) if e.key == key))
        .map(|i| i + 2)
}

/// The name of a section heading: a line `## Name`. `###` and deeper do not start a section.
fn heading_name(text: &str) -> Option<&str> {
    let rest = text.strip_prefix("##")?;
    if rest.trim().is_empty() {
        return Some("");
    }
    rest.strip_prefix([' ', '\t']).map(str::trim)
}

fn escape(text: &str) -> String {
    if heading_name(text).is_some() || text.starts_with("\\##") {
        format!("\\{text}")
    } else {
        text.to_string()
    }
}

fn unescape(text: &str) -> String {
    match text.strip_prefix('\\') {
        Some(rest) if rest.starts_with("##") => rest.to_string(),
        _ => text.to_string(),
    }
}

/// A section text as [`Doc::section`] would give it back.
fn normalized(text: &str) -> String {
    let lines: Vec<&str> = text
        .lines()
        .map(|t| t.strip_suffix('\r').unwrap_or(t))
        .collect();
    let start = lines.iter().position(|t| !t.trim().is_empty());
    let end = lines.iter().rposition(|t| !t.trim().is_empty());
    match (start, end) {
        (Some(s), Some(e)) => lines[s..=e].join("\n"),
        _ => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const DECISION: &str = "---
id: D-0142
at: 2026-11-16T14:32:00Z
by: user                 # user | judge | import
basis: answer            # answer | decision | principle | inference
ticket: DEMO-12
area: [reports, exports]
status: active
custom_key: kept   # a key that Vision does not know
supersedes: null
---
## Question
Must the Export button make a CSV file or an XLSX file?
Choices: CSV | XLSX

## Answer
CSV

## Why

## Notes of the team
Free text that Vision does not know.
";

    #[test]
    fn an_unchanged_file_gives_the_same_bytes() {
        let doc = Doc::parse(DECISION).unwrap();
        assert_eq!(doc.to_text(), DECISION);
        let crlf = DECISION.replace('\n', "\r\n");
        assert_eq!(Doc::parse(&crlf).unwrap().to_text(), crlf);
        let no_end = DECISION.trim_end_matches('\n');
        assert_eq!(Doc::parse(no_end).unwrap().to_text(), no_end);
        let bom = format!("\u{feff}{DECISION}");
        assert_eq!(Doc::parse(&bom).unwrap().to_text(), bom);
        let odd = "---  \n\n# a comment line\nid:   D-0001    # spaced\n\n---\n\n\n## Question\n\n\nQ?\n\n\n";
        assert_eq!(Doc::parse(odd).unwrap().to_text(), odd);
        let mixed = "---\r\nid: D-0001\nby: user\r\n---\n## Answer\r\nx";
        assert_eq!(Doc::parse(mixed).unwrap().to_text(), mixed);
    }

    #[test]
    fn reads_keys_and_sections() {
        let doc = Doc::parse(DECISION).unwrap();
        assert_eq!(doc.get("by"), Some(&Value::str("user")));
        assert_eq!(doc.get("custom_key"), Some(&Value::str("kept")));
        assert_eq!(doc.get("missing"), None);
        assert_eq!(doc.key_line("id"), Some(2));
        assert_eq!(doc.key_line("supersedes"), Some(10));
        assert_eq!(
            doc.section_names(),
            ["Question", "Answer", "Why", "Notes of the team"]
        );
        assert_eq!(
            doc.section("question").unwrap(),
            "Must the Export button make a CSV file or an XLSX file?\nChoices: CSV | XLSX"
        );
        assert_eq!(doc.section("Why").unwrap(), "");
        assert_eq!(doc.section("Applies when"), None);
        assert_eq!(doc.section_line("Question"), Some(12));
        assert_eq!(doc.section_line("Answer"), Some(16));
        let lines: Vec<&str> = DECISION.lines().collect();
        assert_eq!(lines[15], "## Answer");
    }

    #[test]
    fn a_change_rewrites_only_its_line_and_keeps_unknown_parts() {
        let mut doc = Doc::parse(DECISION).unwrap();
        doc.set("status", Value::str("superseded"));
        doc.set("by", Value::str("judge"));
        doc.set("supersedes", Value::null());
        doc.set("new_key", Value::list(["a"]));
        let text = doc.to_text();
        assert!(text.contains("\nstatus: superseded\n"), "{text}");
        assert!(
            text.contains("\nby: judge                 # user | judge | import\n"),
            "{text}"
        );
        assert!(
            text.contains("\nsupersedes: null\nnew_key: [a]\n---\n"),
            "{text}"
        );
        assert!(text.contains("custom_key: kept   # a key that Vision does not know"));
        assert!(text.contains("## Notes of the team\nFree text that Vision does not know.\n"));
        let again = Doc::parse(&text).unwrap();
        assert_eq!(again.keys().last(), Some(&"new_key"));
        assert_eq!(again.to_text(), text);
    }

    #[test]
    fn sections_change_and_grow() {
        let mut doc = Doc::parse(DECISION).unwrap();
        doc.set_section("Why", "The finance team imports CSV.");
        doc.set_section("Answer", "CSV");
        doc.set_section("Retired", "No longer true.");
        let text = doc.to_text();
        assert!(
            text.contains("## Why\nThe finance team imports CSV.\n\n## Notes of the team"),
            "{text}"
        );
        assert!(
            text.ends_with("Free text that Vision does not know.\n\n## Retired\nNo longer true.\n"),
            "{text}"
        );
        let doc = Doc::parse(&text).unwrap();
        assert_eq!(doc.section("Retired").unwrap(), "No longer true.");
        let mut doc = doc;
        assert!(doc.remove_section("Retired"));
        assert!(doc
            .to_text()
            .ends_with("Free text that Vision does not know.\n"));
    }

    #[test]
    fn a_heading_inside_a_text_is_escaped() {
        let mut doc = Doc::new(
            &[("id", Value::str("D-0001"))],
            &[("Answer", "a\n## not a section\nb")],
        );
        doc.set_section("Why", "");
        let text = doc.to_text();
        assert!(text.contains("\\## not a section"), "{text}");
        let doc = Doc::parse(&text).unwrap();
        assert_eq!(doc.section_names(), ["Answer", "Why"]);
        assert_eq!(doc.section("Answer").unwrap(), "a\n## not a section\nb");
    }

    #[test]
    fn crlf_files_get_crlf_lines() {
        let crlf = DECISION.replace('\n', "\r\n");
        let mut doc = Doc::parse(&crlf).unwrap();
        doc.set("status", Value::str("retired"));
        doc.set_section("Retired", "Gone.");
        let text = doc.to_text();
        assert!(!text.replace("\r\n", "").contains('\n'), "{text:?}");
        assert!(text.ends_with("## Retired\r\nGone.\r\n"));
    }

    #[test]
    fn new_docs_have_the_layout_of_section_9_1() {
        let doc = Doc::new(
            &[
                ("id", Value::str("P-001")),
                ("evidence", Value::list(["D-0001"])),
            ],
            &[
                ("Rule", "When X, make sure that Y."),
                ("Because", ""),
                ("Not when", ""),
            ],
        );
        assert_eq!(
            doc.to_text(),
            "---\nid: P-001\nevidence: [D-0001]\n---\n## Rule\nWhen X, make sure that Y.\n\n## Because\n\n## Not when\n"
        );
    }

    #[test]
    fn broken_files_name_the_line() {
        for (text, line, word) in [
            ("", 1, "empty"),
            ("id: D-0001\n", 1, "starts with a `---`"),
            ("---\nid: D-0001\n", 2, "no closing"),
            ("---\nid: D-0001\nid: D-0002\n---\n", 3, "second time"),
            ("---\nid: D-0001\n  x: y\n---\n", 3, "starts with a space"),
            ("---\nid: [a\n---\n", 2, "no closing `]`"),
        ] {
            let e = Doc::parse(text).unwrap_err();
            assert_eq!(e.line, line, "{text:?}: {e}");
            assert!(e.message.contains(word), "{text:?}: {e}");
        }
    }

    #[test]
    fn an_empty_section_and_odd_headings() {
        let text = "---\nid: P-001\n---\nintro\n## Rule\nR\n### Detail\nmore\n##\n## Not when\n";
        let doc = Doc::parse(text).unwrap();
        assert_eq!(doc.section_names(), ["Rule", "", "Not when"]);
        assert_eq!(doc.section("Rule").unwrap(), "R\n### Detail\nmore");
        assert_eq!(doc.section("Not when").unwrap(), "");
        assert_eq!(doc.to_text(), text);
        let doc = Doc::parse("---\nid: P-001\n---\n## A\n## a\n").unwrap();
        assert_eq!(doc.repeated_sections(), ["a"]);
    }
}
