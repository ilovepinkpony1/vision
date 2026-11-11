//! The runtime: the loop of messages and the effects. Background work runs in threads that send
//! messages to the loop over one channel: each load in a thread of its own, the worker in one
//! thread with a queue, and the keys in one thread. The loop never blocks on the ledger, on the
//! tracker, or on a child process.
//!
//! The runtime sets the time of the model before each message. [`Runtime`] needs no terminal,
//! so a test can drive the real loaders and the real worker with it; [`run_terminal`] adds the
//! terminal and draws.

use std::io;
use std::panic::AssertUnwindSafe;
use std::sync::mpsc::{self, Receiver, Sender};
use std::time::{Duration, Instant};

use anyhow::Result;
use ratatui::backend::CrosstermBackend;
use ratatui::Terminal;
use time::OffsetDateTime;

use super::model::{App, Clock};
use super::msg::{ChildResult, Effect, Job, JobDone, Msg};
use super::view::theme::ColorMode;
use super::worker::{self, WorkerConfig};
use super::{load, update, view};
use crate::paths::Paths;

/// The wait for a message while a part of the screen moves.
pub const FAST: Duration = Duration::from_millis(100);
/// The wait for a message while nothing moves.
pub const SLOW: Duration = Duration::from_millis(1000);

/// The model, the channel, and the threads.
pub struct Runtime {
    pub app: App,
    paths: Paths,
    worker: WorkerConfig,
    tx: Sender<Msg>,
    rx: Receiver<Msg>,
    jobs: Option<Sender<Job>>,
    started: Instant,
}

/// How the loop ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Exit {
    Normal,
    /// The process got this signal.
    Signal(i32),
}

impl Runtime {
    pub fn new(paths: Paths, worker: WorkerConfig, size: (u16, u16), colors: ColorMode) -> Runtime {
        let (tx, rx) = mpsc::channel();
        let started = Instant::now();
        let app = App::new(clock(started), size, colors);
        Runtime {
            app,
            paths,
            worker,
            tx,
            rx,
            jobs: None,
            started,
        }
    }

    /// A sender for the threads of the terminal.
    pub fn sender(&self) -> Sender<Msg> {
        self.tx.clone()
    }

    /// Starts the loader.
    pub fn start(&mut self) {
        self.app.clock = clock(self.started);
        let effects = update::start(&mut self.app);
        self.run(effects);
    }

    /// Hands one message to the update, and runs its effects.
    pub fn handle(&mut self, msg: Msg) {
        self.app.clock = clock(self.started);
        let effects = update::update(&mut self.app, msg);
        self.run(effects);
    }

    /// Waits for the next message up to `timeout`.
    pub fn wait(&self, timeout: Duration) -> Option<Msg> {
        self.rx.recv_timeout(timeout).ok()
    }

    /// A message that is already there, with no wait.
    pub fn next_now(&self) -> Option<Msg> {
        self.rx.try_recv().ok()
    }

    /// Handles messages, and a tick when none comes, until `done` holds or `limit` passed.
    /// Gives true when `done` held. For tests and tools with no terminal.
    pub fn run_until(&mut self, limit: Duration, mut done: impl FnMut(&App) -> bool) -> bool {
        let until = Instant::now() + limit;
        while Instant::now() < until {
            if done(&self.app) {
                return true;
            }
            let msg = self.wait(FAST).unwrap_or(Msg::Tick);
            self.handle(msg);
        }
        done(&self.app)
    }

    fn run(&mut self, effects: Vec<Effect>) {
        for effect in effects {
            match effect {
                Effect::LoadSetup => {
                    let paths = self.paths.clone();
                    self.spawn(
                        move || Msg::Setup(load::setup(&paths)),
                        |error| {
                            Msg::Setup(Err(super::model::SetupError {
                                reason: "the read failed".into(),
                                what: error,
                                fix: "Read log/app.err of the home folder, then open Vision again."
                                    .into(),
                            }))
                        },
                    );
                }
                Effect::LoadLocal => {
                    let paths = self.paths.clone();
                    let wall = self.app.clock.wall;
                    self.spawn(
                        move || Msg::Local(load::local(&paths, wall)),
                        |e| Msg::Local(Err(e)),
                    );
                }
                Effect::LoadQueue => {
                    let paths = self.paths.clone();
                    self.spawn(
                        move || Msg::Queue(load::queue(&paths)),
                        |e| Msg::Queue(Err(e)),
                    );
                }
                Effect::LoadStartInfo(key) => {
                    let paths = self.paths.clone();
                    let failed = key.clone();
                    self.spawn(
                        move || Msg::StartInfo {
                            info: load::start_info(&paths, &key),
                            key,
                        },
                        move |e| Msg::StartInfo {
                            key: failed.clone(),
                            info: Err(e),
                        },
                    );
                }
                Effect::StartWorker => {
                    if self.jobs.is_none() {
                        self.jobs = worker::spawn(self.worker.clone(), self.tx.clone()).ok();
                    }
                }
                Effect::Run(job) => self.run_job(job),
                Effect::Quit => self.app.quit = true,
            }
        }
    }

    fn run_job(&mut self, job: Job) {
        if self.jobs.is_none() {
            self.jobs = worker::spawn(self.worker.clone(), self.tx.clone()).ok();
        }
        let sent = match &self.jobs {
            Some(jobs) => jobs.send(job.clone()).is_ok(),
            None => false,
        };
        if !sent {
            let _ = self.tx.send(Msg::JobDone(JobDone {
                id: job.id,
                kind: job.kind,
                result: ChildResult {
                    error: Some(
                        "the worker thread is gone, so no job runs. Open Vision again".into(),
                    ),
                    ..ChildResult::default()
                },
            }));
        }
    }

    /// Runs `work` in a thread and sends its message. A panic sends the message of `failed`.
    fn spawn<W, F>(&self, work: W, failed: F)
    where
        W: FnOnce() -> Msg + Send + 'static,
        F: Fn(String) -> Msg + Send + Sync + 'static,
    {
        let tx = self.tx.clone();
        let failed = std::sync::Arc::new(failed);
        let in_thread = failed.clone();
        let started = std::thread::Builder::new()
            .name("vision-load".into())
            .spawn(move || {
                let msg = std::panic::catch_unwind(AssertUnwindSafe(work)).unwrap_or_else(|_| {
                    in_thread(
                        "the read stopped with an internal error. log/app.err has the details"
                            .into(),
                    )
                });
                let _ = tx.send(msg);
            });
        if let Err(e) = started {
            let _ = self.tx.send(failed(format!(
                "could not start a thread for the read: {e}"
            )));
        }
    }
}

/// The time now: the monotonic time since `started`, the wall clock, and the local offset.
pub fn clock(started: Instant) -> Clock {
    let wall = OffsetDateTime::now_utc();
    Clock {
        elapsed: started.elapsed(),
        wall,
        offset_seconds: local_offset(wall.unix_timestamp()),
    }
}

/// The offset of the local time zone at a time, in seconds, from the C library.
fn local_offset(at: i64) -> i32 {
    let t: libc::time_t = at as libc::time_t;
    // SAFETY: localtime_r writes only into the local value that it gets.
    unsafe {
        let mut tm: libc::tm = std::mem::zeroed();
        if libc::localtime_r(&t, &mut tm).is_null() {
            return 0;
        }
        tm.tm_gmtoff as i32
    }
}

/// The loop with a terminal: draws when something changed or something moves, and waits for a
/// message up to 100 ms while something moves, else up to one second.
pub fn run_terminal(
    rt: &mut Runtime,
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
) -> Result<Exit> {
    rt.start();
    let mut minute = None;
    loop {
        if rt.app.quit {
            return Ok(Exit::Normal);
        }
        let now_minute = rt.app.clock.local_hm();
        if rt.app.dirty || rt.app.animating() || minute != Some(now_minute) {
            terminal.draw(|frame| view::draw(&rt.app, frame))?;
            rt.app.dirty = false;
            minute = Some(now_minute);
        }
        let timeout = if rt.app.animating() { FAST } else { SLOW };
        let mut next = rt.wait(timeout);
        loop {
            let msg = next.take().unwrap_or(Msg::Tick);
            if let Msg::Signal(signal) = msg {
                return Ok(Exit::Signal(signal));
            }
            rt.handle(msg);
            match rt.next_now() {
                Some(msg) => next = Some(msg),
                None => break,
            }
        }
    }
}
