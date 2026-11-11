use std::io::IsTerminal;
use std::process::ExitCode;

use clap::Parser;

use vision::cli::Cli;
use vision::exit;

fn main() -> ExitCode {
    // `vision` with no arguments opens the app when both ends are a terminal. In each other
    // case it prints the help, as before.
    if std::env::args_os().len() == 1
        && std::io::stdin().is_terminal()
        && std::io::stdout().is_terminal()
    {
        return match vision::app::run() {
            Ok(()) => ExitCode::from(exit::DONE),
            Err(e) => exit::report(&e),
        };
    }
    let cli = match Cli::try_parse() {
        Ok(cli) => cli,
        Err(e) => {
            // clap exits with 2 on a usage error, but 2 means "a gate refused" here (section 14).
            let _ = e.print();
            return if e.use_stderr() {
                ExitCode::from(exit::ERROR)
            } else {
                ExitCode::from(exit::DONE)
            };
        }
    };
    match vision::run(cli) {
        Ok(()) => ExitCode::from(exit::DONE),
        Err(e) => exit::report(&e),
    }
}
