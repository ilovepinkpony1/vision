//! `vision model recommend` (section 10): the evidence of the ledger for each model that an
//! agent used, and the rule that picks a model.
//!
//! For each model with at least [`MIN_TURNS`] ended turns: the number of turns, the cost of one
//! turn, and the part of the turns whose end fact held at the first try. The recommendation is
//! the cheapest model whose first-try part is not worse than the best by more than
//! [`MAX_GAP_POINTS`] points. With fewer than [`MIN_TURNS`] measured turns (ended, with a known
//! end fact) for a second model, the model in use stays.

use anyhow::Result;
use serde::Serialize;

use super::ModelChoice;
use crate::ledger::turns::ModelTurns;
use crate::ledger::Ledger;

/// The turns that a model needs before its numbers count.
pub const MIN_TURNS: i64 = 10;
/// The most that the quality of the cheaper model may be below the best, in points.
pub const MAX_GAP_POINTS: f64 = 5.0;

/// One number for one agent, with the SQL that counts it. `?1` is the agent, `?2` the model.
struct Extra {
    name: &'static str,
    sql: &'static str,
    /// Divide by the number of tickets, for a number "for each ticket".
    per_ticket: bool,
}

/// The extra numbers of the Judge: the corrections of the person, and the proposals that the
/// person changed. A correction is a `human` row of the kind `correct` whose object is a
/// question that a Judge turn of this model answered.
const JUDGE_EXTRAS: [Extra; 2] = [
    Extra {
        name: "corrections",
        sql: "SELECT count(*) FROM human h JOIN questions q ON q.id = h.object
              JOIN turns t ON t.id = q.judge_turn
              WHERE h.kind = 'correct' AND t.agent = ?1 AND t.model = ?2",
        per_ticket: false,
    },
    Extra {
        name: "changed proposals",
        sql:
            "SELECT count(*) FROM questions q JOIN turns t ON t.id = q.judge_turn
              WHERE q.outcome = 'propose' AND q.took_proposal = 0 AND t.agent = ?1 AND t.model = ?2",
        per_ticket: false,
    },
];

/// The extra numbers of the Builder, for each ticket where it used the model: the review
/// findings, the red CI rounds, and the QA bugs.
const BUILDER_EXTRAS: [Extra; 3] = [
    Extra {
        name: "review findings per ticket",
        sql: "SELECT count(*) FROM findings WHERE coalesce(source, '') <> 'qa' AND ticket IN
              (SELECT DISTINCT ticket FROM turns WHERE agent = ?1 AND model = ?2)",
        per_ticket: true,
    },
    Extra {
        name: "red CI rounds per ticket",
        sql: "SELECT count(*) FROM checks WHERE kind = 'ci' AND result IN ('fail', 'red') AND ticket IN
              (SELECT DISTINCT ticket FROM turns WHERE agent = ?1 AND model = ?2)",
        per_ticket: true,
    },
    Extra {
        name: "QA bugs per ticket",
        sql: "SELECT count(*) FROM findings WHERE source = 'qa' AND ticket IN
              (SELECT DISTINCT ticket FROM turns WHERE agent = ?1 AND model = ?2)",
        per_ticket: true,
    },
];

/// The numbers of one model of one agent.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct ModelEvidence {
    pub model: String,
    pub turns: i64,
    pub cost_per_turn: f64,
    /// The part of the turns whose end fact held at the first try, in percent, or `None`
    /// when no turn has a known end fact.
    pub first_try: Option<f64>,
    /// The numbers of the Judge or the Builder, by name.
    pub extra: Vec<(String, f64)>,
    /// True when the model has at least [`MIN_TURNS`] turns.
    pub counts: bool,
}

/// The evidence and the recommendation for one agent.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Recommendation {
    pub agent: String,
    /// The model in use.
    pub current: String,
    pub models: Vec<ModelEvidence>,
    /// The recommended model, or `None` when there is too little evidence.
    pub recommended: Option<String>,
    /// One line for people.
    pub text: String,
}

/// The evidence of the ledger for `choice.agent`, and the recommendation.
pub fn recommend(ledger: &Ledger, choice: &ModelChoice) -> Result<Recommendation> {
    let agent = choice.agent.as_str();
    let extras: &[Extra] = match agent {
        "judge" => &JUDGE_EXTRAS,
        "builder" => &BUILDER_EXTRAS,
        _ => &[],
    };
    let mut models = Vec::new();
    for turns in ledger.model_turns(agent)? {
        let mut extra = Vec::new();
        for item in extras {
            let count = ledger.model_count(item.sql, agent, &turns.model)? as f64;
            let value = if item.per_ticket {
                count / (turns.tickets.max(1) as f64)
            } else {
                count
            };
            extra.push((item.name.to_string(), value));
        }
        models.push(evidence(&turns, extra));
    }
    let (recommended, text) = pick(&models, choice);
    Ok(Recommendation {
        agent: agent.to_string(),
        current: choice.model.clone(),
        models,
        recommended,
        text,
    })
}

fn evidence(turns: &ModelTurns, extra: Vec<(String, f64)>) -> ModelEvidence {
    ModelEvidence {
        model: turns.model.clone(),
        turns: turns.turns,
        cost_per_turn: if turns.turns > 0 {
            turns.cost_usd / turns.turns as f64
        } else {
            0.0
        },
        first_try: (turns.end_fact_known > 0)
            .then(|| 100.0 * turns.end_fact_held as f64 / turns.end_fact_known as f64),
        extra,
        counts: turns.turns >= MIN_TURNS,
    }
}

/// The rule of section 10.
fn pick(models: &[ModelEvidence], choice: &ModelChoice) -> (Option<String>, String) {
    let stays = match choice.source {
        super::Source::Default => "the default stays",
        super::Source::Yours { .. } => "your model stays",
    };
    let counted: Vec<&ModelEvidence> = models
        .iter()
        .filter(|m| m.counts && m.first_try.is_some())
        .collect();
    if counted.len() < 2 {
        return (
            None,
            format!("fewer than {MIN_TURNS} measured turns for a second model, so {stays}"),
        );
    }
    let best = counted
        .iter()
        .filter_map(|m| m.first_try)
        .fold(f64::MIN, f64::max);
    let chosen = counted
        .iter()
        .filter(|m| m.first_try.is_some_and(|q| q >= best - MAX_GAP_POINTS))
        .min_by(|a, b| a.cost_per_turn.total_cmp(&b.cost_per_turn));
    let Some(chosen) = chosen else {
        return (None, format!("no model fits the rule, so {stays}"));
    };
    let first_try = chosen.first_try.unwrap_or(0.0);
    let text = if chosen.model == choice.model {
        format!(
            "keep {}: the cheapest model within {MAX_GAP_POINTS} points of the best first-try part ({first_try:.0}% against {best:.0}%)",
            chosen.model
        )
    } else {
        format!(
            "{}: ${:.2} for each turn, first try {first_try:.0}% against the best {best:.0}%. Run `vision model set {} {}` to use it",
            chosen.model, chosen.cost_per_turn, choice.agent, chosen.model
        )
    };
    (Some(chosen.model.clone()), text)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Effort;
    use crate::models::Source;

    fn model(name: &str, turns: i64, cost: f64, first_try: f64) -> ModelEvidence {
        ModelEvidence {
            model: name.into(),
            turns,
            cost_per_turn: cost,
            first_try: Some(first_try),
            extra: Vec::new(),
            counts: turns >= MIN_TURNS,
        }
    }

    fn choice(model: &str) -> ModelChoice {
        ModelChoice {
            agent: "scout".into(),
            model: model.into(),
            effort: Effort::High,
            source: Source::Default,
        }
    }

    #[test]
    fn the_cheapest_model_within_5_points_wins() {
        let models = [
            model("top", 12, 1.0, 95.0),
            model("mid", 10, 0.5, 91.0),
            model("low", 15, 0.1, 80.0),
        ];
        let (picked, text) = pick(&models, &choice("top"));
        assert_eq!(picked.as_deref(), Some("mid"));
        assert!(text.contains("vision model set scout mid"), "{text}");
        let (picked, text) = pick(&models, &choice("mid"));
        assert_eq!(picked.as_deref(), Some("mid"));
        assert!(text.starts_with("keep mid"), "{text}");
    }

    #[test]
    fn too_few_turns_for_a_second_model_keeps_the_default() {
        let models = [model("top", 40, 1.0, 95.0), model("mid", 9, 0.5, 99.0)];
        let (picked, text) = pick(&models, &choice("top"));
        assert_eq!(picked, None);
        assert!(text.ends_with("so the default stays"), "{text}");
    }
}
