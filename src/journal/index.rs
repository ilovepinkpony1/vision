//! The index of the journal in MemPalace (section 16). Each entry is one drawer: the wing of
//! `journal.wing`, the ticket key as the room, and the path of the entry file as its source.
//! A new text of an entry replaces the drawers of the same source: the call deletes by source
//! first and then adds, so that a rewrite leaves no second drawer.
//!
//! Only the text of an entry goes to MemPalace, never a transcript.

use std::collections::BTreeSet;

use anyhow::{bail, Result};
use serde_json::{json, Value};

use super::errors::cut;
use super::mcp::{self, Client};
use crate::ctx::Ctx;
use crate::ledger::Ledger;
use crate::util::{ids, time};

/// The tool that adds a drawer.
pub const ADD_TOOL: &str = "mempalace_add_drawer";
/// The tool that deletes each drawer of one source file.
pub const DELETE_TOOL: &str = "mempalace_delete_by_source";
/// The tool that searches.
pub const SEARCH_TOOL: &str = "mempalace_search";
/// The tools that the index needs.
pub const TOOLS: [&str; 3] = [ADD_TOOL, DELETE_TOOL, SEARCH_TOOL];
/// The longest text of a drawer. MemPalace refuses a text above 100 000 characters.
const MAX_TEXT: usize = 90_000;

/// What one run of the index did.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Filed {
    /// The entries that MemPalace now holds.
    pub filed: usize,
    /// The entries that still wait.
    pub waiting: usize,
    /// Why MemPalace did not answer, when it did not.
    pub error: Option<String>,
    /// The entries whose filing failed for a reason of their own, with why.
    pub failed: Vec<String>,
}

/// Files each entry that waits for the index, at most `limit` of them. With the index off, it
/// files nothing. When MemPalace does not answer, the run stops and gives why in `error`.
pub fn file_waiting(ctx: &Ctx, ledger: &Ledger, limit: Option<usize>) -> Result<Filed> {
    let journal = &ctx.config()?.journal;
    let waiting = ledger.journal_rows_to_index()?;
    let mut report = Filed {
        waiting: waiting.len(),
        ..Filed::default()
    };
    if !journal.index_on() || waiting.is_empty() {
        return Ok(report);
    }
    let mut client = match mcp::connect_config(journal) {
        Ok(client) => client,
        Err(e) => {
            report.error = Some(format!("{e:#}"));
            return Ok(report);
        }
    };
    for row in waiting.iter().take(limit.unwrap_or(usize::MAX)) {
        let Some(relative) = &row.path else {
            continue;
        };
        let path = ctx.paths.home().join(relative);
        let text = match std::fs::read_to_string(&path) {
            Ok(text) => text,
            Err(e) => {
                report.failed.push(format!(
                    "{}: could not read {}: {e}",
                    row.turn,
                    path.display()
                ));
                continue;
            }
        };
        match file_one(
            &mut client,
            &journal.wing,
            &row.ticket,
            &path.display().to_string(),
            &text,
        ) {
            Ok(()) => {
                ledger.set_entry_filed(
                    &row.turn,
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
            Err(e) => report.failed.push(format!("{}: {e:#}", row.turn)),
        }
    }
    for failure in &report.failed {
        ctx.log(&format!("journal: the index refused an entry: {failure}"));
    }
    Ok(report)
}

/// Replaces the drawers of one source with one drawer of the text.
pub fn file_one(
    client: &mut Client,
    wing: &str,
    room: &str,
    source: &str,
    text: &str,
) -> Result<()> {
    let deleted = client.call_tool(
        DELETE_TOOL,
        json!({"source_file": source, "dry_run": false}),
    )?;
    if deleted.get("success").and_then(Value::as_bool) == Some(false) {
        bail!("{DELETE_TOOL} did not delete the old drawers of {source}: {deleted}");
    }
    let added = client.call_tool(
        ADD_TOOL,
        json!({
            "wing": wing, "room": room, "content": cut(text, MAX_TEXT),
            "source_file": source, "added_by": "vision",
        }),
    )?;
    if added.get("success").and_then(Value::as_bool) != Some(true) {
        bail!("{ADD_TOOL} did not add the drawer of {source}: {added}");
    }
    Ok(())
}

/// One hit of a search of MemPalace.
#[derive(Clone, Debug, PartialEq)]
pub struct IndexHit {
    /// The full source path, which names the entry file.
    pub source: String,
    pub room: String,
    pub text: String,
    pub similarity: f64,
}

/// Searches the wing with each query, with the candidates of both the vectors and the words
/// (`union`), because the vectors alone miss an exact token such as a commit hash.
pub fn search(
    client: &mut Client,
    wing: &str,
    queries: &[String],
    limit: usize,
) -> Result<Vec<IndexHit>> {
    let mut hits: Vec<IndexHit> = Vec::new();
    let mut seen = BTreeSet::new();
    for query in queries {
        let query = cut(query.trim(), 250);
        if query.is_empty() {
            continue;
        }
        let found = client.call_tool(
            SEARCH_TOOL,
            json!({
                "query": query, "limit": limit.clamp(1, 100), "wing": wing,
                "candidate_strategy": "union",
            }),
        )?;
        for result in found
            .get("results")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            let text_of = |key: &str| {
                result
                    .get(key)
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string()
            };
            let source = text_of("source_path");
            if source.is_empty() || !seen.insert(source.clone()) {
                continue;
            }
            hits.push(IndexHit {
                source,
                room: text_of("room"),
                text: text_of("text"),
                similarity: result
                    .get("similarity")
                    .and_then(Value::as_f64)
                    .unwrap_or(0.0),
            });
        }
    }
    Ok(hits)
}

/// The tools of [`TOOLS`] that the server lacks.
pub fn missing_tools(client: &mut Client) -> Result<Vec<&'static str>> {
    let names = client.tool_names()?;
    Ok(TOOLS
        .iter()
        .copied()
        .filter(|t| !names.iter().any(|n| n == t))
        .collect())
}
