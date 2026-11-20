//! The model of each agent (section 10): the registry of the agents with the defaults and the
//! reason of each one, the short names, and the model and effort that a turn uses.
//!
//! The person sets a model with `vision model set`, which writes `[agents.<agent>]` with
//! [`crate::config::ConfigEditor::set_agent`]. Vision never changes a model by itself.
//!
//! An adapter can define more agents in `[agents.<name>]` of its `adapter.toml`
//! ([`crate::adapter::AdapterAgent`]). Their values take the place of the defaults of Vision,
//! and the configuration of the person wins over them in the same way.

pub mod recommend;

use std::collections::BTreeMap;
use std::fmt::Write as _;

use anyhow::{bail, Result};
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::adapter::AdapterAgent;
use crate::cli::{ModelArgs, ModelCommand};
use crate::config::{AgentPin, Config, ConfigEditor, Effort, Runner};
use crate::ctx::Ctx;
use crate::ledger::turns::ModelChange;
use crate::ledger::{Event, Ledger};
use crate::util::time;

pub use recommend::{recommend, ModelEvidence, Recommendation, MAX_GAP_POINTS, MIN_TURNS};

/// One agent of Vision, with its default model and the reason for it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AgentSpec {
    pub name: &'static str,
    pub model: &'static str,
    pub effort: Effort,
    pub reason: &'static str,
    /// True for the Judge and the Curator: the system prompt is only their own role file.
    /// A work agent gets `agents/common.md` followed by its role file.
    pub alone: bool,
    /// The result schema in `agents/`.
    pub schema: &'static str,
    /// The program of its turns by default.
    pub runner: Runner,
    /// The model with Claude and the effort that take the work of a failed turn on Codex.
    pub fallback: Option<(&'static str, Effort)>,
    /// The name of its role file in `agents/`, without `.md`. Two agents can share one.
    pub role: &'static str,
}

/// The schema of each work turn (section 7.6).
pub const TURN_SCHEMA: &str = "turn.schema.json";

/// The agents and their defaults (section 10).
pub const AGENTS: [AgentSpec; 16] = [
    AgentSpec {
        name: "scout",
        model: "claude-fable-5-2",
        effort: Effort::High,
        reason: "A wrong reading of the ticket costs the full build. Its turns are short.",
        alone: false,
        schema: TURN_SCHEMA,
        runner: Runner::Claude,
        fallback: None,
        role: "scout",
    },
    AgentSpec {
        name: "builder",
        model: "claude-opus-5-5",
        effort: Effort::Xhigh,
        reason: "It writes the code and uses the most tokens. This model costs half as much as the top model.",
        alone: false,
        schema: TURN_SCHEMA,
        runner: Runner::Claude,
        fallback: None,
        role: "builder",
    },
    AgentSpec {
        name: "reviewer",
        model: "claude-opus-5-5",
        effort: Effort::Max,
        reason: "The review is the last gate before a person reads the code.",
        alone: false,
        schema: TURN_SCHEMA,
        runner: Runner::Claude,
        fallback: None,
        role: "reviewer",
    },
    AgentSpec {
        name: "tester",
        model: "claude-opus-5-5",
        effort: Effort::High,
        reason: "A false pass sends a bug to a person.",
        alone: false,
        schema: TURN_SCHEMA,
        runner: Runner::Claude,
        fallback: None,
        role: "tester",
    },
    AgentSpec {
        name: "shepherd",
        model: "claude-opus-5-5",
        effort: Effort::High,
        reason: "It gives verdicts on review comments and fixes the valid ones.",
        alone: false,
        schema: TURN_SCHEMA,
        runner: Runner::Claude,
        fallback: None,
        role: "shepherd",
    },
    AgentSpec {
        name: "merger",
        model: "claude-opus-5-5",
        effort: Effort::High,
        reason: "It resolves the merge conflicts of a deploy and of the base branch. A wrong merge breaks an env that the whole team uses.",
        alone: false,
        schema: TURN_SCHEMA,
        runner: Runner::Claude,
        fallback: None,
        role: "merger",
    },
    AgentSpec {
        name: "judge",
        model: "claude-fable-5-2",
        effort: Effort::High,
        reason: "It can answer alone, so a wrong answer costs the most. Its turns are short.",
        alone: true,
        schema: "judge.schema.json",
        runner: Runner::Claude,
        fallback: None,
        role: "judge",
    },
    AgentSpec {
        name: "curator",
        model: "claude-fable-5-2",
        effort: Effort::High,
        reason: "It runs seldom, and its proposals change each later answer.",
        alone: true,
        schema: "curator.schema.json",
        runner: Runner::Claude,
        fallback: None,
        role: "curator",
    },
    AgentSpec {
        name: "repro",
        model: "claude-opus-5-5",
        effort: Effort::High,
        reason: "It shows the bug with a real failing spec. A false reproduction sends the whole plan the wrong way.",
        alone: false,
        schema: TURN_SCHEMA,
        runner: Runner::Claude,
        fallback: None,
        role: "repro",
    },
    AgentSpec {
        name: "cause",
        model: "gpt-6.1-sol",
        effort: Effort::High,
        reason: "A second model finds the cause on its own, so its guess does not follow the other agents. It only reads.",
        alone: false,
        schema: TURN_SCHEMA,
        runner: Runner::Codex,
        fallback: Some(("claude-opus-5-5", Effort::High)),
        role: "cause",
    },
    AgentSpec {
        name: "history",
        model: "claude-sonnet-5-5",
        effort: Effort::Medium,
        reason: "It reads commit messages and blame output, which needs little reasoning, so its turns stay cheap.",
        alone: false,
        schema: TURN_SCHEMA,
        runner: Runner::Claude,
        fallback: None,
        role: "history",
    },
    AgentSpec {
        name: "points",
        model: "claude-opus-5-5",
        effort: Effort::High,
        reason: "It tests each point of the ticket against the code. A point that it misses costs a second build.",
        alone: false,
        schema: TURN_SCHEMA,
        runner: Runner::Claude,
        fallback: None,
        role: "points",
    },
    AgentSpec {
        name: "scope",
        model: "claude-opus-5-5",
        effort: Effort::High,
        reason: "It finds each place that a chore touches. A place that it misses breaks the build later.",
        alone: false,
        schema: TURN_SCHEMA,
        runner: Runner::Claude,
        fallback: None,
        role: "scope",
    },
    AgentSpec {
        name: "analyst",
        model: "claude-opus-5-5",
        effort: Effort::Xhigh,
        reason: "It checks each finding against the code and writes the plan and the questions. The plan decides the whole build.",
        alone: false,
        schema: TURN_SCHEMA,
        runner: Runner::Claude,
        fallback: None,
        role: "analyst",
    },
    AgentSpec {
        name: "critic",
        model: "gpt-6.1-sol",
        effort: Effort::Xhigh,
        reason: "A wide plan touches many files, the database, or security. A second model at its top effort finds what the plan missed.",
        alone: false,
        schema: TURN_SCHEMA,
        runner: Runner::Codex,
        fallback: Some(("claude-opus-5-5", Effort::High)),
        role: "critic",
    },
    AgentSpec {
        name: "critic-narrow",
        model: "gpt-6.1-sol",
        effort: Effort::High,
        reason: "A narrow plan has less to miss, so the critic reads it one notch lower.",
        alone: false,
        schema: TURN_SCHEMA,
        runner: Runner::Codex,
        fallback: Some(("claude-opus-5-5", Effort::High)),
        role: "critic",
    },
];

/// The short names of `vision model set` and their full model ids.
pub const SHORT_NAMES: [(&str, &str); 4] = [
    ("fable", "claude-fable-5-2"),
    ("opus", "claude-opus-5-5"),
    ("sonnet", "claude-sonnet-5-5"),
    ("haiku", "claude-haiku-5-5"),
];

/// The agent of this name, or `None`.
pub fn agent(name: &str) -> Option<&'static AgentSpec> {
    AGENTS.iter().find(|a| a.name == name)
}

/// The agent of this name, or an error that lists the agents.
pub fn require_agent(name: &str) -> Result<&'static AgentSpec> {
    agent(name).ok_or_else(|| {
        anyhow::anyhow!(
            "`{name}` is not an agent of Vision. The agents are {}",
            agent_names().join(", ")
        )
    })
}

/// The names of the agents, in the order of section 10.
pub fn agent_names() -> Vec<&'static str> {
    AGENTS.iter().map(|a| a.name).collect()
}

/// The agents that an adapter defines, by name.
pub type Defined = BTreeMap<String, AdapterAgent>;

/// The defaults of one agent: those of Vision, or those that the adapter defines.
#[derive(Clone, Debug, PartialEq)]
pub struct Defaults {
    pub name: String,
    pub model: String,
    pub effort: Effort,
    pub runner: Runner,
    /// The fallback on Claude of an agent on Codex.
    pub fallback: Option<Fallback>,
    pub reason: String,
    /// True for an agent that the adapter defines.
    pub adapter: bool,
    /// True for the Judge and the Curator.
    pub alone: bool,
}

/// The defaults of an agent of Vision or of `defined`, or an error that lists the agents.
pub fn defaults(defined: &Defined, name: &str) -> Result<Defaults> {
    if let Some(spec) = agent(name) {
        return Ok(Defaults {
            name: spec.name.to_string(),
            model: spec.model.to_string(),
            effort: spec.effort,
            runner: spec.runner,
            fallback: spec.fallback.map(|(model, effort)| Fallback {
                model: model.to_string(),
                effort,
            }),
            reason: spec.reason.to_string(),
            adapter: false,
            alone: spec.alone,
        });
    }
    if let Some(given) = defined.get(name) {
        return Ok(Defaults {
            name: given.name.clone(),
            model: given.model.clone(),
            effort: given.effort,
            runner: given.runner,
            fallback: given.fallback.clone(),
            reason: given.reason.clone(),
            adapter: true,
            alone: false,
        });
    }
    bail!(
        "`{name}` is not an agent of Vision or of the adapter. The agents are {}",
        names_in(defined).join(", ")
    )
}

/// The names of the agents: those of Vision in the order of section 10, then those of the
/// adapter by name.
pub fn names_in(defined: &Defined) -> Vec<String> {
    AGENTS
        .iter()
        .map(|a| a.name.to_string())
        .chain(defined.keys().cloned())
        .collect()
}

/// The agents that the adapter of the configuration defines. A broken adapter gives none, with
/// its error, so that the agents of Vision still show.
pub fn defined_agents(ctx: &Ctx) -> (Defined, Option<String>) {
    match crate::adapter::load(ctx) {
        Ok(adapter) => (adapter.agents, None),
        Err(e) => (Defined::new(), Some(format!("{e:#}"))),
    }
}

/// `vision` for an agent of Vision, `adapter` for any other one: only an adapter defines an
/// agent that Vision does not know.
pub fn origin_of(name: &str) -> &'static str {
    if agent(name).is_some() {
        "vision"
    } else {
        "adapter"
    }
}

/// A full model id for a short name, or the id itself after a test of its form.
pub fn full_model_id(model: &str) -> Result<String> {
    let model = model.trim();
    if let Some((_, full)) = SHORT_NAMES
        .iter()
        .find(|(short, _)| short.eq_ignore_ascii_case(model))
    {
        return Ok((*full).to_string());
    }
    let valid = !model.is_empty()
        && model.len() <= 100
        && model.starts_with(|c: char| c.is_ascii_alphanumeric())
        && model.chars().all(|c| {
            c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | '[' | ']' | ':' | '@')
        });
    if !valid {
        bail!(
            "`{model}` is not a model id. Use a full id such as claude-opus-5-5, or one of {}",
            SHORT_NAMES.map(|(short, _)| short).join(", ")
        );
    }
    Ok(model.to_string())
}

/// Where the model of an agent comes from.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum Source {
    /// The default of Vision.
    Default,
    /// The values of `[agents.<agent>]` of the adapter.
    Adapter,
    /// `[agents.<agent>]` of the configuration, pinned at this RFC 3339 time when it is known.
    Yours { since: Option<String> },
}

impl Source {
    /// `default`, `adapter`, or `yours since 2026-11-16`.
    pub fn text(&self) -> String {
        match self {
            Source::Default => "default".to_string(),
            Source::Adapter => "adapter".to_string(),
            Source::Yours { since: Some(at) } => {
                format!("yours since {}", at.get(..10).unwrap_or(at))
            }
            Source::Yours { since: None } => "yours".to_string(),
        }
    }
}

/// The model and the effort that the turns of an agent use.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ModelChoice {
    pub agent: String,
    pub model: String,
    pub effort: Effort,
    pub source: Source,
    /// The program of the turns.
    pub runner: Runner,
    /// The model with Claude of a failed turn on Codex. `None` for an agent on Claude.
    pub fallback: Option<Fallback>,
}

/// The model with Claude and the effort that take the work of a failed turn on Codex.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Fallback {
    pub model: String,
    pub effort: Effort,
}

impl std::fmt::Display for Fallback {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} {}", self.model, self.effort)
    }
}

/// The model and the effort of an agent of Vision: the pin of the person, else the default. A
/// pin can name only the model or only the effort; the other value is the default.
pub fn choice(config: &Config, name: &str) -> Result<ModelChoice> {
    choice_in(config, &Defined::new(), name)
}

/// The model and the effort of an agent of Vision or of `defined`: the pin of the person, else
/// the default of Vision or the values of the adapter.
pub fn choice_in(config: &Config, defined: &Defined, name: &str) -> Result<ModelChoice> {
    let base = defaults(defined, name)?;
    let default_source = if base.adapter {
        Source::Adapter
    } else {
        Source::Default
    };
    let Some(pin) = config.agents.get(name) else {
        return Ok(ModelChoice {
            agent: name.to_string(),
            model: base.model,
            effort: base.effort,
            source: default_source,
            runner: base.runner,
            fallback: match base.runner {
                Runner::Codex => base.fallback,
                Runner::Claude => None,
            },
        });
    };
    let model = match pin.model.as_deref() {
        Some(model) => full_model_id(model)?,
        None => base.model.clone(),
    };
    let runner = pin.runner.unwrap_or(base.runner);
    let fallback = match (runner, &pin.fallback) {
        (Runner::Claude, _) => None,
        (Runner::Codex, Some(given)) => Some(Fallback {
            model: full_model_id(&given.model)?,
            effort: given
                .effort
                .or(base.fallback.as_ref().map(|f| f.effort))
                .unwrap_or(Effort::High),
        }),
        (Runner::Codex, None) => base.fallback.clone(),
    };
    Ok(ModelChoice {
        agent: name.to_string(),
        model,
        effort: pin.effort.unwrap_or(base.effort),
        source: if pin.model.is_some()
            || pin.effort.is_some()
            || pin.runner.is_some()
            || pin.fallback.is_some()
        {
            Source::Yours {
                since: pin.pinned_at.clone(),
            }
        } else {
            default_source
        },
        runner,
        fallback,
    })
}

/// The role file of an agent in `agents/`, without `.md`: its own name, or the name of the role
/// that it shares, such as `critic` for `critic-narrow`.
pub fn role_name(name: &str) -> &str {
    agent(name).map_or(name, |spec| spec.role)
}

/// `vision model`, `model set`, `model reset`, `model recommend`, and `model eval`.
pub fn run(ctx: &Ctx, args: ModelArgs) -> Result<()> {
    match args.command {
        None => show(ctx, args.json),
        Some(ModelCommand::Set {
            agent,
            model,
            effort,
            runner,
        }) => set(ctx, &agent, &model, effort, runner),
        Some(ModelCommand::Reset { agent }) => reset(ctx, &agent),
        Some(ModelCommand::Recommend { agent, json }) => {
            recommend_command(ctx, agent.as_deref(), json || args.json)
        }
        Some(ModelCommand::Eval {
            agent,
            models,
            n,
            json,
        }) => crate::curator::eval(ctx, &agent, &models, n, json || args.json),
    }
}

/// One row of `vision model`.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct ModelRow {
    pub agent: String,
    pub model: String,
    pub effort: Effort,
    /// `default`, or `yours since <date>`.
    pub source: String,
    pub pinned_at: Option<String>,
    pub recommendation: String,
    pub recommended_model: Option<String>,
    /// The reason of the default of Vision.
    pub reason: String,
    /// The program of the turns: `claude` or `codex`.
    pub runner: Runner,
    /// The model with Claude of a failed turn on Codex, such as `claude-opus-5-5 high`.
    pub fallback: Option<String>,
    /// The default model and effort: those of Vision, or those of the adapter.
    pub default_model: String,
    pub default_effort: Effort,
    /// `vision` for an agent of Vision, `adapter` for an agent that the adapter defines.
    pub defined_by: String,
}

/// The rows of `vision model`: the agents of Vision in the order of section 10, then the
/// agents of the adapter by name.
pub fn rows_in(config: &Config, defined: &Defined, ledger: &Ledger) -> Result<Vec<ModelRow>> {
    let mut rows = Vec::new();
    for name in names_in(defined) {
        let base = defaults(defined, &name)?;
        let choice = choice_in(config, defined, &name)?;
        let advice = recommend(ledger, &choice)?;
        rows.push(ModelRow {
            agent: name,
            model: choice.model.clone(),
            effort: choice.effort,
            source: choice.source.text(),
            pinned_at: match &choice.source {
                Source::Yours { since } => since.clone(),
                Source::Default | Source::Adapter => None,
            },
            recommendation: advice.text,
            recommended_model: advice.recommended,
            reason: base.reason,
            runner: choice.runner,
            fallback: choice.fallback.as_ref().map(ToString::to_string),
            default_model: base.model,
            default_effort: base.effort,
            defined_by: if base.adapter { "adapter" } else { "vision" }.to_string(),
        });
    }
    Ok(rows)
}

/// `vision model [--json]`. A broken adapter does not hide the agents of Vision: a line on
/// standard error says why the agents of the adapter are missing.
fn show(ctx: &Ctx, json: bool) -> Result<()> {
    ctx.require_setup()?;
    let (defined, problem) = defined_agents(ctx);
    if let Some(problem) = problem {
        eprintln!("The agents of the adapter are not listed, because the adapter does not load: {problem}");
    }
    let rows = rows_in(ctx.config()?, &defined, &ctx.ledger()?)?;
    if json {
        println!("{}", serde_json::to_string_pretty(&rows)?);
        return Ok(());
    }
    print!("{}", render_model_rows(&rows));
    Ok(())
}

/// The width of the text of `vision model`.
pub const WIDTH: usize = 100;

/// The text of `vision model`: one row for each agent with its model, its effort, and its
/// source, and under it the reason of the default and the recommendation, each wrapped to
/// [`WIDTH`] columns.
pub fn render_model_rows(rows: &[ModelRow]) -> String {
    let mut table: Vec<Vec<String>> = vec![["AGENT", "MODEL", "EFFORT", "SOURCE"]
        .map(String::from)
        .to_vec()];
    for row in rows {
        // A turn on Codex says so next to its model.
        let model = match row.runner {
            Runner::Claude => row.model.clone(),
            Runner::Codex => format!("{} on codex", row.model),
        };
        table.push(vec![
            row.agent.clone(),
            model,
            row.effort.to_string(),
            row.source.clone(),
        ]);
    }
    let lines: Vec<String> = render_rows(&table).lines().map(str::to_string).collect();
    let mut out = String::new();
    let _ = writeln!(out, "{}", lines[0]);
    for (row, line) in rows.iter().zip(&lines[1..]) {
        let _ = writeln!(out, "{line}");
        // A pinned agent shows the default that it left, so that the reason reads right.
        let adapter = row.defined_by == "adapter";
        let pinned = row.source != Source::Default.text() && row.source != Source::Adapter.text();
        let why = if row.reason.is_empty() {
            "the adapter gives no reason"
        } else {
            row.reason.as_str()
        };
        let reason = match (adapter, pinned) {
            (false, true) => format!(
                "reason of the default {} {}: {why}",
                row.default_model, row.default_effort
            ),
            (false, false) => format!("reason of the default: {why}"),
            (true, true) => format!(
                "reason of the default of the adapter {} {}: {why}",
                row.default_model, row.default_effort
            ),
            (true, false) => format!("reason of the adapter: {why}"),
        };
        let mut texts = vec![reason];
        if let Some(fallback) = &row.fallback {
            texts.push(format!("fallback on claude: {fallback}"));
        }
        texts.push(format!("recommendation: {}", row.recommendation));
        for text in texts {
            for part in wrap(&text, "  ", "    ", WIDTH) {
                let _ = writeln!(out, "{part}");
            }
        }
    }
    out
}

/// The lines of `text` with at most `width` characters each: the first one starts with
/// `first`, each later one with `rest`. A word longer than a line stays whole.
fn wrap(text: &str, first: &str, rest: &str, width: usize) -> Vec<String> {
    let mut lines = Vec::new();
    let mut line = first.to_string();
    let mut empty = true;
    for word in text.split_whitespace() {
        let fits = line.chars().count() + usize::from(!empty) + word.chars().count() <= width;
        if !empty && !fits {
            lines.push(std::mem::replace(&mut line, rest.to_string()));
            empty = true;
        }
        if !empty {
            line.push(' ');
        }
        line.push_str(word);
        empty = false;
    }
    lines.push(line);
    lines
}

/// `vision model set AGENT MODEL [--effort E] [--runner R]`: pins the model in
/// `[agents.<agent>]` with `pinned_at`, and records the change in `model_changes`. Without
/// `--effort`, a pinned effort stays, else the effort follows the default. Without `--runner`,
/// a Claude model (a short name or an id that starts with `claude-`) runs on Claude, and any
/// other model keeps the runner of the agent. An agent of the adapter takes a pin as an agent
/// of Vision does.
fn set(
    ctx: &Ctx,
    agent: &str,
    model: &str,
    effort: Option<Effort>,
    runner: Option<Runner>,
) -> Result<()> {
    ctx.require_setup()?;
    let defined = known_agents(ctx, agent)?;
    let base = defaults(&defined, agent)?;
    let model = full_model_id(model)?;
    let config = ctx.config()?;
    let old = choice_in(config, &defined, agent)?;
    let effort = effort.or_else(|| config.agents.get(agent).and_then(|p| p.effort));
    let runner = runner.unwrap_or(if model.starts_with("claude-") {
        Runner::Claude
    } else {
        old.runner
    });
    if runner == Runner::Codex && base.alone {
        bail!("{agent} cannot run on Codex: only a reader turn of a work agent runs on Codex");
    }
    let at = time::now_rfc3339();
    let pinned = config.agents.get(agent);
    let pin = AgentPin {
        model: Some(model.clone()),
        effort,
        runner: (runner != base.runner || pinned.and_then(|p| p.runner).is_some())
            .then_some(runner),
        fallback: pinned.and_then(|p| p.fallback.clone()),
        pinned_at: Some(at.clone()),
    };
    let mut editor = ConfigEditor::open(&ctx.paths.config())?;
    editor.set_agent(agent, &pin)?;
    editor.save()?;
    let effort = effort.unwrap_or(base.effort);
    record(ctx, agent, &model, effort, &old, "set", &at)?;
    let on = |r: Runner| match r {
        Runner::Claude => String::new(),
        Runner::Codex => " on codex".to_string(),
    };
    println!(
        "{agent} now uses {model}{} with the effort {effort} (it used {}{} {}). The value stays until you change it.",
        on(runner),
        old.model,
        on(old.runner),
        old.effort
    );
    Ok(())
}

/// The agents of the adapter, for a command that names `agent`. An agent of Vision needs no
/// adapter, so a broken adapter stops only a command for an agent that Vision does not know.
fn known_agents(ctx: &Ctx, agent: &str) -> Result<Defined> {
    let (defined, problem) = defined_agents(ctx);
    if let (None, Some(problem)) = (crate::models::agent(agent), problem) {
        bail!("`{agent}` is not an agent of Vision, and the adapter does not load, so its agents are unknown: {problem}");
    }
    Ok(defined)
}

/// `vision model reset AGENT`: removes `[agents.<agent>]`, so that the default applies again:
/// the default of Vision, or the values of the adapter for an agent of the adapter.
fn reset(ctx: &Ctx, agent: &str) -> Result<()> {
    ctx.require_setup()?;
    let defined = known_agents(ctx, agent)?;
    let base = defaults(&defined, agent)?;
    let of = if base.adapter {
        "the adapter"
    } else {
        "Vision"
    };
    let old = choice_in(ctx.config()?, &defined, agent)?;
    let mut editor = ConfigEditor::open(&ctx.paths.config())?;
    if !editor.remove_agent(agent)? {
        println!(
            "{agent} already uses the default of {of}: {} {}.",
            base.model, base.effort
        );
        return Ok(());
    }
    editor.save()?;
    let at = time::now_rfc3339();
    record(ctx, agent, &base.model, base.effort, &old, "reset", &at)?;
    println!(
        "{agent} uses the default of {of} again: {} {} (it used {} {}).",
        base.model, base.effort, old.model, old.effort
    );
    Ok(())
}

fn record(
    ctx: &Ctx,
    agent: &str,
    model: &str,
    effort: Effort,
    old: &ModelChoice,
    source: &str,
    at: &str,
) -> Result<()> {
    let ledger = ctx.ledger()?;
    ledger.add_model_change(&ModelChange {
        at,
        agent,
        model,
        effort: effort.as_str(),
        old_model: &old.model,
        old_effort: old.effort.as_str(),
        source,
    })?;
    ledger.add_event(
        &Event::now("user", &format!("model-{source}")).detail(json!({
            "agent": agent, "model": model, "effort": effort,
            "old_model": old.model, "old_effort": old.effort,
        })),
    )?;
    ctx.log(&format!(
        "model {source}: {agent} {} {} -> {model} {effort}",
        old.model, old.effort
    ));
    Ok(())
}

/// `vision model recommend [AGENT] [--json]`.
fn recommend_command(ctx: &Ctx, agent: Option<&str>, json: bool) -> Result<()> {
    ctx.require_setup()?;
    let config = ctx.config()?;
    let ledger = ctx.ledger()?;
    let defined = match agent {
        Some(name) => known_agents(ctx, name)?,
        None => defined_agents(ctx).0,
    };
    let names: Vec<String> = match agent {
        Some(name) => vec![defaults(&defined, name)?.name],
        None => names_in(&defined),
    };
    let mut all = Vec::new();
    for name in names {
        all.push(recommend(&ledger, &choice_in(config, &defined, &name)?)?);
    }
    if json {
        println!("{}", serde_json::to_string_pretty(&all)?);
        return Ok(());
    }
    let mut out = String::new();
    for advice in &all {
        let _ = writeln!(out, "{} (uses {})", advice.agent, advice.current);
        let counted: Vec<&ModelEvidence> = advice.models.iter().filter(|m| m.counts).collect();
        if !counted.is_empty() {
            let extra_names: Vec<String> =
                counted[0].extra.iter().map(|(n, _)| n.clone()).collect();
            let mut header = vec![
                "MODEL".to_string(),
                "TURNS".into(),
                "COST/TURN".into(),
                "FIRST TRY".into(),
            ];
            header.extend(extra_names.iter().map(|n| n.to_uppercase()));
            let mut table = vec![header];
            for m in &counted {
                let mut row = vec![
                    m.model.clone(),
                    m.turns.to_string(),
                    format!("${:.2}", m.cost_per_turn),
                    m.first_try.map_or("-".into(), |q| format!("{q:.0}%")),
                ];
                row.extend(m.extra.iter().map(|(_, v)| format!("{v:.1}")));
                table.push(row);
            }
            for line in render_rows(&table).lines() {
                let _ = writeln!(out, "  {line}");
            }
        }
        let few: Vec<String> = advice
            .models
            .iter()
            .filter(|m| !m.counts)
            .map(|m| format!("{} ({})", m.model, m.turns))
            .collect();
        if !few.is_empty() {
            let _ = writeln!(out, "  fewer than {MIN_TURNS} turns: {}", few.join(", "));
        }
        let _ = writeln!(out, "  recommendation: {}", advice.text);
        if let Some(line) = crate::curator::last_eval_line(&ledger, &advice.agent)? {
            let _ = writeln!(out, "  {line}");
        }
        out.push('\n');
    }
    print!("{out}");
    Ok(())
}

/// A table with aligned columns; the rows may differ in length.
fn render_rows(rows: &[Vec<String>]) -> String {
    let columns = rows.iter().map(Vec::len).max().unwrap_or(0);
    let widths: Vec<usize> = (0..columns)
        .map(|i| {
            rows.iter()
                .filter_map(|r| r.get(i))
                .map(|c| c.chars().count())
                .max()
                .unwrap_or(0)
        })
        .collect();
    let mut out = String::new();
    for row in rows {
        let cells: Vec<String> = row
            .iter()
            .zip(&widths)
            .map(|(cell, width)| format!("{cell:<width$}"))
            .collect();
        let _ = writeln!(out, "{}", cells.join("  ").trim_end());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::AgentPin;

    #[test]
    fn each_agent_has_the_default_of_section_10() {
        let config = Config::default();
        let builder = choice(&config, "builder").unwrap();
        assert_eq!(
            (builder.model.as_str(), builder.effort),
            ("claude-opus-5-5", Effort::Xhigh)
        );
        assert_eq!(builder.source, Source::Default);
        assert_eq!(choice(&config, "reviewer").unwrap().effort, Effort::Max);
        assert!(agent("judge").unwrap().alone);
        assert!(!agent("scout").unwrap().alone);
        assert!(choice(&config, "nobody").is_err());
    }

    fn lane() -> AdapterAgent {
        AdapterAgent {
            name: "cr-codex".into(),
            role: "roles/cr-codex.md".into(),
            model: "gpt-6.1-sol".into(),
            effort: Effort::High,
            runner: Runner::Codex,
            fallback: Some(Fallback {
                model: "claude-opus-5-5".into(),
                effort: Effort::High,
            }),
            reason: "A second model reads the diff.".into(),
        }
    }

    #[test]
    fn an_agent_of_the_adapter_takes_its_values_and_a_pin_wins_over_them() {
        let defined = Defined::from([("cr-codex".to_string(), lane())]);
        let mut config = Config::default();
        let codex = choice_in(&config, &defined, "cr-codex").unwrap();
        assert_eq!(
            (codex.model.as_str(), codex.effort, codex.runner),
            ("gpt-6.1-sol", Effort::High, Runner::Codex)
        );
        assert_eq!(codex.source, Source::Adapter);
        assert_eq!(codex.source.text(), "adapter");
        assert_eq!(codex.fallback.unwrap().model, "claude-opus-5-5");
        assert!(
            choice(&config, "cr-codex").is_err(),
            "Vision alone does not know it"
        );
        let err = choice_in(&config, &defined, "cr-nobody")
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("not an agent of Vision or of the adapter"),
            "{err}"
        );
        assert!(err.ends_with("critic-narrow, cr-codex"), "{err}");
        // A pin of only the effort keeps the model and the runner of the adapter.
        config.agents.insert(
            "cr-codex".into(),
            AgentPin {
                effort: Some(Effort::Xhigh),
                pinned_at: Some("2026-11-16T14:30:00Z".into()),
                ..AgentPin::default()
            },
        );
        let pinned = choice_in(&config, &defined, "cr-codex").unwrap();
        assert_eq!(
            (pinned.model.as_str(), pinned.effort, pinned.runner),
            ("gpt-6.1-sol", Effort::Xhigh, Runner::Codex)
        );
        assert_eq!(pinned.source.text(), "yours since 2026-11-16");
        // A pin of the runner claude drops the fallback.
        config.agents.get_mut("cr-codex").unwrap().runner = Some(Runner::Claude);
        assert_eq!(
            choice_in(&config, &defined, "cr-codex").unwrap().fallback,
            None
        );
        assert_eq!(
            names_in(&defined).last().map(String::as_str),
            Some("cr-codex")
        );
        assert_eq!(origin_of("cr-codex"), "adapter");
        assert_eq!(origin_of("builder"), "vision");
        let base = defaults(&defined, "cr-codex").unwrap();
        assert!(base.adapter && !base.alone);
    }

    #[test]
    fn a_pin_wins_and_a_missing_value_keeps_its_default() {
        let mut config = Config::default();
        config.agents.insert(
            "judge".into(),
            AgentPin {
                model: Some("opus".into()),
                effort: None,
                pinned_at: Some("2026-11-16T14:30:00Z".into()),
                ..AgentPin::default()
            },
        );
        let judge = choice(&config, "judge").unwrap();
        assert_eq!(judge.model, "claude-opus-5-5");
        assert_eq!(judge.effort, Effort::High);
        assert_eq!(judge.source.text(), "yours since 2026-11-16");
    }

    #[test]
    fn the_model_table_fits_100_columns_and_keeps_each_fact() {
        let mut config = Config::default();
        config.agents.insert(
            "judge".into(),
            AgentPin {
                model: Some("claude-haiku-5-5[1m]".into()),
                effort: Some(Effort::Low),
                pinned_at: Some("2026-11-16T14:30:00Z".into()),
                ..AgentPin::default()
            },
        );
        let rows: Vec<ModelRow> = AGENTS
            .iter()
            .map(|spec| {
                let choice = choice(&config, spec.name).unwrap();
                ModelRow {
                    agent: spec.name.to_string(),
                    model: choice.model.clone(),
                    effort: choice.effort,
                    source: choice.source.text(),
                    pinned_at: None,
                    recommendation:
                        "fewer than 10 measured turns for a second model, so the default stays"
                            .into(),
                    recommended_model: None,
                    reason: spec.reason.to_string(),
                    runner: choice.runner,
                    fallback: choice.fallback.as_ref().map(ToString::to_string),
                    default_model: spec.model.to_string(),
                    default_effort: spec.effort,
                    defined_by: "vision".into(),
                }
            })
            .collect();
        let text = render_model_rows(&rows);
        for line in text.lines() {
            assert!(line.chars().count() <= WIDTH, "{line}");
        }
        let lines: Vec<&str> = text.lines().collect();
        assert!(lines[0].starts_with("AGENT"), "{text}");
        let judge = lines.iter().position(|l| l.starts_with("judge ")).unwrap();
        let cells: Vec<&str> = lines[judge].split_whitespace().collect();
        assert_eq!(
            cells,
            [
                "judge",
                "claude-haiku-5-5[1m]",
                "low",
                "yours",
                "since",
                "2026-11-16"
            ]
        );
        let below = lines[judge + 1..].join("\n");
        assert!(
            below.starts_with("  reason of the default claude-fable-5-2 high: It can answer alone"),
            "{text}"
        );
        let builder = lines
            .iter()
            .position(|l| l.starts_with("builder "))
            .unwrap();
        assert!(lines[builder].ends_with("default"), "{text}");
        let reason = format!("{} {}", lines[builder + 1], lines[builder + 2].trim());
        assert_eq!(
            reason,
            format!(
                "  reason of the default: {}",
                agent("builder").unwrap().reason
            )
        );
        assert!(text.contains("  recommendation: fewer than 10 measured turns"));
    }

    #[test]
    fn wrap_keeps_each_word() {
        let lines = wrap("one two three four", "> ", "  ", 9);
        assert_eq!(lines, ["> one two", "  three", "  four"]);
        assert_eq!(wrap("averyverylongword", "", "", 4), ["averyverylongword"]);
    }

    #[test]
    fn short_names_become_full_ids() {
        assert_eq!(full_model_id("haiku").unwrap(), "claude-haiku-5-5");
        assert_eq!(full_model_id("Sonnet").unwrap(), "claude-sonnet-5-5");
        assert_eq!(
            full_model_id("claude-opus-5-5[1m]").unwrap(),
            "claude-opus-5-5[1m]"
        );
        for bad in ["", "two words", "x\"y", "-dash"] {
            assert!(full_model_id(bad).is_err(), "{bad}");
        }
    }
}
