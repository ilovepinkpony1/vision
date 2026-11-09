//! The model of each agent (section 10).
//!
//! Not built yet. A later milestone fills [`run`]. It writes `[agents.<agent>]` with
//! [`crate::config::ConfigEditor::set_agent`] and removes it with
//! [`crate::config::ConfigEditor::remove_agent`].

use anyhow::Result;

use crate::cli::ModelArgs;
use crate::ctx::Ctx;
use crate::exit::not_built;

/// `vision model`, `model set`, `model reset`, `model recommend`, and `model eval`.
pub fn run(_ctx: &Ctx, args: ModelArgs) -> Result<()> {
    Err(not_built(args.name()))
}
