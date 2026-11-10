//! `adapter.toml` (section 11.2): typed values, the parser, and the tests of the step list.
//!
//! The parser collects each problem of the file and reports them together. Each problem names
//! the file, the step (or the table), and what is wrong.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use serde_json::{Map, Value};
use toml::Table;

use super::{table_to_json, toml_to_json, valid_document_name, Origin, FILE};
use crate::config::{Caps, Mode};
use crate::paths::{expand_tilde, Paths, TicketKey};
use crate::turn::{agents_dir, TurnKind};

/// The branch of a ticket when `branch` is missing.
pub const DEFAULT_BRANCH: &str = "vision/{key}";
/// The ref that a new branch starts from when `base` is missing.
pub const DEFAULT_BASE: &str = "origin/main";
/// Seconds between two observe calls of a wait step when `every` is missing.
pub const DEFAULT_EVERY: u64 = 60;

/// A parsed and tested `adapter.toml`.
#[derive(Clone, Debug, PartialEq)]
pub struct Adapter {
    /// `name`, or the name of the folder.
    pub name: String,
    /// The adapter folder: it holds `adapter.toml` and `roles/`.
    pub dir: PathBuf,
    /// The folder whose `bin/` answers the calls: `scripts`, or the adapter folder.
    pub scripts: PathBuf,
    /// The branch of a ticket, with `{key}` for the ticket key.
    pub branch: String,
    /// The ref that a new branch starts from, such as `origin/main`.
    pub base: String,
    /// The steps in the order of the file. The first one is the entry. Exactly one is the end.
    pub steps: Vec<Step>,
    /// The verbs by name.
    pub verbs: BTreeMap<String, Verb>,
    /// The jobs by name.
    pub jobs: BTreeMap<String, Job>,
    /// The envs of `[envs.<name>]`, as free tables.
    pub envs: BTreeMap<String, Map<String, Value>>,
    /// Where [`super::load`] found the folder. `None` for a parsed text.
    pub origin: Option<Origin>,
}

/// One `[[step]]`.
#[derive(Clone, Debug, PartialEq)]
pub struct Step {
    /// Unique in the file, and valid as a part of a turn id, such as `fix-ci`.
    pub id: String,
    pub kind: StepKind,
    /// The step after this one. Each step but a wait step and the end has one.
    pub next: Option<String>,
    /// A wait step: the value of `watch` (as text) -> the next step.
    pub on: BTreeMap<String, String>,
    /// A key of `[caps]`: the step counts each entry.
    pub cap: Option<String>,
    /// The end facts that the step expects.
    pub expects: Vec<Expect>,
    /// What runs when each end fact holds: verbs, usually.
    pub after: Vec<Entry>,
    /// What runs first: jobs, usually. Their documents become inputs.
    pub before: Vec<Entry>,
    /// What runs at the entry of the step, such as `ci.run`.
    pub start: Vec<Entry>,
}

/// The five kinds of step (section 11).
#[derive(Clone, Debug, PartialEq)]
pub enum StepKind {
    Turn(TurnStep),
    Job(JobStep),
    Human(HumanStep),
    Wait(WaitStep),
    End,
}

impl StepKind {
    pub fn name(&self) -> &'static str {
        match self {
            StepKind::Turn(_) => "turn",
            StepKind::Job(_) => "job",
            StepKind::Human(_) => "human",
            StepKind::Wait(_) => "wait",
            StepKind::End => "end",
        }
    }
}

/// A turn step: one agent works in a session.
#[derive(Clone, Debug, PartialEq)]
pub struct TurnStep {
    /// The agent, such as `builder`.
    pub agent: String,
    /// The kind of turn: reader, worker, or tester.
    pub turn: TurnKind,
    /// Documents of the ticket folder that the session gets, such as `approved-plan.md`.
    pub reads: Vec<String>,
    /// `with`: values that go into the facts file under `with`.
    pub with: Map<String, Value>,
}

/// A job step: the adapter runs one job.
#[derive(Clone, Debug, PartialEq)]
pub struct JobStep {
    pub job: String,
    pub args: Map<String, Value>,
}

/// A human step: it waits for a command of the person, such as `approve-plan`.
#[derive(Clone, Debug, PartialEq)]
pub struct HumanStep {
    pub action: String,
}

/// A wait step: it asks `observe` each `every` seconds and moves on the value of `watch`.
#[derive(Clone, Debug, PartialEq)]
pub struct WaitStep {
    /// A path into the answer of `observe` for the ticket, such as `ci.state`.
    pub watch: String,
    pub every: u64,
}

/// One end fact of a step (section 11.2).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Expect {
    /// `doc:<name>`: the result has that document, and it is not empty.
    Doc(String),
    /// `commits`: the branch has a commit that it did not have at the start of the turn.
    Commits,
    /// `clean`: the work tree of the clone is clean.
    Clean,
    /// `job:<name>`: the job passes at the last commit.
    Job(String),
    /// `human:<action>`: the person did that action.
    Human(String),
}

impl Expect {
    /// Reads `doc:pr.md`, `commits`, `clean`, `job:proof`, or `human:approve-plan`.
    pub fn parse(text: &str) -> Result<Expect, String> {
        let bad = || {
            format!(
                "`{text}` is not an end fact. Use doc:<name>, commits, clean, job:<name>, or human:<action>"
            )
        };
        match text {
            "commits" => return Ok(Expect::Commits),
            "clean" => return Ok(Expect::Clean),
            _ => {}
        }
        let (kind, name) = text.split_once(':').ok_or_else(bad)?;
        match kind {
            "doc" if valid_document_name(name) => Ok(Expect::Doc(name.to_string())),
            "doc" => Err(format!(
                "`{text}` names a document that is not a plain file name, such as doc:pr.md"
            )),
            "job" if valid_name(name, true) => Ok(Expect::Job(name.to_string())),
            "human" if valid_name(name, false) => Ok(Expect::Human(name.to_string())),
            _ => Err(bad()),
        }
    }
}

impl fmt::Display for Expect {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Expect::Doc(name) => write!(f, "doc:{name}"),
            Expect::Commits => f.write_str("commits"),
            Expect::Clean => f.write_str("clean"),
            Expect::Job(name) => write!(f, "job:{name}"),
            Expect::Human(action) => write!(f, "human:{action}"),
        }
    }
}

/// What an entry of `after`, `before`, or `start` runs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EntryKind {
    Verb,
    Job,
}

/// One entry of `after`, `before`, or `start`: `"git.push"`, `"job:proof"`,
/// `{ verb = "deploy", args = { env = "staging" } }`, or `{ job = "app-start", args = {...} }`.
#[derive(Clone, Debug, PartialEq)]
pub struct Entry {
    pub kind: EntryKind,
    pub name: String,
    /// The arguments of the call. Empty for a plain name.
    pub args: Map<String, Value>,
}

impl Entry {
    /// `git.push` for a verb, `job:proof` for a job.
    pub fn label(&self) -> String {
        match self.kind {
            EntryKind::Verb => self.name.clone(),
            EntryKind::Job => format!("job:{}", self.name),
        }
    }
}

/// One `[verbs."<name>"]`.
#[derive(Clone, Debug, PartialEq)]
pub struct Verb {
    pub name: String,
    /// The mode at the start. `[modes]` of the configuration wins. `ask` when missing.
    pub mode: Mode,
    /// Text or code leaves the machine, so the gates run first.
    pub leaves: bool,
    /// A read verb changes nothing outside. It needs no gate, no approval, and no idempotency
    /// key, and its answer carries documents. A turn asks for it with a result of the kind
    /// `requests`.
    pub read: bool,
    /// Seconds before the executor kills `bin/act`. `None` takes the default.
    pub timeout: Option<u64>,
}

/// One `[jobs.<name>]`.
#[derive(Clone, Debug, PartialEq)]
pub struct Job {
    pub name: String,
    /// Seconds before the executor kills `bin/job`. `None` takes the default.
    pub timeout: Option<u64>,
    /// True when `bin/job` gets the secrets for this job. A job runs code of the project, such
    /// as the tests that an agent wrote, so the default is false. Only a job that reads the
    /// tracker or the forge, and runs no code of the project, sets it.
    pub secrets: bool,
}

/// The keys of `[caps]` of the configuration, which a step can name in `cap`.
pub fn cap_names() -> Vec<String> {
    match serde_json::to_value(Caps::default()) {
        Ok(Value::Object(map)) => map.keys().cloned().collect(),
        _ => Vec::new(),
    }
}

impl Adapter {
    /// Reads and tests `adapter.toml` of `dir`. The folder of `scripts` must exist.
    pub fn read(dir: &Path) -> Result<Adapter> {
        let file = dir.join(FILE);
        let text = std::fs::read_to_string(&file)
            .with_context(|| format!("could not read {}", file.display()))?;
        let adapter = Adapter::parse(&text, dir)?;
        if !adapter.scripts.is_dir() {
            bail!(
                "{}: `scripts` names {}, which is not a folder. Fix the path: it is relative to the adapter folder, or absolute",
                file.display(),
                adapter.scripts.display()
            );
        }
        Ok(adapter)
    }

    /// Parses and tests the text of `adapter.toml` of the folder `dir`. It reads no file.
    pub fn parse(text: &str, dir: &Path) -> Result<Adapter> {
        let file = dir.join(FILE);
        let mut table: Table = toml::from_str(text).map_err(|e| {
            anyhow::anyhow!(
                "{} is not valid TOML: {}",
                file.display(),
                e.to_string().trim_end()
            )
        })?;
        let mut problems = Problems::default();
        let adapter = parse_table(&mut table, dir, &mut problems);
        if !problems.list.is_empty() {
            bail!(
                "{} is not a valid adapter. Fix it, then run the command again:\n  - {}",
                file.display(),
                problems.list.join("\n  - ")
            );
        }
        Ok(adapter)
    }

    /// The first step of the file.
    pub fn entry(&self) -> &Step {
        &self.steps[0]
    }

    /// The step with the kind `end`.
    pub fn end(&self) -> &Step {
        self.steps
            .iter()
            .find(|s| s.kind == StepKind::End)
            .unwrap_or(&self.steps[0])
    }

    pub fn step(&self, id: &str) -> Option<&Step> {
        self.steps.iter().find(|s| s.id == id)
    }

    /// The verb, or an error that names the verbs of the adapter.
    pub fn verb(&self, name: &str) -> Result<&Verb> {
        self.verbs.get(name).with_context(|| {
            format!(
                "the adapter `{}` has no verb `{name}`. Its verbs: {}",
                self.name,
                names(self.verbs.keys())
            )
        })
    }

    /// The job, or an error that names the jobs of the adapter.
    pub fn job(&self, name: &str) -> Result<&Job> {
        self.jobs.get(name).with_context(|| {
            format!(
                "the adapter `{}` has no job `{name}`. Its jobs: {}",
                self.name,
                names(self.jobs.keys())
            )
        })
    }

    /// The branch of a ticket, such as `vision/DEMO-12`.
    pub fn branch_for(&self, key: &TicketKey) -> String {
        self.branch.replace("{key}", key.as_str())
    }

    /// The path of a script, such as `<scripts>/bin/act`.
    pub fn script_path(&self, name: &str) -> PathBuf {
        self.scripts.join("bin").join(name)
    }

    /// The role file of the adapter that replaces `agents/<agent>.md`: `roles/<agent>.md` of the
    /// adapter folder, else of the scripts folder.
    pub fn role_file(&self, agent: &str) -> Option<PathBuf> {
        if !valid_name(agent, false) {
            return None;
        }
        [&self.dir, &self.scripts]
            .iter()
            .map(|dir| dir.join("roles").join(format!("{agent}.md")))
            .find(|path| path.is_file())
    }

    /// Each agent of a turn step must have a role file.
    pub fn check_agents(&self, paths: &Paths) -> Result<()> {
        for step in &self.steps {
            let StepKind::Turn(turn) = &step.kind else {
                continue;
            };
            let known = crate::models::agent(&turn.agent).is_some()
                || self.role_file(&turn.agent).is_some()
                || agents_dir(paths)
                    .join(format!("{}.md", turn.agent))
                    .is_file();
            if !known {
                bail!(
                    "{}: step `{}`: the agent `{}` has no role file. Use one of {}, or write roles/{}.md in the adapter folder",
                    self.dir.join(FILE).display(),
                    step.id,
                    turn.agent,
                    crate::models::agent_names().join(", "),
                    turn.agent
                );
            }
        }
        Ok(())
    }
}

fn names<'a>(keys: impl Iterator<Item = &'a String>) -> String {
    let list: Vec<&str> = keys.map(String::as_str).collect();
    if list.is_empty() {
        "none".to_string()
    } else {
        list.join(", ")
    }
}

#[derive(Default)]
struct Problems {
    list: Vec<String>,
}

impl Problems {
    fn add(&mut self, at: &str, what: impl fmt::Display) {
        self.list.push(format!("{at}: {what}"));
    }
}

const TOP_KEYS: [&str; 8] = [
    "name", "branch", "base", "scripts", "step", "verbs", "jobs", "envs",
];

fn parse_table(table: &mut Table, dir: &Path, problems: &mut Problems) -> Adapter {
    let folder_name = dir
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "adapter".to_string());
    let name = string(table.remove("name"), "the key `name`", problems).unwrap_or(folder_name);
    let branch = string(table.remove("branch"), "the key `branch`", problems)
        .unwrap_or_else(|| DEFAULT_BRANCH.to_string());
    check_branch(&branch, problems);
    let base = string(table.remove("base"), "the key `base`", problems)
        .unwrap_or_else(|| DEFAULT_BASE.to_string());
    if base.trim().is_empty() || base.contains(char::is_whitespace) {
        problems.add(
            "the key `base`",
            "it must be a git ref with no space, such as origin/main",
        );
    }
    let scripts = match string(table.remove("scripts"), "the key `scripts`", problems) {
        Some(path) => scripts_dir(&path, dir, problems),
        None => dir.to_path_buf(),
    };
    let verbs = parse_verbs(table.remove("verbs"), problems);
    let jobs = parse_jobs(table.remove("jobs"), problems);
    let envs = parse_envs(table.remove("envs"), problems);
    let steps = parse_steps(table.remove("step"), problems);
    for key in table.keys() {
        problems.add(
            &format!("the key `{key}`"),
            format!(
                "it is not a key of adapter.toml. The keys: {}",
                TOP_KEYS.join(", ")
            ),
        );
    }
    let adapter = Adapter {
        name,
        dir: dir.to_path_buf(),
        scripts,
        branch,
        base,
        steps,
        verbs,
        jobs,
        envs,
        origin: None,
    };
    check_links(&adapter, problems);
    adapter
}

fn check_branch(branch: &str, problems: &mut Problems) {
    if !branch.contains("{key}") {
        problems.add(
            "the key `branch`",
            "it must hold {key}, such as \"vision/{key}\"",
        );
    }
    let bad = branch.contains(char::is_whitespace)
        || branch.starts_with(['-', '/'])
        || branch.ends_with(['/', '.'])
        || branch.contains("..")
        || branch.contains(['~', '^', ':', '?', '*', '[', '\\']);
    if bad {
        problems.add(
            "the key `branch`",
            format!("`{branch}` cannot be the name of a git branch"),
        );
    }
}

fn scripts_dir(path: &str, dir: &Path, problems: &mut Problems) -> PathBuf {
    match expand_tilde(path) {
        Ok(p) if p.is_absolute() => p,
        Ok(p) => dir.join(p),
        Err(e) => {
            problems.add("the key `scripts`", format!("{e:#}"));
            dir.to_path_buf()
        }
    }
}

fn parse_verbs(value: Option<toml::Value>, problems: &mut Problems) -> BTreeMap<String, Verb> {
    let mut verbs = BTreeMap::new();
    let Some(value) = value else {
        return verbs;
    };
    let toml::Value::Table(table) = value else {
        problems.add(
            "[verbs]",
            "it must be a table of tables, such as [verbs.\"git.push\"]",
        );
        return verbs;
    };
    for (name, value) in table {
        let at = format!("[verbs.\"{name}\"]");
        if !valid_name(&name, true) {
            problems.add(
                &at,
                "a verb name has letters, digits, '.', '-', or '_', such as git.push",
            );
        }
        let toml::Value::Table(mut fields) = value else {
            problems.add(&at, "it must be a table, such as { mode = \"ask\" }");
            continue;
        };
        let mode_text = string(fields.remove("mode"), &at, problems);
        let leaves = boolean(fields.remove("leaves"), &at, problems).unwrap_or(false);
        let read = boolean(fields.remove("read"), &at, problems).unwrap_or(false);
        let timeout = seconds(fields.remove("timeout"), &format!("{at} timeout"), problems);
        for key in fields.keys() {
            problems.add(
                &at,
                format!("`{key}` is not a key of a verb. The keys: mode, leaves, read, timeout"),
            );
        }
        let mode = match mode_text.as_deref() {
            None => Mode::Ask,
            Some(text) => parse_mode(text).unwrap_or_else(|| {
                problems.add(
                    &at,
                    format!("the mode `{text}` is not one of hold, shadow, ask, auto"),
                );
                Mode::Ask
            }),
        };
        if read && (leaves || mode_text.is_some()) {
            problems.add(
                &at,
                "a read verb changes nothing outside, so it has no mode and no leaves",
            );
        }
        verbs.insert(
            name.clone(),
            Verb {
                name,
                mode,
                leaves,
                read,
                timeout,
            },
        );
    }
    verbs
}

fn parse_mode(text: &str) -> Option<Mode> {
    match text {
        "hold" => Some(Mode::Hold),
        "shadow" => Some(Mode::Shadow),
        "ask" => Some(Mode::Ask),
        "auto" => Some(Mode::Auto),
        _ => None,
    }
}

fn parse_jobs(value: Option<toml::Value>, problems: &mut Problems) -> BTreeMap<String, Job> {
    let mut jobs = BTreeMap::new();
    let Some(value) = value else {
        return jobs;
    };
    let toml::Value::Table(table) = value else {
        problems.add(
            "[jobs]",
            "it must be a table of tables, such as [jobs.proof]",
        );
        return jobs;
    };
    for (name, value) in table {
        let at = format!("[jobs.{name}]");
        if !valid_name(&name, true) {
            problems.add(
                &at,
                "a job name has letters, digits, '.', '-', or '_', such as proof",
            );
        }
        let toml::Value::Table(mut fields) = value else {
            problems.add(&at, "it must be a table, such as { timeout = 600 }");
            continue;
        };
        let timeout = seconds(fields.remove("timeout"), &format!("{at} timeout"), problems);
        let secrets = boolean(fields.remove("secrets"), &at, problems).unwrap_or(false);
        for key in fields.keys() {
            problems.add(
                &at,
                format!("`{key}` is not a key of a job. The keys: timeout, secrets"),
            );
        }
        jobs.insert(
            name.clone(),
            Job {
                name,
                timeout,
                secrets,
            },
        );
    }
    jobs
}

fn parse_envs(
    value: Option<toml::Value>,
    problems: &mut Problems,
) -> BTreeMap<String, Map<String, Value>> {
    let mut envs = BTreeMap::new();
    let Some(value) = value else {
        return envs;
    };
    let toml::Value::Table(table) = value else {
        problems.add(
            "[envs]",
            "it must be a table of tables, such as [envs.staging]",
        );
        return envs;
    };
    for (name, value) in table {
        match value {
            toml::Value::Table(fields) => {
                envs.insert(name, table_to_json(&fields));
            }
            _ => problems.add(&format!("[envs.{name}]"), "it must be a table"),
        }
    }
    envs
}

/// The keys that each kind of step can have, next to `id` and `kind`.
fn allowed_keys(kind: &str) -> &'static [&'static str] {
    match kind {
        "turn" => &[
            "agent", "turn", "reads", "expects", "with", "before", "after", "start", "next", "cap",
        ],
        "job" => &[
            "job", "args", "expects", "before", "after", "start", "next", "cap",
        ],
        "human" => &["action", "expects", "after", "start", "next", "cap"],
        "wait" => &["start", "watch", "on", "every", "cap"],
        _ => &[],
    }
}

fn parse_steps(value: Option<toml::Value>, problems: &mut Problems) -> Vec<Step> {
    let Some(value) = value else {
        problems.add(
            "the steps",
            "the file has no [[step]]. Add the steps, the first one being the entry",
        );
        return Vec::new();
    };
    let toml::Value::Array(items) = value else {
        problems.add(
            "the steps",
            "`step` must be an array of tables: use [[step]]",
        );
        return Vec::new();
    };
    if items.is_empty() {
        problems.add(
            "the steps",
            "the file has no [[step]]. Add the steps, the first one being the entry",
        );
    }
    let mut steps = Vec::new();
    for (index, item) in items.into_iter().enumerate() {
        let toml::Value::Table(table) = item else {
            problems.add(
                &format!("step {}", index + 1),
                "it must be a table: use [[step]]",
            );
            continue;
        };
        if let Some(step) = parse_step(index, table, problems) {
            steps.push(step);
        }
    }
    steps
}

fn parse_step(index: usize, mut table: Table, problems: &mut Problems) -> Option<Step> {
    let number = format!("step {}", index + 1);
    let id = match string(table.remove("id"), &number, problems) {
        Some(id) => id,
        None => {
            problems.add(&number, "it has no id, such as id = \"build\"");
            return None;
        }
    };
    let at = format!("step `{id}`");
    if !valid_step_id(&id) {
        problems.add(
            &at,
            "a step id has letters, digits, '-', or '_', starts with a letter or a digit, and has up to 64 characters",
        );
    }
    let Some(kind) = string(table.remove("kind"), &at, problems) else {
        problems.add(&at, "it has no kind. Use turn, job, human, wait, or end");
        return None;
    };
    if !["turn", "job", "human", "wait", "end"].contains(&kind.as_str()) {
        problems.add(
            &at,
            format!("the kind `{kind}` is not known. Use turn, job, human, wait, or end"),
        );
        return None;
    }
    let allowed = allowed_keys(&kind);
    let extra: Vec<String> = table
        .keys()
        .filter(|k| !allowed.contains(&k.as_str()))
        .cloned()
        .collect();
    for key in extra {
        table.remove(&key);
        problems.add(
            &at,
            format!("the key `{key}` does not belong to a {kind} step"),
        );
    }
    let next = string(table.remove("next"), &at, problems);
    let cap = string(table.remove("cap"), &at, problems);
    let expects = strings(table.remove("expects"), &format!("{at} expects"), problems)
        .into_iter()
        .filter_map(|text| match Expect::parse(&text) {
            Ok(expect) => Some(expect),
            Err(e) => {
                problems.add(&at, e);
                None
            }
        })
        .collect();
    let after = entries(table.remove("after"), &format!("{at} after"), problems);
    let before = entries(table.remove("before"), &format!("{at} before"), problems);
    let start = entries(table.remove("start"), &format!("{at} start"), problems);
    let kind = match kind.as_str() {
        "turn" => StepKind::Turn(turn_step(&mut table, &at, problems)),
        "job" => {
            let job = string(table.remove("job"), &at, problems).unwrap_or_default();
            if job.is_empty() {
                problems.add(&at, "a job step names its job, such as job = \"proof\"");
            }
            let args = object(table.remove("args"), &format!("{at} args"), problems);
            StepKind::Job(JobStep { job, args })
        }
        "human" => {
            let action = string(table.remove("action"), &at, problems).unwrap_or_default();
            if !valid_name(&action, false) {
                problems.add(
                    &at,
                    "a human step names its action, such as action = \"approve-plan\"",
                );
            }
            StepKind::Human(HumanStep { action })
        }
        "wait" => StepKind::Wait(wait_step(&mut table, &at, problems)),
        _ => StepKind::End,
    };
    let on = match &kind {
        StepKind::Wait(_) => on_table(table.remove("on"), &at, problems),
        _ => BTreeMap::new(),
    };
    match &kind {
        StepKind::End => {}
        StepKind::Wait(_) if on.is_empty() => problems.add(
            &at,
            "a wait step moves on the value of watch, so it needs on, such as on = { green = \"ready\" }",
        ),
        StepKind::Wait(_) => {}
        _ if next.is_none() => problems.add(
            &at,
            "it has no next. Each step but a wait step and the end names the step after it",
        ),
        _ => {}
    }
    Some(Step {
        id,
        kind,
        next,
        on,
        cap,
        expects,
        after,
        before,
        start,
    })
}

fn turn_step(table: &mut Table, at: &str, problems: &mut Problems) -> TurnStep {
    let agent = string(table.remove("agent"), at, problems).unwrap_or_default();
    if agent.is_empty() {
        problems.add(
            at,
            "a turn step names its agent, such as agent = \"builder\"",
        );
    } else if !valid_name(&agent, false) {
        problems.add(at, format!("`{agent}` is not a valid agent name"));
    }
    let turn = match string(table.remove("turn"), at, problems).as_deref() {
        Some("reader") => TurnKind::Reader,
        Some("worker") => TurnKind::Worker,
        Some("tester") => TurnKind::Tester,
        Some(other) => {
            problems.add(
                at,
                format!("the kind of turn `{other}` is not one of reader, worker, tester"),
            );
            TurnKind::Reader
        }
        None => {
            problems.add(
                at,
                "a turn step names its kind of turn, such as turn = \"worker\"",
            );
            TurnKind::Reader
        }
    };
    let reads = strings(table.remove("reads"), &format!("{at} reads"), problems);
    for name in &reads {
        if !valid_document_name(name) {
            problems.add(
                at,
                format!(
                    "reads names `{name}`, which is not a plain file name of the ticket folder"
                ),
            );
        }
    }
    let with = object(table.remove("with"), &format!("{at} with"), problems);
    TurnStep {
        agent,
        turn,
        reads,
        with,
    }
}

fn wait_step(table: &mut Table, at: &str, problems: &mut Problems) -> WaitStep {
    let watch = string(table.remove("watch"), at, problems).unwrap_or_default();
    let valid_watch = !watch.is_empty()
        && watch.split('.').all(|part| {
            !part.is_empty()
                && part
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_'))
        });
    if !valid_watch {
        problems.add(
            at,
            "a wait step names a path into the answer of observe in watch, such as watch = \"ci.state\"",
        );
    }
    let every = match table.remove("every") {
        None => DEFAULT_EVERY,
        Some(value) => {
            seconds(Some(value), &format!("{at} every"), problems).unwrap_or(DEFAULT_EVERY)
        }
    };
    WaitStep { watch, every }
}

fn on_table(
    value: Option<toml::Value>,
    at: &str,
    problems: &mut Problems,
) -> BTreeMap<String, String> {
    let mut on = BTreeMap::new();
    let Some(value) = value else {
        return on;
    };
    let toml::Value::Table(table) = value else {
        problems.add(at, "on must be a table, such as on = { green = \"ready\" }");
        return on;
    };
    for (value, target) in table {
        match target {
            toml::Value::String(target) => {
                on.insert(value, target);
            }
            _ => problems.add(
                at,
                format!("on.{value} must name a step, such as \"ready\""),
            ),
        }
    }
    on
}

/// The tests across the file: unique ids, targets, verbs, jobs, caps, one end, and the entry.
fn check_links(adapter: &Adapter, problems: &mut Problems) {
    let mut seen: BTreeMap<&str, usize> = BTreeMap::new();
    for (index, step) in adapter.steps.iter().enumerate() {
        if let Some(first) = seen.insert(&step.id, index) {
            problems.add(
                &format!("step `{}`", step.id),
                format!(
                    "the id is used by more than one step (steps {} and {}). Each step needs its own id",
                    first + 1,
                    index + 1
                ),
            );
        }
    }
    let ids: BTreeSet<&str> = adapter.steps.iter().map(|s| s.id.as_str()).collect();
    let caps = cap_names();
    for step in &adapter.steps {
        let at = format!("step `{}`", step.id);
        if let Some(next) = &step.next {
            if !ids.contains(next.as_str()) {
                problems.add(
                    &at,
                    format!("next names the step `{next}`, which does not exist"),
                );
            }
        }
        for (value, target) in &step.on {
            if !ids.contains(target.as_str()) {
                problems.add(
                    &at,
                    format!("on.{value} names the step `{target}`, which does not exist"),
                );
            }
        }
        for (list, entries) in [
            ("after", &step.after),
            ("before", &step.before),
            ("start", &step.start),
        ] {
            for entry in entries {
                check_entry(adapter, entry, &format!("{at} {list}"), problems);
            }
        }
        for expect in &step.expects {
            if let Expect::Job(job) = expect {
                if !adapter.jobs.contains_key(job) {
                    problems.add(&at, missing_job(job, &format!("expects job:{job}")));
                }
            }
        }
        if let StepKind::Job(job) = &step.kind {
            if !job.job.is_empty() && !adapter.jobs.contains_key(&job.job) {
                problems.add(&at, missing_job(&job.job, "job"));
            }
        }
        if let Some(cap) = &step.cap {
            if !caps.contains(cap) {
                problems.add(
                    &at,
                    format!(
                        "cap `{cap}` is not a key of [caps]. The keys: {}",
                        caps.join(", ")
                    ),
                );
            }
        }
    }
    let ends: Vec<&str> = adapter
        .steps
        .iter()
        .filter(|s| s.kind == StepKind::End)
        .map(|s| s.id.as_str())
        .collect();
    match ends.len() {
        0 if !adapter.steps.is_empty() => problems.add(
            "the steps",
            "no step has the kind end. Add exactly one, such as id = \"done\" with kind = \"end\"",
        ),
        0 | 1 => {}
        _ => problems.add(
            "the steps",
            format!(
                "exactly one step has the kind end, but {} do: {}",
                ends.len(),
                ends.join(", ")
            ),
        ),
    }
    if let Some(first) = adapter.steps.first() {
        if first.kind == StepKind::End {
            problems.add(
                &format!("step `{}`", first.id),
                "the first step is the entry of a ticket, so it cannot be the end",
            );
        }
    }
}

fn check_entry(adapter: &Adapter, entry: &Entry, at: &str, problems: &mut Problems) {
    match entry.kind {
        EntryKind::Verb if !adapter.verbs.contains_key(&entry.name) => problems.add(
            at,
            format!(
                "the verb `{}` is not in [verbs]. Add [verbs.\"{}\"], or fix the name",
                entry.name, entry.name
            ),
        ),
        EntryKind::Job if !adapter.jobs.contains_key(&entry.name) => {
            problems.add(at, missing_job(&entry.name, &format!("job:{}", entry.name)))
        }
        _ => {}
    }
}

fn missing_job(job: &str, what: &str) -> String {
    format!("{what} names the job `{job}`, which is not in [jobs]. Add [jobs.{job}], which can be empty")
}

fn entries(value: Option<toml::Value>, at: &str, problems: &mut Problems) -> Vec<Entry> {
    let Some(value) = value else {
        return Vec::new();
    };
    let toml::Value::Array(items) = value else {
        problems.add(at, "it must be a list, such as [\"git.push\"]");
        return Vec::new();
    };
    items
        .into_iter()
        .filter_map(|item| entry(item, at, problems))
        .collect()
}

fn entry(item: toml::Value, at: &str, problems: &mut Problems) -> Option<Entry> {
    match item {
        toml::Value::String(text) => {
            let (kind, name) = match text.strip_prefix("job:") {
                Some(name) => (EntryKind::Job, name.to_string()),
                None => (EntryKind::Verb, text.clone()),
            };
            if !valid_name(&name, true) {
                problems.add(at, format!("`{text}` is not a verb name or job:<name>"));
                return None;
            }
            Some(Entry {
                kind,
                name,
                args: Map::new(),
            })
        }
        toml::Value::Table(mut table) => {
            let verb = string(table.remove("verb"), at, problems);
            let job = string(table.remove("job"), at, problems);
            let args = object(table.remove("args"), &format!("{at} args"), problems);
            for key in table.keys() {
                problems.add(
                    at,
                    format!("`{key}` is not a key of an entry. The keys: verb or job, and args"),
                );
            }
            let (kind, name) = match (verb, job) {
                (Some(verb), None) => (EntryKind::Verb, verb),
                (None, Some(job)) => (EntryKind::Job, job),
                _ => {
                    problems.add(
                        at,
                        "a table entry names one verb or one job, such as { verb = \"deploy\", args = { env = \"staging\" } }",
                    );
                    return None;
                }
            };
            if !valid_name(&name, true) {
                problems.add(at, format!("`{name}` is not a valid name"));
                return None;
            }
            Some(Entry { kind, name, args })
        }
        _ => {
            problems.add(
                at,
                "each entry is a name, or a table such as { verb = \"deploy\", args = {} }",
            );
            None
        }
    }
}

fn string(value: Option<toml::Value>, at: &str, problems: &mut Problems) -> Option<String> {
    match value? {
        toml::Value::String(text) => Some(text),
        other => {
            problems.add(at, format!("a value must be a string, not `{other}`"));
            None
        }
    }
}

fn strings(value: Option<toml::Value>, at: &str, problems: &mut Problems) -> Vec<String> {
    let Some(value) = value else {
        return Vec::new();
    };
    let toml::Value::Array(items) = value else {
        problems.add(at, "it must be a list of strings");
        return Vec::new();
    };
    items
        .into_iter()
        .filter_map(|item| match item {
            toml::Value::String(text) => Some(text),
            other => {
                problems.add(at, format!("`{other}` is not a string"));
                None
            }
        })
        .collect()
}

fn boolean(value: Option<toml::Value>, at: &str, problems: &mut Problems) -> Option<bool> {
    match value? {
        toml::Value::Boolean(b) => Some(b),
        other => {
            problems.add(at, format!("`{other}` must be true or false"));
            None
        }
    }
}

fn seconds(value: Option<toml::Value>, at: &str, problems: &mut Problems) -> Option<u64> {
    match value? {
        toml::Value::Integer(n) if n > 0 => Some(n as u64),
        other => {
            problems.add(at, format!("`{other}` must be a number of seconds above 0"));
            None
        }
    }
}

fn object(value: Option<toml::Value>, at: &str, problems: &mut Problems) -> Map<String, Value> {
    match value {
        None => Map::new(),
        Some(toml::Value::Table(table)) => table_to_json(&table),
        Some(other) => {
            problems.add(
                at,
                format!(
                    "it must be a table, such as {{ env = \"staging\" }}, not `{}`",
                    toml_to_json(&other)
                ),
            );
            Map::new()
        }
    }
}

/// A name of a verb, a job, an agent, or an action: letters, digits, `-`, `_`, and `.` when
/// `dot` is true, starting with a letter or a digit, up to 64 characters.
fn valid_name(name: &str, dot: bool) -> bool {
    (1..=64).contains(&name.len())
        && name.starts_with(|c: char| c.is_ascii_alphanumeric())
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_' || (dot && c == '.'))
}

/// A step id is a part of a turn id: the rule of [`crate::paths::TurnId`].
fn valid_step_id(id: &str) -> bool {
    valid_name(id, false) && !id.ends_with('-')
}

#[cfg(test)]
mod tests {
    use super::*;

    const GOOD: &str = r#"
name = "toy"
branch = "work/{key}"
base = "origin/trunk"

[[step]]
id = "refine"
kind = "turn"
agent = "scout"
turn = "reader"
reads = ["issue.json"]
expects = ["doc:refine.md"]
after = ["tracker.describe"]
next = "build"

[[step]]
id = "build"
kind = "turn"
agent = "builder"
turn = "worker"
before = ["job:lint", { job = "app-start", args = { env = "local" } }]
expects = ["commits", "clean", "doc:pr.md", "job:proof"]
after = ["git.push", { verb = "deploy", args = { env = "staging" } }]
with = { env = "staging", depth = 2 }
cap = "review_rounds"
next = "ci"

[[step]]
id = "ci"
kind = "wait"
start = ["ci.run"]
watch = "ci.state"
on = { green = "approve", red = "build" }
every = 30

[[step]]
id = "approve"
kind = "human"
action = "approve-merge"
next = "proof"

[[step]]
id = "proof"
kind = "job"
job = "proof"
args = { green = true }
next = "done"

[[step]]
id = "done"
kind = "end"

[verbs."tracker.describe"]
mode = "auto"
leaves = true

[verbs."git.push"]
mode = "shadow"
leaves = true
timeout = 90

[verbs.deploy]

[verbs."ci.run"]
mode = "auto"

[verbs."tracker.issue"]
read = true

[jobs.proof]
timeout = 1800

[jobs.lint]

[jobs.app-start]

[envs.staging]
url = "http://localhost:3000"
"#;

    fn parse(text: &str) -> Result<Adapter> {
        Adapter::parse(text, Path::new("/adapters/toy"))
    }

    fn error(text: &str) -> String {
        format!("{:#}", parse(text).unwrap_err())
    }

    /// The good file with one line replaced.
    fn with(old: &str, new: &str) -> String {
        assert!(GOOD.contains(old), "{old}");
        GOOD.replacen(old, new, 1)
    }

    #[test]
    fn a_good_file_parses_into_typed_values() {
        let adapter = parse(GOOD).unwrap();
        assert_eq!(adapter.name, "toy");
        assert_eq!(adapter.base, "origin/trunk");
        assert_eq!(adapter.scripts, Path::new("/adapters/toy"));
        assert_eq!(adapter.entry().id, "refine");
        assert_eq!(adapter.end().id, "done");
        let key = TicketKey::parse("DEMO-12").unwrap();
        assert_eq!(adapter.branch_for(&key), "work/DEMO-12");
        let build = adapter.step("build").unwrap();
        let StepKind::Turn(turn) = &build.kind else {
            panic!("build is a turn")
        };
        assert_eq!(turn.agent, "builder");
        assert_eq!(turn.turn, TurnKind::Worker);
        assert_eq!(
            turn.with,
            serde_json::json!({"env": "staging", "depth": 2})
                .as_object()
                .unwrap()
                .clone()
        );
        assert_eq!(
            build.expects,
            [
                Expect::Commits,
                Expect::Clean,
                Expect::Doc("pr.md".into()),
                Expect::Job("proof".into())
            ]
        );
        assert_eq!(build.before[0].label(), "job:lint");
        assert_eq!(build.before[1].kind, EntryKind::Job);
        assert_eq!(build.before[1].args["env"], "local");
        assert_eq!(build.after[1].name, "deploy");
        assert_eq!(build.after[1].args["env"], "staging");
        assert_eq!(build.cap.as_deref(), Some("review_rounds"));
        let ci = adapter.step("ci").unwrap();
        assert_eq!(
            ci.kind,
            StepKind::Wait(WaitStep {
                watch: "ci.state".into(),
                every: 30
            })
        );
        assert_eq!(ci.on["red"], "build");
        assert_eq!(adapter.verbs["git.push"].mode, Mode::Shadow);
        assert_eq!(adapter.verbs["git.push"].timeout, Some(90));
        assert_eq!(
            adapter.verbs["deploy"].mode,
            Mode::Ask,
            "ask is the default"
        );
        assert!(adapter.verbs["tracker.issue"].read);
        assert_eq!(adapter.jobs["proof"].timeout, Some(1800));
        assert!(!adapter.jobs["proof"].secrets, "a job gets no secret");
        let reads = parse(&with("[jobs.lint]", "[jobs.lint]\nsecrets = true")).unwrap();
        assert!(reads.jobs["lint"].secrets);
        assert!(
            error(&with("[jobs.lint]", "[jobs.lint]\nsecrets = \"yes\"")).contains("[jobs.lint]")
        );
        assert_eq!(adapter.envs["staging"]["url"], "http://localhost:3000");
        let StepKind::Job(job) = &adapter.step("proof").unwrap().kind else {
            panic!("proof is a job step")
        };
        assert_eq!(job.args["green"], true);
    }

    #[test]
    fn missing_keys_take_their_defaults() {
        let text = "[[step]]\nid = \"a\"\nkind = \"human\"\naction = \"go\"\nnext = \"b\"\n[[step]]\nid = \"b\"\nkind = \"end\"\n";
        let adapter = parse(text).unwrap();
        assert_eq!(adapter.name, "toy", "the folder name");
        assert_eq!(adapter.branch, DEFAULT_BRANCH);
        assert_eq!(adapter.base, DEFAULT_BASE);
    }

    #[test]
    fn scripts_is_relative_to_the_adapter_folder_or_absolute() {
        let text = with("base = \"origin/trunk\"", "scripts = \"../demo\"");
        assert_eq!(
            parse(&text).unwrap().scripts,
            Path::new("/adapters/toy/../demo")
        );
        let text = with("base = \"origin/trunk\"", "scripts = \"/srv/public\"");
        assert_eq!(parse(&text).unwrap().scripts, Path::new("/srv/public"));
    }

    #[test]
    fn each_error_names_the_file_the_step_and_the_problem() {
        let cases: Vec<(String, &str)> = vec![
            (with("id = \"ci\"", "id = \"refine\""), "step `refine`: the id is used by more than one step (steps 1 and 3)"),
            (with("kind = \"wait\"", "kind = \"loop\""), "step `ci`: the kind `loop` is not known"),
            (with("next = \"ci\"", "next = \"cii\""), "step `build`: next names the step `cii`, which does not exist"),
            (with("red = \"build\"", "red = \"fix\""), "step `ci`: on.red names the step `fix`, which does not exist"),
            (with("agent = \"scout\"\n", ""), "step `refine`: a turn step names its agent"),
            (with("turn = \"reader\"", "turn = \"writer\""), "step `refine`: the kind of turn `writer` is not one of reader, worker, tester"),
            (with("turn = \"reader\"\n", ""), "step `refine`: a turn step names its kind of turn"),
            (with("after = [\"tracker.describe\"]", "after = [\"tracker.close\"]"), "step `refine` after: the verb `tracker.close` is not in [verbs]"),
            (with("start = [\"ci.run\"]", "start = [\"ci.start\"]"), "step `ci` start: the verb `ci.start` is not in [verbs]"),
            (with("before = [\"job:lint\"", "before = [\"merge\""), "step `build` before: the verb `merge` is not in [verbs]"),
            (with("\"job:lint\"", "\"job:format\""), "step `build` before: job:format names the job `format`, which is not in [jobs]"),
            (with("\"job:proof\"]", "\"job:prove\"]"), "step `build`: expects job:prove names the job `prove`, which is not in [jobs]"),
            (with("job = \"proof\"\nargs", "job = \"other\"\nargs"), "step `proof`: job names the job `other`"),
            (with("cap = \"review_rounds\"", "cap = \"rounds\""), "step `build`: cap `rounds` is not a key of [caps]"),
            (with("kind = \"end\"", "kind = \"human\"\naction = \"x\"\nnext = \"refine\""), "the steps: no step has the kind end"),
            (with("action = \"approve-merge\"\nnext = \"proof\"", "action = \"approve-merge\"\nnext = \"proof\"\n[[step]]\nid = \"stop\"\nkind = \"end\""), "exactly one step has the kind end, but 2 do: stop, done"),
            (with("expects = [\"doc:refine.md\"]", "expects = [\"refined\"]"), "step `refine`: `refined` is not an end fact"),
            (with("expects = [\"doc:refine.md\"]", "expects = [\"doc:../x\"]"), "step `refine`: `doc:../x` names a document that is not a plain file name"),
            (with("watch = \"ci.state\"", "watch = \"ci..state\""), "step `ci`: a wait step names a path into the answer of observe"),
            (with("on = { green = \"approve\", red = \"build\" }", ""), "step `ci`: a wait step moves on the value of watch, so it needs on"),
            (with("next = \"proof\"", ""), "step `approve`: it has no next"),
            (with("action = \"approve-merge\"", "agent = \"builder\"\naction = \"approve-merge\""), "step `approve`: the key `agent` does not belong to a human step"),
            (with("branch = \"work/{key}\"", "branch = \"work/main\""), "the key `branch`: it must hold {key}"),
            (with("mode = \"shadow\"", "mode = \"sometimes\""), "[verbs.\"git.push\"]: the mode `sometimes` is not one of hold, shadow, ask, auto"),
            (with("[verbs.\"tracker.issue\"]\nread = true", "[verbs.\"tracker.issue\"]\nread = true\nleaves = true"), "a read verb changes nothing outside, so it has no mode and no leaves"),
            (with("timeout = 1800", "timeout = 0"), "[jobs.proof] timeout: `0` must be a number of seconds above 0"),
            (with("[jobs.lint]", "[jobs.lint]\nretries = 2"), "[jobs.lint]: `retries` is not a key of a job"),
            (with("name = \"toy\"", "name = \"toy\"\ncolour = \"blue\""), "the key `colour`: it is not a key of adapter.toml"),
            (with("{ verb = \"deploy\", args = { env = \"staging\" } }", "{ verb = \"deploy\", job = \"lint\" }"), "a table entry names one verb or one job"),
            (with("with = { env = \"staging\", depth = 2 }", "with = \"staging\""), "step `build` with: it must be a table"),
            (with("id = \"refine\"\n", ""), "step 1: it has no id"),
            (with("id = \"done\"", "id = \"done/now\""), "step `done/now`: a step id has letters, digits"),
        ];
        for (text, expected) in cases {
            let err = error(&text);
            assert!(
                err.starts_with("/adapters/toy/adapter.toml is not a valid adapter"),
                "{err}"
            );
            assert!(err.contains(expected), "expected `{expected}` in:\n{err}");
        }
    }

    #[test]
    fn the_entry_cannot_be_the_end_and_an_empty_file_has_no_steps() {
        let err = error("[[step]]\nid = \"done\"\nkind = \"end\"\n");
        assert!(
            err.contains("the first step is the entry of a ticket, so it cannot be the end"),
            "{err}"
        );
        let err = error("name = \"x\"\n");
        assert!(err.contains("the file has no [[step]]"), "{err}");
        let err = error("name = [");
        assert!(err.contains("is not valid TOML"), "{err}");
    }

    #[test]
    fn several_problems_are_reported_together() {
        let text = with("next = \"ci\"", "next = \"cii\"").replacen(
            "cap = \"review_rounds\"",
            "cap = \"rounds\"",
            1,
        );
        let err = error(&text);
        assert!(err.contains("next names the step `cii`"), "{err}");
        assert!(err.contains("cap `rounds`"), "{err}");
    }

    #[test]
    fn caps_are_the_keys_of_the_configuration() {
        let caps = cap_names();
        for cap in ["review_rounds", "ci_fix", "comments"] {
            assert!(caps.contains(&cap.to_string()), "{caps:?}");
        }
    }

    #[test]
    fn end_facts_round_trip() {
        for text in [
            "doc:pr.md",
            "commits",
            "clean",
            "job:proof",
            "human:approve-plan",
        ] {
            assert_eq!(Expect::parse(text).unwrap().to_string(), text);
        }
        assert!(Expect::parse("job:").is_err());
        assert!(Expect::parse("human:a b").is_err());
    }

    #[test]
    fn an_unknown_agent_needs_a_role_file() {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::new(dir.path().join("home"));
        let adapter_dir = dir.path().join("toy");
        let text = GOOD.replacen("agent = \"scout\"", "agent = \"planner\"", 1);
        let adapter = Adapter::parse(&text, &adapter_dir).unwrap();
        let err = adapter.check_agents(&paths).unwrap_err().to_string();
        assert!(
            err.contains("step `refine`: the agent `planner` has no role file"),
            "{err}"
        );
        std::fs::create_dir_all(adapter_dir.join("roles")).unwrap();
        std::fs::write(adapter_dir.join("roles/planner.md"), "# Planner\n").unwrap();
        adapter.check_agents(&paths).unwrap();
        assert_eq!(
            adapter.role_file("planner"),
            Some(adapter_dir.join("roles/planner.md"))
        );
    }
}
