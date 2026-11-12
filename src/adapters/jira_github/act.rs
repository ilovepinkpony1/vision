//! `act`: one verb, its reconcile form, or a read verb (design section 11.1).
//!
//! Each verb that changes something has a reconcile form: with `reconcile` set, it only reads
//! the outside state and answers confirmed, failed, or uncertain. It never acts again.
//!
//! A failure before the verb changed anything is `failed`. A failure after it can have changed
//! something, and each failure of a reconcile, is `uncertain`: the tick asks again.

use std::path::Path;
use std::time::{Duration, Instant};

use serde_json::{json, Map, Value};

use super::adf;
use super::core::{
    args_of, check_key, clean, commit_of, fail, now, object, optional_secret, pretty, py_list,
    py_opt, py_repr_str, py_str, refuse, secret, short, short_opt, splitlines, stamp, text_of,
    truthy, whole, Config, Error, Res, DEFAULT_GITHUB_API,
};
use super::forge::{self, GitHub};
use super::git::Git;
use super::http::{is_local, segment, split, Call};
use super::jira::{self, Jira};
use super::jobs;
use super::observe::pr_number;
use super::proc::TempDir;

/// Set `acted` right before the first call that changes the outside.
#[derive(Default)]
pub struct Acting {
    pub acted: bool,
}

type Verb = fn(&Config, &Map<String, Value>, bool, &mut Acting) -> Res<Value>;
type Read = fn(&Config, &Map<String, Value>) -> Res<Value>;

const VERBS: [(&str, Verb); 11] = [
    ("ci.rerun", ci_rerun),
    ("ci.run", ci_run),
    ("deploy", deploy),
    ("git.push", git_push),
    ("pr.open", pr_open),
    ("review.minimize", review_minimize),
    ("review.reply", review_reply),
    ("review.rerequest", review_rerequest),
    ("review.resolve", review_resolve),
    ("tracker.describe", tracker_describe),
    ("tracker.move", tracker_move),
];

const READS: [(&str, Read); 4] = [
    ("ci.log", ci_log_read),
    ("github.pr", github_pr),
    ("jira.issue", jira_issue),
    ("jira.search", jira_search),
];

pub fn act(call: &Map<String, Value>) -> Res<Value> {
    let config = Config::of(call);
    let verb = call.get("verb");
    let name = verb.and_then(Value::as_str).unwrap_or("");
    let shown = || py_repr_str(&clean(&py_opt(verb), 40));
    if truthy(call.get("read")).is_some() {
        let Some((_, read)) = READS.iter().find(|(n, _)| verb.is_some_and(|_| *n == name)) else {
            let names: Vec<&str> = READS.iter().map(|(n, _)| *n).collect();
            return fail(format!(
                "the adapter jira-github has no read verb {}. Its read verbs: {}",
                shown(),
                names.join(", ")
            ));
        };
        return read(&config, call);
    }
    let Some((_, run)) = VERBS.iter().find(|(n, _)| verb.is_some_and(|_| *n == name)) else {
        let names: Vec<&str> = VERBS.iter().map(|(n, _)| *n).collect();
        return fail(format!(
            "the adapter jira-github has no verb {}. Its verbs: {}",
            shown(),
            names.join(", ")
        ));
    };
    let reconcile = truthy(call.get("reconcile")).is_some();
    let mut state = Acting::default();
    match run(&config, call, reconcile, &mut state) {
        Err(Error::Http { text, status }) => {
            // A definite refusal of the server (400 to 499, but not a time limit or a rate
            // limit) means that the call changed nothing.
            if !reconcile && (400..500).contains(&status) && status != 408 && status != 429 {
                return Ok(answer("failed", &text, None, None, None));
            }
            let state_word = if reconcile || state.acted {
                "uncertain"
            } else {
                "failed"
            };
            Ok(answer(
                state_word,
                &format!("{text}. The outside state is not known"),
                None,
                None,
                None,
            ))
        }
        Err(Error::Failure(text)) | Err(Error::Unreachable(text)) => {
            if reconcile || state.acted {
                Ok(answer(
                    "uncertain",
                    &format!("{text}. The outside state is not known"),
                    None,
                    None,
                    None,
                ))
            } else {
                Ok(answer("failed", &text, None, None, None))
            }
        }
        other => other,
    }
}

fn answer(
    state: &str,
    detail: &str,
    external_id: Option<String>,
    values: Option<Value>,
    documents: Option<Value>,
) -> Value {
    let mut out = Map::new();
    out.insert("state".into(), json!(state));
    out.insert("detail".into(), json!(super::core::mask(detail)));
    if let Some(id) = external_id {
        out.insert("external_id".into(), json!(id));
    }
    if let Some(values) = values.filter(|v| truthy(Some(v)).is_some()) {
        out.insert("set".into(), values);
    }
    if let Some(documents) = documents.filter(|v| truthy(Some(v)).is_some()) {
        out.insert("documents".into(), documents);
    }
    Value::Object(out)
}

fn confirmed(detail: &str, external_id: impl Into<String>) -> Res<Value> {
    Ok(answer(
        "confirmed",
        detail,
        Some(external_id.into()),
        None,
        None,
    ))
}

fn failed(detail: &str) -> Res<Value> {
    Ok(answer("failed", detail, None, None, None))
}

fn uncertain(detail: &str) -> Res<Value> {
    Ok(answer("uncertain", detail, None, None, None))
}

/// True when two texts are the same but for white space at the ends of lines and of the text.
fn same_text(a: &str, b: &str) -> bool {
    fn norm(text: &str) -> String {
        splitlines(text.trim())
            .into_iter()
            .map(str::trim_end)
            .collect::<Vec<_>>()
            .join("\n")
    }
    norm(a) == norm(b)
}

// tracker.describe ------------------------------------------------------------------------------

const BACKUP_IN_DETAIL: usize = 600;

fn description_of(issue: &Value) -> Value {
    jira::fields_of(issue)
        .get("description")
        .cloned()
        .unwrap_or(Value::Null)
}

fn tracker_describe(
    config: &Config,
    call: &Map<String, Value>,
    reconcile: bool,
    state: &mut Acting,
) -> Res<Value> {
    let why = "tracker.describe";
    let key = check_key(call.get("key"), "the ticket key")?;
    let args = args_of(call);
    let text = match args.get("text") {
        Some(Value::String(t)) if !t.trim().is_empty() => t.clone(),
        _ => {
            return fail(
                "tracker.describe needs args.text: the new description, {doc:description.md}",
            )
        }
    };
    let new_doc = adf::to_adf(&text);
    let new_md = adf::to_markdown(&new_doc);
    let tracker = Jira::new(config, why)?;
    let current = description_of(&tracker.issue(&key, &["description"])?);
    let current_md = adf::to_markdown(&current);
    if same_text(&current_md, &new_md) {
        return confirmed(&format!("the description of {key} is the new one"), &key);
    }
    if reconcile {
        return failed(&format!("the description of {key} is not the new one"));
    }
    let seen = seen_description(args.get("seen"))?;
    if !same_text(&current_md, &seen) {
        return refuse(format!(
            "a person edited the description of {key} after Vision read it (Vision read {} characters, Jira has {} now), so the description stays as it is. Run `vision retry {key}` to read it again",
            seen.chars().count(),
            current_md.chars().count()
        ));
    }
    let name = format!("description-backup-{key}-{}.json", stamp());
    let backup = json!({"key": key, "at": now(), "markdown": current_md, "adf": current});
    state.acted = true;
    tracker.set_description(&key, &new_doc)?;
    let after = adf::to_markdown(&description_of(&tracker.issue(&key, &["description"])?));
    if !same_text(&after, &new_md) {
        return uncertain(&format!(
            "Jira took the new description of {key}, but it reads back a different text"
        ));
    }
    let mut detail = format!(
        "the description of {key} is replaced. The old description is in the document {name}"
    );
    let length = current_md.chars().count();
    if length <= BACKUP_IN_DETAIL {
        if current_md.is_empty() {
            detail.push_str(". The old description was empty");
        } else {
            detail.push_str(&format!(". The old text:\n{current_md}"));
        }
    } else {
        detail.push_str(&format!(
            " ({length} characters); Jira also keeps it in the history of the issue"
        ));
    }
    Ok(answer(
        "confirmed",
        &detail,
        Some(key),
        None,
        Some(json!([{"name": name, "content": format!("{}\n", pretty(&backup))}])),
    ))
}

/// The description in issue.json, as observe gave it at the entry of the step.
fn seen_description(seen: Option<&Value>) -> Res<String> {
    let data = match seen {
        Some(Value::String(text)) => serde_json::from_str::<Value>(text).map_err(|_| {
            Error::Failure(
                "args.seen of tracker.describe is not JSON: it must be {doc:issue.json}".into(),
            )
        })?,
        Some(other) => other.clone(),
        None => Value::Null,
    };
    if !data.is_object() {
        return fail(
            "tracker.describe needs args.seen: the issue that observe gave, {doc:issue.json}",
        );
    }
    let paths: [&[&str]; 3] = [&["issue", "untrusted"], &["issue"], &[]];
    for path in paths {
        let mut node = Some(&data);
        for part in path {
            node = node.and_then(|n| n.as_object()).and_then(|n| n.get(*part));
        }
        if let Some(Value::Object(map)) = node {
            if map.contains_key("description") {
                return Ok(map
                    .get("description")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string());
            }
        }
    }
    fail("args.seen of tracker.describe has no description: it must be {doc:issue.json}")
}

// tracker.move ----------------------------------------------------------------------------------

fn tracker_move(
    config: &Config,
    call: &Map<String, Value>,
    reconcile: bool,
    state: &mut Acting,
) -> Res<Value> {
    let why = "tracker.move";
    let key = check_key(call.get("key"), "the ticket key")?;
    let role = match args_of(call).get("status") {
        Some(Value::String(r)) if !r.is_empty() => r.clone(),
        _ => {
            return fail(
                "tracker.move needs args.status: a role of `statuses`, such as in_progress",
            )
        }
    };
    let statuses = config.need("statuses", why)?;
    let target = match statuses.get(&role) {
        Some(Value::String(t)) if statuses.is_object() && !t.trim().is_empty() => {
            t.trim().to_string()
        }
        _ => {
            return fail(format!(
                "tracker.move needs the key `statuses.{role}` in the [adapter] table: the status name of the role"
            ))
        }
    };
    let tracker = Jira::new(config, why)?;
    let now_status = tracker.status(&key)?;
    let external = format!("{key}:{target}");
    if now_status.to_lowercase() == target.to_lowercase() {
        return confirmed(&format!("{key} is in the status {target}"), external);
    }
    if reconcile {
        return failed(&format!(
            "{key} is in the status {now_status}, not {target}"
        ));
    }
    let transitions = tracker.transitions(&key)?;
    let lower = target.to_lowercase();
    let mut found: Vec<&Value> = transitions
        .iter()
        .filter(|t| jira::name_of(t.get("to"), "name").to_lowercase() == lower)
        .collect();
    if found.is_empty() {
        found = transitions
            .iter()
            .filter(|t| text_of(t.get("name")).to_lowercase() == lower)
            .collect();
    }
    let Some(chosen) = found.first() else {
        let listed = transitions
            .iter()
            .map(|t| {
                format!(
                    "{} -> {}",
                    py_opt(t.get("name")),
                    jira::name_of(t.get("to"), "name")
                )
            })
            .collect::<Vec<_>>()
            .join("; ");
        let listed = if listed.is_empty() {
            "none".to_string()
        } else {
            listed
        };
        return failed(&format!(
            "no transition of {key} leads from {now_status} to {target}. The transitions: {listed}"
        ));
    };
    state.acted = true;
    tracker.transition(&key, &py_opt(chosen.get("id")))?;
    let after = tracker.status(&key)?;
    if after.to_lowercase() != lower {
        return uncertain(&format!(
            "Jira took the transition of {key}, but the status reads {after}, not {target}"
        ));
    }
    confirmed(
        &format!("{key} moved from {now_status} to {target}"),
        external,
    )
}

// git.push --------------------------------------------------------------------------------------

/// True for a text that is not a valid branch name.
fn bad_ref(branch: &str) -> bool {
    branch.starts_with(['-', '/', '.'])
        || branch.contains("..")
        || branch
            .chars()
            .any(|c| c.is_whitespace() || "~^:?*[\\".contains(c))
        || branch.contains("@{")
        || branch.ends_with(['/', '.'])
        || super::core::line_end(branch).ends_with(".lock")
        || branch.contains("//")
}

/// The branch that git.push can push: the branch of the call, with the key, never the base.
fn check_push_branch(
    branch: Option<&Value>,
    key: &str,
    config: &Config,
    args: &Map<String, Value>,
) -> Res<String> {
    let branch = match branch {
        Some(Value::String(b)) if !b.is_empty() && !bad_ref(b) => b.clone(),
        other => {
            return refuse(format!(
                "git.push refuses the branch {}: it is not a branch name",
                py_repr_str(&clean(&text_of(other), 80))
            ))
        }
    };
    if args.contains_key("branch") && args.get("branch") != Some(&json!(branch)) {
        return refuse(format!(
            "git.push refuses the branch {}: it pushes only the branch of the call, {branch}",
            py_repr_str(&clean(&py_opt(args.get("branch")), 80))
        ));
    }
    let mut protected = vec![config.base_branch()?];
    protected.extend(config.envs()?.into_iter().map(|e| e.branch));
    if protected.contains(&branch) {
        return refuse(format!(
            "git.push refuses the branch {branch}: it is the base branch or the branch of an env"
        ));
    }
    if !forge::names_key(&branch, key) {
        return refuse(format!(
            "git.push refuses the branch {branch}: it does not hold the key {key}"
        ));
    }
    for flag in ["force", "force_with_lease", "delete", "mirror"] {
        if truthy(args.get(flag)).is_some() {
            return refuse(format!("git.push never uses {}", flag.replace('_', "-")));
        }
    }
    Ok(branch)
}

fn remote_url(config: &Config, why: &str) -> Res<String> {
    let url = match config.text("git_remote_url")? {
        Some(url) => url,
        None => {
            let api = config.text_or("github_api", DEFAULT_GITHUB_API)?;
            if api.trim_end_matches('/') != DEFAULT_GITHUB_API {
                return fail(format!(
                    "{why} needs the key `git_remote_url` in the [adapter] table"
                ));
            }
            format!("https://github.com/{}.git", config.repo(why)?)
        }
    };
    let parts = split(&url);
    if parts.scheme == "https" || (parts.scheme == "http" && is_local(&parts.host)) {
        if !parts.user.is_empty() || parts.password {
            return fail("`git_remote_url` must hold no user and no password");
        }
        return Ok(url);
    }
    if parts.scheme == "file" || (parts.scheme.is_empty() && Path::new(&url).is_absolute()) {
        return Ok(url);
    }
    fail("`git_remote_url` must be an https address or a local path, such as https://github.com/acme/widgets.git")
}

/// The one git configuration value that carries the token: a header for this address only.
fn auth_config(url: &str, token: &str) -> String {
    let value = crate::util::base64::encode(format!("x-access-token:{token}").as_bytes());
    format!("http.{url}.extraheader=Authorization: Basic {value}")
}

/// A redirect never takes the header to a different address.
const NO_REDIRECT: &str = "http.followRedirects=false";

fn temp(prefix: &str) -> Res<TempDir> {
    TempDir::new(prefix).map_err(|e| Error::Unexpected(format!("OSError: {e}")))
}

fn remote_head(url: &str, branch: &str, auth: &str) -> Res<Option<String>> {
    let tmp = temp("vision-ls-")?;
    let config = [NO_REDIRECT.to_string(), auth.to_string()];
    let reference = format!("refs/heads/{branch}");
    let output = Git::new(&["ls-remote", url, &reference], Some(&tmp.path))
        .unchecked()
        .timeout(60)
        .config(&config)
        .run()?;
    drop(tmp);
    if output.code != Some(0) {
        let text = output.stderr_text();
        let lines = splitlines(text.trim());
        let tail = lines[lines.len().saturating_sub(2)..].join(" ");
        return Err(Error::Unreachable(format!(
            "git ls-remote failed: {}",
            clean(&tail, 300)
        )));
    }
    for line in splitlines(&output.stdout_text()) {
        let (sha, rest) = line.split_once('\t').unwrap_or((line, ""));
        if rest.trim() == reference {
            return Ok(Some(sha.trim().to_string()));
        }
    }
    Ok(None)
}

fn git_push(
    config: &Config,
    call: &Map<String, Value>,
    reconcile: bool,
    state: &mut Acting,
) -> Res<Value> {
    let why = "git.push";
    let key = check_key(call.get("key"), "the ticket key")?;
    let args = args_of(call);
    let branch = check_push_branch(truthy(call.get("branch")), &key, config, &args)?;
    let Some(head) = commit_of(truthy(call.get("head"))).map(str::to_string) else {
        return fail("git.push needs the last commit in head");
    };
    let url = remote_url(config, why)?;
    let auth = auth_config(&url, &secret("FORGE_TOKEN", why)?);
    let pushed = || {
        Ok(answer(
            "confirmed",
            &format!("{branch} is at {} on the remote", short(&head)),
            Some(format!("{branch}@{}", short(&head))),
            Some(json!({ "head": head })),
            None,
        ))
    };
    let remote = remote_head(&url, &branch, &auth)?;
    if remote.as_deref() == Some(head.as_str()) {
        return pushed();
    }
    if reconcile {
        return failed(&format!(
            "{branch} is at {} on the remote, not at {}: the push did not happen",
            short_opt(remote.as_deref()),
            short(&head)
        ));
    }
    let clone = match call.get("clone") {
        Some(Value::String(c)) if Path::new(c).is_dir() => c.clone(),
        _ => return fail("git.push needs the clone of the ticket"),
    };
    let clone = Path::new(&clone);
    let on = Git::new(&["symbolic-ref", "--quiet", "--short", "HEAD"], Some(clone))
        .unchecked()
        .text()?;
    if on != branch {
        return refuse(format!(
            "git.push refuses: the clone is on {}, not on the branch of the call {branch}",
            py_repr_str(&clean(&on, 80))
        ));
    }
    let object = format!("{head}^{{commit}}");
    Git::new(&["cat-file", "-e", &object], Some(clone)).run()?;
    state.acted = true;
    let output = push_commit(clone, &url, &branch, &head, &auth)?;
    if output.code != Some(0) {
        let text = output.stderr_text();
        let lines: Vec<&str> = splitlines(&text)
            .into_iter()
            .filter(|l| !l.trim().is_empty() && !l.starts_with("hint:"))
            .collect();
        let header = auth.split_once('=').map(|(_, v)| v).unwrap_or("");
        let err = clean(&lines[lines.len().saturating_sub(4)..].join(" "), 300)
            .replace(header, "[header]");
        let all = lines.join(" ");
        if ["[rejected]", "non-fast-forward", "fetch first"]
            .iter()
            .any(|w| all.contains(w))
        {
            return failed(&format!(
                "the remote {branch} has commits that the clone does not have, and Vision never forces a push: {err}"
            ));
        }
        return uncertain(&format!("git push failed: {err}"));
    }
    if remote_head(&url, &branch, &auth)?.as_deref() != Some(head.as_str()) {
        return uncertain(&format!(
            "git push ended, but {branch} is not at {} on the remote",
            short(&head)
        ));
    }
    pushed()
}

/// Pushes `head` as `branch` from a new bare repo that borrows the objects of the clone, so
/// that no hook, no configuration, and no remote of the clone takes part, and the token never
/// reaches a file of the clone. The refspec has no `+`: a push never forces.
fn push_commit(
    clone: &Path,
    url: &str,
    branch: &str,
    head: &str,
    auth: &str,
) -> Res<super::proc::Output> {
    let objects = Git::new(&["rev-parse", "--git-path", "objects"], Some(clone)).text()?;
    let objects = if Path::new(&objects).is_absolute() {
        Path::new(&objects).to_path_buf()
    } else {
        clone.join(&objects)
    };
    let tmp = temp("vision-push-")?;
    let repo = tmp.path.join("push.git");
    let repo_text = repo.display().to_string();
    Git::new(&["init", "--bare", "--quiet", &repo_text], None).run()?;
    let real = std::fs::canonicalize(&objects).unwrap_or(objects);
    std::fs::write(
        repo.join("objects").join("info").join("alternates"),
        format!("{}\n", real.display()),
    )
    .map_err(|e| Error::Unexpected(format!("OSError: {e}")))?;
    let reference = format!("refs/heads/{branch}");
    Git::new(&["update-ref", &reference, head], Some(&repo)).run()?;
    let refspec = format!("{reference}:{reference}");
    let config = [NO_REDIRECT.to_string(), auth.to_string()];
    let output = Git::new(
        &["push", "--quiet", "--no-follow-tags", url, &refspec],
        Some(&repo),
    )
    .unchecked()
    .timeout(450)
    .config(&config)
    .run();
    drop(tmp);
    output
}

// pr.open ---------------------------------------------------------------------------------------

fn pr_text(text: Option<&Value>) -> Res<(String, String)> {
    let text = match text {
        Some(Value::String(t)) if !t.trim().is_empty() => t.trim().to_string(),
        _ => return fail("pr.open needs args.text: the text of pr.md, {doc:pr.md}"),
    };
    let lines = splitlines(&text);
    let title = lines[0].trim().trim_start_matches('#').trim().to_string();
    if title.is_empty() {
        return fail("the first line of pr.md is empty: it is the title of the pull request");
    }
    let title: String = title.chars().take(256).collect();
    Ok((title, lines[1..].join("\n").trim().to_string()))
}

fn pr_answer(pull: &Value, branch: &str) -> Res<Value> {
    let number = pull.get("number").cloned().unwrap_or(Value::Null);
    let mut values = json!({ "pr": number });
    if let Some(sha) = truthy(pull.get("head")).and_then(|h| commit_of(h.get("sha"))) {
        values["head"] = json!(sha);
    }
    Ok(answer(
        "confirmed",
        &format!("pull request #{} is open for {branch}", py_str(&number)),
        Some(py_str(&number)),
        Some(values),
        None,
    ))
}

fn open_pulls(gh: &GitHub, branch: &str) -> Res<Vec<Value>> {
    let mut pulls = gh.pulls_of(branch, "open")?;
    pulls.sort_by_key(|p| std::cmp::Reverse(forge::int(p, "number").unwrap_or(0)));
    Ok(pulls)
}

fn pr_open(
    config: &Config,
    call: &Map<String, Value>,
    reconcile: bool,
    state: &mut Acting,
) -> Res<Value> {
    let why = "pr.open";
    let branch_value = truthy(call.get("branch"));
    let branch = branch_value.map(py_str).unwrap_or_default();
    if branch.is_empty() || branch == config.base_branch()? {
        return refuse(format!(
            "pr.open refuses the branch {}",
            py_repr_str(&clean(&branch, 80))
        ));
    }
    // The token of the person, when there is one, so that the person is the author.
    let gh = GitHub::new(config, why, optional_secret("PR_TOKEN"))?;
    if let Some(pull) = open_pulls(&gh, &branch)?.first() {
        return pr_answer(pull, &branch);
    }
    if reconcile {
        return failed(&format!("no pull request is open for {branch}"));
    }
    let (title, body) = pr_text(args_of(call).get("text"))?;
    state.acted = true;
    let request = json!({
        "title": title, "head": branch, "base": config.base_branch()?, "body": body, "draft": true,
    });
    let created = gh.client.call(
        "POST",
        &gh.path("pulls"),
        &format!("open the pull request of {branch}"),
        Call {
            body: Some(&request),
            ok: &[201],
            ..Call::default()
        },
    );
    match created {
        Ok(response) => {
            let pull = response.json()?;
            pr_answer(&truthy(Some(&pull)).cloned().unwrap_or(json!({})), &branch)
        }
        Err(Error::Http { status: 422, text }) => {
            if let Some(pull) = gh.pulls_of(&branch, "open")?.first() {
                return pr_answer(pull, &branch);
            }
            failed(&format!(
                "GitHub refused the pull request of {branch}: {text}"
            ))
        }
        Err(e) => Err(e),
    }
}

// ci.run ----------------------------------------------------------------------------------------

/// Waits for `poll` seconds, or less when the time to `deadline` is shorter.
fn pause(poll: f64, deadline: Instant) {
    let left = deadline.saturating_duration_since(Instant::now());
    std::thread::sleep(Duration::from_secs_f64(poll).min(left));
}

fn ci_run(
    config: &Config,
    call: &Map<String, Value>,
    reconcile: bool,
    state: &mut Acting,
) -> Res<Value> {
    let why = "ci.run";
    let branch = text_of(call.get("branch"));
    let Some(head) = commit_of(truthy(call.get("head"))).map(str::to_string) else {
        return fail("ci.run needs the last commit in head");
    };
    let workflow = config.text_need("ci_workflow", why)?;
    let gh = GitHub::new(config, why, None)?;
    if let Some(run) = gh.last_run(Some(&head), &workflow)? {
        return run_found(&run, &workflow, &head);
    }
    if reconcile {
        return uncertain(&format!("no run of {workflow} at {} yet", short(&head)));
    }
    let remote = gh.branch_head(&branch)?;
    if remote.as_deref() != Some(head.as_str()) {
        return failed(&format!(
            "{branch} is at {} on GitHub, not at {}: push it first",
            short_opt(remote.as_deref()),
            short(&head)
        ));
    }
    state.acted = true;
    let body = json!({ "ref": branch });
    match gh.client.call(
        "POST",
        &gh.path(&format!(
            "actions/workflows/{}/dispatches",
            segment(&workflow)
        )),
        &format!("start the workflow {workflow} on {branch}"),
        Call {
            body: Some(&body),
            ok: &[204],
            ..Call::default()
        },
    ) {
        Ok(_) => {}
        Err(Error::Http { status, text }) if (400..500).contains(&status) => return failed(&text),
        Err(e) => return Err(e),
    }
    let find = config.number("ci_find_seconds", 20.0)?;
    let poll = config.number("ci_poll_seconds", 3.0)?;
    let deadline = Instant::now() + Duration::from_secs_f64(find);
    loop {
        if let Some(run) = gh.last_run(Some(&head), &workflow)? {
            return run_found(&run, &workflow, &head);
        }
        if Instant::now() >= deadline {
            break;
        }
        pause(poll, deadline);
    }
    uncertain(&format!(
        "the workflow {workflow} started on {branch}, but no run at {} showed up in {} s. The tick looks again",
        short(&head),
        whole(find)
    ))
}

fn run_found(run: &Value, workflow: &str, head: &str) -> Res<Value> {
    let id = py_opt(run.get("id"));
    confirmed(
        &format!(
            "the run {id} of {workflow} at {} is {}",
            short(head),
            forge::run_state(Some(run))
        ),
        format!("run-{id}"),
    )
}

// review.reply ----------------------------------------------------------------------------------

/// One row of verdicts.json: the comment id, the reply, and the kind when the row names one.
type Verdict = (i64, String, Option<String>);

/// The rows of verdicts.json: a JSON list, or an object with the list under `verdicts`.
fn verdict_rows(text: Option<&Value>) -> Res<Vec<Verdict>> {
    let mut rows = Vec::new();
    for (n, row) in verdict_items(text, "review.reply")?.iter().enumerate() {
        let id = comment_id(row, n)?;
        let reply = row
            .get("reply")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        rows.push((id, reply, row_kind(row)));
    }
    Ok(rows)
}

/// The kind of a row of verdicts.json, when it names one of [`forge::KINDS`].
fn row_kind(row: &Value) -> Option<String> {
    row.get("kind")
        .and_then(Value::as_str)
        .filter(|k| forge::KINDS.contains(k))
        .map(str::to_string)
}

/// The comment id of row `n` (from 0) of verdicts.json: a number, or a text of digits.
fn comment_id(row: &Value, n: usize) -> Res<i64> {
    let raw = row.as_object().and_then(|r| r.get("comment_id"));
    let id = match raw {
        Some(Value::Number(num)) => num.as_i64(),
        Some(Value::String(s)) => {
            let digits = s.trim().trim_start_matches('#');
            if !digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit()) {
                digits.parse().ok()
            } else {
                None
            }
        }
        _ => None,
    };
    id.ok_or_else(|| {
        Error::Failure(format!(
            "row {} of verdicts.json has no comment_id number",
            n + 1
        ))
    })
}

/// The rows of verdicts.json for the verb `why`: a JSON list, or an object with the list under
/// `verdicts`, maybe in a code fence.
fn verdict_items(text: Option<&Value>, why: &str) -> Res<Vec<Value>> {
    let data = match text {
        Some(Value::String(text)) => {
            let mut stripped = text.trim().to_string();
            if stripped.starts_with("```") {
                stripped = match stripped.split_once('\n') {
                    Some((_, rest)) => rest.to_string(),
                    None => String::new(),
                };
                stripped = stripped.trim_end().to_string();
                if let Some(rest) = stripped.strip_suffix("```") {
                    stripped = rest.to_string();
                }
            }
            serde_json::from_str::<Value>(&stripped).map_err(|_| {
                Error::Failure(format!(
                    "args.verdicts of {why} is not JSON: it must be {{doc:verdicts.json}}"
                ))
            })?
        }
        Some(other) => other.clone(),
        None => Value::Null,
    };
    let data = match data {
        Value::Object(map) => map.get("verdicts").cloned().unwrap_or(Value::Null),
        other => other,
    };
    let Value::Array(items) = data else {
        return fail(format!(
            "{why} needs args.verdicts: the JSON list of verdicts.json"
        ));
    };
    Ok(items)
}

/// Posts the reply of each row of verdicts.json in the right place: an inline comment gets a
/// reply in its thread, and a review or a conversation comment gets a new conversation comment
/// that quotes its first line and ends with its mark. The `kind` of a row (copied from
/// comments.json) names the place; with no kind, the id must be of one kind only.
fn review_reply(
    config: &Config,
    call: &Map<String, Value>,
    reconcile: bool,
    state: &mut Acting,
) -> Res<Value> {
    let why = "review.reply";
    let rows = verdict_rows(args_of(call).get("verdicts"))?;
    let own = config.logins("own_logins")?;
    if own.is_empty() {
        return fail("review.reply needs the key `own_logins` in the [adapter] table, so that it never posts a reply twice");
    }
    let gh = GitHub::new(config, why, None)?;
    let branch = text_of(call.get("branch"));
    let Some(pull) = gh.find_pr(pr_number(call.get("pr")), &branch)? else {
        let what = if branch.is_empty() {
            "the ticket".to_string()
        } else {
            branch
        };
        return failed(&format!("no pull request of {what}"));
    };
    let number = forge::int(&pull, "number")
        .ok_or_else(|| Error::Unexpected("KeyError: 'number'".into()))?;
    let mut groups = forge::threads(&gh.review_comments(number)?);
    let root_of: std::collections::BTreeMap<i64, i64> = groups
        .iter()
        .flat_map(|(root, items)| {
            items
                .iter()
                .filter_map(|c| forge::int(c, "id"))
                .map(move |id| (id, *root))
        })
        .collect();
    let reviews: std::collections::BTreeMap<i64, Value> = gh
        .reviews(number)?
        .into_iter()
        .filter_map(|r| forge::int(&r, "id").map(|id| (id, r)))
        .collect();
    let conversation = gh.issue_comments(number)?;
    let talk: std::collections::BTreeMap<i64, Value> = conversation
        .iter()
        .filter(|c| !own.contains(&forge::login(c.get("user")).to_lowercase()))
        .filter_map(|c| forge::int(c, "id").map(|id| (id, c.clone())))
        .collect();
    let (mut marks, _quotes) = forge::reply_marks(&conversation, &own);
    let (mut posted, mut skipped, mut missing, mut unanswered, mut unclear) =
        (Vec::new(), Vec::new(), Vec::new(), Vec::new(), Vec::new());
    for (id, reply, kind) in rows {
        if reply.trim().is_empty() {
            continue;
        }
        let known = |k: &str| match k {
            "inline" => root_of.contains_key(&id),
            "review" => reviews.contains_key(&id),
            _ => talk.contains_key(&id),
        };
        let places: Vec<&str> = forge::KINDS
            .iter()
            .copied()
            .filter(|k| kind.as_deref().is_none_or(|want| want == *k) && known(k))
            .collect();
        if places.is_empty() {
            missing.push(id);
            continue;
        }
        if places.len() > 1 {
            unclear.push(id);
            continue;
        }
        let place = places[0];
        let done = if place == "inline" {
            let root = root_of[&id];
            groups[&root].iter().any(|c| {
                own.contains(&forge::login(c.get("user")).to_lowercase())
                    && same_text(&text_of(c.get("body")), &reply)
            })
        } else {
            marks.contains(&(place.to_string(), id))
        };
        if done {
            skipped.push(id);
            continue;
        }
        if reconcile {
            unanswered.push(id);
            continue;
        }
        state.acted = true;
        if place == "inline" {
            let root = root_of[&id];
            let body = json!({ "body": reply });
            gh.client.call(
                "POST",
                &gh.path(&format!("pulls/{number}/comments/{root}/replies")),
                &format!("reply to the comment {id} of #{number}"),
                Call {
                    body: Some(&body),
                    ok: &[201],
                    ..Call::default()
                },
            )?;
            if let Some(items) = groups.get_mut(&root) {
                items.push(json!({"user": {"login": own[0]}, "body": reply}));
            }
        } else {
            let first = if place == "review" {
                forge::review_first_line(&reviews[&id])
            } else {
                forge::first_line(&text_of(talk[&id].get("body")), 200)
            };
            let quote = format!("> {}", forge::no_mentions(&first));
            let body = json!({ "body": forge::reply_body(place, id, &quote, &reply) });
            gh.client.call(
                "POST",
                &gh.path(&format!("issues/{number}/comments")),
                &format!("answer the {place} {id} of #{number}"),
                Call {
                    body: Some(&body),
                    ok: &[201],
                    ..Call::default()
                },
            )?;
            marks.insert((place.to_string(), id));
        }
        posted.push(id);
    }
    let external = format!("replies-{number}");
    let mut problems = String::new();
    if !missing.is_empty() {
        problems.push_str(&format!(
            ". Pull request #{number} has no comment {}",
            py_list(&missing)
        ));
    }
    if !unclear.is_empty() {
        problems.push_str(&format!(
            ". The ids {} name comments of more than one kind: give each row the kind of comments.json",
            py_list(&unclear)
        ));
    }
    if reconcile {
        if !missing.is_empty() || !unanswered.is_empty() || !unclear.is_empty() {
            return failed(&format!(
                "pull request #{number} has no reply for the comments {}{problems}",
                py_list(&unanswered)
            ));
        }
        return confirmed(&format!("each reply on #{number} is posted"), external);
    }
    let detail = format!(
        "on #{number}: posted the replies to {}, and skipped {}, which have the same reply already",
        py_list(&posted),
        py_list(&skipped)
    );
    if !missing.is_empty() || !unclear.is_empty() {
        return failed(&format!("{detail}{problems}"));
    }
    confirmed(&detail, external)
}

// review.resolve and review.minimize ------------------------------------------------------------

/// One row of verdicts.json with `settle`: the comment id, the kind when the row names one, and
/// the classifier that review.minimize gives GitHub.
struct Settle {
    id: i64,
    kind: Option<String>,
    classifier: &'static str,
}

/// The rows of verdicts.json whose `settle` is true, "resolved", or "outdated": a bot comment
/// that the fix settled. true is "resolved". A row with no `settle`, or with false, is left out.
fn settle_rows(text: Option<&Value>, why: &str) -> Res<Vec<Settle>> {
    let mut rows = Vec::new();
    for (n, row) in verdict_items(text, why)?.iter().enumerate() {
        let word = |w: &str| match row.get("settle") {
            Some(Value::String(text)) => text.trim().eq_ignore_ascii_case(w),
            _ => false,
        };
        let classifier = match row.get("settle") {
            None | Some(Value::Null) | Some(Value::Bool(false)) => continue,
            Some(Value::Bool(true)) => "RESOLVED",
            Some(_) if word("resolved") => "RESOLVED",
            Some(_) if word("outdated") => "OUTDATED",
            Some(other) => {
                let shown = py_repr_str(&clean(&py_str(other), 40));
                return fail(format!(
                    "row {} of verdicts.json has settle {shown}: use true, \"resolved\", or \"outdated\"",
                    n + 1
                ));
            }
        };
        rows.push(Settle {
            id: comment_id(row, n)?,
            kind: row_kind(row),
            classifier,
        });
    }
    Ok(rows)
}

/// True for the login of a bot: it ends with `[bot]`, or it is in `bots` (the lowercase logins
/// of `bot_logins`).
fn is_bot(login: &str, bots: &[String]) -> bool {
    let login = login.trim().to_lowercase();
    !login.is_empty() && (login.ends_with("[bot]") || bots.contains(&login))
}

/// A login for a text, or `nobody`.
fn shown_login(login: &str) -> String {
    if login.trim().is_empty() {
        "nobody".to_string()
    } else {
        clean(login, 60)
    }
}

/// Why review.resolve leaves a thread as it is: its first comment is not from a bot, or a
/// person who is not in `own` wrote in it. `None` when it can resolve the thread.
fn thread_refusal(items: &[Value], root: i64, bots: &[String], own: &[String]) -> Option<String> {
    let author = items
        .iter()
        .find(|c| forge::int(c, "id") == Some(root))
        .map(|c| forge::login(c.get("user")))
        .unwrap_or_default();
    if !is_bot(&author, bots) {
        return Some(format!("its author {} is not a bot", shown_login(&author)));
    }
    items.iter().find_map(|c| {
        let login = forge::login(c.get("user"));
        (!is_bot(&login, bots) && !own.contains(&login.to_lowercase()))
            .then(|| format!("{} wrote in it and is not a bot", shown_login(&login)))
    })
}

/// The pull request of the call: its number, or `None`.
fn pull_number(gh: &GitHub, call: &Map<String, Value>) -> Res<Option<i64>> {
    let branch = text_of(call.get("branch"));
    match gh.find_pr(pr_number(call.get("pr")), &branch)? {
        None => Ok(None),
        Some(pull) => forge::int(&pull, "number")
            .map(Some)
            .ok_or_else(|| Error::Unexpected("KeyError: 'number'".into())),
    }
}

/// The answer when the call names no pull request.
fn no_pull(call: &Map<String, Value>) -> Res<Value> {
    let branch = text_of(call.get("branch"));
    let what = if branch.is_empty() {
        "the ticket".to_string()
    } else {
        branch
    };
    failed(&format!("no pull request of {what}"))
}

/// The end of a detail that names each refused item and why.
fn refusals(refused: &[String]) -> String {
    if refused.is_empty() {
        String::new()
    } else {
        format!(
            ". Refused, because only a thread or a comment of a bot is touched: {}",
            refused.join("; ")
        )
    }
}

/// Resolves the review thread of each inline comment of verdicts.json with `settle`. A thread
/// is touched only when its first comment is from a bot and each other comment is from a bot or
/// from a login of own_logins; any other thread is refused, and the answer says so. A row of a
/// review or of a conversation comment has no thread, so the verb leaves it out.
fn review_resolve(
    config: &Config,
    call: &Map<String, Value>,
    reconcile: bool,
    state: &mut Acting,
) -> Res<Value> {
    let why = "review.resolve";
    let rows = settle_rows(args_of(call).get("verdicts"), why)?;
    let bots = config.logins("bot_logins")?;
    let own = config.logins("own_logins")?;
    let gh = GitHub::new(config, why, None)?;
    let Some(number) = pull_number(&gh, call)? else {
        return no_pull(call);
    };
    let external = format!("resolve-{number}");
    let candidates: Vec<&Settle> = rows
        .iter()
        .filter(|r| r.kind.as_deref().is_none_or(|k| k == "inline"))
        .collect();
    let groups = if candidates.is_empty() {
        Default::default()
    } else {
        forge::threads(&gh.review_comments(number)?)
    };
    let root_of: std::collections::BTreeMap<i64, i64> = groups
        .iter()
        .flat_map(|(root, items)| {
            items
                .iter()
                .filter_map(|c| forge::int(c, "id"))
                .map(move |id| (id, *root))
        })
        .collect();
    let mut threads: Option<Vec<forge::Thread>> = None;
    let mut seen = std::collections::BTreeSet::new();
    let (mut resolved, mut skipped, mut open, mut missing, mut refused) =
        (Vec::new(), Vec::new(), Vec::new(), Vec::new(), Vec::new());
    for row in candidates {
        let Some(&root) = root_of.get(&row.id) else {
            if row.kind.is_some() {
                missing.push(row.id);
            }
            continue;
        };
        if !seen.insert(root) {
            continue;
        }
        if let Some(why) = thread_refusal(&groups[&root], root, &bots, &own) {
            refused.push(format!("the thread of {}: {why}", row.id));
            continue;
        }
        if threads.is_none() {
            threads = Some(gh.review_threads(number)?);
        }
        let Some(thread) = threads
            .iter()
            .flatten()
            .find(|t| t.comments.contains(&root))
        else {
            missing.push(row.id);
            continue;
        };
        if thread.resolved {
            skipped.push(row.id);
            continue;
        }
        if reconcile {
            open.push(row.id);
            continue;
        }
        state.acted = true;
        let data = gh.graphql(
            forge::RESOLVE_MUTATION,
            json!({ "id": thread.id }),
            &format!("resolve the thread of the comment {} of #{number}", row.id),
        )?;
        if data.pointer("/resolveReviewThread/thread/isResolved") != Some(&Value::Bool(true)) {
            return uncertain(&format!(
                "GitHub did not say that the thread of the comment {} of #{number} is resolved. The outside state is not known",
                row.id
            ));
        }
        resolved.push(row.id);
    }
    let problems = if missing.is_empty() {
        String::new()
    } else {
        format!(
            ". Pull request #{number} has no review thread of the comments {}",
            py_list(&missing)
        )
    };
    if reconcile {
        if !open.is_empty() || !missing.is_empty() {
            return failed(&format!(
                "pull request #{number} has open threads of the comments {}{problems}",
                py_list(&open)
            ));
        }
        return confirmed(
            &format!(
                "each settled bot thread on #{number} is resolved{}",
                refusals(&refused)
            ),
            external,
        );
    }
    let detail = format!(
        "on #{number}: resolved the threads of {}, and skipped {}, which are resolved already{}",
        py_list(&resolved),
        py_list(&skipped),
        refusals(&refused)
    );
    if !missing.is_empty() {
        return failed(&format!("{detail}{problems}"));
    }
    confirmed(&detail, external)
}

/// Hides each comment of verdicts.json with `settle`: an inline comment, a review, or a
/// conversation comment, with the classifier of the row. A comment is touched only when its
/// author is a bot; any other comment is refused, and the answer says so. The `kind` of a row
/// names the place; with no kind, the id must be of one kind only.
fn review_minimize(
    config: &Config,
    call: &Map<String, Value>,
    reconcile: bool,
    state: &mut Acting,
) -> Res<Value> {
    let why = "review.minimize";
    let rows = settle_rows(args_of(call).get("verdicts"), why)?;
    let bots = config.logins("bot_logins")?;
    let gh = GitHub::new(config, why, None)?;
    let Some(number) = pull_number(&gh, call)? else {
        return no_pull(call);
    };
    let external = format!("minimize-{number}");
    let by_id = |items: Vec<Value>| -> std::collections::BTreeMap<i64, Value> {
        items
            .into_iter()
            .filter_map(|c| forge::int(&c, "id").map(|id| (id, c)))
            .collect()
    };
    let (inline, reviews, talk) = if rows.is_empty() {
        Default::default()
    } else {
        (
            by_id(gh.review_comments(number)?),
            by_id(gh.reviews(number)?),
            by_id(gh.issue_comments(number)?),
        )
    };
    let place = |kind: &str| match kind {
        "inline" => &inline,
        "review" => &reviews,
        _ => &talk,
    };
    let mut seen = std::collections::BTreeSet::new();
    let mut targets: Vec<(i64, String, &'static str)> = Vec::new();
    let (mut missing, mut unclear, mut refused) = (Vec::new(), Vec::new(), Vec::new());
    for row in &rows {
        let places: Vec<&str> = forge::KINDS
            .iter()
            .copied()
            .filter(|k| {
                row.kind.as_deref().is_none_or(|want| want == *k) && place(k).contains_key(&row.id)
            })
            .collect();
        let kind = match places.as_slice() {
            [] => {
                missing.push(row.id);
                continue;
            }
            [kind] => *kind,
            _ => {
                unclear.push(row.id);
                continue;
            }
        };
        if !seen.insert((kind, row.id)) {
            continue;
        }
        let item = &place(kind)[&row.id];
        let author = forge::login(item.get("user"));
        if !is_bot(&author, &bots) {
            refused.push(format!(
                "the {kind} {}: its author {} is not a bot",
                row.id,
                shown_login(&author)
            ));
            continue;
        }
        let node = forge::text(item, "node_id");
        if node.is_empty() {
            missing.push(row.id);
            continue;
        }
        targets.push((row.id, node.to_string(), row.classifier));
    }
    let hidden_now = if targets.is_empty() {
        Default::default()
    } else {
        let ids: Vec<String> = targets.iter().map(|(_, node, _)| node.clone()).collect();
        gh.minimized(&ids)?
    };
    let (mut hidden, mut skipped, mut open) = (Vec::new(), Vec::new(), Vec::new());
    for (id, node, classifier) in &targets {
        if hidden_now.get(node) == Some(&true) {
            skipped.push(*id);
            continue;
        }
        if reconcile {
            open.push(*id);
            continue;
        }
        state.acted = true;
        let data = gh.graphql(
            forge::MINIMIZE_MUTATION,
            json!({ "id": node, "classifier": classifier }),
            &format!("hide the comment {id} of #{number}"),
        )?;
        if data.pointer("/minimizeComment/minimizedComment/isMinimized") != Some(&Value::Bool(true))
        {
            return uncertain(&format!(
                "GitHub did not say that the comment {id} of #{number} is hidden. The outside state is not known"
            ));
        }
        hidden.push(*id);
    }
    let mut problems = String::new();
    if !missing.is_empty() {
        problems.push_str(&format!(
            ". Pull request #{number} has no comment {}",
            py_list(&missing)
        ));
    }
    if !unclear.is_empty() {
        problems.push_str(&format!(
            ". The ids {} name comments of more than one kind: give each row the kind of comments.json",
            py_list(&unclear)
        ));
    }
    if reconcile {
        if !open.is_empty() || !missing.is_empty() || !unclear.is_empty() {
            return failed(&format!(
                "pull request #{number} still shows the comments {}{problems}",
                py_list(&open)
            ));
        }
        return confirmed(
            &format!(
                "each settled bot comment on #{number} is hidden{}",
                refusals(&refused)
            ),
            external,
        );
    }
    let detail = format!(
        "on #{number}: hid the comments {}, and skipped {}, which are hidden already{}",
        py_list(&hidden),
        py_list(&skipped),
        refusals(&refused)
    );
    if !missing.is_empty() || !unclear.is_empty() {
        return failed(&format!("{detail}{problems}"));
    }
    confirmed(&detail, external)
}

// ci.rerun --------------------------------------------------------------------------------------

/// The run and its attempt before the rerun, from the external id that the first answer
/// stored: `run-<id>-attempt-<n>`, or `run-<id>`. (None, None) for anything else.
fn stored_run(external_id: Option<&Value>) -> (Option<i64>, Option<i64>) {
    let Some(text) = external_id.and_then(Value::as_str) else {
        return (None, None);
    };
    let text = super::core::line_end(text);
    let digits =
        |s: &str, max: usize| (1..=max).contains(&s.len()) && s.bytes().all(|b| b.is_ascii_digit());
    let Some(rest) = text.strip_prefix("run-") else {
        return (None, None);
    };
    let (id, attempt) = match rest.split_once("-attempt-") {
        Some((id, attempt)) => (id, Some(attempt)),
        None => (rest, None),
    };
    if !digits(id, 20) || attempt.is_some_and(|a| !digits(a, 6)) {
        return (None, None);
    }
    (id.parse().ok(), attempt.and_then(|a| a.parse().ok()))
}

/// An RFC 3339 time of GitHub or of the core, or `None`.
fn utc(text: Option<&Value>) -> Option<time::OffsetDateTime> {
    let text = text.and_then(Value::as_str)?.trim();
    if text.is_empty() {
        return None;
    }
    crate::util::time::parse_rfc3339(text).ok()
}

/// Whether the rerun of this operation happened: (yes, how). The attempt of the run before the
/// call, when the first answer stored it, decides. Else the start of the last attempt against
/// the time of the first call: an attempt that started before the call is a rerun that a person
/// started by hand. With neither fact, an attempt above 1 counts.
fn rerun_happened(run: &Value, before: Option<i64>, started: Option<&Value>) -> (bool, String) {
    let attempt = attempt_of(run);
    if let Some(before) = before {
        return (
            attempt > before,
            format!("attempt {attempt}, and the call found attempt {before}"),
        );
    }
    let at_text = py_opt(run.get("run_started_at"));
    let call_text = py_opt(started);
    if let (Some(at), Some(call)) = (utc(run.get("run_started_at")), utc(started)) {
        if at >= call {
            return (
                attempt > 1,
                format!("attempt {attempt} started at {at_text}, after the call at {call_text}"),
            );
        }
        if attempt > 1 {
            return (
                false,
                format!(
                    "attempt {attempt} started at {at_text}, before the call at {call_text}: a person ran it by hand"
                ),
            );
        }
        return (
            false,
            format!(
                "the run is still its first attempt, which started at {at_text}, before the call at {call_text}"
            ),
        );
    }
    (
        attempt > 1,
        format!("attempt {attempt}, and no time of the call to compare"),
    )
}

fn attempt_of(run: &Value) -> i64 {
    truthy(run.get("run_attempt"))
        .and_then(Value::as_i64)
        .unwrap_or(1)
}

fn ci_rerun(
    config: &Config,
    call: &Map<String, Value>,
    reconcile: bool,
    state: &mut Acting,
) -> Res<Value> {
    let why = "ci.rerun";
    let Some(head) = commit_of(truthy(call.get("head"))).map(str::to_string) else {
        return fail("ci.rerun needs the last commit in head");
    };
    let workflow = config.text_need("ci_workflow", why)?;
    let gh = GitHub::new(config, why, None)?;
    let (known_id, before) = if reconcile {
        stored_run(call.get("external_id"))
    } else {
        (None, None)
    };
    let run = match known_id {
        Some(id) => {
            let run = gh.client.get(
                &gh.path(&format!("actions/runs/{id}")),
                &format!("read the run {id}"),
            )?;
            truthy(Some(&run)).cloned()
        }
        None => gh.last_run(Some(&head), &workflow)?,
    };
    let Some(run) = run else {
        return failed(&format!(
            "no run of {workflow} at {}, so nothing runs again",
            short(&head)
        ));
    };
    let run_id = py_opt(run.get("id"));
    let external = format!("run-{run_id}");
    let attempt = attempt_of(&run);
    let status = forge::run_state(Some(&run));
    if reconcile {
        let (happened, how) = rerun_happened(&run, before, call.get("started"));
        if happened {
            return confirmed(
                &format!("the run {run_id} ran again: {how}; it is {status}"),
                external,
            );
        }
        if status == "green" {
            return confirmed(
                &format!(
                    "the run {run_id} is green (attempt {attempt}), so nothing has to run again"
                ),
                external,
            );
        }
        return failed(&format!(
            "the failed jobs of the run {run_id} did not run again for this operation: {how}"
        ));
    }
    if status == "running" {
        return confirmed(
            &format!("the run {run_id} runs (attempt {attempt})"),
            external,
        );
    }
    if status == "green" {
        return confirmed(
            &format!("the run {run_id} is green (attempt {attempt}), so nothing runs again"),
            external,
        );
    }
    let id = forge::int(&run, "id")
        .ok_or_else(|| Error::Unexpected("TypeError: the run has no id".into()))?;
    state.acted = true;
    match gh.client.call(
        "POST",
        &gh.path(&format!("actions/runs/{id}/rerun-failed-jobs")),
        &format!("run the failed jobs of the run {run_id} again"),
        Call {
            ok: &[201, 204],
            ..Call::default()
        },
    ) {
        Ok(_) => {}
        Err(Error::Http { status, text }) if (400..500).contains(&status) => return failed(&text),
        Err(e) => return Err(e),
    }
    let find = config.number("ci_find_seconds", 20.0)?;
    let poll = config.number("ci_poll_seconds", 3.0)?;
    let deadline = Instant::now() + Duration::from_secs_f64(find);
    loop {
        let now = gh.client.get(
            &gh.path(&format!("actions/runs/{id}")),
            &format!("read the run {run_id}"),
        )?;
        let now = truthy(Some(&now)).cloned().unwrap_or(json!({}));
        let now_attempt = truthy(now.get("run_attempt")).and_then(Value::as_i64);
        if now.get("status").and_then(Value::as_str) != Some("completed")
            || now_attempt.unwrap_or(1) > attempt
        {
            return confirmed(
                &format!(
                    "the failed jobs of the run {run_id} run again (attempt {})",
                    now_attempt.unwrap_or(attempt + 1)
                ),
                external,
            );
        }
        if Instant::now() >= deadline {
            // The attempt before the call goes with the answer, so that the reconcile knows
            // which attempt is new.
            return Ok(answer(
                "uncertain",
                &format!(
                    "GitHub took the rerun of the run {run_id}, but it did not start in {} s. The tick looks again",
                    whole(find)
                ),
                Some(format!("run-{run_id}-attempt-{attempt}")),
                None,
                None,
            ));
        }
        pause(poll, deadline);
    }
}

// deploy ----------------------------------------------------------------------------------------

/// Merges the head commit into the branch of an env, with no checkout.
fn deploy(
    config: &Config,
    call: &Map<String, Value>,
    reconcile: bool,
    state: &mut Acting,
) -> Res<Value> {
    let why = "deploy";
    let key = check_key(call.get("key"), "the ticket key")?;
    let Some(head) = commit_of(truthy(call.get("head"))).map(str::to_string) else {
        return fail("deploy needs the last commit in head");
    };
    let name = match args_of(call).get("env") {
        Some(Value::String(n)) if !n.is_empty() => n.clone(),
        _ => return fail("deploy needs args.env: the name of an env of `envs`"),
    };
    let Some(env) = config.envs()?.into_iter().find(|e| e.name == name) else {
        return fail(format!(
            "deploy needs the env {name} in `envs` of the [adapter] table, with its branch"
        ));
    };
    let branch = env.branch;
    if branch == config.base_branch()? || bad_ref(&branch) {
        return refuse(format!(
            "deploy refuses the branch {} of the env {name}: it merges only into a staging branch",
            py_repr_str(&branch)
        ));
    }
    let gh = GitHub::new(config, why, None)?;
    let external = format!("deploy-{name}-{}", short(&head));
    match gh.holds(&branch, &head)? {
        None => {
            return failed(&format!(
                "the branch {branch} of the env {name} does not exist on GitHub"
            ))
        }
        Some(true) => {
            return confirmed(
                &format!("{branch} already holds {}", short(&head)),
                external,
            )
        }
        Some(false) => {}
    }
    if reconcile {
        return failed(&format!(
            "{branch} does not hold {}: the merge did not happen",
            short(&head)
        ));
    }
    state.acted = true;
    let body = json!({
        "base": branch,
        "head": head,
        "commit_message": format!("Merge {key} ({}) into {branch}", short(&head)),
    });
    let response = gh.client.call(
        "POST",
        &gh.path("merges"),
        &format!("merge {} into {branch}", short(&head)),
        Call {
            body: Some(&body),
            ok: &[201, 204, 404, 409],
            ..Call::default()
        },
    )?;
    match response.status {
        201 => {
            let data = response.json()?;
            let sha = text_of(truthy(Some(&data)).and_then(|d| d.get("sha")));
            confirmed(
                &format!(
                    "merged {} into {branch} as {} (201)",
                    short(&head),
                    short(&sha)
                ),
                external,
            )
        }
        204 => confirmed(
            &format!("{branch} already held {} (204)", short(&head)),
            external,
        ),
        409 => {
            let reason = match response.json() {
                Ok(Value::Object(map)) => text_of(map.get("message")),
                _ => String::new(),
            };
            let message = clean(&reason, 300);
            let tail = if message.is_empty() {
                String::new()
            } else {
                format!(": {message}")
            };
            failed(&format!(
                "conflict: {} does not merge into {branch}{tail}. Merge it by hand, or fix the branch",
                short(&head)
            ))
        }
        _ => failed(&format!(
            "GitHub has no branch {branch} or no commit {} (404)",
            short(&head)
        )),
    }
}

// review.rerequest ------------------------------------------------------------------------------

/// Asks each approver whose last review asked for changes for a new review.
fn review_rerequest(
    config: &Config,
    call: &Map<String, Value>,
    reconcile: bool,
    state: &mut Acting,
) -> Res<Value> {
    let why = "review.rerequest";
    let gh = GitHub::new(config, why, None)?;
    let branch = text_of(call.get("branch"));
    let Some(pull) = gh.find_pr(pr_number(call.get("pr")), &branch)? else {
        let what = if branch.is_empty() {
            "the ticket".to_string()
        } else {
            branch
        };
        return failed(&format!("no pull request of {what}"));
    };
    let number = forge::int(&pull, "number")
        .ok_or_else(|| Error::Unexpected("KeyError: 'number'".into()))?;
    let (facts, _state) = forge::pr_facts(&gh, &pull, config)?;
    let names = |key: &str| -> Vec<String> {
        facts
            .get(key)
            .and_then(Value::as_array)
            .map(|items| {
                items
                    .iter()
                    .filter_map(Value::as_str)
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default()
    };
    let asked = names("changes_by");
    let requested = names("requested");
    let todo: Vec<String> = asked
        .iter()
        .filter(|n| !requested.contains(&n.to_lowercase()))
        .cloned()
        .collect();
    let external = format!("rerequest-{number}");
    if asked.is_empty() {
        return confirmed(
            &format!("no approver asked for changes on #{number}"),
            external,
        );
    }
    if todo.is_empty() {
        return confirmed(
            &format!(
                "a new review of {} is requested on #{number}",
                asked.join(", ")
            ),
            external,
        );
    }
    if reconcile {
        return failed(&format!(
            "no new review of {} is requested on #{number}",
            todo.join(", ")
        ));
    }
    state.acted = true;
    let body = json!({ "reviewers": todo });
    match gh.client.call(
        "POST",
        &gh.path(&format!("pulls/{number}/requested_reviewers")),
        &format!("ask {} for a new review of #{number}", todo.join(", ")),
        Call {
            body: Some(&body),
            ok: &[200, 201],
            ..Call::default()
        },
    ) {
        Ok(_) => {}
        Err(Error::Http { status: 422, text }) => return failed(&text),
        Err(e) => return Err(e),
    }
    confirmed(
        &format!("asked {} for a new review of #{number}", todo.join(", ")),
        external,
    )
}

// Read verbs ------------------------------------------------------------------------------------

const SEARCH_MAX: i64 = 50;

fn read_doc(name: &str, data: &Value, detail: &str) -> Res<Value> {
    Ok(json!({
        "detail": detail,
        "documents": [{"name": name, "content": format!("{}\n", pretty(data))}],
    }))
}

fn jira_issue(config: &Config, call: &Map<String, Value>) -> Res<Value> {
    let args = args_of(call);
    let key = check_key(
        truthy(args.get("key")).or(call.get("key")),
        "the ticket key",
    )?;
    let tracker = Jira::new(config, "jira.issue")?;
    let view = tracker.view(&tracker.issue(&key, &jira::ISSUE_FIELDS)?, true)?;
    read_doc(
        &format!("issue-{key}.json"),
        &json!({"key": key, "issue": {"untrusted": view}}),
        &format!("the issue {key}"),
    )
}

fn jira_search(config: &Config, call: &Map<String, Value>) -> Res<Value> {
    let args = args_of(call);
    let jql = match args.get("jql") {
        Some(Value::String(j)) if !j.trim().is_empty() => j.clone(),
        _ => {
            return fail("jira.search needs args.jql, such as \"project = DEMO AND text ~ export\"")
        }
    };
    let limit = match args.get("limit") {
        None => 20,
        Some(Value::Number(n)) => n.as_i64().filter(|l| *l >= 1).unwrap_or(20),
        Some(_) => 20,
    };
    let tracker = Jira::new(config, "jira.search")?;
    let issues = tracker.search(
        &jql,
        &["summary", "status", "issuetype"],
        limit.min(SEARCH_MAX) as usize,
    )?;
    let rows: Vec<Value> = issues
        .iter()
        .map(|i| {
            let fields = jira::fields_of(i);
            json!({
                "key": i.get("key").cloned().unwrap_or(Value::Null),
                "title": truthy(fields.get("summary")).cloned().unwrap_or(json!("")),
                "status": jira::name_of(fields.get("status"), "name"),
                "type": jira::name_of(fields.get("issuetype"), "name"),
            })
        })
        .collect();
    let count = rows.len();
    read_doc(
        "jira-search.json",
        &json!({"jql": jql, "issues": {"untrusted": rows}}),
        &format!("{count} issue(s)"),
    )
}

fn github_pr(config: &Config, call: &Map<String, Value>) -> Res<Value> {
    let args = args_of(call);
    let number = truthy(args.get("number")).or(call.get("pr"));
    let number = match number {
        None | Some(Value::Null) => None,
        Some(Value::Number(n)) if n.as_i64().is_some() => n.as_i64(),
        Some(_) => return fail("github.pr needs args.number: a pull request number"),
    };
    let gh = GitHub::new(config, "github.pr", None)?;
    let branch = text_of(call.get("branch"));
    let Some(pull) = gh.find_pr(number, &branch)? else {
        let what = if branch.is_empty() {
            "the ticket".to_string()
        } else {
            branch
        };
        return fail(format!("no pull request of {what}"));
    };
    let n = forge::int(&pull, "number")
        .ok_or_else(|| Error::Unexpected("KeyError: 'number'".into()))?;
    let (facts, review) = forge::pr_facts(&gh, &pull, config)?;
    let reviews: Vec<Value> = gh
        .reviews(n)?
        .iter()
        .filter(|r| r.get("state").and_then(Value::as_str) != Some("PENDING"))
        .map(|r| {
            json!({
                "author": forge::login(r.get("user")),
                "state": r.get("state").cloned().unwrap_or(Value::Null),
                "text": truthy(r.get("body")).cloned().unwrap_or(json!("")),
            })
        })
        .collect();
    let threads: Vec<Value> = forge::threads(&gh.review_comments(n)?)
        .values()
        .map(|items| {
            Value::Array(
                items
                    .iter()
                    .map(|c| {
                        json!({
                            "id": c.get("id").cloned().unwrap_or(Value::Null),
                            "author": forge::login(c.get("user")),
                            "path": c.get("path").cloned().unwrap_or(Value::Null),
                            "line": truthy(c.get("line")).or(c.get("original_line")).cloned().unwrap_or(Value::Null),
                            "text": truthy(c.get("body")).cloned().unwrap_or(json!("")),
                        })
                    })
                    .collect(),
            )
        })
        .collect();
    let conversation: Vec<Value> = gh
        .issue_comments(n)?
        .iter()
        .map(|c| {
            json!({
                "id": c.get("id").cloned().unwrap_or(Value::Null),
                "author": forge::login(c.get("user")),
                "text": truthy(c.get("body")).cloned().unwrap_or(json!("")),
            })
        })
        .collect();
    let data = object(vec![
        ("pr", Value::Object(facts)),
        ("review_state", json!(review)),
        (
            "untrusted",
            json!({
                "title": truthy(pull.get("title")).cloned().unwrap_or(json!("")),
                "body": truthy(pull.get("body")).cloned().unwrap_or(json!("")),
                "reviews": reviews,
                "threads": threads,
                "conversation": conversation,
            }),
        ),
    ]);
    read_doc(
        &format!("pr-{n}.json"),
        &data,
        &format!("the pull request #{n}"),
    )
}

fn ci_log_read(config: &Config, call: &Map<String, Value>) -> Res<Value> {
    let args = args_of(call);
    let run_id = match args.get("run") {
        None | Some(Value::Null) => None,
        Some(Value::Number(n)) if n.as_i64().is_some() => n.as_i64(),
        Some(_) => {
            return fail("ci.log needs args.run: a run id, or nothing for the last run at the head")
        }
    };
    let head = text_of(call.get("head"));
    if run_id.is_none() && !super::core::is_commit(&head) {
        return fail("ci.log needs the head commit, or args.run");
    }
    let workflow = config.text_need("ci_workflow", "ci.log")?;
    let gh = GitHub::new(config, "ci.log", None)?;
    let keep = (config.number("log_tail_kb", 64.0)? * 1024.0).trunc() as usize;
    let (content, detail) = jobs::ci_log_text(&gh, config, &head, &workflow, keep, run_id)?;
    let name = match run_id.filter(|r| *r != 0) {
        Some(run) => format!("ci-log-{run}.txt"),
        None => format!("ci-log-{}.txt", short(&head)),
    };
    Ok(json!({"detail": detail, "documents": [{"name": name, "content": content}]}))
}
