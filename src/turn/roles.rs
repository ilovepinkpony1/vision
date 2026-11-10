//! The role files and the result schemas of `agents/`, embedded in the binary at build time,
//! so that `vision` works from any folder. A file with the same name in `$VISION_HOME/agents/`
//! wins over the embedded one.

use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use serde_json::Value;

use crate::models;
use crate::paths::Paths;
use crate::util::fs::read_optional;

/// The files of `agents/`, as the binary embeds them.
const EMBEDDED: [(&str, &str); 11] = [
    ("common.md", include_str!("../../agents/common.md")),
    ("scout.md", include_str!("../../agents/scout.md")),
    ("builder.md", include_str!("../../agents/builder.md")),
    ("reviewer.md", include_str!("../../agents/reviewer.md")),
    ("tester.md", include_str!("../../agents/tester.md")),
    ("shepherd.md", include_str!("../../agents/shepherd.md")),
    ("judge.md", include_str!("../../agents/judge.md")),
    ("curator.md", include_str!("../../agents/curator.md")),
    (
        "turn.schema.json",
        include_str!("../../agents/turn.schema.json"),
    ),
    (
        "judge.schema.json",
        include_str!("../../agents/judge.schema.json"),
    ),
    (
        "curator.schema.json",
        include_str!("../../agents/curator.schema.json"),
    ),
];

/// `$VISION_HOME/agents/`: files here replace the embedded ones.
pub fn agents_dir(paths: &Paths) -> PathBuf {
    paths.home().join("agents")
}

/// The names of the embedded files.
pub fn embedded_names() -> Vec<&'static str> {
    EMBEDDED.iter().map(|(name, _)| *name).collect()
}

/// The text of a file of `agents/`, such as `scout.md`: the file of `$VISION_HOME/agents/`
/// when it exists, else the embedded one. The error names the missing file.
pub fn agents_file(paths: &Paths, name: &str) -> Result<String> {
    let valid = !name.is_empty()
        && name.starts_with(|c: char| c.is_ascii_alphanumeric())
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'));
    if !valid {
        bail!("`{name}` is not a valid name for a file of agents/");
    }
    let path = agents_dir(paths).join(name);
    if let Some(text) = read_optional(&path)? {
        return Ok(text);
    }
    if let Some((_, text)) = EMBEDDED.iter().find(|(n, _)| *n == name) {
        return Ok((*text).to_string());
    }
    bail!(
        "the role file agents/{name} does not exist. Write it at {}, or use one of the agents {}",
        path.display(),
        models::agent_names().join(", ")
    )
}

/// The system prompt of a turn of `agent`: `common.md` followed by the role file for a work
/// agent, or only the role file for the Judge and the Curator. `role_file` replaces the role
/// file of `agents/`, such as a role file of an adapter.
pub fn compose_role(paths: &Paths, agent: &str, role_file: Option<&Path>) -> Result<String> {
    let own = match role_file {
        Some(path) => std::fs::read_to_string(path)
            .with_context(|| format!("could not read the role file {}", path.display()))?,
        None => agents_file(paths, &format!("{agent}.md"))?,
    };
    let alone = models::agent(agent).is_some_and(|spec| spec.alone);
    if alone {
        return Ok(own);
    }
    let common = agents_file(paths, "common.md")?;
    Ok(format!("{}\n\n{}", common.trim_end(), own))
}

/// The name of the result schema of `agent` in `agents/`.
pub fn schema_name(agent: &str) -> &'static str {
    models::agent(agent).map_or(models::TURN_SCHEMA, |spec| spec.schema)
}

/// The parsed result schema of `agent`.
pub fn schema(paths: &Paths, agent: &str) -> Result<Value> {
    let name = schema_name(agent);
    let text = agents_file(paths, name)?;
    serde_json::from_str(&text)
        .with_context(|| format!("the schema agents/{name} is not valid JSON. Fix it"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_embedded_schema_parses() {
        let paths = Paths::new("/nonexistent-vision-home");
        for name in embedded_names() {
            let text = agents_file(&paths, name).unwrap();
            if name.ends_with(".json") {
                serde_json::from_str::<Value>(&text).unwrap();
            } else {
                assert!(text.starts_with("# "), "{name}");
            }
        }
    }

    #[test]
    fn a_work_agent_gets_common_first_and_the_judge_only_its_file() {
        let paths = Paths::new("/nonexistent-vision-home");
        let scout = compose_role(&paths, "scout", None).unwrap();
        assert!(scout.starts_with("# Rules for each work turn"), "{scout}");
        assert!(scout.contains("\n\n# Scout\n"), "{scout}");
        let judge = compose_role(&paths, "judge", None).unwrap();
        assert!(judge.starts_with("# Judge"));
        assert!(!judge.contains("# Rules for each work turn"));
        assert_eq!(schema_name("judge"), "judge.schema.json");
        assert_eq!(schema_name("builder"), "turn.schema.json");
    }

    #[test]
    fn a_file_in_the_home_wins_and_a_missing_one_is_named() {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::new(dir.path());
        std::fs::create_dir_all(agents_dir(&paths)).unwrap();
        std::fs::write(agents_dir(&paths).join("scout.md"), "# My scout\n").unwrap();
        let scout = compose_role(&paths, "scout", None).unwrap();
        assert!(scout.ends_with("# My scout\n"), "{scout}");
        let err = compose_role(&paths, "planner", None)
            .unwrap_err()
            .to_string();
        assert!(err.contains("agents/planner.md does not exist"), "{err}");
        assert!(agents_file(&paths, "../x.md").is_err());
    }
}
