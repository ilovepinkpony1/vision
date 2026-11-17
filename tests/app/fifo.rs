//! A FIFO in place of a file that the app reads. The open of each reader waits until the thread
//! of the FIFO opens the other end, so a test can act at the moment of a read, in the middle of
//! the work of the app. Each reader then gets an empty file.

use std::fs::OpenOptions;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;

/// A FIFO and the thread that serves its readers. Dropping it ends the thread and removes the
/// FIFO.
pub struct Fifo {
    path: PathBuf,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl Fifo {
    /// Makes the FIFO at `path` and serves each reader. `at_open(n)` runs at the open `n`, from
    /// 1, before the reader reads on. On macOS, a reader that has not closed its end yet can
    /// count a second time, so only the first open is exact.
    pub fn serve(path: &Path, mut at_open: impl FnMut(usize) + Send + 'static) -> Fifo {
        let made = Command::new("mkfifo")
            .arg(path)
            .status()
            .expect("run mkfifo");
        assert!(made.success(), "mkfifo {}", path.display());
        let stop = Arc::new(AtomicBool::new(false));
        let (fifo, stopped) = (path.to_path_buf(), stop.clone());
        let thread = std::thread::spawn(move || {
            let mut n = 0;
            loop {
                // The open waits for a reader.
                let Ok(writer) = OpenOptions::new().write(true).open(&fifo) else {
                    return;
                };
                if stopped.load(Ordering::SeqCst) {
                    return;
                }
                n += 1;
                at_open(n);
                // The reader reads to the end of the file only after this close.
                drop(writer);
            }
        });
        Fifo {
            path: path.to_path_buf(),
            stop,
            thread: Some(thread),
        }
    }
}

impl Drop for Fifo {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        // A reader that does not wait ends the open of the thread. It stays open until the
        // thread ends, because macOS ends the wait only while a reader is there.
        let reader = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NONBLOCK)
            .open(&self.path);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
        drop(reader);
        let _ = std::fs::remove_file(&self.path);
    }
}
