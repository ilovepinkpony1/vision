//! The store of the memory repo at `$VISION_HOME/memory` (section 9).
//!
//! [`Memory`] reads the files of the work tree of `main`, and each write is one git commit whose
//! author is the writer. A write holds the lock of the memory (`.git/vision.lock`), allocates
//! the next free id, lints the files as they will be, writes each file through a temporary file
//! and a rename, and commits only the paths that it changed. When a step fails, the files go
//! back to their old text, so the repo never keeps half a change or a commit with an invalid
//! file.
//!
//! The person can edit the files with any tool. A write that touches a file with such a change
//! first commits that change as the person, when the file is valid.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::{anyhow, bail, Context, Result};
use serde::Serialize;

use super::doc::Doc;
use super::front::Value;
use super::lint::{lint, LintReport};
use super::model::{
    Basis, Decision, DecisionId, Kind, MemoryFile, MemoryId, NewDecision, NewPrinciple, Principle,
    PrincipleId, Problem, Status, UseKind, Uses, Writer, ALL_AGENTS, ARCHIVE, DECISIONS,
    PRINCIPLES, RETIRED_SECTION,
};
use crate::ctx::Ctx;
use crate::paths::Paths;
use crate::util::fs::{atomic_write, ensure_dir};
use crate::util::git::{self, Author};
use crate::util::time;

/// How long a write waits for the lock of the memory before it gives up.
pub const LOCK_WAIT: Duration = Duration::from_secs(30);

/// The memory repo. See the module text.
#[derive(Clone, Debug)]
pub struct Memory {
    dir: PathBuf,
    git_dir: PathBuf,
    person: Writer,
}

/// Which files a list holds.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum FileType {
    Decision,
    Principle,
}

/// The filters of a list. Each filter that is set must match.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Filter {
    /// Words that must each appear (case does not matter) in the question, the answer, the
    /// reason, or the rule.
    pub text: Option<String>,
    pub kind: Option<Kind>,
    /// One noun of `area`. Case does not matter.
    pub area: Option<String>,
    pub status: Option<Status>,
    /// The value of `by`, such as `judge` or the name of the person.
    pub writer: Option<String>,
    /// `agent` of a principle, or `asked_by` of a decision.
    pub agent: Option<String>,
    pub only: Option<FileType>,
}

impl Filter {
    pub fn matches(&self, file: &MemoryFile) -> bool {
        if let Some(only) = self.only {
            let is = match file {
                MemoryFile::Decision(_) => FileType::Decision,
                MemoryFile::Principle(_) => FileType::Principle,
            };
            if is != only {
                return false;
            }
        }
        if self.kind.is_some() && file.kind() != self.kind {
            return false;
        }
        if self.status.is_some_and(|s| s != file.status()) {
            return false;
        }
        if let Some(area) = &self.area {
            let area = area.trim();
            if !file.area().iter().any(|a| a.eq_ignore_ascii_case(area)) {
                return false;
            }
        }
        if let Some(writer) = &self.writer {
            if !file.by().name().eq_ignore_ascii_case(writer.trim()) {
                return false;
            }
        }
        if let Some(agent) = &self.agent {
            if !file
                .agent()
                .is_some_and(|a| a.eq_ignore_ascii_case(agent.trim()))
            {
                return false;
            }
        }
        if let Some(text) = &self.text {
            let haystack = file.search_texts().join("\n").to_lowercase();
            if !text
                .split_whitespace()
                .all(|word| haystack.contains(&word.to_lowercase()))
            {
                return false;
            }
        }
        true
    }
}

/// The files that match a filter, and the problems of each file that could not be read.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct Listing {
    pub files: Vec<MemoryFile>,
    pub problems: Vec<Problem>,
}

/// What the Judge rules need to know of a source id (section 8.3).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SourceState {
    /// The text is not an id, or no file has it.
    Missing,
    Active,
    /// The file exists with a different status.
    Inactive(Status),
    /// The file exists, but it is not valid.
    Broken,
}

/// The principles that one agent loads, and the Markdown text that its turn gets.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct PrinciplesFile {
    pub agent: String,
    pub text: String,
    /// Each principle in the text, with its version, for the `turns` row and `memory_use`.
    pub loaded: Vec<Loaded>,
    /// The problems of principle files that could not be read, so they are not in the text.
    pub problems: Vec<Problem>,
}

/// One principle that a turn got.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Loaded {
    pub id: PrincipleId,
    pub version: String,
}

/// One commit of the memory.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct CommitInfo {
    pub sha: String,
    pub short: String,
    /// RFC 3339 UTC.
    pub at: String,
    pub author: String,
    pub subject: String,
}

/// The hand edits that [`Memory::commit_hand_edits`] committed, and those that it left.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct HandEdits {
    pub committed: Vec<String>,
    pub left: Vec<Problem>,
}

/// A change would leave an invalid file, so the store changed nothing.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Invalid {
    pub problems: Vec<Problem>,
}

impl fmt::Display for Invalid {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "the change would leave an invalid file, so nothing changed:"
        )?;
        for problem in &self.problems {
            write!(f, "\n  {problem}")?;
        }
        Ok(())
    }
}

impl std::error::Error for Invalid {}

/// A new text for one path of the repo, or `None` to remove the file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Change {
    pub path: String,
    pub text: Option<String>,
}

impl Change {
    pub fn write(path: impl Into<String>, text: impl Into<String>) -> Change {
        Change {
            path: path.into(),
            text: Some(text.into()),
        }
    }
}

/// The held lock of the memory. The system releases it when the process ends.
#[derive(Debug)]
pub(crate) struct MemoryLock {
    _file: File,
}

impl Memory {
    /// Opens the memory of a home. `person` is the value of the configuration key `user`.
    pub fn open(paths: &Paths, person: &str) -> Result<Memory> {
        Memory::open_dir(&paths.memory_dir(), person)
    }

    /// Opens the memory of the home of a command, with the person of its configuration.
    pub fn from_ctx(ctx: &Ctx) -> Result<Memory> {
        ctx.require_setup()?;
        let config = ctx.config()?;
        Memory::open(&ctx.paths, &config.user)
    }

    /// Opens a memory repo in any folder, such as a temporary one in a test.
    pub fn open_dir(dir: &Path, person: &str) -> Result<Memory> {
        let person = Writer::person(person)?;
        if !dir.is_dir() {
            bail!(
                "there is no memory at {}. Run `vision init` to make it",
                dir.display()
            );
        }
        if !git::is_repo_root(dir) {
            bail!(
                "{} is not a git repo. Move it away and run `vision init` to make a new memory",
                dir.display()
            );
        }
        let git_dir = PathBuf::from(git::run(dir, &["rev-parse", "--absolute-git-dir"])?);
        let memory = Memory {
            dir: dir.to_path_buf(),
            git_dir,
            person,
        };
        memory.check_main()?;
        Ok(memory)
    }

    /// The folder of the repo.
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// The `.git` folder of the repo.
    pub fn git_dir(&self) -> &Path {
        &self.git_dir
    }

    /// The person: the writer of the commands of the person.
    pub fn person(&self) -> &Writer {
        &self.person
    }

    /// Vision reads and writes only the branch `main`, so that a reader never sees a proposal.
    pub(crate) fn check_main(&self) -> Result<()> {
        let fix = format!("Run `git -C {} checkout main`", self.dir.display());
        match git::run(&self.dir, &["symbolic-ref", "-q", "HEAD"]) {
            Ok(head) if head == "refs/heads/main" => Ok(()),
            Ok(head) => bail!(
                "the memory repo {} has `{}` checked out, but Vision reads and writes only the branch main. {fix}",
                self.dir.display(),
                head.trim_start_matches("refs/heads/")
            ),
            Err(_) => bail!(
                "the memory repo {} has no branch checked out, but Vision reads and writes only the branch main. {fix}",
                self.dir.display()
            ),
        }
    }

    // ----- Reading -------------------------------------------------------------------------

    /// The text of each file of `decisions/` and `principles/` (path in the repo -> text), and
    /// a problem for each entry that could not be read. Hidden files and `.gitkeep` do not count.
    pub fn snapshot(&self) -> Result<(BTreeMap<String, String>, Vec<Problem>)> {
        let mut files = BTreeMap::new();
        let mut problems = Vec::new();
        for folder in [DECISIONS, PRINCIPLES] {
            let dir = self.dir.join(folder);
            let entries = match fs::read_dir(&dir) {
                Ok(entries) => entries,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
                Err(e) => {
                    return Err(e)
                        .with_context(|| format!("could not read the folder {}", dir.display()))
                }
            };
            for entry in entries {
                let entry = entry
                    .with_context(|| format!("could not read the folder {}", dir.display()))?;
                let name = entry.file_name();
                let Some(name) = name.to_str() else {
                    problems.push(Problem {
                        path: format!("{folder}/{}", name.to_string_lossy()),
                        line: 1,
                        reason: "the file name is not UTF-8. Rename it".into(),
                    });
                    continue;
                };
                if name.starts_with('.') {
                    continue;
                }
                let path = format!("{folder}/{name}");
                if entry.path().is_dir() {
                    problems.push(Problem {
                        path,
                        line: 1,
                        reason: format!("a folder inside {folder}/ is not read. Move its files up, or move it out of the memory"),
                    });
                    continue;
                }
                match fs::read_to_string(entry.path()) {
                    Ok(text) => {
                        files.insert(path, text);
                    }
                    Err(e) => problems.push(Problem {
                        path,
                        line: 1,
                        reason: format!("could not read the file: {e}"),
                    }),
                }
            }
        }
        Ok((files, problems))
    }

    /// The files that match the filter, sorted by id, and the problems of each file that could
    /// not be read.
    pub fn list(&self, filter: &Filter) -> Result<Listing> {
        let (files, mut problems) = self.snapshot()?;
        let mut out = Vec::new();
        for (path, text) in &files {
            if MemoryId::from_path(path).is_none() {
                continue; // the lint names such a file
            }
            match MemoryFile::parse(path, text) {
                Ok(file) => {
                    if filter.matches(&file) {
                        out.push(file);
                    }
                }
                Err(file_problems) => problems.extend(file_problems),
            }
        }
        out.sort_by_key(|f| f.id());
        Ok(Listing {
            files: out,
            problems,
        })
    }

    /// The text of a file, or `None` when the file does not exist.
    pub fn read_text(&self, id: MemoryId) -> Result<Option<String>> {
        self.read_path(&id.path())
    }

    pub(crate) fn read_path(&self, path: &str) -> Result<Option<String>> {
        let full = self.dir.join(path);
        match fs::read_to_string(&full) {
            Ok(text) => Ok(Some(text)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e).with_context(|| format!("could not read {}", full.display())),
        }
    }

    /// One file. The error names a missing file, or each problem of a broken one.
    pub fn get(&self, id: MemoryId) -> Result<MemoryFile> {
        let (_, file) = self.load(id)?;
        Ok(file)
    }

    pub fn decision(&self, id: DecisionId) -> Result<Decision> {
        match self.get(id.into())? {
            MemoryFile::Decision(d) => Ok(d),
            MemoryFile::Principle(_) => bail!("{id} is not a decision"),
        }
    }

    pub fn principle(&self, id: PrincipleId) -> Result<Principle> {
        match self.get(id.into())? {
            MemoryFile::Principle(p) => Ok(p),
            MemoryFile::Decision(_) => bail!("{id} is not a principle"),
        }
    }

    /// The doc and the typed file of an id.
    fn load(&self, id: MemoryId) -> Result<(Doc, MemoryFile)> {
        let path = id.path();
        let full = self.dir.join(&path);
        let text = self.read_path(&path)?.ok_or_else(|| {
            anyhow!(
                "the memory has no {id}: {} does not exist. Run `vision memory` to list the files",
                full.display()
            )
        })?;
        let doc = Doc::parse(&text).map_err(|e| {
            anyhow!(
                "{}:{}: {}. Fix the file, such as with `vision memory edit {id}`",
                full.display(),
                e.line,
                e.message
            )
        })?;
        let file = MemoryFile::parse(&path, &text).map_err(|problems| {
            let lines: Vec<String> = problems
                .iter()
                .map(|p| format!("  {}:{}: {}", full.display(), p.line, p.reason))
                .collect();
            anyhow!(
                "{id} is not a valid file. Fix it, such as with `vision memory edit {id}`:\n{}",
                lines.join("\n")
            )
        })?;
        Ok((doc, file))
    }

    /// True when the text is an id and its file exists.
    pub fn exists(&self, id: &str) -> bool {
        MemoryId::parse(id).is_ok_and(|id| self.dir.join(id.path()).is_file())
    }

    /// The state of a source that the Judge names: missing, active, inactive, or broken.
    pub fn source_state(&self, id: &str) -> SourceState {
        let Ok(id) = MemoryId::parse(id) else {
            return SourceState::Missing;
        };
        if !self.dir.join(id.path()).is_file() {
            return SourceState::Missing;
        }
        match self.get(id) {
            Ok(file) if file.status() == Status::Active => SourceState::Active,
            Ok(file) => SourceState::Inactive(file.status()),
            Err(_) => SourceState::Broken,
        }
    }

    /// True when the text is an id, its file exists and is valid, and its status is `active`.
    pub fn is_active(&self, id: &str) -> bool {
        self.source_state(id) == SourceState::Active
    }

    /// The active principles that an agent loads: its own and those for `all`, by id.
    pub fn principles_for(&self, agent: &str) -> Result<Vec<Principle>> {
        Ok(self.loaded_principles(agent)?.0)
    }

    fn loaded_principles(&self, agent: &str) -> Result<(Vec<Principle>, Vec<Problem>)> {
        let filter = Filter {
            only: Some(FileType::Principle),
            status: Some(Status::Active),
            ..Filter::default()
        };
        let listing = self.list(&filter)?;
        let agent = agent.trim();
        let principles = listing
            .files
            .into_iter()
            .filter_map(|f| match f {
                MemoryFile::Principle(p) => Some(p),
                MemoryFile::Decision(_) => None,
            })
            .filter(|p| p.agent == agent || p.agent == ALL_AGENTS)
            .collect();
        let problems = listing
            .problems
            .into_iter()
            .filter(|p| p.path.starts_with(&format!("{PRINCIPLES}/")))
            .collect();
        Ok((principles, problems))
    }

    /// The principles file of a turn of this agent (section 7.2): one Markdown text of its
    /// active principles, and the version of each one.
    pub fn principles_file(&self, agent: &str) -> Result<PrinciplesFile> {
        let (principles, problems) = self.loaded_principles(agent)?;
        Ok(PrinciplesFile {
            agent: agent.trim().to_string(),
            text: render_principles(agent.trim(), &principles),
            loaded: principles
                .iter()
                .map(|p| Loaded {
                    id: p.id,
                    version: p.version.clone(),
                })
                .collect(),
            problems,
        })
    }

    /// Lints each file of the memory.
    pub fn lint(&self) -> Result<LintReport> {
        let (files, mut problems) = self.snapshot()?;
        problems.extend(lint(&files, None));
        problems.sort();
        Ok(LintReport {
            files: files.len(),
            problems,
        })
    }

    /// The commits of `main`, newest first.
    pub fn log(&self, limit: Option<usize>) -> Result<Vec<CommitInfo>> {
        let mut args = vec![
            "log".to_string(),
            "--format=%H%x1f%h%x1f%aI%x1f%an%x1f%s".to_string(),
        ];
        if let Some(n) = limit {
            args.push(format!("-n{n}"));
        }
        args.push("main".to_string());
        args.push("--".to_string());
        let args: Vec<&str> = args.iter().map(String::as_str).collect();
        let out = git::run(&self.dir, &args)?;
        Ok(out.lines().filter_map(parse_log_line).collect())
    }

    /// The commit that `main` points at.
    pub fn head(&self) -> Result<String> {
        git::run(&self.dir, &["rev-parse", "main"])
    }

    // ----- Writing -------------------------------------------------------------------------

    /// Adds a decision of the person, the Judge, or an import, in one commit whose author is
    /// its writer. The store gives the id and the status `active`.
    pub fn add_decision(&self, new: &NewDecision) -> Result<Decision> {
        check_new_decision(new)?;
        let _lock = self.lock()?;
        self.check_main()?;
        let id = self.next_decision()?;
        let change = Change::write(id.path(), new.render(id, None, None));
        self.check_changes(std::slice::from_ref(&change))?;
        let message = format!("Add {id}: {}", short(&new.question, 60));
        self.write_commit(&[change], &message, &new.by)?;
        self.decision(id)
    }

    /// Replaces an active decision with a new one, in one commit: the old file gets the
    /// status `superseded`, and the new one names it in `supersedes`.
    pub fn supersede(&self, old: DecisionId, new: &NewDecision) -> Result<Decision> {
        check_new_decision(new)?;
        let _lock = self.lock()?;
        self.check_main()?;
        let (mut doc, file) = self.load(old.into())?;
        if file.status() != Status::Active {
            bail!(
                "{old} has the status {}, so it cannot be superseded. Supersede the active decision instead",
                file.status()
            );
        }
        doc.set("status", Value::str(Status::Superseded.as_str()));
        let id = self.next_decision()?;
        let changes = [
            Change::write(old.path(), doc.to_text()),
            Change::write(id.path(), new.render(id, Some(old), None)),
        ];
        self.check_changes(&changes)?;
        let message = format!("Supersede {old} with {id}: {}", short(&new.question, 50));
        self.write_commit(&changes, &message, &new.by)?;
        self.decision(id)
    }

    /// Adds a principle in one commit. A principle of the person is active at once. One of the
    /// Curator is a candidate.
    pub fn add_principle(&self, new: &NewPrinciple) -> Result<Principle> {
        if !matches!(new.by, Writer::Person(_) | Writer::Curator) {
            bail!(
                "only the person or the Curator writes a principle, not {}",
                new.by
            );
        }
        check_agent_name(&new.agent)?;
        if new.rule.trim().is_empty() {
            bail!("a principle needs a rule, such as \"When X, make sure that Y\"");
        }
        let _lock = self.lock()?;
        self.check_main()?;
        let id = self.next_principle()?;
        let change = Change::write(id.path(), new.render(id));
        self.check_changes(std::slice::from_ref(&change))?;
        let message = format!(
            "Add {id} for {}: {}",
            new.agent.trim(),
            short(&new.rule, 50)
        );
        self.write_commit(&[change], &message, &new.by)?;
        self.principle(id)
    }

    /// Sets the status of a file in one commit. `retired` needs the reason, which the file
    /// keeps in a section `## Retired`. Gives false when the file already had the status.
    pub fn set_status(
        &self,
        id: MemoryId,
        status: Status,
        why: Option<&str>,
        writer: &Writer,
    ) -> Result<bool> {
        let allowed = if id.is_decision() {
            Status::DECISION
        } else {
            Status::PRINCIPLE
        };
        if !allowed.contains(&status) {
            let names: Vec<&str> = allowed.iter().map(|s| s.as_str()).collect();
            bail!(
                "{id} cannot have the status {status}. Use one of: {}",
                names.join(", ")
            );
        }
        let why = why.map(str::trim).filter(|w| !w.is_empty());
        if status == Status::Retired && why.is_none() {
            bail!("say why {id} no longer applies. The file keeps the reason");
        }
        let _lock = self.lock()?;
        self.check_main()?;
        let (mut doc, file) = self.load(id)?;
        if file.status() == status {
            return Ok(false);
        }
        doc.set("status", Value::str(status.as_str()));
        let message = if let (Status::Retired, Some(why)) = (status, why) {
            doc.set("retired_at", Value::str(time::now_rfc3339()));
            doc.set_section(RETIRED_SECTION, why);
            format!("Retire {id}: {}", short(why, 60))
        } else {
            doc.remove("retired_at");
            doc.remove_section(RETIRED_SECTION);
            format!("Set {id} to {status}")
        };
        let change = Change::write(id.path(), doc.to_text());
        self.check_changes(std::slice::from_ref(&change))?;
        self.write_commit(&[change], &message, writer)?;
        Ok(true)
    }

    /// Writes `why` into the empty section `## Why` of a decision, in one commit of `writer`,
    /// such as the answer of the person to a question of the Curator. Gives false, and changes
    /// nothing, when the section already has a text.
    pub fn fill_decision_why(&self, id: DecisionId, why: &str, writer: &Writer) -> Result<bool> {
        let why = why.trim();
        if why.is_empty() {
            bail!("the reason for {id} is empty");
        }
        let _lock = self.lock()?;
        self.check_main()?;
        let (mut doc, file) = self.load(id.into())?;
        let MemoryFile::Decision(decision) = file else {
            bail!("{id} is not a decision");
        };
        if !decision.why.trim().is_empty() {
            return Ok(false);
        }
        doc.set_section("Why", why);
        let change = Change::write(id.path(), doc.to_text());
        self.check_changes(std::slice::from_ref(&change))?;
        let message = format!("Write the reason of {id}: {}", short(why, 50));
        self.write_commit(&[change], &message, writer)?;
        Ok(true)
    }

    /// Retires a file in one commit. It stays, and no turn loads it.
    pub fn retire(&self, id: MemoryId, why: &str, writer: &Writer) -> Result<bool> {
        self.set_status(id, Status::Retired, Some(why), writer)
    }

    /// Adds one to a count of `uses` of each principle, in one commit.
    pub fn count_uses(&self, uses: &[(PrincipleId, UseKind)], writer: &Writer) -> Result<()> {
        if uses.is_empty() {
            return Ok(());
        }
        let mut deltas: BTreeMap<PrincipleId, Uses> = BTreeMap::new();
        for (id, kind) in uses {
            deltas.entry(*id).or_default().add(*kind, 1);
        }
        let _lock = self.lock()?;
        self.check_main()?;
        let mut changes = Vec::new();
        for (id, delta) in &deltas {
            let (mut doc, file) = self.load((*id).into())?;
            let MemoryFile::Principle(principle) = file else {
                bail!("{id} is not a principle");
            };
            let mut total = principle.uses;
            total.add(UseKind::Used, delta.used);
            total.add(UseKind::Agreed, delta.agreed);
            total.add(UseKind::Corrected, delta.corrected);
            doc.set("uses", total.to_value());
            changes.push(Change::write(id.path(), doc.to_text()));
        }
        self.check_changes(&changes)?;
        let ids: Vec<String> = deltas.keys().map(|id| id.to_string()).collect();
        let message = format!("Count the uses of {}", ids.join(", "));
        self.write_commit(&changes, &message, writer)?;
        Ok(())
    }

    /// Replaces the whole text of a file, such as after `vision memory edit`. `expected` is the
    /// text that the writer started from: when the file changed since then, nothing changes.
    /// An invalid text is an [`Invalid`] error. Gives false when the text is the same.
    pub fn replace_text(
        &self,
        id: MemoryId,
        expected: &str,
        text: &str,
        writer: &Writer,
    ) -> Result<bool> {
        let _lock = self.lock()?;
        self.check_main()?;
        let current = self.read_text(id)?;
        if current.as_deref() != Some(expected) {
            bail!(
                "{id} changed while you edited it, so your text was not saved. Run the command again"
            );
        }
        if text == expected {
            return Ok(false);
        }
        let change = Change::write(id.path(), text);
        self.check_changes(std::slice::from_ref(&change))?;
        self.write_commit(&[change], &format!("Edit {id}"), writer)?;
        Ok(true)
    }

    /// Commits each valid change that the person made by hand to the files of the memory, as
    /// the person. A file that is not valid, and a removed file, stay as they are.
    pub fn commit_hand_edits(&self) -> Result<HandEdits> {
        let _lock = self.lock()?;
        self.check_main()?;
        self.commit_hand_edits_locked(None)
    }

    /// Reverts one commit of `main` as a new commit whose author is `writer`. The work tree must
    /// hold no change that Vision cannot commit. A revert that conflicts, or that would make a
    /// file invalid, changes nothing.
    pub fn revert(&self, commit: &str, writer: &Writer) -> Result<CommitInfo> {
        let _lock = self.lock()?;
        self.check_main()?;
        let commit = commit.trim();
        let sha = git::run(
            &self.dir,
            &[
                "rev-parse",
                "--verify",
                "-q",
                &format!("{commit}^{{commit}}"),
            ],
        )
        .map_err(|_| {
            anyhow!(
                "the memory has no commit `{commit}`. Run `vision memory log` to see the commits"
            )
        })?;
        if git::run(&self.dir, &["merge-base", "--is-ancestor", &sha, "main"]).is_err() {
            bail!("the commit {commit} is not on the branch main of the memory. Run `vision memory log` to see the commits");
        }
        if git::run(
            &self.dir,
            &["rev-parse", "--verify", "-q", &format!("{sha}^")],
        )
        .is_err()
        {
            bail!(
                "the commit {commit} is the first commit of the memory, so it cannot be reverted"
            );
        }
        let edits = self.commit_hand_edits_locked(None)?;
        let dirty = git::run(
            &self.dir,
            &["status", "--porcelain", "--untracked-files=all"],
        )?;
        if !dirty.is_empty() {
            let left: Vec<String> = edits.left.iter().map(|p| format!("  {p}")).collect();
            bail!(
                "the memory has changes that are not committed, so a revert could lose them:\n{}\n{}\nFix or remove them first",
                dirty,
                left.join("\n")
            );
        }
        let subject = git::run(&self.dir, &["log", "-1", "--format=%s", &sha])?;
        let before: BTreeSet<(String, String)> = self.lint_keys()?;
        let author = author_of(writer);
        let author = Author {
            name: &author.0,
            email: &author.1,
        };
        if let Err(e) = git::run_as(&self.dir, &["revert", "--no-commit", &sha], author) {
            self.reset_hard();
            return Err(e).context(format!(
                "could not revert {commit}: a later commit changed the same lines. Change the files with `vision memory edit` instead"
            ));
        }
        let staged = git::run(&self.dir, &["diff", "--cached", "--name-only"])?;
        if staged.is_empty() {
            self.reset_hard();
            bail!("reverting {commit} changes nothing: a later commit already undid it");
        }
        let new: Vec<String> = self
            .lint()?
            .problems
            .into_iter()
            .filter(|p| !before.contains(&(p.path.clone(), p.reason.clone())))
            .map(|p| format!("  {p}"))
            .collect();
        if !new.is_empty() {
            self.reset_hard();
            bail!(
                "reverting {commit} would leave invalid files, so nothing changed:\n{}",
                new.join("\n")
            );
        }
        let message = format!("Revert \"{subject}\"\n\nThis reverts commit {sha}.");
        if let Err(e) = git::run_as(
            &self.dir,
            &["commit", "--quiet", "--no-verify", "-m", &message],
            author,
        ) {
            self.reset_hard();
            return Err(e);
        }
        self.log(Some(1))?
            .into_iter()
            .next()
            .context("the memory has no commit after the revert")
    }

    fn lint_keys(&self) -> Result<BTreeSet<(String, String)>> {
        Ok(self
            .lint()?
            .problems
            .into_iter()
            .map(|p| (p.path, p.reason))
            .collect())
    }

    /// Puts the work tree back to the last commit. Only after a check that it held no change.
    fn reset_hard(&self) {
        let _ = git::run(&self.dir, &["reset", "--quiet", "--hard", "HEAD"]);
    }

    // ----- The parts of a write ------------------------------------------------------------

    /// Takes the lock of the memory, and waits up to [`LOCK_WAIT`] for it.
    pub(crate) fn lock(&self) -> Result<MemoryLock> {
        let path = self.git_dir.join("vision.lock");
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&path)
            .with_context(|| format!("could not open the lock file {}", path.display()))?;
        let start = Instant::now();
        loop {
            match fs2::FileExt::try_lock_exclusive(&file) {
                Ok(()) => return Ok(MemoryLock { _file: file }),
                Err(e) if e.raw_os_error() == fs2::lock_contended_error().raw_os_error() => {
                    if start.elapsed() > LOCK_WAIT {
                        bail!(
                            "a different vision process held the lock of the memory ({}) for {} seconds. Try again later",
                            path.display(),
                            LOCK_WAIT.as_secs()
                        );
                    }
                    std::thread::sleep(Duration::from_millis(20));
                }
                Err(e) => {
                    return Err(e).with_context(|| format!("could not lock {}", path.display()))
                }
            }
        }
    }

    /// The next free decision id. The lock must be held.
    pub(crate) fn next_decision(&self) -> Result<DecisionId> {
        let n = self.max_number(DECISIONS)? + 1;
        DecisionId::new(n).context("no free decision id")
    }

    /// The next free principle id. The lock must be held.
    pub(crate) fn next_principle(&self) -> Result<PrincipleId> {
        let n = self.max_number(PRINCIPLES)? + 1;
        PrincipleId::new(n).context("no free principle id")
    }

    /// The highest id number of a folder: in the work tree, and in each commit of each branch,
    /// so that an id that a removed file or an open proposal had is never given again.
    pub(crate) fn max_number(&self, folder: &str) -> Result<u32> {
        let number = |name: &str| -> Option<u32> {
            let stem = name.rsplit('/').next()?.strip_suffix(".md")?;
            match MemoryId::parse(stem).ok()? {
                MemoryId::Decision(id) if folder == DECISIONS => Some(id.number()),
                MemoryId::Principle(id) if folder == PRINCIPLES => Some(id.number()),
                _ => None,
            }
        };
        let mut max = 0;
        let dir = self.dir.join(folder);
        if let Ok(entries) = fs::read_dir(&dir) {
            for entry in entries.flatten() {
                if let Some(n) = entry.file_name().to_str().and_then(number) {
                    max = max.max(n);
                }
            }
        }
        if git::run(&self.dir, &["rev-parse", "--verify", "-q", "HEAD"]).is_ok() {
            let names = git::run(
                &self.dir,
                &[
                    "log",
                    "--all",
                    "--format=",
                    "--name-only",
                    "--no-renames",
                    "--",
                    folder,
                ],
            )?;
            for line in names.lines() {
                if let Some(n) = number(line.trim()) {
                    max = max.max(n);
                }
            }
        }
        Ok(max)
    }

    /// Lints the files that the changes write, as the memory will be after them.
    pub(crate) fn check_changes(&self, changes: &[Change]) -> Result<()> {
        let (mut files, _) = self.snapshot()?;
        let mut only = BTreeSet::new();
        for change in changes {
            let is_memory_file = change.path.starts_with(&format!("{DECISIONS}/"))
                || change.path.starts_with(&format!("{PRINCIPLES}/"));
            match &change.text {
                Some(text) => {
                    files.insert(change.path.clone(), text.clone());
                    if is_memory_file {
                        only.insert(change.path.clone());
                    }
                }
                None => {
                    files.remove(&change.path);
                }
            }
        }
        let problems = lint(&files, Some(&only));
        if !problems.is_empty() {
            return Err(Invalid { problems }.into());
        }
        Ok(())
    }

    /// Writes the changes and commits only their paths, with `writer` as the author. The lock
    /// must be held. When the writer is not the person, a hand edit of one of the paths is
    /// committed first, as the person, so that a commit of the Judge never holds a change of
    /// the person. When a step fails, each file gets its old text back. Gives the new commit,
    /// or `None` when no text changed.
    pub(crate) fn write_commit(
        &self,
        changes: &[Change],
        message: &str,
        writer: &Writer,
    ) -> Result<Option<String>> {
        if *writer != self.person {
            let paths: Vec<String> = changes.iter().map(|c| c.path.clone()).collect();
            let edits = self.commit_hand_edits_locked(Some(&paths))?;
            if let Some(problem) = edits.left.first() {
                bail!(
                    "{} was changed by hand and is not valid, so Vision cannot change it: {problem}. Fix the file first",
                    problem.path
                );
            }
        }
        let mut old: Vec<(String, Option<String>)> = Vec::new();
        let mut changed = Vec::new();
        for change in changes {
            let before = self.read_path(&change.path)?;
            if before != change.text {
                old.push((change.path.clone(), before));
                changed.push(change);
            }
        }
        if changed.is_empty() {
            return Ok(None);
        }
        let result = self.write_and_commit(&changed, message, writer);
        if result.is_err() {
            for (path, text) in &old {
                let full = self.dir.join(path);
                let _ = match text {
                    Some(text) => atomic_write(&full, text.as_bytes()),
                    None => fs::remove_file(&full).map_err(Into::into),
                };
            }
            let mut args = vec!["reset", "--quiet", "--"];
            args.extend(old.iter().map(|(p, _)| p.as_str()));
            let _ = git::run(&self.dir, &args);
        }
        result
    }

    fn write_and_commit(
        &self,
        changes: &[&Change],
        message: &str,
        writer: &Writer,
    ) -> Result<Option<String>> {
        for change in changes {
            let full = self.dir.join(&change.path);
            match &change.text {
                Some(text) => {
                    if let Some(parent) = full.parent() {
                        ensure_dir(parent)?;
                    }
                    atomic_write(&full, text.as_bytes())?;
                }
                None => match fs::remove_file(&full) {
                    Ok(()) => {}
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                    Err(e) => {
                        return Err(e)
                            .with_context(|| format!("could not remove {}", full.display()))
                    }
                },
            }
        }
        self.commit_paths(changes.iter().map(|c| c.path.as_str()), message, writer)
    }

    /// Stages and commits only these paths, as `writer`. Gives `None` when the paths hold the
    /// text of the last commit, so that there is nothing to commit.
    pub(crate) fn commit_paths<'a>(
        &self,
        paths: impl IntoIterator<Item = &'a str>,
        message: &str,
        writer: &Writer,
    ) -> Result<Option<String>> {
        let paths: Vec<&str> = paths.into_iter().collect();
        let mut add = vec!["add", "--all", "--"];
        add.extend(paths.iter().copied());
        git::run(&self.dir, &add)?;
        let mut staged = vec!["diff", "--cached", "--name-only", "--"];
        staged.extend(paths.iter().copied());
        if git::run(&self.dir, &staged)?.is_empty() {
            return Ok(None);
        }
        let mut commit = vec![
            "commit",
            "--quiet",
            "--no-verify",
            "-m",
            message,
            "--only",
            "--",
        ];
        commit.extend(paths.iter().copied());
        let (name, email) = author_of(writer);
        git::run_as(
            &self.dir,
            &commit,
            Author {
                name: &name,
                email: &email,
            },
        )?;
        self.head().map(Some)
    }

    /// Commits the hand edits of these paths (or of each memory path) as the person. The lock
    /// must be held.
    pub(crate) fn commit_hand_edits_locked(&self, paths: Option<&[String]>) -> Result<HandEdits> {
        let specs: Vec<&str> = match paths {
            Some(paths) => paths.iter().map(String::as_str).collect(),
            None => vec![DECISIONS, PRINCIPLES, ARCHIVE],
        };
        let changed = self.changed_paths(&specs)?;
        let mut result = HandEdits::default();
        if changed.is_empty() {
            return Ok(result);
        }
        let (files, unreadable) = self.snapshot()?;
        let mut ok = Vec::new();
        for (path, removed) in changed {
            if removed {
                result.left.push(Problem {
                    path: path.clone(),
                    line: 1,
                    reason: format!(
                        "the file was removed by hand. Vision keeps each file: restore it with `git -C {} checkout -- {path}`, or retire it",
                        self.dir.display()
                    ),
                });
                continue;
            }
            if path == ARCHIVE {
                ok.push(path);
                continue;
            }
            if let Some(problem) = unreadable.iter().find(|p| p.path == path) {
                result.left.push(problem.clone());
                continue;
            }
            let only: BTreeSet<String> = [path.clone()].into();
            let problems = if files.contains_key(&path) {
                lint(&files, Some(&only))
            } else {
                vec![Problem {
                    path: path.clone(),
                    line: 1,
                    reason: format!(
                        "only the files of {DECISIONS}/ and {PRINCIPLES}/ are memory files"
                    ),
                }]
            };
            if problems.is_empty() {
                ok.push(path);
            } else {
                result.left.extend(problems);
            }
        }
        if !ok.is_empty() {
            let message = if ok.len() == 1 {
                format!("Hand edit of {}", ok[0])
            } else {
                format!("Hand edits of {} files", ok.len())
            };
            let person = self.person.clone();
            self.commit_paths(ok.iter().map(String::as_str), &message, &person)?;
            result.committed = ok;
        }
        Ok(result)
    }

    /// The paths under these pathspecs that differ from the last commit, and whether each one
    /// was removed.
    pub(crate) fn changed_paths(&self, specs: &[&str]) -> Result<Vec<(String, bool)>> {
        let mut args = vec![
            "status",
            "--porcelain=v1",
            "-z",
            "--untracked-files=all",
            "--",
        ];
        args.extend(specs.iter().copied());
        let out = git::run_bytes(&self.dir, &args)?;
        let text = String::from_utf8_lossy(&out);
        let mut items = text.split('\0').filter(|s| !s.is_empty());
        let mut changed = Vec::new();
        while let Some(item) = items.next() {
            if item.len() < 4 {
                continue;
            }
            let (code, path) = item.split_at(3);
            let code = code.as_bytes();
            let removed = code[0] == b'D' || code[1] == b'D';
            if code[0] == b'R' || code[0] == b'C' {
                if let Some(from) = items.next() {
                    changed.push((from.to_string(), true));
                }
            }
            changed.push((path.to_string(), removed));
        }
        Ok(changed)
    }
}

/// The name and the email of the commits of a writer.
pub(crate) fn author_of(writer: &Writer) -> (String, String) {
    (writer.name().to_string(), writer.email())
}

fn check_new_decision(new: &NewDecision) -> Result<()> {
    if new.question.trim().is_empty() {
        bail!("a decision needs the question");
    }
    if new.answer.trim().is_empty() {
        bail!("a decision needs the answer");
    }
    match (&new.by, new.basis) {
        (Writer::Curator, _) => bail!("the Curator never writes a decision; it proposes changes"),
        (Writer::Judge, Basis::Answer) => bail!(
            "an answer of the Judge has the basis decision, principle, or inference, not answer"
        ),
        (Writer::Person(_) | Writer::Import, basis) if basis != Basis::Answer => {
            bail!("a decision of {} has the basis answer, not {basis}", new.by)
        }
        _ => Ok(()),
    }
}

/// An agent name: `all`, or lowercase letters, digits, `-`, and `_`.
pub(crate) fn check_agent_name(agent: &str) -> Result<()> {
    let agent = agent.trim();
    let valid = !agent.is_empty()
        && agent
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '-' | '_'));
    if !valid {
        bail!("`{agent}` is not an agent name. Use an agent such as judge or builder, or all");
    }
    Ok(())
}

fn parse_log_line(line: &str) -> Option<CommitInfo> {
    let mut parts = line.split('\u{1f}');
    let sha = parts.next()?.to_string();
    let short = parts.next()?.to_string();
    let at_raw = parts.next()?;
    let at = time::parse_rfc3339(at_raw).map_or_else(|_| at_raw.to_string(), time::rfc3339);
    let author = parts.next()?.to_string();
    let subject = parts.next().unwrap_or("").to_string();
    Some(CommitInfo {
        sha,
        short,
        at,
        author,
        subject,
    })
}

/// The first line of a text, cut to `max` characters with `...` at the end.
pub fn short(text: &str, max: usize) -> String {
    let line = text.trim().lines().next().unwrap_or("").trim();
    if line.chars().count() <= max {
        return line.to_string();
    }
    let cut: String = line.chars().take(max.saturating_sub(3)).collect();
    format!("{}...", cut.trim_end())
}

/// The principles file of a turn: a Markdown text of the principles that an agent loads.
pub fn render_principles(agent: &str, principles: &[Principle]) -> String {
    let mut out = format!("# Principles for {agent}\n\n");
    if principles.is_empty() {
        out.push_str("The memory holds no active principle for this agent yet.\n");
        return out;
    }
    out.push_str(
        "The person accepted each rule below. Follow each rule whose case fits your step, but never \
         in a case that its \"Not when\" names. List each rule that you applied in `applied`, with \
         its id and one line that tells how.\n",
    );
    for p in principles {
        out.push_str(&format!("\n## {}\n\n{}\n", p.id, p.rule.trim()));
        if !p.because.trim().is_empty() {
            out.push_str(&format!("\nBecause: {}\n", p.because.trim()));
        }
        if !p.not_when.trim().is_empty() {
            out.push_str(&format!("\nNot when: {}\n", p.not_when.trim()));
        }
        let mut facts = vec![format!("For: {}", p.agent)];
        if let Some(kind) = p.kind {
            facts.push(format!("Kind: {kind}"));
        }
        if !p.area.is_empty() {
            facts.push(format!("Area: {}", p.area.join(", ")));
        }
        out.push_str(&format!("\n{}.\n", facts.join(". ")));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn principle(id: u32, agent: &str, rule: &str, because: &str, not_when: &str) -> Principle {
        Principle {
            id: PrincipleId::new(id).unwrap(),
            agent: agent.into(),
            kind: Some(Kind::Product),
            area: vec!["exports".into()],
            status: Status::Active,
            by: Writer::Person("user".into()),
            evidence: vec![],
            uses: Uses::default(),
            rule: rule.into(),
            because: because.into(),
            not_when: not_when.into(),
            retired: None,
            path: String::new(),
            version: String::new(),
        }
    }

    #[test]
    fn the_principles_text_of_an_agent() {
        let text = render_principles(
            "judge",
            &[
                principle(1, "all", "When X, make sure that Y.", "Z.", ""),
                principle(4, "judge", "When A, make sure that B.", "", "C."),
            ],
        );
        assert_eq!(
            text,
            "# Principles for judge\n\n\
             The person accepted each rule below. Follow each rule whose case fits your step, but never in a case that its \"Not when\" names. List each rule that you applied in `applied`, with its id and one line that tells how.\n\
             \n## P-001\n\nWhen X, make sure that Y.\n\nBecause: Z.\n\nFor: all. Kind: product. Area: exports.\n\
             \n## P-004\n\nWhen A, make sure that B.\n\nNot when: C.\n\nFor: judge. Kind: product. Area: exports.\n"
        );
        assert_eq!(
            render_principles("builder", &[]),
            "# Principles for builder\n\nThe memory holds no active principle for this agent yet.\n"
        );
    }

    #[test]
    fn the_next_id_is_above_each_id_of_the_work_tree_and_of_each_branch() {
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path();
        git::init(repo).unwrap();
        let author = Author {
            name: "t",
            email: "t@localhost",
        };
        fs::create_dir_all(repo.join(DECISIONS)).unwrap();
        fs::create_dir_all(repo.join(PRINCIPLES)).unwrap();
        fs::write(repo.join("decisions/D-0003.md"), "x").unwrap();
        git::commit_all(repo, "one", author).unwrap();
        git::run(repo, &["branch", "proposal/2026-11-16-1"]).unwrap();
        git::run(repo, &["checkout", "-q", "proposal/2026-11-16-1"]).unwrap();
        fs::write(repo.join("principles/P-007.md"), "x").unwrap();
        git::commit_all(repo, "two", author).unwrap();
        git::run(repo, &["checkout", "-q", "main"]).unwrap();
        fs::write(repo.join("decisions/D-0005.md"), "x").unwrap();
        fs::write(repo.join("decisions/D-12.md"), "x").unwrap();
        let memory = Memory::open_dir(repo, "user").unwrap();
        assert_eq!(memory.next_decision().unwrap().to_string(), "D-0006");
        assert_eq!(memory.next_principle().unwrap().to_string(), "P-008");
        fs::remove_file(repo.join("decisions/D-0005.md")).unwrap();
        assert_eq!(memory.next_decision().unwrap().to_string(), "D-0004");
    }

    #[test]
    fn short_cuts_long_lines() {
        assert_eq!(short("one\ntwo", 10), "one");
        assert_eq!(short("abcdefghijkl", 8), "abcde...");
        assert_eq!(short("  ok  ", 8), "ok");
    }

    #[test]
    fn log_lines_read_back() {
        let line = "abc123\u{1f}abc\u{1f}2026-11-16T16:30:00+02:00\u{1f}judge\u{1f}Add D-0001: Q";
        let info = parse_log_line(line).unwrap();
        assert_eq!(info.at, "2026-11-16T14:30:00Z");
        assert_eq!(info.author, "judge");
        assert_eq!(info.subject, "Add D-0001: Q");
    }

    #[test]
    fn filters_match_each_field() {
        let p = MemoryFile::Principle(principle(
            1,
            "judge",
            "When X, make sure that CSV.",
            "Finance imports it.",
            "",
        ));
        assert!(Filter::default().matches(&p));
        assert!(Filter {
            text: Some("csv FINANCE".into()),
            ..Filter::default()
        }
        .matches(&p));
        assert!(!Filter {
            text: Some("xlsx".into()),
            ..Filter::default()
        }
        .matches(&p));
        assert!(Filter {
            area: Some("Exports".into()),
            ..Filter::default()
        }
        .matches(&p));
        assert!(!Filter {
            kind: Some(Kind::Code),
            ..Filter::default()
        }
        .matches(&p));
        assert!(Filter {
            writer: Some("user".into()),
            ..Filter::default()
        }
        .matches(&p));
        assert!(Filter {
            agent: Some("judge".into()),
            ..Filter::default()
        }
        .matches(&p));
        assert!(!Filter {
            only: Some(FileType::Decision),
            ..Filter::default()
        }
        .matches(&p));
        assert!(!Filter {
            status: Some(Status::Retired),
            ..Filter::default()
        }
        .matches(&p));
    }
}
