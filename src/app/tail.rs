//! The reader of the stream files of one ticket, in a thread that lives only while the live view
//! of that ticket is open. A few times each second it looks for the turn folders of the ticket,
//! reads the new complete lines of the stream file of each of the last turns (as `vision logs`
//! reads them), and sends the new events to the loop. A line that the session writes in two
//! parts comes out one time, when it is complete. Of an old turn that does not run, the first
//! read takes only the end of its file that the feed keeps.

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;
use std::sync::Arc;
use std::time::Duration;

use time::OffsetDateTime;

use super::feed::{items_of, Batch, KEEP_STREAM};
use super::load_ticket::parse_line;
use super::msg::Msg;
use crate::paths::{Paths, TicketKey, TurnId};
use crate::turn::Tail;

/// The time between two reads.
pub const EVERY: Duration = Duration::from_millis(250);
/// The last turns of the ticket whose stream files the reader follows.
pub const TURNS: usize = 6;

/// The reader of one ticket. Dropping it stops the thread at its next read.
#[derive(Debug)]
pub struct Follow {
    pub key: String,
    stop: Arc<AtomicBool>,
}

impl Drop for Follow {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
    }
}

/// One followed turn: its tail, and whether its next read is its first read of the lines that
/// were there when the reader began.
struct Followed {
    tail: Tail,
    old: bool,
    /// The turn does not run: its first read takes only the end of its file that the feed
    /// keeps.
    from_end: bool,
}

/// The state of the reader: one tail for each followed turn.
pub struct Reader {
    paths: Paths,
    key: TicketKey,
    tails: BTreeMap<String, Followed>,
    started: bool,
}

impl Reader {
    pub fn new(paths: Paths, key: TicketKey) -> Reader {
        Reader {
            paths,
            key,
            tails: BTreeMap::new(),
            started: false,
        }
    }

    /// One read: the new events of each followed stream file, as batches. The lines that were
    /// in a file at the first read of the reader are old: their time is not known.
    pub fn read(&mut self, now: OffsetDateTime) -> Vec<Batch> {
        let mut names: Vec<String> = std::fs::read_dir(self.paths.turns_dir(&self.key))
            .map(|entries| {
                entries
                    .flatten()
                    .map(|e| e.file_name().to_string_lossy().into_owned())
                    .filter(|name| TurnId::parse(name).is_ok())
                    .collect()
            })
            .unwrap_or_default();
        names.sort();
        let keep = names.split_off(names.len().saturating_sub(TURNS));
        let first = !self.started;
        self.started = true;
        for name in &keep {
            if !self.tails.contains_key(name) {
                let Ok(id) = TurnId::parse(name) else {
                    continue;
                };
                let turn = self.paths.turn(&self.key, &id);
                // A turn that was there at the first read has old lines; a later one is new. An
                // old turn whose supervisor does not run writes no more: the feed keeps only the
                // end of its file. A running turn is read whole, because NOW counts each of its
                // tool calls.
                let from_end = first && !crate::turn::supervisor_alive(&turn.dir);
                let followed = Followed {
                    tail: Tail::new(turn.stream),
                    old: first,
                    from_end,
                };
                self.tails.insert(name.clone(), followed);
            }
        }
        self.tails.retain(|name, _| keep.contains(name));
        let mut out = Vec::new();
        for (name, followed) in &mut self.tails {
            let lines = if followed.from_end {
                followed.tail.read_back(KEEP_STREAM, |line| {
                    parse_line(line).map_or(0, |l| items_of(&l.event))
                })
            } else {
                followed.tail.read_new()
            };
            let was_old = followed.old;
            followed.old = false;
            followed.from_end = false;
            if lines.is_empty() {
                continue;
            }
            let events: Vec<_> = lines.iter().filter_map(|l| parse_line(l)).collect();
            if events.is_empty() {
                continue;
            }
            let id = TurnId::parse(name).ok();
            let stream = id.as_ref().map(|id| self.paths.turn(&self.key, id).stream);
            let at = if was_old {
                stream.as_deref().and_then(modified).unwrap_or(now)
            } else {
                now
            };
            out.push(Batch {
                turn: name.clone(),
                events,
                old: was_old,
                at,
                started: id.map(|id| id.at()),
            });
        }
        out
    }
}

fn modified(path: &Path) -> Option<OffsetDateTime> {
    let time = std::fs::metadata(path).ok()?.modified().ok()?;
    Some(OffsetDateTime::from(time))
}

/// Starts the reader thread of a ticket. Each batch goes to the loop as [`Msg::Stream`].
pub fn follow(paths: Paths, key: &str, tx: Sender<Msg>) -> Option<Follow> {
    let ticket = TicketKey::parse(key).ok()?;
    let stop = Arc::new(AtomicBool::new(false));
    let flag = stop.clone();
    let name = key.to_string();
    std::thread::Builder::new()
        .name("vision-tail".into())
        .spawn(move || {
            let mut reader = Reader::new(paths, ticket);
            while !flag.load(Ordering::SeqCst) {
                for batch in reader.read(OffsetDateTime::now_utc()) {
                    let msg = Msg::Stream {
                        key: name.clone(),
                        batch,
                    };
                    if tx.send(msg).is_err() {
                        return;
                    }
                }
                std::thread::sleep(EVERY);
            }
        })
        .ok()?;
    Some(Follow {
        key: key.to_string(),
        stop,
    })
}
