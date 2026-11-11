//! `vision logs [KEY] [--follow]` and `vision open KEY` (sections 7.5 and 13): the events of a
//! turn as short readable lines, and the way to type into a running turn or take over its
//! session.

use std::fs::File;
use std::io::{BufRead, Read, Seek, SeekFrom, Write};
use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::process::Command;
use std::time::Duration;

use anyhow::{bail, Context, Result};
use serde_json::{json, Value};

use crate::ctx::Ctx;
use crate::ledger::{Event as LedgerEvent, Ledger};
use crate::paths::{TicketKey, TurnId, TurnPaths};
use crate::turn::control::{self, Told};
use crate::turn::launch::LaunchFile;
use crate::turn::state::{self, Phase, TurnState};
use crate::turn::stream::{self, Event};
use crate::turn::supervisor::short;
use crate::util::time;

/// The flag that `vision open` sets when it takes over a session.
pub const PAUSED: &str = "paused";

const FOLLOW_EVERY: Duration = Duration::from_millis(200);

/// The readable lines of one event: the text of the agent, each tool call on one line, the
/// messages that the session got, and the result.
pub fn render_event(event: &Event) -> Vec<String> {
    match event {
        Event::Init {
            session_id,
            tools,
            model,
            ..
        } => vec![format!(
            "session {} started with {}; tools {}",
            session_id.as_deref().unwrap_or("?"),
            model.as_deref().unwrap_or("its model"),
            tools.join(", ")
        )],
        Event::RateLimit { window, part, .. } => match part {
            Some(part) => vec![format!("limit {window} at {:.0}%", part * 100.0)],
            None => Vec::new(),
        },
        Event::Assistant { texts, tools } => {
            let mut lines: Vec<String> = texts
                .iter()
                .filter(|t| !t.trim().is_empty())
                .map(|t| format!("agent: {}", short(t, 300)))
                .collect();
            lines.extend(
                tools
                    .iter()
                    .map(|call| format!("tool {} {}", call.name, tool_summary(&call.input))),
            );
            lines
        }
        Event::User { texts, .. } => texts
            .iter()
            .filter(|t| !t.trim().is_empty())
            .map(|t| format!("message: {}", short(t, 300)))
            .collect(),
        Event::Result(result) => {
            let cost = result
                .cost_usd
                .map(|c| format!(" (${c:.4})"))
                .unwrap_or_default();
            if result.is_error {
                return vec![format!("result: error {}{cost}", result.subtype)];
            }
            match &result.structured_output {
                Some(object) => {
                    let kind = object
                        .get("kind")
                        .and_then(Value::as_str)
                        .unwrap_or("object");
                    let summary = object
                        .get("summary")
                        .and_then(Value::as_str)
                        .map(|s| format!(": {}", short(s, 300)))
                        .unwrap_or_default();
                    vec![format!("result {kind}{summary}{cost}")]
                }
                None => vec![format!("result: no result object{cost}")],
            }
        }
    }
}

/// The most telling part of the input of a tool call, on one line.
pub fn tool_summary(input: &Value) -> String {
    for key in [
        "file_path",
        "path",
        "pattern",
        "command",
        "url",
        "description",
        "prompt",
        "query",
    ] {
        if let Some(text) = input.get(key).and_then(Value::as_str) {
            return short(text, 120);
        }
    }
    match input {
        Value::Null => String::new(),
        Value::Object(map) if map.is_empty() => String::new(),
        other => short(&other.to_string(), 120),
    }
}

/// Reads the new complete lines of a growing file. A last line with no newline waits until it
/// is complete, so each line comes out one time.
pub struct Tail {
    path: PathBuf,
    offset: u64,
    partial: Vec<u8>,
}

impl Tail {
    pub fn new(path: PathBuf) -> Tail {
        Tail {
            path,
            offset: 0,
            partial: Vec::new(),
        }
    }

    /// The complete lines that came since the last call.
    pub fn read_new(&mut self) -> Vec<String> {
        let Ok(mut file) = File::open(&self.path) else {
            return Vec::new();
        };
        if file.seek(SeekFrom::Start(self.offset)).is_err() {
            return Vec::new();
        }
        let mut bytes = Vec::new();
        if file.read_to_end(&mut bytes).is_err() {
            return Vec::new();
        }
        self.offset += bytes.len() as u64;
        self.partial.extend_from_slice(&bytes);
        let mut lines = Vec::new();
        while let Some(end) = self.partial.iter().position(|b| *b == b'\n') {
            let line: Vec<u8> = self.partial.drain(..=end).collect();
            lines.push(String::from_utf8_lossy(&line).into_owned());
        }
        lines
    }
}

/// One turn that `logs` or `open` shows.
struct Shown {
    id: TurnId,
    turn: TurnPaths,
    prefix: String,
    tail: Tail,
    done: bool,
}

impl Shown {
    fn new(id: TurnId, turn: TurnPaths, prefix: String) -> Shown {
        let tail = Tail::new(turn.stream.clone());
        Shown {
            id,
            turn,
            prefix,
            tail,
            done: false,
        }
    }

    /// Prints the lines that are new since the last call.
    fn print_new(&mut self, out: &mut impl Write) -> Result<()> {
        for line in self.tail.read_new() {
            if let Some(event) = stream::parse_line(&line) {
                for text in render_event(&event) {
                    writeln!(out, "{}{text}", self.prefix)?;
                }
            }
        }
        out.flush()?;
        Ok(())
    }

    /// True when the turn will write no more lines. Prints its end.
    fn check_end(&mut self, out: &mut impl Write) -> Result<bool> {
        let current = TurnState::read(&self.turn.status).unwrap_or(None);
        let ended = match &current {
            Some(s) if s.is_ended() => true,
            Some(s) if s.state != Phase::Queued => !state::supervisor_alive(&self.turn.dir),
            _ => false,
        };
        if !ended {
            return Ok(false);
        }
        self.print_new(out)?;
        writeln!(out, "{}{}", self.prefix, end_line(current.as_ref()))?;
        out.flush()?;
        self.done = true;
        Ok(true)
    }
}

fn end_line(state: Option<&TurnState>) -> String {
    match state {
        Some(s) if s.is_ended() => format!(
            "ended: {}{}{}",
            s.reason.map_or("unknown", |r| r.as_str()),
            s.exit_code
                .map(|c| format!(", exit code {c}"))
                .unwrap_or_default(),
            s.detail
                .as_deref()
                .map(|d| format!(". {}", short(d, 300)))
                .unwrap_or_default()
        ),
        _ => "ended: the supervisor stopped before it wrote the end; the next tick marks a crash"
            .to_string(),
    }
}

/// `vision logs [KEY] [--follow]`.
pub fn logs(ctx: &Ctx, key: Option<&TicketKey>, follow: bool) -> Result<()> {
    ctx.require_setup()?;
    let ledger = ctx.ledger()?;
    let mut shown = Vec::new();
    match key {
        Some(key) => {
            let id = match control::running_turn_of(ctx, &ledger, key)? {
                Some(running) => running.id,
                None => match ledger.last_turn(key.as_str())? {
                    Some(row) => row.id,
                    None => {
                        println!("{key} has no turn yet.");
                        return Ok(());
                    }
                },
            };
            let id = TurnId::parse(&id)?;
            let turn = ctx.paths.turn(key, &id);
            shown.push(Shown::new(id, turn, String::new()));
        }
        None => {
            let running = control::running_turns(ctx, &ledger)?;
            if running.is_empty() {
                println!("No turn runs. To see the last turn of a ticket, run `vision logs KEY`.");
                return Ok(());
            }
            for turn in running {
                let id = turn.turn_id()?;
                let key = turn.ticket_key()?;
                let paths = ctx.paths.turn(&key, &id);
                shown.push(Shown::new(id, paths, format!("[{key}] ")));
            }
        }
    }
    let mut out = std::io::stdout().lock();
    for item in &mut shown {
        writeln!(out, "{}turn {}", item.prefix, item.id)?;
        item.print_new(&mut out)?;
        if !follow {
            let current = TurnState::read(&item.turn.status).unwrap_or(None);
            if current.as_ref().is_some_and(TurnState::is_ended) {
                writeln!(out, "{}{}", item.prefix, end_line(current.as_ref()))?;
            } else {
                writeln!(out, "{}still running", item.prefix)?;
            }
        }
    }
    if follow {
        follow_until_done(&mut shown, &mut out)?;
    }
    Ok(())
}

fn follow_until_done(shown: &mut [Shown], out: &mut impl Write) -> Result<()> {
    loop {
        let mut all_done = true;
        for item in shown.iter_mut().filter(|s| !s.done) {
            item.print_new(out)?;
            if !item.check_end(out)? {
                all_done = false;
            }
        }
        if all_done {
            return Ok(());
        }
        std::thread::sleep(FOLLOW_EVERY);
    }
}

/// `vision open KEY`: with a running turn, print its events and send each typed line into its
/// inbox until it ends; Ctrl-C leaves, and the turn continues. With no running turn, pause the
/// ticket and replace this process with `claude --resume <session id>` in the working folder
/// of the last turn.
pub fn open(ctx: &Ctx, key: &TicketKey) -> Result<()> {
    ctx.require_setup()?;
    let ledger = ctx.ledger()?;
    if let Some(running) = control::running_turn_of(ctx, &ledger, key)? {
        let id = running.turn_id()?;
        println!(
            "Attached to the turn {id} of {key}. Type a line and press Enter to send it. \
             Ctrl-C leaves, and the turn continues."
        );
        let paths = ctx.paths.clone();
        let ticket = key.clone();
        std::thread::spawn(move || {
            let ctx = Ctx::new(paths);
            let Ok(ledger) = ctx.ledger() else { return };
            for line in std::io::stdin().lock().lines() {
                let Ok(line) = line else { return };
                if line.trim().is_empty() {
                    continue;
                }
                match control::tell(&ctx, &ledger, &ticket, line.trim_end(), "user") {
                    Ok(Told::Running(_)) => eprintln!("(sent)"),
                    Ok(Told::Stored) => {
                        eprintln!("(the turn ended, so it waits for the next turn)")
                    }
                    Err(e) => eprintln!("error: {e:#}"),
                }
            }
        });
        let turn = ctx.paths.turn(key, &id);
        let mut shown = [Shown::new(id, turn, String::new())];
        let mut out = std::io::stdout().lock();
        follow_until_done(&mut shown, &mut out)?;
        return Ok(());
    }
    take_over(ctx, &ledger, key)
}

fn take_over(ctx: &Ctx, ledger: &Ledger, key: &TicketKey) -> Result<()> {
    let Some(last) = ledger.last_turn(key.as_str())? else {
        bail!("{key} has no turn yet, so there is no session to open");
    };
    let session = last.session.clone().with_context(|| {
        format!(
            "the last turn {} of {key} has no session id to resume",
            last.id
        )
    })?;
    let id = TurnId::parse(&last.id)?;
    let workdir = LaunchFile::read(&ctx.paths.turn(key, &id).dir)
        .map(|f| f.plan.workdir)
        .with_context(|| format!("could not find the working folder of the turn {id}"))?;
    if !workdir.is_dir() {
        bail!(
            "the working folder {} of the turn {id} no longer exists",
            workdir.display()
        );
    }
    let config = ctx.config()?;
    let program = ctx.claude_bin()?;
    ledger.add_ticket_flag(key.as_str(), PAUSED, &time::now_rfc3339())?;
    ledger.add_event(
        &LedgerEvent::now("user", "pause")
            .ticket(key.as_str())
            .detail(json!({"by": "open", "session": session, "turn": last.id})),
    )?;
    ctx.log(&format!(
        "open: paused {key} and took over the session {session}"
    ));
    println!(
        "{key} is paused, so no new turn starts for it. Opening the session {session} in {}.",
        workdir.display()
    );
    std::io::stdout().flush()?;
    let mut command = Command::new(&program);
    command.arg("--resume").arg(&session).current_dir(&workdir);
    if let Some(dir) = config.claude_config_dir(&ctx.paths)? {
        command.env("CLAUDE_CONFIG_DIR", dir);
    }
    let err = command.exec();
    bail!(
        "could not start `{program}`: {err}. Install Claude Code, or set claude.bin in config.toml"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lines(text: &str) -> Vec<String> {
        render_event(&stream::parse_line(text).unwrap())
    }

    #[test]
    fn each_event_becomes_short_lines() {
        assert_eq!(
            lines(
                r#"{"type":"assistant","message":{"content":[{"type":"text","text":"I read\nthe file."},{"type":"tool_use","name":"Read","input":{"file_path":"src/main.rs"}}]}}"#
            ),
            ["agent: I read the file.", "tool Read src/main.rs"]
        );
        assert_eq!(
            lines(
                r#"{"type":"user","message":{"content":[{"type":"text","text":"use the old export"}]}}"#
            ),
            ["message: use the old export"]
        );
        assert_eq!(
            lines(
                r#"{"type":"result","subtype":"success","is_error":false,"structured_output":{"kind":"result","summary":"Done."},"total_cost_usd":0.25}"#
            ),
            ["result result: Done. ($0.2500)"]
        );
        assert!(lines(
            r#"{"type":"user","message":{"content":[{"type":"tool_result","content":"x"}]}}"#
        )
        .is_empty());
    }

    #[test]
    fn a_tail_gives_only_complete_lines() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("s");
        std::fs::write(&path, "one\ntw").unwrap();
        let mut tail = Tail::new(path.clone());
        assert_eq!(tail.read_new(), ["one\n"]);
        std::fs::write(&path, "one\ntwo\n").unwrap();
        assert_eq!(tail.read_new(), ["two\n"]);
        assert!(tail.read_new().is_empty());
    }
}
