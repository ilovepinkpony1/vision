//! The supervisor (section 7.4): `vision turn-run <turn-id>`, the process that owns one
//! running session.
//!
//! 1. Write `turn.json` with the state `starting`, its pid, and the time.
//! 2. Start `claude` with pipes for input and output, in its own process group, with the
//!    environment of the allow list. Write the first message as one line.
//! 3. Copy each output line to `stream.jsonl`. Read each line as an event.
//! 4. At the `init` event, compare the tools, the permission mode, and the MCP servers with the
//!    contract. On a difference, kill the group and end with the reason `contract`.
//! 5. Each second, write each waiting message of `inbox/` as one line, and move its file to
//!    `inbox/sent/`.
//! 6. At a `result` event, wait 2 seconds for a late message. If none came, close the input.
//! 7. At the time limit, kill the process group and end with the reason `timeout`.
//! 8. Write `result.json` and the final `turn.json`.
//!
//! On SIGTERM, SIGINT, or SIGHUP it kills the group and ends with the reason `killed`. When
//! `claude` ends, the supervisor kills what is left of its group, so that no child stays.

use std::collections::BTreeSet;
use std::fs::OpenOptions;
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom, Write};
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicI32, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use serde_json::Value;

use crate::ctx::Ctx;
use crate::ledger::Ledger;
use crate::paths::{TicketKey, TurnId, TurnPaths};
use crate::turn::launch::{Contract, LaunchFile, Plan, STDERR_LOG};
use crate::turn::state::{self, Phase, Reason, TurnState};
use crate::turn::stream::{self, Event, ResultEvent};
use crate::turn::{inbox, settings};
use crate::util::fs::atomic_write;
use crate::util::time;

/// The wait for a late message after a `result` event.
pub const RESULT_GRACE: Duration = Duration::from_secs(2);
/// How often the inbox is read.
pub const INBOX_EVERY: Duration = Duration::from_secs(1);
/// How long `claude` may run after its input closed before the supervisor stops it.
const EXIT_GRACE: Duration = Duration::from_secs(10);
/// The wait between TERM and KILL.
const TERM_GRACE: Duration = Duration::from_secs(1);
const POLL: Duration = Duration::from_millis(50);

static SIGNAL: AtomicI32 = AtomicI32::new(0);

extern "C" fn on_signal(signal: libc::c_int) {
    SIGNAL.store(signal, Ordering::SeqCst);
}

fn install_signal_handlers() {
    let handler: extern "C" fn(libc::c_int) = on_signal;
    for signal in [libc::SIGTERM, libc::SIGINT, libc::SIGHUP] {
        // SAFETY: the handler only stores an integer in an atomic, which is async-signal-safe.
        unsafe {
            libc::signal(signal, handler as libc::sighandler_t);
        }
    }
}

enum Incoming {
    Event(Event),
    Eof,
}

/// `vision turn-run <turn-id>`.
pub fn run(ctx: &Ctx, id: &TurnId) -> Result<()> {
    let ledger = ctx.ledger()?;
    let row = ledger.turn(&id.to_string())?.with_context(|| {
        format!("the ledger has no turn {id}. The runner writes the row before it starts the supervisor")
    })?;
    let key = TicketKey::parse(&row.ticket)?;
    let turn = ctx.paths.turn(&key, id);
    let Some(_lock) = state::take_lock(&turn.dir)? else {
        bail!("the turn {id} already has a supervisor");
    };
    let current = TurnState::read(&turn.status)?.unwrap_or_else(|| TurnState::queued(&row.id));
    if current.is_ended() {
        bail!(
            "the turn {id} already ended with the reason {}",
            current.reason.map_or("unknown", Reason::as_str)
        );
    }
    let plan = LaunchFile::read(&turn.dir)?.plan;
    let now = time::now_rfc3339();
    let mut status = current;
    status.state = Phase::Starting;
    status.pid = Some(std::process::id());
    status.at.clone_from(&now);
    status.started = Some(now);
    status.session = Some(plan.session.clone());
    if ctx.paths.off().exists() {
        finish(
            ctx,
            &turn,
            status.ended(Reason::Killed, Some("Vision is off".into())),
            None,
        )?;
        return Ok(());
    }
    install_signal_handlers();
    status.write(&turn.status)?;
    supervise(ctx, &ledger, &turn, &plan, status)
}

fn supervise(
    ctx: &Ctx,
    ledger: &Ledger,
    turn: &TurnPaths,
    plan: &Plan,
    mut status: TurnState,
) -> Result<()> {
    let stderr_path = turn.dir.join(STDERR_LOG);
    let stderr = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&stderr_path)
        .with_context(|| format!("could not open {}", stderr_path.display()))?;
    let mut command = Command::new(&plan.program);
    command
        .args(&plan.args)
        .current_dir(&plan.workdir)
        .env_clear()
        .envs(settings::session_env(std::env::vars_os(), &plan.env_pass))
        .envs(&plan.env_set)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(stderr)
        .process_group(0);
    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(e) => {
            let detail = format!(
                "could not start `{}`: {e}. Install Claude Code, or set claude.bin in config.toml or VISION_CLAUDE_BIN",
                plan.program
            );
            return finish(ctx, turn, status.ended(Reason::Error, Some(detail)), None);
        }
    };
    let pgid = child.id();
    status.state = Phase::Running;
    status.child_pid = Some(pgid);
    status.at = time::now_rfc3339();
    status.write(&turn.status)?;

    let (Some(stdin), Some(stdout)) = (child.stdin.take(), child.stdout.take()) else {
        stop_group(&mut child, pgid, true);
        let _ = child.wait();
        let detail = "claude started without its pipes".to_string();
        return finish(ctx, turn, status.ended(Reason::Error, Some(detail)), None);
    };
    let writer = spawn_writer(stdin);
    let events = spawn_reader(stdout, turn.stream.clone());
    let mut run = Run {
        writer: Some(writer),
        events,
        eof: false,
        result: None,
        session: None,
        forced: None,
    };
    run.send(&plan.first_message);
    let exit = run.watch(ledger, turn, plan, &mut child, pgid);
    let exit = match exit {
        Some(exit) => Some(exit),
        None => child.wait().ok(),
    };
    // Kill what is left of the group, such as a command that the session started in the
    // background. Then the pipe closes, and the last lines arrive.
    state::signal_group(pgid, libc::SIGKILL);
    run.writer = None;
    run.drain(Duration::from_secs(2));

    let (reason, detail) = match run.forced.take() {
        Some((reason, detail)) => (reason, Some(detail)),
        None => match &run.result {
            Some(result) if !result.is_error => (Reason::Result, None),
            Some(result) => (
                Reason::Error,
                Some(format!(
                    "the session ended with the error result `{}`{}",
                    result.subtype,
                    result
                        .result
                        .as_deref()
                        .map(|r| format!(": {}", short(r, 300)))
                        .unwrap_or_default()
                )),
            ),
            None => (
                Reason::Error,
                Some(format!(
                    "claude {} and gave no result{}",
                    exit_text(exit),
                    tail_text(&stderr_path)
                )),
            ),
        },
    };
    status.exit_code = exit.and_then(|e| e.code());
    if let Some(session) = run.session.take() {
        status.session = Some(session);
    }
    let object = run.result.and_then(|r| r.structured_output);
    finish(ctx, turn, status.ended(reason, detail), object)
}

struct Run {
    writer: Option<Sender<String>>,
    events: Receiver<Incoming>,
    eof: bool,
    result: Option<ResultEvent>,
    session: Option<String>,
    forced: Option<(Reason, String)>,
}

impl Run {
    /// Queues one input line. False when the input is closed.
    fn send(&mut self, text: &str) -> bool {
        let Some(writer) = &self.writer else {
            return false;
        };
        if writer.send(inbox::input_line(text)).is_err() {
            self.writer = None;
            return false;
        }
        true
    }

    /// Runs until `claude` exits or the supervisor stops it. Gives the exit status when the
    /// supervisor saw it.
    fn watch(
        &mut self,
        ledger: &Ledger,
        turn: &TurnPaths,
        plan: &Plan,
        child: &mut Child,
        pgid: u32,
    ) -> Option<ExitStatus> {
        let deadline = Instant::now() + Duration::from_secs(plan.time_limit_secs);
        let mut last_inbox: Option<Instant> = None;
        let mut result_at: Option<Instant> = None;
        let mut closed_at: Option<Instant> = None;
        loop {
            let signal = SIGNAL.load(Ordering::SeqCst);
            if signal != 0 {
                self.forced = Some((
                    Reason::Killed,
                    format!("the supervisor got the signal {signal}"),
                ));
                stop_group(child, pgid, false);
                return None;
            }
            if Instant::now() >= deadline {
                self.forced = Some((
                    Reason::Timeout,
                    format!("the time limit of {} seconds passed", plan.time_limit_secs),
                ));
                stop_group(child, pgid, false);
                return None;
            }
            let incoming = if self.eof {
                thread::sleep(POLL);
                None
            } else {
                match self.events.recv_timeout(POLL) {
                    Ok(incoming) => Some(incoming),
                    Err(RecvTimeoutError::Timeout) => None,
                    Err(RecvTimeoutError::Disconnected) => Some(Incoming::Eof),
                }
            };
            match incoming {
                Some(Incoming::Event(event)) => {
                    if let Event::Init {
                        tools,
                        permission_mode,
                        mcp_servers,
                        ..
                    } = &event
                    {
                        if let Some(problem) = contract_problem(
                            &plan.contract,
                            tools,
                            permission_mode.as_deref(),
                            mcp_servers,
                        ) {
                            self.note(&event);
                            self.forced = Some((Reason::Contract, problem));
                            stop_group(child, pgid, true);
                            return None;
                        }
                    }
                    if matches!(event, Event::Result(_)) {
                        result_at = Some(Instant::now());
                    }
                    self.note(&event);
                }
                Some(Incoming::Eof) => self.eof = true,
                None => {}
            }
            if self.writer.is_some() && last_inbox.is_none_or(|at| at.elapsed() >= INBOX_EVERY) {
                last_inbox = Some(Instant::now());
                if self.deliver(ledger, turn) > 0 {
                    // The session continues with the message, so wait for its next result.
                    result_at = None;
                }
            }
            if let Some(at) = result_at {
                if self.writer.is_some() && at.elapsed() >= RESULT_GRACE {
                    self.writer = None;
                    closed_at = Some(Instant::now());
                }
            }
            if closed_at.is_some_and(|at| at.elapsed() >= EXIT_GRACE) {
                stop_group(child, pgid, false);
                return None;
            }
            match child.try_wait() {
                Ok(Some(exit)) => return Some(exit),
                Ok(None) => {}
                Err(e) => {
                    eprintln!("could not wait for claude: {e}");
                    stop_group(child, pgid, true);
                    return None;
                }
            }
        }
    }

    /// Records the session id and the last result of an event.
    fn note(&mut self, event: &Event) {
        match event {
            Event::Init {
                session_id: Some(session),
                ..
            } => self.session = Some(session.clone()),
            Event::Result(result) => {
                if let Some(session) = &result.session_id {
                    self.session = Some(session.clone());
                }
                self.result = Some(result.clone());
            }
            _ => {}
        }
    }

    /// Reads the events that are left, until the stream ends or `limit` passes.
    fn drain(&mut self, limit: Duration) {
        let until = Instant::now() + limit;
        while !self.eof && Instant::now() < until {
            match self.events.recv_timeout(Duration::from_millis(100)) {
                Ok(Incoming::Event(event)) => self.note(&event),
                Ok(Incoming::Eof) | Err(RecvTimeoutError::Disconnected) => self.eof = true,
                Err(RecvTimeoutError::Timeout) => {}
            }
        }
    }

    /// Writes each waiting message to the session. Gives how many it sent.
    fn deliver(&mut self, ledger: &Ledger, turn: &TurnPaths) -> usize {
        let waiting = match inbox::waiting(&turn.inbox) {
            Ok(waiting) => waiting,
            Err(e) => {
                eprintln!("could not read the inbox: {e:#}");
                return 0;
            }
        };
        let mut sent = 0;
        for (path, message) in waiting {
            if !self.send(&message.text) {
                break;
            }
            sent += 1;
            if let Err(e) = inbox::move_to(&path, &turn.inbox_sent) {
                eprintln!("{e:#}");
            }
            if let Some(id) = message.id {
                if let Err(e) = ledger.set_message_delivered(id, &time::now_rfc3339()) {
                    eprintln!("could not record the delivery of message {id}: {e:#}");
                }
            }
        }
        sent
    }
}

/// The thread that writes input lines. Dropping the sender closes the input of `claude`.
fn spawn_writer(mut stdin: std::process::ChildStdin) -> Sender<String> {
    let (tx, rx) = mpsc::channel::<String>();
    thread::spawn(move || {
        for line in rx {
            let written = stdin
                .write_all(line.as_bytes())
                .and_then(|()| stdin.write_all(b"\n"))
                .and_then(|()| stdin.flush());
            if written.is_err() {
                break;
            }
        }
    });
    tx
}

/// The thread that copies each output line to `stream.jsonl` and parses it. A line with no
/// newline at the end of the output is copied with one.
fn spawn_reader(
    stdout: std::process::ChildStdout,
    stream_path: std::path::PathBuf,
) -> Receiver<Incoming> {
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let mut out = match OpenOptions::new()
            .create(true)
            .append(true)
            .open(&stream_path)
        {
            Ok(file) => Some(file),
            Err(e) => {
                eprintln!("could not open {}: {e}", stream_path.display());
                None
            }
        };
        let mut reader = BufReader::new(stdout);
        let mut line = Vec::new();
        loop {
            line.clear();
            match reader.read_until(b'\n', &mut line) {
                Ok(0) => break,
                Ok(_) => {
                    if !line.ends_with(b"\n") {
                        line.push(b'\n');
                    }
                    if let Some(file) = out.as_mut() {
                        if let Err(e) = file.write_all(&line).and_then(|()| file.flush()) {
                            eprintln!("could not write {}: {e}", stream_path.display());
                            out = None;
                        }
                    }
                    if let Some(event) = stream::parse_line(&String::from_utf8_lossy(&line)) {
                        // When the supervisor stopped listening, the copy still goes on.
                        let _ = tx.send(Incoming::Event(event));
                    }
                }
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(_) => break,
            }
        }
        let _ = tx.send(Incoming::Eof);
    });
    rx
}

/// TERM to the group, a short wait for `claude`, then KILL. `hard` sends KILL at once.
fn stop_group(child: &mut Child, pgid: u32, hard: bool) {
    if !hard {
        state::signal_group(pgid, libc::SIGTERM);
        let until = Instant::now() + TERM_GRACE;
        while Instant::now() < until {
            if let Ok(Some(_)) = child.try_wait() {
                break;
            }
            thread::sleep(Duration::from_millis(20));
        }
    }
    state::signal_group(pgid, libc::SIGKILL);
}

/// The difference between the `init` event and the contract, or `None`.
pub fn contract_problem(
    contract: &Contract,
    tools: &[String],
    permission_mode: Option<&str>,
    mcp_servers: &[String],
) -> Option<String> {
    let want: BTreeSet<&str> = contract.tools.iter().map(String::as_str).collect();
    let got: BTreeSet<&str> = tools.iter().map(String::as_str).collect();
    if want != got {
        let extra: Vec<&str> = got.difference(&want).copied().collect();
        let missing: Vec<&str> = want.difference(&got).copied().collect();
        return Some(format!(
            "the session has the tools [{}], but the contract names [{}] (extra: [{}], missing: [{}])",
            tools.join(", "),
            contract.tools.join(", "),
            extra.join(", "),
            missing.join(", ")
        ));
    }
    if permission_mode != Some(contract.permission_mode.as_str()) {
        return Some(format!(
            "the session has the permission mode {}, but the contract names {}",
            permission_mode.unwrap_or("(none)"),
            contract.permission_mode
        ));
    }
    let want: BTreeSet<&str> = contract.mcp_servers.iter().map(String::as_str).collect();
    let got: BTreeSet<&str> = mcp_servers.iter().map(String::as_str).collect();
    if want != got {
        return Some(format!(
            "the session has the MCP servers [{}], but the contract names [{}]",
            mcp_servers.join(", "),
            contract.mcp_servers.join(", ")
        ));
    }
    None
}

/// Writes `result.json` and the final `turn.json`.
fn finish(ctx: &Ctx, turn: &TurnPaths, status: TurnState, object: Option<Value>) -> Result<()> {
    let object = object.unwrap_or(Value::Null);
    atomic_write(
        &turn.result,
        format!("{}\n", serde_json::to_string_pretty(&object)?).as_bytes(),
    )?;
    status.write(&turn.status)?;
    ctx.log(&format!(
        "turn {}: ended with the reason {}{}",
        status.turn,
        status.reason.map_or("unknown", Reason::as_str),
        status
            .detail
            .as_deref()
            .map(|d| format!(": {d}"))
            .unwrap_or_default()
    ));
    Ok(())
}

fn exit_text(exit: Option<ExitStatus>) -> String {
    use std::os::unix::process::ExitStatusExt;
    match exit {
        Some(e) => match (e.code(), e.signal()) {
            (Some(code), _) => format!("exited with code {code}"),
            (None, Some(signal)) => format!("ended by the signal {signal}"),
            _ => "ended".to_string(),
        },
        None => "ended".to_string(),
    }
}

/// The end of the standard error of `claude`, for the detail of a failure.
fn tail_text(path: &Path) -> String {
    let Ok(mut file) = std::fs::File::open(path) else {
        return String::new();
    };
    let len = file.metadata().map(|m| m.len()).unwrap_or(0);
    let _ = file.seek(SeekFrom::Start(len.saturating_sub(400)));
    let mut bytes = Vec::new();
    let _ = file.read_to_end(&mut bytes);
    let text = String::from_utf8_lossy(&bytes);
    let text = text.trim();
    if text.is_empty() {
        String::new()
    } else {
        format!(". Its standard error ends with: {}", short(text, 400))
    }
}

/// The text on one line, cut to `max` characters.
pub fn short(text: &str, max: usize) -> String {
    let one_line = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if one_line.chars().count() <= max {
        return one_line;
    }
    let cut: String = one_line.chars().take(max.saturating_sub(1)).collect();
    format!("{cut}…")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn contract() -> Contract {
        Contract {
            tools: vec!["Read".into(), "Grep".into()],
            permission_mode: "dontAsk".into(),
            mcp_servers: Vec::new(),
        }
    }

    #[test]
    fn the_contract_compares_sets_and_names_the_difference() {
        let c = contract();
        let tools = |list: &[&str]| list.iter().map(|t| t.to_string()).collect::<Vec<_>>();
        assert_eq!(
            contract_problem(&c, &tools(&["Grep", "Read"]), Some("dontAsk"), &[]),
            None
        );
        let extra =
            contract_problem(&c, &tools(&["Read", "Grep", "Bash"]), Some("dontAsk"), &[]).unwrap();
        assert!(extra.contains("extra: [Bash]"), "{extra}");
        assert!(
            contract_problem(&c, &tools(&["Read", "Grep"]), Some("acceptEdits"), &[])
                .unwrap()
                .contains("permission mode acceptEdits")
        );
        assert!(contract_problem(
            &c,
            &tools(&["Read", "Grep"]),
            Some("dontAsk"),
            &tools(&["x"])
        )
        .unwrap()
        .contains("MCP servers [x]"));
    }

    #[test]
    fn short_keeps_one_line() {
        assert_eq!(short("a\n  b", 10), "a b");
        assert_eq!(short("abcdef", 4), "abc…");
    }
}
