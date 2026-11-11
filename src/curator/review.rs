//! `vision memory review`: the person accepts, edits, or rejects each change of a proposal.
//!
//! With a terminal, it is an interactive pass over each open proposal: the summary of the
//! Curator, the replay numbers and each card that changed side, the dropped changes, and then
//! each open change with its kind, its reason, its evidence, and a diff of its files. The keys:
//! `a` accepts, `e` opens the file in `$EDITOR` and then accepts the edited text when it passes
//! the lint, `r` rejects (with an optional reason), `s` skips, and `q` leaves.
//!
//! Without a terminal it prints the open proposals and exits; `--json` prints them as JSON.
//! `--accept N|all [--answer TEXT]` and `--reject N|all [--why TEXT]` act without a terminal.
//! An answer to a question that names exactly one decision with an empty section `## Why`
//! becomes that reason, as a commit of the person.
//!
//! An accepted change reaches `main` as one commit of the person; a candidate principle
//! becomes active with the person as `by`. Each verdict is a `human` row, so that the next run
//! of the Curator sees each rejected change in its signals. A proposal with no open change is
//! closed: its branch is deleted.

use std::fs;
use std::io::{self, BufRead, IsTerminal, Read, Write};
use std::process::Command;

use anyhow::{anyhow, bail, Context, Result};
use serde::Serialize;
use serde_json::json;

use super::records::{
    self, Dropped, ProposalRecord, CLOSED, EVENT_CLOSED, EVENT_PROPOSAL, HUMAN_ACCEPT, HUMAN_REJECT,
};
use super::replays::{Changed, Replays};
use crate::cli::{Pick, ReviewArgs};
use crate::ctx::Ctx;
use crate::exit::Stop;
use crate::ledger::questions::HumanRow;
use crate::ledger::Ledger;
use crate::memory::store::Invalid;
use crate::memory::{
    Accepted, ChangeState, DecisionId, FileDiff, Memory, Op, ProposalChange, ProposalView,
};
use crate::util::fs::{atomic_write, ensure_dir};
use crate::util::{git, time};

/// One decision that a change names as evidence.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct EvidenceView {
    pub id: String,
    pub exists: bool,
    pub by: Option<String>,
    pub ticket: Option<String>,
    pub question: Option<String>,
    pub answer: Option<String>,
    pub why: Option<String>,
}

/// One change of a proposal, for the review.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct ChangeOverview {
    pub n: usize,
    pub op: Op,
    pub state: ChangeState,
    pub text: String,
    pub reason: String,
    pub path: Option<String>,
    pub removes: Vec<String>,
    pub question: Option<String>,
    pub note: Option<String>,
    pub commit: Option<String>,
    pub evidence: Vec<EvidenceView>,
    pub diffs: Vec<FileDiff>,
    /// A question that names exactly one decision as evidence, whose section `## Why` is empty:
    /// the answer of the person becomes the reason of that decision.
    pub answer_fills: Option<String>,
}

/// One proposal, for the review.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Overview {
    pub name: String,
    pub created: String,
    pub summary: String,
    pub base: String,
    pub tip: String,
    pub open: usize,
    pub accepted: usize,
    pub rejected: usize,
    /// The Curator turn whose result the proposal is.
    pub turn: Option<String>,
    pub dropped: Vec<Dropped>,
    pub replays: Option<Replays>,
    pub changed: Vec<Changed>,
    /// Lines that the proposal loses. Such a proposal cannot be accepted.
    pub lost: Vec<String>,
    pub changes: Vec<ChangeOverview>,
}

/// One proposal with its view of the store, its record, and the evidence of its changes.
pub fn overview(ledger: &Ledger, memory: &Memory, name: &str) -> Result<Overview> {
    let view = memory.proposal(name)?;
    let record = records::read::<ProposalRecord>(ledger, EVENT_PROPOSAL, name)?;
    Ok(build_overview(memory, &view, record))
}

fn build_overview(
    memory: &Memory,
    view: &ProposalView,
    record: Option<ProposalRecord>,
) -> Overview {
    let changes = view
        .changes
        .iter()
        .map(|c| ChangeOverview {
            n: c.change.n,
            op: c.change.op,
            state: c.change.state,
            text: c.change.describe(),
            reason: c.change.reason.clone(),
            path: c.change.path.clone(),
            removes: c.change.removes.clone(),
            question: c.change.question.clone(),
            note: c.change.note.clone(),
            commit: c.change.commit.clone(),
            evidence: evidence(memory, &c.change),
            diffs: c.diffs.clone(),
            answer_fills: answer_fills(memory, &c.change).map(|d| d.to_string()),
        })
        .collect();
    let replays = record.as_ref().and_then(|r| r.replays.clone());
    Overview {
        name: view.file.name.clone(),
        created: view.file.created.clone(),
        summary: view.file.summary.clone(),
        base: view.base.clone(),
        tip: view.tip.clone(),
        open: view.file.count(ChangeState::Open),
        accepted: view.file.count(ChangeState::Accepted),
        rejected: view.file.count(ChangeState::Rejected),
        turn: record.as_ref().map(|r| r.turn.clone()),
        dropped: record
            .as_ref()
            .map(|r| r.dropped.clone())
            .unwrap_or_default(),
        changed: replays.as_ref().map(Replays::changed).unwrap_or_default(),
        replays,
        lost: view.lost.iter().map(ToString::to_string).collect(),
        changes,
    }
}

/// The decisions that a change names as evidence, read from `main`.
fn evidence(memory: &Memory, change: &ProposalChange) -> Vec<EvidenceView> {
    change
        .evidence
        .iter()
        .map(|id| {
            let found = DecisionId::parse(id.trim())
                .ok()
                .and_then(|d| memory.decision(d).ok());
            match found {
                Some(d) => EvidenceView {
                    id: id.trim().to_string(),
                    exists: true,
                    by: Some(d.by.name().to_string()),
                    ticket: d.ticket.clone(),
                    question: Some(d.question.trim().to_string()),
                    answer: Some(d.answer.trim().to_string()),
                    why: Some(d.why.trim().to_string()).filter(|w| !w.is_empty()),
                },
                None => EvidenceView {
                    id: id.trim().to_string(),
                    exists: false,
                    by: None,
                    ticket: None,
                    question: None,
                    answer: None,
                    why: None,
                },
            }
        })
        .collect()
}

/// The decision whose empty section `## Why` the answer to a question gets: the question
/// names exactly one decision as evidence, and that decision has no reason yet.
pub fn answer_fills(memory: &Memory, change: &ProposalChange) -> Option<DecisionId> {
    if change.op != Op::Question {
        return None;
    }
    let mut decisions: Vec<DecisionId> = Vec::new();
    for id in &change.evidence {
        if let Ok(d) = DecisionId::parse(id.trim()) {
            if !decisions.contains(&d) {
                decisions.push(d);
            }
        }
    }
    let [only] = decisions.as_slice() else {
        return None;
    };
    let decision = memory.decision(*only).ok()?;
    decision.why.trim().is_empty().then_some(*only)
}

/// The proposals with an open change, or the one that `only` names.
fn open_names(memory: &Memory, only: Option<&str>) -> Result<Vec<String>> {
    let all = memory.proposals()?;
    if let Some(name) = only {
        let name = name.trim();
        if !all.iter().any(|p| p.name == name) {
            let open: Vec<&str> = all.iter().map(|p| p.name.as_str()).collect();
            bail!(
                "the memory has no proposal {name}. The proposals are: {}",
                if open.is_empty() {
                    "none".to_string()
                } else {
                    open.join(", ")
                }
            );
        }
        return Ok(vec![name.to_string()]);
    }
    Ok(all
        .into_iter()
        .filter(|p| p.problem.is_none() && p.open > 0)
        .map(|p| p.name)
        .collect())
}

/// Closes each proposal with no open change: its branch is deleted, and its record says so.
/// Gives the closed names.
pub fn close_finished(ctx: &Ctx, ledger: &Ledger, memory: &Memory) -> Result<Vec<String>> {
    let mut closed = Vec::new();
    for info in memory.proposals()? {
        if info.problem.is_none() && info.open == 0 && close(ctx, ledger, memory, &info.name)? {
            closed.push(info.name);
        }
    }
    Ok(closed)
}

/// Closes a proposal when it has no open change. Gives true when it did.
pub fn close(ctx: &Ctx, ledger: &Ledger, memory: &Memory, name: &str) -> Result<bool> {
    let Some(info) = memory.proposals()?.into_iter().find(|p| p.name == name) else {
        return Ok(false);
    };
    if info.open > 0 {
        return Ok(false);
    }
    memory.delete_proposal(name)?;
    if let Some(mut record) = records::read::<ProposalRecord>(ledger, EVENT_PROPOSAL, name)? {
        record.state = CLOSED.into();
        records::put(ledger, "user", EVENT_PROPOSAL, name, &record)?;
    }
    records::add(
        ledger,
        "user",
        EVENT_CLOSED,
        &format!("{name}:{}", time::now_rfc3339()),
        &json!({"proposal": name, "accepted": info.accepted, "rejected": info.rejected}),
    )?;
    ctx.log(&format!(
        "curator: closed {name}: {} accepted, {} rejected",
        info.accepted, info.rejected
    ));
    Ok(true)
}

fn object(name: &str, n: usize) -> String {
    format!("{name} change {n}")
}

/// What the acceptance of one change did.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AcceptedChange {
    pub accepted: Accepted,
    /// The decision whose empty section `## Why` got the answer of the person, as a commit of
    /// the person.
    pub reason_of: Option<DecisionId>,
}

impl AcceptedChange {
    /// The line for people about the reason that the answer wrote, or the empty text.
    pub fn reason_text(&self) -> String {
        match self.reason_of {
            Some(id) => format!(
                " Your answer is now the reason (## Why) of {id}, in a commit of yours in the memory."
            ),
            None => String::new(),
        }
    }
}

/// Accepts one change, and records the verdict. `answer` is the answer of the person to a
/// question of the Curator: the next Curator run reads it, and when the question names exactly
/// one decision with an empty section `## Why`, it becomes that reason first, as a commit of
/// the person. An answer to a change that is not a question is an error.
pub fn accept(
    ctx: &Ctx,
    ledger: &Ledger,
    memory: &Memory,
    name: &str,
    change: &ProposalChange,
    answer: Option<&str>,
    edited: bool,
) -> Result<AcceptedChange> {
    let answer = answer.map(str::trim).filter(|a| !a.is_empty());
    if answer.is_some() && change.op != Op::Question {
        bail!(
            "change {} of {name} is not a question ({}), so it takes no answer. Accept it without --answer",
            change.n,
            change.describe()
        );
    }
    // The reason comes before the verdict: when it cannot be written, the change stays open.
    let mut reason_of = None;
    if let (Some(text), Some(id)) = (answer, answer_fills(memory, change)) {
        if memory
            .fill_decision_why(id, text, memory.person())
            .with_context(|| {
                format!(
                    "could not write your answer as the reason of {id}, so change {} stays open",
                    change.n
                )
            })?
        {
            reason_of = Some(id);
            ctx.log(&format!(
                "curator: the answer to change {} of {name} is the reason of {id}",
                change.n
            ));
        }
    }
    let accepted = memory.accept_change(name, change.n)?;
    ledger.add_human(&HumanRow {
        at: time::now_rfc3339(),
        ticket: None,
        kind: HUMAN_ACCEPT.into(),
        object: object(name, change.n),
        value: Some(
            json!({
                "op": change.op, "path": accepted.path, "commit": accepted.commit,
                "question": change.question, "edited": edited,
                "reason_of": reason_of.map(|d| d.to_string()),
            })
            .to_string(),
        ),
        reason: answer.map(str::to_string),
        seconds: None,
    })?;
    ctx.log(&format!("curator: accepted change {} of {name}", change.n));
    Ok(AcceptedChange {
        accepted,
        reason_of,
    })
}

/// Rejects one change, and records it with the reason of the person for the next Curator run.
pub fn reject(
    ctx: &Ctx,
    ledger: &Ledger,
    memory: &Memory,
    name: &str,
    change: &ProposalChange,
    why: Option<&str>,
) -> Result<()> {
    let why = why.map(str::trim).filter(|w| !w.is_empty());
    memory.reject_change(name, change.n, why)?;
    ledger.add_human(&HumanRow {
        at: time::now_rfc3339(),
        ticket: None,
        kind: HUMAN_REJECT.into(),
        object: object(name, change.n),
        value: Some(
            json!({
                "op": change.op, "path": change.path, "removes": change.removes,
                "question": change.question, "reason": change.reason, "evidence": change.evidence,
            })
            .to_string(),
        ),
        reason: why.map(str::to_string),
        seconds: None,
    })?;
    ctx.log(&format!("curator: rejected change {} of {name}", change.n));
    Ok(())
}

/// Opens the file of a change in the editor, and writes the edited text to the proposal
/// branch. Gives false when the text did not change. An invalid text is an [`Invalid`] error;
/// its draft stays in `.git/vision-edit/`, and the next edit of the change opens it again as
/// long as the change did not change on its branch.
pub fn edit(memory: &Memory, name: &str, change: &ProposalChange, editor: &str) -> Result<bool> {
    let Some(path) = change.path.clone().filter(|p| change.files.contains(p)) else {
        bail!(
            "change {} writes no file, so there is nothing to edit",
            change.n
        );
    };
    let bytes = git::run_bytes(
        memory.dir(),
        &["show", &format!("refs/heads/{name}:{path}")],
    )?;
    let original = String::from_utf8(bytes).map_err(|_| anyhow!("{path} is not UTF-8 text"))?;
    let dir = memory.git_dir().join("vision-edit");
    ensure_dir(&dir)?;
    let stem = format!("{}-{}", name.replace('/', "-"), change.n);
    let file = dir.join(format!("{stem}.md"));
    let base = dir.join(format!("{stem}.base"));
    let draft_fits = fs::read_to_string(&base).is_ok_and(|b| b == original) && file.is_file();
    if !draft_fits {
        atomic_write(&file, original.as_bytes())?;
        atomic_write(&base, original.as_bytes())?;
    }
    let status = Command::new("sh")
        .arg("-c")
        .arg(format!("{editor} \"$1\""))
        .arg("sh")
        .arg(&file)
        .status()
        .with_context(|| format!("could not start the editor `{editor}`"))?;
    if !status.success() {
        bail!(
            "the editor `{editor}` ended with {status}, so change {} did not change",
            change.n
        );
    }
    let edited = fs::read_to_string(&file)
        .with_context(|| format!("could not read the edited text {}", file.display()))?;
    let changed = memory.edit_change(name, change.n, &edited)?;
    let _ = fs::remove_file(&file);
    let _ = fs::remove_file(&base);
    Ok(changed)
}

/// The editor of the person: `EDITOR`, else `VISUAL`.
pub fn editor_from_env() -> Option<String> {
    ["EDITOR", "VISUAL"]
        .iter()
        .filter_map(|name| std::env::var(name).ok())
        .find(|value| !value.trim().is_empty())
}

/// `vision memory review`.
pub fn command(ctx: &Ctx, args: ReviewArgs) -> Result<()> {
    ctx.require_setup()?;
    let ledger = ctx.ledger()?;
    let memory = Memory::from_ctx(ctx)?;
    for name in close_finished(ctx, &ledger, &memory)? {
        let line = format!("Closed {name}: each of its changes was accepted or rejected.");
        if args.json {
            eprintln!("{line}");
        } else {
            println!("{line}");
        }
    }
    if args.accept.is_some() || args.reject.is_some() {
        return act(ctx, &ledger, &memory, &args);
    }
    let interactive = !args.json && io::stdin().is_terminal() && io::stdout().is_terminal();
    if interactive {
        let mut keys = TerminalKeys { stdin: io::stdin() };
        let mut out = io::stdout();
        let editor = editor_from_env();
        return session(
            ctx,
            &ledger,
            &memory,
            &mut keys,
            &mut out,
            editor.as_deref(),
            args.proposal.as_deref(),
        );
    }
    let mut views = Vec::new();
    for name in open_names(&memory, args.proposal.as_deref())? {
        views.push(overview(&ledger, &memory, &name)?);
    }
    if args.json {
        println!("{}", serde_json::to_string_pretty(&views)?);
        return Ok(());
    }
    if views.is_empty() {
        println!("Nothing waits for review.");
        return Ok(());
    }
    let mut out = String::new();
    for view in &views {
        out.push_str(&proposal_text(view));
        for change in view.changes.iter().filter(|c| c.state == ChangeState::Open) {
            out.push_str(&change_text(change, view.changes.len()));
        }
        out.push('\n');
    }
    out.push_str("Run `vision memory review` in a terminal, or use --accept N|all [--answer TEXT] and --reject N|all [--why TEXT].\n");
    print!("{out}");
    Ok(())
}

/// The proposal that `--accept` or `--reject` acts on: the named one, or the only open one.
fn pick_proposal(memory: &Memory, named: Option<&str>) -> Result<String> {
    if let Some(name) = named {
        return Ok(open_names(memory, Some(name))?.remove(0));
    }
    let open = open_names(memory, None)?;
    match open.len() {
        0 => bail!("no proposal waits for review"),
        1 => Ok(open[0].clone()),
        _ => bail!(
            "{} proposals wait for review, so name one with --proposal: {}",
            open.len(),
            open.join(", ")
        ),
    }
}

fn targets(view: &ProposalView, pick: Pick) -> Result<Vec<ProposalChange>> {
    match pick {
        Pick::All => Ok(view
            .file
            .changes
            .iter()
            .filter(|c| c.state == ChangeState::Open)
            .cloned()
            .collect()),
        Pick::Change(n) => view
            .file
            .changes
            .iter()
            .find(|c| c.n == n)
            .cloned()
            .map(|c| vec![c])
            .ok_or_else(|| anyhow!("{} has no change {n}", view.file.name)),
    }
}

/// `--accept N|all` or `--reject N|all [--why TEXT]`.
fn act(ctx: &Ctx, ledger: &Ledger, memory: &Memory, args: &ReviewArgs) -> Result<()> {
    if args.answer.is_some() && args.accept.is_none() {
        bail!("--answer goes with --accept N, the number of a question of the Curator");
    }
    let name = pick_proposal(memory, args.proposal.as_deref())?;
    let view = memory.proposal(&name)?;
    let mut failed = 0;
    if let Some(pick) = args.accept {
        let changes = targets(&view, pick)?;
        if args.answer.is_some() {
            if let Some(other) = changes.iter().find(|c| c.op != Op::Question) {
                bail!(
                    "--answer is for a question of the Curator, and change {} of {name} is not one ({}). Accept it without --answer, or name the question with --accept N",
                    other.n,
                    other.describe()
                );
            }
            // One answer is for one question: the same text for two questions can become the
            // reason of a decision that it does not fit.
            if changes.len() > 1 {
                bail!(
                    "--answer is the answer to one question, and {name} has {} open questions. Name the question with --accept N",
                    changes.len()
                );
            }
        }
        for change in changes {
            match accept(
                ctx,
                ledger,
                memory,
                &name,
                &change,
                args.answer.as_deref(),
                false,
            ) {
                Ok(done) => println!(
                    "Accepted change {}: {}{}.{}",
                    change.n,
                    change.describe(),
                    commit_text(&done.accepted),
                    done.reason_text()
                ),
                Err(e) => {
                    failed += 1;
                    eprintln!("error: change {}: {e:#}", change.n);
                }
            }
        }
    }
    if let Some(pick) = args.reject {
        for change in targets(&view, pick)? {
            match reject(ctx, ledger, memory, &name, &change, args.why.as_deref()) {
                Ok(()) => println!("Rejected change {}: {}.", change.n, change.describe()),
                Err(e) => {
                    failed += 1;
                    eprintln!("error: change {}: {e:#}", change.n);
                }
            }
        }
    }
    if close(ctx, ledger, memory, &name)? {
        println!("Closed {name}: each of its changes was accepted or rejected, so its branch is deleted.");
    } else if let Some(info) = memory.proposals()?.into_iter().find(|p| p.name == name) {
        println!("{name} has {} open change(s).", info.open);
    }
    if failed > 0 {
        return Err(Stop::Reported("memory review".into()).into());
    }
    Ok(())
}

fn commit_text(accepted: &Accepted) -> String {
    match &accepted.commit {
        Some(commit) => format!(" in the commit {}", &commit[..commit.len().min(7)]),
        None => String::new(),
    }
}

fn indent(text: &str, by: &str) -> String {
    text.lines().map(|l| format!("{by}{l}\n")).collect()
}

/// The head of a proposal for people: the summary, the replays, the dropped changes, and the
/// lost lines.
pub fn proposal_text(view: &Overview) -> String {
    let mut out = format!(
        "Proposal {} ({} open, {} accepted, {} rejected), made {}\n",
        view.name, view.open, view.accepted, view.rejected, view.created
    );
    if !view.summary.trim().is_empty() {
        out.push_str(&format!("The Curator says: {}\n", view.summary.trim()));
    }
    match &view.replays {
        Some(replays) if !replays.is_done() => {
            if let Some(line) = replays.running_text() {
                out.push_str(&format!("Replay: {line}.\n"));
            }
        }
        Some(replays) => {
            if let (Some(today), Some(proposed)) = (&replays.today, &replays.proposed) {
                out.push_str(&format!(
                    "Replay of the last {} answers of the person:\n  with the memory of today, the Judge gets {} of {} right ({}).\n  with the proposed memory, it gets {} of {} right ({}).\n",
                    replays.cards,
                    today.agree,
                    today.items.len(),
                    today.line(),
                    proposed.agree,
                    proposed.items.len(),
                    proposed.line()
                ));
                if view.changed.is_empty() {
                    out.push_str("  No card changed side.\n");
                } else {
                    out.push_str("  Cards that changed side:\n");
                    for c in &view.changed {
                        out.push_str(&format!(
                            "    {}: {}\n      the person: {}\n      today: {} ({})\n      proposed: {} ({})\n",
                            c.id,
                            c.question,
                            c.person,
                            c.today.as_deref().unwrap_or("-"),
                            c.today_side.text(),
                            c.proposed.as_deref().unwrap_or("-"),
                            c.proposed_side.text()
                        ));
                    }
                }
            }
            for note in &replays.notes {
                out.push_str(&format!("Replay: {note}.\n"));
            }
        }
        None => out.push_str("Replay: none is stored for this proposal.\n"),
    }
    if !view.dropped.is_empty() {
        out.push_str(&format!("Dropped {} change(s):\n", view.dropped.len()));
        for d in &view.dropped {
            out.push_str(&format!("  {}\n", d.line()));
        }
    }
    if !view.lost.is_empty() {
        out.push_str("It loses these lines, so no change of it can be accepted. Reject it:\n");
        for line in &view.lost {
            out.push_str(&format!("  {line}\n"));
        }
    }
    out
}

/// One change for people: its kind, its reason, its evidence, and a diff of its files.
pub fn change_text(change: &ChangeOverview, total: usize) -> String {
    let mut out = format!("\nChange {} of {total}: {}\n", change.n, change.text);
    out.push_str(&format!("  Reason: {}\n", change.reason.trim()));
    if let Some(note) = &change.note {
        out.push_str(&format!("  Note: {note}\n"));
    }
    if let Some(id) = &change.answer_fills {
        out.push_str(&format!(
            "  {id} has no reason yet, so your answer becomes its section ## Why.\n"
        ));
    }
    if !change.evidence.is_empty() {
        out.push_str("  Evidence:\n");
        for e in &change.evidence {
            if e.exists {
                out.push_str(&format!(
                    "    {} ({}, {}): {}\n      answer: {}{}\n",
                    e.id,
                    e.ticket.as_deref().unwrap_or("no ticket"),
                    e.by.as_deref().unwrap_or("?"),
                    e.question.as_deref().unwrap_or(""),
                    e.answer.as_deref().unwrap_or(""),
                    e.why
                        .as_deref()
                        .map(|w| format!("\n      why: {w}"))
                        .unwrap_or_default()
                ));
            } else {
                out.push_str(&format!("    {}: not in the memory\n", e.id));
            }
        }
    }
    for diff in &change.diffs {
        out.push_str(&format!("  Diff of {}:\n", diff.path));
        let body: Vec<&str> = diff
            .diff
            .lines()
            .skip_while(|l| !l.starts_with("@@"))
            .collect();
        out.push_str(&indent(&body.join("\n"), "    "));
    }
    out
}

/// One key of the review.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Key {
    Enter,
    Char(char),
    /// Ctrl-C, Ctrl-D, or the end of the input.
    Quit,
}

/// Where the review reads keys and lines.
pub trait Keys {
    fn key(&mut self) -> Result<Key>;
    /// One line of text, without its newline. `None` at the end of the input.
    fn line(&mut self) -> Result<Option<String>>;
}

/// Reads each key as one line: an empty line is Enter, else its first character.
pub struct LineKeys<R: BufRead> {
    input: R,
}

impl<R: BufRead> LineKeys<R> {
    pub fn new(input: R) -> LineKeys<R> {
        LineKeys { input }
    }
}

impl<R: BufRead> Keys for LineKeys<R> {
    fn key(&mut self) -> Result<Key> {
        match self.line()? {
            None => Ok(Key::Quit),
            Some(line) => Ok(match line.trim().chars().next() {
                None => Key::Enter,
                Some(c) => Key::Char(c.to_ascii_lowercase()),
            }),
        }
    }

    fn line(&mut self) -> Result<Option<String>> {
        let mut line = String::new();
        if self.input.read_line(&mut line)? == 0 {
            return Ok(None);
        }
        Ok(Some(line.trim_end_matches(['\n', '\r']).to_string()))
    }
}

/// The keys of the terminal: one key at a time in raw mode, and lines in the normal mode.
struct TerminalKeys {
    stdin: io::Stdin,
}

/// The raw mode of the terminal while it lives. Drop restores the old mode.
struct RawMode {
    old: libc::termios,
}

impl RawMode {
    fn enter() -> Option<RawMode> {
        // SAFETY: tcgetattr and tcsetattr read and write a termios struct of standard input;
        // the struct is plain data, and a failure only means that there is no raw mode.
        unsafe {
            let mut old: libc::termios = std::mem::zeroed();
            if libc::tcgetattr(libc::STDIN_FILENO, &mut old) != 0 {
                return None;
            }
            let mut raw = old;
            raw.c_lflag &= !(libc::ICANON | libc::ECHO | libc::ISIG);
            raw.c_cc[libc::VMIN] = 1;
            raw.c_cc[libc::VTIME] = 0;
            if libc::tcsetattr(libc::STDIN_FILENO, libc::TCSANOW, &raw) != 0 {
                return None;
            }
            Some(RawMode { old })
        }
    }
}

impl Drop for RawMode {
    fn drop(&mut self) {
        // SAFETY: restores the struct that tcgetattr gave.
        unsafe {
            libc::tcsetattr(libc::STDIN_FILENO, libc::TCSANOW, &self.old);
        }
    }
}

impl Keys for TerminalKeys {
    fn key(&mut self) -> Result<Key> {
        let Some(_raw) = RawMode::enter() else {
            return LineKeys::new(self.stdin.lock()).key();
        };
        let mut lock = self.stdin.lock();
        loop {
            let mut byte = [0u8; 1];
            if lock.read(&mut byte)? == 0 {
                return Ok(Key::Quit);
            }
            match byte[0] {
                b'\r' | b'\n' => return Ok(Key::Enter),
                3 | 4 => return Ok(Key::Quit),
                b if b.is_ascii_alphabetic() => {
                    return Ok(Key::Char(char::from(b.to_ascii_lowercase())))
                }
                _ => continue,
            }
        }
    }

    fn line(&mut self) -> Result<Option<String>> {
        LineKeys::new(self.stdin.lock()).line()
    }
}

/// The interactive review, with lines as keys, for a caller with no terminal such as a test.
pub fn review_lines(
    ctx: &Ctx,
    input: &mut dyn BufRead,
    out: &mut dyn Write,
    editor: Option<&str>,
    only: Option<&str>,
) -> Result<()> {
    let ledger = ctx.ledger()?;
    let memory = Memory::from_ctx(ctx)?;
    close_finished(ctx, &ledger, &memory)?;
    let mut keys = LineKeys::new(input);
    session(ctx, &ledger, &memory, &mut keys, out, editor, only)
}

/// What the person chose for one change.
enum Choice {
    Next,
    Quit,
}

/// The interactive pass over each open proposal.
pub fn session(
    ctx: &Ctx,
    ledger: &Ledger,
    memory: &Memory,
    keys: &mut dyn Keys,
    out: &mut dyn Write,
    editor: Option<&str>,
    only: Option<&str>,
) -> Result<()> {
    let names = open_names(memory, only)?;
    if names.is_empty() {
        writeln!(out, "Nothing waits for review.")?;
        return Ok(());
    }
    'proposals: for name in names {
        let view = overview(ledger, memory, &name)?;
        write!(out, "\n{}", proposal_text(&view))?;
        let total = view.changes.len();
        let open: Vec<usize> = view
            .changes
            .iter()
            .filter(|c| c.state == ChangeState::Open)
            .map(|c| c.n)
            .collect();
        for n in open {
            // Read the change again: an earlier accept can give a file a new id.
            let fresh = memory.proposal(&name)?;
            let Some(change) = fresh.file.changes.iter().find(|c| c.n == n).cloned() else {
                continue;
            };
            if change.state != ChangeState::Open {
                continue;
            }
            let shown = view.changes.iter().find(|c| c.n == n);
            if let Some(shown) = shown {
                write!(out, "{}", change_text(shown, total))?;
            }
            match one_change(ctx, ledger, memory, keys, out, editor, &name, &change)? {
                Choice::Next => {}
                Choice::Quit => {
                    writeln!(out, "Left the review. The other changes stay open.")?;
                    close(ctx, ledger, memory, &name)?;
                    break 'proposals;
                }
            }
        }
        if close(ctx, ledger, memory, &name)? {
            writeln!(
                out,
                "Closed {name}: each of its changes was accepted or rejected, so its branch is deleted."
            )?;
        }
    }
    out.flush()?;
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn one_change(
    ctx: &Ctx,
    ledger: &Ledger,
    memory: &Memory,
    keys: &mut dyn Keys,
    out: &mut dyn Write,
    editor: Option<&str>,
    name: &str,
    change: &ProposalChange,
) -> Result<Choice> {
    let can_edit = change
        .path
        .as_ref()
        .is_some_and(|p| change.files.contains(p));
    loop {
        let edit_key = if can_edit { ", e edit then accept" } else { "" };
        write!(out, "a accept{edit_key}, r reject, s skip, q leave: ")?;
        out.flush()?;
        let key = keys.key()?;
        writeln!(out)?;
        match key {
            Key::Char('a') => {
                let answer = if change.op == Op::Question {
                    match answer_fills(memory, change) {
                        Some(id) => write!(
                            out,
                            "Your answer. It becomes the reason of {id}, and the next Curator run reads it (Enter for none): "
                        )?,
                        None => write!(
                            out,
                            "Your answer, for the next Curator run (Enter for none): "
                        )?,
                    }
                    out.flush()?;
                    keys.line()?
                } else {
                    None
                };
                match accept(ctx, ledger, memory, name, change, answer.as_deref(), false) {
                    Ok(done) => {
                        writeln!(
                            out,
                            "Accepted change {}{}.{}",
                            change.n,
                            commit_text(&done.accepted),
                            done.reason_text()
                        )?;
                        return Ok(Choice::Next);
                    }
                    Err(e) => writeln!(out, "Could not accept it: {e:#}")?,
                }
            }
            Key::Char('e') if can_edit => {
                let Some(editor) = editor else {
                    writeln!(
                        out,
                        "Set EDITOR to your editor, such as `export EDITOR=vi`, to edit a change."
                    )?;
                    continue;
                };
                match edit(memory, name, change, editor) {
                    Ok(_) => match accept(ctx, ledger, memory, name, change, None, true) {
                        Ok(done) => {
                            writeln!(
                                out,
                                "Accepted your text of change {}{}.",
                                change.n,
                                commit_text(&done.accepted)
                            )?;
                            return Ok(Choice::Next);
                        }
                        Err(e) => writeln!(out, "Could not accept it: {e:#}")?,
                    },
                    Err(e) => {
                        if let Some(invalid) = e.downcast_ref::<Invalid>() {
                            writeln!(out, "Your text is not valid, so the change stays open:")?;
                            for problem in &invalid.problems {
                                writeln!(out, "  {problem}")?;
                            }
                        } else {
                            writeln!(out, "Could not edit it: {e:#}")?;
                        }
                    }
                }
            }
            Key::Char('r') => {
                write!(out, "Why, for the next Curator run (Enter for no reason): ")?;
                out.flush()?;
                let why = keys.line()?;
                match reject(ctx, ledger, memory, name, change, why.as_deref()) {
                    Ok(()) => {
                        writeln!(out, "Rejected change {}.", change.n)?;
                        return Ok(Choice::Next);
                    }
                    Err(e) => writeln!(out, "Could not reject it: {e:#}")?,
                }
            }
            Key::Char('s') => return Ok(Choice::Next),
            Key::Char('q') | Key::Quit => return Ok(Choice::Quit),
            _ => {}
        }
    }
}
