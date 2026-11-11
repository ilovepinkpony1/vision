//! The terminal of the app: raw mode and the alternate screen while the app runs, and the
//! normal terminal again on each way out: the normal end, an error, a panic, and the signals
//! SIGTERM, SIGHUP, and SIGINT. Ctrl-C is a key in raw mode, and the app treats it as `q`, so
//! SIGINT comes only from another process.

use std::io::{self, Write};
use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};
use std::sync::mpsc::Sender;

use ratatui::crossterm::cursor::{Hide, Show};
use ratatui::crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
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
        enable_raw_mode()?;
        ACTIVE.store(true, Ordering::SeqCst);
        if let Err(e) = execute!(io::stdout(), EnterAlternateScreen, Hide) {
            leave();
            return Err(e);
        }
        Ok(Session { _private: () })
    }
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
        let _ = execute!(out, Show, LeaveAlternateScreen);
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

/// Ends the process by the signal that it got, with the default action of that signal.
pub fn reraise(signal: i32) {
    // SAFETY: resets one signal to its default action and sends it to this process.
    unsafe {
        libc::signal(signal, libc::SIG_DFL);
        libc::raise(signal);
    }
}

/// Reads the keys and the changes of the window in a thread, and sends them to the loop.
pub fn watch_input(tx: Sender<Msg>) -> io::Result<()> {
    std::thread::Builder::new()
        .name("vision-input".into())
        .spawn(move || loop {
            let msg = match event::read() {
                Ok(Event::Key(key)) => key_of(key).map(Msg::Key),
                Ok(Event::Resize(width, height)) => Some(Msg::Resize { width, height }),
                Ok(_) => None,
                Err(_) => return,
            };
            if let Some(msg) = msg {
                if tx.send(msg).is_err() {
                    return;
                }
            }
        })?;
    Ok(())
}

/// The key of the app for a key of the terminal. Ctrl-C is `q`.
pub fn key_of(key: KeyEvent) -> Option<Key> {
    if key.kind == KeyEventKind::Release {
        return None;
    }
    if key.modifiers.contains(KeyModifiers::CONTROL) {
        return match key.code {
            KeyCode::Char('c') => Some(Key::Char('q')),
            _ => None,
        };
    }
    Some(match key.code {
        KeyCode::Up => Key::Up,
        KeyCode::Down => Key::Down,
        KeyCode::Enter => Key::Enter,
        KeyCode::Esc => Key::Esc,
        KeyCode::Tab => Key::Tab,
        KeyCode::Char(c) => Key::Char(c),
        _ => Key::Other,
    })
}
