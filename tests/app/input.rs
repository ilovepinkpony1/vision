//! The thread of the keys with a fake source: it sends keys, pastes, clicks, and steps of the
//! wheel to the loop, and nothing for a move of the mouse. It reads nothing while the runtime
//! pauses it for a session that owns the terminal, and it reads again after the resume. A source
//! that blocks in its poll, as the terminal does, wakes for a pause with no key. A child that
//! gets the terminal gets it with the mouse off, and the app turns the mouse on again after it.

use std::collections::VecDeque;
use std::io;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use ratatui::crossterm::event::{
    Event, KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
use vision::app::msg::{Key, Mouse, MouseKind, Msg};
use vision::app::session::{open_with, Handover};
use vision::app::terminal::{app_modes, input_loop, normal_modes, InputControl, Source};
use vision::app::worker::WorkerConfig;

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

fn mouse_event(kind: MouseEventKind, modifiers: KeyModifiers) -> Event {
    Event::Mouse(MouseEvent {
        kind,
        column: 30,
        row: 12,
        modifiers,
    })
}

#[test]
fn the_thread_sends_a_click_and_the_wheel_and_nothing_for_a_move() {
    let fake = Fake::default();
    let control = InputControl::default();
    let (tx, rx) = mpsc::channel();
    let thread = {
        let fake = fake.clone();
        let control = control.clone();
        std::thread::spawn(move || input_loop(fake, &tx, &control))
    };
    let none = KeyModifiers::NONE;
    for event in [
        mouse_event(MouseEventKind::Moved, none),
        mouse_event(MouseEventKind::Drag(MouseButton::Left), none),
        mouse_event(MouseEventKind::Up(MouseButton::Left), none),
        mouse_event(MouseEventKind::Down(MouseButton::Right), none),
        mouse_event(MouseEventKind::Down(MouseButton::Left), KeyModifiers::SHIFT),
        mouse_event(MouseEventKind::Down(MouseButton::Left), none),
        mouse_event(MouseEventKind::ScrollUp, none),
        mouse_event(MouseEventKind::ScrollDown, none),
        char_key('z'),
    ] {
        fake.push(event);
    }
    let at = |kind| {
        Msg::Mouse(Mouse {
            kind,
            column: 30,
            row: 12,
        })
    };
    // The move, the drag, the release, the right button, and the click with Shift send
    // nothing: the first message is the click.
    assert_eq!(rx.recv_timeout(WAIT).unwrap(), at(MouseKind::Click));
    assert_eq!(rx.recv_timeout(WAIT).unwrap(), at(MouseKind::WheelUp));
    assert_eq!(rx.recv_timeout(WAIT).unwrap(), at(MouseKind::WheelDown));
    assert_eq!(rx.recv_timeout(WAIT).unwrap(), Msg::Key(Key::Char('z')));
    fake.closed.store(true, Ordering::SeqCst);
    thread.join().unwrap();
}

/// A terminal that records what the app writes to it in a file, with the same modes that the
/// app writes to the real terminal.
struct Recorder {
    file: PathBuf,
}

impl Recorder {
    fn out(&self) -> std::fs::File {
        std::fs::OpenOptions::new()
            .append(true)
            .open(&self.file)
            .unwrap()
    }
}

impl Handover for Recorder {
    fn give(&mut self) {
        normal_modes(&mut self.out()).unwrap();
    }

    fn take(&mut self) -> io::Result<()> {
        app_modes(&mut self.out())
    }
}

/// True when the last mode of the mouse in the text turns it on.
fn mouse_on(text: &str) -> bool {
    let on = text.rfind("\x1b[?1000h");
    let off = text.rfind("\x1b[?1000l");
    match (on, off) {
        (Some(on), Some(off)) => on > off,
        (Some(_), None) => true,
        _ => false,
    }
}

#[test]
fn a_child_gets_the_terminal_with_the_mouse_off_and_the_app_turns_it_on_after() {
    let dir = tempfile::tempdir().unwrap();
    let log = dir.path().join("terminal.log");
    let seen = dir.path().join("seen.log");
    // The child keeps a copy of what the terminal got before it ran.
    let child = dir.path().join("child");
    std::fs::write(
        &child,
        format!("#!/bin/sh\ncp '{}' '{}'\n", log.display(), seen.display()),
    )
    .unwrap();
    std::fs::set_permissions(&child, std::fs::Permissions::from_mode(0o755)).unwrap();
    // The app owns the terminal, with the mouse on.
    let mut out = std::fs::File::create(&log).unwrap();
    app_modes(&mut out).unwrap();
    drop(out);
    assert!(mouse_on(&std::fs::read_to_string(&log).unwrap()));
    // The thread of the keys runs, so that it confirms its pause.
    let fake = Fake::default();
    let control = InputControl::default();
    let (tx, rx) = mpsc::channel();
    let thread = {
        let fake = fake.clone();
        let control = control.clone();
        std::thread::spawn(move || input_loop(fake, &tx, &control))
    };
    let config = WorkerConfig {
        exe: child,
        home: dir.path().to_path_buf(),
        env: None,
    };
    let mut terminal = Recorder { file: log.clone() };
    let args = ["open".to_string(), "DEMO-12".to_string()];
    let result = open_with(&mut terminal, &config, &args, &control);
    assert!(result.ok(), "{result:?}");
    let seen = std::fs::read_to_string(&seen).unwrap();
    assert!(!mouse_on(&seen), "the child got the mouse on: {seen:?}");
    assert!(seen.contains("\x1b[?1003l"), "{seen:?}");
    let after = std::fs::read_to_string(&log).unwrap();
    assert!(after.starts_with(&seen));
    assert!(
        mouse_on(&after),
        "the app did not turn the mouse on: {after:?}"
    );
    // The thread reads again after the child.
    fake.push(char_key('a'));
    assert_eq!(rx.recv_timeout(WAIT).unwrap(), Msg::Key(Key::Char('a')));
    fake.closed.store(true, Ordering::SeqCst);
    thread.join().unwrap();
}
