//! Edits of `config.toml` with `toml_edit`, so that the comments and the order of the file stay.
//!
//! Each edit changes the document in memory. [`ConfigEditor::save`] tests that the result is a
//! valid configuration and then writes it with an atomic rename.

use std::path::{Path, PathBuf};

use anyhow::{anyhow, bail, Context, Result};
use toml_edit::{DocumentMut, Item, Table, Value};

use super::{AgentPin, Config, Mode};
use crate::util::fs::atomic_write;

/// An open `config.toml` that keeps its comments and order.
#[derive(Clone, Debug)]
pub struct ConfigEditor {
    path: Option<PathBuf>,
    doc: DocumentMut,
}

impl ConfigEditor {
    /// Reads the file at `path`. [`ConfigEditor::save`] writes it back there.
    pub fn open(path: &Path) -> Result<ConfigEditor> {
        let text = std::fs::read_to_string(path).with_context(|| {
            format!(
                "could not read {}. Run `vision init` if it does not exist",
                path.display()
            )
        })?;
        let mut editor = ConfigEditor::parse(&text)
            .with_context(|| format!("could not edit {}", path.display()))?;
        editor.path = Some(path.to_path_buf());
        Ok(editor)
    }

    /// Reads configuration text that is not tied to a file.
    pub fn parse(text: &str) -> Result<ConfigEditor> {
        let doc = text
            .parse::<DocumentMut>()
            .map_err(|e| anyhow!("{}", e.to_string().trim_end()))?;
        Ok(ConfigEditor { path: None, doc })
    }

    /// Writes `[agents.<agent>]` with the values of `pin`. A `None` value removes its key.
    /// The other keys and the comments of the table stay.
    pub fn set_agent(&mut self, agent: &str, pin: &AgentPin) -> Result<()> {
        check_name("agent", agent)?;
        let comment = self.trailing_comment();
        let agents = child_table(self.doc.as_table_mut(), "agents", true)?;
        let first = agents.is_empty();
        let table = child_table(agents, agent, false)?;
        set_or_remove(table, "model", pin.model.as_deref());
        set_or_remove(table, "effort", pin.effort.map(|e| e.as_str()));
        set_or_remove(table, "pinned_at", pin.pinned_at.as_deref());
        if first {
            if let Some(comment) = comment {
                // The commented example of section 4 stays just above the first real table.
                table.decor_mut().set_prefix(comment);
                self.doc.set_trailing("");
            }
        }
        Ok(())
    }

    /// Removes `[agents.<agent>]`. Returns false when the table did not exist.
    pub fn remove_agent(&mut self, agent: &str) -> Result<bool> {
        let Some(agents) = self.doc.get_mut("agents") else {
            return Ok(false);
        };
        let agents = agents
            .as_table_mut()
            .ok_or_else(|| anyhow!("`agents` in the configuration must be a table of tables"))?;
        let Some(removed) = agents.remove(agent) else {
            return Ok(false);
        };
        let prefix = removed
            .as_table()
            .and_then(|t| t.decor().prefix())
            .and_then(|p| p.as_str())
            .filter(|p| p.contains('#'))
            .map(str::to_owned);
        let now_empty = agents.is_empty();
        if now_empty {
            self.doc.remove("agents");
        }
        if let Some(prefix) = prefix {
            self.keep_comment(prefix, now_empty);
        }
        Ok(true)
    }

    /// Writes `"<verb>" = "<mode>"` in `[modes]`.
    pub fn set_mode(&mut self, verb: &str, mode: Mode) -> Result<()> {
        check_name("verb", verb)?;
        let modes = child_table(self.doc.as_table_mut(), "modes", false)?;
        set_or_remove(modes, verb, Some(mode.as_str()));
        Ok(())
    }

    /// Writes `config_dir` in `[claude]`.
    pub fn set_claude_config_dir(&mut self, dir: &str) -> Result<()> {
        let claude = child_table(self.doc.as_table_mut(), "claude", false)?;
        set_or_remove(claude, "config_dir", Some(dir));
        Ok(())
    }

    /// Tests that the edited text is a valid configuration.
    pub fn check(&self) -> Result<Config> {
        Config::parse(&self.text()).context("the edit would make the configuration not valid")
    }

    /// Tests the edited text and writes it back to the file that [`ConfigEditor::open`] read.
    pub fn save(&self) -> Result<()> {
        let Some(path) = &self.path else {
            bail!("this configuration was not read from a file, so it cannot be saved");
        };
        self.check()?;
        atomic_write(path, self.text().as_bytes())
    }

    /// The edited text.
    pub fn text(&self) -> String {
        self.doc.to_string()
    }

    /// The comment block at the end of the file, such as the `[agents.judge]` example.
    fn trailing_comment(&self) -> Option<String> {
        self.doc
            .trailing()
            .as_str()
            .filter(|t| t.contains('#'))
            .map(str::to_owned)
    }

    /// Puts a comment block back after its table was removed: on the next agent table, or at the end.
    fn keep_comment(&mut self, comment: String, at_end: bool) {
        if !at_end {
            let next = self
                .doc
                .get_mut("agents")
                .and_then(Item::as_table_mut)
                .and_then(|agents| agents.iter_mut().next())
                .and_then(|(_, item)| item.as_table_mut());
            if let Some(next) = next {
                if !next
                    .decor()
                    .prefix()
                    .and_then(|p| p.as_str())
                    .is_some_and(|p| p.contains('#'))
                {
                    next.decor_mut().set_prefix(comment);
                    return;
                }
            }
        }
        let trailing = self.doc.trailing().as_str().unwrap_or("").to_string();
        self.doc.set_trailing(format!("{comment}{trailing}"));
    }
}

/// The table `key` of `parent`, made when it does not exist. An implicit table has no header
/// of its own, such as `[agents]` above `[agents.judge]`.
fn child_table<'a>(parent: &'a mut Table, key: &str, implicit: bool) -> Result<&'a mut Table> {
    let item = parent.entry(key).or_insert_with(|| {
        let mut table = Table::new();
        table.set_implicit(implicit);
        Item::Table(table)
    });
    item.as_table_mut()
        .ok_or_else(|| anyhow!("`{key}` in the configuration must be a table, such as [{key}]"))
}

/// Sets a string value and keeps the comments around the old value, or removes the key.
fn set_or_remove(table: &mut Table, key: &str, value: Option<&str>) {
    let Some(value) = value else {
        table.remove(key);
        return;
    };
    match table.get_mut(key) {
        Some(Item::Value(old)) => {
            let decor = old.decor().clone();
            *old = Value::from(value);
            *old.decor_mut() = decor;
        }
        _ => {
            table.insert(key, toml_edit::value(value));
        }
    }
}

fn check_name(what: &str, name: &str) -> Result<()> {
    if name.trim().is_empty() || name.contains(['\n', '\r']) {
        bail!("the {what} name `{name}` is empty or has a line break");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{Effort, DEFAULT_TEXT};

    fn pin(model: &str) -> AgentPin {
        AgentPin {
            model: Some(model.into()),
            effort: Some(Effort::Xhigh),
            pinned_at: Some("2026-11-16T14:30:00Z".into()),
        }
    }

    fn comment_lines(text: &str) -> Vec<&str> {
        text.lines().filter(|l| l.contains('#')).collect()
    }

    #[test]
    fn set_agent_keeps_each_comment() {
        let mut editor = ConfigEditor::parse(DEFAULT_TEXT).unwrap();
        editor.set_agent("judge", &pin("claude-opus-5-5")).unwrap();
        let text = editor.text();
        assert_eq!(comment_lines(&text), comment_lines(DEFAULT_TEXT));
        let config = editor.check().unwrap();
        assert_eq!(
            config.agents["judge"].model.as_deref(),
            Some("claude-opus-5-5")
        );
        assert_eq!(config.agents["judge"].effort, Some(Effort::Xhigh));
        assert!(
            text.contains("[agents.judge]\nmodel = \"claude-opus-5-5\"\n"),
            "{text}"
        );
        assert!(
            !text.contains("[agents]\n"),
            "the parent table stays implicit: {text}"
        );
    }

    #[test]
    fn set_then_remove_gives_back_the_same_text() {
        let mut editor = ConfigEditor::parse(DEFAULT_TEXT).unwrap();
        editor.set_agent("judge", &pin("claude-opus-5-5")).unwrap();
        editor.set_agent("scout", &pin("claude-fable-5-2")).unwrap();
        assert!(editor.remove_agent("judge").unwrap());
        assert_eq!(comment_lines(&editor.text()), comment_lines(DEFAULT_TEXT));
        assert!(editor.remove_agent("scout").unwrap());
        assert!(!editor.remove_agent("scout").unwrap());
        assert_eq!(editor.text(), DEFAULT_TEXT);
    }

    #[test]
    fn set_agent_updates_in_place_and_keeps_value_comments() {
        let text = "[agents.judge]\nmodel = \"a\"   # chosen after the eval\neffort = \"low\"\n";
        let mut editor = ConfigEditor::parse(text).unwrap();
        let new = AgentPin {
            model: Some("b".into()),
            effort: None,
            pinned_at: Some("2026-11-17T09:00:00Z".into()),
        };
        editor.set_agent("judge", &new).unwrap();
        assert_eq!(
            editor.text(),
            "[agents.judge]\nmodel = \"b\"   # chosen after the eval\npinned_at = \"2026-11-17T09:00:00Z\"\n"
        );
    }

    #[test]
    fn set_mode_quotes_the_verb_and_keeps_comments() {
        let mut editor = ConfigEditor::parse(DEFAULT_TEXT).unwrap();
        editor.set_mode("git.push", Mode::Shadow).unwrap();
        editor.set_mode("git.push", Mode::Auto).unwrap();
        let text = editor.text();
        assert_eq!(comment_lines(&text), comment_lines(DEFAULT_TEXT));
        assert!(text.contains("\"git.push\" = \"auto\""), "{text}");
        assert_eq!(editor.check().unwrap().modes["git.push"], Mode::Auto);
    }

    #[test]
    fn set_mode_makes_the_table_when_it_is_missing() {
        let mut editor = ConfigEditor::parse("project = \"demo\"\n").unwrap();
        editor.set_mode("pr.open", Mode::Ask).unwrap();
        assert_eq!(editor.check().unwrap().modes["pr.open"], Mode::Ask);
    }

    #[test]
    fn save_writes_the_file_and_refuses_a_bad_result() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, DEFAULT_TEXT).unwrap();
        let mut editor = ConfigEditor::open(&path).unwrap();
        editor.set_mode("git.push", Mode::Hold).unwrap();
        editor.save().unwrap();
        let saved = std::fs::read_to_string(&path).unwrap();
        assert!(saved.contains("\"git.push\" = \"hold\""));

        let mut bad = ConfigEditor::parse("[agents]\njudge = 3\n").unwrap();
        assert!(bad.set_agent("judge", &pin("x")).is_err());
        assert!(
            ConfigEditor::parse(DEFAULT_TEXT).unwrap().save().is_err(),
            "no file to save to"
        );
    }
}
