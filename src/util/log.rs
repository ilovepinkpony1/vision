//! The log: one line for each tick action in `$VISION_HOME/log/vision.log`.

use std::fs::OpenOptions;
use std::io::Write;
use std::path::PathBuf;

use crate::paths::Paths;
use crate::util::time;

/// Appends lines to the log file. Each line is `<RFC 3339 time> [<pid>] <message>`.
#[derive(Clone, Debug)]
pub struct Logger {
    file: PathBuf,
}

impl Logger {
    pub fn new(paths: &Paths) -> Logger {
        Logger {
            file: paths.log_file(),
        }
    }

    /// Appends one line. A failure prints a warning to standard error and is otherwise ignored,
    /// because a log line must never stop a command.
    pub fn line(&self, message: &str) {
        if let Err(e) = self.try_line(message) {
            eprintln!(
                "warning: could not write the log {}: {e}",
                self.file.display()
            );
        }
    }

    /// Appends one line, or gives the error.
    pub fn try_line(&self, message: &str) -> std::io::Result<()> {
        let message = message.replace(['\n', '\r'], " ");
        let line = format!(
            "{} [{}] {}\n",
            time::now_rfc3339(),
            std::process::id(),
            message
        );
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.file)?;
        file.write_all(line.as_bytes())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn appends_one_line_for_each_message() {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::new(dir.path());
        std::fs::create_dir_all(paths.log_dir()).unwrap();
        let log = Logger::new(&paths);
        log.line("first");
        log.line("second\nline");
        let text = std::fs::read_to_string(paths.log_file()).unwrap();
        let lines: Vec<_> = text.lines().collect();
        assert_eq!(lines.len(), 2);
        assert!(lines[0].ends_with("] first"));
        assert!(lines[1].ends_with("] second line"));
    }
}
