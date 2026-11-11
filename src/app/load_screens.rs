//! The reads of the screens Models and Stats, each in a thread of the runtime, through the
//! functions of today that return data: the model table and its evidence of `vision model`, and
//! the report of `vision report`. The cost of each agent comes from the ledger, because the
//! report does not have it. None of them changes the state.

use std::collections::BTreeMap;
use std::time::Duration;

use ::time::OffsetDateTime;

use super::model_table::{Evidence, ModelLine};
use super::stats::{StatsData, StepBar, TicketLine};
use crate::ctx::Ctx;
use crate::models::{self, Source, AGENTS};
use crate::paths::{Paths, TicketKey};
use crate::pipeline::state::Counts;
use crate::pipeline::{report_of, step_order};
use crate::util::time;

/// One row for each agent of Vision, in the order of the model table.
pub fn models(paths: &Paths) -> Result<Vec<ModelLine>, String> {
    read_models(paths).map_err(|e| format!("{e:#}"))
}

fn read_models(paths: &Paths) -> anyhow::Result<Vec<ModelLine>> {
    let ctx = Ctx::new(paths.clone());
    ctx.require_setup()?;
    let config = ctx.config()?;
    let ledger = ctx.ledger()?;
    let mut out = Vec::new();
    for spec in &AGENTS {
        let choice = models::choice(config, spec.name)?;
        let advice = models::recommend(&ledger, &choice)?;
        out.push(ModelLine {
            agent: spec.name.to_string(),
            model: choice.model.clone(),
            effort: choice.effort.to_string(),
            pinned: matches!(choice.source, Source::Yours { .. }),
            since: match &choice.source {
                Source::Yours { since } => since.clone(),
                Source::Default => None,
            },
            default_model: spec.model.to_string(),
            default_effort: spec.effort.to_string(),
            reason: spec.reason.to_string(),
            evidence: advice
                .models
                .iter()
                .map(|m| Evidence {
                    model: m.model.clone(),
                    turns: m.turns,
                    cost_per_turn: m.cost_per_turn,
                })
                .collect(),
            recommendation: advice.text,
            recommended: advice.recommended,
        });
    }
    Ok(out)
}

/// The numbers of the last 7 days before `wall`, or of all time.
pub fn stats(paths: &Paths, week: bool, wall: OffsetDateTime) -> Result<StatsData, String> {
    read_stats(paths, week, wall).map_err(|e| format!("{e:#}"))
}

fn read_stats(paths: &Paths, week: bool, wall: OffsetDateTime) -> anyhow::Result<StatsData> {
    let ctx = Ctx::new(paths.clone());
    ctx.require_setup()?;
    let ledger = ctx.ledger()?;
    let since = week.then(|| time::rfc3339(wall - Duration::from_secs(7 * 24 * 3600)));
    let report = report_of(&ledger, since.as_deref())?;
    // A broken adapter never breaks the numbers: the steps then go by name.
    let adapter_steps: Vec<String> = crate::adapter::load(&ctx)
        .map(|a| a.steps.iter().map(|s| s.id.clone()).collect())
        .unwrap_or_default();
    let total = &report.total;
    let steps = step_order(
        &adapter_steps,
        total.step_seconds.keys().chain(total.step_cost_usd.keys()),
    )
    .into_iter()
    .map(|step| StepBar {
        seconds: total.step_seconds.get(&step).copied().unwrap_or(0),
        cost_usd: total.step_cost_usd.get(&step).copied().unwrap_or(0.0),
        step,
    })
    .collect();
    let finished = ledger
        .tickets()?
        .iter()
        .filter(|row| {
            Counts::of(row)
                .finished
                .is_some_and(|at| since.as_deref().is_none_or(|s| at.as_str() >= s))
        })
        .count();
    let mut by_agent: BTreeMap<String, f64> = BTreeMap::new();
    // The turns that no ticket owns, such as the turns of the Curator: the report does not
    // count their cost, so the screen names it apart.
    let mut outside: BTreeMap<String, f64> = BTreeMap::new();
    for cost in ledger.agent_costs(since.as_deref())? {
        if cost.agent.is_empty() {
            continue;
        }
        let reserved = TicketKey::parse(&cost.ticket).map_or(true, |k| k.is_reserved());
        if reserved {
            if cost.cost_usd > 0.0 {
                *outside.entry(cost.agent).or_default() += cost.cost_usd;
            }
            continue;
        }
        *by_agent.entry(cost.agent).or_default() += cost.cost_usd;
    }
    let mut agents: Vec<(String, f64)> = Vec::new();
    for spec in &AGENTS {
        if let Some(cost) = by_agent.remove(spec.name) {
            agents.push((spec.name.to_string(), cost));
        }
    }
    agents.extend(by_agent);
    Ok(StatsData {
        week,
        cost_usd: total.cost_usd,
        finished,
        without_you: total.cards_by_memory + total.cards_by_judge,
        questions: total.cards_by_memory + total.cards_by_judge + total.cards_to_person,
        minutes: total.human_minutes,
        tickets: report
            .tickets
            .iter()
            .map(|(key, m)| TicketLine {
                key: key.clone(),
                to_you: m.cards_to_person,
                by_memory: m.cards_by_memory,
                by_judge: m.cards_by_judge,
                minutes: m.human_minutes,
                cost_usd: m.cost_usd,
            })
            .collect(),
        steps,
        agents,
        outside: outside.into_iter().collect(),
    })
}
