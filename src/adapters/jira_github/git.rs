//! git as a child process, with the safe options and a plain environment.

use std::path::Path;
use std::process::Command;
use std::time::Duration;

use super::core::{clean, fail, mask, plain_env, Res};
use super::proc::{self, How, Output};

/// Each git call: no hook of the clone, no fsmonitor command, no credential helper, no prompt.
pub const GIT_SAFE: [&str; 8] = [
    "-c",
    "core.hooksPath=/dev/null",
    "-c",
    "core.fsmonitor=false",
    "-c",
    "credential.helper=",
    "-c",
    "core.askPass=",
];

/// The time limit of a git call when the caller gives none.
pub const TIMEOUT: u64 = 120;

/// The plain environment of a git call: no system or global configuration, and no prompt.
pub fn git_env() -> Vec<(String, String)> {
    let mut env = plain_env();
    for (name, value) in [
        ("GIT_TERMINAL_PROMPT", "0"),
        ("GIT_CONFIG_NOSYSTEM", "1"),
        ("GIT_CONFIG_GLOBAL", "/dev/null"),
        ("GIT_ASKPASS", ""),
        ("SSH_ASKPASS", ""),
    ] {
        env.push((name.into(), value.into()));
    }
    env
}

/// One git call.
#[derive(Clone, Debug)]
pub struct Git<'a> {
    pub args: &'a [&'a str],
    pub cwd: Option<&'a Path>,
    /// A failure of git is a failure of the call.
    pub check: bool,
    pub timeout: u64,
    /// Each item `name=value` is one more configuration value of this call. It goes through the
    /// environment (GIT_CONFIG_COUNT, git 2.31 or newer) and not through the command line,
    /// because such a value can hold a token.
    pub config: &'a [String],
}

impl<'a> Git<'a> {
    pub fn new(args: &'a [&'a str], cwd: Option<&'a Path>) -> Git<'a> {
        Git {
            args,
            cwd,
            check: true,
            timeout: TIMEOUT,
            config: &[],
        }
    }

    pub fn unchecked(mut self) -> Git<'a> {
        self.check = false;
        self
    }

    pub fn timeout(mut self, seconds: u64) -> Git<'a> {
        self.timeout = seconds;
        self
    }

    pub fn config(mut self, config: &'a [String]) -> Git<'a> {
        self.config = config;
        self
    }

    /// Runs git. Each text of a failure is masked.
    pub fn run(&self) -> Res<Output> {
        let first = self.args.first().copied().unwrap_or("");
        let mut command = Command::new("git");
        command.args(GIT_SAFE).args(self.args).env_clear();
        if let Some(dir) = self.cwd {
            command.current_dir(dir);
        }
        command.envs(git_env());
        for (index, item) in self.config.iter().enumerate() {
            let (name, value) = item.split_once('=').unwrap_or((item.as_str(), ""));
            command.env(format!("GIT_CONFIG_KEY_{index}"), name);
            command.env(format!("GIT_CONFIG_VALUE_{index}"), value);
        }
        if !self.config.is_empty() {
            command.env("GIT_CONFIG_COUNT", self.config.len().to_string());
        }
        let output = match proc::run(
            command,
            None,
            Some(Duration::from_secs(self.timeout)),
            How::default(),
        ) {
            Ok(output) => output,
            Err(e) => return fail(format!("could not run git: {e}")),
        };
        if output.timed_out {
            return fail(format!("git {first} did not end in {} s", self.timeout));
        }
        if self.check && output.code != Some(0) {
            let text = output.stderr_text();
            let lines = super::core::splitlines(text.trim());
            let tail = lines[lines.len().saturating_sub(3)..].join(" ");
            return fail(format!("git {first} failed: {}", mask(&clean(&tail, 300))));
        }
        Ok(output)
    }

    /// The standard output of a checked call, trimmed.
    pub fn text(&self) -> Res<String> {
        Ok(self.run()?.stdout_text().trim().to_string())
    }
}
