//! The pipeline of section 11.3 through `vision tick` and the ticket commands, with a demo world,
//! clones, and the fake `claude`.

mod pipeline_support;

use std::time::{Duration, Instant};

use pipeline_support::{
    ends_with, git, plan_hash_of, result, runs_then, simple_scenarios, take_and_approve,
    without_resume, World, KEY, PLAN,
};
use serde_json::{json, Value};
use vision::paths::{TicketKey, TurnId};

const LONG: Duration = Duration::from_secs(90);

fn status(world: &World) -> Value {
    world.run(&["status", "--json"]).json()
}

fn ticket_status(world: &World, key: &str) -> Value {
    status(world)["tickets"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["key"] == key)
        .cloned()
        .unwrap_or(Value::Null)
}

/// The facts file of a turn of DEMO-12.
fn facts_of(world: &World, turn: &str) -> Value {
    let paths = world.ctx().paths;
    let dir = paths.turn(
        &TicketKey::parse(KEY).unwrap(),
        &TurnId::parse(turn).unwrap(),
    );
    serde_json::from_str(&std::fs::read_to_string(dir.facts).unwrap()).unwrap()
}

/// The turn rows of one step of DEMO-12, oldest first.
fn step_turns(world: &World, step: &str) -> Vec<vision::ledger::turns::TurnRow> {
    world
        .ledger()
        .turns_of_ticket(KEY)
        .unwrap()
        .into_iter()
        .filter(|t| t.step.as_deref() == Some(step))
        .collect()
}

fn has_flag(world: &World, flag: &str) -> bool {
    world.flags().iter().any(|f| f == flag)
}

#[test]
fn take_refuses_a_reserved_key_a_key_that_is_not_eligible_and_a_held_key() {
    let world = World::new();
    let reserved = world.run(&["take", "curator"]);
    reserved.expect_code(1);
    assert!(reserved.stderr.contains("reserved key"), "{reserved:?}");
    let unknown = world.run(&["take", "DEMO-99"]);
    unknown.expect_code(1);
    assert!(
        unknown
            .stderr
            .contains("the queue of the tracker has no ticket DEMO-99"),
        "{unknown:?}"
    );
    world.world_cmd(&["status", world.root(), "DEMO-13", "Done"]);
    let done = world.run(&["take", "DEMO-13"]);
    done.expect_code(1);
    assert!(
        done.stderr
            .contains("DEMO-13 is not eligible: the status is Done, not To Do"),
        "{done:?}"
    );
    assert!(world.ledger().ticket("DEMO-13").unwrap().is_none());

    world.run(&["take", KEY]).expect_code(0);
    let again = world.run(&["take", KEY]);
    again.expect_code(1);
    assert!(
        again
            .stderr
            .contains("Vision already holds DEMO-12: it is at the step refine"),
        "{again:?}"
    );
    let events = world.event_kinds();
    assert_eq!(events, ["take"], "one take, with its event");
    let spans = world.ledger().spans_of(Some(KEY)).unwrap();
    assert_eq!(spans.len(), 1);
    assert_eq!(spans[0].state, "refine");
    assert_eq!(spans[0].reason.as_deref(), Some("take"));
}

#[test]
fn take_with_no_free_clone_queues_the_ticket_and_says_who_holds_each_clone() {
    let world = World::new();
    world.run(&["take", KEY]).expect_code(0);
    let busy = world.run(&["take", "DEMO-13"]);
    busy.expect_code(0);
    assert!(
        busy.stdout.starts_with(
            "Queued DEMO-13 at the step refine: no slot is free, and it is 1st in line."
        ),
        "{busy:?}"
    );
    assert!(
        busy.stdout.contains(&format!(
            "{}: DEMO-12 holds it (step refine)",
            world.clone().display()
        )),
        "{busy:?}"
    );
    let row = world.ledger().ticket("DEMO-13").unwrap().unwrap();
    assert_eq!(row.clone, None);
    assert_eq!(row.state.as_deref(), Some("refine"));
    assert_eq!(row.flags.as_deref(), Some(r#"["waits-for-slot"]"#));
    let again = world.run(&["take", "DEMO-13"]);
    again.expect_code(1);
    assert!(
        again
            .stderr
            .contains("Vision already holds DEMO-13: it is at the step refine"),
        "{again:?}"
    );

    let none = World::new();
    none.edit_config(|doc| doc["clones"] = toml_edit::value(toml_edit::Array::new()));
    let run = none.run(&["take", KEY]);
    run.expect_code(1);
    assert!(
        run.stderr
            .contains("config.toml lists no clone in `clones`"),
        "{run:?}"
    );
    assert!(none.row().is_none());
}

#[test]
fn retry_with_a_new_branch_moves_the_ticket_to_a_branch_of_the_pattern_made_from_the_base() {
    let world = World::new();
    world.run(&["take", KEY]).expect_code(0);
    let clone = world.clone().to_path_buf();
    let old = world.commit_in(&clone, &[("old.txt", "old work\n")], "Old work");

    // The ticket is on the branch of the pattern: there is no new branch to make.
    let same = world.run(&["retry", KEY, "--step", "refine", "--new-branch"]);
    same.expect_code(1);
    assert!(
        same.stderr
            .contains("DEMO-12 is on the branch vision/DEMO-12 already"),
        "{same:?}"
    );

    // Another pattern, as for a ticket that runs on a branch made by hand.
    world.write_adapter(|text| {
        text.replacen("branch = \"vision/{key}\"", "branch = \"fresh/{key}\"", 1)
    });
    std::fs::write(clone.join("scratch.txt"), "work in progress").unwrap();
    let dirty = world.run(&["retry", KEY, "--step", "refine", "--new-branch"]);
    dirty.expect_code(1);
    assert!(
        dirty.stderr.contains("such as `?? scratch.txt`"),
        "{dirty:?}"
    );
    std::fs::remove_file(clone.join("scratch.txt")).unwrap();

    let moved = world.run(&["retry", KEY, "--step", "refine", "--new-branch"]);
    moved.expect_code(0);
    assert!(
        moved
            .stdout
            .contains("DEMO-12 is on the new branch fresh/DEMO-12 from origin/main"),
        "{moved:?}"
    );
    assert!(
        moved
            .stdout
            .contains("the old branch vision/DEMO-12 stays in git"),
        "{moved:?}"
    );
    let main = git(&clone, &["rev-parse", "origin/main"]);
    assert_eq!(
        git(&clone, &["rev-parse", "--abbrev-ref", "HEAD"]),
        "fresh/DEMO-12"
    );
    assert_eq!(git(&clone, &["rev-parse", "HEAD"]), main);
    assert_eq!(
        git(&clone, &["rev-parse", "vision/DEMO-12"]),
        old,
        "the old branch keeps its commit"
    );
    let row = world.row().unwrap();
    assert_eq!(row.branch.as_deref(), Some("fresh/DEMO-12"));
    assert_eq!(row.head.as_deref(), Some(main.as_str()));
    assert_eq!(row.pr, None);
    assert_eq!(world.step().as_deref(), Some("refine"));
    let message = world.counts()["visit"]["message"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    assert!(
        message.contains("The ticket runs on a new branch now: fresh/DEMO-12"),
        "{message}"
    );
    let events = world.event_kinds();
    assert!(
        events.ends_with(&["branch".to_string(), "move".to_string()]),
        "{events:?}"
    );

    // The ticket is on the branch of the pattern now, so a second new branch is refused.
    world.run(&["retry", KEY, "--new-branch"]).expect_code(1);
}

#[test]
fn a_wrong_plan_hash_is_refused_and_an_edited_plan_keeps_the_first_text_of_the_agent() {
    let world = World::new();
    simple_scenarios(&world);
    let early = world.run(&["approve-plan", KEY, "0123456"]);
    early.expect_code(1);
    assert!(
        early.stderr.contains("Run `vision take DEMO-12`"),
        "{early:?}"
    );
    world.run(&["take", KEY]).expect_code(0);
    let not_yet = world.run(&["approve-plan", KEY, "0123456"]);
    not_yet.expect_code(1);
    assert!(
        not_yet
            .stderr
            .contains("is at the step refine, so no plan of it waits"),
        "{not_yet:?}"
    );
    world.tick_to("plan-ready");

    let plan = world.run(&["plan", KEY]);
    plan.expect_code(0);
    assert!(plan.stdout.contains(PLAN), "{plan:?}");
    let first = plan_hash_of(&plan.stdout);
    assert_eq!(first.len(), 12);
    let wrong = world.run(&["approve-plan", KEY, "0000000"]);
    wrong.expect_code(1);
    assert!(
        wrong.stderr.contains("does not fit the plan of now"),
        "{wrong:?}"
    );
    assert!(
        world.run(&["approve-plan", KEY, &first[..4]]).code == 1,
        "a hash needs 7 characters"
    );

    // The person edits the plan: the old hash no longer fits, and the text of the agent stays.
    let path = world.vision.join("tickets").join(KEY).join("plan.md");
    let edited = format!("{PLAN}\n- Keep the old add.\n");
    std::fs::write(&path, &edited).unwrap();
    let stale = world.run(&["approve-plan", KEY, &first]);
    stale.expect_code(1);
    assert!(
        stale
            .stderr
            .contains("The plan changed, or the hash is wrong"),
        "{stale:?}"
    );
    let plan = world.run(&["plan", KEY]);
    assert!(plan.stdout.contains("You edited plan.md"), "{plan:?}");
    let second = plan_hash_of(&plan.stdout);
    assert_ne!(first, second);
    let need = status(&world)["needs"]
        .as_array()
        .unwrap()
        .iter()
        .find(|n| n["kind"] == "plan")
        .cloned()
        .unwrap();
    assert_eq!(
        need["id"],
        second.as_str(),
        "the need names the hash of now"
    );
    world.run(&["approve-plan", KEY, &second]).expect_code(0);
    assert_eq!(
        world.doc("approved-plan.md").as_deref(),
        Some(edited.as_str())
    );
    assert_eq!(world.doc("plan.agent.md").as_deref(), Some(PLAN));
    let human = world.ledger().human_of(Some(KEY)).unwrap();
    assert_eq!(human.len(), 1);
    assert_eq!(human[0].kind, "approve-plan");
    assert!(human[0].seconds.is_some());
    let again = world.run(&["approve-plan", KEY, &second]);
    again.expect_code(0);
    assert!(again.stdout.contains("already approved"), "{again:?}");
    world.tick_to("build");
}

#[test]
fn a_missing_end_fact_resumes_once_then_needs_you_and_retry_enters_the_step_again() {
    let world = World::new();
    let partial = result("Refined.", &[("refine.md", "Research.")]);
    world.scenario(
        "scout",
        "refine",
        json!([
            ends_with(partial.clone()),
            ends_with(partial),
            ends_with(result(
                "Refined.",
                &[
                    ("refine.md", "Research."),
                    ("description.md", "Add subtract.")
                ]
            ))
        ]),
    );
    world.run(&["take", KEY]).expect_code(0);
    world.tick_until("the flag needs-you", LONG, |w| has_flag(w, "needs-you"));
    let turns = step_turns(&world, "refine");
    assert_eq!(turns.len(), 2, "one resume, no more");
    assert_eq!(
        turns[0].end_fact.as_deref(),
        Some("failed:doc:description.md")
    );
    assert_eq!(
        turns[1].end_fact.as_deref(),
        Some("failed:doc:description.md")
    );
    assert_eq!(
        turns[1].resumes, turns[0].session,
        "the resume continues the session"
    );
    let message = facts_of(&world, &turns[1].id)["message"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(
        message.contains("The end fact doc:description.md does not hold: the result has no document description.md"),
        "{message}"
    );
    assert!(world
        .reason("needs-you")
        .contains("did not hold after a resume"));
    let view = ticket_status(&world, KEY);
    assert_eq!(view["flags"][0]["flag"], "needs-you");
    assert_eq!(view["flags"][0]["command"], "vision retry DEMO-12");
    let flag_needs: Vec<Value> = status(&world)["needs"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|n| n["kind"] == "flag")
        .cloned()
        .collect();
    assert_eq!(flag_needs.len(), 1);
    assert_eq!(flag_needs[0]["id"], "needs-you");
    // Nothing starts while the flag is set.
    world.tick();
    assert_eq!(step_turns(&world, "refine").len(), 2);

    let retry = world.run(&["retry", KEY]);
    retry.expect_code(0);
    assert!(retry.stdout.contains("cleared: needs-you"), "{retry:?}");
    assert!(world.flags().is_empty());
    world.tick_to("plan");
    let turns = step_turns(&world, "refine");
    assert_eq!(turns.len(), 3);
    assert_eq!(turns[2].resumes, None, "a retry starts a new session");
    assert!(facts_of(&world, &turns[2].id)["message"]
        .as_str()
        .unwrap()
        .contains("The person ran `vision retry`"));
    let human = world.ledger().human_of(Some(KEY)).unwrap();
    assert_eq!(human.last().unwrap().kind, "retry");
    assert!(human.last().unwrap().seconds.is_some());
}

#[test]
fn the_proof_of_the_build_gets_the_plan_and_a_missing_document_is_an_empty_text() {
    let world = World::new();
    world.write_adapter(|text| {
        let old = "\"doc:pr.md\", { job = \"proof\", args = { plan = \"{doc:approved-plan.md}\" } }]";
        assert!(text.contains(old));
        text.replacen(
            old,
            "\"doc:pr.md\", { job = \"proof\", args = { plan = \"{doc:approved-plan.md}\", note = \"{doc:nothing.md}\" } }]",
            1,
        )
    });
    simple_scenarios(&world);
    // A change with no new behavior and no test: only the plan can make the proof hold.
    let plan = "# Plan of DEMO-12\n\nA comment for the reader of calc.py.\n\n- **Proof:** green\n";
    world.scenario(
        "builder",
        "plan",
        json!([ends_with(result("Planned.", &[("plan.md", plan)]))]),
    );
    world.scenario(
        "builder",
        "build",
        json!([runs_then(
            "printf '# The functions of the toy calculator.\\n' >> calc.py && git commit -q -am 'Comment calc.py'",
            result("Built.", &[("pr.md", "Comment calc.py\n\nNo new behavior.")])
        )]),
    );
    take_and_approve(&world);
    world.tick_to("review");
    let head = git(world.clone(), &["rev-parse", "HEAD"]);
    let jobs: Vec<(String, String)> = world
        .ledger()
        .checks_of(KEY, Some(&head))
        .unwrap()
        .into_iter()
        .filter(|c| c.kind == "job" && c.source == "job:proof")
        .map(|c| (c.command, c.result))
        .collect();
    let args = json!({"plan": plan, "note": ""});
    assert_eq!(
        jobs,
        [(
            vision::executor::job_command("proof", &args),
            "pass".to_string()
        )],
        "the proof ran once, with the text of the plan and an empty text for the missing document"
    );
    assert!(world.doc("proof.txt").is_some(), "the proof wrote its log");
}

#[test]
fn a_verb_in_the_mode_ask_waits_for_vision_approve_and_then_goes_on() {
    let world = World::new();
    simple_scenarios(&world);
    world.edit_config(|doc| doc["modes"]["tracker.describe"] = toml_edit::value("ask"));
    world.run(&["take", KEY]).expect_code(0);
    world.tick_until("the approval", LONG, |w| {
        status(w)["needs"]
            .as_array()
            .unwrap()
            .iter()
            .any(|n| n["kind"] == "operation")
    });
    let needs = status(&world)["needs"].as_array().unwrap().clone();
    assert_eq!(needs.len(), 1, "one need, reported once: {needs:?}");
    let id = needs[0]["id"].as_str().unwrap();
    let (verb, commit) = id.split_once(' ').unwrap();
    assert_eq!(verb, "tracker.describe");
    assert!(
        world.flags().is_empty(),
        "the mode ask is a need, not a flag"
    );
    world.tick();
    assert_eq!(world.step().as_deref(), Some("refine"));
    assert_eq!(world.acts_of("tracker.describe"), 0);
    world.run(&["approve", KEY, verb, commit]).expect_code(0);
    world.tick_to("plan");
    assert_eq!(world.acts_of("tracker.describe"), 1);
}

#[test]
fn a_write_in_the_mode_shadow_runs_after_promote_retry_and_approve() {
    let world = World::new();
    simple_scenarios(&world);
    world.edit_config(|doc| doc["modes"]["tracker.describe"] = toml_edit::value("shadow"));
    world.run(&["take", KEY]).expect_code(0);
    world.tick_until("the flag needs-you", LONG, |w| has_flag(w, "needs-you"));
    assert_eq!(world.step().as_deref(), Some("refine"));
    // With the mode unchanged, a retry asks again, and the write is kept back again.
    world.run(&["retry", KEY]).expect_code(0);
    world.tick();
    assert!(has_flag(&world, "needs-you"));
    assert_eq!(world.acts_of("tracker.describe"), 0);
    // After the promote, the next tick asks for the same write again in the mode ask.
    world.run(&["promote", "tracker.describe"]).expect_code(0);
    world.run(&["retry", KEY]).expect_code(0);
    world.tick();
    assert!(world.flags().is_empty(), "{:?}", world.flags());
    let needs = status(&world)["needs"].as_array().unwrap().clone();
    let op = needs
        .iter()
        .find(|n| n["kind"] == "operation")
        .unwrap_or_else(|| panic!("no write waits: {needs:?}"));
    let (verb, commit) = op["id"].as_str().unwrap().split_once(' ').unwrap();
    assert_eq!(verb, "tracker.describe");
    assert_eq!(world.acts_of("tracker.describe"), 0);
    world.run(&["approve", KEY, verb, commit]).expect_code(0);
    world.tick_to("plan");
    assert_eq!(world.acts_of("tracker.describe"), 1);
}

#[test]
fn a_refused_gate_needs_you_and_a_retry_runs_the_verb_again() {
    let world = World::new();
    simple_scenarios(&world);
    let key_line = format!("{}{}", "-----BEGIN OPENSSH ", "PRIVATE KEY-----");
    let command = format!(
        "{} && printf '%s\\n' '{key_line}' > id_demo && git add -A && git commit -q -m 'Add a key' && git rm -q id_demo && git commit -q -m 'Remove the key'",
        pipeline_support::BUILD_COMMAND
    );
    world.scenario(
        "builder",
        "build",
        json!([runs_then(
            &command,
            result("Built.", &[("pr.md", "Add subtract\n\nBody.")])
        )]),
    );
    world.scenario(
        "reviewer",
        "review",
        json!([ends_with(result(
            "Reviewed.",
            &[("review.md", "# Review\n\n## Findings\n\nNone.\n")]
        ))]),
    );
    take_and_approve(&world);
    // The commits stay in the clone through the review: the act step open-pr pushes them.
    world.tick_until("the refused push", LONG, |w| has_flag(w, "needs-you"));
    assert_eq!(world.step().as_deref(), Some("open-pr"));
    assert_eq!(step_turns(&world, "review").len(), 1);
    let reason = world.reason("needs-you");
    assert!(
        reason.contains("git.push was refused by the secrets gate"),
        "{reason}"
    );
    assert!(
        !reason.contains("BEGIN OPENSSH"),
        "a refusal never shows the secret: {reason}"
    );
    assert_eq!(world.acts_of("git.push"), 0);
    let pushes = world.ledger().operations_of(KEY, Some("git.push")).unwrap();
    assert_eq!(pushes.len(), 1);
    assert_eq!(pushes[0].state, "failed");

    // A retry frees the failed key, so the push is requested again, and the gate refuses again.
    let retry = world.run(&["retry", KEY]);
    retry.expect_code(0);
    assert!(
        retry.stdout.contains("1 failed operation(s) can run again"),
        "{retry:?}"
    );
    assert!(
        retry
            .stdout
            .contains("runs the operations of the step again"),
        "{retry:?}"
    );
    world.tick_until("the second refusal", LONG, |w| has_flag(w, "needs-you"));
    let pushes = world.ledger().operations_of(KEY, Some("git.push")).unwrap();
    assert_eq!(pushes.len(), 2, "the first row stays as the record");
    assert!(pushes[0]
        .idem
        .as_deref()
        .unwrap()
        .ends_with(&format!(":{}", pushes[0].id)));
    assert_eq!(step_turns(&world, "build").len(), 1, "no new build turn");
    assert_eq!(step_turns(&world, "review").len(), 1, "no new review turn");
    assert_eq!(world.acts_of("pr.open"), 0, "pr.open waits for the push");

    // While an operation is uncertain, retry refuses.
    world
        .ledger()
        .conn()
        .execute(
            "UPDATE operations SET state = 'uncertain' WHERE id = ?1",
            [&pushes[1].id],
        )
        .unwrap();
    let refused = world.run(&["retry", KEY]);
    refused.expect_code(1);
    assert!(refused.stderr.contains(&pushes[1].id), "{refused:?}");
    assert!(
        refused
            .stderr
            .contains("the reconcile step of the tick must settle it first"),
        "{refused:?}"
    );
}

#[test]
fn one_entry_above_the_review_cap_needs_you_with_the_open_findings() {
    let world = World::new();
    simple_scenarios(&world);
    world.write_adapter(|text| {
        // No pull request exists before open-pr, so its draft flag is the value none.
        let old = "cap = \"review_rounds\"\nnext = \"open-pr\"";
        assert!(text.contains(old));
        text.replacen(
            old,
            "cap = \"review_rounds\"\nnext = \"again\"\n\n[[step]]\nid = \"again\"\nkind = \"wait\"\nwatch = \"pr.draft\"\non = { \"none\" = \"review\" }\nevery = 1",
            1,
        )
    });
    world.edit_config(|doc| doc["caps"]["review_rounds"] = toml_edit::value(1));
    let review = "# Review\n\n## Findings\n\n### F1: subtract has no test for floats (major)\nKept.\n\n## Open findings\n\n- F1: subtract has no test for floats\n";
    world.scenario(
        "reviewer",
        "review",
        json!([ends_with(result("Reviewed.", &[("review.md", review)]))]),
    );
    take_and_approve(&world);
    world.tick_until("the cap", LONG, |w| has_flag(w, "needs-you"));
    assert_eq!(world.step().as_deref(), Some("review"));
    assert_eq!(
        step_turns(&world, "review").len(),
        1,
        "the entry above the cap starts no turn"
    );
    let reason = world.reason("needs-you");
    assert!(
        reason.contains("reached its cap review_rounds = 1: this is entry 2"),
        "{reason}"
    );
    assert!(
        reason.contains("review.md: F1: subtract has no test for floats"),
        "{reason}"
    );
    assert_eq!(world.counts()["entries"]["review"], 2);
    let findings = world.ledger().findings_of(KEY).unwrap();
    assert_eq!(findings.len(), 1);
    assert_eq!(findings[0].priority.as_deref(), Some("major"));

    // The person lets it go on: the retry does not count as an entry.
    world.run(&["retry", KEY]).expect_code(0);
    world.tick_until("the second review turn", LONG, |w| {
        step_turns(w, "review").len() == 2
    });
    assert_eq!(world.counts()["entries"]["review"], 2);
}

#[test]
fn stop_frees_the_clone_and_take_starts_the_ticket_again_in_the_clone_of_its_branch() {
    let world = World::new();
    world.scenario(
        "scout",
        "refine",
        json!([
            [{"do": "init"}, {"do": "sleep", "seconds": 60}, {"do": "result", "structured_output": {"kind": "result", "summary": "late"}}],
            ends_with(result("Refined.", &[("refine.md", "R."), ("description.md", "D.")]))
        ]),
    );
    world.run(&["take", KEY]).expect_code(0);
    // The stop must come after the fake session took the first list of its scenario. On a
    // fast machine the turn row exists before the fake starts, and a stop at that moment
    // leaves the first list, which sleeps, for the next run.
    world.tick_until("the running turn", LONG, |w| {
        w.ledger().running_turns_of_ticket(KEY).unwrap() > 0
            && w.scenario_runs("scout", "refine") >= 1
    });
    let busy = world.run(&["retry", KEY]);
    busy.expect_code(1);
    assert!(busy.stderr.contains("runs now"), "{busy:?}");

    let stop = world.run(&["stop", KEY]);
    stop.expect_code(0);
    assert!(stop.stdout.contains("Killed the turn(s)"), "{stop:?}");
    assert!(has_flag(&world, "stopped"));
    let view = ticket_status(&world, KEY);
    assert_eq!(view["flags"][0]["command"], "vision take DEMO-12");
    assert!(world.run(&["retry", KEY]).stderr.contains("is stopped"));
    let human = world.ledger().human_of(Some(KEY)).unwrap();
    assert_eq!(human[0].kind, "stop");
    world.tick();
    world.settle();
    assert_eq!(
        step_turns(&world, "refine").len(),
        1,
        "a stopped ticket starts nothing"
    );
    assert_eq!(
        git(world.clone(), &["rev-parse", "--abbrev-ref", "HEAD"]),
        "vision/DEMO-12",
        "the branch stays"
    );

    // The freed clone takes a different ticket. DEMO-13 works there, so a continue of the
    // stopped one finds no slot and says who holds each clone (section 11.3).
    world.run(&["take", "DEMO-13"]).expect_code(0);
    let blocked = world.run(&["take", KEY]);
    blocked.expect_code(1);
    assert!(
        blocked.stderr.contains("no slot is free for DEMO-12 now")
            && blocked.stderr.contains("DEMO-13 holds it (step refine)"),
        "{blocked:?}"
    );
    world.run(&["stop", "DEMO-13"]).expect_code(0);
    let again = world.run(&["take", KEY]);
    again.expect_code(0);
    assert!(
        again.stdout.contains("starts again at the step refine"),
        "{again:?}"
    );
    assert_eq!(
        git(world.clone(), &["rev-parse", "--abbrev-ref", "HEAD"]),
        "vision/DEMO-12"
    );
    assert!(world.flags().is_empty());
    world.tick_to("plan");
    assert_eq!(step_turns(&world, "refine").len(), 2);
}

#[test]
fn the_limit_flag_clears_by_itself_and_a_budget_refusal_needs_you() {
    let world = World::new();
    simple_scenarios(&world);
    world.run(&["take", KEY]).expect_code(0);
    let resets =
        vision::util::time::rfc3339(vision::util::time::now() + ::time::Duration::seconds(3));
    world
        .ledger()
        .conn()
        .execute(
            "INSERT INTO turns (id, ticket, agent, step, started, ended, status, five_hour, five_hour_resets)
             VALUES ('t-limit', 'DEMO-77', 'scout', 'refine', ?1, ?1, 'done', 0.95, ?2)",
            rusqlite::params![vision::util::time::now_rfc3339(), resets],
        )
        .unwrap();
    world.tick();
    assert!(has_flag(&world, "waits-for-limit"), "{:?}", world.flags());
    assert!(world
        .reason("waits-for-limit")
        .contains("refused by limits.five_hour"));
    assert_eq!(step_turns(&world, "refine").len(), 0);
    assert!(ticket_status(&world, KEY)["now"]
        .as_str()
        .unwrap()
        .starts_with("waits for the limit"));
    let needs = status(&world)["needs"].as_array().unwrap().clone();
    assert!(
        needs.is_empty(),
        "the limit is no need of the person: {needs:?}"
    );
    std::thread::sleep(Duration::from_millis(3200));
    world.tick();
    assert!(world.flags().is_empty(), "{:?}", world.flags());
    assert_eq!(
        step_turns(&world, "refine").len(),
        1,
        "the same tick starts the turn"
    );
    world.settle();

    let other = World::new();
    simple_scenarios(&other);
    other.edit_config(|doc| doc["budget"]["reader"] = toml_edit::value(0));
    other.run(&["take", KEY]).expect_code(0);
    other.tick();
    other.tick();
    assert!(other.flags().iter().any(|f| f == "needs-you"));
    assert!(other
        .reason("needs-you")
        .contains("refused by budget.reader"));
}

#[test]
fn a_crash_between_the_documents_and_the_operations_is_repaired_by_the_next_tick() {
    let world = World::new();
    simple_scenarios(&world);
    world.run(&["take", KEY]).expect_code(0);
    let until = Instant::now() + LONG;
    loop {
        let run = world.run_with(&["tick"], &[("VISION_TEST_CRASH", "documents-written")]);
        if run.code == vision::pipeline::CRASH_CODE {
            break;
        }
        run.expect_code(0);
        assert!(
            Instant::now() < until,
            "the crash point never came:\n{}",
            world.log_text()
        );
        std::thread::sleep(Duration::from_millis(150));
    }
    assert!(world.doc("refine.md").is_some());
    assert_eq!(
        world.doc("description.md").as_deref(),
        Some("Add subtract(a, b).")
    );
    let turns = step_turns(&world, "refine");
    assert_eq!(turns.len(), 1);
    assert_eq!(
        turns[0].end_fact, None,
        "the crash came before the end facts"
    );
    assert!(world.ledger().operations_of(KEY, None).unwrap().is_empty());
    // A hand edit after the crash stays: the documents of a turn are written once.
    let path = world.vision.join("tickets").join(KEY).join("refine.md");
    std::fs::write(&path, "Research, edited.").unwrap();

    world.tick_to("plan");
    assert_eq!(world.doc("refine.md").as_deref(), Some("Research, edited."));
    assert_eq!(step_turns(&world, "refine").len(), 1, "no second turn");
    assert_eq!(world.acts_of("tracker.describe"), 1, "no second operation");
    assert_eq!(world.ledger().operations_of(KEY, None).unwrap().len(), 1);
    assert_eq!(
        world.world_json("tracker.json")["tickets"][0]["description"],
        "Add subtract(a, b)."
    );
}

#[test]
fn a_turn_that_asks_for_facts_gets_the_read_verbs_and_a_refusal_for_each_other_verb() {
    let world = World::new();
    simple_scenarios(&world);
    world.scenario(
        "scout",
        "refine",
        json!([
            ends_with(
                json!({"kind": "requests", "summary": "I need the other ticket.", "requests": [
                    {"verb": "tracker.issue", "args": {"key": "DEMO-13"}},
                    {"verb": "git.push", "args": {}}
                ]})
            ),
            ends_with(result(
                "Refined.",
                &[("refine.md", "R."), ("description.md", "D.")]
            ))
        ]),
    );
    world.run(&["take", KEY]).expect_code(0);
    world.tick_to("plan");
    assert!(world
        .doc("issue-DEMO-13.json")
        .unwrap()
        .contains("Add multiply"));
    let turns = step_turns(&world, "refine");
    assert_eq!(turns.len(), 2);
    assert_eq!(turns[1].resumes, turns[0].session);
    let facts = facts_of(&world, &turns[1].id);
    let message = facts["message"].as_str().unwrap();
    assert!(
        message.contains("- tracker.issue: .vision/issue-DEMO-13.json"),
        "{message}"
    );
    assert!(
        message.contains("- git.push: refused. It is not a read verb"),
        "{message}"
    );
    assert_eq!(
        facts["documents"]["issue-DEMO-13.json"],
        ".vision/issue-DEMO-13.json"
    );
    assert!(world.clone().join(".vision/issue-DEMO-13.json").is_file());
    assert_eq!(world.acts_of("git.push"), 0);
    // Each turn knows the facts that it can ask for: the read verbs, with their `about`, and
    // the two verbs of git that the core serves.
    for turn in &turns {
        let requests = facts_of(&world, &turn.id)["requests"].clone();
        assert_eq!(requests.as_array().unwrap().len(), 3, "{requests}");
        assert_eq!(requests[0]["verb"], "tracker.issue");
        assert_eq!(requests[1]["verb"], "git.log");
        assert_eq!(requests[2]["verb"], "git.blame");
        assert!(
            requests[0]["about"].as_str().unwrap().contains("args: key"),
            "{requests}"
        );
    }
}

#[test]
fn retry_after_vision_open_resumes_the_session_that_the_person_worked_in() {
    let world = World::new();
    simple_scenarios(&world);
    let bad = result("Planned nothing.", &[]);
    world.scenario(
        "builder",
        "plan",
        json!([
            ends_with(bad.clone()),
            ends_with(bad),
            ends_with(result("Planned.", &[("plan.md", PLAN)]))
        ]),
    );
    world.run(&["take", KEY]).expect_code(0);
    world.tick_until("the flag needs-you", LONG, |w| has_flag(w, "needs-you"));
    assert_eq!(world.step().as_deref(), Some("plan"));
    let open = world.run(&["open", KEY]);
    open.expect_code(0);
    assert!(
        open.stdout.contains("fake-claude interactive resume="),
        "{open:?}"
    );
    assert!(has_flag(&world, "paused"));
    let view = ticket_status(&world, KEY);
    let commands: Vec<&str> = view["flags"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| f["command"].as_str().unwrap())
        .collect();
    assert_eq!(commands, ["vision retry DEMO-12", "vision retry DEMO-12"]);

    world.run(&["retry", KEY]).expect_code(0);
    assert!(world.flags().is_empty());
    // A plain retry runs no rule, also with the [[resume]] rules of the demo adapter.
    let kinds = world.event_kinds();
    assert_eq!(kinds.last().map(String::as_str), Some("retry"), "{kinds:?}");
    assert!(!kinds.iter().any(|k| k == "resumed"), "{kinds:?}");
    world.tick_to("plan-ready");
    let turns = step_turns(&world, "plan");
    assert_eq!(turns.len(), 3);
    assert_eq!(
        turns[2].resumes, turns[1].session,
        "the session of the person"
    );
    assert_eq!(
        facts_of(&world, &turns[2].id)["message"],
        vision::pipeline::BY_HAND
    );
}

#[test]
fn retry_to_a_step_refuses_a_step_it_cannot_enter_and_each_case_that_a_plain_retry_refuses() {
    let world = World::new();
    world.scenario(
        "scout",
        "refine",
        json!([
            [{"do": "init"}, {"do": "sleep", "seconds": 60}, {"do": "result", "structured_output": {"kind": "result", "summary": "late"}}],
            ends_with(result("Refined.", &[("refine.md", "R."), ("description.md", "D.")]))
        ]),
    );
    world.run(&["take", KEY]).expect_code(0);

    let unknown = world.run(&["retry", KEY, "--step", "nowhere"]);
    unknown.expect_code(1);
    assert!(
        unknown.stderr.contains(
            "the adapter demo has no step `nowhere`. Move the ticket to one of its steps: refine, plan, plan-ready, build, review, open-pr, ci, fix-ci, ready, comments, ci-review, ready-to-merge"
        ),
        "{unknown:?}"
    );
    assert!(
        !unknown.stderr.contains("ready-to-merge, done"),
        "the end step is no target: {unknown:?}"
    );
    let end = world.run(&["retry", KEY, "--step", "done"]);
    end.expect_code(1);
    assert!(
        end.stderr.contains(
            "`done` is the end step of the adapter demo, and only the pipeline moves a ticket there. To stop the ticket, run `vision stop DEMO-12`"
        ),
        "{end:?}"
    );

    // A turn that runs: the refusal names it.
    world.tick_until("the running turn", LONG, |w| {
        w.ledger().running_turns_of_ticket(KEY).unwrap() > 0
            && w.scenario_runs("scout", "refine") >= 1
    });
    let turn = step_turns(&world, "refine")[0].id.clone();
    let busy = world.run(&["retry", KEY, "--step", "plan"]);
    busy.expect_code(1);
    assert!(
        busy.stderr.contains(&format!(
            "the turn {turn} of DEMO-12 runs now. Wait for it, or run `vision stop DEMO-12`"
        )),
        "{busy:?}"
    );

    // A stopped ticket keeps the rule of a plain retry: `vision take` starts it again first.
    world.run(&["stop", KEY]).expect_code(0);
    for step in ["plan", "nowhere"] {
        let stopped = world.run(&["retry", KEY, "--step", step]);
        stopped.expect_code(1);
        assert!(
            stopped
                .stderr
                .contains("DEMO-12 is stopped. Run `vision take DEMO-12` to start it again"),
            "{stopped:?}"
        );
    }

    // An uncertain operation: the reconcile step of the tick settles it first.
    world.run(&["take", KEY]).expect_code(0);
    world.tick_to("plan");
    let ops = world
        .ledger()
        .operations_of(KEY, Some("tracker.describe"))
        .unwrap();
    assert_eq!(ops.len(), 1);
    world
        .ledger()
        .conn()
        .execute(
            "UPDATE operations SET state = 'uncertain' WHERE id = ?1",
            [&ops[0].id],
        )
        .unwrap();
    let uncertain = world.run(&["retry", KEY, "--step", "build"]);
    uncertain.expect_code(1);
    assert!(uncertain.stderr.contains(&ops[0].id), "{uncertain:?}");
    assert!(
        uncertain
            .stderr
            .contains("the reconcile step of the tick must settle it first"),
        "{uncertain:?}"
    );

    // No refusal moved the ticket or wrote a row of a move.
    assert_eq!(world.step().as_deref(), Some("plan"));
    assert!(!world.event_kinds().iter().any(|k| k == "move"));
    assert!(world
        .ledger()
        .human_of(Some(KEY))
        .unwrap()
        .iter()
        .all(|h| h.kind != "move"));
}

#[test]
fn a_move_clears_the_flags_of_a_retry_and_is_no_entry_for_the_cap_of_its_step() {
    let world = World::new();
    // refine and plan count their entries, and the cap review_rounds = 1 allows one each. With
    // no [[resume]] table, the move of a paused ticket runs no rule, as before the table.
    world.write_adapter(|text| {
        let refine = "next = \"plan\"\n";
        let plan = "next = \"plan-ready\"\n";
        assert!(text.contains(refine) && text.contains(plan));
        without_resume(
            text.replacen(refine, &format!("cap = \"review_rounds\"\n{refine}"), 1)
                .replacen(plan, &format!("cap = \"review_rounds\"\n{plan}"), 1),
        )
    });
    world.edit_config(|doc| doc["caps"]["review_rounds"] = toml_edit::value(1));
    simple_scenarios(&world);
    world.scenario(
        "builder",
        "plan",
        json!([ends_with(result("Planned nothing.", &[]))]),
    );
    world.run(&["take", KEY]).expect_code(0);
    world.tick_until("the flag needs-you", LONG, |w| has_flag(w, "needs-you"));
    assert_eq!(world.step().as_deref(), Some("plan"));
    assert_eq!(step_turns(&world, "plan").len(), 2);
    let reason = world.reason("needs-you");
    // The person also took over the session: a retry clears both flags.
    world.run(&["open", KEY]).expect_code(0);
    assert!(has_flag(&world, "paused"));
    let before = world.counts();
    assert_eq!(before["entries"]["refine"], 1);

    // Back to refine. As an entry of the pipeline, it is the second one, above the cap.
    let moved = world.run(&["retry", KEY, "--step", "refine"]);
    moved.expect_code(0);
    assert!(
        moved.stdout.contains(
            "Moved DEMO-12 from the step plan to the step refine (cleared: needs-you, paused). The move does not count for the cap of refine. The next tick enters the step refine."
        ),
        "{moved:?}"
    );
    assert!(world.flags().is_empty());
    let counts = world.counts();
    assert_eq!(
        counts["reasons"],
        json!({}),
        "the reasons go with the flags"
    );
    assert_eq!(world.step().as_deref(), Some("refine"));
    assert_eq!(counts["entries"]["refine"], 1, "the move is no entry");
    let visit = &counts["visit"];
    assert_eq!(visit["step"], "refine");
    assert_eq!(visit["how"], "move");
    assert_eq!(visit["skip_cap"], true);
    assert_eq!(
        visit["turns"], 1,
        "the turn of the first visit is not one of this visit"
    );
    assert_eq!(visit["resume"], Value::Null, "no session of plan resumes");
    assert!(visit["seq"].as_u64().unwrap() > before["visit"]["seq"].as_u64().unwrap());

    // The evidence: the event, the row of the person, the spans, and the log.
    let human = world.ledger().human_of(Some(KEY)).unwrap();
    let row = human.last().unwrap();
    assert_eq!(
        (row.kind.as_str(), row.object.as_str(), row.value.as_deref()),
        ("move", KEY, Some("refine"))
    );
    assert_eq!(row.reason.as_deref(), Some(reason.as_str()));
    assert!(row.seconds.is_some());
    let event = world
        .ledger()
        .events_of_ticket(KEY)
        .unwrap()
        .into_iter()
        .rfind(|e| e.kind == "move")
        .expect("the event of the move");
    assert_eq!(event.actor, "user");
    let detail: Value = serde_json::from_str(event.detail.as_deref().unwrap()).unwrap();
    assert_eq!(detail["from"], "plan");
    assert_eq!(detail["to"], "refine");
    assert_eq!(detail["cleared"], json!(["needs-you", "paused"]));
    let spans = world.ledger().spans_of(Some(KEY)).unwrap();
    let states: Vec<(&str, Option<&str>, bool)> = spans
        .iter()
        .map(|s| (s.state.as_str(), s.reason.as_deref(), s.ended.is_some()))
        .collect();
    assert_eq!(
        states,
        [
            ("refine", Some("take"), true),
            ("plan", Some("next"), true),
            ("refine", Some("move"), false)
        ]
    );
    let log = world.run(&["log", KEY]);
    log.expect_code(0);
    assert!(
        log.stdout
            .contains("user moved the ticket from plan to refine"),
        "{log:?}"
    );

    // The next tick starts a fresh turn of refine: the cap does not stop it.
    world.tick_until("the second turn of refine", LONG, |w| {
        step_turns(w, "refine").len() == 2
    });
    let turns = step_turns(&world, "refine");
    assert_eq!(turns[1].resumes, None);
    assert!(facts_of(&world, &turns[1].id)["message"]
        .as_str()
        .unwrap()
        .contains("The person moved the ticket to this step from the step plan"));
    // The move of the pipeline to plan counts: its second entry is above the cap.
    world.tick_until("the cap of plan", LONG, |w| has_flag(w, "needs-you"));
    assert_eq!(world.step().as_deref(), Some("plan"));
    assert!(
        world
            .reason("needs-you")
            .contains("the step plan reached its cap review_rounds = 1: this is entry 2"),
        "{}",
        world.reason("needs-you")
    );
    assert_eq!(step_turns(&world, "plan").len(), 2, "no turn above the cap");
}

#[test]
fn pick_log_diff_and_report_show_the_ticket() {
    let world = World::new();
    simple_scenarios(&world);
    world.world_cmd(&["status", world.root(), "DEMO-13", "In Progress"]);
    let pick = world.run(&["pick"]);
    pick.expect_code(0);
    assert!(pick.stdout.contains("DEMO-12  To Do"), "{pick:?}");
    assert!(pick.stdout.contains("yes"), "{pick:?}");
    assert!(
        pick.stdout
            .contains("no: the status is In Progress, not To Do"),
        "{pick:?}"
    );
    world.run(&["take", KEY]).expect_code(0);
    assert!(world.run(&["pick"]).stdout.contains("taken (refine)"));
    world.tick_to("plan-ready");

    let log = world.run(&["log", KEY]);
    log.expect_code(0);
    assert!(log.stdout.contains("user take"), "{log:?}");
    assert!(
        log.stdout.contains("turn ") && log.stdout.contains("end fact held"),
        "{log:?}"
    );
    assert!(log.stdout.contains("operation tracker.describe"), "{log:?}");
    let entries = world.run(&["log", KEY, "--json"]).json();
    let ats: Vec<&str> = entries
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["at"].as_str().unwrap())
        .collect();
    let mut sorted = ats.clone();
    sorted.sort();
    assert_eq!(ats, sorted, "oldest first");

    let none = world.run(&["diff", KEY]);
    none.expect_code(0);
    assert!(
        none.stdout.contains("The forge has each commit"),
        "{none:?}"
    );
    let commit = world.commit_in(world.clone(), &[("notes.txt", "a note")], "Add a note");
    let diff = world.run(&["diff", KEY, "--json"]);
    diff.expect_code(0);
    let view = diff.json();
    assert_eq!(view["commits"][0]["commit"], commit.as_str());
    assert_eq!(view["commits"][0]["subject"], "Add a note");
    assert_eq!(view["compared_with"], "origin/main");
    let text = world.run(&["diff", KEY]);
    assert!(
        text.stdout.contains("1 commit(s) of vision/DEMO-12"),
        "{text:?}"
    );

    let report = world.run(&["report", "--json"]).json();
    let measures = &report["tickets"][KEY];
    assert!(
        measures["step_cost_usd"]["refine"].as_f64().unwrap() > 0.0,
        "{report}"
    );
    assert!(
        measures["step_cost_usd"]["plan"].as_f64().unwrap() > 0.0,
        "{report}"
    );
    assert!(measures["step_seconds"]["plan-ready"].is_i64(), "{report}");
    let week = world.run(&["report", "--week", "--json"]).json();
    assert_eq!(week["tickets"][KEY]["cost_usd"], measures["cost_usd"]);
    let text = world.run(&["report"]);
    text.expect_code(0);
    assert!(
        text.stdout.contains("DEMO-12") && text.stdout.contains("total"),
        "{text:?}"
    );
    assert!(text.stdout.contains("by step:"), "{text:?}");
    let status = world.run(&["status"]);
    assert!(
        status
            .stdout
            .contains("waits for you: vision approve-plan DEMO-12")
            || status.stdout.contains("needs you: 1 plan"),
        "{status:?}"
    );
}

/// A private adapter with the scripts of the demo whose entry is one wait step on the draft
/// flag of the pull request, which does not exist: the watched value is `none`.
fn wait_adapter(world: &World, keys: &str) {
    world.write_adapter(|text| {
        let scripts = text
            .lines()
            .find(|l| l.starts_with("scripts = "))
            .unwrap()
            .to_string();
        format!(
            "name = \"demo\"\nbase = \"origin/main\"\n{scripts}\n\n[[step]]\nid = \"look\"\nkind = \"wait\"\nwatch = \"pr.draft\"\non = {{ \"false\" = \"done\" }}\nevery = 1\n{keys}\n\n[[step]]\nid = \"done\"\nkind = \"end\"\n"
        )
    });
}

#[test]
fn a_wait_step_flags_a_value_that_nothing_names_and_retry_looks_again() {
    let world = World::new();
    wait_adapter(&world, "wait = [\"true\"]");
    world.run(&["take", KEY]).expect_code(0);
    world.tick_until("the flag needs-you", LONG, |w| has_flag(w, "needs-you"));
    let reason = world.reason("needs-you");
    assert!(
        reason.contains(
            "the step look watches pr.draft, and its value `none` is in neither on nor wait"
        ),
        "a missing path is the value none: {reason}"
    );
    let first = world.counts()["visit"]["seq"].as_u64().unwrap();
    world.run(&["retry", KEY]).expect_code(0);
    assert!(world.flags().is_empty());
    let visit = world.counts()["visit"].clone();
    assert!(
        visit["seq"].as_u64().unwrap() > first,
        "retry enters the step again: {visit}"
    );
    assert_eq!(visit["since"], Value::Null, "a new visit looks again");
    world.tick_until("the flag again", LONG, |w| has_flag(w, "needs-you"));
    assert_eq!(world.step().as_deref(), Some("look"));
}

#[test]
fn a_wait_step_above_its_limit_needs_you_and_retry_starts_a_new_limit() {
    let world = World::new();
    wait_adapter(&world, "wait = [\"none\", \"true\"]\nlimit = 2");
    world.run(&["take", KEY]).expect_code(0);
    world.tick();
    let since = world.counts()["visit"]["since"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(world.flags().is_empty(), "the first look starts the clock");
    world.tick_until("the flag needs-you", LONG, |w| has_flag(w, "needs-you"));
    let reason = world.reason("needs-you");
    assert!(
        reason.contains("the step look watched pr.draft for")
            && reason.contains("above its limit of 2 s. The last value: none"),
        "{reason}"
    );
    world.run(&["retry", KEY]).expect_code(0);
    // The first tick of the new visit looks, and its clock starts at that look.
    world.tick();
    assert!(
        world.flags().is_empty(),
        "a new limit: {}",
        world.reason("needs-you")
    );
    let again = world.counts()["visit"]["since"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(again > since, "{again} > {since}");
    world.tick_until("the flag after the new limit", LONG, |w| {
        has_flag(w, "needs-you")
    });
}

#[test]
fn a_turn_step_moves_on_the_kept_findings_of_its_turn() {
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
id = "check"
kind = "turn"
agent = "tester"
turn = "reader"
reads = ["issue.json"]
expects = ["doc:qa.md"]
findings = "qa"
on = {{ findings = "fix", none = "done" }}
cap = "qa_fix"

[[step]]
id = "fix"
kind = "turn"
agent = "builder"
turn = "reader"
reads = ["qa.md"]
expects = ["doc:fix-notes.md"]
next = "check"

[[step]]
id = "done"
kind = "end"
"#
        )
    });
    let bug = "## Bugs\n\n### B1: subtract(2, 5) gives the wrong sign (major)\nSteps: call it.\n\n### B2: the color of the page (minor)\nRejected: style_only.\n";
    // A turn whose only finding is rejected holds no kept finding.
    let clean = "## Bugs\n\n### B2: the color of the page (minor)\nRejected: style_only.\n";
    world.scenario(
        "tester",
        "check",
        json!([
            ends_with(result("One bug.", &[("qa.md", bug)])),
            ends_with(result("No bug.", &[("qa.md", clean)]))
        ]),
    );
    world.scenario(
        "builder",
        "fix",
        json!([ends_with(result(
            "Fixed.",
            &[("fix-notes.md", "B1 is fixed.")]
        ))]),
    );
    world.run(&["take", KEY]).expect_code(0);
    world.tick_until("the end", LONG, |w| w.counts()["finished"].is_string());
    let spans: Vec<String> = world
        .ledger()
        .spans_of(Some(KEY))
        .unwrap()
        .into_iter()
        .map(|s| format!("{} ({})", s.state, s.reason.unwrap_or_default()))
        .collect();
    assert_eq!(
        spans,
        [
            "check (take)",
            "fix (on findings)",
            "check (next)",
            "done (on none)"
        ]
    );
    let findings = world.ledger().findings_of(KEY).unwrap();
    let rows: Vec<(i64, &str, &str)> = findings
        .iter()
        .map(|f| (f.round, f.source.as_str(), f.verdict.as_str()))
        .collect();
    assert_eq!(
        rows,
        [
            (1, "qa", "kept"),
            (1, "qa", "rejected"),
            (2, "qa", "rejected")
        ]
    );
    assert_eq!(
        world.counts()["entries"]["check"],
        2,
        "an entry by on counts for the cap"
    );
}

/// The runs of one job at one commit: its rows of the kind `job`.
fn job_runs(world: &World, job: &str, head: &str) -> Vec<String> {
    world
        .ledger()
        .checks_of(KEY, Some(head))
        .unwrap()
        .into_iter()
        .filter(|c| c.kind == "job" && c.source == format!("job:{job}"))
        .map(|c| c.result)
        .collect()
}

/// The demo adapter with a job step `check` between the approval of the plan and the build.
fn check_step_adapter(world: &World, job: &str) {
    world.write_adapter(|text| {
        let old = "action = \"approve-plan\"\nnext = \"build\"";
        assert!(text.contains(old));
        text.replacen(
            old,
            &format!(
                "action = \"approve-plan\"\nnext = \"check\"\n\n[[step]]\nid = \"check\"\nkind = \"job\"\njob = \"{job}\"\nnext = \"build\""
            ),
            1,
        )
    });
}

#[test]
fn a_job_step_moves_on_after_its_job_passes() {
    let world = World::new();
    check_step_adapter(&world, "second-review");
    simple_scenarios(&world);
    take_and_approve(&world);
    let base = git(world.clone(), &["rev-parse", "HEAD"]);
    world.tick_to("build");
    assert_eq!(job_runs(&world, "second-review", &base), ["pass"]);
    assert!(world.doc("second-review.md").is_some());
    let spans: Vec<String> = world
        .ledger()
        .spans_of(Some(KEY))
        .unwrap()
        .into_iter()
        .map(|s| format!("{} {}", s.state, s.waits_on.unwrap_or_default()))
        .collect();
    assert!(
        spans.contains(&"check job:second-review".to_string()),
        "{spans:?}"
    );
    assert!(world.flags().is_empty());
}

/// The run records of the jobs of the background, as `vision::executor::runs` writes them.
fn job_records(world: &World) -> Vec<vision::executor::runs::RunRecord> {
    vision::executor::runs::all(&world.ctx().paths)
}

/// Ticks until a run of the job has a runner that holds its lock. Gives the record.
fn tick_until_job_runs(world: &World, job: &str) -> vision::executor::runs::RunRecord {
    let paths = world.ctx().paths;
    let until = Instant::now() + LONG;
    loop {
        world.tick();
        if let Some(record) = job_records(world).into_iter().find(|r| {
            r.job == job && r.ran.is_some() && vision::executor::runs::runner_lives(&paths, &r.id)
        }) {
            return record;
        }
        assert!(
            Instant::now() < until,
            "the job {job} never ran:\n{}",
            world.log_text()
        );
        std::thread::sleep(Duration::from_millis(100));
    }
}

#[test]
fn a_pass_does_not_wait_for_a_slow_job_and_a_later_pass_collects_it() {
    let world = World::new();
    check_step_adapter(&world, "second-review");
    simple_scenarios(&world);
    take_and_approve(&world);
    let base = git(world.clone(), &["rev-parse", "HEAD"]);
    std::fs::write(world.world.join("job-delay"), "4").unwrap();
    let record = tick_until_job_runs(&world, "second-review");
    assert_eq!(record.ticket, KEY);
    assert_eq!(world.step().as_deref(), Some("check"));
    // While the job runs, a pass ends at once and stores nothing of the job.
    let started = Instant::now();
    world.tick();
    assert!(
        started.elapsed() < Duration::from_secs(3),
        "a pass waited {:?} for the job",
        started.elapsed()
    );
    assert!(job_runs(&world, "second-review", &base).is_empty());
    let status = world.run(&["status"]).stdout;
    assert!(status.contains("second-review"), "{status}");
    world.tick_to("build");
    assert_eq!(job_runs(&world, "second-review", &base), ["pass"]);
    assert!(job_records(&world).is_empty(), "the pass removed the run");
    let log = world.log_text();
    assert!(
        log.contains("job second-review of DEMO-12 at")
            && log.contains("it runs in the background as"),
        "{log}"
    );
}

#[test]
fn a_stop_ends_the_job_of_the_ticket_and_stores_no_result() {
    let world = World::new();
    check_step_adapter(&world, "second-review");
    simple_scenarios(&world);
    take_and_approve(&world);
    let base = git(world.clone(), &["rev-parse", "HEAD"]);
    std::fs::write(world.world.join("job-delay"), "60").unwrap();
    let record = tick_until_job_runs(&world, "second-review");
    let started = Instant::now();
    let stop = world.run(&["stop", KEY]);
    stop.expect_code(0);
    assert!(
        started.elapsed() < Duration::from_secs(10),
        "the stop waited {:?}",
        started.elapsed()
    );
    assert!(
        stop.stdout
            .contains(&format!("Stopped the job(s) {}", record.id)),
        "{stop:?}"
    );
    let paths = world.ctx().paths;
    vision::executor::runs::wait_stopped(&paths, KEY, Duration::from_secs(10));
    assert!(!vision::executor::runs::runner_lives(&paths, &record.id));
    world.tick();
    assert!(job_records(&world).is_empty(), "the pass removed the run");
    assert!(
        job_runs(&world, "second-review", &base).is_empty(),
        "a stopped job stores no result"
    );
    assert!(world
        .log_text()
        .contains("job second-review of DEMO-12: stopped, so it stores no result"));
    // The job runs again when the ticket goes on.
    std::fs::remove_file(world.world.join("job-delay")).unwrap();
    world.run(&["take", KEY]).expect_code(0);
    world.tick_to("build");
    assert_eq!(job_runs(&world, "second-review", &base), ["pass"]);
}

#[test]
fn a_job_whose_runner_dies_runs_once_more_and_then_fails() {
    let world = World::new();
    check_step_adapter(&world, "second-review");
    simple_scenarios(&world);
    take_and_approve(&world);
    let base = git(world.clone(), &["rev-parse", "HEAD"]);
    std::fs::write(world.world.join("job-delay"), "8").unwrap();
    let kill = |record: &vision::executor::runs::RunRecord| {
        let pid = record.pid.expect("the runner wrote its pid");
        let killed = std::process::Command::new("kill")
            .args(["-9", &pid.to_string()])
            .status()
            .unwrap();
        assert!(killed.success());
        let paths = world.ctx().paths;
        let until = Instant::now() + Duration::from_secs(10);
        while vision::executor::runs::runner_lives(&paths, &record.id) {
            assert!(Instant::now() < until, "the runner did not end");
            std::thread::sleep(Duration::from_millis(50));
        }
    };
    let first = tick_until_job_runs(&world, "second-review");
    kill(&first);
    world.tick();
    assert!(world
        .log_text()
        .contains("its runner ended without an answer, so it runs one more time"));
    assert!(job_runs(&world, "second-review", &base).is_empty());
    let second = tick_until_job_runs(&world, "second-review");
    assert_eq!(second.id, first.id, "the same run starts again");
    kill(&second);
    world.tick_until("the flag of the failed job", LONG, |w| {
        !w.flags().is_empty()
    });
    assert_eq!(job_runs(&world, "second-review", &base), ["error"]);
    assert_eq!(world.step().as_deref(), Some("check"));
}

#[test]
fn a_job_step_whose_job_fails_needs_you_and_does_not_move() {
    let world = World::new();
    // The proof with no plan and no changed test cannot pass.
    check_step_adapter(&world, "proof");
    simple_scenarios(&world);
    take_and_approve(&world);
    let base = git(world.clone(), &["rev-parse", "HEAD"]);
    world.tick_until("the flag needs-you", LONG, |w| has_flag(w, "needs-you"));
    assert_eq!(world.step().as_deref(), Some("check"));
    assert_eq!(job_runs(&world, "proof", &base), ["fail"]);
    let reason = world.reason("needs-you");
    assert!(
        reason.contains("the job of the step check did not pass")
            && reason.contains("see the documents of the job proof"),
        "{reason}"
    );
    assert!(world.doc("proof.txt").is_some(), "the job wrote its log");
    world.tick();
    assert_eq!(
        job_runs(&world, "proof", &base).len(),
        1,
        "a flag stops the job"
    );
    assert_eq!(world.turns_of("build"), 0);
}

/// The demo adapter whose build step first runs the job second-review, then the verb
/// tracker.describe.
fn before_verb_adapter(world: &World) {
    world.write_adapter(|text| {
        let old = "turn = \"worker\"\nreads = [\"approved-plan.md\", \"refine.md\", \"answers.md\"]";
        assert!(text.contains(old));
        text.replacen(
            old,
            "turn = \"worker\"\nbefore = [\"job:second-review\", { verb = \"tracker.describe\", args = { text = \"Work on this ticket started.\" } }]\nreads = [\"approved-plan.md\", \"refine.md\", \"answers.md\"]",
            1,
        )
    });
}

#[test]
fn a_before_verb_in_the_mode_auto_runs_before_the_turn() {
    let world = World::new();
    before_verb_adapter(&world);
    simple_scenarios(&world);
    take_and_approve(&world);
    let base = git(world.clone(), &["rev-parse", "HEAD"]);
    world.tick_to("review");
    assert_eq!(world.acts_of("tracker.describe"), 2, "refine and before");
    assert_eq!(
        world.world_json("tracker.json")["tickets"][0]["description"],
        "Work on this ticket started."
    );
    assert_eq!(job_runs(&world, "second-review", &base), ["pass"]);
    assert_eq!(world.turns_of("build"), 1);
}

#[test]
fn a_before_verb_in_the_mode_ask_waits_and_the_jobs_before_it_run_once() {
    let world = World::new();
    before_verb_adapter(&world);
    simple_scenarios(&world);
    take_and_approve(&world);
    world.edit_config(|doc| doc["modes"]["tracker.describe"] = toml_edit::value("ask"));
    let base = git(world.clone(), &["rev-parse", "HEAD"]);
    world.tick_until("the approval", LONG, |w| {
        status(w)["needs"]
            .as_array()
            .unwrap()
            .iter()
            .any(|n| n["kind"] == "operation")
    });
    assert_eq!(world.step().as_deref(), Some("build"));
    for _ in 0..3 {
        world.tick();
    }
    assert_eq!(
        job_runs(&world, "second-review", &base),
        ["pass"],
        "a before job runs one time for a visit while a later entry waits"
    );
    assert_eq!(world.turns_of("build"), 0, "no turn before the approval");
    assert!(
        world.flags().is_empty(),
        "the mode ask is a need, not a flag"
    );
    assert_eq!(
        ticket_status(&world, KEY)["now"],
        "before a turn of builder: waits for your approval of tracker.describe. Run `vision needs`"
    );
    let needs = status(&world)["needs"].as_array().unwrap().clone();
    let id = needs.iter().find(|n| n["kind"] == "operation").unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();
    let (verb, commit) = id.split_once(' ').unwrap();
    assert_eq!(verb, "tracker.describe");
    world.run(&["approve", KEY, verb, commit]).expect_code(0);
    world.tick_to("review");
    assert_eq!(world.acts_of("tracker.describe"), 2);
    assert_eq!(job_runs(&world, "second-review", &base), ["pass"]);
    assert_eq!(world.turns_of("build"), 1);
}

#[test]
fn a_held_ticket_that_the_queue_no_longer_lists_as_eligible_still_moves() {
    let world = World::new();
    simple_scenarios(&world);
    world.run(&["take", KEY]).expect_code(0);
    // A person moves the issue in the tracker: the queue says that it is not eligible now.
    world.world_cmd(&["status", world.root(), KEY, "In Progress"]);
    world.tick_to("plan");
    let pick = world.run(&["pick", "--json"]).json();
    let row = pick
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["key"] == KEY)
        .unwrap()
        .clone();
    assert_eq!(row["eligible"], false, "{row}");
    assert_eq!(row["reason"], "the status is In Progress, not To Do");
    assert_eq!(row["taken"], "plan");
    let text = world.run(&["pick"]);
    text.expect_code(0);
    assert!(text.stdout.contains("taken (plan)"), "{text:?}");
    let issue: Value = serde_json::from_str(&world.doc("issue.json").unwrap()).unwrap();
    assert_eq!(issue["tracker_status"], "In Progress");
    let again = world.run(&["take", KEY]);
    again.expect_code(1);
    assert!(again.stderr.contains("already holds DEMO-12"), "{again:?}");
}

/// The build of DEMO-12 with no TODO file, so that the first CI run is green.
const GREEN_BUILD: &str = "printf '\\n\\ndef subtract(a, b):\\n    \"\"\"Gives a minus b.\"\"\"\\n    return a - b\\n' >> calc.py && printf 'import unittest\\n\\nfrom calc import subtract\\n\\n\\nclass SubtractTest(unittest.TestCase):\\n    def test_subtract(self):\\n        self.assertEqual(subtract(5, 3), 2)\\n' > test_subtract.py && git add -A && git commit -q -m 'Add subtract'";

#[test]
fn a_commit_of_the_shepherd_goes_through_ci_again_before_the_merge() {
    let world = World::new();
    simple_scenarios(&world);
    world.edit_config(|doc| doc["modes"]["review.reply"] = toml_edit::value("auto"));
    world.scenario(
        "builder",
        "build",
        json!([runs_then(
            GREEN_BUILD,
            result(
                "Built.",
                &[("pr.md", "Add subtract\n\nThe calculator can subtract.")]
            )
        )]),
    );
    world.scenario(
        "reviewer",
        "review",
        json!([ends_with(result(
            "Reviewed.",
            &[("review.md", "# Review\n\n## Findings\n\nNone.\n")]
        ))]),
    );
    let verdicts = r#"[{"comment_id": 1, "verdict": "valid", "fix_commit": "HEAD", "reply": "Added a test for negative numbers."}]"#;
    world.scenario(
        "shepherd",
        "comments",
        json!([runs_then(
            "printf '\\n    def test_negative(self):\\n        self.assertEqual(subtract(-1, 1), -2)\\n' >> test_subtract.py && git commit -q -am 'Test negative numbers'",
            result("Fixed the comment.", &[("verdicts.json", verdicts)])
        )]),
    );
    take_and_approve(&world);
    world.tick_to("ready");
    let built = git(world.clone(), &["rev-parse", "HEAD"]);
    assert_eq!(world.acts_of("ci.run"), 1);
    world.world_cmd(&["ready", world.root(), "1"]);
    world.world_cmd(&[
        "comment",
        world.root(),
        "1",
        "Test negative numbers too",
        "--author",
        "someone",
    ]);
    world.tick_to("ready-to-merge");
    let fixed = git(world.clone(), &["rev-parse", "HEAD"]);
    assert_ne!(fixed, built, "the Shepherd made a commit");
    assert_eq!(
        world.acts_of("ci.run"),
        2,
        "the commit of the Shepherd got its own CI run"
    );
    let ci: Vec<(String, String)> = world
        .ledger()
        .checks_of(KEY, None)
        .unwrap()
        .into_iter()
        .filter(|c| c.kind == "ci")
        .map(|c| (c.head, c.result))
        .collect();
    assert_eq!(
        ci,
        [
            (built, "green".to_string()),
            (fixed.clone(), "green".to_string())
        ]
    );
    let steps: Vec<String> = world
        .ledger()
        .spans_of(Some(KEY))
        .unwrap()
        .into_iter()
        .map(|s| s.state)
        .collect();
    assert_eq!(
        steps[steps.len() - 3..],
        ["comments", "ci-review", "ready-to-merge"]
    );
    let remote = git(
        &world.world.join("remote.git"),
        &["rev-parse", "refs/heads/vision/DEMO-12"],
    );
    assert_eq!(remote, fixed, "the commit is on the forge");
}

/// A sample report of `tests/fixtures/gates`.
fn gate_fixture(name: &str) -> String {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/gates")
        .join(name);
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

#[test]
fn a_failed_gate_resumes_the_turn_and_a_qa_gate_with_low_issues_records_one_notice() {
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
id = "review"
kind = "turn"
agent = "reviewer"
turn = "reader"
reads = ["issue.json"]
expects = [{{ gate = "cr", doc = "cr.md", net_max = 4 }}]
next = "qa"

[[step]]
id = "qa"
kind = "turn"
agent = "tester"
turn = "reader"
reads = ["issue.json"]
expects = [{{ gate = "qa", doc = "qa.md", block = ["critical", "high", "medium"], notify = true }}]
next = "done"

[[step]]
id = "done"
kind = "end"
"#
        )
    });
    world.scenario(
        "reviewer",
        "review",
        json!([
            ends_with(result(
                "Reviewed.",
                &[("cr.md", &gate_fixture("cr-fails.md"))]
            )),
            ends_with(result(
                "Fixed and reviewed again.",
                &[("cr.md", &gate_fixture("cr-passes.md"))]
            ))
        ]),
    );
    world.scenario(
        "tester",
        "qa",
        json!([ends_with(result(
            "Tested.",
            &[("qa.md", &gate_fixture("qa-low.md"))]
        ))]),
    );
    world.run(&["take", KEY]).expect_code(0);
    world.tick_until("the end", LONG, |w| w.counts()["finished"].is_string());

    // The failed gate is a failed end fact: one resume with the failing findings.
    let review = step_turns(&world, "review");
    assert_eq!(review.len(), 2);
    assert_eq!(review[0].end_fact.as_deref(), Some("failed:gate:cr:cr.md"));
    assert_eq!(review[1].end_fact.as_deref(), Some("held"));
    assert_eq!(review[1].resumes, review[0].session);
    let message = facts_of(&world, &review[1].id)["message"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(
        message.contains("The end fact gate:cr:cr.md does not hold"),
        "{message}"
    );
    assert!(
        message.contains(
            "finding 1 `1. [MAJOR][BLOCKING] app/models/report_export.rb:42` with Net +11"
        ),
        "{message}"
    );
    assert!(message.contains("finding 3 "), "{message}");
    assert!(!message.contains("finding 2 "), "{message}");
    let facts = facts_of(&world, &review[0].id);
    assert_eq!(facts["expects"], json!(["gate:cr:cr.md"]));
    assert_eq!(facts["expected_documents"], json!(["cr.md"]));
    assert!(world.doc("cr.md").unwrap().contains("Net: -2 (undo)"));

    // The qa gate passed with two low issues: one notice, and no flag.
    assert!(world.flags().is_empty(), "{:?}", world.flags());
    let qa = step_turns(&world, "qa");
    assert_eq!(qa.len(), 1);
    let notices = world.ledger().notices_of(KEY).unwrap();
    assert_eq!(notices.len(), 1);
    let notice = &notices[0];
    assert_eq!(
        (
            notice.step.as_str(),
            notice.turn.as_str(),
            notice.doc.as_str()
        ),
        ("qa", qa[0].id.as_str(), "qa.md")
    );
    let issues: Vec<(&str, &str, &str)> = notice
        .issues
        .iter()
        .map(|i| (i.id.as_str(), i.severity.as_str(), i.title.as_str()))
        .collect();
    assert_eq!(
        issues,
        [
            ("ISSUE-001", "low", "The export button has a typo"),
            (
                "ISSUE-002",
                "low",
                "The hover color of the export button differs"
            )
        ]
    );
    assert!(world.event_kinds().contains(&"notice".to_string()));

    // More passes add no second notice.
    world.tick();
    world.tick();
    assert_eq!(world.ledger().notices_of(KEY).unwrap().len(), 1);

    // It waits in the snapshot, in `vision status`, and in `vision needs`.
    let name = notice.name();
    let needs: Vec<Value> = status(&world)["needs"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|n| n["kind"] == "notice")
        .cloned()
        .collect();
    assert_eq!(needs.len(), 1);
    assert_eq!(needs[0]["id"], name.as_str());
    let run = world.run(&["needs"]);
    run.expect_code(0);
    assert!(
        run.stdout.contains("1 notice waits until you read it"),
        "{run:?}"
    );
    assert!(
        run.stdout
            .contains("ISSUE-002 low: The hover color of the export button differs"),
        "{run:?}"
    );
    assert!(
        run.stdout
            .contains(&format!("run: vision needs --read {name}")),
        "{run:?}"
    );
    let json = world.run(&["needs", "--json"]).json();
    assert_eq!(json["notices"][0]["id"], name.as_str());
    assert_eq!(json["notices"][0]["issues"][0]["id"], "ISSUE-001");

    // Marking it read removes it, and a second mark changes nothing.
    let read = world.run(&["needs", "--read", &name]);
    read.expect_code(0);
    assert!(
        read.stdout
            .contains(&format!("Marked {name} of {KEY} as read")),
        "{read:?}"
    );
    assert!(world.ledger().unread_notices().unwrap().is_empty());
    let again = world.run(&["needs", "--read", &name]);
    again.expect_code(0);
    assert!(again.stdout.contains("Nothing changed"), "{again:?}");
    let run = world.run(&["needs"]);
    assert!(!run.stdout.contains(&name), "{run:?}");
    let missing = world.run(&["needs", "--read", "N-99"]);
    missing.expect_code(1);
    assert!(
        missing.stderr.contains("there is no notice N-99"),
        "{missing:?}"
    );
}

/// A private adapter with the scripts of the demo and these steps.
fn steps_adapter(world: &World, steps: &str) {
    world.write_adapter(|text| {
        let scripts = text
            .lines()
            .find(|l| l.starts_with("scripts = "))
            .unwrap()
            .to_string();
        format!("name = \"demo\"\nbase = \"origin/main\"\n{scripts}\n\n{steps}")
    });
}

#[test]
fn rounds_gives_a_visit_more_turns_and_then_needs_you() {
    let world = World::new();
    steps_adapter(
        &world,
        r#"[[step]]
id = "notes"
kind = "turn"
agent = "scout"
turn = "reader"
reads = ["issue.json"]
expects = ["doc:notes.md"]
rounds = 3
next = "done"

[[step]]
id = "done"
kind = "end"
"#,
    );
    let partial = result("Notes.", &[]);
    world.scenario(
        "scout",
        "notes",
        json!([
            ends_with(partial.clone()),
            ends_with(partial.clone()),
            ends_with(partial.clone()),
            ends_with(partial)
        ]),
    );
    world.run(&["take", KEY]).expect_code(0);
    world.tick_until("the flag needs-you", LONG, |w| has_flag(w, "needs-you"));
    let turns = step_turns(&world, "notes");
    assert_eq!(turns.len(), 3, "the first turn and two resumes");
    for turn in &turns {
        assert_eq!(turn.end_fact.as_deref(), Some("failed:doc:notes.md"));
    }
    assert_eq!(turns[1].resumes, turns[0].session);
    assert_eq!(turns[2].resumes, turns[1].session);
    let reason = world.reason("needs-you");
    assert!(
        reason.contains("the end fact doc:notes.md did not hold after 2 resumes (rounds = 3)"),
        "{reason}"
    );
    world.tick();
    assert_eq!(
        step_turns(&world, "notes").len(),
        3,
        "nothing starts after it"
    );
}

#[test]
fn a_failed_gate_with_a_fail_route_goes_to_its_step_with_the_report() {
    let world = World::new();
    steps_adapter(
        &world,
        r#"[[step]]
id = "review"
kind = "turn"
agent = "reviewer"
turn = "reader"
reads = ["issue.json"]
expects = [{ gate = "cr", doc = "cr.md" }]
rounds = 3
next = "qa"

[[step]]
id = "qa"
kind = "turn"
agent = "tester"
turn = "reader"
reads = ["issue.json"]
expects = [{ gate = "qa", doc = "qa.md" }]
fail = { qa = "fix-qa" }
cap = "qa_fix"
next = "done"

[[step]]
id = "fix-qa"
kind = "turn"
agent = "builder"
turn = "reader"
reads = ["qa.md"]
expects = ["doc:fix-notes.md"]
cap = "qa_fix"
next = "qa"

[[step]]
id = "done"
kind = "end"
"#,
    );
    // The gate cr has no route: rounds = 3 gives its visit two resumes.
    world.scenario(
        "reviewer",
        "review",
        json!([
            ends_with(result(
                "Reviewed.",
                &[("cr.md", &gate_fixture("cr-fails.md"))]
            )),
            ends_with(result("Again.", &[("cr.md", &gate_fixture("cr-fails.md"))])),
            ends_with(result(
                "Fixed.",
                &[("cr.md", &gate_fixture("cr-passes.md"))]
            ))
        ]),
    );
    world.scenario(
        "tester",
        "qa",
        json!([
            ends_with(result(
                "Tested.",
                &[("qa.md", &gate_fixture("qa-medium.md"))]
            )),
            ends_with(result(
                "Tested again.",
                &[("qa.md", &gate_fixture("qa-clean.md"))]
            ))
        ]),
    );
    world.scenario(
        "builder",
        "fix-qa",
        json!([ends_with(result(
            "Fixed.",
            &[("fix-notes.md", "ISSUE-003 is fixed.")]
        ))]),
    );
    world.run(&["take", KEY]).expect_code(0);
    world.tick_until("the step fix-qa", LONG, |w| {
        w.step().as_deref() == Some("fix-qa") || w.flags().iter().any(|f| f == "needs-you")
    });
    assert!(world.flags().is_empty(), "{}", world.reason("needs-you"));
    let review = step_turns(&world, "review");
    assert_eq!(review.len(), 3, "two resumes of the gate cr, then it holds");
    assert_eq!(review[1].resumes, review[0].session);
    assert_eq!(review[2].resumes, review[1].session);
    assert_eq!(review[2].end_fact.as_deref(), Some("held"));
    // The failed gate qa went at once to its step: no resume, and its report waits in the
    // ticket folder.
    let qa = step_turns(&world, "qa");
    assert_eq!(qa.len(), 1, "no resume of the gate with a route");
    assert_eq!(qa[0].end_fact.as_deref(), Some("failed:gate:qa:qa.md"));
    let report = world.doc("qa.md").expect("qa.md is in the ticket folder");
    assert!(
        report.contains("ISSUE-003: The export of a large report takes 12 seconds"),
        "{report}"
    );
    let detail = world
        .ledger()
        .event_detail(&format!("end-fact:{}", qa[0].id))
        .unwrap()
        .flatten()
        .unwrap();
    assert!(detail.contains("\"blocks\":true"), "{detail}");

    world.tick_until("the end", LONG, |w| w.counts()["finished"].is_string());
    let fix = step_turns(&world, "fix-qa");
    assert_eq!(fix.len(), 1);
    assert_eq!(
        facts_of(&world, &fix[0].id)["documents"]["qa.md"],
        ".vision/qa.md",
        "the fix step reads the report through its reads"
    );
    let spans: Vec<String> = world
        .ledger()
        .spans_of(Some(KEY))
        .unwrap()
        .into_iter()
        .map(|s| format!("{} ({})", s.state, s.reason.unwrap_or_default()))
        .collect();
    assert_eq!(
        spans,
        [
            "review (take)",
            "qa (next)",
            "fix-qa (fail gate:qa:qa.md)",
            "qa (next)",
            "done (next)"
        ],
        "a passing gate goes to next"
    );
    let qa = step_turns(&world, "qa");
    assert_eq!(qa.len(), 2);
    assert_eq!(qa[1].resumes, None, "a new visit starts a new session");
    assert_eq!(qa[1].end_fact.as_deref(), Some("held"));
    assert_eq!(
        world.counts()["entries"]["fix-qa"],
        1,
        "the move by the route counts as an entry for the cap"
    );
    assert_eq!(world.counts()["entries"]["qa"], 2);
}

#[test]
fn a_gate_that_cannot_read_its_report_resumes_even_with_a_route() {
    let world = World::new();
    steps_adapter(
        &world,
        r#"[[step]]
id = "qa"
kind = "turn"
agent = "tester"
turn = "reader"
reads = ["issue.json"]
expects = [{ gate = "qa", doc = "qa.md" }]
fail = { qa = "fix-qa" }
next = "done"

[[step]]
id = "fix-qa"
kind = "turn"
agent = "builder"
turn = "reader"
reads = ["qa.md"]
expects = ["doc:fix-notes.md"]
next = "qa"

[[step]]
id = "done"
kind = "end"
"#,
    );
    world.scenario(
        "tester",
        "qa",
        json!([
            ends_with(result("Tested.", &[("qa.md", "All fine, I think.")])),
            ends_with(result(
                "Tested.",
                &[("qa.md", &gate_fixture("qa-clean.md"))]
            ))
        ]),
    );
    world.run(&["take", KEY]).expect_code(0);
    world.tick_until("the end", LONG, |w| w.counts()["finished"].is_string());
    let qa = step_turns(&world, "qa");
    assert_eq!(
        qa.len(),
        2,
        "the report that the gate cannot read gets a resume"
    );
    assert_eq!(qa[1].resumes, qa[0].session);
    assert_eq!(world.ledger().spans_of(Some(KEY)).unwrap().len(), 2);
    assert_eq!(step_turns(&world, "fix-qa").len(), 0);
}

#[test]
fn a_wait_step_before_its_local_time_runs_nothing_and_says_when_it_starts() {
    let world = World::new();
    // One minute ago in the local time of the machine: its next time comes almost a day later.
    let from =
        vision::util::time::local_hm(vision::util::time::now() - ::time::Duration::minutes(1));
    steps_adapter(
        &world,
        &format!(
            r#"[[step]]
id = "night"
kind = "wait"
start = ["ci.run"]
watch = "pr.draft"
on = {{ "false" = "done" }}
wait = ["none", "true"]
limit = 2
every = 1
from = "{from}"

[[step]]
id = "done"
kind = "end"

[verbs."ci.run"]
mode = "auto"
"#
        ),
    );
    world.run(&["take", KEY]).expect_code(0);
    for _ in 0..3 {
        world.tick();
        std::thread::sleep(Duration::from_millis(1100));
    }
    assert!(world.flags().is_empty(), "{}", world.reason("needs-you"));
    let visit = world.counts()["visit"].clone();
    let opens = vision::util::time::parse_rfc3339(visit["opens"].as_str().unwrap()).unwrap();
    let ahead = vision::util::time::seconds_between(vision::util::time::now(), opens);
    assert!(
        ahead > 23 * 3600 && ahead <= 24 * 3600,
        "{ahead} s: the next {from} after the entry"
    );
    assert_eq!(vision::util::time::local_hm(opens), from);
    assert_eq!(visit["observed"], Value::Null, "no look before the time");
    assert_eq!(visit["since"], Value::Null, "the limit has not started");
    assert_eq!(world.acts_of("ci.run"), 0, "no start verb before the time");
    let view = ticket_status(&world, KEY);
    assert_eq!(view["now"], format!("waits until {from}"));
    // A retry enters the step again and keeps the time of the visit: it never waits for a
    // later day.
    world.run(&["retry", KEY]).expect_code(0);
    let again = world.counts()["visit"].clone();
    assert!(again["seq"].as_u64() > visit["seq"].as_u64(), "{again}");
    assert_eq!(again["opens"], visit["opens"]);
    world.tick();
    assert_eq!(world.acts_of("ci.run"), 0);
}

/// A report of a code review in the form of the gate cr: one numbered finding for each item,
/// each with its place, its tag, its title, and its Net.
fn cr_report(items: &[(&str, &str, &str, &str)]) -> String {
    let mut text = String::from("Reviewed the branch against main.\n\n");
    for (n, (severity, tag, place, title)) in items.iter().enumerate() {
        text.push_str(&format!(
            "{}. [{severity}][{tag}] {place}\n   {title}\n   Fix: change it.\n   Net: +6\n\n",
            n + 1
        ));
    }
    if items.is_empty() {
        text.push_str("No finding passed the bar.\n");
    }
    text
}

#[test]
fn extra_rounds_go_on_while_the_failing_findings_get_fewer_and_then_need_you() {
    let world = World::new();
    steps_adapter(
        &world,
        r#"[[step]]
id = "review"
kind = "turn"
agent = "reviewer"
turn = "reader"
reads = ["issue.json"]
expects = [{ gate = "cr", doc = "cr.md" }]
rounds = 2
extra_rounds = 2
next = "done"

[[step]]
id = "done"
kind = "end"
"#,
    );
    let three = cr_report(&[
        ("MAJOR", "BLOCKING", "app/a.rb:10", "One"),
        ("MAJOR", "BLOCKING", "app/b.rb:20", "Two"),
        ("MINOR", "BLOCKING", "app/c.rb:30", "Three"),
    ]);
    let two = cr_report(&[
        ("MAJOR", "BLOCKING", "app/a.rb:10", "One"),
        ("MAJOR", "BLOCKING", "app/b.rb:20", "Two"),
    ]);
    let one = cr_report(&[("MAJOR", "BLOCKING", "app/a.rb:11", "One")]);
    world.scenario(
        "reviewer",
        "review",
        json!([
            ends_with(result("Three.", &[("cr.md", &three)])),
            ends_with(result("Two.", &[("cr.md", &two)])),
            ends_with(result("One.", &[("cr.md", &one)])),
            ends_with(result("Still one.", &[("cr.md", &one)])),
        ]),
    );
    world.run(&["take", KEY]).expect_code(0);
    world.tick_until("the flag needs-you", LONG, |w| has_flag(w, "needs-you"));
    let turns = step_turns(&world, "review");
    assert_eq!(
        turns.len(),
        4,
        "rounds = 2, then two extra rounds while 3 > 2 > 1, then no more at 1 = 1"
    );
    let ledger = world.ledger();
    let events: Vec<String> = ledger
        .events_of_ticket(KEY)
        .unwrap()
        .into_iter()
        .filter(|e| e.kind == "extra-round")
        .filter_map(|e| e.detail)
        .collect();
    assert_eq!(events.len(), 2, "{events:?}");
    assert!(
        events[0].contains("extra round 1 of 2 (extra_rounds) after 2 failed turn(s): the gate cr failed on 2 item(s), fewer than the 3 of the turn before, and none is critical"),
        "{events:?}"
    );
    assert!(events[1].contains("extra round 2 of 2"), "{events:?}");
    let reason = world.reason("needs-you");
    assert!(
        reason.contains("did not hold after 3 resumes (rounds = 2)")
            && reason.contains("Each of the 2 extra round(s) of extra_rounds is used"),
        "{reason}"
    );
    let rounds = ledger.gate_rounds_of(KEY, "cr").unwrap();
    let failing: Vec<Option<i64>> = rounds.iter().map(|r| r.failing).collect();
    assert_eq!(failing, [Some(3), Some(2), Some(1), Some(1)]);
    assert!(rounds.iter().all(|r| !r.passed && r.head.is_some()));
    // Each finding of each round is a row with its place, and the rounds follow it: a.rb is one
    // finding at the lines 10 and 11.
    let cr: Vec<_> = ledger
        .findings_of(KEY)
        .unwrap()
        .into_iter()
        .filter(|f| f.source == "cr")
        .collect();
    assert_eq!(cr.len(), 3 + 2 + 1 + 1);
    let tracks = ledger.finding_tracks(KEY).unwrap();
    assert_eq!(tracks.len(), 3);
    let a = tracks.iter().find(|t| t.track.path == "app/a.rb").unwrap();
    assert_eq!((a.track.first_round, a.track.last_round), (1, 4));
    assert_eq!(a.track.line, Some(11));
    assert_eq!(a.track.came_back, 0, "it never was gone");
}

#[test]
fn a_finding_that_comes_back_two_times_after_its_fix_is_a_notice_and_a_journal_note() {
    let world = World::new();
    steps_adapter(
        &world,
        r#"[[step]]
id = "review"
kind = "turn"
agent = "reviewer"
turn = "reader"
reads = ["issue.json"]
expects = [{ gate = "cr", doc = "cr.md" }]
rounds = 6
next = "done"

[[step]]
id = "done"
kind = "end"
"#,
    );
    let x = cr_report(&[(
        "MAJOR",
        "BLOCKING",
        "app/models/export.rb:42",
        "A retry sends the report twice",
    )]);
    let y = cr_report(&[(
        "MINOR",
        "BLOCKING",
        "app/views/show.erb:3",
        "A label is long",
    )]);
    let clean = cr_report(&[]);
    world.scenario(
        "reviewer",
        "review",
        json!([
            ends_with(result("X.", &[("cr.md", &x)])),
            ends_with(result("Y.", &[("cr.md", &y)])),
            ends_with(result("X again.", &[("cr.md", &x)])),
            ends_with(result("Y again.", &[("cr.md", &y)])),
            ends_with(result("X a third time.", &[("cr.md", &x)])),
            ends_with(result("Clean.", &[("cr.md", &clean)])),
        ]),
    );
    world.run(&["take", KEY]).expect_code(0);
    world.tick_until("the end", LONG, |w| w.counts()["finished"].is_string());
    let turns = step_turns(&world, "review");
    assert_eq!(turns.len(), 6);
    let ledger = world.ledger();
    let notices = ledger.notices_of(KEY).unwrap();
    assert_eq!(notices.len(), 1, "{notices:?}");
    let notice = &notices[0];
    assert_eq!(
        (
            notice.gate.as_str(),
            notice.doc.as_str(),
            notice.turn.as_str()
        ),
        ("cr", "cr.md", turns[4].id.as_str()),
        "the fifth round brought it back the second time"
    );
    assert_eq!(
        notice.note.as_deref(),
        Some("this finding keeps coming back after its fix")
    );
    assert_eq!(notice.issues[0].id, "F1");
    assert_eq!(
        notice.issues[0].title,
        "1. [MAJOR][BLOCKING] app/models/export.rb:42: A retry sends the report twice"
    );
    assert!(world.flags().is_empty(), "a notice never stops a ticket");
    let needs = world.run(&["needs"]);
    assert!(
        needs
            .stdout
            .contains("1 finding of cr.md of the step review keeps coming back after the fix"),
        "{}",
        needs.stdout
    );
    // The entry of the fifth turn holds the note, so that a later turn finds it.
    let id = TurnId::parse(&turns[4].id).unwrap();
    let entry = std::fs::read_to_string(world.ctx().paths.journal_entry(&vision_key(), &id))
        .expect("the journal entry of the turn");
    assert!(
        entry.contains("- problem: The finding F1 of cr.md keeps coming back after its fix: 1. [MAJOR][BLOCKING] app/models/export.rb:42: A retry sends the report twice"),
        "{entry}"
    );
}

fn vision_key() -> TicketKey {
    TicketKey::parse(KEY).unwrap()
}

#[test]
fn the_resume_after_a_failed_job_names_its_documents() {
    let world = World::new();
    simple_scenarios(&world);
    // The build commits a change with no test, and the plan declares no proof green: the proof
    // fails, and the resume names its log.
    world.scenario(
        "builder",
        "build",
        json!([
            runs_then(
                "printf '# A comment.\\n' >> calc.py && git commit -q -am 'Comment calc.py'",
                result("Built.", &[("pr.md", "Comment calc.py")])
            ),
            ends_with(result("Built again.", &[("pr.md", "Comment calc.py")])),
        ]),
    );
    take_and_approve(&world);
    world.tick_until("the flag needs-you", LONG, |w| has_flag(w, "needs-you"));
    let build = step_turns(&world, "build");
    assert_eq!(build.len(), 2, "one resume");
    assert!(build[0]
        .end_fact
        .as_deref()
        .is_some_and(|f| f.starts_with("failed:job:proof")));
    let facts = facts_of(&world, &build[1].id);
    let message = facts["message"].as_str().unwrap().to_string();
    assert!(
        message.contains("the job proof did not pass at")
            && message.contains("Read its documents: .vision/proof.txt"),
        "{message}"
    );
    assert_eq!(facts["documents"]["proof.txt"], ".vision/proof.txt");
}

/// The `turn.json` of the first turn of a ticket at a step, and its folder.
fn turn_of(
    world: &World,
    ticket: &str,
    step: &str,
) -> Option<(vision::turn::TurnState, std::path::PathBuf)> {
    let row = world
        .ledger()
        .turns_of_ticket(ticket)
        .unwrap()
        .into_iter()
        .find(|t| t.step.as_deref() == Some(step))?;
    let turn = world.ctx().paths.turn(
        &TicketKey::parse(ticket).unwrap(),
        &TurnId::parse(&row.id).unwrap(),
    );
    let state = vision::turn::TurnState::read(&turn.status).unwrap()?;
    Some((state, turn.dir))
}

/// The first event of the fake with this subtype in the stream of a turn folder.
fn fake_event(dir: &std::path::Path, subtype: &str) -> Value {
    std::fs::read_to_string(dir.join("stream.jsonl"))
        .unwrap_or_default()
        .lines()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .find(|event| event["type"] == "system" && event["subtype"] == subtype)
        .unwrap_or(Value::Null)
}

#[test]
fn two_worktree_clones_take_one_ticket_each_and_both_turns_run_at_once() {
    let world = World::new();
    // Two worktrees of the clone of the demo world are the clones, each with its own value.
    let root = world.clone().parent().unwrap().to_path_buf();
    let slots = [root.join("slot-a"), root.join("slot-b")];
    for slot in &slots {
        git(
            world.clone(),
            &[
                "worktree",
                "add",
                "--quiet",
                "--detach",
                slot.to_str().unwrap(),
                "origin/main",
            ],
        );
    }
    let texts: Vec<String> = slots.iter().map(|s| s.display().to_string()).collect();
    world.edit_config(|doc| {
        let mut clones = toml_edit::Array::new();
        for (text, name) in texts.iter().zip(["a", "b"]) {
            let mut env = toml_edit::InlineTable::new();
            env.insert("WORKSPACE", name.into());
            let mut entry = toml_edit::InlineTable::new();
            entry.insert("path", text.as_str().into());
            entry.insert("env", toml_edit::Value::InlineTable(env));
            clones.push(toml_edit::Value::InlineTable(entry));
        }
        doc["clones"] = toml_edit::value(clones);
    });
    world.scenario(
        "scout",
        "refine",
        json!([[
            {"do": "init"},
            {"do": "args"},
            {"do": "env", "show": ["WORKSPACE"]},
            {"do": "sleep", "seconds": 4},
            {"do": "result", "structured_output": result("Refined.", &[("refine.md", "R."), ("description.md", "D.")])}
        ]]),
    );
    // The take uses the clones in their order: each ticket gets its own worktree.
    let first = world.run(&["take", KEY]);
    first.expect_code(0);
    assert!(
        first
            .stdout
            .contains(&format!("in the clone {} ", texts[0])),
        "{first:?}"
    );
    let second = world.run(&["take", "DEMO-13"]);
    second.expect_code(0);
    assert!(
        second
            .stdout
            .contains(&format!("in the clone {} ", texts[1])),
        "{second:?}"
    );
    for (slot, key) in slots.iter().zip([KEY, "DEMO-13"]) {
        assert_eq!(
            git(slot, &["rev-parse", "--abbrev-ref", "HEAD"]),
            format!("vision/{key}")
        );
    }
    // One tick starts both turns, and both sessions run at the same time.
    world.tick();
    let running = |ticket: &str| {
        turn_of(&world, ticket, "refine")
            .is_some_and(|(state, _)| state.state == vision::turn::Phase::Running)
    };
    let until = Instant::now() + Duration::from_secs(20);
    while !(running(KEY) && running("DEMO-13")) {
        assert!(
            Instant::now() < until,
            "the two turns never ran at the same time:\n{}",
            world.log_text()
        );
        std::thread::sleep(Duration::from_millis(50));
    }
    let refined = |w: &World| {
        [KEY, "DEMO-13"].iter().all(|ticket| {
            w.ledger()
                .ticket(ticket)
                .unwrap()
                .and_then(|r| r.state)
                .as_deref()
                != Some("refine")
        })
    };
    world.tick_until("both refine steps", LONG, refined);
    // Each session ran in its own worktree, with the value of its clone.
    for ((slot, key), name) in slots.iter().zip([KEY, "DEMO-13"]).zip(["a", "b"]) {
        let (state, dir) = turn_of(&world, key, "refine").unwrap();
        assert!(state.is_ended(), "{key}: {state:?}");
        let args = fake_event(&dir, "fake_args");
        let cwd = std::fs::canonicalize(args["cwd"].as_str().unwrap()).unwrap();
        assert_eq!(cwd, std::fs::canonicalize(slot).unwrap(), "{key}");
        assert_eq!(
            fake_event(&dir, "fake_env")["values"]["WORKSPACE"],
            name,
            "{key}"
        );
    }
}

#[test]
fn a_branch_that_another_worktree_holds_stops_the_take_with_the_name_of_that_worktree() {
    let world = World::new();
    // The person keeps the branch of DEMO-12 in a worktree of the clone.
    let person = world.clone().parent().unwrap().join("person");
    git(
        world.clone(),
        &[
            "worktree",
            "add",
            "--quiet",
            "-b",
            "vision/DEMO-12",
            person.to_str().unwrap(),
            "origin/main",
        ],
    );
    let named = |run: &pipeline_support::Run| {
        let text = &run.stderr;
        assert!(
            text.contains("holds the branch vision/DEMO-12, and git lets one worktree hold a branch at a time"),
            "{run:?}"
        );
        assert!(
            text.contains("person switch --detach`"),
            "the fix names the worktree: {run:?}"
        );
        assert!(
            text.contains("then run `vision take DEMO-12` again"),
            "{run:?}"
        );
        assert!(
            text.contains("is already used by worktree at"),
            "git names the worktree too: {run:?}"
        );
    };
    let refused = world.run(&["take", KEY]);
    refused.expect_code(1);
    named(&refused);
    assert!(world.row().is_none(), "nothing changed");
    git(&person, &["switch", "--quiet", "--detach"]);
    world.run(&["take", KEY]).expect_code(0);

    // A stopped ticket: its clone left the branch, and the worktree of the person holds it.
    world.run(&["stop", KEY]).expect_code(0);
    git(world.clone(), &["switch", "--quiet", "--detach"]);
    git(&person, &["switch", "--quiet", "vision/DEMO-12"]);
    let again = world.run(&["take", KEY]);
    again.expect_code(1);
    named(&again);
    assert!(
        !again.stderr.contains("is gone from the clone"),
        "{again:?}"
    );
    assert!(has_flag(&world, "stopped"));
    git(&person, &["switch", "--quiet", "--detach"]);
    let back = world.run(&["take", KEY]);
    back.expect_code(0);
    assert!(
        back.stdout.contains("starts again at the step refine"),
        "{back:?}"
    );
    assert_eq!(
        git(world.clone(), &["rev-parse", "--abbrev-ref", "HEAD"]),
        "vision/DEMO-12"
    );
}
