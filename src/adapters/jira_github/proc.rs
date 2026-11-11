//! Child processes with a time limit: git, the commands of the project, and openssl.

use std::io::{Read, Write};
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::process::{Command, ExitStatus, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

/// The wait for the pipes after a kill.
const GRACE: Duration = Duration::from_secs(2);

/// What a child gave.
#[derive(Clone, Debug, Default)]
pub struct Output {
    /// The exit code, or minus the signal that ended the child. `None` when it did not end.
    pub code: Option<i32>,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
    pub timed_out: bool,
    /// Whole seconds from the start to the end.
    pub seconds: u64,
}

impl Output {
    pub fn success(&self) -> bool {
        self.code == Some(0) && !self.timed_out
    }

    pub fn stdout_text(&self) -> String {
        String::from_utf8_lossy(&self.stdout).into_owned()
    }

    pub fn stderr_text(&self) -> String {
        String::from_utf8_lossy(&self.stderr).into_owned()
    }
}

/// How [`run`] starts a child.
#[derive(Clone, Copy, Debug, Default)]
pub struct How {
    /// The child gets its own session and process group, and a time limit kills the group.
    pub session: bool,
    /// Standard error goes into standard output, in the order of the writes.
    pub merge: bool,
}

/// The exit code of a status, or minus its signal.
pub fn code_of(status: ExitStatus) -> i32 {
    status
        .code()
        .unwrap_or_else(|| -status.signal().unwrap_or(0))
}

/// Starts the child of a command in its own session.
pub fn new_session(command: &mut Command) {
    // SAFETY: setsid only changes the session of the new child, and it is safe to call between
    // fork and exec.
    unsafe {
        command.pre_exec(|| {
            libc::setsid();
            Ok(())
        });
    }
}

/// Sends a signal to a process group. A gone group is no error.
pub fn kill_group(pgid: u32, signal: i32) {
    if let Ok(pgid) = libc::pid_t::try_from(pgid) {
        if pgid > 1 {
            // SAFETY: killpg only sends a signal to the processes of this group.
            unsafe {
                libc::killpg(pgid, signal);
            }
        }
    }
}

fn read_all(mut pipe: impl Read + Send + 'static) -> mpsc::Receiver<Vec<u8>> {
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let mut data = Vec::new();
        let _ = pipe.read_to_end(&mut data);
        let _ = tx.send(data);
    });
    rx
}

/// Runs a command with `input` on standard input (or none), and waits at most `timeout`. At
/// the time limit it kills the child (or its group), and the output has `timed_out`.
pub fn run(
    mut command: Command,
    input: Option<Vec<u8>>,
    timeout: Option<Duration>,
    how: How,
) -> std::io::Result<Output> {
    if how.session {
        new_session(&mut command);
    }
    command.stdin(if input.is_some() {
        Stdio::piped()
    } else {
        Stdio::null()
    });
    let mut merged = None;
    if how.merge {
        let (reader, writer) = std::io::pipe()?;
        command.stdout(writer.try_clone()?).stderr(writer);
        merged = Some(reader);
    } else {
        command.stdout(Stdio::piped()).stderr(Stdio::piped());
    }
    let started = Instant::now();
    let mut child = command.spawn()?;
    // The parent drops its ends of the pipes, so that the reads end with the child.
    drop(command);
    if let (Some(mut stdin), Some(input)) = (child.stdin.take(), input) {
        thread::spawn(move || {
            let _ = stdin.write_all(&input);
        });
    }
    let out_rx = match merged {
        Some(reader) => read_all(reader),
        None => match child.stdout.take() {
            Some(stdout) => read_all(stdout),
            None => read_all(std::io::empty()),
        },
    };
    let err_rx = match child.stderr.take() {
        Some(stderr) => read_all(stderr),
        None => read_all(std::io::empty()),
    };
    let deadline = timeout.map(|t| started + t);
    let mut timed_out = false;
    let mut pause = Duration::from_millis(2);
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break Some(status);
        }
        if let Some(deadline) = deadline {
            let now = Instant::now();
            if now >= deadline {
                if how.session {
                    kill_group(child.id(), libc::SIGKILL);
                }
                let _ = child.kill();
                timed_out = true;
                break child.wait().ok();
            }
            thread::sleep(pause.min(deadline - now));
        } else {
            thread::sleep(pause);
        }
        pause = (pause * 2).min(Duration::from_millis(50));
    };
    // The output ends when each process that holds the pipes ends. A child that left a process
    // with the pipes open counts as a time limit too.
    let mut collect = |rx: &mpsc::Receiver<Vec<u8>>| -> Vec<u8> {
        if !timed_out {
            let got = match deadline {
                Some(deadline) => {
                    rx.recv_timeout(deadline.saturating_duration_since(Instant::now()))
                }
                None => rx.recv().map_err(|_| mpsc::RecvTimeoutError::Disconnected),
            };
            match got {
                Ok(data) => return data,
                Err(mpsc::RecvTimeoutError::Disconnected) => return Vec::new(),
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    if how.session {
                        kill_group(child.id(), libc::SIGKILL);
                    }
                    timed_out = true;
                }
            }
        }
        rx.recv_timeout(GRACE).unwrap_or_default()
    };
    let stdout = collect(&out_rx);
    let stderr = collect(&err_rx);
    Ok(Output {
        code: status.map(code_of),
        stdout,
        stderr,
        timed_out,
        seconds: started.elapsed().as_secs(),
    })
}

/// A new folder in the temporary folder (`TMPDIR`) that only this user can read. The folder and
/// each file in it go when the value goes.
#[derive(Debug)]
pub struct TempDir {
    pub path: std::path::PathBuf,
}

impl TempDir {
    pub fn new(prefix: &str) -> std::io::Result<TempDir> {
        use std::os::unix::fs::DirBuilderExt;
        let tmp = std::env::temp_dir();
        for _ in 0..100 {
            let path = tmp.join(format!("{prefix}{}", random_part()));
            match std::fs::DirBuilder::new().mode(0o700).create(&path) {
                Ok(()) => return Ok(TempDir { path }),
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(e) => return Err(e),
            }
        }
        Err(std::io::Error::other(
            "no free name in the temporary folder",
        ))
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

/// A new file in the temporary folder that only this user can read and write. Its name has a
/// random part, so that no other user of a shared temporary folder can put a link at that name
/// first.
pub fn temp_file(
    prefix: &str,
    suffix: &str,
) -> std::io::Result<(std::fs::File, std::path::PathBuf)> {
    use std::os::unix::fs::OpenOptionsExt;
    let tmp = std::env::temp_dir();
    for _ in 0..100 {
        let path = tmp.join(format!("{prefix}{}{suffix}", random_part()));
        match std::fs::OpenOptions::new()
            .append(true)
            .create_new(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW)
            .open(&path)
        {
            Ok(file) => return Ok((file, path)),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e),
        }
    }
    Err(std::io::Error::other(
        "no free name in the temporary folder",
    ))
}

fn random_part() -> String {
    uuid::Uuid::new_v4().simple().to_string()[..8].to_string()
}
