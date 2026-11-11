//! The terminal of the app: raw mode, the alternate screen, and the paste of text as one piece
//! while the app runs, and the normal terminal again on each way out: the normal end, an error,
//! a panic, and the signals SIGTERM, SIGHUP, and SIGINT. Ctrl-C is a key in raw mode, and the
//! app treats it as `q` (it cancels the line of `t`), so SIGINT comes only from another process.
//!
//! The thread that reads the keys can pause: while a child owns the terminal, it reads nothing,
//! and the runtime waits for it to confirm the pause before the child starts.

use std::io::{self, Write};
use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use ratatui::crossterm::cursor::{Hide, Show};
use ratatui::crossterm::event::{
    self, DisableBracketedPaste, EnableBracketedPaste, Event, KeyCode, KeyEvent, KeyEventKind,
    KeyModifiers,
};
use ratatui::crossterm::execute;
use ratatui::crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};

use super::msg::{Key, Msg};
use super::stderr;

/// True while the app owns the terminal.
static ACTIVE: AtomicBool = AtomicBool::new(false);
/// The end of the pipe that the signal handler writes to. -1 before the handler is set.
static SIGNAL_PIPE: AtomicI32 = AtomicI32::new(-1);

/// The terminal in raw mode with the alternate screen. Dropping it gives the terminal back.
#[derive(Debug)]
pub struct Session {
    _private: (),
}

impl Session {
    pub fn enter() -> io::Result<Session> {
        take()?;
        Ok(Session { _private: () })
    }
}

/// Puts the terminal in the state of the app: raw mode, the alternate screen, no cursor, and a
/// paste as one piece. Also after a child gave the terminal back.
pub fn take() -> io::Result<()> {
    enable_raw_mode()?;
    ACTIVE.store(true, Ordering::SeqCst);
    if let Err(e) = execute!(
        io::stdout(),
        EnterAlternateScreen,
        Hide,
        EnableBracketedPaste
    ) {
        leave();
        return Err(e);
    }
    Ok(())
}

impl Drop for Session {
    fn drop(&mut self) {
        leave();
    }
}

/// Gives the terminal back. Safe to call more than once.
pub fn leave() {
    if ACTIVE.swap(false, Ordering::SeqCst) {
        let mut out = io::stdout();
        let _ = execute!(out, DisableBracketedPaste, Show, LeaveAlternateScreen);
        let _ = out.flush();
        let _ = disable_raw_mode();
    }
}

/// A panic of the main thread gives the terminal and standard error back before the message
/// prints. A panic of another thread only prints, to the log file.
pub fn install_panic_hook() {
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        if std::thread::current().name() == Some("main") {
            leave();
            stderr::restore();
        }
        previous(info);
    }));
}

extern "C" fn on_signal(signal: libc::c_int) {
    let fd = SIGNAL_PIPE.load(Ordering::Relaxed);
    if fd >= 0 {
        let byte = signal as u8;
        // SAFETY: write is async-signal-safe, and the byte lives on this stack.
        unsafe {
            libc::write(fd, (&byte as *const u8).cast(), 1);
        }
    }
}

/// Sends `Msg::Signal` to the loop when the process gets SIGTERM, SIGHUP, or SIGINT, so that the loop
/// gives the terminal back before the process ends.
pub fn watch_signals(tx: Sender<Msg>) -> io::Result<()> {
    let mut fds = [0 as libc::c_int; 2];
    // SAFETY: the calls get valid pointers to local values, and the handler only writes a byte.
    unsafe {
        if libc::pipe(fds.as_mut_ptr()) != 0 {
            return Err(io::Error::last_os_error());
        }
        for fd in fds {
            libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC);
        }
        SIGNAL_PIPE.store(fds[1], Ordering::SeqCst);
        for signal in [libc::SIGTERM, libc::SIGHUP, libc::SIGINT] {
            let mut action: libc::sigaction = std::mem::zeroed();
            action.sa_sigaction = on_signal as extern "C" fn(libc::c_int) as usize;
            libc::sigemptyset(&mut action.sa_mask);
            action.sa_flags = libc::SA_RESTART;
            if libc::sigaction(signal, &action, std::ptr::null_mut()) != 0 {
                return Err(io::Error::last_os_error());
            }
        }
    }
    let read_end = fds[0];
    std::thread::Builder::new()
        .name("vision-signals".into())
        .spawn(move || loop {
            let mut byte = 0u8;
            // SAFETY: reads one byte into a local value from a pipe that this process owns.
            let n = unsafe { libc::read(read_end, (&mut byte as *mut u8).cast(), 1) };
            if n == 1 {
                let _ = tx.send(Msg::Signal(i32::from(byte)));
                return;
            }
            if n < 0 && io::Error::last_os_error().kind() == io::ErrorKind::Interrupted {
                continue;
            }
            return;
        })?;
    Ok(())
}

/// While it lives, the app ignores SIGINT, so that a Ctrl-C in a child that owns the terminal
/// ends only the child. Dropping it gives SIGINT its handler of before.
pub struct IgnoreSigint {
    old: libc::sigaction,
}

pub fn ignore_sigint() -> IgnoreSigint {
    // SAFETY: plain calls of sigaction with local values; the old action is kept to restore it.
    unsafe {
        let mut ignore: libc::sigaction = std::mem::zeroed();
        ignore.sa_sigaction = libc::SIG_IGN;
        libc::sigemptyset(&mut ignore.sa_mask);
        let mut old: libc::sigaction = std::mem::zeroed();
        libc::sigaction(libc::SIGINT, &ignore, &mut old);
        IgnoreSigint { old }
    }
}

impl Drop for IgnoreSigint {
    fn drop(&mut self) {
        // SAFETY: restores the action that `ignore_sigint` saved.
        unsafe {
            libc::sigaction(libc::SIGINT, &self.old, std::ptr::null_mut());
        }
    }
}

/// Ends the process by the signal that it got, with the default action of that signal.
pub fn reraise(signal: i32) {
    // SAFETY: resets one signal to its default action and sends it to this process.
    unsafe {
        libc::signal(signal, libc::SIG_DFL);
        libc::raise(signal);
    }
}

/// What the thread of the keys does now.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum InputState {
    Run,
    /// The runtime asked for a pause.
    Pause,
    /// The thread confirmed the pause and reads nothing.
    Paused,
}

/// The control of the thread of the keys. The runtime pauses it while a child owns the
/// terminal.
#[derive(Clone, Debug)]
pub struct InputControl {
    inner: Arc<(Mutex<InputState>, Condvar)>,
}

impl Default for InputControl {
    fn default() -> InputControl {
        InputControl {
            inner: Arc::new((Mutex::new(InputState::Run), Condvar::new())),
        }
    }
}

impl InputControl {
    /// Asks the thread to stop reading, and waits up to `limit` until it confirms. Gives true
    /// when it confirmed: from then on, it reads no key until [`InputControl::resume`].
    pub fn pause(&self, limit: Duration) -> bool {
        let (lock, cond) = &*self.inner;
        let mut state = lock.lock().unwrap_or_else(|e| e.into_inner());
        if *state == InputState::Run {
            *state = InputState::Pause;
        }
        let until = Instant::now() + limit;
        while *state != InputState::Paused {
            let left = until.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return false;
            }
            state = cond
                .wait_timeout(state, left)
                .unwrap_or_else(|e| e.into_inner())
                .0;
        }
        true
    }

    /// Lets the thread read again.
    pub fn resume(&self) {
        let (lock, cond) = &*self.inner;
        *lock.lock().unwrap_or_else(|e| e.into_inner()) = InputState::Run;
        cond.notify_all();
    }

    /// The thread calls this between two reads: on a pause, it confirms and waits until the
    /// runtime lets it read again.
    fn checkpoint(&self) {
        let (lock, cond) = &*self.inner;
        let mut state = lock.lock().unwrap_or_else(|e| e.into_inner());
        if *state == InputState::Pause {
            *state = InputState::Paused;
            cond.notify_all();
        }
        while *state == InputState::Paused {
            state = cond.wait(state).unwrap_or_else(|e| e.into_inner());
        }
    }
}

/// Where the keys come from: the terminal, or a fake in a test.
pub trait Source {
    /// True when an event is ready within `timeout`.
    fn poll(&mut self, timeout: Duration) -> io::Result<bool>;
    fn read(&mut self) -> io::Result<Event>;
}

/// The keys of the terminal.
pub struct Terminal;

impl Source for Terminal {
    fn poll(&mut self, timeout: Duration) -> io::Result<bool> {
        event::poll(timeout)
    }

    fn read(&mut self) -> io::Result<Event> {
        event::read()
    }
}

/// The longest wait for a key before the thread looks at its control again.
const POLL: Duration = Duration::from_millis(100);

/// Reads the keys, the pastes, and the changes of the window in a thread, and sends them to
/// the loop. The thread pauses when `control` asks.
pub fn watch_input(tx: Sender<Msg>, control: InputControl) -> io::Result<()> {
    std::thread::Builder::new()
        .name("vision-input".into())
        .spawn(move || input_loop(Terminal, &tx, &control))?;
    Ok(())
}

/// The loop of the thread of the keys. It ends when the source fails or the loop is gone.
pub fn input_loop(mut source: impl Source, tx: &Sender<Msg>, control: &InputControl) {
    loop {
        control.checkpoint();
        match source.poll(POLL) {
            Ok(true) => {}
            Ok(false) => continue,
            Err(_) => return,
        }
        let msg = match source.read() {
            Ok(Event::Key(key)) => key_of(key).map(Msg::Key),
            Ok(Event::Paste(text)) => Some(Msg::Paste(text)),
            Ok(Event::Resize(width, height)) => Some(Msg::Resize { width, height }),
            Ok(_) => None,
            Err(_) => return,
        };
        if let Some(msg) = msg {
            if tx.send(msg).is_err() {
                return;
            }
        }
    }
}

/// The key of the app for a key of the terminal. Ctrl-C is [`Key::Cancel`], which the update
/// treats as `q`, and as Esc in the line of `t`.
pub fn key_of(key: KeyEvent) -> Option<Key> {
    if key.kind == KeyEventKind::Release {
        return None;
    }
    if key.modifiers.contains(KeyModifiers::CONTROL) {
        return match key.code {
            KeyCode::Char('c') => Some(Key::Cancel),
            _ => None,
        };
    }
    Some(match key.code {
        KeyCode::Up => Key::Up,
        KeyCode::Down => Key::Down,
        KeyCode::Left => Key::Left,
        KeyCode::Right => Key::Right,
        KeyCode::Home => Key::Home,
        KeyCode::End => Key::End,
        KeyCode::PageUp => Key::PageUp,
        KeyCode::PageDown => Key::PageDown,
        KeyCode::Backspace => Key::Backspace,
        KeyCode::Delete => Key::Delete,
        KeyCode::Enter => Key::Enter,
        KeyCode::Esc => Key::Esc,
        KeyCode::Tab => Key::Tab,
        KeyCode::Char(c) => Key::Char(c),
        _ => Key::Other,
    })
}
