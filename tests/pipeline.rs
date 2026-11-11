//! The pipeline of section 11.3 through `vision tick` and the ticket commands, with a demo world,
//! clones, and the fake `claude`.

mod pipeline_support;

use std::time::{Duration, Instant};

use pipeline_support::{
    ends_with, git, plan_hash_of, result, runs_then, simple_scenarios, take_and_approve, World,
    KEY, PLAN,
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
fn take_with_no_free_clone_says_who_holds_each_clone_and_a_dirty_clone_is_not_free() {
    let world = World::new();
    world.run(&["take", KEY]).expect_code(0);
    let busy = world.run(&["take", "DEMO-13"]);
    busy.expect_code(1);
    assert!(busy.stderr.contains("no clone is free"), "{busy:?}");
    assert!(
        busy.stderr.contains(&format!(
            "{}: DEMO-12 holds it (step refine)",
            world.clone().display()
        )),
        "{busy:?}"
    );

    let dirty = World::new();
    std::fs::write(dirty.clone().join("scratch.txt"), "work in progress").unwrap();
    let run = dirty.run(&["take", KEY]);
    run.expect_code(1);
    assert!(
        run.stderr
            .contains("its work tree has 1 change(s), such as `?? scratch.txt`"),
        "{run:?}"
    );
    assert!(dirty.row().is_none());

    dirty.edit_config(|doc| doc["clones"] = toml_edit::value(toml_edit::Array::new()));
    let none = dirty.run(&["take", KEY]);
    none.expect_code(1);
    assert!(
        none.stderr
            .contains("config.toml lists no clone in `clones`"),
        "{none:?}"
    );
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
    take_and_approve(&world);
    world.tick_until("the refused push", LONG, |w| has_flag(w, "needs-you"));
    assert_eq!(world.step().as_deref(), Some("build"));
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
        text.replacen(
            "cap = \"review_rounds\"\nnext = \"ci\"",
            "cap = \"review_rounds\"\nnext = \"again\"\n\n[[step]]\nid = \"again\"\nkind = \"wait\"\nwatch = \"pr.draft\"\non = { \"true\" = \"review\" }\nevery = 1",
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

    // The freed clone takes a different ticket; then the stopped one cannot come back there.
    world.run(&["take", "DEMO-13"]).expect_code(0);
    let blocked = world.run(&["take", KEY]);
    blocked.expect_code(1);
    assert!(
        blocked.stderr.contains("but DEMO-13 holds that clone now"),
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
    // Each turn knows the facts that it can ask for: the read verbs, with their `about`.
    for turn in &turns {
        let requests = facts_of(&world, &turn.id)["requests"].clone();
        assert_eq!(requests.as_array().unwrap().len(), 1, "{requests}");
        assert_eq!(requests[0]["verb"], "tracker.issue");
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
