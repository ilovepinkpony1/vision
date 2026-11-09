//! `vision init`: writes the home folder of section 3, `config.toml` with the defaults and the
//! comments of section 4, the ledger, and the memory repo with one first commit.
//!
//! Each part is made only when it is missing, so a second run changes nothing and says so,
//! and a run on a damaged home makes only the missing parts.

use std::path::PathBuf;

use anyhow::Result;
use serde_json::json;

use crate::config::{ConfigEditor, DEFAULT_TEXT};
use crate::ctx::Ctx;
use crate::ledger::{Event, Ledger, SCHEMA_VERSION};
use crate::paths::{tilde_display, Paths};
use crate::util::fs::{atomic_write, ensure_dir};
use crate::util::git::{self, Author};

/// The author of the first commit of the memory repo.
const INIT_AUTHOR: Author<'static> = Author {
    name: "vision",
    email: "vision@localhost",
};

/// `vision init`.
pub fn run(ctx: &Ctx) -> Result<()> {
    let paths = &ctx.paths;
    let mut made: Vec<String> = Vec::new();

    for (name, dir) in folders(paths) {
        if ensure_dir(&dir)? {
            made.push(name);
        }
    }
    if !paths.config().exists() {
        atomic_write(&paths.config(), default_config(paths)?.as_bytes())?;
        made.push("config.toml".into());
    }
    let existed = paths.ledger().exists();
    let ledger = Ledger::create(&paths.ledger())?;
    if !existed {
        made.push("ledger.sqlite".into());
    } else if let Some(from) = ledger.migrated_from() {
        made.push(format!(
            "ledger.sqlite schema version {SCHEMA_VERSION} (from {from})"
        ));
    }
    if !paths.memory_dir().join(".git").exists() {
        make_memory_repo(paths)?;
        made.push("memory/ as a git repo with one commit".into());
    }

    let home = paths.home().display();
    if made.is_empty() {
        println!("Vision is already set up at {home}. Nothing changed.");
        return Ok(());
    }
    ledger.add_event(&Event::now("user", "init").detail(json!({ "made": made })))?;
    ctx.log(&format!("init: made {}", made.join(", ")));
    println!("Set up Vision at {home}:");
    for part in &made {
        println!("  made {part}");
    }
    println!("Next: run `vision doctor` to test the setup.");
    Ok(())
}

/// The default configuration. `claude.config_dir` names the `claude/` folder of this home.
pub fn default_config(paths: &Paths) -> Result<String> {
    let mut editor = ConfigEditor::parse(DEFAULT_TEXT)?;
    let dir = tilde_display(&paths.claude_dir());
    if dir != "~/.vision/claude" {
        editor.set_claude_config_dir(&dir)?;
    }
    editor.check()?;
    Ok(editor.text())
}

fn folders(paths: &Paths) -> Vec<(String, PathBuf)> {
    vec![
        ("the home folder".into(), paths.home().to_path_buf()),
        ("log/".into(), paths.log_dir()),
        ("claude/".into(), paths.claude_dir()),
        ("sandbox/".into(), paths.sandbox_dir()),
        ("skills/".into(), paths.skills_dir()),
        ("adapters/".into(), paths.adapters_dir()),
        ("tickets/".into(), paths.tickets_dir()),
    ]
}

/// `memory/` with `decisions/`, `principles/`, and an empty `ARCHIVE.md`, in one first commit.
/// Git keeps no empty folder, so each of the two folders gets a `.gitkeep` file.
fn make_memory_repo(paths: &Paths) -> Result<()> {
    for dir in [paths.decisions_dir(), paths.principles_dir()] {
        ensure_dir(&dir)?;
        let keep = dir.join(".gitkeep");
        if !keep.exists() {
            atomic_write(&keep, b"")?;
        }
    }
    if !paths.archive_file().exists() {
        atomic_write(&paths.archive_file(), b"")?;
    }
    let memory = paths.memory_dir();
    git::init(&memory)?;
    git::commit_all(&memory, "Start the memory", INIT_AUTHOR)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;

    #[test]
    fn the_default_config_names_the_claude_folder_of_its_home() {
        let paths = Paths::new("/srv/vision-test-home");
        let text = default_config(&paths).unwrap();
        let config = Config::parse(&text).unwrap();
        assert_eq!(
            config.claude.config_dir.as_deref(),
            Some("/srv/vision-test-home/claude")
        );
        assert!(
            text.contains("\"/srv/vision-test-home/claude\"   # \"\" means: use the default login")
        );
    }
}
