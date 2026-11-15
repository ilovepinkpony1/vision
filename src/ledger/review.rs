//! The queries of the quality of the review (migration 7): the counts of each gate of a turn,
//! the findings of a gate cr that the rounds of a ticket follow, and the escape checks against
//! the review bot of the team.

use anyhow::Result;
use rusqlite::{params, OptionalExtension, Row};
use serde::Serialize;

use super::Ledger;
use crate::pipeline::follow::{GateCount, Track};

/// One row of `gate_rounds`: one gate of one turn whose end facts were tested.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct GateRoundRow {
    pub turn: String,
    pub ticket: String,
    pub step: String,
    /// `cr` or `qa`.
    pub gate: String,
    pub doc: String,
    /// The rounds of this kind of gate in the ticket, from 1.
    pub round: i64,
    /// The last commit of the clone when the gate read its document.
    pub head: Option<String>,
    pub passed: bool,
    /// The items that fail the gate, or `None` when the gate cannot read each item.
    pub failing: Option<i64>,
    /// True when a failing item is CRITICAL (cr) or critical (qa).
    pub critical: bool,
    /// The findings or issues of the document.
    pub findings: i64,
    pub at: String,
}

impl GateRoundRow {
    /// The counts that [`crate::pipeline::follow::converges`] reads.
    pub fn count(&self) -> GateCount {
        GateCount {
            failing: self.failing,
            critical: self.critical,
        }
    }
}

const GATE_COLUMNS: &str =
    "turn, ticket, step, gate, doc, round, head, passed, failing, critical, findings, at";

fn gate_round(row: &Row<'_>) -> rusqlite::Result<GateRoundRow> {
    Ok(GateRoundRow {
        turn: row.get(0)?,
        ticket: row.get(1)?,
        step: row.get::<_, Option<String>>(2)?.unwrap_or_default(),
        gate: row.get(3)?,
        doc: row.get(4)?,
        round: row.get(5)?,
        head: row.get(6)?,
        passed: row.get::<_, Option<i64>>(7)?.unwrap_or(0) != 0,
        failing: row.get(8)?,
        critical: row.get::<_, Option<i64>>(9)?.unwrap_or(0) != 0,
        findings: row.get::<_, Option<i64>>(10)?.unwrap_or(0),
        at: row.get::<_, Option<String>>(11)?.unwrap_or_default(),
    })
}

/// A finding that the rounds of a ticket follow, with what the person reads of it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TrackRow {
    pub track: Track,
    pub severity: Option<String>,
    pub title: Option<String>,
}

/// One row of `escape_checks`.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct EscapeCheckRow {
    pub id: i64,
    pub ticket: String,
    pub pr: Option<i64>,
    pub reviewer: String,
    /// The turn whose gate cr passed.
    pub pass_turn: String,
    /// The head of that pass.
    pub head: Option<String>,
    pub passed_at: Option<String>,
    /// The time of the last look at the reviews of the pull request.
    pub looked: Option<String>,
    /// The first review of the reviewer after the pass, once it exists.
    pub review_id: Option<i64>,
    pub submitted: Option<String>,
    /// The time of the check, or `None` while no such review exists.
    pub checked: Option<String>,
    /// The inline comments of the review.
    pub comments: i64,
    /// The comments of a counted priority.
    pub counted: i64,
    pub escapes: i64,
}

const CHECK_COLUMNS: &str =
    "id, ticket, pr, reviewer, pass_turn, head, passed_at, looked, review_id, submitted,
     checked, coalesce(comments, 0), coalesce(counted, 0), coalesce(escapes, 0)";

fn check_row(row: &Row<'_>) -> rusqlite::Result<EscapeCheckRow> {
    Ok(EscapeCheckRow {
        id: row.get(0)?,
        ticket: row.get(1)?,
        pr: row.get(2)?,
        reviewer: row.get(3)?,
        pass_turn: row.get(4)?,
        head: row.get(5)?,
        passed_at: row.get(6)?,
        looked: row.get(7)?,
        review_id: row.get(8)?,
        submitted: row.get(9)?,
        checked: row.get(10)?,
        comments: row.get(11)?,
        counted: row.get(12)?,
        escapes: row.get(13)?,
    })
}

/// One row of `escapes`, with its check.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct EscapeRow {
    pub id: i64,
    pub check_id: i64,
    pub ticket: String,
    pub reviewer: String,
    pub path: Option<String>,
    pub line: Option<i64>,
    /// The priority that the regex of `[escapes]` read, such as `1`.
    pub priority: Option<String>,
    pub title: Option<String>,
    pub url: Option<String>,
    pub checked: Option<String>,
}

/// An escape to record.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct NewEscape {
    pub path: Option<String>,
    pub line: Option<i64>,
    pub priority: Option<String>,
    pub title: Option<String>,
    pub url: Option<String>,
}

impl Ledger {
    /// The round number of the next gate of the kind `gate` in a ticket: one more than the
    /// last one.
    pub fn next_gate_round(&self, ticket: &str, gate: &str) -> Result<i64> {
        let last: Option<i64> = self.conn().query_row(
            "SELECT max(round) FROM gate_rounds WHERE ticket = ?1 AND gate = ?2",
            params![ticket, gate],
            |row| row.get(0),
        )?;
        Ok(last.unwrap_or(0) + 1)
    }

    /// Records one gate of one turn. Gives false, and records nothing, when the turn already has
    /// a row for that document.
    pub fn add_gate_round(&self, row: &GateRoundRow) -> Result<bool> {
        Ok(self.conn().execute(
            &format!(
                "INSERT OR IGNORE INTO gate_rounds ({GATE_COLUMNS})
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)"
            ),
            params![
                row.turn,
                row.ticket,
                row.step,
                row.gate,
                row.doc,
                row.round,
                row.head,
                i64::from(row.passed),
                row.failing,
                i64::from(row.critical),
                row.findings,
                row.at
            ],
        )? == 1)
    }

    /// The gate of a turn that read the document `doc`.
    pub fn gate_round(&self, turn: &str, doc: &str) -> Result<Option<GateRoundRow>> {
        Ok(self
            .conn()
            .query_row(
                &format!("SELECT {GATE_COLUMNS} FROM gate_rounds WHERE turn = ?1 AND doc = ?2"),
                params![turn, doc],
                gate_round,
            )
            .optional()?)
    }

    /// The gates of the kind `gate` of a ticket, oldest first.
    pub fn gate_rounds_of(&self, ticket: &str, gate: &str) -> Result<Vec<GateRoundRow>> {
        let mut stmt = self.conn().prepare(&format!(
            "SELECT {GATE_COLUMNS} FROM gate_rounds WHERE ticket = ?1 AND gate = ?2 ORDER BY round, rowid"
        ))?;
        let rows = stmt.query_map(params![ticket, gate], gate_round)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// The findings of a gate cr that the rounds of a ticket follow, oldest first.
    pub fn finding_tracks(&self, ticket: &str) -> Result<Vec<TrackRow>> {
        let mut stmt = self.conn().prepare(
            "SELECT id, fingerprint, path, line, first_round, last_round, came_back, noticed, severity, title
             FROM finding_tracks WHERE ticket = ?1 ORDER BY id",
        )?;
        let rows = stmt.query_map([ticket], |row| {
            Ok(TrackRow {
                track: Track {
                    id: row.get(0)?,
                    fingerprint: row.get(1)?,
                    path: row.get(2)?,
                    line: row.get(3)?,
                    first_round: row.get(4)?,
                    last_round: row.get(5)?,
                    came_back: row.get(6)?,
                    noticed: row.get::<_, Option<String>>(7)?.is_some(),
                },
                severity: row.get(8)?,
                title: row.get(9)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Records a new finding that the rounds of a ticket follow. Gives its id.
    pub fn add_track(
        &self,
        ticket: &str,
        track: &Track,
        severity: Option<&str>,
        title: &str,
    ) -> Result<i64> {
        self.conn().execute(
            "INSERT INTO finding_tracks (ticket, fingerprint, path, line, severity, title, first_round, last_round, came_back)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![
                ticket,
                track.fingerprint,
                track.path,
                track.line,
                severity,
                title,
                track.first_round,
                track.last_round,
                track.came_back
            ],
        )?;
        Ok(self.conn().last_insert_rowid())
    }

    /// Writes the round of a finding that showed again: its line, its last round, and the
    /// times that it came back. `noticed` sets the time of its notice once.
    pub fn update_track(
        &self,
        id: i64,
        line: Option<i64>,
        last_round: i64,
        came_back: i64,
        noticed: Option<&str>,
    ) -> Result<()> {
        self.conn().execute(
            "UPDATE finding_tracks SET line = coalesce(?2, line), last_round = ?3, came_back = ?4,
                 noticed = coalesce(noticed, ?5)
             WHERE id = ?1",
            params![id, line, last_round, came_back, noticed],
        )?;
        Ok(())
    }

    /// The escape check of a reviewer for one pass of the gate cr.
    pub fn escape_check(
        &self,
        ticket: &str,
        reviewer: &str,
        pass_turn: &str,
    ) -> Result<Option<EscapeCheckRow>> {
        Ok(self
            .conn()
            .query_row(
                &format!(
                    "SELECT {CHECK_COLUMNS} FROM escape_checks
                     WHERE ticket = ?1 AND reviewer = ?2 AND pass_turn = ?3"
                ),
                params![ticket, reviewer, pass_turn],
                check_row,
            )
            .optional()?)
    }

    /// Starts the escape check of a reviewer for one pass, or gives the one that exists.
    pub fn open_escape_check(&self, row: &EscapeCheckRow) -> Result<EscapeCheckRow> {
        self.conn().execute(
            "INSERT OR IGNORE INTO escape_checks (ticket, pr, reviewer, pass_turn, head, passed_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                row.ticket,
                row.pr,
                row.reviewer,
                row.pass_turn,
                row.head,
                row.passed_at
            ],
        )?;
        self.escape_check(&row.ticket, &row.reviewer, &row.pass_turn)?
            .ok_or_else(|| anyhow::anyhow!("the escape check of {} was not recorded", row.ticket))
    }

    /// Writes the time of a look that found no review yet.
    pub fn set_escape_looked(&self, id: i64, at: &str) -> Result<()> {
        self.conn().execute(
            "UPDATE escape_checks SET looked = ?2 WHERE id = ?1 AND checked IS NULL",
            params![id, at],
        )?;
        Ok(())
    }

    /// Records the check of a review with its escapes. Gives false, and records nothing, when
    /// the check already has a review.
    pub fn finish_escape_check(
        &self,
        check: &EscapeCheckRow,
        escapes: &[NewEscape],
    ) -> Result<bool> {
        let changed = self.conn().execute(
            "UPDATE escape_checks SET pr = coalesce(?2, pr), looked = ?3, review_id = ?4, submitted = ?5,
                 checked = ?6, comments = ?7, counted = ?8, escapes = ?9
             WHERE id = ?1 AND checked IS NULL",
            params![
                check.id,
                check.pr,
                check.looked,
                check.review_id,
                check.submitted,
                check.checked,
                check.comments,
                check.counted,
                escapes.len() as i64
            ],
        )?;
        if changed != 1 {
            return Ok(false);
        }
        for escape in escapes {
            self.conn().execute(
                "INSERT INTO escapes (check_id, path, line, priority, title, url) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![
                    check.id,
                    escape.path,
                    escape.line,
                    escape.priority,
                    escape.title,
                    escape.url
                ],
            )?;
        }
        Ok(true)
    }

    /// The escape checks that found their review, with a check time at or after `since` when
    /// it is given, oldest first.
    pub fn escape_checks(&self, since: Option<&str>) -> Result<Vec<EscapeCheckRow>> {
        let mut stmt = self.conn().prepare(&format!(
            "SELECT {CHECK_COLUMNS} FROM escape_checks
             WHERE checked IS NOT NULL AND (?1 IS NULL OR checked >= ?1) ORDER BY checked, id"
        ))?;
        let rows = stmt.query_map(params![since], check_row)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// The escape checks of a ticket, each one, oldest first.
    pub fn escape_checks_of(&self, ticket: &str) -> Result<Vec<EscapeCheckRow>> {
        let mut stmt = self.conn().prepare(&format!(
            "SELECT {CHECK_COLUMNS} FROM escape_checks WHERE ticket = ?1 ORDER BY id"
        ))?;
        let rows = stmt.query_map([ticket], check_row)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// The escapes of the checks of one pass of the gate cr: the turn whose gate passed.
    pub fn escapes_of_turn(&self, turn: &str) -> Result<Vec<EscapeRow>> {
        self.escapes_where("c.pass_turn = ?1", params![turn])
    }

    /// The escapes of the checks with a check time at or after `since`, or of one ticket, oldest
    /// first.
    pub fn escapes(&self, ticket: Option<&str>, since: Option<&str>) -> Result<Vec<EscapeRow>> {
        self.escapes_where(
            "(?1 IS NULL OR c.ticket = ?1) AND (?2 IS NULL OR c.checked >= ?2)",
            params![ticket, since],
        )
    }

    fn escapes_where(&self, filter: &str, values: impl rusqlite::Params) -> Result<Vec<EscapeRow>> {
        let mut stmt = self.conn().prepare(&format!(
            "SELECT e.id, e.check_id, c.ticket, c.reviewer, e.path, e.line, e.priority, e.title, e.url, c.checked
             FROM escapes e JOIN escape_checks c ON c.id = e.check_id
             WHERE {filter} ORDER BY e.id"
        ))?;
        let rows = stmt.query_map(values, |row| {
            Ok(EscapeRow {
                id: row.get(0)?,
                check_id: row.get(1)?,
                ticket: row.get(2)?,
                reviewer: row.get(3)?,
                path: row.get(4)?,
                line: row.get(5)?,
                priority: row.get(6)?,
                title: row.get(7)?,
                url: row.get(8)?,
                checked: row.get(9)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_gate_round_is_written_once_and_numbered_by_ticket_and_kind() {
        let ledger = Ledger::open_in_memory().unwrap();
        assert_eq!(ledger.next_gate_round("DEMO-12", "cr").unwrap(), 1);
        let row = GateRoundRow {
            turn: "T1".into(),
            ticket: "DEMO-12".into(),
            gate: "cr".into(),
            doc: "cr.md".into(),
            round: 1,
            failing: Some(2),
            ..GateRoundRow::default()
        };
        assert!(ledger.add_gate_round(&row).unwrap());
        assert!(!ledger.add_gate_round(&row).unwrap());
        assert_eq!(ledger.next_gate_round("DEMO-12", "cr").unwrap(), 2);
        assert_eq!(ledger.next_gate_round("DEMO-12", "qa").unwrap(), 1);
        assert_eq!(ledger.gate_round("T1", "cr.md").unwrap(), Some(row));
        assert_eq!(ledger.gate_rounds_of("DEMO-12", "cr").unwrap().len(), 1);
    }

    #[test]
    fn a_track_keeps_its_rounds_and_its_notice() {
        let ledger = Ledger::open_in_memory().unwrap();
        let track = Track {
            fingerprint: "a.rb#1".into(),
            path: "a.rb".into(),
            line: Some(4),
            first_round: 1,
            last_round: 1,
            ..Track::default()
        };
        let id = ledger
            .add_track("DEMO-12", &track, Some("MAJOR"), "A title")
            .unwrap();
        ledger.update_track(id, None, 3, 1, None).unwrap();
        ledger.update_track(id, Some(6), 5, 2, Some("t5")).unwrap();
        ledger.update_track(id, None, 6, 2, Some("t6")).unwrap();
        let rows = ledger.finding_tracks("DEMO-12").unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].track.line, Some(6));
        assert_eq!(rows[0].track.last_round, 6);
        assert_eq!(rows[0].track.came_back, 2);
        assert!(rows[0].track.noticed);
        assert_eq!(rows[0].title.as_deref(), Some("A title"));
    }

    #[test]
    fn an_escape_check_opens_once_and_finishes_once() {
        let ledger = Ledger::open_in_memory().unwrap();
        let open = EscapeCheckRow {
            ticket: "DEMO-12".into(),
            pr: Some(7),
            reviewer: "bot[bot]".into(),
            pass_turn: "T1".into(),
            head: Some("abc".into()),
            ..EscapeCheckRow::default()
        };
        let first = ledger.open_escape_check(&open).unwrap();
        let again = ledger.open_escape_check(&open).unwrap();
        assert_eq!(first.id, again.id);
        ledger.set_escape_looked(first.id, "t1").unwrap();
        let mut done = ledger
            .escape_check("DEMO-12", "bot[bot]", "T1")
            .unwrap()
            .unwrap();
        assert_eq!(done.looked.as_deref(), Some("t1"));
        done.review_id = Some(55);
        done.checked = Some("t2".into());
        done.comments = 3;
        done.counted = 2;
        let escape = NewEscape {
            path: Some("app/a.rb".into()),
            line: Some(4),
            priority: Some("1".into()),
            title: Some("A miss".into()),
            url: None,
        };
        assert!(ledger
            .finish_escape_check(&done, std::slice::from_ref(&escape))
            .unwrap());
        assert!(!ledger.finish_escape_check(&done, &[escape]).unwrap());
        let checks = ledger.escape_checks(None).unwrap();
        assert_eq!(checks.len(), 1);
        assert_eq!(checks[0].escapes, 1);
        assert!(ledger.escape_checks(Some("t3")).unwrap().is_empty());
        let escapes = ledger.escapes(Some("DEMO-12"), None).unwrap();
        assert_eq!(escapes.len(), 1);
        assert_eq!(escapes[0].reviewer, "bot[bot]");
        assert!(ledger.escapes(Some("DEMO-13"), None).unwrap().is_empty());
        assert_eq!(ledger.escapes_of_turn("T1").unwrap().len(), 1);
        assert!(ledger.escapes_of_turn("T2").unwrap().is_empty());
    }
}
