//! Questions and the Judge (section 8): cards, the Judge, answers, and decisions.
//!
//! Not built yet. A later milestone fills [`run`] and [`needs`].

use anyhow::Result;

use crate::cli::QuestionsCommand;
use crate::ctx::Ctx;
use crate::exit::not_built;
use crate::ledger::Ledger;
use crate::snapshot::Need;

/// `vision needs`, `answer`, `answers`, and `correct`.
pub fn run(_ctx: &Ctx, command: QuestionsCommand) -> Result<()> {
    Err(not_built(command.name()))
}

/// The cards that wait for the person. The tick puts them in the snapshot, and
/// `vision status` and `vision prompt` count them.
pub fn needs(_ctx: &Ctx, _ledger: &Ledger) -> Result<Vec<Need>> {
    Ok(Vec::new())
}
