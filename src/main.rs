use std::process::ExitCode;

use clap::Parser;

use vision::cli::Cli;
use vision::exit;

fn main() -> ExitCode {
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
