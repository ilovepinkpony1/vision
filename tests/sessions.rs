//! The archive of the sessions of Claude Code (section 16). The session files here are written
//! by the test in the temporary `HOME` (`~/.claude/projects` of the run), with invented text in
//! the shapes of the real events. The archive is a temporary folder, or the fake `aws` of
//! `tests/bin/fake-aws`, and MemPalace is the fake of `tests/journal_support/palace.rs`. No test
//! reaches the real home, the network, AWS, or MemPalace.

mod common;
#[allow(dead_code)]
#[path = "journal_support/palace.rs"]
mod palace;
#[allow(dead_code)]
#[path = "jira_github_support/server.rs"]
mod server;

use std::collections::BTreeMap;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use common::{Home, Run};
use palace::fake_palace;
use serde_json::{json, Value};
use vision::ledger::Ledger;

const SECRET_VALUE: &str = "sessions-fake-secret-9a8b7c6d";
const SECRET_ENV: (&str, &str) = ("DEMO_TOKEN_SOURCE", SECRET_VALUE);
const PROJECT: &str = "-home-tester-work";
const SESSION: &str = "0b1c2d3e-4f50-4617-8a9b-0c1d2e3f4a5b";
const OTHER: &str = "7a6b5c4d-3e2f-4011-9223-344556677889";

/// A personal access token in its common form, built so that no token sits in the source.
fn fake_token() -> String {
    format!("{}{}", "ghp_", "Z".repeat(36))
}

/// A home with one secret, the archive of the sessions on, a folder of projects of Claude Code
/// in its `HOME`, and a folder with the fake `aws` first in `PATH`.
struct World {
    home: Home,
    _tools: tempfile::TempDir,
    bin: PathBuf,
    aws_dir: PathBuf,
    /// The local archive folder, when the archive uses one.
    archive: PathBuf,
}

impl World {
    /// The archive in a local folder.
    fn local() -> World {
        let world = World::bare();
        let dir = world.archive.display().to_string();
        world.edit_config(|doc| {
            doc["archive"]["dir"] = toml_edit::value(dir);
            doc["sessions"]["enabled"] = toml_edit::value(true);
        });
        world
    }

    /// The archive in the bucket of the fake `aws`.
    fn bucket() -> World {
        let world = World::bare();
        world.edit_config(|doc| {
            doc["archive"]["bucket"] = toml_edit::value("test-bucket");
            doc["sessions"]["enabled"] = toml_edit::value(true);
        });
        world
    }

    fn bare() -> World {
        let home = Home::init();
        let tools = tempfile::tempdir().unwrap();
        let bin = tools.path().join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        std::os::unix::fs::symlink(
            Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/bin/fake-aws"),
            bin.join("aws"),
        )
        .unwrap();
        let world = World {
            aws_dir: tools.path().join("aws"),
            archive: tools.path().join("archive"),
            home,
            bin,
            _tools: tools,
        };
        world.edit_config(|doc| {
            let mut secret = toml_edit::InlineTable::new();
            secret.insert("env", SECRET_ENV.0.into());
            doc["secrets"]["DEMO_TOKEN"] = toml_edit::value(secret);
        });
        std::fs::create_dir_all(world.projects()).unwrap();
        world
    }

    fn edit_config(&self, change: impl FnOnce(&mut toml_edit::DocumentMut)) {
        let path = self.home.path("config.toml");
        let mut doc: toml_edit::DocumentMut =
            std::fs::read_to_string(&path).unwrap().parse().unwrap();
        change(&mut doc);
        std::fs::write(&path, doc.to_string()).unwrap();
    }

    /// `~/.claude/projects` of the runs: the default of `sessions.projects`.
    fn projects(&self) -> PathBuf {
        self.home.user_home.join(".claude").join("projects")
    }

    fn run(&self, args: &[&str]) -> Run {
        self.run_with(args, &[])
    }

    fn run_with(&self, args: &[&str], more: &[(&str, &str)]) -> Run {
        let path = format!(
            "{}:{}",
            self.bin.display(),
            std::env::var("PATH").unwrap_or_default()
        );
        let aws = self.aws_dir.display().to_string();
        let mut env = vec![
            ("PATH", path.as_str()),
            ("FAKE_AWS_DIR", aws.as_str()),
            SECRET_ENV,
        ];
        env.extend_from_slice(more);
        self.home.run_with(args, &env)
    }

    fn ledger(&self) -> Ledger {
        Ledger::open(&self.home.path("ledger.sqlite")).unwrap()
    }

    fn aws_calls(&self) -> Vec<String> {
        std::fs::read_to_string(self.aws_dir.join("calls.log"))
            .unwrap_or_default()
            .lines()
            .map(str::to_string)
            .collect()
    }

    /// Writes a session file under the project and gives it a change time `hours_ago`.
    fn session(&self, rel: &str, lines: &[Value], hours_ago: u64) -> PathBuf {
        let path = self.projects().join(PROJECT).join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let text: String = lines.iter().map(|l| format!("{l}\n")).collect();
        std::fs::write(&path, text).unwrap();
        set_age(&path, hours_ago);
        path
    }

    fn entry(&self, session: &str) -> PathBuf {
        self.home
            .path(&format!("session-journal/{PROJECT}/{session}.md"))
    }
}

fn set_age(path: &Path, hours_ago: u64) {
    let at = SystemTime::now() - Duration::from_secs(hours_ago * 3600);
    std::fs::File::options()
        .append(true)
        .open(path)
        .unwrap()
        .set_modified(at)
        .unwrap();
}

/// Each file under a folder with its size, its change time, and its bytes.
fn picture(root: &Path) -> BTreeMap<PathBuf, (u64, SystemTime, Vec<u8>)> {
    let mut out = BTreeMap::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir).unwrap().flatten() {
            let meta = entry.path().symlink_metadata().unwrap();
            if meta.is_dir() {
                out.insert(entry.path(), (0, meta.modified().unwrap(), Vec::new()));
                stack.push(entry.path());
            } else {
                out.insert(
                    entry.path(),
                    (
                        meta.len(),
                        meta.modified().unwrap(),
                        std::fs::read(entry.path()).unwrap(),
                    ),
                );
            }
        }
    }
    out
}

/// Makes each folder and file under a root read-only while it lives, and writable again when
/// it goes, so that a write into the folder of Claude Code fails loudly.
struct ReadOnly(PathBuf);

impl ReadOnly {
    fn new(root: &Path) -> ReadOnly {
        set_modes(root, 0o555, 0o444);
        ReadOnly(root.to_path_buf())
    }
}

impl Drop for ReadOnly {
    fn drop(&mut self) {
        set_modes(&self.0, 0o755, 0o644);
    }
}

fn set_modes(root: &Path, dir_mode: u32, file_mode: u32) {
    let mut stack = vec![root.to_path_buf()];
    let mut dirs = Vec::new();
    while let Some(dir) = stack.pop() {
        // A folder must be readable and searchable before its entries can be listed.
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).unwrap();
        dirs.push(dir.clone());
        for entry in std::fs::read_dir(&dir).unwrap().flatten() {
            let meta = entry.path().symlink_metadata().unwrap();
            if meta.is_dir() {
                stack.push(entry.path());
            } else {
                std::fs::set_permissions(entry.path(), std::fs::Permissions::from_mode(file_mode))
                    .unwrap();
            }
        }
    }
    for dir in dirs.iter().rev() {
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(dir_mode)).unwrap();
    }
}

fn gunzip(path: &Path) -> String {
    let output = std::process::Command::new("gzip")
        .arg("-d")
        .arg("-c")
        .arg(path)
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    String::from_utf8(output.stdout).unwrap()
}

/// The events of a session in the shapes of the real format: typed messages, tool calls and
/// their results, and each kind of line that the harness adds as a user line.
fn session_lines(id: &str) -> Vec<Value> {
    let cwd = "/home/tester/work";
    let base = |kind: &str, at: &str| {
        json!({"type": kind, "sessionId": id, "timestamp": at, "cwd": cwd,
               "gitBranch": "main", "isSidechain": false, "userType": "external",
               "uuid": format!("u-{at}"), "version": "2.1.0"})
    };
    let with = |mut value: Value, more: Value| {
        for (k, v) in more.as_object().unwrap() {
            value[k] = v.clone();
        }
        value
    };
    let typed = |at: &str, text: &str| {
        with(
            base("user", at),
            json!({"origin": {"kind": "human"}, "promptSource": "typed",
                   "message": {"role": "user", "content": text}}),
        )
    };
    let call = |at: &str, id: &str, name: &str, input: Value| {
        with(
            base("assistant", at),
            json!({"message": {"model": "claude-test", "role": "assistant", "content": [
                {"type": "thinking", "thinking": "THINKING-MARKER"},
                {"type": "tool_use", "id": id, "name": name, "input": input}]}}),
        )
    };
    let result = |at: &str, id: &str, text: &str, error: bool| {
        with(
            base("user", at),
            json!({"message": {"role": "user", "content": [
                {"type": "tool_result", "tool_use_id": id, "content": text, "is_error": error}]},
                "toolUseResult": {"stdout": text}, "sourceToolAssistantUUID": "a"}),
        )
    };
    let token = fake_token();
    vec![
        json!({"type": "last-prompt", "leafUuid": "u-9", "sessionId": id}),
        json!({"type": "permission-mode", "permissionMode": "default", "sessionId": id}),
        typed(
            "2026-11-02T10:00:00.000Z",
            "Add a CSV export to the report page\nwith a header row",
        ),
        with(
            base("attachment", "2026-11-02T10:00:01.000Z"),
            json!({"attachment": {"type": "hook", "content": "ATTACHMENT-MARKER"}}),
        ),
        with(
            base("user", "2026-11-02T10:00:02.000Z"),
            json!({"isMeta": true, "message": {"role": "user", "content": [
                {"type": "text", "text": "<system-reminder>REMINDER-MARKER</system-reminder>"}]}}),
        ),
        call(
            "2026-11-02T10:01:00.000Z",
            "t1",
            "Read",
            json!({"file_path": "/home/tester/work/src/report.rs"}),
        ),
        result(
            "2026-11-02T10:01:01.000Z",
            "t1",
            "TOOL-RESULT-MARKER fn report() {}",
            false,
        ),
        call(
            "2026-11-02T10:02:00.000Z",
            "t2",
            "Bash",
            json!({"command": "cargo test export"}),
        ),
        result(
            "2026-11-02T10:02:30.000Z",
            "t2",
            "Exit code 101\nerror[E0425]: cannot find value `rows` in this scope",
            true,
        ),
        call(
            "2026-11-02T10:03:00.000Z",
            "t3",
            "Edit",
            json!({"file_path": "/home/tester/work/src/export.rs", "old_string": "a", "new_string": "b"}),
        ),
        result(
            "2026-11-02T10:03:01.000Z",
            "t3",
            "The file was updated.",
            false,
        ),
        call(
            "2026-11-02T10:04:00.000Z",
            "t4",
            "Bash",
            json!({"command": "git add -A && git commit -m \"Add the CSV export\""}),
        ),
        result(
            "2026-11-02T10:04:01.000Z",
            "t4",
            "[feature/csv-export 4d5e6f7] Add the CSV export\n 2 files changed",
            false,
        ),
        call(
            "2026-11-02T10:05:00.000Z",
            "t5",
            "Bash",
            json!({"command": "cargo test export"}),
        ),
        result("2026-11-02T10:05:30.000Z", "t5", "test result: ok", false),
        with(
            base("user", "2026-11-02T10:06:00.000Z"),
            json!({"origin": {"kind": "task-notification"}, "promptSource": "system",
                   "message": {"role": "user", "content":
                   "<task-notification><task-id>b1</task-id><summary>TASK-MARKER</summary></task-notification>"}}),
        ),
        with(
            base("user", "2026-11-02T10:07:00.000Z"),
            json!({"message": {"role": "user", "content":
                "<command-name>/clear</command-name>\n<command-message>clear</command-message>\n<command-args></command-args>"}}),
        ),
        with(
            base("user", "2026-11-02T10:07:01.000Z"),
            json!({"message": {"role": "user", "content":
                "<local-command-stdout>LOCAL-MARKER</local-command-stdout>"}}),
        ),
        typed(
            "2026-11-02T10:08:00.000Z",
            &format!("Upload it with the key {SECRET_VALUE} or {token}"),
        ),
        with(
            base("assistant", "2026-11-02T10:09:00.000Z"),
            json!({"message": {"model": "claude-test", "role": "assistant", "content": [
                {"type": "text", "text": format!("The CSV export works. I did not keep {token}.")}]}}),
        ),
        json!({"type": "ai-title", "aiTitle": "CSV export of the report", "sessionId": id}),
    ]
}

/// The lines of a subagent of the session: its prompt is a user line of a side chain.
fn subagent_lines(id: &str) -> Vec<Value> {
    vec![
        json!({"type": "user", "isSidechain": true, "sessionId": id, "agentId": "a1",
               "timestamp": "2026-11-02T10:02:10.000Z",
               "message": {"role": "user", "content": "SUBAGENT-PROMPT-MARKER"}}),
        json!({"type": "assistant", "isSidechain": true, "sessionId": id, "agentId": "a1",
               "timestamp": "2026-11-02T10:02:20.000Z",
               "message": {"role": "assistant", "content": [{"type": "text", "text": "Found it."}]}}),
    ]
}

#[test]
fn a_finished_session_goes_to_the_folder_once_with_a_summary_that_keeps_only_what_the_person_typed()
{
    let w = World::local();
    let main = w.session(&format!("{SESSION}.jsonl"), &session_lines(SESSION), 48);
    let sub = w.session(
        &format!("{SESSION}/subagents/agent-a1.jsonl"),
        &subagent_lines(SESSION),
        48,
    );
    w.session(&format!("{SESSION}/subagents/agent-a1.meta.json"), &[], 48);
    let recent = w.session(&format!("{OTHER}.jsonl"), &session_lines(OTHER), 1);
    let before = picture(&w.projects());
    let lock = ReadOnly::new(&w.projects());

    // A dry run names what waits and writes nothing.
    let dry = w.run(&["sessions", "archive", "--dry-run"]);
    dry.expect_code(0);
    assert!(
        dry.stdout.starts_with(
            "2 session file(s) wait for the archive, 0 summar(y/ies) wait, 1 file(s) still change, 0 file(s) are in the archive. Nothing changed"
        ),
        "{dry:?}"
    );
    assert!(
        dry.stdout.contains(&format!(
            "copy     vision/sessions/{PROJECT}/{SESSION}.jsonl.gz"
        )),
        "{dry:?}"
    );
    assert!(
        dry.stdout.contains(&format!(
            "copy     vision/sessions/{PROJECT}/{SESSION}/subagents/agent-a1.jsonl.gz"
        )),
        "{dry:?}"
    );
    assert!(!w.archive.exists());

    let first = w.run(&["sessions", "archive"]);
    first.expect_code(0);
    assert!(
        first
            .stdout
            .starts_with("2 session file(s) went to the archive"),
        "{first:?}"
    );
    assert!(
        first
            .stdout
            .contains("1 summar(y/ies) written; 1 file(s) still change"),
        "{first:?}"
    );
    let main_gz = w
        .archive
        .join(format!("vision/sessions/{PROJECT}/{SESSION}.jsonl.gz"));
    let sub_gz = w.archive.join(format!(
        "vision/sessions/{PROJECT}/{SESSION}/subagents/agent-a1.jsonl.gz"
    ));
    assert_eq!(gunzip(&main_gz), std::fs::read_to_string(&main).unwrap());
    assert_eq!(gunzip(&sub_gz), std::fs::read_to_string(&sub).unwrap());
    assert!(!w
        .archive
        .join(format!("vision/sessions/{PROJECT}/{OTHER}.jsonl.gz"))
        .exists());
    let names: Vec<String> = std::fs::read_dir(main_gz.parent().unwrap())
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert!(
        names.iter().all(|n| !n.starts_with('.')),
        "no temporary copy stays: {names:?}"
    );
    let row = w
        .ledger()
        .session_row(&format!("{PROJECT}/{SESSION}.jsonl"))
        .unwrap()
        .unwrap();
    assert!(row.main && row.archived.is_some());
    assert_eq!(row.session, SESSION);
    assert_eq!(
        row.size,
        Some(std::fs::metadata(&main).unwrap().len() as i64)
    );
    assert_eq!(
        row.archive_bytes,
        Some(std::fs::metadata(&main_gz).unwrap().len() as i64)
    );
    assert_eq!(
        row.key.as_deref(),
        Some(format!("vision/sessions/{PROJECT}/{SESSION}.jsonl.gz").as_str())
    );

    // The summary: the typed asks, the files, the errors, the commits, and the outcome.
    let text = std::fs::read_to_string(w.entry(SESSION)).unwrap();
    assert!(text.starts_with(&format!("---\nid: {SESSION}\nkind: claude-session\nproject: {PROJECT}\ncwd: /home/tester/work\nbranch: main\ntitle: CSV export of the report\nstarted: 2026-11-02T10:00:00.000Z\nended: 2026-11-02T10:09:00.000Z\nmessages: 2\ntool_calls: 5\narchive: vision/sessions/{PROJECT}/{SESSION}.jsonl.gz\n---\n")), "{text}");
    assert!(text.contains("\n## Asks\n\n- Add a CSV export to the report page\n- Upload it with the key [secret DEMO_TOKEN] or [a personal access token]\n"), "{text}");
    assert!(text.contains("\n## Files\n\n- src/export.rs\n"), "{text}");
    assert!(text.contains("\n## Errors and fixes\n\n- `cargo test export` failed (exit 101): error[E0425]: cannot find value `rows` in this scope Passed later, after the commit 4d5e6f7.\n"), "{text}");
    assert!(
        text.contains("\n## Commits\n\n- 4d5e6f7 Add the CSV export\n"),
        "{text}"
    );
    assert!(
        text.contains(
            "\n## Outcome\n\nThe CSV export works. I did not keep [a personal access token].\n"
        ),
        "{text}"
    );
    for hidden in [
        SECRET_VALUE,
        &fake_token(),
        "TOOL-RESULT-MARKER",
        "REMINDER-MARKER",
        "TASK-MARKER",
        "LOCAL-MARKER",
        "/clear",
        "ATTACHMENT-MARKER",
        "THINKING-MARKER",
        "SUBAGENT-PROMPT-MARKER",
        "with a header row",
    ] {
        assert!(!text.contains(hidden), "{hidden} leaks: {text}");
    }
    assert!(
        !w.home
            .path(&format!("session-journal/{PROJECT}/agent-a1.md"))
            .exists(),
        "a subagent file gets no summary of its own"
    );

    // A second run copies nothing and leaves the archive as it is.
    let kept = std::fs::metadata(&main_gz).unwrap().modified().unwrap();
    let again = w.run(&["sessions", "archive"]);
    again.expect_code(0);
    assert!(
        again
            .stdout
            .starts_with("0 session file(s) went to the archive"),
        "{again:?}"
    );
    assert_eq!(
        std::fs::metadata(&main_gz).unwrap().modified().unwrap(),
        kept
    );

    // The doctor names the folder of the archive and counts the sessions.
    let doctor = w.run(&["doctor"]);
    let line = |name: &str| {
        doctor
            .stdout
            .lines()
            .find(|l| l.split_whitespace().nth(1) == Some(name))
            .unwrap()
            .to_string()
    };
    let archive = line("archive");
    assert!(
        archive.starts_with("ok ")
            && archive.contains(&format!(
                "the folder {} holds 2 file(s)",
                w.archive.display()
            )),
        "{archive}"
    );
    let sessions = line("sessions");
    assert!(sessions.starts_with("ok "), "{sessions}");
    assert!(
        sessions.contains("2 file(s) of 1 session(s) archived"),
        "{sessions}"
    );
    assert!(
        sessions.contains("1 wait, 1 of them still change; 1 summar(y/ies)"),
        "{sessions}"
    );

    drop(lock);
    assert_eq!(picture(&w.projects()), before, "the projects never change");
    assert!(recent.is_file());
}

#[test]
fn a_file_that_changed_in_the_quiet_hours_waits_until_it_is_quiet() {
    let w = World::local();
    w.edit_config(|doc| doc["sessions"]["quiet_hours"] = toml_edit::value(3));
    let main = w.session(&format!("{SESSION}.jsonl"), &session_lines(SESSION), 2);
    let out = w.run(&["sessions", "archive"]);
    out.expect_code(0);
    assert!(
        out.stdout
            .starts_with("0 session file(s) went to the archive"),
        "{out:?}"
    );
    assert!(out.stdout.contains("1 file(s) still change"), "{out:?}");
    assert!(!w.entry(SESSION).exists());
    set_age(&main, 4);
    let later = w.run(&["sessions", "archive"]);
    later.expect_code(0);
    assert!(
        later
            .stdout
            .starts_with("1 session file(s) went to the archive"),
        "{later:?}"
    );
    assert!(w.entry(SESSION).is_file());
}

#[test]
fn a_session_that_grew_goes_again_under_the_same_key_and_its_drawer_is_replaced() {
    let w = World::bucket();
    let (server, palace) = fake_palace();
    let url = format!("{}/mcp", server.url);
    w.edit_config(|doc| doc["journal"]["mempalace_url"] = toml_edit::value(url.clone()));
    let mut lines = session_lines(SESSION);
    let main = w.session(&format!("{SESSION}.jsonl"), &lines, 30);
    let first = w.run(&["sessions", "archive"]);
    first.expect_code(0);
    assert!(
        first
            .stdout
            .contains("1 summar(y/ies) filed in MemPalace, 0 still wait"),
        "{first:?}"
    );
    let key = format!("vision/sessions/{PROJECT}/{SESSION}.jsonl.gz");
    let object = w.aws_dir.join("store/test-bucket").join(&key);
    assert_eq!(gunzip(&object), std::fs::read_to_string(&main).unwrap());
    let calls = w.aws_calls();
    assert!(
        calls
            .iter()
            .any(|c| c.starts_with("s3 cp ") && c.contains(&format!("s3://test-bucket/{key}"))),
        "{calls:?}"
    );
    assert!(
        calls.contains(&format!(
            "s3api head-object --bucket test-bucket --key {key} --output json"
        )),
        "{calls:?}"
    );
    let entry = w.entry(SESSION);
    let text = std::fs::read_to_string(&entry).unwrap();
    {
        let palace = palace.lock().unwrap();
        assert_eq!(palace.drawers.len(), 1);
        let drawer = &palace.drawers[0];
        assert_eq!(drawer["wing"], "claude-sessions");
        assert_eq!(drawer["room"], "home-tester-work");
        assert_eq!(drawer["source_file"], json!(entry.display().to_string()));
        assert_eq!(drawer["content"], json!(text));
        let deletes = palace.calls_of("mempalace_delete_by_source");
        assert_eq!(
            deletes[0],
            json!({"source_file": entry.display().to_string(), "dry_run": false})
        );
    }

    // The session goes on: two more lines, and the file is quiet again.
    lines.push(json!({"type": "user", "sessionId": SESSION, "timestamp": "2026-11-03T09:00:00.000Z",
        "origin": {"kind": "human"}, "message": {"role": "user", "content": "Now add a total row"}}));
    lines.push(json!({"type": "assistant", "sessionId": SESSION, "timestamp": "2026-11-03T09:05:00.000Z",
        "message": {"role": "assistant", "content": [{"type": "text", "text": "The total row is there."}]}}));
    let main = w.session(&format!("{SESSION}.jsonl"), &lines, 26);
    let copies = |w: &World| {
        w.aws_calls()
            .iter()
            .filter(|c| c.starts_with("s3 cp ") && c.contains(&format!("s3://test-bucket/{key}")))
            .count()
    };
    assert_eq!(copies(&w), 1);
    let second = w.run(&["sessions", "archive"]);
    second.expect_code(0);
    assert!(
        second
            .stdout
            .starts_with("1 session file(s) went to the archive"),
        "{second:?}"
    );
    assert_eq!(copies(&w), 2, "the same key again");
    assert_eq!(gunzip(&object), std::fs::read_to_string(&main).unwrap());
    let new_text = std::fs::read_to_string(&entry).unwrap();
    assert!(new_text.contains("- Now add a total row\n"), "{new_text}");
    assert!(
        new_text.contains("\n## Outcome\n\nThe total row is there.\n"),
        "{new_text}"
    );
    {
        let palace = palace.lock().unwrap();
        assert_eq!(palace.drawers.len(), 1, "one drawer for one source");
        assert_eq!(palace.drawers[0]["content"], json!(new_text));
        for drawer in &palace.drawers {
            let content = drawer["content"].as_str().unwrap();
            assert!(
                !content.contains("\"type\""),
                "no raw session text goes to MemPalace"
            );
        }
    }

    // A new change time with the same content sends nothing.
    set_age(&main, 25);
    let touched = w.run(&["sessions", "archive"]);
    touched.expect_code(0);
    assert!(
        touched
            .stdout
            .contains("0 session file(s) went to the archive (0 B); 1 were there already"),
        "{touched:?}"
    );
    assert_eq!(copies(&w), 2);

    // The search asks MemPalace with the words too, in the wing of the sessions.
    let found = w.run(&["sessions", "search", "total row", "--json"]);
    found.expect_code(0);
    let found: Value = serde_json::from_str(&found.stdout).unwrap();
    assert_eq!(found["source"], "mempalace", "{found}");
    let hit = &found["hits"][0];
    assert_eq!(hit["session"], SESSION);
    assert_eq!(hit["project"], PROJECT);
    assert_eq!(hit["date"], "2026-11-02");
    assert_eq!(hit["first_ask"], "Add a CSV export to the report page");
    assert_eq!(hit["archive"], json!(key));
    let searches = palace.lock().unwrap().calls_of("mempalace_search");
    assert_eq!(searches.last().unwrap()["candidate_strategy"], "union");
    assert_eq!(searches.last().unwrap()["wing"], "claude-sessions");
}

#[test]
fn fetch_brings_the_files_back_and_never_writes_into_the_folder_of_claude_code() {
    let w = World::local();
    let main = w.session(&format!("{SESSION}.jsonl"), &session_lines(SESSION), 48);
    let sub = w.session(
        &format!("{SESSION}/subagents/agent-a1.jsonl"),
        &subagent_lines(SESSION),
        48,
    );
    w.run(&["sessions", "archive"]).expect_code(0);
    let before = picture(&w.projects());
    let lock = ReadOnly::new(&w.projects());

    let out = w.home.user_home.join("restored");
    let fetched = w.run(&[
        "sessions",
        "fetch",
        SESSION,
        "--to",
        &out.display().to_string(),
    ]);
    fetched.expect_code(0);
    assert!(
        fetched
            .stdout
            .starts_with("2 file(s) of the session are back:"),
        "{fetched:?}"
    );
    assert_eq!(
        std::fs::read(out.join(format!("{SESSION}.jsonl"))).unwrap(),
        std::fs::read(&main).unwrap()
    );
    assert_eq!(
        std::fs::read(out.join(format!("{SESSION}/subagents/agent-a1.jsonl"))).unwrap(),
        std::fs::read(&sub).unwrap()
    );
    // A second fetch to the same folder writes nothing over the files.
    let twice = w.run(&[
        "sessions",
        "fetch",
        SESSION,
        "--to",
        &out.display().to_string(),
    ]);
    twice.expect_code(1);
    assert!(twice.stderr.contains("is already here"), "{twice:?}");
    // The first 8 characters of the id are enough.
    let short = w.home.user_home.join("short");
    w.run(&[
        "sessions",
        "fetch",
        &SESSION[..8],
        "--to",
        &short.display().to_string(),
    ])
    .expect_code(0);
    assert!(short.join(format!("{SESSION}.jsonl")).is_file());

    // The folder of Claude Code, a folder in it that does not exist yet, and a path with `..`
    // into it are refused.
    let projects = w.projects();
    for target in [
        projects.clone(),
        projects.join(PROJECT).join("new"),
        w.home.user_home.join("restored/../.claude/projects/x"),
    ] {
        let refused = w.run(&[
            "sessions",
            "fetch",
            SESSION,
            "--to",
            &target.display().to_string(),
        ]);
        refused.expect_code(1);
        assert!(
            refused.stderr.contains("a fetch never writes there"),
            "{refused:?}"
        );
    }
    // The default folder of Claude Code is refused too when sessions.projects names another.
    drop(lock);
    let elsewhere = w.home.user_home.join("elsewhere");
    std::fs::create_dir_all(&elsewhere).unwrap();
    w.edit_config(|doc| {
        doc["sessions"]["projects"] = toml_edit::value(elsewhere.display().to_string())
    });
    let lock = ReadOnly::new(&w.projects());
    let refused = w.run(&[
        "sessions",
        "fetch",
        SESSION,
        "--to",
        &w.projects().join("y").display().to_string(),
    ]);
    refused.expect_code(1);
    assert!(
        refused.stderr.contains("a fetch never writes there"),
        "{refused:?}"
    );
    let unknown = w.run(&["sessions", "fetch", "ffffffff-0000"]);
    unknown.expect_code(1);
    assert!(
        unknown.stderr.contains("the archive has no session"),
        "{unknown:?}"
    );
    drop(lock);
    assert_eq!(picture(&w.projects()), before, "the projects never change");
}

#[test]
fn the_summaries_wait_for_the_secrets_and_come_later_from_the_file_or_the_archive() {
    let w = World::local();
    let main = w.session(&format!("{SESSION}.jsonl"), &session_lines(SESSION), 48);
    w.session(&format!("{OTHER}.jsonl"), &session_lines(OTHER), 48);
    // With no value of the secret, the files go to the archive and no summary is written.
    let bare = w.home.run_with(&["sessions", "archive"], &[]);
    bare.expect_code(1);
    assert!(
        bare.stdout
            .starts_with("2 session file(s) went to the archive"),
        "{bare:?}"
    );
    assert!(bare.stdout.contains("DEMO_TOKEN"), "{bare:?}");
    assert!(!w.entry(SESSION).exists() && !w.entry(OTHER).exists());
    let dry = w.run(&["sessions", "archive", "--dry-run"]);
    assert!(
        dry.stdout
            .starts_with("0 session file(s) wait for the archive, 2 summar(y/ies) wait"),
        "{dry:?}"
    );

    // Claude Code deleted one file in the meantime: its summary comes from the archive.
    std::fs::remove_file(&main).unwrap();
    let later = w.run(&["sessions", "archive"]);
    later.expect_code(0);
    assert!(
        later.stdout.contains("2 summar(y/ies) written"),
        "{later:?}"
    );
    let text = std::fs::read_to_string(w.entry(SESSION)).unwrap();
    assert!(
        text.contains("- Add a CSV export to the report page\n"),
        "{text}"
    );
    assert!(!text.contains(SECRET_VALUE), "{text}");
    assert!(w.entry(OTHER).is_file());
}

#[test]
fn the_tick_archives_with_the_hourly_run_and_files_the_summaries() {
    let w = World::local();
    let (server, palace) = fake_palace();
    let url = format!("{}/mcp", server.url);
    w.edit_config(|doc| doc["journal"]["mempalace_url"] = toml_edit::value(url.clone()));
    w.session(&format!("{SESSION}.jsonl"), &session_lines(SESSION), 48);
    w.run(&["tick"]).expect_code(0);
    assert!(w
        .archive
        .join(format!("vision/sessions/{PROJECT}/{SESSION}.jsonl.gz"))
        .is_file());
    assert!(w.entry(SESSION).is_file());
    assert_eq!(palace.lock().unwrap().drawers.len(), 1);
    assert!(w.ledger().last_event_at("archive-run").unwrap().is_some());
    assert!(w.ledger().last_event_at("sessions-run").unwrap().is_some());

    // A second tick in the same hour copies nothing, even for a new finished file.
    w.session(&format!("{OTHER}.jsonl"), &session_lines(OTHER), 48);
    w.run(&["tick"]).expect_code(0);
    assert!(!w
        .archive
        .join(format!("vision/sessions/{PROJECT}/{OTHER}.jsonl.gz"))
        .exists());
    assert!(!w.entry(OTHER).exists());
}

#[test]
fn the_local_search_reads_the_summary_files_and_the_doctor_says_when_the_archive_is_off() {
    let w = World::local();
    w.session(&format!("{SESSION}.jsonl"), &session_lines(SESSION), 48);
    w.run(&["sessions", "archive"]).expect_code(0);
    let found = w.run(&["sessions", "search", "CSV export"]);
    found.expect_code(0);
    assert!(
        found
            .stdout
            .starts_with("1 session(s) from the summary files:"),
        "{found:?}"
    );
    assert!(
        found
            .stdout
            .contains(&format!("{SESSION}  2026-11-02  {PROJECT}")),
        "{found:?}"
    );
    assert!(
        found
            .stdout
            .contains("    Add a CSV export to the report page\n"),
        "{found:?}"
    );
    assert!(
        found.stdout.contains(&format!(
            "    archive: vision/sessions/{PROJECT}/{SESSION}.jsonl.gz\n"
        )),
        "{found:?}"
    );
    let none = w.run(&["sessions", "search", "nothing-like-this"]);
    none.expect_code(0);
    assert!(none.stdout.starts_with("No session fits"), "{none:?}");
    // The summaries of the sessions stay out of the search of the turns.
    let turns = w.run(&["journal", "search", "CSV export"]);
    turns.expect_code(0);
    assert!(turns.stdout.starts_with("No entry fits"), "{turns:?}");

    w.edit_config(|doc| doc["sessions"]["enabled"] = toml_edit::value(false));
    let off = w.run(&["sessions", "archive"]);
    off.expect_code(1);
    assert!(
        off.stderr.contains("the archive of the sessions is off"),
        "{off:?}"
    );
    let doctor = w.run(&["doctor"]);
    let line = doctor
        .stdout
        .lines()
        .find(|l| l.split_whitespace().nth(1) == Some("sessions"))
        .unwrap()
        .to_string();
    assert!(
        line.starts_with("ok ") && line.ends_with("off (sessions.enabled is false)"),
        "{line}"
    );
}
