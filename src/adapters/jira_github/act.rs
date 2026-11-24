//! `act`: one verb, its reconcile form, or a read verb (design section 11.1).
//!
//! Each verb that changes something has a reconcile form: with `reconcile` set, it only reads
//! the outside state and answers confirmed, failed, or uncertain. It never acts again.
//!
//! A failure before the verb changed anything is `failed`. A failure after it can have changed
//! something, and each failure of a reconcile, is `uncertain`: the tick asks again.

use std::path::{Path, PathBuf};
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

const VERBS: [(&str, Verb); 13] = [
    ("ci.rerun", ci_rerun),
    ("ci.run", ci_run),
    ("deploy", deploy),
    ("deploy.fix", deploy_fix),
    ("deploy.merge", deploy_merge),
    ("git.push", git_push),
    ("pr.open", pr_open),
    ("review.minimize", review_minimize),
    ("review.reply", review_reply),
    ("review.rerequest", review_rerequest),
    ("review.resolve", review_resolve),
    ("tracker.describe", tracker_describe),
    ("tracker.move", tracker_move),
];

/// The verbs that write to the pull request of the ticket or to its branch. Each one refuses
/// while the pull request of the call is merged or closed: see [`closed_pr`].
const PR_WRITES: [&str; 7] = [
    "ci.rerun",
    "ci.run",
    "git.push",
    "review.minimize",
    "review.reply",
    "review.rerequest",
    "review.resolve",
];

const READS: [(&str, Read); 5] = [
    ("ci.log", ci_log_read),
    ("github.pr", github_pr),
    ("jira.issue", jira_issue),
    ("jira.search", jira_search),
    ("review.read", review_read),
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
    let guarded = !reconcile && PR_WRITES.contains(&name);
    let outcome = match guarded.then(|| closed_pr(&config, call, name)).transpose() {
        Ok(Some(Some(refusal))) => failed(&refusal),
        Ok(_) => run(&config, call, reconcile, &mut state),
        Err(e) => Err(e),
    };
    match outcome {
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

/// The refusal of the write `verb` when the pull request of the call is merged or closed, so
/// that a write that waited, for example for an approval, never lands on a pull request that
/// is done. A call with no pull request, or with one that GitHub does not know, gets `None`.
fn closed_pr(config: &Config, call: &Map<String, Value>, verb: &str) -> Res<Option<String>> {
    let Some(number) = pr_number(call.get("pr")).filter(|n| *n > 0) else {
        return Ok(None);
    };
    let gh = GitHub::new(config, verb, None)?;
    let pull = match gh.pull(number) {
        Ok(pull) => pull,
        Err(Error::Http { status: 404, .. }) => return Ok(None),
        Err(e) => return Err(e),
    };
    if pull.get("state").and_then(Value::as_str) != Some("closed") {
        return Ok(None);
    }
    let merged =
        pull.get("merged") == Some(&Value::Bool(true)) || truthy(pull.get("merged_at")).is_some();
    let how = if merged { "merged" } else { "closed" };
    Ok(Some(format!(
        "the pull request #{number} is {how}, so {verb} does not write to it or to its branch"
    )))
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
pub(super) fn bad_ref(branch: &str) -> bool {
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

/// The branch that git.push can push: the branch of the call, never the base. The branch holds
/// the key, or it is the head of the open pull request of the call, whose title holds the key.
/// The queue gives such a branch for a ticket that a person started outside Vision.
fn check_push_branch(
    call: &Map<String, Value>,
    key: &str,
    config: &Config,
    args: &Map<String, Value>,
) -> Res<String> {
    let branch = truthy(call.get("branch"));
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
    for flag in ["force", "force_with_lease", "delete", "mirror"] {
        if truthy(args.get(flag)).is_some() {
            return refuse(format!("git.push never uses {}", flag.replace('_', "-")));
        }
    }
    if !forge::names_key(&branch, key) && !pull_names_key(config, call, &branch, key)? {
        return refuse(format!(
            "git.push refuses the branch {branch}: it does not hold the key {key}, and no open \
             pull request of the call has it as its head and {key} in its title"
        ));
    }
    Ok(branch)
}

/// True when the open pull request of the call has `branch` of the repo itself as its head and
/// `key` in its title. A branch of a fork has the same name, but it is another branch.
fn pull_names_key(
    config: &Config,
    call: &Map<String, Value>,
    branch: &str,
    key: &str,
) -> Res<bool> {
    let Some(number) = pr_number(call.get("pr")).filter(|n| *n > 0) else {
        return Ok(false);
    };
    let repo = config.repo("git.push")?;
    let pull = match GitHub::new(config, "git.push", None)?.pull(number) {
        Ok(pull) => pull,
        Err(Error::Http { status: 404, .. }) => return Ok(false),
        Err(e) => return Err(e),
    };
    let head = pull.get("head");
    let from = head
        .and_then(|h| h.get("repo"))
        .and_then(|r| r.get("full_name"))
        .and_then(Value::as_str)
        .unwrap_or("");
    Ok(pull.get("state").and_then(Value::as_str) == Some("open")
        && head.and_then(|h| h.get("ref")).and_then(Value::as_str) == Some(branch)
        && from.eq_ignore_ascii_case(&repo)
        && forge::names_key(&text_of(pull.get("title")), key))
}

pub(super) fn remote_url(config: &Config, why: &str) -> Res<String> {
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
pub(super) fn auth_config(url: &str, token: &str) -> String {
    let value = crate::util::base64::encode(format!("x-access-token:{token}").as_bytes());
    format!("http.{url}.extraheader=Authorization: Basic {value}")
}

/// A redirect never takes the header to a different address.
pub(super) const NO_REDIRECT: &str = "http.followRedirects=false";

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
    let branch = check_push_branch(call, &key, config, &args)?;
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

/// The title and the body of a new pull request, from `args.text`. With `{doc?:pr.md}`, a ticket
/// with no pr.md gives an empty text, which is fine only while a pull request is open.
fn pr_text(text: Option<&Value>, branch: &str) -> Res<(String, String)> {
    let text = match text {
        Some(Value::String(t)) if !t.trim().is_empty() => t.trim().to_string(),
        _ => {
            return fail(format!(
                "no pull request is open for {branch}, and the ticket has no text for a new one: write pr.md in the ticket folder, with the title on the first line, then retry the ticket"
            ))
        }
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
    let (title, body) = pr_text(args_of(call).get("text"), &branch)?;
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
/// in the form of the skill resolve-review, `(Claude) Re-review #<id>: <reply>` or
/// `(Claude) Re #<id>: <reply>`, that ends with its mark. The `kind` of a row (copied from
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
            let body = json!({ "body": forge::reply_body(place, id, &reply) });
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
    // With args.env, the run is the one of the tip of the env branch, which the push of a
    // deploy started: not the run of the head of the ticket.
    let (commit, place) = match env_of(config, call, why)? {
        Some(env) => match gh.branch_head(&env.branch)? {
            Some(tip) => (tip, format!(" (the tip of {})", env.branch)),
            None => {
                return failed(&format!(
                    "the branch {} of the env {} does not exist on GitHub",
                    env.branch, env.name
                ))
            }
        },
        None => (head.clone(), String::new()),
    };
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
        None => gh.last_run(Some(&commit), &workflow)?,
    };
    let Some(run) = run else {
        return failed(&format!(
            "no run of {workflow} at {}{place}, so nothing runs again",
            short(&commit)
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

/// The env of `args.env` of a call, or `None` when the call names no env. A name that `envs`
/// of the [adapter] table does not list is a failure.
pub(super) fn env_of(
    config: &Config,
    call: &Map<String, Value>,
    why: &str,
) -> Res<Option<super::core::Env>> {
    let name = match args_of(call).get("env") {
        None | Some(Value::Null) => return Ok(None),
        Some(Value::String(n)) if !n.trim().is_empty() => n.trim().to_string(),
        Some(other) => {
            return fail(format!(
                "{why} needs args.env as the name of an env of `envs`, not {}",
                py_repr_str(&clean(&py_str(other), 40))
            ))
        }
    };
    match config.envs()?.into_iter().find(|e| e.name == name) {
        Some(env) => Ok(Some(env)),
        None => fail(format!(
            "{why} needs the env {name} in `envs` of the [adapter] table, with its branch"
        )),
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
            let text = format!(
                "conflict: {} does not merge into {branch}{tail}",
                short(&head)
            );
            // The merge starts in a worktree of the clone, so that an agent can resolve it.
            match prepare_merge(config, call, &key, &name, &branch, &head) {
                Ok(merge) => Ok(conflict_answer(&key, &name, &branch, &head, &text, &merge)),
                Err(e) => failed(&format!(
                    "{text}. The merge did not start in the clone: {e}. Merge it by hand, or fix the branch"
                )),
            }
        }
        _ => failed(&format!(
            "GitHub has no branch {branch} or no commit {} (404)",
            short(&head)
        )),
    }
}

/// The document of a deploy that stopped on a merge conflict.
const CONFLICT_DOC: &str = "deploy-conflict.md";

/// The file in the git folder of a merge worktree that lists the files with a conflict.
const CONFLICT_LIST: &str = "vision-conflicts";

/// A merge that waits in its worktree for an agent.
struct Merge {
    /// The tip of the env branch that the merge starts from.
    tip: String,
    /// The folder of the worktree, relative to the clone.
    folder: String,
    message: String,
    files: Vec<String>,
    said: String,
}

/// The folder of the merge of an env, relative to the clone: inside `.vision/`, which the
/// clone ignores.
fn merge_folder(env: &str) -> String {
    format!(".vision/merge/{env}")
}

/// An env name that can be a folder name.
fn plain_name(name: &str) -> bool {
    !name.is_empty()
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_'))
}

fn clone_of(call: &Map<String, Value>) -> Option<PathBuf> {
    match call.get("clone") {
        Some(Value::String(c)) if Path::new(c).is_dir() => Some(PathBuf::from(c)),
        _ => None,
    }
}

/// The message of the merge of the head of a ticket into the branch of an env.
fn merge_message(key: &str, head: &str, branch: &str) -> String {
    format!("Merge {key} ({}) into {branch}", short(head))
}

/// `user.name` and `user.email` from the `git` object of the call (the identity of the commits
/// of a session), as values for [`Git::config`]. With no such object, git makes its own guess,
/// and that guess fails on a host whose name has no domain.
pub(super) fn identity_settings(call: &Map<String, Value>) -> Vec<String> {
    let text = |key: &str| {
        call.get("git")?
            .get(key)?
            .as_str()
            .map(str::trim)
            .filter(|t| !t.is_empty() && !t.contains(['\n', '\r', '\0']))
            .map(str::to_string)
    };
    match (text("name"), text("email")) {
        (Some(name), Some(email)) => {
            vec![format!("user.name={name}"), format!("user.email={email}")]
        }
        _ => Vec::new(),
    }
}

/// The non-empty lines of a text, trimmed, such as the file names that git gives.
pub(super) fn names(text: &str) -> Vec<String> {
    splitlines(text)
        .into_iter()
        .map(str::trim)
        .filter(|f| !f.is_empty())
        .map(str::to_string)
        .collect()
}

/// True when a merge waits in the worktree `work`: git has a MERGE_HEAD there.
pub(super) fn merge_waits(work: &Path) -> Res<bool> {
    let found = Git::new(
        &["rev-parse", "--quiet", "--verify", "MERGE_HEAD"],
        Some(work),
    )
    .unchecked()
    .run()?;
    Ok(found.code == Some(0))
}

/// The path of the list of the files with a conflict, in the git folder of the worktree `work`.
fn conflict_list_path(work: &Path) -> Res<PathBuf> {
    git_file(work, CONFLICT_LIST)
}

/// The path of a file of Vision in the git folder of the worktree `work`, where no commit and
/// no sandbox of a turn reaches it.
fn git_file(work: &Path, name: &str) -> Res<PathBuf> {
    let list = Git::new(&["rev-parse", "--git-path", name], Some(work)).text()?;
    Ok(if Path::new(&list).is_absolute() {
        PathBuf::from(list)
    } else {
        work.join(list)
    })
}

/// The files with a conflict of the merge in `work`, from the list that `deploy` wrote. Empty
/// when the list is gone.
fn conflict_list(work: &Path) -> Res<Vec<String>> {
    Ok(names(
        &std::fs::read_to_string(conflict_list_path(work)?).unwrap_or_default(),
    ))
}

/// Starts the merge of `head` into the env branch in a worktree of the clone, so that an agent
/// can resolve its conflicts: fetches the tip of the branch as `refs/vision/deploy/<env>`, makes
/// the worktree `.vision/merge/<env>` at that tip (an old one goes first), and runs `git merge`
/// with no commit. The worktree has no branch, so no branch of the clone changes.
fn prepare_merge(
    config: &Config,
    call: &Map<String, Value>,
    key: &str,
    env: &str,
    branch: &str,
    head: &str,
) -> Res<Merge> {
    let why = "deploy";
    let Some(clone) = clone_of(call) else {
        return fail("the call names no clone of the ticket");
    };
    if !plain_name(env) {
        return fail(format!("the env name `{env}` cannot name a folder"));
    }
    let url = remote_url(config, why)?;
    let settings = [
        NO_REDIRECT.to_string(),
        auth_config(&url, &secret("FORGE_TOKEN", why)?),
    ];
    let reference = format!("refs/vision/deploy/{env}");
    let refspec = format!("+refs/heads/{branch}:{reference}");
    Git::new(
        &[
            "fetch",
            "--quiet",
            "--no-tags",
            "--no-recurse-submodules",
            &url,
            &refspec,
        ],
        Some(&clone),
    )
    .timeout(450)
    .config(&settings)
    .run()?;
    let tip = Git::new(
        &["rev-parse", "--verify", &format!("{reference}^{{commit}}")],
        Some(&clone),
    )
    .text()?;
    let folder = merge_folder(env);
    remove_merge(&clone, &folder);
    std::fs::create_dir_all(clone.join(".vision").join("merge"))
        .map_err(|e| Error::Unexpected(format!("OSError: {e}")))?;
    Git::new(
        &["worktree", "add", "--detach", "--quiet", &folder, &tip],
        Some(&clone),
    )
    .timeout(600)
    .run()?;
    let work = clone.join(&folder);
    let message = merge_message(key, head, branch);
    // git refuses to start a merge with no committer, also with --no-commit, and it cannot
    // guess one on a host whose name has no domain.
    let identity = identity_settings(call);
    let merged = Git::new(
        &["merge", "--no-ff", "--no-commit", "-m", &message, head],
        Some(&work),
    )
    .unchecked()
    .timeout(600)
    .config(&identity)
    .run()?;
    let files = names(&Git::new(&["diff", "--name-only", "--diff-filter=U"], Some(&work)).text()?);
    if files.is_empty() {
        let started = merge_waits(&work)?;
        remove_merge(&clone, &folder);
        if !started && merged.code != Some(0) {
            let said = format!("{}{}", merged.stdout_text(), merged.stderr_text());
            return fail(format!(
                "git merge of {} did not start: {}",
                short(head),
                clean(said.trim(), 300)
            ));
        }
        return fail(format!(
            "{branch} is at {} now, and {} merges into it with no conflict. Run the step again",
            short(&tip),
            short(head)
        ));
    }
    std::fs::write(
        conflict_list_path(&work)?,
        format!("{}\n", files.join("\n")),
    )
    .map_err(|e| Error::Unexpected(format!("OSError: {e}")))?;
    let said = format!("{}{}", merged.stdout_text(), merged.stderr_text());
    Ok(Merge {
        tip,
        folder,
        message,
        files,
        said: clean(said.trim(), 4000),
    })
}

/// Removes the merge worktree of an env, if there is one.
fn remove_merge(clone: &Path, folder: &str) {
    let _ = Git::new(&["worktree", "remove", "--force", folder], Some(clone))
        .unchecked()
        .run();
    let _ = Git::new(&["worktree", "prune"], Some(clone))
        .unchecked()
        .run();
    let path = clone.join(folder);
    let vision = clone.join(".vision");
    if (path.starts_with(vision.join("merge")) || path.starts_with(vision.join("fix")))
        && path.exists()
    {
        let _ = std::fs::remove_dir_all(&path);
    }
}

/// The failed answer of a deploy that stopped on a merge conflict: `conflict` true, and a
/// document for the agent that resolves it.
fn conflict_answer(
    key: &str,
    env: &str,
    branch: &str,
    head: &str,
    text: &str,
    merge: &Merge,
) -> Value {
    let files = merge
        .files
        .iter()
        .map(|f| format!("- `{f}`"))
        .collect::<Vec<_>>()
        .join("\n");
    let content = format!(
        "# The deploy of {key} to {env} stopped on a merge conflict\n\n\
         The head of the ticket does not merge into the branch of the env. Vision started the merge in a git worktree inside the clone, and the merge waits there with its conflicts.\n\n\
         - Env: {env}\n\
         - Branch of the env: `{branch}`\n\
         - Tip of that branch: `{tip}`\n\
         - Head of the ticket: `{head}`\n\
         - Merge folder: `{folder}`\n\
         - Merge message: `{message}`\n\n\
         ## Files with a conflict\n\n{files}\n\n\
         Resolve each of these files in the merge folder, and change no other file. Do not stage or commit: after the turn, `deploy.merge` stages these files, commits the merge with the message above, and pushes it.\n\n\
         ## What git merge said\n\n```text\n{said}\n```\n",
        tip = merge.tip,
        folder = merge.folder,
        message = merge.message,
        said = merge.said,
    );
    let shown: Vec<&str> = merge.files.iter().take(5).map(String::as_str).collect();
    let more = merge.files.len().saturating_sub(shown.len());
    let mut list = shown.join(", ");
    if more > 0 {
        list.push_str(&format!(" and {more} more"));
    }
    let mut out = answer(
        "failed",
        &format!(
            "{text}. The merge waits in {} with a conflict in {list}",
            merge.folder
        ),
        None,
        None,
        Some(json!([{"name": CONFLICT_DOC, "content": content}])),
    );
    if let Value::Object(map) = &mut out {
        map.insert("conflict".into(), json!(true));
    }
    out
}

// deploy.merge ----------------------------------------------------------------------------------

/// Commits and pushes the merge that an agent resolved in the merge folder of an env (see
/// `deploy`) to the branch of the env. The sandbox of a turn cannot write the git folder of a
/// worktree, so the agent only edits the files with a conflict, and this verb stages them and
/// commits the merge. A merge that a person committed by hand goes as it is. It pushes only a
/// merge of the head into the tip that the branch still has, so the push is a fast-forward,
/// and never a merge that holds a conflict marker in a file that had a conflict. After the
/// push, the merge folder goes away.
fn deploy_merge(
    config: &Config,
    call: &Map<String, Value>,
    reconcile: bool,
    state: &mut Acting,
) -> Res<Value> {
    let why = "deploy.merge";
    let key = check_key(call.get("key"), "the ticket key")?;
    let Some(head) = commit_of(truthy(call.get("head"))).map(str::to_string) else {
        return fail("deploy.merge needs the last commit in head");
    };
    let name = match args_of(call).get("env") {
        Some(Value::String(n)) if !n.is_empty() => n.clone(),
        _ => return fail("deploy.merge needs args.env: the name of an env of `envs`"),
    };
    let Some(env) = config.envs()?.into_iter().find(|e| e.name == name) else {
        return fail(format!(
            "deploy.merge needs the env {name} in `envs` of the [adapter] table, with its branch"
        ));
    };
    let branch = env.branch;
    if branch == config.base_branch()? || bad_ref(&branch) || !plain_name(&name) {
        return refuse(format!(
            "deploy.merge refuses the branch {} of the env {name}: it pushes only into a staging branch",
            py_repr_str(&branch)
        ));
    }
    let gh = GitHub::new(config, why, None)?;
    let external = format!("deploy-merge-{name}-{}", short(&head));
    let clone = clone_of(call);
    let folder = merge_folder(&name);
    match gh.holds(&branch, &head)? {
        None => {
            return failed(&format!(
                "the branch {branch} of the env {name} does not exist on GitHub"
            ))
        }
        Some(true) => {
            if let Some(clone) = &clone {
                remove_merge(clone, &folder);
            }
            return confirmed(
                &format!("{branch} already holds {}", short(&head)),
                external,
            );
        }
        Some(false) => {}
    }
    if reconcile {
        return failed(&format!(
            "{branch} does not hold {}: the push of the merge did not happen",
            short(&head)
        ));
    }
    let Some(clone) = clone else {
        return fail("deploy.merge needs the clone of the ticket");
    };
    let work = clone.join(&folder);
    if !work.is_dir() {
        return failed(&format!(
            "no merge waits in {folder}. Run the deploy step again, so that it starts the merge"
        ));
    }
    if merge_waits(&work)? {
        let message = merge_message(&key, &head, &branch);
        if let Some(refused) = commit_merge(call, &work, &folder, &message)? {
            return failed(&refused);
        }
    }
    let merge = Git::new(&["rev-parse", "--verify", "HEAD^{commit}"], Some(&work)).text()?;
    let line = Git::new(&["rev-list", "--parents", "-n", "1", &merge], Some(&work)).text()?;
    let parents: Vec<&str> = line.split_whitespace().skip(1).collect();
    if parents.len() != 2 || parents[1] != head {
        return failed(&format!(
            "the commit {} in {folder} is not the merge of {} into {branch}: its parents are {}",
            short(&merge),
            short(&head),
            if parents.is_empty() {
                "none".to_string()
            } else {
                parents
                    .iter()
                    .map(|p| short(p))
                    .collect::<Vec<_>>()
                    .join(", ")
            }
        ));
    }
    let tip = parents[0].to_string();
    if let Some(marker) = conflict_marker(&work, &merge)? {
        return failed(&format!(
            "the merge {} still holds a conflict marker at {marker}. Resolve it, then commit the merge again",
            short(&merge)
        ));
    }
    let url = remote_url(config, why)?;
    let auth = auth_config(&url, &secret("FORGE_TOKEN", why)?);
    let remote = remote_head(&url, &branch, &auth)?;
    if remote.as_deref() != Some(tip.as_str()) {
        remove_merge(&clone, &folder);
        return failed(&format!(
            "{branch} moved from {} to {} after the merge started. Run the deploy step again, so that it merges the new tip",
            short(&tip),
            short_opt(remote.as_deref())
        ));
    }
    state.acted = true;
    let output = push_commit(&clone, &url, &branch, &merge, &auth)?;
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
            remove_merge(&clone, &folder);
            return failed(&format!(
                "{branch} moved after the merge started, and Vision never forces a push: {err}. Run the deploy step again, so that it merges the new tip"
            ));
        }
        return uncertain(&format!("git push of the merge failed: {err}"));
    }
    if remote_head(&url, &branch, &auth)?.as_deref() != Some(merge.as_str()) {
        return uncertain(&format!(
            "git push ended, but {branch} is not at the merge {} on the remote",
            short(&merge)
        ));
    }
    remove_merge(&clone, &folder);
    confirmed(
        &format!(
            "pushed the merge {} of {} into {branch}",
            short(&merge),
            short(&head)
        ),
        external,
    )
}

/// Commits the merge that waits in `work` after an agent edited its files with a conflict:
/// stages those files and commits with `message` and the identity of the call. It refuses a
/// file that still holds a conflict marker, and a change to a file that had no conflict.
/// Gives the reason of a refusal, or `None` after the commit.
fn commit_merge(
    call: &Map<String, Value>,
    work: &Path,
    folder: &str,
    message: &str,
) -> Res<Option<String>> {
    let files = conflict_list(work)?;
    if files.is_empty() {
        return Ok(Some(format!(
            "the merge in {folder} is not committed, and the list of its files with a conflict is gone. Run the deploy step again, so that it starts the merge"
        )));
    }
    if let Some(marker) = working_marker(work, &files) {
        return Ok(Some(format!(
            "the merge in {folder} still holds a conflict marker at {marker}. Resolve it, then run the step again"
        )));
    }
    let other: Vec<String> = names(&Git::new(&["diff", "--name-only"], Some(work)).text()?)
        .into_iter()
        .filter(|f| !files.contains(f))
        .collect();
    if !other.is_empty() {
        return Ok(Some(format!(
            "the merge in {folder} changes {}, which had no conflict. Undo that change, then run the step again",
            other.join(", ")
        )));
    }
    let mut add = vec!["add", "-A", "--"];
    add.extend(files.iter().map(String::as_str));
    Git::new(&add, Some(work)).run()?;
    let open = names(&Git::new(&["diff", "--name-only", "--diff-filter=U"], Some(work)).text()?);
    if !open.is_empty() {
        return Ok(Some(format!(
            "the merge in {folder} still has a conflict in {}. Resolve it, then run the step again",
            open.join(", ")
        )));
    }
    let mut settings = identity_settings(call);
    settings.push("commit.gpgsign=false".into());
    Git::new(
        &["commit", "--quiet", "--no-verify", "-m", message],
        Some(work),
    )
    .config(&settings)
    .run()?;
    Ok(None)
}

/// The first conflict marker in the work tree of `work`, in a file of `files`, as
/// `path:line`. A file that is gone or binary has none.
fn working_marker(work: &Path, files: &[String]) -> Option<String> {
    for file in files {
        let Ok(bytes) = std::fs::read(work.join(file)) else {
            continue;
        };
        if bytes.contains(&0) {
            continue;
        }
        let text = String::from_utf8_lossy(&bytes);
        if let Some(index) = text.lines().position(is_marker) {
            return Some(format!("{file}:{}", index + 1));
        }
    }
    None
}

/// A line that starts a side or ends a conflict, as `git merge` writes it.
fn is_marker(line: &str) -> bool {
    ["<<<<<<<", ">>>>>>>"].iter().any(|mark| {
        line.strip_prefix(mark)
            .is_some_and(|rest| rest.is_empty() || rest.starts_with(' '))
    })
}

/// The first conflict marker of the merge commit in a file that had a conflict, as
/// `path:line`, or `None`. With no list of those files, each file that the merge changed
/// counts.
fn conflict_marker(work: &Path, merge: &str) -> Res<Option<String>> {
    let mut files = conflict_list(work)?;
    if files.is_empty() {
        let first = format!("{merge}^1");
        files = names(&Git::new(&["diff", "--name-only", &first, merge], Some(work)).text()?);
    }
    if files.is_empty() {
        return Ok(None);
    }
    let mut args = vec![
        "grep",
        "-n",
        "-I",
        "-E",
        "^(<<<<<<<|>>>>>>>)( |$)",
        merge,
        "--",
    ];
    args.extend(files.iter().map(String::as_str));
    let found = Git::new(&args, Some(work)).unchecked().run()?;
    if found.code != Some(0) {
        return Ok(None);
    }
    let text = found.stdout_text();
    let first = splitlines(&text).first().copied().unwrap_or("").to_string();
    // `git grep` names the commit first: `<commit>:<path>:<line>:<text>`.
    let place: Vec<&str> = first.splitn(4, ':').collect();
    Ok(Some(match place.as_slice() {
        [_, path, line, _] => format!("{path}:{line}"),
        _ => clean(&first, 120),
    }))
}

// env-fix and deploy.fix ------------------------------------------------------------------------

/// The document of the job env-fix for the agent that fixes the CI of an env branch.
const ENV_FIX_DOC: &str = "env-fix.md";

/// The file in the git folder of a fix worktree that names the tip of the env branch where the
/// fix starts.
const FIX_TIP: &str = "vision-fix-tip";

/// The folder of the fix of the CI of an env, relative to the clone: inside `.vision/`, which
/// the clone ignores.
fn fix_folder(env: &str) -> String {
    format!(".vision/fix/{env}")
}

/// The env of a job or verb that writes to an env branch: its name and its branch. It refuses
/// the base branch, a name that cannot be a folder, and a branch name that git refuses.
fn staging_env(
    config: &Config,
    call: &Map<String, Value>,
    why: &str,
    word: &str,
) -> Res<(String, String)> {
    let Some(env) = env_of(config, call, why)? else {
        return fail(format!(
            "{why} needs args.env: the name of an env of `envs`"
        ));
    };
    if env.branch == config.base_branch()? || bad_ref(&env.branch) || !plain_name(&env.name) {
        return refuse(format!(
            "{why} refuses the branch {} of the env {}: it {word} only a staging branch",
            py_repr_str(&env.branch),
            env.name
        ));
    }
    Ok((env.name, env.branch))
}

/// A relative path with no `..`, no root, and no empty part, as an entry of `links`.
fn plain_relative(path: &str) -> bool {
    let p = Path::new(path);
    !path.trim().is_empty()
        && p.components()
            .all(|c| matches!(c, std::path::Component::Normal(_)))
}

/// Links each entry of `links` of the job table from the clone into the fix worktree, so that
/// the tests of the project can run there: files that git ignores, such as a generated
/// configuration or the installed packages. An entry that the clone lacks, that the worktree
/// already has, or that git does not ignore in the worktree (so that a commit could take it)
/// gets no link. Gives the linked entries and the reasons of the others.
fn link_ignored(clone: &Path, work: &Path, links: &[String]) -> (Vec<String>, Vec<String>) {
    let (mut linked, mut skipped) = (Vec::new(), Vec::new());
    for entry in links {
        let entry = entry.trim().trim_end_matches('/');
        if !plain_relative(entry) {
            skipped.push(format!("`{entry}`: not a plain relative path"));
            continue;
        }
        let (source, target) = (clone.join(entry), work.join(entry));
        if !source.exists() {
            skipped.push(format!("`{entry}`: the clone has none"));
            continue;
        }
        if target.symlink_metadata().is_ok() {
            skipped.push(format!("`{entry}`: the branch has its own"));
            continue;
        }
        let ignored = Git::new(&["check-ignore", "-q", "--no-index", entry], Some(work))
            .unchecked()
            .run()
            .is_ok_and(|o| o.code == Some(0));
        if !ignored {
            skipped.push(format!("`{entry}`: git does not ignore it"));
            continue;
        }
        if let Some(parent) = target.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        match std::os::unix::fs::symlink(&source, &target) {
            Ok(()) => linked.push(entry.to_string()),
            Err(e) => skipped.push(format!("`{entry}`: {e}")),
        }
    }
    (linked, skipped)
}

/// The files that the change of the ticket touches: the diff of the base ref of the adapter
/// and the head, in the clone. `None` when git cannot tell.
fn ticket_files(config: &Config, clone: &Path, head: &str) -> Option<Vec<String>> {
    let base = config.base_ref.as_deref().map(str::trim).filter(|b| {
        !b.is_empty()
            && !b.starts_with('-')
            && !b.contains("..")
            && !b.contains(char::is_whitespace)
    })?;
    let range = format!("{base}...{head}");
    let out = Git::new(&["diff", "--name-only", &range, "--"], Some(clone))
        .unchecked()
        .run()
        .ok()?;
    (out.code == Some(0)).then(|| names(&out.stdout_text()))
}

/// The keys of the table of the job env-fix: `links`, the entries that git ignores and that the
/// job links from the clone into the fix worktree.
pub static ENV_FIX_SETTINGS: crate::adapters::JobSettings = crate::adapters::JobSettings {
    keys: &["links"],
    check: check_env_fix,
    env: crate::adapter::JobEnv::Plain,
    no_secrets: false,
    timeout: env_fix_timeout,
};

/// The time limit of the job env-fix when its table has none: the fetch can take 450 s and the
/// worktree 600 s, but each one usually takes seconds.
fn env_fix_timeout(_: &Map<String, Value>) -> (u64, u64) {
    (900, 0)
}

/// The problems of `links` of the job env-fix: it is a list of plain relative paths.
fn check_env_fix(settings: &Map<String, Value>) -> Vec<String> {
    let example = "such as links = [\"config/database.yml\", \"node_modules\"]";
    match settings.get("links") {
        None => Vec::new(),
        Some(Value::Array(items)) => items
            .iter()
            .filter_map(|item| match item.as_str() {
                Some(text) if plain_relative(text.trim().trim_end_matches('/')) => None,
                _ => Some(format!(
                    "links has {}, which is not a plain relative path of the clone, {example}",
                    clean(&py_str(item), 60)
                )),
            })
            .collect(),
        Some(_) => vec![format!(
            "links must be a list of paths of the clone, {example}"
        )],
    }
}

/// The job env-fix: the CI of the tip of an env branch is red, so an agent fixes it. The job
/// fetches the tip as `refs/vision/fix/<env>`, makes the worktree `.vision/fix/<env>` at that
/// tip with no branch (an old one goes first), links the ignored files of `links` of the job
/// table into it, and writes env-fix.md and ci-log.txt: the run, the end of the log of each
/// failed job, the files of the change of the ticket, and what to do. The job never commits
/// and never pushes: deploy.fix does that after the turn.
pub(super) fn env_fix(config: &Config, call: &Map<String, Value>) -> Res<Value> {
    let why = "the job env-fix";
    let Some(clone) = clone_of(call) else {
        return fail(format!("{why} needs the clone of the ticket"));
    };
    let key = check_key(call.get("key"), "the ticket key")?;
    let (name, branch) = staging_env(config, call, why, "fixes")?;
    let head = commit_of(truthy(call.get("head")))
        .map(str::to_string)
        .unwrap_or_default();
    let workflow = config.text_need("ci_workflow", why)?;
    let url = remote_url(config, why)?;
    let settings = [
        NO_REDIRECT.to_string(),
        auth_config(&url, &secret("FORGE_TOKEN", why)?),
    ];
    let reference = format!("refs/vision/fix/{name}");
    let refspec = format!("+refs/heads/{branch}:{reference}");
    Git::new(
        &[
            "fetch",
            "--quiet",
            "--no-tags",
            "--no-recurse-submodules",
            &url,
            &refspec,
        ],
        Some(&clone),
    )
    .timeout(450)
    .config(&settings)
    .run()?;
    let tip = Git::new(
        &["rev-parse", "--verify", &format!("{reference}^{{commit}}")],
        Some(&clone),
    )
    .text()?;
    let folder = fix_folder(&name);
    remove_merge(&clone, &folder);
    std::fs::create_dir_all(clone.join(".vision").join("fix"))
        .map_err(|e| Error::Unexpected(format!("OSError: {e}")))?;
    Git::new(
        &["worktree", "add", "--detach", "--quiet", &folder, &tip],
        Some(&clone),
    )
    .timeout(600)
    .run()?;
    let work = clone.join(&folder);
    std::fs::write(git_file(&work, FIX_TIP)?, format!("{tip}\n"))
        .map_err(|e| Error::Unexpected(format!("OSError: {e}")))?;
    let links: Vec<String> = match call.get("table").and_then(|t| t.get("links")) {
        Some(Value::Array(items)) => items
            .iter()
            .filter_map(Value::as_str)
            .map(str::to_string)
            .collect(),
        _ => Vec::new(),
    };
    let (linked, skipped) = link_ignored(&clone, &work, &links);

    let gh = GitHub::new(config, why, None)?;
    let run = gh.last_run(Some(&tip), &workflow)?;
    let state = forge::run_state(run.as_ref());
    let run_line = match &run {
        Some(run) => format!(
            "{} ({state}, attempt {}): {}",
            py_opt(run.get("id")),
            attempt_of(run),
            text_of(run.get("html_url"))
        ),
        None => format!("none: no run of {workflow} at this tip"),
    };
    let keep = (config.number("log_tail_kb", 64.0)? * 1024.0).trunc() as usize;
    let (log, log_detail) = jobs::ci_log_text(&gh, config, &tip, &workflow, keep, None)?;

    let files = match (head.is_empty(), ticket_files(config, &clone, &head)) {
        (false, Some(files)) if !files.is_empty() => files
            .iter()
            .map(|f| format!("- `{f}`"))
            .collect::<Vec<_>>()
            .join("\n"),
        (false, Some(_)) => "None.".to_string(),
        _ => "Not known: git could not compare the head with the base ref.".to_string(),
    };
    let links_text = match (linked.is_empty(), skipped.is_empty()) {
        (true, true) => String::new(),
        _ => {
            let mut text = String::from("\n## Links from the clone\n\n");
            if !linked.is_empty() {
                text.push_str(&format!(
                    "The fix folder links these ignored entries of the clone, so that the tests can run there: {}.\n",
                    linked
                        .iter()
                        .map(|l| format!("`{l}`"))
                        .collect::<Vec<_>>()
                        .join(", ")
                ));
            }
            if !skipped.is_empty() {
                text.push_str(&format!("No link for: {}.\n", skipped.join("; ")));
            }
            text
        }
    };
    let content = format!(
        "# The CI of {branch} is red at the tip that holds {key}\n\n\
         Vision merged the head of {key} into `{branch}`, the branch of the env {name}. That push started a run of {workflow} at the tip of the branch, and the run stayed red after one more try of its failed jobs. An env that deploys only a green tip does not take the change of {key} until a run there is green.\n\n\
         - Env: {name}\n\
         - Branch of the env: `{branch}`\n\
         - Tip of that branch: `{tip}`\n\
         - Head of the ticket: `{head}`\n\
         - Run: {run_line}\n\
         - Fix folder: `{folder}`\n\n\
         `ci-log.txt` holds the end of the log of each failed job of the run.\n\n\
         ## Files that the change of {key} touches\n\n{files}\n{links_text}\n\
         ## Where the fix goes\n\n\
         Find the cause first. Then write `ci-fix.md`. Its first line is the commit message of the fix, and one line of it says where the fix is:\n\n\
         - `Fixed in: branch`: the cause is in the change of {key}. Fix it in the clone, on the branch of the ticket, and commit there. The fix goes through the pull request again, and a new deploy takes it to `{branch}`.\n\
         - `Fixed in: staging`: the cause is in code that only `{branch}` has, such as a test of another branch, or the way that two branches meet there. Fix it in the fix folder, and edit its files only: do not stage, commit, or push. After the turn, `deploy.fix` commits each change of the fix folder with the first line of `ci-fix.md` and pushes it to `{branch}`, only while the branch is still at `{tip}`.\n\
         - `Fixed in: none`: the cause is outside the code, such as a machine of the CI. Change nothing, and give the reason. The person decides.\n\n\
         Keep notes out of the fix folder: each file there goes into the commit.\n"
    );
    let detail = format!(
        "the fix folder {folder} is at {} of {branch}; the run there is {state}; {log_detail}",
        short(&tip)
    );
    Ok(json!({
        "ok": true,
        "detail": detail,
        "documents": [
            {"name": ENV_FIX_DOC, "content": content},
            {"name": "ci-log.txt", "content": log},
        ],
    }))
}

/// The commit message of a fix from the text of the turn: its first line with text, with no
/// `#` of a heading and no `Commit:` label, at most 200 characters. With no such line, a
/// message that names the ticket and the branch.
fn fix_message(text: Option<&Value>, key: &str, branch: &str) -> String {
    let text = text.and_then(Value::as_str).unwrap_or("");
    let first = text
        .lines()
        .map(|l| l.trim().trim_start_matches('#').trim())
        .map(|l| l.strip_prefix("Commit:").map_or(l, str::trim))
        .find(|l| !l.is_empty() && !l.starts_with("Fixed in:"))
        .unwrap_or("");
    let first = clean(first, 200);
    if first.is_empty() {
        format!("Fix the CI of {branch} for {key}")
    } else {
        first
    }
}

/// The commit of a fix that an uncertain answer of deploy.fix stored in its external id:
/// `deploy-fix-<env>-<commit>`.
fn stored_fix(external_id: Option<&Value>, env: &str) -> Option<String> {
    let text = external_id.and_then(Value::as_str)?;
    let commit = super::core::line_end(text).strip_prefix(&format!("deploy-fix-{env}-"))?;
    is_full_commit(commit).then(|| commit.to_string())
}

fn is_full_commit(text: &str) -> bool {
    text.len() == 40 && text.bytes().all(|b| b.is_ascii_hexdigit())
}

/// Commits the changes that an agent made in the fix folder of an env (see the job env-fix)
/// and pushes the commit to the branch of the env. The sandbox of a turn cannot write the git
/// folder of a worktree, so the agent only edits files, and this verb stages each change and
/// commits it with the first line of `args.text` and the identity of the call. A fix that a
/// person committed by hand on the tip goes as it is. It pushes only a commit whose parent is
/// the tip that the branch still has, so the push is a fast-forward, and never a commit that
/// holds a conflict marker. After the push, the fix folder goes away.
fn deploy_fix(
    config: &Config,
    call: &Map<String, Value>,
    reconcile: bool,
    state: &mut Acting,
) -> Res<Value> {
    let why = "deploy.fix";
    let key = check_key(call.get("key"), "the ticket key")?;
    let (name, branch) = staging_env(config, call, why, "pushes")?;
    let gh = GitHub::new(config, why, None)?;
    let clone = clone_of(call);
    let folder = fix_folder(&name);
    let work = clone.as_ref().map(|c| c.join(&folder));
    let recorded_tip = work
        .as_ref()
        .filter(|w| w.is_dir())
        .and_then(|w| git_file(w, FIX_TIP).ok())
        .and_then(|p| std::fs::read_to_string(p).ok())
        .map(|t| t.trim().to_string())
        .filter(|t| is_full_commit(t));
    if reconcile {
        let folder_commit = work
            .as_ref()
            .filter(|w| w.is_dir())
            .and_then(|w| {
                Git::new(&["rev-parse", "--verify", "HEAD^{commit}"], Some(w))
                    .text()
                    .ok()
            })
            .filter(|c| Some(c) != recorded_tip.as_ref());
        let Some(commit) = stored_fix(call.get("external_id"), &name).or(folder_commit) else {
            return failed(&format!(
                "no commit of a fix to look for: the push of the fix to {branch} did not happen"
            ));
        };
        return match gh.holds(&branch, &commit)? {
            Some(true) => {
                if let Some(clone) = &clone {
                    remove_merge(clone, &folder);
                }
                confirmed(
                    &format!("{branch} holds the fix {}", short(&commit)),
                    format!("deploy-fix-{name}-{commit}"),
                )
            }
            _ => failed(&format!(
                "{branch} does not hold the fix {}: its push did not happen",
                short(&commit)
            )),
        };
    }
    let (Some(clone), Some(work)) = (clone, work) else {
        return fail("deploy.fix needs the clone of the ticket");
    };
    if !work.is_dir() {
        return failed(&format!(
            "no fix waits in {folder}. Run the step again, so that the job env-fix makes the fix folder"
        ));
    }
    let Some(tip) = recorded_tip else {
        return failed(&format!(
            "the fix folder {folder} names no tip of {branch}. Run the step again, so that the job env-fix makes it again"
        ));
    };
    let now = Git::new(&["rev-parse", "--verify", "HEAD^{commit}"], Some(&work)).text()?;
    let commit = if now == tip {
        Git::new(&["add", "-A"], Some(&work)).run()?;
        let files = names(&Git::new(&["diff", "--cached", "--name-only"], Some(&work)).text()?);
        if files.is_empty() {
            return failed(&format!(
                "the fix folder {folder} has no change, so nothing goes to {branch}"
            ));
        }
        if let Some(marker) = working_marker(&work, &files) {
            let _ = Git::new(&["reset", "--quiet"], Some(&work))
                .unchecked()
                .run();
            return failed(&format!(
                "the fix in {folder} holds a conflict marker at {marker}. Remove it, then run the step again"
            ));
        }
        let message = fix_message(args_of(call).get("text"), &key, &branch);
        let mut settings = identity_settings(call);
        settings.push("commit.gpgsign=false".into());
        Git::new(
            &["commit", "--quiet", "--no-verify", "-m", &message],
            Some(&work),
        )
        .config(&settings)
        .run()?;
        Git::new(&["rev-parse", "--verify", "HEAD^{commit}"], Some(&work)).text()?
    } else {
        let parent = Git::new(&["rev-parse", "--verify", "HEAD^1"], Some(&work))
            .unchecked()
            .run()?;
        if parent.code != Some(0) || parent.stdout_text().trim() != tip {
            return failed(&format!(
                "the commit {} in {folder} does not start at the tip {} of {branch}: push only one commit on that tip",
                short(&now),
                short(&tip)
            ));
        }
        now
    };
    if let Some(marker) = conflict_marker(&work, &commit)? {
        return failed(&format!(
            "the fix {} holds a conflict marker at {marker}. Remove it, then run the step again",
            short(&commit)
        ));
    }
    let files = names(
        &Git::new(
            &["diff", "--name-only", &format!("{commit}^1"), &commit],
            Some(&work),
        )
        .text()?,
    );
    let url = remote_url(config, why)?;
    let auth = auth_config(&url, &secret("FORGE_TOKEN", why)?);
    let remote = remote_head(&url, &branch, &auth)?;
    if remote.as_deref() != Some(tip.as_str()) {
        remove_merge(&clone, &folder);
        return failed(&format!(
            "{branch} moved from {} to {} after the fix started, and Vision never forces a push. Run the step again, so that the fix starts at the new tip",
            short(&tip),
            short_opt(remote.as_deref())
        ));
    }
    let external = format!("deploy-fix-{name}-{commit}");
    state.acted = true;
    let output = push_commit(&clone, &url, &branch, &commit, &auth)?;
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
            remove_merge(&clone, &folder);
            return failed(&format!(
                "{branch} moved after the fix started, and Vision never forces a push: {err}. Run the step again, so that the fix starts at the new tip"
            ));
        }
        return Ok(answer(
            "uncertain",
            &format!("git push of the fix failed: {err}"),
            Some(external),
            None,
            None,
        ));
    }
    if remote_head(&url, &branch, &auth)?.as_deref() != Some(commit.as_str()) {
        return Ok(answer(
            "uncertain",
            &format!(
                "git push ended, but {branch} is not at the fix {} on the remote",
                short(&commit)
            ),
            Some(external),
            None,
            None,
        ));
    }
    remove_merge(&clone, &folder);
    confirmed(
        &format!(
            "pushed the fix {} to {branch} on its tip {}: {}",
            short(&commit),
            short(&tip),
            files.join(", ")
        ),
        external,
    )
}

// review.rerequest ------------------------------------------------------------------------------

/// Asks each reviewer whose last review asked for changes for a new review: an approver, or
/// another reviewer whose change request counts (see [`forge::pr_facts`]). It never asks a GitHub
/// App: GitHub ignores such a request, and an app reviews each push by itself.
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
    let (facts, _state) = forge::pr_facts(&gh, &pull, config, &std::collections::BTreeSet::new())?;
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
    let (apps, people): (Vec<String>, Vec<String>) =
        asked.iter().cloned().partition(|n| forge::is_app(n));
    let todo: Vec<String> = people
        .iter()
        .filter(|n| !requested.contains(&n.to_lowercase()))
        .cloned()
        .collect();
    let app_note = if apps.is_empty() {
        String::new()
    } else {
        format!(
            "; no request goes to {}, because an app reviews each push by itself",
            apps.join(", ")
        )
    };
    let external = format!("rerequest-{number}");
    if asked.is_empty() {
        return confirmed(
            &format!("no reviewer asked for changes on #{number}"),
            external,
        );
    }
    if todo.is_empty() {
        let text = if people.is_empty() {
            format!(
                "no request goes to {} on #{number}, because an app reviews each push by itself",
                apps.join(", ")
            )
        } else {
            format!(
                "a new review of {} is requested on #{number}{app_note}",
                people.join(", ")
            )
        };
        return confirmed(&text, external);
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
        &format!(
            "asked {} for a new review of #{number}{app_note}",
            todo.join(", ")
        ),
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
    let (facts, review) = forge::pr_facts(&gh, &pull, config, &std::collections::BTreeSet::new())?;
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

/// `review.read`: the first review of the login `args.reviewer` that was submitted after
/// `args.after` (an RFC 3339 time) on the pull request of the ticket, with its inline comments,
/// as review.json. With no such review, `review` is null. The texts are untrusted data. The
/// escape check of the core reads it (design section 11.6).
fn review_read(config: &Config, call: &Map<String, Value>) -> Res<Value> {
    let args = args_of(call);
    let reviewer = text_of(args.get("reviewer")).trim().to_string();
    if reviewer.is_empty()
        || reviewer
            .chars()
            .any(|c| c.is_control() || c.is_whitespace())
    {
        return fail(
            "review.read needs args.reviewer: the login of a reviewer, such as review-bot[bot]",
        );
    }
    let after_text = text_of(args.get("after"));
    let Ok(after) = crate::util::time::parse_rfc3339(after_text.trim()) else {
        return fail(
            "review.read needs args.after: an RFC 3339 time, such as 2026-11-16T14:30:00Z",
        );
    };
    let number = match truthy(args.get("number")).or(call.get("pr")) {
        None | Some(Value::Null) => None,
        Some(Value::Number(n)) if n.as_i64().is_some() => n.as_i64(),
        Some(_) => return fail("review.read takes args.number: a pull request number"),
    };
    let gh = GitHub::new(config, "review.read", None)?;
    let branch = text_of(call.get("branch"));
    let Some(pull) = gh.find_pr(number, &branch)? else {
        return fail("review.read found no pull request of the ticket");
    };
    let n = forge::int(&pull, "number")
        .ok_or_else(|| Error::Unexpected("KeyError: 'number'".into()))?;
    let submitted =
        |r: &Value| crate::util::time::parse_rfc3339(forge::text(r, "submitted_at")).ok();
    let mut found: Vec<(::time::OffsetDateTime, Value)> = gh
        .reviews(n)?
        .into_iter()
        .filter(|r| forge::login(r.get("user")).eq_ignore_ascii_case(&reviewer))
        .filter(|r| r.get("state").and_then(Value::as_str) != Some("PENDING"))
        .filter_map(|r| submitted(&r).filter(|at| *at > after).map(|at| (at, r)))
        .collect();
    found.sort_by_key(|(at, _)| *at);
    let Some((_, review)) = found.into_iter().next() else {
        let data = json!({"pr": n, "reviewer": reviewer, "after": after_text, "review": null, "comments": []});
        return read_doc(
            "review.json",
            &data,
            &format!("no review of {reviewer} on #{n} after {after_text}"),
        );
    };
    let id = forge::int(&review, "id")
        .ok_or_else(|| Error::Unexpected("KeyError: 'id' of a review".into()))?;
    let comments: Vec<Value> = gh
        .review_comments(n)?
        .into_iter()
        .filter(|c| forge::int(c, "pull_request_review_id") == Some(id))
        .filter(|c| c.get("in_reply_to_id").is_none_or(Value::is_null))
        .map(|c| {
            json!({
                "id": c.get("id").cloned().unwrap_or(Value::Null),
                "path": c.get("path").cloned().unwrap_or(Value::Null),
                "line": truthy(c.get("line")).or(c.get("original_line")).cloned().unwrap_or(Value::Null),
                "url": c.get("html_url").cloned().unwrap_or(Value::Null),
                "untrusted": truthy(c.get("body")).cloned().unwrap_or(json!("")),
            })
        })
        .collect();
    let count = comments.len();
    let data = json!({
        "pr": n,
        "reviewer": reviewer,
        "after": after_text,
        "review": {
            "id": id,
            "author": forge::login(review.get("user")),
            "state": review.get("state").cloned().unwrap_or(Value::Null),
            "submitted_at": review.get("submitted_at").cloned().unwrap_or(Value::Null),
            "commit": review.get("commit_id").cloned().unwrap_or(Value::Null),
            "url": review.get("html_url").cloned().unwrap_or(Value::Null),
        },
        "comments": comments,
    });
    read_doc(
        "review.json",
        &data,
        &format!("the review {id} of {reviewer} on #{n} with {count} inline comment(s)"),
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
