//! The script runner of the executor with a built-in adapter: it starts
//! `vision adapter-script <name> <observe|act|job>` with the contract of a script of `bin/`
//! (the same input, environment, exit codes, masking, and time limit).

use std::path::PathBuf;
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use vision::executor::{Answer, Builtin, Runner, Script, ScriptError, Secrets};

use crate::fixtures::{comment_at, jira_issue, pull, GitHubWorld, HEAD, REPO_PATH};
use crate::jira_github_support::{
    all_secrets, secret, vision_bin, AdapterTest, Reply, BASE_REF, BRANCH, KEY,
};

const SEARCH: &str = "/rest/api/3/search/jql";
const LONG: Duration = Duration::from_secs(120);

fn builtin(name: &str, exe: Result<PathBuf, String>) -> Option<Builtin> {
    Some(Builtin {
        name: name.into(),
        exe,
    })
}

fn runner<'a>(t: &AdapterTest, secrets: &'a Secrets) -> Runner<'a> {
    Runner {
        scripts: t.tmp.path().to_path_buf(),
        builtin: builtin("jira-github", Ok(vision_bin())),
        config: t.config(json!({})),
        base_ref: BASE_REF.into(),
        secrets,
    }
}

fn queue(t: &AdapterTest, summary: &str) {
    t.server.json_route(
        "POST",
        SEARCH,
        json!({"issues": [jira_issue("DEMO-1", summary)], "isLast": true}),
        200,
    );
    t.server
        .json_route("GET", &format!("{REPO_PATH}/branches"), json!([]), 200);
    t.server
        .json_route("GET", &format!("{REPO_PATH}/pulls"), json!([]), 200);
}

#[test]
fn the_runner_starts_the_binary_and_masks_each_secret_of_the_answer() {
    let t = AdapterTest::new();
    // A title that holds a secret: the answer never carries the value.
    queue(&t, &format!("Rotate {}", secret("JIRA_TOKEN")));
    let secrets = Secrets::from_values(all_secrets());
    let runner = runner(&t, &secrets);
    assert!(runner.has(Script::Observe));
    assert_eq!(runner.path(Script::Act), vision_bin());
    let answer = runner.observe(json!({"what": "queue"})).unwrap();
    let Answer::Done(map) = answer else {
        panic!("observe is done: {answer:?}")
    };
    let tickets = map["tickets"].as_array().unwrap();
    assert_eq!(tickets.len(), 1);
    assert_eq!(tickets[0]["key"], "DEMO-1");
    assert_eq!(tickets[0]["eligible"], true);
    let text = Value::Object(map.clone()).to_string();
    for (_, value) in all_secrets() {
        assert!(!text.contains(&value), "a secret is in the answer");
    }
    // The call carried the token of Jira, and the input of the runner: config and base_ref.
    let search = &t.server.calls(Some("POST"), Some(SEARCH))[0];
    assert!(search.headers["authorization"].starts_with("Basic "));
    assert_eq!(search.body["jql"], "project = DEMO AND status = \"To Do\"");
}

#[test]
fn a_refusal_is_exit_code_two_and_a_failure_names_the_script() {
    let t = AdapterTest::new();
    let secrets = Secrets::from_values(all_secrets());
    let runner = runner(&t, &secrets);
    let refused = runner
        .act(
            json!({"verb": "git.push", "op": "op-test", "key": KEY, "branch": "feature/other",
                   "args": {}, "head": HEAD, "clone": t.tmp.path().display().to_string()}),
            LONG,
        )
        .unwrap();
    let Answer::Refused(map) = refused else {
        panic!("git.push of another branch is refused: {refused:?}")
    };
    assert!(
        map["detail"]
            .as_str()
            .unwrap()
            .contains("does not hold the key DEMO-12"),
        "{map:?}"
    );
    // A job that reads GitHub, with no secret: exit code 1, and the error names the secret.
    let w = GitHubWorld::new(&t.server, Some(pull(false)));
    w.lock().comments = vec![comment_at(
        20,
        "lead",
        "Why a loop?",
        None,
        "lib/x.py",
        Some(9),
    )];
    let job = json!({"job": "comments", "key": KEY, "branch": BRANCH, "args": {}, "pr": 7});
    let err = runner.job(job.clone(), LONG, false).unwrap_err();
    match &err {
        ScriptError::Exit { script, code, tail } => {
            assert_eq!((script.as_str(), *code), ("bin/job", 1));
            assert!(tail.contains("FORGE_TOKEN"), "{tail}");
        }
        other => panic!("exit code 1: {other:?}"),
    }
    assert!(err.may_have_acted());
    // The same job with the secrets of the key `secrets = true`.
    let Answer::Done(map) = runner.job(job, LONG, true).unwrap() else {
        panic!("the job is done")
    };
    let rows: Value =
        serde_json::from_str(map["documents"][0]["content"].as_str().unwrap()).unwrap();
    assert_eq!(rows[0]["id"], 20);
}

#[test]
fn the_time_limit_kills_the_built_in_adapter() {
    let t = AdapterTest::new();
    queue(&t, "Slow");
    t.server.route("POST", SEARCH, |_| {
        Reply::json(200, json!({"issues": [], "isLast": true})).delayed(Duration::from_secs(3))
    });
    let secrets = Secrets::from_values(all_secrets());
    let runner = runner(&t, &secrets);
    let started = Instant::now();
    let err = runner
        .call(
            Script::Observe,
            json!({"what": "queue"}),
            Duration::from_secs(1),
            true,
        )
        .unwrap_err();
    assert!(
        matches!(&err, ScriptError::Timeout { script, secs: 1, .. } if script == "bin/observe"),
        "{err:?}"
    );
    assert!(started.elapsed() < Duration::from_secs(3), "{err}");
}

#[test]
fn a_missing_binary_and_an_unknown_name() {
    let t = AdapterTest::new();
    let secrets = Secrets::from_values(Vec::new());
    let mut runner = runner(&t, &secrets);
    runner.builtin = builtin(
        "jira-github",
        Err("this program is not the vision binary".into()),
    );
    assert!(!runner.has(Script::Observe));
    let err = runner.observe(json!({"what": "queue"})).unwrap_err();
    assert_eq!(
        err,
        ScriptError::Start {
            path: "the built-in adapter jira-github".into(),
            why: "this program is not the vision binary".into(),
        }
    );
    assert!(!err.may_have_acted());
    let missing = t.path("no-such-vision");
    runner.builtin = builtin("jira-github", Ok(missing.clone()));
    assert!(!runner.has(Script::Observe));
    assert_eq!(
        runner.observe(json!({"what": "queue"})).unwrap_err(),
        ScriptError::Missing {
            path: missing.display().to_string()
        }
    );
    // The loader accepts only a known name. The binary itself refuses another one.
    runner.builtin = builtin("jira-gitlab", Ok(vision_bin()));
    match runner.observe(json!({"what": "queue"})).unwrap_err() {
        ScriptError::Exit { code, tail, .. } => {
            assert_eq!(code, 1);
            assert!(tail.contains("no built-in adapter `jira-gitlab`"), "{tail}");
        }
        other => panic!("exit code 1: {other:?}"),
    }
}
