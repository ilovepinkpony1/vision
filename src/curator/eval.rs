//! `vision model eval AGENT --models A,B [--n N]` (section 10): replays past work with each
//! model and prints one row for each. So far it replays only the Judge: the last N cards that
//! the person answered, with the effort of the Judge, against the memory of today. It marks
//! the cheapest model whose agreement is at most 5 points under the best one, and it changes
//! no setting. Each evaluation is an event `model-eval`, which `vision model recommend` shows.

use anyhow::{bail, Result};
use serde::{Deserialize, Serialize};

use super::records::EVENT_EVAL;
use crate::ctx::Ctx;
use crate::exit::Stop;
use crate::ledger::{Event, Ledger};
use crate::memory::Memory;
use crate::models::{self, MAX_GAP_POINTS};
use crate::paths::TicketKey;
use crate::questions::{replay, ReplayOptions, JUDGE};

/// One model of an evaluation.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct EvalRow {
    pub model: String,
    pub turn: Option<String>,
    /// The replayed cards.
    pub items: usize,
    pub agree: usize,
    pub disagree: usize,
    pub to_person: usize,
    /// The part of the cards that the Judge answered as the person did, in percent.
    pub agreement: f64,
    pub cost_usd: f64,
    pub seconds: f64,
    pub timed_out: bool,
    /// Why the replay did not run.
    pub error: Option<String>,
}

/// One evaluation, as the event keeps it.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Evaluation {
    pub agent: String,
    pub at: String,
    pub n: u32,
    pub effort: String,
    pub rows: Vec<EvalRow>,
    /// The cheapest model whose agreement is at most 5 points under the best one.
    pub recommended: Option<String>,
}

/// The rule of section 10 on the rows that ran: the cheapest model whose agreement is not
/// worse than the best by more than [`MAX_GAP_POINTS`] points.
pub fn pick(rows: &[EvalRow]) -> Option<String> {
    let ran: Vec<&EvalRow> = rows
        .iter()
        .filter(|r| r.error.is_none() && r.items > 0)
        .collect();
    let best = ran.iter().map(|r| r.agreement).fold(f64::MIN, f64::max);
    ran.iter()
        .filter(|r| r.agreement >= best - MAX_GAP_POINTS)
        .min_by(|a, b| a.cost_usd.total_cmp(&b.cost_usd))
        .map(|r| r.model.clone())
}

/// `vision model eval`.
pub fn command(
    ctx: &Ctx,
    agent: &str,
    models: &[String],
    n: Option<u32>,
    json: bool,
) -> Result<()> {
    ctx.require_setup()?;
    let spec = models::require_agent(agent.trim())?;
    if spec.name != JUDGE {
        return Err(Stop::NotBuilt(format!(
            "model eval {name}: eval replays only the Judge so far. Run `vision model recommend {name}` to see the evidence of the ledger",
            name = spec.name
        ))
        .into());
    }
    let mut ids: Vec<String> = Vec::new();
    for model in models {
        let id = models::full_model_id(model)?;
        if !ids.contains(&id) {
            ids.push(id);
        }
    }
    if ids.is_empty() {
        bail!("name the models to compare, such as --models opus,sonnet");
    }
    let config = ctx.config()?;
    let n = n.unwrap_or(config.curator.replay).max(1);
    let effort = models::choice(config, JUDGE)?.effort;
    let memory = Memory::from_ctx(ctx)?;
    let ledger = ctx.ledger()?;
    if ledger.questions_answered_by_person(1)?.is_empty() {
        bail!("the person answered no card yet, so there is nothing to replay. Answer cards with `vision needs` first");
    }
    let key = TicketKey::parse("eval")?;
    let mut rows = Vec::new();
    for model in &ids {
        if !json {
            eprintln!("Replaying with {model}...");
        }
        let options = ReplayOptions {
            n,
            memory: memory.dir().to_path_buf(),
            model: Some(model.clone()),
            effort: Some(effort),
            key: key.clone(),
            wait: None,
        };
        let row = match replay(ctx, &ledger, &options) {
            Ok(report) => {
                let items = report.items.len();
                EvalRow {
                    model: model.clone(),
                    turn: Some(report.turn.clone()),
                    items,
                    agree: report.agree,
                    disagree: report.disagree,
                    to_person: report.to_person,
                    agreement: if items == 0 {
                        0.0
                    } else {
                        100.0 * report.agree as f64 / items as f64
                    },
                    cost_usd: report.cost_usd,
                    seconds: report.seconds,
                    timed_out: report.timed_out,
                    error: None,
                }
            }
            Err(e) => EvalRow {
                model: model.clone(),
                error: Some(format!("{e:#}")),
                ..EvalRow::default()
            },
        };
        rows.push(row);
    }
    if rows.iter().all(|r| r.error.is_some()) {
        let reasons: Vec<String> = rows
            .iter()
            .map(|r| format!("{}: {}", r.model, r.error.as_deref().unwrap_or("")))
            .collect();
        bail!("no replay ran: {}", reasons.join("; "));
    }
    let evaluation = Evaluation {
        agent: JUDGE.into(),
        at: crate::util::time::now_rfc3339(),
        n,
        effort: effort.to_string(),
        recommended: pick(&rows),
        rows,
    };
    ledger.add_event(&Event::now("user", EVENT_EVAL).detail(serde_json::to_value(&evaluation)?))?;
    ctx.log(&format!(
        "model eval: {} with {} model(s), the cheapest within {MAX_GAP_POINTS} points: {}",
        evaluation.agent,
        evaluation.rows.len(),
        evaluation.recommended.as_deref().unwrap_or("none")
    ));
    if json {
        println!("{}", serde_json::to_string_pretty(&evaluation)?);
    } else {
        print!("{}", text(&evaluation));
    }
    Ok(())
}

/// The table of an evaluation for people.
pub fn text(evaluation: &Evaluation) -> String {
    let items = evaluation.rows.iter().map(|r| r.items).max().unwrap_or(0);
    let mut out = format!(
        "Replayed the last {items} answers of the person with the Judge (effort {}), against the memory of today.\n",
        evaluation.effort
    );
    let mut table = vec![[
        "MODEL",
        "AGREE",
        "DISAGREE",
        "TO PERSON",
        "COST",
        "TIME",
        "",
    ]
    .map(String::from)
    .to_vec()];
    for row in &evaluation.rows {
        let mark = if evaluation.recommended.as_deref() == Some(row.model.as_str()) {
            format!("<- the cheapest within {MAX_GAP_POINTS} points of the best")
        } else {
            String::new()
        };
        match &row.error {
            Some(error) => table.push(vec![
                row.model.clone(),
                "-".into(),
                "-".into(),
                "-".into(),
                "-".into(),
                "-".into(),
                format!("did not run: {error}"),
            ]),
            None => table.push(vec![
                row.model.clone(),
                format!("{}/{} ({:.0}%)", row.agree, row.items, row.agreement),
                row.disagree.to_string(),
                row.to_person.to_string(),
                format!("${:.2}", row.cost_usd),
                format!("{:.0}s", row.seconds),
                if row.timed_out {
                    format!("timed out {mark}").trim().to_string()
                } else {
                    mark
                },
            ]),
        }
    }
    out.push_str(&render(&table));
    out.push_str("This command changes no setting. To use a model: vision model set judge MODEL\n");
    out
}

fn render(rows: &[Vec<String>]) -> String {
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
        out.push_str(cells.join("  ").trim_end());
        out.push('\n');
    }
    out
}

/// One line about the last evaluation of an agent, for `vision model recommend`.
pub fn last_line(ledger: &Ledger, agent: &str) -> Result<Option<String>> {
    for row in ledger.events_of_kind(EVENT_EVAL, 50)? {
        let Some(evaluation) = super::records::detail_of::<Evaluation>(&row) else {
            continue;
        };
        if evaluation.agent != agent {
            continue;
        }
        let rows: Vec<String> = evaluation
            .rows
            .iter()
            .filter(|r| r.error.is_none())
            .map(|r| format!("{} {:.0}% ${:.2}", r.model, r.agreement, r.cost_usd))
            .collect();
        return Ok(Some(format!(
            "last eval ({}, {} answers): {}; the cheapest within {MAX_GAP_POINTS} points: {}",
            evaluation.at.get(..10).unwrap_or(&evaluation.at),
            evaluation.rows.iter().map(|r| r.items).max().unwrap_or(0),
            rows.join(", "),
            evaluation.recommended.as_deref().unwrap_or("none")
        )));
    }
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(model: &str, agreement: f64, cost: f64) -> EvalRow {
        EvalRow {
            model: model.into(),
            items: 10,
            agreement,
            cost_usd: cost,
            ..EvalRow::default()
        }
    }

    #[test]
    fn the_cheapest_model_within_5_points_is_marked() {
        let rows = [
            row("top", 90.0, 1.0),
            row("mid", 85.0, 0.4),
            row("low", 70.0, 0.1),
        ];
        assert_eq!(pick(&rows).as_deref(), Some("mid"));
        let rows = [row("top", 90.0, 1.0), row("mid", 84.0, 0.4)];
        assert_eq!(pick(&rows).as_deref(), Some("top"));
        let mut failed = row("cheap", 100.0, 0.0);
        failed.error = Some("refused".into());
        assert_eq!(
            pick(&[failed.clone(), row("top", 50.0, 1.0)]).as_deref(),
            Some("top")
        );
        assert_eq!(pick(&[failed]), None);
    }

    #[test]
    fn the_table_marks_the_pick_and_changes_nothing() {
        let evaluation = Evaluation {
            agent: "judge".into(),
            effort: "high".into(),
            rows: vec![row("top", 90.0, 1.0), row("mid", 90.0, 0.5)],
            recommended: Some("mid".into()),
            ..Evaluation::default()
        };
        let text = text(&evaluation);
        assert!(text.contains("changes no setting"), "{text}");
        let mid = text.lines().find(|l| l.starts_with("mid")).unwrap();
        assert!(mid.contains("<- the cheapest within 5 points"), "{text}");
        assert!(!text
            .lines()
            .find(|l| l.starts_with("top"))
            .unwrap()
            .contains("<-"));
    }
}
