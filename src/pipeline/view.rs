//! The rows of the status view and the needs of the pipeline. Both read only the ledger, the
//! ticket rows, and the ticket folders, so that a broken adapter never breaks the snapshot.

use std::collections::BTreeMap;

use anyhow::Result;
use serde::Serialize;

use super::machine::Ticket;
use super::state::{clear_command, NEEDS_YOU, PAUSED, STOPPED, WAITS_FOR_LIMIT};
use crate::ctx::Ctx;
use crate::executor::{self, short};
use crate::ledger::Ledger;
use crate::snapshot::{Need, NeedKind};
use crate::util::fs::read_optional;
use crate::util::{ids, time};

/// The length of a plan hash as `vision plan` shows it.
pub const SHORT_HASH: usize = 12;

/// The hash of a plan text: sha256 of its bytes.
pub fn plan_hash(bytes: &[u8]) -> String {
    ids::sha256_hex(bytes)
}

/// One ticket row of `vision status`.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct TicketView {
    pub key: String,
    pub step: Option<String>,
    /// The kind of the step: turn, job, human, wait, or end.
    pub kind: Option<String>,
    /// One line: what works now, or what the ticket waits for.
    pub now: String,
    /// The running work turn: its agent and its minutes.
    pub works: Option<Works>,
    /// The flags with their reasons and the commands that clear them.
    pub flags: Vec<FlagView>,
    /// The needs of the ticket by kind, such as `{"card": 1}`.
    pub needs: BTreeMap<String, usize>,
    pub marks: Marks,
    /// The cost of the turns of the ticket so far, in dollars.
    pub cost_usd: f64,
    pub pr: Option<i64>,
    pub branch: Option<String>,
    pub clone: Option<String>,
    pub updated: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Works {
    pub agent: String,
    pub turn: String,
    pub minutes: i64,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct FlagView {
    pub flag: String,
    pub reason: String,
    pub since: Option<String>,
    /// The command that clears the flag.
    pub command: String,
}

/// The marks of a ticket, each with its commit.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct Marks {
    /// The newest result of each job, such as the proof.
    pub jobs: Vec<Mark>,
    /// The CI: the newest CI check, or the state that `observe` gave last.
    pub ci: Option<Mark>,
    /// Each env with the commit that it runs.
    pub envs: BTreeMap<String, Option<String>>,
    /// Who approved the pull request.
    pub approved_by: Vec<String>,
    /// The short hash of the approved plan.
    pub plan: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Mark {
    pub name: String,
    pub result: String,
    pub commit: String,
}

/// The needs of the pipeline: each plan that waits for `vision approve-plan`, and each ticket
/// with the flag `needs-you` or `paused`. The cards, the operations, and the proposals are the
/// needs of their own modules.
pub fn needs(ctx: &Ctx, ledger: &Ledger) -> Result<Vec<Need>> {
    let mut out = Vec::new();
    for row in ledger.tickets()? {
        let Ok(ticket) = Ticket::of(row) else {
            continue;
        };
        if ticket.key.is_reserved() || ticket.counts.finished.is_some() {
            continue;
        }
        for flag in &ticket.flags {
            if flag != NEEDS_YOU && flag != PAUSED {
                continue;
            }
            let reason = ticket
                .counts
                .reasons
                .get(flag)
                .map(|r| r.reason.clone())
                .unwrap_or_else(|| default_reason(flag).to_string());
            out.push(Need {
                kind: NeedKind::Flag,
                ticket: Some(ticket.key.to_string()),
                id: flag.clone(),
                text: format!("{flag} at the step {}: {reason}", ticket.state()),
            });
        }
        if let Some(need) = plan_need(ctx, ledger, &ticket)? {
            out.push(need);
        }
    }
    Ok(out)
}

fn default_reason(flag: &str) -> &'static str {
    match flag {
        PAUSED => "you took over the session with vision open. Run vision retry when you are done",
        STOPPED => "you stopped it with vision stop",
        WAITS_FOR_LIMIT => "a limit of the subscription refused a turn",
        _ => "a rule stopped it",
    }
}

/// The plan that waits for `vision approve-plan`: a human step with that action whose visit has
/// no approval yet, and a `plan.md` in the ticket folder.
fn plan_need(ctx: &Ctx, ledger: &Ledger, ticket: &Ticket) -> Result<Option<Need>> {
    let visit = ticket.visit();
    if visit.kind != "human" || visit.action.as_deref() != Some("approve-plan") {
        return Ok(None);
    }
    if ticket.flags.iter().any(|f| f == STOPPED) {
        return Ok(None);
    }
    let mark = super::machine::human_mark(&ticket.key, visit.seq, "approve-plan");
    if ledger.event_detail(&mark)?.is_some() {
        return Ok(None);
    }
    let Ok(bytes) = std::fs::read(ctx.paths.ticket_doc(&ticket.key, "plan.md")) else {
        return Ok(None);
    };
    let hash = plan_hash(&bytes);
    Ok(Some(Need {
        kind: NeedKind::Plan,
        ticket: Some(ticket.key.to_string()),
        id: hash[..SHORT_HASH].to_string(),
        text: format!(
            "the plan of {} waits for your approval at the step {}",
            ticket.key, visit.step
        ),
    }))
}

/// The rows of `vision status`, one for each ticket, by key.
pub fn rows(ctx: &Ctx, ledger: &Ledger, all_needs: &[Need]) -> Result<Vec<TicketView>> {
    let running = crate::turn::running_turns(ctx, ledger)?;
    let now = time::now();
    let mut out = Vec::new();
    for row in ledger.tickets()? {
        let Ok(ticket) = Ticket::of(row.clone()) else {
            continue;
        };
        if ticket.key.is_reserved() {
            continue;
        }
        let key = ticket.key.to_string();
        let visit = ticket.visit();
        let mut needs: BTreeMap<String, usize> = BTreeMap::new();
        for need in all_needs
            .iter()
            .filter(|n| n.ticket.as_deref() == Some(&key))
        {
            *needs.entry(need_word(need.kind).to_string()).or_default() += 1;
        }
        let flags: Vec<FlagView> = ticket
            .flags
            .iter()
            .map(|flag| {
                let reason = ticket.counts.reasons.get(flag);
                FlagView {
                    flag: flag.clone(),
                    reason: reason
                        .map(|r| r.reason.clone())
                        .unwrap_or_else(|| default_reason(flag).to_string()),
                    since: reason.map(|r| r.at.clone()),
                    command: clear_command(flag, &key),
                }
            })
            .collect();
        let mine: Vec<_> = running.iter().filter(|t| t.ticket == key).collect();
        let work = mine.iter().rev().find(|t| {
            !t.agent
                .as_deref()
                .and_then(crate::models::agent)
                .is_some_and(|spec| spec.alone)
        });
        let works = work.map(|t| Works {
            agent: t.agent.clone().unwrap_or_default(),
            turn: t.id.clone(),
            minutes: t
                .started
                .as_deref()
                .and_then(|s| time::parse_rfc3339(s).ok())
                .map_or(0, |at| time::seconds_between(at, now) / 60),
        });
        let judging = mine.len() > usize::from(work.is_some());
        let now_text = now_line(ledger, &ticket, &works, judging, &needs, &flags)?;
        out.push(TicketView {
            key: key.clone(),
            step: row.state.clone(),
            kind: (!visit.kind.is_empty()).then(|| visit.kind.clone()),
            now: now_text,
            works,
            flags,
            needs,
            marks: marks(ctx, ledger, &ticket)?,
            cost_usd: ledger.spent_on_ticket(&key)?,
            pr: row.pr,
            branch: row.branch.clone(),
            clone: row.clone.clone(),
            updated: row.updated.clone(),
        });
    }
    Ok(out)
}

fn need_word(kind: NeedKind) -> &'static str {
    match kind {
        NeedKind::Card => "card",
        NeedKind::Plan => "plan",
        NeedKind::Operation => "operation",
        NeedKind::Flag => "flag",
        NeedKind::Proposal => "proposal",
    }
}

fn now_line(
    ledger: &Ledger,
    ticket: &Ticket,
    works: &Option<Works>,
    judging: bool,
    needs: &BTreeMap<String, usize>,
    flags: &[FlagView],
) -> Result<String> {
    let key = &ticket.key;
    if ticket.counts.finished.is_some() {
        let merged = ticket
            .counts
            .seen
            .as_ref()
            .and_then(|s| s.pr.as_ref())
            .filter(|pr| pr.merged)
            .map(|pr| format!(" with the merged pull request #{}", pr.number));
        return Ok(format!("done{}", merged.unwrap_or_default()));
    }
    if let Some(works) = works {
        return Ok(format!("{} works ({} min)", works.agent, works.minutes));
    }
    if let Some(flag) = flags.first() {
        let word = match flag.flag.as_str() {
            NEEDS_YOU => "needs you",
            PAUSED => "paused",
            STOPPED => "stopped",
            WAITS_FOR_LIMIT => "waits for the limit",
            other => other,
        };
        let reason: String = flag.reason.chars().take(60).collect();
        let more = if flag.reason.chars().count() > 60 {
            "..."
        } else {
            ""
        };
        return Ok(format!("{word}: {reason}{more}"));
    }
    if !needs.is_empty() {
        let list: Vec<String> = needs.iter().map(|(k, n)| format!("{n} {k}")).collect();
        return Ok(format!(
            "needs you: {}. Run `vision needs`",
            list.join(", ")
        ));
    }
    if judging {
        return Ok("the Judge reads the cards".into());
    }
    let open = ledger.open_cards(key.as_str())?;
    if open > 0 {
        return Ok(format!("{open} card(s) wait for an answer"));
    }
    let visit = ticket.visit();
    for op in executor::operations_of(ledger, key)?.into_iter().rev() {
        if op.state == executor::OpState::Uncertain {
            return Ok(format!(
                "waits for the reconcile of {} ({}), which is uncertain",
                op.verb, op.id
            ));
        }
    }
    let text = match visit.kind.as_str() {
        "human" => format!(
            "waits for you: vision {} {key}",
            visit.action.as_deref().unwrap_or("?")
        ),
        "wait" => match &visit.watched {
            Some(value) => format!("waits: the watched value is {value}"),
            None => "waits for its first look".into(),
        },
        "turn" => format!(
            "next: a turn of {}",
            visit.agent.as_deref().unwrap_or("the agent")
        ),
        "end" => "the end step finishes it".into(),
        _ => "the next tick moves it".into(),
    };
    Ok(text)
}

fn marks(ctx: &Ctx, ledger: &Ledger, ticket: &Ticket) -> Result<Marks> {
    let key = ticket.key.as_str();
    let mut marks = Marks::default();
    let checks = ledger.latest_checks(key)?;
    for check in &checks {
        if check.kind == executor::JOB_CHECK {
            marks.jobs.push(Mark {
                name: check
                    .source
                    .strip_prefix("job:")
                    .unwrap_or(&check.source)
                    .to_string(),
                result: check.result.clone(),
                commit: short(&check.head).to_string(),
            });
        }
    }
    let ci_check = checks.iter().rev().find(|c| c.kind == "ci");
    let seen = ticket.counts.seen.as_ref();
    let seen_ci = seen
        .and_then(|s| s.ci.as_ref())
        .filter(|ci| ci.head.is_some());
    marks.ci = match (seen_ci, ci_check) {
        (Some(ci), _) if !ci.state.is_empty() && ci.state != "none" => Some(Mark {
            name: "ci".into(),
            result: ci.state.clone(),
            commit: short(ci.head.as_deref().unwrap_or("")).to_string(),
        }),
        (_, Some(check)) => Some(Mark {
            name: "ci".into(),
            result: check.result.clone(),
            commit: short(&check.head).to_string(),
        }),
        _ => None,
    };
    if let Some(seen) = seen {
        for (name, value) in &seen.envs {
            marks.envs.insert(
                name.clone(),
                value
                    .get("head")
                    .and_then(serde_json::Value::as_str)
                    .map(|h| short(h).to_string()),
            );
        }
        if let Some(pr) = &seen.pr {
            marks.approved_by = pr.approved_by.clone();
        }
    }
    if let Some(text) = read_optional(&ctx.paths.ticket_doc(&ticket.key, "approved-plan.md"))? {
        marks.plan = Some(plan_hash(text.as_bytes())[..SHORT_HASH].to_string());
    }
    Ok(marks)
}

/// The marks as one short text, for the table.
pub fn marks_text(marks: &Marks) -> String {
    let mut parts = Vec::new();
    for job in &marks.jobs {
        parts.push(format!("{} {}@{}", job.name, job.result, job.commit));
    }
    if let Some(ci) = &marks.ci {
        parts.push(format!("ci {}@{}", ci.result, ci.commit));
    }
    for (name, head) in &marks.envs {
        parts.push(format!("{name}@{}", head.as_deref().unwrap_or("-")));
    }
    if !marks.approved_by.is_empty() {
        parts.push(format!("approved by {}", marks.approved_by.join(",")));
    }
    if parts.is_empty() {
        "-".into()
    } else {
        parts.join(" ")
    }
}
