//! The exit codes of section 14, and the errors that choose a code other than 1.

use std::process::ExitCode;

pub const DONE: u8 = 0;
pub const ERROR: u8 = 1;
pub const GATE_REFUSED: u8 = 2;
pub const NOT_BUILT: u8 = 3;

/// An error that ends a command with its own exit code or message.
#[derive(Debug, thiserror::Error)]
pub enum Stop {
    /// The command belongs to a later milestone. Exit code 3.
    #[error("not built yet: {0}")]
    NotBuilt(String),
    /// A gate of the executor refused the call. Exit code 2.
    #[error("a gate refused: {0}")]
    GateRefused(String),
    /// The command already printed what went wrong. Exit code 1 with no more text.
    #[error("{0}")]
    Reported(String),
}

/// The error of a command that is not built yet, such as `not_built("memory show")`.
pub fn not_built(name: &str) -> anyhow::Error {
    Stop::NotBuilt(name.to_string()).into()
}

/// The exit code for an error.
pub fn code(err: &anyhow::Error) -> u8 {
    match err.downcast_ref::<Stop>() {
        Some(Stop::NotBuilt(_)) => NOT_BUILT,
        Some(Stop::GateRefused(_)) => GATE_REFUSED,
        Some(Stop::Reported(_)) | None => ERROR,
    }
}

/// Prints an error to standard error and gives its exit code.
pub fn report(err: &anyhow::Error) -> ExitCode {
    match err.downcast_ref::<Stop>() {
        Some(Stop::NotBuilt(name)) => eprintln!("not built yet: {name}"),
        Some(Stop::Reported(_)) => {}
        Some(Stop::GateRefused(_)) | None => eprintln!("error: {err:#}"),
    }
    ExitCode::from(code(err))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codes_follow_section_14() {
        assert_eq!(code(&not_built("pick")), NOT_BUILT);
        assert_eq!(
            code(&Stop::GateRefused("diff too big".into()).into()),
            GATE_REFUSED
        );
        assert_eq!(code(&anyhow::anyhow!("boom")), ERROR);
        assert_eq!(code(&Stop::Reported("doctor".into()).into()), ERROR);
        assert_eq!(DONE, 0);
    }
}
