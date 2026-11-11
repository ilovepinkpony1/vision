mod common;

use common::Home;
use vision::lock::TickLock;

#[test]
fn status_on_an_empty_home_shows_the_tick_the_switch_and_the_limits() {
    let home = Home::init();
    let run = home.run(&["status"]);
    run.expect_code(0);
    for part in [
        "tick     no tick yet",
        "switch   on",
        "five-hour  no reading yet; new turns stop at 60%",
        "seven-day  no reading yet; new turns stop at 85%",
        "needs    nothing waits for you",
        "tickets  none",
    ] {
        assert!(run.stdout.contains(part), "{part:?} is missing: {run:?}");
    }
}

#[test]
fn status_json_on_an_empty_home() {
    let home = Home::init();
    let run = home.run(&["status", "--json"]);
    run.expect_code(0);
    let json: serde_json::Value = serde_json::from_str(&run.stdout).unwrap();
    assert_eq!(json["off"], false);
    assert!(json["tick"]["last"].is_null());
    assert!(json["tick"]["age_seconds"].is_null());
    assert_eq!(json["limits"]["five_hour"]["stop_at"], 0.6);
    assert!(json["limits"]["five_hour"]["reading"].is_null());
    assert_eq!(json["limits"]["seven_day"]["stop_at"], 0.85);
    assert_eq!(json["needs"], serde_json::json!([]));
    assert_eq!(json["tickets"], serde_json::json!([]));
}

#[test]
fn status_after_a_tick_and_off_shows_both() {
    let home = Home::init();
    home.run(&["tick"]).expect_code(0);
    home.run(&["off"]).expect_code(0);
    let run = home.run(&["status", "--json"]);
    run.expect_code(0);
    let json: serde_json::Value = serde_json::from_str(&run.stdout).unwrap();
    assert_eq!(json["off"], true);
    assert!(json["tick"]["age_seconds"]
        .as_i64()
        .is_some_and(|age| (0..60).contains(&age)));
    assert_eq!(json["tick"]["outcome"], "done");
    let text = home.run(&["status"]);
    assert!(text.stdout.contains("switch   off"), "{text:?}");
    assert!(text.stdout.contains(" ago ("), "{text:?}");
}

#[test]
fn status_before_init_says_what_to_do() {
    let home = Home::new();
    let run = home.run(&["status"]);
    run.expect_code(1);
    assert!(run.stderr.contains("vision init"), "{run:?}");
}

#[test]
fn prompt_shows_the_needs_and_the_age_of_the_last_tick() {
    let home = Home::new();
    assert_eq!(home.run(&["prompt"]).expect_code(0).stdout, "V:?\n");

    home.run(&["init"]).expect_code(0);
    assert_eq!(home.run(&["prompt"]).expect_code(0).stdout, "V:0 -\n");

    home.run(&["tick"]).expect_code(0);
    let after_tick = home.run(&["prompt"]);
    after_tick.expect_code(0);
    assert!(
        is_prompt_with_seconds(after_tick.stdout.trim_end()),
        "{after_tick:?}"
    );

    home.run(&["off"]).expect_code(0);
    let off = home.run(&["prompt"]);
    assert!(off.stdout.trim_end().ends_with(" off"), "{off:?}");
}

#[test]
fn prompt_never_fails_loudly() {
    let home = Home::init();
    std::fs::write(home.path("state.json"), "{ not json").unwrap();
    std::fs::write(home.path("heartbeat"), "yesterday\n").unwrap();
    let run = home.run(&["prompt"]);
    run.expect_code(0);
    assert_eq!(run.stdout, "V:? -\n");
    assert_eq!(run.stderr, "");
}

/// `V:0 12s`
fn is_prompt_with_seconds(line: &str) -> bool {
    line.strip_prefix("V:0 ")
        .and_then(|age| age.strip_suffix('s'))
        .is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
}

#[test]
fn status_names_the_job_that_a_pass_runs_now_and_a_tick_clears_a_stale_record() {
    let home = Home::init();
    let lock = TickLock::try_acquire(&home.path("lock"))
        .unwrap()
        .expect("the test takes the lock of the tick, as a pass does");
    let guard = vision::running::start(&home.paths(), "job", "proof", "DEMO-12");

    let run = home.run(&["status", "--json"]);
    run.expect_code(0);
    let json: serde_json::Value = serde_json::from_str(&run.stdout).unwrap();
    assert_eq!(json["running"]["what"], "job");
    assert_eq!(json["running"]["name"], "proof");
    assert_eq!(json["running"]["ticket"], "DEMO-12");
    assert_eq!(json["running"]["pid"], std::process::id());
    let text = home.run(&["status"]);
    assert!(
        text.stdout
            .contains("pass     runs the job proof for DEMO-12 since "),
        "{text:?}"
    );

    drop(guard);
    let json: serde_json::Value =
        serde_json::from_str(&home.run(&["status", "--json"]).stdout).unwrap();
    assert!(json["running"].is_null());
    assert!(!home.run(&["status"]).stdout.contains("pass     runs"));

    // A crash leaves the record behind: with no holder of the lock, nobody trusts it, and the
    // next tick removes it.
    std::mem::forget(vision::running::start(
        &home.paths(),
        "operation",
        "git.push",
        "DEMO-12",
    ));
    drop(lock);
    assert!(home.path("running.json").exists());
    let json: serde_json::Value =
        serde_json::from_str(&home.run(&["status", "--json"]).stdout).unwrap();
    assert!(json["running"].is_null());
    home.run(&["tick"]).expect_code(0);
    assert!(!home.path("running.json").exists());
}
