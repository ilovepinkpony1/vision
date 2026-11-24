//! The sections of the tab "To take" of Home: one section for each status of the tracker.
//! The statuses of `app.sections` of the configuration come first, in that order. Each other
//! status follows, in the order of the queue. The tickets of one status keep the order of the
//! queue, so the order of the rows is the order of the arrows too.

use std::ops::Range;

use crate::config::same_status;
use crate::pipeline::commands::PickRow;

/// One section: the status as the tracker writes it, and the part of the ordered rows that
/// has that status.
#[derive(Clone, Debug, PartialEq)]
pub struct Section {
    pub status: String,
    pub rows: Range<usize>,
}

/// Puts the rows in the order of the sections. Rows with the same status keep their order.
pub fn order(rows: &mut Vec<&PickRow>, first: &[String]) {
    let mut statuses: Vec<String> = first.to_vec();
    for row in rows.iter() {
        if !statuses.iter().any(|s| same_status(s, &row.status)) {
            statuses.push(row.status.clone());
        }
    }
    let rank = |row: &PickRow| {
        statuses
            .iter()
            .position(|s| same_status(s, &row.status))
            .unwrap_or(usize::MAX)
    };
    rows.sort_by_key(|row| rank(row));
}

/// The sections of rows that are in the order of [`order`]: each run of rows with the same
/// status. The title of a section is the status of its first row, and a row with no status
/// shows under `no status`.
pub fn sections(rows: &[&PickRow]) -> Vec<Section> {
    let mut out: Vec<Section> = Vec::new();
    for (at, row) in rows.iter().enumerate() {
        match out.last_mut() {
            Some(last) if same_status(&rows[last.rows.start].status, &row.status) => {
                last.rows.end = at + 1;
            }
            _ => {
                let status = row.status.trim();
                out.push(Section {
                    status: if status.is_empty() {
                        "no status".to_string()
                    } else {
                        status.to_string()
                    },
                    rows: at..at + 1,
                });
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(key: &str, status: &str) -> PickRow {
        PickRow {
            key: key.into(),
            title: String::new(),
            status: status.into(),
            eligible: true,
            reason: String::new(),
            taken: None,
            branch: None,
            pr: None,
        }
    }

    fn keys(rows: &[&PickRow]) -> Vec<String> {
        rows.iter().map(|r| r.key.clone()).collect()
    }

    #[test]
    fn the_statuses_of_the_configuration_come_first_and_the_others_keep_the_queue_order() {
        let all = [
            row("A-1", "In Testing"),
            row("A-2", "To Do"),
            row("A-3", "in development"),
            row("A-4", "Backlog"),
            row("A-5", "In Testing"),
            row("A-6", "To Do"),
            row("A-7", "In Development"),
        ];
        let mut rows: Vec<&PickRow> = all.iter().collect();
        let first = vec!["In Development".to_string(), " In Testing ".to_string()];
        order(&mut rows, &first);
        assert_eq!(
            keys(&rows),
            ["A-3", "A-7", "A-1", "A-5", "A-2", "A-6", "A-4"]
        );
        assert_eq!(
            sections(&rows),
            vec![
                Section {
                    status: "in development".into(),
                    rows: 0..2
                },
                Section {
                    status: "In Testing".into(),
                    rows: 2..4
                },
                Section {
                    status: "To Do".into(),
                    rows: 4..6
                },
                Section {
                    status: "Backlog".into(),
                    rows: 6..7
                },
            ]
        );
    }

    #[test]
    fn with_no_configuration_each_status_is_a_section_in_the_order_of_the_queue() {
        let all = [
            row("A-1", "To Do"),
            row("A-2", "In Review"),
            row("A-3", "To Do"),
            row("A-4", ""),
        ];
        let mut rows: Vec<&PickRow> = all.iter().collect();
        order(&mut rows, &[]);
        assert_eq!(keys(&rows), ["A-1", "A-3", "A-2", "A-4"]);
        let titles: Vec<String> = sections(&rows).into_iter().map(|s| s.status).collect();
        assert_eq!(titles, ["To Do", "In Review", "no status"]);
    }

    #[test]
    fn a_status_of_the_configuration_with_no_ticket_gives_no_section() {
        let all = [row("A-1", "To Do")];
        let mut rows: Vec<&PickRow> = all.iter().collect();
        order(&mut rows, &["In Regression".to_string()]);
        assert_eq!(
            sections(&rows),
            vec![Section {
                status: "To Do".into(),
                rows: 0..1
            }]
        );
        assert!(sections(&[]).is_empty());
    }
}
