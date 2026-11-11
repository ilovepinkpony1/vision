//! `o` of the live view, after its card: the app gives the terminal to `vision open KEY`, which
//! pauses the ticket and starts the interactive session of its last turn, waits for it, and
//! takes the terminal again.
//!
//! Three things hold while the child owns the terminal:
//!
//! - The thread of the keys of the app reads nothing. The runtime asks it to pause and waits
//!   for its confirmation before the child starts.
//! - A Ctrl-C ends only the child: the app ignores SIGINT in that time, and the child gets the
//!   default action again before it runs.
//! - The terminal is right again afterwards, also after a child that crashed: the app writes
//!   back the terminal settings of before, resets the attributes and the mouse, and draws the
//!   whole screen again.

use std::io::Write;
use std::os::unix::process::CommandExt;
use std::process::Command;
use std::time::Duration;

use ratatui::crossterm::event::DisableMouseCapture;
use ratatui::crossterm::execute;

use super::msg::ChildResult;
use super::stderr;
use super::terminal::{self, InputControl};
use super::worker::WorkerConfig;
use crate::paths::HOME_ENV;

/// The longest wait for the thread of the keys to confirm its pause.
pub const PAUSE_LIMIT: Duration = Duration::from_secs(3);

/// Runs `vision open KEY` with the terminal, and gives the terminal back to the app.
pub fn open(config: &WorkerConfig, key: &str, input: &InputControl) -> ChildResult {
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
    terminal::leave();
    let settings = Settings::read();
    let result = {
        let _sigint = terminal::ignore_sigint();
        let _stderr = stderr::lend();
        run(config, key)
    };
    // A child that crashed can leave the terminal in raw mode, with no echo, or with odd
    // attributes: the settings of before come back first.
    if let Some(settings) = &settings {
        settings.write();
    }
    // What the person typed for the session and the session did not read is no key of the app.
    // SAFETY: drops the input that waits on the terminal of standard input.
    unsafe {
        libc::tcflush(0, libc::TCIFLUSH);
    }
    let mut out = std::io::stdout();
    let _ = write!(out, "\x1b[0m");
    let _ = execute!(out, DisableMouseCapture);
    let _ = out.flush();
    let taken = terminal::take();
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
fn run(config: &WorkerConfig, key: &str) -> ChildResult {
    let mut command = Command::new(&config.exe);
    command.args(["open", key]);
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
                "could not start {} open {key}: {e}",
                config.exe.display()
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
