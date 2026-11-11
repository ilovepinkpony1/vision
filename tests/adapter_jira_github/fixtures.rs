//! Canned objects of Jira and GitHub, and a small stateful world on the fake server.

use std::sync::{Arc, Mutex, MutexGuard};

use serde_json::{json, Value};

use crate::jira_github_support::{FakeServer, Record, Reply, BRANCH, KEY};

pub const HEAD: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
pub const OTHER: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
pub const REPO_PATH: &str = "/repos/acme/widgets";

pub fn adf_doc(paragraphs: &[&str]) -> Value {
    let content: Vec<Value> = paragraphs
        .iter()
        .map(|p| json!({"type": "paragraph", "content": [{"type": "text", "text": p}]}))
        .collect();
    json!({"type": "doc", "version": 1, "content": content})
}

/// An issue of Jira, as its REST answer gives it.
pub struct Issue<'a> {
    pub key: &'a str,
    pub summary: &'a str,
    pub status: &'a str,
    pub description: Value,
    pub comments: Vec<(&'a str, &'a str)>,
    pub parent: Option<&'a str>,
}

impl Default for Issue<'_> {
    fn default() -> Self {
        Issue {
            key: KEY,
            summary: "Export the report",
            status: "To Do",
            description: Value::Null,
            comments: Vec::new(),
            parent: None,
        }
    }
}

impl Issue<'_> {
    pub fn json(&self) -> Value {
        let comments: Vec<Value> = self
            .comments
            .iter()
            .enumerate()
            .map(|(n, (author, text))| {
                json!({"id": (10 + n).to_string(), "author": {"displayName": author},
                       "created": "2026-11-01T10:00:00.000+0000", "body": adf_doc(&[text])})
            })
            .collect();
        let mut fields = json!({
            "summary": self.summary,
            "status": {"name": self.status},
            "description": self.description,
            "issuetype": {"name": "Task"},
            "priority": {"name": "Medium"},
            "labels": ["reports"],
            "assignee": null,
            "comment": {"comments": comments, "total": self.comments.len()},
        });
        if let Some(parent) = self.parent {
            fields["parent"] = json!({"key": parent, "fields": {"summary": "The parent"}});
        }
        json!({"key": self.key, "fields": fields})
    }
}

/// `jira_issue(key, summary)` of the first version of the tests.
pub fn jira_issue(key: &str, summary: &str) -> Value {
    Issue {
        key,
        summary,
        ..Issue::default()
    }
    .json()
}

/// A pull request of GitHub.
#[derive(Clone)]
pub struct Pull {
    pub number: i64,
    pub draft: bool,
    pub sha: String,
    pub state: String,
    pub merged: bool,
    pub mergeable_state: String,
    pub requested: Vec<String>,
    /// `None`: true unless the state is dirty. `Some(value)`: that value.
    pub mergeable: Option<Value>,
}

impl Default for Pull {
    fn default() -> Self {
        Pull {
            number: 7,
            draft: true,
            sha: HEAD.into(),
            state: "open".into(),
            merged: false,
            mergeable_state: "clean".into(),
            requested: Vec::new(),
            mergeable: None,
        }
    }
}

impl Pull {
    pub fn json(&self) -> Value {
        let mergeable = self
            .mergeable
            .clone()
            .unwrap_or_else(|| json!(self.mergeable_state != "dirty"));
        let requested: Vec<Value> = self.requested.iter().map(|r| json!({"login": r})).collect();
        json!({
            "number": self.number,
            "state": self.state,
            "draft": self.draft,
            "merged": self.merged,
            "merged_at": if self.merged { json!("2026-11-02T10:00:00Z") } else { Value::Null },
            "mergeable": mergeable,
            "mergeable_state": self.mergeable_state,
            "head": {"ref": BRANCH, "sha": self.sha},
            "base": {"ref": "main"},
            "requested_reviewers": requested,
            "html_url": format!("https://github.example/acme/widgets/pull/{}", self.number),
            "title": "Export the report",
        })
    }
}

/// The default pull request #7, ready or a draft.
pub fn pull(draft: bool) -> Value {
    Pull {
        draft,
        ..Pull::default()
    }
    .json()
}

pub fn review(id: i64, login: &str, state: &str) -> Value {
    json!({"id": id, "user": {"login": login}, "state": state})
}

pub fn review_with(id: i64, login: &str, state: &str, body: &str) -> Value {
    let mut item = review(id, login, state);
    item["body"] = json!(body);
    item
}

pub fn comment(id: i64, login: &str, body: &str) -> Value {
    comment_at(id, login, body, None, "app.py", Some(3))
}

pub fn reply(id: i64, login: &str, body: &str, reply_to: i64) -> Value {
    comment_at(id, login, body, Some(reply_to), "app.py", Some(3))
}

pub fn comment_at(
    id: i64,
    login: &str,
    body: &str,
    reply_to: Option<i64>,
    path: &str,
    line: Option<i64>,
) -> Value {
    let mut item = json!({"id": id, "user": {"login": login}, "body": body, "path": path,
                          "line": line, "original_line": line,
                          "created_at": format!("2026-11-02T10:00:{:02}Z", id % 60)});
    if let Some(root) = reply_to {
        item["in_reply_to_id"] = json!(root);
    }
    item
}

pub fn issue_comment(id: i64, login: &str, body: &str) -> Value {
    json!({"id": id, "user": {"login": login}, "body": body,
           "created_at": format!("2026-11-02T11:00:{:02}Z", id % 60)})
}

/// A workflow run.
#[derive(Clone)]
pub struct Run {
    pub id: i64,
    pub sha: String,
    pub status: String,
    pub conclusion: Option<String>,
    pub path: String,
    pub attempt: i64,
    pub started_at: Option<String>,
}

impl Run {
    pub fn new(id: i64) -> Run {
        Run {
            id,
            sha: HEAD.into(),
            status: "completed".into(),
            conclusion: Some("success".into()),
            path: ".github/workflows/ci.yml".into(),
            attempt: 1,
            started_at: None,
        }
    }

    pub fn conclusion(mut self, conclusion: Option<&str>) -> Run {
        self.conclusion = conclusion.map(str::to_string);
        self
    }

    pub fn status(mut self, status: &str) -> Run {
        self.status = status.into();
        self
    }

    pub fn sha(mut self, sha: &str) -> Run {
        self.sha = sha.into();
        self
    }

    pub fn path(mut self, path: &str) -> Run {
        self.path = path.into();
        self
    }

    pub fn attempt(mut self, attempt: i64) -> Run {
        self.attempt = attempt;
        self
    }

    pub fn started_at(mut self, at: &str) -> Run {
        self.started_at = Some(at.into());
        self
    }

    pub fn json(&self) -> Value {
        let mut item = json!({"id": self.id, "head_sha": self.sha, "status": self.status,
                              "conclusion": self.conclusion, "path": self.path,
                              "created_at": format!("2026-11-02T10:{:02}:00Z", self.id % 60),
                              "run_attempt": self.attempt,
                              "html_url": format!("https://github.example/acme/widgets/actions/runs/{}", self.id)});
        if let Some(at) = &self.started_at {
            item["run_started_at"] = json!(at);
        }
        item
    }
}

/// A run that ended with success, as `run(id)` of the first version of the tests.
pub fn run(id: i64) -> Value {
    Run::new(id).json()
}

/// A run that ended with this conclusion.
pub fn run_ended(id: i64, conclusion: &str) -> Value {
    Run::new(id).conclusion(Some(conclusion)).json()
}

/// A run that has not ended.
pub fn run_open(id: i64, status: &str) -> Value {
    Run::new(id).status(status).conclusion(None).json()
}

/// The state of the pull request #7 of the branch, with its reviews, comments, and runs.
pub struct GhState {
    pub pr: Option<Value>,
    pub reviews: Vec<Value>,
    pub comments: Vec<Value>,
    pub issue_comments: Vec<Value>,
    pub runs: Vec<Value>,
    pub branch_sha: Option<String>,
    pub next_id: i64,
}

/// The pull request #7 of the branch, with its reviews, comments, and runs, as state that the
/// fake server serves and that the POST routes change.
#[derive(Clone)]
pub struct GitHubWorld {
    pub state: Arc<Mutex<GhState>>,
}

fn page(items: &[Value], req: &Record) -> Value {
    let per: usize = req
        .query
        .get("per_page")
        .and_then(|p| p.parse().ok())
        .unwrap_or(30);
    let page: usize = req
        .query
        .get("page")
        .and_then(|p| p.parse().ok())
        .unwrap_or(1);
    let start = (page.saturating_sub(1) * per).min(items.len());
    let end = (page * per).min(items.len());
    Value::Array(items[start..end].to_vec())
}

impl GitHubWorld {
    pub fn new(server: &FakeServer, pr: Option<Value>) -> GitHubWorld {
        let world = GitHubWorld {
            state: Arc::new(Mutex::new(GhState {
                pr,
                reviews: Vec::new(),
                comments: Vec::new(),
                issue_comments: Vec::new(),
                runs: Vec::new(),
                branch_sha: Some(HEAD.into()),
                next_id: 900,
            })),
        };
        world.install(server);
        world
    }

    pub fn lock(&self) -> MutexGuard<'_, GhState> {
        self.state.lock().unwrap()
    }

    fn install(&self, s: &FakeServer) {
        let st = Arc::clone(&self.state);
        s.route("GET", &format!("{REPO_PATH}/pulls"), move |r| {
            let st = st.lock().unwrap();
            let Some(pr) = &st.pr else {
                return Reply::json(200, json!([]));
            };
            if r.query.get("head").map(String::as_str) != Some(&format!("acme:{BRANCH}")) {
                return Reply::json(200, json!([]));
            }
            let state = r
                .query
                .get("state")
                .cloned()
                .unwrap_or_else(|| "open".into());
            if state != "all" && pr["state"] != state.as_str() {
                return Reply::json(200, json!([]));
            }
            Reply::json(200, page(std::slice::from_ref(pr), r))
        });
        let st = Arc::clone(&self.state);
        s.route("GET", &format!("{REPO_PATH}/pulls/7"), move |_| {
            match &st.lock().unwrap().pr {
                Some(pr) => Reply::json(200, pr.clone()),
                None => Reply::json(404, json!({"message": "Not Found"})),
            }
        });
        let st = Arc::clone(&self.state);
        s.route("GET", &format!("{REPO_PATH}/pulls/7/reviews"), move |r| {
            Reply::json(200, page(&st.lock().unwrap().reviews, r))
        });
        let st = Arc::clone(&self.state);
        s.route("GET", &format!("{REPO_PATH}/pulls/7/comments"), move |r| {
            Reply::json(200, page(&st.lock().unwrap().comments, r))
        });
        let st = Arc::clone(&self.state);
        s.route("GET", &format!("{REPO_PATH}/issues/7/comments"), move |r| {
            Reply::json(200, page(&st.lock().unwrap().issue_comments, r))
        });
        let st = Arc::clone(&self.state);
        s.route(
            "POST",
            &format!("{REPO_PATH}/issues/7/comments"),
            move |r| {
                let mut st = st.lock().unwrap();
                st.next_id += 1;
                let id = st.next_id;
                let item = issue_comment(id, "vision-bot", r.body["body"].as_str().unwrap_or(""));
                st.issue_comments.push(item.clone());
                Reply::json(201, item)
            },
        );
        let st = Arc::clone(&self.state);
        s.route("GET", &format!("{REPO_PATH}/actions/runs"), move |r| {
            let st = st.lock().unwrap();
            let sha = r.query.get("head_sha").cloned().unwrap_or_default();
            let runs: Vec<Value> = st
                .runs
                .iter()
                .filter(|run| run["head_sha"] == sha.as_str())
                .cloned()
                .collect();
            Reply::json(
                200,
                json!({"total_count": 0, "workflow_runs": page(&runs, r)}),
            )
        });
        let st = Arc::clone(&self.state);
        s.route(
            "GET",
            &format!("{REPO_PATH}/git/ref/heads/{BRANCH}"),
            move |_| match &st.lock().unwrap().branch_sha {
                Some(sha) => Reply::json(
                    200,
                    json!({"ref": format!("refs/heads/{BRANCH}"), "object": {"sha": sha, "type": "commit"}}),
                ),
                None => Reply::json(404, json!({"message": "Not Found"})),
            },
        );
    }

    /// A reply route for each root comment of the moment, as `add_reply_route` of the first
    /// version of the tests.
    pub fn add_reply_route(&self, server: &FakeServer) {
        let roots: Vec<i64> = self
            .lock()
            .comments
            .iter()
            .filter(|c| c.get("in_reply_to_id").is_none())
            .filter_map(|c| c["id"].as_i64())
            .collect();
        for root in roots {
            let st = Arc::clone(&self.state);
            server.route(
                "POST",
                &format!("{REPO_PATH}/pulls/7/comments/{root}/replies"),
                move |r| {
                    let root: i64 = r.path.rsplit('/').nth(1).unwrap().parse().unwrap();
                    let mut st = st.lock().unwrap();
                    st.next_id += 1;
                    let id = st.next_id;
                    let item = reply(
                        id,
                        "vision-bot",
                        r.body["body"].as_str().unwrap_or(""),
                        root,
                    );
                    st.comments.push(item.clone());
                    Reply::json(201, item)
                },
            );
        }
    }
}
