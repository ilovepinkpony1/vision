//! The archive (section 16): a local folder, or an object store, S3 or another service with
//! the same API. For a store, Vision runs the `aws` program as a child process with the
//! environment of the tick, so the credentials stay with that program. No session ever gets
//! them. A local folder holds the same keys as paths under it, so that a change of the
//! configuration and `vision archive move-to-bucket` move the archive into a bucket later.
//!
//! - A turn folder goes up as `<prefix><KEY>/<turn-id>.tar.gz` (tar with gzip, as child
//!   processes) when its ticket ended (done or stopped), or when the turn ended more than one
//!   day ago. Each journal entry goes up as `<prefix>journal/<KEY>/<turn-id>.md`, and again
//!   after each new text.
//! - After each upload, `aws s3api head-object` (or the file in the folder) gives the size in
//!   the store. Only an equal size marks the turn archived. A copy into the folder goes to a
//!   temporary name first and then takes the name of its key.
//! - A local turn folder is deleted only when it is archived, `keep_days` days after the
//!   confirmation, and only when its ticket no longer moves, so that the pipeline never misses a
//!   folder that it reads. The journal and the ledger are never deleted.
//!
//! The tick runs the upload at most once an hour. `vision archive` uploads what waits and then
//! deletes what is due.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use ::time::OffsetDateTime;
use anyhow::{bail, Context, Result};
use serde::Serialize;
use serde_json::{json, Value};

use super::errors::cut;
use crate::adapters::jira_github::proc::{self, How, TempDir};
use crate::config::Archive;
use crate::ctx::Ctx;
use crate::ledger::journal::JournalRow;
use crate::ledger::{Event, Ledger};
use crate::paths::{Paths, TicketKey, TurnId};
use crate::util::time;

/// The event of each run of the upload. The tick reads the last one.
pub const RUN_EVENT: &str = "archive-run";
/// Seconds between two runs of the upload in the tick.
pub const EVERY_SECS: i64 = 3600;
/// Seconds after the end of a turn before its folder goes up, when its ticket still moves.
pub const AGE_SECS: i64 = 86_400;
/// The most turn folders that one run of the tick uploads. `vision archive` uploads each one.
pub const UPLOADS_PER_TICK: usize = 20;
/// The time limit of one copy.
const COPY_TIMEOUT: Duration = Duration::from_secs(900);
/// The time limit of one look at the store.
const LOOK_TIMEOUT: Duration = Duration::from_secs(60);
/// The program of the store.
pub const PROGRAM: &str = "aws";

/// The message when the archive is off.
pub const OFF: &str =
    "the archive is off. Set archive.dir (a local folder) or archive.bucket in config.toml to turn it on";

/// Where a store keeps its objects.
#[derive(Clone, Debug)]
enum Place {
    /// A bucket, through the `aws` program.
    Bucket { program: PathBuf },
    /// A local folder: each key is a path under it.
    Dir { root: PathBuf },
}

/// The store of `[archive]`: a local folder, or a bucket through the `aws` program.
#[derive(Clone, Debug)]
pub struct Store {
    config: Archive,
    place: Place,
}

impl Store {
    /// The store of the configuration. Fails when the archive is off, or when it uses a bucket
    /// and `aws` is not found.
    pub fn new(config: &Archive) -> Result<Store> {
        if !config.on() {
            bail!("{OFF}");
        }
        if let Some(root) = config.dir_path()? {
            return Ok(Store {
                config: config.clone(),
                place: Place::Dir { root },
            });
        }
        Store::bucket(config)
    }

    /// The store of the bucket of the configuration, also for `vision archive move-to-bucket`.
    pub fn bucket(config: &Archive) -> Result<Store> {
        if !config.bucket_on() {
            bail!("archive.bucket is empty. Name the bucket in config.toml first");
        }
        let Some(program) = crate::util::process::find_program(PROGRAM) else {
            bail!("the aws program is not found in PATH. Install the AWS CLI, or turn the archive off with archive.bucket = \"\"");
        };
        Ok(Store {
            config: config.clone(),
            place: Place::Bucket { program },
        })
    }

    /// The options of each call: the endpoint, the profile, and the region, when they are set.
    fn options(&self) -> Vec<String> {
        let mut out = Vec::new();
        for (flag, value) in [
            ("--endpoint-url", &self.config.endpoint),
            ("--profile", &self.config.profile),
            ("--region", &self.config.region),
        ] {
            if !value.trim().is_empty() {
                out.push(flag.to_string());
                out.push(value.trim().to_string());
            }
        }
        out
    }

    fn bucket_name(&self) -> &str {
        self.config.bucket.trim()
    }

    /// The local folder of the store, or `None` for a bucket.
    pub fn dir(&self) -> Option<&Path> {
        match &self.place {
            Place::Dir { root } => Some(root),
            Place::Bucket { .. } => None,
        }
    }

    /// The address of a key, such as `s3://bucket/vision/DEMO-12/T1.tar.gz`, or its path in
    /// the local folder.
    pub fn url(&self, key: &str) -> String {
        match &self.place {
            Place::Bucket { .. } => format!("s3://{}/{key}", self.bucket_name()),
            Place::Dir { root } => root.join(key).display().to_string(),
        }
    }

    /// The path of a key in the local folder. A key part that is empty, `.`, or `..` is refused,
    /// so that no key leaves the folder.
    fn local_path(root: &Path, key: &str) -> Result<PathBuf> {
        let mut path = root.to_path_buf();
        for part in key.split('/') {
            if part.is_empty() || part == "." || part == ".." || part.contains(['\0', '\\']) {
                bail!("the key `{key}` is not a safe path in the archive folder");
            }
            path.push(part);
        }
        Ok(path)
    }

    fn program(&self) -> Result<&Path> {
        match &self.place {
            Place::Bucket { program } => Ok(program),
            Place::Dir { .. } => bail!("the archive is a local folder, so it runs no aws program"),
        }
    }

    fn call(&self, args: Vec<String>, timeout: Duration) -> Result<proc::Output> {
        let program = self.program()?;
        let mut command = Command::new(program);
        command.args(&args).args(self.options());
        let output = proc::run(
            command,
            None,
            Some(timeout),
            How {
                session: true,
                merge: false,
            },
        )
        .with_context(|| format!("could not run {}", program.display()))?;
        if output.timed_out {
            bail!(
                "aws {} took more than {} s",
                args.first().map(String::as_str).unwrap_or(""),
                timeout.as_secs()
            );
        }
        Ok(output)
    }

    fn checked(&self, args: Vec<String>, timeout: Duration) -> Result<proc::Output> {
        let what = args.iter().take(2).cloned().collect::<Vec<_>>().join(" ");
        let output = self.call(args, timeout)?;
        if !output.success() {
            bail!(
                "aws {what} failed with the exit code {}: {}",
                output.code.unwrap_or(-1),
                cut(output.stderr_text().trim(), 300)
            );
        }
        Ok(output)
    }

    /// Copies a local file to a key. In a local folder, the copy goes to a temporary name
    /// first and then takes the name of the key.
    pub fn upload(&self, file: &Path, key: &str) -> Result<()> {
        if let Place::Dir { root } = &self.place {
            let path = Store::local_path(root, key)?;
            if let Some(parent) = path.parent() {
                crate::util::fs::ensure_dir(parent)?;
            }
            return crate::util::fs::atomic_copy(file, &path).map(|_| ());
        }
        self.checked(
            vec![
                "s3".into(),
                "cp".into(),
                file.display().to_string(),
                self.url(key),
                "--only-show-errors".into(),
            ],
            COPY_TIMEOUT,
        )
        .map(|_| ())
    }

    /// Copies a key to a local file.
    pub fn download(&self, key: &str, file: &Path) -> Result<()> {
        if let Place::Dir { root } = &self.place {
            let path = Store::local_path(root, key)?;
            if !path.is_file() {
                bail!("the archive folder has no {}", path.display());
            }
            std::fs::copy(&path, file).with_context(|| {
                format!("could not copy {} to {}", path.display(), file.display())
            })?;
            return Ok(());
        }
        self.checked(
            vec![
                "s3".into(),
                "cp".into(),
                self.url(key),
                file.display().to_string(),
                "--only-show-errors".into(),
            ],
            COPY_TIMEOUT,
        )
        .map(|_| ())
    }

    /// The size of a key in the store.
    pub fn size(&self, key: &str) -> Result<u64> {
        if let Place::Dir { root } = &self.place {
            let path = Store::local_path(root, key)?;
            return Ok(std::fs::metadata(&path)
                .with_context(|| format!("could not read {}", path.display()))?
                .len());
        }
        let output = self.checked(
            vec![
                "s3api".into(),
                "head-object".into(),
                "--bucket".into(),
                self.bucket_name().to_string(),
                "--key".into(),
                key.to_string(),
                "--output".into(),
                "json".into(),
            ],
            LOOK_TIMEOUT,
        )?;
        let value: Value =
            serde_json::from_slice(&output.stdout).context("aws s3api head-object gave no JSON")?;
        value
            .get("ContentLength")
            .and_then(Value::as_u64)
            .context("aws s3api head-object gave no ContentLength")
    }

    /// Copies a file and confirms its size in the store. Gives the size.
    pub fn upload_checked(&self, file: &Path, key: &str) -> Result<u64> {
        let local = std::fs::metadata(file)
            .with_context(|| format!("could not read {}", file.display()))?
            .len();
        self.upload(file, key)?;
        let stored = self.size(key)?;
        if stored != local {
            bail!(
                "the store holds {stored} bytes of {}, but the local file has {local}",
                self.url(key)
            );
        }
        Ok(local)
    }

    /// One look at the prefix, for `vision doctor`. An empty prefix answers too. For a local
    /// folder: its path and its size, with no write.
    pub fn probe(&self) -> Result<String> {
        if let Place::Dir { root } = &self.place {
            return probe_dir(root);
        }
        let prefix = self.url(&self.config.prefix);
        let output = self.call(
            vec![
                "s3".into(),
                "ls".into(),
                prefix.clone(),
                "--max-items".into(),
                "1".into(),
            ],
            LOOK_TIMEOUT,
        )?;
        // `aws s3 ls` ends with code 1 and says nothing when the prefix holds no object yet.
        let stderr = output.stderr_text();
        match output.code {
            Some(0) => Ok(format!("{prefix} answers")),
            Some(1) if stderr.trim().is_empty() => {
                Ok(format!("{prefix} answers, and it holds nothing yet"))
            }
            code => bail!(
                "aws s3 ls {prefix} failed with the exit code {}: {}",
                code.unwrap_or(-1),
                cut(stderr.trim(), 300)
            ),
        }
    }
}

/// Where the archive of the configuration is, for a person: `s3://bucket/prefix`, or the
/// folder.
pub fn place(config: &Archive) -> String {
    match config.dir_path() {
        Ok(Some(root)) => format!("the folder {}", root.join(&config.prefix).display()),
        Ok(None) => format!("s3://{}/{}", config.bucket.trim(), config.prefix),
        Err(e) => format!("archive.dir ({e:#})"),
    }
}

/// The files and the bytes under a folder, with no link followed. A missing folder holds
/// nothing.
pub fn folder_size(root: &Path) -> (u64, u64) {
    let (mut files, mut bytes) = (0, 0);
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let Ok(meta) = entry.path().symlink_metadata() else {
                continue;
            };
            if meta.is_dir() {
                stack.push(entry.path());
            } else if meta.is_file() {
                files += 1;
                bytes += meta.len();
            }
        }
    }
    (files, bytes)
}

/// The line of `vision doctor` for a local folder: its path and its size. A folder that does
/// not exist yet is fine: the first run makes it.
fn probe_dir(root: &Path) -> Result<String> {
    if !root.exists() {
        return Ok(format!(
            "the folder {} does not exist yet; the first run makes it",
            root.display()
        ));
    }
    if !root.is_dir() {
        bail!(
            "archive.dir names {}, which is not a folder",
            root.display()
        );
    }
    let c_path = std::ffi::CString::new(root.as_os_str().as_encoded_bytes())
        .context("the path of the archive folder has a NUL byte")?;
    // SAFETY: access only reads the path and the permissions; it changes nothing.
    if unsafe { libc::access(c_path.as_ptr(), libc::W_OK) } != 0 {
        bail!("the folder {} is not writable", root.display());
    }
    let (files, bytes) = folder_size(root);
    Ok(format!(
        "the folder {} holds {files} file(s), {}",
        root.display(),
        crate::util::fs::human_bytes(bytes)
    ))
}

/// One file of an old archive folder: its key and its size.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct FolderFile {
    pub key: String,
    #[serde(skip)]
    pub path: PathBuf,
    pub bytes: u64,
}

/// Each file under an archive folder, by key: its path under the folder, with `/`. A name that
/// starts with `.` is left out: a copy that is not done yet, or a file of the system. A link is
/// never followed.
pub fn folder_files(root: &Path) -> Result<Vec<FolderFile>> {
    let mut out = Vec::new();
    let mut stack = vec![(root.to_path_buf(), String::new())];
    while let Some((dir, prefix)) = stack.pop() {
        let entries =
            std::fs::read_dir(&dir).with_context(|| format!("could not read {}", dir.display()))?;
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            if name.starts_with('.') {
                continue;
            }
            let Ok(meta) = entry.path().symlink_metadata() else {
                continue;
            };
            let key = format!("{prefix}{name}");
            if meta.is_dir() {
                stack.push((entry.path(), format!("{key}/")));
            } else if meta.is_file() {
                out.push(FolderFile {
                    key,
                    path: entry.path(),
                    bytes: meta.len(),
                });
            }
        }
    }
    out.sort_by(|a, b| a.key.cmp(&b.key));
    Ok(out)
}

/// What a move of a folder into the bucket did.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct Moved {
    /// The files that the bucket now holds and that left the folder.
    pub moved: usize,
    pub bytes: u64,
    pub failed: Vec<String>,
}

/// Uploads each file of an old archive folder to the same key in the bucket, confirms its
/// size, and only then deletes the local file. Empty folders that stay go too, but never the
/// folder itself.
pub fn move_to_bucket(store: &Store, root: &Path, files: &[FolderFile]) -> Moved {
    let mut report = Moved::default();
    for file in files {
        match store.upload_checked(&file.path, &file.key) {
            Ok(bytes) => match std::fs::remove_file(&file.path) {
                Ok(()) => {
                    report.moved += 1;
                    report.bytes += bytes;
                }
                Err(e) => report.failed.push(format!(
                    "{}: the bucket holds it, but the local file could not be deleted: {e}",
                    file.key
                )),
            },
            Err(e) => report.failed.push(format!("{}: {e:#}", file.key)),
        }
    }
    remove_empty_dirs(root, root);
    report
}

/// Removes each empty folder under `dir`, deepest first, but never `root`.
fn remove_empty_dirs(root: &Path, dir: &Path) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.symlink_metadata().is_ok_and(|m| m.is_dir()) {
            remove_empty_dirs(root, &path);
        }
    }
    if dir != root {
        // Only an empty folder goes: remove_dir refuses any other.
        let _ = std::fs::remove_dir(dir);
    }
}

/// The key of the archive of a turn folder.
pub fn folder_key(config: &Archive, ticket: &str, turn: &str) -> String {
    format!("{}{ticket}/{turn}.tar.gz", config.prefix)
}

/// The key of a journal entry.
pub fn entry_key(config: &Archive, ticket: &str, turn: &str) -> String {
    format!("{}journal/{ticket}/{turn}.md", config.prefix)
}

/// One local turn folder.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Folder {
    pub ticket: String,
    pub turn: String,
    pub dir: PathBuf,
}

/// What a run of the archive does.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct Plan {
    /// The turn folders that go up.
    pub uploads: Vec<Folder>,
    /// The journal entries that go up: (ticket, turn, file).
    pub entries: Vec<(String, String, PathBuf)>,
    /// The turn folders that are due for deletion.
    pub deletions: Vec<Folder>,
    /// The turn folders that the archive holds and that stay for now.
    pub kept: usize,
}

/// Each local turn folder, oldest first.
pub fn turn_folders(paths: &Paths) -> Vec<Folder> {
    let mut out = Vec::new();
    let Ok(tickets) = std::fs::read_dir(paths.tickets_dir()) else {
        return out;
    };
    for ticket in tickets.flatten() {
        let name = ticket.file_name().to_string_lossy().into_owned();
        let Ok(key) = TicketKey::parse(&name) else {
            continue;
        };
        let Ok(turns) = std::fs::read_dir(paths.turns_dir(&key)) else {
            continue;
        };
        for turn in turns.flatten() {
            let id = turn.file_name().to_string_lossy().into_owned();
            if turn.path().is_dir() && TurnId::parse(&id).is_ok() {
                out.push(Folder {
                    ticket: key.to_string(),
                    turn: id,
                    dir: turn.path(),
                });
            }
        }
    }
    out.sort_by(|a, b| a.turn.cmp(&b.turn));
    out
}

/// True when the ticket no longer moves: it reached its end, or the person stopped it. A
/// reserved key has no ticket that moves.
fn ticket_rests(ledger: &Ledger, key: &str) -> Result<bool> {
    if TicketKey::parse(key).is_ok_and(|k| k.is_reserved()) {
        return Ok(true);
    }
    let Some(row) = ledger.ticket(key)? else {
        return Ok(true);
    };
    let ticket = crate::pipeline::state::flags_of(&row);
    let finished = row
        .counts
        .as_deref()
        .and_then(|c| serde_json::from_str::<Value>(c).ok())
        .is_some_and(|c| c.get("finished").is_some_and(|f| !f.is_null()));
    Ok(finished || ticket.iter().any(|f| f == "stopped"))
}

/// Seconds since an RFC 3339 time, or since the change of a folder when the time is unknown.
fn age(at: Option<&str>, dir: &Path, now: OffsetDateTime) -> i64 {
    if let Some(at) = at.and_then(|a| time::parse_rfc3339(a).ok()) {
        return time::seconds_between(at, now);
    }
    std::fs::metadata(dir)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|m| m.elapsed().ok())
        .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX))
}

/// What waits for the archive and what is due for deletion.
pub fn plan(ctx: &Ctx, ledger: &Ledger, now: OffsetDateTime) -> Result<Plan> {
    let config = &ctx.config()?.archive;
    let mut plan = Plan::default();
    for folder in turn_folders(&ctx.paths) {
        let row = ledger.turn(&folder.turn)?;
        if row
            .as_ref()
            .is_some_and(|r| r.status.as_deref() == Some(crate::ledger::turns::STATUS_RUNNING))
        {
            continue;
        }
        let journal = ledger.journal_row(&folder.turn)?;
        let rests = ticket_rests(ledger, &folder.ticket)?;
        match journal.as_ref().and_then(|j| j.archived.clone()) {
            None => {
                let ended = row.as_ref().and_then(|r| r.ended.clone());
                if rests || age(ended.as_deref(), &folder.dir, now) >= AGE_SECS {
                    plan.uploads.push(folder);
                }
            }
            Some(archived) => {
                let days = i64::from(config.keep_days);
                let due = age(Some(&archived), &folder.dir, now) >= days * 86_400;
                if due && rests {
                    plan.deletions.push(folder);
                } else {
                    plan.kept += 1;
                }
            }
        }
    }
    for row in ledger.journal_rows()? {
        if !row.entry_waits_for_archive() {
            continue;
        }
        if let Some(relative) = &row.path {
            let file = ctx.paths.home().join(relative);
            if file.is_file() {
                plan.entries
                    .push((row.ticket.clone(), row.turn.clone(), file));
            }
        }
    }
    Ok(plan)
}

/// What a run did.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct Report {
    pub uploaded: Vec<String>,
    pub entries: usize,
    pub deleted: Vec<String>,
    pub failed: Vec<String>,
}

/// Uploads the turn folders and the entries of the plan, at most `limit` folders.
pub fn upload(
    ctx: &Ctx,
    ledger: &Ledger,
    store: &Store,
    plan: &Plan,
    limit: Option<usize>,
) -> Result<Report> {
    let config = &ctx.config()?.archive;
    let mut report = Report::default();
    let temp = TempDir::new("vision-archive").context("could not make a temporary folder")?;
    for folder in plan.uploads.iter().take(limit.unwrap_or(usize::MAX)) {
        match upload_folder(store, config, &temp.path, folder) {
            Ok(bytes) => {
                ledger.set_turn_archived(
                    &folder.turn,
                    &folder.ticket,
                    i64::try_from(bytes).unwrap_or(i64::MAX),
                    &time::now_rfc3339(),
                )?;
                report.uploaded.push(folder.turn.clone());
            }
            Err(e) => report.failed.push(format!("{}: {e:#}", folder.turn)),
        }
    }
    for (ticket, turn, file) in &plan.entries {
        let text = match std::fs::read(file) {
            Ok(text) => text,
            Err(e) => {
                report.failed.push(format!("the entry of {turn}: {e}"));
                continue;
            }
        };
        match store.upload_checked(file, &entry_key(config, ticket, turn)) {
            Ok(_) => {
                ledger.set_entry_sent(turn, &crate::util::ids::sha256_hex(&text))?;
                report.entries += 1;
            }
            Err(e) => report.failed.push(format!("the entry of {turn}: {e:#}")),
        }
    }
    Ok(report)
}

/// Makes the tar of one turn folder and uploads it. Gives the confirmed size.
fn upload_folder(store: &Store, config: &Archive, temp: &Path, folder: &Folder) -> Result<u64> {
    let parent = folder
        .dir
        .parent()
        .context("a turn folder has a parent folder")?;
    let file = temp.join(format!("{}.tar.gz", folder.turn));
    let mut command = Command::new("tar");
    command
        .env("COPYFILE_DISABLE", "1")
        .arg("-czf")
        .arg(&file)
        .arg("-C")
        .arg(parent)
        .arg(&folder.turn);
    let output = proc::run(command, None, Some(COPY_TIMEOUT), How::default())
        .context("could not run tar")?;
    if !output.success() {
        bail!("tar failed: {}", cut(output.stderr_text().trim(), 300));
    }
    let bytes = store.upload_checked(&file, &folder_key(config, &folder.ticket, &folder.turn))?;
    let _ = std::fs::remove_file(&file);
    Ok(bytes)
}

/// Deletes each turn folder of the plan that is due. The row keeps the time.
pub fn delete_due(ledger: &Ledger, plan: &Plan, report: &mut Report) -> Result<()> {
    for folder in &plan.deletions {
        let archived = ledger
            .journal_row(&folder.turn)?
            .and_then(|r: JournalRow| r.archived);
        if archived.is_none() {
            continue;
        }
        match std::fs::remove_dir_all(&folder.dir) {
            Ok(()) => {
                ledger.set_turn_removed(&folder.turn, Some(&time::now_rfc3339()))?;
                report.deleted.push(folder.turn.clone());
            }
            Err(e) => report.failed.push(format!(
                "{}: could not delete {}: {e}",
                folder.turn,
                folder.dir.display()
            )),
        }
    }
    Ok(())
}

/// The tick hook: with the archive on, at most once an hour, it uploads what waits, and then
/// runs the archive of the sessions of Claude Code when it is on. It deletes nothing: `vision
/// archive` does that.
pub fn tick(ctx: &Ctx, ledger: &Ledger, now: OffsetDateTime) -> Result<()> {
    let config = ctx.config()?;
    if !config.archive.on() {
        return Ok(());
    }
    if let Some(last) = ledger.last_event_at(RUN_EVENT)? {
        if time::parse_rfc3339(&last).is_ok_and(|at| time::seconds_between(at, now) < EVERY_SECS) {
            return Ok(());
        }
    }
    // The event comes first, so that a run that fails waits an hour too.
    ledger.add_event(&Event::now("tick", RUN_EVENT).detail(json!({"by": "tick"})))?;
    let turns = upload_turns(ctx, ledger, now);
    if config.sessions.enabled {
        if let Err(e) = super::sessions::tick(ctx, ledger) {
            ctx.log(&format!(
                "sessions: the archive waits for the next run: {e:#}"
            ));
        }
    }
    turns
}

/// The upload of the turn folders and the entries of one run of the tick.
fn upload_turns(ctx: &Ctx, ledger: &Ledger, now: OffsetDateTime) -> Result<()> {
    let config = &ctx.config()?.archive;
    let plan = plan(ctx, ledger, now)?;
    if plan.uploads.is_empty() && plan.entries.is_empty() {
        return Ok(());
    }
    let store = Store::new(config)?;
    let report = upload(ctx, ledger, &store, &plan, Some(UPLOADS_PER_TICK))?;
    ctx.log(&format!(
        "archive: {} turn folder(s) and {} entr(y/ies) went up{}",
        report.uploaded.len(),
        report.entries,
        if report.failed.is_empty() {
            String::new()
        } else {
            format!("; failed: {}", report.failed.join("; "))
        }
    ));
    Ok(())
}

/// Downloads the archive of a turn folder and unpacks it back to its place. Gives the folder.
pub fn fetch(ctx: &Ctx, ledger: &Ledger, turn: &TurnId) -> Result<PathBuf> {
    let config = &ctx.config()?.archive;
    let id = turn.to_string();
    let ticket = match ledger.turn(&id)? {
        Some(row) => row.ticket,
        None => match ledger.journal_row(&id)? {
            Some(row) => row.ticket,
            None => bail!("the ledger has no turn {id}"),
        },
    };
    let key = TicketKey::parse(&ticket)?;
    let dir = ctx.paths.turn(&key, turn).dir;
    if dir.exists() {
        bail!("the turn folder {} is already here", dir.display());
    }
    let store = Store::new(config)?;
    let temp = TempDir::new("vision-fetch").context("could not make a temporary folder")?;
    let file = temp.path.join(format!("{id}.tar.gz"));
    store.download(&folder_key(config, &ticket, &id), &file)?;
    // Each member must be the turn folder or a path in it.
    let mut list = Command::new("tar");
    list.arg("-tzf").arg(&file);
    let listed =
        proc::run(list, None, Some(LOOK_TIMEOUT), How::default()).context("could not run tar")?;
    if !listed.success() {
        bail!(
            "the archive of {id} is not a tar file: {}",
            cut(listed.stderr_text().trim(), 300)
        );
    }
    for member in listed.stdout_text().lines() {
        let member = member.trim_end_matches('/');
        let inside = member == id || member.starts_with(&format!("{id}/"));
        if !inside || member.split('/').any(|part| part == "..") {
            bail!("the archive of {id} holds `{member}`, which is not in its turn folder, so nothing was unpacked");
        }
    }
    let parent = ctx.paths.turns_dir(&key);
    crate::util::fs::ensure_dir(&parent)?;
    let mut unpack = Command::new("tar");
    unpack.arg("-xzf").arg(&file).arg("-C").arg(&parent);
    let output =
        proc::run(unpack, None, Some(COPY_TIMEOUT), How::default()).context("could not run tar")?;
    if !output.success() {
        bail!(
            "tar could not unpack the archive of {id}: {}",
            cut(output.stderr_text().trim(), 300)
        );
    }
    let bytes = std::fs::metadata(&file).map(|m| m.len()).unwrap_or(0);
    // The folder stays `keep_days` days again from now.
    ledger.set_turn_archived(
        &id,
        &ticket,
        i64::try_from(bytes).unwrap_or(0),
        &time::now_rfc3339(),
    )?;
    ledger.set_turn_removed(&id, None)?;
    Ok(dir)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_keys_follow_the_prefix() {
        let config = Archive {
            bucket: "b".into(),
            ..Archive::default()
        };
        assert_eq!(
            folder_key(&config, "DEMO-12", "T1"),
            "vision/DEMO-12/T1.tar.gz"
        );
        assert_eq!(
            entry_key(&config, "DEMO-12", "T1"),
            "vision/journal/DEMO-12/T1.md"
        );
        let bare = Archive {
            prefix: String::new(),
            ..config
        };
        assert_eq!(folder_key(&bare, "DEMO-12", "T1"), "DEMO-12/T1.tar.gz");
    }
}
