//! The refused requests of a review (section 11.3). A round of review comments ends with
//! `verdicts.json`: a verdict for each comment that it read. Vision follows each requested
//! change across the rounds in `refusals.json` of the ticket folder. It counts the rounds that
//! refused it with `incorrect`, `out-of-scope`, or `low-priority`, and it keeps the reason of
//! each refusal. A fix (`valid` or `already-done`) ends the count.
//!
//! A comment of a new round is the same request as a counted one when it is in the same
//! thread, or when it is an inline comment of the same reviewer within [`NEAR`] lines of the
//! same file. A review and a conversation comment are the same request only by their id.
//!
//! The refusal that reaches `caps.refusals` needs the decision of the person. Until a card with
//! the topics [`REFUSED_REVIEW`] and `comment:<id>` has the answer [`FIX`] or [`KEEP`], the end
//! facts of the round do not hold, and the resume tells the agent to ask that card. [`KEEP`]
//! lets the refusal stand and starts its count again from 0. [`FIX`] needs a fix of the
//! request in the same round.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::docs::{self, Verdict};
use crate::ledger::questions::{QuestionRow, STATE_ANSWERED, STATE_NEEDLESS};
use crate::questions::card::{fold, Answer, Card};
pub use crate::questions::rules::REFUSED_REVIEW;

/// The document of the ticket folder that holds the counts.
pub const REFUSALS: &str = "refusals.json";
/// The choice of the card that asks the agent to fix the request.
pub const FIX: &str = "Fix it";
/// The choice of the card that lets the refusal stand.
pub const KEEP: &str = "Keep refusing";
/// Two inline comments of one reviewer in one file are one request at most this many lines
/// apart.
pub const NEAR: i64 = 3;
/// The reasons that a count keeps: the newest ones.
const REASONS_KEPT: usize = 5;
/// The turns whose rounds the file remembers, so that a second pass over one turn counts
/// nothing new: the newest ones.
const ROUNDS_KEPT: usize = 50;
/// The longest text of a request that a count keeps, in characters.
const REQUEST_MAX: usize = 600;

/// The verdicts that refuse a requested change.
pub const REFUSING: [&str; 3] = ["incorrect", "out-of-scope", "low-priority"];
/// The verdicts that fix a requested change.
pub const FIXING: [&str; 2] = ["valid", "already-done"];

/// The text of `refusals.json`: the turns whose rounds it counted, and the requests.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Refusals {
    pub rounds: Vec<String>,
    pub requests: Vec<Track>,
}

/// One requested change that rounds refused.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Track {
    /// `inline`, `review`, or `conversation`.
    pub kind: String,
    /// The ids of the comments that asked for it, oldest first.
    pub ids: Vec<i64>,
    pub author: String,
    pub path: Option<String>,
    pub line: Option<i64>,
    /// The refusals since the last fix or the last decision of the person.
    pub count: u32,
    /// What the newest comment asks, cut.
    pub request: String,
    /// The reasons of the counted refusals, oldest first.
    pub reasons: Vec<Reason>,
    /// When the count last changed, RFC 3339 UTC.
    pub updated: String,
}

/// One refusal of a request.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Reason {
    pub turn: String,
    pub verdict: String,
    pub reply: String,
}

/// One comment of `comments.json`, with what the count needs.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Comment {
    pub id: i64,
    pub kind: String,
    pub author: String,
    pub path: Option<String>,
    pub line: Option<i64>,
    pub text: String,
}

/// The decision of the person on a card about a refused request.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Decision {
    Fix,
    Keep,
}

/// One decision of the person, with the comments that its card names.
#[derive(Clone, Debug, PartialEq)]
pub struct Decided {
    pub ids: BTreeSet<i64>,
    pub decision: Decision,
    /// When the person answered, RFC 3339 UTC.
    pub at: String,
}

/// The counts of `refusals.json`. A missing or broken file counts nothing.
pub fn parse(text: Option<&str>) -> Refusals {
    text.and_then(|t| serde_json::from_str(t).ok())
        .unwrap_or_default()
}

/// The text of `refusals.json`.
pub fn render(refusals: &Refusals) -> String {
    format!(
        "{}\n",
        serde_json::to_string_pretty(refusals).unwrap_or_else(|_| "{}".into())
    )
}

/// The counts after the round of `turn` held: [`update`] of its requests, once for each
/// turn. A turn that the file counted already changes nothing.
pub fn count_round(
    refusals: &Refusals,
    verdicts: &[Verdict],
    comments: &[Comment],
    decisions: &[Decided],
    turn: &str,
    now: &str,
) -> Refusals {
    if refusals.rounds.iter().any(|t| t == turn) {
        return refusals.clone();
    }
    let mut rounds = refusals.rounds.clone();
    rounds.push(turn.to_string());
    let extra = rounds.len().saturating_sub(ROUNDS_KEPT);
    rounds.drain(..extra);
    Refusals {
        rounds,
        requests: update(&refusals.requests, verdicts, comments, decisions, turn, now),
    }
}

/// The comments of `comments.json`. A row with no id is left out.
pub fn comments(text: &str) -> Vec<Comment> {
    let Ok(Value::Array(items)) = docs::loose_json(text) else {
        return Vec::new();
    };
    items
        .iter()
        .filter_map(|c| {
            let id = c.get("id").and_then(Value::as_i64)?;
            let text = c
                .get("text")
                .and_then(|t| t.get("untrusted").or(Some(t)))
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            Some(Comment {
                id,
                kind: c
                    .get("kind")
                    .and_then(Value::as_str)
                    .unwrap_or("inline")
                    .to_string(),
                author: c
                    .get("author")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string(),
                path: c.get("path").and_then(Value::as_str).map(str::to_string),
                line: c.get("line").and_then(Value::as_i64),
                text,
            })
        })
        .collect()
}

/// The decisions of the person in the cards of a ticket: each answered card with the topic
/// [`REFUSED_REVIEW`], with the comments of its topics `comment:<id>`. A card that the person
/// marked as needless keeps the refusal. An answer that names no choice decides nothing.
pub fn decisions(rows: &[QuestionRow]) -> Vec<Decided> {
    let mut out = Vec::new();
    for row in rows {
        let Ok(card) = serde_json::from_str::<Card>(&row.card) else {
            continue;
        };
        if !card.topics.iter().any(|t| fold(t) == fold(REFUSED_REVIEW)) {
            continue;
        }
        let ids: BTreeSet<i64> = card
            .topics
            .iter()
            .filter_map(|t| {
                let t = t.trim().to_lowercase();
                t.strip_prefix("comment:")?
                    .trim()
                    .trim_start_matches('#')
                    .parse()
                    .ok()
            })
            .collect();
        if ids.is_empty() {
            continue;
        }
        let decision = match row.state.as_str() {
            STATE_NEEDLESS => Some(Decision::Keep),
            STATE_ANSWERED => row
                .answer
                .as_deref()
                .and_then(|a| serde_json::from_str::<Answer>(a).ok())
                .and_then(|a| decision_of(&a.text)),
            _ => None,
        };
        let Some(decision) = decision else {
            continue;
        };
        let at = row
            .answered
            .clone()
            .or_else(|| row.created.clone())
            .unwrap_or_default();
        out.push(Decided { ids, decision, at });
    }
    out
}

/// The decision of an answer text: [`FIX`] or [`KEEP`], in any case.
fn decision_of(text: &str) -> Option<Decision> {
    let text = fold(text);
    if text == fold(FIX) {
        Some(Decision::Fix)
    } else if text == fold(KEEP) {
        Some(Decision::Keep)
    } else {
        None
    }
}

/// The index of the count that follows the request of `comment`, if any.
fn find(tracks: &[Track], comment: &Comment) -> Option<usize> {
    tracks
        .iter()
        .position(|t| t.ids.contains(&comment.id))
        .or_else(|| {
            if comment.kind != "inline" {
                return None;
            }
            tracks.iter().position(|t| {
                t.kind == "inline"
                    && !t.author.is_empty()
                    && t.author.eq_ignore_ascii_case(&comment.author)
                    && t.path.is_some()
                    && t.path == comment.path
                    && matches!((t.line, comment.line), (Some(a), Some(b)) if (a - b).abs() <= NEAR)
            })
        })
}

/// The newest decision of the person for a request that is newer than its count.
fn decided(track: Option<&Track>, comment: &Comment, decisions: &[Decided]) -> Option<Decision> {
    let after = track.map_or("", |t| t.updated.as_str());
    decisions
        .iter()
        .filter(|d| {
            d.ids.contains(&comment.id)
                || track.is_some_and(|t| t.ids.iter().any(|i| d.ids.contains(i)))
        })
        .filter(|d| d.at.as_str() > after)
        .max_by(|a, b| a.at.cmp(&b.at))
        .map(|d| d.decision)
}

/// The comment of a verdict: its row of `comments.json`, or a comment with only its id.
fn comment_of(verdict: &Verdict, comments: &[Comment]) -> Comment {
    comments
        .iter()
        .find(|c| c.id == verdict.comment_id)
        .cloned()
        .unwrap_or(Comment {
            id: verdict.comment_id,
            kind: "inline".into(),
            ..Comment::default()
        })
}

/// What stops the end facts of a round: one text for each refused request that needs the
/// decision of the person, and for each refused request that the person decided to fix. Empty
/// when the round can hold. `limit` 0 stops nothing.
pub fn check(
    tracks: &[Track],
    verdicts: &[Verdict],
    comments: &[Comment],
    decisions: &[Decided],
    limit: u32,
) -> Vec<String> {
    if limit == 0 {
        return Vec::new();
    }
    let mut out = Vec::new();
    for verdict in verdicts {
        if !REFUSING.contains(&verdict.verdict.as_str()) {
            continue;
        }
        let comment = comment_of(verdict, comments);
        let track = find(tracks, &comment).map(|i| &tracks[i]);
        match decided(track, &comment, decisions) {
            Some(Decision::Keep) => continue,
            Some(Decision::Fix) => {
                out.push(format!(
                    "The person decided on a card to fix the comment {}{}. Fix it, and give its row of verdicts.json the verdict valid with the fix commit.",
                    comment.id,
                    place(&comment)
                ));
                continue;
            }
            None => {}
        }
        let count = track.map_or(0, |t| t.count) + 1;
        if count < limit {
            continue;
        }
        let mut earlier = String::new();
        for (n, reason) in track
            .map_or(&[][..], |t| t.reasons.as_slice())
            .iter()
            .enumerate()
        {
            earlier.push_str(&format!(
                " {}. {}: {}",
                n + 1,
                reason.verdict,
                one_line(&reason.reply, 300)
            ));
        }
        out.push(format!(
            "The comment {id}{place} asks for a change that this would refuse for the {nth} time{earlier}. Do not refuse it again on your own: ask the person with one card. Its kind is \"code\", its text says what the comment asks, its context gives each reason of the refusals and your reason now, its options are \"{FIX}\" and \"{KEEP}\", and its topics are [\"{REFUSED_REVIEW}\", \"comment:{id}\"]. After the answer, fix it with the verdict valid, or keep the refusal in verdicts.json.",
            id = comment.id,
            place = place(&comment),
            nth = ordinal(count),
            earlier = if earlier.is_empty() {
                String::new()
            } else {
                format!(". The earlier refusals:{earlier}")
            },
        ));
    }
    out
}

/// The counts after a round whose end facts held: a refusal counts one more, or starts again
/// from 0 after the decision [`KEEP`] of the person, and a fix ends the count.
pub fn update(
    tracks: &[Track],
    verdicts: &[Verdict],
    comments: &[Comment],
    decisions: &[Decided],
    turn: &str,
    now: &str,
) -> Vec<Track> {
    let mut tracks = tracks.to_vec();
    for verdict in verdicts {
        let comment = comment_of(verdict, comments);
        let at = find(&tracks, &comment);
        if FIXING.contains(&verdict.verdict.as_str()) {
            if let Some(i) = at {
                tracks.remove(i);
            }
            continue;
        }
        if !REFUSING.contains(&verdict.verdict.as_str()) {
            continue;
        }
        let decision = decided(at.map(|i| &tracks[i]), &comment, decisions);
        let i = match at {
            Some(i) => i,
            None => {
                tracks.push(Track {
                    kind: comment.kind.clone(),
                    author: comment.author.clone(),
                    ..Track::default()
                });
                tracks.len() - 1
            }
        };
        let track = &mut tracks[i];
        if !track.ids.contains(&comment.id) {
            track.ids.push(comment.id);
        }
        if comment.path.is_some() {
            track.path = comment.path.clone();
            track.line = comment.line;
        }
        if !comment.text.trim().is_empty() {
            track.request = cut(comment.text.trim(), REQUEST_MAX);
        }
        if decision == Some(Decision::Keep) {
            track.count = 0;
            track.reasons.clear();
        } else {
            track.count += 1;
            track.reasons.push(Reason {
                turn: turn.to_string(),
                verdict: verdict.verdict.clone(),
                reply: cut(verdict.reply.trim(), REQUEST_MAX),
            });
            let extra = track.reasons.len().saturating_sub(REASONS_KEPT);
            track.reasons.drain(..extra);
        }
        track.updated = now.to_string();
    }
    tracks
}

/// ` (author, path:line)` of a comment, or nothing that it lacks.
fn place(comment: &Comment) -> String {
    let at = match (&comment.path, comment.line) {
        (Some(path), Some(line)) => format!("{path}:{line}"),
        (Some(path), None) => path.clone(),
        _ => String::new(),
    };
    match (comment.author.is_empty(), at.is_empty()) {
        (true, true) => String::new(),
        (false, true) => format!(" ({})", comment.author),
        (true, false) => format!(" ({at})"),
        (false, false) => format!(" ({}, {at})", comment.author),
    }
}

/// `1st`, `2nd`, `3rd`, `4th`, `11th`, `21st`, and so on.
fn ordinal(n: u32) -> String {
    let suffix = match (n % 10, n % 100) {
        (_, 11..=13) => "th",
        (1, _) => "st",
        (2, _) => "nd",
        (3, _) => "rd",
        _ => "th",
    };
    format!("{n}{suffix}")
}

fn one_line(text: &str, max: usize) -> String {
    cut(&text.split_whitespace().collect::<Vec<_>>().join(" "), max)
}

fn cut(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    let mut out: String = text.chars().take(max.saturating_sub(1)).collect();
    out.push('…');
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn verdict(id: i64, verdict: &str, reply: &str) -> Verdict {
        Verdict {
            comment_id: id,
            verdict: verdict.into(),
            fix_commit: None,
            reply: reply.into(),
        }
    }

    fn inline(id: i64, author: &str, path: &str, line: i64) -> Comment {
        Comment {
            id,
            kind: "inline".into(),
            author: author.into(),
            path: Some(path.into()),
            line: Some(line),
            text: format!("Use find_each here ({id})"),
        }
    }

    fn round(tracks: &[Track], verdicts: &[Verdict], comments: &[Comment], n: u32) -> Vec<Track> {
        update(
            tracks,
            verdicts,
            comments,
            &[],
            &format!("turn-{n}"),
            &format!("2026-11-24T10:0{n}:00Z"),
        )
    }

    #[test]
    fn a_request_refused_in_the_same_thread_or_at_the_same_place_counts_up_to_the_limit() {
        let one = inline(10, "review-bot[bot]", "app/offer.rb", 42);
        let tracks = round(
            &[],
            &[verdict(10, "low-priority", "Won't fix (net +1)")],
            std::slice::from_ref(&one),
            1,
        );
        assert_eq!(tracks.len(), 1);
        assert_eq!(tracks[0].count, 1);
        // The same thread in the next round.
        let tracks = round(
            &tracks,
            &[verdict(10, "incorrect", "Not a bug: 50 rows at most")],
            &[one],
            2,
        );
        assert_eq!(tracks[0].count, 2);
        // A new thread of the same reviewer 2 lines away is the same request: it needs the
        // person now.
        let two = inline(20, "Review-Bot[bot]", "app/offer.rb", 44);
        let refused = [verdict(20, "out-of-scope", "Won't fix: pre-existing")];
        let stops = check(&tracks, &refused, std::slice::from_ref(&two), &[], 3);
        assert_eq!(stops.len(), 1, "{stops:?}");
        let text = &stops[0];
        assert!(
            text.contains("The comment 20 (Review-Bot[bot], app/offer.rb:44)"),
            "{text}"
        );
        assert!(text.contains("refuse for the 3rd time"), "{text}");
        assert!(
            text.contains("1. low-priority: Won't fix (net +1)"),
            "{text}"
        );
        assert!(
            text.contains("2. incorrect: Not a bug: 50 rows at most"),
            "{text}"
        );
        assert!(
            text.contains("\"refused-review\", \"comment:20\""),
            "{text}"
        );
        // Below the limit, nothing stops.
        assert!(check(&tracks, &refused, &[two], &[], 4).is_empty());
        assert!(check(&tracks, &refused, &[inline(20, "x", "y", 1)], &[], 0).is_empty());
    }

    #[test]
    fn another_reviewer_another_file_or_lines_far_apart_start_their_own_count() {
        let tracks = round(
            &[],
            &[verdict(10, "incorrect", "no")],
            &[inline(10, "a", "f.rb", 42)],
            1,
        );
        for other in [
            inline(20, "b", "f.rb", 42),
            inline(21, "a", "g.rb", 42),
            inline(22, "a", "f.rb", 46),
        ] {
            let next = round(
                &tracks,
                &[verdict(other.id, "incorrect", "no")],
                std::slice::from_ref(&other),
                2,
            );
            assert_eq!(next.len(), 2, "{other:?} is a new request");
            assert_eq!(next[1].count, 1);
        }
        // A review counts only by its id.
        let review = Comment {
            id: 30,
            kind: "review".into(),
            author: "a".into(),
            ..Comment::default()
        };
        let next = round(&tracks, &[verdict(30, "incorrect", "no")], &[review], 2);
        assert_eq!(next.len(), 2);
    }

    #[test]
    fn a_fix_ends_the_count_and_a_question_leaves_it() {
        let c = inline(10, "a", "f.rb", 1);
        let tracks = round(
            &[],
            &[verdict(10, "incorrect", "no")],
            std::slice::from_ref(&c),
            1,
        );
        let same = round(
            &tracks,
            &[verdict(10, "question", "It counts rows.")],
            std::slice::from_ref(&c),
            2,
        );
        assert_eq!(same, tracks);
        assert!(round(
            &tracks,
            &[verdict(10, "valid", "Fixed in abc1234")],
            std::slice::from_ref(&c),
            3
        )
        .is_empty());
        assert!(round(
            &tracks,
            &[verdict(10, "already-done", "Already fixed")],
            &[c],
            3
        )
        .is_empty());
    }

    #[test]
    fn the_decision_keep_lets_the_refusal_stand_and_starts_the_count_again() {
        let c = inline(10, "a", "f.rb", 1);
        let mut tracks = Vec::new();
        for n in 1..=2 {
            tracks = round(
                &tracks,
                &[verdict(10, "incorrect", "no")],
                std::slice::from_ref(&c),
                n,
            );
        }
        let keep = Decided {
            ids: [10].into(),
            decision: Decision::Keep,
            at: "2026-11-24T10:05:00Z".into(),
        };
        let refused = [verdict(10, "incorrect", "still no")];
        assert!(check(
            &tracks,
            &refused,
            std::slice::from_ref(&c),
            std::slice::from_ref(&keep),
            3
        )
        .is_empty());
        let after = update(
            &tracks,
            &refused,
            std::slice::from_ref(&c),
            std::slice::from_ref(&keep),
            "turn-3",
            "2026-11-24T10:06:00Z",
        );
        assert_eq!(after[0].count, 0);
        assert!(after[0].reasons.is_empty());
        // The old decision is older than the count now: the next refusals count again.
        let next = update(
            &after,
            &refused,
            std::slice::from_ref(&c),
            std::slice::from_ref(&keep),
            "turn-4",
            "2026-11-24T10:07:00Z",
        );
        assert_eq!(next[0].count, 1);
        // A decision older than the count decides nothing.
        let old = Decided {
            at: "2026-11-24T09:00:00Z".into(),
            ..keep
        };
        assert_eq!(check(&tracks, &refused, &[c], &[old], 3).len(), 1);
    }

    #[test]
    fn the_decision_fix_stops_a_refusal_and_lets_a_fix_hold() {
        let c = inline(10, "a", "f.rb", 1);
        let tracks = round(
            &[],
            &[verdict(10, "incorrect", "no")],
            std::slice::from_ref(&c),
            1,
        );
        let fix = Decided {
            ids: [10].into(),
            decision: Decision::Fix,
            at: "2026-11-24T10:05:00Z".into(),
        };
        let stops = check(
            &tracks,
            &[verdict(10, "incorrect", "no")],
            std::slice::from_ref(&c),
            std::slice::from_ref(&fix),
            3,
        );
        assert_eq!(stops.len(), 1);
        assert!(
            stops[0].contains("decided on a card to fix the comment 10"),
            "{stops:?}"
        );
        assert!(check(
            &tracks,
            &[verdict(10, "valid", "Fixed in abc1234")],
            &[c],
            &[fix],
            3
        )
        .is_empty());
    }

    #[test]
    fn the_decisions_come_from_answered_cards_with_the_topic_and_the_comment() {
        let row = |topics: Value, state: &str, answer: Option<&str>| QuestionRow {
            card:
                json!({"text": "q", "options": [{"label": FIX}, {"label": KEEP}], "topics": topics})
                    .to_string(),
            state: state.into(),
            answer: answer.map(|a| json!({"choice": 1, "text": a, "by": "person"}).to_string()),
            answered: Some("2026-11-24T10:05:00Z".into()),
            ..QuestionRow::default()
        };
        let rows = vec![
            row(
                json!(["refused-review", "comment:10"]),
                STATE_ANSWERED,
                Some("keep refusing"),
            ),
            row(
                json!(["Refused-Review", "comment:#11", "comment: 12"]),
                STATE_ANSWERED,
                Some(FIX),
            ),
            row(
                json!(["refused-review", "comment:13"]),
                STATE_NEEDLESS,
                None,
            ),
            row(json!(["refused-review", "comment:14"]), "asked", None),
            row(json!(["exports", "comment:15"]), STATE_ANSWERED, Some(FIX)),
            row(json!(["refused-review"]), STATE_ANSWERED, Some(FIX)),
            row(
                json!(["refused-review", "comment:16"]),
                STATE_ANSWERED,
                Some("Something else"),
            ),
        ];
        let got: Vec<(Vec<i64>, Decision)> = decisions(&rows)
            .into_iter()
            .map(|d| (d.ids.into_iter().collect(), d.decision))
            .collect();
        assert_eq!(
            got,
            [
                (vec![10], Decision::Keep),
                (vec![11, 12], Decision::Fix),
                (vec![13], Decision::Keep),
            ]
        );
    }

    #[test]
    fn ordinals_read_as_words_do() {
        let got: Vec<String> = [1, 2, 3, 4, 11, 12, 13, 21, 22, 103].map(ordinal).to_vec();
        assert_eq!(
            got,
            ["1st", "2nd", "3rd", "4th", "11th", "12th", "13th", "21st", "22nd", "103rd"]
        );
    }

    #[test]
    fn the_comments_and_the_file_read_what_they_can() {
        let text = r#"[{"id": 5, "kind": "inline", "author": "a", "path": "f.rb", "line": 3, "text": {"untrusted": "Rename it"}},
                       {"id": 6, "kind": "review", "author": "b", "text": "Plain"}, {"kind": "inline"}]"#;
        let got = comments(text);
        assert_eq!(got.len(), 2);
        assert_eq!(got[0].text, "Rename it");
        assert_eq!(got[1].text, "Plain");
        assert_eq!(got[1].path, None);
        assert_eq!(parse(None), Refusals::default());
        assert_eq!(parse(Some("not json")), Refusals::default());
        let tracks = round(&[], &[verdict(5, "incorrect", "no")], &got, 1);
        assert_eq!(tracks[0].request, "Rename it");
        let file = Refusals {
            rounds: vec!["turn-1".into()],
            requests: tracks,
        };
        assert_eq!(parse(Some(&render(&file))), file);
    }

    #[test]
    fn a_second_pass_over_one_turn_counts_nothing_new() {
        let c = inline(10, "a", "f.rb", 1);
        let refused = [verdict(10, "incorrect", "no")];
        let once = count_round(
            &Refusals::default(),
            &refused,
            std::slice::from_ref(&c),
            &[],
            "turn-1",
            "2026-11-24T10:01:00Z",
        );
        assert_eq!(once.requests[0].count, 1);
        assert_eq!(once.rounds, ["turn-1"]);
        let twice = count_round(
            &once,
            &refused,
            std::slice::from_ref(&c),
            &[],
            "turn-1",
            "2026-11-24T10:02:00Z",
        );
        assert_eq!(twice, once);
        let next = count_round(&once, &refused, &[c], &[], "turn-2", "2026-11-24T10:03:00Z");
        assert_eq!(next.requests[0].count, 2);
        assert_eq!(next.rounds, ["turn-1", "turn-2"]);
    }
}
