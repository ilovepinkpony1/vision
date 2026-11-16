//! The act step (sections 11.2 and 11.3), driven through `vision tick` with small private
//! adapters on the scripts of the demo: no turn, only the verbs of `run`, in order, through
//! the executor, with the modes, the gates, the idempotency keys, and the `{doc:NAME}` rule of
//! the `after` verbs. Nothing calls the real `claude`, and no test starts a turn.

mod pipeline_support;

use std::time::Duration;

use pipeline_support::{git, World, KEY};
use serde_json::Value;

const LONG: Duration = Duration::from_secs(60);

/// The text of `pr.md` of the tests: the title of the pull request, then its body.
const PR: &str = "Add subtract\n\nThe calculator can subtract.";

/// The verbs of the private adapters, each one in the mode auto as in the shipped adapters.
const VERBS: &str = r#"
[verbs."git.push"]
mode = "auto"
leaves = true

[verbs."pr.open"]
mode = "auto"
leaves = true

[verbs."tracker.issue"]
read = true
"#;

/// A private adapter with the scripts of the demo. The act step `open-pr` comes first, with
/// `run` and the more keys of `keys`. Its next is `look`, a wait step on the draft flag of the
/// pull request: a draft goes back to `open-pr`, and a pull request that is ready goes to the
/// end.
fn act_adapter(world: &World, run: &str, keys: &str) {
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
id = "open-pr"
kind = "act"
run = {run}
next = "look"
{keys}

[[step]]
id = "look"
kind = "wait"
watch = "pr.draft"
on = {{ "true" = "open-pr", "false" = "done" }}
every = 1

[[step]]
id = "done"
kind = "end"
{VERBS}"#
        )
    });
}

/// The `run` of the shipped adapters: the push, then the pull request with pr.md.
const PUSH_AND_OPEN: &str =
    r#"["git.push", { verb = "pr.open", args = { text = "{doc:pr.md}" } }]"#;

fn status(world: &World) -> Value {
    let run = world.run(&["status", "--json"]);
    run.expect_code(0);
    run.json()
}

fn ticket_status(world: &World) -> Value {
    status(world)["tickets"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["key"] == KEY)
        .cloned()
        .expect("the status has DEMO-12")
}

fn has_flag(world: &World, flag: &str) -> bool {
    world.flags().iter().any(|f| f == flag)
}

/// Writes a document in the ticket folder of DEMO-12.
fn write_doc(world: &World, name: &str, text: &str) {
    let dir = world.vision.join("tickets").join(KEY);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join(name), text).unwrap();
}

/// The `act.log` entries of one verb for one ticket.
fn acts_for(world: &World, verb: &str, key: &str) -> usize {
    std::fs::read_to_string(world.world.join("act.log"))
        .unwrap_or_default()
        .lines()
        .filter_map(|l| serde_json::from_str::<Value>(l).ok())
        .filter(|e| e["verb"] == verb && e["key"] == key)
        .count()
}

/// The commit of the branch of DEMO-12 on the remote of the demo world, or `None`.
fn remote_head(world: &World) -> Option<String> {
    let output = std::process::Command::new("git")
        .arg("-C")
        .arg(world.world.join("remote.git"))
        .args([
            "rev-parse",
            "--verify",
            "--quiet",
            "refs/heads/vision/DEMO-12",
        ])
        .output()
        .unwrap();
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).trim().to_string())
}

/// The id of the operation that waits for the approval, and its commit, from `vision status`.
fn waiting_operation(world: &World) -> Option<(String, String)> {
    status(world)["needs"]
        .as_array()
        .unwrap()
        .iter()
        .find(|n| n["kind"] == "operation")
        .and_then(|n| n["id"].as_str())
        .and_then(|id| id.split_once(' '))
        .map(|(verb, commit)| (verb.to_string(), commit.to_string()))
}

/// Takes DEMO-12, with one commit of the change in the clone and pr.md in its folder.
fn take_with_change(world: &World) -> String {
    world.run(&["take", KEY]).expect_code(0);
    write_doc(world, "pr.md", PR);
    world.commit_in(
        world.clone(),
        &[("notes.txt", "subtract comes next.\n")],
        "Add a note",
    )
}

#[test]
fn an_act_step_runs_its_verbs_in_order_and_moves_one_step_in_one_tick() {
    let world = World::new();
    act_adapter(&world, PUSH_AND_OPEN, "");
    let head = take_with_change(&world);
    let view = ticket_status(&world);
    assert_eq!(view["kind"], "act");
    assert_eq!(view["now"], "next: git.push");

    // Each tick requests one verb, and the move to the next step waits for the next tick.
    world.tick();
    assert_eq!(world.step().as_deref(), Some("open-pr"));
    assert_eq!(
        (world.acts_of("git.push"), world.acts_of("pr.open")),
        (1, 0)
    );
    assert_eq!(remote_head(&world).as_deref(), Some(head.as_str()));
    assert_eq!(ticket_status(&world)["now"], "next: pr.open");
    world.tick();
    assert_eq!(world.step().as_deref(), Some("open-pr"));
    assert_eq!(world.acts_of("pr.open"), 1);
    assert_eq!(
        ticket_status(&world)["now"],
        "its verbs are done, so the next tick moves it to the next step"
    );
    let prs = world.world_json("prs.json");
    assert_eq!(prs["prs"][0]["title"], "Add subtract", "the text of pr.md");
    assert_eq!(prs["prs"][0]["body"], "The calculator can subtract.");
    assert_eq!(prs["prs"][0]["head"], head.as_str());
    assert_eq!(world.row().unwrap().pr, Some(1), "pr.open set the row");
    world.tick();
    assert_eq!(world.step().as_deref(), Some("look"));
    assert_eq!(world.acts_of("git.push"), 1, "the move ran no verb");

    // A rule of the step asked for each operation, with no turn, and each one is confirmed.
    let ops = world.ledger().operations_of(KEY, None).unwrap();
    let rows: Vec<(&str, &str, Option<&str>, Option<&str>)> = ops
        .iter()
        .map(|o| {
            (
                o.verb.as_str(),
                o.state.as_str(),
                o.source.as_deref(),
                o.turn.as_deref(),
            )
        })
        .collect();
    assert_eq!(
        rows,
        [
            ("git.push", "confirmed", Some("rule"), None),
            ("pr.open", "confirmed", Some("rule"), None)
        ]
    );
    let spans = world.ledger().spans_of(Some(KEY)).unwrap();
    assert_eq!(spans[0].state, "open-pr");
    assert_eq!(spans[0].waits_on.as_deref(), Some("verbs:git.push,pr.open"));
    assert_eq!(
        world.ledger().step_turn_ids(KEY, "open-pr").unwrap().len(),
        0
    );

    // The team marks the pull request ready, and the ticket ends.
    world.world_cmd(&["ready", world.root(), "1"]);
    world.tick_until("the end", LONG, |w| w.counts()["finished"].is_string());
    assert_eq!(world.acts_of("git.push"), 1);
    assert_eq!(world.acts_of("pr.open"), 1);
}

#[test]
fn a_verb_in_the_mode_ask_waits_for_vision_approve_and_then_the_ticket_moves() {
    let world = World::new();
    act_adapter(&world, PUSH_AND_OPEN, "");
    world.edit_config(|doc| doc["modes"]["git.push"] = toml_edit::value("ask"));
    let head = take_with_change(&world);
    world.tick_until("the approval", LONG, |w| waiting_operation(w).is_some());
    let (verb, commit) = waiting_operation(&world).unwrap();
    assert_eq!(
        (verb.as_str(), commit.as_str()),
        ("git.push", head.as_str())
    );
    assert!(
        world.flags().is_empty(),
        "the mode ask is a need, not a flag"
    );
    assert_eq!(
        ticket_status(&world)["now"],
        "waits for your approval of git.push. Run `vision needs`"
    );
    for _ in 0..2 {
        world.tick();
    }
    assert_eq!(world.step().as_deref(), Some("open-pr"));
    assert_eq!(
        world.acts_of("git.push"),
        0,
        "nothing runs before the approval"
    );
    assert_eq!(world.acts_of("pr.open"), 0, "pr.open waits for the push");

    world.run(&["approve", KEY, &verb, &commit]).expect_code(0);
    assert_eq!(world.acts_of("git.push"), 1);
    world.tick_to("look");
    assert_eq!(world.acts_of("pr.open"), 1);
    assert!(world.flags().is_empty());
}

#[test]
fn hold_and_shadow_flag_an_act_step_and_promote_and_retry_move_it() {
    let world = World::new();
    act_adapter(&world, PUSH_AND_OPEN, "");
    world.edit_config(|doc| {
        doc["modes"]["git.push"] = toml_edit::value("hold");
        doc["modes"]["pr.open"] = toml_edit::value("shadow");
    });
    take_with_change(&world);

    // The mode hold keeps the push back and asks for the person.
    world.tick_until("the flag of hold", LONG, |w| has_flag(w, "needs-you"));
    assert_eq!(world.step().as_deref(), Some("open-pr"));
    let reason = world.reason("needs-you");
    assert!(
        reason.contains("git.push is in the mode hold, so the executor called nothing"),
        "{reason}"
    );
    assert_eq!(world.acts_of("git.push"), 0);
    // The person moves git.push up to auto, and the retry asks for it again.
    for _ in 0..3 {
        world.run(&["promote", "git.push"]).expect_code(0);
    }
    let retry = world.run(&["retry", KEY]);
    retry.expect_code(0);
    assert!(
        retry.stdout.contains(
            "The step runs no turn, so the next tick runs the operations of the step again"
        ),
        "{retry:?}"
    );

    // The push runs, and the mode shadow keeps pr.open back with what it intends.
    world.tick_until("the flag of shadow", LONG, |w| has_flag(w, "needs-you"));
    assert_eq!(world.acts_of("git.push"), 1);
    assert_eq!(world.acts_of("pr.open"), 0);
    let reason = world.reason("needs-you");
    assert!(
        reason.contains("pr.open is in the mode shadow: the executor recorded what it would do"),
        "{reason}"
    );
    let open = world.ledger().operations_of(KEY, Some("pr.open")).unwrap();
    assert_eq!(open.len(), 1);
    assert_eq!(open[0].state, "pending");
    assert!(
        open[0]
            .result
            .as_deref()
            .unwrap_or("")
            .contains("Add subtract"),
        "the intent holds the text of pr.md: {:?}",
        open[0].result
    );
    for _ in 0..2 {
        world.run(&["promote", "pr.open"]).expect_code(0);
    }
    world.run(&["retry", KEY]).expect_code(0);
    world.tick_to("look");
    assert_eq!(world.acts_of("pr.open"), 1);
    assert_eq!(world.acts_of("git.push"), 1, "the confirmed push stays");
    assert!(world.flags().is_empty());
}

#[test]
fn a_missing_document_and_a_failed_operation_flag_an_act_step_and_a_retry_runs_it_again() {
    let world = World::new();
    act_adapter(
        &world,
        r#"[{ verb = "pr.open", args = { text = "{doc:pr.md}" } }]"#,
        "",
    );
    world.run(&["take", KEY]).expect_code(0);
    world.commit_in(world.clone(), &[("notes.txt", "a note\n")], "Add a note");

    // A `{doc:NAME}` with no document stops the ticket, and nothing runs.
    world.tick_until("the flag of the missing document", LONG, |w| {
        has_flag(w, "needs-you")
    });
    let reason = world.reason("needs-you");
    assert!(
        reason.contains(
            "an argument names the document pr.md, which the ticket folder does not have"
        ),
        "{reason}"
    );
    assert!(world.ledger().operations_of(KEY, None).unwrap().is_empty());

    // With the document, pr.open runs, and it fails: the branch is not on the remote.
    write_doc(&world, "pr.md", PR);
    world.run(&["retry", KEY]).expect_code(0);
    world.tick_until("the flag of the failure", LONG, |w| {
        has_flag(w, "needs-you")
    });
    let reason = world.reason("needs-you");
    assert!(
        reason.contains(
            "pr.open failed: the branch vision/DEMO-12 is not on the remote: push it first"
        ),
        "{reason}"
    );
    assert_eq!(world.step().as_deref(), Some("open-pr"));
    let open = world.ledger().operations_of(KEY, Some("pr.open")).unwrap();
    assert_eq!(open.len(), 1);
    assert_eq!(open[0].state, "failed");

    // The person pushes the branch, and the retry frees the failed key: pr.open runs again.
    git(
        world.clone(),
        &[
            "push",
            "--quiet",
            "origin",
            "HEAD:refs/heads/vision/DEMO-12",
        ],
    );
    let retry = world.run(&["retry", KEY]);
    retry.expect_code(0);
    assert!(
        retry.stdout.contains("1 failed operation(s) can run again"),
        "{retry:?}"
    );
    world.tick_to("look");
    let open = world.ledger().operations_of(KEY, Some("pr.open")).unwrap();
    assert_eq!(open.len(), 2, "the first row stays as the record");
    assert_eq!(open[1].state, "confirmed");
    assert_eq!(world.world_json("prs.json")["prs"][0]["number"], 1);
}

#[test]
fn a_second_entry_at_a_new_head_pushes_again_pr_open_answers_with_the_open_pull_request_and_the_cap_counts(
) {
    let world = World::new();
    act_adapter(&world, PUSH_AND_OPEN, "cap = 2");
    let first = take_with_change(&world);
    world.tick_until("the second entry", LONG, |w| {
        w.counts()["entries"]["open-pr"] == 2
    });
    // The tick of the move ran no verb of the step.
    assert_eq!(
        (world.acts_of("git.push"), world.acts_of("pr.open")),
        (1, 1)
    );
    assert_eq!(remote_head(&world).as_deref(), Some(first.as_str()));

    // A new commit before the verbs of the second entry run.
    let second = world.commit_in(
        world.clone(),
        &[("notes.txt", "subtract comes next, with a test.\n")],
        "Name the test",
    );
    world.tick_until("the end of the second entry", LONG, |w| {
        w.step().as_deref() == Some("look")
    });
    assert_eq!(world.acts_of("git.push"), 2, "the new head is pushed");
    assert_eq!(remote_head(&world).as_deref(), Some(second.as_str()));
    assert_eq!(world.acts_of("pr.open"), 2);
    let prs = world.world_json("prs.json");
    assert_eq!(
        prs["prs"].as_array().unwrap().len(),
        1,
        "pr.open answers with the open pull request"
    );
    let open = world.ledger().operations_of(KEY, Some("pr.open")).unwrap();
    assert_eq!(open.len(), 2);
    assert!(open.iter().all(|o| o.state == "confirmed"), "{open:?}");
    assert_eq!(open[1].external_id.as_deref(), Some("1"));
    assert_eq!(world.row().unwrap().pr, Some(1));

    // The third entry is above the cap: the ticket stops, and no verb runs.
    world.tick_until("the cap", LONG, |w| has_flag(w, "needs-you"));
    assert_eq!(world.step().as_deref(), Some("open-pr"));
    assert_eq!(world.counts()["entries"]["open-pr"], 3);
    let reason = world.reason("needs-you");
    assert!(
        reason.contains("the step open-pr reached its cap 2 = 2: this is entry 3"),
        "{reason}"
    );
    assert_eq!(world.acts_of("git.push"), 2);
    assert_eq!(world.acts_of("pr.open"), 2);

    // The person lets it go on: the retry does not count as an entry, and each verb of the
    // head of now is confirmed already, so the ticket moves with no new push.
    world.run(&["retry", KEY]).expect_code(0);
    world.tick_to("look");
    assert_eq!(world.counts()["entries"]["open-pr"], 3);
    assert_eq!(world.acts_of("git.push"), 2);
    assert_eq!(
        world
            .ledger()
            .operations_of(KEY, Some("git.push"))
            .unwrap()
            .len(),
        2
    );
}

#[test]
fn the_move_of_an_act_step_counts_as_an_entry_for_the_cap_of_its_next() {
    let world = World::new();
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
id = "push"
kind = "act"
run = ["git.push"]
next = "open-pr"

[[step]]
id = "open-pr"
kind = "act"
run = [{{ verb = "pr.open", args = {{ text = "{{doc:pr.md}}" }} }}]
cap = 1
next = "look"

[[step]]
id = "look"
kind = "wait"
watch = "pr.draft"
on = {{ "true" = "push", "false" = "done" }}
every = 1

[[step]]
id = "done"
kind = "end"
{VERBS}"#
        )
    });
    take_with_change(&world);
    // The draft sends the ticket back to push. Its push at the same head is confirmed, so
    // push moves at once, and that move is the second entry into open-pr.
    world.tick_until("the cap of open-pr", LONG, |w| has_flag(w, "needs-you"));
    assert_eq!(world.step().as_deref(), Some("open-pr"));
    assert_eq!(world.counts()["entries"]["open-pr"], 2);
    let reason = world.reason("needs-you");
    assert!(
        reason.contains("the step open-pr reached its cap 1 = 1: this is entry 2"),
        "{reason}"
    );
    assert_eq!(
        (world.acts_of("git.push"), world.acts_of("pr.open")),
        (1, 1),
        "the second visit of push ran no verb, and the cap stopped pr.open"
    );
}

#[test]
fn a_when_that_does_not_hold_skips_an_act_step_to_its_next() {
    let world = World::with_clones(2);
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

[kinds]
map = {{ Bug = "bug", Story = "feature" }}
default = "feature"

[[step]]
id = "push"
kind = "act"
run = ["git.push"]
when = ["bug"]
next = "done"

[[step]]
id = "done"
kind = "end"
{VERBS}"#
        )
    });
    world.world_cmd(&["type", world.root(), KEY, "Bug"]);
    world.run(&["take", KEY, "DEMO-13"]).expect_code(0);
    let ledger = world.ledger();
    let finished = |key: &str| {
        ledger
            .ticket(key)
            .unwrap()
            .and_then(|r| r.counts)
            .and_then(|c| serde_json::from_str::<Value>(&c).ok())
            .is_some_and(|c| c["finished"].is_string())
    };
    world.tick_until("the end of both tickets", LONG, |_| {
        finished(KEY) && finished("DEMO-13")
    });
    // The bug ran the step; the feature skipped it, with the event skipped.
    assert_eq!(acts_for(&world, "git.push", KEY), 1);
    assert_eq!(acts_for(&world, "git.push", "DEMO-13"), 0);
    let skipped: Vec<Value> = ledger
        .events_of_ticket("DEMO-13")
        .unwrap()
        .into_iter()
        .filter(|e| e.kind == "skipped")
        .filter_map(|e| serde_json::from_str(e.detail.as_deref()?).ok())
        .collect();
    assert_eq!(skipped.len(), 1, "{skipped:?}");
    assert_eq!(skipped[0]["step"], "push");
    assert_eq!(skipped[0]["to"], "done");
    let spans = ledger.spans_of(Some("DEMO-13")).unwrap();
    assert_eq!(spans[0].waits_on.as_deref(), Some("skipped"));
    assert!(ledger
        .events_of_ticket(KEY)
        .unwrap()
        .iter()
        .all(|e| e.kind != "skipped"));
}
