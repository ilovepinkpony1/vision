//! The memory (section 9): files, the repo, commands, lint, archive, and import.
//!
//! Not built yet. A later milestone fills [`run`]. `vision init` already makes the repo at
//! `$VISION_HOME/memory` with `decisions/`, `principles/`, and an empty `ARCHIVE.md`.
//! [`crate::util::git`] runs git.

use anyhow::Result;

use crate::cli::MemoryArgs;
use crate::ctx::Ctx;
use crate::exit::not_built;

/// `vision memory` and each of its subcommands.
pub fn run(_ctx: &Ctx, args: MemoryArgs) -> Result<()> {
    Err(not_built(args.name()))
}
