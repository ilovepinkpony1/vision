//! The launch API (section 7): the library calls that start a turn, resume a session, and say
//! which rule refuses a new turn.
//!
//! A launch tests the rules, writes the turn folder (the composed role file, `settings.json`,
//! `facts.json`, `launch.json`, and a `queued` `turn.json`), copies the session view into
//! `.vision/` of the working folder, writes the `turns` row, and then starts
//! `vision turn-run <id>` as a detached process in its own session and process group.

use std::collections::BTreeMap;
use std::fs::{self, OpenOptions};
use std::io;
use std::os::unix::process::CommandExt;
use std::path::{Component, Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::OnceLock;

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};

use crate::config::Effort;
use crate::ctx::Ctx;
use crate::ledger::turns::{NewTurn, Window};
use crate::ledger::{Event, Ledger};
use crate::models;
use crate::paths::{TicketKey, TurnId, TurnPaths, HOME_ENV};
use crate::turn::state::{Reason, TurnState};
use crate::turn::{inbox, roles, settings, TurnKind};
use crate::util::fs::{atomic_write, ensure_dir};
use crate::util::{git, ids, time};

/// The text in a path of the session view, or in a facts value that starts with `.vision/`,
/// that becomes the turn id, such as `.vision/turns/{turn}/issue.json`.
pub const TURN_PLACEHOLDER: &str = "{turn}";

/// The file in the turn folder that holds the request and the start line.
pub const LAUNCH_FILE: &str = "launch.json";
/// The composed system prompt of the turn.
pub const ROLE_FILE: &str = "role.md";
/// Standard output and standard error of the supervisor.
pub const SUPERVISOR_LOG: &str = "supervisor.log";
/// Standard error of `claude`.
pub const STDERR_LOG: &str = "stderr.log";

/// A file to copy into the session view before the turn starts.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ViewFile {
    /// The file to copy, such as `tickets/DEMO-12/approved-plan.md` of the home.
    pub from: PathBuf,
    /// The path in `.vision/` of the working folder, such as `approved-plan.md` or
    /// `turns/{turn}/issue.json`. `{turn}` becomes the turn id.
    pub to: String,
}

/// What a later milestone gives to start a turn.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct LaunchRequest {
    #[serde(with = "key_text")]
    pub ticket: TicketKey,
    /// The agent, such as `builder`. Its role file and its model come from it.
    pub agent: String,
    /// The step id of the adapter, such as `build`.
    pub step: String,
    pub kind: TurnKind,
    /// The working folder of the session: the clone, or a folder of the turn.
    pub workdir: PathBuf,
    /// The facts object of section 7.2. The runner sets `turn`, `agent`, `step`, `kind`,
    /// `expects`, `message`, and `ticket.key`. In each string value that starts with
    /// `.vision/`, `{turn}` becomes the turn id.
    #[serde(default)]
    pub facts: Value,
    /// Files to copy into `.vision/` of the working folder.
    #[serde(default)]
    pub view: Vec<ViewFile>,
    /// More folders that the session can read: `--add-dir`.
    #[serde(default)]
    pub add_dirs: Vec<PathBuf>,
    /// The end facts that the step expects, such as `commits` or `doc:pr.md`.
    #[serde(default)]
    pub expects: Vec<String>,
    /// The session to resume, or `None` for a new session.
    #[serde(default)]
    pub resumes: Option<String>,
    /// The `message` of `facts.json`, such as the answers to the cards, or `continue`.
    #[serde(default)]
    pub message: Option<String>,
    /// A role file that replaces `agents/<agent>.md`, such as one of an adapter.
    #[serde(default)]
    pub role_file: Option<PathBuf>,
    /// A model that replaces the model of the agent, such as for `vision model eval`.
    #[serde(default)]
    pub model: Option<String>,
    /// An effort that replaces the effort of the agent.
    #[serde(default)]
    pub effort: Option<Effort>,
    /// Seconds before the supervisor kills the turn. `None` takes `[time]` of the kind.
    #[serde(default)]
    pub time_limit_secs: Option<u64>,
    /// The turn that this turn retries with `continue`. Only the collector sets it.
    #[serde(default)]
    pub retry_of: Option<String>,
    /// True when a failed turn gets no automatic resume, such as a replay turn that nobody reads.
    #[serde(default)]
    pub no_retry: bool,
}

impl LaunchRequest {
    /// A request with no facts, no view, and a new session.
    pub fn new(
        ticket: TicketKey,
        agent: &str,
        step: &str,
        kind: TurnKind,
        workdir: impl Into<PathBuf>,
    ) -> LaunchRequest {
        LaunchRequest {
            ticket,
            agent: agent.to_string(),
            step: step.to_string(),
            kind,
            workdir: workdir.into(),
            facts: Value::Object(Map::new()),
            view: Vec::new(),
            add_dirs: Vec::new(),
            expects: Vec::new(),
            resumes: None,
            message: None,
            role_file: None,
            model: None,
            effort: None,
            time_limit_secs: None,
            retry_of: None,
            no_retry: false,
        }
    }
}

/// What the supervisor compares at the `init` event.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Contract {
    pub tools: Vec<String>,
    pub permission_mode: String,
    pub mcp_servers: Vec<String>,
}

/// The start line and the limits of one turn, resolved at launch, so that the supervisor
/// does not depend on a configuration that changes during the turn.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Plan {
    pub turn: String,
    pub ticket: String,
    pub agent: String,
    pub kind: TurnKind,
    /// The Claude Code binary.
    pub program: String,
    pub args: Vec<String>,
    pub workdir: PathBuf,
    /// The names of `claude.env_pass`.
    pub env_pass: Vec<String>,
    /// The fixed values of `claude.env`, and the values that the runner sets:
    /// `CLAUDE_CONFIG_DIR`, and `GIT_CONFIG_GLOBAL` of a worker.
    pub env_set: BTreeMap<String, String>,
    pub first_message: String,
    pub time_limit_secs: u64,
    pub contract: Contract,
    pub session: String,
    pub model: String,
    pub effort: Effort,
    /// The result schema in `agents/`.
    pub schema: String,
}

/// `launch.json` of a turn folder.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct LaunchFile {
    pub request: LaunchRequest,
    pub plan: Plan,
}

impl LaunchFile {
    pub fn read(turn_dir: &Path) -> Result<LaunchFile> {
        let path = turn_dir.join(LAUNCH_FILE);
        let text = fs::read_to_string(&path)
            .with_context(|| format!("could not read {}", path.display()))?;
        serde_json::from_str(&text).with_context(|| format!("{} is not valid", path.display()))
    }
}

/// A turn that was written, and started unless it came from [`prepare`].
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Launched {
    #[serde(serialize_with = "as_text")]
    pub id: TurnId,
    #[serde(serialize_with = "key_text::serialize")]
    pub ticket: TicketKey,
    pub session: String,
    pub dir: PathBuf,
    pub model: String,
    pub effort: Effort,
    pub supervisor_pid: Option<u32>,
}

/// The rule that refuses a new turn.
#[derive(Clone, Debug, PartialEq, thiserror::Error)]
pub enum Refusal {
    #[error("refused by the kill switch: Vision is off. Run `vision on` to start turns again")]
    Off,
    #[error("refused by budget.{kind}: the budget of one {kind} turn is ${budget}. Raise budget.{kind} in config.toml")]
    TurnBudget { kind: TurnKind, budget: f64 },
    #[error("refused by budget.ticket: the turns of {ticket} cost ${spent:.2} of ${budget}. Raise budget.ticket in config.toml to continue")]
    TicketBudget {
        ticket: String,
        spent: f64,
        budget: f64,
    },
    #[error("refused by budget.week: the turns of the last 7 days cost ${spent:.2} of ${budget}. Wait, or raise budget.week in config.toml")]
    WeekBudget { spent: f64, budget: f64 },
    #[error("refused by limits.{window}: the {window} window is at {part_percent:.0}%, and new turns stop at {stop_percent:.0}%. {resets}")]
    Limit {
        window: &'static str,
        part_percent: f64,
        stop_percent: f64,
        resets: String,
    },
}

impl Refusal {
    /// The rule, such as `off`, `budget.ticket`, or `limits.five_hour`.
    pub fn rule(&self) -> String {
        match self {
            Refusal::Off => "off".into(),
            Refusal::TurnBudget { kind, .. } => format!("budget.{kind}"),
            Refusal::TicketBudget { .. } => "budget.ticket".into(),
            Refusal::WeekBudget { .. } => "budget.week".into(),
            Refusal::Limit { window, .. } => format!("limits.{window}"),
        }
    }

    /// True for a subscription window, which frees itself at its reset time.
    pub fn is_limit(&self) -> bool {
        matches!(self, Refusal::Limit { .. })
    }
}

/// The rule that refuses a new turn of `kind` for `ticket` now, or `None`. In this order: the
/// kill switch, the budget of one turn, of the ticket (not for a reserved key), of the week,
/// and the subscription windows. A reading after its reset time does not count.
pub fn refusal(
    ctx: &Ctx,
    ledger: &Ledger,
    ticket: &TicketKey,
    kind: TurnKind,
) -> Result<Option<Refusal>> {
    if ctx.paths.off().exists() {
        return Ok(Some(Refusal::Off));
    }
    let config = ctx.config()?;
    let budget = config.budget.for_kind(kind);
    if budget <= 0.0 {
        return Ok(Some(Refusal::TurnBudget { kind, budget }));
    }
    if !ticket.is_reserved() {
        let spent = ledger.spent_on_ticket(ticket.as_str())?;
        if spent >= config.budget.ticket {
            return Ok(Some(Refusal::TicketBudget {
                ticket: ticket.to_string(),
                spent,
                budget: config.budget.ticket,
            }));
        }
    }
    let now = time::now();
    let week_ago = time::rfc3339(now - ::time::Duration::days(7));
    let spent = ledger.spent_since(&week_ago)?;
    if spent >= config.budget.week {
        return Ok(Some(Refusal::WeekBudget {
            spent,
            budget: config.budget.week,
        }));
    }
    let now_text = time::rfc3339(now);
    for (window, stop) in [
        (Window::FiveHour, config.limits.five_hour),
        (Window::SevenDay, config.limits.seven_day),
    ] {
        if let Some(reading) = ledger.active_reading(window, &now_text)? {
            if reading.part >= stop {
                return Ok(Some(Refusal::Limit {
                    window: window.column(),
                    part_percent: reading.part * 100.0,
                    stop_percent: stop * 100.0,
                    resets: match reading.resets {
                        Some(at) => format!("It resets at {at}"),
                        None => "The next turn that reports the window updates it".into(),
                    },
                }));
            }
        }
    }
    Ok(None)
}

/// Starts a turn: [`prepare`], then [`start_supervisor`]. A refusal is a [`Refusal`] inside
/// the error, so that a caller can find the rule with `err.downcast_ref::<Refusal>()`.
pub fn launch(ctx: &Ctx, ledger: &Ledger, request: &LaunchRequest) -> Result<Launched> {
    let mut launched = prepare(ctx, ledger, request)?;
    launched.supervisor_pid = Some(start_supervisor(ctx, &launched)?);
    Ok(launched)
}

/// Resumes the session of an ended turn with a message, such as the answers to its cards.
/// The new turn keeps the agent, the step, the kind, the working folder, and the facts.
pub fn resume(ctx: &Ctx, ledger: &Ledger, from: &TurnId, message: &str) -> Result<Launched> {
    let mut request = request_of(ctx, ledger, from)?;
    let row = ledger
        .turn(&from.to_string())?
        .with_context(|| format!("the ledger has no turn {from}"))?;
    let session = row.session.with_context(|| {
        format!("the turn {from} has no session id, so it cannot be resumed. Start a new turn")
    })?;
    request.resumes = Some(session);
    request.message = Some(message.to_string());
    request.retry_of = None;
    launch(ctx, ledger, &request)
}

/// The request that started a turn, from its `launch.json`.
pub fn request_of(ctx: &Ctx, ledger: &Ledger, id: &TurnId) -> Result<LaunchRequest> {
    Ok(LaunchFile::read(&turn_paths(ctx, ledger, id)?.dir)?.request)
}

/// The folder of a turn, from the ticket of its row.
pub fn turn_paths(ctx: &Ctx, ledger: &Ledger, id: &TurnId) -> Result<TurnPaths> {
    let row = ledger
        .turn(&id.to_string())?
        .with_context(|| format!("the ledger has no turn {id}"))?;
    let key = TicketKey::parse(&row.ticket)?;
    Ok(ctx.paths.turn(&key, id))
}

/// The first message of a turn (section 7.2).
pub fn first_message(id: &TurnId) -> String {
    format!("Turn {id}. Read .vision/turns/{id}/facts.json and do the step that it names.")
}

/// Writes a turn and its row, and starts nothing. [`start_supervisor`] starts it.
pub fn prepare(ctx: &Ctx, ledger: &Ledger, request: &LaunchRequest) -> Result<Launched> {
    ctx.require_setup()?;
    if let Some(refused) = refusal(ctx, ledger, &request.ticket, request.kind)? {
        return Err(refused.into());
    }
    let config = ctx.config()?;
    let paths = &ctx.paths;
    let workdir = std::path::absolute(&request.workdir)
        .with_context(|| format!("could not use the folder {}", request.workdir.display()))?;
    if !workdir.is_dir() {
        bail!(
            "the working folder {} does not exist. Give the clone or a folder of the turn",
            workdir.display()
        );
    }
    let mut add_dirs = Vec::new();
    for dir in &request.add_dirs {
        let dir = std::path::absolute(dir)
            .with_context(|| format!("could not use the folder {}", dir.display()))?;
        if !dir.is_dir() {
            bail!("the folder {} for --add-dir does not exist", dir.display());
        }
        add_dirs.push(dir);
    }
    let role = roles::compose_role(paths, &request.agent, request.role_file.as_deref())?;
    let schema = roles::schema(paths, &request.agent)?;
    let mut choice = models::choice(config, &request.agent)?;
    if let Some(model) = &request.model {
        choice.model = models::full_model_id(model)?;
    }
    if let Some(effort) = request.effort {
        choice.effort = effort;
    }
    // The supervisor starts the binary with the environment of the session, and `claude.env`
    // can give that session a `PATH` of its own. So a plain name is resolved here, with the
    // `PATH` of this process. A name that is not found stays, and the start names the problem.
    let program = ctx.claude_bin()?;
    let program = crate::util::process::find_program(&program)
        .map_or(program, |path| path.display().to_string());
    let session = request.resumes.clone().unwrap_or_else(ids::new_session_id);
    let now = time::now();
    let first_n = ledger.count_turns(request.ticket.as_str(), &request.step)? + 1;
    let (id, turn) = allocate(ctx, &request.ticket, &request.step, now, first_n)?;

    let written = (|| -> Result<Plan> {
        let gitconfig = settings::write_gitconfig(paths, &config.user)?;
        let rendered = settings::render(
            request.kind,
            &workdir,
            &add_dirs,
            &config.sandbox,
            &gitconfig,
        )?;
        atomic_write(
            &turn.settings,
            settings::settings_text(paths, request.kind, &rendered, config.claude.attribution)?
                .as_bytes(),
        )?;
        let role_path = turn.dir.join(ROLE_FILE);
        atomic_write(&role_path, role.as_bytes())?;
        ensure_dir(&turn.inbox_sent)?;

        let facts = build_facts(request, &id)?;
        let facts_text = format!("{}\n", serde_json::to_string_pretty(&facts)?);
        atomic_write(&turn.facts, facts_text.as_bytes())?;
        let view_turn = workdir.join(".vision").join("turns").join(id.to_string());
        ensure_dir(&view_turn)?;
        atomic_write(&view_turn.join("facts.json"), facts_text.as_bytes())?;
        copy_view(&workdir, &id, &request.view)?;
        exclude_view(ctx, &workdir);

        // The fixed values of `claude.env` come first, so that a value of the runner wins.
        let mut env_set = config.claude.env.clone();
        if let Some(dir) = config.claude_config_dir(paths)? {
            env_set.insert("CLAUDE_CONFIG_DIR".into(), dir.display().to_string());
        }
        if settings::sandboxed(request.kind) {
            env_set.insert("GIT_CONFIG_GLOBAL".into(), gitconfig.display().to_string());
        }
        let tools = settings::tools(request.kind);
        let mode = settings::permission_mode(request.kind);
        let mut args: Vec<String> = vec![
            "-p".into(),
            "--model".into(),
            choice.model.clone(),
            "--effort".into(),
            choice.effort.to_string(),
            "--input-format".into(),
            "stream-json".into(),
            "--output-format".into(),
            "stream-json".into(),
            "--verbose".into(),
            "--replay-user-messages".into(),
            "--restricted".into(),
            "--tools".into(),
            tools.join(","),
            "--settings".into(),
            turn.settings.display().to_string(),
            "--strict-mcp-config".into(),
            "--permission-mode".into(),
            mode.into(),
            "--permission-prompts".into(),
            "none".into(),
            "--append-system-prompt-file".into(),
            role_path.display().to_string(),
            "--json-schema".into(),
            schema.to_string(),
            "--max-budget-usd".into(),
            config.budget.for_kind(request.kind).to_string(),
        ];
        match &request.resumes {
            Some(resumed) => args.extend(["--resume".into(), resumed.clone()]),
            None => args.extend(["--session-id".into(), session.clone()]),
        }
        for dir in &add_dirs {
            args.extend(["--add-dir".into(), dir.display().to_string()]);
        }
        let plan = Plan {
            turn: id.to_string(),
            ticket: request.ticket.to_string(),
            agent: request.agent.clone(),
            kind: request.kind,
            program: program.clone(),
            args,
            workdir: workdir.clone(),
            env_pass: config.claude.env_pass.clone(),
            env_set,
            first_message: first_message(&id),
            time_limit_secs: request
                .time_limit_secs
                .unwrap_or_else(|| config.time.for_kind(request.kind).saturating_mul(60)),
            contract: Contract {
                tools: settings::contract_tools(request.kind),
                permission_mode: mode.into(),
                mcp_servers: Vec::new(),
            },
            session: session.clone(),
            model: choice.model.clone(),
            effort: choice.effort,
            schema: roles::schema_name(&request.agent).into(),
        };
        let launch_file = LaunchFile {
            request: request.clone(),
            plan: plan.clone(),
        };
        atomic_write(
            &turn.dir.join(LAUNCH_FILE),
            format!("{}\n", serde_json::to_string_pretty(&launch_file)?).as_bytes(),
        )?;
        TurnState::queued(&id.to_string()).write(&turn.status)?;
        Ok(plan)
    })();
    let plan = match written {
        Ok(plan) => plan,
        Err(e) => {
            let _ = fs::remove_dir_all(&turn.dir);
            return Err(e.context(format!("could not write the turn {id}")));
        }
    };

    let started = time::rfc3339(now);
    let id_text = id.to_string();
    let inserted = ledger.insert_turn(&NewTurn {
        id: &id_text,
        ticket: request.ticket.as_str(),
        agent: &request.agent,
        step: &request.step,
        kind: request.kind.as_str(),
        session: &session,
        resumes: request.resumes.as_deref(),
        model: &plan.model,
        effort: plan.effort.as_str(),
        started: &started,
        role_hash: &ids::sha256_hex(role.as_bytes()),
    });
    if let Err(e) = inserted {
        let _ = fs::remove_dir_all(&turn.dir);
        return Err(e.context(format!("could not write the row of the turn {id}")));
    }
    // A waiting message of the person is for the work agent of the ticket, so the Judge and
    // the Curator leave it for the next work turn.
    if !models::agent(&request.agent).is_some_and(|spec| spec.alone) {
        deliver_waiting(ctx, ledger, &request.ticket, &id, &turn)?;
    }
    ledger.add_event(
        &Event::now("tick", "turn-started")
            .ticket(request.ticket.as_str())
            .external_id(&format!("turn-started:{id}"))
            .detail(json!({
                "turn": id_text, "agent": request.agent, "step": request.step,
                "kind": request.kind, "model": plan.model, "effort": plan.effort,
                "session": session, "resumes": request.resumes, "retry_of": request.retry_of,
            })),
    )?;
    ctx.log(&format!(
        "turn {id}: written for {} ({} {}, {} {})",
        request.ticket, request.agent, request.kind, plan.model, plan.effort
    ));
    Ok(Launched {
        id,
        ticket: request.ticket.clone(),
        session,
        dir: turn.dir,
        model: plan.model,
        effort: plan.effort,
        supervisor_pid: None,
    })
}

/// Starts `vision turn-run <id>` as a detached process: its own session and process group,
/// standard input from `/dev/null`, and its output in `supervisor.log` of the turn folder.
/// Gives its pid.
pub fn start_supervisor(ctx: &Ctx, launched: &Launched) -> Result<u32> {
    let started = (|| -> Result<u32> {
        let program = vision_bin()?;
        let log_path = launched.dir.join(SUPERVISOR_LOG);
        let log = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&log_path)
            .with_context(|| format!("could not open {}", log_path.display()))?;
        let mut command = Command::new(&program);
        command
            .arg("turn-run")
            .arg(launched.id.to_string())
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
        // Reap the supervisor when it ends while this process still lives, so that it never
        // stays a zombie. When this process ends first, the system adopts and reaps it.
        std::thread::spawn(move || {
            let _ = child.wait();
        });
        Ok(pid)
    })();
    match started {
        Ok(pid) => {
            ctx.log(&format!("turn {}: supervisor {pid} started", launched.id));
            Ok(pid)
        }
        Err(e) => {
            let status = launched.dir.join("turn.json");
            if let Ok(Some(state)) = TurnState::read(&status) {
                let _ = state
                    .ended(
                        Reason::Error,
                        Some(format!("the supervisor did not start: {e:#}")),
                    )
                    .write(&status);
            }
            Err(e.context(format!(
                "could not start the supervisor of the turn {}",
                launched.id
            )))
        }
    }
}

static VISION_BIN: OnceLock<PathBuf> = OnceLock::new();

/// Names the `vision` binary that runs the supervisor. The default is this program, which is
/// right for the binary. A test that calls the library sets it to the built binary.
pub fn set_vision_bin(path: impl Into<PathBuf>) {
    let _ = VISION_BIN.set(path.into());
}

fn vision_bin() -> Result<PathBuf> {
    if let Some(path) = VISION_BIN.get() {
        return Ok(path.clone());
    }
    let exe = std::env::current_exe().context("could not find the path of this program")?;
    let stem = exe
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    if !stem.starts_with("vision") {
        bail!(
            "this program ({}) is not the vision binary, so it cannot start a supervisor. \
             Call vision::turn::set_vision_bin with the path of the vision binary first",
            exe.display()
        );
    }
    Ok(exe)
}

/// The first free id of the step, and its new folder. The folder acts as the claim of the id.
fn allocate(
    ctx: &Ctx,
    key: &TicketKey,
    step: &str,
    now: ::time::OffsetDateTime,
    first_n: u32,
) -> Result<(TurnId, TurnPaths)> {
    ensure_dir(&ctx.paths.turns_dir(key))?;
    for n in first_n..first_n.saturating_add(1000) {
        let id = TurnId::new(now, key, step, n)?;
        let turn = ctx.paths.turn(key, &id);
        match fs::create_dir(&turn.dir) {
            Ok(()) => return Ok((id, turn)),
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(e) => {
                return Err(e)
                    .with_context(|| format!("could not create the folder {}", turn.dir.display()))
            }
        }
    }
    bail!("could not find a free turn id for {key} {step}")
}

/// The facts of section 7.2, with the values that the runner owns.
fn build_facts(request: &LaunchRequest, id: &TurnId) -> Result<Value> {
    let mut facts = match &request.facts {
        Value::Null => Map::new(),
        Value::Object(map) => map.clone(),
        _ => bail!("the facts of a turn must be a JSON object"),
    };
    for value in facts.values_mut() {
        fill_placeholder(value, id);
    }
    let ticket = facts
        .entry("ticket")
        .or_insert_with(|| Value::Object(Map::new()));
    match ticket {
        Value::Object(map) => {
            map.insert("key".into(), json!(request.ticket.as_str()));
        }
        _ => bail!(
            "`ticket` in the facts of a turn must be an object, such as {{\"key\": \"DEMO-12\"}}"
        ),
    }
    facts.insert("turn".into(), json!(id.to_string()));
    facts.insert("agent".into(), json!(request.agent));
    facts.insert("step".into(), json!(request.step));
    facts.insert("kind".into(), json!(request.kind));
    facts.insert("expects".into(), json!(request.expects));
    facts.insert("message".into(), json!(request.message));
    Ok(Value::Object(facts))
}

fn fill_placeholder(value: &mut Value, id: &TurnId) {
    match value {
        Value::String(text) if text.starts_with(".vision/") && text.contains(TURN_PLACEHOLDER) => {
            *text = text.replace(TURN_PLACEHOLDER, &id.to_string());
        }
        Value::Array(items) => items.iter_mut().for_each(|v| fill_placeholder(v, id)),
        Value::Object(map) => map.values_mut().for_each(|v| fill_placeholder(v, id)),
        _ => {}
    }
}

/// Copies each file of the view into `.vision/` of the working folder.
fn copy_view(workdir: &Path, id: &TurnId, view: &[ViewFile]) -> Result<()> {
    let root = workdir.join(".vision");
    for file in view {
        let to = file.to.replace(TURN_PLACEHOLDER, &id.to_string());
        let relative = Path::new(&to);
        let safe = !to.is_empty()
            && relative
                .components()
                .all(|c| matches!(c, Component::Normal(_)));
        if !safe {
            bail!(
                "`{}` is not a path in .vision/. Use a relative path such as approved-plan.md",
                file.to
            );
        }
        if !file.from.is_file() {
            bail!(
                "{} does not exist or is not a file, so it cannot go into the session view",
                file.from.display()
            );
        }
        let target = root.join(relative);
        if let Some(parent) = target.parent() {
            ensure_dir(parent)?;
        }
        fs::copy(&file.from, &target).with_context(|| {
            format!(
                "could not copy {} to {}",
                file.from.display(),
                target.display()
            )
        })?;
    }
    Ok(())
}

/// In a clone, lists `.vision` in the exclude file of git, so that the view never shows as a
/// change (section 7.2). A failure is only a log line: the turn can still run.
fn exclude_view(ctx: &Ctx, workdir: &Path) {
    if !workdir.join(".git").exists() {
        return;
    }
    let result = (|| -> Result<bool> {
        let exclude = git::run(workdir, &["rev-parse", "--git-path", "info/exclude"])?;
        let exclude = workdir.join(exclude);
        let text = crate::util::fs::read_optional(&exclude)?.unwrap_or_default();
        let listed = text
            .lines()
            .any(|l| matches!(l.trim(), ".vision" | ".vision/" | "/.vision" | "/.vision/"));
        if listed {
            return Ok(false);
        }
        if let Some(parent) = exclude.parent() {
            ensure_dir(parent)?;
        }
        let mut new = text;
        if !new.is_empty() && !new.ends_with('\n') {
            new.push('\n');
        }
        new.push_str("/.vision/\n");
        atomic_write(&exclude, new.as_bytes())?;
        Ok(true)
    })();
    if let Err(e) = result {
        ctx.log(&format!(
            "could not list .vision in the git exclude file of {}: {e:#}",
            workdir.display()
        ));
    }
}

/// Moves the messages that wait for the next turn of the ticket into the inbox of this turn.
fn deliver_waiting(
    ctx: &Ctx,
    ledger: &Ledger,
    key: &TicketKey,
    id: &TurnId,
    turn: &TurnPaths,
) -> Result<()> {
    let waiting = inbox::waiting(&inbox::ticket_inbox(&ctx.paths, key))?;
    for (path, message) in waiting {
        inbox::move_to(&path, &turn.inbox)?;
        if let Some(message_id) = message.id {
            ledger.set_message_turn(message_id, Some(&id.to_string()))?;
        }
    }
    Ok(())
}

fn as_text<S: serde::Serializer>(id: &TurnId, s: S) -> Result<S::Ok, S::Error> {
    s.collect_str(id)
}

/// Serde for a [`TicketKey`] as its text.
mod key_text {
    use serde::{Deserialize, Deserializer, Serializer};

    use crate::paths::TicketKey;

    pub fn serialize<S: Serializer>(key: &TicketKey, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(key.as_str())
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<TicketKey, D::Error> {
        let text = String::deserialize(d)?;
        TicketKey::parse(&text).map_err(serde::de::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ::time::macros::datetime;

    fn id() -> TurnId {
        let key = TicketKey::parse("DEMO-12").unwrap();
        TurnId::new(datetime!(2026-11-16 14:30:00 UTC), &key, "build", 1).unwrap()
    }

    #[test]
    fn the_runner_owns_some_facts_and_fills_the_turn_id() {
        let mut request = LaunchRequest::new(
            TicketKey::parse("DEMO-12").unwrap(),
            "builder",
            "build",
            TurnKind::Worker,
            "/clone",
        );
        request.facts = json!({
            "turn": "wrong", "ticket": {"title": {"untrusted": "Export {turn}"},
            "issue": ".vision/turns/{turn}/issue.json"}, "branch": "vision/DEMO-12"
        });
        request.expects = vec!["commits".into()];
        let facts = build_facts(&request, &id()).unwrap();
        assert_eq!(facts["turn"], "20261116T143000Z-DEMO-12-build-1");
        assert_eq!(facts["ticket"]["key"], "DEMO-12");
        assert_eq!(
            facts["ticket"]["issue"],
            ".vision/turns/20261116T143000Z-DEMO-12-build-1/issue.json"
        );
        assert_eq!(
            facts["ticket"]["title"]["untrusted"], "Export {turn}",
            "outside text is never changed"
        );
        assert_eq!(facts["expects"], json!(["commits"]));
        assert_eq!(facts["message"], Value::Null);
        assert_eq!(facts["kind"], "worker");
        request.facts = json!([1]);
        assert!(build_facts(&request, &id()).is_err());
    }

    #[test]
    fn a_view_path_must_stay_in_the_view() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("plan.md");
        fs::write(&source, "plan").unwrap();
        let view = |to: &str| {
            vec![ViewFile {
                from: source.clone(),
                to: to.into(),
            }]
        };
        copy_view(dir.path(), &id(), &view("turns/{turn}/plan.md")).unwrap();
        assert!(dir
            .path()
            .join(".vision/turns/20261116T143000Z-DEMO-12-build-1/plan.md")
            .is_file());
        for bad in ["../x", "/etc/x", "", "a/../../b"] {
            assert!(copy_view(dir.path(), &id(), &view(bad)).is_err(), "{bad}");
        }
    }

    #[test]
    fn a_request_round_trips_through_json() {
        let request = LaunchRequest::new(
            TicketKey::parse("DEMO-12").unwrap(),
            "scout",
            "refine",
            TurnKind::Reader,
            "/clone",
        );
        let text = serde_json::to_string(&request).unwrap();
        assert!(text.contains("\"ticket\":\"DEMO-12\""), "{text}");
        let back: LaunchRequest = serde_json::from_str(&text).unwrap();
        assert_eq!(back, request);
    }

    #[test]
    fn each_refusal_names_its_rule() {
        let limit = Refusal::Limit {
            window: "five_hour",
            part_percent: 75.0,
            stop_percent: 60.0,
            resets: "It resets at 2026-11-16T18:00:00Z".into(),
        };
        assert_eq!(limit.rule(), "limits.five_hour");
        assert!(limit
            .to_string()
            .starts_with("refused by limits.five_hour: the five_hour window is at 75%"));
        assert!(limit.is_limit());
        let turn = Refusal::TurnBudget {
            kind: TurnKind::Reader,
            budget: 0.0,
        };
        assert_eq!(turn.rule(), "budget.reader");
        assert!(!Refusal::Off.is_limit());
    }
}
