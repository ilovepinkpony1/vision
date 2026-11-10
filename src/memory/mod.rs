//! The memory (section 9): files, the repo, commands, lint, archive, and import.
//!
//! The library API that later milestones build on:
//!
//! - [`Memory`] is the store of the repo at `$VISION_HOME/memory`. It reads the files
//!   ([`Memory::get`], [`Memory::list`] with a [`Filter`], [`Memory::source_state`],
//!   [`Memory::is_active`]), writes them as one commit each ([`Memory::add_decision`],
//!   [`Memory::supersede`], [`Memory::add_principle`], [`Memory::set_status`],
//!   [`Memory::retire`], [`Memory::count_uses`], [`Memory::import_jsonl`]), gives the
//!   principles of an agent ([`Memory::principles_for`], [`Memory::principles_file`]), and the
//!   history ([`Memory::log`], [`Memory::revert`]).
//! - The proposals of the Curator: [`Memory::build_proposal`], [`Memory::proposals`],
//!   [`Memory::proposal`], [`Memory::edit_change`], [`Memory::accept_change`],
//!   [`Memory::accept_all`], [`Memory::reject_change`], [`Memory::reject_all`], and
//!   [`Memory::delete_proposal`].
//! - The pure parts: [`doc::Doc`] (the file format), [`lint::lint`], and
//!   [`archive::lost_lines`] (the rule "no line is lost").
//!
//! `vision init` makes the repo with `decisions/`, `principles/`, and an empty `ARCHIVE.md`.

pub mod archive;
mod commands;
pub mod doc;
pub mod front;
pub mod import;
pub mod lint;
pub mod model;
pub mod proposal;
pub mod store;

pub use commands::{run, split_because};
pub use import::{ImportReport, Skipped};
pub use lint::LintReport;
pub use model::{
    Basis, Decision, DecisionId, Kind, MemoryFile, MemoryId, NewDecision, NewPrinciple, Principle,
    PrincipleId, Problem, Status, UseKind, Uses, Writer,
};
pub use proposal::{
    Accepted, Built, ChangeState, ChangeView, CuratorChange, FileDiff, Op, ProposalChange,
    ProposalFile, ProposalInfo, ProposalView, SkippedChange,
};
pub use store::{
    render_principles, CommitInfo, FileType, Filter, HandEdits, Invalid, Listing, Loaded, Memory,
    PrinciplesFile, SourceState,
};
