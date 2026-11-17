//! A child that gets the terminal: `vision open KEY` after the open card of the live view, which
//! pauses the ticket and starts the interactive session of its last turn, or `vision memory
//! review --proposal NAME` after the card of a proposal of the Curator. The app gives the
//! terminal to the child, waits for it, and takes the terminal again.
//!
//! Three things hold while the child owns the terminal:
//!
//! - The thread of the keys of the app reads nothing. The runtime asks it to pause and waits
//!   for its confirmation before the child starts.
//! - A Ctrl-C ends only the child: the app ignores SIGINT in that time, and the child gets the
//!   default action again before it runs.
//! - The child gets the normal modes of the terminal, with each mode of the mouse off, and the
//!   terminal is right again afterwards, also after a child that crashed: the app writes back the
//!   terminal settings of before, resets the attributes, turns off each mode of the mouse that
//!   the child left, turns on its own modes, and draws the whole screen again.

use std::io::{self, Write};
use std::os::unix::process::CommandExt;
use std::process::Command;
use std::time::Duration;

use super::msg::ChildResult;
use super::stderr;
use super::terminal::{self, InputControl};
use super::worker::WorkerConfig;
use crate::paths::HOME_ENV;

/// The longest wait for the thread of the keys to confirm its pause.
pub const PAUSE_LIMIT: Duration = Duration::from_secs(3);

/// The terminal as a child gets it and gives it back: the terminal of the app, or a fake in a
/// test that records what each step writes.
pub trait Handover {
    /// Gives the terminal to the child: the normal modes, with each mode of the mouse off.
    fn give(&mut self);
    /// Takes the terminal again after the child: the modes of the app, with the mouse on.
    fn take(&mut self) -> io::Result<()>;
}

/// The terminal of the app on standard input and standard output.
#[derive(Default)]
pub struct Real {
    /// The settings of the terminal while the child owns it, to write back after it.
    settings: Option<Settings>,
}

impl Handover for Real {
    fn give(&mut self) {
        terminal::leave();
        self.settings = Settings::read();
    }

    fn take(&mut self) -> io::Result<()> {
        // A child that crashed can leave the terminal in raw mode, with no echo, or with odd
        // attributes: the settings of before come back first.
        if let Some(settings) = &self.settings {
            settings.write();
        }
        // What the person typed for the session and the session did not read is no key of the
        // app.
        // SAFETY: drops the input that waits on the terminal of standard input.
        unsafe {
            libc::tcflush(0, libc::TCIFLUSH);
        }
        let mut out = io::stdout();
        let _ = write!(out, "\x1b[0m");
        let _ = out.flush();
        // The modes of the app turn off each mode of the mouse that the child left first.
        terminal::take()
    }
}

/// Runs the child with these arguments with the terminal, and gives the terminal back to the
/// app.
pub fn open(config: &WorkerConfig, args: &[String], input: &InputControl) -> ChildResult {
    open_with(&mut Real::default(), config, args, input)
}

/// [`open`] with the terminal of `handover`.
pub fn open_with(
    handover: &mut impl Handover,
    config: &WorkerConfig,
    args: &[String],
    input: &InputControl,
) -> ChildResult {
    if !input.pause(PAUSE_LIMIT) {
        input.resume();
        return ChildResult {
            error: Some(
                "the keys of the app did not stop in time, so the session did not open. Try again"
                    .into(),
            ),
            ..ChildResult::default()
        };
    }
    handover.give();
    let result = {
        let _sigint = terminal::ignore_sigint();
        let _stderr = stderr::lend();
        run(config, args)
    };
    let taken = handover.take();
    input.resume();
    match taken {
        Ok(()) => result,
        Err(e) => ChildResult {
            error: Some(format!("the terminal did not come back: {e}")),
            ..result
        },
    }
}

/// The child with the terminal as its standard input and output.
fn run(config: &WorkerConfig, args: &[String]) -> ChildResult {
    let mut command = Command::new(&config.exe);
    command.args(args);
    if let Some(env) = &config.env {
        command.env_clear().envs(env.iter().map(|(k, v)| (k, v)));
    }
    command.env(HOME_ENV, &config.home);
    // SAFETY: signal is async-signal-safe; the child gets the default action of SIGINT again,
    // because an ignored signal stays ignored across exec.
    unsafe {
        command.pre_exec(|| {
            libc::signal(libc::SIGINT, libc::SIG_DFL);
            Ok(())
        });
    }
    match command.status() {
        Ok(status) => ChildResult {
            code: status.code(),
            ..ChildResult::default()
        },
        Err(e) => ChildResult {
            error: Some(format!(
                "could not start {} {}: {e}",
                config.exe.display(),
                args.join(" ")
            )),
            ..ChildResult::default()
        },
    }
}

/// The settings of the terminal (termios) of standard input.
struct Settings {
    termios: libc::termios,
}

impl Settings {
    fn read() -> Option<Settings> {
        // SAFETY: tcgetattr writes into a local value.
        unsafe {
            let mut termios: libc::termios = std::mem::zeroed();
            (libc::tcgetattr(0, &mut termios) == 0).then_some(Settings { termios })
        }
    }

    fn write(&self) {
        // SAFETY: writes back settings that tcgetattr gave for the same descriptor.
        unsafe {
            libc::tcsetattr(0, libc::TCSANOW, &self.termios);
        }
    }
}
