//! The thread of the keys with a fake source: it sends keys and pastes to the loop, it reads
//! nothing while the runtime pauses it for a session that owns the terminal, and it reads again
//! after the resume. A source that blocks in its poll, as the terminal does, wakes for a pause
//! with no key.

use std::collections::VecDeque;
use std::io;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use ratatui::crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
use vision::app::msg::{Key, Msg};
use vision::app::terminal::{input_loop, InputControl, Source};

/// A source of events that the test fills. It counts each read.
#[derive(Clone, Default)]
struct Fake {
    events: Arc<Mutex<VecDeque<Event>>>,
    reads: Arc<AtomicUsize>,
    closed: Arc<AtomicBool>,
}

impl Fake {
    fn push(&self, event: Event) {
        self.events.lock().unwrap().push_back(event);
    }

    fn waiting(&self) -> usize {
        self.events.lock().unwrap().len()
    }
}

impl Source for Fake {
    fn poll(&mut self, timeout: Duration) -> io::Result<bool> {
        if self.closed.load(Ordering::SeqCst) {
            return Err(io::Error::other("closed"));
        }
        if self.waiting() > 0 {
            return Ok(true);
        }
        std::thread::sleep(timeout.min(Duration::from_millis(5)));
        Ok(self.waiting() > 0)
    }

    fn read(&mut self) -> io::Result<Event> {
        self.reads.fetch_add(1, Ordering::SeqCst);
        self.events
            .lock()
            .unwrap()
            .pop_front()
            .ok_or_else(|| io::Error::other("no event"))
    }
}

fn char_key(c: char) -> Event {
    Event::Key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE))
}

const WAIT: Duration = Duration::from_secs(5);

#[test]
fn the_thread_of_the_keys_reads_nothing_while_it_is_paused() {
    let fake = Fake::default();
    let control = InputControl::default();
    let (tx, rx) = mpsc::channel();
    let thread = {
        let fake = fake.clone();
        let control = control.clone();
        std::thread::spawn(move || input_loop(fake, &tx, &control))
    };

    fake.push(char_key('a'));
    assert_eq!(rx.recv_timeout(WAIT).unwrap(), Msg::Key(Key::Char('a')));

    assert!(
        control.pause(Duration::from_secs(3)),
        "the thread confirms the pause"
    );
    let reads = fake.reads.load(Ordering::SeqCst);
    fake.push(char_key('b'));
    fake.push(Event::Paste("pasted text".into()));
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(
        fake.reads.load(Ordering::SeqCst),
        reads,
        "no read while paused"
    );
    assert_eq!(fake.waiting(), 2, "the keys stay for the session");
    assert!(rx.try_recv().is_err());

    control.resume();
    assert_eq!(rx.recv_timeout(WAIT).unwrap(), Msg::Key(Key::Char('b')));
    assert_eq!(
        rx.recv_timeout(WAIT).unwrap(),
        Msg::Paste("pasted text".into())
    );

    // A second pause and resume work the same way.
    assert!(control.pause(Duration::from_secs(3)));
    control.resume();
    fake.push(Event::Key(KeyEvent::new(
        KeyCode::Char('c'),
        KeyModifiers::CONTROL,
    )));
    assert_eq!(rx.recv_timeout(WAIT).unwrap(), Msg::Key(Key::Cancel));

    fake.closed.store(true, Ordering::SeqCst);
    thread.join().unwrap();
}

#[test]
fn a_pause_with_no_thread_gives_up_after_its_limit() {
    let control = InputControl::default();
    let started = std::time::Instant::now();
    assert!(!control.pause(Duration::from_millis(100)));
    assert!(started.elapsed() >= Duration::from_millis(100));
    control.resume();
}

/// The events that wait, and true after a wake.
type Waiting = Mutex<(VecDeque<Event>, bool)>;

/// A source that waits in its poll until an event comes or its waker runs, as the poll of the
/// terminal does. A wake gives what the signal of the terminal gives: a change of the window
/// with the same size. It counts each poll.
#[derive(Clone, Default)]
struct Blocking {
    state: Arc<(Waiting, Condvar)>,
    polls: Arc<AtomicUsize>,
    closed: Arc<AtomicBool>,
}

impl Blocking {
    fn push(&self, event: Event) {
        let (lock, cond) = &*self.state;
        lock.lock().unwrap().0.push_back(event);
        cond.notify_all();
    }

    fn wake(&self) {
        let (lock, cond) = &*self.state;
        lock.lock().unwrap().1 = true;
        cond.notify_all();
    }
}

impl Source for Blocking {
    fn poll(&mut self, timeout: Duration) -> io::Result<bool> {
        self.polls.fetch_add(1, Ordering::SeqCst);
        if self.closed.load(Ordering::SeqCst) {
            return Err(io::Error::other("closed"));
        }
        let (lock, cond) = &*self.state;
        let mut state = lock.lock().unwrap();
        let until = Instant::now() + timeout;
        while state.0.is_empty() && !state.1 {
            let left = until.saturating_duration_since(Instant::now());
            if left.is_zero() {
                break;
            }
            state = cond.wait_timeout(state, left).unwrap().0;
        }
        if std::mem::take(&mut state.1) {
            state.0.push_back(Event::Resize(80, 24));
        }
        Ok(!state.0.is_empty())
    }

    fn read(&mut self) -> io::Result<Event> {
        let (lock, _) = &*self.state;
        lock.lock()
            .unwrap()
            .0
            .pop_front()
            .ok_or_else(|| io::Error::other("no event"))
    }
}

#[test]
fn a_pause_wakes_the_thread_that_waits_for_a_key() {
    let fake = Blocking::default();
    let control = InputControl::default();
    {
        let fake = fake.clone();
        control.set_waker(move || fake.wake());
    }
    let (tx, rx) = mpsc::channel();
    let thread = {
        let fake = fake.clone();
        let control = control.clone();
        std::thread::spawn(move || input_loop(fake, &tx, &control))
    };
    fake.push(char_key('a'));
    assert_eq!(rx.recv_timeout(WAIT).unwrap(), Msg::Key(Key::Char('a')));

    // With no key, the thread stays in one poll: it does not wake to look at its control.
    std::thread::sleep(Duration::from_millis(200));
    let polls = fake.polls.load(Ordering::SeqCst);
    std::thread::sleep(Duration::from_millis(400));
    assert_eq!(fake.polls.load(Ordering::SeqCst), polls, "no poll ended");

    // A pause needs no key: it wakes the thread, and the thread confirms it.
    let started = Instant::now();
    assert!(control.pause(Duration::from_secs(3)));
    assert!(
        started.elapsed() < Duration::from_secs(1),
        "{:?}",
        started.elapsed()
    );
    assert_eq!(
        rx.recv_timeout(WAIT).unwrap(),
        Msg::Resize {
            width: 80,
            height: 24
        },
        "the wake reads as a change of the window with the same size"
    );
    fake.push(char_key('b'));
    std::thread::sleep(Duration::from_millis(100));
    assert!(rx.try_recv().is_err(), "no read while paused");

    control.resume();
    assert_eq!(rx.recv_timeout(WAIT).unwrap(), Msg::Key(Key::Char('b')));

    fake.closed.store(true, Ordering::SeqCst);
    fake.wake();
    thread.join().unwrap();
}
