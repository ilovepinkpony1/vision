//! A fake Jira and GitHub with state, for the whole path of a ticket.
//!
//! The world serves the calls of the adapter on a [`FakeServer`] and keeps the state that the
//! path needs:
//!
//! - Jira: issues with an ADF description, a status, and transitions; the search of the queue.
//! - GitHub: the branches of a local bare repo, pull requests, reviews, review comments with
//!   their replies, conversation comments, requested reviewers, and workflow runs with jobs and
//!   logs. Each comment and each review has a node id. The GraphQL calls give the review
//!   threads with their state, resolve a thread, hide a comment, and tell which comments are
//!   hidden. A run runs `ci.sh` of the commit in a temporary folder: its exit code is the
//!   conclusion and its output is the log of its one job. Each attempt of a run shows `queued`
//!   at its first read and `in_progress` at its second, and it has ended from its third read on,
//!   as a real run takes its time.
//!
//! A person or a test plays the team with the control calls (`POST /_control/<action>`, a JSON
//! body): ready, review, comment (with `in_reply_to` for a reply in a thread), issue_comment,
//! conflict, merge, and `GET /_control/state`.
//! They are not calls of the adapter, so the request log leaves them out.
//!
//! Each call of the adapter must carry the token of its service. The world checks it, and the
//! request log records only which token came (`jira`, `forge`, `none`, or `bad`), never a value.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};

use serde_json::{json, Map, Value};

use super::server::{FakeServer, Record, Reply};

pub const REPO: &str = "acme/widgets";
pub const OWNER: &str = "acme";
pub const REPO_PATH: &str = "/repos/acme/widgets";
pub const WORKFLOW: &str = "ci.yml";
const LOG_KEEP: usize = 20000;

fn transitions() -> Vec<Value> {
    vec![
        json!({"id": "11", "name": "Start", "to": {"name": "In Progress"}}),
        json!({"id": "21", "name": "Ask for a review", "to": {"name": "In Review"}}),
        json!({"id": "31", "name": "Finish", "to": {"name": "Done"}}),
        json!({"id": "41", "name": "Stop", "to": {"name": "To Do"}}),
    ]
}

fn now() -> String {
    vision::util::time::now_rfc3339()
}

/// A plain text as an ADF document: one paragraph for each block of lines.
pub fn adf(text: &str) -> Value {
    let blocks: Vec<Value> = text
        .split("\n\n")
        .map(str::trim)
        .filter(|b| !b.is_empty())
        .map(|b| json!({"type": "paragraph", "content": [{"type": "text", "text": b}]}))
        .collect();
    json!({"type": "doc", "version": 1, "content": blocks})
}

fn plain_env() -> Vec<(String, String)> {
    let mut env: Vec<(String, String)> = ["PATH", "HOME", "LANG", "TMPDIR"]
        .iter()
        .filter_map(|k| std::env::var(k).ok().map(|v| (k.to_string(), v)))
        .collect();
    for (k, v) in [
        ("GIT_CONFIG_NOSYSTEM", "1"),
        ("GIT_CONFIG_GLOBAL", "/dev/null"),
        ("GIT_TERMINAL_PROMPT", "0"),
    ] {
        env.push((k.into(), v.into()));
    }
    env
}

/// git as the team runs it. Gives (success, standard output).
fn git(args: &[&str], cwd: &Path) -> (bool, String) {
    let output = Command::new("git")
        .args([
            "-c",
            "core.hooksPath=/dev/null",
            "-c",
            "commit.gpgsign=false",
            "-c",
            "user.name=team",
            "-c",
            "user.email=team@example.invalid",
        ])
        .args(args)
        .current_dir(cwd)
        .env_clear()
        .envs(plain_env())
        .output()
        .expect("run git");
    (
        output.status.success(),
        String::from_utf8_lossy(&output.stdout).into_owned(),
    )
}

fn git_ok(args: &[&str], cwd: &Path) -> String {
    let (ok, out) = git(args, cwd);
    assert!(ok, "git {args:?} failed in the world");
    out
}

/// One workflow run: what GitHub shows, the reads of the adapter, and the job of each attempt.
struct Run {
    data: Map<String, Value>,
    reads: u32,
    jobs: Vec<i64>,
}

/// The state of the fake services, and the handler of each call.
pub struct World {
    pub remote: PathBuf,
    pub jira_token: Option<String>,
    pub forge_token: Option<String>,
    pub jira_user: Option<String>,
    issues: BTreeMap<String, Value>,
    pulls: BTreeMap<i64, Value>,
    reviews: BTreeMap<i64, Vec<Value>>,
    comments: BTreeMap<i64, Vec<Value>>,
    issue_comments: BTreeMap<i64, Vec<Value>>,
    runs: Vec<Run>,
    jobs: BTreeMap<i64, Value>,
    logs: BTreeMap<i64, String>,
    next_pull: i64,
    next_id: i64,
    next_run: i64,
    /// The review threads that are resolved, by the id of their first comment.
    pub resolved: BTreeSet<i64>,
    /// The hidden comments: node id -> the reason, such as `outdated`.
    pub minimized: BTreeMap<String, String>,
    /// Each call of the adapter, with no header value.
    pub log: Vec<Value>,
}

/// The classifiers that minimizeComment takes.
const CLASSIFIERS: [&str; 6] = [
    "SPAM",
    "ABUSE",
    "OFF_TOPIC",
    "OUTDATED",
    "DUPLICATE",
    "RESOLVED",
];

/// An answer of the GraphQL API with one error.
fn graphql_error(message: &str) -> Reply {
    Reply::json(200, json!({"data": null, "errors": [{"message": message}]}))
}

/// A page of a list, as GitHub pages it with `page` and `per_page`.
fn page(items: &[Value], record: &Record) -> Value {
    let per: usize = record
        .query
        .get("per_page")
        .and_then(|p| p.parse().ok())
        .unwrap_or(30);
    let page: usize = record
        .query
        .get("page")
        .and_then(|p| p.parse().ok())
        .unwrap_or(1);
    let start = (page.saturating_sub(1) * per).min(items.len());
    let end = (page * per).min(items.len());
    Value::Array(items[start..end].to_vec())
}

fn body_of(record: &Record) -> Map<String, Value> {
    match &record.body {
        Value::Object(map) => map.clone(),
        _ => Map::new(),
    }
}

fn text<'a>(value: &'a Value, key: &str) -> &'a str {
    value.get(key).and_then(Value::as_str).unwrap_or("")
}

fn not_found() -> Option<Reply> {
    Some(Reply::json(404, json!({"message": "Not Found"})))
}

impl World {
    pub fn new(
        remote: &Path,
        jira_token: Option<&str>,
        forge_token: Option<&str>,
        jira_user: Option<&str>,
    ) -> World {
        World {
            remote: remote.to_path_buf(),
            jira_token: jira_token.map(str::to_string),
            forge_token: forge_token.map(str::to_string),
            jira_user: jira_user.map(str::to_string),
            issues: BTreeMap::new(),
            pulls: BTreeMap::new(),
            reviews: BTreeMap::new(),
            comments: BTreeMap::new(),
            issue_comments: BTreeMap::new(),
            runs: Vec::new(),
            jobs: BTreeMap::new(),
            logs: BTreeMap::new(),
            next_pull: 7,
            next_id: 1000,
            next_run: 40,
            resolved: BTreeSet::new(),
            minimized: BTreeMap::new(),
            log: Vec::new(),
        }
    }

    /// Opens a pull request of a branch of the bare repo, as the person does. Gives its number.
    pub fn open_pull(&mut self, branch: &str) -> i64 {
        let mut body = Map::new();
        body.insert("head".into(), json!(branch));
        body.insert("title".into(), json!("A change"));
        let reply = self.create_pull(&body);
        assert_eq!(reply.status, 201, "the pull request of {branch}");
        self.next_pull - 1
    }

    pub fn add_issue(&mut self, key: &str, title: &str, description: &str) {
        self.issues.insert(
            key.into(),
            json!({"key": key, "summary": title, "description": adf(description), "status": "To Do"}),
        );
    }

    fn new_id(&mut self, wanted: Option<&Value>) -> i64 {
        if let Some(id) = wanted.and_then(Value::as_i64) {
            return id;
        }
        self.next_id += 1;
        self.next_id
    }

    // The bare repo.

    fn branch_head(&self, branch: &str) -> Option<String> {
        let reference = format!("refs/heads/{branch}");
        let (ok, out) = git(
            &["rev-parse", "--verify", "--quiet", &reference],
            &self.remote,
        );
        let out = out.trim().to_string();
        (ok && !out.is_empty()).then_some(out)
    }

    fn branches(&self) -> Vec<String> {
        git_ok(
            &["for-each-ref", "--format=%(refname:strip=2)", "refs/heads"],
            &self.remote,
        )
        .lines()
        .filter(|l| !l.is_empty())
        .map(str::to_string)
        .collect()
    }

    /// Runs ci.sh of the commit. Gives (conclusion, log).
    fn run_ci(&self, sha: &str) -> (String, String) {
        let tmp = tempfile::Builder::new()
            .prefix("fake-ci-")
            .tempdir()
            .unwrap();
        let archive = tmp.path().join("commit.tar");
        let archive_text = archive.display().to_string();
        git_ok(
            &["archive", "--format=tar", "-o", &archive_text, sha],
            &self.remote,
        );
        let tree = tmp.path().join("tree");
        std::fs::create_dir_all(&tree).unwrap();
        let status = Command::new("tar")
            .arg("-xf")
            .arg(&archive)
            .arg("-C")
            .arg(&tree)
            .status()
            .expect("run tar");
        assert!(status.success(), "tar of the commit {sha}");
        if !tree.join("ci.sh").is_file() {
            return (
                "success".into(),
                "There is no ci.sh at this commit.\n".into(),
            );
        }
        let (reader, writer) = std::io::pipe().unwrap();
        let mut child = Command::new("sh")
            .arg("ci.sh")
            .current_dir(&tree)
            .env_clear()
            .envs(plain_env())
            .stdin(Stdio::null())
            .stdout(writer.try_clone().unwrap())
            .stderr(writer)
            .spawn()
            .expect("run ci.sh");
        let mut output = Vec::new();
        {
            use std::io::Read;
            let mut reader = reader;
            reader.read_to_end(&mut output).unwrap();
        }
        let status = child.wait().unwrap();
        let text = String::from_utf8_lossy(&output).into_owned();
        let count = text.chars().count();
        let text: String = text.chars().skip(count.saturating_sub(LOG_KEEP)).collect();
        let conclusion = if status.success() {
            "success"
        } else {
            "failure"
        };
        (conclusion.into(), text)
    }

    // Views.

    fn pull_view(&self, pull: &Value) -> Value {
        let mut view = pull.clone();
        if view["merged"] != json!(true) {
            let reference = text(&view["head"], "ref").to_string();
            if let Some(sha) = self.branch_head(&reference) {
                view["head"]["sha"] = json!(sha);
            }
        }
        let requested: Vec<Value> = view["requested"]
            .as_array()
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .map(|n| json!({ "login": n }))
            .collect();
        let map = view.as_object_mut().unwrap();
        map.insert("requested_reviewers".into(), Value::Array(requested));
        map.remove("requested");
        view
    }

    fn issue_view(&self, key: &str, fields: Option<&str>) -> Value {
        let issue = &self.issues[key];
        let all = json!({
            "summary": issue["summary"],
            "status": {"name": issue["status"]},
            "description": issue["description"],
            "issuetype": {"name": "Task"},
            "priority": {"name": "Medium"},
            "labels": [],
            "assignee": null,
            "comment": {"comments": [], "total": 0},
        });
        let all = all.as_object().unwrap();
        let wanted: Vec<String> = match fields {
            Some(f) if f.split(',').any(|x| !x.is_empty()) => f
                .split(',')
                .filter(|x| !x.is_empty())
                .map(str::to_string)
                .collect(),
            _ => all.keys().cloned().collect(),
        };
        let mut out = Map::new();
        for field in wanted {
            if let Some(value) = all.get(&field) {
                out.insert(field, value.clone());
            }
        }
        json!({"key": key, "fields": out})
    }

    /// The run as GitHub shows it. A read of the adapter counts toward the end of the attempt;
    /// a read of the control calls does not.
    fn run_view(run: &mut Run, count: bool) -> Value {
        let reads = run.reads;
        if count {
            run.reads += 1;
        }
        let mut view = run.data.clone();
        if reads < 2 {
            view.insert(
                "status".into(),
                json!(if reads == 0 { "queued" } else { "in_progress" }),
            );
            view.insert("conclusion".into(), Value::Null);
        }
        view.insert(
            "path".into(),
            json!(format!(".github/workflows/{WORKFLOW}")),
        );
        let id = run.data["id"].as_i64().unwrap_or(0);
        view.insert(
            "html_url".into(),
            json!(format!("https://github.example/{REPO}/actions/runs/{id}")),
        );
        Value::Object(view)
    }

    // The handler.

    pub fn auth_of(&self, record: &Record) -> &'static str {
        let Some(header) = record.headers.get("authorization") else {
            return "none";
        };
        if let Some(forge) = &self.forge_token {
            if *header == format!("Bearer {forge}") {
                return "forge";
            }
        }
        if let (Some(jira), Some(encoded)) = (&self.jira_token, header.strip_prefix("Basic ")) {
            let Some(decoded) = vision::util::base64::decode(encoded) else {
                return "bad";
            };
            let decoded = String::from_utf8_lossy(&decoded).into_owned();
            let (user, token) = decoded.split_once(':').unwrap_or((decoded.as_str(), ""));
            if token == jira && self.jira_user.as_deref().is_none_or(|u| u == user) {
                return "jira";
            }
        }
        "bad"
    }

    pub fn handle(&mut self, record: &Record, auth: &str) -> Option<Reply> {
        let path = record.path.as_str();
        if path == "/health" {
            return Some(Reply::json(200, json!({"ok": true})));
        }
        if let Some(id) = path.strip_prefix("/storage/logs/") {
            let id: i64 = id.parse().unwrap_or(0);
            let log = self.logs.get(&id).cloned().unwrap_or_default();
            return Some(Reply::text(200, &log));
        }
        if let Some(rest) = path.strip_prefix("/rest/api/3") {
            if !path.starts_with("/rest/api/3/") {
                return None;
            }
            if auth != "jira" {
                return Some(Reply::json(
                    401,
                    json!({"errorMessages": ["The token of Jira is missing or wrong."]}),
                ));
            }
            return Some(self.jira(record, rest));
        }
        if let Some(rest) = path.strip_prefix(REPO_PATH) {
            if auth != "forge" {
                return Some(Reply::json(401, json!({"message": "Bad credentials"})));
            }
            return self.github(record, rest);
        }
        if path == "/graphql" && record.method == "POST" {
            if auth != "forge" {
                return Some(Reply::json(401, json!({"message": "Bad credentials"})));
            }
            return Some(self.graphql(record));
        }
        None
    }

    // GraphQL.

    /// The review threads of a pull request: the id of the first comment of each, and the ids
    /// of its comments in their order.
    fn threads_of(&self, number: i64) -> Vec<(i64, Vec<i64>)> {
        let mut threads: Vec<(i64, Vec<i64>)> = Vec::new();
        for comment in self.comments.get(&number).into_iter().flatten() {
            let id = comment["id"].as_i64().unwrap_or(0);
            let root = comment["in_reply_to_id"].as_i64().unwrap_or(id);
            match threads.iter_mut().find(|(r, _)| *r == root) {
                Some((_, ids)) => ids.push(id),
                None => threads.push((root, vec![id])),
            }
        }
        threads
    }

    /// True when a comment or a review has this node id.
    fn node_exists(&self, node: &str) -> bool {
        self.comments
            .values()
            .chain(self.issue_comments.values())
            .chain(self.reviews.values())
            .flatten()
            .any(|item| item["node_id"] == node)
    }

    fn graphql(&mut self, record: &Record) -> Reply {
        let body = body_of(record);
        let query = body.get("query").and_then(Value::as_str).unwrap_or("");
        let vars = body.get("variables").cloned().unwrap_or_else(|| json!({}));
        if query.contains("resolveReviewThread") {
            let id = text(&vars, "id").to_string();
            let root = id
                .strip_prefix("PRRT_")
                .and_then(|r| r.parse::<i64>().ok())
                .filter(|root| {
                    self.pulls
                        .keys()
                        .any(|n| self.threads_of(*n).iter().any(|(r, _)| r == root))
                });
            let Some(root) = root else {
                return graphql_error(&format!(
                    "Could not resolve to a node with the global id of '{id}'"
                ));
            };
            self.resolved.insert(root);
            return Reply::json(
                200,
                json!({"data": {"resolveReviewThread": {"thread": {"id": id, "isResolved": true}}}}),
            );
        }
        if query.contains("minimizeComment") {
            let id = text(&vars, "id").to_string();
            let classifier = text(&vars, "classifier").to_string();
            if !CLASSIFIERS.contains(&classifier.as_str()) {
                return graphql_error(&format!(
                    "Argument 'classifier' has an invalid value ({classifier})"
                ));
            }
            if !self.node_exists(&id) {
                return graphql_error(&format!(
                    "Could not resolve to a node with the global id of '{id}'"
                ));
            }
            let reason = classifier.to_lowercase();
            self.minimized.insert(id, reason.clone());
            return Reply::json(
                200,
                json!({"data": {"minimizeComment": {"minimizedComment": {"isMinimized": true, "minimizedReason": reason}}}}),
            );
        }
        if query.contains("reviewThreads") {
            if text(&vars, "owner") != OWNER || format!("{OWNER}/{}", text(&vars, "name")) != REPO {
                return Reply::json(200, json!({"data": {"repository": null}}));
            }
            let number = vars["number"].as_i64().unwrap_or(0);
            if !self.pulls.contains_key(&number) {
                return Reply::json(200, json!({"data": {"repository": {"pullRequest": null}}}));
            }
            let nodes: Vec<Value> = self
                .threads_of(number)
                .into_iter()
                .map(|(root, ids)| {
                    let comments: Vec<Value> =
                        ids.iter().map(|id| json!({ "databaseId": id })).collect();
                    json!({"id": format!("PRRT_{root}"), "isResolved": self.resolved.contains(&root),
                           "comments": {"nodes": comments}})
                })
                .collect();
            return Reply::json(
                200,
                json!({"data": {"repository": {"pullRequest": {"reviewThreads": {
                    "pageInfo": {"hasNextPage": false, "endCursor": null}, "nodes": nodes}}}}}),
            );
        }
        if query.contains("nodes(ids:") {
            let nodes: Vec<Value> = vars["ids"]
                .as_array()
                .cloned()
                .unwrap_or_default()
                .iter()
                .map(|id| {
                    let id = id.as_str().unwrap_or("");
                    if !self.node_exists(id) {
                        return Value::Null;
                    }
                    let reason = self.minimized.get(id);
                    json!({"id": id, "isMinimized": reason.is_some(), "minimizedReason": reason})
                })
                .collect();
            return Reply::json(200, json!({"data": {"nodes": nodes}}));
        }
        graphql_error("the fake world does not know this query")
    }

    fn jira(&mut self, record: &Record, path: &str) -> Reply {
        let method = record.method.as_str();
        let body = body_of(record);
        if method == "POST" && path == "/search/jql" {
            let fields: Vec<String> = body
                .get("fields")
                .and_then(Value::as_array)
                .map(|f| {
                    f.iter()
                        .filter_map(Value::as_str)
                        .map(str::to_string)
                        .collect()
                })
                .unwrap_or_default();
            let fields = fields.join(",");
            let issues: Vec<Value> = self
                .issues
                .iter()
                .filter(|(_, i)| i["status"] == "To Do")
                .map(|(k, _)| self.issue_view(k, Some(&fields)))
                .collect();
            return Reply::json(200, json!({"issues": issues, "isLast": true}));
        }
        let missing = || {
            Reply::json(
                404,
                json!({"errorMessages": ["Issue does not exist or you do not have permission to see it."]}),
            )
        };
        let Some(rest) = path.strip_prefix("/issue/") else {
            return missing();
        };
        let (key, transitions_path) = match rest.strip_suffix("/transitions") {
            Some(key) => (key, true),
            None => (rest, false),
        };
        if !vision::adapters::jira_github::core::is_key(key) || !self.issues.contains_key(key) {
            return missing();
        }
        if transitions_path {
            if method == "GET" {
                let status = self.issues[key]["status"].clone();
                let list: Vec<Value> = transitions()
                    .into_iter()
                    .filter(|t| t["to"]["name"] != status)
                    .collect();
                return Reply::json(200, json!({ "transitions": list }));
            }
            if method == "POST" {
                let wanted = match body.get("transition").and_then(|t| t.get("id")) {
                    Some(Value::String(s)) => s.clone(),
                    Some(other) => other.to_string(),
                    None => "None".into(),
                };
                let Some(found) = transitions().into_iter().find(|t| t["id"] == wanted) else {
                    return Reply::json(
                        400,
                        json!({"errorMessages": [format!("The transition {wanted} is not valid.")]}),
                    );
                };
                self.issues.get_mut(key).unwrap()["status"] = found["to"]["name"].clone();
                return Reply::empty(204);
            }
        } else if method == "GET" {
            return Reply::json(
                200,
                self.issue_view(key, record.query.get("fields").map(String::as_str)),
            );
        } else if method == "PUT" {
            let description = body.get("fields").and_then(|f| f.get("description"));
            match description {
                Some(d) if d.is_object() && d["type"] == "doc" => {
                    self.issues.get_mut(key).unwrap()["description"] = d.clone();
                    return Reply::empty(204);
                }
                _ => {
                    return Reply::json(
                        400,
                        json!({"errors": {"description": "Operation value must be an ADF document"}}),
                    )
                }
            }
        }
        Reply::json(405, json!({"errorMessages": ["Method not allowed"]}))
    }

    fn github(&mut self, record: &Record, path: &str) -> Option<Reply> {
        let method = record.method.as_str();
        let body = body_of(record);
        if method == "GET" && path == "/branches" {
            let names: Vec<Value> = self
                .branches()
                .into_iter()
                .map(|b| json!({ "name": b }))
                .collect();
            return Some(Reply::json(200, page(&names, record)));
        }
        if let (Some(branch), "GET") = (path.strip_prefix("/git/ref/heads/"), method) {
            return match self.branch_head(branch) {
                Some(sha) => Some(Reply::json(
                    200,
                    json!({"ref": format!("refs/heads/{branch}"), "object": {"sha": sha, "type": "commit"}}),
                )),
                None => not_found(),
            };
        }
        if path == "/pulls" {
            if method == "POST" {
                return Some(self.create_pull(&body));
            }
            let state = record
                .query
                .get("state")
                .cloned()
                .unwrap_or_else(|| "open".into());
            let head = record.query.get("head");
            let mut found = Vec::new();
            for pull in self.pulls.values() {
                if let Some(head) = head {
                    if format!("{OWNER}:{}", text(&pull["head"], "ref")) != *head {
                        continue;
                    }
                }
                if state != "all" && text(pull, "state") != state {
                    continue;
                }
                found.push(self.pull_view(pull));
            }
            return Some(Reply::json(200, page(&found, record)));
        }
        if let Some(rest) = path.strip_prefix("/pulls/") {
            let (number, tail) = match rest.find('/') {
                Some(i) => (&rest[..i], &rest[i..]),
                None => (rest, ""),
            };
            if !number.is_empty() && number.bytes().all(|b| b.is_ascii_digit()) {
                let number: i64 = number.parse().unwrap_or(0);
                if !self.pulls.contains_key(&number) {
                    return not_found();
                }
                return Some(self.pull_call(record, number, tail));
            }
        }
        if let Some(rest) = path.strip_prefix("/issues/") {
            if let Some(number) = rest.strip_suffix("/comments") {
                if let Ok(number) = number.parse::<i64>() {
                    if !self.pulls.contains_key(&number) {
                        return not_found();
                    }
                    if method == "POST" {
                        let id = self.new_id(None);
                        let comment = json!({"id": id, "node_id": format!("IC_{id}"),
                                             "user": {"login": "vision-bot"},
                                             "body": text(&Value::Object(body.clone()), "body"),
                                             "created_at": now()});
                        self.issue_comments
                            .entry(number)
                            .or_default()
                            .push(comment.clone());
                        return Some(Reply::json(201, comment));
                    }
                    let items = self
                        .issue_comments
                        .get(&number)
                        .cloned()
                        .unwrap_or_default();
                    return Some(Reply::json(200, page(&items, record)));
                }
            }
        }
        Some(self.actions(record, path))
    }

    fn create_pull(&mut self, body: &Map<String, Value>) -> Reply {
        let branch = body.get("head").and_then(Value::as_str).unwrap_or("");
        let head = if branch.is_empty() {
            None
        } else {
            self.branch_head(branch)
        };
        let Some(head) = head else {
            return Reply::json(
                422,
                json!({"message": "Validation Failed", "errors": [{"message": format!("No commits between main and {branch}")}]}),
            );
        };
        if self
            .pulls
            .values()
            .any(|p| text(&p["head"], "ref") == branch && text(p, "state") == "open")
        {
            return Reply::json(
                422,
                json!({"message": "Validation Failed", "errors": [{"message": "A pull request already exists"}]}),
            );
        }
        let number = self.next_pull;
        self.next_pull += 1;
        let base = body
            .get("base")
            .and_then(Value::as_str)
            .filter(|b| !b.is_empty())
            .unwrap_or("main");
        let pull = json!({
            "number": number, "state": "open",
            "draft": body.get("draft").and_then(Value::as_bool).unwrap_or(false),
            "merged": false, "merged_at": null, "mergeable": true, "mergeable_state": "clean",
            "title": body.get("title").and_then(Value::as_str).unwrap_or(""),
            "body": body.get("body").and_then(Value::as_str).unwrap_or(""),
            "head": {"ref": branch, "sha": head}, "base": {"ref": base},
            "requested": [], "html_url": format!("https://github.example/{REPO}/pull/{number}"),
        });
        self.pulls.insert(number, pull.clone());
        Reply::json(201, self.pull_view(&pull))
    }

    fn pull_call(&mut self, record: &Record, number: i64, rest: &str) -> Reply {
        let method = record.method.as_str();
        let body = body_of(record);
        if rest.is_empty() && method == "GET" {
            return Reply::json(200, self.pull_view(&self.pulls[&number]));
        }
        if rest == "/reviews" && method == "GET" {
            let items = self.reviews.get(&number).cloned().unwrap_or_default();
            return Reply::json(200, page(&items, record));
        }
        if rest == "/comments" && method == "GET" {
            let items = self.comments.get(&number).cloned().unwrap_or_default();
            return Reply::json(200, page(&items, record));
        }
        if let Some(root) = rest
            .strip_prefix("/comments/")
            .and_then(|r| r.strip_suffix("/replies"))
            .and_then(|r| r.parse::<i64>().ok())
        {
            if method == "POST" {
                let items = self.comments.get(&number).cloned().unwrap_or_default();
                let first = items.iter().find(|c| c["id"] == root);
                let Some(first) =
                    first.filter(|f| f.get("in_reply_to_id").is_none_or(Value::is_null))
                else {
                    return Reply::json(404, json!({"message": "Not Found"}));
                };
                let id = self.new_id(None);
                let reply = json!({"id": id, "node_id": format!("PRRC_{id}"), "user": {"login": "vision-bot"},
                                   "body": text(&Value::Object(body.clone()), "body"),
                                   "in_reply_to_id": root, "path": first["path"], "line": first["line"],
                                   "original_line": first["line"], "created_at": now()});
                self.comments.entry(number).or_default().push(reply.clone());
                return Reply::json(201, reply);
            }
        }
        if rest == "/requested_reviewers" && method == "POST" {
            let pull = self.pulls.get_mut(&number).unwrap();
            for name in body
                .get("reviewers")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default()
            {
                let list = pull["requested"].as_array_mut().unwrap();
                if !list.contains(&name) {
                    list.push(name);
                }
            }
            let pull = pull.clone();
            return Reply::json(201, self.pull_view(&pull));
        }
        Reply::json(404, json!({"message": "Not Found"}))
    }

    fn actions(&mut self, record: &Record, path: &str) -> Reply {
        let method = record.method.as_str();
        let body = body_of(record);
        if method == "GET" && path == "/actions/runs" {
            let sha = record.query.get("head_sha").cloned();
            let mut runs = Vec::new();
            for run in self.runs.iter_mut().rev() {
                if sha.as_deref().is_none_or(|s| run.data["head_sha"] == s) {
                    runs.push(World::run_view(run, true));
                }
            }
            return Reply::json(
                200,
                json!({"total_count": runs.len(), "workflow_runs": page(&runs, record)}),
            );
        }
        if method == "POST" && path == format!("/actions/workflows/{WORKFLOW}/dispatches") {
            let reference = body
                .get("ref")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            let Some(sha) = self.branch_head(&reference) else {
                return Reply::json(
                    422,
                    json!({"message": format!("No ref found for: {reference}")}),
                );
            };
            self.next_run += 1;
            let mut run = Run {
                data: json!({"id": self.next_run, "head_sha": sha, "head_branch": reference,
                             "event": "workflow_dispatch", "created_at": now(), "run_attempt": 0})
                .as_object()
                .unwrap()
                .clone(),
                reads: 0,
                jobs: Vec::new(),
            };
            self.attempt(&mut run);
            self.runs.push(run);
            return Reply::empty(204);
        }
        if let Some(rest) = path.strip_prefix("/actions/runs/") {
            let (id, tail) = match rest.find('/') {
                Some(i) => (&rest[..i], &rest[i..]),
                None => (rest, ""),
            };
            if let (Ok(id), true) = (
                id.parse::<i64>(),
                matches!(tail, "" | "/rerun-failed-jobs" | "/jobs"),
            ) {
                let Some(index) = self.runs.iter().position(|r| r.data["id"] == id) else {
                    return Reply::json(404, json!({"message": "Not Found"}));
                };
                if tail == "/rerun-failed-jobs" && method == "POST" {
                    let mut run = self.runs.remove(index);
                    let reply = if run.reads < 2 {
                        Reply::json(
                            403,
                            json!({"message": "This workflow run is not completed"}),
                        )
                    } else if run.data["conclusion"] == "success" {
                        Reply::json(403, json!({"message": "This workflow run cannot be rerun"}))
                    } else {
                        self.attempt(&mut run);
                        Reply::json(201, json!({}))
                    };
                    self.runs.insert(index, run);
                    return reply;
                }
                if tail == "/jobs" && method == "GET" {
                    let run = &self.runs[index];
                    let mut jobs: Vec<Value> = run
                        .jobs
                        .iter()
                        .rev()
                        .take(1)
                        .map(|j| self.jobs[j].clone())
                        .collect();
                    if run.reads < 2 {
                        for job in &mut jobs {
                            job["status"] = json!("in_progress");
                            job["conclusion"] = Value::Null;
                        }
                    }
                    return Reply::json(
                        200,
                        json!({"total_count": jobs.len(), "jobs": page(&jobs, record)}),
                    );
                }
                if tail.is_empty() && method == "GET" {
                    return Reply::json(200, World::run_view(&mut self.runs[index], true));
                }
            }
        }
        if let (Some(id), "GET") = (
            path.strip_prefix("/actions/jobs/")
                .and_then(|r| r.strip_suffix("/logs"))
                .and_then(|r| r.parse::<i64>().ok()),
            method,
        ) {
            if !self.logs.contains_key(&id) {
                return Reply::json(404, json!({"message": "Not Found"}));
            }
            let host = record
                .headers
                .get("host")
                .cloned()
                .unwrap_or_else(|| "127.0.0.1".into());
            return Reply::empty(302).header(
                "Location",
                &format!("http://{host}/storage/logs/{id}?sig=fake"),
            );
        }
        Reply::json(404, json!({"message": "Not Found"}))
    }

    /// One attempt of a run: it runs ci.sh of the commit and ends at once.
    fn attempt(&mut self, run: &mut Run) {
        let sha = text(&Value::Object(run.data.clone()), "head_sha").to_string();
        let (conclusion, log) = self.run_ci(&sha);
        let attempt = run.data["run_attempt"].as_i64().unwrap_or(0) + 1;
        run.data.insert("run_attempt".into(), json!(attempt));
        run.reads = 0;
        run.data.insert("status".into(), json!("completed"));
        run.data.insert("conclusion".into(), json!(conclusion));
        run.data.insert("run_started_at".into(), json!(now()));
        run.data.insert("updated_at".into(), json!(now()));
        let id = run.data["id"].as_i64().unwrap_or(0);
        let job_id = id * 100 + attempt;
        let step = json!({"name": "Run ci.sh", "conclusion": conclusion, "number": 1});
        self.jobs.insert(
            job_id,
            json!({"id": job_id, "run_id": id, "name": "test", "status": "completed",
                   "conclusion": conclusion, "steps": [step], "run_attempt": attempt}),
        );
        self.logs.insert(job_id, log);
        run.jobs.push(job_id);
    }

    // Control: the team.

    pub fn control(&mut self, action: &str, body: &Map<String, Value>) -> Reply {
        let number = body.get("number").and_then(Value::as_i64).unwrap_or(0);
        if action == "state" {
            return Reply::json(200, self.state());
        }
        if !self.pulls.contains_key(&number) {
            return Reply::json(
                404,
                json!({"message": format!("no pull request #{number}")}),
            );
        }
        match action {
            "ready" => {
                self.pulls.get_mut(&number).unwrap()["draft"] = json!(false);
                Reply::json(200, self.pull_view(&self.pulls[&number]))
            }
            "review" => {
                let login = body["login"].clone();
                let id = self.new_id(body.get("id"));
                let commit = self.pull_view(&self.pulls[&number])["head"]["sha"].clone();
                let review = json!({"id": id, "node_id": format!("PRR_{id}"), "user": {"login": login}, "state": body["state"],
                                    "body": body.get("body").and_then(Value::as_str).unwrap_or(""),
                                    "submitted_at": now(), "commit_id": commit});
                self.reviews.entry(number).or_default().push(review.clone());
                let pull = self.pulls.get_mut(&number).unwrap();
                pull["requested"]
                    .as_array_mut()
                    .unwrap()
                    .retain(|n| *n != login);
                Reply::json(201, review)
            }
            "comment" => {
                let id = self.new_id(body.get("id"));
                let line = body.get("line").cloned().unwrap_or(Value::Null);
                let mut comment = json!({"id": id, "node_id": format!("PRRC_{id}"),
                                     "user": {"login": body["login"]}, "body": body["body"],
                                     "path": body.get("path").cloned().unwrap_or(Value::Null),
                                     "line": line, "original_line": line, "created_at": now()});
                if let Some(root) = body.get("in_reply_to").and_then(Value::as_i64) {
                    comment["in_reply_to_id"] = json!(root);
                }
                self.comments
                    .entry(number)
                    .or_default()
                    .push(comment.clone());
                Reply::json(201, comment)
            }
            "issue_comment" => {
                let id = self.new_id(body.get("id"));
                let comment = json!({"id": id, "node_id": format!("IC_{id}"),
                                     "user": {"login": body["login"]}, "body": body["body"],
                                     "created_at": now()});
                self.issue_comments
                    .entry(number)
                    .or_default()
                    .push(comment.clone());
                Reply::json(201, comment)
            }
            "conflict" => {
                // The base moved on, and the pull request conflicts with it, or no longer does.
                let conflict = body
                    .get("conflict")
                    .and_then(Value::as_bool)
                    .unwrap_or(true);
                let pull = self.pulls.get_mut(&number).unwrap();
                pull["mergeable"] = json!(!conflict);
                pull["mergeable_state"] = json!(if conflict { "dirty" } else { "clean" });
                let pull = pull.clone();
                Reply::json(200, self.pull_view(&pull))
            }
            "merge" => self.merge(number),
            other => Reply::json(
                404,
                json!({"message": format!("no control action {other}")}),
            ),
        }
    }

    fn merge(&mut self, number: i64) -> Reply {
        let pull = self.pulls[&number].clone();
        if pull["merged"] == json!(true) {
            return Reply::json(200, self.pull_view(&pull));
        }
        let branch = text(&pull["head"], "ref").to_string();
        let head = self.branch_head(&branch);
        let work = tempfile::Builder::new()
            .prefix("fake-merge-")
            .tempdir()
            .unwrap();
        let work_text = work.path().display().to_string();
        let remote_text = self.remote.display().to_string();
        git_ok(
            &["clone", "--quiet", &remote_text, &work_text],
            &self.remote,
        );
        let refspec = format!("refs/heads/{branch}:refs/remotes/origin/{branch}");
        git_ok(&["fetch", "--quiet", "origin", &refspec], work.path());
        let message = format!("Merge pull request #{number} from {branch}");
        let theirs = format!("origin/{branch}");
        git_ok(
            &["merge", "--quiet", "--no-ff", "-m", &message, &theirs],
            work.path(),
        );
        git_ok(
            &["push", "--quiet", "origin", "HEAD:refs/heads/main"],
            work.path(),
        );
        let pull = self.pulls.get_mut(&number).unwrap();
        pull["merged"] = json!(true);
        pull["merged_at"] = json!(now());
        pull["state"] = json!("closed");
        pull["mergeable"] = Value::Null;
        pull["mergeable_state"] = json!("unknown");
        pull["head"]["sha"] = json!(head);
        let pull = pull.clone();
        Reply::json(200, self.pull_view(&pull))
    }

    pub fn state(&mut self) -> Value {
        let pulls: Map<String, Value> = self
            .pulls
            .iter()
            .map(|(n, p)| (n.to_string(), self.pull_view(p)))
            .collect();
        let by_number = |m: &BTreeMap<i64, Vec<Value>>| -> Map<String, Value> {
            m.iter()
                .map(|(n, items)| (n.to_string(), Value::Array(items.clone())))
                .collect()
        };
        let runs: Vec<Value> = self
            .runs
            .iter_mut()
            .map(|r| World::run_view(r, false))
            .collect();
        json!({
            "issues": self.issues,
            "pulls": pulls,
            "reviews": by_number(&self.reviews),
            "comments": by_number(&self.comments),
            "issue_comments": by_number(&self.issue_comments),
            "runs": runs,
            "resolved": self.resolved,
            "minimized": self.minimized,
        })
    }
}

/// Serves the world on the server: each request that no route answers goes to the world. Each
/// call of the adapter goes into the log of the world, with no header value.
pub fn install(server: &FakeServer, world: Arc<Mutex<World>>) {
    server.fallback(move |record| {
        let mut world = world.lock().unwrap();
        let auth = world.auth_of(record);
        if let Some(action) = record.path.strip_prefix("/_control/") {
            return Some(world.control(action, &body_of(record)));
        }
        let result = world.handle(record, auth);
        let entry = json!({
            "method": record.method, "path": record.path, "raw_path": record.raw_path,
            "query": record.query, "auth": auth, "body": record.body,
            "status": result.as_ref().map(|r| r.status).unwrap_or(404),
        });
        world.log.push(entry);
        result
    });
}
