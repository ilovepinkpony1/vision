//! File helpers. Each state file is written to a temporary name first and then renamed,
//! so that a reader never sees half a file.

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};

use anyhow::{anyhow, Context, Result};

static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Writes `bytes` to `path` through a temporary file in the same folder and a rename.
/// The folder must exist.
pub fn atomic_write(path: &Path, bytes: &[u8]) -> Result<()> {
    let dir = path
        .parent()
        .filter(|d| !d.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let name = path
        .file_name()
        .ok_or_else(|| anyhow!("cannot write {}: the path has no file name", path.display()))?;
    let counter = TEMP_COUNTER.fetch_add(1, Ordering::Relaxed);
    let temp = dir.join(format!(
        ".{}.{}-{}.tmp",
        name.to_string_lossy(),
        std::process::id(),
        counter
    ));
    let written = write_new(&temp, bytes).and_then(|()| {
        fs::rename(&temp, path)
            .with_context(|| format!("could not rename {} to {}", temp.display(), path.display()))
    });
    if written.is_err() {
        let _ = fs::remove_file(&temp);
    }
    written.with_context(|| format!("could not write {}", path.display()))
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
    fn atomic_write_fails_cleanly_without_the_folder() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("missing").join("state.json");
        let err = atomic_write(&path, b"x").unwrap_err();
        assert!(format!("{err:#}").contains("could not write"));
        assert!(!path.exists());
    }

    #[test]
    fn read_optional_gives_none_for_a_missing_file() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(read_optional(&dir.path().join("none")).unwrap(), None);
    }
}
