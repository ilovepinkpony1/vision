//! The adapter (section 11): the step list and the scripts that know one project.
//!
//! The library API that the pipeline and the executor build on:
//!
//! - [`load`] and [`load_with`] find the adapter of the configuration key `project` (see
//!   [`locate`] for the order of the places) and give the parsed and tested [`Adapter`].
//! - [`Adapter`] holds the steps in the order of the file (the first one is the entry), the
//!   branch pattern, the base ref, the verbs, the jobs, the envs, and the folder whose `bin/`
//!   answers the calls of the executor.
//! - [`Step`] and [`StepKind`] give each step with its typed keys, and [`Entry`] one item of
//!   `after`, `before`, or `start`, with its arguments.
//!
//! Only the executor starts the scripts of an adapter: see [`crate::executor`].

mod find;
mod model;

use std::path::Path;

use anyhow::Result;
use serde_json::{Map, Value};

use crate::config::Config;
use crate::ctx::Ctx;
use crate::paths::Paths;

pub use find::{locate, Located, Origin};
pub use model::{
    cap_names, Adapter, Entry, EntryKind, Expect, HumanStep, Job, JobStep, Step, StepKind,
    TurnStep, Verb, WaitStep, DEFAULT_BASE, DEFAULT_BRANCH, DEFAULT_EVERY, FINDINGS, NONE,
};

/// The name of the step list in an adapter folder.
pub const FILE: &str = "adapter.toml";

/// The adapter of the configuration of `ctx`.
pub fn load(ctx: &Ctx) -> Result<Adapter> {
    load_with(ctx.config()?, &ctx.paths)
}

/// Finds the adapter of `config.project`, parses its `adapter.toml`, and tests it. A turn step
/// must name an agent that has a role file: one of Vision, one in `$VISION_HOME/agents/`, or one
/// in `roles/` of the adapter.
pub fn load_with(config: &Config, paths: &Paths) -> Result<Adapter> {
    let located = locate(config, paths)?;
    let mut adapter = Adapter::read(&located.dir)?;
    adapter.origin = Some(located.origin);
    adapter.check_agents(paths)?;
    Ok(adapter)
}

/// True for a name that a document of the ticket folder can have, such as `plan.md` or
/// `ci-log.txt`: letters, digits, `.`, `-`, and `_`, starting with a letter or a digit, with no
/// folder part, and up to 100 characters.
pub fn valid_document_name(name: &str) -> bool {
    (1..=100).contains(&name.len())
        && name.starts_with(|c: char| c.is_ascii_alphanumeric())
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_'))
        && name != "turns"
}

/// A TOML value as JSON. A date or a time becomes its text.
pub fn toml_to_json(value: &toml::Value) -> Value {
    match value {
        toml::Value::String(s) => Value::String(s.clone()),
        toml::Value::Integer(i) => Value::from(*i),
        toml::Value::Float(f) => serde_json::Number::from_f64(*f)
            .map(Value::Number)
            .unwrap_or(Value::Null),
        toml::Value::Boolean(b) => Value::Bool(*b),
        toml::Value::Datetime(d) => Value::String(d.to_string()),
        toml::Value::Array(items) => Value::Array(items.iter().map(toml_to_json).collect()),
        toml::Value::Table(table) => Value::Object(table_to_json(table)),
    }
}

/// A TOML table as a JSON object.
pub fn table_to_json(table: &toml::Table) -> Map<String, Value> {
    table
        .iter()
        .map(|(key, value)| (key.clone(), toml_to_json(value)))
        .collect()
}

/// The file of an adapter folder.
pub fn file_of(dir: &Path) -> std::path::PathBuf {
    dir.join(FILE)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn document_names_have_no_folder_part() {
        for good in ["plan.md", "ci-log.txt", "issue-DEMO-12.json", "a_b"] {
            assert!(valid_document_name(good), "{good}");
        }
        for bad in [
            "",
            ".hidden",
            "../x",
            "a/b",
            "a b",
            "turns",
            &"x".repeat(101),
        ] {
            assert!(!valid_document_name(bad), "{bad}");
        }
    }

    #[test]
    fn toml_becomes_json() {
        let value: toml::Table =
            toml::from_str("a = 1\nb = [true, \"x\"]\nc = { d = 1.5 }\n").unwrap();
        assert_eq!(
            Value::Object(table_to_json(&value)),
            serde_json::json!({"a": 1, "b": [true, "x"], "c": {"d": 1.5}})
        );
    }
}
