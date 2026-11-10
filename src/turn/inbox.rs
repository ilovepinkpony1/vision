//! Message files (section 7.5): `{"text": "...", "source": "user", "at": "..."}`, written to a
//! temporary name and then renamed. The inbox of a running turn is `inbox/` of its folder. A
//! message for the next turn of a ticket waits in `tickets/<KEY>/inbox/`.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::paths::{Paths, TicketKey};
use crate::util::fs::{atomic_write, ensure_dir};

/// One message file.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct MessageFile {
    pub text: String,
    pub source: String,
    pub at: String,
    /// The id of its row in `messages`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<i64>,
}

/// The folder of the messages that wait for the next turn of a ticket.
pub fn ticket_inbox(paths: &Paths, key: &TicketKey) -> PathBuf {
    paths.ticket_dir(key).join("inbox")
}

/// One line of standard input of a session (section 7.4).
pub fn input_line(text: &str) -> String {
    json!({"type": "user", "message": {"role": "user", "content": [{"type": "text", "text": text}]}})
        .to_string()
}

/// Writes a message file into `dir`, named so that the files sort in the order of their ids.
pub fn write(dir: &Path, message: &MessageFile) -> Result<PathBuf> {
    ensure_dir(dir)?;
    let name = match message.id {
        Some(id) => format!("m-{id:012}.json"),
        None => format!(
            "m-t{}.json",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ),
    };
    let path = dir.join(name);
    atomic_write(&path, serde_json::to_string(message)?.as_bytes())?;
    Ok(path)
}

/// The message files that wait in `dir`, in order. A file that is not a valid message is
/// renamed to `<name>.bad`, so that it never blocks the others.
pub fn waiting(dir: &Path) -> Result<Vec<(PathBuf, MessageFile)>> {
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e).with_context(|| format!("could not read {}", dir.display())),
    };
    let mut paths: Vec<PathBuf> = entries
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| {
            p.is_file()
                && p.extension().is_some_and(|e| e == "json")
                && !p
                    .file_name()
                    .is_some_and(|n| n.to_string_lossy().starts_with('.'))
        })
        .collect();
    paths.sort();
    let mut out = Vec::new();
    for path in paths {
        let parsed = fs::read_to_string(&path)
            .ok()
            .and_then(|text| serde_json::from_str::<MessageFile>(&text).ok());
        match parsed {
            Some(message) => out.push((path, message)),
            None => {
                let mut bad = path.clone().into_os_string();
                bad.push(".bad");
                let _ = fs::rename(&path, bad);
            }
        }
    }
    Ok(out)
}

/// Moves a message file into the folder `to`, keeping its name.
pub fn move_to(path: &Path, to: &Path) -> Result<PathBuf> {
    ensure_dir(to)?;
    let name = path
        .file_name()
        .with_context(|| format!("{} has no file name", path.display()))?;
    let target = to.join(name);
    fs::rename(path, &target)
        .with_context(|| format!("could not move {} to {}", path.display(), to.display()))?;
    Ok(target)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn message(id: i64, text: &str) -> MessageFile {
        MessageFile {
            text: text.into(),
            source: "user".into(),
            at: "2026-11-16T14:30:00Z".into(),
            id: Some(id),
        }
    }

    #[test]
    fn messages_wait_in_order_and_move() {
        let dir = tempfile::tempdir().unwrap();
        let inbox = dir.path().join("inbox");
        write(&inbox, &message(10, "second")).unwrap();
        write(&inbox, &message(9, "first")).unwrap();
        fs::write(inbox.join("broken.json"), "{").unwrap();
        let waiting_now = waiting(&inbox).unwrap();
        let texts: Vec<_> = waiting_now.iter().map(|(_, m)| m.text.as_str()).collect();
        assert_eq!(texts, ["first", "second"]);
        assert!(inbox.join("broken.json.bad").exists());
        move_to(&waiting_now[0].0, &inbox.join("sent")).unwrap();
        assert_eq!(waiting(&inbox).unwrap().len(), 1);
        assert!(inbox.join("sent/m-000000000009.json").exists());
    }

    #[test]
    fn an_input_line_is_one_line_of_stream_json() {
        let line = input_line("use the old export\nplease");
        assert!(!line.contains('\n'));
        let value: serde_json::Value = serde_json::from_str(&line).unwrap();
        assert_eq!(value["type"], "user");
        assert_eq!(
            value["message"]["content"][0]["text"],
            "use the old export\nplease"
        );
    }
}
