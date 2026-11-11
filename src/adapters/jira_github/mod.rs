//! The adapter for Jira Cloud and GitHub (design section 11.5), built into the binary.
//!
//! `adapters/jira-github/adapter.toml` says `scripts = "builtin:jira-github"`. The executor then
//! starts `vision adapter-script jira-github <observe|act|job>`, with the same contract as a
//! script of `bin/`: one JSON object on standard input, one JSON object on standard output, and
//! the exit code 0, 1, or 2.
//!
//! The modules:
//!
//! - [`core`]: the call loop, the errors, the configuration, the secrets, and the text helpers.
//! - [`http`], [`git`], [`proc`]: HTTP with a time limit, git, and child processes.
//! - [`adf`]: Atlassian Document Format to Markdown and back.
//! - [`jira`], [`forge`]: the two clients.
//! - [`observe`], [`act`], [`jobs`]: the three calls of the contract.
//! - [`app_token`]: `vision app-token`, the installation token of a GitHub App.

pub mod act;
pub mod adf;
pub mod app_token;
pub mod core;
pub mod forge;
pub mod git;
pub mod http;
pub mod jira;
pub mod jobs;
pub mod observe;
pub mod proc;

/// The name of the adapter after `builtin:`.
pub const NAME: &str = "jira-github";

/// Runs one call of the adapter: `observe`, `act`, or `job`. Gives the exit code.
pub fn run(script: &str) -> u8 {
    match script {
        "observe" => core::main(observe::observe),
        "act" => core::main(act::act),
        "job" => core::main(jobs::job),
        other => {
            eprintln!("the adapter {NAME} has no script {other}. Its scripts: observe, act, job");
            1
        }
    }
}
