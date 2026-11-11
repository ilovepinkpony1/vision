//! observe: the queue, and one ticket with each review state and each CI state.

use std::time::Duration;

use serde_json::{json, Value};

use crate::fixtures::{
    adf_doc, comment, issue_comment, jira_issue, pull, reply, review, review_with, run, run_ended,
    run_open, GitHubWorld, Issue, Pull, Run, HEAD, OTHER, REPO_PATH,
};
use crate::jira_github_support::{
    secret, secrets_without, AdapterTest, Outcome, Reply, BRANCH, KEY,
};

const SEARCH: &str = "/rest/api/3/search/jql";

fn queue_test() -> AdapterTest {
    let t = AdapterTest::new();
    t.server.queue(
        "POST",
        SEARCH,
        vec![
            Reply::json(
                200,
                json!({"issues": [jira_issue("DEMO-1", "Free one"), jira_issue("DEMO-2", "Has a branch")],
                       "nextPageToken": "page-2"}),
            ),
            Reply::json(
                200,
                json!({"issues": [jira_issue("DEMO-3", "Has a pull request")], "isLast": true}),
            ),
        ],
    );
    t.server.json_route(
        "GET",
        &format!("{REPO_PATH}/branches"),
        json!([{"name": "main"}, {"name": "feature/demo-2-export"}, {"name": "vision/DEMO-12"}]),
        200,
    );
    t.server.json_route(
        "GET",
        &format!("{REPO_PATH}/pulls"),
        json!([{"number": 4, "title": "DEMO-3: add the export", "head": {"ref": "export"}}]),
        200,
    );
    t
}

#[test]
fn queue_eligible_and_not_eligible_issues() {
    let t = queue_test();
    let result = t.observe(json!({"what": "queue"}));
    assert_eq!(result.code, 0, "{result:?}");
    let tickets = result.json["tickets"].as_array().unwrap();
    let keys: Vec<&str> = tickets.iter().map(|t| t["key"].as_str().unwrap()).collect();
    assert_eq!(keys, ["DEMO-1", "DEMO-2", "DEMO-3"]);
    assert_eq!(
        tickets[0],
        json!({"key": "DEMO-1", "title": "Free one", "status": "To Do", "eligible": true, "reason": ""})
    );
    assert_eq!(tickets[1]["eligible"], false);
    assert!(tickets[1]["reason"]
        .as_str()
        .unwrap()
        .contains("the branch feature/demo-2-export already has DEMO-2"));
    assert_eq!(tickets[2]["eligible"], false);
    assert!(tickets[2]["reason"]
        .as_str()
        .unwrap()
        .contains("the pull request #4 already has DEMO-3"));
    // vision/DEMO-12 does not hold DEMO-1: a key matches only as a whole.
    let searches = t.server.calls(Some("POST"), Some(SEARCH));
    assert_eq!(searches.len(), 2);
    assert_eq!(
        searches[0].body["jql"],
        "project = DEMO AND status = \"To Do\""
    );
    assert_eq!(searches[0].body["fields"], json!(["summary", "status"]));
    assert!(searches[0].body.get("nextPageToken").is_none());
    assert_eq!(searches[1].body["nextPageToken"], "page-2");
    let auth = &searches[0].headers["authorization"];
    assert!(auth.starts_with("Basic "), "{auth}");
    let gh = &t
        .server
        .calls(Some("GET"), Some(&format!("{REPO_PATH}/branches")))[0]
        .headers;
    assert_eq!(
        gh["authorization"],
        format!("Bearer {}", secret("FORGE_TOKEN"))
    );
    assert_eq!(gh["accept"], "application/vnd.github+json");
    assert_eq!(gh["x-github-api-version"], "2022-11-28");
    assert_eq!(
        t.server
            .calls(Some("GET"), Some(&format!("{REPO_PATH}/pulls")))[0]
            .query["state"],
        "open"
    );
}

#[test]
fn queue_a_missing_key_and_a_missing_secret_are_named() {
    let t = queue_test();
    let result =
        t.observe(json!({"what": "queue", "config": t.config(json!({"queue_jql": null}))}));
    assert_eq!(result.code, 1);
    assert!(result.stderr.contains("queue_jql"), "{result:?}");
    let result = t.observe_with(json!({"what": "queue"}), &secrets_without(&["JIRA_TOKEN"]));
    assert_eq!(result.code, 1);
    assert!(result.stderr.contains("JIRA_TOKEN"), "{result:?}");
    assert!(
        t.server.calls(None, None).is_empty(),
        "nothing is called without the secret"
    );
}

#[test]
fn queue_a_token_never_travels_in_clear_text() {
    let t = queue_test();
    let result = t.observe(json!({"what": "queue",
        "config": t.config(json!({"jira_url": "http://example.atlassian.net"}))}));
    assert_eq!(result.code, 1);
    assert!(result.stderr.contains("jira_url"), "{result:?}");
    assert!(result.stderr.contains("https://"), "{result:?}");
}

fn ticket_test() -> (AdapterTest, GitHubWorld) {
    let t = AdapterTest::new();
    let description = json!({"type": "doc", "version": 1, "content": [
        {"type": "heading", "attrs": {"level": 2}, "content": [{"type": "text", "text": "Goal"}]},
        {"type": "paragraph", "content": [{"type": "text", "text": "Export "},
                                          {"type": "text", "text": "fast", "marks": [{"type": "strong"}]}]},
    ]});
    let issue = Issue {
        description,
        status: "In Progress",
        comments: vec![("Sam", "Please keep CSV")],
        parent: Some("DEMO-1"),
        ..Issue::default()
    }
    .json();
    t.server
        .json_route("GET", &format!("/rest/api/3/issue/{KEY}"), issue, 200);
    t.server.json_route(
        "GET",
        "/rest/api/3/issue/DEMO-1",
        json!({"key": "DEMO-1", "fields": {"summary": "Reports", "description": adf_doc(&["The epic"])}}),
        200,
    );
    let world = GitHubWorld::new(&t.server, Some(pull(true)));
    (t, world)
}

fn observe_ticket(t: &AdapterTest, pr: Value, config: Value) -> Value {
    let result = t.observe(
        json!({"what": "ticket", "key": KEY, "branch": BRANCH, "pr": pr,
                                  "clone": null, "config": t.config(config)}),
    );
    assert_eq!(result.code, 0, "{result:?}");
    result.json
}

fn state_of(t: &AdapterTest) -> String {
    observe_ticket(t, json!(7), json!({}))["review"]["state"]
        .as_str()
        .unwrap()
        .to_string()
}

#[test]
fn ticket_the_issue_and_the_shape_of_the_answer() {
    let (t, _w) = ticket_test();
    let facts = observe_ticket(&t, json!(7), json!({}));
    let keys: Vec<&String> = facts.as_object().unwrap().keys().collect();
    assert_eq!(
        keys,
        ["ci", "envs", "issue", "pr", "review", "tracker_status"]
    );
    let issue = &facts["issue"];
    assert_eq!(issue["key"], KEY);
    assert_eq!(issue["title"], "Export the report");
    assert_eq!(issue["description"], "## Goal\n\nExport **fast**");
    assert_eq!(issue["status"], "In Progress");
    assert_eq!(
        issue["parent"],
        json!({"key": "DEMO-1", "title": "Reports", "description": "The epic"})
    );
    assert_eq!(issue["comments"][0]["author"], "Sam");
    assert_eq!(issue["comments"][0]["text"], "Please keep CSV");
    assert_eq!(facts["tracker_status"], "In Progress");
    let pr = &facts["pr"];
    for key in [
        "number",
        "draft",
        "head",
        "merged",
        "closed",
        "conflict",
        "approved_by",
        "changes_requested",
        "comments_waiting",
    ] {
        assert!(pr.get(key).is_some(), "{key}");
    }
    assert_eq!(
        (&pr["number"], &pr["draft"], &pr["head"]),
        (&json!(7), &json!(true), &json!(HEAD))
    );
    assert_eq!(facts["ci"], json!({"head": HEAD, "state": "none"}));
    assert_eq!(facts["envs"], json!({}));
    let fields = t
        .server
        .calls(Some("GET"), Some(&format!("/rest/api/3/issue/{KEY}")))[0]
        .query["fields"]
        .clone();
    assert!(fields.split(',').any(|f| f == "description"), "{fields}");
}

#[test]
fn ticket_each_review_state() {
    let (t, w) = ticket_test();
    // Run each case in order, changing the world between them.
    {
        let mut s = w.lock();
        s.pr = Some(pull(false));
        s.reviews = vec![];
        s.comments = vec![];
    }
    assert_eq!(state_of(&t), "waiting");
    w.lock().reviews = vec![review(1, "outsider", "APPROVED")];
    assert_eq!(state_of(&t), "waiting", "only approvers count");
    w.lock().reviews = vec![review(1, "lead", "APPROVED")];
    let facts = observe_ticket(&t, json!(7), json!({}));
    assert_eq!(facts["review"]["state"], "approved");
    assert_eq!(facts["pr"]["approved_by"], json!(["lead"]));
    w.lock()
        .reviews
        .push(review(2, "lead", "CHANGES_REQUESTED"));
    let facts = observe_ticket(&t, json!(7), json!({}));
    assert_eq!(facts["review"]["state"], "changes");
    assert_eq!(facts["pr"]["changes_requested"], true);
    assert_eq!(facts["pr"]["approved_by"], json!([]));
    w.lock().pr = Some(
        Pull {
            draft: false,
            requested: vec!["lead".into()],
            ..Pull::default()
        }
        .json(),
    );
    assert_eq!(state_of(&t), "waiting", "the review is asked again");
    w.lock().comments = vec![
        comment(10, "lead", "Rename this"),
        reply(11, "vision-bot", "Done", 10),
        comment(12, "lead", "And this?"),
    ];
    let facts = observe_ticket(&t, json!(7), json!({}));
    assert_eq!(facts["review"]["state"], "comments");
    assert_eq!(
        facts["pr"]["comments_waiting"], 1,
        "the answered thread does not wait"
    );
    w.lock().pr = Some(
        Pull {
            draft: false,
            state: "closed".into(),
            merged: true,
            ..Pull::default()
        }
        .json(),
    );
    assert_eq!(state_of(&t), "merged");
    w.lock().pr = Some(
        Pull {
            draft: false,
            state: "closed".into(),
            ..Pull::default()
        }
        .json(),
    );
    let facts = observe_ticket(&t, json!(7), json!({}));
    assert_eq!(facts["review"]["state"], "closed");
    assert_eq!(facts["pr"]["closed"], true);
    {
        let mut s = w.lock();
        s.pr = Some(
            Pull {
                draft: false,
                mergeable_state: "dirty".into(),
                ..Pull::default()
            }
            .json(),
        );
        s.comments = vec![];
    }
    let facts = observe_ticket(&t, json!(7), json!({}));
    assert_eq!(facts["pr"]["conflict"], true);
    assert_eq!(
        facts["review"]["state"], "changes",
        "the changes of lead come before the conflict"
    );
}

#[test]
fn ticket_a_conflict_with_the_base_is_its_own_state_in_its_order() {
    let (t, w) = ticket_test();
    let dirty = |state: &str, merged: bool| {
        Pull {
            draft: false,
            mergeable_state: "dirty".into(),
            state: state.into(),
            merged,
            ..Pull::default()
        }
        .json()
    };
    // merged, closed, comments, changes, conflict, approved, waiting
    {
        let mut s = w.lock();
        s.pr = Some(dirty("open", false));
        s.reviews = vec![];
        s.comments = vec![];
    }
    let facts = observe_ticket(&t, json!(7), json!({}));
    assert_eq!(facts["pr"]["conflict"], true);
    assert_eq!(facts["review"]["state"], "conflict");
    w.lock().reviews = vec![review(1, "lead", "APPROVED")];
    assert_eq!(state_of(&t), "conflict", "a conflict comes before approved");
    w.lock()
        .reviews
        .push(review(2, "lead", "CHANGES_REQUESTED"));
    assert_eq!(state_of(&t), "changes", "changes come before a conflict");
    {
        let mut s = w.lock();
        s.reviews = vec![review(1, "lead", "APPROVED")];
        s.comments = vec![comment(10, "lead", "Rename this")];
    }
    assert_eq!(
        state_of(&t),
        "comments",
        "a comment comes before a conflict"
    );
    {
        let mut s = w.lock();
        s.comments = vec![];
        s.pr = Some(
            Pull {
                draft: false,
                mergeable: Some(json!(false)),
                ..Pull::default()
            }
            .json(),
        );
    }
    assert_eq!(state_of(&t), "conflict", "mergeable false is a conflict");
    w.lock().pr = Some(dirty("closed", true));
    assert_eq!(state_of(&t), "merged");
    w.lock().pr = Some(dirty("closed", false));
    assert_eq!(state_of(&t), "closed");
    // GitHub has not computed it yet: no conflict.
    w.lock().pr = Some(
        Pull {
            draft: false,
            mergeable: Some(Value::Null),
            mergeable_state: "unknown".into(),
            ..Pull::default()
        }
        .json(),
    );
    let facts = observe_ticket(&t, json!(7), json!({}));
    assert_eq!(facts["pr"]["conflict"], false);
    assert_eq!(facts["review"]["state"], "approved");
}

#[test]
fn ticket_each_kind_of_comment_waits_and_the_state_agrees() {
    let (t, w) = ticket_test();
    {
        let mut s = w.lock();
        s.pr = Some(pull(false));
        s.reviews = vec![review(1, "lead", "APPROVED")];
        s.comments = vec![];
    }
    assert_eq!(state_of(&t), "approved");
    w.lock().issue_comments = vec![issue_comment(60, "lead", "One more thing: the README.")];
    let facts = observe_ticket(&t, json!(7), json!({}));
    assert_eq!(
        facts["review"]["state"], "comments",
        "a conversation comment waits"
    );
    assert_eq!(facts["pr"]["comments_waiting"], 1);
    w.lock()
        .reviews
        .push(review_with(2, "other", "COMMENTED", "Why CSV?"));
    assert_eq!(
        observe_ticket(&t, json!(7), json!({}))["pr"]["comments_waiting"],
        2,
        "a review with a body waits"
    );
    w.lock().issue_comments.extend([
        issue_comment(
            61,
            "vision-bot",
            "> One more thing: the README.\n\nDone.\n\n<!-- vision-reply conversation 60 -->",
        ),
        issue_comment(62, "person", "> Why CSV?\n\nThe ticket asks for it."),
    ]);
    let facts = observe_ticket(&t, json!(7), json!({}));
    assert_eq!(
        facts["pr"]["comments_waiting"], 0,
        "a mark or a quote answers each one"
    );
    assert_eq!(facts["review"]["state"], "approved");
}

#[test]
fn ticket_a_comment_of_an_ignored_login_does_not_change_the_state() {
    let (t, w) = ticket_test();
    {
        let mut s = w.lock();
        s.pr = Some(pull(false));
        s.reviews = vec![review(1, "lead", "APPROVED")];
        s.comments = vec![];
        s.issue_comments = vec![issue_comment(60, "ci-bot", "Deployed the preview.")];
    }
    assert_eq!(state_of(&t), "comments");
    let facts = observe_ticket(&t, json!(7), json!({"ignore_logins": ["ci-bot"]}));
    assert_eq!(facts["pr"]["comments_waiting"], 0);
    assert_eq!(facts["review"]["state"], "approved");
    // An entry with a kind: the status comment never waits, and a review of the bot still does.
    let facts = observe_ticket(
        &t,
        json!(7),
        json!({"ignore_logins": ["ci-bot:conversation"]}),
    );
    assert_eq!(facts["review"]["state"], "approved");
    w.lock().reviews.push(review_with(
        2,
        "ci-bot",
        "COMMENTED",
        "A null check is missing.",
    ));
    let facts = observe_ticket(
        &t,
        json!(7),
        json!({"ignore_logins": ["ci-bot:conversation"]}),
    );
    assert_eq!(facts["pr"]["comments_waiting"], 1);
    assert_eq!(facts["review"]["state"], "comments");
    let result = t.observe(
        json!({"what": "ticket", "key": KEY, "branch": BRANCH, "pr": 7,
        "config": t.config(json!({"ignore_logins": ["ci-bot:other"]}))}),
    );
    assert_eq!(result.code, 1);
    assert!(
        result.stderr.contains("inline, review, or conversation"),
        "{result:?}"
    );
}

#[test]
fn ticket_any_reviewer_counts_when_approvers_is_empty() {
    let (t, w) = ticket_test();
    w.lock().reviews = vec![review(1, "someone", "APPROVED")];
    let facts = observe_ticket(&t, json!(7), json!({"approvers": []}));
    assert_eq!(facts["review"]["state"], "approved");
    w.lock().reviews = vec![review(1, "person", "APPROVED")];
    assert_eq!(
        observe_ticket(&t, json!(7), json!({"approvers": []}))["review"]["state"],
        "waiting",
        "an own login never counts"
    );
}

#[test]
fn ticket_each_ci_state() {
    let (t, w) = ticket_test();
    let cases: Vec<(Vec<Value>, &str)> = vec![
        (vec![], "none"),
        (vec![run_open(1, "in_progress")], "running"),
        (vec![run_open(1, "queued")], "running"),
        (vec![run(1)], "green"),
        (vec![run_ended(1, "failure")], "red"),
        (vec![run_ended(1, "cancelled")], "red"),
        (vec![run_ended(1, "failure"), run(2)], "green"),
        (
            vec![
                run(1),
                Run::new(2)
                    .path(".github/workflows/lint.yml")
                    .conclusion(Some("failure"))
                    .json(),
            ],
            "green",
        ),
        (
            vec![Run::new(1).sha(OTHER).conclusion(Some("failure")).json()],
            "none",
        ),
    ];
    for (runs, state) in cases {
        w.lock().runs = runs.clone();
        let ci = observe_ticket(&t, json!(7), json!({}))["ci"].clone();
        assert_eq!(ci["state"], state, "{runs:?}");
        assert_eq!(ci["head"], HEAD);
    }
    let runs = t
        .server
        .calls(Some("GET"), Some(&format!("{REPO_PATH}/actions/runs")));
    assert_eq!(runs.last().unwrap().query["head_sha"], HEAD);
}

#[test]
fn ticket_no_pull_request_uses_the_branch() {
    let (t, w) = ticket_test();
    {
        let mut s = w.lock();
        s.pr = None;
        s.runs = vec![run(1)];
    }
    let facts = observe_ticket(&t, Value::Null, json!({}));
    assert_eq!(facts["pr"], Value::Null);
    assert_eq!(facts["review"]["state"], "none");
    assert_eq!(facts["ci"]["state"], "green");
    w.lock().branch_sha = None;
    let facts = observe_ticket(&t, Value::Null, json!({}));
    assert_eq!(facts["ci"], json!({"head": null, "state": "none"}));
}

#[test]
fn ticket_each_env_tells_whether_it_holds_the_head() {
    let (t, _w) = ticket_test();
    let s = &t.server;
    s.json_route(
        "GET",
        &format!("{REPO_PATH}/git/ref/heads/staging"),
        json!({"object": {"sha": OTHER}}),
        200,
    );
    s.json_route(
        "GET",
        &format!("{REPO_PATH}/git/ref/heads/demo"),
        json!({"object": {"sha": HEAD}}),
        200,
    );
    s.json_route(
        "GET",
        &format!("{REPO_PATH}/compare/staging...{HEAD}"),
        json!({"status": "ahead"}),
        200,
    );
    s.json_route(
        "GET",
        &format!("{REPO_PATH}/compare/demo...{HEAD}"),
        json!({"status": "identical"}),
        200,
    );
    let envs = json!([{"name": "staging", "branch": "staging"}, {"name": "demo", "branch": "demo"},
                      {"name": "gone", "branch": "gone"}]);
    let facts = observe_ticket(&t, json!(7), json!({ "envs": envs }));
    assert_eq!(
        facts["envs"],
        json!({
            "staging": {"branch": "staging", "head": OTHER, "holds": false},
            "demo": {"branch": "demo", "head": HEAD, "holds": true},
            "gone": {"branch": "gone", "head": null, "holds": false},
        })
    );
}

#[test]
fn ticket_the_deployment_of_an_env_tells_whether_the_env_runs_the_head() {
    let (t, _w) = ticket_test();
    let s = &t.server;
    let deployments = format!("{REPO_PATH}/deployments");
    for branch in ["staging", "demo"] {
        s.json_route(
            "GET",
            &format!("{REPO_PATH}/git/ref/heads/{branch}"),
            json!({"object": {"sha": HEAD}}),
            200,
        );
        s.json_route(
            "GET",
            &format!("{REPO_PATH}/compare/{branch}...{HEAD}"),
            json!({"status": "identical"}),
            200,
        );
    }
    let envs = json!([{"name": "staging", "branch": "staging", "deployment": "staging-env"},
                      {"name": "demo", "branch": "demo"}]);
    let config = json!({ "envs": envs });

    // No deployment of the environment yet.
    s.json_route("GET", &deployments, json!([]), 200);
    let facts = observe_ticket(&t, json!(7), config.clone())["envs"].clone();
    assert_eq!(
        facts["staging"],
        json!({"branch": "staging", "head": HEAD, "holds": true, "deployed": false,
               "deployment_sha": null, "deployed_at": null})
    );
    assert_eq!(
        facts["demo"],
        json!({"branch": "demo", "head": HEAD, "holds": true}),
        "an env with no deployment key gets no new fact"
    );
    let calls = s.calls(Some("GET"), Some(&deployments));
    assert_eq!(calls.len(), 1, "one call for the one env with a deployment");
    assert_eq!(
        serde_json::to_value(&calls[0].query).unwrap(),
        json!({"environment": "staging-env", "per_page": "1"})
    );
    assert_eq!(
        calls[0].raw_path, deployments,
        "the query is not part of the path"
    );

    // The newest deployment runs an older commit: the env does not run the head yet.
    s.json_route(
        "GET",
        &deployments,
        json!([{"id": 3, "sha": OTHER, "environment": "staging-env", "created_at": "2026-11-02T10:00:00Z"}]),
        200,
    );
    s.json_route(
        "GET",
        &format!("{REPO_PATH}/compare/{OTHER}...{HEAD}"),
        json!({"status": "ahead"}),
        200,
    );
    let staging = observe_ticket(&t, json!(7), config.clone())["envs"]["staging"].clone();
    assert_eq!(
        (
            &staging["deployed"],
            &staging["deployment_sha"],
            &staging["deployed_at"]
        ),
        (&json!(false), &json!(OTHER), &json!("2026-11-02T10:00:00Z"))
    );

    // A newer commit that holds the head, and then the head itself, with no compare.
    let newer = "c".repeat(40);
    s.json_route(
        "GET",
        &deployments,
        json!([{"id": 4, "sha": newer, "environment": "staging-env", "created_at": "2026-11-03T10:00:00Z"}]),
        200,
    );
    s.json_route(
        "GET",
        &format!("{REPO_PATH}/compare/{newer}...{HEAD}"),
        json!({"status": "behind"}),
        200,
    );
    let staging = observe_ticket(&t, json!(7), config.clone())["envs"]["staging"].clone();
    assert_eq!(
        (&staging["deployed"], &staging["deployment_sha"]),
        (&json!(true), &json!(newer))
    );
    s.json_route(
        "GET",
        &deployments,
        json!([{"id": 5, "sha": HEAD, "environment": "staging-env", "created_at": "2026-11-04T10:00:00Z"}]),
        200,
    );
    let same = format!("{REPO_PATH}/compare/{HEAD}...{HEAD}");
    let compares = s.count("GET", &same);
    let staging = observe_ticket(&t, json!(7), config.clone())["envs"]["staging"].clone();
    assert_eq!(
        (&staging["deployed"], &staging["deployed_at"]),
        (&json!(true), &json!("2026-11-04T10:00:00Z"))
    );
    assert_eq!(s.count("GET", &same), compares);
    assert!(
        !s.calls(None, None)
            .iter()
            .any(|c| c.path.contains("/statuses")),
        "the statuses of a deployment are not read"
    );
    assert_eq!(s.count("GET", &deployments), 4, "one call for each look");

    // A bad deployment key is named.
    let result = t.observe(json!({"what": "ticket", "key": KEY, "branch": BRANCH, "pr": 7,
        "config": t.config(json!({"envs": [{"name": "staging", "branch": "staging", "deployment": 3}]}))}));
    assert_eq!(result.code, 1);
    assert!(
        result.stderr.contains("`deployment` of the env staging"),
        "{result:?}"
    );
}

#[test]
fn ticket_an_http_error_names_the_call_and_the_status_never_a_header() {
    let (t, _w) = ticket_test();
    t.server.json_route(
        "GET",
        &format!("/rest/api/3/issue/{KEY}"),
        json!({"errorMessages": ["Issue does not exist or you do not have permission to see it."]}),
        404,
    );
    let result = t.observe(json!({"what": "ticket", "key": KEY, "branch": BRANCH, "pr": 7}));
    assert_eq!(result.code, 1);
    assert!(
        result.stderr.contains(
            "jira: read the issue DEMO-12 (GET /rest/api/3/issue/DEMO-12) answered HTTP 404"
        ),
        "{result:?}"
    );
    assert!(result.stderr.contains("Issue does not exist"), "{result:?}");
    assert!(!result.stderr.contains("Authorization"));
    assert!(!result.stderr.contains("Basic"));
}

#[test]
fn ticket_each_http_call_has_a_time_limit() {
    let (t, _w) = ticket_test();
    t.server
        .route("GET", &format!("/rest/api/3/issue/{KEY}"), |_| {
            Reply::json(200, json!({})).delayed(Duration::from_secs(3))
        });
    let result = t.observe(
        json!({"what": "ticket", "key": KEY, "branch": BRANCH, "pr": 7,
                                  "config": t.config(json!({"http_timeout": 1}))}),
    );
    assert_eq!(result.code, 1);
    assert!(
        result.stderr.contains("read the issue DEMO-12"),
        "{result:?}"
    );
    assert!(result.stderr.contains("in 1 s"), "{result:?}");
}

#[test]
fn ticket_a_bad_key_never_reaches_a_request() {
    let (t, _w) = ticket_test();
    let result: Outcome =
        t.observe(json!({"what": "ticket", "key": "DEMO-1/../x", "branch": BRANCH}));
    assert_eq!(result.code, 1);
    assert!(t.server.calls(None, None).is_empty());
}
