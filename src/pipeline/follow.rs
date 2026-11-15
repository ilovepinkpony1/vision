//! The findings of a gate cr followed across the rounds of a ticket (section 11.3), as pure
//! functions.
//!
//! - [`fingerprint`]: the path of a finding and a hash of its normalized title.
//! - [`follow`]: the track of each finding of a new round. Two findings of two rounds are the
//!   same when their fingerprints are equal, or when their paths are equal and their lines are
//!   at most [`NEAR_LINES`] apart. A finding that shows again after a round where it was gone
//!   comes back.
//! - [`converges`]: whether a loop of failed turns converges, for `extra_rounds` of a step.
//!
//! The ledger part is in [`crate::ledger::review`], and the tick writes it with the end facts
//! of a turn.

use crate::util::ids;

/// Two findings on the same path are the same when their lines are at most this far apart.
pub const NEAR_LINES: i64 = 3;
/// A finding that came back this many times after its fix becomes a notice for the person.
pub const NOTICE_AFTER: i64 = 2;
/// The longest title that the ledger keeps.
pub const TITLE_MAX: usize = 200;

/// The title of a finding for the fingerprint: lower case, with no digit and no punctuation,
/// each run of white space as one space.
pub fn normalize(title: &str) -> String {
    title
        .to_lowercase()
        .chars()
        .map(|c| if c.is_alphabetic() { c } else { ' ' })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// The fingerprint of a finding: its path, `#`, and the first 16 hex characters of the SHA-256
/// of its normalized title, such as `app/order.rb#1f2e3d4c5b6a7980`.
pub fn fingerprint(path: &str, title: &str) -> String {
    let hash = ids::sha256_hex(normalize(title).as_bytes());
    format!("{path}#{}", &hash[..16])
}

/// A text cut to `max` characters, with `...` when it was cut.
pub fn cut_title(text: &str, max: usize) -> String {
    let text = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if text.chars().count() <= max {
        return text;
    }
    let mut out: String = text.chars().take(max.saturating_sub(3)).collect();
    out.push_str("...");
    out
}

/// A finding that the rounds of a ticket follow, as the ledger keeps it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Track {
    pub id: i64,
    pub fingerprint: String,
    pub path: String,
    pub line: Option<i64>,
    pub first_round: i64,
    pub last_round: i64,
    pub came_back: i64,
    /// True when the person already got the notice that it keeps coming back.
    pub noticed: bool,
}

/// One finding of the new round.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Seen {
    pub fingerprint: String,
    pub path: String,
    pub line: Option<i64>,
}

/// What [`follow`] gives for one finding of the new round, in the order of the findings.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Followed {
    /// The index of its track in the list of tracks, or `None` for a new finding.
    pub track: Option<usize>,
    /// True when an earlier round had the finding.
    pub seen_before: bool,
    /// True when the finding shows again after a round where it was gone.
    pub came_back: bool,
    /// The number of times it came back, with this round.
    pub times: i64,
    /// True when this round makes it a notice: it came back [`NOTICE_AFTER`] times, and the
    /// person has no notice of it yet.
    pub notice: bool,
}

/// True when two findings are the same: equal fingerprints, or equal paths with lines at most
/// [`NEAR_LINES`] apart.
pub fn same(track: &Track, seen: &Seen) -> bool {
    if track.fingerprint == seen.fingerprint {
        return true;
    }
    track.path == seen.path
        && matches!((track.line, seen.line), (Some(a), Some(b)) if (a - b).abs() <= NEAR_LINES)
}

/// The track of each finding of the round `round`. Each track takes at most one finding of a
/// round: a finding with the same fingerprint first, then the nearest line on the same path.
pub fn follow(tracks: &[Track], round: i64, seen: &[Seen]) -> Vec<Followed> {
    let mut taken = vec![false; tracks.len()];
    let mut chosen: Vec<Option<usize>> = vec![None; seen.len()];
    // The equal fingerprints first, so that a near line never takes the track of a finding
    // that is named the same.
    for (i, finding) in seen.iter().enumerate() {
        if let Some(t) =
            (0..tracks.len()).find(|t| !taken[*t] && tracks[*t].fingerprint == finding.fingerprint)
        {
            taken[t] = true;
            chosen[i] = Some(t);
        }
    }
    for (i, finding) in seen.iter().enumerate() {
        if chosen[i].is_some() {
            continue;
        }
        let near = (0..tracks.len())
            .filter(|t| !taken[*t] && same(&tracks[*t], finding))
            .min_by_key(|t| match (tracks[*t].line, finding.line) {
                (Some(a), Some(b)) => (a - b).abs(),
                _ => i64::MAX,
            });
        if let Some(t) = near {
            taken[t] = true;
            chosen[i] = Some(t);
        }
    }
    chosen
        .into_iter()
        .map(|track| match track {
            None => Followed {
                track: None,
                seen_before: false,
                came_back: false,
                times: 0,
                notice: false,
            },
            Some(t) => {
                let old = &tracks[t];
                let came_back = old.last_round < round - 1;
                let times = old.came_back + i64::from(came_back);
                Followed {
                    track: Some(t),
                    seen_before: true,
                    came_back,
                    times,
                    notice: came_back && times >= NOTICE_AFTER && !old.noticed,
                }
            }
        })
        .collect()
}

/// The counts of one gate in one turn, for [`converges`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct GateCount {
    /// The items that fail the gate, or `None` when the gate cannot read its document.
    pub failing: Option<i64>,
    /// True when a failing item is CRITICAL (cr) or critical (qa).
    pub critical: bool,
}

/// Why one more turn is granted to a loop that converges, or `None` when it does not: the
/// failing items of the gate in the last turn are fewer than in the turn before, and no failing
/// item is critical. A turn whose document the gate cannot read gives no number, so the
/// loop does not converge.
pub fn converges(gate: &str, before: Option<GateCount>, last: Option<GateCount>) -> Option<String> {
    let (before, last) = (before?, last?);
    let (was, now) = (before.failing?, last.failing?);
    if last.critical || now >= was {
        return None;
    }
    Some(format!(
        "the gate {gate} failed on {now} item(s), fewer than the {was} of the turn before, and none is critical"
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn track(id: i64, path: &str, line: i64, title: &str, last: i64) -> Track {
        Track {
            id,
            fingerprint: fingerprint(path, title),
            path: path.into(),
            line: Some(line),
            first_round: 1,
            last_round: last,
            came_back: 0,
            noticed: false,
        }
    }

    fn seen(path: &str, line: i64, title: &str) -> Seen {
        Seen {
            fingerprint: fingerprint(path, title),
            path: path.into(),
            line: Some(line),
        }
    }

    #[test]
    fn the_fingerprint_reads_no_case_digit_or_punctuation() {
        assert_eq!(
            normalize("  A retry SENDS the report 2 times, again!  "),
            "a retry sends the report times again"
        );
        assert_eq!(
            fingerprint("app/a.rb", "Retry sends twice (line 42)."),
            fingerprint("app/a.rb", "retry sends twice line")
        );
        assert_ne!(
            fingerprint("app/a.rb", "Retry sends twice"),
            fingerprint("app/b.rb", "Retry sends twice")
        );
        assert!(fingerprint("app/a.rb", "x").starts_with("app/a.rb#"));
        assert_eq!(fingerprint("a", "x").len(), "a#".len() + 16);
    }

    #[test]
    fn a_finding_is_the_same_by_fingerprint_or_by_a_near_line() {
        let old = track(1, "app/a.rb", 40, "Retry sends twice", 1);
        assert!(same(&old, &seen("app/a.rb", 90, "Retry sends twice")));
        assert!(same(&old, &seen("app/a.rb", 43, "A new title")));
        assert!(same(&old, &seen("app/a.rb", 37, "A new title")));
        assert!(!same(&old, &seen("app/a.rb", 44, "A new title")));
        assert!(!same(&old, &seen("app/b.rb", 40, "A new title")));
        let mut no_line = seen("app/a.rb", 40, "A new title");
        no_line.line = None;
        assert!(!same(&old, &no_line));
    }

    #[test]
    fn a_finding_that_shows_again_after_a_gap_comes_back() {
        let tracks = vec![
            track(1, "app/a.rb", 40, "Retry sends twice", 2),
            track(2, "app/b.rb", 10, "A typo", 1),
        ];
        let round = [
            seen("app/a.rb", 41, "Retry sends the mail twice"),
            seen("app/b.rb", 10, "A typo"),
            seen("app/c.rb", 1, "New"),
        ];
        let out = follow(&tracks, 3, &round);
        assert_eq!(out[0].track, Some(0));
        assert!(out[0].seen_before && !out[0].came_back);
        assert_eq!(out[1].track, Some(1));
        assert!(out[1].came_back, "gone in round 2, back in round 3");
        assert_eq!(out[1].times, 1);
        assert!(!out[1].notice);
        assert_eq!(out[2].track, None);
        assert!(!out[2].seen_before);
    }

    #[test]
    fn the_second_return_is_a_notice_once() {
        let mut old = track(1, "app/a.rb", 40, "Retry sends twice", 3);
        old.came_back = 1;
        let out = follow(
            std::slice::from_ref(&old),
            5,
            &[seen("app/a.rb", 40, "Retry")],
        );
        assert!(out[0].came_back && out[0].notice);
        assert_eq!(out[0].times, 2);
        old.noticed = true;
        old.came_back = 2;
        old.last_round = 5;
        let out = follow(
            std::slice::from_ref(&old),
            7,
            &[seen("app/a.rb", 40, "Retry")],
        );
        assert!(out[0].came_back && !out[0].notice, "one notice is enough");
    }

    #[test]
    fn each_track_takes_one_finding_and_a_fingerprint_wins_over_a_near_line() {
        let tracks = vec![track(1, "app/a.rb", 40, "Retry sends twice", 1)];
        let round = [
            seen("app/a.rb", 41, "Another finding"),
            seen("app/a.rb", 80, "Retry sends twice"),
        ];
        let out = follow(&tracks, 2, &round);
        assert_eq!(out[0].track, None);
        assert_eq!(out[1].track, Some(0));
    }

    #[test]
    fn a_loop_converges_while_its_failing_items_get_fewer_and_none_is_critical() {
        let count = |n: i64, critical: bool| {
            Some(GateCount {
                failing: Some(n),
                critical,
            })
        };
        assert!(converges("cr", count(3, false), count(2, false))
            .unwrap()
            .contains("2 item(s), fewer than the 3"));
        assert_eq!(converges("cr", count(2, false), count(2, false)), None);
        assert_eq!(converges("cr", count(2, false), count(3, false)), None);
        assert_eq!(converges("qa", count(3, false), count(1, true)), None);
        assert_eq!(converges("cr", None, count(1, false)), None);
        let unread = Some(GateCount {
            failing: None,
            critical: false,
        });
        assert_eq!(converges("cr", count(3, false), unread), None);
    }

    #[test]
    fn a_title_is_cut_with_a_mark() {
        assert_eq!(cut_title("a  b\nc", 10), "a b c");
        let long = "x".repeat(250);
        let cut = cut_title(&long, TITLE_MAX);
        assert_eq!(cut.chars().count(), TITLE_MAX);
        assert!(cut.ends_with("..."));
    }
}
