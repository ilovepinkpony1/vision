//! The configuration (section 4): typed values with defaults, loading, and `vision config`.
//!
//! A missing key takes its default. An unknown key is an error that names the key. The free
//! tables `[modes]`, `[secrets]`, `[adapter]`, `[agents.<name>]`, and the `adapter` table of an
//! entry of `clones` accept any names.

mod edit;

use std::collections::BTreeMap;
use std::fmt;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};

use crate::ctx::Ctx;
use crate::paths::{expand_tilde, tilde_display, Paths};
use crate::turn::TurnKind;

pub use edit::ConfigEditor;

/// The file that `vision init` writes, with the defaults and the comments of section 4.
pub const DEFAULT_TEXT: &str = include_str!("default.toml");

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    /// The name of the adapter.
    pub project: String,
    /// The writer name of the person in the memory.
    pub user: String,
    /// The clones that Vision owns, in the order of `vision take`: a path, or a table with the
    /// path and the values of that clone.
    #[serde(deserialize_with = "clone_entries")]
    pub clones: Vec<CloneEntry>,
    pub claude: Claude,
    /// The Codex CLI of the agents whose runner is `codex` (section 7.3).
    pub codex: Codex,
    /// The turns in tabs of Herdr.app (section 7.7).
    pub herdr: Herdr,
    /// The session of the person about a ticket, `vision discuss` (section 15.3).
    pub discuss: Discuss,
    pub budget: Budget,
    pub time: TimeLimits,
    pub limits: Limits,
    pub caps: Caps,
    pub judge: Judge,
    pub curator: Curator,
    pub sandbox: Sandbox,
    pub gates: Gates,
    /// How a plan gets its approval (section 11.3).
    pub plan: PlanValues,
    pub git: Git,
    /// The journal of the turns and its index in MemPalace (section 16).
    pub journal: Journal,
    /// The archive of the turn folders in a local folder or an object store (section 16).
    pub archive: Archive,
    /// The archive of the sessions of Claude Code of the person (section 16).
    pub sessions: Sessions,
    /// The escape check: the findings of a review bot of the team that our review missed.
    pub escapes: Escapes,
    /// The values of the full-screen app (section 15).
    pub app: AppValues,
    /// Verb -> mode. Only `vision promote` and `vision demote` change them.
    pub modes: BTreeMap<String, Mode>,
    /// Name -> how the executor gets it. A session never gets one.
    pub secrets: BTreeMap<String, Secret>,
    /// A free table. The executor gives it to each adapter script.
    pub adapter: toml::Table,
    /// One entry for each agent that the person pinned (section 10).
    pub agents: BTreeMap<String, AgentPin>,
}

impl Default for Config {
    fn default() -> Config {
        Config {
            project: "demo".into(),
            user: "user".into(),
            clones: Vec::new(),
            claude: Claude::default(),
            codex: Codex::default(),
            herdr: Herdr::default(),
            discuss: Discuss::default(),
            budget: Budget::default(),
            time: TimeLimits::default(),
            limits: Limits::default(),
            caps: Caps::default(),
            judge: Judge::default(),
            curator: Curator::default(),
            sandbox: Sandbox::default(),
            gates: Gates::default(),
            plan: PlanValues::default(),
            git: Git::default(),
            journal: Journal::default(),
            archive: Archive::default(),
            sessions: Sessions::default(),
            escapes: Escapes::default(),
            app: AppValues::default(),
            modes: BTreeMap::new(),
            secrets: BTreeMap::new(),
            adapter: toml::Table::new(),
            agents: BTreeMap::new(),
        }
    }
}

/// One entry of `clones`: the folder of a clone, and the values of each ticket in that clone.
/// In `config.toml` it is a path, such as `"~/work/app"`, or a table with `path`, `env`, and
/// `adapter`. An entry with no values prints as a path again.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct CloneEntry {
    /// The folder of the clone. `~` is allowed.
    pub path: String,
    /// More variables with a fixed value for each session of a ticket in this clone, and for
    /// each job of it with `env = "session"`. They win over `claude.env`, and a value of the
    /// runner wins over them. The rules of `claude.env` apply.
    pub env: BTreeMap<String, String>,
    /// Keys that win over the same keys of `[adapter]` in each call of a ticket in this clone.
    pub adapter: toml::Table,
}

impl CloneEntry {
    /// True when the entry has values of its own, so it is a table in `config.toml`.
    pub fn has_values(&self) -> bool {
        !self.env.is_empty() || !self.adapter.is_empty()
    }
}

impl Serialize for CloneEntry {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeMap;
        if !self.has_values() {
            return serializer.serialize_str(&self.path);
        }
        let mut map = serializer.serialize_map(None)?;
        map.serialize_entry("path", &self.path)?;
        if !self.env.is_empty() {
            map.serialize_entry("env", &self.env)?;
        }
        if !self.adapter.is_empty() {
            map.serialize_entry("adapter", &self.adapter)?;
        }
        map.end()
    }
}

/// The name of an entry of `clones` in a message: its number from 1, and its path when it has
/// one, such as `clones entry 2 (~/work/app-b)`.
fn entry_name(number: usize, path: Option<&str>) -> String {
    match path {
        Some(path) => format!("clones entry {number} ({path})"),
        None => format!("clones entry {number}"),
    }
}

/// Reads `clones`: each entry is a path, or a table with `path`, `env`, and `adapter`. A table
/// with another key or with no path is an error that names the entry. The form
/// `[[clones]]`, one table for each clone, gives the same list.
fn clone_entries<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Vec<CloneEntry>, D::Error> {
    use serde::de::Error;
    let raw = Vec::<toml::Value>::deserialize(deserializer)?;
    let mut entries = Vec::new();
    for (at, value) in raw.into_iter().enumerate() {
        let number = at + 1;
        let mut table = match value {
            toml::Value::String(path) => {
                entries.push(CloneEntry {
                    path,
                    ..CloneEntry::default()
                });
                continue;
            }
            toml::Value::Table(table) => table,
            _ => {
                return Err(D::Error::custom(format!(
                    "{} is neither a path nor a table. Write the folder of the clone, such as \"~/work/app\", or a table such as {{ path = \"~/work/app-b\", env = {{ WORKSPACE = \"app-b\" }} }}",
                    entry_name(number, None)
                )))
            }
        };
        let path = match table.remove("path") {
            Some(toml::Value::String(path)) if !path.trim().is_empty() => path,
            Some(toml::Value::String(_)) | None => {
                return Err(D::Error::custom(format!(
                    "{} has no path. A table of clones needs path, the folder of the clone, such as {{ path = \"~/work/app-b\" }}",
                    entry_name(number, None)
                )))
            }
            Some(_) => {
                return Err(D::Error::custom(format!(
                    "{}: path must be the folder of the clone as text, such as path = \"~/work/app-b\"",
                    entry_name(number, None)
                )))
            }
        };
        let name = entry_name(number, Some(&path));
        if let Some(key) = table
            .keys()
            .find(|key| !matches!(key.as_str(), "env" | "adapter"))
        {
            return Err(D::Error::custom(format!(
                "{name} has the unknown key `{key}`. A table of clones has path, env, and adapter"
            )));
        }
        let env = match table.remove("env") {
            None => BTreeMap::new(),
            Some(toml::Value::Table(env)) => {
                let mut texts = BTreeMap::new();
                for (key, value) in env {
                    let toml::Value::String(text) = value else {
                        return Err(D::Error::custom(format!(
                            "{name}: env.{key} must be text, such as env = {{ {key} = \"app-b\" }}"
                        )));
                    };
                    texts.insert(key, text);
                }
                texts
            }
            Some(_) => {
                let text = format!(
                    "{name}: env must be a table of texts, such as env = {{ WORKSPACE = \"app-b\" }}"
                );
                return Err(D::Error::custom(text));
            }
        };
        let adapter = match table.remove("adapter") {
            None => toml::Table::new(),
            Some(toml::Value::Table(adapter)) => adapter,
            Some(_) => {
                return Err(D::Error::custom(format!(
                    "{name}: adapter must be a table, such as adapter = {{ app_url = \"http://localhost:3500/\" }}"
                )))
            }
        };
        entries.push(CloneEntry { path, env, adapter });
    }
    Ok(entries)
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Claude {
    /// The Claude Code binary. `VISION_CLAUDE_BIN` wins over it.
    pub bin: String,
    /// `CLAUDE_CONFIG_DIR` of the sessions. `""` means: use the default login. When the key is
    /// missing, [`Config::load`] sets it to the `claude/` folder of `VISION_HOME`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub config_dir: Option<String>,
    /// More environment names that a session gets.
    pub env_pass: Vec<String>,
    /// More variables with a fixed value that a session gets, such as the name of a test
    /// database of the clone. A value of the runner wins over one of them. A session can read
    /// its environment, so a secret never goes here.
    pub env: BTreeMap<String, String>,
    /// True keeps the lines that Claude Code adds about itself: a co-author trailer in each
    /// commit and a line in the text of a pull request. False turns them off in `settings.json`.
    pub attribution: bool,
    /// Folders of Claude Code plugins that each work turn loads with `--plugin-dir`: a plugin
    /// folder (it has `.claude-plugin/plugin.json`) or a folder of plugins. `~` is allowed.
    /// A turn can read each one and never write it.
    pub plugins: Vec<String>,
}

impl Default for Claude {
    fn default() -> Claude {
        Claude {
            bin: "claude".into(),
            config_dir: None,
            env_pass: Vec::new(),
            env: BTreeMap::new(),
            attribution: true,
            plugins: Vec::new(),
        }
    }
}

/// `[codex]`: the Codex CLI for the reader turns of the agents whose runner is `codex`. Codex
/// uses its own login; Vision never reads the folder of that login.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Codex {
    /// The Codex binary. `VISION_CODEX_BIN` wins over it.
    pub bin: String,
    /// Dollars for one million tokens, to give a turn on Codex a cost. `None`: the cost is 0,
    /// and only the tokens are recorded.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub price: Option<CodexPrice>,
}

impl Default for Codex {
    fn default() -> Codex {
        Codex {
            bin: "codex".into(),
            price: None,
        }
    }
}

/// The turns in tabs of Herdr.app (section 7.7): each Claude turn of a work agent runs as an
/// interactive session in a terminal of herdr-core, which Herdr.app shows as a tab of one space,
/// and the person types into it there.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Herdr {
    /// False: each turn runs headless, with no pane.
    pub enabled: bool,
    /// The space of Herdr.app where each turn opens its tab `<ticket> <agent>`.
    pub session: String,
    /// The herdr-core CLI, which starts, reads, and types into the terminals.
    pub bin: String,
    /// The program of Herdr.app (`herdr-app`), which names and shows the tabs.
    pub app: String,
}

impl Default for Herdr {
    fn default() -> Herdr {
        Herdr {
            enabled: false,
            session: "vision".into(),
            bin: "hc".into(),
            app: "/Applications/Herdr.app/Contents/MacOS/Herdr".into(),
        }
    }
}

/// The Claude Code session of the person about a ticket, which `vision discuss` starts (section
/// 15.3). It is the session of the person, not a turn: it gets no flag of section 7.3, no
/// environment of `[claude]`, and no result schema.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Discuss {
    /// The command that starts the session. It runs in the shell of the person (`$SHELL -lic`),
    /// so an alias or a function of the shell works, and the first message goes after `--`.
    pub command: String,
}

impl Default for Discuss {
    fn default() -> Discuss {
        Discuss {
            command: "claude".into(),
        }
    }
}

/// The price of Codex tokens: dollars for one million input tokens and for one million output
/// tokens.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CodexPrice {
    pub input: f64,
    pub output: f64,
}

impl CodexPrice {
    /// The cost in dollars of these tokens.
    pub fn cost(&self, input: i64, output: i64) -> f64 {
        (input.max(0) as f64 * self.input + output.max(0) as f64 * self.output) / 1_000_000.0
    }
}

/// The program that runs the turns of an agent (section 10).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, clap::ValueEnum)]
#[serde(rename_all = "lowercase")]
pub enum Runner {
    /// Claude Code: each kind of turn.
    Claude,
    /// The Codex CLI: only reader turns.
    Codex,
}

impl Runner {
    pub fn as_str(self) -> &'static str {
        match self {
            Runner::Claude => "claude",
            Runner::Codex => "codex",
        }
    }
}

impl fmt::Display for Runner {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// The author and the committer of each commit of a session. Empty values give the name of
/// `user` with "(vision)" and an address that names nobody. A GitHub App bot has the name
/// `<app>[bot]` and the address `<id>+<app>[bot]@users.noreply.github.com`, so that GitHub
/// links each commit to the bot.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Git {
    pub name: String,
    pub email: String,
}

/// Dollars at list price, as Claude Code reports them.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Budget {
    pub reader: f64,
    pub worker: f64,
    pub tester: f64,
    pub ticket: f64,
    pub week: f64,
}

impl Default for Budget {
    fn default() -> Budget {
        Budget {
            reader: 3.0,
            worker: 25.0,
            tester: 25.0,
            ticket: 100.0,
            week: 300.0,
        }
    }
}

impl Budget {
    /// The budget of one turn of this kind.
    pub fn for_kind(&self, kind: TurnKind) -> f64 {
        match kind {
            TurnKind::Reader => self.reader,
            TurnKind::Worker => self.worker,
            TurnKind::Tester => self.tester,
        }
    }
}

/// Minutes before the supervisor kills a turn.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct TimeLimits {
    pub reader: u64,
    pub worker: u64,
    pub tester: u64,
}

impl Default for TimeLimits {
    fn default() -> TimeLimits {
        TimeLimits {
            reader: 20,
            worker: 120,
            tester: 120,
        }
    }
}

impl TimeLimits {
    /// The minutes of one turn of this kind.
    pub fn for_kind(&self, kind: TurnKind) -> u64 {
        match kind {
            TurnKind::Reader => self.reader,
            TurnKind::Worker => self.worker,
            TurnKind::Tester => self.tester,
        }
    }
}

/// No new turn above these parts (0 to 1) of the subscription windows.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Limits {
    pub five_hour: f64,
    pub seven_day: f64,
}

impl Default for Limits {
    fn default() -> Limits {
        Limits {
            five_hour: 0.60,
            seven_day: 0.85,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Caps {
    pub review_rounds: u32,
    pub ci_fix: u32,
    pub qa_fix: u32,
    pub merge: u32,
    pub comments: u32,
    pub judge_minutes: u32,
    /// Minutes between two looks for a merge of the pull request of a ticket that waits at a
    /// step that does not watch it. 0 looks at each tick.
    pub merge_look_minutes: u32,
    /// The most jobs that run in the background at one time. Each runs in the clone of its
    /// ticket, so two jobs of different clones do not meet.
    pub jobs_at_once: u32,
    /// The rounds of review comments in which one requested change can be refused before the
    /// person decides: at this refusal, the turn asks the person with a card. 0 turns the
    /// count off.
    pub refusals: u32,
}

impl Default for Caps {
    fn default() -> Caps {
        Caps {
            review_rounds: 3,
            ci_fix: 2,
            qa_fix: 2,
            merge: 3,
            comments: 3,
            judge_minutes: 15,
            merge_look_minutes: 10,
            jobs_at_once: 2,
            refusals: 3,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Judge {
    /// The Judge also answers alone from its own inference.
    pub answer_when_sure: bool,
    pub always_ask: Vec<String>,
}

impl Default for Judge {
    fn default() -> Judge {
        let always_ask = [
            "security",
            "money",
            "customer-data",
            "production",
            "deploy",
            "delete",
            "irreversible",
            "credentials",
            "external-fact",
        ];
        Judge {
            answer_when_sure: true,
            always_ask: always_ask.map(String::from).to_vec(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Curator {
    pub replay: u32,
}

impl Default for Curator {
    fn default() -> Curator {
        Curator { replay: 20 }
    }
}

/// What a worker session can reach, next to its clone (section 4).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Sandbox {
    /// More folders that it can read, such as a toolchain folder. `~` is allowed.
    pub allow_read: Vec<String>,
    pub allow_domains: Vec<String>,
    pub allow_unix_sockets: Vec<String>,
}

impl Default for Sandbox {
    fn default() -> Sandbox {
        Sandbox {
            allow_read: Vec::new(),
            allow_domains: vec!["localhost".into(), "127.0.0.1".into()],
            allow_unix_sockets: Vec::new(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Gates {
    /// A diff with more changed lines waits for the approval of the person. 0 turns the size
    /// gate off.
    pub max_diff_lines: u64,
    /// A diff that touches one of these paths, and that the approved plan does not name, waits
    /// for the approval of the person. An empty list turns the sensitive gate off.
    pub sensitive: Vec<String>,
}

/// How a plan gets its approval (section 11.3).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct PlanValues {
    /// True: at the step of `approve-plan`, the tick approves the plan itself, as `vision
    /// approve-plan` would, and the ticket goes on with no stop.
    pub auto_approve: bool,
}

impl Default for Gates {
    fn default() -> Gates {
        Gates {
            max_diff_lines: 3000,
            sensitive: vec![".github/".into(), "db/migrate/".into()],
        }
    }
}

/// The journal of the turns (section 16). The entry files are the source of truth, and
/// MemPalace is the index over them, for the turns and for the person. With no address, the
/// index is off, and the search reads the files.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Journal {
    /// The address of the MCP server of MemPalace over HTTP, such as
    /// `http://127.0.0.1:8765/mcp`. `""` turns the index off.
    pub mempalace_url: String,
    /// The wing of MemPalace that holds the entries. The room is the ticket key.
    pub wing: String,
    /// The number of entries that a work turn gets in `related.md`.
    pub related: u32,
    /// Seconds for one call to MemPalace.
    pub timeout: u64,
}

impl Default for Journal {
    fn default() -> Journal {
        Journal {
            mempalace_url: String::new(),
            wing: "vision".into(),
            related: 5,
            timeout: 5,
        }
    }
}

impl Journal {
    /// True when the index of MemPalace is on.
    pub fn index_on(&self) -> bool {
        !self.mempalace_url.trim().is_empty()
    }
}

/// The archive of the turn folders and the journal entries (section 16): a local folder, or an
/// object store, S3 or a service that speaks the same API. The `aws` program does each call to
/// a store, with its own credentials. With no folder and no bucket, the archive is off.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Archive {
    /// A local folder that holds the archive, such as `~/vision-archive`, with the same keys as
    /// paths under it. `""` means: no folder. It never goes together with a bucket.
    pub dir: String,
    /// The bucket, such as `my-vision-archive`. `""` means: no bucket.
    pub bucket: String,
    /// The start of each key in the bucket.
    pub prefix: String,
    /// The address of a service that is not AWS, for `--endpoint-url`. `""` means AWS.
    pub endpoint: String,
    /// The profile of the `aws` program, for `--profile`. `""` means its default.
    pub profile: String,
    /// Days that a local turn folder stays after the archive confirmed it.
    pub keep_days: u32,
    /// The region, for `--region`. `""` means the region of the profile.
    pub region: String,
}

impl Default for Archive {
    fn default() -> Archive {
        Archive {
            dir: String::new(),
            bucket: String::new(),
            prefix: "vision/".into(),
            endpoint: String::new(),
            profile: String::new(),
            keep_days: 14,
            region: String::new(),
        }
    }
}

impl Archive {
    /// True when the archive is on: in a folder or in a bucket.
    pub fn on(&self) -> bool {
        self.dir_on() || self.bucket_on()
    }

    /// True when the archive writes into a local folder.
    pub fn dir_on(&self) -> bool {
        !self.dir.trim().is_empty()
    }

    /// True when the archive goes to a bucket.
    pub fn bucket_on(&self) -> bool {
        !self.bucket.trim().is_empty()
    }

    /// The local folder with `~` expanded, or `None` when the archive has no folder.
    pub fn dir_path(&self) -> Result<Option<PathBuf>> {
        if !self.dir_on() {
            return Ok(None);
        }
        expand_tilde(self.dir.trim()).map(Some)
    }
}

/// The escape check (section 11.6): after our gate cr passed, the first review of a review bot
/// of the team on the pull request. Each comment of a counted priority that no finding of our
/// review matches is an escape. With no reviewer, the check is off.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Escapes {
    /// The logins of the review bots whose findings Vision compares, such as `review-bot[bot]`.
    pub reviewers: Vec<String>,
    /// A regular expression whose group 1 is the priority of an inline comment, such as
    /// `\*\*\[P([0-3])\]`. See [`crate::util::regex`] for the syntax.
    pub priority: String,
    /// The priorities that count as an escape when no finding of ours matches.
    pub count: Vec<String>,
    /// A finding of ours matches a comment on the same path within this many lines.
    pub lines: u32,
    /// The read verb of the adapter that gives the first review of a login after a time, with
    /// its inline comments.
    pub verb: String,
    /// Seconds between two looks at the reviews of one pull request while the review is
    /// missing.
    pub every: u64,
}

impl Default for Escapes {
    fn default() -> Escapes {
        Escapes {
            reviewers: Vec::new(),
            priority: "\\*\\*\\[P([0-3])\\]".into(),
            count: vec!["0".into(), "1".into(), "2".into()],
            lines: 3,
            verb: "review.read".into(),
            every: 900,
        }
    }
}

impl Escapes {
    /// True when the check is on: it names a reviewer.
    pub fn on(&self) -> bool {
        !self.reviewers.is_empty()
    }
}

/// The values of the full-screen app (section 15.3).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct AppValues {
    /// The statuses of the tracker whose sections come first in the tab "To take" of Home, in
    /// this order. Each other status of the queue follows, in the order of the queue.
    pub sections: Vec<String>,
    /// The steps of the adapter whose tickets show in the tab "Ready to merge" of Home: their
    /// pull request waits for the merge of a person. Each other ticket in progress shows in
    /// the tab "In progress".
    pub merge_steps: Vec<String>,
}

impl Default for AppValues {
    fn default() -> AppValues {
        AppValues {
            sections: Vec::new(),
            merge_steps: vec!["ready-to-merge".to_string()],
        }
    }
}

/// True when two statuses of the tracker are the same: equal without the case and without
/// the spaces at their ends, such as `In Review` and `in review`.
pub fn same_status(a: &str, b: &str) -> bool {
    a.trim().to_lowercase() == b.trim().to_lowercase()
}

/// The archive of the sessions of Claude Code of the person (section 16). Claude Code deletes
/// its session files after some days; Vision copies each finished file to the archive, and
/// writes a short summary of each session to the journal.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Sessions {
    /// True turns the archive of the sessions on. It needs the archive.
    pub enabled: bool,
    /// The folder of the session files of Claude Code. `~` is allowed. Vision only reads it.
    pub projects: String,
    /// A session file counts as finished when it has not changed for this many hours.
    pub quiet_hours: u32,
    /// The wing of MemPalace for the summaries. The room is the project folder.
    pub wing: String,
}

impl Default for Sessions {
    fn default() -> Sessions {
        Sessions {
            enabled: false,
            projects: "~/.claude/projects".into(),
            quiet_hours: 24,
            wing: "claude-sessions".into(),
        }
    }
}

impl Sessions {
    /// The folder of the session files, with `~` expanded.
    pub fn projects_path(&self) -> Result<PathBuf> {
        expand_tilde(self.projects.trim())
    }
}

/// The mode of a verb on the trust ladder (section 12).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    Hold,
    Shadow,
    Ask,
    Auto,
}

impl Mode {
    pub fn as_str(self) -> &'static str {
        match self {
            Mode::Hold => "hold",
            Mode::Shadow => "shadow",
            Mode::Ask => "ask",
            Mode::Auto => "auto",
        }
    }
}

impl fmt::Display for Mode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// How the executor gets a secret: `{ cmd = "..." }` or `{ env = "..." }`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Secret {
    /// The standard output of this shell command.
    Cmd(String),
    /// The value of this environment variable of the executor.
    Env(String),
}

/// The effort level of a model (section 10).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, clap::ValueEnum)]
#[serde(rename_all = "lowercase")]
pub enum Effort {
    Low,
    Medium,
    High,
    Xhigh,
    Max,
}

impl Effort {
    pub fn as_str(self) -> &'static str {
        match self {
            Effort::Low => "low",
            Effort::Medium => "medium",
            Effort::High => "high",
            Effort::Xhigh => "xhigh",
            Effort::Max => "max",
        }
    }
}

impl fmt::Display for Effort {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// The model of one agent that the person pinned: `[agents.<name>]`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct AgentPin {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub effort: Option<Effort>,
    /// The program of the turns: `claude` or `codex`. Only a reader can run on Codex.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub runner: Option<Runner>,
    /// The model with Claude that takes the work of a turn on Codex that fails.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fallback: Option<FallbackPin>,
    /// RFC 3339 UTC time of the pin.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pinned_at: Option<String>,
}

/// `fallback` of `[agents.<name>]`: a model with Claude and its effort.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct FallbackPin {
    pub model: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub effort: Option<Effort>,
}

impl Config {
    /// Reads `config.toml` of the home. A missing `claude.config_dir` becomes the `claude/`
    /// folder of the home, so that a session never uses a different home by accident.
    pub fn load(paths: &Paths) -> Result<Config> {
        let path = paths.config();
        let text = match std::fs::read_to_string(&path) {
            Ok(text) => text,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => bail!(
                "no configuration at {}. Run `vision init` to set up Vision there",
                path.display()
            ),
            Err(e) => return Err(e).with_context(|| format!("could not read {}", path.display())),
        };
        let mut config = Config::parse(&text).with_context(|| {
            format!(
                "the configuration {} is not valid. Fix it, then run the command again",
                path.display()
            )
        })?;
        if config.claude.config_dir.is_none() {
            config.claude.config_dir = Some(tilde_display(&paths.claude_dir()));
        }
        Ok(config)
    }

    /// Reads configuration text. The error names an unknown key and its line.
    pub fn parse(text: &str) -> Result<Config> {
        let config: Config =
            toml::from_str(text).map_err(|e| anyhow::anyhow!("{}", e.to_string().trim_end()))?;
        config.check()?;
        Ok(config)
    }

    /// `CLAUDE_CONFIG_DIR` for the sessions, or `None` for the default login.
    pub fn claude_config_dir(&self, paths: &Paths) -> Result<Option<PathBuf>> {
        match self.claude.config_dir.as_deref() {
            Some("") => Ok(None),
            Some(dir) => expand_tilde(dir).map(Some),
            None => Ok(Some(paths.claude_dir())),
        }
    }

    /// The clones with `~` expanded.
    pub fn clone_paths(&self) -> Result<Vec<PathBuf>> {
        self.clones.iter().map(|c| expand_tilde(&c.path)).collect()
    }

    /// The entry of `clones` for the folder of a clone, such as the clone of a ticket row: the
    /// same path, or the same real path.
    pub fn clone_entry(&self, clone: &Path) -> Option<&CloneEntry> {
        let real = clone.canonicalize().ok();
        self.clones.iter().find(|entry| {
            let Ok(path) = expand_tilde(&entry.path) else {
                return false;
            };
            path == clone || (real.is_some() && path.canonicalize().ok() == real)
        })
    }

    /// The `[adapter]` table of a call for a ticket in `clone`: the keys of the `adapter` table
    /// of that clone win over the same keys (a shallow merge). A call with no clone, and a clone
    /// with no entry, get the table as it is.
    pub fn adapter_for(&self, clone: Option<&Path>) -> toml::Table {
        let mut table = self.adapter.clone();
        if let Some(entry) = clone.and_then(|clone| self.clone_entry(clone)) {
            for (key, value) in &entry.adapter {
                table.insert(key.clone(), value.clone());
            }
        }
        table
    }

    /// The variables with a fixed value of a session of a ticket in `clone`: `claude.env`, and
    /// the `env` of that clone over it. The runner sets its own values after these.
    pub fn env_for(&self, clone: Option<&Path>) -> BTreeMap<String, String> {
        let mut env = self.claude.env.clone();
        if let Some(entry) = clone.and_then(|clone| self.clone_entry(clone)) {
            for (name, value) in &entry.env {
                env.insert(name.clone(), value.clone());
            }
        }
        env
    }

    /// The folder that `adapter.dir` of the free `[adapter]` table names, or `None` when the key
    /// is missing. `~` is allowed, and a relative path is relative to the home of Vision.
    pub fn adapter_dir(&self, paths: &Paths) -> Result<Option<PathBuf>> {
        let Some(value) = self.adapter.get("dir") else {
            return Ok(None);
        };
        let Some(dir) = value.as_str().filter(|d| !d.trim().is_empty()) else {
            bail!("adapter.dir must be the path of an adapter folder, such as \"~/adapters/demo\"");
        };
        let dir = expand_tilde(dir)?;
        Ok(Some(if dir.is_absolute() {
            dir
        } else {
            paths.home().join(dir)
        }))
    }

    fn check(&self) -> Result<()> {
        for (key, value) in [
            ("limits.five_hour", self.limits.five_hour),
            ("limits.seven_day", self.limits.seven_day),
        ] {
            if !(0.0..=1.0).contains(&value) {
                bail!("{key} is {value}, but it must be a part between 0 and 1, such as 0.60");
            }
        }
        check_env("claude.env", &self.claude.env)?;
        self.check_clones()?;
        for entry in &self.claude.plugins {
            if entry.trim().is_empty() || entry.contains(['\0', '\n', '\r']) {
                bail!("claude.plugins has an empty or broken entry. Each entry is the path of a plugin folder, such as \"~/plugins/mine\"");
            }
        }
        self.check_journal()?;
        self.check_archive()?;
        self.check_sessions()?;
        self.check_escapes()?;
        self.check_app()?;
        if self.codex.bin.trim().is_empty() || self.codex.bin.contains(['\0', '\n', '\r']) {
            bail!("codex.bin is the Codex binary, such as \"codex\"");
        }
        if let Some(price) = &self.codex.price {
            if !(price.input.is_finite() && price.input >= 0.0)
                || !(price.output.is_finite() && price.output >= 0.0)
            {
                bail!("codex.price gives dollars for one million tokens, such as {{ input = 1.25, output = 10.0 }}, each 0 or more");
            }
        }
        for name in ["judge", "curator"] {
            if self.agents.get(name).and_then(|p| p.runner) == Some(Runner::Codex) {
                bail!("agents.{name}.runner is codex, but only a reader turn of a work agent runs on Codex. Remove the key");
            }
        }
        let (name, email) = (self.git.name.trim(), self.git.email.trim());
        if name.is_empty() != email.is_empty() {
            bail!("git.name and git.email go together: set both, or leave both empty");
        }
        if name
            .chars()
            .any(|c| c.is_control() || matches!(c, '"' | '\\' | '<' | '>'))
        {
            bail!("git.name must be one line with no quote, backslash, `<`, or `>`");
        }
        if !email.is_empty()
            && (email.chars().any(|c| {
                c.is_control() || c.is_whitespace() || matches!(c, '"' | '\\' | '<' | '>')
            }) || !email.contains('@'))
        {
            bail!("git.email must be one address with `@` and no space, such as 7+app[bot]@users.noreply.github.com");
        }
        Ok(())
    }
}

/// The rules of `claude.env` and of the `env` of a clone (section 7.3): the name of a variable,
/// no name that the runner sets, and one line of text. `key` names the table in a message.
fn check_env(key: &str, env: &BTreeMap<String, String>) -> Result<()> {
    for (name, value) in env {
        let valid = !name.is_empty()
            && name.chars().enumerate().all(|(at, c)| {
                c == '_' || c.is_ascii_alphabetic() || (at > 0 && c.is_ascii_digit())
            });
        if !valid {
            bail!(
                "{key} has the name `{name}`, but the name of a variable has only letters, digits, and `_`, and it starts with no digit"
            );
        }
        if RUNNER_ENV.contains(&name.as_str()) {
            bail!("{key} cannot set {name}, because the runner sets it for each session");
        }
        if value.contains(['\0', '\n', '\r']) {
            bail!("{key}.{name} must be one line of text");
        }
    }
    Ok(())
}

impl Config {
    /// The values of each entry of `clones` follow their rules, and no two entries name the
    /// same clone. Two paths name the same clone when their real paths are equal. A path that
    /// does not exist, or whose `~` cannot expand, compares as it is written.
    fn check_clones(&self) -> Result<()> {
        let mut seen: Vec<(PathBuf, String)> = Vec::new();
        for (at, entry) in self.clones.iter().enumerate() {
            let name = entry_name(at + 1, Some(&entry.path));
            check_env(&format!("{name}: env"), &entry.env)?;
            if entry.adapter.contains_key("dir") {
                bail!("{name}: adapter.dir names the folder of the adapter, and one adapter serves each clone. Set it in [adapter]");
            }
            let path = expand_tilde(&entry.path).unwrap_or_else(|_| PathBuf::from(&entry.path));
            let real = path.canonicalize().unwrap_or(path);
            if let Some((_, first)) = seen.iter().find(|(other, _)| *other == real) {
                bail!(
                    "{first} and {name} name the same clone {}. Keep one of them",
                    real.display()
                );
            }
            seen.push((real, name));
        }
        Ok(())
    }

    fn check_escapes(&self) -> Result<()> {
        let escapes = &self.escapes;
        for login in &escapes.reviewers {
            if login.trim().is_empty() || login.chars().any(|c| c.is_control() || c.is_whitespace())
            {
                bail!("escapes.reviewers holds `{login}`, but each entry is one login with no space, such as \"review-bot[bot]\"");
            }
        }
        let regex = crate::util::regex::Regex::new(&escapes.priority)
            .map_err(|e| anyhow::anyhow!("escapes.priority: {e}"))?;
        if regex.groups() == 0 {
            bail!("escapes.priority has no group: its group 1 is the priority, such as \"\\\\*\\\\*\\\\[P([0-3])\\\\]\"");
        }
        if escapes.count.iter().any(|c| c.trim().is_empty()) {
            bail!("escapes.count holds an empty priority. Name each priority that counts, such as [\"0\", \"1\"]");
        }
        if escapes.lines > 1000 {
            bail!(
                "escapes.lines is {}, but it must be at most 1000",
                escapes.lines
            );
        }
        if escapes.verb.trim().is_empty() || escapes.verb.contains(char::is_whitespace) {
            bail!("escapes.verb must name a read verb of the adapter, such as \"review.read\"");
        }
        if escapes.every < 60 {
            bail!(
                "escapes.every is {}, but it must be at least 60 seconds",
                escapes.every
            );
        }
        Ok(())
    }

    /// Each entry of `app.sections` is one line of text, and no status shows two times.
    fn check_app(&self) -> Result<()> {
        let sections = &self.app.sections;
        for (at, status) in sections.iter().enumerate() {
            if status.trim().is_empty() || status.contains(['\0', '\n', '\r']) {
                bail!("app.sections has an empty or broken entry. Each entry is a status of the tracker, such as \"In Review\"");
            }
            if sections[..at].iter().any(|s| same_status(s, status)) {
                bail!(
                    "app.sections names the status \"{}\" two times",
                    status.trim()
                );
            }
        }
        Ok(())
    }

    fn check_journal(&self) -> Result<()> {
        let journal = &self.journal;
        if journal.index_on() {
            check_address(
                &journal.mempalace_url,
                "journal.mempalace_url",
                "http://127.0.0.1:8765/mcp",
            )?;
        }
        if !safe_name(&journal.wing) {
            bail!("journal.wing must be a name of letters, digits, `-`, `_`, or `.`, such as \"vision\"");
        }
        if journal.related > 50 {
            bail!(
                "journal.related is {}, but a turn gets at most 50 entries",
                journal.related
            );
        }
        if !(1..=120).contains(&journal.timeout) {
            bail!(
                "journal.timeout is {}, but it must be from 1 to 120 seconds",
                journal.timeout
            );
        }
        Ok(())
    }

    fn check_archive(&self) -> Result<()> {
        let archive = &self.archive;
        let bucket = archive.bucket.trim();
        if !bucket.is_empty()
            && !(bucket.len() >= 3
                && bucket.len() <= 63
                && bucket.chars().all(|c| {
                    c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '-' | '.')
                }))
        {
            bail!("archive.bucket must be the name of a bucket, such as \"my-vision-archive\": 3 to 63 lowercase letters, digits, `-`, or `.`, with no s3://");
        }
        let prefix = &archive.prefix;
        if prefix.starts_with('/')
            || prefix.contains("..")
            || prefix.chars().any(|c| c.is_control() || c.is_whitespace())
            || !(prefix.is_empty() || prefix.ends_with('/'))
        {
            bail!("archive.prefix must be empty or a path that ends with `/`, such as \"vision/\"");
        }
        if !archive.endpoint.trim().is_empty() {
            check_address(
                &archive.endpoint,
                "archive.endpoint",
                "https://store.example.com",
            )?;
        }
        for (key, value) in [
            ("archive.profile", &archive.profile),
            ("archive.region", &archive.region),
        ] {
            if !value.is_empty() && !safe_name(value) {
                bail!("{key} must be a name of letters, digits, `-`, `_`, or `.`");
            }
        }
        let dir = archive.dir.trim();
        if !dir.is_empty() {
            let rooted = dir == "~" || dir.starts_with("~/") || dir.starts_with('/');
            if !rooted || dir.chars().any(char::is_control) {
                bail!("archive.dir must be an absolute path or a path that starts with ~/, such as \"~/vision-archive\"");
            }
            if !bucket.is_empty() {
                bail!("archive.dir and archive.bucket go to different places: set one of them, and leave the other empty. To move a folder into a bucket, see `vision archive move-to-bucket`");
            }
        }
        Ok(())
    }

    fn check_sessions(&self) -> Result<()> {
        let sessions = &self.sessions;
        if !safe_name(&sessions.wing) {
            bail!("sessions.wing must be a name of letters, digits, `-`, `_`, or `.`, such as \"claude-sessions\"");
        }
        if sessions.wing == self.journal.wing {
            bail!("sessions.wing and journal.wing are both \"{}\": give the summaries of the sessions a wing of their own, such as \"claude-sessions\"", sessions.wing);
        }
        if !(1..=8760).contains(&sessions.quiet_hours) {
            bail!(
                "sessions.quiet_hours is {}, but it must be from 1 to 8760 hours",
                sessions.quiet_hours
            );
        }
        let projects = sessions.projects.trim();
        let rooted = projects == "~" || projects.starts_with("~/") || projects.starts_with('/');
        if !rooted || projects.chars().any(char::is_control) {
            bail!("sessions.projects must be an absolute path or a path that starts with ~/, such as \"~/.claude/projects\"");
        }
        if sessions.enabled && !self.archive.on() {
            bail!("sessions.enabled needs the archive: set archive.dir (a local folder) or archive.bucket in config.toml");
        }
        Ok(())
    }
}

/// True for a short name of letters, digits, `-`, `_`, and `.`.
fn safe_name(name: &str) -> bool {
    (1..=64).contains(&name.len())
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
}

/// An address for a service: https, or http to this machine, with no user part.
fn check_address(url: &str, key: &str, example: &str) -> Result<()> {
    let url = url.trim();
    let (scheme, rest) = url.split_once("://").unwrap_or(("", url));
    let host_port = rest.split(['/', '?', '#']).next().unwrap_or("");
    let host = if let Some(inner) = host_port.strip_prefix('[') {
        inner.split(']').next().unwrap_or("")
    } else {
        host_port.split(':').next().unwrap_or("")
    };
    let local = matches!(host, "localhost" | "127.0.0.1" | "::1");
    let fits = !host.is_empty()
        && !host_port.contains('@')
        && !url.chars().any(|c| c.is_control() || c.is_whitespace())
        && (scheme == "https" || (scheme == "http" && local));
    if !fits {
        bail!(
            "{key} must be an https address, or an http address of this machine, such as {example}"
        );
    }
    Ok(())
}

/// The variables that the runner sets for a session. `claude.env` cannot set them.
pub const RUNNER_ENV: [&str; 2] = ["CLAUDE_CONFIG_DIR", "GIT_CONFIG_GLOBAL"];

/// `vision config [--json]`: prints the configuration in use, with the defaults of missing keys.
pub fn show(ctx: &Ctx, json: bool) -> Result<()> {
    let config = ctx.config()?;
    if json {
        println!("{}", serde_json::to_string_pretty(config)?);
    } else {
        let text = toml::to_string_pretty(config).context("could not print the configuration")?;
        println!(
            "# The configuration in use: {}",
            ctx.paths.config().display()
        );
        println!("# A missing key shows its default.\n");
        print!("{text}");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_default_file_holds_the_defaults() {
        let mut parsed = Config::parse(DEFAULT_TEXT).unwrap();
        assert_eq!(
            parsed.claude.config_dir.as_deref(),
            Some("~/.vision/claude")
        );
        parsed.claude.config_dir = None;
        assert_eq!(parsed, Config::default());
    }

    #[test]
    fn defaults_follow_section_4() {
        let config = Config::default();
        assert_eq!(config.project, "demo");
        assert_eq!(config.budget.for_kind(TurnKind::Worker), 25.0);
        assert_eq!(config.time.for_kind(TurnKind::Reader), 20);
        assert_eq!(config.limits.five_hour, 0.60);
        assert_eq!(config.caps.judge_minutes, 15);
        assert!(config
            .judge
            .always_ask
            .contains(&"external-fact".to_string()));
        assert_eq!(config.gates.max_diff_lines, 3000);
        assert_eq!(config.sandbox.allow_domains, ["localhost", "127.0.0.1"]);
        assert!(config.sandbox.allow_read.is_empty());
        assert!(config.claude.attribution);
        assert!(config.claude.env.is_empty());
        let fixed =
            Config::parse("[claude]\nenv = { TEST_DB = \"app_test_2\", A1 = \"\" }\n").unwrap();
        assert_eq!(fixed.claude.env["TEST_DB"], "app_test_2");
        for (text, part) in [
            ("env = { \"1A\" = \"x\" }", "starts with no digit"),
            (
                "env = { \"A-B\" = \"x\" }",
                "has only letters, digits, and `_`",
            ),
            ("env = { GIT_CONFIG_GLOBAL = \"x\" }", "the runner sets it"),
            ("env = { A = \"x\\ny\" }", "must be one line"),
            ("env = { A = 1 }", ""),
        ] {
            let err = Config::parse(&format!("[claude]\n{text}\n")).unwrap_err();
            assert!(format!("{err:#}").contains(part), "{text}: {err:#}");
        }
        assert!(config.claude.plugins.is_empty());
        let plugins =
            Config::parse("[claude]\nplugins = [\"~/plugins/one\", \"/opt/two\"]\n").unwrap();
        assert_eq!(plugins.claude.plugins, ["~/plugins/one", "/opt/two"]);
        let err = Config::parse("[claude]\nplugins = [\" \"]\n").unwrap_err();
        assert!(format!("{err:#}").contains("claude.plugins"), "{err:#}");
        assert!(Config::parse("[claude]\nplugins = \"/one\"\n").is_err());
        let off = Config::parse("[claude]\nattribution = false\n").unwrap();
        assert!(!off.claude.attribution);
        assert!(Config::parse("[claude]\nattribution = \"no\"\n").is_err());
    }

    /// The text of an error of the loader, with its causes.
    fn refusal(text: &str) -> String {
        format!("{:#}", Config::parse(text).unwrap_err())
    }

    #[test]
    fn an_entry_of_clones_is_a_path_or_a_table_and_the_array_of_tables_gives_the_same() {
        let strings = Config::parse("clones = [\"/work/app\", \"~/work/b\"]\n").unwrap();
        assert_eq!(
            strings.clones,
            [
                CloneEntry {
                    path: "/work/app".into(),
                    ..CloneEntry::default()
                },
                CloneEntry {
                    path: "~/work/b".into(),
                    ..CloneEntry::default()
                }
            ]
        );
        assert!(!strings.clones[0].has_values());
        let mixed = Config::parse(
            r#"clones = [
  "/work/app",
  { path = "/work/app-b", env = { WORKSPACE = "app-b" }, adapter = { app_url = "http://localhost:3500/", app_health_url = "http://localhost:3500/health", port = 3500 } },
]
"#,
        )
        .unwrap();
        let b = &mixed.clones[1];
        assert_eq!(b.path, "/work/app-b");
        assert_eq!(b.env["WORKSPACE"], "app-b");
        assert_eq!(b.adapter["port"].as_integer(), Some(3500));
        assert!(b.has_values());
        // The array of tables, after the root keys, gives the same list.
        let tables = Config::parse(
            r#"project = "demo"

[[clones]]
path = "/work/app"

[[clones]]
path = "/work/app-b"
env = { WORKSPACE = "app-b" }

[clones.adapter]
app_url = "http://localhost:3500/"
app_health_url = "http://localhost:3500/health"
port = 3500
"#,
        )
        .unwrap();
        assert_eq!(tables.clones, mixed.clones);
        // A table with only a path is the same entry as the path.
        let only = Config::parse("clones = [{ path = \"/work/app\" }]\n").unwrap();
        assert_eq!(only.clones, strings.clones[..1]);
        assert_eq!(
            only.clone_paths().unwrap(),
            [PathBuf::from("/work/app")],
            "the folders keep their order"
        );
    }

    #[test]
    fn the_loader_refuses_a_broken_entry_of_clones_and_names_it() {
        for (text, part) in [
            (
                "clones = [\"/a\", { path = \"/b\", evn = { A = \"1\" } }]",
                "clones entry 2 (/b) has the unknown key `evn`",
            ),
            (
                "clones = [\"/a\", { env = { A = \"1\" } }]",
                "clones entry 2 has no path",
            ),
            ("clones = [{ path = \"\" }]", "clones entry 1 has no path"),
            (
                "clones = [{ path = 3 }]",
                "clones entry 1: path must be the folder of the clone as text",
            ),
            ("clones = [3]", "clones entry 1 is neither a path nor a table"),
            (
                "clones = [{ path = \"/b\", env = \"A=1\" }]",
                "clones entry 1 (/b): env must be a table of texts",
            ),
            (
                "clones = [{ path = \"/b\", env = { A = 1 } }]",
                "clones entry 1 (/b): env.A must be text",
            ),
            (
                "clones = [{ path = \"/b\", adapter = \"x\" }]",
                "clones entry 1 (/b): adapter must be a table",
            ),
            // The env of a clone follows each rule of claude.env.
            (
                "clones = [{ path = \"/b\", env = { \"1A\" = \"x\" } }]",
                "clones entry 1 (/b): env has the name `1A`, but the name of a variable has only letters, digits, and `_`, and it starts with no digit",
            ),
            (
                "clones = [{ path = \"/b\", env = { GIT_CONFIG_GLOBAL = \"x\" } }]",
                "clones entry 1 (/b): env cannot set GIT_CONFIG_GLOBAL, because the runner sets it for each session",
            ),
            (
                "clones = [{ path = \"/b\", env = { A = \"x\\ny\" } }]",
                "clones entry 1 (/b): env.A must be one line of text",
            ),
            (
                "clones = [{ path = \"/b\", adapter = { dir = \"/other\" } }]",
                "clones entry 1 (/b): adapter.dir names the folder of the adapter",
            ),
            (
                "clones = [\"/work/app\", { path = \"/work/app\", env = { A = \"1\" } }]",
                "clones entry 1 (/work/app) and clones entry 2 (/work/app) name the same clone /work/app",
            ),
        ] {
            let error = refusal(&format!("{text}\n"));
            assert!(error.contains(part), "{text}: {error}");
        }
        // Two paths of one folder name the same clone: the loader compares the real paths.
        let dir = tempfile::tempdir().unwrap();
        let real = dir.path().join("app");
        std::fs::create_dir(&real).unwrap();
        let link = dir.path().join("link");
        std::os::unix::fs::symlink(&real, &link).unwrap();
        let error = refusal(&format!(
            "clones = [{:?}, {:?}]\n",
            real.display().to_string(),
            link.display().to_string()
        ));
        assert!(error.contains("name the same clone"), "{error}");
        assert!(error.contains("clones entry 2"), "{error}");
    }

    #[test]
    fn the_values_of_a_clone_win_over_claude_env_and_the_adapter_table() {
        let config = Config::parse(
            r#"clones = ["/work/app", { path = "/work/app-b", env = { WORKSPACE = "app-b", ONLY_B = "1" }, adapter = { app_url = "http://localhost:3500/" } }]

[claude]
env = { WORKSPACE = "app", SHARED = "yes" }

[adapter]
app_url = "http://localhost:3000/"
board = "DEMO"
"#,
        )
        .unwrap();
        let b = Path::new("/work/app-b");
        assert_eq!(config.clone_entry(b).unwrap().path, "/work/app-b");
        assert_eq!(
            config.env_for(Some(b)),
            BTreeMap::from([
                ("ONLY_B".to_string(), "1".to_string()),
                ("SHARED".to_string(), "yes".to_string()),
                ("WORKSPACE".to_string(), "app-b".to_string()),
            ])
        );
        let table = config.adapter_for(Some(b));
        assert_eq!(table["app_url"].as_str(), Some("http://localhost:3500/"));
        assert_eq!(table["board"].as_str(), Some("DEMO"));
        // A clone with no values, a clone that is not in clones, and no clone: the tables as
        // they are.
        for clone in [
            Some(Path::new("/work/app")),
            Some(Path::new("/other")),
            None,
        ] {
            assert_eq!(config.env_for(clone), config.claude.env, "{clone:?}");
            assert_eq!(config.adapter_for(clone), config.adapter, "{clone:?}");
        }
    }

    #[test]
    fn clones_print_as_they_load_and_a_list_of_paths_prints_as_before() {
        // A list of paths prints as a list of texts.
        let strings = Config::parse("clones = [\"/work/app\", \"~/work/b\"]\n").unwrap();
        let text = toml::to_string_pretty(&strings).unwrap();
        assert!(
            text.contains("clones = [\n    \"/work/app\",\n    \"~/work/b\",\n]"),
            "{text}"
        );
        assert_eq!(
            serde_json::to_value(&strings).unwrap()["clones"],
            serde_json::json!(["/work/app", "~/work/b"])
        );
        // A table prints with its values, and the text loads back to the same values.
        let tables = Config::parse(
            "[[clones]]\npath = \"/work/app\"\n\n[[clones]]\npath = \"/work/app-b\"\nenv = { WORKSPACE = \"app-b\" }\nadapter = { port = 3500 }\n",
        )
        .unwrap();
        let text = toml::to_string_pretty(&tables).unwrap();
        assert!(
            text.contains("{ path = \"/work/app-b\", env = { WORKSPACE = \"app-b\" }, adapter = { port = 3500 } }"),
            "{text}"
        );
        assert_eq!(Config::parse(&text).unwrap(), tables);
        assert_eq!(
            serde_json::to_value(&tables).unwrap()["clones"][1],
            serde_json::json!({"path": "/work/app-b", "env": {"WORKSPACE": "app-b"}, "adapter": {"port": 3500}})
        );
    }

    #[test]
    fn the_git_identity_needs_both_values_and_safe_text() {
        assert_eq!(Config::default().git, Git::default());
        let bot = Config::parse(
            "[git]\nname = \"app[bot]\"\nemail = \"7+app[bot]@users.noreply.github.com\"\n",
        )
        .unwrap();
        assert_eq!(bot.git.name, "app[bot]");
        for (text, part) in [
            ("[git]\nname = \"app[bot]\"\n", "go together"),
            ("[git]\nemail = \"a@b\"\n", "go together"),
            ("[git]\nname = \"a<b\"\nemail = \"a@b\"\n", "git.name"),
            ("[git]\nname = \"a\"\nemail = \"a b@c\"\n", "git.email"),
            ("[git]\nname = \"a\"\nemail = \"nobody\"\n", "git.email"),
        ] {
            let error = Config::parse(text).unwrap_err().to_string();
            assert!(error.contains(part), "{text}: {error}");
        }
    }

    #[test]
    fn the_journal_and_the_archive_are_off_by_default() {
        let config = Config::default();
        assert!(!config.journal.index_on());
        assert_eq!(config.journal.wing, "vision");
        assert_eq!(config.journal.related, 5);
        assert_eq!(config.journal.timeout, 5);
        assert!(!config.archive.on());
        assert_eq!(config.archive.prefix, "vision/");
        assert_eq!(config.archive.keep_days, 14);
        let on = Config::parse(
            "[journal]\nmempalace_url = \"http://127.0.0.1:8765/mcp\"\n[archive]\nbucket = \"my-archive\"\nendpoint = \"https://store.example.com\"\nprofile = \"archive\"\n",
        )
        .unwrap();
        assert!(on.journal.index_on());
        assert!(on.archive.on());
        for (text, part) in [
            (
                "[journal]\nmempalace_url = \"http://example.com/mcp\"\n",
                "journal.mempalace_url",
            ),
            (
                "[journal]\nmempalace_url = \"ftp://127.0.0.1/mcp\"\n",
                "journal.mempalace_url",
            ),
            ("[journal]\nwing = \"a/b\"\n", "journal.wing"),
            ("[journal]\nrelated = 99\n", "journal.related"),
            ("[journal]\ntimeout = 0\n", "journal.timeout"),
            ("[archive]\nbucket = \"s3://x\"\n", "archive.bucket"),
            ("[archive]\nprefix = \"vision\"\n", "archive.prefix"),
            ("[archive]\nprefix = \"../x/\"\n", "archive.prefix"),
            (
                "[archive]\nendpoint = \"http://example.com\"\n",
                "archive.endpoint",
            ),
            ("[archive]\nprofile = \"a b\"\n", "archive.profile"),
            ("[archive]\nkept = 3\n", "kept"),
        ] {
            let err = Config::parse(text).unwrap_err();
            assert!(format!("{err:#}").contains(part), "{text}: {err:#}");
        }
        assert!(Config::parse("[archive]\nprefix = \"\"\n").is_ok());
    }

    #[test]
    fn the_escape_check_is_off_by_default_and_its_pattern_needs_a_group() {
        let config = Config::default();
        assert!(!config.escapes.on());
        assert_eq!(config.escapes.priority, r"\*\*\[P([0-3])\]");
        assert_eq!(config.escapes.count, ["0", "1", "2"]);
        assert_eq!(config.escapes.lines, 3);
        assert_eq!(config.escapes.verb, "review.read");
        let on = Config::parse(
            "[escapes]\nreviewers = [\"review-bot[bot]\"]\npriority = \"\\\\(P(\\\\d)\\\\)\"\n",
        )
        .unwrap();
        assert!(on.escapes.on());
        for (text, part) in [
            (
                "[escapes]\npriority = \"(a\"\n",
                "escapes.priority: the pattern `(a`",
            ),
            (
                "[escapes]\npriority = \"P[0-3]\"\n",
                "escapes.priority has no group",
            ),
            ("[escapes]\nreviewers = [\"a b\"]\n", "escapes.reviewers"),
            ("[escapes]\ncount = [\"\"]\n", "escapes.count"),
            ("[escapes]\nevery = 5\n", "escapes.every"),
            ("[escapes]\nlines = 5000\n", "escapes.lines"),
            ("[escapes]\nverb = \"\"\n", "escapes.verb"),
        ] {
            let err = Config::parse(text).unwrap_err();
            assert!(format!("{err:#}").contains(part), "{text}: {err:#}");
        }
    }

    #[test]
    fn the_sections_of_the_app_are_empty_by_default_and_name_each_status_once() {
        assert!(Config::default().app.sections.is_empty());
        let config = Config::parse("[app]\nsections = [\"In Progress\", \"In Review\"]\n").unwrap();
        assert_eq!(config.app.sections, ["In Progress", "In Review"]);
        for (text, part) in [
            ("[app]\nsections = [\"\"]\n", "app.sections has an empty"),
            (
                "[app]\nsections = [\"a\\nb\"]\n",
                "app.sections has an empty",
            ),
            (
                "[app]\nsections = [\"In Review\", \" in review\"]\n",
                "app.sections names the status \"in review\" two times",
            ),
            ("[app]\norder = []\n", "order"),
        ] {
            let err = Config::parse(text).unwrap_err();
            assert!(format!("{err:#}").contains(part), "{text}: {err:#}");
        }
        assert!(same_status(" In Review", "in review "));
        assert!(!same_status("In Review", "In Testing"));
    }

    #[test]
    fn the_archive_takes_a_folder_or_a_bucket_and_the_sessions_need_one() {
        let config = Config::default();
        assert!(!config.archive.dir_on());
        assert_eq!(config.archive.dir_path().unwrap(), None);
        assert!(!config.sessions.enabled);
        assert_eq!(config.sessions.projects, "~/.claude/projects");
        assert_eq!(config.sessions.quiet_hours, 24);
        assert_eq!(config.sessions.wing, "claude-sessions");
        let local = Config::parse("[archive]\ndir = \"/srv/archive\"\n").unwrap();
        assert!(local.archive.on() && local.archive.dir_on() && !local.archive.bucket_on());
        assert_eq!(
            local.archive.dir_path().unwrap(),
            Some(PathBuf::from("/srv/archive"))
        );
        let on = Config::parse(
            "[archive]\ndir = \"~/vision-archive\"\n[sessions]\nenabled = true\nquiet_hours = 2\n",
        )
        .unwrap();
        assert!(on.sessions.enabled);
        for (text, part) in [
            (
                "[archive]\ndir = \"/a\"\nbucket = \"my-archive\"\n",
                "archive.dir and archive.bucket",
            ),
            ("[archive]\ndir = \"relative/x\"\n", "archive.dir"),
            (
                "[sessions]\nenabled = true\n",
                "sessions.enabled needs the archive",
            ),
            ("[sessions]\nquiet_hours = 0\n", "sessions.quiet_hours"),
            ("[sessions]\nwing = \"a b\"\n", "sessions.wing"),
            ("[sessions]\nwing = \"vision\"\n", "a wing of their own"),
            ("[sessions]\nprojects = \"x\"\n", "sessions.projects"),
            ("[sessions]\nenable = true\n", "enable"),
        ] {
            let err = Config::parse(text).unwrap_err();
            assert!(format!("{err:#}").contains(part), "{text}: {err:#}");
        }
    }

    #[test]
    fn a_missing_key_takes_its_default() {
        let config = Config::parse("project = \"other\"\n[budget]\nworker = 40\n").unwrap();
        assert_eq!(config.project, "other");
        assert_eq!(config.budget.worker, 40.0);
        assert_eq!(config.budget.reader, 3.0);
        assert_eq!(config.caps, Caps::default());
    }

    #[test]
    fn an_unknown_key_is_an_error_that_names_it() {
        let err = Config::parse("[budget]\nwroker = 40\n")
            .unwrap_err()
            .to_string();
        assert!(err.contains("wroker"), "{err}");
        let err = Config::parse("colour = \"blue\"\n")
            .unwrap_err()
            .to_string();
        assert!(err.contains("colour"), "{err}");
        let err = Config::parse("[agents.judge]\nmodle = \"x\"\n")
            .unwrap_err()
            .to_string();
        assert!(err.contains("modle"), "{err}");
    }

    #[test]
    fn free_tables_accept_any_names() {
        let text = r#"
[modes]
"git.push" = "auto"
"pr.open" = "ask"

[secrets]
TRACKER_TOKEN = { cmd = "print-token" }
FORGE_TOKEN = { env = "FORGE_TOKEN" }

[adapter]
board = "DEMO"
nested = { any = [1, 2] }

[agents.judge]
model = "claude-fable-5-2"
effort = "high"
pinned_at = "2026-11-16T14:30:00Z"
"#;
        let config = Config::parse(text).unwrap();
        assert_eq!(config.modes["git.push"], Mode::Auto);
        assert_eq!(
            config.secrets["FORGE_TOKEN"],
            Secret::Env("FORGE_TOKEN".into())
        );
        assert_eq!(config.adapter["board"].as_str(), Some("DEMO"));
        assert_eq!(config.agents["judge"].effort, Some(Effort::High));
    }

    #[test]
    fn bad_values_are_errors() {
        assert!(Config::parse("[modes]\n\"git.push\" = \"sometimes\"\n").is_err());
        assert!(Config::parse("[limits]\nfive_hour = 60\n").is_err());
        assert!(Config::parse("[secrets]\nX = { file = \"/tmp/x\" }\n").is_err());
    }

    #[test]
    fn adapter_dir_reads_the_free_table() {
        let paths = Paths::new("/v");
        assert_eq!(Config::default().adapter_dir(&paths).unwrap(), None);
        let config = Config::parse("[adapter]\ndir = \"/elsewhere/demo\"\n").unwrap();
        assert_eq!(
            config.adapter_dir(&paths).unwrap(),
            Some(PathBuf::from("/elsewhere/demo"))
        );
        let config = Config::parse("[adapter]\ndir = \"adapters/mine\"\n").unwrap();
        assert_eq!(
            config.adapter_dir(&paths).unwrap(),
            Some(PathBuf::from("/v/adapters/mine"))
        );
        let config = Config::parse("[adapter]\ndir = 3\n").unwrap();
        assert!(config.adapter_dir(&paths).is_err());
    }

    #[test]
    fn claude_config_dir_follows_the_rules() {
        let paths = Paths::new("/v");
        let mut config = Config::default();
        assert_eq!(
            config.claude_config_dir(&paths).unwrap(),
            Some(PathBuf::from("/v/claude"))
        );
        config.claude.config_dir = Some(String::new());
        assert_eq!(config.claude_config_dir(&paths).unwrap(), None);
        config.claude.config_dir = Some("/elsewhere".into());
        assert_eq!(
            config.claude_config_dir(&paths).unwrap(),
            Some(PathBuf::from("/elsewhere"))
        );
    }
}
