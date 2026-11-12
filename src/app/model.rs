//! The model of the app: plain data that the update changes and the view draws. It holds no
//! handle, no thread, and no clock. The runtime sets the time in [`Clock`] before each message,
//! so that each screen and each key can be tested without a terminal.

use std::collections::VecDeque;
use std::time::Duration;

use time::OffsetDateTime;

use super::chain::Chain;
use super::model_table::ModelsState;
use super::msg::{Job, JobKind};
use super::needs::NeedsCard;
use super::now::{self, Now, Pass};
use super::picture::{Fitted, Picture};
use super::stats::StatsState;
use super::view::theme::ColorMode;
use crate::pipeline::commands::PickRow;
use crate::pipeline::facts::StateFacts;
use crate::pipeline::TicketView;
use crate::running::Running;
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
    /// The live view of one ticket. [`App::live`] holds its state.
    Live {
        key: String,
    },
    /// The model and the effort of each agent. [`App::models`] holds its state.
    Models,
    /// The measures of the ledger. [`App::stats`] holds its state.
    Stats,
}

/// A card over the screen.
#[derive(Clone, Debug, PartialEq)]
pub enum Card {
    Start(StartCard),
    Resume(ResumeCard),
    Kill(KillCard),
    Quit,
    Help,
    /// `n`: the things that wait for the person, one card at a time.
    Need(NeedsCard),
    /// A long error text. Esc closes it.
    Error {
        title: String,
        text: String,
    },
    /// `x` of the live view: what a stop does, and the stop.
    Stop(StopCard),
    /// `r` of the live view: what a retry does, and the retry.
    Retry(RetryCard),
    /// `o` of the live view: what an open of the session does.
    Open(OpenCard),
    /// After the session of `vision open` ended: continue the ticket, or keep it paused.
    Opened(OpenedCard),
}

#[derive(Clone, Debug, PartialEq)]
pub struct StopCard {
    pub key: String,
    pub step: String,
    pub branch: Option<String>,
    /// The agent whose turn runs now.
    pub agent: Option<String>,
    pub run: Run,
}

#[derive(Clone, Debug, PartialEq)]
pub struct RetryCard {
    pub key: String,
    pub step: String,
    /// The agent of the step, for a turn step.
    pub agent: Option<String>,
    /// The person took over the session.
    pub paused: bool,
    /// The turn of the step held its end facts, so the retry runs its writes again.
    pub held: bool,
    /// Why a rule stopped the ticket.
    pub reason: Option<String>,
    pub run: Run,
}

#[derive(Clone, Debug, PartialEq)]
pub struct OpenCard {
    pub key: String,
    /// The agent whose turn runs now: then the card says how to reach it, or how to stop the
    /// turn and take over its session.
    pub running: Option<String>,
    /// The agent of the last turn, whose session opens.
    pub last: Option<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct OpenedCard {
    pub key: String,
    /// The exit code of `vision open`, or `None` when a signal ended it or it did not start.
    pub code: Option<i32>,
    /// Why it did not start, or the last line of its error text.
    pub error: Option<String>,
    pub run: Run,
    /// The agent whose running turn the person stopped to take over its session: the ticket
    /// is stopped and paused, and continuing it runs the take and then the retry.
    pub took_over: Option<String>,
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
    /// The job or the operation that a pass runs now.
    pub running: Option<Running>,
    /// The reset time of the reading of each window that is in force, RFC 3339 UTC: the 5-hour
    /// window, then the 7-day window.
    pub resets: [Option<String>; 2],
}

/// One ticket in progress.
#[derive(Clone, Debug, PartialEq)]
pub struct Held {
    pub view: TicketView,
    /// The facts of its state, for [`now::now`].
    pub facts: StateFacts,
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
    /// The states of the tickets in progress when the last pass started, to see what the pass
    /// changed.
    pub before_pass: Vec<TicketState>,
    /// True from the end of a pass until the local data loaded again.
    pub pass_ended: bool,
    /// The quick passes in a row: passes that started at once because the pass before them
    /// left work ready.
    pub quick: u32,
}

/// The state of one ticket in short: its key, its step, and the title of its state.
pub type TicketState = (String, String, String);

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

/// A color of the theme by its meaning, for a text that the model holds, such as a word of the
/// activity or a column of a pager. The view turns it into a color.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Hue {
    Fg,
    Dim,
    Faint,
    Ok,
    Warn,
    Bad,
    Info,
    Accent,
    Primary,
    /// The color of an agent, such as `builder`.
    Agent(String),
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
    /// The live view, while it is open.
    pub live: Option<super::live::Live>,
    /// The screen Models.
    pub models: ModelsState,
    /// The screen Stats.
    pub stats: StatsState,
    /// The chain that runs, or that ended and still shows on its card.
    pub chain: Option<Chain>,
    /// The picture of the loader: `None` while it loads, `Some(None)` for the android.
    pub picture: Option<Option<Picture>>,
    /// The picture of the loader fit to the room that the window leaves for it. The update
    /// makes it when the picture arrives and when the window changes size, so the view does
    /// not shrink the picture on each frame.
    pub picture_fit: Option<Fitted>,
    /// The last number given to a load of the needs or to a chain.
    pub seq: u64,
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
            live: None,
            models: ModelsState::default(),
            stats: StatsState::default(),
            chain: None,
            picture: None,
            picture_fit: None,
            seq: 0,
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
        let card = match &self.card {
            Some(Card::Start(card)) => card.info.is_none() || card.run.job().is_some(),
            Some(Card::Resume(card)) => card.run.job().is_some(),
            Some(Card::Kill(card)) => card.run.job().is_some(),
            Some(Card::Stop(card)) => card.run.job().is_some(),
            Some(Card::Retry(card)) => card.run.job().is_some(),
            Some(Card::Opened(card)) => card.run.job().is_some(),
            Some(Card::Need(card)) => card.items.is_none(),
            _ => false,
        };
        let screen = match self.screen {
            Screen::Models => self.models.data.loading(),
            Screen::Stats => self.stats.data.loading(),
            _ => false,
        };
        let chain = self.chain.as_ref().is_some_and(Chain::running);
        card || screen || chain || self.live_moves()
    }

    /// The chain of this owner, while it runs or after it failed.
    pub fn chain_of(&self, owner: &super::chain::Owner) -> Option<&Chain> {
        self.chain.as_ref().filter(|c| c.owner == *owner)
    }

    /// The next number for a load of the needs or for a chain.
    pub fn next_seq(&mut self) -> u64 {
        self.seq += 1;
        self.seq
    }

    /// True when the live view shows something that moves: a working agent, a spinner, a
    /// cursor that blinks, or a pager that loads.
    fn live_moves(&self) -> bool {
        let Some(live) = &self.live else {
            return false;
        };
        if live.tell.is_some()
            || live
                .pagers
                .iter()
                .any(|p| p.state == super::pager::PagerState::Loading)
        {
            return true;
        }
        let Some(data) = &live.data.data else {
            return live.data.loading();
        };
        let now = self.now_of(&data.view, &data.facts);
        now.spinner || now.seconds.is_some()
    }

    /// The engine as a ticket sees it: off, paused, a pass that runs or waits, or idle.
    pub fn pass(&self) -> Pass {
        if self.off() {
            return Pass::Off;
        }
        let running = self
            .engine
            .running
            .as_ref()
            .is_some_and(|r| r.job.kind == JobKind::Pass);
        if running {
            return Pass::Runs;
        }
        if self.engine.paused {
            return Pass::Paused;
        }
        if self.engine.queue.iter().any(|j| j.kind == JobKind::Pass) {
            return Pass::Waits;
        }
        Pass::Idle
    }

    /// The state of a ticket in the words of the app.
    pub fn now_of(&self, view: &TicketView, facts: &StateFacts) -> Now {
        let running = self.local.data.as_ref().and_then(|l| l.running.as_ref());
        now::now(&now::Input {
            view,
            facts,
            needs: self.needs(),
            running,
            pass: self.pass(),
            wall: self.clock.wall,
            offset_seconds: self.clock.offset_seconds,
        })
    }

    /// The state of each ticket in progress in short, and whether the next pass acts on one of
    /// them at once.
    pub fn ticket_states(&self) -> (Vec<TicketState>, bool) {
        let mut ready = false;
        let states = self
            .held()
            .iter()
            .map(|held| {
                let now = self.now_of(&held.view, &held.facts);
                ready |= now.ready;
                (
                    held.view.key.clone(),
                    held.view.step.clone().unwrap_or_default(),
                    now.title,
                )
            })
            .collect();
        (states, ready)
    }

    /// True when a part of the screen counts seconds without moving otherwise, such as the time
    /// since the last pass of the live view: the runtime draws it each second.
    pub fn shows_seconds(&self) -> bool {
        matches!(self.screen, Screen::Live { .. })
    }

    /// True when the window is big enough for the app.
    pub fn fits(&self) -> bool {
        self.size.0 >= MIN_WIDTH && self.size.1 >= MIN_HEIGHT
    }
}
