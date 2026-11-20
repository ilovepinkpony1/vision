//! Jobs in the background (section 11.1). A job such as the proof can run for many minutes. A
//! pass does not wait for it, because a pass holds the lock of the tick, and each action of the
//! person and each other ticket waits for that lock.
//!
//! 1. A pass asks for a run with [`request`]. The run gets a folder `jobs/<id>/` with `run.json`,
//!    the call of `bin/job` with no secret, in the phase `queued`.
//! 2. [`launch`] starts `vision job-run <id>` as a detached process for each queued run, up to
//!    `caps.jobs_at_once` runs and one run for each ticket at a time.
//! 3. The runner ([`run_command`]) holds the lock `runner.lock` while it lives. It reads the
//!    secrets, runs the script, writes the masked answer to `answer.json`, and ends the run.
//! 4. A later pass collects the run ([`collect`]): it stores the checks, the documents, and the
//!    row of the kind `job` as a job in the pass does. It also stores one event whose external id
//!    is the mark of the run, so that the caller that asked for the run finds its outcome
//!    ([`state`]). Then it removes the folder.
//!
//! A run that the person stopped stores no outcome, so the job runs again later. A runner that
//! died without an answer runs one more time; a second death is a failure of the job.

use std::fs::{File, OpenOptions};
use std::io;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};

use super::jobs::{self, JobCall};
use super::scripts::{Answer, ScriptError, Secrets};
use crate::ctx::Ctx;
use crate::ledger::{Event, Ledger};
use crate::paths::{Paths, HOME_ENV};
use crate::tick::JobRun;
use crate::util::fs::{atomic_write, read_optional};
use crate::util::time;

/// How long a run that a pass started may stay without its lock before it counts as dead: the
/// time that a new runner needs to start and take the lock.
const START_GRACE: Duration = Duration::from_secs(60);

/// The phase of a run in `run.json`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RunPhase {
    /// It waits for a free place.
    Queued,
    /// A pass started its runner.
    Running,
    /// The runner wrote its answer. The next pass collects it.
    Ended,
}

/// `run.json`: one run of a job in the background.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RunRecord {
    /// Such as `20261120T083753Z-DEMO-12-proof`.
    pub id: String,
    pub ticket: String,
    pub step: String,
    pub job: String,
    /// Who waits for the outcome: the external id of the event that stores it.
    pub mark: String,
    pub call: JobCall,
    pub phase: RunPhase,
    /// When the pass asked for it.
    pub queued: String,
    /// When a pass started its runner.
    #[serde(default)]
    pub launched: Option<String>,
    /// The process id of the runner.
    #[serde(default)]
    pub pid: Option<u32>,
    /// When the runner took its lock. A run with this time and a free lock lost its runner.
    #[serde(default)]
    pub ran: Option<String>,
    #[serde(default)]
    pub ended: Option<String>,
    /// The seconds of the script.
    #[serde(default)]
    pub seconds: Option<i64>,
}

/// `answer.json`: what the runner got, with each secret masked.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum RunAnswer {
    /// The script ended with code 0.
    Done { answer: Map<String, Value> },
    /// The script refused the call (code 2).
    Refused { answer: Map<String, Value> },
    /// The script failed, timed out, or could not start.
    Failed { why: String },
    /// The person stopped it. No outcome is stored.
    Stopped,
}

/// The stored outcome of a run, for the caller that asked for it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RunOutcome {
    pub run: String,
    pub job: String,
    pub head: String,
    pub ok: bool,
    #[serde(default)]
    pub failure: Option<String>,
    #[serde(default)]
    pub detail: String,
    /// The documents that it wrote to the ticket folder.
    #[serde(default)]
    pub documents: Vec<String>,
}

impl RunOutcome {
    /// Why it did not pass: the failure of the script, else what the job said.
    pub fn why(&self) -> String {
        self.failure.clone().unwrap_or_else(|| self.detail.clone())
    }
}

/// What a pass knows of the run of a mark.
#[derive(Clone, Debug, PartialEq)]
pub enum RunState {
    /// No run of the mark waits, runs, or ended.
    None,
    /// A run waits for a place, runs, or ended and waits for the next pass.
    InFlight(Box<RunRecord>),
    /// A run ended, and a pass stored its outcome.
    Ended(RunOutcome),
}

fn dir_of(paths: &Paths, id: &str) -> PathBuf {
    paths.jobs_dir().join(id)
}

fn record_path(dir: &Path) -> PathBuf {
    dir.join("run.json")
}

fn answer_path(dir: &Path) -> PathBuf {
    dir.join("answer.json")
}

fn lock_path(dir: &Path) -> PathBuf {
    dir.join("runner.lock")
}

/// The log of the runner: its own standard output and error.
pub fn log_path(paths: &Paths, id: &str) -> PathBuf {
    dir_of(paths, id).join("runner.log")
}

fn write_record(paths: &Paths, record: &RunRecord) -> Result<()> {
    let dir = dir_of(paths, &record.id);
    std::fs::create_dir_all(&dir).with_context(|| format!("could not make {}", dir.display()))?;
    let mut text = serde_json::to_string_pretty(record)?;
    text.push('\n');
    atomic_write(&record_path(&dir), text.as_bytes())
}

fn read_record(dir: &Path) -> Option<RunRecord> {
    let text = read_optional(&record_path(dir)).ok().flatten()?;
    serde_json::from_str(&text).ok()
}

/// Each run that has a folder, the oldest first.
pub fn all(paths: &Paths) -> Vec<RunRecord> {
    let Ok(entries) = std::fs::read_dir(paths.jobs_dir()) else {
        return Vec::new();
    };
    let mut runs: Vec<RunRecord> = entries
        .filter_map(|e| e.ok())
        .filter_map(|e| read_record(&e.path()))
        .collect();
    runs.sort_by(|a, b| (&a.queued, &a.id).cmp(&(&b.queued, &b.id)));
    runs
}

/// The runs of a ticket that wait, run, or wait for the next pass.
pub fn of_ticket(paths: &Paths, ticket: &str) -> Vec<RunRecord> {
    all(paths)
        .into_iter()
        .filter(|r| r.ticket == ticket)
        .collect()
}

/// The run of a job of a ticket that waits or runs now, if any.
pub fn in_flight_of(paths: &Paths, ticket: &str, job: &str) -> Option<RunRecord> {
    all(paths)
        .into_iter()
        .find(|r| r.ticket == ticket && r.job == job)
}

/// The runs that wait or run, as records of the view: `what` is `job` for a run and
/// `queued job` for a run that waits for a place. A run that ended and waits for the next pass
/// is left out.
pub fn views(paths: &Paths) -> Vec<crate::running::Running> {
    all(paths)
        .into_iter()
        .filter(|r| r.phase != RunPhase::Ended)
        .map(|r| crate::running::Running {
            what: if r.phase == RunPhase::Queued {
                "queued job".into()
            } else {
                "job".into()
            },
            name: r.job,
            ticket: r.ticket,
            started: r.launched.unwrap_or(r.queued),
            pid: r.pid.unwrap_or(0),
        })
        .collect()
}

/// The number of runs that ended and wait for a pass to collect them.
pub fn ended_count(paths: &Paths) -> usize {
    all(paths)
        .iter()
        .filter(|r| r.phase == RunPhase::Ended)
        .count()
}

/// The outcome of the mark, or its run that is in flight.
pub fn state(paths: &Paths, ledger: &Ledger, mark: &str) -> Result<RunState> {
    if let Some(detail) = ledger.event_detail(mark)? {
        let outcome = detail
            .as_deref()
            .and_then(|text| serde_json::from_str::<RunOutcome>(text).ok());
        if let Some(outcome) = outcome {
            return Ok(RunState::Ended(outcome));
        }
    }
    Ok(match all(paths).into_iter().find(|r| r.mark == mark) {
        Some(record) => RunState::InFlight(Box::new(record)),
        None => RunState::None,
    })
}

/// The mark of a run that an act step or a job step asks for: the job at the commit of now.
pub fn head_mark(run: &JobRun, head: &str) -> String {
    format!(
        "job-run:{}:{}:{}:{}",
        run.ticket,
        run.step,
        head,
        jobs::job_command(&run.job, &run.args)
    )
}

/// Asks for a run of a job, unless the mark has one already. Builds the call now, writes the
/// run in the phase `queued`, and starts the runners that have a place. Gives the state of the
/// mark after that. `mark` `None` is the mark of [`head_mark`].
pub fn request(ctx: &Ctx, ledger: &Ledger, run: &JobRun, mark: Option<&str>) -> Result<RunState> {
    let call = jobs::prepare(ctx, ledger, run)?;
    let mark = match mark {
        Some(mark) => mark.to_string(),
        None => head_mark(run, &call.head),
    };
    let known = state(&ctx.paths, ledger, &mark)?;
    if known != RunState::None {
        return Ok(known);
    }
    let now = time::now();
    let id = format!(
        "{}-{}-{}",
        time::compact(now),
        run.ticket,
        run.job
            .replace(|c: char| !c.is_ascii_alphanumeric() && c != '-', "-")
    );
    let id = unique_id(&ctx.paths, &id);
    let record = RunRecord {
        id: id.clone(),
        ticket: run.ticket.to_string(),
        step: run.step.clone(),
        job: run.job.clone(),
        mark,
        call,
        phase: RunPhase::Queued,
        queued: time::rfc3339(now),
        launched: None,
        pid: None,
        ran: None,
        ended: None,
        seconds: None,
    };
    write_record(&ctx.paths, &record)?;
    ctx.log(&format!(
        "job {} of {} at {}: it runs in the background as {id}",
        run.job,
        run.ticket,
        super::short(&record.call.head)
    ));
    launch(ctx)?;
    let record = read_record(&dir_of(&ctx.paths, &id)).unwrap_or(record);
    Ok(RunState::InFlight(Box::new(record)))
}

fn unique_id(paths: &Paths, id: &str) -> String {
    let mut candidate = id.to_string();
    let mut n = 2;
    while dir_of(paths, &candidate).exists() {
        candidate = format!("{id}-{n}");
        n += 1;
    }
    candidate
}

/// Starts a runner for each queued run that has a place: at most `caps.jobs_at_once` runs at
/// a time, and one for each ticket.
pub fn launch(ctx: &Ctx) -> Result<()> {
    let limit = usize::try_from(ctx.config()?.caps.jobs_at_once.max(1)).unwrap_or(1);
    let runs = all(&ctx.paths);
    let mut busy: Vec<String> = runs
        .iter()
        .filter(|r| r.phase == RunPhase::Running)
        .map(|r| r.ticket.clone())
        .collect();
    for mut record in runs.into_iter().filter(|r| r.phase == RunPhase::Queued) {
        if busy.len() >= limit {
            break;
        }
        if busy.contains(&record.ticket) {
            continue;
        }
        // The record is written before the runner starts, so that a fast runner never finds
        // its own writes undone. The runner writes its pid.
        record.phase = RunPhase::Running;
        record.launched = Some(time::now_rfc3339());
        write_record(&ctx.paths, &record)?;
        if let Err(e) = start_runner(ctx, &record.id) {
            record.phase = RunPhase::Queued;
            record.launched = None;
            write_record(&ctx.paths, &record)?;
            return Err(e);
        }
        busy.push(record.ticket.clone());
    }
    Ok(())
}

/// Starts `vision job-run <id>` as a detached process: its own session and process group,
/// standard input from `/dev/null`, and its output in `runner.log`. Gives its pid.
fn start_runner(ctx: &Ctx, id: &str) -> Result<u32> {
    let program = crate::turn::vision_bin()?;
    let log_path = log_path(&ctx.paths, id);
    let log = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)
        .with_context(|| format!("could not open {}", log_path.display()))?;
    let mut command = Command::new(&program);
    command
        .arg("job-run")
        .arg(id)
        .env(HOME_ENV, ctx.paths.home())
        .current_dir(ctx.paths.home())
        .stdin(Stdio::null())
        .stdout(log.try_clone()?)
        .stderr(log);
    // SAFETY: setsid is async-signal-safe, and the closure touches no other state.
    unsafe {
        command.pre_exec(|| {
            if libc::setsid() < 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let mut child = command
        .spawn()
        .with_context(|| format!("could not start {}", program.display()))?;
    let pid = child.id();
    // Reap the runner when it ends while this process still lives, so that it never stays a
    // zombie. When this process ends first, the system adopts and reaps it.
    std::thread::spawn(move || {
        let _ = child.wait();
    });
    Ok(pid)
}

/// The held lock of a runner. The system releases it when the process ends.
struct RunnerLock {
    _file: File,
}

fn take_lock(dir: &Path) -> Result<Option<RunnerLock>> {
    let path = lock_path(dir);
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&path)
        .with_context(|| format!("could not open {}", path.display()))?;
    match fs2::FileExt::try_lock_exclusive(&file) {
        Ok(()) => Ok(Some(RunnerLock { _file: file })),
        Err(e) if e.raw_os_error() == fs2::lock_contended_error().raw_os_error() => Ok(None),
        Err(e) => Err(e).with_context(|| format!("could not lock {}", path.display())),
    }
}

/// True while a runner of the run lives: it holds the lock. A test of the lock never blocks.
pub fn runner_lives(paths: &Paths, id: &str) -> bool {
    let dir = dir_of(paths, id);
    if !lock_path(&dir).exists() {
        return false;
    }
    matches!(take_lock(&dir), Ok(None))
}

extern "C" fn on_signal(_signal: libc::c_int) {
    super::scripts::request_stop();
}

fn install_signal_handlers() {
    let handler: extern "C" fn(libc::c_int) = on_signal;
    for signal in [libc::SIGTERM, libc::SIGINT, libc::SIGHUP] {
        // SAFETY: the handler only stores to an atomic, which is async-signal-safe.
        unsafe {
            libc::signal(signal, handler as libc::sighandler_t);
        }
    }
}

/// `vision job-run <id>`: the runner of one run. A second runner of the same run exits at once.
pub fn run_command(ctx: &Ctx, id: &str) -> Result<()> {
    let dir = dir_of(&ctx.paths, id);
    let Some(_lock) = take_lock(&dir)? else {
        println!("another runner runs the job {id}");
        return Ok(());
    };
    install_signal_handlers();
    let Some(mut record) = read_record(&dir) else {
        anyhow::bail!(
            "the job run {id} has no readable run.json in {}",
            dir.display()
        );
    };
    if record.phase == RunPhase::Ended {
        return Ok(());
    }
    record.pid = Some(std::process::id());
    record.ran = Some(time::now_rfc3339());
    record.phase = RunPhase::Running;
    write_record(&ctx.paths, &record)?;
    let started = Instant::now();
    let answer = match ctx
        .config()
        .and_then(Secrets::read)
        .and_then(|secrets| jobs::call(ctx, &record.call, &secrets))
    {
        Ok(Ok(Answer::Done(map))) => RunAnswer::Done { answer: map },
        Ok(Ok(Answer::Refused(map))) => RunAnswer::Refused { answer: map },
        Ok(Err(ScriptError::Stopped { .. })) => RunAnswer::Stopped,
        Ok(Err(e)) => RunAnswer::Failed { why: e.to_string() },
        Err(e) => RunAnswer::Failed {
            why: format!("{e:#}"),
        },
    };
    // A stop that came while the secrets were read also ends the run with no outcome.
    let answer = if super::scripts::stop_requested() {
        RunAnswer::Stopped
    } else {
        answer
    };
    let mut text = serde_json::to_string_pretty(&answer)?;
    text.push('\n');
    atomic_write(&answer_path(&dir), text.as_bytes())?;
    record.phase = RunPhase::Ended;
    record.ended = Some(time::now_rfc3339());
    record.seconds = Some(i64::try_from(started.elapsed().as_secs()).unwrap_or(i64::MAX));
    write_record(&ctx.paths, &record)
}

fn read_answer(dir: &Path) -> Option<RunAnswer> {
    let text = read_optional(&answer_path(dir)).ok().flatten()?;
    serde_json::from_str(&text).ok()
}

fn remove(paths: &Paths, id: &str) {
    let _ = std::fs::remove_dir_all(dir_of(paths, id));
}

/// The external id of the event that counts the deaths of the runner of a mark.
fn crash_mark(mark: &str) -> String {
    format!("{mark}:runner-died")
}

/// Tick step 4: collects each run that ended, repairs each run whose runner died, and starts
/// the runners that have a place. A run that cannot be collected stays for the next pass.
pub fn collect(ctx: &Ctx, ledger: &Ledger) -> Result<()> {
    for record in all(&ctx.paths) {
        let dir = dir_of(&ctx.paths, &record.id);
        match record.phase {
            RunPhase::Queued => {}
            RunPhase::Ended => match read_answer(&dir) {
                Some(answer) => store(ctx, ledger, &record, answer)?,
                None => died(ctx, ledger, record)?,
            },
            RunPhase::Running => {
                // A runner that took its lock and lost it died. A runner that never took it
                // gets the time to start.
                let young = record.ran.is_none()
                    && record
                        .launched
                        .as_deref()
                        .and_then(|at| time::parse_rfc3339(at).ok())
                        .is_some_and(|at| {
                            time::seconds_between(at, time::now()) < START_GRACE.as_secs() as i64
                        });
                if !young && !runner_lives(&ctx.paths, &record.id) {
                    // The runner may have written its end between the read and the test.
                    match read_record(&dir) {
                        Some(again) if again.phase == RunPhase::Ended => {
                            if let Some(answer) = read_answer(&dir) {
                                store(ctx, ledger, &again, answer)?;
                            } else {
                                died(ctx, ledger, again)?;
                            }
                        }
                        Some(again) => died(ctx, ledger, again)?,
                        None => {}
                    }
                }
            }
        }
    }
    launch(ctx)
}

/// Stores the answer of an ended run, then removes its folder.
fn store(ctx: &Ctx, ledger: &Ledger, record: &RunRecord, answer: RunAnswer) -> Result<()> {
    let run = JobRun {
        ticket: crate::paths::TicketKey::parse(&record.ticket)?,
        step: record.step.clone(),
        job: record.job.clone(),
        args: record.call.args.clone(),
    };
    let answer = match answer {
        RunAnswer::Stopped => {
            ctx.log(&format!(
                "job {} of {}: stopped, so it stores no result and runs again later",
                record.job, record.ticket
            ));
            remove(&ctx.paths, &record.id);
            return Ok(());
        }
        RunAnswer::Done { answer } => Ok(Answer::Done(answer)),
        RunAnswer::Refused { answer } => Ok(Answer::Refused(answer)),
        RunAnswer::Failed { why } => Err(why),
    };
    // The runner masked the answer with the secrets of its time.
    let result = jobs::finish(
        ctx,
        ledger,
        &run,
        &record.call,
        answer,
        record.seconds.unwrap_or(0),
        &Secrets::default(),
    )?;
    let outcome = RunOutcome {
        run: record.id.clone(),
        job: record.job.clone(),
        head: record.call.head.clone(),
        ok: result.ok && result.failure.is_none(),
        failure: result.failure.clone(),
        detail: result.detail.clone(),
        documents: result.documents.clone(),
    };
    ledger.add_event(
        &Event::now("executor", "job-run")
            .ticket(&record.ticket)
            .external_id(&record.mark)
            .detail(serde_json::to_value(&outcome)?),
    )?;
    remove(&ctx.paths, &record.id);
    Ok(())
}

/// A run whose runner died without an answer runs one more time. A second death stores a
/// failure, so that a runner that always dies cannot hold the ticket forever.
fn died(ctx: &Ctx, ledger: &Ledger, mut record: RunRecord) -> Result<()> {
    let log = log_path(&ctx.paths, &record.id);
    if ledger.event_detail(&crash_mark(&record.mark))?.is_none() {
        ledger.add_event(
            &Event::now("executor", "job-run-died")
                .ticket(&record.ticket)
                .external_id(&crash_mark(&record.mark))
                .detail(json!({"run": record.id, "job": record.job})),
        )?;
        ctx.log(&format!(
            "job {} of {}: its runner ended without an answer, so it runs one more time. See {}",
            record.job,
            record.ticket,
            log.display()
        ));
        record.phase = RunPhase::Queued;
        record.launched = None;
        record.pid = None;
        record.ran = None;
        let _ = std::fs::remove_file(answer_path(&dir_of(&ctx.paths, &record.id)));
        return write_record(&ctx.paths, &record);
    }
    let why = format!(
        "the runner of the job ended two times without an answer, as when the Mac restarts during the job. See {}",
        log.display()
    );
    store(ctx, ledger, &record, RunAnswer::Failed { why })
}

/// Stops the runs of a ticket, or each run when `ticket` is `None`: a queued run ends at once,
/// and the runner of a running run gets SIGTERM, kills its script, and ends with no outcome.
/// Gives the ids of the runs that it stopped.
pub fn stop(paths: &Paths, ticket: Option<&str>) -> Vec<String> {
    let mut stopped = Vec::new();
    for mut record in all(paths) {
        if ticket.is_some_and(|t| t != record.ticket) || record.phase == RunPhase::Ended {
            continue;
        }
        let dir = dir_of(paths, &record.id);
        let lives = runner_lives(paths, &record.id);
        // A runner that just took its lock writes its pid at once.
        if lives && record.pid.is_none() {
            let until = Instant::now() + Duration::from_secs(2);
            while record.pid.is_none() && Instant::now() < until {
                std::thread::sleep(Duration::from_millis(20));
                if let Some(again) = read_record(&dir) {
                    record = again;
                }
            }
            if record.phase == RunPhase::Ended {
                continue;
            }
        }
        match (record.phase, lives, record.pid) {
            (RunPhase::Running, true, Some(pid)) => {
                if let Ok(pid) = libc::pid_t::try_from(pid) {
                    // SAFETY: the runner holds its lock, so this pid is the runner of this run.
                    unsafe {
                        libc::kill(pid, libc::SIGTERM);
                    }
                }
            }
            _ => {
                // No runner lives: the run ends here with no outcome. A runner that starts
                // later finds the end and exits.
                let text = serde_json::to_string(&RunAnswer::Stopped).unwrap_or_default();
                let _ = atomic_write(&answer_path(&dir), text.as_bytes());
                record.phase = RunPhase::Ended;
                record.ended = Some(time::now_rfc3339());
                let _ = write_record(paths, &record);
            }
        }
        stopped.push(record.id);
    }
    stopped
}

/// Waits up to `limit` until no runner of the ticket lives, for a caller that stopped them.
pub fn wait_stopped(paths: &Paths, ticket: &str, limit: Duration) {
    let until = Instant::now() + limit;
    while Instant::now() < until
        && of_ticket(paths, ticket)
            .iter()
            .any(|r| runner_lives(paths, &r.id))
    {
        std::thread::sleep(Duration::from_millis(100));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_run_answer_keeps_its_kind_in_json() {
        let mut map = Map::new();
        map.insert("ok".into(), json!(true));
        for answer in [
            RunAnswer::Done {
                answer: map.clone(),
            },
            RunAnswer::Refused { answer: map },
            RunAnswer::Failed { why: "x".into() },
            RunAnswer::Stopped,
        ] {
            let text = serde_json::to_string(&answer).unwrap();
            assert_eq!(serde_json::from_str::<RunAnswer>(&text).unwrap(), answer);
        }
        assert_eq!(
            serde_json::to_string(&RunAnswer::Stopped).unwrap(),
            r#"{"kind":"stopped"}"#
        );
    }

    #[test]
    fn the_why_of_an_outcome_is_the_failure_before_the_detail() {
        let mut outcome = RunOutcome {
            run: "r".into(),
            job: "proof".into(),
            head: "abc".into(),
            ok: false,
            failure: None,
            detail: "1 test fails".into(),
            documents: Vec::new(),
        };
        assert_eq!(outcome.why(), "1 test fails");
        outcome.failure = Some("bin/job failed".into());
        assert_eq!(outcome.why(), "bin/job failed");
    }
}
