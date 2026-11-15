//! The built-in adapters (section 11.1): adapters whose scripts are part of this binary.
//!
//! An `adapter.toml` with `scripts = "builtin:<name>"` uses one of them. The executor then
//! starts the current binary as the script: `vision adapter-script <name> <observe|act|job>`,
//! a hidden command. The contract stays the one of a script in `bin/`: the same input, the same
//! output, the same environment, the same time limits, and the same exit codes. A crash of a
//! built-in adapter ends only its own process, so it is a failed script, never a crash of the
//! tick.

pub mod jira_github;

/// The prefix of the key `scripts` that names a built-in adapter.
pub const PREFIX: &str = "builtin:";

/// The names of the built-in adapters.
pub const NAMES: [&str; 1] = [jira_github::NAME];

/// True when `name` is a built-in adapter.
pub fn is_builtin(name: &str) -> bool {
    NAMES.contains(&name)
}

/// The keys of the table of a job that a built-in adapter knows by its name, such as the
/// precheck of jira-github: the loader of `adapter.toml` takes them next to `timeout`,
/// `secrets`, `command`, `doc`, and `env`, tests them here, and the executor gives them to the
/// call in `table`.
pub struct JobSettings {
    /// The keys of the job.
    pub keys: &'static [&'static str],
    /// Tests the values of the keys. Gives one text for each problem.
    pub check: fn(&serde_json::Map<String, serde_json::Value>) -> Vec<String>,
    /// The environment of the job when its table has no `env`.
    pub env: crate::adapter::JobEnv,
    /// True for a job that runs code of the project: it never gets a secret or the
    /// environment of the tick.
    pub no_secrets: bool,
    /// The time limit of the job when its table has none, and the most that is too short, in
    /// seconds, from the values.
    pub timeout: fn(&serde_json::Map<String, serde_json::Value>) -> (u64, u64),
}

/// The keys of the job `job` of the built-in adapter `builtin`, or `None` for a job that has
/// none.
pub fn job_settings(builtin: &str, job: &str) -> Option<&'static JobSettings> {
    match (builtin, job) {
        (jira_github::NAME, "precheck") => Some(&jira_github::precheck::SETTINGS),
        _ => None,
    }
}

/// `vision adapter-script NAME SCRIPT`: runs one call of a built-in adapter. Gives the exit
/// code: 0 done, 1 a failure, 2 a refusal.
pub fn run_script(name: &str, script: &str) -> u8 {
    match name {
        jira_github::NAME => jira_github::run(script),
        other => {
            eprintln!(
                "no built-in adapter `{other}`. The built-in adapters: {}",
                NAMES.join(", ")
            );
            1
        }
    }
}
