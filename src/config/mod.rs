//! The configuration (section 4): typed values with defaults, loading, and `vision config`.
//!
//! A missing key takes its default. An unknown key is an error that names the key. The free
//! tables `[modes]`, `[secrets]`, `[adapter]`, and `[agents.<name>]` accept any names.

mod edit;

use std::collections::BTreeMap;
use std::fmt;
use std::path::PathBuf;

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
    /// Absolute paths of the clones that Vision owns. `~` is allowed.
    pub clones: Vec<String>,
    pub claude: Claude,
    pub budget: Budget,
    pub time: TimeLimits,
    pub limits: Limits,
    pub caps: Caps,
    pub judge: Judge,
    pub curator: Curator,
    pub sandbox: Sandbox,
    pub gates: Gates,
    pub git: Git,
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
            budget: Budget::default(),
            time: TimeLimits::default(),
            limits: Limits::default(),
            caps: Caps::default(),
            judge: Judge::default(),
            curator: Curator::default(),
            sandbox: Sandbox::default(),
            gates: Gates::default(),
            git: Git::default(),
            modes: BTreeMap::new(),
            secrets: BTreeMap::new(),
            adapter: toml::Table::new(),
            agents: BTreeMap::new(),
        }
    }
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
}

impl Default for Claude {
    fn default() -> Claude {
        Claude {
            bin: "claude".into(),
            config_dir: None,
            env_pass: Vec::new(),
            env: BTreeMap::new(),
            attribution: true,
        }
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
    pub max_diff_lines: u64,
    pub sensitive: Vec<String>,
}

impl Default for Gates {
    fn default() -> Gates {
        Gates {
            max_diff_lines: 3000,
            sensitive: vec![".github/".into(), "db/migrate/".into()],
        }
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
    /// RFC 3339 UTC time of the pin.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pinned_at: Option<String>,
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
        self.clones.iter().map(|c| expand_tilde(c)).collect()
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
        for (name, value) in &self.claude.env {
            let valid = !name.is_empty()
                && name.chars().enumerate().all(|(at, c)| {
                    c == '_' || c.is_ascii_alphabetic() || (at > 0 && c.is_ascii_digit())
                });
            if !valid {
                bail!(
                    "claude.env has the name `{name}`, but the name of a variable has only letters, digits, and `_`, and it starts with no digit"
                );
            }
            if RUNNER_ENV.contains(&name.as_str()) {
                bail!("claude.env cannot set {name}, because the runner sets it for each session");
            }
            if value.contains(['\0', '\n', '\r']) {
                bail!("claude.env.{name} must be one line of text");
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
        let off = Config::parse("[claude]\nattribution = false\n").unwrap();
        assert!(!off.claude.attribution);
        assert!(Config::parse("[claude]\nattribution = \"no\"\n").is_err());
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
