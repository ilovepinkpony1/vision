//! The escape check (section 11.6): what the review bot of the team found on our pull request
//! that our own review missed. It gives a number for the quality of our reviewer, and lessons
//! for the memory.
//!
//! When a ticket has a pull request and our gate cr passed at a head, the tick looks for the
//! first review of each reviewer of `[escapes]` that was submitted after that pass. The executor
//! reads it with the read verb `escapes.verb` of the adapter, at most once each `escapes.every`
//! seconds while it is missing. Once it exists, the check reads its inline comments one time:
//! each comment of a counted priority is matched against each finding that the gate cr
//! documents in each round of the ticket, on the same path within `escapes.lines` lines. A
//! comment with no match is an escape: a row of `escapes`, a row of `findings` with the source
//! `escape`, and a note in the journal entry of the turn whose gate passed, so that MemPalace
//! and later turns find it.
//!
//! The check stores no comment text beyond its title: the first bold line, masked and cut to
//! 200 characters.

use anyhow::{Context, Result};
use serde_json::{json, Value};

use crate::config::Escapes;
use crate::ctx::Ctx;
use crate::executor::Secrets;
use crate::ledger::pipeline::FindingRow;
use crate::ledger::review::{EscapeCheckRow, NewEscape};
use crate::ledger::{Event, Ledger};
use crate::paths::TicketKey;
use crate::pipeline::follow::{self, TITLE_MAX};
use crate::pipeline::rounds::CR_SOURCE;
use crate::pipeline::state::Counts;
use crate::tick::Pass;
use crate::util::regex::Regex;
use crate::util::time;

/// The source of the `findings` rows of an escape.
pub const ESCAPE_SOURCE: &str = "escape";

/// One inline comment of the review of a review bot.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Comment {
    pub path: Option<String>,
    pub line: Option<i64>,
    pub url: Option<String>,
    /// The text of the comment: untrusted data, never stored.
    pub body: String,
}

/// The review of a review bot, as the read verb gives it.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Review {
    pub id: i64,
    pub submitted: Option<String>,
    pub comments: Vec<Comment>,
}

/// A finding of our review: its place.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Ours {
    pub path: String,
    pub line: Option<i64>,
}

/// What the check found in one review.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Outcome {
    /// The comments of a counted priority.
    pub counted: i64,
    /// The comments of a counted priority that no finding of ours matched.
    pub escapes: Vec<NewEscape>,
}

/// Reads the document of the read verb: `{"review": null}` while the review is missing, or
/// `{"review": {"id", "submitted_at"}, "comments": [{"path", "line", "url", "untrusted"}]}`.
pub fn parse_review(text: &str) -> Result<Option<Review>, String> {
    let value: Value =
        serde_json::from_str(text).map_err(|e| format!("the review document is not JSON: {e}"))?;
    let review = match value.get("review") {
        None | Some(Value::Null) => return Ok(None),
        Some(review) => review,
    };
    let id = review
        .get("id")
        .and_then(Value::as_i64)
        .ok_or("the review of the document has no id")?;
    let comments = value
        .get("comments")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .map(|c| Comment {
                    path: c
                        .get("path")
                        .and_then(Value::as_str)
                        .map(clean_path)
                        .filter(|p| !p.is_empty()),
                    line: c.get("line").and_then(Value::as_i64),
                    url: c.get("url").and_then(Value::as_str).map(str::to_string),
                    body: match c.get("untrusted") {
                        Some(Value::String(text)) => text.clone(),
                        Some(Value::Object(map)) => map
                            .get("body")
                            .and_then(Value::as_str)
                            .unwrap_or("")
                            .to_string(),
                        _ => String::new(),
                    },
                })
                .collect()
        })
        .unwrap_or_default();
    Ok(Some(Review {
        id,
        submitted: review
            .get("submitted_at")
            .and_then(Value::as_str)
            .map(str::to_string),
        comments,
    }))
}

/// A path of the repo with no `./` and no `/` in front.
fn clean_path(path: &str) -> String {
    let path = path.trim();
    path.strip_prefix("./")
        .unwrap_or(path)
        .trim_start_matches('/')
        .to_string()
}

/// The title of a comment: its first bold line with no emphasis marks, or else its first line
/// of text.
pub fn title_of(body: &str) -> String {
    let lines = || body.lines().map(str::trim).filter(|l| !l.is_empty());
    let line = lines()
        .find(|l| l.contains("**") || l.contains("__"))
        .or_else(|| lines().next())
        .unwrap_or("");
    line.replace("**", "")
        .replace("__", "")
        .trim_start_matches(['#', '>', '-', '*', ' '])
        .trim()
        .to_string()
}

/// The priority of a comment: group 1 of the pattern, trimmed, or `None` when the pattern does
/// not match.
pub fn priority_of(pattern: &Regex, body: &str) -> Option<String> {
    pattern
        .captures(body)?
        .get(1)?
        .as_deref()
        .map(str::trim)
        .filter(|p| !p.is_empty())
        .map(str::to_string)
}

/// True when a finding of ours matches a comment: the same path, and lines at most `lines`
/// apart. A comment or a finding with no line matches each line of its path.
pub fn matches(ours: &Ours, comment: &Comment, lines: u32) -> bool {
    let Some(path) = &comment.path else {
        return false;
    };
    if ours.path != *path {
        return false;
    }
    match (ours.line, comment.line) {
        (Some(a), Some(b)) => (a - b).abs() <= i64::from(lines),
        _ => true,
    }
}

/// Matches each comment of a counted priority against the findings of ours. Each title is
/// masked by `mask` and cut to [`TITLE_MAX`] characters.
pub fn judge(
    review: &Review,
    ours: &[Ours],
    config: &Escapes,
    pattern: &Regex,
    mask: &dyn Fn(&str) -> String,
) -> Outcome {
    let mut outcome = Outcome::default();
    for comment in &review.comments {
        let Some(priority) = priority_of(pattern, &comment.body) else {
            continue;
        };
        if !config.count.iter().any(|c| c.trim() == priority) {
            continue;
        }
        outcome.counted += 1;
        if ours.iter().any(|o| matches(o, comment, config.lines)) {
            continue;
        }
        outcome.escapes.push(NewEscape {
            path: comment.path.clone(),
            line: comment.line,
            priority: Some(priority),
            title: Some(follow::cut_title(
                &mask(&title_of(&comment.body)),
                TITLE_MAX,
            )),
            url: comment.url.clone(),
        });
    }
    outcome
}

/// The tick hook. It never fails the tick: each problem goes to the log, one line for each
/// tick, and a later tick tries again.
pub fn tick(pass: &mut Pass) {
    if let Err(e) = check_all(pass.ctx, &pass.ledger, pass.now) {
        pass.ctx.log(&format!(
            "escapes: the check waits for the next tick: {e:#}"
        ));
    }
}

/// Runs the escape check of each ticket with a pull request and a pass of the gate cr. Gives
/// the number of reviews that it checked.
pub fn check_all(ctx: &Ctx, ledger: &Ledger, now: ::time::OffsetDateTime) -> Result<usize> {
    let config = ctx.config()?;
    if !config.escapes.on() {
        return Ok(0);
    }
    let pattern = Regex::new(&config.escapes.priority)
        .map_err(|e| anyhow::anyhow!("escapes.priority: {e}"))?;
    let mut secrets: Option<Secrets> = None;
    let mut checked = 0;
    let mut problems = Vec::new();
    for row in ledger.tickets()? {
        let Ok(key) = TicketKey::parse(&row.key) else {
            continue;
        };
        if key.is_reserved()
            || row.state.as_deref().is_none_or(str::is_empty)
            || Counts::of(&row).finished.is_some()
        {
            continue;
        }
        let Some(pr) = row.pr else {
            continue;
        };
        let Some(pass) = ledger
            .gate_rounds_of(key.as_str(), CR_SOURCE)?
            .into_iter()
            .rfind(|r| r.passed)
        else {
            continue;
        };
        for reviewer in &config.escapes.reviewers {
            let check = ledger.open_escape_check(&EscapeCheckRow {
                ticket: key.to_string(),
                pr: Some(pr),
                reviewer: reviewer.clone(),
                pass_turn: pass.turn.clone(),
                head: pass.head.clone(),
                passed_at: Some(pass.at.clone()),
                ..EscapeCheckRow::default()
            })?;
            if check.checked.is_some() {
                continue;
            }
            let due = check
                .looked
                .as_deref()
                .and_then(|t| time::parse_rfc3339(t).ok())
                .is_none_or(|at| {
                    time::seconds_between(at, now)
                        >= i64::try_from(config.escapes.every).unwrap_or(i64::MAX)
                });
            if !due {
                continue;
            }
            if secrets.is_none() {
                secrets = Some(Secrets::read(config)?);
            }
            let secrets = secrets.as_ref().context("the secrets")?;
            match check_one(
                ctx,
                ledger,
                &key,
                &check,
                &pass,
                &config.escapes,
                &pattern,
                secrets,
                now,
            ) {
                Ok(true) => checked += 1,
                Ok(false) => {}
                Err(e) => {
                    ledger.set_escape_looked(check.id, &time::rfc3339(now))?;
                    problems.push(format!("{key} ({reviewer}): {e:#}"));
                }
            }
        }
    }
    if !problems.is_empty() {
        ctx.log(&format!(
            "escapes: {} check(s) could not read their review: {}",
            problems.len(),
            problems.join("; ")
        ));
    }
    Ok(checked)
}

/// One look for the review of one reviewer. Gives true when it found and checked the review.
#[allow(clippy::too_many_arguments)]
fn check_one(
    ctx: &Ctx,
    ledger: &Ledger,
    key: &TicketKey,
    check: &EscapeCheckRow,
    pass: &crate::ledger::review::GateRoundRow,
    config: &Escapes,
    pattern: &Regex,
    secrets: &Secrets,
    now: ::time::OffsetDateTime,
) -> Result<bool> {
    let at = time::rfc3339(now);
    let answer = crate::executor::read(
        ctx,
        ledger,
        key,
        &config.verb,
        &json!({"reviewer": check.reviewer, "after": pass.at}),
    )?;
    let Some(document) = answer.documents.first() else {
        anyhow::bail!("the read verb {} gave no document", config.verb);
    };
    let Some(review) = parse_review(&document.content).map_err(anyhow::Error::msg)? else {
        ledger.set_escape_looked(check.id, &at)?;
        return Ok(false);
    };
    let ours: Vec<Ours> = ledger
        .findings_of(key.as_str())?
        .into_iter()
        .filter(|f| f.source == CR_SOURCE)
        .filter_map(|f| {
            Some(Ours {
                path: clean_path(f.path.as_deref()?),
                line: f.line,
            })
        })
        .collect();
    let mask = |text: &str| crate::executor::gates::mask_token_forms(&secrets.mask(text));
    let outcome = judge(&review, &ours, config, pattern, &mask);
    let done = EscapeCheckRow {
        looked: Some(at.clone()),
        review_id: Some(review.id),
        submitted: review.submitted.clone(),
        checked: Some(at.clone()),
        comments: review.comments.len() as i64,
        counted: outcome.counted,
        ..check.clone()
    };
    let recorded = ledger.in_transaction(|ledger| {
        if !ledger.finish_escape_check(&done, &outcome.escapes)? {
            return Ok(false);
        }
        for escape in &outcome.escapes {
            let title = escape.title.clone().unwrap_or_default();
            let path = escape.path.clone().unwrap_or_default();
            ledger.add_finding(&FindingRow {
                ticket: key.to_string(),
                round: pass.round,
                fingerprint: follow::fingerprint(&path, &title),
                priority: escape.priority.clone(),
                source: ESCAPE_SOURCE.to_string(),
                verdict: "kept".to_string(),
                path: escape.path.clone(),
                line: escape.line,
                title: Some(title),
                turn: Some(pass.turn.clone()),
                ..FindingRow::default()
            })?;
        }
        ledger.add_event(
            &Event::now("tick", "escape-check")
                .ticket(key.as_str())
                .external_id(&format!("escape-check:{}", check.id))
                .detail(json!({
                    "reviewer": check.reviewer, "review": review.id, "pr": check.pr,
                    "turn": pass.turn, "head": pass.head, "comments": done.comments,
                    "counted": outcome.counted, "escapes": outcome.escapes.len(),
                })),
        )?;
        Ok(true)
    })?;
    if !recorded {
        return Ok(false);
    }
    ctx.log(&format!(
        "escapes: the review {} of {} on {key} has {} counted comment(s), and {} escaped our review",
        review.id,
        check.reviewer,
        outcome.counted,
        outcome.escapes.len()
    ));
    // The note goes into the entry of the turn whose gate passed now, even when that turn ended
    // more than the days of the journal tick ago.
    if !outcome.escapes.is_empty() {
        if let Some(row) = ledger.turn(&pass.turn)? {
            let inputs = crate::journal::Inputs::new(ctx);
            if let Err(e) = crate::journal::write_entry(ctx, ledger, &row, &inputs, false) {
                ctx.log(&format!(
                    "escapes: the journal entry of {} waits for the journal tick: {e:#}",
                    pass.turn
                ));
            }
        }
    }
    Ok(true)
}

/// The escape numbers of a time window: the checked pull requests, the escapes by priority, and
/// the escapes for each checked pull request.
#[derive(Clone, Debug, Default, PartialEq, serde::Serialize)]
pub struct EscapeMeasures {
    /// The pull requests whose review a check read.
    pub checked_prs: usize,
    /// The escapes of the counted priorities, by priority.
    pub by_priority: std::collections::BTreeMap<String, usize>,
    pub escapes: usize,
    /// Escapes for each checked pull request, or `None` with no check.
    pub rate: Option<f64>,
}

/// The escape numbers of the checks at or after `since`, or of all time, or of one ticket.
pub fn measures(
    ledger: &Ledger,
    ticket: Option<&str>,
    since: Option<&str>,
) -> Result<EscapeMeasures> {
    let checks = ledger.escape_checks(since)?;
    let prs: std::collections::BTreeSet<(String, Option<i64>)> = checks
        .iter()
        .filter(|c| ticket.is_none_or(|t| c.ticket == t))
        .map(|c| (c.ticket.clone(), c.pr))
        .collect();
    let mut out = EscapeMeasures {
        checked_prs: prs.len(),
        ..EscapeMeasures::default()
    };
    for escape in ledger.escapes(ticket, since)? {
        out.escapes += 1;
        *out.by_priority
            .entry(escape.priority.unwrap_or_else(|| "-".into()))
            .or_default() += 1;
    }
    if out.checked_prs > 0 {
        out.rate = Some(out.escapes as f64 / out.checked_prs as f64);
    }
    Ok(out)
}

/// The escape numbers in one line, such as `3 checked PRs, escapes P0 0 · P1 2, 0.67 per PR`.
pub fn measures_line(m: &EscapeMeasures, counted: &[String]) -> String {
    if m.checked_prs == 0 {
        return "no pull request checked yet".to_string();
    }
    let mut parts: Vec<String> = counted
        .iter()
        .map(|p| format!("{p}: {}", m.by_priority.get(p).copied().unwrap_or(0)))
        .collect();
    for (p, n) in &m.by_priority {
        if !counted.contains(p) {
            parts.push(format!("{p}: {n}"));
        }
    }
    format!(
        "{} checked PR(s), {} escape(s) (by priority {}), {:.2} per PR",
        m.checked_prs,
        m.escapes,
        parts.join(", "),
        m.rate.unwrap_or(0.0)
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config() -> Escapes {
        Escapes {
            reviewers: vec!["review-bot[bot]".into()],
            ..Escapes::default()
        }
    }

    fn comment(path: &str, line: i64, body: &str) -> Comment {
        Comment {
            path: Some(path.into()),
            line: Some(line),
            url: Some(format!("https://example.invalid/{path}#{line}")),
            body: body.into(),
        }
    }

    #[test]
    fn the_title_is_the_first_bold_line_and_the_priority_its_group() {
        let pattern = Regex::new(&Escapes::default().priority).unwrap();
        let body = "Some context.\n\n**[P1] Handle a nil user before the export**\n\nThe export...";
        assert_eq!(title_of(body), "[P1] Handle a nil user before the export");
        assert_eq!(priority_of(&pattern, body).as_deref(), Some("1"));
        assert_eq!(title_of("plain first line\nsecond"), "plain first line");
        assert_eq!(priority_of(&pattern, "no badge"), None);
    }

    #[test]
    fn a_counted_comment_with_no_finding_near_it_is_an_escape() {
        let pattern = Regex::new(&Escapes::default().priority).unwrap();
        let review = Review {
            id: 9,
            submitted: None,
            comments: vec![
                comment("app/a.rb", 12, "**[P1] Matched within 3 lines**"),
                comment("app/a.rb", 30, "**[P0] Missed on the same file**"),
                comment(
                    "app/b.rb",
                    5,
                    "**[P2] Missed on another file with token-abc**",
                ),
                comment("app/c.rb", 1, "**[P3] Not counted**"),
                comment("app/d.rb", 1, "No priority at all"),
            ],
        };
        let ours = [
            Ours {
                path: "app/a.rb".into(),
                line: Some(10),
            },
            Ours {
                path: "app/x.rb".into(),
                line: Some(5),
            },
        ];
        let mask = |t: &str| t.replace("token-abc", "[masked]");
        let out = judge(&review, &ours, &config(), &pattern, &mask);
        assert_eq!(out.counted, 3);
        let places: Vec<(Option<&str>, Option<i64>, Option<&str>)> = out
            .escapes
            .iter()
            .map(|e| (e.path.as_deref(), e.line, e.priority.as_deref()))
            .collect();
        assert_eq!(
            places,
            [
                (Some("app/a.rb"), Some(30), Some("0")),
                (Some("app/b.rb"), Some(5), Some("2"))
            ]
        );
        assert_eq!(
            out.escapes[1].title.as_deref(),
            Some("[P2] Missed on another file with [masked]")
        );
        assert!(out.escapes[0].url.is_some());
    }

    #[test]
    fn a_comment_with_no_line_matches_each_finding_of_its_path() {
        let mut c = comment("app/a.rb", 1, "x");
        c.line = None;
        let ours = Ours {
            path: "app/a.rb".into(),
            line: Some(400),
        };
        assert!(matches(&ours, &c, 3));
        c.path = None;
        assert!(!matches(&ours, &c, 3));
    }

    #[test]
    fn the_review_document_reads_with_or_without_a_review() {
        assert_eq!(
            parse_review("{\"review\": null, \"comments\": []}"),
            Ok(None)
        );
        let doc = json!({
            "review": {"id": 5, "submitted_at": "2026-11-02T12:00:00Z"},
            "comments": [{"path": "./app/a.rb", "line": 3, "url": "u", "untrusted": "**[P1] x**"}],
        });
        let review = parse_review(&doc.to_string()).unwrap().unwrap();
        assert_eq!(review.id, 5);
        assert_eq!(review.comments[0].path.as_deref(), Some("app/a.rb"));
        assert_eq!(review.comments[0].body, "**[P1] x**");
        assert!(parse_review("not json").is_err());
        assert!(parse_review("{\"review\": {}}").is_err());
    }

    #[test]
    fn the_measures_line_names_each_counted_priority() {
        let mut m = EscapeMeasures {
            checked_prs: 3,
            escapes: 2,
            rate: Some(2.0 / 3.0),
            ..EscapeMeasures::default()
        };
        m.by_priority.insert("1".into(), 2);
        let counted = ["0".to_string(), "1".to_string(), "2".to_string()];
        assert_eq!(
            measures_line(&m, &counted),
            "3 checked PR(s), 2 escape(s) (by priority 0: 0, 1: 2, 2: 0), 0.67 per PR"
        );
        assert_eq!(
            measures_line(&EscapeMeasures::default(), &counted),
            "no pull request checked yet"
        );
    }
}
