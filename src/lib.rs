//! Vision takes a ticket from a tracker to a pull request that is ready to merge.
//!
//! The specification is `docs/design.md`. This crate is one binary with a library: `main.rs`
//! parses the command line and calls [`run`].

pub mod adapter;
pub mod app;
pub mod cli;
pub mod config;
pub mod ctx;
pub mod curator;
pub mod doctor;
pub mod executor;
pub mod exit;
pub mod init;
pub mod install;
pub mod ledger;
pub mod lock;
pub mod memory;
pub mod models;
pub mod paths;
pub mod pipeline;
pub mod questions;
pub mod running;
pub mod snapshot;
pub mod status;
pub mod tick;
pub mod turn;
pub mod util;

use anyhow::Result;

use cli::{Cli, Command};
use ctx::Ctx;

/// Runs one parsed command line.
pub fn run(cli: Cli) -> Result<()> {
    let ctx = Ctx::from_env();
    if let Command::Prompt = cli.command {
        // The shell prompt must never fail loudly, even without a home.
        status::prompt(ctx.ok().as_ref());
        return Ok(());
    }
    dispatch(&ctx?, cli.command)
}

fn dispatch(ctx: &Ctx, command: Command) -> Result<()> {
    match command {
        Command::Init => init::run(ctx),
        Command::Config { json } => config::show(ctx, json),
        Command::Status { json } => status::status(ctx, json),
        Command::Watch => status::watch(ctx),
        Command::Prompt => {
            status::prompt(Some(ctx));
            Ok(())
        }
        Command::Tick => tick::command(ctx),
        Command::On => status::switch(ctx, false),
        Command::Off => status::switch(ctx, true),
        Command::Doctor { json } => doctor::run(ctx, json),
        Command::Pipeline(command) => pipeline::run(ctx, command),
        Command::Turn(command) => turn::run(ctx, command),
        Command::Questions(command) => questions::run(ctx, command),
        Command::Memory(args) => memory::run(ctx, args),
        Command::Curate => curator::run(ctx),
        Command::Model(args) => models::run(ctx, args),
        Command::Executor(command) => executor::run(ctx, command),
        Command::Install { print } => install::install(ctx, print),
        Command::Uninstall { print } => install::uninstall(ctx, print),
    }
}
