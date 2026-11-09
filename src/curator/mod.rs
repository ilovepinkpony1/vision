//! The Curator (section 9.4): signals, proposals, replay, and review.
//!
//! Not built yet. A later milestone fills [`run`].

use anyhow::Result;

use crate::ctx::Ctx;
use crate::exit::not_built;

/// `vision curate`: run the Curator now.
pub fn run(_ctx: &Ctx) -> Result<()> {
    Err(not_built("curate"))
}
