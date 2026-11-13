//! `vision journal search | show | reindex | rebuild` and `vision archive [fetch | status]`.

use std::collections::BTreeSet;

use anyhow::{bail, Result};
use serde_json::json;

use super::archive::{self, Store};
use super::search::{self, Source};
use super::{entry_files, find_entry, index, write_entry, Inputs, Written};
use crate::cli::{ArchiveArgs, ArchiveCommand, JournalCommand};
use crate::ctx::Ctx;
use crate::util::time;

/// The hits of `vision journal search` with no `--limit`.
const DEFAULT_LIMIT: usize = 10;

pub fn journal(ctx: &Ctx, command: JournalCommand) -> Result<()> {
    ctx.require_setup()?;
    match command {
        JournalCommand::Search { text, json, limit } => search_command(ctx, &text, json, limit),
        JournalCommand::Show { id } => show(ctx, &id),
        JournalCommand::Reindex { all } => reindex(ctx, all),
        JournalCommand::Rebuild => rebuild(ctx),
    }
}

fn search_command(ctx: &Ctx, text: &str, json: bool, limit: Option<usize>) -> Result<()> {
    if text.trim().is_empty() {
        bail!("give a text to search for, such as `vision journal search \"cannot find value\"`");
    }
    let limit = limit.unwrap_or(DEFAULT_LIMIT).clamp(1, 100);
    let found = search::search(ctx, &[text.to_string()], limit, &BTreeSet::new())?;
    if json {
        println!("{}", serde_json::to_string_pretty(&found)?);
        return Ok(());
    }
    let source = match found.source {
        Source::Mempalace => "MemPalace",
        Source::Local => "the entry files",
    };
    if let Some(note) = &found.note {
        println!("MemPalace did not answer, so the search read the entry files: {note}");
    }
    if found.hits.is_empty() {
        println!("No entry fits. The search read {source}.");
        return Ok(());
    }
    println!("{} entr(y/ies) from {source}:", found.hits.len());
    for hit in &found.hits {
        println!("{}  {} {}", hit.id, hit.ticket, hit.step);
        if !hit.summary.is_empty() {
            println!("    {}", hit.summary);
        }
    }
    println!("Show one with `vision journal show ID`.");
    Ok(())
}

fn show(ctx: &Ctx, id: &str) -> Result<()> {
    let id = id.trim().trim_end_matches(".md");
    let Some(path) = find_entry(&ctx.paths.journal_dir(), id) else {
        bail!("the journal has no entry {id}. Find one with `vision journal search TEXT`");
    };
    print!("{}", std::fs::read_to_string(&path)?);
    Ok(())
}

fn reindex(ctx: &Ctx, all: bool) -> Result<()> {
    let config = ctx.config()?;
    if !config.journal.index_on() {
        bail!("the index is off. Set journal.mempalace_url in config.toml, such as \"http://127.0.0.1:8765/mcp\"");
    }
    let ledger = ctx.ledger()?;
    if all {
        ledger.forget_filed()?;
    }
    let report = index::file_waiting(ctx, &ledger, None)?;
    if let Some(why) = &report.error {
        bail!(
            "MemPalace does not answer, so {} entr(y/ies) still wait: {why}",
            report.waiting
        );
    }
    println!(
        "{} entr(y/ies) filed in MemPalace, {} still wait",
        report.filed, report.waiting
    );
    for failure in &report.failed {
        println!("refused: {failure}");
    }
    if !report.failed.is_empty() {
        bail!("{} entr(y/ies) were refused", report.failed.len());
    }
    Ok(())
}

/// Writes the entry of each turn whose folder is still here, also the turns of before the
/// journal. Then the index files them, when it is on.
fn rebuild(ctx: &Ctx) -> Result<()> {
    let config = ctx.config()?;
    let ledger = ctx.ledger()?;
    let inputs = Inputs::new(ctx);
    // The secrets load first: with no secret, no entry is written.
    inputs.secrets()?;
    let (mut written, mut same, mut skipped, mut failed) = (0, 0, 0, 0);
    for folder in archive::turn_folders(&ctx.paths) {
        let Some(row) = ledger.turn(&folder.turn)? else {
            skipped += 1;
            continue;
        };
        match write_entry(ctx, &ledger, &row, &inputs, true) {
            Ok(Written::Written) => written += 1,
            Ok(Written::Unchanged) => same += 1,
            Ok(Written::Skipped(_)) => skipped += 1,
            Err(e) => {
                failed += 1;
                eprintln!("could not write the entry of {}: {e:#}", folder.turn);
            }
        }
    }
    println!(
        "{written} entr(y/ies) written, {same} unchanged, {skipped} turn(s) with no entry, {failed} failed. The journal holds {} entr(y/ies)",
        entry_files(&ctx.paths.journal_dir()).len()
    );
    if config.journal.index_on() {
        let report = index::file_waiting(ctx, &ledger, None)?;
        match &report.error {
            Some(why) => println!(
                "MemPalace does not answer, so {} entr(y/ies) wait for `vision journal reindex`: {why}",
                report.waiting
            ),
            None => println!("{} entr(y/ies) filed in MemPalace", report.filed),
        }
    }
    if failed > 0 {
        bail!("{failed} entr(y/ies) could not be written");
    }
    Ok(())
}

pub fn archive(ctx: &Ctx, args: ArchiveArgs) -> Result<()> {
    ctx.require_setup()?;
    match args.command {
        None => run(ctx, args.dry_run),
        Some(ArchiveCommand::Fetch { turn }) => {
            let ledger = ctx.ledger()?;
            let dir = archive::fetch(ctx, &ledger, &turn)?;
            println!("{turn} is back at {}", dir.display());
            Ok(())
        }
        Some(ArchiveCommand::Status { json }) => status(ctx, json),
        Some(ArchiveCommand::MoveToBucket { from, dry_run }) => move_to_bucket(ctx, &from, dry_run),
    }
}

/// `vision archive move-to-bucket --from DIR [--dry-run]`.
fn move_to_bucket(ctx: &Ctx, from: &str, dry_run: bool) -> Result<()> {
    let config = &ctx.config()?.archive;
    if !config.bucket_on() {
        bail!("archive.bucket is empty. Name the bucket in config.toml (and leave archive.dir empty), then move the folder");
    }
    let root = crate::paths::expand_tilde(from.trim())?;
    if !root.is_dir() {
        bail!("{} is not a folder", root.display());
    }
    let files = archive::folder_files(&root)?;
    let bytes: u64 = files.iter().map(|f| f.bytes).sum();
    let human = crate::util::fs::human_bytes;
    if dry_run {
        println!(
            "{} file(s), {}, wait to move from {} to s3://{}/. Nothing changed",
            files.len(),
            human(bytes),
            root.display(),
            config.bucket.trim()
        );
        for file in &files {
            println!("move  {}", file.key);
        }
        return Ok(());
    }
    let store = Store::bucket(config)?;
    let report = archive::move_to_bucket(&store, &root, &files);
    println!(
        "{} file(s), {}, moved to s3://{}/; {} failed and stay in {}",
        report.moved,
        human(report.bytes),
        config.bucket.trim(),
        report.failed.len(),
        root.display()
    );
    for failure in &report.failed {
        println!("failed: {failure}");
    }
    if !report.failed.is_empty() {
        bail!("{} file(s) did not move", report.failed.len());
    }
    Ok(())
}

fn run(ctx: &Ctx, dry_run: bool) -> Result<()> {
    let config = &ctx.config()?.archive;
    if !config.on() {
        bail!("{}", archive::OFF);
    }
    let ledger = ctx.ledger()?;
    let now = time::now();
    let plan = archive::plan(ctx, &ledger, now)?;
    if dry_run {
        println!(
            "{} turn folder(s) and {} entr(y/ies) wait for the upload; {} turn folder(s) are due for deletion. Nothing changed",
            plan.uploads.len(),
            plan.entries.len(),
            plan.deletions.len()
        );
        for folder in &plan.uploads {
            println!(
                "upload  {}",
                archive::folder_key(config, &folder.ticket, &folder.turn)
            );
        }
        for (ticket, turn, _) in &plan.entries {
            println!("upload  {}", archive::entry_key(config, ticket, turn));
        }
        for folder in &plan.deletions {
            println!("delete  {}", folder.dir.display());
        }
        return Ok(());
    }
    ledger.add_event(
        &crate::ledger::Event::now("user", archive::RUN_EVENT).detail(json!({"by": "command"})),
    )?;
    let store = Store::new(config)?;
    let mut report = archive::upload(ctx, &ledger, &store, &plan, None)?;
    // A folder that went up in this run waits `keep_days` days, so only the plan of before the
    // run can be due.
    archive::delete_due(&ledger, &plan, &mut report)?;
    println!(
        "{} turn folder(s) and {} entr(y/ies) went up; {} turn folder(s) deleted",
        report.uploaded.len(),
        report.entries,
        report.deleted.len()
    );
    for failure in &report.failed {
        println!("failed: {failure}");
    }
    if !report.failed.is_empty() {
        bail!("{} step(s) of the archive failed", report.failed.len());
    }
    Ok(())
}

fn status(ctx: &Ctx, json: bool) -> Result<()> {
    let config = &ctx.config()?.archive;
    let ledger = ctx.ledger()?;
    let rows = ledger.journal_rows()?;
    let archived = rows.iter().filter(|r| r.archived.is_some()).count();
    let removed = rows.iter().filter(|r| r.removed.is_some()).count();
    let sent = rows
        .iter()
        .filter(|r| r.hash.is_some() && !r.entry_waits_for_archive())
        .count();
    let plan = archive::plan(ctx, &ledger, time::now())?;
    let last = ledger.last_event_at(archive::RUN_EVENT)?;
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({
                "on": config.on(), "dir": config.dir, "bucket": config.bucket, "prefix": config.prefix,
                "endpoint": config.endpoint, "profile": config.profile, "region": config.region,
                "keep_days": config.keep_days, "archived": archived, "removed": removed,
                "entries_sent": sent, "waiting": plan.uploads.len(),
                "entries_waiting": plan.entries.len(), "due": plan.deletions.len(),
                "last_run": last,
            }))?
        );
        return Ok(());
    }
    if config.on() {
        let mut where_ = archive::place(config);
        if config.bucket_on() && !config.endpoint.is_empty() {
            where_.push_str(&format!(" at {}", config.endpoint));
        }
        if config.bucket_on() && !config.profile.is_empty() {
            where_.push_str(&format!(" with the profile {}", config.profile));
        }
        println!(
            "archive   on: {where_}. A local turn folder stays {} days after the archive",
            config.keep_days
        );
    } else {
        println!("archive   off. Set archive.dir or archive.bucket in config.toml to turn it on");
    }
    println!(
        "folders   {archived} archived, {removed} of them deleted here; {} wait for the upload; {} due for deletion",
        plan.uploads.len(),
        plan.deletions.len()
    );
    println!(
        "entries   {sent} in the archive; {} wait",
        plan.entries.len()
    );
    println!("last run  {}", last.as_deref().unwrap_or("never"));
    Ok(())
}
