//! The shared context that each command gets: the paths, the configuration, the ledger, and the log.

use std::cell::OnceCell;

use anyhow::{bail, Result};

use crate::config::Config;
use crate::ledger::Ledger;
use crate::paths::Paths;
use crate::util::log::Logger;

/// The variable that points `vision` at a different Claude Code binary, such as a fake in tests.
pub const CLAUDE_BIN_ENV: &str = "VISION_CLAUDE_BIN";

/// The context of one command. The configuration loads on first use, so that a command such as
/// `vision prompt` never reads it, and `vision init` can run before the file exists.
#[derive(Debug)]
pub struct Ctx {
    pub paths: Paths,
    config: OnceCell<Config>,
    log: Logger,
}

impl Ctx {
    pub fn new(paths: Paths) -> Ctx {
        let log = Logger::new(&paths);
        Ctx {
            paths,
            config: OnceCell::new(),
            log,
        }
    }

    /// The context of `VISION_HOME`.
    pub fn from_env() -> Result<Ctx> {
        Ok(Ctx::new(Paths::from_env()?))
    }

    /// The configuration, loaded from `config.toml` on the first call.
    pub fn config(&self) -> Result<&Config> {
        if let Some(config) = self.config.get() {
            return Ok(config);
        }
        let loaded = Config::load(&self.paths)?;
        Ok(self.config.get_or_init(|| loaded))
    }

    /// Opens a new connection to the ledger. It must exist: `vision init` creates it.
    pub fn ledger(&self) -> Result<Ledger> {
        Ledger::open(&self.paths.ledger())
    }

    /// Appends one line to `log/vision.log`.
    pub fn log(&self, message: &str) {
        self.log.line(message);
    }

    /// The Claude Code binary: `VISION_CLAUDE_BIN` when it is set, else `claude.bin` of the configuration.
    pub fn claude_bin(&self) -> Result<String> {
        match std::env::var(CLAUDE_BIN_ENV) {
            Ok(bin) if !bin.is_empty() => Ok(bin),
            _ => Ok(self.config()?.claude.bin.clone()),
        }
    }

    /// Fails with a clear next step when `vision init` has not run in this home.
    pub fn require_setup(&self) -> Result<()> {
        if !self.paths.config().is_file() {
            bail!(
                "Vision is not set up at {}. Run `vision init` first, or set VISION_HOME to the right folder",
                self.paths.home().display()
            );
        }
        Ok(())
    }
}
