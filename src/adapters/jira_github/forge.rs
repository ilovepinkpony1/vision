//! The client of GitHub REST, and the facts of a pull request, its reviews, and its CI. A few
//! calls that REST does not have (the review threads, their state, and the hidden comments) go
//! to the GraphQL API of GitHub.

use std::collections::{BTreeMap, BTreeSet};

use serde_json::{json, Map, Value};

use super::core::{
    clean, fail, is_commit, py_repr_str, secret, short, truthy, Config, Error, Res,
    DEFAULT_GITHUB_API,
};
use super::http::{check_base, query, ref_path, segment, Call, Client};

const GREEN: [&str; 1] = ["success"];
const RED: [&str; 4] = ["failure", "timed_out", "startup_failure", "cancelled"];
const REVIEW_STATES: [&str; 3] = ["APPROVED", "CHANGES_REQUESTED", "DISMISSED"];

/// The three kinds of a comment that waits.
pub const KINDS: [&str; 3] = ["inline", "review", "conversation"];

pub struct GitHub {
    pub owner: String,
    pub name: String,
    pub client: Client,
}

/// One review thread of a pull request, as the GraphQL API gives it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Thread {
    /// The node id of the thread, which resolveReviewThread takes.
    pub id: String,
    pub resolved: bool,
    /// The REST ids of its comments (`databaseId` in GraphQL).
    pub comments: Vec<i64>,
}

const THREADS_QUERY: &str = "query($owner: String!, $name: String!, $number: Int!, $after: String) { repository(owner: $owner, name: $name) { pullRequest(number: $number) { reviewThreads(first: 100, after: $after) { pageInfo { hasNextPage endCursor } nodes { id isResolved comments(first: 100) { nodes { databaseId } } } } } } }";

const MINIMIZED_QUERY: &str =
    "query($ids: [ID!]!) { nodes(ids: $ids) { id ... on Minimizable { isMinimized minimizedReason } } }";

/// Resolves one review thread: the variable `id` is the node id of the thread.
pub const RESOLVE_MUTATION: &str =
    "mutation($id: ID!) { resolveReviewThread(input: {threadId: $id}) { thread { id isResolved } } }";

/// Hides one comment: the variables `id` (the node id of the comment) and `classifier`, such as
/// OUTDATED or RESOLVED.
pub const MINIMIZE_MUTATION: &str = "mutation($id: ID!, $classifier: ReportedContentClassifiers!) { minimizeComment(input: {subjectId: $id, classifier: $classifier}) { minimizedComment { isMinimized minimizedReason } } }";

/// The most pages of review threads that one call reads.
const THREAD_PAGES: usize = 20;

/// The base address of the GraphQL API for the REST address of the configuration:
/// `https://api.github.com` gives `https://api.github.com/graphql`, and the REST address
/// `https://<host>/api/v3` of a GitHub Enterprise Server gives `https://<host>/api/graphql`.
pub fn graphql_base(rest: &str) -> String {
    let rest = rest.trim_end_matches('/');
    rest.strip_suffix("/v3").unwrap_or(rest).to_string()
}

/// An integer field of an object, or `None`. A bool is not one.
pub fn int(value: &Value, key: &str) -> Option<i64> {
    match value.get(key) {
        Some(Value::Number(n)) => n.as_i64(),
        _ => None,
    }
}

/// A text field of an object, or "".
pub fn text<'a>(value: &'a Value, key: &str) -> &'a str {
    value.get(key).and_then(Value::as_str).unwrap_or("")
}

/// The items of a JSON list that are objects.
fn objects(items: Vec<Value>) -> Vec<Value> {
    items.into_iter().filter(Value::is_object).collect()
}

impl GitHub {
    pub fn new(config: &Config, why: &str, token: Option<String>) -> Res<GitHub> {
        let repo = config.repo(why)?;
        let (owner, name) = repo.split_once('/').unwrap_or((&repo, ""));
        let (owner, name) = (owner.to_string(), name.to_string());
        let base = check_base(
            &config.text_or("github_api", DEFAULT_GITHUB_API)?,
            "github_api",
        )?;
        let token = match token {
            Some(token) => token,
            None => secret("FORGE_TOKEN", why)?,
        };
        let headers = vec![
            ("Authorization".to_string(), format!("Bearer {token}")),
            (
                "Accept".to_string(),
                "application/vnd.github+json".to_string(),
            ),
            ("X-GitHub-Api-Version".to_string(), "2022-11-28".to_string()),
            (
                "User-Agent".to_string(),
                "vision-jira-github-adapter".to_string(),
            ),
        ];
        let client = Client {
            service: "github".into(),
            base,
            headers,
            timeout: config.http_timeout()?,
        };
        Ok(GitHub {
            owner,
            name,
            client,
        })
    }

    /// The path of the repo, with more parts, such as `/repos/acme/widgets/pulls`.
    pub fn path(&self, rest: &str) -> String {
        let base = format!("/repos/{}/{}", segment(&self.owner), segment(&self.name));
        if rest.is_empty() {
            base
        } else {
            format!("{base}/{rest}")
        }
    }

    // Branches and commits.

    /// The commit of a branch on GitHub, or `None` when it does not exist.
    pub fn branch_head(&self, branch: &str) -> Res<Option<String>> {
        let data = match self.client.get(
            &self.path(&format!("git/ref/heads/{}", ref_path(branch))),
            &format!("read the branch {branch}"),
        ) {
            Ok(data) => data,
            Err(Error::Http { status: 404, .. }) => return Ok(None),
            Err(e) => return Err(e),
        };
        Ok(truthy(data.get("object"))
            .and_then(|o| o.get("sha"))
            .and_then(Value::as_str)
            .map(str::to_string))
    }

    pub fn branches(&self, max_pages: usize) -> Res<Vec<String>> {
        Ok(self
            .client
            .pages(
                &self.path("branches"),
                "list the branches",
                &[],
                100,
                max_pages,
            )?
            .iter()
            .filter_map(|b| b.get("name").and_then(Value::as_str))
            .map(str::to_string)
            .collect())
    }

    /// True when `branch` holds the commit `head`; `None` when the branch does not exist.
    pub fn holds(&self, branch: &str, head: &str) -> Res<Option<bool>> {
        self.holds_ref(&ref_path(branch), branch, head)
    }

    /// True when the commit `commit` holds the commit `head`: the same test as [`holds`], with
    /// a commit in the place of the branch. `None` when GitHub does not know the commit.
    ///
    /// [`holds`]: GitHub::holds
    pub fn commit_holds(&self, commit: &str, head: &str) -> Res<Option<bool>> {
        if commit == head {
            return Ok(Some(true));
        }
        self.holds_ref(&segment(commit), &short(commit), head)
    }

    fn holds_ref(&self, reference: &str, shown: &str, head: &str) -> Res<Option<bool>> {
        let data = match self.client.get(
            &self.path(&format!("compare/{reference}...{}", segment(head))),
            &format!("compare {shown} with {}", short(head)),
        ) {
            Ok(data) => data,
            Err(Error::Http { status: 404, .. }) => return Ok(None),
            Err(e) => return Err(e),
        };
        Ok(Some(matches!(
            data.get("status").and_then(Value::as_str),
            Some("behind" | "identical")
        )))
    }

    /// The newest deployment of a GitHub environment, or `None`. One call: the statuses of the
    /// deployment are not read.
    pub fn latest_deployment(&self, environment: &str) -> Res<Option<Value>> {
        let data = self.client.get_query(
            &self.path("deployments"),
            &format!("list the deployments of {environment}"),
            query(&[("environment", environment), ("per_page", "1")]),
        )?;
        Ok(match data {
            Value::Array(items) => items.into_iter().find(Value::is_object),
            _ => None,
        })
    }

    // Pull requests.

    pub fn pulls_of(&self, branch: &str, state: &str) -> Res<Vec<Value>> {
        let head = format!("{}:{branch}", self.owner);
        Ok(objects(self.client.pages(
            &self.path("pulls"),
            &format!("list the pull requests of {branch}"),
            &query(&[("head", &head), ("state", state)]),
            100,
            3,
        )?))
    }

    pub fn pull(&self, number: i64) -> Res<Value> {
        let value = self.client.get(
            &self.path(&format!("pulls/{number}")),
            &format!("read the pull request #{number}"),
        )?;
        Ok(truthy(Some(&value)).cloned().unwrap_or(json!({})))
    }

    /// The pull request of the call: the number of the ticket row, else the open one of the
    /// branch, else the newest merged one, else the newest one. Read in full, or `None`.
    pub fn find_pr(&self, number: Option<i64>, branch: &str) -> Res<Option<Value>> {
        if let Some(number) = number.filter(|n| *n != 0) {
            match self.pull(number) {
                Ok(pull) => return Ok(Some(pull)),
                Err(Error::Http { status: 404, .. }) => {}
                Err(e) => return Err(e),
            }
        }
        if branch.is_empty() {
            return Ok(None);
        }
        let mut pulls = self.pulls_of(branch, "all")?;
        pulls.sort_by_key(|p| std::cmp::Reverse(int(p, "number").unwrap_or(0)));
        let chosen = pulls
            .iter()
            .find(|p| text(p, "state") == "open")
            .or_else(|| pulls.iter().find(|p| truthy(p.get("merged_at")).is_some()))
            .or_else(|| pulls.first());
        match chosen {
            Some(p) => match int(p, "number") {
                Some(n) => Ok(Some(self.pull(n)?)),
                None => Err(Error::Unexpected(
                    "TypeError: a pull request has no number".into(),
                )),
            },
            None => Ok(None),
        }
    }

    pub fn reviews(&self, number: i64) -> Res<Vec<Value>> {
        Ok(objects(self.client.pages(
            &self.path(&format!("pulls/{number}/reviews")),
            &format!("list the reviews of #{number}"),
            &[],
            100,
            20,
        )?))
    }

    pub fn review_comments(&self, number: i64) -> Res<Vec<Value>> {
        Ok(objects(self.client.pages(
            &self.path(&format!("pulls/{number}/comments")),
            &format!("list the review comments of #{number}"),
            &[],
            100,
            20,
        )?))
    }

    /// The conversation comments of the pull request.
    pub fn issue_comments(&self, number: i64) -> Res<Vec<Value>> {
        Ok(objects(self.client.pages(
            &self.path(&format!("issues/{number}/comments")),
            &format!("list the conversation comments of #{number}"),
            &[],
            100,
            20,
        )?))
    }

    // GraphQL.

    /// One call of the GraphQL API: its `data`. An answer with `errors` is a failure that names
    /// them.
    pub fn graphql(&self, query: &str, variables: Value, what: &str) -> Res<Value> {
        let client = Client {
            base: graphql_base(&self.client.base),
            ..self.client.clone()
        };
        let body = json!({ "query": query, "variables": variables });
        let value = client
            .call(
                "POST",
                "/graphql",
                what,
                Call {
                    body: Some(&body),
                    ..Call::default()
                },
            )?
            .json()?;
        if let Some(errors) = value
            .get("errors")
            .and_then(Value::as_array)
            .filter(|e| !e.is_empty())
        {
            let messages: Vec<String> = errors
                .iter()
                .map(|e| clean(text(e, "message"), 200))
                .collect();
            return fail(format!(
                "github: {what} (GraphQL) answered with errors: {}",
                messages.join("; ")
            ));
        }
        Ok(value.get("data").cloned().unwrap_or(Value::Null))
    }

    /// The review threads of a pull request, with their state and the ids of their comments.
    pub fn review_threads(&self, number: i64) -> Res<Vec<Thread>> {
        let mut threads = Vec::new();
        let mut after = Value::Null;
        for _ in 0..THREAD_PAGES {
            let data = self.graphql(
                THREADS_QUERY,
                json!({"owner": self.owner, "name": self.name, "number": number, "after": after}),
                &format!("read the review threads of #{number}"),
            )?;
            let page = data
                .pointer("/repository/pullRequest/reviewThreads")
                .cloned()
                .unwrap_or(Value::Null);
            for node in page
                .get("nodes")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
            {
                let comments = node
                    .pointer("/comments/nodes")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                    .filter_map(|c| int(c, "databaseId"))
                    .collect();
                threads.push(Thread {
                    id: text(node, "id").to_string(),
                    resolved: node.get("isResolved") == Some(&Value::Bool(true)),
                    comments,
                });
            }
            let more = page.pointer("/pageInfo/hasNextPage") == Some(&Value::Bool(true));
            after = page
                .pointer("/pageInfo/endCursor")
                .cloned()
                .unwrap_or(Value::Null);
            if !more || after.is_null() {
                break;
            }
        }
        Ok(threads)
    }

    /// Whether each comment is hidden, by its node id. A node that GitHub does not know is not
    /// in the answer.
    pub fn minimized(&self, ids: &[String]) -> Res<BTreeMap<String, bool>> {
        let mut out = BTreeMap::new();
        for chunk in ids.chunks(100) {
            let data = self.graphql(
                MINIMIZED_QUERY,
                json!({ "ids": chunk }),
                &format!("read whether {} comment(s) are hidden", chunk.len()),
            )?;
            for node in data
                .get("nodes")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter(|n| n.is_object())
            {
                out.insert(
                    text(node, "id").to_string(),
                    node.get("isMinimized") == Some(&Value::Bool(true)),
                );
            }
        }
        Ok(out)
    }

    // Actions.

    /// The runs of the workflow file at the commit, newest first.
    pub fn runs_at(&self, head: &str, workflow: &str) -> Res<Vec<Value>> {
        let runs = self.client.pages(
            &self.path("actions/runs"),
            &format!("list the runs of {}", short(head)),
            &query(&[("head_sha", head)]),
            100,
            3,
        )?;
        let mut mine: Vec<Value> = runs
            .into_iter()
            .filter(|r| {
                r.is_object()
                    && r.get("head_sha").and_then(Value::as_str) == Some(head)
                    && workflow_matches(r, workflow)
            })
            .collect();
        mine.sort_by(|a, b| {
            let key = |r: &Value| {
                (
                    truthy(r.get("created_at"))
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_string(),
                    int(r, "id").unwrap_or(0),
                )
            };
            key(b).cmp(&key(a))
        });
        Ok(mine)
    }

    pub fn last_run(&self, head: Option<&str>, workflow: &str) -> Res<Option<Value>> {
        match head.filter(|h| !h.is_empty()) {
            Some(head) => Ok(self.runs_at(head, workflow)?.into_iter().next()),
            None => Ok(None),
        }
    }
}

pub fn workflow_matches(run: &Value, workflow: &str) -> bool {
    let path = super::core::py_str(truthy(run.get("path")).unwrap_or(&json!("")));
    let path = path.split('@').next().unwrap_or("");
    let name = workflow.rsplit('/').next().unwrap_or(workflow);
    let id = super::core::py_str(run.get("workflow_id").unwrap_or(&Value::Null));
    path.rsplit('/').next().unwrap_or(path) == name || id == workflow
}

/// green or red for a run that ended; running, none, or the conclusion for the others.
pub fn run_state(run: Option<&Value>) -> String {
    let Some(run) = run.filter(|r| truthy(Some(r)).is_some()) else {
        return "none".into();
    };
    if run.get("status").and_then(Value::as_str) != Some("completed") {
        return "running".into();
    }
    let conclusion = truthy(run.get("conclusion"))
        .map(super::core::py_str)
        .unwrap_or_else(|| "unknown".into());
    if GREEN.contains(&conclusion.as_str()) {
        "green".into()
    } else if RED.contains(&conclusion.as_str()) {
        "red".into()
    } else {
        conclusion
    }
}

/// The login of a user object, or "".
pub fn login(user: Option<&Value>) -> String {
    match user {
        Some(Value::Object(map)) => truthy(map.get("login"))
            .map(super::core::py_str)
            .unwrap_or_default(),
        _ => String::new(),
    }
}

fn user_of(c: &Value) -> String {
    login(c.get("user"))
}

/// The review comment threads: the root comment, then its replies, oldest first, by the id of
/// the root.
pub fn threads(comments: &[Value]) -> BTreeMap<i64, Vec<Value>> {
    let mut by_id: BTreeMap<i64, Value> = BTreeMap::new();
    for c in comments {
        if let Some(id) = int(c, "id") {
            by_id.insert(id, c.clone());
        }
    }
    let root_of = |c: &Value| -> i64 {
        let mut c = c.clone();
        let mut seen = BTreeSet::new();
        loop {
            let parent = int(&c, "in_reply_to_id");
            let id = int(&c, "id");
            match parent.and_then(|p| by_id.get(&p)) {
                Some(next) if !id.is_some_and(|i| seen.contains(&i)) => {
                    if let Some(i) = id {
                        seen.insert(i);
                    }
                    c = next.clone();
                }
                _ => break,
            }
        }
        truthy(c.get("in_reply_to_id"))
            .and_then(Value::as_i64)
            .or_else(|| int(&c, "id"))
            .unwrap_or(0)
    };
    let mut groups: BTreeMap<i64, Vec<Value>> = BTreeMap::new();
    for c in by_id.values() {
        groups.entry(root_of(c)).or_default().push(c.clone());
    }
    groups
}

/// The threads whose last comment is not from one of `own` (lowercase logins).
pub fn waiting_threads(comments: &[Value], own: &[String]) -> BTreeMap<i64, Vec<Value>> {
    threads(comments)
        .into_iter()
        .filter(|(_, items)| {
            let last = items.last().map(user_of).unwrap_or_default().to_lowercase();
            !own.contains(&last)
        })
        .collect()
}

/// The last line of a reply of Vision to a review or to a conversation comment: it names the
/// kind and the id of the comment that it answers. Gives (kind, id) for such a line.
pub fn mark_of(line: &str) -> Option<(String, i64)> {
    let line = super::core::line_end(line);
    let rest = line
        .strip_prefix("<!-- vision-reply ")?
        .strip_suffix(" -->")?;
    let (kind, id) = rest.split_once(' ')?;
    if !matches!(kind, "review" | "conversation") {
        return None;
    }
    if !(1..=20).contains(&id.len()) || !id.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    Some((kind.to_string(), id.parse().ok()?))
}

/// The first line of a text that is not empty, cut, with no mark of an HTML comment, so that a
/// quote of it can never hold the mark of a reply.
pub fn first_line(text: &str, limit: usize) -> String {
    for line in super::core::splitlines(text) {
        let line = line.trim();
        if !line.is_empty() {
            let line = line.replace("<!--", "").replace("-->", "");
            let line = line.trim();
            return if line.chars().count() <= limit {
                line.to_string()
            } else {
                format!("{}...", line.chars().take(limit).collect::<String>())
            };
        }
    }
    String::new()
}

/// A text whose mentions call nobody: a zero-width space after each `@` that starts a name. A
/// quote of a comment must not notify its people a second time.
pub fn no_mentions(text: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    let mut out = String::with_capacity(text.len());
    for (i, &c) in chars.iter().enumerate() {
        out.push(c);
        if c == '@' && chars.get(i + 1).is_some_and(|n| n.is_ascii_alphanumeric()) {
            out.push('\u{200b}');
        }
    }
    out
}

/// What the conversation comments of `own` (lowercase logins) answer: the (kind, id) of the
/// mark of their last line, and the quoted first lines.
pub fn reply_marks(
    issue_comments: &[Value],
    own: &[String],
) -> (BTreeSet<(String, i64)>, BTreeSet<String>) {
    let mut marks = BTreeSet::new();
    let mut quotes = BTreeSet::new();
    for c in issue_comments {
        if !own.contains(&user_of(c).to_lowercase()) {
            continue;
        }
        let body = super::core::py_str(truthy(c.get("body")).unwrap_or(&json!("")));
        let lines: Vec<&str> = super::core::splitlines(&body)
            .into_iter()
            .map(str::trim)
            .filter(|l| !l.is_empty())
            .collect();
        if let Some(last) = lines.last() {
            if let Some(mark) = mark_of(last) {
                marks.insert(mark);
            }
        }
        quotes.extend(
            lines
                .iter()
                .filter(|l| l.starts_with("> ") && l.chars().count() > 2)
                .map(|l| l.to_string()),
        );
    }
    (marks, quotes)
}

/// A reply to a review or a conversation comment: a new conversation comment that quotes the
/// first line of the comment, the reply, and the mark of the comment.
pub fn reply_body(kind: &str, id: i64, original: &str, reply: &str) -> String {
    format!(
        "{original}\n\n{}\n\n<!-- vision-reply {kind} {id} -->",
        reply.trim()
    )
}

/// The last review state of each author (APPROVED, CHANGES_REQUESTED, or DISMISSED), by the
/// lowercase login, and the login as GitHub shows it.
pub fn last_states(reviews: &[Value]) -> (BTreeMap<String, String>, BTreeMap<String, String>) {
    let mut sorted: Vec<&Value> = reviews.iter().collect();
    sorted.sort_by_key(|r| int(r, "id").unwrap_or(0));
    let mut last = BTreeMap::new();
    let mut shown = BTreeMap::new();
    for review in sorted {
        let state = text(review, "state");
        let name = user_of(review);
        if REVIEW_STATES.contains(&state) && !name.is_empty() {
            last.insert(name.to_lowercase(), state.to_string());
            shown.insert(name.to_lowercase(), name);
        }
    }
    (last, shown)
}

pub fn review_first_line(review: &Value) -> String {
    let line = first_line(&body_of(review), 200);
    if line.is_empty() {
        format!("{} asked for changes.", user_of(review))
    } else {
        line
    }
}

/// The body of a comment or a review, or "".
pub fn body_of(item: &Value) -> String {
    super::core::py_str(truthy(item.get("body")).unwrap_or(&json!("")))
}

/// The comments that never wait, from the entries of `ignore_logins`: a login (each kind of its
/// comments), or `<login>:<kind>` (only that kind), with the kind inline, review, or
/// conversation, such as "review-bot[bot]:conversation". A login can hold `[`, `]`, and `-`, so
/// an entry splits at its last `:`. Logins are lowercase.
#[derive(Clone, Debug, Default)]
pub struct Ignore {
    every: BTreeSet<String>,
    kinds: BTreeMap<String, BTreeSet<String>>,
}

impl Ignore {
    pub fn new(entries: &[String]) -> Res<Ignore> {
        let mut ignore = Ignore::default();
        for entry in entries {
            let text = entry.trim().to_lowercase();
            if text.is_empty() {
                continue;
            }
            let Some((name, kind)) = text.rsplit_once(':') else {
                ignore.every.insert(text);
                continue;
            };
            if !KINDS.contains(&kind) || name.trim().is_empty() {
                return fail(format!(
                    "the entry {} of `ignore_logins` must be a login or <login>:<kind>, with the kind inline, review, or conversation, such as \"review-bot[bot]:conversation\"",
                    py_repr_str(&clean(entry, 80))
                ));
            }
            ignore
                .kinds
                .entry(kind.to_string())
                .or_default()
                .insert(name.trim().to_string());
        }
        Ok(ignore)
    }

    pub fn skips(&self, name: &str, kind: &str) -> bool {
        let name = name.to_lowercase();
        self.every.contains(&name) || self.kinds.get(kind).is_some_and(|set| set.contains(&name))
    }
}

/// Each comment of a pull request that waits for an answer, as one row with its kind:
///
/// - `inline`: a thread of review comments whose last comment is not from `own`. Its id is the
///   id of the first comment, and a reply goes into the thread.
/// - `review`: a review with a body of a login that is not in `own`, that asks for changes
///   (while that is the last state of its author) or that comments. A review with no body has
///   nothing to answer: its inline comments are rows of their own.
/// - `conversation`: a conversation comment of a login that is not in `own`.
///
/// A review or a conversation comment waits until a conversation comment of `own` answers it:
/// one with its mark, or one that quotes its first line. A comment that `ignore` names never
/// waits: the rows leave it out, as if nobody wrote it. The text of each row is untrusted.
pub fn waiting_rows(
    reviews: &[Value],
    review_comments: &[Value],
    issue_comments: &[Value],
    own: &[String],
    ignore: &Ignore,
) -> Vec<Value> {
    let mut rows = Vec::new();
    let review_comments: Vec<Value> = review_comments
        .iter()
        .filter(|c| !ignore.skips(&user_of(c), "inline"))
        .cloned()
        .collect();
    for (root, items) in waiting_threads(&review_comments, own) {
        let first = &items[0];
        let last = &items[items.len() - 1];
        let mine: Vec<usize> = items
            .iter()
            .enumerate()
            .filter(|(_, c)| own.contains(&user_of(c).to_lowercase()))
            .map(|(i, _)| i)
            .collect();
        let new: &[Value] = match mine.last() {
            Some(i) => &items[i + 1..],
            None => &items,
        };
        let text = if new.len() == 1 {
            body_of(&new[0])
        } else {
            new.iter()
                .map(|c| format!("{}: {}", user_of(c), body_of(c)))
                .collect::<Vec<_>>()
                .join("\n\n")
        };
        let line = truthy(first.get("line"))
            .or_else(|| first.get("original_line"))
            .cloned()
            .unwrap_or(Value::Null);
        rows.push(json!({
            "id": root,
            "kind": "inline",
            "author": user_of(last),
            "path": first.get("path").cloned().unwrap_or(Value::Null),
            "line": line,
            "text": {"untrusted": text},
            "thread": items.iter().map(|c| json!({
                "id": c.get("id").cloned().unwrap_or(Value::Null),
                "author": user_of(c),
                "text": {"untrusted": body_of(c)},
            })).collect::<Vec<_>>(),
        }));
    }
    let (marks, quotes) = reply_marks(issue_comments, own);
    let answered = |kind: &str, id: i64, text: &str| -> bool {
        let line = first_line(text, 200);
        marks.contains(&(kind.to_string(), id))
            || (!line.is_empty() && quotes.contains(&format!("> {line}")))
    };
    let (last, _shown) = last_states(reviews);
    let mut sorted: Vec<&Value> = reviews.iter().collect();
    sorted.sort_by_key(|r| int(r, "id").unwrap_or(0));
    for review in sorted {
        let name = user_of(review);
        let state = text(review, "state");
        let body = body_of(review);
        let Some(id) = int(review, "id") else {
            continue;
        };
        if name.is_empty() || own.contains(&name.to_lowercase()) || ignore.skips(&name, "review") {
            continue;
        }
        if body.trim().is_empty() {
            continue;
        }
        if state == "CHANGES_REQUESTED" {
            if last.get(&name.to_lowercase()).map(String::as_str) != Some("CHANGES_REQUESTED") {
                continue;
            }
        } else if state != "COMMENTED" {
            continue;
        }
        if answered("review", id, &body) {
            continue;
        }
        rows.push(json!({
            "id": id,
            "kind": "review",
            "state": state,
            "author": name,
            "path": null,
            "line": null,
            "text": {"untrusted": body},
            "thread": [{"id": id, "author": name, "text": {"untrusted": body}}],
        }));
    }
    let mut sorted: Vec<&Value> = issue_comments.iter().collect();
    sorted.sort_by_key(|c| int(c, "id").unwrap_or(0));
    for c in sorted {
        let name = user_of(c);
        let Some(id) = int(c, "id") else {
            continue;
        };
        if name.is_empty()
            || own.contains(&name.to_lowercase())
            || ignore.skips(&name, "conversation")
        {
            continue;
        }
        let body = body_of(c);
        if answered("conversation", id, &body) {
            continue;
        }
        rows.push(json!({
            "id": id,
            "kind": "conversation",
            "author": name,
            "path": null,
            "line": null,
            "text": {"untrusted": body},
            "thread": [{"id": id, "author": name, "text": {"untrusted": body}}],
        }));
    }
    rows
}

/// The pull request facts of design 11.1, with the review state of the in-review step.
///
/// The review state is the first of these that holds: `merged`, `closed`, `comments` (a comment
/// waits), `changes` (a reviewer asks for changes and is not asked again), `conflict` (the open
/// pull request conflicts with its base), `approved`, and `waiting`.
pub fn pr_facts(gh: &GitHub, pr: &Value, config: &Config) -> Res<(Map<String, Value>, String)> {
    let Some(number) = int(pr, "number") else {
        return Err(Error::Unexpected("KeyError: 'number'".into()));
    };
    let approvers = config.logins("approvers")?;
    let own = config.logins("own_logins")?;
    let ignore = Ignore::new(&config.logins("ignore_logins")?)?;
    let needed = config.number("approvals_needed", 1.0)?.trunc() as usize;
    let counts = |name: &str| {
        if approvers.is_empty() {
            !own.iter().any(|o| o == name)
        } else {
            approvers.iter().any(|a| a == name)
        }
    };
    let reviews = gh.reviews(number)?;
    let (last, shown) = last_states(&reviews);
    let mut approved_by: Vec<String> = last
        .iter()
        .filter(|(n, s)| s.as_str() == "APPROVED" && counts(n))
        .map(|(n, _)| shown[n].clone())
        .collect();
    approved_by.sort();
    let mut asked: Vec<String> = last
        .iter()
        .filter(|(n, s)| s.as_str() == "CHANGES_REQUESTED" && counts(n))
        .map(|(n, _)| shown[n].clone())
        .collect();
    asked.sort();
    let requested: BTreeSet<String> = match truthy(pr.get("requested_reviewers")) {
        Some(Value::Array(users)) => users
            .iter()
            .map(|u| login(Some(u)).to_lowercase())
            .collect(),
        _ => BTreeSet::new(),
    };
    // Each comment that waits, of each kind: the same rows as the job comments gives.
    let waiting = waiting_rows(
        &reviews,
        &gh.review_comments(number)?,
        &gh.issue_comments(number)?,
        &own,
        &ignore,
    );
    let merged = truthy(pr.get("merged")).is_some() || truthy(pr.get("merged_at")).is_some();
    let closed = text(pr, "state") == "closed" && !merged;
    let conflict =
        text(pr, "mergeable_state") == "dirty" || pr.get("mergeable") == Some(&Value::Bool(false));
    let head = truthy(pr.get("head")).cloned().unwrap_or(json!({}));
    let base = truthy(pr.get("base")).cloned().unwrap_or(json!({}));
    let mut facts = Map::new();
    facts.insert("number".into(), json!(number));
    facts.insert("draft".into(), json!(truthy(pr.get("draft")).is_some()));
    facts.insert(
        "head".into(),
        head.get("sha").cloned().unwrap_or(Value::Null),
    );
    facts.insert(
        "branch".into(),
        head.get("ref").cloned().unwrap_or(Value::Null),
    );
    facts.insert(
        "base".into(),
        base.get("ref").cloned().unwrap_or(Value::Null),
    );
    facts.insert("merged".into(), json!(merged));
    facts.insert("closed".into(), json!(closed));
    facts.insert("conflict".into(), json!(conflict));
    facts.insert("approved_by".into(), json!(approved_by));
    facts.insert("changes_requested".into(), json!(!asked.is_empty()));
    facts.insert("changes_by".into(), json!(asked));
    facts.insert("requested".into(), json!(requested));
    facts.insert("comments_waiting".into(), json!(waiting.len()));
    facts.insert(
        "url".into(),
        truthy(pr.get("html_url")).cloned().unwrap_or(json!("")),
    );
    let state = if merged {
        "merged"
    } else if closed {
        "closed"
    } else if !waiting.is_empty() {
        "comments"
    } else if asked.iter().any(|n| !requested.contains(&n.to_lowercase())) {
        "changes"
    } else if conflict {
        "conflict"
    } else if approved_by.len() >= needed && asked.is_empty() {
        "approved"
    } else {
        "waiting"
    };
    Ok((facts, state.to_string()))
}

/// True when the text names the key as a whole: no letter or digit before it, and no digit
/// after it. Any case.
pub fn names_key(text: &str, key: &str) -> bool {
    let hay: Vec<char> = text.chars().flat_map(char::to_lowercase).collect();
    let needle: Vec<char> = key.chars().flat_map(char::to_lowercase).collect();
    if needle.is_empty() || needle.len() > hay.len() {
        return needle.is_empty();
    }
    (0..=hay.len() - needle.len()).any(|i| {
        hay[i..i + needle.len()] == needle[..]
            && (i == 0 || !hay[i - 1].is_ascii_alphanumeric())
            && hay
                .get(i + needle.len())
                .is_none_or(|c| !c.is_ascii_digit())
    })
}

/// The commit of a field when it is a commit id, else `None`.
pub fn commit_field(value: &Value, key: &str) -> Option<String> {
    value
        .get(key)
        .and_then(Value::as_str)
        .filter(|s| is_commit(s))
        .map(str::to_string)
}
