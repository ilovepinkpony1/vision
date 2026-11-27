//! `observe`: the facts of the queue or of one ticket (design section 11.1).
//!
//! observe only reads. It never changes Jira or GitHub.

use std::collections::BTreeSet;

use serde_json::{json, Map, Value};

use super::core::{
    check_key, clean, fail, is_commit, is_key, py_opt, py_repr_str, text_field, text_of, truthy,
    whole, Config, Res,
};
use super::forge::{self, GitHub};
use super::http::{query, send};
use super::jira::{self, Jira};

const QUEUE_LIMIT: f64 = 100.0;

pub fn observe(call: &Map<String, Value>) -> Res<Value> {
    let config = Config::of(call);
    match call.get("what").and_then(Value::as_str) {
        Some("queue") => queue(&config),
        Some("ticket") => ticket(&config, call),
        _ => fail(format!(
            "observe: `what` must be queue or ticket, not {}",
            py_repr_str(&clean(&py_opt(call.get("what")), 40))
        )),
    }
}

/// The issues of queue_jql. An issue whose key a branch or an open pull request of the repo
/// already has in its name is not eligible: it is in progress outside Vision, and its row names
/// its branch and its open pull request, so that a take can start it at the step where it is.
fn queue(config: &Config) -> Res<Value> {
    let why = "the queue";
    let jql = config.text_need("queue_jql", why)?;
    let tracker = Jira::new(config, why)?;
    let limit = whole(config.number("queue_limit", QUEUE_LIMIT)?).max(0) as usize;
    let issues = tracker.search(&jql, &["summary", "status"], limit)?;
    let gh = GitHub::new(config, why, None)?;
    let branches = gh.branches(20)?;
    let pulls: Vec<Value> = gh
        .client
        .pages(
            &gh.path("pulls"),
            "list the open pull requests",
            &query(&[("state", "open")]),
            100,
            10,
        )?
        .into_iter()
        .filter(Value::is_object)
        .collect();
    let mut tickets = Vec::new();
    for raw in &issues {
        let Some(key) = raw.get("key").and_then(Value::as_str).filter(|k| is_key(k)) else {
            continue;
        };
        let fields = jira::fields_of(raw);
        let branch = branches.iter().find(|b| forge::names_key(b, key));
        let pull = pulls.iter().find(|p| {
            forge::names_key(&text_of(p.get("title")), key)
                || forge::names_key(
                    &text_of(truthy(p.get("head")).and_then(|h| h.get("ref"))),
                    key,
                )
        });
        let reason = if let Some(branch) = branch {
            format!(
                "the branch {} already has {key} in its name",
                clean(branch, 100)
            )
        } else if let Some(pull) = pull {
            format!(
                "the pull request #{} already has {key} in its name",
                py_opt(pull.get("number"))
            )
        } else {
            String::new()
        };
        let mut ticket = json!({
            "key": key,
            "title": truthy(fields.get("summary")).cloned().unwrap_or(json!("")),
            "status": jira::name_of(fields.get("status"), "name"),
            "eligible": reason.is_empty(),
            "reason": reason,
        });
        // A ticket that is in progress outside Vision: its branch, and its open pull request.
        // The branch of the pull request wins over another branch with the key.
        let pull_branch = pull
            .and_then(|p| truthy(p.get("head")))
            .map(|h| text_of(h.get("ref")))
            .filter(|b| !b.trim().is_empty());
        if let Some(branch) = pull_branch.or_else(|| branch.cloned()) {
            ticket["branch"] = json!(branch);
        }
        if let Some(number) = pull.and_then(|p| pr_number(p.get("number"))) {
            ticket["pr"] = json!(number);
        }
        tickets.push(ticket);
    }
    Ok(json!({ "tickets": tickets }))
}

/// The marks of the comments that a finished round of comments read, from the field `seen`:
/// each text of a list. Another value gives none.
fn seen_marks(call: &Map<String, Value>) -> BTreeSet<String> {
    match call.get("seen") {
        Some(Value::Array(items)) => items
            .iter()
            .filter_map(Value::as_str)
            .map(str::to_string)
            .collect(),
        _ => BTreeSet::new(),
    }
}

/// The number of a pull request in a call field: an int, or a text of digits.
pub fn pr_number(value: Option<&Value>) -> Option<i64> {
    match truthy(value)? {
        Value::Number(n) => n.as_i64().or_else(|| n.as_f64().map(|f| f.trunc() as i64)),
        Value::String(s) => s.trim().parse().ok(),
        _ => None,
    }
}

fn ticket(config: &Config, call: &Map<String, Value>) -> Res<Value> {
    let why = "observe of one ticket";
    let key = check_key(call.get("key"), "the ticket key")?;
    let branch = text_field(call, "branch");
    let tracker = Jira::new(config, why)?;
    let issue = tracker.view(&tracker.issue(&key, &jira::ISSUE_FIELDS)?, true)?;
    let gh = GitHub::new(config, why, None)?;
    let workflow = config.text_need("ci_workflow", why)?;
    let pr = gh.find_pr(pr_number(call.get("pr")), branch)?;
    let (pr_view, review) = match &pr {
        Some(pr) => {
            let (facts, state) = forge::pr_facts(&gh, pr, config, &seen_marks(call))?;
            (Value::Object(facts), state)
        }
        None => (Value::Null, "none".to_string()),
    };
    // The head commit: the head of the pull request, else the branch on GitHub.
    let mut head: Option<String> = truthy(pr_view.get("head")).map(super::core::py_str);
    if head.is_none() && !branch.is_empty() {
        head = gh.branch_head(branch)?;
    }
    let run = match &head {
        Some(head) => gh.last_run(Some(head), &workflow)?,
        None => None,
    };
    let mut ci = json!({"head": head, "state": forge::run_state(run.as_ref())});
    if let Some(run) = &run {
        ci["run"] = run.get("id").cloned().unwrap_or(Value::Null);
        ci["attempt"] = truthy(run.get("run_attempt")).cloned().unwrap_or(json!(1));
        ci["url"] = truthy(run.get("html_url")).cloned().unwrap_or(json!(""));
    }
    let mut envs = Map::new();
    for env in config.envs()? {
        let tip = gh.branch_head(&env.branch)?;
        let holds = match (&head, &tip) {
            (Some(head), Some(_)) => gh.holds(&env.branch, head)?.unwrap_or(false),
            _ => false,
        };
        let mut facts = json!({"branch": env.branch, "head": tip, "holds": holds});
        if let (true, Some(tip)) = (holds, &tip) {
            for (k, v) in env_ci_facts(&gh, tip, &workflow)? {
                facts[k] = v;
            }
        }
        if let Some(deployment) = &env.deployment {
            for (k, v) in deployed_facts(&gh, deployment, head.as_deref())? {
                facts[k] = v;
            }
        }
        if let Some(url) = &env.version_url {
            let head = head.as_deref().filter(|_| holds);
            for (k, v) in running_facts(&gh, url, head)? {
                facts[k] = v;
            }
        }
        envs.insert(env.name.clone(), facts);
    }
    let status = issue["status"].clone();
    Ok(json!({
        "issue": issue,
        "tracker_status": status,
        "pr": pr_view,
        "review": {"state": review},
        "ci": ci,
        "envs": envs,
    }))
}

/// The CI of the tip of an env branch that holds the head of the ticket: `ci` is the state of
/// the last run of the CI workflow at that tip (green, red, running, or none), with `ci_run`
/// and `ci_url` of that run. A push to a staging branch starts that run, and an env whose
/// deploy waits for a green run never takes a red tip. observe asks for it only while the
/// branch holds the head, so an env that does not hold it has none of the three facts.
fn env_ci_facts(gh: &GitHub, tip: &str, workflow: &str) -> Res<Map<String, Value>> {
    let run = gh.last_run(Some(tip), workflow)?;
    let mut out = Map::new();
    out.insert("ci".into(), json!(forge::run_state(run.as_ref())));
    out.insert(
        "ci_run".into(),
        run.as_ref()
            .and_then(|r| r.get("id"))
            .cloned()
            .unwrap_or(Value::Null),
    );
    out.insert(
        "ci_url".into(),
        run.as_ref()
            .and_then(|r| truthy(r.get("html_url")))
            .cloned()
            .unwrap_or(Value::Null),
    );
    Ok(out)
}

/// The facts of the newest deployment of a GitHub environment: `deployed` is true when its
/// commit holds the head of the ticket (the same test as `holds`), with `deployment_sha` and
/// `deployed_at`. With no deployment, `deployed` is false and the two others are null.
fn deployed_facts(gh: &GitHub, environment: &str, head: Option<&str>) -> Res<Map<String, Value>> {
    let latest = gh.latest_deployment(environment)?;
    let sha = latest
        .as_ref()
        .and_then(|d| d.get("sha"))
        .and_then(Value::as_str)
        .filter(|s| is_commit(s))
        .map(str::to_string);
    let at = latest
        .as_ref()
        .and_then(|d| d.get("created_at"))
        .and_then(Value::as_str)
        .map(str::to_string);
    let deployed = match (&sha, head) {
        (Some(sha), Some(head)) => gh.commit_holds(sha, head)?.unwrap_or(false),
        _ => false,
    };
    let mut out = Map::new();
    out.insert("deployed".into(), json!(deployed));
    out.insert("deployment_sha".into(), json!(sha));
    out.insert("deployed_at".into(), json!(at));
    Ok(out)
}

/// Seconds for one read of the version page of an env: a page that hangs must not hold the
/// look of each ticket.
const VERSION_TIMEOUT: f64 = 10.0;

/// The commit that an env runs now, from its version page. `running_sha` is the first commit
/// id on the page (40 hex digits), and `running` is true when that commit holds the head of the
/// ticket (the same test as `deployed`). `head` is `None` while the branch of the env does not
/// hold the head: no deploy can run it then, so the page is not read, `running` is false, and
/// `running_sha` is null. A page that names no commit, does not answer with 200, or names a
/// commit that GitHub does not know gives `running` null, so a watch reads none.
fn running_facts(gh: &GitHub, url: &str, head: Option<&str>) -> Res<Map<String, Value>> {
    let mut out = Map::new();
    let Some(head) = head else {
        out.insert("running".into(), json!(false));
        out.insert("running_sha".into(), Value::Null);
        return Ok(out);
    };
    let sha = match send("GET", url, &[], None, VERSION_TIMEOUT, Some(65536)) {
        Ok(page) if page.status == 200 => String::from_utf8_lossy(&page.body)
            .split(|c: char| !c.is_ascii_hexdigit())
            .map(str::to_ascii_lowercase)
            .find(|word| word.len() == 40 && is_commit(word)),
        _ => None,
    };
    let running = match &sha {
        Some(sha) => gh.commit_holds(sha, head)?,
        None => None,
    };
    out.insert("running".into(), json!(running));
    out.insert("running_sha".into(), json!(sha));
    Ok(out)
}
