//! `vision doctor`: tests the setup. It prints one line for each test, records each result in
//! the `health` table, and exits with code 1 when a test fails.

use anyhow::Result;
use serde::Serialize;

use crate::config::Config;
use crate::ctx::Ctx;
use crate::exit::Stop;
use crate::install;
use crate::ledger::{HealthRow, Ledger, SCHEMA_VERSION};
use crate::status::read_heartbeat;
use crate::util::process::find_program;
use crate::util::{git, time};

/// A heartbeat older than this many seconds is stale: three ticks were missed.
pub const FRESH_SECONDS: i64 = 180;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Level {
    Ok,
    Warn,
    Fail,
}

impl Level {
    fn word(self) -> &'static str {
        match self {
            Level::Ok => "ok",
            Level::Warn => "warn",
            Level::Fail => "fail",
        }
    }
}

/// The result of one test.
#[derive(Clone, Debug, Serialize)]
pub struct Check {
    pub test: &'static str,
    pub result: Level,
    pub detail: String,
}

impl Check {
    fn new(test: &'static str, result: Level, detail: impl Into<String>) -> Check {
        Check {
            test,
            result,
            detail: detail.into(),
        }
    }
}

#[derive(Serialize)]
struct Report<'a> {
    ok: bool,
    tests: &'a [Check],
}

/// `vision doctor [--json]`.
pub fn run(ctx: &Ctx, json: bool) -> Result<()> {
    let (checks, ledger) = run_checks(ctx);
    if let Some(ledger) = &ledger {
        if let Err(e) = record(ledger, &checks) {
            eprintln!("warning: could not record the results in the health table: {e:#}");
        }
    }
    let failed = checks.iter().filter(|c| c.result == Level::Fail).count();
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&Report {
                ok: failed == 0,
                tests: &checks
            })?
        );
    } else {
        print!("{}", render(&checks));
    }
    if failed > 0 {
        return Err(Stop::Reported(format!("{failed} test(s) failed")).into());
    }
    Ok(())
}

/// Runs each test in order. Gives the ledger when it opened, so that the results can be recorded.
pub fn run_checks(ctx: &Ctx) -> (Vec<Check>, Option<Ledger>) {
    let (ledger_check, ledger) = check_ledger(ctx);
    let checks = vec![
        check_home(ctx),
        check_config(ctx),
        ledger_check,
        check_heartbeat(ctx),
        check_git(ctx),
        check_claude(ctx),
        check_memory(ctx),
        check_adapter(ctx),
    ];
    (checks, ledger)
}

/// The adapter of the key `project` loads, and its scripts are executable.
fn check_adapter(ctx: &Ctx) -> Check {
    let adapter = match Config::load(&ctx.paths)
        .and_then(|config| crate::adapter::load_with(&config, &ctx.paths))
    {
        Ok(adapter) => adapter,
        Err(e) => return Check::new("adapter", Level::Fail, format!("{e:#}")),
    };
    let summary = format!(
        "{} from {}: {} steps, {} verbs, {} jobs",
        adapter.name,
        adapter.dir.display(),
        adapter.steps.len(),
        adapter.verbs.len(),
        adapter.jobs.len()
    );
    if let Some(name) = &adapter.builtin {
        // The loader accepts only a known name, and this binary answers the calls.
        return Check::new(
            "adapter",
            Level::Ok,
            format!(
                "{summary}, with the built-in scripts {}{name}",
                crate::adapters::PREFIX
            ),
        );
    }
    let missing: Vec<String> = ["observe", "act", "job"]
        .iter()
        .map(|name| adapter.script_path(name))
        .filter(|path| find_program(&path.display().to_string()).is_none())
        .map(|path| path.display().to_string())
        .collect();
    if missing.is_empty() {
        Check::new("adapter", Level::Ok, summary)
    } else {
        Check::new(
            "adapter",
            Level::Warn,
            format!(
                "{summary}, but {} is missing or not executable",
                missing.join(", ")
            ),
        )
    }
}

fn check_home(ctx: &Ctx) -> Check {
    let paths = &ctx.paths;
    let home = paths.home().display();
    if !paths.home().is_dir() {
        return Check::new(
            "home",
            Level::Fail,
            format!("{home} does not exist. Run `vision init`"),
        );
    }
    let files = [
        ("config.toml", paths.config()),
        ("ledger.sqlite", paths.ledger()),
    ];
    let folders = [
        ("log/", paths.log_dir()),
        ("claude/", paths.claude_dir()),
        ("sandbox/", paths.sandbox_dir()),
        ("skills/", paths.skills_dir()),
        ("memory/", paths.memory_dir()),
        ("adapters/", paths.adapters_dir()),
        ("tickets/", paths.tickets_dir()),
    ];
    let missing: Vec<&str> = files
        .iter()
        .filter(|(_, path)| !path.is_file())
        .chain(folders.iter().filter(|(_, path)| !path.is_dir()))
        .map(|(name, _)| *name)
        .collect();
    if missing.is_empty() {
        Check::new("home", Level::Ok, format!("{home} has each part"))
    } else {
        Check::new(
            "home",
            Level::Fail,
            format!(
                "{home} lacks {}. Run `vision init` to make them",
                missing.join(", ")
            ),
        )
    }
}

fn check_config(ctx: &Ctx) -> Check {
    match Config::load(&ctx.paths) {
        Ok(_) => Check::new("config", Level::Ok, "config.toml is valid"),
        Err(e) => Check::new("config", Level::Fail, format!("{e:#}")),
    }
}

fn check_ledger(ctx: &Ctx) -> (Check, Option<Ledger>) {
    let ledger = match Ledger::open(&ctx.paths.ledger()) {
        Ok(ledger) => ledger,
        Err(e) => return (Check::new("ledger", Level::Fail, format!("{e:#}")), None),
    };
    let check = match ledger.schema_version() {
        Ok(version) if version == SCHEMA_VERSION => {
            let migrated = ledger
                .migrated_from()
                .map(|from| format!(", migrated from version {from} now"))
                .unwrap_or_default();
            Check::new(
                "ledger",
                Level::Ok,
                format!("schema version {version}{migrated}"),
            )
        }
        Ok(version) => Check::new(
            "ledger",
            Level::Fail,
            format!("schema version {version}, but this vision needs {SCHEMA_VERSION}"),
        ),
        Err(e) => Check::new("ledger", Level::Fail, format!("{e:#}")),
    };
    (check, Some(ledger))
}

fn check_heartbeat(ctx: &Ctx) -> Check {
    let last = match read_heartbeat(&ctx.paths) {
        Ok(last) => last,
        Err(e) => {
            return Check::new(
                "heartbeat",
                Level::Fail,
                format!("{e:#}. Run `vision tick`"),
            )
        }
    };
    let age = last.map(|at| time::seconds_between(at, time::now()));
    if let Some(age) = age.filter(|age| *age <= FRESH_SECONDS) {
        return Check::new(
            "heartbeat",
            Level::Ok,
            format!("the last tick was {} ago", time::short_age(age)),
        );
    }
    let what = match age {
        Some(age) => format!("the last tick was {} ago", time::short_age(age)),
        None => "no tick yet".to_string(),
    };
    if install::is_installed(&ctx.paths) {
        Check::new(
            "heartbeat",
            Level::Fail,
            format!(
                "{what}, but the timer is installed. Run `vision tick` to see the error, and read {}",
                install::error_log(&ctx.paths).display()
            ),
        )
    } else {
        Check::new(
            "heartbeat",
            Level::Warn,
            format!(
                "{what}, and the timer is not installed. Run `vision install` to tick each minute"
            ),
        )
    }
}

/// git runs. The detail also names the author of each commit of a session.
fn check_git(ctx: &Ctx) -> Check {
    match git::version() {
        Ok(version) => {
            let commits = ctx.config().ok().map(|config| {
                let (name, email) = crate::turn::identity(&config.user, &config.git);
                format!("; sessions commit as {name} <{email}>")
            });
            Check::new(
                "git",
                Level::Ok,
                format!("{version}{}", commits.unwrap_or_default()),
            )
        }
        Err(e) => Check::new("git", Level::Fail, format!("{e:#}")),
    }
}

fn check_claude(ctx: &Ctx) -> Check {
    let bin = match ctx.claude_bin() {
        Ok(bin) => bin,
        Err(e) => {
            return Check::new(
                "claude",
                Level::Fail,
                format!("could not read claude.bin: {e:#}"),
            )
        }
    };
    match find_program(&bin) {
        Some(path) => Check::new("claude", Level::Ok, path.display().to_string()),
        None => Check::new(
            "claude",
            Level::Fail,
            format!(
                "`{bin}` is not found. Install Claude Code, or set claude.bin in config.toml or VISION_CLAUDE_BIN"
            ),
        ),
    }
}

fn check_memory(ctx: &Ctx) -> Check {
    let dir = ctx.paths.memory_dir();
    if !dir.is_dir() {
        return Check::new(
            "memory",
            Level::Fail,
            format!("{} does not exist. Run `vision init`", dir.display()),
        );
    }
    if git::is_repo_root(&dir) {
        Check::new(
            "memory",
            Level::Ok,
            format!("{} is a git repo", dir.display()),
        )
    } else {
        Check::new(
            "memory",
            Level::Fail,
            format!(
                "{} is not a git repo. Run `vision init` to make it one",
                dir.display()
            ),
        )
    }
}

fn record(ledger: &Ledger, checks: &[Check]) -> Result<()> {
    let at = time::now_rfc3339();
    for check in checks {
        let detail = match check.result {
            Level::Warn => format!("warning: {}", check.detail),
            _ => check.detail.clone(),
        };
        ledger.add_health(&HealthRow {
            at: at.clone(),
            test: check.test.to_string(),
            ok: check.result != Level::Fail,
            detail,
        })?;
    }
    Ok(())
}

fn render(checks: &[Check]) -> String {
    let width = checks.iter().map(|c| c.test.len()).max().unwrap_or(0);
    let mut out = String::new();
    for check in checks {
        out.push_str(&format!(
            "{:<4}  {:<width$}  {}\n",
            check.result.word(),
            check.test,
            check.detail
        ));
    }
    let count = |level: Level| checks.iter().filter(|c| c.result == level).count();
    out.push_str(&format!(
        "{} tests: {} ok, {} warning(s), {} failed\n",
        checks.len(),
        count(Level::Ok),
        count(Level::Warn),
        count(Level::Fail)
    ));
    out
}
