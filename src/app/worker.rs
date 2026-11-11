//! The worker: one thread with a queue that runs one job at a time. A job is a pass (the child
//! `vision tick`) or an action (a command of today, such as `vision take KEY`). Each job runs
//! as a child process of the same binary with no standard input; the worker keeps its standard
//! output and its standard error, and sends the result to the loop. A crash of a child cannot
//! take the app down, and no text of a child reaches the screen unless the app shows it.

use std::ffi::OsString;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::mpsc::{self, Sender};

use super::msg::{ChildResult, Job, JobDone, Msg};
use crate::paths::HOME_ENV;

/// How the worker starts a child.
#[derive(Clone, Debug)]
pub struct WorkerConfig {
    /// The binary: `std::env::current_exe()` in the real program, the built binary or a fake
    /// script in a test.
    pub exe: PathBuf,
    /// The home of Vision. Each child gets it as `VISION_HOME`.
    pub home: PathBuf,
    /// With a value, the child gets only these variables (and `VISION_HOME`). Without one, it
    /// gets the environment of the app.
    pub env: Option<Vec<(OsString, OsString)>>,
}

/// Starts the worker thread. Gives the sender of its queue.
pub fn spawn(config: WorkerConfig, tx: Sender<Msg>) -> std::io::Result<Sender<Job>> {
    let (jobs, queue) = mpsc::channel::<Job>();
    std::thread::Builder::new()
        .name("vision-worker".into())
        .spawn(move || {
            for job in queue {
                let args = job.kind.args();
                let result =
                    std::panic::catch_unwind(|| run_child(&config, &args)).unwrap_or_else(|_| {
                        ChildResult {
                            error: Some("the worker stopped with an internal error".into()),
                            ..ChildResult::default()
                        }
                    });
                let done = JobDone {
                    id: job.id,
                    kind: job.kind,
                    result,
                };
                if tx.send(Msg::JobDone(done)).is_err() {
                    return;
                }
            }
        })?;
    Ok(jobs)
}

/// Runs one child to its end.
pub fn run_child(config: &WorkerConfig, args: &[String]) -> ChildResult {
    let mut command = Command::new(&config.exe);
    command
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Some(env) = &config.env {
        command.env_clear().envs(env.iter().map(|(k, v)| (k, v)));
    }
    command.env(HOME_ENV, &config.home);
    match command.output() {
        Ok(output) => ChildResult {
            code: output.status.code(),
            stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
            error: None,
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
