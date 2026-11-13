//! The journal of the turns (section 16): one Markdown entry for each turn of a ticket, the
//! index of the entries in MemPalace, the search for a work turn and for the person, and the
//! archive of the turn folders in an object store.
//!
//! The entry files are the source of truth. MemPalace is only an index over them, and it is
//! never required: when it is off or does not answer, the search reads the files, and a later
//! tick or `vision journal reindex` files what waits. The archive is off until the person names
//! a local folder or a bucket.
//!
//! - [`tick`] is the hook at the end of each tick: it writes the entries of the turns that
//!   ended, files the entries that wait for the index, and runs the upload of the archive at
//!   most once an hour.
//! - [`write_entry`] writes the entry of one turn. A second run for the same turn writes the
//!   same file.
//! - [`search`] finds entries for a text, and [`search::related_for`] gives `related.md` of a
//!   work turn.
//! - [`sessions`] archives the session files of Claude Code of the person and writes a summary
//!   of each session to `session-journal/`, which no work turn reads.

pub mod archive;
mod commands;
pub mod entry;
pub mod errors;
pub mod index;
pub mod mcp;
pub mod search;
pub mod sessions;

use std::cell::{Cell, OnceCell};
use std::path::{Path, PathBuf};

use ::time::{Duration, OffsetDateTime};
use anyhow::{Context, Result};

use crate::adapter::Adapter;
use crate::cli::{ArchiveArgs, JournalCommand};
use crate::ctx::Ctx;
use crate::executor::Secrets;
use crate::ledger::journal::EntryWrite;
use crate::ledger::turns::{TurnRow, STATUS_RUNNING};
use crate::ledger::Ledger;
use crate::paths::{TicketKey, TurnId};
use crate::tick::Pass;
use crate::util::fs::{atomic_write, ensure_dir, read_optional};
use crate::util::{git, ids, time};

/// The tick writes the entries of the turns that ended in this many days. `vision journal
/// rebuild` writes the older ones.
pub const RECENT_DAYS: i64 = 7;
/// The most entries that one tick files in MemPalace. `vision journal reindex` files the rest
/// at once.
pub const INDEX_PER_TICK: usize = 20;

/// What a write of one entry did.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Written {
    /// The turn gets no entry, with why.
    Skipped(String),
    /// The entry file already holds this text.
    Unchanged,
    /// The entry file got a new text.
    Written,
}

/// The tick hook. It never fails the tick: each problem goes to the log, and a later tick
/// tries again.
pub fn tick(pass: &mut Pass) {
    let ctx = pass.ctx;
    if let Err(e) = write_recent(ctx, &pass.ledger, pass.now) {
        ctx.log(&format!(
            "journal: the entries wait for the next tick: {e:#}"
        ));
    }
    let mut answers = true;
    match index::file_waiting(ctx, &pass.ledger, Some(INDEX_PER_TICK)) {
        Ok(report) => {
            if let Some(why) = &report.error {
                answers = false;
                // One line for each tick, however many entries wait.
                ctx.log(&format!(
                    "journal: {} entr(y/ies) wait for the index, because MemPalace does not answer: {why}",
                    report.waiting
                ));
            }
        }
        Err(e) => ctx.log(&format!(
            "journal: the index waits for the next tick: {e:#}"
        )),
    }
    if let Err(e) = archive::tick(ctx, &pass.ledger, pass.now) {
        ctx.log(&format!(
            "archive: the upload waits for the next run: {e:#}"
        ));
    }
    // The summaries of the sessions go after the archive run, which writes them. They wait
    // when MemPalace did not answer in this tick, so that a dead server costs one time limit,
    // not two.
    if answers && ctx.config().is_ok_and(|c| c.sessions.enabled) {
        match sessions::file_waiting(ctx, &pass.ledger, Some(INDEX_PER_TICK)) {
            Ok(report) => {
                if let Some(why) = &report.error {
                    ctx.log(&format!(
                        "sessions: {} summar(y/ies) wait for the index, because MemPalace does not answer: {why}",
                        report.waiting
                    ));
                }
            }
            Err(e) => ctx.log(&format!(
                "sessions: the index waits for the next tick: {e:#}"
            )),
        }
    }
}

/// The inputs that a write needs only when an entry gets a new text: the secrets, whose
/// commands can be slow, and the adapter. Each one loads at most once.
pub struct Inputs<'a> {
    ctx: &'a Ctx,
    secrets: OnceCell<Secrets>,
    adapter: OnceCell<Option<Adapter>>,
    /// The secrets could not be read.
    failed: Cell<bool>,
}

impl<'a> Inputs<'a> {
    pub fn new(ctx: &'a Ctx) -> Inputs<'a> {
        Inputs {
            ctx,
            secrets: OnceCell::new(),
            adapter: OnceCell::new(),
            failed: Cell::new(false),
        }
    }

    /// The secrets of `[secrets]`. No entry is written without their masking.
    pub fn secrets(&self) -> Result<&Secrets> {
        if let Some(secrets) = self.secrets.get() {
            return Ok(secrets);
        }
        let read = Secrets::read(self.ctx.config()?)
            .context("the secrets of [secrets] could not be read, so no entry is written");
        match read {
            Ok(read) => Ok(self.secrets.get_or_init(|| read)),
            Err(e) => {
                self.failed.set(true);
                Err(e)
            }
        }
    }

    /// True when the secrets could not be read, so that no later write can succeed.
    pub fn failed(&self) -> bool {
        self.failed.get()
    }

    /// The adapter, or `None` when it does not load: an entry then has no gate results.
    pub fn adapter(&self) -> Option<&Adapter> {
        self.adapter
            .get_or_init(|| crate::adapter::load(self.ctx).ok())
            .as_ref()
    }
}

/// Writes the entry of each turn that ended in the last [`RECENT_DAYS`] days and whose facts
/// changed. Gives the number of new texts.
pub fn write_recent(ctx: &Ctx, ledger: &Ledger, now: OffsetDateTime) -> Result<usize> {
    let since = time::rfc3339(now - Duration::days(RECENT_DAYS));
    let ids = ledger.turn_ids_ended_since(&since)?;
    let inputs = Inputs::new(ctx);
    let mut written = 0;
    for id in ids {
        let Some(row) = ledger.turn(&id)? else {
            continue;
        };
        match write_entry(ctx, ledger, &row, &inputs, false) {
            Ok(Written::Written) => written += 1,
            Ok(_) => {}
            // One line for the tick: each later entry needs the same secrets.
            Err(e) if inputs.failed() => return Err(e),
            Err(e) => ctx.log(&format!(
                "journal: could not write the entry of {id}: {e:#}"
            )),
        }
    }
    Ok(written)
}

/// Writes the entry of one turn. With `force`, the entry is built again even when the facts
/// of the ledger did not change. The text passes the masking of the secrets before it is
/// written. A turn with no ticket and a turn that runs get no entry.
pub fn write_entry(
    ctx: &Ctx,
    ledger: &Ledger,
    row: &TurnRow,
    inputs: &Inputs<'_>,
    force: bool,
) -> Result<Written> {
    let key = TicketKey::parse(&row.ticket)?;
    if key.is_reserved() {
        return Ok(Written::Skipped(format!(
            "a turn of {key} has no ticket, so it gets no entry"
        )));
    }
    if row.status.as_deref().is_none_or(|s| s == STATUS_RUNNING) {
        return Ok(Written::Skipped("the turn still runs".into()));
    }
    let id = TurnId::parse(&row.id)?;
    let path = ctx.paths.journal_entry(&key, &id);
    let existing = ledger.journal_row(&row.id)?;
    let signature = entry::signature(ledger, row)?;
    let same_signature = existing
        .as_ref()
        .is_some_and(|e| e.signature.as_deref() == Some(signature.as_str()));
    if !force && same_signature && path.is_file() {
        return Ok(Written::Unchanged);
    }
    // The archive deleted the turn folder: the entry keeps what it read from that folder.
    if path.is_file() && !ctx.paths.turn(&key, &id).dir.is_dir() {
        return Ok(Written::Unchanged);
    }
    let head = existing
        .as_ref()
        .and_then(|e| e.head.clone())
        .or_else(|| first_head(ctx, ledger, row, &key, &id));
    let built = entry::build(ctx, ledger, row, inputs.adapter(), head.clone())?;
    // The values of [secrets], then the common forms of tokens that the gates detect.
    let text =
        crate::executor::gates::mask_token_forms(&inputs.secrets()?.mask(&entry::render(&built)));
    let hash = ids::sha256_hex(text.as_bytes());
    let relative = relative_path(&key, &id);
    let now = time::now_rfc3339();
    let same_text = existing
        .as_ref()
        .is_some_and(|e| e.hash.as_deref() == Some(hash.as_str()))
        && read_optional(&path)?.as_deref() == Some(text.as_str());
    if same_text {
        ledger.set_entry_signature(&row.id, &signature)?;
        return Ok(Written::Unchanged);
    }
    if let Some(dir) = path.parent() {
        ensure_dir(dir)?;
    }
    atomic_write(&path, text.as_bytes())?;
    ledger.record_entry(&EntryWrite {
        turn: &row.id,
        ticket: &row.ticket,
        path: &relative,
        hash: &hash,
        signature: &signature,
        head: head.as_deref(),
        written: &now,
    })?;
    Ok(Written::Written)
}

/// The folder of the journal that a work turn reads, made when it is missing, as its real
/// path: the sandbox compares real paths.
pub fn turn_dir(paths: &crate::paths::Paths) -> Result<PathBuf> {
    let dir = paths.journal_dir();
    ensure_dir(&dir)?;
    std::fs::canonicalize(&dir).with_context(|| format!("could not read {}", dir.display()))
}

/// The path of an entry relative to the home, as the ledger keeps it.
pub fn relative_path(key: &TicketKey, id: &TurnId) -> String {
    format!("journal/{key}/{id}.md")
}

/// The last commit of a turn at its first write: the last commit of the clone when the turn
/// is the last turn of a ticket that holds a clone, else the commit of its facts file.
fn first_head(
    ctx: &Ctx,
    ledger: &Ledger,
    row: &TurnRow,
    key: &TicketKey,
    id: &TurnId,
) -> Option<String> {
    let last = ledger.last_turn(&row.ticket).ok().flatten();
    if last.is_some_and(|last| last.id == row.id) {
        let clone = ledger
            .ticket(&row.ticket)
            .ok()
            .flatten()
            .and_then(|t| t.clone)
            .filter(|c| !c.trim().is_empty())
            .and_then(|c| crate::paths::expand_tilde(&c).ok());
        if let Some(head) = clone.and_then(|c| git::run(&c, &["rev-parse", "HEAD"]).ok()) {
            return Some(head);
        }
    }
    let facts = read_optional(&ctx.paths.turn(key, id).facts)
        .ok()
        .flatten()?;
    let facts: serde_json::Value = serde_json::from_str(&facts).ok()?;
    facts
        .get("head")
        .and_then(serde_json::Value::as_str)
        .map(str::to_string)
}

/// Each entry file of the journal, newest first. The name of an entry starts with the start
/// time of its turn, so the order of the names is the order of the turns.
pub fn entry_files(journal: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    let Ok(tickets) = std::fs::read_dir(journal) else {
        return files;
    };
    for ticket in tickets.flatten() {
        let Ok(entries) = std::fs::read_dir(ticket.path()) else {
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
    files.sort_by(|a, b| b.file_name().cmp(&a.file_name()));
    files
}

/// The entry file of a turn id, wherever its ticket folder is.
pub fn find_entry(journal: &Path, id: &str) -> Option<PathBuf> {
    let name = format!("{id}.md");
    entry_files(journal)
        .into_iter()
        .find(|p| p.file_name().is_some_and(|n| n.to_string_lossy() == name))
}

/// `vision journal ...`.
pub fn run(ctx: &Ctx, command: JournalCommand) -> Result<()> {
    commands::journal(ctx, command)
}

/// `vision archive ...`.
pub fn run_archive(ctx: &Ctx, args: ArchiveArgs) -> Result<()> {
    commands::archive(ctx, args)
}
