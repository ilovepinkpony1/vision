mod common;

use common::Home;
use vision::ledger::Ledger;
use vision::lock::TickLock;
use vision::util::time::parse_rfc3339;

const ALL_STEPS: [&str; 8] = [
    "lock",
    "heartbeat",
    "reconcile",
    "collect",
    "end-facts",
    "select",
    "act",
    "snapshot",
];
const OFF_STEPS: [&str; 3] = ["lock", "heartbeat", "snapshot"];

fn steps(home: &Home) -> Vec<String> {
    serde_json::from_value(home.snapshot()["tick"]["steps"].clone()).unwrap()
}

#[test]
fn a_tick_runs_each_step_and_writes_the_heartbeat_and_the_snapshot() {
    let home = Home::init();
    let run = home.run(&["tick"]);
    run.expect_code(0);
    assert!(run.stdout.starts_with("tick done in "), "{run:?}");
    parse_rfc3339(&home.read("heartbeat")).expect("the heartbeat is an RFC 3339 time");
    assert_eq!(steps(&home), ALL_STEPS);
    let snapshot = home.snapshot();
    assert_eq!(snapshot["tick"]["outcome"], "done");
    assert_eq!(snapshot["off"], false);
    assert_eq!(snapshot["limits"]["five_hour"]["stop_at"], 0.6);
}

#[test]
fn a_tick_with_the_off_file_calls_no_hook() {
    let home = Home::init();
    let off = home.run(&["off"]);
    off.expect_code(0);
    assert!(home.path("off").is_file());

    let run = home.run(&["tick"]);
    run.expect_code(0);
    assert!(run.stdout.contains("Vision is off"), "{run:?}");
    assert!(
        home.path("heartbeat").is_file(),
        "step 2 writes the heartbeat even when off"
    );
    assert_eq!(steps(&home), OFF_STEPS);
    assert_eq!(home.snapshot()["tick"]["outcome"], "off");
    assert_eq!(home.snapshot()["off"], true);

    home.run(&["on"]).expect_code(0);
    assert!(!home.path("off").exists());
    home.run(&["tick"]).expect_code(0);
    assert_eq!(steps(&home), ALL_STEPS);
}

#[test]
fn on_and_off_are_recorded_and_say_when_nothing_changes() {
    let home = Home::init();
    home.run(&["off"]).expect_code(0);
    let again = home.run(&["off"]);
    again.expect_code(0);
    assert!(again.stdout.contains("already off"), "{again:?}");
    home.run(&["on"]).expect_code(0);
    assert!(home.run(&["on"]).stdout.contains("already on"));

    let ledger = Ledger::open(&home.path("ledger.sqlite")).unwrap();
    let kinds: Vec<String> = ledger
        .recent_events(None, 10)
        .unwrap()
        .into_iter()
        .map(|e| e.kind)
        .collect();
    assert_eq!(kinds, ["on", "off", "init"]);
    let log = home.read("log/vision.log");
    assert!(log.contains("off: the person switched Vision off"), "{log}");
}

#[test]
fn a_tick_that_cannot_get_the_lock_exits_with_0_and_logs_it() {
    let home = Home::init();
    let held = TickLock::try_acquire(&home.path("lock"))
        .unwrap()
        .expect("the test takes the lock first");

    let run = home.run(&["tick"]);
    run.expect_code(0);
    assert!(run.stdout.contains("skipped"), "{run:?}");
    assert!(
        !home.path("heartbeat").exists(),
        "a skipped tick stops before step 2"
    );
    assert!(!home.path("state.json").exists());
    let log = home.read("log/vision.log");
    assert!(
        log.contains("tick skipped: another tick holds the lock"),
        "{log}"
    );

    drop(held);
    home.run(&["tick"]).expect_code(0);
    assert!(home.path("heartbeat").exists());
}

#[test]
fn a_tick_before_init_says_what_to_do() {
    let home = Home::new();
    let run = home.run(&["tick"]);
    run.expect_code(1);
    assert!(run.stderr.contains("vision init"), "{run:?}");
    assert!(!home.vision.exists(), "a failed tick makes no home");
}
