//! The worker with a fake program: a shell script that the test writes in the place of the
//! binary. It logs the start and the end of each run. A pass waits while a file exists, so a
//! test holds a pass as long as it wants and waits for facts, never for a time.

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, Sender};
use std::time::{Duration, Instant};

use tempfile::TempDir;
use vision::app::model::{App, Card, PassEnd, Run};
use vision::app::msg::{Action, Effect, Job, JobKind, Key, Msg};
use vision::app::worker::{self, WorkerConfig};

use super::support;

const FAKE: &str = r#"#!/bin/sh
dir="$(dirname "$0")"
echo "start $*" >> "$dir/log"
case "$1" in
  tick)
    if [ -f "$dir/lock-held" ]; then
      echo "tick skipped: another tick holds the lock"
    else
      while [ -f "$dir/hold" ]; do sleep 0.02; done
      echo "tick done in 5 ms"
    fi
    ;;
  take)
    if [ "$2" = "DEMO-99" ]; then
      echo "error: DEMO-99: the queue of the tracker has no ticket DEMO-99. \`vision pick\` lists the queue" >&2
      echo "end $*" >> "$dir/log"
      exit 1
    fi
    echo "Took $2."
    ;;
  off) echo "Vision is off." ;;
  on) echo "Vision is on." ;;
esac
echo "home $VISION_HOME" >> "$dir/log"
echo "end $*" >> "$dir/log"
"#;

/// A folder with the fake program, and a worker that runs it.
struct Fake {
    dir: TempDir,
    jobs: Sender<Job>,
    rx: Receiver<Msg>,
    app: App,
    t: f64,
}

impl Fake {
    fn new() -> Fake {
        let dir = tempfile::tempdir().unwrap();
        let exe = dir.path().join("vision");
        std::fs::write(&exe, FAKE).unwrap();
        std::fs::set_permissions(&exe, std::fs::Permissions::from_mode(0o755)).unwrap();
        let (tx, rx) = mpsc::channel();
        let config = WorkerConfig {
            exe,
            home: dir.path().join("home"),
            env: Some(vec![(
                "PATH".into(),
                std::env::var_os("PATH").unwrap_or_default(),
            )]),
        };
        let jobs = worker::spawn(config, tx).unwrap();
        Fake {
            app: support::home((110, 32)),
            dir,
            jobs,
            rx,
            t: 10.0,
        }
    }

    fn path(&self, name: &str) -> PathBuf {
        self.dir.path().join(name)
    }

    fn log(&self) -> Vec<String> {
        std::fs::read_to_string(self.path("log"))
            .unwrap_or_default()
            .lines()
            .filter(|l| !l.starts_with("home "))
            .map(str::to_string)
            .collect()
    }

    /// One message to the update at a later time; each job of its effects goes to the worker.
    fn send(&mut self, at: f64, msg: Msg) -> Vec<Effect> {
        self.t = at;
        let fx = support::send(&mut self.app, at, msg);
        for effect in &fx {
            if let Effect::Run(job) = effect {
                self.jobs.send(job.clone()).unwrap();
            }
        }
        fx
    }

    /// Waits for the end of the next job and hands it to the update.
    fn next_done(&mut self) -> Vec<Effect> {
        let msg = self
            .rx
            .recv_timeout(Duration::from_secs(60))
            .expect("the worker ended no job");
        assert!(matches!(msg, Msg::JobDone(_)), "{msg:?}");
        let at = self.t + 0.1;
        self.send(at, msg)
    }

    /// Waits until the log has a line.
    fn wait_for_line(&self, line: &str) {
        let until = Instant::now() + Duration::from_secs(60);
        while !self.log().iter().any(|l| l == line) {
            assert!(Instant::now() < until, "no line {line:?}: {:?}", self.log());
            std::thread::sleep(Duration::from_millis(20));
        }
    }
}

fn touch(path: &Path) {
    std::fs::write(path, "").unwrap();
}

#[test]
fn an_action_waits_behind_a_long_pass_and_the_worker_runs_one_job_at_a_time() {
    let mut fake = Fake::new();
    touch(&fake.path("hold"));
    let fx = fake.send(61.0, Msg::Tick);
    assert!(fx
        .iter()
        .any(|e| matches!(e, Effect::Run(j) if j.kind == JobKind::Pass)));
    fake.wait_for_line("start tick");
    // The person asks for the kill switch while the pass runs: it waits with a notice.
    fake.send(62.0, Msg::Key(Key::Char('K')));
    let fx = fake.send(62.1, Msg::Key(Key::Enter));
    assert!(!fx.iter().any(|e| matches!(e, Effect::Run(_))), "{fx:?}");
    assert!(fake
        .app
        .notice
        .as_ref()
        .unwrap()
        .text
        .contains("waits for the pass"));
    // The header shows the pass and its time after 2 seconds.
    fake.send(70.0, Msg::Tick);
    assert!(support::screen(&fake.app).contains("pass 0:09"));
    std::fs::remove_file(fake.path("hold")).unwrap();
    let fx = fake.next_done();
    assert!(
        fx.iter()
            .any(|e| matches!(e, Effect::Run(j) if j.kind == JobKind::Action(Action::Off))),
        "{fx:?}"
    );
    let fx = fake.next_done();
    assert_eq!(
        fake.app.notice.as_ref().unwrap().text,
        "Vision is off. Each running turn stopped. Press K to turn it on again."
    );
    assert_eq!(fake.app.card, None);
    // A pass follows the action.
    assert!(fx
        .iter()
        .any(|e| matches!(e, Effect::Run(j) if j.kind == JobKind::Pass)));
    fake.next_done();
    assert_eq!(
        fake.log(),
        [
            "start tick",
            "end tick",
            "start off",
            "end off",
            "start tick",
            "end tick"
        ]
    );
    let home = std::fs::read_to_string(fake.path("log")).unwrap();
    assert!(
        home.contains(&format!("home {}", fake.path("home").display())),
        "each child gets VISION_HOME: {home}"
    );
}

#[test]
fn a_failed_action_keeps_its_card_with_the_text_of_its_standard_error() {
    let mut fake = Fake::new();
    let mut card = super::snapshots::start_card();
    if let Some(Card::Start(start)) = card.card.as_mut() {
        start.key = "DEMO-99".into();
    }
    fake.app = card;
    let fx = fake.send(20.0, Msg::Key(Key::Enter));
    assert!(fx.iter().any(|e| matches!(e, Effect::Run(_))));
    fake.next_done();
    let Some(Card::Start(start)) = &fake.app.card else {
        panic!("{:?}", fake.app.card)
    };
    assert_eq!(
        start.run,
        Run::Failed(
            "DEMO-99: the queue of the tracker has no ticket DEMO-99. `vision pick` lists the queue"
                .into()
        )
    );
}

#[test]
fn a_pass_while_another_tick_holds_the_lock_is_not_an_error() {
    let mut fake = Fake::new();
    touch(&fake.path("lock-held"));
    fake.send(61.0, Msg::Tick);
    fake.next_done();
    assert_eq!(fake.app.engine.last_pass, Some(PassEnd::Skipped));
    assert_eq!(fake.app.notice, None);
}

#[test]
fn a_paused_engine_runs_no_pass_and_still_runs_an_action() {
    let mut fake = Fake::new();
    fake.send(20.0, Msg::Key(Key::Char('p')));
    let fx = fake.send(200.0, Msg::Tick);
    assert!(!fx.iter().any(|e| matches!(e, Effect::Run(_))));
    fake.send(201.0, Msg::Key(Key::Char('K')));
    fake.send(201.1, Msg::Key(Key::Enter));
    fake.next_done();
    assert_eq!(fake.log(), ["start off", "end off"]);
}

#[test]
fn a_program_that_does_not_start_gives_a_reason() {
    let config = WorkerConfig {
        exe: PathBuf::from("/nonexistent/vision"),
        home: PathBuf::from("/nonexistent/home"),
        env: None,
    };
    let result = worker::run_child(&config, &["tick".to_string()]);
    assert!(!result.ok());
    assert!(
        result
            .failure()
            .starts_with("could not start /nonexistent/vision tick"),
        "{}",
        result.failure()
    );
}
