//! Turns (section 7): the start line, the supervisor, the stream parser, the contract, the
//! inbox, and the collection of ended turns.
//!
//! Not built yet. A later milestone fills [`run`], [`collect_ended_turns`], and [`start_turn`].

use std::fmt;

use anyhow::Result;
use serde::{Deserialize, Serialize};

use crate::cli::TurnCommand;
use crate::ctx::Ctx;
use crate::exit::not_built;
use crate::tick::{Pass, TurnStart};

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
pub fn run(_ctx: &Ctx, command: TurnCommand) -> Result<()> {
    Err(not_built(command.name()))
}

/// Tick step 4: for each turn that ended, write its row in `turns`, its `result.json`, and its
/// limit reading. A turn whose `turn.json` says `running` with a dead pid ends with the reason `crash`.
pub fn collect_ended_turns(_pass: &mut Pass) -> Result<()> {
    Ok(())
}

/// Tick step 7 for [`crate::tick::Action::StartTurn`]: when the limits allow a new turn, write
/// its folder and `facts.json`, and start `vision turn-run <turn-id>` as a detached process.
pub fn start_turn(_pass: &mut Pass, _start: &TurnStart) -> Result<()> {
    Ok(())
}
