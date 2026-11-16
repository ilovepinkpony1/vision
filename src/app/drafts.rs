//! The drafts of the question form: one small JSON file for each ticket,
//! `tickets/<KEY>/answer-drafts.json`, in the form that `vision answer --file` reads. The app
//! writes it after each change of a draft, through the atomic write of the repo, so the drafts
//! survive a close of the form and a restart of the app. The send of a set removes the drafts
//! of its questions, and a file with no draft left goes away.
//!
//! A file that is missing, cannot be read, or does not parse counts as no drafts, and an entry
//! that is not valid is left out. The next write replaces such a file.

use std::collections::BTreeMap;
use std::path::PathBuf;

use anyhow::Result;

use super::needs::Draft;
use crate::paths::{Paths, TicketKey};
use crate::questions::set::{parse_entries, render_entries};
use crate::util::fs::{atomic_write, ensure_dir};

/// The name of the drafts file in the folder of a ticket.
pub const FILE: &str = "answer-drafts.json";

/// The name of the file of one send, in the folder of a ticket. The worker writes it before
/// `vision answer --file` runs, and removes it after.
pub const SEND_FILE: &str = "answer-send.json";

/// The drafts file of a ticket.
pub fn path(paths: &Paths, key: &TicketKey) -> PathBuf {
    paths.ticket_doc(key, FILE)
}

/// The drafts of a ticket, by question id.
pub fn read(paths: &Paths, ticket: &str) -> BTreeMap<String, Draft> {
    let Ok(key) = TicketKey::parse(ticket) else {
        return BTreeMap::new();
    };
    let Ok(text) = std::fs::read_to_string(path(paths, &key)) else {
        return BTreeMap::new();
    };
    let Ok(entries) = parse_entries(&text) else {
        return BTreeMap::new();
    };
    entries
        .iter()
        .filter_map(|(id, entry)| Draft::of_entry(entry).map(|d| (id.clone(), d)))
        .collect()
}

/// The text of a file of answers for these drafts: the drafts file, or the file of a send.
pub fn text(drafts: &[(String, Draft)]) -> String {
    let entries = drafts
        .iter()
        .map(|(id, draft)| (id.clone(), draft.entry()))
        .collect();
    render_entries(&entries)
}

/// Writes the drafts of a ticket in place of its file. With no draft, the file goes away.
pub fn save(paths: &Paths, ticket: &str, drafts: &[(String, Draft)]) -> Result<()> {
    let key = TicketKey::parse(ticket)?;
    let file = path(paths, &key);
    if drafts.is_empty() {
        return match std::fs::remove_file(&file) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e.into()),
        };
    }
    ensure_dir(&paths.ticket_dir(&key))?;
    atomic_write(&file, text(drafts).as_bytes())
}

/// Removes the drafts of these questions from the file of a ticket.
pub fn drop(paths: &Paths, ticket: &str, ids: &[String]) -> Result<()> {
    let left: Vec<(String, Draft)> = read(paths, ticket)
        .into_iter()
        .filter(|(id, _)| !ids.contains(id))
        .collect();
    save(paths, ticket, &left)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::needs::Reply;

    fn home() -> (tempfile::TempDir, Paths) {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::new(dir.path());
        (dir, paths)
    }

    #[test]
    fn the_drafts_file_keeps_the_drafts_and_a_drop_removes_some() {
        let (_dir, paths) = home();
        assert!(read(&paths, "DEMO-12").is_empty());
        let drafts = vec![
            (
                "Q-0001".to_string(),
                Draft::Answer {
                    reply: Reply::Choice(1),
                    why: "People compare.".into(),
                },
            ),
            ("Q-0002".to_string(), Draft::Needless("code".into())),
        ];
        save(&paths, "DEMO-12", &drafts).unwrap();
        let file = path(&paths, &TicketKey::parse("DEMO-12").unwrap());
        let text = std::fs::read_to_string(&file).unwrap();
        assert!(text.contains("\"choice\": 2"), "{text}");
        assert_eq!(
            read(&paths, "DEMO-12").into_iter().collect::<Vec<_>>(),
            drafts
        );
        drop(&paths, "DEMO-12", &["Q-0001".to_string()]).unwrap();
        assert_eq!(
            read(&paths, "DEMO-12").keys().cloned().collect::<Vec<_>>(),
            ["Q-0002"]
        );
        drop(&paths, "DEMO-12", &["Q-0002".to_string()]).unwrap();
        assert!(!file.exists(), "a file with no draft goes away");
        // A file that does not parse counts as no drafts.
        std::fs::write(&file, "not json").unwrap();
        assert!(read(&paths, "DEMO-12").is_empty());
        assert!(save(&paths, "../x", &drafts).is_err());
    }
}
