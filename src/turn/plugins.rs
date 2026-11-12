//! The plugins of the work turns (section 7.3): the folders of `claude.plugins`, the plugins in
//! them, and the folders that a turn reads and never writes.
//!
//! A folder of `claude.plugins` is a Claude Code plugin folder (it has
//! `.claude-plugin/plugin.json`) or a folder of plugins. Each one goes to the start line as
//! `--plugin-dir`. A skill of a plugin reads the files that it links, and a worker runs its
//! scripts, so the turn reads each plugin folder. A skill folder can be a symlink to another
//! place, and a file of a skill can be a symlink too, so the real path of each such link is a
//! folder that the turn reads as well.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use serde_json::Value;

use crate::paths::expand_tilde;

/// The file that makes a folder a plugin folder.
pub const MANIFEST: &str = ".claude-plugin/plugin.json";
/// The most entries that the walk of the skill folders reads, so that a large tree costs little.
const MAX_ENTRIES: usize = 20_000;
/// The deepest folder that the walk of a skill folder reads.
const MAX_DEPTH: usize = 8;

/// One plugin in a folder of `claude.plugins`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Plugin {
    /// The plugin folder.
    pub dir: PathBuf,
    /// `name` of its `plugin.json`, else the name of the folder.
    pub name: String,
    /// The number of its skills: the folders in `skills/` with a `SKILL.md`.
    pub skills: usize,
}

/// What the plugins give a turn.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Plugins {
    /// The folders for `--plugin-dir`, as the configuration names them, with `~` expanded.
    pub dirs: Vec<PathBuf>,
    /// The folders that the turn reads and never writes: each folder of `dirs`, its real path,
    /// and the real path of each link in the skill folders that leaves the plugin folder.
    pub read: Vec<PathBuf>,
}

impl Plugins {
    pub fn is_empty(&self) -> bool {
        self.dirs.is_empty()
    }
}

/// The plugins in `dir`: `dir` itself when it is a plugin folder, else each folder in it that
/// is one. An error names a folder that does not exist or a `plugin.json` that is not valid.
pub fn find(dir: &Path) -> Result<Vec<Plugin>> {
    if !dir.is_dir() {
        bail!("{} does not exist or is not a folder", dir.display());
    }
    if dir.join(MANIFEST).is_file() {
        return Ok(vec![plugin(dir)?]);
    }
    let mut found = Vec::new();
    let mut children: Vec<PathBuf> = fs::read_dir(dir)
        .with_context(|| format!("could not read the folder {}", dir.display()))?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.join(MANIFEST).is_file())
        .collect();
    children.sort();
    for child in children {
        found.push(plugin(&child)?);
    }
    Ok(found)
}

fn plugin(dir: &Path) -> Result<Plugin> {
    let manifest = dir.join(MANIFEST);
    let text = fs::read_to_string(&manifest)
        .with_context(|| format!("could not read {}", manifest.display()))?;
    let value: Value = serde_json::from_str(&text)
        .with_context(|| format!("{} is not valid JSON", manifest.display()))?;
    let name = value
        .get("name")
        .and_then(Value::as_str)
        .map(str::to_string)
        .unwrap_or_else(|| {
            dir.file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default()
        });
    let skills = skill_dirs(dir)
        .iter()
        .filter(|d| d.join("SKILL.md").is_file())
        .count();
    Ok(Plugin {
        dir: dir.to_path_buf(),
        name,
        skills,
    })
}

/// The folders in `skills/` of a plugin, a symlink to a folder too, in the order of their names.
fn skill_dirs(plugin: &Path) -> Vec<PathBuf> {
    let Ok(entries) = fs::read_dir(plugin.join("skills")) else {
        return Vec::new();
    };
    let mut dirs: Vec<PathBuf> = entries
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.is_dir())
        .collect();
    dirs.sort();
    dirs
}

/// The plugins of the entries of `claude.plugins`. Each folder must hold at least one plugin.
pub fn resolve(entries: &[String]) -> Result<Plugins> {
    let mut plugins = Plugins::default();
    for entry in entries {
        let dir = expand_tilde(entry)
            .with_context(|| format!("could not read claude.plugins entry `{entry}`"))?;
        let dir = std::path::absolute(&dir)
            .with_context(|| format!("could not use the folder {}", dir.display()))?;
        let found = find(&dir).with_context(|| format!("claude.plugins names `{entry}`"))?;
        if found.is_empty() {
            bail!(
                "claude.plugins names `{entry}`, which holds no plugin: neither it nor a folder in it has {MANIFEST}"
            );
        }
        if !plugins.dirs.contains(&dir) {
            plugins.dirs.push(dir.clone());
        }
        let mut roots = vec![dir.clone()];
        if let Ok(real) = fs::canonicalize(&dir) {
            roots.push(real);
        }
        for root in &roots {
            add(&mut plugins.read, root.clone());
        }
        for plugin in &found {
            for skill in skill_dirs(&plugin.dir) {
                for target in link_targets(&skill) {
                    if !roots.iter().any(|r| target.starts_with(r)) {
                        add(&mut plugins.read, target);
                    }
                }
            }
        }
    }
    Ok(plugins)
}

/// Adds a folder to a list, unless the list has it or a folder that holds it.
fn add(list: &mut Vec<PathBuf>, dir: PathBuf) {
    if list.iter().any(|known| dir.starts_with(known)) {
        return;
    }
    list.retain(|known| !known.starts_with(&dir));
    list.push(dir);
}

/// The real paths that a skill folder reaches through links: the folder itself when it is a
/// link, and each link in it. A link to a file gives the folder of that file. A link that leads
/// nowhere gives nothing.
fn link_targets(skill: &Path) -> Vec<PathBuf> {
    let mut out = BTreeSet::new();
    let mut seen = 0usize;
    let mut stack = vec![(skill.to_path_buf(), 0usize)];
    while let Some((path, depth)) = stack.pop() {
        if seen >= MAX_ENTRIES {
            break;
        }
        seen += 1;
        let Ok(meta) = fs::symlink_metadata(&path) else {
            continue;
        };
        if meta.file_type().is_symlink() {
            let Ok(real) = fs::canonicalize(&path) else {
                continue;
            };
            if real.is_dir() {
                out.insert(real.clone());
                // The folder of the link can hold links of its own, such as a skill folder
                // whose files link to a shared folder.
                if depth < MAX_DEPTH {
                    push_children(&real, depth + 1, &mut stack);
                }
            } else if let Some(parent) = real.parent() {
                out.insert(parent.to_path_buf());
            }
            continue;
        }
        if meta.is_dir() && depth < MAX_DEPTH {
            push_children(&path, depth + 1, &mut stack);
        }
    }
    out.into_iter().collect()
}

fn push_children(dir: &Path, depth: usize, stack: &mut Vec<(PathBuf, usize)>) {
    if let Ok(entries) = fs::read_dir(dir) {
        for entry in entries.flatten() {
            stack.push((entry.path(), depth));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::symlink;

    fn make_plugin(dir: &Path, name: &str, skills: &[&str]) {
        fs::create_dir_all(dir.join(".claude-plugin")).unwrap();
        fs::write(
            dir.join(MANIFEST),
            format!("{{\"name\": \"{name}\", \"version\": \"1.0.0\"}}"),
        )
        .unwrap();
        for skill in skills {
            let folder = dir.join("skills").join(skill);
            fs::create_dir_all(&folder).unwrap();
            fs::write(
                folder.join("SKILL.md"),
                format!("---\nname: {skill}\n---\n"),
            )
            .unwrap();
        }
    }

    #[test]
    fn a_plugin_folder_and_a_folder_of_plugins_are_found() {
        let dir = tempfile::tempdir().unwrap();
        let one = dir.path().join("one");
        make_plugin(&one, "one", &["review", "commit"]);
        let found = find(&one).unwrap();
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].name, "one");
        assert_eq!(found[0].skills, 2);
        let many = dir.path().join("many");
        make_plugin(&many.join("b"), "bee", &["x"]);
        make_plugin(&many.join("a"), "ay", &[]);
        fs::create_dir_all(many.join("not-a-plugin")).unwrap();
        let names: Vec<String> = find(&many).unwrap().into_iter().map(|p| p.name).collect();
        assert_eq!(names, ["ay", "bee"]);
        assert!(find(&dir.path().join("missing")).is_err());
        let empty = dir.path().join("empty");
        fs::create_dir_all(&empty).unwrap();
        assert!(find(&empty).unwrap().is_empty());
        fs::write(one.join(MANIFEST), "{").unwrap();
        let err = format!("{:#}", find(&one).unwrap_err());
        assert!(err.contains("is not valid JSON"), "{err}");
    }

    #[test]
    fn the_read_list_follows_the_links_of_the_skill_folders() {
        let dir = tempfile::tempdir().unwrap();
        let root = fs::canonicalize(dir.path()).unwrap();
        // A shared folder of skills outside the plugin, as a person keeps them.
        let shared = root.join("shared");
        fs::create_dir_all(shared.join("qa/sections")).unwrap();
        fs::write(shared.join("qa/SKILL.md"), "qa").unwrap();
        fs::create_dir_all(shared.join("refs")).unwrap();
        fs::write(shared.join("refs/taxonomy.md"), "t").unwrap();
        symlink(shared.join("refs"), shared.join("qa/references")).unwrap();
        fs::create_dir_all(root.join("lone")).unwrap();
        fs::write(root.join("lone/script.sh"), "echo").unwrap();

        let plugin = root.join("mine");
        make_plugin(&plugin, "mine", &["inside"]);
        symlink(shared.join("qa"), plugin.join("skills/qa")).unwrap();
        symlink(
            root.join("lone/script.sh"),
            plugin.join("skills/inside/run.sh"),
        )
        .unwrap();
        symlink(root.join("nowhere"), plugin.join("skills/broken")).unwrap();

        let plugins = resolve(&[plugin.display().to_string()]).unwrap();
        assert_eq!(plugins.dirs, std::slice::from_ref(&plugin));
        assert_eq!(
            plugins.read,
            [
                plugin.clone(),
                root.join("lone"),
                shared.join("qa"),
                shared.join("refs")
            ],
            "the plugin, the folder of a linked file, a linked skill, and a link inside it"
        );
    }

    #[test]
    fn a_folder_with_no_plugin_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let err = format!(
            "{:#}",
            resolve(&[dir.path().display().to_string()]).unwrap_err()
        );
        assert!(err.contains("holds no plugin"), "{err}");
        let err = format!(
            "{:#}",
            resolve(&[dir.path().join("gone").display().to_string()]).unwrap_err()
        );
        assert!(err.contains("does not exist"), "{err}");
        assert!(resolve(&[]).unwrap().is_empty());
    }

    #[test]
    fn a_folder_that_holds_another_one_keeps_only_the_outer_one() {
        let mut list = vec![PathBuf::from("/a/b")];
        add(&mut list, PathBuf::from("/a/b/c"));
        assert_eq!(list, [PathBuf::from("/a/b")]);
        add(&mut list, PathBuf::from("/a"));
        assert_eq!(list, [PathBuf::from("/a")]);
        add(&mut list, PathBuf::from("/ab"));
        assert_eq!(list, [PathBuf::from("/a"), PathBuf::from("/ab")]);
    }
}
