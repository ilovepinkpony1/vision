//! The command tree of `vision`: sections 7.5, 8.4, 9.2, 10, and 13 of the design.
//!
//! This file is final for the milestones that follow. Each group of commands that a later
//! milestone builds is one enum, which `main` hands to the entry function of its module:
//! [`PipelineCommand`], [`TurnCommand`], [`QuestionsCommand`], [`MemoryArgs`], [`ModelArgs`],
//! and [`ExecutorCommand`]. The `display_order` values follow the list of section 13.

use std::path::PathBuf;

use clap::{ArgGroup, Args, Parser, Subcommand, ValueEnum};

use crate::config::Effort;
use crate::paths::{TicketKey, TurnId};

const AFTER_HELP: &str = "\
Environment:
  VISION_HOME        the state folder (default: ~/.vision)
  VISION_CLAUDE_BIN  the Claude Code binary (default: claude.bin of config.toml)

Exit codes: 0 done, 1 error, 2 a gate refused, 3 not built yet.";

/// Takes a ticket from a tracker to a pull request that is ready to merge.
#[derive(Debug, Parser)]
#[command(name = "vision", version, arg_required_else_help = true, after_help = AFTER_HELP)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Write the home folder, the configuration, the ledger, and the memory repo
    #[command(display_order = 1)]
    Init,

    /// Print the configuration in use, with the default of each missing key
    #[command(display_order = 2)]
    Config {
        /// Print JSON instead of TOML
        #[arg(long)]
        json: bool,
    },

    /// Show each ticket, its state, its marks, and the limits
    #[command(display_order = 3)]
    Status {
        /// Print JSON instead of text
        #[arg(long)]
        json: bool,
    },

    /// Show the status view, drawn again each 2 seconds until Ctrl-C
    #[command(display_order = 4)]
    Watch,

    /// Print a short status for the shell prompt, such as `V:0 12s`
    #[command(display_order = 5)]
    Prompt,

    /// Run one pass: reconcile, collect, test, select, act, and write the snapshot
    #[command(display_order = 6)]
    Tick,

    /// Turn Vision on again after `vision off`
    #[command(display_order = 7)]
    On,

    /// The kill switch: stop each action until `vision on`
    #[command(display_order = 8)]
    Off,

    /// Test the setup
    #[command(display_order = 9)]
    Doctor {
        /// Print JSON instead of text
        #[arg(long)]
        json: bool,
    },

    #[command(flatten)]
    Pipeline(PipelineCommand),

    #[command(flatten)]
    Turn(TurnCommand),

    #[command(flatten)]
    Questions(QuestionsCommand),

    /// List and search the memory, or change it
    #[command(display_order = 26)]
    Memory(MemoryArgs),

    /// Run the Curator now
    #[command(display_order = 27)]
    Curate,

    /// Show or set the model of each agent
    #[command(display_order = 28)]
    Model(ModelArgs),

    #[command(flatten)]
    Executor(ExecutorCommand),

    /// Install the system timer that runs `vision tick` each minute
    #[command(display_order = 32)]
    Install {
        /// Print the timer files and the commands, and change nothing
        #[arg(long)]
        print: bool,
    },

    /// Remove the system timer
    #[command(display_order = 33)]
    Uninstall {
        /// Print the commands and the files to remove, and change nothing
        #[arg(long)]
        print: bool,
    },

    /// Print an installation token of a GitHub App, as the command of a secret
    // A repeated option takes its last value.
    #[command(display_order = 34, long_about = APP_TOKEN_ABOUT, args_override_self = true)]
    AppToken(AppTokenArgs),

    /// Run one call of a built-in adapter: JSON on standard input, JSON on standard output
    #[command(hide = true)]
    AdapterScript {
        /// The built-in adapter, such as jira-github
        name: String,
        /// The call: observe, act, or job
        #[arg(value_parser = ["observe", "act", "job"])]
        script: String,
    },
}

const APP_TOKEN_ABOUT: &str = "\
Print an installation token of a GitHub App, and nothing else.

The command makes a JWT signed with RS256 by the openssl program, and asks GitHub for an
installation token. The private key comes from a file (--key-file) or from the standard output
of a shell command (--key-command). A key from a command goes to openssl through a pipe: it
never goes into a file, and it is never printed. A command can print the PEM file as it is, or
as one base64 line, as a keychain item can hold it.

Use it as the command of a secret in config.toml:

  [secrets]
  FORGE_TOKEN = { cmd = \"vision app-token --app-id 123 --installation-id 456 --key-file ~/.keys/app.pem\" }";

/// The arguments of `vision app-token`.
#[derive(Debug, Args)]
#[command(group(ArgGroup::new("key").required(true).args(["key_file", "key_command"])))]
pub struct AppTokenArgs {
    /// The app id (or the client id) of the GitHub App
    #[arg(long, value_name = "ID")]
    pub app_id: String,
    /// The id of the installation
    #[arg(long, value_name = "ID")]
    pub installation_id: i64,
    /// The file of the private key (PEM)
    #[arg(long, value_name = "PATH")]
    pub key_file: Option<String>,
    /// A shell command that prints the private key: the PEM text, or the PEM text as base64
    #[arg(long, value_name = "CMD")]
    pub key_command: Option<String>,
    /// The REST address of GitHub
    #[arg(long, value_name = "URL", default_value = "https://api.github.com")]
    pub api: String,
    /// Seconds for each step: the key command, openssl, and the call to GitHub
    #[arg(long, value_name = "S", default_value_t = 30.0)]
    pub timeout: f64,
}

/// The commands of the pipeline (section 11).
#[derive(Debug, Subcommand)]
pub enum PipelineCommand {
    /// List the eligible tickets, with the reason for each other one
    #[command(display_order = 10)]
    Pick {
        /// Print JSON instead of a table
        #[arg(long)]
        json: bool,
    },

    /// Start one or more tickets
    #[command(display_order = 11)]
    Take {
        /// The ticket keys, such as DEMO-12
        #[arg(required = true, num_args = 1..)]
        keys: Vec<TicketKey>,
    },

    /// Show the plan of a ticket and its hash
    #[command(display_order = 12)]
    Plan {
        /// The ticket key, such as DEMO-12
        key: TicketKey,
    },

    /// Approve the plan that has this hash
    #[command(display_order = 13)]
    ApprovePlan {
        /// The ticket key, such as DEMO-12
        key: TicketKey,
        /// The hash that `vision plan` shows
        hash: String,
    },

    /// Show the timeline of one ticket
    #[command(display_order = 15)]
    Log {
        /// The ticket key, such as DEMO-12
        key: TicketKey,
        /// Print JSON instead of a table
        #[arg(long)]
        json: bool,
    },

    /// Show the commits of the ticket that are not on the forge
    #[command(display_order = 19)]
    Diff {
        /// The ticket key, such as DEMO-12
        key: TicketKey,
        /// Print JSON instead of a table
        #[arg(long)]
        json: bool,
    },

    /// Stop a ticket
    #[command(display_order = 20)]
    Stop {
        /// The ticket key, such as DEMO-12
        key: TicketKey,
    },

    /// Start the step of a ticket again
    #[command(display_order = 21)]
    Retry {
        /// The ticket key, such as DEMO-12
        key: TicketKey,
    },

    /// Show the measures
    #[command(display_order = 31)]
    Report {
        /// Only the last 7 days
        #[arg(long)]
        week: bool,
        /// Print JSON instead of a table
        #[arg(long)]
        json: bool,
    },
}

impl PipelineCommand {
    /// The command as the person types it, such as `approve-plan`.
    pub fn name(&self) -> &'static str {
        match self {
            PipelineCommand::Pick { .. } => "pick",
            PipelineCommand::Take { .. } => "take",
            PipelineCommand::Plan { .. } => "plan",
            PipelineCommand::ApprovePlan { .. } => "approve-plan",
            PipelineCommand::Log { .. } => "log",
            PipelineCommand::Diff { .. } => "diff",
            PipelineCommand::Stop { .. } => "stop",
            PipelineCommand::Retry { .. } => "retry",
            PipelineCommand::Report { .. } => "report",
        }
    }
}

/// The commands of turns (section 7).
#[derive(Debug, Subcommand)]
pub enum TurnCommand {
    /// Supervise one turn. The tick starts it as a detached process
    #[command(hide = true)]
    TurnRun {
        /// The turn id, such as 20261116T143000Z-DEMO-12-build-1
        turn: TurnId,
    },

    /// Show the events of the running turn
    #[command(display_order = 16)]
    Logs {
        /// The ticket key, such as DEMO-12. Without it: each running turn
        key: Option<TicketKey>,
        /// Keep printing new events until Ctrl-C
        #[arg(short, long)]
        follow: bool,
    },

    /// Attach to the running turn and type into it, or take over the session
    #[command(display_order = 17)]
    Open {
        /// The ticket key, such as DEMO-12
        key: TicketKey,
    },

    /// Send a message to the running turn, or to the next turn of the ticket
    #[command(display_order = 18)]
    Tell {
        /// The ticket key, such as DEMO-12
        key: TicketKey,
        /// The message
        text: String,
    },
}

impl TurnCommand {
    /// The command as the person types it, such as `tell`.
    pub fn name(&self) -> &'static str {
        match self {
            TurnCommand::TurnRun { .. } => "turn-run",
            TurnCommand::Logs { .. } => "logs",
            TurnCommand::Open { .. } => "open",
            TurnCommand::Tell { .. } => "tell",
        }
    }
}

/// The commands of questions (section 8.4).
#[derive(Debug, Subcommand)]
pub enum QuestionsCommand {
    /// Show the cards and the approvals that wait, one at a time, and the notices
    #[command(display_order = 22)]
    Needs {
        /// Print the list as JSON instead of the interactive view
        #[arg(long)]
        json: bool,
        /// Mark the notice ID as read, such as N-3, and show nothing else
        #[arg(long, value_name = "ID", conflicts_with = "json")]
        read: Option<String>,
    },

    /// Answer a card, or mark it as needless
    #[command(display_order = 23)]
    Answer(AnswerArgs),

    /// List the answers that the Judge gave alone, newest first
    #[command(display_order = 24)]
    Answers {
        /// Only the answers of the last N days
        #[arg(long, value_name = "N")]
        days: Option<u32>,
        /// Print JSON instead of a table
        #[arg(long)]
        json: bool,
    },

    /// Replace an answer of the Judge with the right answer
    #[command(display_order = 25)]
    Correct {
        /// The id of the card, or of the decision file of the Judge
        id: String,
        /// The right answer
        answer: String,
        /// Why this answer is right
        #[arg(long, value_name = "W")]
        why: Option<String>,
    },
}

impl QuestionsCommand {
    /// The command as the person types it, such as `answers`.
    pub fn name(&self) -> &'static str {
        match self {
            QuestionsCommand::Needs { .. } => "needs",
            QuestionsCommand::Answer(_) => "answer",
            QuestionsCommand::Answers { .. } => "answers",
            QuestionsCommand::Correct { .. } => "correct",
        }
    }
}

/// `vision answer ID --choice N | --text "T" [--why "W"] [--when "C"]`, or `--needless SOURCE`.
#[derive(Debug, Args)]
#[command(group(ArgGroup::new("reply").required(true).args(["choice", "text", "needless"])))]
pub struct AnswerArgs {
    /// The id of the card
    pub id: String,
    /// The number of a choice, as `vision needs` shows it
    #[arg(long, value_name = "N")]
    pub choice: Option<u32>,
    /// An answer in your own words
    #[arg(long, value_name = "T")]
    pub text: Option<String>,
    /// The card did not need you: name where the answer already was
    #[arg(long, value_enum, value_name = "SOURCE")]
    pub needless: Option<Needless>,
    /// Why this answer is right
    #[arg(long, value_name = "W", conflicts_with = "needless")]
    pub why: Option<String>,
    /// Where the decision applies
    #[arg(long, value_name = "C", conflicts_with = "needless")]
    pub when: Option<String>,
}

/// Where the answer to a needless card already was.
#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub enum Needless {
    /// The code had the answer
    Code,
    /// The ticket had the answer
    Ticket,
    /// The memory had the answer
    Memory,
    /// Each choice is fine
    NoMatter,
}

/// `vision memory [QUERY] [--kind K] [--area A] [--status S]`, or one of its subcommands (section 9.2).
#[derive(Debug, Args)]
#[command(args_conflicts_with_subcommands = true)]
pub struct MemoryArgs {
    #[command(subcommand)]
    pub command: Option<MemoryCommand>,
    /// Words to search for in the decisions and the principles
    pub query: Option<String>,
    /// Only files of this kind
    #[arg(long, value_enum, value_name = "K")]
    pub kind: Option<MemoryKind>,
    /// Only files of this area, such as exports
    #[arg(long, value_name = "A")]
    pub area: Option<String>,
    /// Only files with this status
    #[arg(long, value_enum, value_name = "S")]
    pub status: Option<MemoryStatus>,
    /// Print JSON instead of a table
    #[arg(long)]
    pub json: bool,
}

impl MemoryArgs {
    /// The command as the person types it, such as `memory teach`.
    pub fn name(&self) -> &'static str {
        match &self.command {
            None => "memory",
            Some(MemoryCommand::Show { .. }) => "memory show",
            Some(MemoryCommand::Edit { .. }) => "memory edit",
            Some(MemoryCommand::Teach { .. }) => "memory teach",
            Some(MemoryCommand::Retire { .. }) => "memory retire",
            Some(MemoryCommand::Lint { .. }) => "memory lint",
            Some(MemoryCommand::Import { .. }) => "memory import",
            Some(MemoryCommand::Review(_)) => "memory review",
            Some(MemoryCommand::Log { .. }) => "memory log",
            Some(MemoryCommand::Revert { .. }) => "memory revert",
        }
    }
}

#[derive(Debug, Subcommand)]
pub enum MemoryCommand {
    /// Show one file, with its evidence and its uses
    Show {
        /// The id, such as D-0142 or P-007
        id: String,
        /// Print JSON instead of text
        #[arg(long)]
        json: bool,
    },

    /// Open a file in $EDITOR, lint it, and commit
    Edit {
        /// The id, such as D-0142 or P-007
        id: String,
    },

    /// Write a principle directly. It is active at once
    Teach {
        /// The agent that loads it, or "all"
        agent: String,
        /// The rule, such as "When X, make sure that Y, because Z"
        text: String,
    },

    /// Retire a file. It stays, and no turn loads it
    Retire {
        /// The id, such as D-0142 or P-007
        id: String,
        /// Why it no longer applies
        why: String,
    },

    /// Test the format of each file
    Lint {
        /// Print JSON instead of text
        #[arg(long)]
        json: bool,
    },

    /// Read past answers from a JSONL file as decisions
    Import {
        /// The JSONL file
        file: PathBuf,
    },

    /// Accept, edit, or reject each change of a proposal
    Review(ReviewArgs),

    /// Show the history of the memory
    Log {
        /// Print JSON instead of a table
        #[arg(long)]
        json: bool,
    },

    /// Make a commit that undoes one commit
    Revert {
        /// The commit to undo
        commit: String,
    },
}

/// `vision memory review [--json] [--proposal NAME] [--accept N|all [--answer TEXT] | --reject N|all
/// [--why TEXT]]`.
/// Without `--accept` and `--reject`, a terminal gets the interactive review.
#[derive(Debug, Args)]
pub struct ReviewArgs {
    /// Print the open proposals as JSON, and change nothing
    #[arg(long)]
    pub json: bool,
    /// The proposal, such as proposal/2026-11-16-1. Without it: each open proposal
    #[arg(long, value_name = "NAME")]
    pub proposal: Option<String>,
    /// Accept change N of the proposal, or each open change with `all`
    #[arg(long, value_name = "N|all", value_parser = parse_pick, conflicts_with_all = ["reject", "json"])]
    pub accept: Option<Pick>,
    /// Reject change N of the proposal, or each open change with `all`
    #[arg(long, value_name = "N|all", value_parser = parse_pick, conflicts_with = "json")]
    pub reject: Option<Pick>,
    /// Why you reject it. The next run of the Curator reads it
    #[arg(long, value_name = "TEXT", requires = "reject")]
    pub why: Option<String>,
    /// Your answer to a question of the Curator, with --accept N. The next run of the Curator
    /// reads it. When the question names one decision with no reason, the answer becomes it
    #[arg(long, value_name = "TEXT", requires = "accept", conflicts_with_all = ["reject", "json"])]
    pub answer: Option<String>,
}

/// One change of a proposal by its number, or each open change.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Pick {
    All,
    /// The number of the change, from 1.
    Change(usize),
}

fn parse_pick(text: &str) -> Result<Pick, String> {
    let text = text.trim();
    if text.eq_ignore_ascii_case("all") {
        return Ok(Pick::All);
    }
    match text.parse::<usize>() {
        Ok(n) if n > 0 => Ok(Pick::Change(n)),
        _ => Err(format!(
            "`{text}` is not the number of a change (from 1) or `all`"
        )),
    }
}

/// The kind of a decision or a principle.
#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub enum MemoryKind {
    Product,
    Architecture,
    Scope,
    Code,
}

/// The status of a decision or a principle.
#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub enum MemoryStatus {
    Active,
    Candidate,
    Superseded,
    Retired,
}

/// `vision model`, or one of its subcommands (section 10).
#[derive(Debug, Args)]
#[command(args_conflicts_with_subcommands = true)]
pub struct ModelArgs {
    #[command(subcommand)]
    pub command: Option<ModelCommand>,
    /// Print JSON instead of a table
    #[arg(long)]
    pub json: bool,
}

impl ModelArgs {
    /// The command as the person types it, such as `model set`.
    pub fn name(&self) -> &'static str {
        match &self.command {
            None => "model",
            Some(ModelCommand::Set { .. }) => "model set",
            Some(ModelCommand::Reset { .. }) => "model reset",
            Some(ModelCommand::Recommend { .. }) => "model recommend",
            Some(ModelCommand::Eval { .. }) => "model eval",
        }
    }
}

#[derive(Debug, Subcommand)]
pub enum ModelCommand {
    /// Pin the model of an agent. The value stays until you change it
    Set {
        /// The agent, such as builder or judge
        agent: String,
        /// A full model id, or one of fable, opus, sonnet, haiku
        model: String,
        /// The effort level
        #[arg(long, value_enum, value_name = "E")]
        effort: Option<Effort>,
    },

    /// Go back to the default model of Vision
    Reset {
        /// The agent, such as builder or judge
        agent: String,
    },

    /// Show the evidence for each model that an agent used
    Recommend {
        /// The agent, such as builder or judge. Without it: each agent
        agent: Option<String>,
        /// Print JSON instead of a table
        #[arg(long)]
        json: bool,
    },

    /// Replay past work with each model and compare
    Eval {
        /// The agent, such as builder or judge
        agent: String,
        /// The models to compare, such as opus,sonnet
        #[arg(long, value_delimiter = ',', required = true, num_args = 1.., value_name = "A,B")]
        models: Vec<String>,
        /// The number of past items to replay
        #[arg(long, value_name = "N")]
        n: Option<u32>,
        /// Print JSON instead of a table
        #[arg(long)]
        json: bool,
    },
}

/// The commands of the executor (section 12).
#[derive(Debug, Subcommand)]
pub enum ExecutorCommand {
    /// Approve one operation that waits in the mode ask
    #[command(display_order = 14)]
    Approve {
        /// The ticket key, such as DEMO-12
        key: TicketKey,
        /// The verb, such as git.push
        verb: String,
        /// The commit that the operation acts on
        commit: String,
        /// Approve only the operation with this id, and nothing if it no longer waits
        #[arg(long, value_name = "ID")]
        op: Option<String>,
    },

    /// Move a verb one step up the trust ladder: hold, shadow, ask, auto
    #[command(display_order = 29)]
    Promote {
        /// The verb, such as git.push
        verb: String,
    },

    /// Move a verb one step down the trust ladder: auto, ask, shadow, hold
    #[command(display_order = 30)]
    Demote {
        /// The verb, such as git.push
        verb: String,
    },
}

impl ExecutorCommand {
    /// The command as the person types it, such as `promote`.
    pub fn name(&self) -> &'static str {
        match self {
            ExecutorCommand::Approve { .. } => "approve",
            ExecutorCommand::Promote { .. } => "promote",
            ExecutorCommand::Demote { .. } => "demote",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    #[test]
    fn the_tree_is_valid() {
        Cli::command().debug_assert();
    }

    fn parse(args: &[&str]) -> Result<Cli, clap::Error> {
        Cli::try_parse_from(std::iter::once("vision").chain(args.iter().copied()))
    }

    #[test]
    fn every_command_of_section_13_parses() {
        let lines: &[&[&str]] = &[
            &["init"],
            &["config", "--json"],
            &["status", "--json"],
            &["watch"],
            &["prompt"],
            &["tick"],
            &["on"],
            &["off"],
            &["doctor", "--json"],
            &["pick"],
            &["take", "DEMO-12", "DEMO-13"],
            &["plan", "DEMO-12"],
            &["approve-plan", "DEMO-12", "ab12"],
            &["approve", "DEMO-12", "git.push", "9f3e21c"],
            &["log", "DEMO-12"],
            &["logs"],
            &["logs", "DEMO-12", "--follow"],
            &["open", "DEMO-12"],
            &["tell", "DEMO-12", "use the old export"],
            &["diff", "DEMO-12"],
            &["stop", "DEMO-12"],
            &["retry", "DEMO-12"],
            &["needs"],
            &["needs", "--json"],
            &["needs", "--read", "N-3"],
            &[
                "answer", "Q-1", "--choice", "2", "--why", "W", "--when", "C",
            ],
            &["answer", "Q-1", "--text", "CSV"],
            &["answer", "Q-1", "--needless", "no-matter"],
            &["answers", "--days", "7"],
            &["correct", "Q-1", "XLSX", "--why", "W"],
            &["memory"],
            &[
                "memory", "export", "--kind", "product", "--area", "exports", "--status", "active",
            ],
            &["memory", "show", "D-0142"],
            &["memory", "edit", "P-007"],
            &[
                "memory",
                "teach",
                "judge",
                "When X, make sure that Y, because Z",
            ],
            &["memory", "retire", "P-007", "no longer true"],
            &["memory", "lint"],
            &["memory", "import", "answers.jsonl"],
            &["memory", "review"],
            &["memory", "review", "--json"],
            &[
                "memory",
                "review",
                "--proposal",
                "proposal/2026-11-16-1",
                "--accept",
                "all",
            ],
            &["memory", "review", "--accept", "2"],
            &["memory", "review", "--reject", "3", "--why", "too wide"],
            &["memory", "review", "--reject", "all"],
            &["memory", "log"],
            &["memory", "revert", "abc123"],
            &["curate"],
            &["model"],
            &["model", "set", "judge", "opus", "--effort", "xhigh"],
            &["model", "reset", "judge"],
            &["model", "recommend"],
            &[
                "model",
                "eval",
                "judge",
                "--models",
                "opus,sonnet",
                "--n",
                "5",
            ],
            &["promote", "git.push"],
            &["demote", "git.push"],
            &["report", "--week"],
            &["install", "--print"],
            &["uninstall"],
            &["turn-run", "20261116T143000Z-DEMO-12-build-1"],
        ];
        for line in lines {
            if let Err(e) = parse(line) {
                panic!("vision {} must parse: {e}", line.join(" "));
            }
        }
    }

    #[test]
    fn bad_lines_are_refused() {
        let lines: &[&[&str]] = &[
            &["take"],
            &["take", "../x"],
            &["answer", "Q-1"],
            &["answer", "Q-1", "--choice", "1", "--text", "T"],
            &["answer", "Q-1", "--needless", "code", "--why", "W"],
            &["answer", "Q-1", "--needless", "maybe"],
            &["model", "set", "judge", "opus", "--effort", "huge"],
            &["model", "eval", "judge"],
            &["turn-run", "build-1"],
            &["memory", "show", "D-1", "--kind", "code"],
            &["memory", "review", "--accept", "0"],
            &["memory", "review", "--accept", "some"],
            &["memory", "review", "--why", "W"],
            &["memory", "review", "--accept", "1", "--reject", "2"],
            &["memory", "review", "--json", "--accept", "1"],
        ];
        for line in lines {
            assert!(
                parse(line).is_err(),
                "vision {} must be refused",
                line.join(" ")
            );
        }
    }

    #[test]
    fn subcommands_win_over_the_memory_query() {
        let cli = parse(&["memory", "lint"]).unwrap();
        let Command::Memory(args) = cli.command else {
            panic!("not memory")
        };
        assert_eq!(args.name(), "memory lint");
        let cli = parse(&["memory", "exports"]).unwrap();
        let Command::Memory(args) = cli.command else {
            panic!("not memory")
        };
        assert_eq!(args.query.as_deref(), Some("exports"));
    }

    #[test]
    fn review_takes_a_number_or_all() {
        let cli = parse(&[
            "memory",
            "review",
            "--accept",
            "ALL",
            "--proposal",
            "proposal/x",
        ])
        .unwrap();
        let Command::Memory(MemoryArgs {
            command: Some(MemoryCommand::Review(args)),
            ..
        }) = cli.command
        else {
            panic!("not memory review")
        };
        assert_eq!(args.accept, Some(Pick::All));
        assert_eq!(args.proposal.as_deref(), Some("proposal/x"));
        let cli = parse(&["memory", "review", "--reject", "3", "--why", "W"]).unwrap();
        let Command::Memory(MemoryArgs {
            command: Some(MemoryCommand::Review(args)),
            ..
        }) = cli.command
        else {
            panic!("not memory review")
        };
        assert_eq!(args.reject, Some(Pick::Change(3)));
        assert_eq!(args.why.as_deref(), Some("W"));
    }

    #[test]
    fn app_token_is_visible_and_adapter_script_is_hidden() {
        let cli = parse(&[
            "app-token",
            "--app-id",
            "123",
            "--installation-id",
            "456",
            "--key-command",
            "cat key.pem",
        ])
        .unwrap();
        let Command::AppToken(args) = cli.command else {
            panic!("app-token")
        };
        assert_eq!(
            (args.app_id.as_str(), args.installation_id, args.timeout),
            ("123", 456, 30.0)
        );
        assert_eq!(args.api, "https://api.github.com");
        assert!(parse(&["app-token", "--app-id", "1", "--installation-id", "2"]).is_err());
        assert!(parse(&[
            "app-token",
            "--app-id",
            "1",
            "--installation-id",
            "2",
            "--key-file",
            "a",
            "--key-command",
            "b"
        ])
        .is_err());
        assert!(matches!(
            parse(&["adapter-script", "jira-github", "act"])
                .unwrap()
                .command,
            Command::AdapterScript { .. }
        ));
        assert!(parse(&["adapter-script", "jira-github", "other"]).is_err());
        let help = Cli::command().render_help().to_string();
        assert!(help.contains("app-token"), "{help}");
        assert!(!help.contains("adapter-script"), "{help}");
    }

    #[test]
    fn turn_run_is_hidden() {
        let help = Cli::command().render_help().to_string();
        assert!(!help.contains("turn-run"));
        assert!(help.contains("approve-plan"));
    }
}
