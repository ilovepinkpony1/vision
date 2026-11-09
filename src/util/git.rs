//! `git` as a child process. No git library is linked.
//!
//! Each call turns off commit signing and hooks, and ignores the `GIT_*` folder variables of the
//! caller, so that the result does not depend on the global setup of the person.

use std::path::Path;
use std::process::{Command, Output};

use anyhow::{bail, Context, Result};

/// The writer of a commit in the memory repo: the person, `judge`, `curator`, or `import`.
#[derive(Clone, Copy, Debug)]
pub struct Author<'a> {
    pub name: &'a str,
    pub email: &'a str,
}

/// Runs `git -C <dir> <args>` and gives its standard output without the last newline.
pub fn run(dir: &Path, args: &[&str]) -> Result<String> {
    let output = command(dir, None)
        .args(args)
        .output()
        .context("could not run git. Install git and make sure that it is in PATH")?;
    stdout_or_error(dir, args, output)
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
