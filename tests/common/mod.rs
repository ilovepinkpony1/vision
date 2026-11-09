//! Helpers for the integration tests: a temporary `VISION_HOME`, a temporary `HOME`, and a way
//! to run the built binary in them. A test never touches the real home folder or the real
//! `claude`: `VISION_CLAUDE_BIN` points at a fake that is never run in these tests.

// Each test file uses a different part of these helpers.
#![allow(dead_code)]

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

use tempfile::TempDir;
use vision::paths::Paths;

/// The result of one run of the binary.
#[derive(Debug)]
pub struct Run {
    pub code: i32,
    pub stdout: String,
    pub stderr: String,
}

impl Run {
    /// Panics with the full output when the exit code is not `code`.
    pub fn expect_code(&self, code: i32) -> &Run {
        assert_eq!(self.code, code, "unexpected exit code: {self:#?}");
        self
    }
}

/// A temporary `VISION_HOME` and `HOME`. Both go away when the value drops.
pub struct Home {
    _dir: TempDir,
    /// The `VISION_HOME` of the runs. It does not exist until `vision init`.
    pub vision: PathBuf,
    /// The `HOME` of the runs: an empty folder.
    pub user_home: PathBuf,
    /// A fake Claude Code binary for `VISION_CLAUDE_BIN`.
    pub fake_claude: PathBuf,
}

impl Home {
    /// A home where `vision init` has not run.
    pub fn new() -> Home {
        let dir = tempfile::tempdir().expect("make a temporary folder");
        let user_home = dir.path().join("user");
        std::fs::create_dir(&user_home).expect("make the temporary HOME");
        let fake_claude = dir.path().join("fake-claude");
        std::fs::write(
            &fake_claude,
            "#!/bin/sh\necho 'fake claude: never run in these tests' >&2\nexit 9\n",
        )
        .expect("write the fake claude");
        std::fs::set_permissions(&fake_claude, std::fs::Permissions::from_mode(0o755))
            .expect("chmod the fake");
        Home {
            vision: dir.path().join("vision"),
            user_home,
            fake_claude,
            _dir: dir,
        }
    }

    /// A home after a successful `vision init`.
    pub fn init() -> Home {
        let home = Home::new();
        home.run(&["init"]).expect_code(0);
        home
    }

    /// Runs `vision <args>` in this home.
    pub fn run(&self, args: &[&str]) -> Run {
        self.run_with(args, &[])
    }

    /// Runs `vision <args>` in this home with more environment values.
    pub fn run_with(&self, args: &[&str], env: &[(&str, &str)]) -> Run {
        let mut command = Command::new(env!("CARGO_BIN_EXE_vision"));
        command
            .args(args)
            .env_clear()
            .env("PATH", std::env::var_os("PATH").unwrap_or_default())
            .env("HOME", &self.user_home)
            .env("VISION_HOME", &self.vision)
            .env("VISION_CLAUDE_BIN", &self.fake_claude);
        if let Some(tmp) = std::env::var_os("TMPDIR") {
            command.env("TMPDIR", tmp);
        }
        for (name, value) in env {
            command.env(name, value);
        }
        let output = command.output().expect("run the vision binary");
        Run {
            code: output.status.code().unwrap_or(-1),
            stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        }
    }

    /// A path in `VISION_HOME`.
    pub fn path(&self, relative: &str) -> PathBuf {
        self.vision.join(relative)
    }

    /// The text of a file in `VISION_HOME`.
    pub fn read(&self, relative: &str) -> String {
        let path = self.path(relative);
        std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
    }

    /// The paths of this home, for the library API.
    pub fn paths(&self) -> Paths {
        Paths::new(&self.vision)
    }

    /// The parsed `state.json`.
    pub fn snapshot(&self) -> serde_json::Value {
        serde_json::from_str(&self.read("state.json")).expect("state.json is JSON")
    }
}

/// Runs git in `dir` and gives its standard output.
pub fn git(dir: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .expect("run git");
    assert!(
        output.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout)
        .trim_end()
        .to_string()
}
