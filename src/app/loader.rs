//! The loader of the app: one thread that keeps one connection to the ledger, one context with
//! the configuration and the adapter, and the proposals of the Curator, and that reads the data
//! of Home and of the live view.
//!
//! The update asks for the data about each second. The loader first makes a cheap check: the
//! data version of the ledger, the stamps of the files that the screens read, the job that a
//! pass runs, the turns that run with their minutes, the timer, and the proposals. It reads the
//! data again only when the check changed, when the update has no data, or when the last read
//! is 15 seconds old. Else it answers with one small message. The configuration is read again
//! only when its file changes, and the adapter only when what its load read changes: its
//! `adapter.toml`, its scripts folder, or a role file of `[agents]`. The proposals of the memory
//! are read again only after an action or a session ended, when the refs of the memory change,
//! or each minute, and the count of the commits that the forge does not have each 10 seconds.

use std::ffi::OsString;
use std::panic::AssertUnwindSafe;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, Sender};
use std::time::{Duration, Instant};

use time::OffsetDateTime;

use super::load;
use super::load_ticket::{self, EventLines};
use super::msg::Msg;
use crate::ctx::Ctx;
use crate::ledger::Ledger;
use crate::memory::{Memory, ProposalInfo};
use crate::paths::{Paths, TicketKey};
use crate::running::Running;
use crate::turn::Phase;
use crate::util::fs::{stamp, Stamp};
use crate::util::time as vtime;

/// The data is read again at least this often, whatever the check says.
pub const FULL_EVERY: Duration = Duration::from_secs(15);
/// The proposals of the memory are read again at least this often.
pub const MEMORY_EVERY: Duration = Duration::from_secs(60);
/// The count of the commits that the forge does not have is read again after this time. It
/// needs three git commands, and it changes only with a commit or a push.
pub const COMMITS_EVERY: Duration = Duration::from_secs(10);

/// What the runtime asks of the loader.
#[derive(Clone, Debug, PartialEq)]
pub enum Request {
    /// The local data of Home. `fresh` when the model has none, so that the loader reads it
    /// whatever the check says.
    Local { wall: OffsetDateTime, fresh: bool },
    /// The data of the live view of a ticket.
    Ticket { key: String, fresh: bool },
    /// An action or a session ended: the memory can have changed.
    Changed,
    /// The live view closed: the loader drops what it kept for it, such as the lines of the
    /// events of its ticket.
    LiveClosed,
}

/// The thread of the loader. Dropping it ends the thread after the read that runs.
#[derive(Debug)]
pub struct Loader {
    tx: Sender<Request>,
}

impl Loader {
    /// Starts the thread. Each answer goes to `out`.
    pub fn spawn(paths: Paths, out: Sender<Msg>) -> std::io::Result<Loader> {
        let (tx, rx) = mpsc::channel();
        std::thread::Builder::new()
            .name("vision-loader".into())
            .spawn(move || serve(paths, rx, out))?;
        Ok(Loader { tx })
    }

    /// Hands a request to the thread. False when the thread is gone.
    pub fn send(&self, request: Request) -> bool {
        self.tx.send(request).is_ok()
    }
}

/// The text of a read that stopped with a panic.
const PANIC_TEXT: &str = "the read stopped with an internal error. log/app.err has the details";

fn serve(paths: Paths, rx: Receiver<Request>, out: Sender<Msg>) {
    let mut state = State::new(paths.clone());
    for request in rx {
        let msg = match request {
            Request::Changed => {
                state.memory_changed();
                continue;
            }
            Request::LiveClosed => {
                state.live_closed();
                continue;
            }
            Request::Local { wall, fresh } => guarded(
                &mut state,
                &paths,
                |s| s.local(wall, fresh),
                |e| Msg::Local(Err(e)),
            ),
            Request::Ticket { key, fresh } => {
                let failed = key.clone();
                guarded(
                    &mut state,
                    &paths,
                    |s| s.ticket(&key, fresh),
                    move |e| Msg::Ticket {
                        key: failed.clone(),
                        data: Box::new(Err(e)),
                    },
                )
            }
        };
        if out.send(msg).is_err() {
            return;
        }
    }
}

/// Runs one read. After a panic, the loader starts again with no state, and the update gets
/// the message of `failed`.
fn guarded(
    state: &mut State,
    paths: &Paths,
    read: impl FnOnce(&mut State) -> Msg,
    failed: impl FnOnce(String) -> Msg,
) -> Msg {
    match std::panic::catch_unwind(AssertUnwindSafe(|| read(state))) {
        Ok(msg) => msg,
        Err(_) => {
            *state = State::new(paths.clone());
            failed(PANIC_TEXT.into())
        }
    }
}

/// The reads of the loader so far, for the tests.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Reads {
    /// The reads of the local data.
    pub local: usize,
    /// The reads of the data of the live view.
    pub ticket: usize,
    /// The reads of the proposals of the memory.
    pub memory: usize,
    /// The reads of the configuration: one for each context.
    pub config: usize,
}

/// What the load of the adapter read: its folder, the stamp of its `adapter.toml`, and the
/// answer of each test that the load makes on the disk ([`crate::adapter::Adapter::disk_tests`]).
/// When it changes, the context forgets the adapter, and the next read loads it again.
#[derive(Clone, Debug, PartialEq)]
struct AdapterKey {
    dir: PathBuf,
    file: Option<Stamp>,
    tests: Vec<bool>,
}

/// What a check found: when it differs from the check of the last read, the data is read again.
#[derive(Clone, Debug, PartialEq)]
struct Check {
    /// The data version of the ledger.
    version: i64,
    /// `config.toml` and the file `off`.
    files: Vec<Option<Stamp>>,
    adapter: Option<AdapterKey>,
    /// Each ticket that the data shows, with each entry of its folder, such as `plan.md`.
    folders: Vec<(String, Vec<(OsString, Stamp)>)>,
    /// The job or the operation that a pass runs now.
    running: Option<Running>,
    /// Each turn that runs, with its phase and its minutes so far.
    turns: Vec<(String, Phase, i64)>,
    timer: bool,
    /// The number of the changes of the proposals of the memory, for the local data.
    proposals: Option<u64>,
}

/// When a kind of data was read last, and what the check found then.
#[derive(Clone, Debug, Default)]
struct Seen {
    check: Option<Check>,
    at: Option<Instant>,
}

impl Seen {
    fn due(&self, check: &Check, now: Instant) -> bool {
        self.check.as_ref() != Some(check)
            || self
                .at
                .is_none_or(|at| now.duration_since(at) >= FULL_EVERY)
    }
}

/// The proposals of the Curator as the memory gave them last, and the handle of the memory.
#[derive(Debug, Default)]
struct Proposals {
    memory: Option<Memory>,
    list: Vec<ProposalInfo>,
    /// The stamps of the refs of the memory before the last read.
    refs: Option<Vec<Option<Stamp>>>,
    at: Option<Instant>,
    /// An action or a session ended since the last read.
    stale: bool,
    /// The number of the changes of the list.
    generation: u64,
    reads: usize,
}

impl Proposals {
    /// Reads the memory again when an action or a session ended, when its refs
    /// changed, or after [`MEMORY_EVERY`]. Else it keeps the handle and the list, and runs no
    /// git command.
    fn refresh(&mut self, ctx: &Ctx, now: Instant) {
        let refs = match &self.memory {
            Some(memory) => memory.refs_stamp(),
            None => crate::memory::store::refs_stamp(&ctx.paths.memory_dir().join(".git")),
        };
        let due = self.stale
            || self.refs.as_ref() != Some(&refs)
            || self
                .at
                .is_none_or(|at| now.duration_since(at) >= MEMORY_EVERY);
        if !due {
            return;
        }
        // A memory that cannot be read gives no proposal, as for the snapshot.
        self.memory = Memory::from_ctx(ctx).ok();
        let list = self
            .memory
            .as_ref()
            .and_then(|memory| memory.proposals().ok())
            .unwrap_or_default();
        if list != self.list {
            self.list = list;
            self.generation += 1;
        }
        // The stamps from before the read: a change during the read shows at the next look.
        self.refs = Some(refs);
        self.at = Some(now);
        self.stale = false;
        self.reads += 1;
    }
}

/// The state of the live view of one ticket.
#[derive(Debug)]
struct Live {
    key: String,
    seen: Seen,
    events: EventLines,
    /// The count of the commits that the forge does not have, when it was read, and the count
    /// that the update got last.
    unpushed: Option<usize>,
    unpushed_at: Option<Instant>,
    sent_unpushed: Option<usize>,
}

impl Live {
    fn new(key: &str) -> Live {
        Live {
            key: key.to_string(),
            seen: Seen::default(),
            events: EventLines::new(key),
            unpushed: None,
            unpushed_at: None,
            sent_unpushed: None,
        }
    }
}

/// What the loader keeps from one read to the next.
#[derive(Debug)]
pub struct State {
    paths: Paths,
    ctx: Ctx,
    /// The stamp of `config.toml` when the context was made.
    config: Option<Stamp>,
    /// What the load of the adapter read, when the context kept it.
    adapter: Option<AdapterKey>,
    /// The tests that a load of the adapter makes on the disk, from the parse of its
    /// `adapter.toml` when the file changed last. `None` when there is no adapter, or when the
    /// file does not parse.
    disk_tests: Option<Vec<(PathBuf, bool)>>,
    ledger: Option<Ledger>,
    /// The stamp of the ledger file of the connection.
    ledger_file: Option<Stamp>,
    proposals: Proposals,
    local: Seen,
    /// The tickets that the local data showed last.
    local_keys: Vec<String>,
    live: Option<Live>,
    reads: Reads,
}

impl State {
    pub fn new(paths: Paths) -> State {
        State {
            ctx: Ctx::keeping_adapter(paths.clone()),
            config: stamp(&paths.config()),
            adapter: None,
            disk_tests: None,
            ledger: None,
            ledger_file: None,
            proposals: Proposals::default(),
            local: Seen::default(),
            local_keys: Vec::new(),
            live: None,
            reads: Reads {
                config: 1,
                ..Reads::default()
            },
            paths,
        }
    }

    /// The reads so far.
    pub fn reads(&self) -> Reads {
        Reads {
            memory: self.proposals.reads,
            ..self.reads
        }
    }

    /// An action or a session ended: the next check reads the memory again.
    pub fn memory_changed(&mut self) {
        self.proposals.stale = true;
    }

    /// The live view closed: what the loader kept for it goes, and the next view of a ticket
    /// reads its data whole.
    pub fn live_closed(&mut self) {
        self.live = None;
    }

    /// The local data, or [`Msg::LocalSame`] when the check found no change.
    pub fn local(&mut self, wall: OffsetDateTime, fresh: bool) -> Msg {
        match self.read_local(wall, fresh) {
            Ok(Some(local)) => Msg::Local(Ok(local)),
            Ok(None) => Msg::LocalSame,
            Err(e) => {
                self.failed(&e);
                self.local = Seen::default();
                Msg::Local(Err(format!("{e:#}")))
            }
        }
    }

    /// The data of the live view of a ticket, or [`Msg::TicketSame`] when the check found no
    /// change.
    pub fn ticket(&mut self, key: &str, fresh: bool) -> Msg {
        match self.read_ticket(key, fresh) {
            Ok(Some(data)) => Msg::Ticket {
                key: key.to_string(),
                data: Box::new(Ok(data)),
            },
            Ok(None) => Msg::TicketSame {
                key: key.to_string(),
            },
            Err(e) => {
                self.failed(&e);
                if let Some(live) = self.live.as_mut() {
                    live.seen = Seen::default();
                }
                Msg::Ticket {
                    key: key.to_string(),
                    data: Box::new(Err(format!("{e:#}"))),
                }
            }
        }
    }

    /// After a read that failed with an error of SQLite or of the disk, the next read opens a
    /// new connection. Another error, such as a ticket that the ledger no longer has, keeps the
    /// connection.
    fn failed(&mut self, error: &anyhow::Error) {
        let broken = error
            .chain()
            .any(|cause| cause.is::<rusqlite::Error>() || cause.is::<std::io::Error>());
        if broken {
            self.ledger = None;
            self.ledger_file = None;
        }
    }

    /// A new context when `config.toml` changed, and no kept adapter when what its load read
    /// changed: the adapter is in another folder, its `adapter.toml` changed, or a test that the
    /// load makes on the disk gives another answer, such as a role file that went away.
    fn refresh_ctx(&mut self) {
        let config = stamp(&self.paths.config());
        if config != self.config {
            self.ctx = Ctx::keeping_adapter(self.paths.clone());
            self.config = config;
            self.adapter = None;
            self.disk_tests = None;
            self.reads.config += 1;
        }
        // The adapter: where it is, by the configuration, and the stamp of its file.
        let located = self
            .ctx
            .config()
            .ok()
            .and_then(|config| crate::adapter::locate(config, &self.paths).ok())
            .map(|located| {
                let file = located.dir.join(crate::adapter::FILE);
                (located.dir, stamp(&file))
            });
        // The tests come from a parse of the text of the file, which reads no other file, each
        // time the file changes.
        let same_file = match (&self.adapter, &located) {
            (Some(kept), Some((dir, file))) => kept.dir == *dir && kept.file == *file,
            _ => false,
        };
        if !same_file {
            self.disk_tests = located.as_ref().and_then(|(dir, _)| {
                let text = std::fs::read_to_string(dir.join(crate::adapter::FILE)).ok()?;
                let adapter = crate::adapter::Adapter::parse(&text, dir).ok()?;
                Some(adapter.disk_tests())
            });
        }
        let key = located.map(|(dir, file)| AdapterKey {
            dir,
            file,
            tests: crate::adapter::disk_answers(self.disk_tests.as_deref().unwrap_or_default()),
        });
        if key != self.adapter {
            self.ctx.forget_adapter();
            self.adapter = key;
        }
    }

    /// The connection, opened again when there is none or when the ledger file is a new one.
    fn connect(&mut self) -> anyhow::Result<()> {
        let file = stamp(&self.paths.ledger());
        let same_file = match (&self.ledger_file, &file) {
            (Some(old), Some(new)) => old.ino == new.ino,
            _ => false,
        };
        if self.ledger.is_none() || !same_file {
            self.ledger = Some(self.ctx.ledger()?);
            self.ledger_file = file;
        }
        Ok(())
    }

    /// The cheap check: one pragma, a few stamps, the running record, and the turns that run.
    fn check(&self, keys: &[String], proposals: Option<u64>) -> anyhow::Result<Check> {
        let Some(ledger) = self.ledger.as_ref() else {
            anyhow::bail!("the loader has no connection to the ledger");
        };
        let version = ledger.data_version()?;
        let now = vtime::now();
        let turns = crate::turn::running_turns(&self.ctx, ledger)?
            .into_iter()
            .map(|turn| {
                let minutes = turn
                    .started
                    .as_deref()
                    .and_then(|s| vtime::parse_rfc3339(s).ok())
                    .map_or(0, |at| vtime::seconds_between(at, now) / 60);
                (turn.id, turn.phase, minutes)
            })
            .collect();
        let folders = keys
            .iter()
            .map(|key| {
                let entries = TicketKey::parse(key)
                    .map(|key| folder_stamp(&self.paths.ticket_dir(&key)))
                    .unwrap_or_default();
                (key.clone(), entries)
            })
            .collect();
        Ok(Check {
            version,
            files: vec![self.config, stamp(&self.paths.off())],
            adapter: self.adapter.clone(),
            folders,
            running: crate::running::read(&self.paths),
            turns,
            timer: crate::install::is_installed(&self.paths),
            proposals,
        })
    }

    fn read_local(
        &mut self,
        wall: OffsetDateTime,
        fresh: bool,
    ) -> anyhow::Result<Option<super::model::Local>> {
        let now = Instant::now();
        self.refresh_ctx();
        self.connect()?;
        self.proposals.refresh(&self.ctx, now);
        let keys = self.local_keys.clone();
        let check = self.check(&keys, Some(self.proposals.generation))?;
        if !fresh && !self.local.due(&check, now) {
            return Ok(None);
        }
        let Some(ledger) = self.ledger.as_ref() else {
            anyhow::bail!("the loader has no connection to the ledger");
        };
        let open = crate::curator::open_of(&self.proposals.list, ledger);
        let local = load::local_with(&self.ctx, ledger, crate::curator::needs_of(open), wall)?;
        self.reads.local += 1;
        let shown: Vec<String> = local.tickets.iter().map(|h| h.view.key.clone()).collect();
        // The check of this read names the folders of the tickets that it shows. Each other part
        // stays as the check before the read found it, so a write during the read shows at the
        // next look. A ticket that the check before the read did not name gets no entry of its
        // folder, so the next look reads again when its folder has an entry.
        let check = if shown == keys {
            check
        } else {
            let folders = shown
                .iter()
                .map(|key| {
                    check
                        .folders
                        .iter()
                        .find(|(named, _)| named == key)
                        .cloned()
                        .unwrap_or_else(|| (key.clone(), Vec::new()))
                })
                .collect();
            self.local_keys = shown;
            Check { folders, ..check }
        };
        self.local = Seen {
            check: Some(check),
            at: Some(now),
        };
        Ok(Some(local))
    }

    fn read_ticket(
        &mut self,
        key: &str,
        fresh: bool,
    ) -> anyhow::Result<Option<super::live::TicketData>> {
        let now = Instant::now();
        self.refresh_ctx();
        self.connect()?;
        if self.live.as_ref().is_none_or(|live| live.key != key) {
            self.live = Some(Live::new(key));
        }
        let check = self.check(&[key.to_string()], None)?;
        let (Some(ledger), Some(live)) = (self.ledger.as_ref(), self.live.as_mut()) else {
            anyhow::bail!("the loader has no connection to the ledger");
        };
        let unpushed_due = live
            .unpushed_at
            .is_none_or(|at| now.duration_since(at) >= COMMITS_EVERY);
        if unpushed_due {
            live.unpushed = load_ticket::unpushed(&self.ctx, ledger, key);
            live.unpushed_at = Some(now);
        }
        let due = fresh || live.seen.due(&check, now);
        if !due && live.unpushed == live.sent_unpushed {
            return Ok(None);
        }
        // The read of the safety net also tests the event lines that it keeps.
        let check_events = fresh || live.seen.check.as_ref() == Some(&check);
        let data = load_ticket::ticket_with(
            &self.ctx,
            ledger,
            key,
            &mut live.events,
            check_events,
            live.unpushed,
        )?;
        live.sent_unpushed = data.unpushed;
        live.seen = Seen {
            check: Some(check),
            at: Some(now),
        };
        self.reads.ticket += 1;
        Ok(Some(data))
    }
}

/// The name and the stamp of each entry of a folder, by name. An entry that is written or
/// renamed, a new entry, and a removed entry each change it. A folder that is not there gives
/// no entry. The files of the question form are left out: no screen of the loader reads them,
/// and the form writes its drafts file after each draft.
fn folder_stamp(dir: &Path) -> Vec<(OsString, Stamp)> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut out: Vec<(OsString, Stamp)> = entries
        .flatten()
        .filter(|entry| !super::drafts::is_form_file(&entry.file_name()))
        .filter_map(|entry| {
            let path = entry.path();
            Some((entry.file_name(), stamp(&path)?))
        })
        .collect();
    out.sort_by(|a, b| a.0.cmp(&b.0));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A home that `vision init` made, with no ticket.
    fn home() -> (tempfile::TempDir, Paths) {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::new(dir.path().join("home"));
        crate::init::run(&Ctx::new(paths.clone())).unwrap();
        (dir, paths)
    }

    fn error_of(msg: Msg) -> String {
        match msg {
            Msg::Ticket { data, .. } => (*data).unwrap_err(),
            other => panic!("no error: {other:?}"),
        }
    }

    #[test]
    fn a_read_that_finds_no_ticket_keeps_the_connection_and_an_error_of_sqlite_drops_it() {
        let (_dir, paths) = home();
        let mut state = State::new(paths.clone());
        // The live view of a ticket that the ledger does not have, each second.
        for _ in 0..2 {
            let error = error_of(state.ticket("DEMO-999", false));
            assert!(error.contains("no ticket DEMO-999"), "{error}");
            assert!(
                state.ledger.is_some(),
                "a read that finds no ticket keeps the connection"
            );
        }
        // An error of SQLite drops the connection, and the next read opens a new one.
        let other = Ledger::open(&paths.ledger()).unwrap();
        other
            .conn()
            .execute_batch("ALTER TABLE tickets RENAME TO tickets_away")
            .unwrap();
        let error = error_of(state.ticket("DEMO-999", false));
        assert!(error.contains("tickets"), "{error}");
        assert!(
            state.ledger.is_none(),
            "an error of SQLite drops the connection"
        );
        other
            .conn()
            .execute_batch("ALTER TABLE tickets_away RENAME TO tickets")
            .unwrap();
        let error = error_of(state.ticket("DEMO-999", false));
        assert!(error.contains("no ticket DEMO-999"), "{error}");
        assert!(state.ledger.is_some());
    }
}
