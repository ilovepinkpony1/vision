//! `bin/observe` (section 11.1): the facts of the queue and of one ticket, read as untrusted
//! data into typed values.

use anyhow::{anyhow, bail, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};

use super::clean;
use super::context::TicketContext;
use super::scripts::{Answer, Runner, Secrets};
use crate::adapter;
use crate::ctx::Ctx;
use crate::ledger::Ledger;
use crate::paths::TicketKey;

/// One ticket of the queue.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct QueueTicket {
    #[serde(serialize_with = "key_text")]
    pub key: TicketKey,
    pub title: String,
    pub status: String,
    pub eligible: bool,
    /// Why the ticket is not eligible. Empty for an eligible ticket.
    pub reason: String,
}

/// The facts of one ticket.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct TicketFacts {
    /// The whole answer, for the paths of `watch`. Each secret is masked.
    pub answer: Value,
    /// The issue as the tracker gives it. Its text is untrusted.
    pub issue: Value,
    pub tracker_status: Option<String>,
    pub pr: Option<PrFacts>,
    pub ci: Option<CiFacts>,
    pub envs: Map<String, Value>,
}

/// The pull request of a ticket.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct PrFacts {
    pub number: i64,
    pub draft: bool,
    pub head: Option<String>,
    pub merged: bool,
    pub closed: bool,
    pub conflict: bool,
    pub approved_by: Vec<String>,
    pub changes_requested: bool,
    pub comments_waiting: u64,
}

/// The CI of a ticket.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct CiFacts {
    pub head: Option<String>,
    /// Such as `green`, `red`, `pending`, or `none`.
    pub state: String,
}

fn key_text<S: serde::Serializer>(key: &TicketKey, serializer: S) -> Result<S::Ok, S::Error> {
    serializer.serialize_str(key.as_str())
}

impl TicketFacts {
    /// The value at a path of `watch`, such as `ci.state`.
    pub fn watch(&self, path: &str) -> Option<&Value> {
        path.split('.')
            .try_fold(&self.answer, |value, part| value.get(part))
    }

    /// The value at a path of `watch` as the text that `on` compares: a string as it is, a
    /// boolean or a number as JSON, and `None` for a missing value, a null, a list, or a table.
    pub fn watch_text(&self, path: &str) -> Option<String> {
        match self.watch(path)? {
            Value::String(s) => Some(s.clone()),
            Value::Bool(b) => Some(b.to_string()),
            Value::Number(n) => Some(n.to_string()),
            _ => None,
        }
    }
}

/// `observe {"what": "queue"}`: the tickets of the tracker with their eligibility.
pub fn observe_queue(ctx: &Ctx) -> Result<Vec<QueueTicket>> {
    let answer = call(ctx, json!({"what": "queue"}))?;
    let items = answer
        .get("tickets")
        .and_then(Value::as_array)
        .context("bin/observe answered the queue with no `tickets` list")?;
    let mut tickets = Vec::new();
    for item in items {
        let key_text = item.get("key").and_then(Value::as_str).unwrap_or("");
        let key = TicketKey::parse(key_text).with_context(|| {
            format!(
                "bin/observe gave the ticket key `{}`",
                clean::strip(key_text, false, 80)
            )
        })?;
        tickets.push(QueueTicket {
            key,
            title: clean::line(item.get("title"), 300).unwrap_or_default(),
            status: clean::line(item.get("status"), 100).unwrap_or_default(),
            eligible: item
                .get("eligible")
                .and_then(Value::as_bool)
                .unwrap_or(false),
            reason: clean::line(item.get("reason"), 300).unwrap_or_default(),
        });
    }
    Ok(tickets)
}

/// `observe {"what": "ticket", ...}` with the branch, the pull request, and the clone of the
/// ticket row (or the branch pattern of the adapter for a ticket with no row).
pub fn observe_ticket(ctx: &Ctx, ledger: &Ledger, key: &TicketKey) -> Result<TicketFacts> {
    let adapter = adapter::load(ctx)?;
    let ticket = TicketContext::load(ledger, &adapter, key)?;
    let input = json!({
        "what": "ticket",
        "key": key.as_str(),
        "branch": ticket.branch,
        "pr": ticket.pr,
        "clone": ticket.clone_text(),
    });
    let answer = call(ctx, input)?;
    let wrong = |what: &str| format!("bin/observe answered {key} with a wrong {what}");
    let pr = match answer.get("pr") {
        None | Some(Value::Null) => None,
        Some(value) => {
            Some(serde_json::from_value::<PrFacts>(value.clone()).with_context(|| wrong("pr"))?)
        }
    };
    let ci = match answer.get("ci") {
        None | Some(Value::Null) => None,
        Some(value) => {
            Some(serde_json::from_value::<CiFacts>(value.clone()).with_context(|| wrong("ci"))?)
        }
    };
    let envs = match answer.get("envs") {
        None | Some(Value::Null) => Map::new(),
        Some(Value::Object(map)) => map.clone(),
        Some(_) => bail!(wrong("envs")),
    };
    Ok(TicketFacts {
        issue: answer.get("issue").cloned().unwrap_or(Value::Null),
        tracker_status: clean::line(answer.get("tracker_status"), 100),
        pr,
        ci,
        envs,
        answer: Value::Object(answer),
    })
}

fn call(ctx: &Ctx, input: Value) -> Result<Map<String, Value>> {
    let config = ctx.config()?;
    let adapter = adapter::load_with(config, &ctx.paths)?;
    let secrets = Secrets::read(config)?;
    match Runner::new(&adapter, config, &secrets).observe(input) {
        Ok(Answer::Done(map)) => Ok(map),
        Ok(Answer::Refused(map)) => Err(anyhow!(
            "bin/observe refused the call: {}",
            clean::text(map.get("detail"), clean::DETAIL_MAX)
        )),
        Err(e) => Err(anyhow!(e)),
    }
}
