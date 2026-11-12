//! act: tracker.describe, tracker.move, git.push, pr.open, ci.run, ci.rerun, deploy,
//! review.reply, review.rerequest, review.resolve, review.minimize, the read verbs, and the
//! reconcile form of each verb.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex};

use serde_json::{json, Value};
use vision::adapters::jira_github::adf;

use crate::fixtures::{
    adf_doc, comment, issue_comment, jira_issue, pull, reply, review, review_with, run_ended,
    run_open, GitHubWorld, Issue, Pull, Run, HEAD, OTHER, REPO_PATH,
};
use crate::jira_github_support::world::{install, World};
use crate::jira_github_support::{
    assert_no_secret_in_files, git, header_hash, read_calls, secret, secret_forms, secrets_without,
    AdapterTest, Outcome, Reply, Repos, BRANCH, KEY,
};

fn issue_path() -> String {
    format!("/rest/api/3/issue/{KEY}")
}

fn transitions_path() -> String {
    format!("/rest/api/3/issue/{KEY}/transitions")
}

/// issue.json as the pipeline writes it from the observe answer.
fn issue_json(description: &str) -> String {
    serde_json::to_string_pretty(&json!({"key": KEY, "tracker_status": "To Do",
        "issue": {"untrusted": {"key": KEY, "description": description}}}))
    .unwrap()
}

fn has(outcome: &Outcome, text: &str) -> bool {
    outcome.detail().contains(text)
}

// tracker.describe ------------------------------------------------------------------------------

const NEW: &str = "## Problem\n\nThe export is **slow**.\n\n- one\n- two";

fn describe_test() -> (AdapterTest, Arc<Mutex<Value>>) {
    let t = AdapterTest::new();
    let description = Arc::new(Mutex::new(adf_doc(&["The old text"])));
    let d = Arc::clone(&description);
    t.server.route("GET", &issue_path(), move |_| {
        Reply::json(
            200,
            Issue {
                description: d.lock().unwrap().clone(),
                ..Issue::default()
            }
            .json(),
        )
    });
    let d = Arc::clone(&description);
    t.server.route("PUT", &issue_path(), move |r| {
        *d.lock().unwrap() = r.body["fields"]["description"].clone();
        Reply::empty(204)
    });
    (t, description)
}

#[test]
fn describe_it_replaces_the_description_and_gives_the_backup() {
    let (t, _d) = describe_test();
    let args = json!({"text": NEW, "seen": issue_json("The old text")});
    let result = t.act("tracker.describe").args(args.clone()).run();
    assert_eq!(result.code, 0, "{result:?}");
    assert_eq!(result.state(), "confirmed", "{result:?}");
    let puts = t.server.calls(Some("PUT"), Some(&issue_path()));
    assert_eq!(puts.len(), 1);
    assert_eq!(
        puts[0].body,
        json!({"fields": {"description": adf::to_adf(NEW)}})
    );
    let docs = result.json["documents"].as_array().unwrap();
    assert_eq!(docs.len(), 1);
    let name = docs[0]["name"].as_str().unwrap();
    let stamp = name
        .strip_prefix("description-backup-DEMO-12-")
        .and_then(|s| s.strip_suffix(".json"))
        .unwrap();
    assert!(
        stamp.len() == 16
            && stamp.as_bytes()[8] == b'T'
            && stamp.ends_with('Z')
            && stamp[..8].bytes().all(|b| b.is_ascii_digit()),
        "{name}"
    );
    let backup: Value = serde_json::from_str(docs[0]["content"].as_str().unwrap()).unwrap();
    assert_eq!(backup["adf"], adf_doc(&["The old text"]));
    assert_eq!(backup["markdown"], "The old text");
    assert!(has(&result, name));
    assert!(has(&result, "The old text"));
    // A second call finds the new text and changes nothing.
    let again = t.act("tracker.describe").args(args).run();
    assert_eq!(again.state(), "confirmed");
    assert_eq!(t.server.count("PUT", &issue_path()), 1);
}

#[test]
fn describe_it_refuses_after_a_person_edited_the_description() {
    let (t, _d) = describe_test();
    let result = t
        .act("tracker.describe")
        .args(json!({"text": NEW, "seen": issue_json("An older text")}))
        .run();
    assert_eq!(result.code, 2, "{result:?}");
    assert!(has(&result, "a person edited the description of DEMO-12"));
    assert!(t.server.calls(Some("PUT"), Some(&issue_path())).is_empty());
}

#[test]
fn describe_the_reconcile_only_reads() {
    let (t, d) = describe_test();
    let args = json!({"text": NEW, "seen": issue_json("The old text")});
    let result = t
        .act("tracker.describe")
        .args(args.clone())
        .reconcile()
        .run();
    assert_eq!(result.state(), "failed");
    *d.lock().unwrap() = adf::to_adf(NEW);
    let result = t.act("tracker.describe").args(args).reconcile().run();
    assert_eq!(result.state(), "confirmed");
    assert!(t.server.calls(Some("PUT"), Some(&issue_path())).is_empty());
}

#[test]
fn describe_missing_arguments_keys_and_secrets_are_named() {
    let (t, _d) = describe_test();
    let result = t.act("tracker.describe").args(json!({"text": NEW})).run();
    assert_eq!(result.state(), "failed");
    assert!(has(&result, "args.seen"));
    let result = t
        .act("tracker.describe")
        .args(json!({"text": NEW, "seen": issue_json("x")}))
        .config(t.config(json!({"jira_user": null})))
        .run();
    assert_eq!(result.state(), "failed");
    assert!(has(&result, "jira_user"));
    let result = t
        .act("tracker.describe")
        .args(json!({"text": NEW, "seen": issue_json("x")}))
        .secrets(secrets_without(&["JIRA_TOKEN"]))
        .run();
    assert_eq!(result.state(), "failed");
    assert!(has(&result, "JIRA_TOKEN"));
    assert!(t.server.calls(Some("PUT"), Some(&issue_path())).is_empty());
}

#[test]
fn describe_a_server_error_after_the_change_is_uncertain_and_a_refusal_is_failed() {
    let (t, _d) = describe_test();
    let args = json!({"text": NEW, "seen": issue_json("The old text")});
    t.server
        .json_route("PUT", &issue_path(), json!({"message": "boom"}), 502);
    let result = t.act("tracker.describe").args(args.clone()).run();
    assert_eq!(result.state(), "uncertain", "{result:?}");
    assert!(has(&result, "HTTP 502"));
    t.server.json_route(
        "PUT",
        &issue_path(),
        json!({"errors": {"description": "Operation value must be an ADF document"}}),
        400,
    );
    let result = t.act("tracker.describe").args(args).run();
    assert_eq!(result.state(), "failed", "{result:?}");
    assert!(has(
        &result,
        "HTTP 400: description: Operation value must be an ADF document"
    ));
}

// tracker.move ----------------------------------------------------------------------------------

fn move_test() -> (AdapterTest, Arc<Mutex<String>>) {
    let t = AdapterTest::new();
    let status = Arc::new(Mutex::new("To Do".to_string()));
    let s = Arc::clone(&status);
    t.server.route("GET", &issue_path(), move |_| {
        Reply::json(
            200,
            Issue {
                status: &s.lock().unwrap(),
                ..Issue::default()
            }
            .json(),
        )
    });
    t.server.json_route(
        "GET",
        &transitions_path(),
        json!({"transitions": [
            {"id": "11", "name": "Start", "to": {"name": "In Progress"}},
            {"id": "21", "name": "Block", "to": {"name": "Blocked"}},
        ]}),
        200,
    );
    let s = Arc::clone(&status);
    t.server.route("POST", &transitions_path(), move |r| {
        let to = match r.body["transition"]["id"].as_str() {
            Some("11") => "In Progress",
            _ => "Blocked",
        };
        *s.lock().unwrap() = to.to_string();
        Reply::empty(204)
    });
    (t, status)
}

#[test]
fn move_a_transition_leads_there() {
    let (t, _s) = move_test();
    let result = t
        .act("tracker.move")
        .args(json!({"status": "in_progress"}))
        .run();
    assert_eq!(result.state(), "confirmed", "{result:?}");
    assert_eq!(
        t.server.calls(Some("POST"), Some(&transitions_path()))[0].body,
        json!({"transition": {"id": "11"}})
    );
    assert!(has(&result, "moved from To Do to In Progress"));
}

#[test]
fn move_the_status_already_holds() {
    let (t, s) = move_test();
    *s.lock().unwrap() = "in progress".into();
    let result = t
        .act("tracker.move")
        .args(json!({"status": "in_progress"}))
        .run();
    assert_eq!(result.state(), "confirmed");
    assert!(t
        .server
        .calls(Some("POST"), Some(&transitions_path()))
        .is_empty());
}

#[test]
fn move_no_transition_leads_there() {
    let (t, _s) = move_test();
    let result = t
        .act("tracker.move")
        .args(json!({"status": "in_review"}))
        .run();
    assert_eq!(result.state(), "failed");
    assert!(has(
        &result,
        "no transition of DEMO-12 leads from To Do to In Review"
    ));
    assert!(has(&result, "Start -> In Progress; Block -> Blocked"));
    assert!(t
        .server
        .calls(Some("POST"), Some(&transitions_path()))
        .is_empty());
}

#[test]
fn move_the_reconcile_only_reads() {
    let (t, s) = move_test();
    let args = json!({"status": "in_progress"});
    assert_eq!(
        t.act("tracker.move")
            .args(args.clone())
            .reconcile()
            .run()
            .state(),
        "failed"
    );
    *s.lock().unwrap() = "In Progress".into();
    assert_eq!(
        t.act("tracker.move").args(args).reconcile().run().state(),
        "confirmed"
    );
    assert!(t
        .server
        .calls(Some("POST"), Some(&transitions_path()))
        .is_empty());
    assert!(t
        .server
        .calls(Some("GET"), Some(&transitions_path()))
        .is_empty());
}

#[test]
fn move_an_unknown_role_names_the_key() {
    let (t, _s) = move_test();
    let result = t
        .act("tracker.move")
        .args(json!({"status": "shipped"}))
        .run();
    assert_eq!(result.state(), "failed");
    assert!(has(&result, "statuses.shipped"));
}

// git.push --------------------------------------------------------------------------------------

struct Push {
    t: AdapterTest,
    repos: Repos,
    log: std::path::PathBuf,
}

impl Push {
    fn new() -> Push {
        let mut t = AdapterTest::new();
        let repos = Repos::new(&t.path("repos"));
        let log = t.git_spy();
        Push { t, repos, log }
    }

    fn push(&self, reconcile: bool, args: Value, fields: &[(&str, Value)]) -> Outcome {
        let mut call = self
            .t
            .act("git.push")
            .args(args)
            .config(
                self.t
                    .config(json!({"git_remote_url": self.repos.remote.display().to_string()})),
            )
            .field("clone", json!(self.repos.clone.display().to_string()))
            .field("head", json!(self.repos.head));
        for (key, value) in fields {
            call = call.field(key, value.clone());
        }
        if reconcile {
            call = call.reconcile();
        }
        call.run()
    }

    fn pushes(&self) -> usize {
        read_calls(&self.log)
            .iter()
            .filter(|c| c.args.iter().any(|a| a == "push"))
            .count()
    }
}

#[test]
fn push_the_bare_repo_gets_the_branch_with_no_force_and_no_token_in_a_file() {
    let p = Push::new();
    let result = p.push(false, json!({}), &[]);
    assert_eq!(result.state(), "confirmed", "{result:?}");
    assert_eq!(result.json["set"], json!({"head": p.repos.head}));
    assert_eq!(
        p.repos.remote_head(BRANCH).as_deref(),
        Some(p.repos.head.as_str())
    );
    let calls = read_calls(&p.log);
    let pushes: Vec<_> = calls
        .iter()
        .filter(|c| c.args.iter().any(|a| a == "push"))
        .collect();
    assert_eq!(pushes.len(), 1);
    let push = &pushes[0].args;
    for flag in [
        "--force",
        "-f",
        "--force-with-lease",
        "--mirror",
        "--delete",
        "--prune",
    ] {
        assert!(!push.iter().any(|a| a == flag), "{flag}");
    }
    assert_eq!(
        push.last().unwrap(),
        &format!("refs/heads/{BRANCH}:refs/heads/{BRANCH}"),
        "no + in the refspec"
    );
    assert!(push.iter().any(|a| a == "core.hooksPath=/dev/null"));
    let config = &pushes[0].config;
    assert!(
        config.iter().any(|c| c == "http.followRedirects=false"),
        "a redirect never takes the header elsewhere"
    );
    let header = format!(
        "http.{}.extraheader={}",
        p.repos.remote.display(),
        header_hash(secret("FORGE_TOKEN"))
    );
    assert!(
        config.contains(&header),
        "the token goes as one header value in the environment of the call: {config:?}"
    );
    for call in &calls {
        assert!(call.args.iter().any(|a| a == "core.hooksPath=/dev/null"));
        assert!(
            !call
                .env
                .iter()
                .any(|n| ["JIRA_TOKEN", "FORGE_TOKEN", "PR_TOKEN"].contains(&n.as_str())),
            "git never gets a secret under its own name"
        );
        // A command line is visible to each user of the machine, so the header is never on it.
        assert!(
            !call
                .args
                .iter()
                .any(|a| a.contains("extraheader") || a.contains("Authorization")),
            "{:?}",
            call.args
        );
        for form in secret_forms() {
            assert!(
                !call.args.iter().any(|a| a.contains(&form)),
                "a secret is on a command line"
            );
        }
    }
    assert_no_secret_in_files(&[&p.repos.clone, &p.repos.remote, &p.t.home, &p.t.scratch]);
    let config = std::fs::read_to_string(p.repos.clone.join(".git/config")).unwrap();
    assert!(!config.contains("extraheader"));
    assert_eq!(
        std::fs::read_dir(&p.t.scratch).unwrap().count(),
        0,
        "the temporary push repo is gone"
    );
}

#[test]
fn push_a_second_push_and_the_reconcile_only_read() {
    let p = Push::new();
    assert_eq!(p.push(true, json!({}), &[]).state(), "failed");
    assert_eq!(p.push(false, json!({}), &[]).state(), "confirmed");
    assert_eq!(p.push(false, json!({}), &[]).state(), "confirmed");
    assert_eq!(p.push(true, json!({}), &[]).state(), "confirmed");
    assert_eq!(p.pushes(), 1);
}

#[test]
fn push_it_refuses_the_base_branch_another_branch_and_force() {
    let p = Push::new();
    type Case = (Value, Vec<(&'static str, Value)>, &'static str);
    let cases: Vec<Case> = vec![
        (
            json!({}),
            vec![("branch", json!("main"))],
            "the base branch",
        ),
        (
            json!({"branch": "vision/DEMO-13"}),
            vec![],
            "only the branch of the call",
        ),
        (json!({"force": true}), vec![], "never uses force"),
        (
            json!({}),
            vec![("branch", json!("feature/other"))],
            "does not hold the key DEMO-12",
        ),
        (
            json!({}),
            vec![("branch", json!("-vision/DEMO-12"))],
            "not a branch name",
        ),
    ];
    for (args, fields, text) in cases {
        let result = p.push(false, args.clone(), &fields);
        assert_eq!(result.code, 2, "{args} {result:?}");
        assert!(has(&result, text), "{text}: {result:?}");
    }
    assert!(read_calls(&p.log).is_empty(), "a refusal runs no git");
    assert_eq!(p.repos.remote_head(BRANCH), None);
}

#[test]
fn push_a_branch_that_moved_on_the_remote_is_never_forced() {
    let p = Push::new();
    assert_eq!(p.push(false, json!({}), &[]).state(), "confirmed");
    let other = p.t.path("other");
    git(
        &[
            "clone",
            "--quiet",
            "--branch",
            BRANCH,
            p.repos.remote.to_str().unwrap(),
            other.to_str().unwrap(),
        ],
        p.t.tmp.path(),
    );
    std::fs::write(other.join("x.txt"), "x\n").unwrap();
    git(&["add", "-A"], &other);
    git(&["commit", "--quiet", "-m", "someone else"], &other);
    git(&["push", "--quiet", "origin", BRANCH], &other);
    let theirs = p.repos.remote_head(BRANCH);
    p.repos.write(&[("y.txt", "y\n")]);
    let mine = p.repos.commit("mine");
    let result = p.push(false, json!({}), &[("head", json!(mine))]);
    assert_eq!(result.state(), "failed", "{result:?}");
    assert!(has(&result, "never forces a push"));
    assert_eq!(p.repos.remote_head(BRANCH), theirs);
}

#[test]
fn push_the_clone_must_be_on_the_branch_of_the_call() {
    let p = Push::new();
    git(&["checkout", "--quiet", "main"], &p.repos.clone);
    let result = p.push(false, json!({}), &[]);
    assert_eq!(result.code, 2, "{result:?}");
    assert!(has(&result, "the clone is on 'main'"));
}

#[test]
fn push_the_remote_must_be_https_or_local() {
    let p = Push::new();
    let result =
        p.t.act("git.push")
            .field("clone", json!(p.repos.clone.display().to_string()))
            .field("head", json!(p.repos.head))
            .config(p.t.config(json!({"git_remote_url": "http://example.com/acme/widgets.git"})))
            .run();
    assert_eq!(result.state(), "failed");
    assert!(has(&result, "git_remote_url"));
}

// pr.open ---------------------------------------------------------------------------------------

const PR_TEXT: &str = "# Export the report\n\nThe export now makes a CSV file.\n\nCloses DEMO-12.";

fn pr_test() -> (AdapterTest, GitHubWorld) {
    let t = AdapterTest::new();
    let w = GitHubWorld::new(&t.server, None);
    let st = Arc::clone(&w.state);
    t.server
        .route("POST", &format!("{REPO_PATH}/pulls"), move |_| {
            let pr = pull(true);
            st.lock().unwrap().pr = Some(pr.clone());
            Reply::json(201, pr)
        });
    (t, w)
}

#[test]
fn pr_a_new_draft_pull_request_with_the_token_of_the_person() {
    let (t, _w) = pr_test();
    let result = t
        .act("pr.open")
        .args(json!({"text": PR_TEXT}))
        .field("head", json!(HEAD))
        .run();
    assert_eq!(result.state(), "confirmed", "{result:?}");
    assert_eq!(result.json["set"], json!({"pr": 7, "head": HEAD}));
    assert_eq!(result.json["external_id"], "7");
    let post = &t
        .server
        .calls(Some("POST"), Some(&format!("{REPO_PATH}/pulls")))[0];
    assert_eq!(
        post.body,
        json!({"title": "Export the report", "head": BRANCH, "base": "main",
               "body": "The export now makes a CSV file.\n\nCloses DEMO-12.", "draft": true})
    );
    assert_eq!(
        post.headers["authorization"],
        format!("Bearer {}", secret("PR_TOKEN"))
    );
}

#[test]
fn pr_without_the_token_of_the_person_the_forge_token_opens_it() {
    let (t, _w) = pr_test();
    let result = t
        .act("pr.open")
        .args(json!({"text": PR_TEXT}))
        .secrets(secrets_without(&["PR_TOKEN"]))
        .run();
    assert_eq!(result.state(), "confirmed", "{result:?}");
    let post = &t
        .server
        .calls(Some("POST"), Some(&format!("{REPO_PATH}/pulls")))[0];
    assert_eq!(
        post.headers["authorization"],
        format!("Bearer {}", secret("FORGE_TOKEN"))
    );
}

#[test]
fn pr_an_existing_pull_request_is_returned() {
    let (t, w) = pr_test();
    w.lock().pr = Some(pull(true));
    let result = t.act("pr.open").args(json!({"text": PR_TEXT})).run();
    assert_eq!(result.state(), "confirmed");
    assert_eq!(result.json["set"]["pr"], 7);
    assert!(t
        .server
        .calls(Some("POST"), Some(&format!("{REPO_PATH}/pulls")))
        .is_empty());
}

#[test]
fn pr_the_reconcile_only_reads() {
    let (t, w) = pr_test();
    let args = json!({"text": PR_TEXT});
    assert_eq!(
        t.act("pr.open")
            .args(args.clone())
            .reconcile()
            .run()
            .state(),
        "failed"
    );
    w.lock().pr = Some(pull(true));
    assert_eq!(
        t.act("pr.open").args(args).reconcile().run().state(),
        "confirmed"
    );
    assert!(t
        .server
        .calls(Some("POST"), Some(&format!("{REPO_PATH}/pulls")))
        .is_empty());
}

#[test]
fn pr_a_refused_pull_request_is_failed() {
    let (t, _w) = pr_test();
    t.server.json_route(
        "POST",
        &format!("{REPO_PATH}/pulls"),
        json!({"message": "Validation Failed", "errors": [{"message": "No commits between main and x"}]}),
        422,
    );
    let result = t.act("pr.open").args(json!({"text": PR_TEXT})).run();
    assert_eq!(result.state(), "failed", "{result:?}");
    assert!(has(&result, "HTTP 422"));
    assert!(has(&result, "No commits between"));
}

// ci.run ----------------------------------------------------------------------------------------

fn dispatch_path() -> String {
    format!("{REPO_PATH}/actions/workflows/ci.yml/dispatches")
}

fn ci_test() -> (AdapterTest, GitHubWorld, Arc<Mutex<bool>>) {
    let t = AdapterTest::new();
    let w = GitHubWorld::new(&t.server, Some(pull(true)));
    let start_run = Arc::new(Mutex::new(true));
    let (st, start) = (Arc::clone(&w.state), Arc::clone(&start_run));
    t.server.route("POST", &dispatch_path(), move |_| {
        if *start.lock().unwrap() {
            st.lock().unwrap().runs.push(run_open(41, "queued"));
        }
        Reply::empty(204)
    });
    (t, w, start_run)
}

#[test]
fn ci_run_the_new_run_shows_up() {
    let (t, _w, _s) = ci_test();
    let result = t.act("ci.run").field("head", json!(HEAD)).run();
    assert_eq!(result.state(), "confirmed", "{result:?}");
    assert_eq!(result.json["external_id"], "run-41");
    assert_eq!(
        t.server.calls(Some("POST"), Some(&dispatch_path()))[0].body,
        json!({"ref": BRANCH})
    );
}

#[test]
fn ci_run_no_run_shows_up() {
    let (t, _w, start) = ci_test();
    *start.lock().unwrap() = false;
    let result = t
        .act("ci.run")
        .field("head", json!(HEAD))
        .config(t.config(json!({"ci_find_seconds": 1})))
        .run();
    assert_eq!(result.state(), "uncertain", "{result:?}");
    assert!(has(&result, "no run at"));
    assert_eq!(t.server.count("POST", &dispatch_path()), 1);
}

#[test]
fn ci_run_a_run_of_the_head_is_used_and_nothing_starts() {
    let (t, w, _s) = ci_test();
    w.lock().runs = vec![Run::new(40).conclusion(Some("failure")).json()];
    let result = t.act("ci.run").field("head", json!(HEAD)).run();
    assert_eq!(result.state(), "confirmed");
    assert_eq!(result.json["external_id"], "run-40");
    assert!(t
        .server
        .calls(Some("POST"), Some(&dispatch_path()))
        .is_empty());
}

#[test]
fn ci_run_a_branch_that_is_not_pushed_is_failed() {
    let (t, w, _s) = ci_test();
    w.lock().branch_sha = Some(OTHER.into());
    let result = t.act("ci.run").field("head", json!(HEAD)).run();
    assert_eq!(result.state(), "failed");
    assert!(has(&result, "push it first"));
    assert!(t
        .server
        .calls(Some("POST"), Some(&dispatch_path()))
        .is_empty());
}

#[test]
fn ci_run_the_reconcile_only_reads() {
    let (t, w, _s) = ci_test();
    assert_eq!(
        t.act("ci.run")
            .field("head", json!(HEAD))
            .reconcile()
            .run()
            .state(),
        "uncertain"
    );
    w.lock().runs = vec![Run::new(41).json()];
    assert_eq!(
        t.act("ci.run")
            .field("head", json!(HEAD))
            .reconcile()
            .run()
            .state(),
        "confirmed"
    );
    assert!(t
        .server
        .calls(Some("POST"), Some(&dispatch_path()))
        .is_empty());
}

#[test]
fn ci_run_a_workflow_with_no_dispatch_trigger_is_failed() {
    let (t, _w, _s) = ci_test();
    t.server.json_route(
        "POST",
        &dispatch_path(),
        json!({"message": "Workflow does not have 'workflow_dispatch' trigger"}),
        422,
    );
    let result = t.act("ci.run").field("head", json!(HEAD)).run();
    assert_eq!(result.state(), "failed");
    assert!(has(&result, "workflow_dispatch"));
}

// review.reply ----------------------------------------------------------------------------------

fn reply_test() -> (AdapterTest, GitHubWorld, String) {
    let t = AdapterTest::new();
    let w = GitHubWorld::new(&t.server, Some(pull(false)));
    w.lock().comments = vec![
        comment(10, "lead", "Rename this"),
        comment(20, "lead", "Why a loop?"),
        reply(21, "lead", "Still?", 20),
    ];
    w.add_reply_route(&t.server);
    let verdicts = json!([
        {"comment_id": 10, "verdict": "valid", "fix_commit": "abc1234", "reply": "Renamed in abc1234."},
        {"comment_id": "21", "verdict": "incorrect", "fix_commit": null, "reply": "The loop reads one page."},
        {"comment_id": 30, "verdict": "question", "fix_commit": null, "reply": ""},
    ])
    .to_string();
    (t, w, verdicts)
}

fn reply_posts(t: &AdapterTest) -> Vec<crate::jira_github_support::Record> {
    t.server
        .calls(Some("POST"), None)
        .into_iter()
        .filter(|c| c.path.ends_with("/replies"))
        .collect()
}

#[test]
fn reply_two_runs_post_each_reply_once() {
    let (t, _w, verdicts) = reply_test();
    let call = || {
        t.act("review.reply")
            .args(json!({ "verdicts": verdicts }))
            .field("pr", json!(7))
            .run()
    };
    let result = call();
    assert_eq!(result.state(), "confirmed", "{result:?}");
    let posts: Vec<(String, Value)> = reply_posts(&t)
        .into_iter()
        .map(|p| (p.path, p.body))
        .collect();
    assert_eq!(
        posts,
        [
            (
                format!("{REPO_PATH}/pulls/7/comments/10/replies"),
                json!({"body": "Renamed in abc1234."})
            ),
            (
                format!("{REPO_PATH}/pulls/7/comments/20/replies"),
                json!({"body": "The loop reads one page."})
            ),
        ],
        "a reply to a reply goes to the root of its thread"
    );
    let again = call();
    assert_eq!(again.state(), "confirmed", "{again:?}");
    assert!(has(&again, "skipped [10, 21]"), "{again:?}");
    assert_eq!(reply_posts(&t).len(), 2);
}

#[test]
fn reply_the_reconcile_only_reads() {
    let (t, _w, verdicts) = reply_test();
    let call = |reconcile: bool| {
        let c = t
            .act("review.reply")
            .args(json!({ "verdicts": verdicts }))
            .field("pr", json!(7));
        if reconcile {
            c.reconcile().run()
        } else {
            c.run()
        }
    };
    assert_eq!(call(true).state(), "failed");
    call(false);
    assert_eq!(call(true).state(), "confirmed");
    assert_eq!(reply_posts(&t).len(), 2);
}

#[test]
fn reply_a_missing_comment_is_failed_after_the_others() {
    let (t, _w, _v) = reply_test();
    let verdicts = json!([{"comment_id": 10, "verdict": "valid", "reply": "Done."},
                          {"comment_id": 99, "verdict": "valid", "reply": "Done too."}])
    .to_string();
    let result = t
        .act("review.reply")
        .args(json!({ "verdicts": verdicts }))
        .field("pr", json!(7))
        .run();
    assert_eq!(result.state(), "failed");
    assert!(has(&result, "no comment [99]"));
}

#[test]
fn reply_own_logins_is_needed() {
    let (t, _w, verdicts) = reply_test();
    let result = t
        .act("review.reply")
        .args(json!({ "verdicts": verdicts }))
        .field("pr", json!(7))
        .config(t.config(json!({"own_logins": null})))
        .run();
    assert_eq!(result.state(), "failed");
    assert!(has(&result, "own_logins"));
}

#[test]
fn reply_verdicts_in_a_code_fence_and_bad_rows() {
    let (t, _w, verdicts) = reply_test();
    let fenced = format!("```json\n{verdicts}\n```");
    let result = t
        .act("review.reply")
        .args(json!({ "verdicts": fenced }))
        .field("pr", json!(7))
        .run();
    assert_eq!(result.state(), "confirmed", "{result:?}");
    let bad = json!([{"verdict": "valid", "reply": "x"}]).to_string();
    let result = t
        .act("review.reply")
        .args(json!({ "verdicts": bad }))
        .field("pr", json!(7))
        .run();
    assert_eq!(result.state(), "failed");
    assert!(has(&result, "comment_id"));
}

fn kinds_test() -> (AdapterTest, GitHubWorld, String) {
    let t = AdapterTest::new();
    let w = GitHubWorld::new(&t.server, Some(pull(false)));
    {
        let mut s = w.lock();
        s.comments = vec![comment(10, "lead", "Rename this")];
        s.reviews = vec![review_with(
            40,
            "lead",
            "CHANGES_REQUESTED",
            "Please rename the export.\nAnd keep CSV.",
        )];
        s.issue_comments = vec![issue_comment(
            60,
            "other",
            "Add a note <!-- vision-reply review 40 --> to the README.",
        )];
    }
    w.add_reply_route(&t.server);
    let verdicts = json!([
        {"comment_id": 10, "kind": "inline", "verdict": "valid", "reply": "Renamed."},
        {"comment_id": 40, "kind": "review", "verdict": "valid", "reply": "Renamed, and CSV stays."},
        {"comment_id": 60, "verdict": "out-of-scope", "reply": "That belongs to a later ticket."},
    ])
    .to_string();
    (t, w, verdicts)
}

fn kind_posts(t: &AdapterTest) -> Vec<crate::jira_github_support::Record> {
    t.server
        .calls(Some("POST"), None)
        .into_iter()
        .filter(|c| c.path.ends_with("/replies") || c.path.ends_with("/issues/7/comments"))
        .collect()
}

#[test]
fn reply_kinds_each_kind_gets_its_reply_once() {
    let (t, _w, verdicts) = kinds_test();
    let issue_comments = format!("{REPO_PATH}/issues/7/comments");
    let call = || {
        t.act("review.reply")
            .args(json!({ "verdicts": verdicts }))
            .field("pr", json!(7))
            .run()
    };
    let result = call();
    assert_eq!(result.state(), "confirmed", "{result:?}");
    let posts = kind_posts(&t);
    let paths: Vec<&str> = posts.iter().map(|p| p.path.as_str()).collect();
    assert_eq!(
        paths,
        [
            format!("{REPO_PATH}/pulls/7/comments/10/replies").as_str(),
            issue_comments.as_str(),
            issue_comments.as_str()
        ]
    );
    assert_eq!(posts[0].body, json!({"body": "Renamed."}));
    assert_eq!(
        posts[1].body["body"],
        "> Please rename the export.\n\nRenamed, and CSV stays.\n\n<!-- vision-reply review 40 -->"
    );
    assert_eq!(
        posts[2].body["body"],
        "> Add a note  vision-reply review 40  to the README.\n\nThat belongs to a later ticket.\n\n<!-- vision-reply conversation 60 -->",
        "a quote never carries the mark of a different comment"
    );
    let again = call();
    assert_eq!(again.state(), "confirmed", "{again:?}");
    assert!(has(&again, "skipped [10, 40, 60]"), "{again:?}");
    assert_eq!(kind_posts(&t).len(), 3);
    // The rows of the job comments agree: nothing waits now.
    let result = t
        .job("comments")
        .field("pr", json!(7))
        .secrets(crate::jira_github_support::all_secrets())
        .run();
    let rows: Value =
        serde_json::from_str(result.json["documents"][0]["content"].as_str().unwrap()).unwrap();
    assert_eq!(rows, json!([]));
}

#[test]
fn reply_kinds_a_quote_calls_nobody() {
    let (t, w, _v) = kinds_test();
    w.lock().issue_comments = vec![issue_comment(
        61,
        "other",
        "@lead and @acme/team: is a@b.c right?",
    )];
    let verdicts =
        json!([{"comment_id": 61, "kind": "conversation", "verdict": "question", "reply": "Yes."}])
            .to_string();
    let result = t
        .act("review.reply")
        .args(json!({ "verdicts": verdicts }))
        .field("pr", json!(7))
        .run();
    assert_eq!(result.state(), "confirmed", "{result:?}");
    let body = kind_posts(&t)[0].body["body"].as_str().unwrap().to_string();
    assert!(
        body.starts_with("> @\u{200b}lead and @\u{200b}acme/team: is a@\u{200b}b.c right?\n\nYes."),
        "{body}"
    );
    assert!(
        !body.contains("@lead"),
        "a mention in a quote would notify the person a second time"
    );
}

#[test]
fn reply_kinds_the_reconcile_only_reads() {
    let (t, _w, verdicts) = kinds_test();
    let call = |reconcile: bool| {
        let c = t
            .act("review.reply")
            .args(json!({ "verdicts": verdicts }))
            .field("pr", json!(7));
        if reconcile {
            c.reconcile().run()
        } else {
            c.run()
        }
    };
    let result = call(true);
    assert_eq!(result.state(), "failed");
    assert!(has(&result, "[10, 40, 60]"), "{result:?}");
    call(false);
    let result = call(true);
    assert_eq!(result.state(), "confirmed", "{result:?}");
    assert_eq!(kind_posts(&t).len(), 3);
}

#[test]
fn reply_kinds_an_id_of_two_kinds_needs_its_kind() {
    let (t, w, _v) = kinds_test();
    w.lock().issue_comments.push(issue_comment(
        10,
        "other",
        "The same id, in the conversation.",
    ));
    let verdicts = json!([{"comment_id": 10, "verdict": "valid", "reply": "Renamed."}]).to_string();
    let result = t
        .act("review.reply")
        .args(json!({ "verdicts": verdicts }))
        .field("pr", json!(7))
        .run();
    assert_eq!(result.state(), "failed", "{result:?}");
    assert!(has(&result, "name comments of more than one kind"));
    assert!(kind_posts(&t).is_empty());
    let verdicts =
        json!([{"comment_id": 10, "kind": "conversation", "verdict": "valid", "reply": "Done."}])
            .to_string();
    let result = t
        .act("review.reply")
        .args(json!({ "verdicts": verdicts }))
        .field("pr", json!(7))
        .run();
    assert_eq!(result.state(), "confirmed", "{result:?}");
    let paths: Vec<String> = kind_posts(&t).into_iter().map(|p| p.path).collect();
    assert_eq!(paths, [format!("{REPO_PATH}/issues/7/comments")]);
}

// ci.rerun --------------------------------------------------------------------------------------

fn rerun_path() -> String {
    format!("{REPO_PATH}/actions/runs/41/rerun-failed-jobs")
}

fn rerun_test() -> (AdapterTest, GitHubWorld, Arc<Mutex<bool>>) {
    let t = AdapterTest::new();
    let w = GitHubWorld::new(&t.server, Some(pull(true)));
    w.lock().runs = vec![run_ended(41, "failure")];
    let starts = Arc::new(Mutex::new(true));
    let st = Arc::clone(&w.state);
    t.server
        .route("GET", &format!("{REPO_PATH}/actions/runs/41"), move |_| {
            Reply::json(200, st.lock().unwrap().runs[0].clone())
        });
    let (st, go) = (Arc::clone(&w.state), Arc::clone(&starts));
    t.server.route("POST", &rerun_path(), move |_| {
        if *go.lock().unwrap() {
            st.lock().unwrap().runs[0] = Run::new(41)
                .status("queued")
                .conclusion(None)
                .attempt(2)
                .json();
        }
        Reply::json(201, json!({}))
    });
    (t, w, starts)
}

#[test]
fn rerun_the_failed_jobs_run_again() {
    let (t, _w, _s) = rerun_test();
    let result = t.act("ci.rerun").field("head", json!(HEAD)).run();
    assert_eq!(result.state(), "confirmed", "{result:?}");
    assert!(has(&result, "attempt 2"));
    assert_eq!(result.json["external_id"], "run-41");
    assert_eq!(t.server.count("POST", &rerun_path()), 1);
}

#[test]
fn rerun_a_green_or_running_run_needs_nothing() {
    let (t, w, _s) = rerun_test();
    for state in [Run::new(41).json(), run_open(41, "in_progress")] {
        w.lock().runs = vec![state];
        assert_eq!(
            t.act("ci.rerun").field("head", json!(HEAD)).run().state(),
            "confirmed"
        );
    }
    assert!(t.server.calls(Some("POST"), Some(&rerun_path())).is_empty());
}

#[test]
fn rerun_no_run_and_a_rerun_that_does_not_start() {
    let (t, w, starts) = rerun_test();
    w.lock().runs = vec![];
    let result = t.act("ci.rerun").field("head", json!(HEAD)).run();
    assert_eq!(result.state(), "failed");
    assert!(has(&result, "nothing runs again"));
    w.lock().runs = vec![run_ended(41, "failure")];
    *starts.lock().unwrap() = false;
    let result = t
        .act("ci.rerun")
        .field("head", json!(HEAD))
        .config(t.config(json!({"ci_find_seconds": 1})))
        .run();
    assert_eq!(result.state(), "uncertain", "{result:?}");
}

#[test]
fn rerun_the_reconcile_only_reads() {
    let (t, w, _s) = rerun_test();
    assert_eq!(
        t.act("ci.rerun")
            .field("head", json!(HEAD))
            .reconcile()
            .run()
            .state(),
        "failed"
    );
    w.lock().runs = vec![Run::new(41).conclusion(Some("failure")).attempt(2).json()];
    assert_eq!(
        t.act("ci.rerun")
            .field("head", json!(HEAD))
            .reconcile()
            .run()
            .state(),
        "confirmed"
    );
    assert!(t.server.calls(Some("POST"), Some(&rerun_path())).is_empty());
}

#[test]
fn rerun_an_uncertain_answer_names_the_attempt_and_the_reconcile_reads_it() {
    let (t, w, starts) = rerun_test();
    *starts.lock().unwrap() = false;
    let first = t
        .act("ci.rerun")
        .field("head", json!(HEAD))
        .config(t.config(json!({"ci_find_seconds": 0.5})))
        .run();
    assert_eq!(first.state(), "uncertain", "{first:?}");
    assert_eq!(first.json["external_id"], "run-41-attempt-1");
    // The reconcile reads the run of the external id, and a new attempt is the rerun.
    w.lock().runs[0] = Run::new(41).conclusion(Some("failure")).attempt(2).json();
    let runs_41 = format!("{REPO_PATH}/actions/runs/41");
    let reads = t.server.count("GET", &runs_41);
    let result = t
        .act("ci.rerun")
        .field("head", json!(HEAD))
        .field("external_id", json!("run-41-attempt-1"))
        .field("started", json!("2026-11-02T11:00:00Z"))
        .reconcile()
        .run();
    assert_eq!(result.state(), "confirmed", "{result:?}");
    assert!(has(&result, "attempt 2, and the call found attempt 1"));
    let result = t
        .act("ci.rerun")
        .field("head", json!(HEAD))
        .field("external_id", json!("run-41-attempt-2"))
        .reconcile()
        .run();
    assert_eq!(result.state(), "failed", "{result:?}");
    assert_eq!(t.server.count("GET", &runs_41), reads + 2);
    assert!(
        t.server
            .calls(Some("GET"), Some(&format!("{REPO_PATH}/actions/runs")))
            .len()
            <= 1,
        "the reconcile needs no search for the run"
    );
    assert_eq!(t.server.count("POST", &rerun_path()), 1);
}

#[test]
fn rerun_a_rerun_that_a_person_started_by_hand_before_the_call_is_not_this_operation() {
    let (t, w, _s) = rerun_test();
    // The first call ended with no answer, so the reconcile has only the time of the call.
    w.lock().runs = vec![Run::new(41)
        .conclusion(Some("failure"))
        .attempt(2)
        .started_at("2026-11-02T10:30:00Z")
        .json()];
    let result = t
        .act("ci.rerun")
        .field("head", json!(HEAD))
        .field("external_id", Value::Null)
        .field("started", json!("2026-11-02T11:00:00Z"))
        .reconcile()
        .run();
    assert_eq!(result.state(), "failed", "{result:?}");
    assert!(has(
        &result,
        "before the call at 2026-11-02T11:00:00Z: a person ran it by hand"
    ));
    w.lock().runs = vec![Run::new(41)
        .status("queued")
        .conclusion(None)
        .attempt(3)
        .started_at("2026-11-02T11:00:04Z")
        .json()];
    let result = t
        .act("ci.rerun")
        .field("head", json!(HEAD))
        .field("external_id", Value::Null)
        .field("started", json!("2026-11-02T11:00:00Z"))
        .reconcile()
        .run();
    assert_eq!(result.state(), "confirmed", "{result:?}");
    assert!(has(&result, "after the call"));
    assert!(t.server.calls(Some("POST"), Some(&rerun_path())).is_empty());
}

// The base branch -------------------------------------------------------------------------------

const BASE_TEXT: &str = "# Export the report\n\nThe body.";

#[test]
fn base_ref_gives_the_base_of_a_pull_request() {
    let (mut t, _w) = pr_test();
    t.base_ref = "origin/release/2.0".into();
    let result = t
        .act("pr.open")
        .args(json!({"text": BASE_TEXT}))
        .field("head", json!(HEAD))
        .run();
    assert_eq!(result.state(), "confirmed", "{result:?}");
    assert_eq!(
        t.server
            .calls(Some("POST"), Some(&format!("{REPO_PATH}/pulls")))[0]
            .body["base"],
        "release/2.0"
    );
}

#[test]
fn base_branch_overrides_base_ref() {
    let (t, _w) = pr_test();
    let result = t
        .act("pr.open")
        .args(json!({"text": BASE_TEXT}))
        .field("head", json!(HEAD))
        .config(t.config(json!({"base_branch": "develop"})))
        .run();
    assert_eq!(result.state(), "confirmed", "{result:?}");
    assert_eq!(
        t.server
            .calls(Some("POST"), Some(&format!("{REPO_PATH}/pulls")))[0]
            .body["base"],
        "develop"
    );
}

#[test]
fn git_push_never_pushes_the_base_branch_of_base_ref() {
    let (mut t, _w) = pr_test();
    t.base_ref = "origin/trunk".into();
    let result = t
        .act("git.push")
        .field("branch", json!("trunk"))
        .field("head", json!(HEAD))
        .run();
    assert_eq!(result.code, 2, "{result:?}");
    assert!(has(&result, "the base branch"));
    let result = t
        .act("pr.open")
        .args(json!({"text": BASE_TEXT}))
        .field("branch", json!("trunk"))
        .run();
    assert_eq!(result.code, 2, "{result:?}");
    assert!(t
        .server
        .calls(Some("POST"), Some(&format!("{REPO_PATH}/pulls")))
        .is_empty());
}

// deploy ----------------------------------------------------------------------------------------

fn merges_path() -> String {
    format!("{REPO_PATH}/merges")
}

fn deploy_test() -> (AdapterTest, Arc<Mutex<String>>, Arc<Mutex<Reply>>) {
    let t = AdapterTest::new();
    let status = Arc::new(Mutex::new("diverged".to_string()));
    let s = Arc::clone(&status);
    t.server.route(
        "GET",
        &format!("{REPO_PATH}/compare/staging...{HEAD}"),
        move |_| Reply::json(200, json!({"status": *s.lock().unwrap()})),
    );
    let merge = Arc::new(Mutex::new(Reply::json(201, json!({"sha": "c".repeat(40)}))));
    let m = Arc::clone(&merge);
    t.server
        .route("POST", &merges_path(), move |_| m.lock().unwrap().clone());
    (t, status, merge)
}

fn deploy(t: &AdapterTest, env: &str, reconcile: bool) -> Outcome {
    let config = t.config(json!({"envs": [{"name": "staging", "branch": "staging"},
                                          {"name": "prod", "branch": "main"}]}));
    let call = t
        .act("deploy")
        .args(json!({ "env": env }))
        .field("head", json!(HEAD))
        .config(config);
    if reconcile {
        call.reconcile().run()
    } else {
        call.run()
    }
}

#[test]
fn deploy_201_merges_the_head() {
    let (t, _s, _m) = deploy_test();
    let result = deploy(&t, "staging", false);
    assert_eq!(result.state(), "confirmed", "{result:?}");
    assert!(has(&result, "(201)"));
    let post = &t.server.calls(Some("POST"), Some(&merges_path()))[0];
    assert_eq!(
        post.body,
        json!({"base": "staging", "head": HEAD,
               "commit_message": format!("Merge DEMO-12 ({}) into staging", &HEAD[..12])})
    );
}

#[test]
fn deploy_204_means_the_branch_holds_the_head() {
    let (t, _s, m) = deploy_test();
    *m.lock().unwrap() = Reply::empty(204);
    let result = deploy(&t, "staging", false);
    assert_eq!(result.state(), "confirmed");
    assert!(has(&result, "(204)"));
}

#[test]
fn deploy_409_is_a_conflict() {
    let (t, _s, m) = deploy_test();
    *m.lock().unwrap() = Reply::json(409, json!({"message": "Merge conflict"}));
    let result = deploy(&t, "staging", false);
    assert_eq!(result.state(), "failed");
    assert!(has(&result, "conflict"));
}

#[test]
fn deploy_a_branch_that_holds_the_head_and_the_reconcile() {
    let (t, s, _m) = deploy_test();
    assert_eq!(deploy(&t, "staging", true).state(), "failed");
    *s.lock().unwrap() = "behind".into();
    assert_eq!(deploy(&t, "staging", false).state(), "confirmed");
    assert_eq!(deploy(&t, "staging", true).state(), "confirmed");
    assert!(t
        .server
        .calls(Some("POST"), Some(&merges_path()))
        .is_empty());
}

#[test]
fn deploy_an_unknown_env_and_the_base_branch() {
    let (t, _s, _m) = deploy_test();
    let result = deploy(&t, "qa", false);
    assert_eq!(result.state(), "failed");
    assert!(has(&result, "the env qa in `envs`"));
    let result = deploy(&t, "prod", false);
    assert_eq!(result.code, 2, "{result:?}");
    assert!(has(&result, "only into a staging branch"));
    assert!(t
        .server
        .calls(Some("POST"), Some(&merges_path()))
        .is_empty());
}

// review.rerequest ------------------------------------------------------------------------------

fn request_path() -> String {
    format!("{REPO_PATH}/pulls/7/requested_reviewers")
}

fn rerequest_test() -> (AdapterTest, GitHubWorld) {
    let t = AdapterTest::new();
    let w = GitHubWorld::new(&t.server, Some(pull(false)));
    w.lock().reviews = vec![
        review(1, "lead", "APPROVED"),
        review(2, "lead", "CHANGES_REQUESTED"),
        review(3, "outsider", "CHANGES_REQUESTED"),
    ];
    let st = Arc::clone(&w.state);
    t.server.route("POST", &request_path(), move |r| {
        let requested: Vec<String> = r.body["reviewers"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap().to_string())
            .collect();
        let pr = Pull {
            draft: false,
            requested,
            ..Pull::default()
        }
        .json();
        st.lock().unwrap().pr = Some(pr.clone());
        Reply::json(201, pr)
    });
    (t, w)
}

#[test]
fn rerequest_it_asks_each_approver_who_asked_for_changes_once() {
    let (t, _w) = rerequest_test();
    let result = t.act("review.rerequest").field("pr", json!(7)).run();
    assert_eq!(result.state(), "confirmed", "{result:?}");
    assert_eq!(
        t.server.calls(Some("POST"), Some(&request_path()))[0].body,
        json!({"reviewers": ["lead"]})
    );
    let again = t.act("review.rerequest").field("pr", json!(7)).run();
    assert_eq!(again.state(), "confirmed");
    assert_eq!(t.server.count("POST", &request_path()), 1);
}

#[test]
fn rerequest_no_approver_asked_for_changes() {
    let (t, w) = rerequest_test();
    w.lock().reviews = vec![review(1, "lead", "APPROVED")];
    let result = t.act("review.rerequest").field("pr", json!(7)).run();
    assert_eq!(result.state(), "confirmed");
    assert!(t
        .server
        .calls(Some("POST"), Some(&request_path()))
        .is_empty());
}

#[test]
fn rerequest_the_reconcile_only_reads() {
    let (t, w) = rerequest_test();
    assert_eq!(
        t.act("review.rerequest")
            .field("pr", json!(7))
            .reconcile()
            .run()
            .state(),
        "failed"
    );
    w.lock().pr = Some(
        Pull {
            draft: false,
            requested: vec!["lead".into()],
            ..Pull::default()
        }
        .json(),
    );
    assert_eq!(
        t.act("review.rerequest")
            .field("pr", json!(7))
            .reconcile()
            .run()
            .state(),
        "confirmed"
    );
    assert!(t
        .server
        .calls(Some("POST"), Some(&request_path()))
        .is_empty());
}

// Read verbs ------------------------------------------------------------------------------------

fn reads_test() -> AdapterTest {
    let t = AdapterTest::new();
    t.server.json_route(
        "GET",
        &issue_path(),
        Issue {
            description: adf_doc(&["Make it **work**"]),
            comments: vec![("Sam", "ok")],
            ..Issue::default()
        }
        .json(),
        200,
    );
    t.server.json_route(
        "POST",
        "/rest/api/3/search/jql",
        json!({"issues": [jira_issue("DEMO-3", "Other")], "isLast": true}),
        200,
    );
    let w = GitHubWorld::new(&t.server, Some(pull(false)));
    {
        let mut s = w.lock();
        s.reviews = vec![review(1, "lead", "COMMENTED")];
        s.comments = vec![comment(10, "lead", "Why?")];
        s.runs = vec![run_ended(41, "failure")];
    }
    t.server.json_route(
        "GET",
        &format!("{REPO_PATH}/actions/runs/41"),
        run_ended(41, "failure"),
        200,
    );
    t.server.json_route(
        "GET",
        &format!("{REPO_PATH}/actions/runs/41/jobs"),
        json!({"jobs": []}),
        200,
    );
    t
}

fn doc_of(result: &Outcome) -> (String, String) {
    assert_eq!(result.code, 0, "{result:?}");
    let docs = result.json["documents"].as_array().unwrap();
    assert_eq!(docs.len(), 1);
    (
        docs[0]["name"].as_str().unwrap().to_string(),
        docs[0]["content"].as_str().unwrap().to_string(),
    )
}

#[test]
fn read_jira_issue() {
    let t = reads_test();
    let (name, content) = doc_of(&t.act("jira.issue").read().run());
    assert_eq!(name, "issue-DEMO-12.json");
    let data: Value = serde_json::from_str(&content).unwrap();
    assert_eq!(
        data["issue"]["untrusted"]["description"],
        "Make it \\*\\*work\\*\\*"
    );
    assert_eq!(data["issue"]["untrusted"]["comments"][0]["text"], "ok");
    let result = t
        .act("jira.issue")
        .args(json!({"key": "DEMO-1/../x"}))
        .read()
        .run();
    assert_eq!(result.code, 1);
}

#[test]
fn read_jira_search() {
    let t = reads_test();
    let (name, content) = doc_of(
        &t.act("jira.search")
            .args(json!({"jql": "project = DEMO", "limit": 500}))
            .read()
            .run(),
    );
    assert_eq!(name, "jira-search.json");
    let data: Value = serde_json::from_str(&content).unwrap();
    assert_eq!(data["issues"]["untrusted"][0]["key"], "DEMO-3");
    let body = &t.server.calls(Some("POST"), Some("/rest/api/3/search/jql"))[0].body;
    assert_eq!(
        (&body["jql"], &body["maxResults"]),
        (&json!("project = DEMO"), &json!(50))
    );
    assert_eq!(t.act("jira.search").args(json!({})).read().run().code, 1);
}

#[test]
fn read_github_pr() {
    let t = reads_test();
    let (name, content) = doc_of(&t.act("github.pr").read().field("pr", json!(7)).run());
    assert_eq!(name, "pr-7.json");
    let data: Value = serde_json::from_str(&content).unwrap();
    assert_eq!(data["pr"]["number"], 7);
    assert_eq!(data["review_state"], "comments");
    assert_eq!(data["untrusted"]["threads"][0][0]["text"], "Why?");
}

#[test]
fn read_ci_log() {
    let t = reads_test();
    let (name, content) = doc_of(&t.act("ci.log").read().field("head", json!(HEAD)).run());
    assert_eq!(name, format!("ci-log-{}.txt", &HEAD[..12]));
    assert!(content.contains("The run 41 of ci.yml"), "{content}");
    let (name, _content) = doc_of(
        &t.act("ci.log")
            .args(json!({"run": 41}))
            .read()
            .field("head", json!(HEAD))
            .run(),
    );
    assert_eq!(name, "ci-log-41.txt");
}

#[test]
fn read_an_unknown_read_verb() {
    let t = reads_test();
    let result = t.act("jira.delete").read().run();
    assert_eq!(result.code, 1);
    assert!(result.stderr.contains("has no read verb"), "{result:?}");
}

#[test]
fn an_unknown_verb_fails() {
    let t = AdapterTest::new();
    let result = t.act("pr.merge").run();
    assert_eq!(result.code, 1);
    assert!(
        result.stderr.contains("has no verb 'pr.merge'"),
        "{result:?}"
    );
}

// review.resolve and review.minimize ------------------------------------------------------------

/// A world with the pull request #7 of the branch of the ticket, and comments of bots and of
/// people: an inline thread of a bot with a reply of the bot of Vision (10, 11), an inline
/// comment of a person (20), a thread of a bot of `bot_logins` with a reply of a person (30,
/// 31), a review of a bot (40), a conversation comment of a bot (60), and one of a person (61).
fn settle_test() -> (AdapterTest, Arc<Mutex<World>>) {
    let t = AdapterTest::new();
    let repos = Repos::new(&t.path("repos"));
    git(
        &[
            "push",
            "--quiet",
            repos.remote.to_str().unwrap(),
            &format!("{BRANCH}:refs/heads/{BRANCH}"),
        ],
        &repos.clone,
    );
    let mut world = World::new(
        &repos.remote,
        Some(secret("JIRA_TOKEN")),
        Some(secret("FORGE_TOKEN")),
        Some("bot@example.invalid"),
    );
    assert_eq!(world.open_pull(BRANCH), 7);
    let control = |world: &mut World, action: &str, body: Value| {
        let reply = world.control(action, body.as_object().unwrap());
        assert_eq!(reply.status, 201, "{action} {body}");
    };
    for (id, login, root) in [
        (10, "review-bot[bot]", None),
        (11, "vision-bot", Some(10)),
        (20, "lead", None),
        (30, "checker", None),
        (31, "lead", Some(30)),
    ] {
        let mut body = json!({"number": 7, "id": id, "login": login, "body": format!("Comment {id}"),
                              "path": "app.py", "line": 3});
        if let Some(root) = root {
            body["in_reply_to"] = json!(root);
        }
        control(&mut world, "comment", body);
    }
    control(
        &mut world,
        "review",
        json!({"number": 7, "id": 40, "login": "review-bot[bot]", "state": "COMMENTED", "body": "Two findings."}),
    );
    for (id, login) in [(60, "review-bot[bot]"), (61, "lead")] {
        control(
            &mut world,
            "issue_comment",
            json!({"number": 7, "id": id, "login": login, "body": format!("Note {id}")}),
        );
    }
    let world = Arc::new(Mutex::new(world));
    install(&t.server, Arc::clone(&world));
    (t, world)
}

/// The rows of the Shepherd: each one settles its comment but 12, which has no `settle`.
fn settle_verdicts() -> String {
    json!([
        {"comment_id": 10, "kind": "inline", "verdict": "valid", "reply": "Fixed.", "settle": true},
        {"comment_id": 20, "kind": "inline", "verdict": "valid", "reply": "Fixed.", "settle": true},
        {"comment_id": 30, "kind": "inline", "verdict": "already-done", "reply": "Done.", "settle": "outdated"},
        {"comment_id": 40, "kind": "review", "verdict": "valid", "reply": "Both fixed.", "settle": "resolved"},
        {"comment_id": 60, "kind": "conversation", "verdict": "valid", "reply": "Fixed.", "settle": true},
        {"comment_id": 61, "kind": "conversation", "verdict": "question", "reply": "Which one?", "settle": true},
        {"comment_id": 12, "verdict": "incorrect", "reply": "No."},
    ])
    .to_string()
}

/// The GraphQL mutations that the world got, oldest first.
fn mutations(world: &Arc<Mutex<World>>) -> Vec<Value> {
    world
        .lock()
        .unwrap()
        .log
        .iter()
        .filter(|e| {
            e["path"] == "/graphql"
                && e["body"]["query"]
                    .as_str()
                    .is_some_and(|q| q.starts_with("mutation"))
        })
        .cloned()
        .collect()
}

fn settle_call(t: &AdapterTest, verb: &str, reconcile: bool) -> Outcome {
    let call = t
        .act(verb)
        .args(json!({ "verdicts": settle_verdicts() }))
        .field("pr", json!(7))
        .config(t.config(json!({"bot_logins": ["Checker"]})));
    if reconcile {
        call.reconcile().run()
    } else {
        call.run()
    }
}

#[test]
fn resolve_resolves_only_the_threads_of_bots() {
    let (t, world) = settle_test();
    let result = settle_call(&t, "review.resolve", false);
    assert_eq!(result.state(), "confirmed", "{result:?}");
    let detail = result.detail();
    assert!(
        detail.starts_with(
            "on #7: resolved the threads of [10], and skipped [], which are resolved already"
        ),
        "{detail}"
    );
    assert!(
        detail.contains("Refused, because only a thread or a comment of a bot is touched: the thread of 20: its author lead is not a bot; the thread of 30: lead wrote in it and is not a bot"),
        "a reply of a person keeps the thread of a bot open: {detail}"
    );
    assert_eq!(
        world.lock().unwrap().resolved,
        BTreeSet::from([10]),
        "the reply of the bot of Vision in its thread does not stop it"
    );
    let calls = mutations(&world);
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0]["body"]["variables"], json!({"id": "PRRT_10"}));
    assert_eq!(calls[0]["auth"], "forge");
    // A second run resolves nothing again.
    let again = settle_call(&t, "review.resolve", false);
    assert_eq!(again.state(), "confirmed", "{again:?}");
    assert!(again.detail().contains("skipped [10]"), "{again:?}");
    assert_eq!(mutations(&world).len(), 1);
}

#[test]
fn resolve_the_reconcile_only_reads() {
    let (t, world) = settle_test();
    let before = settle_call(&t, "review.resolve", true);
    assert_eq!(before.state(), "failed", "{before:?}");
    assert!(
        before
            .detail()
            .contains("open threads of the comments [10]"),
        "{before:?}"
    );
    assert!(mutations(&world).is_empty(), "a reconcile only reads");
    settle_call(&t, "review.resolve", false);
    let after = settle_call(&t, "review.resolve", true);
    assert_eq!(after.state(), "confirmed", "{after:?}");
    assert!(
        after
            .detail()
            .starts_with("each settled bot thread on #7 is resolved"),
        "{after:?}"
    );
    assert_eq!(mutations(&world).len(), 1);
}

#[test]
fn resolve_a_missing_inline_comment_is_failed_after_the_others() {
    let (t, world) = settle_test();
    let verdicts = json!([
        {"comment_id": 10, "kind": "inline", "verdict": "valid", "settle": true},
        {"comment_id": 99, "kind": "inline", "verdict": "valid", "settle": true},
        {"comment_id": 98, "verdict": "valid", "settle": true},
    ])
    .to_string();
    let result = t
        .act("review.resolve")
        .args(json!({ "verdicts": verdicts }))
        .field("pr", json!(7))
        .run();
    assert_eq!(result.state(), "failed", "{result:?}");
    assert!(
        result.detail().contains("resolved the threads of [10]")
            && result
                .detail()
                .contains("has no review thread of the comments [99]"),
        "a row with no kind and no inline comment is left out: {result:?}"
    );
    assert_eq!(world.lock().unwrap().resolved, BTreeSet::from([10]));
}

#[test]
fn minimize_hides_only_the_comments_of_bots_with_their_classifier() {
    let (t, world) = settle_test();
    let result = settle_call(&t, "review.minimize", false);
    assert_eq!(result.state(), "confirmed", "{result:?}");
    let detail = result.detail();
    assert!(
        detail.starts_with(
            "on #7: hid the comments [10, 30, 40, 60], and skipped [], which are hidden already"
        ),
        "{detail}"
    );
    assert!(
        detail.contains("Refused, because only a thread or a comment of a bot is touched: the inline 20: its author lead is not a bot; the conversation 61: its author lead is not a bot"),
        "{detail}"
    );
    let hidden = world.lock().unwrap().minimized.clone();
    assert_eq!(
        hidden,
        BTreeMap::from([
            ("PRRC_10".to_string(), "resolved".to_string()),
            ("PRRC_30".to_string(), "outdated".to_string()),
            ("PRR_40".to_string(), "resolved".to_string()),
            ("IC_60".to_string(), "resolved".to_string()),
        ]),
        "a login of bot_logins counts as a bot, in any case"
    );
    assert!(mutations(&world).iter().all(|m| m["auth"] == "forge"));
    let again = settle_call(&t, "review.minimize", false);
    assert_eq!(again.state(), "confirmed", "{again:?}");
    assert!(
        again.detail().contains("skipped [10, 30, 40, 60]"),
        "{again:?}"
    );
    assert_eq!(mutations(&world).len(), 4);
}

#[test]
fn minimize_the_reconcile_only_reads_and_a_bot_needs_its_name() {
    let (t, world) = settle_test();
    let before = settle_call(&t, "review.minimize", true);
    assert_eq!(before.state(), "failed", "{before:?}");
    assert!(
        before
            .detail()
            .contains("still shows the comments [10, 30, 40, 60]"),
        "{before:?}"
    );
    assert!(mutations(&world).is_empty());
    settle_call(&t, "review.minimize", false);
    let after = settle_call(&t, "review.minimize", true);
    assert_eq!(after.state(), "confirmed", "{after:?}");
    // With no bot_logins, the login checker is a person.
    let (t, world) = settle_test();
    let result = t
        .act("review.minimize")
        .args(json!({ "verdicts": settle_verdicts() }))
        .field("pr", json!(7))
        .run();
    assert_eq!(result.state(), "confirmed", "{result:?}");
    assert!(
        result
            .detail()
            .contains("the inline 30: its author checker is not a bot"),
        "{result:?}"
    );
    assert!(!world.lock().unwrap().minimized.contains_key("PRRC_30"));
}

#[test]
fn settle_rows_and_missing_comments_are_named() {
    let (t, _world) = settle_test();
    let bad = json!([{"comment_id": 10, "settle": "later"}]).to_string();
    for verb in ["review.resolve", "review.minimize"] {
        let result = t
            .act(verb)
            .args(json!({ "verdicts": bad }))
            .field("pr", json!(7))
            .run();
        assert_eq!(result.state(), "failed", "{verb}: {result:?}");
        assert!(
            result
                .detail()
                .contains("row 1 of verdicts.json has settle 'later': use true"),
            "{verb}: {result:?}"
        );
        let none = t.act(verb).field("pr", json!(7)).run();
        assert_eq!(none.state(), "failed");
        assert!(
            none.detail()
                .contains(&format!("{verb} needs args.verdicts")),
            "{none:?}"
        );
    }
    let missing = json!([
        {"comment_id": 60, "settle": true},
        {"comment_id": 99, "settle": true},
    ])
    .to_string();
    let result = t
        .act("review.minimize")
        .args(json!({ "verdicts": missing }))
        .field("pr", json!(7))
        .run();
    assert_eq!(result.state(), "failed", "{result:?}");
    assert!(
        result.detail().contains("hid the comments [60]")
            && result.detail().contains("has no comment [99]"),
        "{result:?}"
    );
}
