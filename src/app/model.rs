//! The model of the app: plain data that the update changes and the view draws. It holds no
//! handle, no thread, and no clock. The runtime sets the time in [`Clock`] before each message,
//! so that each screen and each key can be tested without a terminal.

use std::collections::VecDeque;
use std::time::Duration;

use time::OffsetDateTime;

use super::msg::Job;
use super::view::theme::ColorMode;
use crate::pipeline::commands::PickRow;
use crate::pipeline::TicketView;
use crate::snapshot::{LimitsView, Need};

/// The smallest window that the app draws in.
pub const MIN_WIDTH: u16 = 100;
pub const MIN_HEIGHT: u16 = 28;

/// The stages of the loader, in their order.
pub const STAGE_HOME: usize = 0;
pub const STAGE_LEDGER: usize = 1;
pub const STAGE_TRACKER: usize = 2;
pub const STAGE_ENGINE: usize = 3;

/// The time as the runtime gives it.
#[derive(Clone, Debug, PartialEq)]
pub struct Clock {
    /// The time since the app started, from a monotonic clock.
    pub elapsed: Duration,
    /// The time of the wall clock, in UTC.
    pub wall: OffsetDateTime,
    /// The offset of the local time from UTC, in seconds.
    pub offset_seconds: i32,
}

impl Clock {
    pub fn at(elapsed: Duration, wall: OffsetDateTime) -> Clock {
        Clock {
            elapsed,
            wall,
            offset_seconds: 0,
        }
    }

    /// The local time as hours and minutes.
    pub fn local_hm(&self) -> (u8, u8) {
        let local = self.wall + time::Duration::seconds(i64::from(self.offset_seconds));
        (local.hour(), local.minute())
    }

    /// The seconds since the start, as a float, for the animations.
    pub fn seconds(&self) -> f64 {
        self.elapsed.as_secs_f64()
    }
}

/// The screen under the cards.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Screen {
    /// The loader.
    Boot,
    Home,
    /// The plain detail screen of one ticket in progress. Part A2 replaces it with the live view.
    Detail {
        key: String,
    },
}

/// A card over the screen.
#[derive(Clone, Debug, PartialEq)]
pub enum Card {
    Start(StartCard),
    Resume(ResumeCard),
    Kill(KillCard),
    Quit,
    Help,
    Needs,
    /// A long error text. Esc closes it.
    Error {
        title: String,
        text: String,
    },
}

/// The state of the action of a card.
#[derive(Clone, Debug, PartialEq)]
pub enum Run {
    Idle,
    /// The action waits in the queue of the worker behind the job that runs now.
    Waiting {
        job: u64,
    },
    Running {
        job: u64,
    },
    Failed(String),
}

impl Run {
    /// The job of the action while it waits or runs.
    pub fn job(&self) -> Option<u64> {
        match self {
            Run::Waiting { job } | Run::Running { job } => Some(*job),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct StartCard {
    pub key: String,
    pub title: String,
    /// `None` while it loads.
    pub info: Option<Result<StartInfo, String>>,
    pub run: Run,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ResumeCard {
    pub key: String,
    pub title: String,
    pub step: String,
    pub clone: Option<String>,
    pub branch: Option<String>,
    pub run: Run,
}

#[derive(Clone, Debug, PartialEq)]
pub struct KillCard {
    /// True when Vision is off, so the card offers `vision on`.
    pub turn_on: bool,
    pub run: Run,
}

/// What the start card says about a ticket.
#[derive(Clone, Debug, PartialEq)]
pub struct StartInfo {
    /// The clone that the take will use, with `~` for the home folder, or why none is free.
    pub clone: Result<String, String>,
    pub branch: String,
    pub base: String,
    /// The step ids of the adapter, in order.
    pub steps: Vec<String>,
    /// The first step: its id, and for a turn its agent, model, and effort.
    pub first: FirstStep,
    /// One sentence about the writes of the adapter in this home.
    pub writes: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct FirstStep {
    pub step: String,
    /// The kind of the step: turn, job, human, wait, or end.
    pub kind: String,
    pub agent: Option<String>,
    pub model: Option<String>,
    pub effort: Option<String>,
}

/// The loader.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Boot {
    pub stages: [Stage; 4],
    /// The person pressed a key: the loader leaves as soon as the home folder is read.
    pub skip: bool,
    /// The home folder is not set up, or its configuration cannot be read.
    pub blocked: Option<SetupError>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Stage {
    pub state: StageState,
    /// When the stage started.
    pub since: Option<Duration>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub enum StageState {
    #[default]
    Waiting,
    Running,
    Done(String),
    Failed(String),
}

impl Stage {
    pub fn settled(&self) -> bool {
        matches!(self.state, StageState::Done(_) | StageState::Failed(_))
    }
}

/// The home folder, as the first stage reads it.
#[derive(Clone, Debug, PartialEq)]
pub struct Setup {
    /// The home folder, with `~` for the folder of the person.
    pub home: String,
    pub adapter: String,
    /// The step ids of the adapter, in order.
    pub steps: Vec<String>,
}

/// Why the home folder cannot be used.
#[derive(Clone, Debug, PartialEq)]
pub struct SetupError {
    /// A short reason for the stage line.
    pub reason: String,
    /// What is missing or wrong, in full.
    pub what: String,
    /// What to do.
    pub fix: String,
}

/// The local data: it loads again about each second.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Local {
    /// True when the `off` file is present.
    pub off: bool,
    pub limits: LimitsView,
    pub needs: Vec<Need>,
    /// The tickets that Vision holds and that are not finished.
    pub tickets: Vec<Held>,
    /// The number of turns that run.
    pub turns: usize,
    /// The agents of the work turns that run now.
    pub agents: Vec<String>,
    pub week: Week,
    /// True when the system timer of this home is installed.
    pub timer: bool,
}

/// One ticket in progress.
#[derive(Clone, Debug, PartialEq)]
pub struct Held {
    pub view: TicketView,
    /// When Vision took it, RFC 3339 UTC.
    pub taken: Option<String>,
    pub stopped: bool,
    /// True when the ticket waits for the person.
    pub needs_you: bool,
}

/// The numbers of the last 7 days.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Week {
    pub cost_usd: f64,
    /// The tickets whose end step ran in the window.
    pub finished: usize,
    /// The questions that the memory or the Judge answered without the person.
    pub without_you: usize,
    /// Each question with an outcome.
    pub questions: usize,
}

/// A value that loads in a thread, again and again.
#[derive(Clone, Debug, PartialEq)]
pub struct Fetch<T> {
    /// The last value that loaded.
    pub data: Option<T>,
    /// The error of the last load, when it failed.
    pub error: Option<String>,
    /// When the load that runs now started.
    pub since: Option<Duration>,
    /// When the last load ended.
    pub at: Option<Duration>,
    /// Load again as soon as the load that runs now ends.
    pub again: bool,
}

impl<T> Default for Fetch<T> {
    fn default() -> Self {
        Fetch {
            data: None,
            error: None,
            since: None,
            at: None,
            again: false,
        }
    }
}

impl<T> Fetch<T> {
    pub fn loading(&self) -> bool {
        self.since.is_some()
    }
}

/// The selection of Home: a ticket key, so that a refresh never moves it to another ticket.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct HomeState {
    pub selected: Option<String>,
    /// Select this key as soon as it shows, such as a ticket that was just taken.
    pub select_next: Option<String>,
}

/// The engine: the queue of the worker and its state.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Engine {
    pub started: bool,
    /// No new pass starts. Actions still run. It lives in the memory of the app only.
    pub paused: bool,
    pub queue: VecDeque<Job>,
    pub running: Option<RunningJob>,
    pub last_pass_start: Option<Duration>,
    pub last_pass: Option<PassEnd>,
    pub next_id: u64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct RunningJob {
    pub job: Job,
    pub since: Duration,
}

/// How the last pass ended.
#[derive(Clone, Debug, PartialEq)]
pub enum PassEnd {
    Done,
    /// Another tick held the lock, such as the tick of the system timer. Not an error.
    Skipped,
    Failed(String),
}

/// One line at the bottom that goes away after a few seconds.
#[derive(Clone, Debug, PartialEq)]
pub struct Notice {
    pub text: String,
    pub tone: Tone,
    pub until: Duration,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tone {
    Ok,
    Info,
    Bad,
}

/// One row of a box of Home, for the selection.
#[derive(Clone, Debug, PartialEq)]
pub enum Item<'a> {
    Held(&'a Held),
    Queue(&'a PickRow),
}

impl Item<'_> {
    pub fn key(&self) -> &str {
        match self {
            Item::Held(held) => &held.view.key,
            Item::Queue(row) => &row.key,
        }
    }
}

/// The whole model.
#[derive(Clone, Debug, PartialEq)]
pub struct App {
    pub clock: Clock,
    pub size: (u16, u16),
    pub colors: ColorMode,
    pub screen: Screen,
    pub card: Option<Card>,
    pub boot: Boot,
    pub setup: Option<Setup>,
    pub local: Fetch<Local>,
    pub queue: Fetch<Vec<PickRow>>,
    pub home: HomeState,
    pub engine: Engine,
    pub notice: Option<Notice>,
    /// The app leaves.
    pub quit: bool,
    /// Something that the view shows changed since the last draw.
    pub dirty: bool,
}

impl App {
    pub fn new(clock: Clock, size: (u16, u16), colors: ColorMode) -> App {
        App {
            clock,
            size,
            colors,
            screen: Screen::Boot,
            card: None,
            boot: Boot::default(),
            setup: None,
            local: Fetch::default(),
            queue: Fetch::default(),
            home: HomeState::default(),
            engine: Engine::default(),
            notice: None,
            quit: false,
            dirty: true,
        }
    }

    /// The tickets in progress, or none before the first load.
    pub fn held(&self) -> &[Held] {
        self.local
            .data
            .as_ref()
            .map_or(&[], |l| l.tickets.as_slice())
    }

    pub fn held_ticket(&self, key: &str) -> Option<&Held> {
        self.held().iter().find(|h| h.view.key == key)
    }

    /// The title of a ticket in progress: the one of the ledger, else the one of the queue of
    /// the tracker. A ticket that was taken before the ledger kept titles has only the second.
    pub fn title_of<'a>(&'a self, held: &'a Held) -> &'a str {
        if let Some(title) = held.view.title.as_deref().filter(|t| !t.is_empty()) {
            return title;
        }
        self.queue
            .data
            .as_ref()
            .and_then(|rows| rows.iter().find(|r| r.key == held.view.key))
            .map_or("", |r| r.title.as_str())
    }

    /// The rows of the queue that are not in progress.
    pub fn queue_rows(&self) -> Vec<&PickRow> {
        let Some(rows) = &self.queue.data else {
            return Vec::new();
        };
        rows.iter()
            .filter(|r| r.taken.is_none() && self.held_ticket(&r.key).is_none())
            .collect()
    }

    /// The rows of both boxes of Home, in order.
    pub fn items(&self) -> Vec<Item<'_>> {
        let mut items: Vec<Item> = self.held().iter().map(Item::Held).collect();
        items.extend(self.queue_rows().into_iter().map(Item::Queue));
        items
    }

    /// The index of the selected row in [`App::items`].
    pub fn selected_index(&self) -> Option<usize> {
        let key = self.home.selected.as_deref()?;
        self.items().iter().position(|i| i.key() == key)
    }

    /// The needs of the local data.
    pub fn needs(&self) -> &[Need] {
        self.local.data.as_ref().map_or(&[], |l| l.needs.as_slice())
    }

    /// True when Vision is off by the kill switch.
    pub fn off(&self) -> bool {
        self.local.data.as_ref().is_some_and(|l| l.off)
    }

    /// True when a part of the screen moves, so that the runtime draws often.
    pub fn animating(&self) -> bool {
        if self.screen == Screen::Boot {
            return true;
        }
        if self.engine.running.is_some() || self.queue.loading() {
            return true;
        }
        match &self.card {
            Some(Card::Start(card)) => card.info.is_none() || card.run.job().is_some(),
            Some(Card::Resume(card)) => card.run.job().is_some(),
            Some(Card::Kill(card)) => card.run.job().is_some(),
            _ => false,
        }
    }

    /// True when the window is big enough for the app.
    pub fn fits(&self) -> bool {
        self.size.0 >= MIN_WIDTH && self.size.1 >= MIN_HEIGHT
    }
}
