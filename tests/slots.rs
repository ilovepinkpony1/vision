//! The slots and the queue of section 11.3 through `vision take`, `vision tick`, `vision stop`,
//! and the continue, with a demo world and one clone.

mod pipeline_support;

use std::path::Path;
use std::time::Duration;

use pipeline_support::{git, World, KEY};
use serde_json::{json, Value};

const LONG: Duration = Duration::from_secs(90);
const WAITS: &str = "waits-for-slot";

/// A private demo adapter around one wait step. The act steps run `ci.run` in the mode ask, so a
/// ticket there holds its slot until `vision approve`. The wait step `watch` only watches the
/// tracker status: a ticket there can give its slot, and the status Done moves it to `second`,
/// which needs a slot again.
fn slot_adapter(world: &World) {
    world.write_adapter(|text| {
        let scripts = text
            .lines()
            .find(|l| l.starts_with("scripts = "))
            .unwrap()
            .to_string();
        format!(
            r#"name = "demo"
base = "origin/main"
{scripts}

[[step]]
id = "first"
kind = "act"
run = ["ci.run"]
next = "watch"

[[step]]
id = "watch"
kind = "wait"
watch = "tracker_status"
on = {{ "Done" = "second" }}
wait = ["To Do", "In Progress"]
every = 1

[[step]]
id = "second"
kind = "act"
run = [{{ verb = "ci.run", args = {{ round = 2 }} }}]
next = "done"

[[step]]
id = "done"
kind = "end"

[verbs."ci.run"]
mode = "ask"
"#
        )
    });
}

/// A third ticket of the tracker, so that two takes can wait behind a ticket that started.
fn add_ticket(world: &World, key: &str) {
    let mut tracker = world.world_json("tracker.json");
    tracker["tickets"].as_array_mut().unwrap().push(json!({
        "key": key, "title": "Add divide to the calculator",
        "description": "Add a function divide(a, b) that gives a divided by b, with a test.",
        "status": "To Do", "assignee": null, "type": "Story",
    }));
    std::fs::write(
        world.world.join("tracker.json"),
        serde_json::to_string_pretty(&tracker).unwrap(),
    )
    .unwrap();
}

fn row(world: &World, key: &str) -> vision::ledger::TicketRow {
    world
        .ledger()
        .ticket(key)
        .unwrap()
        .unwrap_or_else(|| panic!("{key} has no row"))
}

fn clone_of(world: &World, key: &str) -> Option<String> {
    row(world, key).clone.filter(|c| !c.trim().is_empty())
}

fn step_of(world: &World, key: &str) -> String {
    row(world, key).state.unwrap_or_default()
}

fn flags_of(world: &World, key: &str) -> Vec<String> {
    row(world, key)
        .flags
        .and_then(|f| serde_json::from_str(&f).ok())
        .unwrap_or_default()
}

fn counts_of(world: &World, key: &str) -> Value {
    row(world, key)
        .counts
        .and_then(|c| serde_json::from_str(&c).ok())
        .unwrap_or(Value::Null)
}

fn reason_of(world: &World, key: &str, flag: &str) -> String {
    counts_of(world, key)["reasons"][flag]["reason"]
        .as_str()
        .unwrap_or_default()
        .to_string()
}

/// The details of the events `slot` of a ticket, oldest first.
fn slot_events(world: &World, key: &str) -> Vec<Value> {
    world
        .ledger()
        .events_of_ticket(key)
        .unwrap()
        .into_iter()
        .filter(|e| e.kind == "slot")
        .filter_map(|e| serde_json::from_str(e.detail.as_deref()?).ok())
        .collect()
}

/// Approves the operation of `key` that waits in the mode ask.
fn approve(world: &World, key: &str) {
    let status = world.run(&["status", "--json"]).json();
    let id = status["needs"]
        .as_array()
        .unwrap()
        .iter()
        .find(|n| n["kind"] == "operation" && n["ticket"] == key)
        .and_then(|n| n["id"].as_str())
        .map(str::to_string)
        .unwrap_or_else(|| panic!("no operation of {key} waits: {status}"));
    let (verb, commit) = id.split_once(' ').unwrap();
    world.run(&["approve", key, verb, commit]).expect_code(0);
}

fn waits_for_approval(world: &World, key: &str) -> bool {
    world.run(&["status", "--json"]).json()["needs"]
        .as_array()
        .unwrap()
        .iter()
        .any(|n| n["kind"] == "operation" && n["ticket"] == key)
}

fn branch_of(clone: &Path) -> String {
    git(clone, &["rev-parse", "--abbrev-ref", "HEAD"])
        .trim()
        .to_string()
}

#[test]
fn an_idle_ticket_gives_its_slot_and_a_ticket_that_started_comes_back_before_a_new_take() {
    let world = World::with_clones(1);
    slot_adapter(&world);
    add_ticket(&world, "DEMO-14");
    let clone = world.clone().to_path_buf();
    let clone_text = clone.display().to_string();

    // DEMO-12 takes the slot. Its verb waits for an approval, so it holds the slot.
    world.run(&["take", KEY]).expect_code(0);
    world.tick_until("the verb of DEMO-12 waits", LONG, |w| {
        waits_for_approval(w, KEY)
    });
    let queued = world.run(&["take", "DEMO-13"]);
    queued.expect_code(0);
    assert!(
        queued.stdout.starts_with(
            "Queued DEMO-13 at the step first: no slot is free, and it is 1st in line."
        ),
        "{queued:?}"
    );
    assert!(
        queued
            .stdout
            .contains(&format!("{clone_text}: DEMO-12 holds it (step first)")),
        "{queued:?}"
    );
    assert_eq!(clone_of(&world, "DEMO-13"), None);
    assert_eq!(flags_of(&world, "DEMO-13"), [WAITS]);
    assert_eq!(reason_of(&world, "DEMO-13", WAITS), "1st in line: no slot is free");
    let status = world.run(&["status", "--json"]).json();
    assert!(
        status["needs"]
            .as_array()
            .unwrap()
            .iter()
            .all(|n| n["ticket"] != "DEMO-13"),
        "a ticket in the queue needs nothing of the person: {status}"
    );

    // The approval moves DEMO-12 to the step watch, which only watches: its slot goes to
    // DEMO-13, and DEMO-12 waits in the background.
    approve(&world, KEY);
    world.tick_until("DEMO-13 holds the clone", LONG, |w| {
        clone_of(w, "DEMO-13").is_some()
    });
    assert_eq!(step_of(&world, KEY), "watch");
    assert_eq!(clone_of(&world, KEY), None, "DEMO-12 is in the background");
    assert_eq!(clone_of(&world, "DEMO-13").as_deref(), Some(clone_text.as_str()));
    assert!(flags_of(&world, "DEMO-13").is_empty());
    assert_eq!(branch_of(&clone), "vision/DEMO-13");
    let gave = slot_events(&world, KEY);
    assert_eq!(gave.len(), 1, "{gave:?}");
    assert_eq!(gave[0]["gave"], clone_text.as_str());
    assert_eq!(gave[0]["to"], "DEMO-13");
    assert_eq!(counts_of(&world, KEY)["slot"]["last"], clone_text.as_str());
    let kinds: Vec<String> = world
        .ledger()
        .events_of_ticket("DEMO-13")
        .unwrap()
        .into_iter()
        .map(|e| e.kind)
        .collect();
    assert!(
        kinds.starts_with(&["queued".to_string()]) && kinds.contains(&"take".to_string()),
        "{kinds:?}"
    );
    let now = world.run(&["status", "--json"]).json()["tickets"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["key"] == KEY)
        .map(|t| t["now"].as_str().unwrap_or_default().to_string())
        .unwrap();
    assert!(now.starts_with("in the background, waits"), "{now}");

    // The background ticket is still watched: Done moves it to the step second, which needs a
    // slot. DEMO-13 holds the slot while its verb waits, so DEMO-12 waits in the queue.
    world.tick_until("the verb of DEMO-13 waits", LONG, |w| {
        waits_for_approval(w, "DEMO-13")
    });
    world.world_cmd(&["status", world.root(), KEY, "Done"]);
    world.tick_until("DEMO-12 waits for a slot", LONG, |w| {
        flags_of(w, KEY).iter().any(|f| f == WAITS)
    });
    assert_eq!(step_of(&world, KEY), "second");
    assert_eq!(reason_of(&world, KEY, WAITS), "1st in line: no slot is free");

    // A new take comes after DEMO-12, which started first.
    let late = world.run(&["take", "DEMO-14"]);
    late.expect_code(0);
    assert!(
        late.stdout
            .starts_with("Queued DEMO-14 at the step first: no slot is free, and it is 2nd in line."),
        "{late:?}"
    );

    // DEMO-13 only watches after its approval: DEMO-12 gets the slot, not DEMO-14.
    approve(&world, "DEMO-13");
    world.tick_until("DEMO-12 holds the clone again", LONG, |w| {
        clone_of(w, KEY).is_some()
    });
    assert_eq!(branch_of(&clone), "vision/DEMO-12");
    assert!(flags_of(&world, KEY).is_empty());
    assert_eq!(clone_of(&world, "DEMO-13"), None);
    assert_eq!(step_of(&world, "DEMO-13"), "watch");
    assert_eq!(clone_of(&world, "DEMO-14"), None);
    world.tick();
    assert_eq!(reason_of(&world, "DEMO-14", WAITS), "1st in line: no slot is free");
    let took = slot_events(&world, KEY);
    assert_eq!(took.last().unwrap()["took"], clone_text.as_str(), "{took:?}");

    // DEMO-12 ends: its slot goes to DEMO-14, and DEMO-13 stays in the background.
    world.tick_until("the second verb of DEMO-12 waits", LONG, |w| {
        waits_for_approval(w, KEY)
    });
    approve(&world, KEY);
    world.tick_until("DEMO-14 holds the clone", LONG, |w| {
        clone_of(w, "DEMO-14").is_some()
    });
    assert!(counts_of(&world, KEY)["finished"].is_string());
    assert_eq!(clone_of(&world, "DEMO-13"), None);
    assert_eq!(branch_of(&clone), "vision/DEMO-14");
}

#[test]
fn a_stop_takes_a_ticket_out_of_the_queue_and_a_continue_puts_it_back_at_the_end() {
    let world = World::with_clones(1);
    slot_adapter(&world);
    world.run(&["take", KEY]).expect_code(0);
    world.run(&["take", "DEMO-13"]).expect_code(0);
    assert_eq!(flags_of(&world, "DEMO-13"), [WAITS]);

    let stop = world.run(&["stop", "DEMO-13"]);
    stop.expect_code(0);
    assert!(
        stop.stdout.contains("It leaves the queue of the slots."),
        "{stop:?}"
    );
    assert_eq!(flags_of(&world, "DEMO-13"), ["stopped"]);
    world.tick();
    assert_eq!(flags_of(&world, "DEMO-13"), ["stopped"], "a stopped ticket stays out");

    let again = world.run(&["take", "DEMO-13"]);
    again.expect_code(0);
    assert!(
        again
            .stdout
            .starts_with("Queued DEMO-13 at the step first: no slot is free, and it is 1st in line."),
        "{again:?}"
    );
    assert_eq!(flags_of(&world, "DEMO-13"), [WAITS]);

    // DEMO-12 stops: its slot is free, and the next pass gives it to DEMO-13.
    world.run(&["stop", KEY]).expect_code(0);
    world.tick_until("DEMO-13 holds the clone", LONG, |w| {
        clone_of(w, "DEMO-13").is_some()
    });
    assert_eq!(branch_of(world.clone()), "vision/DEMO-13");

    // A continue of DEMO-12 finds no slot: DEMO-13 holds it, and it does not only watch.
    world.tick_until("the verb of DEMO-13 waits", LONG, |w| {
        waits_for_approval(w, "DEMO-13")
    });
    let busy = world.run(&["take", KEY]);
    busy.expect_code(1);
    assert!(
        busy.stderr.contains("no slot is free for DEMO-12 now"),
        "{busy:?}"
    );
    assert_eq!(flags_of(&world, KEY), ["stopped"]);

    // After its approval DEMO-13 only watches, so a continue of DEMO-12 takes its slot.
    approve(&world, "DEMO-13");
    world.tick_until("DEMO-13 waits at watch", LONG, |w| step_of(w, "DEMO-13") == "watch");
    let back = world.run(&["take", KEY]);
    back.expect_code(0);
    assert_eq!(branch_of(world.clone()), "vision/DEMO-12");
    assert_eq!(
        clone_of(&world, KEY).as_deref(),
        Some(world.clone().display().to_string().as_str())
    );
    assert_eq!(clone_of(&world, "DEMO-13"), None);
    assert!(flags_of(&world, KEY).is_empty(), "{back:?}");
}

#[test]
fn a_clone_with_changes_is_no_free_slot_and_the_take_waits_until_it_is_clean() {
    let world = World::new();
    std::fs::write(world.clone().join("scratch.txt"), "work in progress").unwrap();
    let run = world.run(&["take", KEY]);
    run.expect_code(0);
    assert!(
        run.stdout
            .contains("its work tree has 1 change(s), such as `?? scratch.txt`"),
        "{run:?}"
    );
    assert_eq!(clone_of(&world, KEY), None);
    std::fs::remove_file(world.clone().join("scratch.txt")).unwrap();
    world.tick();
    assert!(clone_of(&world, KEY).is_some());
    assert_eq!(step_of(&world, KEY), "refine");
    assert!(flags_of(&world, KEY).is_empty());
}
