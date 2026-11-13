//! `vision sessions archive | search | fetch`.

use std::path::{Path, PathBuf};
use std::time::SystemTime;

use anyhow::{bail, Result};
use serde::Serialize;

use super::summary::{self, SessionEntry};
use super::{archive, file_waiting, plan, record_run, session_key, summary_entries};
use crate::cli::SessionsCommand;
use crate::ctx::Ctx;
use crate::journal::archive::Store;
use crate::journal::errors::cut;
use crate::journal::search::{words, Source};
use crate::journal::{index, mcp};

/// The hits of `vision sessions search` with no `--limit`.
const DEFAULT_LIMIT: usize = 10;

pub fn run(ctx: &Ctx, command: SessionsCommand) -> Result<()> {
    ctx.require_setup()?;
    match command {
        SessionsCommand::Archive { dry_run } => archive_command(ctx, dry_run),
        SessionsCommand::Search { text, json, limit } => search_command(ctx, &text, json, limit),
        SessionsCommand::Fetch { session, to } => fetch_command(ctx, &session, to),
    }
}

fn archive_command(ctx: &Ctx, dry_run: bool) -> Result<()> {
    let config = ctx.config()?;
    if !config.sessions.enabled {
        bail!("the archive of the sessions is off. Set sessions.enabled = true in config.toml, with archive.dir or archive.bucket");
    }
    let ledger = ctx.ledger()?;
    let plan = plan(ctx, &ledger, SystemTime::now())?;
    if dry_run {
        println!(
            "{} session file(s) wait for the archive, {} summar(y/ies) wait, {} file(s) still change, {} file(s) are in the archive. Nothing changed",
            plan.copies.len(),
            plan.summaries.len(),
            plan.changing,
            plan.archived
        );
        for file in &plan.copies {
            println!("copy     {}", session_key(&config.archive, &file.rel));
        }
        for row in &plan.summaries {
            println!("summary  {}", row.session);
        }
        return Ok(());
    }
    let store = Store::new(&config.archive)?;
    let report = archive(ctx, &ledger, &store, &plan, None)?;
    record_run(&ledger, "user", &report)?;
    println!(
        "{} session file(s) went to the archive ({}); {} were there already; {} summar(y/ies) written; {} file(s) still change",
        report.copied.len(),
        crate::util::fs::human_bytes(report.bytes),
        report.same,
        report.summaries,
        plan.changing + report.moved
    );
    if config.journal.index_on() {
        let filed = file_waiting(ctx, &ledger, None)?;
        match &filed.error {
            Some(why) => println!(
                "MemPalace does not answer, so {} summar(y/ies) wait for the index: {why}",
                filed.waiting
            ),
            None => println!(
                "{} summar(y/ies) filed in MemPalace, {} still wait",
                filed.filed, filed.waiting
            ),
        }
    }
    for failure in &report.failed {
        println!("failed: {failure}");
    }
    if !report.failed.is_empty() {
        bail!(
            "{} step(s) of the archive of the sessions failed",
            report.failed.len()
        );
    }
    Ok(())
}

/// One session that a search found.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
struct Hit {
    session: String,
    project: String,
    /// The day of the start, such as 2026-11-02.
    date: String,
    started: Option<String>,
    title: Option<String>,
    first_ask: Option<String>,
    archive: Option<String>,
    entry: PathBuf,
}

#[derive(Serialize)]
struct Found {
    source: Source,
    note: Option<String>,
    hits: Vec<Hit>,
}

fn hit_of(entry: SessionEntry) -> Hit {
    Hit {
        date: entry
            .started
            .as_deref()
            .map(|s| s.chars().take(10).collect())
            .unwrap_or_default(),
        session: entry.session,
        project: entry.project,
        started: entry.started,
        title: entry.title,
        first_ask: entry.first_ask,
        archive: entry.archive,
        entry: entry.path,
    }
}

fn search_command(ctx: &Ctx, text: &str, json: bool, limit: Option<usize>) -> Result<()> {
    if text.trim().is_empty() {
        bail!("give a text to search for, such as `vision sessions search \"export\"`");
    }
    let limit = limit.unwrap_or(DEFAULT_LIMIT).clamp(1, 100);
    let found = search(ctx, text, limit)?;
    if json {
        println!("{}", serde_json::to_string_pretty(&found)?);
        return Ok(());
    }
    let source = match found.source {
        Source::Mempalace => "MemPalace",
        Source::Local => "the summary files",
    };
    if let Some(note) = &found.note {
        println!("MemPalace did not answer, so the search read the summary files: {note}");
    }
    if found.hits.is_empty() {
        println!("No session fits. The search read {source}.");
        return Ok(());
    }
    println!("{} session(s) from {source}:", found.hits.len());
    for hit in &found.hits {
        println!("{}  {}  {}", hit.session, hit.date, hit.project);
        if let Some(ask) = &hit.first_ask {
            println!("    {}", cut(ask, 160));
        }
        if let Some(key) = &hit.archive {
            println!("    archive: {key}");
        }
    }
    println!("Bring one back with `vision sessions fetch SESSION-ID --to DIR`.");
    Ok(())
}

fn search(ctx: &Ctx, text: &str, limit: usize) -> Result<Found> {
    let config = ctx.config()?;
    let mut note = None;
    if config.journal.index_on() {
        match search_index(ctx, text, limit) {
            Ok(hits) => {
                return Ok(Found {
                    source: Source::Mempalace,
                    note: None,
                    hits,
                })
            }
            Err(e) => note = Some(format!("{e:#}")),
        }
    }
    Ok(Found {
        source: Source::Local,
        note,
        hits: local_search(&ctx.paths.session_journal_dir(), text, limit),
    })
}

fn search_index(ctx: &Ctx, text: &str, limit: usize) -> Result<Vec<Hit>> {
    let config = ctx.config()?;
    let mut client = mcp::connect_config(&config.journal)?;
    let found = index::search(
        &mut client,
        &config.sessions.wing,
        &[text.to_string()],
        limit,
    )?;
    let journal = ctx.paths.session_journal_dir();
    let mut hits = Vec::new();
    for hit in found {
        // The local file is the source of truth: the hit only names it. The text of the hit
        // serves when the file is not on this machine.
        let source = Path::new(&hit.source);
        let id = source
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default();
        let entry = summary::read_entry(source)
            .or_else(|| {
                summary_entries(&journal)
                    .into_iter()
                    .find(|p| p.file_stem().is_some_and(|s| s.to_string_lossy() == id))
                    .and_then(|p| summary::read_entry(&p))
            })
            .or_else(|| summary::parse(&hit.text, source));
        if let Some(entry) = entry {
            hits.push(hit_of(entry));
        }
        if hits.len() >= limit {
            break;
        }
    }
    Ok(hits)
}

/// The plain search over the summary files: more words of the query first, the newest
/// session first among equals.
fn local_search(journal: &Path, text: &str, limit: usize) -> Vec<Hit> {
    let words = words(&[text.to_string()]);
    if words.is_empty() {
        return Vec::new();
    }
    let mut scored: Vec<(usize, SessionEntry)> = summary_entries(journal)
        .into_iter()
        .filter_map(|p| summary::read_entry(&p))
        .filter_map(|entry| {
            let lower = entry.text.to_lowercase();
            let score = words.iter().filter(|w| lower.contains(w.as_str())).count();
            (score > 0).then_some((score, entry))
        })
        .collect();
    scored.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| b.1.started.cmp(&a.1.started)));
    scored
        .into_iter()
        .take(limit)
        .map(|(_, entry)| hit_of(entry))
        .collect()
}

fn fetch_command(ctx: &Ctx, session: &str, to: Option<PathBuf>) -> Result<()> {
    let ledger = ctx.ledger()?;
    let to = match to {
        Some(to) => to,
        None => std::env::current_dir()?,
    };
    let files = super::fetch(ctx, &ledger, session, &to)?;
    println!("{} file(s) of the session are back:", files.len());
    for file in &files {
        println!("{}", file.display());
    }
    Ok(())
}
