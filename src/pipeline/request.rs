//! The launch request of a turn step (section 7.2): the clone as the working folder, the facts
//! of the step, the files of the session view, and the role file of the adapter.

use anyhow::{bail, Context, Result};
use serde_json::{json, Map, Value};

use super::machine::{expected_docs, Ticket};
use crate::adapter::{self, StepKind};
use crate::ctx::Ctx;
use crate::executor::TicketContext;
use crate::ledger::Ledger;
use crate::memory::Memory;
use crate::tick::TurnStart;
use crate::turn::{LaunchRequest, ViewFile};
use crate::util::fs::{atomic_write, read_optional};

/// The folder of the ticket that holds the files that only the session view needs, such as the
/// principles of an agent. Its name cannot be the name of a document.
pub const VIEW_DIR: &str = ".view";

/// The launch request of `start`.
pub fn build(ctx: &Ctx, ledger: &Ledger, start: &TurnStart) -> Result<LaunchRequest> {
    let config = ctx.config()?;
    let adapter = adapter::load_with(config, &ctx.paths)?;
    let Some(step) = adapter.step(&start.step) else {
        bail!(
            "the step {} is not in the adapter {}",
            start.step,
            adapter.name
        );
    };
    let StepKind::Turn(turn) = &step.kind else {
        bail!("the step {} is not a turn step", step.id);
    };
    let Some(row) = ledger.ticket(start.ticket.as_str())? else {
        bail!(
            "{} has no row in the ledger. Run `vision take {}`",
            start.ticket,
            start.ticket
        );
    };
    let ticket = Ticket::of(row)?;
    let clone = ticket.clone_dir()?;
    let visit = ticket.visit();
    let key = &ticket.key;
    let context = TicketContext::load(ledger, &adapter, key)?;
    let base = context.base(&adapter)?;

    let mut request = LaunchRequest::new(key.clone(), &start.agent, &step.id, start.kind, &clone);
    request.resumes = start.resumes.clone();
    request.message = start.message.clone();
    request.expects = step.expects.iter().map(ToString::to_string).collect();
    request.role_file = adapter.role_file(&start.agent);

    let mut view = Vec::new();
    let issue_path = ctx.paths.ticket_doc(key, "issue.json");
    let mut title = Value::Null;
    let mut issue = Value::Null;
    if let Some(text) = read_optional(&issue_path)? {
        let value: Value = serde_json::from_str(&text).unwrap_or(Value::Null);
        if let Some(t) = value
            .pointer("/issue/untrusted/title")
            .and_then(Value::as_str)
        {
            title = json!({"untrusted": t});
        }
        view.push(ViewFile {
            from: issue_path,
            to: "turns/{turn}/issue.json".into(),
        });
        issue = json!(".vision/turns/{turn}/issue.json");
    }

    let mut documents = Map::new();
    for name in &turn.reads {
        let path = ctx.paths.ticket_doc(key, name);
        if path.is_file() {
            view.push(ViewFile {
                from: path,
                to: name.clone(),
            });
            documents.insert(name.clone(), json!(format!(".vision/{name}")));
        } else {
            documents.insert(name.clone(), Value::Null);
        }
    }
    for name in &visit.extra {
        if documents.contains_key(name) {
            continue;
        }
        let path = ctx.paths.ticket_doc(key, name);
        if path.is_file() {
            view.push(ViewFile {
                from: path,
                to: name.clone(),
            });
            documents.insert(name.clone(), json!(format!(".vision/{name}")));
        }
    }

    let memory = Memory::from_ctx(ctx)?;
    let principles = memory.principles_file(&start.agent)?;
    let staged = ctx
        .paths
        .ticket_dir(key)
        .join(VIEW_DIR)
        .join(format!("principles-{}.md", start.agent));
    if let Some(parent) = staged.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("could not make {}", parent.display()))?;
    }
    atomic_write(&staged, principles.text.as_bytes())?;
    view.push(ViewFile {
        from: staged,
        to: "turns/{turn}/principles.md".into(),
    });
    let loaded: Vec<Value> = principles
        .loaded
        .iter()
        .map(|p| json!({"id": p.id.to_string(), "version": p.version}))
        .collect();

    let mut facts = json!({
        "ticket": {"key": key.as_str(), "title": title, "issue": issue},
        "branch": context.branch,
        "base": base,
        "head": context.head,
        "pr": context.pr,
        "documents": documents,
        "expected_documents": expected_docs(step),
        "principles": ".vision/turns/{turn}/principles.md",
        "principles_loaded": loaded,
        "skills": {},
        "sensitive_topics": config.judge.always_ask,
        "sensitive_paths": config.gates.sensitive,
    });
    if !turn.with.is_empty() {
        facts["with"] = Value::Object(turn.with.clone());
    }
    // The read verbs that the session can ask for with a result of the kind `requests`.
    facts["requests"] = Value::Array(
        adapter
            .verbs
            .values()
            .filter(|v| v.read)
            .map(|v| json!({"verb": v.name, "about": v.about}))
            .collect(),
    );
    request.facts = facts;
    request.view = view;
    Ok(request)
}
