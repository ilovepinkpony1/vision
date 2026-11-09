//! The files on disk (section 3): `VISION_HOME`, each path in it, ticket keys, and turn ids.

use std::env;
use std::ffi::OsString;
use std::fmt;
use std::path::{Path, PathBuf};
use std::str::FromStr;

use anyhow::{bail, Context, Result};
use time::OffsetDateTime;

use crate::turn::TurnKind;
use crate::util::time::{compact, parse_compact};

/// The name of the variable that points `vision` at its state folder.
pub const HOME_ENV: &str = "VISION_HOME";

/// The state folder and each path in it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Paths {
    home: PathBuf,
}

impl Paths {
    /// Reads `VISION_HOME`. The default is `~/.vision`. A relative value is made absolute,
    /// because the timer and the supervisor run in other folders.
    pub fn from_env() -> Result<Paths> {
        let home = match env::var_os(HOME_ENV).filter(|v| !v.is_empty()) {
            Some(value) => expand_tilde_os(value)?,
            None => default_home()?,
        };
        let home = std::path::absolute(&home)
            .with_context(|| format!("could not make {} an absolute path", home.display()))?;
        Ok(Paths::new(home))
    }

    pub fn new(home: impl Into<PathBuf>) -> Paths {
        Paths { home: home.into() }
    }

    pub fn home(&self) -> &Path {
        &self.home
    }

    /// True when the home is `~/.vision`.
    pub fn is_default(&self) -> bool {
        default_home().is_ok_and(|default| default == self.home)
    }

    pub fn config(&self) -> PathBuf {
        self.home.join("config.toml")
    }

    pub fn ledger(&self) -> PathBuf {
        self.home.join("ledger.sqlite")
    }

    pub fn state(&self) -> PathBuf {
        self.home.join("state.json")
    }

    pub fn heartbeat(&self) -> PathBuf {
        self.home.join("heartbeat")
    }

    pub fn lock(&self) -> PathBuf {
        self.home.join("lock")
    }

    pub fn off(&self) -> PathBuf {
        self.home.join("off")
    }

    pub fn log_dir(&self) -> PathBuf {
        self.home.join("log")
    }

    pub fn log_file(&self) -> PathBuf {
        self.log_dir().join("vision.log")
    }

    /// `CLAUDE_CONFIG_DIR` of the sessions, with its own login.
    pub fn claude_dir(&self) -> PathBuf {
        self.home.join("claude")
    }

    pub fn sandbox_dir(&self) -> PathBuf {
        self.home.join("sandbox")
    }

    /// The Claude Code settings file of one kind of turn, such as `sandbox/worker.json`.
    pub fn sandbox_settings(&self, kind: TurnKind) -> PathBuf {
        self.sandbox_dir().join(format!("{}.json", kind.as_str()))
    }

    pub fn skills_dir(&self) -> PathBuf {
        self.home.join("skills")
    }

    pub fn memory_dir(&self) -> PathBuf {
        self.home.join("memory")
    }

    pub fn decisions_dir(&self) -> PathBuf {
        self.memory_dir().join("decisions")
    }

    pub fn principles_dir(&self) -> PathBuf {
        self.memory_dir().join("principles")
    }

    pub fn archive_file(&self) -> PathBuf {
        self.memory_dir().join("ARCHIVE.md")
    }

    /// The private adapters. They win over the adapters of the repo.
    pub fn adapters_dir(&self) -> PathBuf {
        self.home.join("adapters")
    }

    pub fn adapter_dir(&self, name: &str) -> PathBuf {
        self.adapters_dir().join(name)
    }

    pub fn tickets_dir(&self) -> PathBuf {
        self.home.join("tickets")
    }

    pub fn ticket_dir(&self, key: &TicketKey) -> PathBuf {
        self.tickets_dir().join(key.as_str())
    }

    /// A document of a ticket, such as `plan.md` or `approved-plan.md`.
    pub fn ticket_doc(&self, key: &TicketKey, name: &str) -> PathBuf {
        self.ticket_dir(key).join(name)
    }

    pub fn turns_dir(&self, key: &TicketKey) -> PathBuf {
        self.ticket_dir(key).join("turns")
    }

    /// The folder of one turn and the files in it.
    pub fn turn(&self, key: &TicketKey, id: &TurnId) -> TurnPaths {
        TurnPaths::new(self.turns_dir(key).join(id.to_string()))
    }
}

/// The folder of one turn and the files in it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TurnPaths {
    pub dir: PathBuf,
    /// `facts.json`: the input of the turn.
    pub facts: PathBuf,
    /// `stream.jsonl`: each event line of the session.
    pub stream: PathBuf,
    /// `turn.json`: the status that the supervisor writes.
    pub status: PathBuf,
    /// `result.json`: the result object of the turn.
    pub result: PathBuf,
    /// `inbox/`: messages that wait for the session.
    pub inbox: PathBuf,
    /// `inbox/sent/`: messages that the session got.
    pub inbox_sent: PathBuf,
}

impl TurnPaths {
    fn new(dir: PathBuf) -> TurnPaths {
        let inbox = dir.join("inbox");
        TurnPaths {
            facts: dir.join("facts.json"),
            stream: dir.join("stream.jsonl"),
            status: dir.join("turn.json"),
            result: dir.join("result.json"),
            inbox_sent: inbox.join("sent"),
            inbox,
            dir,
        }
    }
}

/// A ticket key that is safe as a folder name, such as `DEMO-12`.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct TicketKey(String);

impl TicketKey {
    pub fn parse(key: &str) -> Result<TicketKey> {
        let valid = (1..=64).contains(&key.len())
            && key.starts_with(|c: char| c.is_ascii_alphanumeric())
            && key
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'));
        if !valid {
            bail!(
                "`{key}` is not a valid ticket key. Use up to 64 letters, digits, '-', '_', or '.', \
                 starting with a letter or a digit, such as DEMO-12"
            );
        }
        Ok(TicketKey(key.to_string()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for TicketKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl FromStr for TicketKey {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<TicketKey> {
        TicketKey::parse(s)
    }
}

/// A turn id: `<UTC time>-<step>-<n>`, such as `20261116T143000Z-build-1`. It is safe as a folder name.
///
/// The step may hold `-`, such as `fix-ci`; `n` is the last part and starts at 1.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct TurnId {
    at: OffsetDateTime,
    step: String,
    n: u32,
}

impl TurnId {
    /// A new id. `at` is cut to whole seconds in UTC.
    pub fn new(at: OffsetDateTime, step: &str, n: u32) -> Result<TurnId> {
        check_step(step)?;
        if n == 0 {
            bail!("the number of a turn id starts at 1");
        }
        let at = parse_compact(&compact(at))?;
        Ok(TurnId {
            at,
            step: step.to_string(),
            n,
        })
    }

    pub fn parse(text: &str) -> Result<TurnId> {
        let bad = || format!("`{text}` is not a turn id, such as 20261116T143000Z-build-1");
        let (time_part, rest) = text.split_at_checked(16).with_context(bad)?;
        let rest = rest.strip_prefix('-').with_context(bad)?;
        let (step, n) = rest.rsplit_once('-').with_context(bad)?;
        let valid_n = !n.is_empty() && !n.starts_with('0') && n.bytes().all(|b| b.is_ascii_digit());
        if !valid_n {
            bail!(bad());
        }
        let n: u32 = n.parse().with_context(bad)?;
        let at = parse_compact(time_part).with_context(bad)?;
        TurnId::new(at, step, n).with_context(bad)
    }

    pub fn at(&self) -> OffsetDateTime {
        self.at
    }

    pub fn step(&self) -> &str {
        &self.step
    }

    pub fn n(&self) -> u32 {
        self.n
    }
}

impl fmt::Display for TurnId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}-{}-{}", compact(self.at), self.step, self.n)
    }
}

impl FromStr for TurnId {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<TurnId> {
        TurnId::parse(s)
    }
}

/// The folder of the person: `HOME`.
pub fn user_home() -> Result<PathBuf> {
    env::var_os("HOME")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .context("HOME is not set. Set HOME, or set VISION_HOME to the state folder")
}

/// Expands `~` or `~/...` at the start of a path to the home folder of the person.
pub fn expand_tilde(path: &str) -> Result<PathBuf> {
    expand_tilde_os(OsString::from(path))
}

/// Shows a path with `~` for the home folder of the person, when the path is in it.
pub fn tilde_display(path: &Path) -> String {
    if let Ok(home) = user_home() {
        if path == home {
            return "~".to_string();
        }
        if let Ok(rest) = path.strip_prefix(&home) {
            return format!("~/{}", rest.display());
        }
    }
    path.display().to_string()
}

fn expand_tilde_os(path: OsString) -> Result<PathBuf> {
    let text = path.to_string_lossy();
    if text == "~" {
        return user_home();
    }
    if let Some(rest) = text.strip_prefix("~/") {
        return Ok(user_home()?.join(rest));
    }
    Ok(PathBuf::from(path))
}

fn default_home() -> Result<PathBuf> {
    Ok(user_home()?.join(".vision"))
}

fn check_step(step: &str) -> Result<()> {
    let valid = !step.is_empty()
        && step.len() <= 64
        && step.starts_with(|c: char| c.is_ascii_alphanumeric())
        && !step.ends_with('-')
        && step
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_'));
    if !valid {
        bail!(
            "`{step}` is not a valid step id. Use letters, digits, '-', or '_', \
             starting with a letter or a digit, such as build or fix-ci"
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use time::macros::datetime;

    #[test]
    fn turn_id_has_the_form_of_section_3() {
        let id = TurnId::new(datetime!(2026-11-16 14:30:00.9 UTC), "build", 1).unwrap();
        assert_eq!(id.to_string(), "20261116T143000Z-build-1");
        assert_eq!(id.at(), datetime!(2026-11-16 14:30:00 UTC));
    }

    #[test]
    fn turn_id_round_trips_with_a_hyphen_in_the_step() {
        let id = TurnId::parse("20261116T143000Z-fix-ci-12").unwrap();
        assert_eq!(id.step(), "fix-ci");
        assert_eq!(id.n(), 12);
        assert_eq!(id.to_string(), "20261116T143000Z-fix-ci-12");
    }

    #[test]
    fn turn_id_rejects_bad_text() {
        for bad in [
            "",
            "build-1",
            "20261116T143000Z-build",
            "20261116T143000Z--1",
            "20261116T143000Z-build-0",
            "20261116T143000Z-build-01",
            "20261116T143000Z-build-x",
            "20261116T143000Z-../x-1",
            "2026-11-16T14:30-build-1",
            "20261116T143000Zbuild-1",
        ] {
            assert!(TurnId::parse(bad).is_err(), "{bad} must not parse");
        }
        assert!(TurnId::new(datetime!(2026-11-16 14:30:00 UTC), "a/b", 1).is_err());
    }

    #[test]
    fn ticket_keys_are_safe_folder_names() {
        assert_eq!(TicketKey::parse("DEMO-12").unwrap().as_str(), "DEMO-12");
        for bad in ["", "..", ".hidden", "a/b", "a b", "-x"] {
            assert!(TicketKey::parse(bad).is_err(), "{bad} must not parse");
        }
    }

    #[test]
    fn turn_paths_follow_section_3() {
        let paths = Paths::new("/v");
        let key = TicketKey::parse("DEMO-12").unwrap();
        let id = TurnId::parse("20261116T143000Z-build-1").unwrap();
        let turn = paths.turn(&key, &id);
        assert_eq!(
            turn.dir,
            Path::new("/v/tickets/DEMO-12/turns/20261116T143000Z-build-1")
        );
        assert_eq!(turn.status, turn.dir.join("turn.json"));
        assert_eq!(turn.inbox_sent, turn.dir.join("inbox/sent"));
        assert_eq!(
            paths.sandbox_settings(TurnKind::Reader),
            Path::new("/v/sandbox/reader.json")
        );
        assert_eq!(
            paths.ticket_doc(&key, "plan.md"),
            Path::new("/v/tickets/DEMO-12/plan.md")
        );
    }
}
