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
use super::msg::{ChildResult, Effect, Handoff, Job, JobDone, Msg};
use super::session;
use super::tail::{self, Follow};
use super::terminal::InputControl;
use super::view::theme::ColorMode;
use super::worker::{self, WorkerConfig};
use super::{load, load_needs, load_screens, load_ticket, picture, update, view};
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
    /// The reader of the stream files of the ticket of the live view.
    follow: Option<Follow>,
    /// The children that wait for the terminal: only [`run_terminal`] can give it.
    opens: Vec<Handoff>,
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
            follow: None,
            opens: Vec::new(),
        }
    }

    /// The configuration of the children of the worker, for a child that gets the terminal.
    pub fn worker(&self) -> &WorkerConfig {
        &self.worker
    }

    /// The ticket of the reader of stream files, while one runs.
    pub fn following(&self) -> Option<&str> {
        self.follow.as_ref().map(|f| f.key.as_str())
    }

    /// The next child that waits for the terminal.
    pub fn take_open(&mut self) -> Option<Handoff> {
        if self.opens.is_empty() {
            None
        } else {
            Some(self.opens.remove(0))
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
                Effect::LoadTicket(key) => {
                    let paths = self.paths.clone();
                    let failed = key.clone();
                    self.spawn(
                        move || Msg::Ticket {
                            data: Box::new(load_ticket::ticket(&paths, &key)),
                            key,
                        },
                        move |e| Msg::Ticket {
                            key: failed.clone(),
                            data: Box::new(Err(e)),
                        },
                    );
                }
                Effect::Follow(key) => {
                    // Dropping the old reader stops its thread.
                    self.follow =
                        key.and_then(|key| tail::follow(self.paths.clone(), &key, self.tx.clone()));
                }
                Effect::LoadPlan(key) => {
                    let paths = self.paths.clone();
                    let failed = key.clone();
                    self.spawn(
                        move || Msg::Plan {
                            plan: load_ticket::plan(&paths, &key),
                            key,
                        },
                        move |e| Msg::Plan {
                            key: failed.clone(),
                            plan: Err(e),
                        },
                    );
                }
                Effect::LoadDiff(key) => {
                    let paths = self.paths.clone();
                    let failed = key.clone();
                    self.spawn(
                        move || Msg::Diff {
                            diff: load_ticket::diff(&paths, &key),
                            key,
                        },
                        move |e| Msg::Diff {
                            key: failed.clone(),
                            diff: Err(e),
                        },
                    );
                }
                Effect::LoadTurn { key, turn } => {
                    let paths = self.paths.clone();
                    let failed = (key.clone(), turn.clone());
                    self.spawn(
                        move || Msg::TurnEvents {
                            events: load_ticket::turn_events(&paths, &key, &turn),
                            key,
                            turn,
                        },
                        move |e| Msg::TurnEvents {
                            key: failed.0.clone(),
                            turn: failed.1.clone(),
                            events: Err(e),
                        },
                    );
                }
                Effect::OpenSession(key) => self.opens.push(Handoff::Session(key)),
                Effect::OpenReview(name) => self.opens.push(Handoff::Review(name)),
                Effect::LoadNeeds(load) => {
                    let paths = self.paths.clone();
                    self.spawn(
                        move || Msg::Needs {
                            load,
                            items: Box::new(load_needs::needs(&paths)),
                        },
                        move |e| Msg::Needs {
                            load,
                            items: Box::new(Err(e)),
                        },
                    );
                }
                Effect::LoadCheck { chain, check } => {
                    let paths = self.paths.clone();
                    self.spawn(
                        move || Msg::Checked {
                            chain,
                            facts: Box::new(load_needs::check_facts(&paths, &check)),
                        },
                        move |e| Msg::Checked {
                            chain,
                            facts: Box::new(Err(e)),
                        },
                    );
                }
                Effect::LoadModels => {
                    let paths = self.paths.clone();
                    self.spawn(
                        move || Msg::Models(Box::new(load_screens::models(&paths))),
                        |e| Msg::Models(Box::new(Err(e))),
                    );
                }
                Effect::LoadStats { week } => {
                    let paths = self.paths.clone();
                    let wall = self.app.clock.wall;
                    self.spawn(
                        move || Msg::Stats {
                            week,
                            data: Box::new(load_screens::stats(&paths, week, wall)),
                        },
                        move |e| Msg::Stats {
                            week,
                            data: Box::new(Err(e)),
                        },
                    );
                }
                Effect::LoadPicture => {
                    let home = self.paths.home().to_path_buf();
                    self.spawn(
                        move || Msg::Picture(Box::new(picture::read(&home))),
                        |_| Msg::Picture(Box::new(None)),
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
        offset_seconds: crate::util::time::local_offset(wall.unix_timestamp()),
    }
}

/// Clears the screen and forgets what it showed, so that the next draw writes each cell again.
/// Gives the size of the window now. `Terminal::clear` of ratatui asks the terminal for the
/// position of its cursor and fails when no answer comes in time; a resize to the size of now
/// clears the same way with no question.
fn redraw_all(terminal: &mut Terminal<CrosstermBackend<io::Stdout>>) -> io::Result<(u16, u16)> {
    let size = terminal.size()?;
    terminal.resize(ratatui::layout::Rect::new(0, 0, size.width, size.height))?;
    Ok((size.width, size.height))
}

/// The loop with a terminal: draws when something changed or something moves, and waits for a
/// message up to 100 ms while something moves, else up to one second. A session that waits for
/// the terminal runs here, between two messages.
pub fn run_terminal(
    rt: &mut Runtime,
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    input: &InputControl,
) -> Result<Exit> {
    rt.start();
    let mut shown = None;
    loop {
        if rt.app.quit {
            return Ok(Exit::Normal);
        }
        while let Some(handoff) = rt.take_open() {
            let result = session::open(rt.worker(), &handoff.args(), input);
            // The child drew on the terminal: the next draw writes each cell again.
            let (width, height) = redraw_all(terminal)?;
            if (width, height) != rt.app.size {
                rt.handle(Msg::Resize { width, height });
            }
            rt.handle(match handoff {
                Handoff::Session(key) => Msg::SessionEnded { key, result },
                Handoff::Review(proposal) => Msg::ReviewEnded { proposal, result },
            });
        }
        // The clock of the header shows minutes; the live view also shows seconds.
        let (hour, minute) = rt.app.clock.local_hm();
        let second = if rt.app.shows_seconds() {
            rt.app.clock.wall.second()
        } else {
            0
        };
        let now_shown = (hour, minute, second);
        if rt.app.dirty || rt.app.animating() || shown != Some(now_shown) {
            terminal.draw(|frame| view::draw(&rt.app, frame))?;
            rt.app.dirty = false;
            shown = Some(now_shown);
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
