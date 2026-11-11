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
