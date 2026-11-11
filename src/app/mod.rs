//! The app: `vision` with no arguments in a terminal opens a full-screen program. A loader
//! with an android shows while the data loads; Home lists the tickets in progress and the
//! queue of the tracker; the app is the engine while its window is open.
//!
//! The code has three layers that stay apart:
//!
//! - [`model`]: plain data, [`model::App`].
//! - [`update`]: a function that takes a [`msg::Msg`] (a key, a tick of the clock, a result of
//!   a background thread), changes the model, and gives the [`msg::Effect`]s to run.
//! - [`view`]: a function from the model to a ratatui frame.
//!
//! The update and the view do no input or output, read no clock, and read no file, so each
//! screen can be tested without a terminal. [`runtime`] runs the effects: the reads in
//! threads ([`load`]), and each change of the state through the command of today as a child
//! process of the same binary ([`worker`]).

pub mod feed;
pub mod line;
pub mod live;
pub mod load;
pub mod load_ticket;
pub mod model;
pub mod moments;
pub mod msg;
pub mod now;
pub mod pager;
pub mod pages;
pub mod runtime;
pub mod session;
pub mod stderr;
pub mod tail;
pub mod terminal;
pub mod text;
pub mod update;
pub mod view;
pub mod worker;

use anyhow::{Context, Result};
use ratatui::backend::CrosstermBackend;
use ratatui::Terminal;

use crate::paths::Paths;
use runtime::{Exit, Runtime};
use view::theme::ColorMode;
use worker::WorkerConfig;

/// Opens the app in this terminal, and gives the terminal back when it leaves.
pub fn run() -> Result<()> {
    let paths = Paths::from_env()?;
    let exe = std::env::current_exe().context("could not find the path of this program")?;
    let colors = ColorMode::from_env();
    let redirect = stderr::redirect(&stderr::log_path(&paths))
        .context("could not point standard error at the log file of the app")?;
    terminal::install_panic_hook();
    let session = terminal::Session::enter().context("could not set up the terminal")?;
    let exit = (|| -> Result<Exit> {
        let mut screen = Terminal::new(CrosstermBackend::new(std::io::stdout()))?;
        let size = screen.size()?;
        let worker = WorkerConfig {
            exe,
            home: paths.home().to_path_buf(),
            env: None,
        };
        let mut rt = Runtime::new(paths, worker, (size.width, size.height), colors);
        terminal::watch_signals(rt.sender())?;
        let input = terminal::InputControl::default();
        terminal::watch_input(rt.sender(), input.clone())?;
        runtime::run_terminal(&mut rt, &mut screen, &input)
    })();
    drop(session);
    drop(redirect);
    match exit? {
        Exit::Normal => Ok(()),
        Exit::Signal(signal) => {
            terminal::reraise(signal);
            Ok(())
        }
    }
}
