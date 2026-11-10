//! Where the adapter of a project is (section 11.1).
//!
//! The order: `adapter.dir` of the configuration, then `$VISION_HOME/adapters/<name>/`, then the
//! folder `adapters/<name>/` next to the binary, then the folder `adapters/<name>/` of the source
//! tree that built the binary, so that a development build finds the demo adapter.

use std::fmt;
use std::path::{Path, PathBuf};

use anyhow::{bail, Result};
use serde::Serialize;

use super::FILE;
use crate::config::Config;
use crate::paths::Paths;

/// Where an adapter was found.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Origin {
    /// The folder that `adapter.dir` of the configuration names.
    Configured,
    /// `$VISION_HOME/adapters/<name>/`: a private adapter.
    Home,
    /// `adapters/<name>/` next to the binary.
    NextToBinary,
    /// `adapters/<name>/` of the source tree that built the binary.
    SourceTree,
}

impl Origin {
    pub fn as_str(self) -> &'static str {
        match self {
            Origin::Configured => "adapter.dir",
            Origin::Home => "the home of Vision",
            Origin::NextToBinary => "the folder of the binary",
            Origin::SourceTree => "the source tree",
        }
    }
}

impl fmt::Display for Origin {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// An adapter folder and how it was found.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Located {
    pub dir: PathBuf,
    pub origin: Origin,
}

/// Finds the folder of the adapter of `config.project`. A folder that exists must hold
/// `adapter.toml`. The error names each place that was tried.
pub fn locate(config: &Config, paths: &Paths) -> Result<Located> {
    let name = config.project.as_str();
    check_name(name)?;
    if let Some(dir) = config.adapter_dir(paths)? {
        if dir.join(FILE).is_file() {
            return Ok(Located {
                dir,
                origin: Origin::Configured,
            });
        }
        bail!(
            "adapter.dir in config.toml names {}, but that folder has no {FILE}. Fix adapter.dir, or remove it to use the adapter `{name}`",
            dir.display()
        );
    }
    let candidates = candidates(name, paths);
    for (dir, origin) in &candidates {
        if dir.join(FILE).is_file() {
            return Ok(Located {
                dir: dir.clone(),
                origin: *origin,
            });
        }
        if dir.is_dir() {
            bail!(
                "the adapter folder {} has no {FILE}. Write it there, or remove the folder",
                dir.display()
            );
        }
    }
    let tried: Vec<String> = candidates
        .iter()
        .map(|(dir, _)| dir.display().to_string())
        .collect();
    bail!(
        "no adapter `{name}` (the key `project` of config.toml). Looked in {}. Put the adapter in {}, or set adapter.dir in config.toml",
        tried.join(", "),
        paths.adapter_dir(name).display()
    )
}

/// The folders to try after `adapter.dir`, in order.
fn candidates(name: &str, paths: &Paths) -> Vec<(PathBuf, Origin)> {
    let mut list = vec![(paths.adapter_dir(name), Origin::Home)];
    if let Some(dir) = std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(Path::to_path_buf))
    {
        list.push((dir.join("adapters").join(name), Origin::NextToBinary));
    }
    list.push((
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("adapters")
            .join(name),
        Origin::SourceTree,
    ));
    list
}

fn check_name(name: &str) -> Result<()> {
    let valid = (1..=64).contains(&name.len())
        && name.starts_with(|c: char| c.is_ascii_alphanumeric())
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'));
    if !valid {
        bail!(
            "`{name}` (the key `project` of config.toml) is not a valid adapter name. Use letters, digits, '-', '_', or '.', such as demo"
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config(text: &str) -> Config {
        Config::parse(text).unwrap()
    }

    #[test]
    fn the_source_tree_has_the_demo_adapter() {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::new(dir.path());
        let located = locate(&config("project = \"demo\"\n"), &paths).unwrap();
        assert_eq!(located.origin, Origin::SourceTree);
        assert!(located.dir.ends_with("adapters/demo"));
    }

    #[test]
    fn the_home_wins_over_the_source_tree_and_adapter_dir_wins_over_both() {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::new(dir.path().join("home"));
        let private = paths.adapter_dir("demo");
        std::fs::create_dir_all(&private).unwrap();
        std::fs::write(private.join(FILE), "").unwrap();
        let located = locate(&config("project = \"demo\"\n"), &paths).unwrap();
        assert_eq!(
            located,
            Located {
                dir: private,
                origin: Origin::Home
            }
        );

        let other = dir.path().join("other");
        std::fs::create_dir_all(&other).unwrap();
        std::fs::write(other.join(FILE), "").unwrap();
        let text = format!(
            "project = \"demo\"\n[adapter]\ndir = \"{}\"\n",
            other.display()
        );
        let located = locate(&config(&text), &paths).unwrap();
        assert_eq!(
            located,
            Located {
                dir: other,
                origin: Origin::Configured
            }
        );
    }

    #[test]
    fn a_missing_adapter_names_each_place() {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::new(dir.path());
        let err = locate(&config("project = \"nothing-here\"\n"), &paths)
            .unwrap_err()
            .to_string();
        assert!(err.contains("no adapter `nothing-here`"), "{err}");
        assert!(
            err.contains(&paths.adapter_dir("nothing-here").display().to_string()),
            "{err}"
        );
        assert!(err.contains("adapter.dir"), "{err}");
    }

    #[test]
    fn a_folder_without_its_file_and_a_bad_dir_are_errors() {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::new(dir.path());
        std::fs::create_dir_all(paths.adapter_dir("demo")).unwrap();
        let err = locate(&config("project = \"demo\"\n"), &paths)
            .unwrap_err()
            .to_string();
        assert!(err.contains("has no adapter.toml"), "{err}");

        let text = "project = \"demo\"\n[adapter]\ndir = \"/no/such/folder\"\n";
        let err = locate(&config(text), &paths).unwrap_err().to_string();
        assert!(
            err.contains("adapter.dir in config.toml names /no/such/folder"),
            "{err}"
        );
        let err = locate(&config("project = \"../x\"\n"), &paths)
            .unwrap_err()
            .to_string();
        assert!(err.contains("not a valid adapter name"), "{err}");
    }
}
