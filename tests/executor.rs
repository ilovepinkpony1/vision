//! The executor with the demo adapter: operations in each mode, idempotency, approval, the
//! gates, reconcile, the ladder, jobs, read verbs, and the needs of the person.

mod common;
mod executor_support;

use executor_support::{job, key, op, Demo, BRANCH, KEY};
use serde_json::{json, Value};
use vision::executor::{self, OpState, Outcome};

#[test]
fn git_push_and_pr_open_run_in_the_mode_auto() {
    let demo = Demo::new();
    let ctx = demo.ctx();
    let ledger = demo.ledger();
    let head = demo.commit_subtract();

    let push = executor::request(&ctx, &ledger, &op("git.push", json!({}))).unwrap();
    assert_eq!(push.outcome, Outcome::Confirmed, "{push:?}");
    assert_eq!(push.state, OpState::Confirmed);
    assert_eq!(push.head, head);
    assert_eq!(push.mode, "auto");
    assert_eq!(
        push.external_id.as_deref(),
        Some(format!("{BRANCH}@{}", &head[..12]).as_str())
    );
    assert_eq!(demo.remote_branch(BRANCH).as_deref(), Some(head.as_str()));
    assert!(push.ended.is_some() && push.started.is_some());

    let open = executor::request(
        &ctx,
        &ledger,
        &op(
            "pr.open",
            json!({"title": "Add subtract", "body": "With a test."}),
        ),
    )
    .unwrap();
    assert_eq!(open.outcome, Outcome::Confirmed, "{open:?}");
    assert_eq!(open.set.pr, Some(1));
    let prs = demo.world_json("prs.json");
    assert_eq!(prs["prs"][0]["number"], 1);
    assert_eq!(prs["prs"][0]["branch"], BRANCH);
    assert_eq!(prs["prs"][0]["title"], "Add subtract");
    assert_eq!(prs["prs"][0]["draft"], true);
    let row = demo.ticket_row();
    assert_eq!(row.pr, Some(1));
    assert_eq!(row.head.as_deref(), Some(head.as_str()));
    assert!(row.updated.is_some());
}

#[test]
fn the_same_operation_a_second_time_does_nothing_new() {
    let demo = Demo::new();
    let ctx = demo.ctx();
    let ledger = demo.ledger();
    demo.commit_subtract();
    let first = executor::request(&ctx, &ledger, &op("git.push", json!({}))).unwrap();
    let second = executor::request(&ctx, &ledger, &op("git.push", json!({}))).unwrap();
    assert_eq!(first.id, second.id);
    assert_eq!(second.outcome, Outcome::Confirmed);
    assert_eq!(demo.acts_of("git.push"), 1, "act ran once");
    let lookup = executor::lookup(&ctx, &ledger, &op("git.push", json!({})))
        .unwrap()
        .unwrap();
    assert_eq!(lookup.id, first.id);
    assert_eq!(executor::operations_of(&ledger, &key()).unwrap().len(), 1);

    // Different arguments or a new commit make a new key.
    let forced = executor::request(&ctx, &ledger, &op("git.push", json!({"force": true}))).unwrap();
    assert_ne!(forced.id, first.id);
    let head = demo.commit(&[("notes.txt", "more\n")], "More");
    assert!(executor::lookup(&ctx, &ledger, &op("git.push", json!({})))
        .unwrap()
        .is_none());
    let third = executor::request(&ctx, &ledger, &op("git.push", json!({}))).unwrap();
    assert_eq!(third.outcome, Outcome::Confirmed);
    assert_eq!(demo.remote_branch(BRANCH).as_deref(), Some(head.as_str()));
    assert_eq!(demo.acts_of("git.push"), 3);
}

#[test]
fn ask_waits_for_vision_approve_with_the_exact_command_of_needs() {
    let demo = Demo::new();
    let ctx = demo.ctx();
    let head = demo.commit_subtract();
    demo.set_mode("git.push", "ask");
    let ledger = demo.ledger();
    let waiting = executor::request(&ctx, &ledger, &op("git.push", json!({}))).unwrap();
    let command = format!("vision approve {KEY} git.push {head}");
    assert_eq!(
        waiting.outcome,
        Outcome::Waiting {
            command: command.clone()
        }
    );
    assert_eq!(waiting.state, OpState::Pending);
    assert!(waiting.outcome.needs_you());
    assert!(demo.remote_branch(BRANCH).is_none(), "nothing ran");
    assert_eq!(demo.acts_of("git.push"), 0);

    // `vision needs` prints the exact command, and that command runs the operation.
    let needs = demo.run(&["needs", "--json"]);
    needs.expect_code(0);
    let view: Value = serde_json::from_str(&needs.stdout).unwrap();
    let others = view["others"].as_array().unwrap();
    assert_eq!(others.len(), 1, "{view}");
    assert_eq!(others[0]["kind"], "operation");
    assert_eq!(others[0]["command"], command.as_str());
    let words: Vec<&str> = command.split_whitespace().skip(1).collect();
    let run = demo.run(&words);
    run.expect_code(0);
    assert!(run.stdout.contains("git.push of DEMO-12"), "{run:?}");
    assert!(run.stdout.contains("confirmed"), "{run:?}");
    assert_eq!(demo.remote_branch(BRANCH).as_deref(), Some(head.as_str()));
    let done = executor::operation(&ledger, &waiting.id).unwrap().unwrap();
    assert_eq!(done.outcome, Outcome::Confirmed);
    let human = ledger.human_rows(&waiting.id).unwrap();
    assert_eq!(human.len(), 1);
    assert_eq!(human[0].kind, "approve");
    assert_eq!(
        human[0].value.as_deref(),
        Some(format!("git.push {head}").as_str())
    );

    // Nothing waits now, and the same request gives the stored result.
    let again = demo.run(&words);
    again.expect_code(1);
    assert!(
        again
            .stderr
            .contains("nothing of git.push waits for your approval on DEMO-12"),
        "{again:?}"
    );
    let view: Value = serde_json::from_str(&demo.run(&["needs", "--json"]).stdout).unwrap();
    assert!(view["others"].as_array().unwrap().is_empty());
    assert_eq!(
        executor::request(&ctx, &ledger, &op("git.push", json!({})))
            .unwrap()
            .id,
        waiting.id
    );
    assert_eq!(demo.acts_of("git.push"), 1);
}

#[test]
fn a_stale_commit_is_an_error_that_tells_what_changed() {
    let demo = Demo::new();
    let ctx = demo.ctx();
    let old = demo.commit_subtract();
    demo.set_mode("git.push", "ask");
    let ledger = demo.ledger();
    let waiting = executor::request(&ctx, &ledger, &op("git.push", json!({}))).unwrap();
    let new = demo.commit(&[("notes.txt", "a note\n")], "Write a note");

    let wrong = demo.run(&["approve", KEY, "git.push", &new]);
    wrong.expect_code(1);
    assert!(
        wrong
            .stderr
            .contains(&format!("the commit {new} does not match")),
        "{wrong:?}"
    );
    assert!(wrong.stderr.contains(&old), "{wrong:?}");

    let stale = demo.run(&["approve", KEY, "git.push", &old[..7]]);
    stale.expect_code(1);
    assert!(stale.stderr.contains("the commit is stale"), "{stale:?}");
    assert!(
        stale
            .stderr
            .contains(&format!("was asked at {}", &old[..12])),
        "{stale:?}"
    );
    assert!(
        stale.stderr.contains(&format!("now at {}", &new[..12])),
        "{stale:?}"
    );
    assert!(
        stale.stderr.contains("1 new commit(s)") && stale.stderr.contains("Write a note"),
        "{stale:?}"
    );
    assert!(demo.remote_branch(BRANCH).is_none());
    let row = executor::operation(&ledger, &waiting.id).unwrap().unwrap();
    assert_eq!(row.outcome, Outcome::Failed);
    assert!(row.detail.starts_with("stale:"), "{row:?}");

    // The request at the new commit waits again, for the new commit.
    let next = executor::request(&ctx, &ledger, &op("git.push", json!({}))).unwrap();
    assert_eq!(
        next.outcome,
        Outcome::Waiting {
            command: format!("vision approve {KEY} git.push {new}")
        }
    );
}

#[test]
fn a_new_commit_ends_the_older_waiting_operation() {
    let demo = Demo::new();
    let ctx = demo.ctx();
    demo.commit_subtract();
    demo.set_mode("git.push", "ask");
    let ledger = demo.ledger();
    let first = executor::request(&ctx, &ledger, &op("git.push", json!({}))).unwrap();
    demo.commit(&[("notes.txt", "a note\n")], "Write a note");
    let second = executor::request(&ctx, &ledger, &op("git.push", json!({}))).unwrap();
    let first = executor::operation(&ledger, &first.id).unwrap().unwrap();
    assert_eq!(first.outcome, Outcome::Failed);
    assert!(
        first
            .detail
            .contains(&format!("The operation {} replaces it", second.id)),
        "{first:?}"
    );
    assert_eq!(executor::needs(&ctx, &ledger).unwrap().len(), 1);
}

#[test]
fn shadow_and_hold_call_nothing_and_a_promotion_lets_the_same_operation_run() {
    let demo = Demo::new();
    let ctx = demo.ctx();
    demo.commit_subtract();
    demo.set_mode("git.push", "shadow");
    let ledger = demo.ledger();
    let shadow = executor::request(&ctx, &ledger, &op("git.push", json!({}))).unwrap();
    assert_eq!(shadow.outcome, Outcome::Shadowed);
    assert!(shadow.outcome.needs_you());
    let intent = shadow.intent.clone().unwrap();
    assert_eq!(intent["verb"], "git.push");
    assert_eq!(intent["branch"], BRANCH);
    assert_eq!(intent["gates"], "the gates would pass");
    assert!(shadow.detail.contains("mode shadow"), "{shadow:?}");
    assert_eq!(demo.acts_of("git.push"), 0);
    assert!(demo.remote_branch(BRANCH).is_none());

    demo.set_mode("git.push", "hold");
    let ctx = demo.ctx();
    let held = executor::request(&ctx, &ledger, &op("git.push", json!({}))).unwrap();
    assert_eq!(held.id, shadow.id, "a pending operation keeps its row");
    assert_eq!(held.outcome, Outcome::Held);
    assert!(held.detail.contains("vision promote git.push"), "{held:?}");
    assert_eq!(demo.acts_of("git.push"), 0);
    assert!(
        executor::needs(&ctx, &ledger).unwrap().is_empty(),
        "hold and shadow do not wait for approve"
    );

    demo.set_mode("git.push", "auto");
    let ctx = demo.ctx();
    let ran = executor::request(&ctx, &ledger, &op("git.push", json!({}))).unwrap();
    assert_eq!(ran.id, shadow.id);
    assert_eq!(ran.outcome, Outcome::Confirmed);
    assert_eq!(demo.acts_of("git.push"), 1);
}

#[test]
fn approve_with_an_operation_id_runs_only_that_operation() {
    let demo = Demo::new();
    let ctx = demo.ctx();
    let head = demo.commit_subtract();
    demo.set_mode("git.push", "ask");
    let ledger = demo.ledger();
    // Two writes of one verb wait at one commit: other arguments are another operation.
    let plain = executor::request(&ctx, &ledger, &op("git.push", json!({}))).unwrap();
    let forced = executor::request(&ctx, &ledger, &op("git.push", json!({"force": true}))).unwrap();
    assert_ne!(plain.id, forced.id);
    assert!(matches!(plain.outcome, Outcome::Waiting { .. }));
    assert!(matches!(forced.outcome, Outcome::Waiting { .. }));

    // An id that does not wait sends nothing, also while other writes wait there.
    let unknown = demo.run(&["approve", KEY, "git.push", &head, "--op", "no-such-id"]);
    unknown.expect_code(1);
    assert!(
        unknown.stderr.contains("does not wait for your approval")
            && unknown.stderr.contains("nothing was sent")
            && unknown.stderr.contains("2 other operation(s)"),
        "{unknown:?}"
    );
    assert_eq!(demo.acts_of("git.push"), 0);

    // The id of one of them runs that one, and the other one still waits.
    let run = demo.run(&["approve", KEY, "git.push", &head, "--op", &plain.id]);
    run.expect_code(0);
    assert_eq!(demo.acts_of("git.push"), 1);
    let done = executor::operation(&ledger, &plain.id).unwrap().unwrap();
    assert_eq!(done.outcome, Outcome::Confirmed);
    let other = executor::operation(&ledger, &forced.id).unwrap().unwrap();
    assert!(
        matches!(other.outcome, Outcome::Waiting { .. }),
        "{other:?}"
    );
    assert_eq!(ledger.human_rows(&plain.id).unwrap().len(), 1);
    assert!(ledger.human_rows(&forced.id).unwrap().is_empty());

    // The same id a second time: it no longer waits, so nothing runs.
    let again = demo.run(&["approve", KEY, "git.push", &head, "--op", &plain.id]);
    again.expect_code(1);
    assert!(
        again.stderr.contains("1 other operation(s)") && again.stderr.contains("the write changed"),
        "{again:?}"
    );
    assert_eq!(demo.acts_of("git.push"), 1);
}

#[test]
fn approve_refuses_a_verb_in_the_mode_hold_or_shadow_and_unknown_verbs() {
    let demo = Demo::new();
    let head = demo.commit_subtract();
    demo.set_mode("git.push", "ask");
    let ledger = demo.ledger();
    executor::request(&demo.ctx(), &ledger, &op("git.push", json!({}))).unwrap();
    demo.set_mode("git.push", "shadow");
    let run = demo.run(&["approve", KEY, "git.push", &head]);
    run.expect_code(1);
    assert!(
        run.stderr.contains("git.push is in the mode shadow now"),
        "{run:?}"
    );
    let run = demo.run(&["approve", KEY, "git.force", &head]);
    run.expect_code(1);
    assert!(run.stderr.contains("has no verb `git.force`"), "{run:?}");
    let run = demo.run(&["approve", KEY, "tracker.issue", &head]);
    run.expect_code(1);
    assert!(run.stderr.contains("read verb"), "{run:?}");
    let err = executor::request(&demo.ctx(), &ledger, &op("tracker.issue", json!({}))).unwrap_err();
    assert!(err.to_string().contains("read verb"), "{err}");
    let err = executor::request(&demo.ctx(), &ledger, &op("git.push", json!([1]))).unwrap_err();
    assert!(
        format!("{err:#}").contains("must be a JSON object"),
        "{err:#}"
    );
}

#[test]
fn a_secret_in_the_diff_is_refused_and_never_stored() {
    let demo = Demo::new();
    let secret = "tok-4f9a2c71e0";
    demo.edit_config(|doc| {
        let mut source = toml_edit::InlineTable::new();
        source.insert("cmd", format!("printf '{secret}\\n'").into());
        doc["secrets"]["FORGE_TOKEN"] = toml_edit::value(source);
    });
    demo.commit_subtract();
    demo.commit(
        &[(
            "deploy.cfg",
            &format!("[forge]\nuser = demo\ntoken = {secret}\n"),
        )],
        "Add the deploy settings",
    );
    let ctx = demo.ctx();
    let ledger = demo.ledger();
    let result = executor::request(&ctx, &ledger, &op("git.push", json!({}))).unwrap();
    assert_eq!(
        result.outcome,
        Outcome::Refused {
            gate: "secrets".into()
        },
        "{result:?}"
    );
    assert!(
        result
            .detail
            .contains("the value of the secret FORGE_TOKEN in deploy.cfg line 3"),
        "{result:?}"
    );
    assert!(!result.detail.contains(secret));
    assert!(demo.remote_branch(BRANCH).is_none());
    assert_eq!(demo.acts_of("git.push"), 0);

    // A secret in the text of a pull request is refused before the call.
    let open = executor::request(
        &ctx,
        &ledger,
        &op(
            "pr.open",
            json!({"title": "x", "body": format!("use {secret}")}),
        ),
    )
    .unwrap();
    assert_eq!(
        open.outcome,
        Outcome::Refused {
            gate: "secrets".into()
        }
    );
    assert!(open.detail.contains("FORGE_TOKEN"), "{open:?}");
    assert!(
        !demo.ledger_text().contains(secret),
        "the ledger holds the secret"
    );
    assert!(
        !demo.log_text().contains(secret),
        "the log holds the secret"
    );
    assert!(demo.ledger_text().contains("[secret FORGE_TOKEN]"));
}

#[test]
fn a_private_key_in_the_diff_is_refused() {
    let demo = Demo::new();
    demo.commit(
        &[(
            "keys/deploy",
            &format!("{}{}\nabc\n", "-----BEGIN OPENSSH ", "PRIVATE KEY-----"),
        )],
        "Add a key",
    );
    let result =
        executor::request(&demo.ctx(), &demo.ledger(), &op("git.push", json!({}))).unwrap();
    assert_eq!(
        result.outcome,
        Outcome::Refused {
            gate: "secrets".into()
        }
    );
    assert!(
        result
            .detail
            .contains("a line that looks like a private key in keys/deploy line 1"),
        "{result:?}"
    );
}

#[test]
fn a_secret_that_a_later_commit_removed_is_still_refused() {
    let demo = Demo::new();
    let key_line = format!("{}{}", "-----BEGIN OPENSSH ", "PRIVATE KEY-----");
    let added = demo.commit(
        &[("keys/deploy", &format!("{key_line}\nabc\n"))],
        "Add a key",
    );
    demo.commit(&[("keys/deploy", "moved to the vault\n")], "Remove the key");
    // The diff from the base to the last commit no longer has the key, but the push sends both
    // commits.
    let result =
        executor::request(&demo.ctx(), &demo.ledger(), &op("git.push", json!({}))).unwrap();
    assert_eq!(
        result.outcome,
        Outcome::Refused {
            gate: "secrets".into()
        },
        "{result:?}"
    );
    assert!(
        result.detail.contains(&format!(
            "a line that looks like a private key in keys/deploy line 1 of the commit {}",
            &added[..12]
        )),
        "{result:?}"
    );
    assert!(demo.remote_branch(BRANCH).is_none());
    assert_eq!(demo.acts_of("git.push"), 0);
}

#[test]
fn an_attribute_of_the_repo_cannot_hide_a_file_from_the_gates() {
    let demo = Demo::new();
    let key_line = format!("{}{}", "-----BEGIN OPENSSH ", "PRIVATE KEY-----");
    // `-diff` makes git show the file as binary, with no lines.
    demo.commit(
        &[
            (".gitattributes", "keys/* -diff\n"),
            ("keys/deploy", &format!("{key_line}\nabc\n")),
        ],
        "Add a key that the attributes hide",
    );
    let result =
        executor::request(&demo.ctx(), &demo.ledger(), &op("git.push", json!({}))).unwrap();
    assert_eq!(
        result.outcome,
        Outcome::Refused {
            gate: "secrets".into()
        },
        "{result:?}"
    );
    assert!(
        result
            .detail
            .contains("a line that looks like a private key in keys/deploy line 1"),
        "{result:?}"
    );
    assert_eq!(demo.acts_of("git.push"), 0);
}

#[test]
fn a_diff_that_is_too_large_is_refused() {
    let demo = Demo::new();
    demo.edit_config(|doc| doc["gates"]["max_diff_lines"] = toml_edit::value(5));
    let lines: String = (1..=12).map(|n| format!("line {n}\n")).collect();
    demo.commit(&[("big.txt", &lines)], "A big file");
    let result =
        executor::request(&demo.ctx(), &demo.ledger(), &op("git.push", json!({}))).unwrap();
    assert_eq!(
        result.outcome,
        Outcome::Refused {
            gate: "size".into()
        }
    );
    assert!(
        result
            .detail
            .contains("the diff changes 12 lines, more than gates.max_diff_lines (5)"),
        "{result:?}"
    );
    assert!(demo.remote_branch(BRANCH).is_none());
    let run = demo.run(&["status", "--json"]);
    run.expect_code(0);
}

#[test]
fn a_sensitive_path_is_held_unless_the_approved_plan_names_it() {
    let demo = Demo::new();
    let head = demo.commit(
        &[(".github/workflows/ci.yml", "on: push\n")],
        "Run the CI on push",
    );
    let ctx = demo.ctx();
    let ledger = demo.ledger();
    let held = executor::request(&ctx, &ledger, &op("git.push", json!({}))).unwrap();
    let command = format!("vision approve {KEY} git.push {head}");
    assert_eq!(
        held.outcome,
        Outcome::Waiting {
            command: command.clone()
        },
        "{held:?}"
    );
    assert_eq!(held.held, [".github/workflows/ci.yml"]);
    assert!(
        held.detail
            .contains("which the approved plan does not name"),
        "{held:?}"
    );
    assert!(demo.remote_branch(BRANCH).is_none());
    let needs = executor::needs(&ctx, &ledger).unwrap();
    assert_eq!(needs.len(), 1);
    assert_eq!(needs[0].id, format!("git.push {head}"));
    assert!(
        needs[0]
            .text
            .contains("is held: the diff touches .github/workflows/ci.yml"),
        "{needs:?}"
    );

    // The approved plan names the path: the next request runs it.
    let plan = demo.home.path("tickets/DEMO-12/approved-plan.md");
    std::fs::create_dir_all(plan.parent().unwrap()).unwrap();
    std::fs::write(
        &plan,
        "# Plan\n\nChange .github/workflows/ci.yml to run on push.\n",
    )
    .unwrap();
    let ran = executor::request(&ctx, &ledger, &op("git.push", json!({}))).unwrap();
    assert_eq!(ran.id, held.id);
    assert_eq!(ran.outcome, Outcome::Confirmed, "{ran:?}");
    assert_eq!(demo.remote_branch(BRANCH).as_deref(), Some(head.as_str()));
}

#[test]
fn the_person_can_approve_a_held_sensitive_path() {
    let demo = Demo::new();
    let head = demo.commit(
        &[("db/migrate/001_init.sql", "CREATE TABLE t (id INTEGER);\n")],
        "Add a migration",
    );
    let held = executor::request(&demo.ctx(), &demo.ledger(), &op("git.push", json!({}))).unwrap();
    assert!(matches!(held.outcome, Outcome::Waiting { .. }), "{held:?}");
    let run = demo.run(&["approve", KEY, "git.push", &head]);
    run.expect_code(0);
    assert_eq!(demo.remote_branch(BRANCH).as_deref(), Some(head.as_str()));
}

#[test]
fn promote_and_demote_move_one_step_on_the_ladder() {
    let demo = Demo::new();
    let run = demo.run(&["demote", "git.push"]);
    run.expect_code(0);
    assert!(run.stdout.starts_with("git.push: auto -> ask."), "{run:?}");
    let config = std::fs::read_to_string(demo.home.path("config.toml")).unwrap();
    assert!(config.contains("\"git.push\" = \"ask\""), "{config}");
    assert!(
        config.contains("# verb -> hold | shadow | ask | auto"),
        "the comments stay"
    );
    for expected in ["shadow", "hold"] {
        demo.run(&["demote", "git.push"]).expect_code(0);
        let config = std::fs::read_to_string(demo.home.path("config.toml")).unwrap();
        assert!(
            config.contains(&format!("\"git.push\" = \"{expected}\"")),
            "{config}"
        );
    }
    let bottom = demo.run(&["demote", "git.push"]);
    bottom.expect_code(1);
    assert!(
        bottom
            .stderr
            .contains("git.push is already hold, the bottom of the ladder"),
        "{bottom:?}"
    );
    for _ in 0..3 {
        demo.run(&["promote", "git.push"]).expect_code(0);
    }
    let top = demo.run(&["promote", "git.push"]);
    top.expect_code(1);
    assert!(top.stderr.contains("already auto"), "{top:?}");
    let review = demo.run(&["promote", "review.reply"]);
    review.expect_code(0);
    assert!(
        review.stdout.starts_with("review.reply: ask -> auto."),
        "{review:?}"
    );
    let unknown = demo.run(&["promote", "git.force"]);
    unknown.expect_code(1);
    assert!(
        unknown.stderr.contains("has no verb `git.force`"),
        "{unknown:?}"
    );
    let read = demo.run(&["promote", "tracker.issue"]);
    read.expect_code(1);
    assert!(read.stderr.contains("read verb"), "{read:?}");

    let ledger = demo.ledger();
    let rows = ledger.human_rows("git.push").unwrap();
    assert_eq!(rows.len(), 6);
    assert_eq!(rows[0].kind, "demote");
    assert_eq!(rows[0].value.as_deref(), Some("ask"));
    assert_eq!(rows[0].reason.as_deref(), Some("from auto"));
    assert_eq!(rows[5].kind, "promote");
    assert_eq!(rows[5].value.as_deref(), Some("auto"));
}

#[test]
fn reconcile_after_a_crash_never_runs_the_operation_again() {
    let demo = Demo::new();
    let head = demo.commit_subtract();
    demo.set_mode("git.push", "ask");
    let ctx = demo.ctx();
    let ledger = demo.ledger();
    let asked = executor::request(&ctx, &ledger, &op("git.push", json!({}))).unwrap();
    // The crash: act pushed, and the process died before it stored the answer.
    executor_support::git(
        &demo.clone,
        &[
            "push",
            "--quiet",
            demo.world.join("remote.git").to_str().unwrap(),
            &format!("HEAD:refs/heads/{BRANCH}"),
        ],
    );
    ledger
        .conn()
        .execute(
            "UPDATE operations SET state = 'executing' WHERE id = ?1",
            [&asked.id],
        )
        .unwrap();
    let crashed = executor::request(&ctx, &ledger, &op("git.push", json!({}))).unwrap();
    assert_eq!(
        crashed.outcome,
        Outcome::Executing,
        "a request never runs it a second time"
    );

    let tick = demo.run(&["tick"]);
    tick.expect_code(0);
    let done = executor::operation(&ledger, &asked.id).unwrap().unwrap();
    assert_eq!(done.outcome, Outcome::Confirmed, "{done:?}");
    assert!(done.detail.starts_with("reconcile:"), "{done:?}");
    assert_eq!(demo.ticket_row().head.as_deref(), Some(head.as_str()));
    assert_eq!(
        demo.acts_of("git.push"),
        0,
        "the reconcile only reads the outside"
    );
    let outcomes = ledger.outcomes_of(&asked.id).unwrap();
    assert_eq!(outcomes.len(), 1);
    assert_eq!(outcomes[0].kind, "reconcile");
    assert_eq!(outcomes[0].state.as_deref(), Some("confirmed"));
}

#[test]
fn reconcile_finds_an_operation_that_did_not_happen() {
    let demo = Demo::new();
    demo.commit_subtract();
    demo.set_mode("pr.open", "ask");
    let ctx = demo.ctx();
    let ledger = demo.ledger();
    let asked = executor::request(&ctx, &ledger, &op("pr.open", json!({}))).unwrap();
    ledger
        .conn()
        .execute(
            "UPDATE operations SET state = 'uncertain' WHERE id = ?1",
            [&asked.id],
        )
        .unwrap();
    let results = executor::reconcile(&ctx, &ledger).unwrap();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].outcome, Outcome::Failed);
    assert!(
        results[0].detail.contains("no pull request is open"),
        "{:?}",
        results[0]
    );
    assert!(demo.world_json("prs.json")["prs"]
        .as_array()
        .unwrap()
        .is_empty());
    assert!(
        executor::reconcile(&ctx, &ledger).unwrap().is_empty(),
        "a failed operation is final"
    );
}

/// An adapter whose scripts fail, refuse, or never end.
fn broken_adapter(demo: &Demo) {
    let dir = demo.home.user_home.join("broken adapter");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("adapter.toml"),
        r#"name = "broken"

[[step]]
id = "work"
kind = "job"
job = "hang"
next = "done"

[[step]]
id = "done"
kind = "end"

[verbs."x.fail"]
mode = "auto"

[verbs."x.refuse"]
mode = "auto"

[verbs."x.hang"]
mode = "auto"
timeout = 1

[jobs.hang]
timeout = 1

[jobs.fail]
"#,
    )
    .unwrap();
    let act = r#"#!/usr/bin/env python3
import json, sys, time
req = json.load(sys.stdin)
verb = req.get("verb")
if req.get("reconcile"):
    print(json.dumps({"state": "failed", "detail": "nothing happened outside"}))
    sys.exit(0)
if verb == "x.fail":
    print("first line", file=sys.stderr)
    print("the forge is down", file=sys.stderr)
    sys.exit(1)
if verb == "x.refuse":
    print("this branch is protected", file=sys.stderr)
    sys.exit(2)
time.sleep(30)
"#;
    let job = r#"#!/usr/bin/env python3
import json, sys, time
req = json.load(sys.stdin)
if req.get("job") == "fail":
    print("the runner is gone", file=sys.stderr)
    sys.exit(3)
time.sleep(30)
"#;
    executor_support::write_script(&dir.join("bin/act"), act);
    executor_support::write_script(&dir.join("bin/job"), job);
    executor_support::write_script(
        &dir.join("bin/observe"),
        "#!/bin/sh\necho '{\"tickets\": []}'\n",
    );
    let dir = dir.display().to_string();
    demo.edit_config(|doc| doc["adapter"]["dir"] = toml_edit::value(dir));
}

/// The value of the secret of the probe adapter.
const PROBE_SECRET: &str = "probe-secret-value-4711";

/// An adapter whose `bin/act` and `bin/job` write each call object to `calls.jsonl` in the
/// home of the person, and answer by the verb:
///
/// - `x.describe`: confirmed, with documents: one good one (with the secret in it), one that
///   the pipeline owns, one with a folder part.
/// - `x.no`: failed, with a document.
/// - `x.later`: uncertain with the external id `ext-7`; its reconcile is confirmed with a
///   document.
/// - `x.move`: confirmed; `once = true` and the mode ask.
fn probe_adapter(demo: &Demo) -> std::path::PathBuf {
    let dir = demo.home.user_home.join("probe adapter");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("adapter.toml"),
        r#"name = "probe"
base = "origin/main"

[[step]]
id = "work"
kind = "job"
job = "look"
next = "done"

[[step]]
id = "done"
kind = "end"

[verbs."x.describe"]
mode = "auto"

[verbs."x.no"]
mode = "auto"

[verbs."x.later"]
mode = "auto"

[verbs."x.move"]
mode = "ask"
once = true

[jobs.look]
"#,
    )
    .unwrap();
    let calls = demo.home.user_home.join("calls.jsonl");
    let act = format!(
        r#"#!/usr/bin/env python3
import json, sys
req = json.load(sys.stdin)
with open({calls:?}, "a") as f:
    f.write(json.dumps(req) + "\n")
verb = req.get("verb")
docs = [{{"name": "backup-1.json", "content": "old text {PROBE_SECRET}"}},
        {{"name": "plan.md", "content": "x"}},
        {{"name": "../escape.txt", "content": "x"}}]
if verb == "x.describe":
    out = {{"state": "confirmed", "detail": "done", "documents": docs}}
elif verb == "x.no":
    out = {{"state": "failed", "detail": "no", "documents": [{{"name": "no.txt", "content": "x"}}]}}
elif verb == "x.later" and not req.get("reconcile"):
    out = {{"state": "uncertain", "detail": "not known yet", "external_id": "ext-7"}}
elif verb == "x.later":
    out = {{"state": "confirmed", "detail": "it happened", "documents": [{{"name": "later.txt", "content": "found"}}]}}
else:
    out = {{"state": "confirmed", "detail": "moved"}}
print(json.dumps(out))
"#
    );
    let job = format!(
        r#"#!/usr/bin/env python3
import json, sys
req = json.load(sys.stdin)
with open({calls:?}, "a") as f:
    f.write(json.dumps(req) + "\n")
print(json.dumps({{"ok": True, "detail": "looked"}}))
"#
    );
    let observe = format!(
        r#"#!/usr/bin/env python3
import json, sys
req = json.load(sys.stdin)
with open({calls:?}, "a") as f:
    f.write(json.dumps(req) + "\n")
print(json.dumps({{"tickets": []}}))
"#
    );
    executor_support::write_script(&dir.join("bin/act"), &act);
    executor_support::write_script(&dir.join("bin/job"), &job);
    executor_support::write_script(&dir.join("bin/observe"), &observe);
    let dir_text = dir.display().to_string();
    demo.edit_config(|doc| {
        doc["adapter"]["dir"] = toml_edit::value(dir_text);
        let mut secret = toml_edit::InlineTable::new();
        secret.insert("cmd", format!("printf {PROBE_SECRET}").into());
        doc["secrets"]["PROBE_TOKEN"] = toml_edit::value(secret);
    });
    calls
}

/// The call objects that the probe adapter recorded.
fn probe_calls(calls: &std::path::Path) -> Vec<Value> {
    std::fs::read_to_string(calls)
        .unwrap_or_default()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

#[test]
fn the_documents_of_a_confirmed_act_answer_go_to_the_ticket_folder() {
    let demo = Demo::new();
    probe_adapter(&demo);
    let ctx = demo.ctx();
    let ledger = demo.ledger();
    let done = executor::request(&ctx, &ledger, &op("x.describe", json!({}))).unwrap();
    assert_eq!(done.outcome, Outcome::Confirmed, "{done:?}");
    assert_eq!(done.documents, ["backup-1.json"]);
    let folder = demo.home.path("tickets/DEMO-12");
    assert_eq!(
        std::fs::read_to_string(folder.join("backup-1.json")).unwrap(),
        "old text [secret PROBE_TOKEN]",
        "a document is masked"
    );
    assert!(
        !folder.join("plan.md").exists(),
        "the pipeline owns plan.md"
    );
    assert!(!demo.home.path("tickets/escape.txt").exists());
    assert!(
        done.detail
            .contains("the document `plan.md` belongs to the person or the pipeline")
            && done.detail.contains("is not a plain file name"),
        "each dropped document is named: {done:?}"
    );
    let stored = executor::operation(&ledger, &done.id).unwrap().unwrap();
    assert_eq!(
        stored.documents,
        ["backup-1.json"],
        "the row keeps the names"
    );

    let no = executor::request(&ctx, &ledger, &op("x.no", json!({}))).unwrap();
    assert_eq!(no.outcome, Outcome::Failed);
    assert!(no.documents.is_empty());
    assert!(
        !folder.join("no.txt").exists(),
        "a failed answer writes nothing"
    );

    // The documents of a confirmed reconcile answer go there too.
    let later = executor::request(&ctx, &ledger, &op("x.later", json!({}))).unwrap();
    assert_eq!(later.outcome, Outcome::Uncertain);
    assert!(!folder.join("later.txt").exists());
    let results = executor::reconcile(&ctx, &ledger).unwrap();
    assert_eq!(results[0].outcome, Outcome::Confirmed, "{results:?}");
    assert_eq!(results[0].documents, ["later.txt"]);
    assert_eq!(
        std::fs::read_to_string(folder.join("later.txt")).unwrap(),
        "found"
    );
}

#[test]
fn a_verb_with_once_runs_one_time_for_a_ticket_at_the_commit_of_its_first_request() {
    let demo = Demo::new();
    let calls = probe_adapter(&demo);
    let ctx = demo.ctx();
    let ledger = demo.ledger();
    let first = demo.commit(&[("notes.txt", "one\n")], "One");
    let asked = executor::request(
        &ctx,
        &ledger,
        &op("x.move", json!({"status": "in_progress"})),
    )
    .unwrap();
    let command = format!("vision approve {KEY} x.move {first}");
    assert_eq!(
        asked.outcome,
        Outcome::Waiting {
            command: command.clone()
        }
    );
    // A new commit asks for the same operation: it waits at the commit of the first request.
    let second = demo.commit(&[("notes.txt", "two\n")], "Two");
    let again = executor::request(
        &ctx,
        &ledger,
        &op("x.move", json!({"status": "in_progress"})),
    )
    .unwrap();
    assert_eq!(again.id, asked.id, "the key has no commit");
    assert_eq!(again.outcome, Outcome::Waiting { command });
    assert_eq!(again.head, first);
    let found = executor::lookup(
        &ctx,
        &ledger,
        &op("x.move", json!({"status": "in_progress"})),
    )
    .unwrap()
    .unwrap();
    assert_eq!(
        found.id, asked.id,
        "the lookup at the newer commit finds it"
    );
    let needs = executor::needs(&ctx, &ledger).unwrap();
    assert_eq!(needs.len(), 1);
    assert_eq!(needs[0].id, format!("x.move {first}"));
    // The approval of the first commit runs it, though the ticket is at a newer commit now.
    let ran = executor::approve(&ctx, &ledger, &key(), "x.move", &first, None).unwrap();
    assert_eq!(ran.len(), 1);
    assert_eq!(ran[0].outcome, Outcome::Confirmed, "{ran:?}");
    let moves: Vec<Value> = probe_calls(&calls)
        .into_iter()
        .filter(|c| c["verb"] == "x.move")
        .collect();
    assert_eq!(moves.len(), 1);
    assert_eq!(
        moves[0]["head"], first,
        "the call names the commit of the first request"
    );
    // A later commit and a later request: nothing runs again.
    demo.commit(&[("notes.txt", "three\n")], "Three");
    let later = executor::request(
        &ctx,
        &ledger,
        &op("x.move", json!({"status": "in_progress"})),
    )
    .unwrap();
    assert_eq!(later.id, asked.id);
    assert_eq!(later.outcome, Outcome::Confirmed);
    // Other arguments are another operation.
    let other =
        executor::request(&ctx, &ledger, &op("x.move", json!({"status": "in_review"}))).unwrap();
    assert_ne!(other.id, asked.id);
    assert!(matches!(other.outcome, Outcome::Waiting { .. }));
    assert_ne!(
        second, other.head,
        "it waits at the commit of its own first request"
    );
    assert_eq!(
        executor::once_key(KEY, "x.move", &json!({"a": 1})),
        executor::once_key(KEY, "x.move", &json!({"a": 1}))
    );
    assert_ne!(
        executor::once_key(KEY, "x.move", &json!({"a": 1})),
        executor::idempotency_key(KEY, "x.move", "", &json!({"a": 1})),
        "a once key never meets the key of a ticket with no commit"
    );
}

#[test]
fn a_reconcile_gets_the_external_id_and_the_start_and_each_call_gets_the_base_ref() {
    let demo = Demo::new();
    let calls = probe_adapter(&demo);
    let ctx = demo.ctx();
    let ledger = demo.ledger();
    demo.commit(&[("notes.txt", "one\n")], "One");
    let later = executor::request(&ctx, &ledger, &op("x.later", json!({}))).unwrap();
    assert_eq!(later.outcome, Outcome::Uncertain);
    assert_eq!(later.external_id.as_deref(), Some("ext-7"));
    let started = later.started.clone().expect("the time of the first call");
    // An operation that crashed before its answer has no external id.
    let crashed = executor::request(&ctx, &ledger, &op("x.describe", json!({"n": 2}))).unwrap();
    ledger
        .conn()
        .execute(
            "UPDATE operations SET state = 'executing', external_id = NULL WHERE id = ?1",
            [&crashed.id],
        )
        .unwrap();
    executor::reconcile(&ctx, &ledger).unwrap();
    executor::job(&ctx, &ledger, &job("look", json!({}))).unwrap();
    executor::observe_queue(&ctx).unwrap();

    let recorded = probe_calls(&calls);
    let reconciles: Vec<&Value> = recorded.iter().filter(|c| c["reconcile"] == true).collect();
    assert_eq!(reconciles.len(), 2, "{recorded:?}");
    let of = |verb: &str| *reconciles.iter().find(|c| c["verb"] == verb).unwrap();
    assert_eq!(of("x.later")["external_id"], "ext-7");
    assert_eq!(of("x.later")["started"], started.as_str());
    assert_eq!(of("x.describe")["external_id"], Value::Null);
    assert!(of("x.describe")["started"].is_string());
    let first_call = recorded
        .iter()
        .find(|c| c["verb"] == "x.later" && c.get("reconcile").is_none())
        .unwrap();
    assert!(
        first_call.get("external_id").is_none() && first_call.get("started").is_none(),
        "only a reconcile gets the facts of the first call: {first_call}"
    );
    // Each call object of act, job, and observe names the base of adapter.toml.
    assert!(recorded.iter().any(|c| c["job"] == "look"));
    assert!(recorded.iter().any(|c| c["what"] == "queue"));
    for call in &recorded {
        assert_eq!(call["base_ref"], "origin/main", "{call}");
    }
}

#[test]
fn a_script_that_fails_refuses_or_never_ends() {
    let demo = Demo::new();
    broken_adapter(&demo);
    let ctx = demo.ctx();
    let ledger = demo.ledger();

    let failed = executor::request(&ctx, &ledger, &op("x.fail", json!({}))).unwrap();
    assert_eq!(failed.outcome, Outcome::Uncertain, "{failed:?}");
    assert!(
        failed.detail.contains("bin/act failed with exit code 1"),
        "{failed:?}"
    );
    assert!(failed.detail.contains("the forge is down"), "{failed:?}");

    let refused = executor::request(&ctx, &ledger, &op("x.refuse", json!({}))).unwrap();
    assert_eq!(
        refused.outcome,
        Outcome::Refused {
            gate: "script".into()
        }
    );
    assert!(
        refused.detail.contains("this branch is protected"),
        "{refused:?}"
    );

    let started = std::time::Instant::now();
    let hung = executor::request(&ctx, &ledger, &op("x.hang", json!({}))).unwrap();
    assert!(started.elapsed() < std::time::Duration::from_secs(8));
    assert_eq!(hung.outcome, Outcome::Uncertain);
    assert!(hung.detail.contains("did not end in 1 s"), "{hung:?}");

    // An uncertain operation never runs again by itself: the reconcile ends it.
    let again = executor::request(&ctx, &ledger, &op("x.fail", json!({}))).unwrap();
    assert_eq!(again.id, failed.id);
    assert_eq!(again.outcome, Outcome::Uncertain);
    let results = executor::reconcile(&ctx, &ledger).unwrap();
    assert_eq!(results.len(), 2);
    assert!(
        results.iter().all(|r| r.outcome == Outcome::Failed),
        "{results:?}"
    );

    let hang = executor::job(&ctx, &ledger, &job("hang", json!({}))).unwrap();
    assert!(!hang.ok);
    assert!(
        hang.failure
            .as_deref()
            .unwrap()
            .contains("did not end in 1 s"),
        "{hang:?}"
    );
    let fail = executor::job(&ctx, &ledger, &job("fail", json!({}))).unwrap();
    assert!(
        fail.failure.as_deref().unwrap().contains("exit code 3"),
        "{fail:?}"
    );
    assert!(
        fail.failure
            .as_deref()
            .unwrap()
            .contains("the runner is gone"),
        "{fail:?}"
    );
    let checks = ledger.checks_of(KEY, None).unwrap();
    let results: Vec<(&str, &str, &str)> = checks
        .iter()
        .map(|c| (c.kind.as_str(), c.result.as_str(), c.cause.as_str()))
        .collect();
    assert_eq!(
        results,
        [("job", "error", "infra"), ("job", "error", "infra")]
    );
}

#[test]
fn the_proof_is_red_on_the_base_and_green_at_the_last_commit() {
    let demo = Demo::new();
    let head = demo.commit_subtract();
    let base = executor_support::git(&demo.clone, &["merge-base", "origin/main", "HEAD"]);
    let ctx = demo.ctx();
    let ledger = demo.ledger();
    let result = executor::job(&ctx, &ledger, &job("proof", json!({}))).unwrap();
    assert!(result.ok, "{result:?}");
    assert_eq!(result.head, head);
    assert!(
        result
            .detail
            .contains(&format!("fail on the base {}", &base[..12])),
        "{result:?}"
    );
    assert_eq!(result.documents, ["proof.txt"]);
    let proof = std::fs::read_to_string(demo.home.path("tickets/DEMO-12/proof.txt")).unwrap();
    assert!(proof.contains("cannot import name 'subtract'"), "{proof}");
    let checks = ledger.checks_of(KEY, Some(&head)).unwrap();
    let rows: Vec<(&str, &str, &str)> = checks
        .iter()
        .map(|c| (c.kind.as_str(), c.result.as_str(), c.source.as_str()))
        .collect();
    assert_eq!(
        rows,
        [
            ("proof-head", "pass", "job:proof"),
            ("proof-base", "fail", "job:proof"),
            ("job", "pass", "job:proof")
        ]
    );
    assert_eq!(checks[0].command, executor_support::TEST_COMMAND);
    assert_eq!(
        executor::job_passed(&ledger, KEY, "proof", &head).unwrap(),
        Some(true)
    );

    // Any new commit voids the proof. A change with no test does not prove anything.
    let newer = demo.commit(&[("notes.txt", "a note\n")], "Write a note");
    assert_eq!(
        executor::job_passed(&ledger, KEY, "proof", &newer).unwrap(),
        None
    );
    executor_support::git(&demo.clone, &["reset", "--quiet", "--hard", &base]);
    let calc = std::fs::read_to_string(demo.clone.join("calc.py")).unwrap();
    let untested = demo.commit(
        &[(
            "calc.py",
            &format!("{calc}\n\ndef negate(a):\n    return -a\n"),
        )],
        "Add negate",
    );
    let result = executor::job(&ctx, &ledger, &job("proof", json!({}))).unwrap();
    assert!(!result.ok);
    assert!(result.detail.contains("no test changed"), "{result:?}");
    assert_eq!(
        executor::job_passed(&ledger, KEY, "proof", &untested).unwrap(),
        Some(false)
    );
    let green = executor::job(&ctx, &ledger, &job("proof", json!({"green": true}))).unwrap();
    assert!(green.ok, "{green:?}");
    assert_eq!(
        executor::job_passed(&ledger, KEY, "proof", &untested).unwrap(),
        Some(true)
    );
}

#[test]
fn the_proof_reads_the_plan_and_a_job_runs_again_when_its_arguments_change() {
    let demo = Demo::new();
    // A change of a test that also passes on the base proves nothing by itself.
    let calc_test = std::fs::read_to_string(demo.clone.join("test_calc.py")).unwrap();
    let head = demo.commit(
        &[(
            "test_calc.py",
            &calc_test.replace(
                "self.assertEqual(add(2, 3), 5)",
                "self.assertEqual(add(2, 3), 5)\n        self.assertEqual(add(0, 0), 0)",
            ),
        )],
        "Test add with zero",
    );
    let ctx = demo.ctx();
    let ledger = demo.ledger();
    let run = |args: Value| executor::job(&ctx, &ledger, &job("proof", args)).unwrap();
    let none = run(json!({"plan": "# Plan\n\n## Proof\n\ntest_calc.py covers it.\n"}));
    assert!(!none.ok, "{none:?}");
    assert!(none.detail.contains("also pass on the base"), "{none:?}");
    // One test file declared green, after list and emphasis marks.
    let file = json!({"plan": "# Plan\n\n- **Proof:** green test_calc.py\n"});
    let one = run(file.clone());
    assert!(one.ok, "{one:?}");
    assert!(
        one.detail.contains("declares each changed test green"),
        "{one:?}"
    );
    // The whole proof declared green, in any case.
    let whole = json!({"plan": "PROOF: Green - a test of an existing function\n"});
    assert!(run(whole.clone()).ok);
    assert!(
        run(json!({"plan": ""}))
            .detail
            .contains("also pass on the base"),
        "an empty plan declares nothing"
    );

    // The check row of the whole job names its arguments, so each set of arguments has its
    // own result at the same commit.
    let rows: Vec<String> = ledger
        .checks_of(KEY, Some(&head))
        .unwrap()
        .into_iter()
        .filter(|c| c.kind == "job")
        .map(|c| c.command)
        .collect();
    assert_eq!(rows.len(), 4);
    assert_eq!(rows[1], executor::job_command("proof", &file));
    assert!(rows[1].starts_with("proof ") && rows[1].len() == "proof ".len() + 16);
    assert_eq!(executor::job_command("proof", &json!({})), "proof");
    assert_eq!(
        executor::job_passed_with(&ledger, KEY, "proof", &head, &file).unwrap(),
        Some(true)
    );
    assert_eq!(
        executor::job_passed_with(&ledger, KEY, "proof", &head, &json!({"plan": ""})).unwrap(),
        Some(false)
    );
    let changed = json!({"plan": "# Plan\n\nNothing is declared.\n", "other": 1});
    assert_eq!(
        executor::job_passed_with(&ledger, KEY, "proof", &head, &changed).unwrap(),
        None,
        "new arguments at the same commit: the job runs again"
    );
    assert_eq!(
        executor::job_passed(&ledger, KEY, "proof", &head).unwrap(),
        Some(false),
        "the newest run with any arguments"
    );
}

#[test]
fn the_tick_hooks_run_a_job_and_an_operation() {
    let demo = Demo::new();
    let head = demo.commit_subtract();
    let ctx = demo.ctx();
    let mut pass = vision::tick::Pass {
        ctx: &ctx,
        ledger: demo.ledger(),
        now: vision::util::time::now(),
    };
    executor::run_job(&mut pass, &job("second-review", json!({}))).unwrap();
    executor::run_operation(&mut pass, &op("git.push", json!({}))).unwrap();
    executor::reconcile_operations(&mut pass).unwrap();
    assert_eq!(demo.remote_branch(BRANCH).as_deref(), Some(head.as_str()));
    assert_eq!(
        executor::job_passed(&pass.ledger, KEY, "second-review", &head).unwrap(),
        Some(true)
    );
    let review =
        std::fs::read_to_string(demo.home.path("tickets/DEMO-12/second-review.md")).unwrap();
    assert!(review.contains("- calc.py (+5 -0)"), "{review}");
    assert!(review.contains("- test_subtract.py (+8 -0)"), "{review}");
    let unknown = executor::job(&ctx, &demo.ledger(), &job("deploy", json!({}))).unwrap_err();
    assert!(
        unknown.to_string().contains("has no job `deploy`"),
        "{unknown}"
    );
}

#[test]
fn a_read_verb_answers_with_documents_and_needs_no_approval() {
    let demo = Demo::new();
    let ctx = demo.ctx();
    let ledger = demo.ledger();
    let answer = executor::read(
        &ctx,
        &ledger,
        &key(),
        "tracker.issue",
        &json!({"key": "DEMO-13"}),
    )
    .unwrap();
    assert_eq!(answer.documents.len(), 1);
    assert_eq!(answer.documents[0].name, "issue-DEMO-13.json");
    let issue: Value = serde_json::from_str(&answer.documents[0].content).unwrap();
    assert_eq!(issue["title"], "Add multiply to the calculator");
    assert!(
        ledger.operations_of(KEY, None).unwrap().is_empty(),
        "no operation row, no key"
    );
    let events = ledger.recent_events(Some(KEY), 5).unwrap();
    assert_eq!(events[0].kind, "read");
    assert!(events[0]
        .detail
        .as_deref()
        .unwrap()
        .contains("issue-DEMO-13.json"));
    let err = executor::read(&ctx, &ledger, &key(), "git.push", &json!({})).unwrap_err();
    assert!(
        err.to_string().contains("git.push is not a read verb"),
        "{err}"
    );
    assert!(
        err.to_string().contains("The read verbs: tracker.issue"),
        "{err}"
    );
    let err = executor::read(
        &ctx,
        &ledger,
        &key(),
        "tracker.issue",
        &json!({"key": "../x"}),
    )
    .unwrap_err();
    assert!(format!("{err:#}").contains("needs a ticket key"), "{err:#}");
}

#[test]
fn ci_run_and_review_reply_in_the_demo_world() {
    let demo = Demo::new();
    let ctx = demo.ctx();
    let ledger = demo.ledger();
    let head = demo.commit_subtract();
    for verb in ["git.push", "pr.open", "ci.run"] {
        let result = executor::request(&ctx, &ledger, &op(verb, json!({}))).unwrap();
        assert_eq!(result.outcome, Outcome::Confirmed, "{verb}: {result:?}");
    }
    assert_eq!(demo.world_json("ci.json")[&head]["state"], "green");
    let red = demo.commit(&[("test_broken.py", "import unittest\n\n\nclass Broken(unittest.TestCase):\n    def test_fails(self):\n        self.fail('red')\n")], "A red test");
    executor::request(&ctx, &ledger, &op("ci.run", json!({}))).unwrap();
    assert_eq!(demo.world_json("ci.json")[&red]["state"], "red");
    let log = executor::job(&ctx, &ledger, &job("ci-log", json!({}))).unwrap();
    assert_eq!(log.documents, ["ci-log.txt"]);

    demo.world_cmd(&["comment", demo.root(), "1", "Add a docstring"]);
    let comments = executor::job(&ctx, &ledger, &job("comments", json!({}))).unwrap();
    assert_eq!(comments.documents, ["comments.json"]);
    let reply = executor::request(
        &ctx,
        &ledger,
        &op(
            "review.reply",
            json!({"comment": 1, "body": "Done in the next commit."}),
        ),
    )
    .unwrap();
    assert!(
        matches!(reply.outcome, Outcome::Waiting { .. }),
        "review.reply starts in the mode ask: {reply:?}"
    );
    let run = demo.run(&["approve", KEY, "review.reply", &red]);
    run.expect_code(0);
    let prs = demo.world_json("prs.json");
    assert_eq!(
        prs["prs"][0]["comments"][0]["replies"][0]["body"],
        "Done in the next commit."
    );
}

#[test]
fn an_operation_needs_a_ticket_row() {
    let demo = Demo::new();
    let ledger = demo.ledger();
    let other = vision::tick::OperationRun {
        ticket: vision::paths::TicketKey::parse("DEMO-99").unwrap(),
        verb: "git.push".into(),
        args: json!({}),
        turn: None,
    };
    let err = executor::request(&demo.ctx(), &ledger, &other).unwrap_err();
    assert!(
        err.to_string().contains("Run `vision take DEMO-99` first"),
        "{err}"
    );
}

#[test]
fn the_tests_of_the_proof_never_get_a_secret() {
    let demo = Demo::new();
    demo.edit_config(|doc| {
        let mut source = toml_edit::InlineTable::new();
        source.insert("cmd", "printf 'tok-77aa21'".into());
        doc["secrets"]["FORGE_TOKEN"] = toml_edit::value(source);
    });
    let calc = std::fs::read_to_string(demo.clone.join("calc.py")).unwrap();
    demo.commit(
        &[
            (
                "calc.py",
                &format!("{calc}\n\ndef secret_seen():\n    import os\n    return \"FORGE_TOKEN\" in os.environ\n"),
            ),
            (
                "test_env.py",
                "import unittest\n\nfrom calc import secret_seen\n\n\nclass EnvTest(unittest.TestCase):\n    def test_no_secret(self):\n        self.assertFalse(secret_seen())\n",
            ),
        ],
        "Test the environment",
    );
    let result = executor::job(&demo.ctx(), &demo.ledger(), &job("proof", json!({}))).unwrap();
    assert!(result.ok, "the tests saw the secret: {result:?}");
}

#[test]
fn a_job_and_an_operation_write_the_record_of_what_runs_while_their_script_runs() {
    let demo = Demo::new();
    let dir = demo.home.user_home.join("record adapter");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("adapter.toml"),
        r#"name = "record"
base = "origin/main"

[[step]]
id = "work"
kind = "job"
job = "look"
next = "done"

[[step]]
id = "done"
kind = "end"

[verbs."x.describe"]
mode = "auto"

[jobs.look]
"#,
    )
    .unwrap();
    // Each script copies the record into a file while it runs, then answers.
    let running = demo.home.vision.join("running.json");
    let seen = demo.home.user_home.join("seen.jsonl");
    let script = |answer: &str| {
        format!(
            r#"#!/usr/bin/env python3
import json, sys
json.load(sys.stdin)
with open({running:?}) as f:
    record = f.read()
with open({seen:?}, "a") as f:
    f.write(record.strip() + "\n")
print(json.dumps({answer}))
"#
        )
    };
    executor_support::write_script(
        &dir.join("bin/act"),
        &script(r#"{"state": "confirmed", "detail": "done"}"#),
    );
    executor_support::write_script(
        &dir.join("bin/job"),
        &script(r#"{"ok": True, "detail": "looked"}"#),
    );
    executor_support::write_script(
        &dir.join("bin/observe"),
        "#!/usr/bin/env python3\nimport json\nprint(json.dumps({\"tickets\": []}))\n",
    );
    let dir_text = dir.display().to_string();
    demo.edit_config(|doc| doc["adapter"]["dir"] = toml_edit::value(dir_text));
    let ctx = demo.ctx();
    let ledger = demo.ledger();

    let looked = executor::job(&ctx, &ledger, &job("look", json!({}))).unwrap();
    assert!(looked.ok, "{looked:?}");
    assert!(!running.exists(), "the record goes when the job ends");
    let done = executor::request(&ctx, &ledger, &op("x.describe", json!({}))).unwrap();
    assert_eq!(done.state, OpState::Confirmed, "{done:?}");
    assert!(!running.exists(), "the record goes when the operation ends");

    let records: Vec<Value> = std::fs::read_to_string(&seen)
        .unwrap()
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    assert_eq!(records.len(), 2, "{records:?}");
    assert_eq!(records[0]["what"], "job");
    assert_eq!(records[0]["name"], "look");
    assert_eq!(records[0]["ticket"], KEY);
    assert_eq!(records[0]["pid"], std::process::id());
    assert!(records[0]["started"].as_str().is_some());
    assert_eq!(records[1]["what"], "operation");
    assert_eq!(records[1]["name"], "x.describe");
}
