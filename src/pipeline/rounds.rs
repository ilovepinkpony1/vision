//! The rounds of the gates of a turn (migration 7). When the end facts of a turn are tested,
//! each gate of its step that has a document gets one row in `gate_rounds`: its round in the
//! ticket, whether it passed, and the items that fail it. Each finding of a gate cr with a
//! place gets one row in `findings` with the source `cr`, and the rounds of the ticket follow
//! it in `finding_tracks` (see [`super::follow`]). A finding that came back
//! [`super::follow::NOTICE_AFTER`] times after its fix becomes a notice for the person.
//!
//! The tick writes each of these rows in the transaction of the end fact of the turn, so a
//! second pass over the same turn records nothing new.

use std::collections::BTreeMap;

use anyhow::Result;

use super::follow::{self, Seen, Track, TITLE_MAX};
use super::gates;
use crate::adapter::{Expect, GateRule, Step};
use crate::executor::Secrets;
use crate::ledger::notices::NoticeIssue;
use crate::ledger::pipeline::FindingRow;
use crate::ledger::review::GateRoundRow;
use crate::ledger::Ledger;

/// The source of the `findings` rows of a gate cr.
pub const CR_SOURCE: &str = "cr";

/// A notice of the findings of one document that keep coming back after their fix.
#[derive(Clone, Debug, PartialEq)]
pub struct BackNotice {
    pub doc: String,
    pub issues: Vec<NoticeIssue>,
    pub note: String,
}

/// The note of a notice of findings that keep coming back.
pub const BACK_NOTE: &str = "this finding keeps coming back after its fix";

/// The counts of a gate in one document: the items that fail it (`None` when the gate cannot
/// read each item), whether one of them is critical, and the number of items.
pub fn gate_count(rule: &GateRule, text: &str) -> (Option<i64>, bool, i64) {
    match rule {
        GateRule::Cr { net_max } => {
            let report = gates::cr_report(text);
            let failing: Vec<&gates::CrFinding> = report
                .findings
                .iter()
                .filter(|f| f.fails(*net_max))
                .collect();
            let readable = report.stray.is_empty()
                && report
                    .findings
                    .iter()
                    .all(|f| f.severity.is_some() && f.blocking.is_some() && f.net.is_some());
            let critical = failing
                .iter()
                .any(|f| f.severity.as_deref() == Some("CRITICAL"));
            (
                readable.then_some(failing.len() as i64),
                critical,
                report.findings.len() as i64,
            )
        }
        GateRule::Qa { block, .. } => {
            let report = gates::qa_report(text);
            let blocking: Vec<&gates::QaIssue> = report
                .issues
                .iter()
                .filter(|i| i.severity.is_some_and(|s| block.contains(&s)))
                .collect();
            let readable = report.issues.iter().all(|i| i.severity.is_some());
            let critical = blocking
                .iter()
                .any(|i| i.severity == Some(crate::adapter::Severity::Critical));
            (
                readable.then_some(blocking.len() as i64),
                critical,
                report.issues.len() as i64,
            )
        }
    }
}

/// Records the gates of one turn: a row of `gate_rounds` for each gate whose document the
/// result has, and the findings of each gate cr with their tracks. Gives the notices of the
/// findings that keep coming back. Each title is masked and cut to [`TITLE_MAX`] characters.
#[allow(clippy::too_many_arguments)]
pub fn record(
    ledger: &Ledger,
    ticket: &str,
    step: &Step,
    turn: &str,
    head: &str,
    docs: &BTreeMap<String, String>,
    secrets: &Secrets,
    now: &str,
) -> Result<Vec<BackNotice>> {
    let mut notices = Vec::new();
    for expect in &step.expects {
        let Expect::Gate(gate) = expect else {
            continue;
        };
        let Some(text) = docs.get(&gate.doc).filter(|t| !t.trim().is_empty()) else {
            continue;
        };
        if ledger.gate_round(turn, &gate.doc)?.is_some() {
            continue;
        }
        let kind = gate.kind();
        let round = ledger.next_gate_round(ticket, kind)?;
        let (failing, critical, findings) = gate_count(&gate.rule, text);
        ledger.add_gate_round(&GateRoundRow {
            turn: turn.to_string(),
            ticket: ticket.to_string(),
            step: step.id.clone(),
            gate: kind.to_string(),
            doc: gate.doc.clone(),
            round,
            head: Some(head.to_string()),
            passed: gates::judge(gate, text).is_ok(),
            failing,
            critical,
            findings,
            at: now.to_string(),
        })?;
        if let GateRule::Cr { .. } = gate.rule {
            let issues = follow_cr(ledger, ticket, turn, round, text, secrets, now)?;
            if !issues.is_empty() {
                notices.push(BackNotice {
                    doc: gate.doc.clone(),
                    issues,
                    note: BACK_NOTE.to_string(),
                });
            }
        }
    }
    Ok(notices)
}

/// The findings of one round of a gate cr: one `findings` row each, with its track. Gives the
/// issues of a notice for each finding that came back [`follow::NOTICE_AFTER`] times.
fn follow_cr(
    ledger: &Ledger,
    ticket: &str,
    turn: &str,
    round: i64,
    text: &str,
    secrets: &Secrets,
    now: &str,
) -> Result<Vec<NoticeIssue>> {
    let found: Vec<gates::CrFinding> = gates::cr_findings(text)
        .into_iter()
        .filter(|f| f.path.is_some())
        .collect();
    let rows = ledger.finding_tracks(ticket)?;
    let tracks: Vec<Track> = rows.iter().map(|r| r.track.clone()).collect();
    let seen: Vec<Seen> = found
        .iter()
        .map(|f| {
            let path = f.path.clone().unwrap_or_default();
            Seen {
                fingerprint: follow::fingerprint(&path, &f.title),
                path,
                line: f.at.map(i64::from),
            }
        })
        .collect();
    let followed = follow::follow(&tracks, round, &seen);
    let mut issues = Vec::new();
    for ((finding, seen), out) in found.iter().zip(&seen).zip(&followed) {
        let title = follow::cut_title(&secrets.mask(&finding.title), TITLE_MAX);
        let track = match out.track {
            Some(index) => {
                let old = &tracks[index];
                let noticed = out.notice.then_some(now);
                ledger.update_track(old.id, seen.line, round, out.times, noticed)?;
                old.id
            }
            None => ledger.add_track(
                ticket,
                &Track {
                    fingerprint: seen.fingerprint.clone(),
                    path: seen.path.clone(),
                    line: seen.line,
                    first_round: round,
                    last_round: round,
                    ..Track::default()
                },
                finding.severity.as_deref(),
                &title,
            )?,
        };
        if out.notice {
            let first = follow::cut_title(&secrets.mask(&finding.line), TITLE_MAX);
            issues.push(NoticeIssue {
                id: format!("F{}", finding.number),
                severity: finding
                    .severity
                    .as_deref()
                    .map(str::to_ascii_lowercase)
                    .unwrap_or_default(),
                title: if title.is_empty() {
                    first
                } else {
                    format!("{first}: {title}")
                },
            });
        }
        ledger.add_finding(&FindingRow {
            ticket: ticket.to_string(),
            round,
            fingerprint: seen.fingerprint.clone(),
            priority: finding.severity.as_deref().map(str::to_ascii_lowercase),
            source: CR_SOURCE.to_string(),
            verdict: "kept".to_string(),
            came_back: out.seen_before,
            path: Some(seen.path.clone()),
            line: seen.line,
            title: Some(title),
            turn: Some(turn.to_string()),
            track: Some(track),
            ..FindingRow::default()
        })?;
    }
    Ok(issues)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapter::{Gate, StepKind, TurnStep};
    use crate::turn::TurnKind;

    fn step() -> Step {
        Step {
            id: "review".into(),
            kind: StepKind::Turn(TurnStep {
                agent: "reviewer".into(),
                turn: TurnKind::Worker,
                reads: Vec::new(),
                with: Default::default(),
                findings: None,
                team: Vec::new(),
            }),
            next: Some("done".into()),
            on: BTreeMap::new(),
            cap: None,
            rounds: 2,
            extra_rounds: 0,
            fail: BTreeMap::new(),
            expects: vec![Expect::Gate(Gate {
                doc: "cr.md".into(),
                rule: GateRule::Cr { net_max: 4.0 },
            })],
            expect_args: BTreeMap::new(),
            after: Vec::new(),
            before: Vec::new(),
            start: Vec::new(),
            when: Vec::new(),
        }
    }

    fn report(items: &[(&str, &str, &str)]) -> BTreeMap<String, String> {
        let mut text = String::from("A review.\n\n");
        for (n, (place, tag, title)) in items.iter().enumerate() {
            text.push_str(&format!(
                "{}. [MAJOR][{tag}] {place}\n   {title}\n   Net: +6\n\n",
                n + 1
            ));
        }
        BTreeMap::from([("cr.md".to_string(), text)])
    }

    fn record_round(
        ledger: &Ledger,
        turn: &str,
        docs: &BTreeMap<String, String>,
    ) -> Vec<BackNotice> {
        record(
            ledger,
            "DEMO-12",
            &step(),
            turn,
            "abc",
            docs,
            &Secrets::from_values(vec![("TOKEN".into(), "sekret-value-123".into())]),
            "2026-11-15T10:00:00Z",
        )
        .unwrap()
    }

    #[test]
    fn the_counts_of_a_gate_name_the_failing_items_and_a_critical_one() {
        let cr = GateRule::Cr { net_max: 4.0 };
        let text = "1. [CRITICAL][FOLLOW-UP] a.rb:1\n  x\n  Net: +9\n2. [NIT][FOLLOW-UP] b.rb:2\n  y\n  Net: +1\n";
        assert_eq!(gate_count(&cr, text), (Some(1), true, 2));
        let unread = "1. [MAJOR][FOLLOW-UP] a.rb:1\n  x\n";
        assert_eq!(gate_count(&cr, unread), (None, false, 1));
        let qa = GateRule::Qa {
            block: vec![
                crate::adapter::Severity::Critical,
                crate::adapter::Severity::High,
            ],
            notify: false,
        };
        let text = "## Issues\n\n### ISSUE-001: Broken\n\n**Severity:** high\n\n### ISSUE-002: Typo\n\n**Severity:** low\n";
        assert_eq!(gate_count(&qa, text), (Some(1), false, 2));
    }

    #[test]
    fn a_finding_that_comes_back_two_times_after_its_fix_is_one_notice() {
        let ledger = Ledger::open_in_memory().unwrap();
        let with = report(&[("app/a.rb:40", "BLOCKING", "Retry sends the report twice")]);
        let without = report(&[]);
        assert!(record_round(&ledger, "T1", &with).is_empty());
        assert!(record_round(&ledger, "T2", &without).is_empty());
        // Back once, at a line two away and with a new title: the same finding.
        let back = report(&[("app/a.rb:42", "BLOCKING", "The retry sends it twice")]);
        assert!(record_round(&ledger, "T3", &back).is_empty());
        assert!(record_round(&ledger, "T4", &without).is_empty());
        let notices = record_round(&ledger, "T5", &with);
        assert_eq!(notices.len(), 1);
        assert_eq!(notices[0].doc, "cr.md");
        assert_eq!(notices[0].note, BACK_NOTE);
        assert_eq!(notices[0].issues[0].id, "F1");
        assert_eq!(notices[0].issues[0].severity, "major");
        assert!(
            notices[0].issues[0]
                .title
                .starts_with("1. [MAJOR][BLOCKING] app/a.rb:40: Retry sends"),
            "{:?}",
            notices[0].issues[0]
        );
        // A second pass over the same turn records nothing, and a later return gives no second
        // notice.
        assert!(record_round(&ledger, "T5", &with).is_empty());
        assert!(record_round(&ledger, "T6", &without).is_empty());
        assert!(record_round(&ledger, "T7", &with).is_empty());
        let tracks = ledger.finding_tracks("DEMO-12").unwrap();
        let a = tracks.iter().find(|t| t.track.path == "app/a.rb").unwrap();
        assert_eq!((a.track.first_round, a.track.last_round), (1, 7));
        assert_eq!(a.track.came_back, 3);
        assert!(a.track.noticed);
        let rows: Vec<FindingRow> = ledger
            .findings_of("DEMO-12")
            .unwrap()
            .into_iter()
            .filter(|f| f.source == CR_SOURCE)
            .collect();
        assert_eq!(rows.len(), 4);
        assert_eq!(rows[0].path.as_deref(), Some("app/a.rb"));
        assert_eq!(rows[0].line, Some(40));
        assert!(!rows[0].came_back);
        assert!(rows[1].came_back);
        assert_eq!(rows[1].line, Some(42));
        assert_eq!(rows[0].turn.as_deref(), Some("T1"));
        let rounds = ledger.gate_rounds_of("DEMO-12", "cr").unwrap();
        assert_eq!(rounds.len(), 7);
        assert_eq!(rounds[0].failing, Some(1));
        assert!(!rounds[0].passed);
        assert_eq!(rounds[1].failing, Some(0));
        assert!(rounds[1].passed);
    }

    #[test]
    fn a_title_is_masked_and_cut() {
        let ledger = Ledger::open_in_memory().unwrap();
        let long = format!("The token sekret-value-123 shows {}", "x".repeat(300));
        record_round(
            &ledger,
            "T1",
            &report(&[("app/a.rb:1", "FOLLOW-UP", &long)]),
        );
        let row = &ledger.findings_of("DEMO-12").unwrap()[0];
        let title = row.title.as_deref().unwrap();
        assert!(!title.contains("sekret-value-123"), "{title}");
        assert!(title.chars().count() <= TITLE_MAX, "{title}");
    }
}
