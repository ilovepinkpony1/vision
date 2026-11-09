//! Process helpers: is a pid alive, and where is a program.

use std::env;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use crate::paths::expand_tilde;

/// True when a process with this pid exists, even when it belongs to a different user.
pub fn pid_alive(pid: u32) -> bool {
    let Ok(pid) = libc::pid_t::try_from(pid) else {
        return false;
    };
    if pid <= 0 {
        return false;
    }
    // SAFETY: signal 0 sends nothing. It only tests that the process exists.
    let rc = unsafe { libc::kill(pid, 0) };
    rc == 0 || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

/// Finds a program: a path (with `~` expanded) when the name has a `/`, else the first match in `PATH`.
pub fn find_program(name: &str) -> Option<PathBuf> {
    if name.is_empty() {
        return None;
    }
    if name.contains('/') {
        let path = expand_tilde(name).ok()?;
        return is_executable(&path).then_some(path);
    }
    let search = env::var_os("PATH")?;
    env::split_paths(&search)
        .map(|dir| dir.join(name))
        .find(|path| is_executable(path))
}

fn is_executable(path: &Path) -> bool {
    path.metadata()
        .map(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn this_process_is_alive() {
        assert!(pid_alive(std::process::id()));
    }

    #[test]
    fn impossible_pids_are_dead() {
        assert!(!pid_alive(0));
        assert!(!pid_alive(u32::MAX));
    }

    #[test]
    fn finds_sh_in_path() {
        assert!(find_program("sh").is_some());
        assert!(find_program("no-such-program-for-vision-tests").is_none());
        assert!(find_program("/no/such/path").is_none());
    }
}
