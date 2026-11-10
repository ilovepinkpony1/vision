//! The proposals of the Curator (section 9.4), as library functions for the curator milestone.
//!
//! [`Memory::build_proposal`] writes the changes of the Curator (`agents/curator.schema.json`)
//! to a new branch `proposal/<date>-<n>` in one commit. It works in a separate work tree inside
//! `.git/`, so the branch `main` and its files never change: a reader of the memory never sees
//! half a proposal. The branch also holds `proposal.json`, the list of its changes and the state
//! of each one, and `ARCHIVE.md` with each line that the proposal removes or rewrites.
//!
//! - `add` gets the next free id and the status `candidate`, with the Curator as `by`.
//! - `edit` replaces the text of an existing file.
//! - `merge` writes the file that stays, and retires each file of `removes` (the files stay, so
//!   that each id that a decision names keeps its file).
//! - `retire` sets the status `retired` and keeps the reason in `## Retired`.
//! - `question` changes no file. It waits for the person.
//!
//! A change that names a missing file, or that would leave an invalid file, is skipped and
//! named in [`Built::skipped`]. Two changes of one file are refused, so that each change can be
//! accepted alone.
//!
//! [`Memory::edit_change`] replaces the text of the file of one open change on its branch, after
//! the lint and the rule "no line is lost", so that the person can edit a change and then
//! accept it.
//!
//! [`Memory::accept_change`] brings one change to `main` as one commit of the person: an
//! accepted candidate becomes `active`, and its `by` becomes the person. It refuses when the
//! proposal loses a line ([`super::archive::lost_lines`]) or when `main` changed a file of the
//! change since the proposal was made.

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

use ::time::Date;
use anyhow::{anyhow, bail, Context, Result};
use serde::{Deserialize, Serialize};

use super::archive::{
    append_entry, entry_of, lost_lines, removed_lines, render_entry, LostLine, PROPOSAL_FILE,
};
use super::doc::Doc;
use super::front::Value;
use super::lint::lint;
use super::model::{
    MemoryId, PrincipleId, Status, Writer, ARCHIVE, DECISIONS, PRINCIPLES, RETIRED_SECTION,
};
use super::store::{author_of, Change, Invalid, Memory};
use crate::util::fs::{atomic_write, ensure_dir};
use crate::util::git::{self, Author};
use crate::util::time;

/// The start of the name of each proposal branch.
pub const PROPOSAL_PREFIX: &str = "proposal/";

/// The kind of a change of the Curator.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Op {
    Add,
    Edit,
    Merge,
    Retire,
    Question,
}

impl Op {
    pub fn as_str(self) -> &'static str {
        match self {
            Op::Add => "add",
            Op::Edit => "edit",
            Op::Merge => "merge",
            Op::Retire => "retire",
            Op::Question => "question",
        }
    }
}

/// One change of the output of the Curator, as `agents/curator.schema.json` has it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CuratorChange {
    pub op: Op,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub removes: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub question: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub evidence: Vec<String>,
    pub reason: String,
}

/// The state of one change of a proposal.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ChangeState {
    Open,
    Accepted,
    Rejected,
}

/// One change of a proposal, as `proposal.json` keeps it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProposalChange {
    /// The number of the change in the proposal, from 1.
    pub n: usize,
    pub op: Op,
    pub reason: String,
    #[serde(default)]
    pub evidence: Vec<String>,
    /// The file that the change writes or retires.
    #[serde(default)]
    pub path: Option<String>,
    /// For a merge: the files that it retires.
    #[serde(default)]
    pub removes: Vec<String>,
    #[serde(default)]
    pub question: Option<String>,
    /// Each path that the change writes.
    #[serde(default)]
    pub files: Vec<String>,
    pub state: ChangeState,
    /// The commit of `main` of an accepted change.
    #[serde(default)]
    pub commit: Option<String>,
    /// The reason of a rejection, or the new path of a file that got a new id at acceptance.
    #[serde(default)]
    pub note: Option<String>,
}

impl ProposalChange {
    /// One line for people, such as `edit principles/P-007.md`.
    pub fn describe(&self) -> String {
        match self.op {
            Op::Question => format!("question: {}", self.question.as_deref().unwrap_or("")),
            Op::Merge => format!(
                "merge into {}, retiring {}",
                self.path.as_deref().unwrap_or("?"),
                self.removes.join(", ")
            ),
            op => format!("{} {}", op.as_str(), self.path.as_deref().unwrap_or("?")),
        }
    }
}

/// `proposal.json` of a proposal branch.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProposalFile {
    /// The branch, such as `proposal/2026-11-16-1`.
    pub name: String,
    /// The commit of `main` that the proposal starts from.
    pub base: String,
    /// RFC 3339 UTC.
    pub created: String,
    pub summary: String,
    pub changes: Vec<ProposalChange>,
}

impl ProposalFile {
    pub fn count(&self, state: ChangeState) -> usize {
        self.changes.iter().filter(|c| c.state == state).count()
    }
}

/// A change of the Curator that the build left out, and why.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct SkippedChange {
    /// The index of the change in the list of the Curator, from 0.
    pub index: usize,
    pub op: Op,
    pub reason: String,
}

/// The result of [`Memory::build_proposal`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Built {
    /// The new proposal, or `None` when no change could be written.
    pub proposal: Option<ProposalFile>,
    pub skipped: Vec<SkippedChange>,
}

/// One proposal branch, for a list.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ProposalInfo {
    pub name: String,
    pub summary: String,
    pub created: String,
    pub open: usize,
    pub accepted: usize,
    pub rejected: usize,
    /// Why the branch could not be read, when it could not.
    pub problem: Option<String>,
}

/// The diff of one file of a change.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct FileDiff {
    pub path: String,
    /// A unified diff from the start of the proposal to its branch.
    pub diff: String,
}

/// One change with the diff of each of its files.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ChangeView {
    pub change: ProposalChange,
    pub diffs: Vec<FileDiff>,
}

/// A proposal with its changes and the lines that it loses.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ProposalView {
    pub file: ProposalFile,
    /// Where the branch left `main`.
    pub base: String,
    pub tip: String,
    pub changes: Vec<ChangeView>,
    /// Each line that the proposal loses. A proposal that loses a line cannot be accepted.
    pub lost: Vec<LostLine>,
}

/// One accepted change.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Accepted {
    pub n: usize,
    /// The commit on `main`, or `None` for a question.
    pub commit: Option<String>,
    /// The file that the change wrote on `main`. An added file gets a new id when its id was
    /// taken in the meantime.
    pub path: Option<String>,
}

/// The files and the description of one planned change.
struct Planned {
    writes: Vec<(String, String)>,
    change: ProposalChange,
}

impl Memory {
    /// Writes the changes of the Curator to a new branch `proposal/<date>-<n>`, in one commit
    /// of the Curator. `main` does not change. See the module text.
    pub fn build_proposal(
        &self,
        summary: &str,
        changes: &[CuratorChange],
        date: Date,
    ) -> Result<Built> {
        let _lock = self.lock()?;
        self.check_main()?;
        self.commit_hand_edits_locked(None)?;
        let base = self.head()?;
        let name = self.next_proposal_name(date)?;
        let mut tree = Worktree::add(self, &name, Some(&base))?;
        let before = read_tree(&tree.dir)?;
        let mut after = before.clone();
        let mut next = self.max_number(PRINCIPLES)? + 1;
        let mut touched: BTreeMap<String, usize> = BTreeMap::new();
        let mut planned: Vec<ProposalChange> = Vec::new();
        let mut skipped = Vec::new();
        for (index, change) in changes.iter().enumerate() {
            let start = next;
            let n = planned.len() + 1;
            let result = plan(change, n, &before, &mut next).and_then(|plan| {
                for (path, _) in &plan.writes {
                    if let Some(other) = touched.get(path) {
                        return Err(format!(
                            "it changes {path}, which change {other} of this proposal changes too. Propose one change for each file"
                        ));
                    }
                }
                let mut overlay = after.clone();
                let mut only = BTreeSet::new();
                for (path, text) in &plan.writes {
                    overlay.insert(path.clone(), text.clone());
                    only.insert(path.clone());
                }
                let problems = lint(&overlay, Some(&only));
                if !problems.is_empty() {
                    let lines: Vec<String> = problems.iter().map(|p| p.to_string()).collect();
                    return Err(format!("it would leave an invalid file: {}", lines.join("; ")));
                }
                Ok((plan, overlay))
            });
            match result {
                Ok((plan, overlay)) => {
                    after = overlay;
                    for (path, _) in &plan.writes {
                        touched.insert(path.clone(), n);
                    }
                    planned.push(plan.change);
                }
                Err(reason) => {
                    next = start;
                    skipped.push(SkippedChange {
                        index,
                        op: change.op,
                        reason,
                    });
                }
            }
        }
        if planned.is_empty() {
            return Ok(Built {
                proposal: None,
                skipped,
            });
        }
        let empty = String::new();
        let mut archive = after.get(ARCHIVE).cloned().unwrap_or_default();
        for change in &planned {
            let files: Vec<(String, Vec<String>)> = change
                .files
                .iter()
                .map(|path| {
                    let old = before.get(path).unwrap_or(&empty);
                    let new = after.get(path).unwrap_or(&empty);
                    (path.clone(), removed_lines(old, new))
                })
                .collect();
            if files.iter().any(|(_, lines)| !lines.is_empty()) {
                let title = entry_title(&name, change.n);
                let entry = render_entry(&title, &change.describe(), &base, &change.reason, &files);
                archive = append_entry(&archive, &entry);
            }
        }
        after.insert(ARCHIVE.to_string(), archive);
        for (path, text) in &after {
            if before.get(path) != Some(text) {
                let full = tree.dir.join(path);
                if let Some(parent) = full.parent() {
                    ensure_dir(parent)?;
                }
                atomic_write(&full, text.as_bytes())?;
            }
        }
        let file = ProposalFile {
            name: name.clone(),
            base: base.clone(),
            created: time::now_rfc3339(),
            summary: summary.trim().to_string(),
            changes: planned,
        };
        write_proposal_file(&tree.dir, &file)?;
        let noun = if file.changes.len() == 1 {
            "change"
        } else {
            "changes"
        };
        let message = format!(
            "Propose {} {noun}: {}",
            file.changes.len(),
            super::store::short(summary, 60)
        );
        tree.commit(&message, &Writer::Curator)?;
        Ok(Built {
            proposal: Some(file),
            skipped,
        })
    }

    /// Each proposal branch, by name.
    pub fn proposals(&self) -> Result<Vec<ProposalInfo>> {
        let mut out = Vec::new();
        for name in self.proposal_branches()? {
            let info = match self.read_proposal_file(&name) {
                Ok(file) => ProposalInfo {
                    name: name.clone(),
                    summary: file.summary.clone(),
                    created: file.created.clone(),
                    open: file.count(ChangeState::Open),
                    accepted: file.count(ChangeState::Accepted),
                    rejected: file.count(ChangeState::Rejected),
                    problem: None,
                },
                Err(e) => ProposalInfo {
                    name: name.clone(),
                    summary: String::new(),
                    created: String::new(),
                    open: 0,
                    accepted: 0,
                    rejected: 0,
                    problem: Some(format!("{e:#}")),
                },
            };
            out.push(info);
        }
        Ok(out)
    }

    /// One proposal: its changes with a diff of each file, and the lines that it loses.
    pub fn proposal(&self, name: &str) -> Result<ProposalView> {
        let tip = self.branch_tip(name)?;
        let file = self.read_proposal_file(&tip)?;
        let base = self.merge_base(&tip)?;
        let mut changes = Vec::new();
        for change in &file.changes {
            let mut diffs = Vec::new();
            for path in &change.files {
                let diff = git::run(
                    self.dir(),
                    &[
                        "diff",
                        "--no-color",
                        "--no-ext-diff",
                        "--no-renames",
                        &base,
                        &tip,
                        "--",
                        path,
                    ],
                )?;
                diffs.push(FileDiff {
                    path: path.clone(),
                    diff,
                });
            }
            changes.push(ChangeView {
                change: change.clone(),
                diffs,
            });
        }
        let lost = self.lost_lines_of(&base, &tip)?;
        Ok(ProposalView {
            file,
            base,
            tip,
            changes,
            lost,
        })
    }

    /// The lines that a proposal branch loses, compared with where it left `main`.
    pub fn proposal_lost_lines(&self, name: &str) -> Result<Vec<LostLine>> {
        let tip = self.branch_tip(name)?;
        let base = self.merge_base(&tip)?;
        self.lost_lines_of(&base, &tip)
    }

    /// Brings one change of a proposal to `main` as one commit of the person, and marks it
    /// accepted on the proposal branch.
    pub fn accept_change(&self, name: &str, n: usize) -> Result<Accepted> {
        let _lock = self.lock()?;
        self.check_main()?;
        let tip = self.branch_tip(name)?;
        let mut file = self.read_proposal_file(&tip)?;
        let index = change_index(&file, n)?;
        let change = file.changes[index].clone();
        let accepted = match self.accepted_before(name, n)? {
            Some(commit) => Accepted {
                n,
                commit: Some(commit),
                path: change.path.clone(),
            },
            None if change.op == Op::Question => Accepted {
                n,
                commit: None,
                path: None,
            },
            None => {
                let base = self.merge_base(&tip)?;
                let lost = self.lost_lines_of(&base, &tip)?;
                if !lost.is_empty() {
                    let lines: Vec<String> = lost.iter().map(|l| format!("  {l}")).collect();
                    bail!(
                        "{name} loses lines that its ARCHIVE.md does not hold, so no change of it can be accepted. Reject it:\n{}",
                        lines.join("\n")
                    );
                }
                self.apply_change(name, &change, &base, &tip)?
            }
        };
        let entry = &mut file.changes[index];
        entry.state = ChangeState::Accepted;
        entry.commit = accepted.commit.clone();
        if accepted.path != change.path {
            if let (Some(old), Some(new)) = (&change.path, &accepted.path) {
                entry.note = Some(format!("accepted as {new}"));
                for path in &mut entry.files {
                    if path == old {
                        *path = new.clone();
                    }
                }
                entry.path = Some(new.clone());
            }
        }
        self.save_proposal_file(name, &file, &format!("Accept change {n}"))?;
        Ok(accepted)
    }

    /// Replaces the text of the file of one open change on its proposal branch, such as after
    /// the person edited it in `vision memory review`, in one commit of the person. The text
    /// must pass the lint (an [`Invalid`] error), and the rule "no line is lost" must still hold:
    /// the archive entry of the change is written again, from the text at the start of the
    /// proposal to the new text. `main` does not change. Gives false when the text is the same.
    pub fn edit_change(&self, name: &str, n: usize, text: &str) -> Result<bool> {
        let _lock = self.lock()?;
        self.check_main()?;
        let tip = self.branch_tip(name)?;
        let mut file = self.read_proposal_file(&tip)?;
        let index = change_index(&file, n)?;
        let change = file.changes[index].clone();
        let Some(path) = change.path.clone().filter(|p| change.files.contains(p)) else {
            bail!("change {n} of {name} writes no file, so there is nothing to edit");
        };
        let base = self.merge_base(&tip)?;
        let mut tree = Worktree::add(self, name, None)?;
        let mut files = read_tree(&tree.dir)?;
        if files.get(&path).map(String::as_str) == Some(text) {
            return Ok(false);
        }
        files.insert(path.clone(), text.to_string());
        let only: BTreeSet<String> = [path.clone()].into();
        let problems = lint(&files, Some(&only));
        if !problems.is_empty() {
            return Err(Invalid { problems }.into());
        }
        let mut removed = Vec::new();
        for p in &change.files {
            let old = self.show(&base, p)?.unwrap_or_default();
            let new = files.get(p).cloned().unwrap_or_default();
            removed.push((p.clone(), removed_lines(&old, &new)));
        }
        let title = entry_title(name, n);
        let entry = removed
            .iter()
            .any(|(_, lines)| !lines.is_empty())
            .then(|| render_entry(&title, &change.describe(), &base, &change.reason, &removed));
        let archive = files.get(ARCHIVE).cloned().unwrap_or_default();
        files.insert(
            ARCHIVE.to_string(),
            replace_entry(&archive, &title, entry.as_deref()),
        );
        let changed = git::run(
            self.dir(),
            &["diff", "--name-only", "--no-renames", &base, &tip],
        )?;
        let mut paths: BTreeSet<String> = changed.lines().map(str::to_string).collect();
        paths.insert(path.clone());
        paths.insert(ARCHIVE.to_string());
        paths.remove(PROPOSAL_FILE);
        let mut before = BTreeMap::new();
        let mut after = BTreeMap::new();
        for p in &paths {
            if let Some(old) = self.show(&base, p)? {
                before.insert(p.clone(), old);
            }
            if let Some(new) = files.get(p) {
                after.insert(p.clone(), new.clone());
            }
        }
        let lost = lost_lines(&before, &after);
        if !lost.is_empty() {
            let lines: Vec<String> = lost.iter().map(|l| format!("  {l}")).collect();
            bail!(
                "the edited text of change {n} would lose lines, so nothing changed:\n{}",
                lines.join("\n")
            );
        }
        for p in [&path, &ARCHIVE.to_string()] {
            if let Some(new) = files.get(p) {
                atomic_write(&tree.dir.join(p), new.as_bytes())?;
            }
        }
        file.changes[index].note = Some(format!("edited by {}", self.person().name()));
        write_proposal_file(&tree.dir, &file)?;
        let person = self.person().clone();
        tree.commit(&format!("Edit change {n}"), &person)?;
        Ok(true)
    }

    /// Accepts each open change of a proposal, in order. Gives the result of each one: a change
    /// that fails does not stop the next.
    pub fn accept_all(&self, name: &str) -> Result<Vec<(usize, Result<Accepted, String>)>> {
        let file = self.read_proposal_file(&self.branch_tip(name)?)?;
        let open: Vec<usize> = file
            .changes
            .iter()
            .filter(|c| c.state == ChangeState::Open)
            .map(|c| c.n)
            .collect();
        Ok(open
            .into_iter()
            .map(|n| (n, self.accept_change(name, n).map_err(|e| format!("{e:#}"))))
            .collect())
    }

    /// Marks one open change as rejected. `main` does not change.
    pub fn reject_change(&self, name: &str, n: usize, why: Option<&str>) -> Result<()> {
        let _lock = self.lock()?;
        self.check_main()?;
        let mut file = self.read_proposal_file(&self.branch_tip(name)?)?;
        let index = change_index(&file, n)?;
        file.changes[index].state = ChangeState::Rejected;
        file.changes[index].note = why
            .map(str::trim)
            .filter(|w| !w.is_empty())
            .map(str::to_string);
        self.save_proposal_file(name, &file, &format!("Reject change {n}"))
    }

    /// Marks each open change as rejected. Gives the number of changes.
    pub fn reject_all(&self, name: &str, why: Option<&str>) -> Result<usize> {
        let _lock = self.lock()?;
        self.check_main()?;
        let mut file = self.read_proposal_file(&self.branch_tip(name)?)?;
        let why = why
            .map(str::trim)
            .filter(|w| !w.is_empty())
            .map(str::to_string);
        let mut count = 0;
        for change in &mut file.changes {
            if change.state == ChangeState::Open {
                change.state = ChangeState::Rejected;
                change.note = why.clone();
                count += 1;
            }
        }
        if count > 0 {
            self.save_proposal_file(name, &file, &format!("Reject {count} changes"))?;
        }
        Ok(count)
    }

    /// Deletes a proposal branch whose changes are each accepted or rejected.
    pub fn delete_proposal(&self, name: &str) -> Result<()> {
        let _lock = self.lock()?;
        let file = self.read_proposal_file(&self.branch_tip(name)?)?;
        let open: Vec<String> = file
            .changes
            .iter()
            .filter(|c| c.state == ChangeState::Open)
            .map(|c| c.n.to_string())
            .collect();
        if !open.is_empty() {
            bail!(
                "{name} still has open changes ({}). Accept or reject each one first",
                open.join(", ")
            );
        }
        self.clean_worktrees();
        git::run(self.dir(), &["branch", "--quiet", "-D", name])?;
        Ok(())
    }

    // ----- The parts -----------------------------------------------------------------------

    fn proposal_branches(&self) -> Result<Vec<String>> {
        let out = git::run(
            self.dir(),
            &[
                "for-each-ref",
                "--format=%(refname)",
                "refs/heads/proposal/",
            ],
        )?;
        Ok(out
            .lines()
            .filter_map(|l| l.trim().strip_prefix("refs/heads/"))
            .map(str::to_string)
            .collect())
    }

    fn next_proposal_name(&self, date: Date) -> Result<String> {
        let day = format!(
            "{:04}-{:02}-{:02}",
            date.year(),
            u8::from(date.month()),
            date.day()
        );
        let prefix = format!("{PROPOSAL_PREFIX}{day}-");
        let number = |name: &str| name.trim().strip_prefix(&prefix)?.parse::<u32>().ok();
        let mut max = self
            .proposal_branches()?
            .iter()
            .filter_map(|b| number(b))
            .max()
            .unwrap_or(0);
        // A closed proposal leaves its name in `main`: in the last line of each accepted change
        // and in the archive. A new proposal never takes such a name, so that its changes never
        // match the commits and the archive entries of the old one.
        let log = git::run(
            self.dir(),
            &[
                "log",
                "main",
                "--format=%B",
                &format!("--grep=^Proposal: {prefix}"),
            ],
        )?;
        let archive = self.read_path(ARCHIVE)?.unwrap_or_default();
        let named = log
            .lines()
            .filter_map(|l| l.trim().strip_prefix("Proposal: "))
            .chain(archive.lines().filter_map(|l| l.strip_prefix("## ")))
            .filter_map(|l| l.split(" change ").next())
            .filter_map(number);
        max = named.fold(max, u32::max);
        Ok(format!("{prefix}{}", max + 1))
    }

    /// The commit of a proposal branch. The error names the open proposals.
    fn branch_tip(&self, name: &str) -> Result<String> {
        if !name.starts_with(PROPOSAL_PREFIX) {
            bail!(
                "`{name}` is not a proposal. A proposal is a branch such as proposal/2026-11-16-1"
            );
        }
        git::run(
            self.dir(),
            &[
                "rev-parse",
                "--verify",
                "-q",
                &format!("refs/heads/{name}^{{commit}}"),
            ],
        )
        .map_err(|_| anyhow!("the memory has no proposal {name}"))
    }

    fn merge_base(&self, tip: &str) -> Result<String> {
        git::run(self.dir(), &["merge-base", "main", tip])
            .context("the proposal has no commit in common with main")
    }

    /// The commit of `main` that already brought this change, after a stop between the commit
    /// and the mark on the proposal branch.
    fn accepted_before(&self, name: &str, n: usize) -> Result<Option<String>> {
        let out = git::run(
            self.dir(),
            &[
                "log",
                "main",
                "--format=%H",
                &format!("--grep=^{}$", proposal_trailer(name, n)),
            ],
        )?;
        Ok(out.lines().next().map(str::to_string))
    }

    fn read_proposal_file(&self, rev: &str) -> Result<ProposalFile> {
        let text = self.show(rev, PROPOSAL_FILE)?.ok_or_else(|| {
            anyhow!("{rev} has no {PROPOSAL_FILE}, so it is not a proposal of Vision")
        })?;
        serde_json::from_str(&text).with_context(|| {
            format!("the {PROPOSAL_FILE} of {rev} is not valid. Delete the branch")
        })
    }

    fn save_proposal_file(&self, name: &str, file: &ProposalFile, message: &str) -> Result<()> {
        let mut tree = Worktree::add(self, name, None)?;
        write_proposal_file(&tree.dir, file)?;
        let person = self.person().clone();
        tree.commit(message, &person)?;
        Ok(())
    }

    /// The text of a file at a commit, or `None` when the commit has no such file.
    fn show(&self, rev: &str, path: &str) -> Result<Option<String>> {
        if !self.tree_paths(rev)?.contains(path) {
            return Ok(None);
        }
        let bytes = git::run_bytes(self.dir(), &["show", &format!("{rev}:{path}")])?;
        String::from_utf8(bytes)
            .map(Some)
            .map_err(|_| anyhow!("{path} at {rev} is not UTF-8 text"))
    }

    fn tree_paths(&self, rev: &str) -> Result<BTreeSet<String>> {
        let out = git::run_bytes(self.dir(), &["ls-tree", "-r", "--name-only", "-z", rev])?;
        Ok(String::from_utf8_lossy(&out)
            .split('\0')
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .collect())
    }

    fn lost_lines_of(&self, base: &str, tip: &str) -> Result<Vec<LostLine>> {
        let changed = git::run(
            self.dir(),
            &["diff", "--name-only", "--no-renames", base, tip],
        )?;
        let mut paths: BTreeSet<String> = changed.lines().map(str::to_string).collect();
        paths.insert(ARCHIVE.to_string());
        paths.remove(PROPOSAL_FILE);
        let mut before = BTreeMap::new();
        let mut after = BTreeMap::new();
        for path in paths {
            if let Some(text) = self.show(base, &path)? {
                before.insert(path.clone(), text);
            }
            if let Some(text) = self.show(tip, &path)? {
                after.insert(path, text);
            }
        }
        Ok(lost_lines(&before, &after))
    }

    /// Writes the files of one change to `main` and commits them as the person.
    fn apply_change(
        &self,
        name: &str,
        change: &ProposalChange,
        base: &str,
        tip: &str,
    ) -> Result<Accepted> {
        let n = change.n;
        let mut changes: Vec<Change> = Vec::new();
        let mut final_path = change.path.clone();
        let mut renamed: Option<(String, String)> = None;
        for path in &change.files {
            let base_text = self.show(base, path)?;
            let main_text = self.read_path(path)?;
            let tip_text = self.show(tip, path)?;
            let mut target = path.clone();
            let writes_new_file = base_text.is_none()
                && change.path.as_deref() == Some(path.as_str())
                && matches!(change.op, Op::Add | Op::Merge);
            if main_text != base_text {
                if !writes_new_file {
                    bail!(
                        "{path} changed on main after {name} was made, so change {n} cannot be accepted. Reject it, and let the Curator propose again"
                    );
                }
                let id = self.next_principle()?;
                renamed = Some((id_of(path), id.to_string()));
                target = id.path();
                final_path = Some(target.clone());
            }
            let Some(mut text) = tip_text else {
                changes.push(Change {
                    path: target,
                    text: None,
                });
                continue;
            };
            let is_written_principle = change.path.as_deref() == Some(path.as_str())
                && path.starts_with(&format!("{PRINCIPLES}/"))
                && matches!(change.op, Op::Add | Op::Edit | Op::Merge);
            if is_written_principle {
                let mut doc = Doc::parse(&text).map_err(|e| {
                    anyhow!(
                        "{path} of {name} is not valid: line {}: {}",
                        e.line,
                        e.message
                    )
                })?;
                let status = doc
                    .get("status")
                    .and_then(Value::as_text)
                    .and_then(Status::parse);
                if status == Some(Status::Candidate) {
                    doc.set("status", Value::str(Status::Active.as_str()));
                }
                doc.set("by", Value::str(self.person().name()));
                if let Some((_, new)) = &renamed {
                    doc.set("id", Value::str(new.clone()));
                }
                text = doc.to_text();
            }
            changes.push(Change::write(target, text));
        }
        if let Some((old, new)) = &renamed {
            for c in &mut changes {
                if Some(&c.path) != final_path.as_ref() {
                    if let Some(text) = &mut c.text {
                        *text = text
                            .replace(&format!("Merged into {old}"), &format!("Merged into {new}"));
                    }
                }
            }
        }
        if let Some(tip_archive) = self.show(tip, ARCHIVE)? {
            if let Some(entry) = entry_of(&tip_archive, &entry_title(name, n)) {
                let main_archive = self.read_path(ARCHIVE)?.unwrap_or_default();
                changes.push(Change::write(ARCHIVE, append_entry(&main_archive, &entry)));
            }
        }
        self.check_changes(&changes)?;
        let message = format!(
            "Accept change {n} of {name}: {}\n\n{}\n\n{}",
            super::store::short(&change.describe(), 60),
            change.reason.trim(),
            proposal_trailer(name, n)
        );
        let person = self.person().clone();
        let commit = self.write_commit(&changes, &message, &person)?;
        Ok(Accepted {
            n,
            commit,
            path: final_path,
        })
    }

    /// Removes the work trees that a stopped process left in `.git/vision-work/`. The lock
    /// must be held.
    fn clean_worktrees(&self) {
        let dir = self.git_dir().join(WORK_DIR);
        if let Ok(entries) = std::fs::read_dir(&dir) {
            for entry in entries.flatten() {
                let _ = std::fs::remove_dir_all(entry.path());
            }
        }
        let _ = git::run(self.dir(), &["worktree", "prune"]);
    }
}

/// The folder in `.git/` of the work trees of the proposals.
const WORK_DIR: &str = "vision-work";

/// A separate work tree of a proposal branch, removed on drop. A new branch that was never
/// committed is deleted too.
struct Worktree<'a> {
    memory: &'a Memory,
    dir: PathBuf,
    branch: String,
    /// True when this work tree made the branch.
    made: bool,
    committed: bool,
}

impl<'a> Worktree<'a> {
    /// A work tree of `branch`: a new branch at `base`, or the existing branch when `base` is `None`.
    fn add(memory: &'a Memory, branch: &str, base: Option<&str>) -> Result<Worktree<'a>> {
        memory.clean_worktrees();
        let dir = memory
            .git_dir()
            .join(WORK_DIR)
            .join(uuid::Uuid::new_v4().to_string());
        if let Some(parent) = dir.parent() {
            ensure_dir(parent)?;
        }
        let dir_text = dir.to_string_lossy().to_string();
        match base {
            Some(base) => git::run(
                memory.dir(),
                &["worktree", "add", "--quiet", "-b", branch, &dir_text, base],
            )?,
            None => git::run(
                memory.dir(),
                &["worktree", "add", "--quiet", &dir_text, branch],
            )?,
        };
        Ok(Worktree {
            memory,
            dir,
            branch: branch.to_string(),
            made: base.is_some(),
            committed: false,
        })
    }

    fn commit(&mut self, message: &str, writer: &Writer) -> Result<()> {
        git::run(&self.dir, &["add", "--all"])?;
        let (name, email) = author_of(writer);
        git::run_as(
            &self.dir,
            &["commit", "--quiet", "--no-verify", "-m", message],
            Author {
                name: &name,
                email: &email,
            },
        )?;
        self.committed = true;
        Ok(())
    }
}

impl Drop for Worktree<'_> {
    fn drop(&mut self) {
        let dir = self.dir.to_string_lossy().to_string();
        if git::run(self.memory.dir(), &["worktree", "remove", "--force", &dir]).is_err() {
            let _ = std::fs::remove_dir_all(&self.dir);
            let _ = git::run(self.memory.dir(), &["worktree", "prune"]);
        }
        if self.made && !self.committed {
            let _ = git::run(
                self.memory.dir(),
                &["branch", "--quiet", "-D", &self.branch],
            );
        }
    }
}

/// The title of the archive entry of one change, such as `proposal/2026-11-16-1 change 2`.
fn entry_title(name: &str, n: usize) -> String {
    format!("{name} change {n}")
}

/// The archive with the entry of this title replaced by `entry`, or removed when `entry` is
/// `None`. An archive with no such entry gets `entry` at its end. Each other line stays as it is.
fn replace_entry(archive: &str, title: &str, entry: Option<&str>) -> String {
    let heading = format!("## {title}");
    let bare = |line: &str| line.strip_suffix('\r').unwrap_or(line).to_string();
    let lines: Vec<&str> = archive.split('\n').collect();
    let Some(start) = lines.iter().position(|l| bare(l).trim_end() == heading) else {
        return match entry {
            Some(entry) => append_entry(archive, entry),
            None => archive.to_string(),
        };
    };
    let end = lines[start + 1..]
        .iter()
        .position(|l| bare(l).starts_with("## "))
        .map_or(lines.len(), |i| start + 1 + i);
    let mut parts: Vec<String> = Vec::new();
    for part in [
        lines[..start].join("\n"),
        entry.unwrap_or("").to_string(),
        lines[end..].join("\n"),
    ] {
        let part = part.trim_end_matches(['\n', '\r']);
        if !part.trim().is_empty() {
            parts.push(part.to_string());
        }
    }
    let mut out = parts.join("\n\n");
    if !out.is_empty() {
        out.push('\n');
    }
    out
}

/// The last line of the commit message of an accepted change.
fn proposal_trailer(name: &str, n: usize) -> String {
    format!("Proposal: {name} change {n}")
}

fn change_index(file: &ProposalFile, n: usize) -> Result<usize> {
    let index = file
        .changes
        .iter()
        .position(|c| c.n == n)
        .ok_or_else(|| anyhow!("{} has no change {n}", file.name))?;
    let state = file.changes[index].state;
    if state != ChangeState::Open {
        let word = if state == ChangeState::Accepted {
            "accepted"
        } else {
            "rejected"
        };
        bail!("change {n} of {} is already {word}", file.name);
    }
    Ok(index)
}

fn id_of(path: &str) -> String {
    MemoryId::from_path(path).map_or_else(|| path.to_string(), |id| id.to_string())
}

fn write_proposal_file(dir: &std::path::Path, file: &ProposalFile) -> Result<()> {
    let mut text = serde_json::to_string_pretty(file)?;
    text.push('\n');
    atomic_write(&dir.join(PROPOSAL_FILE), text.as_bytes())
}

/// The files of the memory in a work tree: `decisions/`, `principles/`, and `ARCHIVE.md`.
fn read_tree(dir: &std::path::Path) -> Result<BTreeMap<String, String>> {
    let mut files = BTreeMap::new();
    for folder in [DECISIONS, PRINCIPLES] {
        let Ok(entries) = std::fs::read_dir(dir.join(folder)) else {
            continue;
        };
        for entry in entries.flatten() {
            let Some(name) = entry.file_name().to_str().map(str::to_string) else {
                continue;
            };
            if name.starts_with('.') || !entry.path().is_file() {
                continue;
            }
            if let Ok(text) = std::fs::read_to_string(entry.path()) {
                files.insert(format!("{folder}/{name}"), text);
            }
        }
    }
    if let Ok(text) = std::fs::read_to_string(dir.join(ARCHIVE)) {
        files.insert(ARCHIVE.to_string(), text);
    }
    Ok(files)
}

/// The files of one change of the Curator, checked against the files before the proposal.
fn plan(
    change: &CuratorChange,
    n: usize,
    before: &BTreeMap<String, String>,
    next: &mut u32,
) -> Result<Planned, String> {
    let reason = change.reason.trim();
    if reason.is_empty() {
        return Err("it has no reason".into());
    }
    let mut out = ProposalChange {
        n,
        op: change.op,
        reason: reason.to_string(),
        evidence: change.evidence.clone(),
        path: None,
        removes: Vec::new(),
        question: None,
        files: Vec::new(),
        state: ChangeState::Open,
        commit: None,
        note: None,
    };
    let mut writes: Vec<(String, String)> = Vec::new();
    match change.op {
        Op::Question => {
            let question = change.question.as_deref().map(str::trim).unwrap_or("");
            if question.is_empty() {
                return Err("a question needs the text in `question`".into());
            }
            out.question = Some(question.to_string());
        }
        Op::Add => {
            let path = change.path.as_deref().unwrap_or("");
            if !is_new_path(path) {
                return Err(format!(
                    "an add names a new path such as principles/NEW-1.md, not `{path}`"
                ));
            }
            let (target, text) = new_principle(change, next)?;
            out.path = Some(target.clone());
            writes.push((target, text));
        }
        Op::Edit => {
            let path = existing(change.path.as_deref(), before)?;
            let content = content_of(change)?;
            Doc::parse(content).map_err(|e| {
                format!(
                    "the content is not a memory file: line {}: {}",
                    e.line, e.message
                )
            })?;
            out.path = Some(path.clone());
            writes.push((path, content.to_string()));
        }
        Op::Merge => {
            let path = change.path.as_deref().unwrap_or("");
            let (target, text) = if is_new_path(path) {
                new_principle(change, next)?
            } else {
                let path = existing(Some(path), before)?;
                let content = content_of(change)?;
                Doc::parse(content).map_err(|e| {
                    format!(
                        "the content is not a memory file: line {}: {}",
                        e.line, e.message
                    )
                })?;
                (path, content.to_string())
            };
            if change.removes.is_empty() {
                return Err("a merge lists the files that it replaces in `removes`".into());
            }
            out.path = Some(target.clone());
            writes.push((target.clone(), text));
            let stays = id_of(&target);
            for remove in &change.removes {
                let remove = existing(Some(remove), before)?;
                if remove == target || out.removes.contains(&remove) {
                    continue;
                }
                let reason = format!("Merged into {stays}. {reason}");
                if let Some(text) = retired_text(&remove, before, &reason)? {
                    writes.push((remove.clone(), text));
                }
                out.removes.push(remove);
            }
        }
        Op::Retire => {
            let target = change.target.as_deref().or(change.path.as_deref());
            let path = existing(target, before)?;
            match retired_text(&path, before, reason)? {
                Some(text) => writes.push((path.clone(), text)),
                None => return Err(format!("{} is already retired", id_of(&path))),
            }
            out.path = Some(path);
        }
    }
    out.files = writes.iter().map(|(p, _)| p.clone()).collect();
    Ok(Planned {
        writes,
        change: out,
    })
}

fn is_new_path(path: &str) -> bool {
    path.strip_prefix(&format!("{PRINCIPLES}/NEW-"))
        .and_then(|rest| rest.strip_suffix(".md"))
        .is_some_and(|n| {
            !n.is_empty()
                && n.chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
        })
}

fn content_of(change: &CuratorChange) -> Result<&str, String> {
    match change.content.as_deref() {
        Some(c) if !c.trim().is_empty() => Ok(c),
        _ => Err(format!(
            "an {} needs the full new file in `content`",
            change.op.as_str()
        )),
    }
}

/// The path of an existing file, from an id such as `P-007` or a path such as `principles/P-007.md`.
fn existing(target: Option<&str>, before: &BTreeMap<String, String>) -> Result<String, String> {
    let target = target.map(str::trim).unwrap_or("");
    let path = match MemoryId::parse(target) {
        Ok(id) => id.path(),
        Err(_) => target.to_string(),
    };
    if MemoryId::from_path(&path).is_none() || !before.contains_key(&path) {
        return Err(format!("`{target}` is not a file of the memory"));
    }
    Ok(path)
}

/// A new candidate principle from the content of an add or a merge, with the next free id.
fn new_principle(change: &CuratorChange, next: &mut u32) -> Result<(String, String), String> {
    let content = content_of(change)?;
    let mut doc = Doc::parse(content).map_err(|e| {
        format!(
            "the content is not a memory file: line {}: {}",
            e.line, e.message
        )
    })?;
    let id = PrincipleId::new(*next).ok_or("no free principle id")?;
    *next += 1;
    doc.set("id", Value::str(id.to_string()));
    doc.set("status", Value::str(Status::Candidate.as_str()));
    doc.set("by", Value::str(Writer::Curator.name()));
    Ok((id.path(), doc.to_text()))
}

/// The text of a file with the status `retired` and the reason, or `None` when it is retired.
fn retired_text(
    path: &str,
    before: &BTreeMap<String, String>,
    reason: &str,
) -> Result<Option<String>, String> {
    let text = before
        .get(path)
        .ok_or_else(|| format!("{path} is not a file of the memory"))?;
    let mut doc = Doc::parse(text)
        .map_err(|e| format!("{path} is not valid: line {}: {}", e.line, e.message))?;
    let status = doc
        .get("status")
        .and_then(Value::as_text)
        .and_then(Status::parse);
    if status == Some(Status::Retired) {
        return Ok(None);
    }
    doc.set("status", Value::str(Status::Retired.as_str()));
    doc.set("retired_at", Value::str(time::now_rfc3339()));
    doc.set_section(RETIRED_SECTION, reason);
    Ok(Some(doc.to_text()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn curator_changes_follow_the_schema() {
        let json = r#"{"summary": "s", "changes": [
            {"op": "add", "path": "principles/NEW-1.md", "content": "---\n---\n", "evidence": ["D-0001"], "reason": "r"},
            {"op": "retire", "target": "P-001", "reason": "r"},
            {"op": "question", "question": "Q?", "reason": "r"}]}"#;
        #[derive(Deserialize)]
        struct Output {
            changes: Vec<CuratorChange>,
        }
        let out: Output = serde_json::from_str(json).unwrap();
        assert_eq!(out.changes[0].op, Op::Add);
        assert_eq!(out.changes[1].target.as_deref(), Some("P-001"));
        assert!(serde_json::from_str::<CuratorChange>(
            r#"{"op": "add", "reason": "r", "colour": 1}"#
        )
        .is_err());
    }

    #[test]
    fn an_archive_entry_is_replaced_or_removed_and_the_rest_stays() {
        let archive =
            "## p change 1\n\n- Reason: a\n\n> one  \n\n## p change 2\n\n- Reason: b\n\n> two\n";
        assert_eq!(
            replace_entry(
                archive,
                "p change 1",
                Some("## p change 1\n\n- Reason: c\n")
            ),
            "## p change 1\n\n- Reason: c\n\n## p change 2\n\n- Reason: b\n\n> two\n"
        );
        assert_eq!(
            replace_entry(archive, "p change 2", None),
            "## p change 1\n\n- Reason: a\n\n> one  \n"
        );
        assert_eq!(
            replace_entry(archive, "p change 3", Some("## p change 3\n")),
            format!("{}\n\n## p change 3\n", archive.trim_end())
        );
        assert_eq!(replace_entry("", "p change 1", None), "");
    }

    #[test]
    fn new_paths_have_the_form_of_the_design() {
        assert!(is_new_path("principles/NEW-1.md"));
        assert!(!is_new_path("principles/P-001.md"));
        assert!(!is_new_path("decisions/NEW-1.md"));
        assert!(!is_new_path("principles/NEW-../x.md"));
    }
}
