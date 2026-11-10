//! The commands of section 9.2: `vision memory [QUERY]`, `show`, `edit`, `teach`, `retire`,
//! `lint`, `import`, `log`, and `revert`. `review` belongs to the curator milestone.

use std::fs;
use std::path::Path;
use std::process::Command;

use anyhow::{anyhow, bail, Context, Result};
use serde::Serialize;
use serde_json::json;

use super::model::{MemoryFile, MemoryId, NewPrinciple, Problem, ALL_AGENTS};
use super::store::{short, Filter, Invalid, Memory};
use crate::cli::{MemoryArgs, MemoryCommand};
use crate::ctx::Ctx;
use crate::exit::{not_built, Stop};
use crate::ledger::memory_use::MemoryUse;
use crate::models;
use crate::util::fs::{atomic_write, ensure_dir};

/// `vision memory` and each of its subcommands.
pub fn run(ctx: &Ctx, args: MemoryArgs) -> Result<()> {
    let MemoryArgs {
        command,
        query,
        kind,
        area,
        status,
        json,
    } = args;
    match command {
        None => {
            let filter = Filter {
                text: query,
                kind: kind.map(Into::into),
                area,
                status: status.map(Into::into),
                ..Filter::default()
            };
            list(ctx, &filter, json)
        }
        Some(MemoryCommand::Show { id, json }) => show(ctx, &id, json),
        Some(MemoryCommand::Edit { id }) => edit(ctx, &id),
        Some(MemoryCommand::Teach { agent, text }) => teach(ctx, &agent, &text),
        Some(MemoryCommand::Retire { id, why }) => retire(ctx, &id, &why),
        Some(MemoryCommand::Lint { json }) => lint(ctx, json),
        Some(MemoryCommand::Import { file }) => import(ctx, &file),
        Some(MemoryCommand::Review) => Err(not_built("memory review")),
        Some(MemoryCommand::Log { json }) => log(ctx, json),
        Some(MemoryCommand::Revert { commit }) => revert(ctx, &commit),
    }
}

fn list(ctx: &Ctx, filter: &Filter, json: bool) -> Result<()> {
    let memory = Memory::from_ctx(ctx)?;
    let listing = memory.list(filter)?;
    if json {
        println!("{}", serde_json::to_string_pretty(&listing)?);
        return Ok(());
    }
    for problem in &listing.problems {
        eprintln!("warning: {problem}");
    }
    if !listing.problems.is_empty() {
        eprintln!("warning: run `vision memory lint` to see each problem of the memory");
    }
    if listing.files.is_empty() {
        if *filter == Filter::default() {
            println!("The memory is empty. Decisions come from answers, and `vision memory teach` writes a principle.");
        } else {
            println!("No file matches.");
        }
        return Ok(());
    }
    let rows: Vec<Vec<String>> = listing
        .files
        .iter()
        .map(|file| {
            vec![
                file.id().to_string(),
                file.status().to_string(),
                file.kind().map_or("-".to_string(), |k| k.to_string()),
                dash(&file.area().join(",")),
                file.by().to_string(),
                dash(file.agent().unwrap_or("")),
                short(file.headline(), 70),
            ]
        })
        .collect();
    print!(
        "{}",
        table(
            &["ID", "STATUS", "KIND", "AREA", "BY", "AGENT", "TEXT"],
            &rows
        )
    );
    Ok(())
}

/// One file that a shown file names, or that names it.
#[derive(Debug, Serialize)]
struct Reference {
    /// `source`, `supersedes`, `superseded by`, `evidence`, or `evidence for`.
    role: &'static str,
    id: String,
    exists: bool,
    status: Option<String>,
    by: Option<String>,
    text: Option<String>,
}

fn show(ctx: &Ctx, id: &str, json: bool) -> Result<()> {
    let id = MemoryId::parse(id)?;
    let memory = Memory::from_ctx(ctx)?;
    let text = memory
        .read_text(id)?
        .ok_or_else(|| anyhow!("the memory has no {id}. Run `vision memory` to list the files"))?;
    let path = id.path();
    let parsed = MemoryFile::parse(&path, &text);
    let all = memory.list(&Filter::default())?.files;
    let find = |want: &str| all.iter().find(|f| f.id().to_string() == want);
    let reference = |role: &'static str, want: String| match find(&want) {
        Some(f) => Reference {
            role,
            id: want,
            exists: true,
            status: Some(f.status().to_string()),
            by: Some(f.by().to_string()),
            text: Some(short(f.headline(), 70)),
        },
        None => Reference {
            role,
            exists: memory.exists(&want),
            id: want,
            status: None,
            by: None,
            text: None,
        },
    };
    let mut references = Vec::new();
    if let Ok(file) = &parsed {
        match file {
            MemoryFile::Decision(d) => {
                for source in &d.sources {
                    references.push(reference("source", source.to_string()));
                }
                if let Some(old) = d.supersedes {
                    references.push(reference("supersedes", old.to_string()));
                }
            }
            MemoryFile::Principle(p) => {
                for evidence in &p.evidence {
                    references.push(reference("evidence", evidence.to_string()));
                }
            }
        }
    }
    for other in &all {
        match other {
            MemoryFile::Decision(d) if d.supersedes.map(MemoryId::from) == Some(id) => {
                references.push(reference("superseded by", d.id.to_string()));
            }
            MemoryFile::Decision(d) if d.sources.contains(&id) => {
                references.push(reference("source of", d.id.to_string()));
            }
            MemoryFile::Principle(p) if p.evidence.iter().any(|e| MemoryId::from(*e) == id) => {
                references.push(reference("evidence for", p.id.to_string()));
            }
            _ => {}
        }
    }
    let (turns, ledger_problem) = match ctx
        .ledger()
        .and_then(|l| l.memory_uses(&id.to_string(), 20))
    {
        Ok(rows) => (rows, None),
        Err(e) => (Vec::new(), Some(format!("{e:#}"))),
    };
    let problems: Vec<Problem> = parsed.as_ref().err().cloned().unwrap_or_default();
    let uses = match &parsed {
        Ok(MemoryFile::Principle(p)) => Some(p.uses),
        _ => None,
    };
    if json {
        let value = json!({
            "id": id.to_string(),
            "path": path,
            "text": text,
            "file": parsed.as_ref().ok(),
            "problems": problems,
            "references": references,
            "uses": uses,
            "turns": turns,
        });
        println!("{}", serde_json::to_string_pretty(&value)?);
        return Ok(());
    }
    print!("{text}");
    if !text.ends_with('\n') {
        println!();
    }
    println!("\n# {}", memory.dir().join(&path).display());
    for problem in &problems {
        println!("# problem: {problem}");
    }
    if !references.is_empty() {
        let rows: Vec<Vec<String>> = references
            .iter()
            .map(|r| {
                vec![
                    r.role.to_string(),
                    r.id.clone(),
                    r.status.clone().unwrap_or_else(|| {
                        if r.exists { "not valid" } else { "missing" }.to_string()
                    }),
                    dash(r.by.as_deref().unwrap_or("")),
                    r.text.clone().unwrap_or_default(),
                ]
            })
            .collect();
        println!("\nNamed files:");
        print!(
            "{}",
            indent(&table(&["ROLE", "ID", "STATUS", "BY", "TEXT"], &rows))
        );
    }
    if let Some(uses) = uses {
        println!(
            "\nUses: used {}, agreed {}, corrected {}.",
            uses.used, uses.agreed, uses.corrected
        );
    }
    print_turns(&turns, ledger_problem.as_deref());
    Ok(())
}

fn print_turns(turns: &[MemoryUse], problem: Option<&str>) {
    if let Some(problem) = problem {
        println!("\nThe turns that got it are unknown: {problem}");
        return;
    }
    if turns.is_empty() {
        println!("\nNo turn got it yet.");
        return;
    }
    println!("\nThe last turns that got it:");
    let rows: Vec<Vec<String>> = turns
        .iter()
        .map(|t| {
            vec![
                t.turn.clone(),
                if t.applied { "applied" } else { "not applied" }.to_string(),
                t.how.clone().unwrap_or_default(),
            ]
        })
        .collect();
    print!("{}", indent(&table(&["TURN", "USE", "HOW"], &rows)));
}

fn edit(ctx: &Ctx, id: &str) -> Result<()> {
    let id = MemoryId::parse(id)?;
    let memory = Memory::from_ctx(ctx)?;
    let original = memory
        .read_text(id)?
        .ok_or_else(|| anyhow!("the memory has no {id}. Run `vision memory` to list the files"))?;
    let editor = editor()?;
    // The draft keeps a text that was not valid, so that the next edit opens it again, as long
    // as the file did not change in the meantime.
    let dir = memory.git_dir().join("vision-edit");
    ensure_dir(&dir)?;
    let path = dir.join(format!("{id}.md"));
    let base = dir.join(format!("{id}.base"));
    let draft_fits = fs::read_to_string(&base).is_ok_and(|b| b == original) && path.is_file();
    if draft_fits {
        eprintln!("Opening your last text of {id}, which was not valid.");
    } else {
        atomic_write(&path, original.as_bytes())?;
        atomic_write(&base, original.as_bytes())?;
    }
    let forget = || {
        let _ = fs::remove_file(&path);
        let _ = fs::remove_file(&base);
    };
    let status = Command::new("sh")
        .arg("-c")
        .arg(format!("{editor} \"$1\""))
        .arg("sh")
        .arg(&path)
        .status()
        .with_context(|| format!("could not start the editor `{editor}`"))?;
    if !status.success() {
        if fs::read_to_string(&path).is_ok_and(|t| t == original) {
            forget();
        }
        bail!("the editor `{editor}` ended with {status}, so {id} did not change");
    }
    let edited = fs::read_to_string(&path)
        .with_context(|| format!("could not read the edited text {}", path.display()))?;
    match memory.replace_text(id, &original, &edited, memory.person()) {
        Ok(true) => {
            forget();
            let head = memory.head()?;
            println!("Saved {id} in the commit {}.", &head[..head.len().min(7)]);
            Ok(())
        }
        Ok(false) => {
            forget();
            println!("No change to {id}.");
            Ok(())
        }
        Err(e) => {
            if let Some(invalid) = e.downcast_ref::<Invalid>() {
                eprintln!("error: the edited {id} is not valid, so the file stays as it was:");
                for problem in &invalid.problems {
                    eprintln!("  {problem}");
                }
                eprintln!(
                    "Your text is kept in {}. Run `vision memory edit {id}` again to fix it.",
                    path.display()
                );
                return Err(Stop::Reported("memory edit".into()).into());
            }
            Err(e)
        }
    }
}

/// The editor of the person: `EDITOR`, else `VISUAL`.
fn editor() -> Result<String> {
    for name in ["EDITOR", "VISUAL"] {
        if let Ok(value) = std::env::var(name) {
            if !value.trim().is_empty() {
                return Ok(value);
            }
        }
    }
    bail!("set EDITOR to your editor, such as `export EDITOR=vi`, and run the command again")
}

fn teach(ctx: &Ctx, agent: &str, text: &str) -> Result<()> {
    let agent = agent.trim();
    if agent != ALL_AGENTS && models::agent(agent).is_none() {
        bail!(
            "`{agent}` is not an agent of Vision. Use {ALL_AGENTS}, or one of: {}",
            models::agent_names().join(", ")
        );
    }
    let (rule, because) = split_because(text);
    if rule.is_empty() {
        bail!("the rule is empty. Write it as \"When X, make sure that Y, because Z\"");
    }
    let memory = Memory::from_ctx(ctx)?;
    let principle = memory.add_principle(&NewPrinciple {
        agent: agent.to_string(),
        kind: None,
        area: Vec::new(),
        by: memory.person().clone(),
        evidence: Vec::new(),
        rule,
        because,
        not_when: String::new(),
    })?;
    println!("Taught {} to {agent}. It is active at once.", principle.id);
    println!("  Rule: {}", principle.rule);
    if !principle.because.is_empty() {
        println!("  Because: {}", principle.because);
    }
    Ok(())
}

/// Splits "When X, make sure that Y, because Z" into the rule and the reason. Without the word
/// "because", the whole text is the rule.
pub fn split_because(text: &str) -> (String, String) {
    let text = text.trim();
    let bytes = text.as_bytes();
    let word = b"because";
    let is_word_char = |c: char| c.is_alphanumeric() || c == '_';
    let mut at = None;
    for i in 0..bytes.len() {
        if !text.is_char_boundary(i) || i + word.len() > bytes.len() {
            continue;
        }
        if !bytes[i..i + word.len()].eq_ignore_ascii_case(word) {
            continue;
        }
        let before_ok = text[..i]
            .chars()
            .next_back()
            .is_none_or(|c| !is_word_char(c));
        let after_ok = text[i + word.len()..]
            .chars()
            .next()
            .is_none_or(|c| !is_word_char(c));
        if before_ok && after_ok && i > 0 {
            at = Some(i);
            break;
        }
    }
    let Some(i) = at else {
        return (sentence(text), String::new());
    };
    let rule = text[..i]
        .trim_end_matches(|c: char| c.is_whitespace() || matches!(c, ',' | ';' | ':' | '-'));
    let reason = text[i + word.len()..]
        .trim_start_matches(|c: char| c.is_whitespace() || matches!(c, ',' | ':' | '-'));
    if rule.trim().is_empty() {
        return (sentence(text), String::new());
    }
    (sentence(rule), sentence(reason))
}

/// A text with a capital first letter and an end mark.
fn sentence(text: &str) -> String {
    let text = text.trim();
    if text.is_empty() {
        return String::new();
    }
    let mut chars = text.chars();
    let first: String = chars
        .next()
        .map(|c| c.to_uppercase().collect())
        .unwrap_or_default();
    let mut out = format!("{first}{}", chars.as_str());
    if !out.ends_with(['.', '!', '?']) {
        out.push('.');
    }
    out
}

fn retire(ctx: &Ctx, id: &str, why: &str) -> Result<()> {
    let id = MemoryId::parse(id)?;
    let memory = Memory::from_ctx(ctx)?;
    if memory.retire(id, why, memory.person())? {
        println!("Retired {id}. The file stays, and no turn loads it.");
    } else {
        println!("{id} is already retired.");
    }
    Ok(())
}

fn lint(ctx: &Ctx, json: bool) -> Result<()> {
    let memory = Memory::from_ctx(ctx)?;
    let report = memory.lint()?;
    if json {
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        for problem in &report.problems {
            println!("{problem}");
        }
        let files = plural(report.files, "file", "files");
        match report.problems.len() {
            0 => println!("{files} in {}: no problem.", memory.dir().display()),
            n => println!(
                "{files} in {}: {}. Fix each one, such as with `vision memory edit ID`.",
                memory.dir().display(),
                plural(n, "problem", "problems")
            ),
        }
    }
    if report.is_clean() {
        Ok(())
    } else {
        Err(Stop::Reported("memory lint".into()).into())
    }
}

fn import(ctx: &Ctx, file: &Path) -> Result<()> {
    let text =
        fs::read_to_string(file).with_context(|| format!("could not read {}", file.display()))?;
    let memory = Memory::from_ctx(ctx)?;
    let source = file.file_name().map_or_else(
        || file.display().to_string(),
        |n| n.to_string_lossy().to_string(),
    );
    let report = memory.import_jsonl(&text, &source)?;
    for skipped in &report.skipped {
        eprintln!("line {}: skipped: {}", skipped.line, skipped.reason);
    }
    for warning in &report.warnings {
        eprintln!("warning: {warning}");
    }
    println!(
        "Read {}: {} added, {} skipped, {} already there.",
        plural(report.read, "line", "lines"),
        report.added.len(),
        report.skipped.len(),
        report.already
    );
    if let (Some(first), Some(last)) = (report.added.first(), report.added.last()) {
        if first == last {
            println!("The new decision is {first}.");
        } else {
            println!("The new decisions are {first} to {last}.");
        }
    }
    Ok(())
}

fn log(ctx: &Ctx, json: bool) -> Result<()> {
    let memory = Memory::from_ctx(ctx)?;
    let commits = memory.log(None)?;
    if json {
        println!("{}", serde_json::to_string_pretty(&commits)?);
        return Ok(());
    }
    let rows: Vec<Vec<String>> = commits
        .iter()
        .map(|c| {
            vec![
                c.short.clone(),
                c.at.clone(),
                c.author.clone(),
                short(&c.subject, 70),
            ]
        })
        .collect();
    print!("{}", table(&["COMMIT", "TIME", "AUTHOR", "SUBJECT"], &rows));
    Ok(())
}

fn revert(ctx: &Ctx, commit: &str) -> Result<()> {
    let memory = Memory::from_ctx(ctx)?;
    let made = memory.revert(commit, memory.person())?;
    println!(
        "Reverted {commit} with the new commit {}: {}",
        made.short, made.subject
    );
    Ok(())
}

fn dash(text: &str) -> String {
    if text.is_empty() {
        "-".into()
    } else {
        text.to_string()
    }
}

fn plural(n: usize, one: &str, many: &str) -> String {
    format!("{n} {}", if n == 1 { one } else { many })
}

fn indent(text: &str) -> String {
    text.lines().map(|l| format!("  {l}\n")).collect()
}

/// A table with a header and columns that line up. The last column has no padding.
fn table(header: &[&str], rows: &[Vec<String>]) -> String {
    let mut widths: Vec<usize> = header.iter().map(|h| h.chars().count()).collect();
    for row in rows {
        for (i, cell) in row.iter().enumerate() {
            if let Some(w) = widths.get_mut(i) {
                *w = (*w).max(cell.chars().count());
            }
        }
    }
    let line = |cells: Vec<&str>| -> String {
        let last = cells.len().saturating_sub(1);
        let mut out = String::new();
        for (i, cell) in cells.iter().enumerate() {
            out.push_str(cell);
            if i < last {
                let pad = widths[i] - cell.chars().count() + 2;
                out.push_str(&" ".repeat(pad));
            }
        }
        out.trim_end().to_string() + "\n"
    };
    let mut out = line(header.to_vec());
    for row in rows {
        out.push_str(&line(row.iter().map(String::as_str).collect()));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn because_splits_the_rule_and_the_reason() {
        assert_eq!(
            split_because("When X, make sure that Y, because Z"),
            ("When X, make sure that Y.".to_string(), "Z.".to_string())
        );
        assert_eq!(
            split_because("when a report is exported, use CSV BECAUSE: finance imports it."),
            (
                "When a report is exported, use CSV.".to_string(),
                "Finance imports it.".to_string()
            )
        );
        assert_eq!(
            split_because("Keep the becausey word whole"),
            ("Keep the becausey word whole.".to_string(), String::new())
        );
        assert_eq!(
            split_because("Because it is late, stop"),
            ("Because it is late, stop.".to_string(), String::new())
        );
        assert_eq!(
            split_because("Use the old API!"),
            ("Use the old API!".to_string(), String::new())
        );
        assert_eq!(
            split_because("Prüfe é because ü"),
            ("Prüfe é.".to_string(), "Ü.".to_string())
        );
    }

    #[test]
    fn tables_line_up() {
        let rows = vec![
            vec!["D-0001".to_string(), "a".to_string(), "x".to_string()],
            vec!["P-001".to_string(), "longer".to_string(), String::new()],
        ];
        assert_eq!(
            table(&["ID", "S", "T"], &rows),
            "ID      S       T\nD-0001  a       x\nP-001   longer\n"
        );
    }
}
