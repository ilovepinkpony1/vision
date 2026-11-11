//! The thread of the keys with a fake source: it sends keys and pastes to the loop, it reads
//! nothing while the runtime pauses it for a session that owns the terminal, and it reads again
//! after the resume.

use std::collections::VecDeque;
use std::io;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::Duration;

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
