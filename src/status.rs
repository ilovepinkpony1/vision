//! `vision status`, `watch`, `prompt`, `on`, and `off`.

use std::fmt::Write as _;
use std::io::Write as _;
use std::time::Duration;

use ::time::OffsetDateTime;
use anyhow::{bail, Context, Result};
use serde::Serialize;

use crate::ctx::Ctx;
use crate::ledger::Event;
use crate::paths::Paths;
use crate::pipeline::view::marks_text;
use crate::pipeline::TicketView;
use crate::running::Running;
use crate::snapshot::{collect_needs, LimitView, LimitsView, Need, NeedKind, Outcome, Snapshot};
use crate::util::fs::{atomic_write, read_optional};
use crate::util::time;

/// What `vision status` shows. `--json` prints this value.
#[derive(Debug, Serialize)]
pub struct StatusView {
    pub home: String,
    /// True when the `off` file is present.
    pub off: bool,
    pub tick: TickView,
    pub limits: LimitsView,
    pub needs: Vec<Need>,
    /// One row for each ticket: its step, what works or waits, its marks, and its cost.
    pub tickets: Vec<TicketView>,
    /// The job or the operation that a pass runs now, or `None`.
    pub running: Option<Running>,
}

#[derive(Debug, Serialize)]
pub struct TickView {
    /// The time of the last heartbeat, RFC 3339 UTC.
    pub last: Option<String>,
    pub age_seconds: Option<i64>,
    /// The outcome of the last tick, from the snapshot.
    pub outcome: Option<Outcome>,
    pub error: Option<String>,
}

/// `vision status [--json]`.
pub fn status(ctx: &Ctx, json: bool) -> Result<()> {
    let view = StatusView::read(ctx)?;
    if json {
        println!("{}", serde_json::to_string_pretty(&view)?);
    } else {
        print!("{}", view.render());
    }
    Ok(())
}

/// `vision watch`: the status view, drawn again each 2 seconds until Ctrl-C.
pub fn watch(ctx: &Ctx) -> Result<()> {
    ctx.require_setup()?;
    loop {
        // A new context each time, so that a change of the configuration shows.
        let fresh = Ctx::new(ctx.paths.clone());
        let text = match StatusView::read(&fresh) {
            Ok(view) => view.render(),
            Err(e) => format!("error: {e:#}\n"),
        };
        let mut out = std::io::stdout().lock();
        write!(
            out,
            "\x1b[2J\x1b[H{text}\nDrawn again each 2 seconds. Ctrl-C leaves.\n"
        )?;
        out.flush()?;
        drop(out);
        std::thread::sleep(Duration::from_secs(2));
    }
}

/// `vision prompt`: the number of items that wait for the person and the age of the last tick,
/// such as `V:0 12s`, with ` off` when the kill switch is on. It reads two small files, never
/// the ledger or the configuration, and it never fails: it prints `V:?` instead.
pub fn prompt(ctx: Option<&Ctx>) {
    let line = ctx.map_or_else(|| "V:?".to_string(), |ctx| prompt_line(&ctx.paths));
    let _ = writeln!(std::io::stdout(), "{line}");
}

fn prompt_line(paths: &Paths) -> String {
    if !paths.config().is_file() {
        return "V:?".to_string();
    }
    let needs =
        Snapshot::needs_count(&paths.state()).map_or_else(|_| "?".to_string(), |n| n.to_string());
    let age = match read_heartbeat(paths) {
        Ok(Some(at)) => time::short_age(time::seconds_between(at, time::now())),
        _ => "-".to_string(),
    };
    let off = if paths.off().exists() { " off" } else { "" };
    format!("V:{needs} {age}{off}")
}

/// `vision on` and `vision off`. The kill switch works even when the ledger is broken: the
/// file is what counts, and the ledger event is a record of it.
pub fn switch(ctx: &Ctx, off: bool) -> Result<()> {
    if !ctx.paths.home().is_dir() {
        bail!(
            "Vision is not set up at {}. Run `vision init` first",
            ctx.paths.home().display()
        );
    }
    let path = ctx.paths.off();
    if path.exists() == off {
        if off {
            println!("Vision is already off. Run `vision on` to resume.");
            kill_turns(ctx);
        } else {
            println!("Vision is already on.");
        }
        return Ok(());
    }
    if off {
        atomic_write(&path, format!("{}\n", time::now_rfc3339()).as_bytes())?;
    } else {
        match std::fs::remove_file(&path) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => {
                return Err(e).with_context(|| format!("could not remove {}", path.display()))
            }
        }
    }
    let word = if off { "off" } else { "on" };
    ctx.log(&format!("{word}: the person switched Vision {word}"));
    let recorded = ctx
        .ledger()
        .and_then(|ledger| ledger.add_event(&Event::now("user", word)));
    if let Err(e) = recorded {
        eprintln!("warning: Vision is {word}, but the ledger did not record it: {e:#}");
    }
    if off {
        println!("Vision is off: each tick now only writes the heartbeat and the snapshot. Run `vision on` to resume.");
        kill_turns(ctx);
    } else {
        println!("Vision is on: the next tick acts again.");
    }
    Ok(())
}

/// `vision off` also kills each running turn.
fn kill_turns(ctx: &Ctx) {
    match crate::turn::kill_all(ctx) {
        Ok(killed) if killed.is_empty() => {}
        Ok(killed) => {
            ctx.log(&format!("off: killed the turn(s) {}", killed.join(", ")));
            println!(
                "Killed {} running turn(s): {}",
                killed.len(),
                killed.join(", ")
            );
        }
        Err(e) => eprintln!("warning: could not kill each running turn: {e:#}"),
    }
}

impl StatusView {
    /// Reads the heartbeat, the snapshot, the configuration, and the ledger.
    pub fn read(ctx: &Ctx) -> Result<StatusView> {
        ctx.require_setup()?;
        let config = ctx.config()?;
        let ledger = ctx.ledger()?;
        // A damaged runtime file is a warning: the next tick writes it again.
        let last = read_heartbeat(&ctx.paths).unwrap_or_else(|e| {
            eprintln!("warning: {e:#}");
            None
        });
        let snapshot = Snapshot::read(&ctx.paths.state()).unwrap_or_else(|e| {
            eprintln!("warning: {e:#}");
            None
        });
        let tick = TickView {
            last: last.map(time::rfc3339),
            age_seconds: last.map(|at| time::seconds_between(at, time::now())),
            outcome: snapshot.as_ref().map(|s| s.tick.outcome),
            error: snapshot.and_then(|s| s.tick.error),
        };
        let needs = collect_needs(ctx, &ledger)?;
        let tickets = crate::pipeline::status_rows(ctx, &ledger, &needs)?;
        Ok(StatusView {
            home: ctx.paths.home().display().to_string(),
            off: ctx.paths.off().exists(),
            tick,
            limits: LimitsView::new(&config.limits, ledger.latest_limits()?),
            needs,
            tickets,
            running: crate::running::read(&ctx.paths),
        })
    }

    /// The text view.
    pub fn render(&self) -> String {
        let mut out = String::new();
        let _ = writeln!(out, "home     {}", self.home);
        let tick = match (&self.tick.last, self.tick.age_seconds) {
            (Some(at), Some(age)) => format!("{} ago ({at})", time::short_age(age)),
            _ => "no tick yet. Run `vision tick`, or `vision install` to tick each minute"
                .to_string(),
        };
        let _ = writeln!(out, "tick     {tick}");
        if let (Some(Outcome::Failed), Some(error)) = (self.tick.outcome, &self.tick.error) {
            let _ = writeln!(out, "         the last tick failed: {error}");
        }
        let switch = if self.off {
            "off: no action runs. Run `vision on` to resume"
        } else {
            "on"
        };
        let _ = writeln!(out, "switch   {switch}");
        if let Some(running) = &self.running {
            let _ = writeln!(
                out,
                "pass     runs {} for {} since {}",
                running.label(),
                running.ticket,
                running.started
            );
        }
        let _ = writeln!(
            out,
            "limits   {}",
            limit_line("five-hour", &self.limits.five_hour)
        );
        let _ = writeln!(
            out,
            "         {}",
            limit_line("seven-day", &self.limits.seven_day)
        );
        if self.needs.is_empty() {
            let _ = writeln!(out, "needs    nothing waits for you");
        } else {
            let _ = writeln!(
                out,
                "needs    {} waiting. Run `vision needs`",
                self.needs.len()
            );
            for need in &self.needs {
                let ticket = need.ticket.as_deref().unwrap_or("-");
                let _ = writeln!(
                    out,
                    "         {} {ticket} {}: {}",
                    need_word(need.kind),
                    need.id,
                    need.text
                );
            }
        }
        if self.tickets.is_empty() {
            let _ = writeln!(out, "tickets  none. Start one with `vision take KEY`");
        } else {
            let _ = writeln!(out, "tickets  {}\n", self.tickets.len());
            out.push_str(&ticket_table(&self.tickets));
        }
        out
    }
}

/// Reads the heartbeat file, or gives `None` before the first tick.
pub fn read_heartbeat(paths: &Paths) -> Result<Option<OffsetDateTime>> {
    let path = paths.heartbeat();
    let Some(text) = read_optional(&path)? else {
        return Ok(None);
    };
    let at = time::parse_rfc3339(&text).with_context(|| {
        format!(
            "{} is not valid. The next tick writes it again",
            path.display()
        )
    })?;
    Ok(Some(at))
}

fn limit_line(name: &str, view: &LimitView) -> String {
    let stop = percent(view.stop_at);
    match &view.reading {
        None => format!("{name}  no reading yet; new turns stop at {stop}"),
        Some(reading) => {
            let above = if reading.part >= view.stop_at {
                ", so no new turn starts"
            } else {
                ""
            };
            format!(
                "{name}  {} at {}; new turns stop at {stop}{above}",
                percent(reading.part),
                reading.at
            )
        }
    }
}

fn percent(part: f64) -> String {
    format!("{:.0}%", part * 100.0)
}

fn need_word(kind: NeedKind) -> &'static str {
    match kind {
        NeedKind::Card => "card",
        NeedKind::Plan => "plan",
        NeedKind::Operation => "operation",
        NeedKind::Flag => "flag",
        NeedKind::Proposal => "proposal",
        NeedKind::Notice => "notice",
    }
}

fn ticket_table(tickets: &[TicketView]) -> String {
    let dash = |value: Option<&str>| value.unwrap_or("-").to_string();
    let mut rows = vec![["KEY", "STEP", "NOW", "MARKS", "PR", "COST"].map(String::from)];
    for ticket in tickets {
        rows.push([
            ticket.key.clone(),
            dash(ticket.step.as_deref()),
            ticket.now.clone(),
            marks_text(&ticket.marks),
            ticket
                .pr
                .map_or_else(|| "-".to_string(), |pr| pr.to_string()),
            crate::pipeline::money(ticket.cost_usd),
        ]);
    }
    let widths: Vec<usize> = (0..6)
        .map(|i| rows.iter().map(|r| r[i].chars().count()).max().unwrap_or(0))
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
    for ticket in tickets {
        for flag in &ticket.flags {
            let _ = writeln!(
                out,
                "         {} {}: {}. Clears it: {}",
                ticket.key, flag.flag, flag.reason, flag.command
            );
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ledger::LimitReading;

    fn view(key: &str, step: Option<&str>, now: &str) -> TicketView {
        TicketView {
            key: key.into(),
            title: None,
            step: step.map(str::to_string),
            kind: None,
            now: now.into(),
            works: None,
            flags: Vec::new(),
            needs: Default::default(),
            marks: Default::default(),
            cost_usd: 0.0,
            pr: None,
            branch: None,
            clone: None,
            updated: None,
        }
    }

    #[test]
    fn the_table_aligns_its_columns() {
        let mut first = view("DEMO-12", Some("build"), "builder works (3 min)");
        first.pr = Some(7);
        first.cost_usd = 1.5;
        first.marks.jobs.push(crate::pipeline::view::Mark {
            name: "proof".into(),
            result: "pass".into(),
            commit: "1a2b3c4d5e6f".into(),
        });
        first.flags.push(crate::pipeline::view::FlagView {
            flag: "paused".into(),
            reason: "you took over the session".into(),
            since: None,
            command: "vision retry DEMO-12".into(),
        });
        let tickets = vec![first, view("DEMO-1", None, "-")];
        let table = ticket_table(&tickets);
        let lines: Vec<_> = table.lines().collect();
        assert_eq!(
            lines[0],
            "KEY      STEP   NOW                    MARKS                    PR  COST"
        );
        assert_eq!(
            lines[1],
            "DEMO-12  build  builder works (3 min)  proof pass@1a2b3c4d5e6f  7   $1.50"
        );
        assert_eq!(
            lines[2],
            "DEMO-1   -      -                      -                        -   $0.00"
        );
        assert_eq!(
            lines[3],
            "         DEMO-12 paused: you took over the session. Clears it: vision retry DEMO-12"
        );
    }

    #[test]
    fn a_limit_line_says_when_new_turns_stop() {
        let mut view = LimitView {
            stop_at: 0.6,
            reading: None,
        };
        assert_eq!(
            limit_line("five-hour", &view),
            "five-hour  no reading yet; new turns stop at 60%"
        );
        view.reading = Some(LimitReading {
            part: 0.75,
            at: "2026-11-16T14:30:00Z".into(),
            turn: "t".into(),
        });
        assert!(limit_line("five-hour", &view).ends_with(
            "75% at 2026-11-16T14:30:00Z; new turns stop at 60%, so no new turn starts"
        ));
    }
}
