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
    pub plan_approval: String,
    pub claude: Claude,
    pub budget: Budget,
    pub time: TimeLimits,
    pub limits: Limits,
    pub caps: Caps,
    pub judge: Judge,
    pub curator: Curator,
    pub gates: Gates,
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
            plan_approval: "always".into(),
            claude: Claude::default(),
            budget: Budget::default(),
            time: TimeLimits::default(),
            limits: Limits::default(),
            caps: Caps::default(),
            judge: Judge::default(),
            curator: Curator::default(),
            gates: Gates::default(),
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
}

impl Default for Claude {
    fn default() -> Claude {
        Claude {
            bin: "claude".into(),
            config_dir: None,
            env_pass: Vec::new(),
        }
    }
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

    fn check(&self) -> Result<()> {
        for (key, value) in [
            ("limits.five_hour", self.limits.five_hour),
            ("limits.seven_day", self.limits.seven_day),
        ] {
            if !(0.0..=1.0).contains(&value) {
                bail!("{key} is {value}, but it must be a part between 0 and 1, such as 0.60");
            }
        }
        Ok(())
    }
}

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
