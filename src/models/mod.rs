//! The model of each agent (section 10): the registry of the agents with the defaults and the
//! reason of each one, the short names, and the model and effort that a turn uses.
//!
//! The person sets a model with `vision model set`, which writes `[agents.<agent>]` with
//! [`crate::config::ConfigEditor::set_agent`]. Vision never changes a model by itself.

pub mod recommend;

use std::fmt::Write as _;

use anyhow::{bail, Result};
use serde::Serialize;
use serde_json::json;

use crate::cli::{ModelArgs, ModelCommand};
use crate::config::{AgentPin, Config, ConfigEditor, Effort};
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
}

/// The schema of each work turn (section 7.6).
pub const TURN_SCHEMA: &str = "turn.schema.json";

/// The agents and their defaults (section 10).
pub const AGENTS: [AgentSpec; 7] = [
    AgentSpec {
        name: "scout",
        model: "claude-fable-5-2",
        effort: Effort::High,
        reason: "A wrong reading of the ticket costs the full build. Its turns are short.",
        alone: false,
        schema: TURN_SCHEMA,
    },
    AgentSpec {
        name: "builder",
        model: "claude-opus-5-5",
        effort: Effort::Xhigh,
        reason: "It writes the code and uses the most tokens. This model costs half as much as the top model.",
        alone: false,
        schema: TURN_SCHEMA,
    },
    AgentSpec {
        name: "reviewer",
        model: "claude-opus-5-5",
        effort: Effort::Max,
        reason: "The review is the last gate before a person reads the code.",
        alone: false,
        schema: TURN_SCHEMA,
    },
    AgentSpec {
        name: "tester",
        model: "claude-opus-5-5",
        effort: Effort::High,
        reason: "A false pass sends a bug to a person.",
        alone: false,
        schema: TURN_SCHEMA,
    },
    AgentSpec {
        name: "shepherd",
        model: "claude-opus-5-5",
        effort: Effort::High,
        reason: "It gives verdicts on review comments and fixes the valid ones.",
        alone: false,
        schema: TURN_SCHEMA,
    },
    AgentSpec {
        name: "judge",
        model: "claude-fable-5-2",
        effort: Effort::High,
        reason: "It can answer alone, so a wrong answer costs the most. Its turns are short.",
        alone: true,
        schema: "judge.schema.json",
    },
    AgentSpec {
        name: "curator",
        model: "claude-fable-5-2",
        effort: Effort::High,
        reason: "It runs seldom, and its proposals change each later answer.",
        alone: true,
        schema: "curator.schema.json",
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
    /// `[agents.<agent>]` of the configuration, pinned at this RFC 3339 time when it is known.
    Yours { since: Option<String> },
}

impl Source {
    /// `default`, or `yours since 2026-11-16`.
    pub fn text(&self) -> String {
        match self {
            Source::Default => "default".to_string(),
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
}

/// The model and the effort of an agent: the pin of the person, else the default. A pin may
/// name only the model or only the effort; the other value is the default.
pub fn choice(config: &Config, name: &str) -> Result<ModelChoice> {
    let spec = require_agent(name)?;
    let Some(pin) = config.agents.get(name) else {
        return Ok(ModelChoice {
            agent: name.to_string(),
            model: spec.model.to_string(),
            effort: spec.effort,
            source: Source::Default,
        });
    };
    let model = match pin.model.as_deref() {
        Some(model) => full_model_id(model)?,
        None => spec.model.to_string(),
    };
    Ok(ModelChoice {
        agent: name.to_string(),
        model,
        effort: pin.effort.unwrap_or(spec.effort),
        source: if pin.model.is_some() || pin.effort.is_some() {
            Source::Yours {
                since: pin.pinned_at.clone(),
            }
        } else {
            Source::Default
        },
    })
}

/// `vision model`, `model set`, `model reset`, `model recommend`, and `model eval`.
pub fn run(ctx: &Ctx, args: ModelArgs) -> Result<()> {
    match args.command {
        None => show(ctx, args.json),
        Some(ModelCommand::Set {
            agent,
            model,
            effort,
        }) => set(ctx, &agent, &model, effort),
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
}

/// The rows of `vision model`, in the order of section 10.
pub fn rows(config: &Config, ledger: &Ledger) -> Result<Vec<ModelRow>> {
    let mut rows = Vec::new();
    for spec in &AGENTS {
        let choice = choice(config, spec.name)?;
        let advice = recommend(ledger, &choice)?;
        rows.push(ModelRow {
            agent: spec.name.to_string(),
            model: choice.model.clone(),
            effort: choice.effort,
            source: choice.source.text(),
            pinned_at: match &choice.source {
                Source::Yours { since } => since.clone(),
                Source::Default => None,
            },
            recommendation: advice.text,
            recommended_model: advice.recommended,
            reason: spec.reason.to_string(),
        });
    }
    Ok(rows)
}

/// `vision model [--json]`.
fn show(ctx: &Ctx, json: bool) -> Result<()> {
    ctx.require_setup()?;
    let rows = rows(ctx.config()?, &ctx.ledger()?)?;
    if json {
        println!("{}", serde_json::to_string_pretty(&rows)?);
        return Ok(());
    }
    let mut table: Vec<Vec<String>> = vec![[
        "AGENT",
        "MODEL",
        "EFFORT",
        "SOURCE",
        "RECOMMENDATION",
        "REASON",
    ]
    .map(String::from)
    .to_vec()];
    for row in &rows {
        // A pinned agent shows the default that it left, so that the reason reads right.
        let reason = match agent(&row.agent) {
            Some(spec) if row.source != Source::Default.text() => {
                format!("default: {} {}. {}", spec.model, spec.effort, row.reason)
            }
            _ => row.reason.clone(),
        };
        table.push(vec![
            row.agent.clone(),
            row.model.clone(),
            row.effort.to_string(),
            row.source.clone(),
            row.recommendation.clone(),
            reason,
        ]);
    }
    print!("{}", render_rows(&table));
    Ok(())
}

/// `vision model set AGENT MODEL [--effort E]`: pins the model in `[agents.<agent>]` with
/// `pinned_at`, and records the change in `model_changes`. Without `--effort`, a pinned effort
/// stays, else the effort follows the default.
fn set(ctx: &Ctx, agent: &str, model: &str, effort: Option<Effort>) -> Result<()> {
    ctx.require_setup()?;
    let spec = require_agent(agent)?;
    let model = full_model_id(model)?;
    let config = ctx.config()?;
    let old = choice(config, agent)?;
    let effort = effort.or_else(|| config.agents.get(agent).and_then(|p| p.effort));
    let at = time::now_rfc3339();
    let pin = AgentPin {
        model: Some(model.clone()),
        effort,
        pinned_at: Some(at.clone()),
    };
    let mut editor = ConfigEditor::open(&ctx.paths.config())?;
    editor.set_agent(agent, &pin)?;
    editor.save()?;
    let effort = effort.unwrap_or(spec.effort);
    record(ctx, agent, &model, effort, &old, "set", &at)?;
    println!(
        "{agent} now uses {model} with the effort {effort} (it used {} {}). The value stays until you change it.",
        old.model, old.effort
    );
    Ok(())
}

/// `vision model reset AGENT`: removes `[agents.<agent>]`, so that the default applies again.
fn reset(ctx: &Ctx, agent: &str) -> Result<()> {
    ctx.require_setup()?;
    let spec = require_agent(agent)?;
    let old = choice(ctx.config()?, agent)?;
    let mut editor = ConfigEditor::open(&ctx.paths.config())?;
    if !editor.remove_agent(agent)? {
        println!(
            "{agent} already uses the default: {} {}.",
            spec.model, spec.effort
        );
        return Ok(());
    }
    editor.save()?;
    let at = time::now_rfc3339();
    record(ctx, agent, spec.model, spec.effort, &old, "reset", &at)?;
    println!(
        "{agent} uses the default of Vision again: {} {} (it used {} {}).",
        spec.model, spec.effort, old.model, old.effort
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
    let names: Vec<&str> = match agent {
        Some(name) => vec![require_agent(name)?.name],
        None => agent_names(),
    };
    let mut all = Vec::new();
    for name in names {
        all.push(recommend(&ledger, &choice(config, name)?)?);
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

    #[test]
    fn a_pin_wins_and_a_missing_value_keeps_its_default() {
        let mut config = Config::default();
        config.agents.insert(
            "judge".into(),
            AgentPin {
                model: Some("opus".into()),
                effort: None,
                pinned_at: Some("2026-11-16T14:30:00Z".into()),
            },
        );
        let judge = choice(&config, "judge").unwrap();
        assert_eq!(judge.model, "claude-opus-5-5");
        assert_eq!(judge.effort, Effort::High);
        assert_eq!(judge.source.text(), "yours since 2026-11-16");
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
