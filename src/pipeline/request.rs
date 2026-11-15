//! The launch request of a turn step (section 7.2): the clone as the working folder, the facts
//! of the step, the files of the session view, and the role file of the adapter.

use anyhow::{bail, Context, Result};
use serde_json::{json, Map, Value};

use super::machine::{expected_docs, Ticket};
use crate::adapter::{self, StepKind};
use crate::ctx::Ctx;
use crate::executor::TicketContext;
use crate::journal::search::RelatedInput;
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
    // A member of a team: its own view of the step, with its reads and its end facts.
    let member = step.member(&start.agent).cloned();
    let view = match &member {
        Some(member) => step.member_view(member),
        None if !turn.team.is_empty() => bail!(
            "the step {} has no member {} in its team",
            step.id,
            start.agent
        ),
        None => step.clone(),
    };
    let step = &view;
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
    // An agent that shares the role of another, such as `critic-narrow`, can take the role
    // file of that agent in the adapter.
    request.role_file = adapter.role_file(&start.agent).or_else(|| {
        let role = crate::models::role_name(&start.agent);
        (role != start.agent)
            .then(|| adapter.role_file(role))
            .flatten()
    });
    request.member = member.is_some();
    request.defined = adapter.agents.get(&start.agent).cloned();
    if start.fallback {
        let fallback = crate::models::choice_in(config, &adapter.agents, &start.agent)?
            .fallback
            .with_context(|| format!("the agent {} has no fallback", start.agent))?;
        request.runner = Some(crate::config::Runner::Claude);
        request.model = Some(fallback.model);
        request.effort = Some(fallback.effort);
    }

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
    // A member gets the answers of its own read verbs, never those of another member. A new
    // entry of a team step gives each member the answers of the person, such as a hint.
    let mut extra: Vec<String> = match &member {
        Some(member) => visit
            .member_extra
            .get(&member.agent)
            .cloned()
            .unwrap_or_default(),
        None => visit.extra.clone(),
    };
    if member.is_some() && visit.n > 1 {
        extra.push("answers.md".into());
    }
    for name in &extra {
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

    // The journal: its folder, which the turn can read, and the entries that are near the step.
    let journal_dir = crate::journal::turn_dir(&ctx.paths)?;
    let related = match related_input(
        ctx,
        ledger,
        &adapter,
        &ticket,
        start,
        &clone,
        base.as_deref(),
        &turn.reads,
    )
    .and_then(|input| {
        crate::journal::search::related_text(ctx, &input, config.journal.related as usize)
    }) {
        Ok(found) => found,
        Err(e) => {
            ctx.log(&format!(
                "journal: no related.md for {} at the step {}: {e:#}",
                key, step.id
            ));
            None
        }
    };
    let related_value = match related {
        Some(text) => {
            let staged = ctx.paths.ticket_dir(key).join(VIEW_DIR).join("related.md");
            atomic_write(&staged, text.as_bytes())?;
            view.push(ViewFile {
                from: staged,
                to: "turns/{turn}/related.md".into(),
            });
            json!(".vision/turns/{turn}/related.md")
        }
        None => Value::Null,
    };

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
        "related": related_value,
        "journal": journal_dir.display().to_string(),
        "sensitive_topics": config.judge.always_ask,
        "sensitive_paths": config.gates.sensitive,
    });
    if !turn.with.is_empty() {
        facts["with"] = Value::Object(turn.with.clone());
    }
    // The read verbs that the session can ask for with a result of the kind `requests`: the
    // read verbs of the adapter, and the read verbs of git that the core serves.
    let mut requests: Vec<Value> = adapter
        .verbs
        .values()
        .filter(|v| v.read)
        .map(|v| json!({"verb": v.name, "about": v.about}))
        .collect();
    for verb in super::git_verbs::NAMES {
        if !adapter.verbs.contains_key(verb) {
            requests.push(json!({"verb": verb, "about": super::git_verbs::about(verb)}));
        }
    }
    facts["requests"] = Value::Array(requests);
    if let Some(kind) = &ticket.counts.kind {
        facts["ticket"]["kind"] = json!(kind);
    }
    request.facts = facts;
    request.view = view;
    Ok(request)
}

/// What a work turn searches the journal with: the title of its ticket, the paths of its diff
/// or else of its approved plan, and the error lines of the documents that it reads, such as
/// a CI log or the report of a gate. The turns of the session that it resumes are left out,
/// because the session already knows them.
#[allow(clippy::too_many_arguments)]
fn related_input(
    ctx: &Ctx,
    ledger: &Ledger,
    adapter: &crate::adapter::Adapter,
    ticket: &Ticket,
    start: &crate::tick::TurnStart,
    clone: &std::path::Path,
    base: Option<&str>,
    reads: &[String],
) -> Result<RelatedInput> {
    use crate::journal::search::{diff_paths, error_lines, plan_paths};
    let key = &ticket.key;
    let mut input = RelatedInput {
        title: ticket.row.title.clone().filter(|t| !t.trim().is_empty()),
        ..RelatedInput::default()
    };
    if let Some(text) = read_optional(&ctx.paths.ticket_doc(key, "issue.json"))? {
        let value: Value = serde_json::from_str(&text).unwrap_or(Value::Null);
        if let Some(title) = value
            .pointer("/issue/untrusted/title")
            .and_then(Value::as_str)
        {
            input.title = Some(title.to_string());
        }
    }
    if let Some(base) = base {
        if let Ok(names) = crate::util::git::run(clone, &["diff", "--name-only", base, "HEAD"]) {
            input.paths = diff_paths(&names);
        }
    }
    if input.paths.is_empty() {
        if let Some(plan) = read_optional(&ctx.paths.ticket_doc(key, "approved-plan.md"))? {
            input.paths = plan_paths(&plan);
        }
    }
    let gate_docs: Vec<&str> = adapter
        .steps
        .iter()
        .flat_map(|s| s.expects.iter())
        .filter_map(|e| match e {
            crate::adapter::Expect::Gate(gate) => Some(gate.doc.as_str()),
            _ => None,
        })
        .collect();
    let visit = ticket.visit();
    for name in reads.iter().chain(visit.extra.iter()) {
        let lower = name.to_lowercase();
        if !(lower.contains("log") || gate_docs.contains(&name.as_str())) {
            continue;
        }
        if let Some(text) = read_optional(&ctx.paths.ticket_doc(key, name))? {
            for line in error_lines(&text) {
                if input.errors.len() < 3 && !input.errors.contains(&line) {
                    input.errors.push(line);
                }
            }
        }
    }
    if let Some(session) = &start.resumes {
        input.exclude = ledger
            .turns_of_ticket(key.as_str())?
            .into_iter()
            .filter(|t| t.session.as_deref() == Some(session.as_str()))
            .map(|t| t.id)
            .collect();
    }
    Ok(input)
}
