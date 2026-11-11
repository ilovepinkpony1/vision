//! Turns (section 7): the start line, the supervisor, the stream parser, the contract, the
//! inbox, and the collection of ended turns.
//!
//! The library API that later milestones build on:
//!
//! - [`launch`] starts a turn from a [`LaunchRequest`], or refuses it with a [`Refusal`] that
//!   names the rule. [`refusal`] tests the rules alone. [`resume`] resumes the session of an
//!   ended turn with a message.
//! - [`outcome`] reads the outcome of a turn: status, reason, result object, session, cost,
//!   usage, and limits.
//! - [`running_turns`], [`kill_turn`], and [`kill_all`] control the running turns, and
//!   [`tell`] sends a message to one.
//! - [`collect_ended_turns`] and [`start_turn`] are the tick hooks.

mod collect;
mod control;
mod inbox;
mod launch;
mod roles;
mod schema;
mod settings;
mod state;
mod stream;
mod supervisor;
mod view;

use std::fmt;

use anyhow::Result;
use serde::{Deserialize, Serialize};

use crate::cli::TurnCommand;
use crate::ctx::Ctx;
use crate::tick::{Pass, TurnStart};

pub use collect::{collect, outcome, Collected, TurnOutcome, TurnStatus, CONTINUE};
pub use control::{kill_all, kill_turn, running_turn_of, running_turns, tell, RunningTurn, Told};
pub use inbox::MessageFile;
pub use launch::{
    first_message, launch, prepare, refusal, request_of, resume, set_vision_bin, start_supervisor,
    turn_paths, vision_bin, Contract, LaunchFile, LaunchRequest, Launched, Plan, Refusal, ViewFile,
    TURN_PLACEHOLDER,
};
pub use roles::{agents_dir, agents_file, compose_role, embedded_names, schema_name};
pub use schema::problems as schema_problems;
pub use settings::{gitconfig_path, identity, permission_mode, session_env, tools, ALLOWED_ENV};
pub use state::{Phase, Reason, TurnState};
pub use stream::{
    parse_value as parse_stream_value, Event as StreamEvent, ResultEvent, ToolCall, Usage,
};
pub use view::{render_event, tool_summary, Tail, PAUSED};

/// The three kinds of turn (section 7.1).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TurnKind {
    Reader,
    Worker,
    Tester,
}

impl TurnKind {
    pub fn as_str(self) -> &'static str {
        match self {
            TurnKind::Reader => "reader",
            TurnKind::Worker => "worker",
            TurnKind::Tester => "tester",
        }
    }
}

impl fmt::Display for TurnKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// `vision turn-run`, `logs`, `open`, and `tell`.
pub fn run(ctx: &Ctx, command: TurnCommand) -> Result<()> {
    match command {
        TurnCommand::TurnRun { turn } => supervisor::run(ctx, &turn),
        TurnCommand::Logs { key, follow } => view::logs(ctx, key.as_ref(), follow),
        TurnCommand::Open { key } => view::open(ctx, &key),
        TurnCommand::Tell { key, text } => control::tell_command(ctx, &key, &text),
    }
}

/// Tick step 4: for each turn that ended, write its row in `turns`, its `result.json`, and its
/// limit reading. A turn whose `turn.json` says it runs with a dead supervisor ends with the
/// reason `crash`. A failed turn gets one automatic resume with `continue`.
pub fn collect_ended_turns(pass: &mut Pass) -> Result<()> {
    collect::collect_ended_turns(pass)
}

/// Tick step 7 for [`crate::tick::Action::StartTurn`]: the pipeline turns the step into a
/// launch request, and [`launch`] tests the rules, writes the turn, and starts its supervisor.
/// A refusal is an error that holds a [`Refusal`].
pub fn start_turn(pass: &mut Pass, start: &TurnStart) -> Result<()> {
    let request = crate::pipeline::launch_request(pass, start)?;
    launch(pass.ctx, &pass.ledger, &request)?;
    Ok(())
}
