//! The archive in an object store (section 16), with the fake `aws` of `tests/bin/fake-aws`.
//! The fake keeps the objects as files in a temporary folder. No test reaches the real AWS.

mod common;

use std::path::{Path, PathBuf};

use common::{Home, Run};
use serde_json::{json, Value};
use vision::ledger::turns::{NewTurn, TurnEnd};
use vision::ledger::{Ledger, TicketRow};
use vision::paths::{TicketKey, TurnId};
use vision::util::time;

/// A home with the archive on, and a folder with the fake `aws` first in `PATH`.
struct World {
    home: Home,
    _tools: tempfile::TempDir,
    bin: PathBuf,
    aws_dir: PathBuf,
}

impl World {
    fn new() -> World {
        let home = Home::init();
        let tools = tempfile::tempdir().unwrap();
        let bin = tools.path().join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        std::os::unix::fs::symlink(
            Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/bin/fake-aws"),
            bin.join("aws"),
        )
        .unwrap();
        let aws_dir = tools.path().join("aws");
        let world = World {
            home,
            bin,
            aws_dir,
            _tools: tools,
        };
        world.edit_config(|doc| {
            doc["archive"]["bucket"] = toml_edit::value("test-bucket");
            doc["archive"]["endpoint"] = toml_edit::value("https://store.example.com");
            doc["archive"]["profile"] = toml_edit::value("archive");
        });
        world
    }

    fn edit_config(&self, change: impl FnOnce(&mut toml_edit::DocumentMut)) {
        let path = self.home.path("config.toml");
        let mut doc: toml_edit::DocumentMut =
            std::fs::read_to_string(&path).unwrap().parse().unwrap();
        change(&mut doc);
        std::fs::write(&path, doc.to_string()).unwrap();
    }

    fn path_env(&self) -> String {
        format!(
            "{}:{}",
            self.bin.display(),
            std::env::var("PATH").unwrap_or_default()
        )
    }

    fn run(&self, args: &[&str]) -> Run {
        self.run_with(args, &[])
    }

    fn run_with(&self, args: &[&str], more: &[(&str, &str)]) -> Run {
        let path = self.path_env();
        let aws = self.aws_dir.display().to_string();
        let mut env = vec![("PATH", path.as_str()), ("FAKE_AWS_DIR", aws.as_str())];
        env.extend_from_slice(more);
        self.home.run_with(args, &env)
    }

    fn ledger(&self) -> Ledger {
        Ledger::open(&self.home.path("ledger.sqlite")).unwrap()
    }

    /// The calls that the fake `aws` got.
    fn calls(&self) -> Vec<String> {
        std::fs::read_to_string(self.aws_dir.join("calls.log"))
            .unwrap_or_default()
            .lines()
            .map(str::to_string)
            .collect()
    }

    fn object(&self, key: &str) -> PathBuf {
        self.aws_dir.join("store/test-bucket").join(key)
    }

    /// A ticket row, finished or not, with no step, so that the pipeline leaves it alone.
    fn ticket(&self, key: &str, finished: bool) {
        let counts = finished.then(|| json!({"finished": "2026-11-01T00:00:00Z"}).to_string());
        self.ledger()
            .put_ticket(&TicketRow {
                key: key.into(),
                counts,
                ..TicketRow::default()
            })
            .unwrap();
    }

    /// One ended turn with a folder. Gives its id.
    fn turn(&self, key: &str, step: &str, n: u32, ended_minutes_ago: i64) -> String {
        let ended = time::now() - ::time::Duration::minutes(ended_minutes_ago);
        let started = ended - ::time::Duration::minutes(5);
        let ticket = TicketKey::parse(key).unwrap();
        let id = TurnId::new(started, &ticket, step, n).unwrap().to_string();
        let ledger = self.ledger();
        ledger
            .insert_turn(&NewTurn {
                id: &id,
                ticket: key,
                agent: "builder",
                step,
                kind: "worker",
                session: &format!("s-{id}"),
                resumes: None,
                model: "claude-opus-5-5",
                effort: "xhigh",
                started: &time::rfc3339(started),
                role_hash: "role",
            })
            .unwrap();
        ledger
            .end_turn(
                &id,
                &TurnEnd {
                    status: "done".into(),
                    reason: "result".into(),
                    ended: time::rfc3339(ended),
                    ..TurnEnd::default()
                },
            )
            .unwrap();
        let turn = self
            .home
            .paths()
            .turn(&ticket, &TurnId::parse(&id).unwrap());
        std::fs::create_dir_all(&turn.dir).unwrap();
        std::fs::write(
            &turn.result,
            json!({"kind": "result", "summary": format!("Did {step}.")}).to_string(),
        )
        .unwrap();
        std::fs::write(&turn.stream, "{\"type\":\"system\",\"subtype\":\"init\"}\n").unwrap();
        id
    }

    fn turn_dir(&self, key: &str, id: &str) -> PathBuf {
        self.home.path(&format!("tickets/{key}/turns/{id}"))
    }
}

fn tar_list(file: &Path) -> Vec<String> {
    let output = std::process::Command::new("tar")
        .arg("-tzf")
        .arg(file)
        .output()
        .unwrap();
    assert!(output.status.success());
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(|l| l.trim_end_matches('/').to_string())
        .collect()
}

#[test]
fn the_archive_uploads_confirms_deletes_when_due_and_fetches_a_folder_back() {
    let w = World::new();
    w.ticket("DONE-1", true);
    w.ticket("ACTIVE-2", false);
    w.ticket("OLD-3", false);
    let done = w.turn("DONE-1", "build", 1, 120);
    let recent = w.turn("ACTIVE-2", "build", 1, 120);
    let old = w.turn("OLD-3", "build", 1, 3 * 24 * 60);
    w.run(&["journal", "rebuild"]).expect_code(0);

    // A dry run names what waits and calls nothing.
    let dry = w.run(&["archive", "--dry-run"]);
    dry.expect_code(0);
    assert!(dry.stdout.starts_with("2 turn folder(s) and 3 entr(y/ies) wait for the upload; 0 turn folder(s) are due for deletion. Nothing changed"), "{dry:?}");
    assert!(
        dry.stdout
            .contains(&format!("upload  vision/DONE-1/{done}.tar.gz")),
        "{dry:?}"
    );
    assert!(
        dry.stdout
            .contains(&format!("upload  vision/OLD-3/{old}.tar.gz")),
        "{dry:?}"
    );
    assert!(
        !dry.stdout.contains(&format!("{recent}.tar.gz")),
        "a recent turn of a ticket that moves waits: {dry:?}"
    );
    assert!(
        dry.stdout
            .contains(&format!("upload  vision/journal/ACTIVE-2/{recent}.md")),
        "{dry:?}"
    );
    assert!(w.calls().is_empty(), "{:?}", w.calls());

    let first = w.run(&["archive"]);
    first.expect_code(0);
    assert!(
        first
            .stdout
            .starts_with("2 turn folder(s) and 3 entr(y/ies) went up; 0 turn folder(s) deleted"),
        "{first:?}"
    );
    let calls = w.calls();
    let copy = format!(
        "s3 cp {} s3://test-bucket/vision/DONE-1/{done}.tar.gz --only-show-errors --endpoint-url https://store.example.com --profile archive",
        std::env::temp_dir().display()
    );
    assert!(
        calls.iter().any(|c| c.starts_with("s3 cp ") && c.ends_with(&format!("s3://test-bucket/vision/DONE-1/{done}.tar.gz --only-show-errors --endpoint-url https://store.example.com --profile archive"))),
        "{calls:?} (like {copy})"
    );
    assert!(
        calls.contains(&format!("s3api head-object --bucket test-bucket --key vision/DONE-1/{done}.tar.gz --output json --endpoint-url https://store.example.com --profile archive")),
        "{calls:?}"
    );
    let tar = w.object(&format!("vision/DONE-1/{done}.tar.gz"));
    let members = tar_list(&tar);
    assert!(members.contains(&done), "{members:?}");
    assert!(
        members.contains(&format!("{done}/result.json")),
        "{members:?}"
    );
    assert!(
        members
            .iter()
            .all(|m| m == &done || m.starts_with(&format!("{done}/"))),
        "{members:?}"
    );
    assert_eq!(
        std::fs::read_to_string(w.object(&format!("vision/journal/DONE-1/{done}.md"))).unwrap(),
        w.home.read(&format!("journal/DONE-1/{done}.md"))
    );
    let row = w.ledger().journal_row(&done).unwrap().unwrap();
    assert!(row.archived.is_some());
    assert_eq!(
        row.archive_bytes,
        Some(std::fs::metadata(&tar).unwrap().len() as i64)
    );
    assert_eq!(row.entry_sent, row.hash);
    assert!(w
        .ledger()
        .journal_row(&recent)
        .unwrap()
        .unwrap()
        .archived
        .is_none());

    // A second run has nothing to send.
    let count = w.calls().len();
    let again = w.run(&["archive"]);
    again.expect_code(0);
    assert!(
        again
            .stdout
            .starts_with("0 turn folder(s) and 0 entr(y/ies) went up"),
        "{again:?}"
    );
    assert_eq!(w.calls().len(), count);

    // With keep_days = 0 the folder of the finished ticket goes; the folder of a ticket that
    // still moves stays, and each journal entry stays.
    w.edit_config(|doc| doc["archive"]["keep_days"] = toml_edit::value(0));
    let due = w.run(&["archive"]);
    due.expect_code(0);
    assert!(due.stdout.contains("; 1 turn folder(s) deleted"), "{due:?}");
    assert!(!w.turn_dir("DONE-1", &done).exists());
    assert!(w.turn_dir("OLD-3", &old).exists());
    assert!(w.home.path(&format!("journal/DONE-1/{done}.md")).is_file());
    assert!(w
        .ledger()
        .journal_row(&done)
        .unwrap()
        .unwrap()
        .removed
        .is_some());
    // A new fact of the ledger does not rewrite an entry whose turn folder is gone.
    let kept = w.home.read(&format!("journal/DONE-1/{done}.md"));
    w.ledger().set_end_fact(&done, "held").unwrap();
    w.run(&["tick"]).expect_code(0);
    assert_eq!(w.home.read(&format!("journal/DONE-1/{done}.md")), kept);
    assert!(kept.contains("Did build."), "{kept}");

    let status = w.run(&["archive", "status", "--json"]);
    status.expect_code(0);
    let status: Value = serde_json::from_str(&status.stdout).unwrap();
    assert_eq!(status["on"], true);
    assert_eq!(status["archived"], 2);
    assert_eq!(status["removed"], 1);
    assert_eq!(status["entries_sent"], 3);
    assert_eq!(status["waiting"], 0);
    assert!(status["last_run"].is_string());

    // Fetch brings the folder back, and the countdown starts again.
    let fetched = w.run(&["archive", "fetch", &done]);
    fetched.expect_code(0);
    assert!(fetched.stdout.contains("is back at"), "{fetched:?}");
    assert!(w.turn_dir("DONE-1", &done).join("result.json").is_file());
    assert!(w
        .ledger()
        .journal_row(&done)
        .unwrap()
        .unwrap()
        .removed
        .is_none());
    let twice = w.run(&["archive", "fetch", &done]);
    twice.expect_code(1);
    assert!(twice.stderr.contains("is already here"), "{twice:?}");
}

#[test]
fn a_size_that_differs_in_the_store_marks_nothing_archived() {
    let w = World::new();
    w.ticket("DONE-1", true);
    let done = w.turn("DONE-1", "build", 1, 30);
    let short = w.run_with(&["archive"], &[("FAKE_AWS_SHORT", "1")]);
    short.expect_code(1);
    assert!(short.stdout.contains("the store holds"), "{short:?}");
    assert!(w
        .ledger()
        .journal_row(&done)
        .unwrap()
        .is_none_or(|r| r.archived.is_none()));
    // The folder stays, because nothing was confirmed.
    w.edit_config(|doc| doc["archive"]["keep_days"] = toml_edit::value(0));
    let next = w.run(&["archive"]);
    next.expect_code(0);
    assert!(next.stdout.contains("1 turn folder(s)"), "{next:?}");
    assert!(
        w.turn_dir("DONE-1", &done).exists(),
        "a folder goes only one run after the confirmation"
    );
}

#[test]
fn the_tick_uploads_at_most_once_an_hour_and_deletes_nothing() {
    let w = World::new();
    w.edit_config(|doc| doc["archive"]["keep_days"] = toml_edit::value(0));
    w.ticket("DONE-1", true);
    let done = w.turn("DONE-1", "build", 1, 30);
    w.run(&["tick"]).expect_code(0);
    assert!(w.object(&format!("vision/DONE-1/{done}.tar.gz")).is_file());
    assert!(
        w.object(&format!("vision/journal/DONE-1/{done}.md"))
            .is_file(),
        "the tick wrote the entry first"
    );
    let count = w.calls().len();
    w.run(&["tick"]).expect_code(0);
    assert_eq!(
        w.calls().len(),
        count,
        "the second tick in the hour calls nothing"
    );
    assert!(
        w.turn_dir("DONE-1", &done).exists(),
        "the tick deletes nothing"
    );
    assert!(w.ledger().last_event_at("archive-run").unwrap().is_some());
}

#[test]
fn the_doctor_tests_the_prefix_of_the_archive() {
    let w = World::new();
    let line = |run: &Run| {
        run.stdout
            .lines()
            .find(|l| l.contains(" archive "))
            .unwrap()
            .to_string()
    };
    let ok = w.run(&["doctor"]);
    let ok = line(&ok);
    assert!(ok.starts_with("ok "), "{ok}");
    assert!(
        ok.ends_with("s3://test-bucket/vision/ answers, and it holds nothing yet"),
        "{ok}"
    );
    assert!(w.calls().contains(&"s3 ls s3://test-bucket/vision/ --max-items 1 --endpoint-url https://store.example.com --profile archive".to_string()), "{:?}", w.calls());
    let failed = w.run_with(&["doctor"], &[("FAKE_AWS_LS_FAIL", "1")]);
    failed.expect_code(1);
    let failed = line(&failed);
    assert!(failed.starts_with("fail"), "{failed}");
    assert!(failed.contains("Unable to locate credentials"), "{failed}");
    let missing = w.home.run_with(&["doctor"], &[("PATH", "/usr/bin:/bin")]);
    let missing = line(&missing);
    assert!(missing.starts_with("fail"), "{missing}");
    assert!(
        missing.contains("the aws program is not found"),
        "{missing}"
    );
    let nothing = w.run(&["archive"]);
    nothing.expect_code(0);
    w.edit_config(|doc| doc["archive"]["bucket"] = toml_edit::value(""));
    let refused = w.run(&["archive"]);
    refused.expect_code(1);
    assert!(refused.stderr.contains("the archive is off"), "{refused:?}");
}
