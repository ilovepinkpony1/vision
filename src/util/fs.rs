//! File helpers. Each state file is written to a temporary name first and then renamed,
//! so that a reader never sees half a file.

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use anyhow::{anyhow, Context, Result};

static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

/// The temporary name of a file in its own folder: `.<name>.<pid>-<n>.tmp`. A name that
/// starts with `.` marks a file that is not done yet.
fn temp_path(path: &Path) -> Result<PathBuf> {
    let dir = path
        .parent()
        .filter(|d| !d.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let name = path
        .file_name()
        .ok_or_else(|| anyhow!("cannot write {}: the path has no file name", path.display()))?;
    let counter = TEMP_COUNTER.fetch_add(1, Ordering::Relaxed);
    Ok(dir.join(format!(
        ".{}.{}-{}.tmp",
        name.to_string_lossy(),
        std::process::id(),
        counter
    )))
}

/// True when `name` is the name of a temporary file of a write of the file `file`, as
/// [`atomic_write`] names it.
pub fn is_temp_of(name: &str, file: &str) -> bool {
    name.strip_prefix('.')
        .and_then(|rest| rest.strip_prefix(file))
        .and_then(|rest| rest.strip_prefix('.'))
        .is_some_and(|rest| rest.ends_with(".tmp"))
}

/// Writes `bytes` to `path` through a temporary file in the same folder and a rename.
/// The folder must exist.
pub fn atomic_write(path: &Path, bytes: &[u8]) -> Result<()> {
    let temp = temp_path(path)?;
    let written = write_new(&temp, bytes).and_then(|()| {
        fs::rename(&temp, path)
            .with_context(|| format!("could not rename {} to {}", temp.display(), path.display()))
    });
    if written.is_err() {
        let _ = fs::remove_file(&temp);
    }
    written.with_context(|| format!("could not write {}", path.display()))
}

/// Copies the file `from` to `path` through a temporary file in the same folder and a rename,
/// so that `path` is either the old file or the whole new one. The folder must exist. Gives the
/// bytes copied.
pub fn atomic_copy(from: &Path, path: &Path) -> Result<u64> {
    let temp = temp_path(path)?;
    let copied = (|| -> Result<u64> {
        let mut source =
            fs::File::open(from).with_context(|| format!("could not read {}", from.display()))?;
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temp)
            .with_context(|| format!("could not create {}", temp.display()))?;
        let bytes = std::io::copy(&mut source, &mut file)?;
        file.sync_all()?;
        fs::rename(&temp, path).with_context(|| {
            format!("could not rename {} to {}", temp.display(), path.display())
        })?;
        Ok(bytes)
    })();
    if copied.is_err() {
        let _ = fs::remove_file(&temp);
    }
    copied.with_context(|| format!("could not copy {} to {}", from.display(), path.display()))
}

/// A size in bytes for a person, such as `512 B`, `3.4 MB`, or `1.1 GB`.
pub fn human_bytes(bytes: u64) -> String {
    const UNITS: [&str; 4] = ["KB", "MB", "GB", "TB"];
    if bytes < 1000 {
        return format!("{bytes} B");
    }
    let mut value = bytes as f64 / 1000.0;
    let mut unit = 0;
    while value >= 1000.0 && unit + 1 < UNITS.len() {
        value /= 1000.0;
        unit += 1;
    }
    format!("{value:.1} {}", UNITS[unit])
}

/// Creates the folder and its parents. Returns true when the folder did not exist.
pub fn ensure_dir(path: &Path) -> Result<bool> {
    if path.is_dir() {
        return Ok(false);
    }
    fs::create_dir_all(path)
        .with_context(|| format!("could not create the folder {}", path.display()))?;
    Ok(true)
}

/// Reads a file as text, or gives `None` when it does not exist.
pub fn read_optional(path: &Path) -> Result<Option<String>> {
    match fs::read_to_string(path) {
        Ok(text) => Ok(Some(text)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e).with_context(|| format!("could not read {}", path.display())),
    }
}

/// What a change of a file or of a folder changes: its size, the time of its last write to the
/// nanosecond, and its inode. A write in place, a rename over it, and a new or removed entry of
/// a folder each give a new stamp. A reader that keeps what it read compares stamps to know
/// when to read again.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Stamp {
    pub len: u64,
    pub mtime: i64,
    pub mtime_nsec: i64,
    pub ino: u64,
}

/// The stamp of a path, or `None` when nothing is there.
pub fn stamp(path: &Path) -> Option<Stamp> {
    use std::os::unix::fs::MetadataExt;
    let meta = fs::metadata(path).ok()?;
    Some(Stamp {
        len: meta.size(),
        mtime: meta.mtime(),
        mtime_nsec: meta.mtime_nsec(),
        ino: meta.ino(),
    })
}

fn write_new(path: &Path, bytes: &[u8]) -> Result<()> {
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .with_context(|| format!("could not create {}", path.display()))?;
    file.write_all(bytes)?;
    file.sync_all()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn atomic_write_creates_and_replaces() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.json");
        atomic_write(&path, b"one").unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), "one");
        atomic_write(&path, b"two").unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), "two");
        let names: Vec<_> = fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(names, vec!["state.json"], "no temporary file stays");
    }

    #[test]
    fn the_name_of_a_temporary_file_names_its_file() {
        let path = Path::new("/home/tickets/DEMO-12/answer-drafts.json");
        let temp = temp_path(path).unwrap();
        let name = temp.file_name().unwrap().to_str().unwrap();
        assert!(is_temp_of(name, "answer-drafts.json"), "{name}");
        assert!(!is_temp_of(name, "answer-send.json"));
        assert!(!is_temp_of("answer-drafts.json", "answer-drafts.json"));
        assert!(!is_temp_of(".answer-drafts.json", "answer-drafts.json"));
        assert!(!is_temp_of(".answer-drafts.json.tmp", "answer-drafts.json"));
    }

    #[test]
    fn atomic_write_fails_cleanly_without_the_folder() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("missing").join("state.json");
        let err = atomic_write(&path, b"x").unwrap_err();
        assert!(format!("{err:#}").contains("could not write"));
        assert!(!path.exists());
    }

    #[test]
    fn atomic_copy_replaces_the_whole_file_and_leaves_no_temporary_file() {
        let dir = tempfile::tempdir().unwrap();
        let from = dir.path().join("from.bin");
        fs::write(&from, b"new text").unwrap();
        let out = dir.path().join("out");
        fs::create_dir(&out).unwrap();
        let path = out.join("to.bin");
        fs::write(&path, b"old").unwrap();
        assert_eq!(atomic_copy(&from, &path).unwrap(), 8);
        assert_eq!(fs::read(&path).unwrap(), b"new text");
        let names: Vec<_> = fs::read_dir(&out)
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(names, vec!["to.bin"]);
        assert!(atomic_copy(&dir.path().join("none"), &path).is_err());
        assert_eq!(fs::read(&path).unwrap(), b"new text");
    }

    #[test]
    fn human_bytes_picks_a_unit() {
        assert_eq!(human_bytes(512), "512 B");
        assert_eq!(human_bytes(3_400_000), "3.4 MB");
        assert_eq!(human_bytes(1_100_000_000), "1.1 GB");
    }

    #[test]
    fn read_optional_gives_none_for_a_missing_file() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(read_optional(&dir.path().join("none")).unwrap(), None);
    }
}
