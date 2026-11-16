//! The worker: one thread with a queue that runs one job at a time. A job is a pass (the child
//! `vision tick`) or an action (a command of today, such as `vision take KEY`). Each job runs
//! as a child process of the same binary with no standard input; the worker keeps its standard
//! output and its standard error, and sends the result to the loop. A crash of a child cannot
//! take the app down, and no text of a child reaches the screen unless the app shows it.
//!
//! An action that reads a file, such as the send of a question form, gets it from the worker:
//! the worker writes the file in the home folder, runs the child in that folder, and removes
//! the file after the child ended.

use std::ffi::OsString;
use std::path::{Component, Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc::{self, Sender};

use super::msg::{ChildResult, Job, JobDone, JobKind, Msg};
use crate::paths::HOME_ENV;
use crate::util::fs::{atomic_write, ensure_dir};

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
                let result = std::panic::catch_unwind(|| run_job(&config, &job.kind))
                    .unwrap_or_else(|_| ChildResult {
                        error: Some("the worker stopped with an internal error".into()),
                        ..ChildResult::default()
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

/// Runs the child of one job to its end. A job with a file to read gets it first, and runs in
/// the home folder.
pub fn run_job(config: &WorkerConfig, kind: &JobKind) -> ChildResult {
    let args = kind.args();
    let Some((path, text)) = kind.input() else {
        return run_child(config, &args);
    };
    let failed = |error: String| ChildResult {
        error: Some(error),
        ..ChildResult::default()
    };
    // The path comes from the model: only a plain path inside the home folder is written.
    let inside = Path::new(&path)
        .components()
        .all(|c| matches!(c, Component::Normal(_)));
    if !inside {
        return failed(format!("the file {path} is not inside the home folder"));
    }
    let file = config.home.join(&path);
    let written = file
        .parent()
        .map_or(Ok(()), |dir| ensure_dir(dir).map(|_| ()))
        .and_then(|()| atomic_write(&file, text.as_bytes()));
    if let Err(e) = written {
        return failed(format!("could not write {}: {e:#}", file.display()));
    }
    let result = run_child_in(config, &args, Some(&config.home));
    let _ = std::fs::remove_file(&file);
    result
}

/// Runs one child to its end.
pub fn run_child(config: &WorkerConfig, args: &[String]) -> ChildResult {
    run_child_in(config, args, None)
}

/// Runs one child to its end, in the folder `dir` when it is given.
fn run_child_in(config: &WorkerConfig, args: &[String], dir: Option<&Path>) -> ChildResult {
    let mut command = Command::new(&config.exe);
    command
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Some(dir) = dir {
        command.current_dir(dir);
    }
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
