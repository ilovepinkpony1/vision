//! What the executor knows of one ticket before a call: its row, its clone, its branch, its pull
//! request, its last commit, and the base commit of its branch.

use std::path::PathBuf;

use anyhow::{bail, Context, Result};

use crate::adapter::Adapter;
use crate::ledger::{Ledger, TicketRow};
use crate::paths::{expand_tilde, TicketKey};
use crate::util::git;

/// The facts of one ticket for a call of the executor.
#[derive(Clone, Debug, PartialEq)]
pub struct TicketContext {
    pub key: TicketKey,
    /// The row of `tickets`, when the ticket has one.
    pub row: Option<TicketRow>,
    /// The clone of the ticket, when its row names one.
    pub clone: Option<PathBuf>,
    /// The branch of the row, else the branch pattern of the adapter.
    pub branch: String,
    /// The pull request number of the row.
    pub pr: Option<i64>,
    /// The last commit: `HEAD` of the clone, else the `head` of the row.
    pub head: Option<String>,
}

impl TicketContext {
    /// Reads the row of the ticket and the last commit of its clone. A clone that the row names
    /// must exist and be a git repo.
    pub fn load(ledger: &Ledger, adapter: &Adapter, key: &TicketKey) -> Result<TicketContext> {
        let row = ledger.ticket(key.as_str())?;
        let clone = match row.as_ref().and_then(|r| r.clone.as_deref()) {
            Some(text) if !text.trim().is_empty() => Some(expand_tilde(text)?),
            _ => None,
        };
        let head = match &clone {
            Some(dir) => {
                if !dir.is_dir() {
                    bail!(
                        "the clone {} of {key} does not exist. Fix the clone, or run `vision stop {key}`",
                        dir.display()
                    );
                }
                Some(git::run(dir, &["rev-parse", "HEAD"]).with_context(|| {
                    format!(
                        "could not read the last commit of the clone {} of {key}",
                        dir.display()
                    )
                })?)
            }
            None => row
                .as_ref()
                .and_then(|r| r.head.clone())
                .filter(|h| !h.trim().is_empty()),
        };
        let branch = row
            .as_ref()
            .and_then(|r| r.branch.clone())
            .filter(|b| !b.trim().is_empty())
            .unwrap_or_else(|| adapter.branch_for(key));
        let pr = row.as_ref().and_then(|r| r.pr);
        Ok(TicketContext {
            key: key.clone(),
            row,
            clone,
            branch,
            pr,
            head,
        })
    }

    /// Fails with a next step when the ticket has no row.
    pub fn require_row(&self) -> Result<&TicketRow> {
        self.row.as_ref().with_context(|| {
            format!(
                "the ticket {} has no row in the ledger. Run `vision take {}` first",
                self.key, self.key
            )
        })
    }

    /// The last commit, or the empty text when the ticket has none.
    pub fn head_text(&self) -> String {
        self.head.clone().unwrap_or_default()
    }

    /// The clone as text, for a call object.
    pub fn clone_text(&self) -> Option<String> {
        self.clone.as_ref().map(|c| c.display().to_string())
    }

    /// The base commit of the branch: the merge base of the base ref of the adapter and the last
    /// commit. `None` when the ticket has no clone.
    pub fn base(&self, adapter: &Adapter) -> Result<Option<String>> {
        let (Some(clone), Some(head)) = (&self.clone, &self.head) else {
            return Ok(None);
        };
        let base = git::run(clone, &["merge-base", &adapter.base, head]).with_context(|| {
            format!(
                "could not find the base of {} in {}: the ref {} of the adapter is missing or has no common commit. Fetch the clone",
                self.key,
                clone.display(),
                adapter.base
            )
        })?;
        Ok(Some(base))
    }
}

/// The diff from `base` to `head` in the clone, as the gates read it: no color, no external
/// tool, no context lines, renames found, and the prefixes `a/` and `b/`. `--text` shows each
/// file as text, so that an attribute of the repo, such as `-diff`, cannot hide a file from the
/// gates.
pub fn diff(clone: &std::path::Path, base: &str, head: &str) -> Result<String> {
    git::run(
        clone,
        &[
            "-c",
            "core.quotePath=false",
            "diff",
            "--no-color",
            "--no-ext-diff",
            "--no-textconv",
            "--text",
            "--find-renames",
            "--unified=0",
            "--src-prefix=a/",
            "--dst-prefix=b/",
            base,
            head,
        ],
    )
    .with_context(|| {
        format!(
            "could not read the diff {base}..{head} in {}",
            clone.display()
        )
    })
}

/// The patch of each commit after `base` up to `head` that is not a merge, oldest first, with
/// the same form as [`diff`]. A line `commit <hash>` starts each patch. A push sends each of
/// these commits, so the secrets gate reads them all.
pub fn history(clone: &std::path::Path, base: &str, head: &str) -> Result<String> {
    git::run(
        clone,
        &[
            "-c",
            "core.quotePath=false",
            "log",
            "--reverse",
            "--no-merges",
            "--no-show-signature",
            "--format=commit %H",
            "--patch",
            "--no-color",
            "--no-ext-diff",
            "--no-textconv",
            "--text",
            "--find-renames",
            "--unified=0",
            "--src-prefix=a/",
            "--dst-prefix=b/",
            &format!("{base}..{head}"),
        ],
    )
    .with_context(|| {
        format!(
            "could not read the commits {base}..{head} in {}",
            clone.display()
        )
    })
}

/// The commits from `old` to `new` in the clone, newest first, as one short line each.
pub fn commits_between(clone: &std::path::Path, old: &str, new: &str) -> Vec<String> {
    git::run(
        clone,
        &[
            "log",
            "--format=%h %s",
            "--max-count=5",
            &format!("{old}..{new}"),
        ],
    )
    .map(|text| {
        text.lines()
            .map(|line| line.chars().filter(|c| !c.is_control()).take(100).collect())
            .collect()
    })
    .unwrap_or_default()
}

/// The commit for people: its first 12 characters, or `-` for none.
pub fn short(commit: &str) -> &str {
    if commit.is_empty() {
        "-"
    } else {
        commit.get(..12).unwrap_or(commit)
    }
}
