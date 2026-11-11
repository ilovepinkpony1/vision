//! Operations (section 12): the mode of a verb, the idempotency key, the gates in front of a
//! verb with `leaves`, the call of `act`, the approval of the person, the reconcile, and the
//! trust ladder.
//!
//! Each operation gets its row in the state `pending` before anything runs. The same key never
//! runs a second time: a later request gets the state of the first operation. A `pending` row
//! never ran, so a later request looks at the mode again, and runs it when the mode allows.
//! The callers must hold the lock of the tick, so that a reconcile never meets a running call.

use std::fmt;
use std::time::Duration;

use anyhow::{anyhow, bail, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};

use super::clean::{self, object_args, Document, DETAIL_MAX};
use super::context::{commits_between, diff, history, short, TicketContext};
use super::gates::{self, FileDiff, Gate, Text};
use super::jobs::write_documents;
use super::scripts::{Answer, Runner, Script, Secrets, DEFAULT_TIMEOUT};
use crate::adapter::{self, Adapter, Verb};
use crate::config::{Config, ConfigEditor, Mode};
use crate::ctx::Ctx;
use crate::ledger::operations::{
    NewOperation, OperationRow, OutcomeRow, STATE_CONFIRMED, STATE_EXECUTING, STATE_FAILED,
    STATE_PENDING, STATE_UNCERTAIN,
};
use crate::ledger::questions::HumanRow;
use crate::ledger::Ledger;
use crate::paths::TicketKey;
use crate::snapshot::{Need, NeedKind};
use crate::tick::OperationRun;
use crate::util::fs::read_optional;
use crate::util::{ids, time};

/// The trust ladder, from the bottom.
pub const LADDER: [Mode; 4] = [Mode::Hold, Mode::Shadow, Mode::Ask, Mode::Auto];

/// The document of the ticket folder that names the sensitive paths that the person approved.
pub const APPROVED_PLAN: &str = "approved-plan.md";

/// The states of an operation.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum OpState {
    Pending,
    Executing,
    Confirmed,
    Failed,
    Uncertain,
}

impl OpState {
    pub fn as_str(self) -> &'static str {
        match self {
            OpState::Pending => STATE_PENDING,
            OpState::Executing => STATE_EXECUTING,
            OpState::Confirmed => STATE_CONFIRMED,
            OpState::Failed => STATE_FAILED,
            OpState::Uncertain => STATE_UNCERTAIN,
        }
    }

    fn parse(text: &str) -> Option<OpState> {
        [
            OpState::Pending,
            OpState::Executing,
            OpState::Confirmed,
            OpState::Failed,
            OpState::Uncertain,
        ]
        .into_iter()
        .find(|s| s.as_str() == text)
    }

    /// True for `confirmed` and `failed`. An `uncertain` operation is reconciled at each tick.
    pub fn is_final(self) -> bool {
        matches!(self, OpState::Confirmed | OpState::Failed)
    }
}

/// What an operation means for its caller.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum Outcome {
    /// The verb ran, and the outside confirms it.
    Confirmed,
    /// It waits for the person: the mode `ask`, or a diff that touches a sensitive path that the
    /// approved plan does not name. `command` is the exact line to type.
    Waiting { command: String },
    /// The mode `hold`: nothing was called.
    Held,
    /// The mode `shadow`: the intent was recorded, and nothing was called.
    Shadowed,
    /// A gate refused it: `secrets`, `size`, or `script` (exit code 2 of `bin/act`).
    Refused { gate: String },
    /// `act` answered that it failed, or the operation went stale.
    Failed,
    /// The outside state is not known. Each tick reconciles it. It never runs again by itself.
    Uncertain,
    /// `act` was called and no answer was stored, such as after a crash. The next tick
    /// reconciles it.
    Executing,
    /// Recorded and not run yet. The next request runs it when the mode allows.
    Pending,
}

impl Outcome {
    /// True when the pipeline sets the flag `needs-you` for it.
    pub fn needs_you(&self) -> bool {
        !matches!(
            self,
            Outcome::Confirmed | Outcome::Executing | Outcome::Pending
        )
    }

    pub fn name(&self) -> &'static str {
        match self {
            Outcome::Confirmed => "confirmed",
            Outcome::Waiting { .. } => "waiting",
            Outcome::Held => "held",
            Outcome::Shadowed => "shadowed",
            Outcome::Refused { .. } => "refused",
            Outcome::Failed => "failed",
            Outcome::Uncertain => "uncertain",
            Outcome::Executing => "executing",
            Outcome::Pending => "pending",
        }
    }
}

impl fmt::Display for Outcome {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Outcome::Refused { gate } => write!(f, "refused by the {gate} gate"),
            other => f.write_str(other.name()),
        }
    }
}

/// The values for the ticket row that a confirmed operation gives (section 11.1).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SetValues {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pr: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub head: Option<String>,
}

impl SetValues {
    pub fn is_empty(&self) -> bool {
        self.pr.is_none() && self.head.is_none()
    }
}

/// One operation as the caller reads it.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct OperationResult {
    pub id: String,
    pub ticket: String,
    pub verb: String,
    /// The last commit that the operation acts on. Empty for a ticket with no commit.
    pub head: String,
    pub args: Value,
    /// The mode of the verb when the executor last looked at the operation.
    pub mode: String,
    pub state: OpState,
    pub outcome: Outcome,
    /// What happened, for people. Each secret is masked.
    pub detail: String,
    pub external_id: Option<String>,
    pub set: SetValues,
    /// The documents of the confirmed answer that the executor wrote to the ticket folder.
    pub documents: Vec<String>,
    /// The sensitive paths that wait for the approval of the person.
    pub held: Vec<String>,
    /// For the mode shadow: what the verb would do.
    pub intent: Option<Value>,
    pub source: Option<String>,
    pub turn: Option<String>,
    pub started: Option<String>,
    pub ended: Option<String>,
}

/// What the `result` column holds.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
struct Stored {
    #[serde(default, skip_serializing_if = "String::is_empty")]
    detail: String,
    #[serde(default, skip_serializing_if = "SetValues::is_empty")]
    set: SetValues,
    /// The documents of the confirmed answer, written to the ticket folder.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    documents: Vec<String>,
    /// The gate that refused the operation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    gate: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    held: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    intent: Option<Value>,
    /// When the operation was recorded.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    asked: Option<String>,
    /// When the person approved it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    approved: Option<String>,
}

/// What the `args` column holds: the call of the operation.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
struct CallRecord {
    #[serde(default)]
    args: Value,
    #[serde(default)]
    head: String,
    #[serde(default)]
    branch: String,
    #[serde(default)]
    clone: Option<String>,
}

/// The idempotency key of an operation: a hash of the ticket, the verb, the last commit, and
/// the arguments, with the keys of each object in order.
pub fn idempotency_key(ticket: &str, verb: &str, head: &str, args: &Value) -> String {
    let text = Value::Array(vec![
        Value::from(ticket),
        Value::from(verb),
        Value::from(head),
        canonical(args),
    ])
    .to_string();
    ids::sha256_hex(text.as_bytes())
}

/// The idempotency key of a verb that runs one time for a ticket (`once = true`): a hash of the
/// ticket, the verb, and the arguments. It has no commit, so a later commit asks for the same
/// operation.
pub fn once_key(ticket: &str, verb: &str, args: &Value) -> String {
    let text = Value::Array(vec![
        Value::from(ticket),
        Value::from(verb),
        Value::Null,
        canonical(args),
    ])
    .to_string();
    ids::sha256_hex(text.as_bytes())
}

/// The key of an operation of `verb` at `head`.
fn key_of(ticket: &str, verb: &Verb, head: &str, args: &Value) -> String {
    if verb.once {
        once_key(ticket, &verb.name, args)
    } else {
        idempotency_key(ticket, &verb.name, head, args)
    }
}

/// A short hash of the arguments of a call, with the keys of each object in order. Empty
/// arguments (`{}` or `null`) give the empty text.
pub fn args_hash(args: &Value) -> String {
    match args {
        Value::Null => String::new(),
        Value::Object(map) if map.is_empty() => String::new(),
        other => ids::sha256_hex(canonical(other).to_string().as_bytes())[..16].to_string(),
    }
}

fn canonical(value: &Value) -> Value {
    match value {
        Value::Object(map) => {
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort();
            let mut out = Map::new();
            for key in keys {
                out.insert(key.clone(), canonical(&map[key]));
            }
            Value::Object(out)
        }
        Value::Array(items) => Value::Array(items.iter().map(canonical).collect()),
        other => other.clone(),
    }
}

/// The mode of a verb: `[modes]` of the configuration wins over the `mode` of the adapter.
/// An unknown verb and a read verb are errors.
pub fn mode_of(config: &Config, adapter: &Adapter, verb: &str) -> Result<Mode> {
    let spec = adapter.verb(verb)?;
    if spec.read {
        bail!("`{verb}` is a read verb: it changes nothing outside, so it has no mode");
    }
    Ok(config.modes.get(verb).copied().unwrap_or(spec.mode))
}

/// The next mode up the ladder, or `None` at the top.
pub fn promoted(mode: Mode) -> Option<Mode> {
    let index = LADDER.iter().position(|m| *m == mode)?;
    LADDER.get(index + 1).copied()
}

/// The next mode down the ladder, or `None` at the bottom.
pub fn demoted(mode: Mode) -> Option<Mode> {
    let index = LADDER.iter().position(|m| *m == mode)?;
    index.checked_sub(1).map(|i| LADDER[i])
}

/// The commit as the person types it: the full commit, or `-` for none.
pub fn commit_arg(head: &str) -> &str {
    if head.is_empty() {
        "-"
    } else {
        head
    }
}

/// The exact line that approves an operation.
pub fn approve_command(ticket: &str, verb: &str, head: &str) -> String {
    format!("vision approve {ticket} {verb} {}", commit_arg(head))
}

/// Records the operation as `pending` with its idempotency key, tests the gates when the verb
/// has `leaves`, and runs it when the mode allows. An operation with the same key that is no
/// longer pending is not run again: the caller gets its state.
pub fn request(ctx: &Ctx, ledger: &Ledger, op: &OperationRun) -> Result<OperationResult> {
    let config = ctx.config()?;
    let adapter = adapter::load_with(config, &ctx.paths)?;
    let verb = acting_verb(&adapter, &op.verb)?.clone();
    let args = object_args(&op.args).with_context(|| format!("the operation {}", op.verb))?;
    let ticket = TicketContext::load(ledger, &adapter, &op.ticket)?;
    ticket.require_row()?;
    let head = ticket.head_text();
    let idem = key_of(op.ticket.as_str(), &verb, &head, &args);
    if let Some(row) = ledger.operation_by_idem(&idem)? {
        if row.state != STATE_PENDING {
            return result_of(&row);
        }
    }
    let secrets = Secrets::read(config)?;
    let mode = mode_of(config, &adapter, &op.verb)?;
    let mut call = CallRecord {
        args,
        head,
        branch: ticket.branch.clone(),
        clone: ticket.clone_text(),
    };
    let row = record(ledger, op, &idem, &call, mode, &secrets)?;
    if row.state != STATE_PENDING {
        return result_of(&row);
    }
    if verb.once {
        // The operation of the first request stays: a newer commit does not make it stale, and
        // its approval command keeps its commit. A verb with `once` has no `leaves` (the loader
        // refuses that), so no gate reads a diff here.
        call.head = call_of(&row).head;
    } else {
        supersede(ctx, ledger, &row, &call.head)?;
    }
    // A pending operation is judged again from the start: the mode, the plan, or the secrets
    // may have changed since the last request.
    let mut stored = stored_of(&row);
    stored.held.clear();
    stored.intent = None;
    stored.gate = None;
    if let Some(name) = secrets.found_in(&call.args) {
        let reason = format!(
            "the arguments of {} hold the value of the secret {name}, so nothing was called. Remove it from the text",
            op.verb
        );
        return refuse(ctx, ledger, &row, stored, Gate::Secrets.as_str(), reason);
    }
    let command = approve_command(op.ticket.as_str(), &op.verb, &call.head);
    match mode {
        Mode::Hold => {
            stored.detail = format!(
                "{} is in the mode hold, so the executor called nothing. Run `vision promote {}` to let it run",
                op.verb, op.verb
            );
            pending(ctx, ledger, &row, mode, &stored)
        }
        Mode::Shadow => {
            let gates = if verb.leaves {
                match test_gates(ctx, config, &adapter, &ticket, &call, &secrets, false) {
                    Ok(verdict) => verdict.describe(),
                    Err(e) => format!("could not test the gates: {e:#}"),
                }
            } else {
                "no gate: nothing leaves the machine".to_string()
            };
            let mut intent = json!({
                "verb": op.verb, "key": op.ticket.as_str(), "branch": call.branch,
                "head": call.head, "args": call.args, "gates": gates,
            });
            secrets.mask_json(&mut intent);
            stored.intent = Some(intent);
            stored.detail = format!(
                "{} is in the mode shadow: the executor recorded what it would do, and called nothing. Run `vision promote {}` to ask before it runs",
                op.verb, op.verb
            );
            pending(ctx, ledger, &row, mode, &stored)
        }
        Mode::Ask | Mode::Auto => {
            if verb.leaves {
                match test_gates(ctx, config, &adapter, &ticket, &call, &secrets, false)? {
                    Verdict::Refuse { gate, reason } => {
                        return refuse(ctx, ledger, &row, stored, gate.as_str(), reason)
                    }
                    Verdict::Hold(paths) => {
                        stored.detail = format!(
                            "the diff touches {}, which the approved plan does not name. Run `{command}` to let it run, or name the path in the plan",
                            paths.join(", ")
                        );
                        stored.held = paths;
                        return pending(ctx, ledger, &row, mode, &stored);
                    }
                    Verdict::Pass => {}
                }
            }
            if mode == Mode::Ask {
                stored.detail = format!("{} waits for your approval: run `{command}`", op.verb);
                return pending(ctx, ledger, &row, mode, &stored);
            }
            execute(
                ctx, ledger, config, &adapter, &verb, &row, &call, &secrets, mode, stored,
            )
        }
    }
}

/// The stored operation of this request at the last commit of the ticket, if any. It runs
/// nothing.
pub fn lookup(ctx: &Ctx, ledger: &Ledger, op: &OperationRun) -> Result<Option<OperationResult>> {
    let adapter = adapter::load(ctx)?;
    let verb = adapter.verb(&op.verb)?.clone();
    let args = object_args(&op.args)?;
    let ticket = TicketContext::load(ledger, &adapter, &op.ticket)?;
    let idem = key_of(op.ticket.as_str(), &verb, &ticket.head_text(), &args);
    ledger
        .operation_by_idem(&idem)?
        .map(|row| result_of(&row))
        .transpose()
}

/// One operation by its id.
pub fn operation(ledger: &Ledger, id: &str) -> Result<Option<OperationResult>> {
    ledger.operation(id)?.map(|row| result_of(&row)).transpose()
}

/// The operations of a ticket, oldest first.
pub fn operations_of(ledger: &Ledger, ticket: &TicketKey) -> Result<Vec<OperationResult>> {
    ledger
        .operations_of(ticket.as_str(), None)?
        .iter()
        .map(result_of)
        .collect()
}

/// `vision approve KEY VERB COMMIT`: runs each operation of the verb that waits for the person
/// at that commit, if the ticket is still at that commit. The secrets and the size gates run
/// again; the approval covers the sensitive paths. A stale commit is an error that tells what
/// changed. The caller holds the lock of the tick.
pub fn approve(
    ctx: &Ctx,
    ledger: &Ledger,
    key: &TicketKey,
    verb_name: &str,
    commit: &str,
) -> Result<Vec<OperationResult>> {
    let config = ctx.config()?;
    let adapter = adapter::load_with(config, &ctx.paths)?;
    let verb = adapter.verb(verb_name)?.clone();
    if verb.read {
        bail!("{verb_name} is a read verb: it changes nothing outside, so it needs no approval");
    }
    let mode = mode_of(config, &adapter, verb_name)?;
    if matches!(mode, Mode::Hold | Mode::Shadow) {
        bail!(
            "{verb_name} is in the mode {mode} now, so the executor calls nothing for it. Run `vision promote {verb_name}` first, then approve again"
        );
    }
    let waiting: Vec<OperationRow> = ledger
        .operations_of(key.as_str(), Some(verb_name))?
        .into_iter()
        .filter(|row| row.state == STATE_PENDING && waits_for_person(row))
        .collect();
    if waiting.is_empty() {
        bail!(
            "nothing of {verb_name} waits for your approval on {key}. `vision needs` lists what waits"
        );
    }
    let matching: Vec<&OperationRow> = waiting
        .iter()
        .filter(|row| commit_matches(&call_of(row).head, commit))
        .collect();
    if matching.is_empty() {
        let heads: Vec<String> = waiting
            .iter()
            .map(|row| commit_arg(&call_of(row).head).to_string())
            .collect();
        bail!(
            "the commit {commit} does not match: {verb_name} of {key} waits at {}. Run `vision needs` for the exact command",
            heads.join(", ")
        );
    }
    let ticket = TicketContext::load(ledger, &adapter, key)?;
    let now_head = ticket.head_text();
    let asked_head = call_of(matching[0]).head;
    // A verb that runs one time for a ticket has no commit in its key: a newer commit leaves
    // it as it is. It has no `leaves`, so no gate reads a diff.
    if asked_head != now_head && !verb.once {
        let mut change = String::new();
        if let Some(clone) = &ticket.clone {
            let commits = commits_between(clone, &asked_head, &now_head);
            if !commits.is_empty() {
                change = format!(" ({} new commit(s): {})", commits.len(), commits.join("; "));
            }
        }
        for row in &matching {
            let mut stored = stored_of(row);
            stored.detail = format!(
                "stale: {key} moved from {} to {} before the approval, so this operation never runs",
                short(&asked_head),
                short(&now_head)
            );
            end(ledger, row, OpState::Failed, None, &stored)?;
        }
        bail!(
            "the commit is stale: {verb_name} of {key} was asked at {}, but {key} is now at {}{change}. The next tick asks again at the new commit; approve that one",
            short(&asked_head),
            short(&now_head)
        );
    }
    let secrets = Secrets::read(config)?;
    let mut results = Vec::new();
    for row in matching {
        let at = time::now_rfc3339();
        // The time from the request of the operation to the approval of the person.
        let seconds = stored_of(row)
            .asked
            .as_deref()
            .and_then(|asked| time::parse_rfc3339(asked).ok())
            .map(|asked| time::seconds_between(asked, time::now()).max(0));
        ledger.add_human(&HumanRow {
            at: at.clone(),
            ticket: Some(key.to_string()),
            kind: "approve".into(),
            object: row.id.clone(),
            value: Some(format!("{verb_name} {}", commit_arg(&asked_head))),
            reason: None,
            seconds,
        })?;
        let call = call_of(row);
        let mut stored = stored_of(row);
        stored.approved = Some(at);
        if let Some(name) = secrets.found_in(&call.args) {
            let reason =
                format!("the arguments of {verb_name} hold the value of the secret {name}");
            results.push(refuse(
                ctx,
                ledger,
                row,
                stored,
                Gate::Secrets.as_str(),
                reason,
            )?);
            continue;
        }
        if verb.leaves {
            if let Verdict::Refuse { gate, reason } =
                test_gates(ctx, config, &adapter, &ticket, &call, &secrets, true)?
            {
                results.push(refuse(ctx, ledger, row, stored, gate.as_str(), reason)?);
                continue;
            }
        }
        results.push(execute(
            ctx, ledger, config, &adapter, &verb, row, &call, &secrets, mode, stored,
        )?);
    }
    Ok(results)
}

/// Tick step 3: asks `act` with `reconcile` set to true about each operation in the state
/// `executing` or `uncertain`, and stores the answer. A call that fails makes the operation
/// `uncertain`; an error before the call, such as a secret that cannot be read, leaves it as it
/// was and is logged. Only a broken adapter is an error of the whole step.
pub fn reconcile(ctx: &Ctx, ledger: &Ledger) -> Result<Vec<OperationResult>> {
    let rows = ledger.operations_in_states(&[STATE_EXECUTING, STATE_UNCERTAIN])?;
    if rows.is_empty() {
        return Ok(Vec::new());
    }
    let config = ctx.config()?;
    let adapter = adapter::load_with(config, &ctx.paths)?;
    let mut results = Vec::new();
    for row in rows {
        match reconcile_one(ctx, ledger, config, &adapter, &row) {
            Ok(result) => results.push(result),
            Err(e) => ctx.log(&format!(
                "operation {}: the reconcile of {} of {} failed: {e:#}",
                row.id, row.verb, row.ticket
            )),
        }
    }
    Ok(results)
}

fn reconcile_one(
    ctx: &Ctx,
    ledger: &Ledger,
    config: &Config,
    adapter: &Adapter,
    row: &OperationRow,
) -> Result<OperationResult> {
    let call = call_of(row);
    let mut stored = stored_of(row);
    let (state, external_id) = match adapter.verb(&row.verb) {
        Err(e) => {
            stored.detail = format!("reconcile: {e:#}. The operation stays uncertain");
            (OpState::Uncertain, None)
        }
        Ok(verb) => {
            let secrets = Secrets::read(config)?;
            let runner = Runner::new(adapter, config, &secrets);
            let pr = ledger.ticket(&row.ticket)?.and_then(|r| r.pr);
            let input = act_input(row, &call, pr, true);
            let record = crate::running::start(&ctx.paths, "reconcile", &row.verb, &row.ticket);
            let answer = runner.act(input, timeout_of(verb));
            drop(record);
            match answer {
                Ok(Answer::Done(map)) => {
                    let answer = act_answer(&map);
                    stored.detail = format!("reconcile: {}", answer.detail);
                    stored.set = answer.set;
                    if answer.state == OpState::Confirmed {
                        stored.documents = act_documents(ctx, row, &answer.documents, &secrets)?;
                    }
                    (answer.state, answer.external_id)
                }
                Ok(Answer::Refused(map)) => {
                    stored.detail = format!(
                        "reconcile: bin/act refused to answer: {}. The operation stays uncertain",
                        clean::text(map.get("detail"), DETAIL_MAX)
                    );
                    (OpState::Uncertain, None)
                }
                Err(e) => {
                    stored.detail =
                        format!("reconcile: {e}. The operation stays uncertain, and the next tick asks again");
                    (OpState::Uncertain, None)
                }
            }
        }
    };
    ledger.add_outcome(&OutcomeRow {
        op: row.id.clone(),
        kind: "reconcile".into(),
        check_at: None,
        checked_at: Some(time::now_rfc3339()),
        state: Some(state.as_str().into()),
        detail: Some(stored.detail.clone()),
    })?;
    finish(ctx, ledger, row, state, external_id, stored)
}

/// `vision promote VERB` (`up` is true) and `vision demote VERB`: moves the verb one step on the
/// ladder hold, shadow, ask, auto, writes `[modes]` of the configuration, and records a row in
/// `human`. Gives the old and the new mode.
pub fn change_mode(ctx: &Ctx, ledger: &Ledger, verb: &str, up: bool) -> Result<(Mode, Mode)> {
    let config = ctx.config()?;
    let adapter = adapter::load_with(config, &ctx.paths)?;
    let old = mode_of(config, &adapter, verb)?;
    let new = if up { promoted(old) } else { demoted(old) };
    let Some(new) = new else {
        let (end, word) = if up {
            ("top", "auto")
        } else {
            ("bottom", "hold")
        };
        bail!("{verb} is already {word}, the {end} of the ladder hold, shadow, ask, auto");
    };
    let mut editor = ConfigEditor::open(&ctx.paths.config())?;
    editor.set_mode(verb, new)?;
    editor.save()?;
    ledger.add_human(&HumanRow {
        at: time::now_rfc3339(),
        ticket: None,
        kind: if up { "promote" } else { "demote" }.into(),
        object: verb.to_string(),
        value: Some(new.as_str().into()),
        reason: Some(format!("from {old}")),
        seconds: None,
    })?;
    ctx.log(&format!(
        "{}: {verb} moved from {old} to {new}",
        if up { "promote" } else { "demote" }
    ));
    Ok((old, new))
}

/// Frees the idempotency key of each `failed` operation of a ticket, so that the same verb at
/// the same commit with the same arguments can run again after `vision retry`. The row stays
/// as the record: its `idem` gets the suffix `:<id>`, so that the column stays unique. A row in
/// any other state is never touched. Gives the number of freed rows.
pub fn release_failed(ledger: &Ledger, key: &TicketKey) -> Result<usize> {
    Ok(ledger.conn().execute(
        "UPDATE operations SET idem = idem || ':' || id
         WHERE ticket = ?1 AND state = ?2 AND idem IS NOT NULL AND idem NOT LIKE '%:' || id",
        rusqlite::params![key.as_str(), STATE_FAILED],
    )?)
}

/// The operations that wait for `vision approve`: the mode ask, and the diffs held for a
/// sensitive path. `Need.id` is `<verb> <commit>`, so that `vision approve KEY <id>` is the
/// exact command.
pub fn needs(ledger: &Ledger) -> Result<Vec<Need>> {
    let mut needs = Vec::new();
    for row in ledger.operations_in_states(&[STATE_PENDING])? {
        if !waits_for_person(&row) {
            continue;
        }
        let call = call_of(&row);
        let stored = stored_of(&row);
        let at = short(&call.head);
        let text = if stored.held.is_empty() {
            let args = call.args.to_string();
            let args = if args == "{}" {
                String::new()
            } else {
                format!(" ({})", clean::strip(&args, false, 80))
            };
            format!("{} at {at} waits for your approval{args}", row.verb)
        } else {
            format!(
                "{} at {at} is held: the diff touches {}, which the approved plan does not name",
                row.verb,
                stored.held.join(", ")
            )
        };
        needs.push(Need {
            kind: NeedKind::Operation,
            ticket: Some(row.ticket.clone()),
            id: format!("{} {}", row.verb, commit_arg(&call.head)),
            text,
        });
    }
    Ok(needs)
}

/// The verb of an operation: it must exist and must not be a read verb.
fn acting_verb<'a>(adapter: &'a Adapter, name: &str) -> Result<&'a Verb> {
    let verb = adapter.verb(name)?;
    if verb.read {
        bail!(
            "`{name}` is a read verb: a turn asks for it with a result of the kind requests, and the executor serves it with executor::read, not as an operation"
        );
    }
    Ok(verb)
}

fn record(
    ledger: &Ledger,
    op: &OperationRun,
    idem: &str,
    call: &CallRecord,
    mode: Mode,
    secrets: &Secrets,
) -> Result<OperationRow> {
    if let Some(row) = ledger.operation_by_idem(idem)? {
        return Ok(row);
    }
    let mut stored_call = serde_json::to_value(call)?;
    secrets.mask_json(&mut stored_call);
    let stored = Stored {
        asked: Some(time::now_rfc3339()),
        ..Stored::default()
    };
    let id = format!("op-{}", uuid::Uuid::new_v4().simple());
    ledger.add_operation(&NewOperation {
        id: &id,
        idem,
        source: if op.turn.is_some() { "turn" } else { "rule" },
        turn: op.turn.as_deref(),
        ticket: op.ticket.as_str(),
        verb: &op.verb,
        args: &stored_call.to_string(),
        mode: mode.as_str(),
        result: &serde_json::to_string(&stored)?,
    })?;
    ledger.operation_by_idem(idem)?.ok_or_else(|| {
        anyhow!(
            "the operation {} was recorded, but it cannot be read back",
            op.verb
        )
    })
}

/// Ends each older pending operation of the same ticket and verb at a different commit: it can
/// never be approved, because the ticket moved.
fn supersede(ctx: &Ctx, ledger: &Ledger, row: &OperationRow, head: &str) -> Result<()> {
    for other in ledger.operations_of(&row.ticket, Some(&row.verb))? {
        if other.id == row.id || other.state != STATE_PENDING {
            continue;
        }
        let old_head = call_of(&other).head;
        if old_head == head {
            continue;
        }
        let mut stored = stored_of(&other);
        stored.detail = format!(
            "stale: {} moved from {} to {}, so this operation never runs. The operation {} replaces it",
            row.ticket,
            short(&old_head),
            short(head),
            row.id
        );
        end(ledger, &other, OpState::Failed, None, &stored)?;
        ctx.log(&format!("operation {}: {}", other.id, stored.detail));
    }
    Ok(())
}

/// What the gates say about one call.
enum Verdict {
    Pass,
    Refuse { gate: Gate, reason: String },
    Hold(Vec<String>),
}

impl Verdict {
    fn describe(&self) -> String {
        match self {
            Verdict::Pass => "the gates would pass".to_string(),
            Verdict::Refuse { gate, reason } => format!("the {gate} gate would refuse: {reason}"),
            Verdict::Hold(paths) => {
                format!("the sensitive gate would hold it for {}", paths.join(", "))
            }
        }
    }
}

/// The gates of section 12 on what leaves: the diff from the base to the commit of the call,
/// and each text of the arguments. `approved` is true after the approval of the person, which
/// covers the sensitive paths.
fn test_gates(
    ctx: &Ctx,
    config: &Config,
    adapter: &Adapter,
    ticket: &TicketContext,
    call: &CallRecord,
    secrets: &Secrets,
    approved: bool,
) -> Result<Verdict> {
    let mut sources = Vec::new();
    collect_texts(&call.args, "args", &mut sources);
    let texts: Vec<Text<'_>> = sources
        .iter()
        .map(|(source, text)| Text { source, text })
        .collect();
    // The diff from the base to the commit, and the patch of each commit between them. The size
    // gate and the sensitive gate read the diff. The secrets gate reads both.
    let (files, commits) = match (&ticket.clone, call.head.is_empty()) {
        (Some(clone), false) => {
            let base = ticket.base(adapter)?.unwrap_or_default();
            (
                gates::parse_diff(&diff(clone, &base, &call.head)?),
                gates::parse_diff(&history(clone, &base, &call.head)?),
            )
        }
        _ => (Vec::new(), Vec::new()),
    };
    let leaving: Vec<FileDiff> = files.iter().chain(&commits).cloned().collect();
    if let Err(reason) = gates::secrets_gate(&leaving, &texts, &secrets.pairs()) {
        return Ok(Verdict::Refuse {
            gate: Gate::Secrets,
            reason,
        });
    }
    if let Err(reason) = gates::size_gate(&files, config.gates.max_diff_lines) {
        return Ok(Verdict::Refuse {
            gate: Gate::Size,
            reason,
        });
    }
    if !approved {
        let plan = read_optional(&ctx.paths.ticket_doc(&ticket.key, APPROVED_PLAN))?;
        let held = gates::sensitive_gate(&files, &config.gates.sensitive, plan.as_deref());
        if !held.is_empty() {
            return Ok(Verdict::Hold(held));
        }
    }
    Ok(Verdict::Pass)
}

/// Each string of the arguments with its path, such as `args.body`.
fn collect_texts(value: &Value, path: &str, out: &mut Vec<(String, String)>) {
    match value {
        Value::String(text) => out.push((path.to_string(), text.clone())),
        Value::Array(items) => {
            for (index, item) in items.iter().enumerate() {
                collect_texts(item, &format!("{path}[{index}]"), out);
            }
        }
        Value::Object(map) => {
            for (key, item) in map {
                collect_texts(item, &format!("{path}.{key}"), out);
            }
        }
        _ => {}
    }
}

/// Calls `act` for a pending operation, and stores the answer.
#[allow(clippy::too_many_arguments)]
fn execute(
    ctx: &Ctx,
    ledger: &Ledger,
    config: &Config,
    adapter: &Adapter,
    verb: &Verb,
    row: &OperationRow,
    call: &CallRecord,
    secrets: &Secrets,
    mode: Mode,
    mut stored: Stored,
) -> Result<OperationResult> {
    let runner = Runner::new(adapter, config, secrets);
    if !runner.has(Script::Act) {
        bail!(
            "{} does not exist or is not executable, so {} of {} cannot run. Fix the adapter; the operation waits",
            runner.path(Script::Act).display(),
            row.verb,
            row.ticket
        );
    }
    if !ledger.claim_operation(&row.id, mode.as_str(), &time::now_rfc3339())? {
        // A different caller took it first.
        return ledger
            .operation(&row.id)?
            .map(|row| result_of(&row))
            .transpose()?
            .ok_or_else(|| anyhow!("the operation {} is gone", row.id));
    }
    let pr = ledger.ticket(&row.ticket)?.and_then(|r| r.pr);
    let input = act_input(row, call, pr, false);
    stored.held.clear();
    let record = crate::running::start(&ctx.paths, "operation", &row.verb, &row.ticket);
    let answer = runner.act(input, timeout_of(verb));
    drop(record);
    let (state, external_id) = match answer {
        Ok(Answer::Done(map)) => {
            let answer = act_answer(&map);
            stored.detail = answer.detail;
            stored.set = answer.set;
            if answer.state == OpState::Confirmed {
                stored.documents = act_documents(ctx, row, &answer.documents, secrets)?;
            }
            (answer.state, answer.external_id)
        }
        Ok(Answer::Refused(map)) => {
            stored.gate = Some("script".into());
            stored.detail = format!(
                "bin/act refused the call: {}",
                clean::text(map.get("detail"), DETAIL_MAX)
            );
            (OpState::Failed, None)
        }
        Err(e) if !e.may_have_acted() => {
            ledger.unclaim_operation(&row.id)?;
            return Err(anyhow!(e).context(format!("{} of {} did not start", row.verb, row.ticket)));
        }
        Err(e) => {
            stored.detail = format!(
                "{e}. The outside state is not known, so each tick asks bin/act again with reconcile"
            );
            (OpState::Uncertain, None)
        }
    };
    finish(ctx, ledger, row, state, external_id, stored)
}

/// The call object of `act` (section 11.1).
fn act_input(row: &OperationRow, call: &CallRecord, pr: Option<i64>, reconcile: bool) -> Value {
    let mut input = json!({
        "verb": row.verb,
        "op": row.id,
        "key": row.ticket,
        "clone": call.clone,
        "branch": call.branch,
        "head": call.head,
        "pr": pr,
        "args": call.args,
    });
    if reconcile {
        // The facts of the first call, so that the script needs no guess: the external id that
        // its answer stored, if any, and the time of the call.
        input["reconcile"] = Value::Bool(true);
        input["external_id"] = json!(row.external_id);
        input["started"] = json!(row.started);
    }
    input
}

/// Writes the documents of a confirmed answer to the ticket folder, with the rules of the
/// documents of a job: masked, plain names only, and never a name that the person or the
/// pipeline owns. Gives their names.
fn act_documents(
    ctx: &Ctx,
    row: &OperationRow,
    documents: &[Document],
    secrets: &Secrets,
) -> Result<Vec<String>> {
    if documents.is_empty() {
        return Ok(Vec::new());
    }
    let key = TicketKey::parse(&row.ticket)?;
    write_documents(&ctx.paths, &key, documents, secrets)
}

fn timeout_of(verb: &Verb) -> Duration {
    verb.timeout
        .map(Duration::from_secs)
        .unwrap_or(DEFAULT_TIMEOUT)
}

/// The answer of `act`, read as untrusted data.
struct ActAnswer {
    state: OpState,
    detail: String,
    external_id: Option<String>,
    set: SetValues,
    /// The valid documents of the answer. A dropped one is a note of `detail`.
    documents: Vec<Document>,
}

fn act_answer(map: &Map<String, Value>) -> ActAnswer {
    let mut notes = Vec::new();
    let state = match map.get("state").and_then(Value::as_str) {
        Some("confirmed") => OpState::Confirmed,
        Some("failed") => OpState::Failed,
        Some("uncertain") => OpState::Uncertain,
        other => {
            notes.push(format!(
                "bin/act gave no valid state ({}), so the state is uncertain",
                clean::strip(&format!("{other:?}"), false, 40)
            ));
            OpState::Uncertain
        }
    };
    let mut set = SetValues::default();
    if let Some(values) = map.get("set").and_then(Value::as_object) {
        match values.get("pr") {
            None | Some(Value::Null) => {}
            Some(value) => match value.as_i64().filter(|n| (1..1_000_000_000).contains(n)) {
                Some(pr) => set.pr = Some(pr),
                None => {
                    notes.push("bin/act gave a set.pr that is not a pull request number".into())
                }
            },
        }
        match values.get("head") {
            None | Some(Value::Null) => {}
            Some(value) => match value.as_str().filter(|h| is_commit(h)) {
                Some(head) => set.head = Some(head.to_ascii_lowercase()),
                None => notes.push("bin/act gave a set.head that is not a commit".into()),
            },
        }
    }
    let (documents, dropped) = clean::documents(map.get("documents"));
    notes.extend(dropped);
    let mut detail = clean::text(map.get("detail"), DETAIL_MAX);
    for note in notes {
        if !detail.is_empty() {
            detail.push_str(". ");
        }
        detail.push_str(&note);
    }
    ActAnswer {
        state,
        detail,
        external_id: clean::line(map.get("external_id"), 200),
        set,
        documents,
    }
}

fn is_commit(text: &str) -> bool {
    (4..=64).contains(&text.len()) && text.bytes().all(|b| b.is_ascii_hexdigit())
}

fn commit_matches(head: &str, commit: &str) -> bool {
    let commit = commit.trim().to_ascii_lowercase();
    if head.is_empty() {
        return commit == "-";
    }
    head == commit || (commit.len() >= 7 && head.starts_with(&commit))
}

fn waits_for_person(row: &OperationRow) -> bool {
    row.mode == Mode::Ask.as_str() || !stored_of(row).held.is_empty()
}

fn refuse(
    ctx: &Ctx,
    ledger: &Ledger,
    row: &OperationRow,
    mut stored: Stored,
    gate: &str,
    reason: String,
) -> Result<OperationResult> {
    stored.gate = Some(gate.to_string());
    stored.detail = reason;
    stored.held.clear();
    finish(ctx, ledger, row, OpState::Failed, None, stored)
}

/// Stores the mode and the result of an operation that stays pending.
fn pending(
    ctx: &Ctx,
    ledger: &Ledger,
    row: &OperationRow,
    mode: Mode,
    stored: &Stored,
) -> Result<OperationResult> {
    ledger.update_pending_operation(&row.id, mode.as_str(), &serde_json::to_string(stored)?)?;
    let result = read_back(ledger, row)?;
    log(ctx, &result);
    Ok(result)
}

/// Stores the end of an operation, writes its `set` values to the ticket row when it is
/// confirmed, and logs it.
fn finish(
    ctx: &Ctx,
    ledger: &Ledger,
    row: &OperationRow,
    state: OpState,
    external_id: Option<String>,
    stored: Stored,
) -> Result<OperationResult> {
    end(ledger, row, state, external_id.as_deref(), &stored)?;
    if state == OpState::Confirmed && !stored.set.is_empty() {
        let written = ledger.set_ticket_values(
            &row.ticket,
            stored.set.pr,
            stored.set.head.as_deref(),
            &time::now_rfc3339(),
        )?;
        if !written {
            ctx.log(&format!(
                "operation {}: {} has no ticket row, so its set values were not written",
                row.id, row.ticket
            ));
        }
    }
    let result = read_back(ledger, row)?;
    log(ctx, &result);
    Ok(result)
}

fn end(
    ledger: &Ledger,
    row: &OperationRow,
    state: OpState,
    external_id: Option<&str>,
    stored: &Stored,
) -> Result<()> {
    let ended = state.is_final().then(time::now_rfc3339);
    ledger.end_operation(
        &row.id,
        state.as_str(),
        external_id,
        ended.as_deref(),
        &serde_json::to_string(stored)?,
    )
}

fn read_back(ledger: &Ledger, row: &OperationRow) -> Result<OperationResult> {
    let row = ledger
        .operation(&row.id)?
        .ok_or_else(|| anyhow!("the operation {} is gone from the ledger", row.id))?;
    result_of(&row)
}

fn log(ctx: &Ctx, result: &OperationResult) {
    ctx.log(&format!(
        "operation {}: {} of {} at {}: {}{}",
        result.id,
        result.verb,
        result.ticket,
        short(&result.head),
        result.outcome,
        if result.detail.is_empty() {
            String::new()
        } else {
            format!(": {}", result.detail)
        }
    ));
}

fn call_of(row: &OperationRow) -> CallRecord {
    serde_json::from_str(&row.args).unwrap_or_default()
}

fn stored_of(row: &OperationRow) -> Stored {
    row.result
        .as_deref()
        .and_then(|text| serde_json::from_str(text).ok())
        .unwrap_or_default()
}

/// The typed view of a row.
fn result_of(row: &OperationRow) -> Result<OperationResult> {
    let call = call_of(row);
    let stored = stored_of(row);
    let state = OpState::parse(&row.state).with_context(|| {
        format!(
            "the operation {} has the state `{}`, which this vision does not know",
            row.id, row.state
        )
    })?;
    let outcome = match state {
        OpState::Confirmed => Outcome::Confirmed,
        OpState::Failed => match &stored.gate {
            Some(gate) => Outcome::Refused { gate: gate.clone() },
            None => Outcome::Failed,
        },
        OpState::Uncertain => Outcome::Uncertain,
        OpState::Executing => Outcome::Executing,
        OpState::Pending => match row.mode.as_str() {
            "hold" => Outcome::Held,
            "shadow" => Outcome::Shadowed,
            "ask" => Outcome::Waiting {
                command: approve_command(&row.ticket, &row.verb, &call.head),
            },
            _ if !stored.held.is_empty() => Outcome::Waiting {
                command: approve_command(&row.ticket, &row.verb, &call.head),
            },
            _ => Outcome::Pending,
        },
    };
    Ok(OperationResult {
        id: row.id.clone(),
        ticket: row.ticket.clone(),
        verb: row.verb.clone(),
        head: call.head,
        args: call.args,
        mode: row.mode.clone(),
        state,
        outcome,
        detail: stored.detail,
        external_id: row.external_id.clone(),
        set: stored.set,
        documents: stored.documents,
        held: stored.held,
        intent: stored.intent,
        source: row.source.clone(),
        turn: row.turn.clone(),
        started: row.started.clone(),
        ended: row.ended.clone(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_ladder_moves_one_step_and_stops_at_the_ends() {
        assert_eq!(promoted(Mode::Hold), Some(Mode::Shadow));
        assert_eq!(promoted(Mode::Shadow), Some(Mode::Ask));
        assert_eq!(promoted(Mode::Ask), Some(Mode::Auto));
        assert_eq!(promoted(Mode::Auto), None);
        assert_eq!(
            demoted(Mode::Auto),
            Some(Mode::Ask),
            "one wrong operation moves a verb back to ask"
        );
        assert_eq!(demoted(Mode::Ask), Some(Mode::Shadow));
        assert_eq!(demoted(Mode::Shadow), Some(Mode::Hold));
        assert_eq!(demoted(Mode::Hold), None);
    }

    #[test]
    fn the_idempotency_key_hashes_ticket_verb_commit_and_arguments() {
        let key = idempotency_key(
            "DEMO-12",
            "git.push",
            "abc",
            &json!({"a": 1, "b": {"y": 2, "x": 1}}),
        );
        assert_eq!(key.len(), 64);
        let same = idempotency_key(
            "DEMO-12",
            "git.push",
            "abc",
            &json!({"b": {"x": 1, "y": 2}, "a": 1}),
        );
        assert_eq!(key, same, "the order of the keys does not matter");
        for other in [
            idempotency_key(
                "DEMO-13",
                "git.push",
                "abc",
                &json!({"a": 1, "b": {"y": 2, "x": 1}}),
            ),
            idempotency_key(
                "DEMO-12",
                "pr.open",
                "abc",
                &json!({"a": 1, "b": {"y": 2, "x": 1}}),
            ),
            idempotency_key(
                "DEMO-12",
                "git.push",
                "abd",
                &json!({"a": 1, "b": {"y": 2, "x": 1}}),
            ),
            idempotency_key(
                "DEMO-12",
                "git.push",
                "abc",
                &json!({"a": 2, "b": {"y": 2, "x": 1}}),
            ),
        ] {
            assert_ne!(key, other);
        }
    }

    #[test]
    fn the_mode_of_the_configuration_wins_and_unknown_verbs_are_errors() {
        let text = "[[step]]\nid = \"a\"\nkind = \"human\"\naction = \"go\"\nnext = \"b\"\n[[step]]\nid = \"b\"\nkind = \"end\"\n[verbs.\"git.push\"]\nmode = \"auto\"\n[verbs.\"tracker.issue\"]\nread = true\n";
        let adapter = Adapter::parse(text, std::path::Path::new("/a/toy")).unwrap();
        let mut config = Config::default();
        assert_eq!(mode_of(&config, &adapter, "git.push").unwrap(), Mode::Auto);
        config.modes.insert("git.push".into(), Mode::Shadow);
        assert_eq!(
            mode_of(&config, &adapter, "git.push").unwrap(),
            Mode::Shadow
        );
        let err = mode_of(&config, &adapter, "pr.merge")
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("has no verb `pr.merge`. Its verbs: git.push, tracker.issue"),
            "{err}"
        );
        assert!(mode_of(&config, &adapter, "tracker.issue")
            .unwrap_err()
            .to_string()
            .contains("read verb"));
    }

    #[test]
    fn act_answers_are_read_as_untrusted_data() {
        let map = json!({"state": "confirmed", "detail": "ok\u{1b}[2J", "external_id": 7,
                         "set": {"pr": 7, "head": "ABC1234", "other": 1}})
        .as_object()
        .unwrap()
        .clone();
        let answer = act_answer(&map);
        assert_eq!(answer.state, OpState::Confirmed);
        assert_eq!(answer.detail, "ok[2J");
        assert_eq!(answer.external_id.as_deref(), Some("7"));
        assert_eq!(
            answer.set,
            SetValues {
                pr: Some(7),
                head: Some("abc1234".into())
            }
        );
        let bad = json!({"state": "done", "set": {"pr": -1, "head": "../x"}})
            .as_object()
            .unwrap()
            .clone();
        let answer = act_answer(&bad);
        assert_eq!(answer.state, OpState::Uncertain);
        assert!(answer.set.is_empty());
        assert!(
            answer.detail.contains("no valid state"),
            "{}",
            answer.detail
        );
        assert!(
            answer.detail.contains("set.pr") && answer.detail.contains("set.head"),
            "{}",
            answer.detail
        );
    }

    #[test]
    fn a_commit_matches_in_full_or_by_a_prefix_of_seven() {
        let head = "9f3e21c0123456789abcdef0123456789abcdef0";
        assert!(commit_matches(head, head));
        assert!(commit_matches(head, "9F3E21C"));
        assert!(!commit_matches(head, "9f3e21"));
        assert!(!commit_matches(head, "9f3e21d"));
        assert!(commit_matches("", "-"));
        assert!(!commit_matches("", "abc1234"));
        assert_eq!(
            approve_command("DEMO-12", "git.push", ""),
            "vision approve DEMO-12 git.push -"
        );
    }

    #[test]
    fn outcomes_say_when_the_person_is_needed() {
        assert!(!Outcome::Confirmed.needs_you());
        assert!(!Outcome::Executing.needs_you());
        assert!(Outcome::Held.needs_you());
        assert!(Outcome::Shadowed.needs_you());
        assert!(Outcome::Refused {
            gate: "size".into()
        }
        .needs_you());
        assert_eq!(
            Outcome::Refused {
                gate: "size".into()
            }
            .to_string(),
            "refused by the size gate"
        );
    }
}
