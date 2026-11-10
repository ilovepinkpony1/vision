//! The rule "no line is lost" (section 9.4) and `ARCHIVE.md`.
//!
//! Each line that a proposal removes or rewrites goes to `ARCHIVE.md` word for word, with the
//! commit that held it and the reason. One entry of the archive looks like this:
//!
//! ```text
//! ## proposal/2026-11-16-1 change 2
//!
//! - Change: edit principles/P-007.md
//! - Commit: 9f3e21c4d2a1...
//! - Reason: The rule was too wide.
//!
//! From principles/P-007.md:
//!
//! > When an internal user exports a report, make sure that the file is CSV.
//! ```
//!
//! Each archived line follows `> ` exactly. [`removed_lines`] and [`render_entry`] build the
//! entries when a proposal is built. [`lost_lines`] is the separate pure checker: it compares
//! the files before a proposal with the files of the proposal, and gives each line that is
//! gone and not in a new entry of the archive. Blank lines do not count.

use std::collections::BTreeMap;

use serde::Serialize;

use super::model::ARCHIVE;

/// The file of a proposal branch that holds its state. It never reaches the main branch, and
/// the checker skips it.
pub const PROPOSAL_FILE: &str = "proposal.json";

/// One line that a proposal loses.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct LostLine {
    /// The file that held the line.
    pub path: String,
    /// The line number in that file before the proposal, from 1.
    pub line: usize,
    pub text: String,
}

impl std::fmt::Display for LostLine {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}:{}: {}", self.path, self.line, self.text)
    }
}

/// The lines of a text that count: each line without its `\r`, with blank lines left out.
fn counted_lines(text: &str) -> impl Iterator<Item = (usize, &str)> {
    text.split('\n')
        .map(|l| l.strip_suffix('\r').unwrap_or(l))
        .enumerate()
        .map(|(i, l)| (i + 1, l))
        .filter(|(_, l)| !l.trim().is_empty())
}

fn counts(text: &str) -> BTreeMap<&str, usize> {
    let mut map = BTreeMap::new();
    for (_, line) in counted_lines(text) {
        *map.entry(line).or_insert(0) += 1;
    }
    map
}

/// The lines of `old` that `new` does not have, in the order of `old`. A line that `new` has
/// at a different place does not count. A line that appears twice in `old` and once in `new`
/// counts once.
pub fn removed_lines(old: &str, new: &str) -> Vec<String> {
    let mut left = counts(new);
    let mut out = Vec::new();
    for (_, line) in counted_lines(old) {
        match left.get_mut(line) {
            Some(n) if *n > 0 => *n -= 1,
            _ => out.push(line.to_string()),
        }
    }
    out
}

/// The text of one archive entry. `files` holds each file and its removed lines. A file with
/// no removed line is left out.
pub fn render_entry(
    title: &str,
    change: &str,
    commit: &str,
    reason: &str,
    files: &[(String, Vec<String>)],
) -> String {
    let one_line = |t: &str| t.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut out = format!(
        "## {title}\n\n- Change: {}\n- Commit: {commit}\n- Reason: {}\n",
        one_line(change),
        one_line(reason)
    );
    for (path, lines) in files {
        if lines.is_empty() {
            continue;
        }
        out.push_str(&format!("\nFrom {path}:\n\n"));
        for line in lines {
            out.push_str("> ");
            out.push_str(line);
            out.push('\n');
        }
    }
    out
}

/// The archive with one more entry at its end, after one blank line.
pub fn append_entry(archive: &str, entry: &str) -> String {
    if archive.trim().is_empty() {
        return entry.to_string();
    }
    let mut out = archive.trim_end_matches(['\n', '\r']).to_string();
    out.push_str("\n\n");
    out.push_str(entry);
    out
}

/// The text of the entry with this title, from its `## ` line to the next one.
pub fn entry_of(archive: &str, title: &str) -> Option<String> {
    let heading = format!("## {title}");
    let mut out: Option<Vec<&str>> = None;
    for line in archive.split('\n') {
        let bare = line.strip_suffix('\r').unwrap_or(line);
        if let Some(lines) = &mut out {
            if bare.starts_with("## ") {
                break;
            }
            lines.push(line);
        } else if bare.trim_end() == heading {
            out = Some(vec![line]);
        }
    }
    let lines = out?;
    let text = lines.join("\n");
    Some(format!("{}\n", text.trim_end_matches(['\n', '\r'])))
}

/// The archived lines of an archive: each `> ` line of an entry that names its commit and its
/// reason. A quote outside such an entry does not count.
pub fn archived_lines(archive: &str) -> Vec<String> {
    struct Entry {
        commit: bool,
        reason: bool,
        quotes: Vec<String>,
    }
    let mut entries: Vec<Entry> = Vec::new();
    for line in archive.split('\n') {
        let line = line.strip_suffix('\r').unwrap_or(line);
        if line.starts_with("## ") {
            entries.push(Entry {
                commit: false,
                reason: false,
                quotes: Vec::new(),
            });
            continue;
        }
        let Some(entry) = entries.last_mut() else {
            continue;
        };
        if let Some(rest) = line.strip_prefix("- Commit:") {
            entry.commit |= !rest.trim().is_empty();
        } else if let Some(rest) = line.strip_prefix("- Reason:") {
            entry.reason |= !rest.trim().is_empty();
        } else if let Some(quote) = line.strip_prefix("> ") {
            entry.quotes.push(quote.to_string());
        }
    }
    entries
        .into_iter()
        .filter(|e| e.commit && e.reason)
        .flat_map(|e| e.quotes)
        .filter(|q| !q.trim().is_empty())
        .collect()
}

/// The checker of "no line is lost". `before` holds the files before the proposal and `after`
/// the files of the proposal (path -> text). A file that is the same on both sides may be left
/// out of both. A line counts as kept when the file still has it (at any place), when a
/// different file of the proposal gained it (a moved line), or when a new entry of
/// `ARCHIVE.md` holds it word for word with its commit and reason. Each other line is lost.
pub fn lost_lines(
    before: &BTreeMap<String, String>,
    after: &BTreeMap<String, String>,
) -> Vec<LostLine> {
    let empty = String::new();
    // Lines that some file of the proposal gained, for the moved lines.
    let mut gained: BTreeMap<String, usize> = BTreeMap::new();
    for (path, text) in after {
        if path == PROPOSAL_FILE || path == ARCHIVE {
            continue;
        }
        let old = counts(before.get(path).unwrap_or(&empty));
        for (line, n) in counts(text) {
            let extra = n.saturating_sub(old.get(line).copied().unwrap_or(0));
            if extra > 0 {
                *gained.entry(line.to_string()).or_insert(0) += extra;
            }
        }
    }
    // Lines that the new entries of the archive hold.
    let mut archived: BTreeMap<String, usize> = BTreeMap::new();
    for line in archived_lines(after.get(ARCHIVE).unwrap_or(&empty)) {
        *archived.entry(line).or_insert(0) += 1;
    }
    for line in archived_lines(before.get(ARCHIVE).unwrap_or(&empty)) {
        if let Some(n) = archived.get_mut(&line) {
            *n = n.saturating_sub(1);
        }
    }
    let mut lost = Vec::new();
    for (path, text) in before {
        if path == PROPOSAL_FILE {
            continue;
        }
        let mut left = counts(after.get(path).unwrap_or(&empty));
        for (number, line) in counted_lines(text) {
            if let Some(n) = left.get_mut(line).filter(|n| **n > 0) {
                *n -= 1;
                continue;
            }
            if let Some(n) = gained.get_mut(line).filter(|n| **n > 0) {
                *n -= 1;
                continue;
            }
            if let Some(n) = archived.get_mut(line).filter(|n| **n > 0) {
                *n -= 1;
                continue;
            }
            lost.push(LostLine {
                path: path.clone(),
                line: number,
                text: line.to_string(),
            });
        }
    }
    lost
}

#[cfg(test)]
mod tests {
    use super::*;

    const P7: &str = "---\nid: P-007\nstatus: active\n---\n## Rule\nWhen X, make sure that Y.\n\n## Because\nZ.\n";

    fn files(items: &[(&str, &str)]) -> BTreeMap<String, String> {
        items
            .iter()
            .map(|(p, t)| (p.to_string(), t.to_string()))
            .collect()
    }

    fn entry(lines: &[&str]) -> String {
        render_entry(
            "proposal/2026-11-16-1 change 1",
            "edit principles/P-007.md",
            "9f3e21c",
            "The rule was too wide.",
            &[(
                "principles/P-007.md".into(),
                lines.iter().map(|l| l.to_string()).collect(),
            )],
        )
    }

    #[test]
    fn removed_lines_count_each_line_once_and_ignore_moves() {
        assert_eq!(removed_lines("a\nb\nc\n", "c\na\n"), ["b"]);
        assert_eq!(removed_lines("a\na\n\n", "a\n"), ["a"]);
        assert_eq!(removed_lines("a\r\nb\r\n", "a\nb\n"), Vec::<String>::new());
        assert!(removed_lines("", "x").is_empty());
    }

    #[test]
    fn a_removed_line_is_lost_until_the_archive_holds_it() {
        let after_text = P7.replace("## Because\nZ.\n", "## Because\n");
        let before = files(&[("principles/P-007.md", P7), ("ARCHIVE.md", "")]);
        let after = files(&[("principles/P-007.md", &after_text), ("ARCHIVE.md", "")]);
        assert_eq!(
            lost_lines(&before, &after),
            [LostLine {
                path: "principles/P-007.md".into(),
                line: 9,
                text: "Z.".into()
            }]
        );
        let archived = files(&[
            ("principles/P-007.md", &after_text),
            ("ARCHIVE.md", &entry(&["Z."])),
        ]);
        assert!(lost_lines(&before, &archived).is_empty());
    }

    #[test]
    fn a_rewritten_line_is_lost_until_the_archive_holds_the_old_text() {
        let after_text = P7.replace(
            "When X, make sure that Y.",
            "When X and W, make sure that Y.",
        );
        let before = files(&[("principles/P-007.md", P7)]);
        let after = files(&[("principles/P-007.md", &after_text)]);
        let lost = lost_lines(&before, &after);
        assert_eq!(lost.len(), 1);
        assert_eq!(lost[0].text, "When X, make sure that Y.");
        assert_eq!(lost[0].line, 6);
        // A near copy is not word for word.
        let near = files(&[
            ("principles/P-007.md", &after_text),
            ("ARCHIVE.md", &entry(&["When X, make sure that Y"])),
        ]);
        assert_eq!(lost_lines(&before, &near).len(), 1);
        let exact = files(&[
            ("principles/P-007.md", &after_text),
            ("ARCHIVE.md", &entry(&["When X, make sure that Y."])),
        ]);
        assert!(lost_lines(&before, &exact).is_empty());
    }

    #[test]
    fn a_moved_line_is_not_lost() {
        let reordered = "---\nstatus: active\nid: P-007\n---\n## Because\nZ.\n\n## Rule\nWhen X, make sure that Y.\n";
        let before = files(&[("principles/P-007.md", P7)]);
        assert!(lost_lines(&before, &files(&[("principles/P-007.md", reordered)])).is_empty());
        // A line that moves to a different file of the proposal is kept too.
        let before = files(&[(
            "principles/P-001.md",
            "---\nid: P-001\n---\n## Rule\nA.\n\n## Not when\nB.\n",
        )]);
        let after = files(&[
            (
                "principles/P-001.md",
                "---\nid: P-001\n---\n## Rule\nA.\n\n## Not when\n",
            ),
            (
                "principles/P-002.md",
                "---\nid: P-002\n---\n## Rule\nC.\n\n## Not when\nB.\n",
            ),
        ]);
        assert!(lost_lines(&before, &after).is_empty());
    }

    #[test]
    fn a_hand_edit_that_drops_a_line_is_caught() {
        // The builder archived the rewritten rule, but a hand edit then dropped the reason too.
        let after_text = "---\nid: P-007\nstatus: active\n---\n## Rule\nWhen X and W, make sure that Y.\n\n## Because\n";
        let before = files(&[("principles/P-007.md", P7), ("ARCHIVE.md", "")]);
        let after = files(&[
            ("principles/P-007.md", after_text),
            ("ARCHIVE.md", &entry(&["When X, make sure that Y."])),
            (PROPOSAL_FILE, "{}"),
        ]);
        let lost = lost_lines(&before, &after);
        assert_eq!(
            lost,
            [LostLine {
                path: "principles/P-007.md".into(),
                line: 9,
                text: "Z.".into()
            }]
        );
        // A hand edit that removes an old entry of the archive loses its lines.
        let old_archive = entry(&["old text"]);
        let before = files(&[("ARCHIVE.md", &old_archive)]);
        let after = files(&[("ARCHIVE.md", "")]);
        let lost = lost_lines(&before, &after);
        assert!(lost.iter().any(|l| l.text == "> old text"), "{lost:?}");
        // A quote with no commit or no reason proves nothing.
        let before = files(&[("principles/P-007.md", P7)]);
        let fake = "## hand made\n\n> Z.\n";
        let after = files(&[
            ("principles/P-007.md", &P7.replace("Z.\n", "")),
            ("ARCHIVE.md", fake),
        ]);
        assert_eq!(lost_lines(&before, &after).len(), 1);
        // A quote that the archive held before the proposal proves nothing new.
        let held = entry(&["Z."]);
        let before = files(&[("principles/P-007.md", P7), ("ARCHIVE.md", &held)]);
        let after = files(&[
            ("principles/P-007.md", &P7.replace("Z.\n", "")),
            ("ARCHIVE.md", &held),
        ]);
        assert_eq!(lost_lines(&before, &after).len(), 1);
    }

    #[test]
    fn a_removed_file_loses_each_line() {
        let before = files(&[("principles/P-007.md", P7)]);
        let after = files(&[]);
        assert_eq!(lost_lines(&before, &after).len(), 8);
    }

    #[test]
    fn entries_render_append_and_come_back() {
        let one = entry(&["Z.", "> quoted"]);
        assert_eq!(
            one,
            "## proposal/2026-11-16-1 change 1\n\n- Change: edit principles/P-007.md\n- Commit: 9f3e21c\n- Reason: The rule was too wide.\n\nFrom principles/P-007.md:\n\n> Z.\n> > quoted\n"
        );
        assert_eq!(archived_lines(&one), ["Z.", "> quoted"]);
        let two = render_entry(
            "proposal/2026-11-16-1 change 2",
            "retire P-001",
            "9f3e21c",
            "Wrong.\nNow.",
            &[],
        );
        let archive = append_entry(&append_entry("", &one), &two);
        assert!(
            archive.contains("> > quoted\n\n## proposal/2026-11-16-1 change 2\n"),
            "{archive}"
        );
        assert!(archive.contains("- Reason: Wrong. Now.\n"), "{archive}");
        assert_eq!(
            entry_of(&archive, "proposal/2026-11-16-1 change 1").unwrap(),
            one
        );
        assert_eq!(
            entry_of(&archive, "proposal/2026-11-16-1 change 2").unwrap(),
            two
        );
        assert_eq!(entry_of(&archive, "proposal/2026-11-16-1 change 3"), None);
    }
}
