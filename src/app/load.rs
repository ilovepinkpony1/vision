//! The reads of the app. The local data of Home reads through the context and the connection
//! that the loader of the app keeps ([`super::loader`]); each other read runs in a thread of
//! the runtime, makes its own context from the paths, and opens its own ledger connection, as
//! `vision watch` does. Each one reads through the functions of today that return data: the
//! status rows, the rows of `vision pick`, the report, the model table, and the adapter. None
//! of them changes the state.

use std::time::Duration;

use ::time::OffsetDateTime;

use super::model::{ContinueView, FirstStep, Held, Local, Setup, SetupError, StartInfo, Week};
use crate::adapter::{self, StepKind};
use crate::config::Mode;
use crate::ctx::Ctx;
use crate::executor;
use crate::ledger::turns::Window;
use crate::ledger::{Ledger, TicketRow};
use crate::paths::{tilde_display, Paths, TicketKey};
use crate::pipeline::commands::{pick_rows, report_of, PickRow};
use crate::pipeline::state::{flags_of, Counts, NEEDS_YOU, PAUSED, STOPPED};
use crate::snapshot::{LimitsView, Need};
use crate::util::time;

/// The first stage of the loader: the configuration and the adapter.
pub fn setup(paths: &Paths) -> Result<Setup, SetupError> {
    let home = tilde_display(paths.home());
    if !paths.config().is_file() {
        return Err(SetupError {
            reason: "not set up".into(),
            what: format!("Vision is not set up at {home}: config.toml is missing."),
            fix: "Run vision init to make the home folder, then open Vision again.".into(),
        });
    }
    let ctx = Ctx::new(paths.clone());
    let config = ctx.config().map_err(|e| SetupError {
        reason: "config.toml has an error".into(),
        what: format!("config.toml has an error: {e:#}"),
        fix: "Fix config.toml, then open Vision again. vision doctor tests the setup.".into(),
    })?;
    let adapter = adapter::load_with(config, paths).map_err(|e| SetupError {
        reason: "the adapter does not load".into(),
        what: format!("The adapter {} does not load: {e:#}", config.project),
        fix: "Fix its adapter.toml, or the key project of config.toml, then open Vision again."
            .into(),
    })?;
    Ok(Setup {
        home,
        adapter: adapter.name.clone(),
        steps: adapter.steps.iter().map(|s| s.id.clone()).collect(),
        sections: config.app.sections.clone(),
        merge_steps: config.app.merge_steps.clone(),
    })
}

/// The local data: the tickets in progress, the turns that run, the needs, the numbers of the
/// last 7 days before `wall`, and the timer. A read with its own context and connection, such
/// as for a test; the app reads through [`super::loader`], which keeps them.
pub fn local(paths: &Paths, wall: OffsetDateTime) -> Result<Local, String> {
    let read = || -> anyhow::Result<Local> {
        let ctx = Ctx::new(paths.clone());
        ctx.require_setup()?;
        let ledger = ctx.ledger()?;
        let proposals = crate::curator::needs(&ctx, &ledger)?;
        local_with(&ctx, &ledger, proposals, wall)
    };
    read().map_err(|e| format!("{e:#}"))
}

/// True for a ticket that Home shows in progress: it has a step and its end step did not run.
pub fn in_progress(row: &TicketRow) -> bool {
    !row.state.as_deref().is_none_or(str::is_empty) && Counts::of(row).finished.is_none()
}

/// The local data from a context and a connection that the caller keeps, with the needs of the
/// proposals of the Curator. Each read runs in one read transaction, and only the tickets in
/// progress get the reads of one ticket.
pub fn local_with(
    ctx: &Ctx,
    ledger: &Ledger,
    proposals: Vec<Need>,
    wall: OffsetDateTime,
) -> anyhow::Result<Local> {
    ledger.in_read(|ledger| read_local(ctx, ledger, proposals, wall))
}

fn read_local(
    ctx: &Ctx,
    ledger: &Ledger,
    proposals: Vec<Need>,
    wall: OffsetDateTime,
) -> anyhow::Result<Local> {
    let config = ctx.config()?;
    let needs = crate::snapshot::collect_needs_with(ctx, ledger, proposals)?;
    let rows = ledger.tickets()?;
    let running = crate::turn::running_turns(ctx, ledger)?;
    let views = crate::pipeline::view::rows_with(ctx, ledger, &needs, &running, in_progress)?;
    let mut facts = crate::pipeline::facts::state_facts_with(ctx, ledger, &running, in_progress)?;
    let since = time::rfc3339(wall - Duration::from_secs(7 * 24 * 3600));
    let mut tickets = Vec::new();
    let mut agents: Vec<String> = Vec::new();
    for ticket in views {
        let Some(row) = rows.iter().find(|r| r.key == ticket.key) else {
            continue;
        };
        let flags = flags_of(row);
        if let Some(works) = &ticket.works {
            if !agents.contains(&works.agent) {
                agents.push(works.agent.clone());
            }
        }
        let at = facts.iter().position(|f| f.key == ticket.key);
        tickets.push(Held {
            facts: at.map(|i| facts.swap_remove(i)).unwrap_or_default(),
            stopped: flags.iter().any(|f| f == STOPPED),
            // A notice never stops a ticket, so it does not mark the ticket as one that needs you.
            needs_you: ticket
                .needs
                .keys()
                .any(|k| k != crate::pipeline::view::NOTICE)
                || flags.iter().any(|f| f == NEEDS_YOU || f == PAUSED),
            taken: row.taken.clone(),
            view: ticket,
        });
    }
    let finished = rows
        .iter()
        .filter(|row| {
            Counts::of(row)
                .finished
                .is_some_and(|at| at.as_str() >= since.as_str())
        })
        .count();
    let report = report_of(ledger, Some(&since))?;
    let total = &report.total;
    let now = time::rfc3339(wall);
    let reset = |window| -> anyhow::Result<Option<String>> {
        Ok(ledger.active_reading(window, &now)?.and_then(|r| r.resets))
    };
    let resets = [reset(Window::FiveHour)?, reset(Window::SevenDay)?];
    Ok(Local {
        off: ctx.paths.off().exists(),
        limits: LimitsView::new(&config.limits, ledger.latest_limits()?),
        needs,
        tickets,
        turns: running.len(),
        agents,
        week: Week {
            cost_usd: total.cost_usd,
            finished,
            without_you: total.cards_by_memory + total.cards_by_judge,
            questions: total.cards_by_memory + total.cards_by_judge + total.cards_to_person,
        },
        timer: crate::install::is_installed(&ctx.paths),
        running: crate::running::read(&ctx.paths),
        jobs: crate::executor::runs::views(&ctx.paths),
        jobs_ended: crate::executor::runs::ended_count(&ctx.paths),
        panes: running
            .iter()
            .filter(|t| t.pane.is_some())
            .map(|t| t.ticket.clone())
            .collect(),
        resets,
    })
}

/// The queue of the tracker: the rows of `vision pick`.
pub fn queue(paths: &Paths) -> Result<Vec<PickRow>, String> {
    pick_rows(&Ctx::new(paths.clone())).map_err(|e| format!("{e:#}"))
}

/// What the continue card shows for a stopped or paused ticket. It makes the one call of
/// observe of the `[[resume]]` rules of the adapter, and it changes nothing.
pub fn resume(paths: &Paths, key: &str) -> Result<ContinueView, String> {
    let read = || -> anyhow::Result<ContinueView> {
        let ctx = Ctx::new(paths.clone());
        let key = TicketKey::parse(key)?;
        crate::pipeline::resume::read_view(&ctx, &key)
    };
    read().map_err(|e| format!("{e:#}"))
}

/// What the start card of a ticket that is in progress outside Vision shows. It makes the one
/// call of observe of the `[[resume]]` rules of the adapter, with the branch and the pull
/// request of the queue, and it changes nothing.
pub fn outside(
    paths: &Paths,
    key: &str,
    branch: &str,
    pr: Option<i64>,
) -> Result<ContinueView, String> {
    let read = || -> anyhow::Result<ContinueView> {
        let ctx = Ctx::new(paths.clone());
        let key = TicketKey::parse(key)?;
        crate::pipeline::resume::read_outside_view(&ctx, &key, branch, pr)
    };
    read().map_err(|e| format!("{e:#}"))
}

/// What the start card says about a ticket.
pub fn start_info(paths: &Paths, key: &str) -> Result<StartInfo, String> {
    read_start_info(paths, key).map_err(|e| format!("{e:#}"))
}

fn read_start_info(paths: &Paths, key: &str) -> anyhow::Result<StartInfo> {
    let ctx = Ctx::new(paths.clone());
    let config = ctx.config()?;
    let adapter = adapter::load_with(config, paths)?;
    let key = TicketKey::parse(key)?;
    let clone = crate::pipeline::slots::next_slot(&ctx).map_err(|e| format!("{e:#}"));
    let entry = adapter.entry();
    let mut first = FirstStep {
        step: entry.id.clone(),
        kind: entry.kind.name().to_string(),
        agent: None,
        model: None,
        effort: None,
    };
    if let StepKind::Turn(turn) = &entry.kind {
        first.agent = Some(turn.agent.clone());
        if let Ok(choice) = crate::models::choice_in(config, &adapter.agents, &turn.agent) {
            first.model = Some(choice.model);
            first.effort = Some(choice.effort.to_string());
        }
    }
    let mut counts = [0usize; 4];
    for (name, verb) in &adapter.verbs {
        if verb.read {
            continue;
        }
        let mode = executor::mode_of(config, &adapter, name)?;
        counts[mode_index(mode)] += 1;
    }
    Ok(StartInfo {
        clone,
        branch: adapter.branch_for(&key),
        base: adapter.base.clone(),
        steps: adapter.steps.iter().map(|s| s.id.clone()).collect(),
        first,
        writes: writes_line(&counts),
    })
}

const MODES: [Mode; 4] = [Mode::Hold, Mode::Shadow, Mode::Ask, Mode::Auto];

fn mode_index(mode: Mode) -> usize {
    MODES.iter().position(|m| *m == mode).unwrap_or(0)
}

/// One sentence about the write verbs of the adapter, by their mode in this home.
pub fn writes_line(counts: &[usize; 4]) -> String {
    let total: usize = counts.iter().sum();
    if total == 0 {
        return "The adapter has no write verb, so nothing goes to the tracker or the forge."
            .into();
    }
    let used: Vec<(Mode, usize)> = MODES
        .iter()
        .zip(counts)
        .filter(|(_, n)| **n > 0)
        .map(|(m, n)| (*m, *n))
        .collect();
    if let [(mode, n)] = used.as_slice() {
        let head = if *n == 1 {
            format!("1 write verb is in the mode {mode}.")
        } else {
            format!("{n} write verbs are in the mode {mode}.")
        };
        let tail = match mode {
            Mode::Hold => "Vision stops the ticket at each one and waits for you.",
            Mode::Shadow => "Vision asks you the first time that it wants to send each one.",
            Mode::Ask => "Vision asks you before it sends each one.",
            Mode::Auto => "Vision sends each one with no question.",
        };
        let what = if *n == 1 {
            tail.replace("each one", "it")
        } else {
            tail.to_string()
        };
        return format!("{head} {what}");
    }
    let parts: Vec<String> = used
        .iter()
        .rev()
        .map(|(mode, n)| format!("{n} in the mode {mode}"))
        .collect();
    let list = match parts.as_slice() {
        [a, b] => format!("{a} and {b}"),
        [rest @ .., last] => format!("{}, and {last}", rest.join(", ")),
        [] => String::new(),
    };
    format!(
        "Of {total} write verbs, {list}. Vision asks you before each write that is not in the mode auto."
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_line_about_writes_counts_the_verbs_by_mode() {
        assert_eq!(
            writes_line(&[0, 9, 0, 0]),
            "9 write verbs are in the mode shadow. Vision asks you the first time that it wants to send each one."
        );
        assert_eq!(
            writes_line(&[0, 0, 1, 4]),
            "Of 5 write verbs, 4 in the mode auto and 1 in the mode ask. Vision asks you before each write that is not in the mode auto."
        );
        assert_eq!(
            writes_line(&[0, 0, 1, 0]),
            "1 write verb is in the mode ask. Vision asks you before it sends it."
        );
        assert!(writes_line(&[0; 4]).contains("no write verb"));
    }
}
