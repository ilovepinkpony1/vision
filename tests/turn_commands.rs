//! `vision tell`, `logs`, `open`, and `off` with running turns of the fake `claude`.

mod turn_support;

use std::process::{Command, Stdio};
use std::time::Duration;

use serde_json::{json, Value};
use turn_support::{fake_claude, result_object, wait_for, wait_gone, TestHome};
use vision::turn::{self, Reason, TurnKind, TurnStatus};

fn pid_of(event: &Value, field: &str) -> u32 {
    event[field].as_u64().expect("a pid") as u32
}

#[test]
fn tell_sends_a_message_into_a_running_turn() {
    let home = TestHome::new();
    let scenario = home.scenario(
        "tell",
        json!([[
            {"do": "init"},
            {"do": "text", "text": "waiting for you"},
            {"do": "wait_input", "timeout": 15},
            {"do": "result", "structured_output": result_object("Used the message.")}
        ]]),
    );
    let launched =
        home.launch(&home.request("DEMO-12", "scout", "refine", TurnKind::Reader, &scenario));
    home.wait_stream(&launched, "waiting for you");
    let run = home.run(&["tell", "DEMO-12", "use the old export"]);
    run.expect_code(0);
    assert!(run.stdout.contains("Sent to the running turn"), "{run:?}");
    home.wait_ended(&launched);
    let stream = home.turn_file(&launched, "stream.jsonl");
    assert!(stream.contains("got: use the old export"), "{stream}");
    let sent: Vec<_> = std::fs::read_dir(launched.dir.join("inbox/sent"))
        .unwrap()
        .collect();
    assert_eq!(sent.len(), 1);
    let ledger = home.ledger();
    let message = ledger.message(1).unwrap().expect("a row in messages");
    assert_eq!(message.text, "use the old export");
    assert_eq!(message.source, "user");
    assert_eq!(
        message.turn.as_deref(),
        Some(launched.id.to_string().as_str())
    );
    assert!(message.delivered.is_some());
    home.collect();
    let outcome = turn::outcome(&home.ctx(), &ledger, &launched.id).unwrap();
    assert_eq!(outcome.status, TurnStatus::Done);
}

#[test]
fn tell_with_no_running_turn_waits_for_the_next_turn() {
    let home = TestHome::new();
    let run = home.run(&["tell", "DEMO-12", "remember the old export"]);
    run.expect_code(0);
    assert!(run.stdout.contains("waits for its next turn"), "{run:?}");
    assert!(home
        .vision
        .join("tickets/DEMO-12/inbox/m-000000000001.json")
        .is_file());
    let ledger = home.ledger();
    assert_eq!(ledger.message(1).unwrap().unwrap().turn, None);

    let scenario = home.scenario(
        "next",
        json!([[
            {"do": "init"},
            {"do": "wait_input", "timeout": 15},
            {"do": "result", "structured_output": result_object("ok")}
        ]]),
    );
    let launched =
        home.launch(&home.request("DEMO-12", "scout", "refine", TurnKind::Reader, &scenario));
    home.wait_ended(&launched);
    let stream = home.turn_file(&launched, "stream.jsonl");
    assert!(stream.contains("got: remember the old export"), "{stream}");
    let message = ledger.message(1).unwrap().unwrap();
    assert_eq!(
        message.turn.as_deref(),
        Some(launched.id.to_string().as_str())
    );
    assert!(message.delivered.is_some());
    home.run(&["tell", "DEMO-12", " "]).expect_code(1);
}

#[test]
fn logs_shows_short_readable_lines() {
    let home = TestHome::new();
    let none = home.run(&["logs"]);
    none.expect_code(0);
    assert!(none.stdout.contains("No turn runs"), "{none:?}");
    assert!(home
        .run(&["logs", "DEMO-99"])
        .stdout
        .contains("DEMO-99 has no turn yet"));

    let scenario = home.scenario(
        "logs",
        json!([[
            {"do": "init"},
            {"do": "text", "text": "I read the issue."},
            {"do": "tool", "name": "Read", "input": {"file_path": "notes.txt"}},
            {"do": "rate_limit", "window": "five_hour", "utilization": 0.25},
            {"do": "result", "structured_output": result_object("Refined."), "cost": 0.5}
        ]]),
    );
    let launched =
        home.launch(&home.request("DEMO-12", "scout", "refine", TurnKind::Reader, &scenario));
    home.wait_ended(&launched);
    let run = home.run(&["logs", "DEMO-12"]);
    run.expect_code(0);
    let id = launched.id.to_string();
    for line in [
        format!("turn {id}"),
        format!(
            "message: Turn {id}. Read .vision/turns/{id}/facts.json and do the step that it names."
        ),
        "agent: I read the issue.".to_string(),
        "tool Read notes.txt".to_string(),
        "limit five_hour at 25%".to_string(),
        "result result: Refined. ($0.5000)".to_string(),
        "ended: result, exit code 0".to_string(),
    ] {
        assert!(
            run.stdout.lines().any(|l| l == line),
            "{line:?} is missing: {}",
            run.stdout
        );
    }
}

#[test]
fn logs_follow_prints_until_the_turn_ends() {
    let home = TestHome::new();
    let scenario = home.scenario(
        "follow",
        json!([[
            {"do": "init"},
            {"do": "text", "text": "first"},
            {"do": "sleep", "seconds": 1.0},
            {"do": "text", "text": "second"},
            {"do": "result", "structured_output": result_object("Done.")}
        ]]),
    );
    let launched =
        home.launch(&home.request("DEMO-12", "scout", "refine", TurnKind::Reader, &scenario));
    home.wait_stream(&launched, "first");
    let all = home.run(&["logs"]);
    assert!(all.stdout.contains("[DEMO-12] turn "), "{all:?}");
    assert!(all.stdout.contains("[DEMO-12] still running"), "{all:?}");
    let run = home.run(&["logs", "DEMO-12", "--follow"]);
    run.expect_code(0);
    for line in [
        "agent: first",
        "agent: second",
        "result result: Done. (",
        "ended: result, exit code 0",
    ] {
        assert!(
            run.stdout.lines().any(|l| l.starts_with(line)),
            "{line:?} is missing: {}",
            run.stdout
        );
    }
    assert!(home
        .state("DEMO-12", &launched.id.to_string())
        .unwrap()
        .is_ended());
}

#[test]
fn off_kills_each_running_turn() {
    let home = TestHome::new();
    let scenario = home.scenario(
        "off",
        json!([[{"do": "init"}, {"do": "spawn_child"}, {"do": "hang"}]]),
    );
    let launched =
        home.launch(&home.request("DEMO-12", "scout", "refine", TurnKind::Reader, &scenario));
    let child = wait_for(Duration::from_secs(20), "the child of the fake", || {
        home.events(&launched, "system", Some("fake_child"))
            .into_iter()
            .next()
    });
    let fake = pid_of(
        &home.events(&launched, "system", Some("init"))[0],
        "fake_pid",
    );
    let run = home.run(&["off"]);
    run.expect_code(0);
    assert!(run.stdout.contains("Killed 1 running turn(s)"), "{run:?}");
    let state = home.state("DEMO-12", &launched.id.to_string()).unwrap();
    assert_eq!(state.reason, Some(Reason::Killed), "{state:?}");
    wait_gone(fake);
    wait_gone(pid_of(&child, "pid"));
    let collected = home.collect();
    assert_eq!(
        collected[0].status,
        TurnStatus::Failed,
        "a killed turn is not resumed"
    );
    assert_eq!(collected[0].reason, Reason::Killed);
    let again = home.run(&["off"]);
    assert!(!again.stdout.contains("Killed"), "{again:?}");
    home.run(&["on"]).expect_code(0);
}

#[test]
fn open_sends_typed_lines_into_a_running_turn() {
    let home = TestHome::new();
    let scenario = home.scenario(
        "open",
        json!([[
            {"do": "init"},
            {"do": "text", "text": "ready"},
            {"do": "wait_input", "timeout": 15},
            {"do": "result", "structured_output": result_object("Hurried.")}
        ]]),
    );
    let launched =
        home.launch(&home.request("DEMO-12", "scout", "refine", TurnKind::Reader, &scenario));
    home.wait_stream(&launched, "ready");
    let run = home.run_with(&["open", "DEMO-12"], &[], Some("please hurry\n"));
    run.expect_code(0);
    assert!(run.stdout.contains("Attached to the turn"), "{run:?}");
    for line in [
        "agent: got: please hurry",
        "result result: Hurried. (",
        "ended: result, exit code 0",
    ] {
        assert!(
            run.stdout.lines().any(|l| l.starts_with(line)),
            "{line:?} is missing: {run:?}"
        );
    }
    assert!(run.stderr.contains("(sent)"), "{run:?}");
}

#[test]
fn open_with_no_running_turn_pauses_the_ticket_and_takes_over_the_session() {
    let home = TestHome::new();
    let missing = home.run(&["open", "DEMO-12"]);
    missing.expect_code(1);
    assert!(missing.stderr.contains("no turn yet"), "{missing:?}");

    let scenario = home.scenario(
        "take-over",
        json!([[{"do": "init"}, {"do": "result", "structured_output": result_object("ok")}]]),
    );
    let launched =
        home.launch(&home.request("DEMO-12", "scout", "refine", TurnKind::Reader, &scenario));
    home.wait_ended(&launched);
    home.collect();
    let run = home.run(&["open", "DEMO-12"]);
    run.expect_code(0);
    assert!(run.stdout.contains("DEMO-12 is paused"), "{run:?}");
    let line = run
        .stdout
        .lines()
        .find(|l| l.starts_with("fake-claude interactive"))
        .unwrap_or_else(|| panic!("the fake did not run: {run:?}"));
    assert!(
        line.contains(&format!("resume={}", launched.session)),
        "{line}"
    );
    let cwd = line.split(" cwd=").nth(1).unwrap();
    assert_eq!(
        std::fs::canonicalize(cwd).unwrap(),
        std::fs::canonicalize(&home.work).unwrap()
    );
    assert_eq!(home.ledger().ticket_flags("DEMO-12").unwrap(), ["paused"]);
}

#[test]
fn the_tick_collects_an_ended_turn_and_a_turn_with_no_folder() {
    let home = TestHome::new();
    let scenario = home.scenario(
        "tick",
        json!([[{"do": "init"}, {"do": "result", "structured_output": result_object("ok"), "cost": 0.3}]]),
    );
    let launched =
        home.launch(&home.request("DEMO-12", "scout", "refine", TurnKind::Reader, &scenario));
    home.wait_ended(&launched);
    let ledger = home.ledger();
    ledger
        .conn()
        .execute(
            "INSERT INTO turns (id, ticket, agent, step, kind, started, status)
             VALUES ('20200101T000000Z-DEMO-13-refine-1', 'DEMO-13', 'scout', 'refine', 'reader', '2020-01-01T00:00:00Z', 'running')",
            [],
        )
        .unwrap();
    home.run(&["tick"]).expect_code(0);
    let row = ledger.turn(&launched.id.to_string()).unwrap().unwrap();
    assert_eq!(row.status.as_deref(), Some("done"));
    assert_eq!(row.cost_usd, Some(0.3));
    let lost = ledger
        .turn("20200101T000000Z-DEMO-13-refine-1")
        .unwrap()
        .unwrap();
    assert_eq!(lost.status.as_deref(), Some("failed"));
    assert_eq!(lost.reason.as_deref(), Some("crash"));
    let status = home.run(&["status", "--json"]);
    let view: Value = serde_json::from_str(&status.stdout).unwrap();
    assert_eq!(view["tick"]["outcome"], "done", "{view}");
}

#[test]
fn turn_run_of_an_unknown_turn_is_an_error() {
    let home = TestHome::new();
    let run = home.run(&["turn-run", "20261116T143000Z-DEMO-12-build-1"]);
    run.expect_code(1);
    assert!(run.stderr.contains("the ledger has no turn"), "{run:?}");
}

#[test]
fn the_fake_is_executable_python() {
    let output = Command::new(fake_claude())
        .arg("--resume")
        .arg("s-1")
        .stdin(Stdio::null())
        .output()
        .expect("run the fake");
    assert!(output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stdout).starts_with("fake-claude interactive resume=s-1")
    );
}
