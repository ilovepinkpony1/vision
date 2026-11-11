//! The fake Jira and GitHub with state of the test of the whole path
//! (tests/e2e_jira_github.rs): its checks of the tokens, its CI, and its control calls.

use std::sync::{Arc, Mutex};

use serde_json::{json, Value};

use crate::jira_github_support::server::call;
use crate::jira_github_support::world::{install, World};
use crate::jira_github_support::{git, FakeServer};

const JIRA: &str = "jira-world-token-5a6b7c8d";
const FORGE: &str = "forge-world-token-1e2f3a4b";
const USER: &str = "bot@example.invalid";

struct WorldTest {
    _tmp: tempfile::TempDir,
    remote: std::path::PathBuf,
    server: FakeServer,
    world: Arc<Mutex<World>>,
}

impl WorldTest {
    fn new() -> WorldTest {
        let tmp = tempfile::tempdir().unwrap();
        let remote = tmp.path().join("remote.git");
        let work = tmp.path().join("work");
        std::fs::create_dir_all(&work).unwrap();
        git(
            &["init", "--bare", "--quiet", &remote.display().to_string()],
            tmp.path(),
        );
        git(&["init", "--quiet"], &work);
        std::fs::write(work.join("ci.sh"), "echo checking; test ! -f TODO.txt\n").unwrap();
        git(&["add", "-A"], &work);
        git(&["commit", "--quiet", "-m", "start"], &work);
        let remote_text = remote.display().to_string();
        git(
            &["push", "--quiet", &remote_text, "HEAD:refs/heads/main"],
            &work,
        );
        std::fs::write(work.join("TODO.txt"), "x\n").unwrap();
        git(&["add", "-A"], &work);
        git(&["commit", "--quiet", "-m", "todo"], &work);
        git(
            &[
                "push",
                "--quiet",
                &remote_text,
                "HEAD:refs/heads/vision/DEMO-12",
            ],
            &work,
        );
        let mut world = World::new(&remote, Some(JIRA), Some(FORGE), Some(USER));
        world.add_issue("DEMO-12", "Add subtract", "The calculator can only add.");
        let world = Arc::new(Mutex::new(world));
        let server = FakeServer::start();
        install(&server, Arc::clone(&world));
        WorldTest {
            _tmp: tmp,
            remote,
            server,
            world,
        }
    }

    fn call(
        &self,
        method: &str,
        path: &str,
        body: Option<Value>,
        auth: Option<&str>,
    ) -> (u16, Value) {
        let headers: Vec<(&str, &str)> = auth.map(|a| ("Authorization", a)).into_iter().collect();
        call(self.server.port, method, path, body.as_ref(), &headers)
    }

    fn jira(&self, method: &str, path: &str, body: Option<Value>) -> (u16, Value) {
        let auth = format!(
            "Basic {}",
            vision::util::base64::encode(format!("{USER}:{JIRA}").as_bytes())
        );
        self.call(method, &format!("/rest/api/3{path}"), body, Some(&auth))
    }

    fn gh(&self, method: &str, path: &str, body: Option<Value>) -> (u16, Value) {
        let auth = format!("Bearer {FORGE}");
        self.call(
            method,
            &format!("/repos/acme/widgets{path}"),
            body,
            Some(&auth),
        )
    }
}

#[test]
fn the_world_checks_tokens_and_keeps_state() {
    let w = WorldTest::new();
    assert_eq!(
        w.call("GET", "/rest/api/3/issue/DEMO-12", None, None).0,
        401
    );
    assert_eq!(
        w.call(
            "GET",
            "/repos/acme/widgets/branches",
            None,
            Some("Bearer x")
        )
        .0,
        401
    );
    let (status, issue) = w.jira(
        "GET",
        "/issue/DEMO-12?fields=summary,status,description",
        None,
    );
    assert_eq!(status, 200);
    assert_eq!(issue["fields"]["status"], json!({"name": "To Do"}));
    assert_eq!(
        issue["fields"]["description"]["content"][0]["content"][0]["text"],
        "The calculator can only add."
    );
    assert_eq!(
        w.jira(
            "POST",
            "/issue/DEMO-12/transitions",
            Some(json!({"transition": {"id": "11"}}))
        )
        .0,
        204
    );
    assert_eq!(
        w.jira("GET", "/issue/DEMO-12?fields=status", None).1["fields"]["status"],
        json!({"name": "In Progress"})
    );
    let (_, branches) = w.gh("GET", "/branches", None);
    let mut names: Vec<&str> = branches
        .as_array()
        .unwrap()
        .iter()
        .map(|b| b["name"].as_str().unwrap())
        .collect();
    names.sort();
    assert_eq!(names, ["main", "vision/DEMO-12"]);
    let (status, pull) = w.gh(
        "POST",
        "/pulls",
        Some(json!({"title": "T", "head": "vision/DEMO-12", "base": "main", "draft": true})),
    );
    assert_eq!(
        (status, &pull["number"], &pull["draft"]),
        (201, &json!(7), &json!(true))
    );
    // A dispatch runs ci.sh of the head: TODO.txt makes it red.
    assert_eq!(
        w.gh(
            "POST",
            "/actions/workflows/ci.yml/dispatches",
            Some(json!({"ref": "vision/DEMO-12"}))
        )
        .0,
        204
    );
    // A run takes its time: queued, in progress, and then ended.
    let head = pull["head"]["sha"].as_str().unwrap().to_string();
    let mut seen = Vec::new();
    let mut runs = Value::Null;
    for _ in 0..3 {
        runs = w
            .gh("GET", &format!("/actions/runs?head_sha={head}"), None)
            .1["workflow_runs"]
            .clone();
        let run = &runs[0];
        seen.push((
            run["status"].clone(),
            run["conclusion"].clone(),
            run["run_attempt"].clone(),
        ));
    }
    assert_eq!(
        seen,
        [
            (json!("queued"), Value::Null, json!(1)),
            (json!("in_progress"), Value::Null, json!(1)),
            (json!("completed"), json!("failure"), json!(1)),
        ]
    );
    let id = runs[0]["id"].as_i64().unwrap();
    assert_eq!(
        w.gh(
            "POST",
            &format!("/actions/runs/{id}/rerun-failed-jobs"),
            None
        )
        .0,
        201
    );
    assert_eq!(
        w.gh("GET", &format!("/actions/runs/{id}"), None).1["run_attempt"],
        2
    );
    assert_eq!(
        w.gh(
            "POST",
            &format!("/actions/runs/{id}/rerun-failed-jobs"),
            None
        )
        .0,
        403,
        "a run that runs cannot run again"
    );
    w.gh("GET", &format!("/actions/runs/{id}"), None);
    let jobs = w.gh("GET", &format!("/actions/runs/{id}/jobs"), None).1["jobs"].clone();
    let conclusions: Vec<&Value> = jobs
        .as_array()
        .unwrap()
        .iter()
        .map(|j| &j["conclusion"])
        .collect();
    assert_eq!(conclusions, [&json!("failure")]);
    // The team: ready, a review, and the merge.
    assert_eq!(
        w.call("POST", "/_control/ready", Some(json!({"number": 7})), None)
            .1["draft"],
        false
    );
    w.call(
        "POST",
        "/_control/review",
        Some(json!({"number": 7, "login": "lead", "state": "APPROVED", "id": 601})),
        None,
    );
    assert_eq!(w.gh("GET", "/pulls/7/reviews", None).1[0]["id"], 601);
    let merged = w
        .call("POST", "/_control/merge", Some(json!({"number": 7})), None)
        .1;
    assert_eq!(merged["merged"], true);
    assert!(git(&["log", "-1", "--format=%s", "main"], &w.remote).contains("Merge pull request #7"));
    // The log has each call of the adapter, with no header value, and no control call.
    let log = w.world.lock().unwrap().log.clone();
    let text = serde_json::to_string(&log).unwrap();
    assert!(!text.contains(JIRA));
    assert!(!text.contains(FORGE));
    let auths: Vec<&Value> = log.iter().take(2).map(|e| &e["auth"]).collect();
    assert_eq!(auths, [&json!("none"), &json!("bad")]);
    assert!(!log
        .iter()
        .any(|e| e["path"].as_str().unwrap().starts_with("/_control/")));
    // The state call gives the state of the team for a person.
    let (status, state) = w.call("GET", "/_control/state", None, None);
    assert_eq!(status, 200);
    assert_eq!(state["pulls"]["7"]["merged"], true);
    assert_eq!(state["issues"]["DEMO-12"]["status"], "In Progress");
}
