"""bin/observe: the facts of the queue or of one ticket (design section 11.1).

observe only reads. It never changes Jira or GitHub.
"""

from . import core, forge, jira

QUEUE_LIMIT = 100


def observe(request):
    config = core.Config(request.get("config"))
    what = request.get("what")
    if what == "queue":
        return queue(config)
    if what == "ticket":
        return ticket(config, request)
    raise core.Failure("observe: `what` must be queue or ticket, not %r" % core.clean(what, 40))


def queue(config):
    """The issues of queue_jql. An issue whose key a branch or an open pull request of the repo
    already has in its name is not eligible."""
    why = "the queue"
    jql = config.text("queue_jql", why)
    tracker = jira.Jira(config, why)
    issues = tracker.search(jql, ["summary", "status"], limit=int(config.number("queue_limit", QUEUE_LIMIT)))
    gh = forge.GitHub(config, why)
    branches = gh.branches()
    pulls = [p for p in gh.client.pages(gh.path("pulls"), "list the open pull requests", query={"state": "open"}, max_pages=10)
             if isinstance(p, dict)]
    tickets = []
    for raw in issues:
        key = raw.get("key")
        if not isinstance(key, str) or not core.KEY.match(key):
            continue
        fields = raw.get("fields") or {}
        reason = ""
        branch = next((b for b in branches if forge.names_key(b, key)), None)
        pull = next(
            (p for p in pulls
             if forge.names_key(p.get("title"), key) or forge.names_key((p.get("head") or {}).get("ref"), key)),
            None,
        )
        if branch:
            reason = "the branch %s already has %s in its name" % (core.clean(branch, 100), key)
        elif pull:
            reason = "the pull request #%s already has %s in its name" % (pull.get("number"), key)
        tickets.append({
            "key": key,
            "title": fields.get("summary") or "",
            "status": jira.name_of(fields.get("status")),
            "eligible": not reason,
            "reason": reason,
        })
    return {"tickets": tickets}


def ticket(config, request):
    why = "observe of one ticket"
    key = core.check_key(request.get("key"))
    branch = request.get("branch") or ""
    tracker = jira.Jira(config, why)
    issue = tracker.view(tracker.issue(key, jira.ISSUE_FIELDS))
    gh = forge.GitHub(config, why)
    workflow = config.text("ci_workflow", why)
    pr = gh.find_pr(request.get("pr"), branch)
    pr_view, review = None, "none"
    if pr:
        pr_view, review = forge.pr_facts(gh, pr, config)
    # The head commit: the head of the pull request, else the branch on GitHub.
    head = pr_view["head"] if pr_view else None
    if not head and branch:
        head = gh.branch_head(branch)
    run = gh.last_run(head, workflow) if head else None
    ci = {"head": head, "state": forge.run_state(run)}
    if run:
        ci.update({"run": run.get("id"), "attempt": run.get("run_attempt") or 1, "url": run.get("html_url") or ""})
    envs = {}
    for env in config.envs():
        tip = gh.branch_head(env["branch"])
        holds = gh.holds(env["branch"], head) if head and tip else False
        envs[env["name"]] = {"branch": env["branch"], "head": tip, "holds": bool(holds)}
    return {
        "issue": issue,
        "tracker_status": issue["status"],
        "pr": pr_view,
        "review": {"state": review},
        "ci": ci,
        "envs": envs,
    }
