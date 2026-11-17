//! The terminal of the app: raw mode, the alternate screen, the paste of text as one piece, and
//! the clicks and the wheel of the mouse while the app runs, and the normal terminal again on
//! each way out: the normal end, an error, a panic, and the signals SIGTERM, SIGHUP, and SIGINT.
//! Ctrl-C is a key in raw mode, and the app treats it as `q` (it cancels the line of `t`), so
//! SIGINT comes only from another process.
//!
//! The terminal reports a press and a release of a button and each step of the wheel, but no
//! move of the mouse, so a move never wakes the app. With the mouse on, a terminal selects text
//! only with a modifier, such as Shift.
//!
//! The thread that reads the keys waits in its poll until a key, a click, or a step of the wheel
//! comes. It can pause: while a child owns the terminal, it reads nothing, and the runtime wakes
//! it and waits for it to confirm the pause before the child starts.

use std::fmt;
use std::io::{self, Write};
use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use ratatui::crossterm::cursor::{Hide, Show};
use ratatui::crossterm::event::{
    self, DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, Event, KeyCode,
    KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
use ratatui::crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use ratatui::crossterm::{execute, Command};

use super::msg::{Key, Mouse, MouseKind, Msg};
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

/// Puts the terminal in the state of the app: raw mode and [`app_modes`]. Also after a child
/// gave the terminal back.
pub fn take() -> io::Result<()> {
    enable_raw_mode()?;
    ACTIVE.store(true, Ordering::SeqCst);
    if let Err(e) = app_modes(&mut io::stdout()) {
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

/// Gives the terminal back: [`normal_modes`] and no raw mode. Safe to call more than once. Each
/// way out calls it, and so does the hand-over of the terminal to a child.
pub fn leave() {
    if ACTIVE.swap(false, Ordering::SeqCst) {
        let _ = normal_modes(&mut io::stdout());
        let _ = disable_raw_mode();
    }
}

/// Turns on the report of the buttons and of the wheel of the mouse, with no report of a move:
/// the normal tracking (1000) with the cells of SGR (1006).
#[derive(Clone, Copy, Debug)]
struct EnableClicks;

impl Command for EnableClicks {
    fn write_ansi(&self, f: &mut impl fmt::Write) -> fmt::Result {
        f.write_str("\x1b[?1000h\x1b[?1006h")
    }
}

/// The modes of the app on the terminal: the alternate screen, no cursor, a paste as one piece,
/// and the clicks and the wheel of the mouse. Each mode of the mouse that was on before goes
/// first, so that no report of a move stays on.
pub fn app_modes(out: &mut impl Write) -> io::Result<()> {
    execute!(
        out,
        EnterAlternateScreen,
        Hide,
        EnableBracketedPaste,
        DisableMouseCapture,
        EnableClicks
    )
}

/// The normal modes of the terminal: no mode of the mouse, no paste as one piece, the cursor,
/// and the main screen. [`DisableMouseCapture`] turns off each mode of the mouse, also one that
/// the app did not turn on.
pub fn normal_modes(out: &mut impl Write) -> io::Result<()> {
    execute!(
        out,
        DisableMouseCapture,
        DisableBracketedPaste,
        Show,
        LeaveAlternateScreen
    )?;
    out.flush()
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

/// What wakes the thread of the keys while it waits for a key.
type Waker = Arc<dyn Fn() + Send + Sync>;

/// The control of the thread of the keys. The runtime pauses it while a child owns the
/// terminal.
#[derive(Clone)]
pub struct InputControl {
    inner: Arc<(Mutex<InputState>, Condvar)>,
    waker: Arc<Mutex<Option<Waker>>>,
}

impl std::fmt::Debug for InputControl {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let state = *self.inner.0.lock().unwrap_or_else(|e| e.into_inner());
        f.debug_struct("InputControl")
            .field("state", &state)
            .finish_non_exhaustive()
    }
}

impl Default for InputControl {
    fn default() -> InputControl {
        InputControl {
            inner: Arc::new((Mutex::new(InputState::Run), Condvar::new())),
            waker: Arc::new(Mutex::new(None)),
        }
    }
}

/// While a pause waits for the confirmation, it wakes the thread again after this time.
const WAKE_AGAIN: Duration = Duration::from_millis(250);

impl InputControl {
    /// Sets what wakes the thread while it waits for a key, so that a pause needs no key. The
    /// thread waits in its poll with no timer, so with no waker a pause waits for a key or the
    /// end of the poll.
    pub fn set_waker(&self, waker: impl Fn() + Send + Sync + 'static) {
        *self.waker.lock().unwrap_or_else(|e| e.into_inner()) = Some(Arc::new(waker));
    }

    fn wake(&self) {
        let waker = self.waker.lock().unwrap_or_else(|e| e.into_inner()).clone();
        if let Some(waker) = waker {
            waker();
        }
    }

    /// Asks the thread to stop reading, wakes it, and waits up to `limit` until it confirms.
    /// Gives true when it confirmed: from then on, it reads no key until
    /// [`InputControl::resume`].
    pub fn pause(&self, limit: Duration) -> bool {
        let (lock, cond) = &*self.inner;
        {
            let mut state = lock.lock().unwrap_or_else(|e| e.into_inner());
            if *state == InputState::Run {
                *state = InputState::Pause;
            }
        }
        let until = Instant::now() + limit;
        loop {
            // The waker runs with no lock held: it can take a lock of its own source.
            self.wake();
            let step = (Instant::now() + WAKE_AGAIN).min(until);
            let mut state = lock.lock().unwrap_or_else(|e| e.into_inner());
            while *state != InputState::Paused {
                let left = step.saturating_duration_since(Instant::now());
                if left.is_zero() {
                    break;
                }
                state = cond
                    .wait_timeout(state, left)
                    .unwrap_or_else(|e| e.into_inner())
                    .0;
            }
            if *state == InputState::Paused {
                return true;
            }
            if Instant::now() >= until {
                return false;
            }
        }
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

/// The longest wait for a key before the thread looks at its control again. A pause wakes the
/// thread before that, so the thread wakes for nothing else than a key, a paste, an event of the
/// mouse, or a change of the window.
const POLL: Duration = Duration::from_secs(60);

/// Reads the keys, the pastes, the clicks, the steps of the wheel, and the changes of the window
/// in a thread, and sends them to the loop. The thread pauses when `control` asks.
pub fn watch_input(tx: Sender<Msg>, control: InputControl) -> io::Result<()> {
    // The event reader of crossterm wakes on the signal of a change of the window and reports
    // the size of now: a pause sends that signal to wake the thread. The change of the window
    // that the loop then gets has the same size, so it changes nothing.
    control.set_waker(|| {
        // SAFETY: raise only sends a signal to this thread; the handler of crossterm for it
        // writes one byte to a pipe.
        unsafe {
            libc::raise(libc::SIGWINCH);
        }
    });
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
            Ok(Event::Mouse(mouse)) => mouse_of(mouse).map(Msg::Mouse),
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

/// The event of the app for an event of the mouse: a press of the left button, or a step of the
/// wheel. A release, a move, a drag, another button, and an event with Shift, Alt, or Ctrl give
/// none, so that they never reach the loop.
pub fn mouse_of(event: MouseEvent) -> Option<Mouse> {
    if !event.modifiers.is_empty() {
        return None;
    }
    let kind = match event.kind {
        MouseEventKind::Down(MouseButton::Left) => MouseKind::Click,
        MouseEventKind::ScrollUp => MouseKind::WheelUp,
        MouseEventKind::ScrollDown => MouseKind::WheelDown,
        _ => return None,
    };
    Some(Mouse {
        kind,
        column: event.column,
        row: event.row,
    })
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

#[cfg(test)]
mod tests {
    use super::*;

    fn text(write: impl Fn(&mut Vec<u8>) -> io::Result<()>) -> String {
        let mut out = Vec::new();
        write(&mut out).unwrap();
        String::from_utf8(out).unwrap()
    }

    #[test]
    fn the_app_turns_on_the_clicks_and_the_wheel_but_no_report_of_a_move() {
        let on = text(app_modes);
        let enable = on.rfind("\x1b[?1000h").expect("the clicks are on");
        assert!(on.contains("\x1b[?1006h"), "the cells of SGR: {on:?}");
        // Each mode of the mouse goes first, and no report of a move comes back after it.
        let off = on
            .rfind("\x1b[?1003l")
            .expect("the report of each move is off first");
        assert!(off < enable, "{on:?}");
        assert!(!on.contains("\x1b[?1003h"), "{on:?}");
        assert!(!on.contains("\x1b[?1002h"), "{on:?}");
    }

    #[test]
    fn the_normal_modes_turn_off_each_mode_of_the_mouse() {
        let off = text(normal_modes);
        for mode in ["1000", "1002", "1003", "1006", "1015"] {
            assert!(off.contains(&format!("\x1b[?{mode}l")), "{mode}: {off:?}");
        }
        assert!(!off.contains("\x1b[?1000h"), "{off:?}");
    }

    #[test]
    fn only_a_press_of_the_left_button_and_the_wheel_reach_the_loop() {
        let event = |kind, modifiers| MouseEvent {
            kind,
            column: 12,
            row: 7,
            modifiers,
        };
        let none = KeyModifiers::NONE;
        assert_eq!(
            mouse_of(event(MouseEventKind::Down(MouseButton::Left), none)),
            Some(Mouse {
                kind: MouseKind::Click,
                column: 12,
                row: 7
            })
        );
        assert_eq!(
            mouse_of(event(MouseEventKind::ScrollUp, none)).map(|m| m.kind),
            Some(MouseKind::WheelUp)
        );
        assert_eq!(
            mouse_of(event(MouseEventKind::ScrollDown, none)).map(|m| m.kind),
            Some(MouseKind::WheelDown)
        );
        for kind in [
            MouseEventKind::Up(MouseButton::Left),
            MouseEventKind::Drag(MouseButton::Left),
            MouseEventKind::Moved,
            MouseEventKind::Down(MouseButton::Right),
            MouseEventKind::Down(MouseButton::Middle),
            MouseEventKind::ScrollLeft,
        ] {
            assert_eq!(mouse_of(event(kind, none)), None, "{kind:?}");
        }
        assert_eq!(
            mouse_of(event(
                MouseEventKind::Down(MouseButton::Left),
                KeyModifiers::SHIFT
            )),
            None,
            "a click with Shift is for the terminal"
        );
    }
}
