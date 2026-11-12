//! The gates of `expects` (section 11.2) as pure functions: a step names a document of its
//! turn, and code reads that document and decides. These are not the gates of the executor
//! (section 12).
//!
//! - The gate `cr` reads the report of a code review. Each finding is a numbered item whose
//!   first line is `N. [SEVERITY][BLOCKING|FOLLOW-UP] path:line`, and that has its own line
//!   `Net: +N`. The gate passes when no finding has a Net above `net_max` and no finding is
//!   tagged BLOCKING.
//! - The gate `qa` reads the report of a QA run. Each issue is a heading with an id such as
//!   `ISSUE-003`, and a field of its entry gives its severity. The gate passes when no issue has
//!   a severity of `block`. The count table of the report is only a check: when it disagrees
//!   with the issues, the gate trusts the issues and says so.
//!
//! A report that the gate cannot read fails the gate, and the text says what it could not read.

use std::collections::BTreeMap;

use crate::adapter::{Gate, GateRule, Severity};

/// The severities of a finding of a code review.
const CR_SEVERITIES: [&str; 4] = ["CRITICAL", "MAJOR", "MINOR", "NIT"];

/// What a gate found in a document: `Ok` with notes when it passes, `Err` with the text for
/// the agent when it fails. The text lists exactly what failed.
pub type Verdict = Result<Vec<String>, String>;

/// Judges the text of the document of a gate. The text is not empty: the caller tests a
/// missing or an empty document first.
pub fn judge(gate: &Gate, text: &str) -> Verdict {
    match &gate.rule {
        GateRule::Cr { net_max } => judge_cr(&gate.doc, text, *net_max),
        GateRule::Qa { block, .. } => judge_qa(&gate.doc, text, block),
    }
}

/// True when the document names at least one item that blocks: a finding of the gate cr that
/// is tagged BLOCKING or whose Net is above `net_max`, or an issue of the gate qa whose
/// severity is in `block`. The items that the gate cannot read do not count. A `fail` route of
/// a step goes only on such an item: a report that the gate cannot read goes back to its turn.
pub fn blocks(gate: &Gate, text: &str) -> bool {
    match &gate.rule {
        GateRule::Cr { net_max } => cr_report(text).findings.iter().any(|f| {
            f.header_readable()
                && (f.blocking == Some(true) || f.net.is_some_and(|net| net > *net_max))
        }),
        GateRule::Qa { block, .. } => qa_report(text)
            .issues
            .iter()
            .any(|i| i.severity.is_some_and(|s| block.contains(&s))),
    }
}

// ----- The gate cr ------------------------------------------------------------------------

/// One finding of a code review report.
#[derive(Clone, Debug, PartialEq)]
pub struct CrFinding {
    /// The number of the item, such as 7.
    pub number: u32,
    /// The first line of the item, trimmed, such as `7. [MAJOR][BLOCKING] app/order.rb:142`.
    pub line: String,
    /// `CRITICAL`, `MAJOR`, `MINOR`, or `NIT`; `None` when the first line names none.
    pub severity: Option<String>,
    /// True for `[BLOCKING]`, false for `[FOLLOW-UP]`; `None` when the first line names none.
    pub blocking: Option<bool>,
    /// The number of its first `Net:` line, such as 12.0 or -2.0.
    pub net: Option<f64>,
}

impl CrFinding {
    fn header_readable(&self) -> bool {
        self.severity.is_some() && self.blocking.is_some()
    }
}

/// What a code review report says.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct CrReport {
    /// The findings in their order.
    pub findings: Vec<CrFinding>,
    /// Each line `Net:` that belongs to no finding: one outside a finding, or a second one in a
    /// finding. Such a line shows a finding whose first line the gate cannot read.
    pub stray: Vec<String>,
}

/// Reads a code review report. A line that starts with a number, a dot, and `[` starts a
/// finding (a heading mark or a bold mark in front of it is allowed); the finding ends at the
/// next such line or at a heading.
pub fn cr_report(text: &str) -> CrReport {
    let mut report = CrReport::default();
    let mut open = false;
    for raw in text.lines() {
        let line = raw.trim();
        if let Some(finding) = cr_header(line) {
            report.findings.push(finding);
            open = true;
            continue;
        }
        if line.starts_with('#') {
            open = false;
            continue;
        }
        let Some(net) = net_of(line) else {
            continue;
        };
        match report.findings.last_mut() {
            Some(last) if open && last.net.is_none() => last.net = Some(net),
            _ => report.stray.push(line.to_string()),
        }
    }
    report
}

/// The findings of a code review report, in their order.
pub fn cr_findings(text: &str) -> Vec<CrFinding> {
    cr_report(text).findings
}

/// Reads the first line of a finding: `N. [SEVERITY][TAG] path:line`. `None` for a line that
/// does not start a finding.
fn cr_header(line: &str) -> Option<CrFinding> {
    let line = line.trim_start_matches(['#', '*', '_', ' ']);
    let digits: String = line.chars().take_while(char::is_ascii_digit).collect();
    let rest = line[digits.len()..].strip_prefix('.')?;
    let number: u32 = digits.parse().ok()?;
    let mut rest = rest.trim_start();
    if !rest.starts_with('[') {
        return None;
    }
    let mut tags = Vec::new();
    while let Some(inner) = rest.strip_prefix('[') {
        let Some(end) = inner.find(']') else {
            break;
        };
        tags.push(inner[..end].trim().to_ascii_uppercase());
        rest = inner[end + 1..].trim_start();
    }
    let severity = tags
        .iter()
        .find(|t| CR_SEVERITIES.contains(&t.as_str()))
        .cloned();
    let blocking = tags.iter().find_map(|t| match t.as_str() {
        "BLOCKING" => Some(true),
        "FOLLOW-UP" | "FOLLOWUP" | "FOLLOW UP" => Some(false),
        _ => None,
    });
    Some(CrFinding {
        number,
        line: line.trim_end_matches(['*', '_']).to_string(),
        severity,
        blocking,
        net: None,
    })
}

/// The number of a line `Net: +12`, `- Net: -2 (undo)`, or `**Net:** +4.5`. `None` for a line
/// that is not a Net line, or whose number cannot be read.
fn net_of(line: &str) -> Option<f64> {
    let line = line.trim_start_matches(['-', '*', '+', ' ', '\t']);
    let (label, value) = line.split_once(':')?;
    if !label.trim_matches(['*', ' ']).eq_ignore_ascii_case("net") {
        return None;
    }
    let value = value.trim_start_matches(['*', ' ', '\t']);
    let (negative, digits) = match value.chars().next()? {
        '+' => (false, &value[1..]),
        '-' => (true, &value[1..]),
        '\u{2212}' => (true, &value['\u{2212}'.len_utf8()..]),
        _ => (false, value),
    };
    let digits = digits.trim_start();
    let end = digits
        .char_indices()
        .find(|(_, c)| !(c.is_ascii_digit() || *c == '.'))
        .map_or(digits.len(), |(i, _)| i);
    let number: f64 = digits[..end].trim_end_matches('.').parse().ok()?;
    Some(if negative { -number } else { number })
}

/// A Net as the report writes it: always with its sign.
fn signed(net: f64) -> String {
    format!("{net:+}")
}

fn judge_cr(doc: &str, text: &str, net_max: f64) -> Verdict {
    let report = cr_report(text);
    let mut failing = Vec::new();
    let mut unreadable = Vec::new();
    for line in &report.stray {
        unreadable.push(format!(
            "the line `{line}` belongs to no finding: it stands outside a numbered finding, or a finding has two such lines. Give each finding its own first line `N. [SEVERITY][BLOCKING|FOLLOW-UP] path:line`"
        ));
    }
    for finding in &report.findings {
        let first = format!("finding {} `{}`", finding.number, finding.line);
        if !finding.header_readable() {
            unreadable.push(format!(
                "{first} has a first line that names no severity (CRITICAL, MAJOR, MINOR, or NIT) or no tag (BLOCKING or FOLLOW-UP)"
            ));
            continue;
        }
        let Some(net) = finding.net else {
            unreadable.push(format!("{first} has no readable line `Net: +N`"));
            continue;
        };
        let mut why = Vec::new();
        if finding.blocking == Some(true) {
            why.push("tagged BLOCKING".to_string());
        }
        if net > net_max {
            why.push(format!("Net above {}", signed(net_max)));
        }
        if !why.is_empty() {
            failing.push(format!(
                "{first} with Net {} ({})",
                signed(net),
                why.join(", ")
            ));
        }
    }
    if failing.is_empty() && unreadable.is_empty() {
        return Ok(Vec::new());
    }
    let mut parts = Vec::new();
    if !failing.is_empty() {
        parts.push(format!(
            "the gate cr passes when no finding of {doc} has a Net above {} and no finding is tagged BLOCKING, but {} {} it: {}",
            signed(net_max),
            failing.len(),
            if failing.len() == 1 { "finding fails" } else { "findings fail" },
            failing.join("; ")
        ));
    }
    if !unreadable.is_empty() {
        parts.push(format!(
            "the gate cr cannot read each finding of {doc}: {}",
            unreadable.join("; ")
        ));
    }
    Err(parts.join(". "))
}

// ----- The gate qa ------------------------------------------------------------------------

/// One issue of a QA report.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct QaIssue {
    /// The id, such as `ISSUE-003`.
    pub id: String,
    /// The title of its heading after the id, such as `The total shows no currency`.
    pub title: String,
    /// The severity of its entry; `None` when no field of the entry gives one.
    pub severity: Option<Severity>,
}

/// What a QA report says.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct QaReport {
    /// The issues in their order, one for each id.
    pub issues: Vec<QaIssue>,
    /// The count table: severity -> count, when the report has one.
    pub table: Option<BTreeMap<Severity, u32>>,
    /// The report has a section of issues or findings, also an empty one.
    pub section: bool,
}

impl QaReport {
    /// The number of issues of each severity, from the issues.
    fn counts(&self) -> BTreeMap<Severity, u32> {
        let mut counts: BTreeMap<Severity, u32> = Severity::ALL.iter().map(|s| (*s, 0)).collect();
        for severity in self.issues.iter().filter_map(|i| i.severity) {
            *counts.entry(severity).or_default() += 1;
        }
        counts
    }

    /// The note when the count table disagrees with the issues, else `None`.
    pub fn disagreement(&self) -> Option<String> {
        let table = self.table.as_ref()?;
        let table: BTreeMap<Severity, u32> = Severity::ALL
            .iter()
            .map(|s| (*s, table.get(s).copied().unwrap_or(0)))
            .collect();
        let counts = self.counts();
        if table == counts {
            return None;
        }
        let list = |map: &BTreeMap<Severity, u32>| {
            map.iter()
                .map(|(s, n)| format!("{n} {s}"))
                .collect::<Vec<_>>()
                .join(", ")
        };
        Some(format!(
            "the count table says {}, but the issues say {}. The gate trusts the issues",
            list(&table),
            list(&counts)
        ))
    }
}

/// Reads a QA report: each heading with an id `ISSUE-<digits>` starts the entry of an issue,
/// which ends at the next heading. The first field of the entry whose label ends with
/// `severity` gives the severity, in a table row (`| **Severity** | high |`) or a line
/// (`- Classification / severity: product defect; high`). Two headings with the same id are one
/// issue. A heading inside a fenced code block does not count.
///
/// When the report has a section `Issues` or `Findings`, only a heading in it starts a new
/// issue. A heading of the same id in a later section, such as the evidence before and after a
/// fix, adds to that issue and starts none.
pub fn qa_report(text: &str) -> QaReport {
    let headings = headings(text);
    let mut report = QaReport {
        section: headings.iter().any(|(_, _, name)| section_name(name)),
        ..QaReport::default()
    };
    let mut section: Option<usize> = None;
    let mut current: Option<usize> = None;
    let mut table: BTreeMap<Severity, u32> = BTreeMap::new();
    let mut next_heading = headings.iter().peekable();
    for (number, raw) in text.lines().enumerate() {
        if let Some((_, level, heading)) = next_heading.next_if(|(at, _, _)| *at == number) {
            current = None;
            if section.is_some_and(|l| *level <= l) {
                section = None;
            }
            if section_name(heading) {
                section = Some(*level);
                continue;
            }
            let Some((id, title)) = issue_heading(heading) else {
                continue;
            };
            let known = report.issues.iter().position(|i| i.id == id);
            current = match known {
                Some(at) => Some(at),
                None if section.is_some() || !report.section => {
                    report.issues.push(QaIssue {
                        id,
                        title,
                        severity: None,
                    });
                    Some(report.issues.len() - 1)
                }
                None => None,
            };
            continue;
        }
        let line = raw.trim();
        if let Some((severity, count)) = count_row(line) {
            table.entry(severity).or_insert(count);
            continue;
        }
        let Some(at) = current else {
            continue;
        };
        if let Some(found) = severity_field(line) {
            let issue = &mut report.issues[at];
            // Two entries of one id that disagree: the more severe one counts.
            issue.severity = Some(issue.severity.map_or(found, |s| s.min(found)));
        }
    }
    if !table.is_empty() {
        report.table = Some(table);
    }
    report
}

/// The headings of a Markdown text outside fenced code blocks: the line number, the level,
/// and the text. A line that starts with a bold issue id, such as `**ISSUE-003: A title**`,
/// counts as a heading below each other one.
fn headings(text: &str) -> Vec<(usize, usize, String)> {
    let mut out = Vec::new();
    let mut fenced = false;
    for (number, raw) in text.lines().enumerate() {
        let line = raw.trim();
        if line.starts_with("```") || line.starts_with("~~~") {
            fenced = !fenced;
            continue;
        }
        if fenced {
            continue;
        }
        if line
            .get(..8)
            .is_some_and(|start| start.eq_ignore_ascii_case("**ISSUE-"))
        {
            out.push((number, 7, line.trim_matches('*').trim().to_string()));
            continue;
        }
        if !line.starts_with('#') {
            continue;
        }
        let level = line.chars().take_while(|c| *c == '#').count();
        out.push((number, level, line[level..].trim().to_string()));
    }
    out
}

/// True for the heading of the section of the issues: `Issues` or `Findings`.
fn section_name(heading: &str) -> bool {
    let name = heading.trim_matches(['*', ' ']).to_ascii_lowercase();
    name == "issues" || name == "findings"
}

/// The id and the title of a heading such as `ISSUE-003: The total shows no currency`.
fn issue_heading(heading: &str) -> Option<(String, String)> {
    let upper = heading.to_ascii_uppercase();
    let start = upper.find("ISSUE-")?;
    let digits: String = upper[start + 6..]
        .chars()
        .take_while(char::is_ascii_digit)
        .collect();
    if digits.is_empty() {
        return None;
    }
    let end = start + 6 + digits.len();
    let title = heading[end..]
        .trim_start_matches([':', '-', ' ', '\u{2014}', '\u{2013}', '*'])
        .trim_end_matches('*')
        .trim()
        .to_string();
    Some((format!("ISSUE-{digits}"), title))
}

/// A row of the count table: `| Critical | 0 |` or `| **High** | 2 |`.
fn count_row(line: &str) -> Option<(Severity, u32)> {
    let cells = cells(line)?;
    let severity = Severity::parse(cells.first()?.trim_matches('*').trim())?;
    let count = cells.get(1)?.trim_matches('*').trim().parse().ok()?;
    Some((severity, count))
}

/// The cells of a table row, or `None` for a line that is not one.
fn cells(line: &str) -> Option<Vec<&str>> {
    let inner = line.strip_prefix('|')?;
    let inner = inner.strip_suffix('|').unwrap_or(inner);
    Some(inner.split('|').map(str::trim).collect())
}

/// The severity of a field whose label ends with `severity`. With more than one severity word
/// in the value, the most severe one counts.
fn severity_field(line: &str) -> Option<Severity> {
    let (label, value) = match cells(line) {
        Some(cells) => (cells.first().copied()?, cells.get(1).copied()?),
        None => line
            .trim_start_matches(['-', '*', '+', ' ', '\t'])
            .split_once(':')?,
    };
    let label = label.trim_matches(['*', '_', ' ']).to_ascii_lowercase();
    if !label.ends_with("severity") {
        return None;
    }
    value
        .split(|c: char| !c.is_ascii_alphabetic())
        .filter_map(Severity::parse)
        .min()
}

fn judge_qa(doc: &str, text: &str, block: &[Severity]) -> Verdict {
    let report = qa_report(text);
    let mut notes = Vec::new();
    if let Some(note) = report.disagreement() {
        notes.push(note);
    }
    if report.issues.is_empty() && report.table.is_none() && !report.section {
        return Err(format!(
            "the gate qa reads no severity in {doc}: it has no issue with an id such as ISSUE-001, no count table of severities, and no section Issues or Findings"
        ));
    }
    let unreadable: Vec<String> = report
        .issues
        .iter()
        .filter(|i| i.severity.is_none())
        .map(|i| format!("{} `{}`", i.id, i.title))
        .collect();
    let blocking: Vec<String> = report
        .issues
        .iter()
        .filter(|i| i.severity.is_some_and(|s| block.contains(&s)))
        .map(|i| {
            format!(
                "{} {} `{}`",
                i.id,
                i.severity.map_or("", Severity::as_str),
                i.title
            )
        })
        .collect();
    // The count table counts more blocking issues than the gate read: an entry that the parser
    // does not know hides a blocking issue, so the gate fails and never passes on a guess.
    let hidden: Vec<String> = match &report.table {
        Some(table) => block
            .iter()
            .filter_map(|s| {
                let counted = table.get(s).copied().unwrap_or(0);
                let read = report
                    .issues
                    .iter()
                    .filter(|i| i.severity == Some(*s))
                    .count() as u32;
                (counted > read).then(|| format!("{} {}", counted - read, s.as_str()))
            })
            .collect(),
        None => Vec::new(),
    };
    if unreadable.is_empty() && blocking.is_empty() && hidden.is_empty() {
        return Ok(notes);
    }
    let mut parts = Vec::new();
    if !hidden.is_empty() {
        parts.push(format!(
            "the count table of {doc} counts blocking issues that the gate cannot read as entries: {}. Write each issue with its id and its severity",
            hidden.join(", ")
        ));
    }
    if !blocking.is_empty() {
        let words: Vec<&str> = block.iter().map(|s| s.as_str()).collect();
        parts.push(format!(
            "the gate qa passes when no issue of {doc} has a severity of {}, but {} {}: {}",
            words.join(", "),
            blocking.len(),
            if blocking.len() == 1 {
                "issue has one"
            } else {
                "issues have one"
            },
            blocking.join("; ")
        ));
    }
    if !unreadable.is_empty() {
        parts.push(format!(
            "the gate qa reads no severity for {}: {}. Give each issue a field Severity with critical, high, medium, or low",
            if unreadable.len() == 1 { "one issue".to_string() } else { format!("{} issues", unreadable.len()) },
            unreadable.join("; ")
        ));
    }
    parts.extend(notes);
    Err(parts.join(". "))
}

/// The issues of a QA report that passed its gate and that do not block: the ones that a
/// notice lists. Each one has a severity, because a report with an unreadable severity fails.
pub fn notice_issues(block: &[Severity], text: &str) -> Vec<QaIssue> {
    qa_report(text)
        .issues
        .into_iter()
        .filter(|i| i.severity.is_some_and(|s| !block.contains(&s)))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cr(net_max: f64) -> Gate {
        Gate {
            doc: "cr.md".into(),
            rule: GateRule::Cr { net_max },
        }
    }

    fn qa() -> Gate {
        Gate {
            doc: "qa.md".into(),
            rule: GateRule::Qa {
                block: crate::adapter::QA_BLOCK.to_vec(),
                notify: true,
            },
        }
    }

    fn finding(n: u32, tags: &str, net: &str) -> String {
        format!(
            "{n}. {tags} app/models/order.rb:{n}\n   A text.\n   Fix: a fix.\n   Pros: +5 a gain\n   Cons: -1 a cost\n   {net}\n"
        )
    }

    #[test]
    fn each_severity_and_each_tag_reads() {
        for severity in CR_SEVERITIES {
            for (tag, blocking) in [("BLOCKING", true), ("FOLLOW-UP", false)] {
                let text = finding(1, &format!("[{severity}][{tag}]"), "Net: +1");
                let found = cr_findings(&text);
                assert_eq!(found.len(), 1, "{text}");
                assert_eq!(found[0].severity.as_deref(), Some(severity));
                assert_eq!(found[0].blocking, Some(blocking));
                assert_eq!(found[0].net, Some(1.0));
                assert_eq!(judge(&cr(4.0), &text).is_ok(), !blocking, "{text}");
            }
        }
    }

    #[test]
    fn only_a_readable_blocking_item_blocks() {
        // A finding that fails the gate blocks.
        let tagged = finding(1, "[MAJOR][BLOCKING]", "Net: +1");
        assert!(blocks(&cr(4.0), &tagged));
        let high = finding(1, "[MINOR][FOLLOW-UP]", "Net: +5");
        assert!(blocks(&cr(4.0), &high));
        // A report that passes, or one that the gate only cannot read, does not block.
        assert!(!blocks(
            &cr(4.0),
            &finding(1, "[MINOR][FOLLOW-UP]", "Net: +4")
        ));
        assert!(!blocks(
            &cr(4.0),
            &finding(1, "[MINOR][FOLLOW-UP]", "no net")
        ));
        assert!(!blocks(&cr(4.0), &finding(1, "[MINOR]", "Net: +9")));
        // A blocking finding still blocks next to one that the gate cannot read.
        let mixed = format!(
            "{}{}",
            finding(1, "[MINOR]", "Net: +9"),
            tagged.replace("1. ", "2. ")
        );
        assert!(judge(&cr(4.0), &mixed).is_err());
        assert!(blocks(&cr(4.0), &mixed));
        // An issue of a blocking severity blocks; a low one, or one with no severity, does not.
        let issue = |severity: &str| {
            format!("## Issues\n\n### ISSUE-001: The total is wrong\n\n| Field | Value |\n|---|---|\n| **Severity** | {severity} |\n")
        };
        assert!(blocks(&qa(), &issue("medium")));
        assert!(!blocks(&qa(), &issue("low")));
        assert!(!blocks(&qa(), &issue("unknown")));
        assert!(judge(&qa(), &issue("unknown")).is_err());
    }

    #[test]
    fn a_net_of_4_passes_and_a_net_of_5_fails() {
        let four = finding(1, "[MINOR][FOLLOW-UP]", "Net: +4");
        assert_eq!(judge(&cr(4.0), &four), Ok(Vec::new()));
        let five = finding(2, "[MINOR][FOLLOW-UP]", "Net: +5");
        let error = judge(&cr(4.0), &five).unwrap_err();
        assert!(
            error.contains("finding 2 `2. [MINOR][FOLLOW-UP] app/models/order.rb:2` with Net +5 (Net above +4)"),
            "{error}"
        );
        assert!(judge(&cr(5.0), &five).is_ok());
    }

    #[test]
    fn decimals_negatives_and_text_after_the_number_read() {
        for (line, net) in [
            ("Net: +4.5", 4.5),
            ("Net: -2 (undo)", -2.0),
            ("Net: -0.5", -0.5),
            ("Net: +12", 12.0),
            ("Net: 3", 3.0),
            ("**Net:** +7", 7.0),
            ("- Net: +1.5 because", 1.5),
            ("Net: \u{2212}3", -3.0),
        ] {
            assert_eq!(net_of(line), Some(net), "{line}");
        }
        for line in ["Net: high", "Pros: +3 net gain", "Network: +3", "Net:"] {
            assert_eq!(net_of(line), None, "{line}");
        }
        let half = finding(1, "[MINOR][FOLLOW-UP]", "Net: +4.5");
        let error = judge(&cr(4.0), &half).unwrap_err();
        assert!(error.contains("with Net +4.5"), "{error}");
        let undo = finding(1, "[MAJOR][FOLLOW-UP]", "Net: -2 (undo)");
        assert!(judge(&cr(4.0), &undo).is_ok());
    }

    #[test]
    fn a_finding_with_no_net_line_fails_and_is_named() {
        let text = format!(
            "{}2. [MINOR][FOLLOW-UP] app/x.rb:9\n   No net here.\n\nSkipped 1:\napp/y.rb:3 — a rename — Net: +2\n",
            finding(1, "[NIT][FOLLOW-UP]", "Net: +1")
        );
        let found = cr_findings(&text);
        assert_eq!(found[1].net, None, "a dropped item never lends its Net");
        let error = judge(&cr(4.0), &text).unwrap_err();
        assert!(
            error.contains(
                "finding 2 `2. [MINOR][FOLLOW-UP] app/x.rb:9` has no readable line `Net: +N`"
            ),
            "{error}"
        );
        assert!(!error.contains("finding 1"), "{error}");
    }

    #[test]
    fn an_unreadable_first_line_fails() {
        let text = finding(3, "[MAJR]", "Net: +1");
        let error = judge(&cr(4.0), &text).unwrap_err();
        assert!(
            error.contains("finding 3 `3. [MAJR] app/models/order.rb:3`"),
            "{error}"
        );
        assert!(error.contains("names no severity"), "{error}");
    }

    #[test]
    fn a_net_line_that_belongs_to_no_finding_fails() {
        // A finding whose first line has no number joins the finding before it.
        let text = format!(
            "{}- [MAJOR][BLOCKING] app/b.rb:2\n   A text.\n   Net: +12\n",
            finding(1, "[MINOR][FOLLOW-UP]", "Net: +2")
        );
        let report = cr_report(&text);
        assert_eq!(report.findings.len(), 1);
        assert_eq!(report.stray, ["Net: +12"]);
        let error = judge(&cr(4.0), &text).unwrap_err();
        assert!(
            error.contains("the line `Net: +12` belongs to no finding"),
            "{error}"
        );
        let error = judge(&cr(4.0), "A summary.\n\nNet: +3\n").unwrap_err();
        assert!(error.contains("belongs to no finding"), "{error}");
        // A heading mark or a bold mark in front of the first line is allowed.
        for header in [
            "### 1. [MAJOR][BLOCKING] app/a.rb:1",
            "**1. [MAJOR][BLOCKING] app/a.rb:1**",
        ] {
            let text = format!("{header}\nA text.\nNet: +12\n");
            let found = cr_findings(&text);
            assert_eq!(found.len(), 1, "{header}");
            assert_eq!(found[0].line, "1. [MAJOR][BLOCKING] app/a.rb:1");
            assert_eq!(found[0].net, Some(12.0), "{header}");
        }
    }

    #[test]
    fn an_empty_list_of_findings_passes() {
        let text = "# Code review\n\nNo finding passed the bar. Skipped 2:\napp/a.rb:1 — a name — Net: +1\n";
        assert!(cr_findings(text).is_empty());
        assert_eq!(judge(&cr(4.0), text), Ok(Vec::new()));
    }

    #[test]
    fn the_message_names_each_failing_finding() {
        let text = format!(
            "{}{}{}",
            finding(1, "[MAJOR][BLOCKING]", "Net: +12"),
            finding(2, "[MINOR][FOLLOW-UP]", "Net: +3"),
            finding(3, "[MINOR][FOLLOW-UP]", "Net: +6")
        );
        let error = judge(&cr(4.0), &text).unwrap_err();
        assert!(error.contains("2 findings fail it"), "{error}");
        assert!(
            error.contains("finding 1 `1. [MAJOR][BLOCKING] app/models/order.rb:1` with Net +12 (tagged BLOCKING, Net above +4)"),
            "{error}"
        );
        assert!(error.contains("finding 3 "), "{error}");
        assert!(!error.contains("finding 2 "), "{error}");
    }

    fn issue(n: u32, severity: &str, title: &str) -> String {
        format!(
            "### ISSUE-{n:03}: {title}\n\n| Field | Value |\n|-------|-------|\n| **Severity** | {severity} |\n| **Category** | visual |\n\n**Description:** It is wrong.\n\n---\n\n"
        )
    }

    fn report(counts: [u32; 4], issues: &str) -> String {
        format!(
            "# QA Report: Example\n\n## Health Score: 88/100\n\n## Summary\n\n| Severity | Count |\n|----------|-------|\n| Critical | {} |\n| High | {} |\n| Medium | {} |\n| Low | {} |\n| **Total** | **{}** |\n\n## Issues\n\n{issues}## Fixes Applied\n\n#### ISSUE-001: Before and after\n**Before:** a picture\n",
            counts[0],
            counts[1],
            counts[2],
            counts[3],
            counts.iter().sum::<u32>()
        )
    }

    #[test]
    fn a_report_with_only_low_issues_passes_and_gives_them_to_the_notice() {
        let text = report(
            [0, 0, 0, 2],
            &format!(
                "{}{}",
                issue(1, "low", "The footer has a typo"),
                issue(2, "low", "A hover color differs")
            ),
        );
        assert_eq!(judge(&qa(), &text), Ok(Vec::new()));
        let low = notice_issues(&crate::adapter::QA_BLOCK, &text);
        assert_eq!(
            low,
            vec![
                QaIssue {
                    id: "ISSUE-001".into(),
                    title: "The footer has a typo".into(),
                    severity: Some(Severity::Low)
                },
                QaIssue {
                    id: "ISSUE-002".into(),
                    title: "A hover color differs".into(),
                    severity: Some(Severity::Low)
                },
            ]
        );
    }

    #[test]
    fn one_medium_issue_fails() {
        let text = report(
            [0, 0, 1, 1],
            &format!(
                "{}{}",
                issue(1, "medium", "The export is slow"),
                issue(2, "low", "A typo")
            ),
        );
        let error = judge(&qa(), &text).unwrap_err();
        assert!(
            error.contains("1 issue has one: ISSUE-001 medium `The export is slow`"),
            "{error}"
        );
        assert!(!error.contains("ISSUE-002"), "{error}");
    }

    #[test]
    fn each_severity_blocks_only_when_block_names_it() {
        for severity in Severity::ALL {
            let text = report([0; 4], &issue(1, severity.as_str(), "A thing"));
            let blocks = crate::adapter::QA_BLOCK.contains(&severity);
            let verdict = judge(&qa(), &text);
            assert_eq!(verdict.is_err(), blocks, "{severity}: {verdict:?}");
        }
    }

    #[test]
    fn a_report_with_no_issue_passes() {
        let text = report([0; 4], "No issue was found.\n\n");
        assert_eq!(judge(&qa(), &text), Ok(Vec::new()));
        assert!(notice_issues(&crate::adapter::QA_BLOCK, &text).is_empty());
    }

    #[test]
    fn a_report_with_no_severity_at_all_fails() {
        let error = judge(&qa(), "QA could not start: the sign-in failed.\n").unwrap_err();
        assert!(error.contains("reads no severity in qa.md"), "{error}");
        let text = "## Issues\n\n### ISSUE-004: A dead button\n\nIt does nothing.\n";
        let error = judge(&qa(), text).unwrap_err();
        assert!(
            error.contains("reads no severity for one issue: ISSUE-004 `A dead button`"),
            "{error}"
        );
    }

    #[test]
    fn the_issues_win_over_the_count_table_and_the_note_says_so() {
        let text = report([0, 0, 0, 0], &issue(1, "high", "Checkout does nothing"));
        let error = judge(&qa(), &text).unwrap_err();
        assert!(
            error.contains("ISSUE-001 high `Checkout does nothing`"),
            "{error}"
        );
        assert!(
            error.contains("the count table says 0 critical, 0 high, 0 medium, 0 low, but the issues say 0 critical, 1 high, 0 medium, 0 low. The gate trusts the issues"),
            "{error}"
        );
        // The table counts a high issue that no entry shows: the gate never passes on a guess.
        let text = report([0, 1, 0, 1], &issue(1, "low", "A typo"));
        let error = judge(&qa(), &text).unwrap_err();
        assert!(
            error.contains("counts blocking issues that the gate cannot read as entries: 1 high"),
            "{error}"
        );
        // The table counts more low issues than the entries: low never blocks, so only a note.
        let text = report([0, 0, 0, 2], &issue(1, "low", "A typo"));
        let notes = judge(&qa(), &text).unwrap();
        assert_eq!(notes.len(), 1, "{notes:?}");
        assert!(notes[0].contains("The gate trusts the issues"), "{notes:?}");
    }

    #[test]
    fn a_report_with_text_of_any_script_reads() {
        let text = "## Issues\n\nÉté — ünïcode\n\n### ISSUE-001: Кнопка не працює\n\n| **Severity** | low |\n\n1. [NIT][FOLLOW-UP] доступ.rb:1\n   Net: +1\n";
        assert_eq!(qa_report(text).issues[0].title, "Кнопка не працює");
        assert_eq!(cr_findings(text).len(), 1);
    }

    #[test]
    fn the_sample_reports_of_the_fixtures_read() {
        let fails = include_str!("../../tests/fixtures/gates/cr-fails.md");
        let found = cr_findings(fails);
        assert_eq!(found.len(), 4, "the skipped items are no findings");
        let nets: Vec<Option<f64>> = found.iter().map(|f| f.net).collect();
        assert_eq!(nets, [Some(11.0), Some(2.0), Some(5.5), Some(1.0)]);
        let error = judge(&cr(4.0), fails).unwrap_err();
        assert!(error.contains("2 findings fail it"), "{error}");
        assert!(
            error.contains("finding 1 `1. [MAJOR][BLOCKING] app/models/report_export.rb:42` with Net +11 (tagged BLOCKING, Net above +4)"),
            "{error}"
        );
        assert!(
            error.contains("finding 3 `3. [MINOR][FOLLOW-UP] spec/models/report_export_spec.rb:8` with Net +5.5 (Net above +4)"),
            "{error}"
        );
        let passes = include_str!("../../tests/fixtures/gates/cr-passes.md");
        assert_eq!(judge(&cr(4.0), passes), Ok(Vec::new()));

        let low = include_str!("../../tests/fixtures/gates/qa-low.md");
        assert_eq!(judge(&qa(), low), Ok(Vec::new()));
        let issues = notice_issues(&crate::adapter::QA_BLOCK, low);
        let ids: Vec<&str> = issues.iter().map(|i| i.id.as_str()).collect();
        assert_eq!(ids, ["ISSUE-001", "ISSUE-002"]);
        assert_eq!(issues[0].title, "The export button has a typo");
        let medium = include_str!("../../tests/fixtures/gates/qa-medium.md");
        let error = judge(&qa(), medium).unwrap_err();
        assert!(
            error.contains("ISSUE-003 medium `The export of a large report takes 12 seconds`"),
            "{error}"
        );
        assert!(!error.contains("count table"), "{error}");
        let clean = include_str!("../../tests/fixtures/gates/qa-clean.md");
        assert_eq!(judge(&qa(), clean), Ok(Vec::new()));
        assert!(notice_issues(&crate::adapter::QA_BLOCK, clean).is_empty());
    }

    #[test]
    fn the_functional_form_and_a_fenced_heading_read() {
        let text = "# Functional QA Report: example\n\n## Findings\n\n### ISSUE-007: The import drops a row\n\n- Classification / severity: PRODUCT DEFECT; high, the row is lost.\n- Observed: one row is missing.\n\n```sh\n# ISSUE-099: not a heading\nrun --import\n```\n";
        let parsed = qa_report(text);
        assert_eq!(parsed.issues.len(), 1, "{parsed:?}");
        assert_eq!(parsed.issues[0].severity, Some(Severity::High));
        assert!(judge(&qa(), text).is_err());
        // An entry whose first line is a bold id, and a list of the top issues that is no entry.
        let text = "## Top 3 Things to Fix\n\n1. **ISSUE-001: A dead button** — it does nothing.\n\n## Issues\n\n**ISSUE-001: A dead button**\n\n- **Severity:** high\n";
        let parsed = qa_report(text);
        assert_eq!(parsed.issues.len(), 1, "{parsed:?}");
        assert_eq!(parsed.issues[0].title, "A dead button");
        assert_eq!(parsed.issues[0].severity, Some(Severity::High));
    }
}
