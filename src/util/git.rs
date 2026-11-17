//! `git` as a child process. No git library is linked.
//!
//! Each call turns off commit signing and hooks, and ignores the `GIT_*` folder variables of the
//! caller, so that the result does not depend on the global setup of the person.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use anyhow::{bail, Context, Result};

/// The writer of a commit in the memory repo: the person, `judge`, `curator`, or `import`.
#[derive(Clone, Copy, Debug)]
pub struct Author<'a> {
    pub name: &'a str,
    pub email: &'a str,
}

/// True when a text from outside, such as the branch of a pull request, can be the name of a
/// local git branch that Vision passes to git: no space, no control character, no `..`, no
/// leading `-` or `/`, and none of the characters that git refuses.
pub fn valid_branch(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 200
        && !name.chars().any(|c| c.is_whitespace() || c.is_control())
        && !name.starts_with(['-', '/', '.'])
        && !name.ends_with(['/', '.'])
        && !name.ends_with(".lock")
        && !name.contains("..")
        && !name.contains("@{")
        && !name.contains("//")
        && !name.contains(['~', '^', ':', '?', '*', '[', '\\'])
        && name != "@"
}

/// Runs `git -C <dir> <args>` and gives its standard output without the last newline.
pub fn run(dir: &Path, args: &[&str]) -> Result<String> {
    let output = command(dir, None)
        .args(args)
        .output()
        .context("could not run git. Install git and make sure that it is in PATH")?;
    stdout_or_error(dir, args, output)
}

/// Fetches each remote of a clone with `git fetch --quiet --all --prune`, the fetch of the take
/// of a new ticket. A remote that asks for a login fails at once and asks nothing, because a
/// thread of the app can run the fetch while the app holds the terminal. A fetch changes no
/// branch and no work tree.
pub fn fetch_all(dir: &Path) -> Result<String> {
    let args = ["fetch", "--quiet", "--all", "--prune"];
    let output = command(dir, None)
        .env("GIT_TERMINAL_PROMPT", "0")
        .args(args)
        .output()
        .context("could not run git. Install git and make sure that it is in PATH")?;
    stdout_or_error(dir, &args, output)
}

/// Runs `git -C <dir> <args>` with `author` as the author and the committer, such as a commit
/// of only some paths, and gives its standard output without the last newline.
pub fn run_as(dir: &Path, args: &[&str], author: Author<'_>) -> Result<String> {
    let output = command(dir, Some(author))
        .args(args)
        .output()
        .context("could not run git. Install git and make sure that it is in PATH")?;
    stdout_or_error(dir, args, output)
}

/// Runs `git -C <dir> <args>` and gives its standard output as it is, such as the bytes of a
/// file at a commit.
pub fn run_bytes(dir: &Path, args: &[&str]) -> Result<Vec<u8>> {
    let output = command(dir, None)
        .args(args)
        .output()
        .context("could not run git. Install git and make sure that it is in PATH")?;
    if !output.status.success() {
        stdout_or_error(dir, args, output)?;
        bail!("`git {}` failed in {}", args.join(" "), dir.display());
    }
    Ok(output.stdout)
}

/// Stages each change in `dir` and makes one commit with `author` as the author and the committer.
pub fn commit_all(dir: &Path, message: &str, author: Author<'_>) -> Result<()> {
    run(dir, &["add", "--all"])?;
    let args = ["commit", "--quiet", "--no-verify", "-m", message];
    let output = command(dir, Some(author))
        .args(args)
        .output()
        .context("could not run git commit")?;
    stdout_or_error(dir, &args, output).map(|_| ())
}

/// Starts a new repo in `dir` with the branch `main`.
pub fn init(dir: &Path) -> Result<()> {
    run(dir, &["-c", "init.defaultBranch=main", "init", "--quiet"]).map(|_| ())
}

/// True when `dir` is the top folder of a git work tree, not a folder inside a bigger repo.
pub fn is_repo_root(dir: &Path) -> bool {
    let Ok(top) = run(dir, &["rev-parse", "--show-toplevel"]) else {
        return false;
    };
    match (Path::new(&top).canonicalize(), dir.canonicalize()) {
        (Ok(top), Ok(dir)) => top == dir,
        _ => false,
    }
}

/// The common git folder of `dir` when `dir` is in a linked worktree, such as `/work/app/.git`
/// for the worktree `/work/app-b`, as git gives it. `None` for a normal clone and for a folder
/// that git does not know. Git keeps the objects, the refs, the configuration, and the index of
/// each worktree in that folder.
pub fn worktree_common_dir(dir: &Path) -> Option<PathBuf> {
    let out = run(dir, &["rev-parse", "--git-dir", "--git-common-dir"]).ok()?;
    let mut lines = out.lines();
    // A relative path of git is relative to the folder of the call.
    let own = dir.join(lines.next()?.trim());
    let common = dir.join(lines.next()?.trim());
    let (Ok(own_real), Ok(common_real)) = (own.canonicalize(), common.canonicalize()) else {
        return None;
    };
    (own_real != common_real).then_some(common)
}

/// One worktree of a repo, from `git worktree list --porcelain`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Worktree {
    pub path: PathBuf,
    /// The branch that it holds, such as `vision/DEMO-12`, or `None` for a detached head.
    pub branch: Option<String>,
    /// True for the folder of a bare repo, which has no work tree.
    pub bare: bool,
}

/// The worktrees of the repo of `dir`. The first one is the main worktree.
pub fn worktrees(dir: &Path) -> Result<Vec<Worktree>> {
    let text = run(dir, &["worktree", "list", "--porcelain"])?;
    let mut out: Vec<Worktree> = Vec::new();
    for line in text.lines() {
        if let Some(path) = line.strip_prefix("worktree ") {
            out.push(Worktree {
                path: PathBuf::from(path),
                branch: None,
                bare: false,
            });
        } else if let Some(last) = out.last_mut() {
            if let Some(branch) = line.strip_prefix("branch ") {
                let branch = branch.strip_prefix("refs/heads/").unwrap_or(branch);
                last.branch = Some(branch.to_string());
            } else if line == "bare" {
                last.bare = true;
            }
        }
    }
    Ok(out)
}

/// The worktree of the repo of `dir`, other than `dir`, that holds `branch`. Git lets one
/// worktree hold a branch at a time, so a switch to that branch in `dir` fails.
pub fn worktree_holding(dir: &Path, branch: &str) -> Option<PathBuf> {
    let own = dir.canonicalize().unwrap_or_else(|_| dir.to_path_buf());
    worktrees(dir)
        .ok()?
        .into_iter()
        .filter(|tree| tree.branch.as_deref() == Some(branch))
        .map(|tree| tree.path)
        .find(|path| path.canonicalize().unwrap_or_else(|_| path.clone()) != own)
}

/// The output of `git --version`.
pub fn version() -> Result<String> {
    let output = Command::new("git")
        .arg("--version")
        .output()
        .context("could not run git. Install git and make sure that it is in PATH")?;
    stdout_or_error(Path::new("."), &["--version"], output)
}

fn command(dir: &Path, author: Option<Author<'_>>) -> Command {
    let mut command = Command::new("git");
    command
        .arg("-C")
        .arg(dir)
        .args([
            "-c",
            "commit.gpgsign=false",
            "-c",
            "core.hooksPath=/dev/null",
        ])
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
        .env_remove("GIT_OBJECT_DIRECTORY");
    if let Some(author) = author {
        command
            .env("GIT_AUTHOR_NAME", author.name)
            .env("GIT_AUTHOR_EMAIL", author.email)
            .env("GIT_COMMITTER_NAME", author.name)
            .env("GIT_COMMITTER_EMAIL", author.email);
    }
    command
}

fn stdout_or_error(dir: &Path, args: &[&str], output: Output) -> Result<String> {
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!(
            "`git {}` failed in {}: {}",
            args.join(" "),
            dir.display(),
            stderr.trim()
        );
    }
    Ok(String::from_utf8_lossy(&output.stdout)
        .trim_end()
        .to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_branch_from_outside_must_be_a_name_that_git_takes() {
        for good in ["feature/demo-12-export", "export", "vision/DEMO-12", "a.b", "fix_1"] {
            assert!(valid_branch(good), "{good}");
        }
        let long = "a".repeat(201);
        for bad in [
            "",
            "two words",
            "tab\there",
            "-delete",
            "/root",
            ".hidden",
            "end/",
            "end.",
            "main.lock",
            "a..b",
            "a@{1}",
            "a//b",
            "a~1",
            "a^",
            "a:b",
            "a?",
            "a*",
            "a[1]",
            "a\\b",
            "@",
            long.as_str(),
        ] {
            assert!(!valid_branch(bad), "{bad:?}");
        }
    }

    const AUTHOR: Author<'static> = Author {
        name: "test",
        email: "test@example.invalid",
    };

    /// A repo with one commit, a worktree of it on the branch `feature`, and a detached
    /// worktree, in a temporary folder. Gives the folder, the main checkout, and the two
    /// worktrees.
    fn repo_with_worktrees() -> (tempfile::TempDir, PathBuf, PathBuf, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let main = dir.path().join("main");
        std::fs::create_dir(&main).unwrap();
        init(&main).unwrap();
        std::fs::write(main.join("a.txt"), "a\n").unwrap();
        commit_all(&main, "Start", AUTHOR).unwrap();
        let feature = dir.path().join("feature");
        let detached = dir.path().join("detached");
        run(
            &main,
            &[
                "worktree",
                "add",
                "--quiet",
                "-b",
                "feature",
                feature.to_str().unwrap(),
            ],
        )
        .unwrap();
        run(
            &main,
            &[
                "worktree",
                "add",
                "--quiet",
                "--detach",
                detached.to_str().unwrap(),
            ],
        )
        .unwrap();
        (dir, main, feature, detached)
    }

    #[test]
    fn a_linked_worktree_has_the_common_git_folder_of_its_main_checkout() {
        let (dir, main, feature, detached) = repo_with_worktrees();
        let common = main.join(".git").canonicalize().unwrap();
        for tree in [&feature, &detached] {
            let found = worktree_common_dir(tree).expect("a linked worktree");
            assert_eq!(found.canonicalize().unwrap(), common, "{}", tree.display());
            assert!(is_repo_root(tree), "{}", tree.display());
        }
        // The main checkout, a folder in it, and a folder that git does not know.
        assert_eq!(worktree_common_dir(&main), None);
        std::fs::create_dir(main.join("sub")).unwrap();
        assert_eq!(worktree_common_dir(&main.join("sub")), None);
        let plain = dir.path().join("plain");
        std::fs::create_dir(&plain).unwrap();
        assert_eq!(worktree_common_dir(&plain), None);
    }

    #[test]
    fn the_list_of_worktrees_names_the_main_checkout_first_and_the_holder_of_a_branch() {
        let (_dir, main, feature, detached) = repo_with_worktrees();
        let trees = worktrees(&feature).unwrap();
        let real = |p: &Path| p.canonicalize().unwrap();
        // The main checkout comes first. Git lists the linked worktrees in an order of its own.
        let mut found: Vec<(PathBuf, Option<&str>)> = trees
            .iter()
            .map(|t| (real(&t.path), t.branch.as_deref()))
            .collect();
        assert_eq!(found.remove(0), (real(&main), Some("main")));
        found.sort();
        let mut linked = vec![(real(&feature), Some("feature")), (real(&detached), None)];
        linked.sort();
        assert_eq!(found, linked);
        assert!(trees.iter().all(|t| !t.bare));
        // The holder of a branch is another worktree, never the folder that asks.
        assert_eq!(
            worktree_holding(&detached, "feature").map(|p| real(&p)),
            Some(real(&feature))
        );
        assert_eq!(worktree_holding(&feature, "feature"), None);
        assert_eq!(
            worktree_holding(&feature, "main").map(|p| real(&p)),
            Some(real(&main))
        );
        assert_eq!(worktree_holding(&main, "no-such-branch"), None);
    }
}
