//! The archive of the sessions of Claude Code of the person (section 16).
//!
//! Claude Code keeps one file of JSON lines for each session under its folder of projects
//! (`~/.claude/projects/<project folder>/<session id>.jsonl`), and the files of the subagents of
//! a session in a folder of the session (`<session id>/subagents/agent-<id>.jsonl`, and
//! `.../subagents/workflows/<run>/...jsonl`). It deletes them itself after some days. Vision
//! only reads that folder: it never writes, moves, or deletes a file in it.
//!
//! - A file is finished when it has not changed for `sessions.quiet_hours` hours. A finished
//!   file whose content the archive does not hold yet goes to the archive as
//!   `<prefix>sessions/<project folder>/<file>.gz`, through a copy in a temporary folder and the
//!   `gzip` program. A file that grew after its archive, such as a resumed session, goes again
//!   under the same key: the new copy replaces the old one after the size check.
//! - The summary of each session ([`summary`]) goes to
//!   `session-journal/<project folder>/<session id>.md`, after the secrets and the forms of
//!   tokens are masked, and MemPalace files it with the replace by source of the journal: the
//!   wing of `sessions.wing`, the project folder as the room. The raw session text never goes
//!   to MemPalace.
//! - The ledger table `sessions` keeps what the archive and the index hold.
//!
//! The tick runs the archive at most once an hour, with the archive run of the turn folders,
//! and at most [`TICK_FILES`] files or [`TICK_BYTES`] bytes in one run.

mod commands;
pub mod summary;

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{bail, Context, Result};
use serde::Serialize;
use serde_json::json;

use super::archive::Store;
use super::errors::cut;
use super::index::{self, Filed};
use super::{mcp, Inputs};
use crate::adapters::jira_github::proc::{self, How, TempDir};
use crate::cli::SessionsCommand;
use crate::config::Archive;
use crate::ctx::Ctx;
use crate::ledger::sessions::{SessionArchived, SessionRow};
use crate::ledger::{Event, Ledger};
use crate::util::fs::{atomic_write, ensure_dir, read_optional};
use crate::util::{ids, time};

/// The most files that one run of the tick copies. `vision sessions archive` copies each one.
pub const TICK_FILES: usize = 40;
/// The most bytes of session files that one run of the tick copies.
pub const TICK_BYTES: u64 = 1_000_000_000;
/// The event of each run.
pub const RUN_EVENT: &str = "sessions-run";
/// The folder of the summaries in the journal.
/// The room of a project folder with no usable name.
pub const ROOM_FALLBACK: &str = "sessions";

/// The folder of the summaries under the home. No work turn reads it.
pub const SUMMARY_FOLDER: &str = "session-journal";
/// The deepest folder under a session folder that the scan reads.
const MAX_DEPTH: usize = 6;
/// The time limit of one call of `gzip`.
const GZIP_TIMEOUT: Duration = Duration::from_secs(900);

/// `vision sessions ...`.
pub fn run(ctx: &Ctx, command: SessionsCommand) -> Result<()> {
    commands::run(ctx, command)
}

/// One session file under the folder of the projects.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct SessionFile {
    #[serde(skip)]
    pub path: PathBuf,
    /// The path relative to the folder of the projects, with `/`.
    pub rel: String,
    pub project: String,
    pub session: String,
    /// True for the file of the session itself, false for the file of a subagent.
    pub main: bool,
    pub size: u64,
    /// The change time in nanoseconds since 1970.
    pub mtime: i64,
}

/// The key of a session file in the archive: `<prefix>sessions/<rel>.gz`.
pub fn session_key(config: &Archive, rel: &str) -> String {
    format!("{}sessions/{rel}.gz", config.prefix)
}

/// The summary entry of a session, relative to the home.
pub fn entry_rel(project: &str, session: &str) -> String {
    format!("{SUMMARY_FOLDER}/{project}/{session}.md")
}

/// The room of a project folder in MemPalace: the name with each character that is not a
/// letter, a digit, a space, or one of `_ . ' -` as `-`, with the ends that are not a letter
/// or a digit left out (MemPalace wants a name that starts and ends with one), and at most 120
/// characters. `-Users-me-work` gives `Users-me-work`.
pub fn room_of(project: &str) -> String {
    let clean: String = project
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '_' | ' ' | '.' | '\'' | '-') {
                c
            } else {
                '-'
            }
        })
        .collect::<String>()
        .replace("..", ".");
    let trimmed = clean.trim_matches(|c: char| !c.is_ascii_alphanumeric());
    let cut: String = trimmed.chars().take(120).collect();
    let room = cut.trim_end_matches(|c: char| !c.is_ascii_alphanumeric());
    if room.is_empty() {
        ROOM_FALLBACK.to_string()
    } else {
        room.to_string()
    }
}

/// True for a name that is safe as one part of a key and of a path: not empty, not `.` or
/// `..`, with no control character and no `/` or `\`.
fn safe_part(name: &str) -> bool {
    !name.is_empty()
        && name != "."
        && name != ".."
        && !name.starts_with('.')
        && !name
            .chars()
            .any(|c| c.is_control() || c == '/' || c == '\\')
}

fn mtime_nanos(meta: &std::fs::Metadata) -> i64 {
    meta.modified()
        .ok()
        .and_then(|m| m.duration_since(UNIX_EPOCH).ok())
        .map_or(0, |d| i64::try_from(d.as_nanos()).unwrap_or(i64::MAX))
}

/// Each session file under the folder of the projects, by path: `<project>/<session>.jsonl`
/// and each `*.jsonl` in a folder `<project>/<session>/`. A link is never followed, and a name
/// that starts with `.` is left out.
pub fn scan(projects: &Path) -> Vec<SessionFile> {
    let mut out = Vec::new();
    let Ok(projects_dir) = std::fs::read_dir(projects) else {
        return out;
    };
    for project in projects_dir.flatten() {
        let name = project.file_name().to_string_lossy().into_owned();
        if !safe_part(&name) || !project.path().symlink_metadata().is_ok_and(|m| m.is_dir()) {
            continue;
        }
        let Ok(entries) = std::fs::read_dir(project.path()) else {
            continue;
        };
        for entry in entries.flatten() {
            let file_name = entry.file_name().to_string_lossy().into_owned();
            if !safe_part(&file_name) {
                continue;
            }
            let Ok(meta) = entry.path().symlink_metadata() else {
                continue;
            };
            if meta.is_file() {
                if let Some(session) = file_name.strip_suffix(".jsonl") {
                    out.push(SessionFile {
                        path: entry.path(),
                        rel: format!("{name}/{file_name}"),
                        project: name.clone(),
                        session: session.to_string(),
                        main: true,
                        size: meta.len(),
                        mtime: mtime_nanos(&meta),
                    });
                }
            } else if meta.is_dir() {
                let base = format!("{name}/{file_name}");
                walk_session(&entry.path(), &base, &name, &file_name, 0, &mut out);
            }
        }
    }
    out.sort_by(|a, b| a.rel.cmp(&b.rel));
    out
}

/// Each `*.jsonl` file in a folder of a session, such as the files of its subagents.
fn walk_session(
    dir: &Path,
    rel: &str,
    project: &str,
    session: &str,
    depth: usize,
    out: &mut Vec<SessionFile>,
) {
    if depth > MAX_DEPTH {
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if !safe_part(&name) {
            continue;
        }
        let Ok(meta) = entry.path().symlink_metadata() else {
            continue;
        };
        let path_rel = format!("{rel}/{name}");
        if meta.is_dir() {
            walk_session(&entry.path(), &path_rel, project, session, depth + 1, out);
        } else if meta.is_file() && name.ends_with(".jsonl") {
            out.push(SessionFile {
                path: entry.path(),
                rel: path_rel,
                project: project.to_string(),
                session: session.to_string(),
                main: false,
                size: meta.len(),
                mtime: mtime_nanos(&meta),
            });
        }
    }
}

/// What a run of the archive of the sessions does.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct Plan {
    /// The finished files whose content the archive does not hold yet, as far as their size and
    /// change time tell.
    pub copies: Vec<SessionFile>,
    /// The archived main files whose summary waits.
    pub summaries: Vec<SessionRow>,
    /// The changed files that are not finished yet.
    pub changing: usize,
    /// The files whose content the archive holds.
    pub archived: usize,
}

/// What waits. `now` is the time that the quiet hours count from.
pub fn plan(ctx: &Ctx, ledger: &Ledger, now: SystemTime) -> Result<Plan> {
    let config = &ctx.config()?.sessions;
    let projects = config.projects_path()?;
    let quiet = i128::from(config.quiet_hours) * 3_600_000_000_000;
    let now = now
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| i128::try_from(d.as_nanos()).unwrap_or(i128::MAX));
    let mut plan = Plan::default();
    for file in scan(&projects) {
        let row = ledger.session_row(&file.rel)?;
        let same = row.as_ref().is_some_and(|r| {
            r.archived.is_some()
                && r.size == i64::try_from(file.size).ok()
                && r.mtime == Some(file.mtime)
        });
        if same {
            plan.archived += 1;
            continue;
        }
        if now - i128::from(file.mtime) < quiet {
            plan.changing += 1;
            continue;
        }
        plan.copies.push(file);
    }
    for row in ledger.session_rows()? {
        if row.summary_waits() && !plan.copies.iter().any(|c| c.rel == row.path) {
            plan.summaries.push(row);
        }
    }
    Ok(plan)
}

/// The most files and bytes of one run, or none.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Limit {
    pub files: usize,
    pub bytes: u64,
}

/// What a run did.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct Report {
    /// The files that went to the archive.
    pub copied: Vec<String>,
    /// The gzip bytes that went to the archive.
    pub bytes: u64,
    /// The files whose change time changed but whose content the archive already holds.
    pub same: usize,
    /// The files that changed during the copy: a later run copies them.
    pub moved: usize,
    /// The summaries that were written or found the same.
    pub summaries: usize,
    /// The files that wait for a later run because of a limit.
    pub later: usize,
    pub failed: Vec<String>,
}

/// A copy of a session file in a temporary folder, and the hash of its content.
struct Snapshot {
    file: PathBuf,
    hash: String,
    size: u64,
}

/// Copies a session file to the temporary folder. `None` when the file changed during the
/// copy. The session file itself is only read.
fn snapshot(
    source: &Path,
    size: u64,
    mtime: i64,
    temp: &Path,
    n: usize,
) -> Result<Option<Snapshot>> {
    let file = temp.join(format!("{n}.jsonl"));
    let copied = std::fs::copy(source, &file)
        .with_context(|| format!("could not read {}", source.display()))?;
    let after = std::fs::metadata(source)
        .with_context(|| format!("could not read {}", source.display()))?;
    if copied != size || after.len() != size || mtime_nanos(&after) != mtime {
        let _ = std::fs::remove_file(&file);
        return Ok(None);
    }
    let hash =
        ids::sha256_file(&file).with_context(|| format!("could not read {}", file.display()))?;
    Ok(Some(Snapshot {
        file,
        hash,
        size: copied,
    }))
}

/// Runs the `gzip` program on a file: `-n` leaves out the name and the time, so that the same
/// content gives the same bytes. Gives the `.gz` file; the file itself goes.
fn gzip(file: &Path) -> Result<PathBuf> {
    let mut command = Command::new("gzip");
    command.arg("-n").arg("-f").arg(file);
    let output = proc::run(command, None, Some(GZIP_TIMEOUT), How::default())
        .context("could not run gzip")?;
    if !output.success() {
        bail!("gzip failed: {}", cut(output.stderr_text().trim(), 300));
    }
    let mut name = file.as_os_str().to_owned();
    name.push(".gz");
    Ok(PathBuf::from(name))
}

/// Unpacks a `.gz` file next to it. Gives the file; the `.gz` file goes.
fn gunzip(file: &Path) -> Result<PathBuf> {
    let mut command = Command::new("gzip");
    command.arg("-d").arg("-f").arg(file);
    let output = proc::run(command, None, Some(GZIP_TIMEOUT), How::default())
        .context("could not run gzip")?;
    if !output.success() {
        bail!(
            "gzip could not unpack {}: {}",
            file.display(),
            cut(output.stderr_text().trim(), 300)
        );
    }
    Ok(file.with_extension(""))
}

/// The summary text of a session file, masked and cut: the secrets of `[secrets]` and the
/// forms of tokens that the gates of the executor detect.
fn summary_text(
    inputs: &Inputs<'_>,
    file: &Path,
    project: &str,
    session: &str,
    key: &str,
) -> Result<String> {
    // The secrets come first: with no masking, no summary is read or written.
    let secrets = inputs.secrets()?;
    let summary = summary::read_file(file, session, project, key)?;
    let masked =
        crate::executor::gates::mask_token_forms(&secrets.mask(&summary::render(&summary)));
    let mut text = cut(&masked, summary::MAX_ENTRY_CHARS);
    if !text.ends_with('\n') {
        text.push('\n');
    }
    Ok(text)
}

/// Writes a summary entry when its text changed, and records it. Gives true for a new text.
fn write_summary(
    ctx: &Ctx,
    ledger: &Ledger,
    rel: &str,
    project: &str,
    session: &str,
    text: &str,
    of_hash: &str,
) -> Result<bool> {
    let entry = entry_rel(project, session);
    let path = ctx.paths.home().join(&entry);
    let changed = read_optional(&path)?.as_deref() != Some(text);
    if changed {
        if let Some(dir) = path.parent() {
            ensure_dir(dir)?;
        }
        atomic_write(&path, text.as_bytes())?;
    }
    ledger.set_session_summary(rel, &entry, &ids::sha256_hex(text.as_bytes()), of_hash)?;
    Ok(changed)
}

/// Copies the files of the plan to the archive and writes the summaries that wait, within the
/// limit. A summary needs the secrets: when they cannot be read, the copies go on and the
/// summaries wait for a later run.
pub fn archive(
    ctx: &Ctx,
    ledger: &Ledger,
    store: &Store,
    plan: &Plan,
    limit: Option<Limit>,
) -> Result<Report> {
    let config = ctx.config()?;
    let inputs = Inputs::new(ctx);
    let temp = TempDir::new("vision-sessions").context("could not make a temporary folder")?;
    let mut report = Report::default();
    let mut sent_bytes = 0u64;
    let mut summary_failed = false;
    for (n, file) in plan.copies.iter().enumerate() {
        if let Some(limit) = limit {
            if n >= limit.files || sent_bytes >= limit.bytes {
                report.later = plan.copies.len() - n;
                break;
            }
        }
        sent_bytes += file.size;
        match copy_one(
            ctx,
            ledger,
            store,
            &config.archive,
            &inputs,
            file,
            &temp.path,
            n,
        ) {
            Ok(Copied { sent, summary }) => {
                match sent {
                    Some(bytes) => {
                        report.copied.push(file.rel.clone());
                        report.bytes += bytes;
                    }
                    None => report.same += 1,
                }
                match summary {
                    Some(Ok(())) => report.summaries += 1,
                    Some(Err(e)) => {
                        if !summary_failed {
                            report
                                .failed
                                .push(format!("the summary of {}: {e:#}", file.rel));
                        }
                        summary_failed |= inputs.failed();
                    }
                    None => {}
                }
            }
            Err(CopyError::Moved) => report.moved += 1,
            Err(CopyError::Failed(e)) => report.failed.push(format!("{}: {e:#}", file.rel)),
        }
    }
    if !inputs.failed() {
        for (n, row) in plan.summaries.iter().enumerate() {
            match summary_of_row(
                ctx,
                ledger,
                store,
                &inputs,
                row,
                &temp.path,
                plan.copies.len() + n,
            ) {
                Ok(true) => report.summaries += 1,
                Ok(false) => {}
                Err(e) => {
                    report
                        .failed
                        .push(format!("the summary of {}: {e:#}", row.path));
                    if inputs.failed() {
                        break;
                    }
                }
            }
        }
    }
    Ok(report)
}

struct Copied {
    /// The gzip bytes sent, or `None` when the archive already held the content.
    sent: Option<u64>,
    /// The result of the summary of a main file, or `None` when it needed none.
    summary: Option<Result<()>>,
}

enum CopyError {
    /// The file changed during the copy.
    Moved,
    Failed(anyhow::Error),
}

impl From<anyhow::Error> for CopyError {
    fn from(e: anyhow::Error) -> CopyError {
        CopyError::Failed(e)
    }
}

#[allow(clippy::too_many_arguments)]
fn copy_one(
    ctx: &Ctx,
    ledger: &Ledger,
    store: &Store,
    config: &Archive,
    inputs: &Inputs<'_>,
    file: &SessionFile,
    temp: &Path,
    n: usize,
) -> Result<Copied, CopyError> {
    let row = ledger.session_row(&file.rel)?;
    let Some(snap) = snapshot(&file.path, file.size, file.mtime, temp, n)? else {
        return Err(CopyError::Moved);
    };
    let key = session_key(config, &file.rel);
    // The summary reads the copy before gzip replaces it, and it goes to the journal only after
    // the archive confirmed the copy.
    let wants_summary = file.main
        && !inputs.failed()
        && row
            .as_ref()
            .is_none_or(|r| r.summary_of.as_deref() != Some(snap.hash.as_str()));
    let text =
        wants_summary.then(|| summary_text(inputs, &snap.file, &file.project, &file.session, &key));
    let held = row
        .as_ref()
        .is_some_and(|r| r.archived.is_some() && r.hash.as_deref() == Some(snap.hash.as_str()));
    let size = i64::try_from(snap.size).unwrap_or(i64::MAX);
    let sent = if held {
        ledger.set_session_mtime(&file.rel, file.mtime)?;
        let _ = std::fs::remove_file(&snap.file);
        None
    } else {
        let gz = gzip(&snap.file)?;
        let bytes = store.upload_checked(&gz, &key)?;
        let _ = std::fs::remove_file(&gz);
        ledger.record_session_archived(&SessionArchived {
            path: &file.rel,
            project: &file.project,
            session: &file.session,
            main: file.main,
            hash: &snap.hash,
            size,
            mtime: file.mtime,
            key: &key,
            archived: &time::now_rfc3339(),
            archive_bytes: i64::try_from(bytes).unwrap_or(i64::MAX),
        })?;
        Some(bytes)
    };
    let summary = text.map(|text| {
        text.and_then(|text| {
            write_summary(
                ctx,
                ledger,
                &file.rel,
                &file.project,
                &file.session,
                &text,
                &snap.hash,
            )
            .map(|_| ())
        })
    });
    Ok(Copied { sent, summary })
}

/// Writes the summary of an archived main file whose summary waits: from the file when it
/// still has the archived content, else from the archive. Gives false when the file changed
/// since its archive: a later run copies it and writes its summary then.
fn summary_of_row(
    ctx: &Ctx,
    ledger: &Ledger,
    store: &Store,
    inputs: &Inputs<'_>,
    row: &SessionRow,
    temp: &Path,
    n: usize,
) -> Result<bool> {
    let (Some(hash), Some(key)) = (&row.hash, &row.key) else {
        return Ok(false);
    };
    let projects = ctx.config()?.sessions.projects_path()?;
    let source = projects.join(&row.path);
    let local = std::fs::metadata(&source).ok().filter(|m| {
        Some(m.len()) == row.size.and_then(|s| u64::try_from(s).ok())
            && Some(mtime_nanos(m)) == row.mtime
    });
    let file = match local {
        Some(meta) => match snapshot(&source, meta.len(), mtime_nanos(&meta), temp, n)? {
            Some(snap) => snap,
            None => return Ok(false),
        },
        None => {
            let gz = temp.join(format!("{n}.jsonl.gz"));
            store.download(key, &gz)?;
            let file = gunzip(&gz)?;
            let hash = ids::sha256_file(&file)?;
            let size = std::fs::metadata(&file)?.len();
            Snapshot { file, hash, size }
        }
    };
    if &file.hash != hash {
        let _ = std::fs::remove_file(&file.file);
        return Ok(false);
    }
    let text = summary_text(inputs, &file.file, &row.project, &row.session, key);
    let _ = std::fs::remove_file(&file.file);
    write_summary(
        ctx,
        ledger,
        &row.path,
        &row.project,
        &row.session,
        &text?,
        hash,
    )?;
    Ok(true)
}

/// Files each summary that waits for the index, at most `limit` of them, in the wing of
/// `sessions.wing` with the project folder as the room and the path of the entry as the
/// source. With the index off, it files nothing.
pub fn file_waiting(ctx: &Ctx, ledger: &Ledger, limit: Option<usize>) -> Result<Filed> {
    let config = ctx.config()?;
    let waiting = ledger.session_rows_to_index()?;
    let mut report = Filed {
        waiting: waiting.len(),
        ..Filed::default()
    };
    if !config.journal.index_on() || waiting.is_empty() {
        return Ok(report);
    }
    let mut client = match mcp::connect_config(&config.journal) {
        Ok(client) => client,
        Err(e) => {
            report.error = Some(format!("{e:#}"));
            return Ok(report);
        }
    };
    for row in waiting.iter().take(limit.unwrap_or(usize::MAX)) {
        let Some(entry) = &row.entry else {
            continue;
        };
        let path = ctx.paths.home().join(entry);
        let text = match std::fs::read_to_string(&path) {
            Ok(text) => text,
            Err(e) => {
                report.failed.push(format!(
                    "{}: could not read {}: {e}",
                    row.session,
                    path.display()
                ));
                continue;
            }
        };
        match index::file_one(
            &mut client,
            &config.sessions.wing,
            &room_of(&row.project),
            &path.display().to_string(),
            &text,
        ) {
            Ok(()) => {
                ledger.set_session_filed(
                    &row.path,
                    &ids::sha256_hex(text.as_bytes()),
                    &time::now_rfc3339(),
                )?;
                report.filed += 1;
                report.waiting -= 1;
            }
            Err(e) if mcp::is_unreachable(&e) => {
                report.error = Some(format!("{e:#}"));
                break;
            }
            Err(e) => report.failed.push(format!("{}: {e:#}", row.session)),
        }
    }
    for failure in &report.failed {
        ctx.log(&format!("sessions: the index refused a summary: {failure}"));
    }
    Ok(report)
}

/// The run of the tick, inside the hourly run of the archive: with the archive of the
/// sessions on, it copies at most [`TICK_FILES`] files or [`TICK_BYTES`] bytes.
pub fn tick(ctx: &Ctx, ledger: &Ledger) -> Result<()> {
    let config = ctx.config()?;
    if !config.sessions.enabled {
        return Ok(());
    }
    let plan = plan(ctx, ledger, SystemTime::now())?;
    if plan.copies.is_empty() && plan.summaries.is_empty() {
        return Ok(());
    }
    let store = Store::new(&config.archive)?;
    let report = archive(
        ctx,
        ledger,
        &store,
        &plan,
        Some(Limit {
            files: TICK_FILES,
            bytes: TICK_BYTES,
        }),
    )?;
    record_run(ledger, "tick", &report)?;
    ctx.log(&format!(
        "sessions: {} file(s) went to the archive, {} summar(y/ies) written, {} wait for a later run{}",
        report.copied.len(),
        report.summaries,
        report.later + report.moved,
        if report.failed.is_empty() {
            String::new()
        } else {
            format!("; failed: {}", report.failed.join("; "))
        }
    ));
    Ok(())
}

/// One event for each run, with its counts.
fn record_run(ledger: &Ledger, by: &str, report: &Report) -> Result<()> {
    ledger.add_event(&Event::now(by, RUN_EVENT).detail(json!({
        "by": by, "copied": report.copied.len(), "bytes": report.bytes,
        "summaries": report.summaries, "failed": report.failed.len(),
    })))?;
    Ok(())
}

/// The files of a session in the ledger, for an exact id or a unique start of at least 8
/// characters.
fn session_rows_for(ledger: &Ledger, id: &str) -> Result<Vec<SessionRow>> {
    let exact = ledger.session_files(id)?;
    if !exact.is_empty() || id.chars().count() < 8 {
        return Ok(exact);
    }
    let mut sessions: Vec<String> = ledger
        .session_rows()?
        .into_iter()
        .filter(|r| r.session.starts_with(id))
        .map(|r| r.session)
        .collect();
    sessions.sort();
    sessions.dedup();
    match sessions.as_slice() {
        [one] => ledger.session_files(one),
        [] => Ok(Vec::new()),
        many => bail!(
            "`{id}` starts {} sessions ({}). Give more of the id",
            many.len(),
            many.join(", ")
        ),
    }
}

/// The real path of a path that does not exist yet: the real path of its nearest folder that
/// exists, with the rest of the path after it.
fn real_path(path: &Path) -> Result<PathBuf> {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()?.join(path)
    };
    let mut rest = Vec::new();
    let mut at = absolute.as_path();
    loop {
        if let Ok(real) = std::fs::canonicalize(at) {
            let mut out = real;
            for part in rest.iter().rev() {
                out.push(part);
            }
            return Ok(out);
        }
        let Some(name) = at.file_name() else {
            bail!("could not find the folder {}", absolute.display());
        };
        if name == ".." {
            bail!(
                "the folder {} has `..` in a part that does not exist",
                absolute.display()
            );
        }
        rest.push(name.to_os_string());
        at = at.parent().context("a path has a parent")?;
    }
}

/// The folders that a fetch never writes into: `sessions.projects` and `~/.claude/projects`.
fn guarded_folders(ctx: &Ctx) -> Result<Vec<PathBuf>> {
    let mut out = vec![real_path(&ctx.config()?.sessions.projects_path()?)?];
    if let Ok(home) = crate::paths::user_home() {
        out.push(real_path(&home.join(".claude").join("projects"))?);
    }
    Ok(out)
}

/// Brings the archived files of a session back into `to`: `<session>.jsonl`, and the files of
/// its subagents under `<session>/`. Never into the folder of the projects, and never over a
/// file that exists. Gives the files.
pub fn fetch(ctx: &Ctx, ledger: &Ledger, id: &str, to: &Path) -> Result<Vec<PathBuf>> {
    let config = ctx.config()?;
    let id = id.trim();
    let mut rows = session_rows_for(ledger, id)?;
    rows.retain(|r| r.archived.is_some() && r.key.is_some());
    if rows.is_empty() {
        // The ledger of another machine: the summary names the key of the main file.
        let found = summary_entries(&ctx.paths.session_journal_dir())
            .into_iter()
            .filter_map(|p| summary::read_entry(&p))
            .find(|e| e.session == id);
        match found.and_then(|e| e.archive.map(|key| (e.project, key))) {
            Some((project, key)) => rows.push(SessionRow {
                path: format!("{project}/{id}.jsonl"),
                project,
                session: id.to_string(),
                main: true,
                key: Some(key),
                ..SessionRow::default()
            }),
            None => bail!(
                "the archive has no session {id}. Find one with `vision sessions search TEXT`"
            ),
        }
    }
    let target = real_path(to)?;
    for guarded in guarded_folders(ctx)? {
        if target.starts_with(&guarded) {
            bail!(
                "{} is in the folder of Claude Code ({}), and a fetch never writes there. Give another folder with --to",
                target.display(),
                guarded.display()
            );
        }
    }
    // Each file lands at its path under the project folder: `<session>.jsonl` or
    // `<session>/subagents/...`.
    let mut plan = Vec::new();
    for row in &rows {
        let inner = row
            .path
            .split_once('/')
            .map(|(_, rest)| rest.to_string())
            .context("a session path has a project folder")?;
        if inner.split('/').any(|part| !safe_part(part)) {
            bail!("the archived path {} is not safe", row.path);
        }
        let out = target.join(&inner);
        if out.exists() {
            bail!("{} is already here, so nothing was fetched", out.display());
        }
        plan.push((row.key.clone().unwrap_or_default(), out));
    }
    let store = Store::new(&config.archive)?;
    let temp = TempDir::new("vision-fetch").context("could not make a temporary folder")?;
    let mut written = Vec::new();
    for (n, (key, out)) in plan.into_iter().enumerate() {
        let gz = temp.path.join(format!("{n}.jsonl.gz"));
        store.download(&key, &gz)?;
        let file = gunzip(&gz)?;
        if let Some(dir) = out.parent() {
            ensure_dir(dir)?;
        }
        crate::util::fs::atomic_copy(&file, &out)?;
        let _ = std::fs::remove_file(&file);
        written.push(out);
    }
    Ok(written)
}

/// Each summary entry file under `session-journal/` (the folder `dir`).
pub fn summary_entries(dir: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    let Ok(projects) = std::fs::read_dir(dir) else {
        return files;
    };
    for project in projects.flatten() {
        let Ok(entries) = std::fs::read_dir(project.path()) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let name = entry.file_name().to_string_lossy().into_owned();
            if path.is_file() && name.ends_with(".md") && !name.starts_with('.') {
                files.push(path);
            }
        }
    }
    files.sort();
    files
}

/// The facts of the line `sessions` of `vision doctor`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Health {
    pub archived: usize,
    pub sessions: usize,
    pub waiting: usize,
    pub changing: usize,
    pub archive_bytes: u64,
    pub summaries: usize,
    pub to_index: usize,
}

/// Counts what the archive holds and what waits, with no write.
pub fn health(ctx: &Ctx, ledger: &Ledger) -> Result<Health> {
    let plan = plan(ctx, ledger, SystemTime::now())?;
    let rows = ledger.session_rows()?;
    let mut sessions: Vec<&str> = rows
        .iter()
        .filter(|r| r.archived.is_some())
        .map(|r| r.session.as_str())
        .collect();
    sessions.sort_unstable();
    sessions.dedup();
    Ok(Health {
        archived: rows.iter().filter(|r| r.archived.is_some()).count(),
        sessions: sessions.len(),
        waiting: plan.copies.len() + plan.changing,
        changing: plan.changing,
        archive_bytes: rows
            .iter()
            .filter_map(|r| r.archive_bytes)
            .map(|b| u64::try_from(b).unwrap_or(0))
            .sum(),
        summaries: rows.iter().filter(|r| r.summary_hash.is_some()).count(),
        to_index: rows.iter().filter(|r| r.waits_for_index()).count(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_room_starts_and_ends_with_a_letter_or_a_digit() {
        assert_eq!(room_of("-Users-me-work"), "Users-me-work");
        assert_eq!(room_of("-private-tmp-x--"), "private-tmp-x");
        assert_eq!(room_of("---"), "sessions");
        assert_eq!(room_of("a/b..c"), "a-b.c");
        assert_eq!(room_of(&format!("-{}", "x".repeat(300))).len(), 120);
    }

    #[test]
    fn the_key_and_the_entry_follow_the_project_folder() {
        let config = Archive::default();
        assert_eq!(
            session_key(&config, "-p/s1.jsonl"),
            "vision/sessions/-p/s1.jsonl.gz"
        );
        assert_eq!(entry_rel("-p", "s1"), "session-journal/-p/s1.md");
        assert!(safe_part("-Users-me-work"));
        assert!(!safe_part(".."));
        assert!(!safe_part(".hidden"));
        assert!(!safe_part("a\\b"));
    }

    #[test]
    fn the_scan_finds_the_main_files_and_the_files_of_the_subagents() {
        let dir = tempfile::tempdir().unwrap();
        let project = dir.path().join("-home-me-work");
        let subagents = project.join("s1").join("subagents");
        std::fs::create_dir_all(subagents.join("workflows").join("wf_1")).unwrap();
        std::fs::create_dir_all(project.join("memory")).unwrap();
        std::fs::create_dir_all(project.join("s1").join("tool-results")).unwrap();
        std::fs::write(project.join("s1.jsonl"), "{}\n").unwrap();
        std::fs::write(project.join(".hidden.jsonl"), "{}\n").unwrap();
        std::fs::write(project.join("notes.txt"), "x").unwrap();
        std::fs::write(project.join("memory").join("m.md"), "x").unwrap();
        std::fs::write(subagents.join("agent-a1.jsonl"), "{}\n").unwrap();
        std::fs::write(subagents.join("agent-a1.meta.json"), "{}").unwrap();
        std::fs::write(
            subagents
                .join("workflows")
                .join("wf_1")
                .join("agent-b2.jsonl"),
            "{}\n",
        )
        .unwrap();
        std::fs::write(project.join("s1").join("tool-results").join("t.txt"), "x").unwrap();
        let files = scan(dir.path());
        let rels: Vec<&str> = files.iter().map(|f| f.rel.as_str()).collect();
        assert_eq!(
            rels,
            [
                "-home-me-work/s1.jsonl",
                "-home-me-work/s1/subagents/agent-a1.jsonl",
                "-home-me-work/s1/subagents/workflows/wf_1/agent-b2.jsonl"
            ]
        );
        assert!(files[0].main && !files[1].main);
        assert!(files
            .iter()
            .all(|f| f.session == "s1" && f.project == "-home-me-work"));
        assert_eq!(files[0].size, 3);
    }
}
