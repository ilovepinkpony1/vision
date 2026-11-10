"""bin/act: one verb, its reconcile form, or a read verb (design section 11.1).

Each verb that changes something has a reconcile form: with `reconcile` set, it only reads the
outside state and answers confirmed, failed, or uncertain. It never acts again.

A failure before the verb changed anything is `failed`. A failure after it may have changed
something, and each failure of a reconcile, is `uncertain`: the tick asks again.
"""

import base64
import json
import os
import re
import tempfile
import time
import urllib.parse

from . import adf, core, forge, jira


class Acting:
    """Set `acted` right before the first call that changes the outside."""

    def __init__(self):
        self.acted = False


def act(request):
    config = core.Config(request.get("config"))
    verb = request.get("verb")
    if request.get("read"):
        if verb not in READS:
            raise core.Failure("the adapter jira-github has no read verb %r. Its read verbs: %s" % (
                core.clean(verb, 40), ", ".join(sorted(READS))))
        return READS[verb](config, request)
    if verb not in VERBS:
        raise core.Failure("the adapter jira-github has no verb %r. Its verbs: %s" % (
            core.clean(verb, 40), ", ".join(sorted(VERBS))))
    reconcile = bool(request.get("reconcile"))
    state = Acting()
    try:
        return VERBS[verb](config, request, reconcile, state)
    except core.HttpError as e:
        # A definite refusal of the server (400 to 499, but not a time limit or a rate limit)
        # means that the call changed nothing.
        if not reconcile and 400 <= e.status < 500 and e.status not in (408, 429):
            return answer("failed", str(e))
        return answer("uncertain" if reconcile or state.acted else "failed", "%s. The outside state is not known" % e)
    except core.Failure as e:
        if reconcile or state.acted:
            return answer("uncertain", "%s. The outside state is not known" % e)
        return answer("failed", str(e))


def answer(state, detail, external_id=None, values=None, documents=None):
    out = {"state": state, "detail": core.mask(detail)}
    if external_id is not None:
        out["external_id"] = external_id
    if values:
        out["set"] = values
    if documents:
        out["documents"] = documents
    return out


def confirmed(detail, external_id=None, values=None, documents=None):
    return answer("confirmed", detail, external_id, values, documents)


def failed(detail):
    return answer("failed", detail)


def uncertain(detail):
    return answer("uncertain", detail)


def same_text(a, b):
    def norm(text):
        return "\n".join(line.rstrip() for line in (text or "").strip().splitlines())
    return norm(a) == norm(b)


# tracker.describe -----------------------------------------------------------------------------

BACKUP_IN_DETAIL = 600


def tracker_describe(config, request, reconcile, state):
    why = "tracker.describe"
    key = core.check_key(request.get("key"))
    args = request.get("args") or {}
    text = args.get("text")
    if not isinstance(text, str) or not text.strip():
        raise core.Failure("tracker.describe needs args.text: the new description, {doc:description.md}")
    new_doc = adf.to_adf(text)
    new_md = adf.to_markdown(new_doc)
    tracker = jira.Jira(config, why)
    current = (tracker.issue(key, ["description"]).get("fields") or {}).get("description")
    current_md = adf.to_markdown(current)
    if same_text(current_md, new_md):
        return confirmed("the description of %s is the new one" % key, key)
    if reconcile:
        return failed("the description of %s is not the new one" % key)
    seen = seen_description(args.get("seen"))
    if not same_text(current_md, seen):
        raise core.Refusal(
            "a person edited the description of %s after Vision read it (Vision read %d characters, Jira has %d now), "
            "so the description stays as it is. Run `vision retry %s` to read it again" % (key, len(seen), len(current_md), key)
        )
    name = "description-backup-%s-%s.json" % (key, core.stamp())
    backup = {"key": key, "at": core.now(), "markdown": current_md, "adf": current}
    state.acted = True
    tracker.set_description(key, new_doc)
    after = adf.to_markdown((tracker.issue(key, ["description"]).get("fields") or {}).get("description"))
    if not same_text(after, new_md):
        return uncertain("Jira took the new description of %s, but it reads back a different text" % key)
    detail = "the description of %s is replaced. The old description is in the document %s" % (key, name)
    if len(current_md) <= BACKUP_IN_DETAIL:
        detail += ". The old text:\n%s" % current_md if current_md else ". The old description was empty"
    else:
        detail += " (%d characters); Jira also keeps it in the history of the issue" % len(current_md)
    return confirmed(detail, key, documents=[{"name": name, "content": json.dumps(backup, indent=2) + "\n"}])


def seen_description(seen):
    """The description in issue.json, as observe gave it at the entry of the step."""
    data = seen
    if isinstance(seen, str):
        try:
            data = json.loads(seen)
        except ValueError:
            raise core.Failure("args.seen of tracker.describe is not JSON: it must be {doc:issue.json}")
    if not isinstance(data, dict):
        raise core.Failure("tracker.describe needs args.seen: the issue that observe gave, {doc:issue.json}")
    for path in (("issue", "untrusted"), ("issue",), ()):
        node = data
        for part in path:
            node = node.get(part) if isinstance(node, dict) else None
        if isinstance(node, dict) and "description" in node:
            value = node.get("description")
            return value if isinstance(value, str) else ""
    raise core.Failure("args.seen of tracker.describe has no description: it must be {doc:issue.json}")


# tracker.move ---------------------------------------------------------------------------------


def tracker_move(config, request, reconcile, state):
    why = "tracker.move"
    key = core.check_key(request.get("key"))
    role = (request.get("args") or {}).get("status")
    if not isinstance(role, str) or not role:
        raise core.Failure("tracker.move needs args.status: a role of `statuses`, such as in_progress")
    statuses = config.need("statuses", why)
    if not isinstance(statuses, dict) or not isinstance(statuses.get(role), str) or not statuses[role].strip():
        raise core.Failure("tracker.move needs the key `statuses.%s` in the [adapter] table: the status name of the role" % role)
    target = statuses[role].strip()
    tracker = jira.Jira(config, why)
    now_status = tracker.status(key)
    external = "%s:%s" % (key, target)
    if now_status.lower() == target.lower():
        return confirmed("%s is in the status %s" % (key, target), external)
    if reconcile:
        return failed("%s is in the status %s, not %s" % (key, now_status, target))
    transitions = tracker.transitions(key)
    match = [t for t in transitions if jira.name_of(t.get("to")).lower() == target.lower()]
    match = match or [t for t in transitions if str(t.get("name") or "").lower() == target.lower()]
    if not match:
        listed = "; ".join("%s -> %s" % (t.get("name"), jira.name_of(t.get("to"))) for t in transitions) or "none"
        return failed("no transition of %s leads from %s to %s. The transitions: %s" % (key, now_status, target, listed))
    state.acted = True
    tracker.transition(key, match[0].get("id"))
    after = tracker.status(key)
    if after.lower() != target.lower():
        return uncertain("Jira took the transition of %s, but the status reads %s, not %s" % (key, after, target))
    return confirmed("%s moved from %s to %s" % (key, now_status, target), external)


# git.push -------------------------------------------------------------------------------------

BAD_REF = re.compile(r"(^[-/.])|(\.\.)|([\s~^:?*\[\\])|(@\{)|([/.]$)|(\.lock$)|(//)")


def check_push_branch(branch, key, config, args):
    """The branch that git.push may push: the branch of the call, with the key, never the base."""
    if not isinstance(branch, str) or not branch or BAD_REF.search(branch):
        raise core.Refusal("git.push refuses the branch %r: it is not a branch name" % core.clean(branch, 80))
    if "branch" in args and args.get("branch") != branch:
        raise core.Refusal("git.push refuses the branch %r: it pushes only the branch of the call, %s" % (
            core.clean(args.get("branch"), 80), branch))
    protected = [config.base_branch()] + [e["branch"] for e in config.envs()]
    if branch in protected:
        raise core.Refusal("git.push refuses the branch %s: it is the base branch or the branch of an env" % branch)
    if not forge.names_key(branch, key):
        raise core.Refusal("git.push refuses the branch %s: it does not hold the key %s" % (branch, key))
    for flag in ("force", "force_with_lease", "delete", "mirror"):
        if args.get(flag):
            raise core.Refusal("git.push never uses %s" % flag.replace("_", "-"))


def remote_url(config, why):
    url = config.text("git_remote_url")
    if url is None:
        api = config.text("github_api", default=core.DEFAULT_GITHUB_API).rstrip("/")
        if api != core.DEFAULT_GITHUB_API:
            raise core.Failure("%s needs the key `git_remote_url` in the [adapter] table" % why)
        url = "https://github.com/%s.git" % config.repo(why)
    parts = urllib.parse.urlsplit(url)
    local = parts.hostname in ("localhost", "127.0.0.1", "::1")
    if parts.scheme == "https" or (parts.scheme == "http" and local):
        if parts.username or parts.password:
            raise core.Failure("`git_remote_url` must hold no user and no password")
        return url
    if parts.scheme == "file" or (parts.scheme == "" and os.path.isabs(url)):
        return url
    raise core.Failure("`git_remote_url` must be an https address or a local path, such as https://github.com/acme/widgets.git")


def auth_config(url, token):
    """The one git configuration value that carries the token: a header for this address only."""
    value = base64.b64encode(("x-access-token:%s" % token).encode("utf-8")).decode("ascii")
    return "http.%s.extraheader=Authorization: Basic %s" % (url, value)


# A redirect never takes the header to a different address.
NO_REDIRECT = "http.followRedirects=false"


def remote_head(url, branch, auth):
    with tempfile.TemporaryDirectory(prefix="vision-ls-") as tmp:
        proc = core.git(["ls-remote", url, "refs/heads/" + branch], cwd=tmp, check=False,
                        config=[NO_REDIRECT, auth], timeout=60)
    if proc.returncode != 0:
        err = proc.stderr.decode("utf-8", "replace").strip().splitlines()[-2:]
        raise core.Unreachable("git ls-remote failed: %s" % core.clean(" ".join(err)))
    for line in proc.stdout.decode("utf-8", "replace").splitlines():
        sha, _, ref = line.partition("\t")
        if ref.strip() == "refs/heads/" + branch:
            return sha.strip()
    return None


def git_push(config, request, reconcile, state):
    why = "git.push"
    key = core.check_key(request.get("key"))
    branch = request.get("branch") or ""
    head = request.get("head") or ""
    args = request.get("args") or {}
    check_push_branch(branch, key, config, args)
    if not core.COMMIT.match(head):
        raise core.Failure("git.push needs the last commit in head")
    url = remote_url(config, why)
    auth = auth_config(url, core.secret("FORGE_TOKEN", why))
    pushed = confirmed("%s is at %s on the remote" % (branch, core.short(head)), "%s@%s" % (branch, core.short(head)), {"head": head})
    remote = remote_head(url, branch, auth)
    if remote == head:
        return pushed
    if reconcile:
        return failed("%s is at %s on the remote, not at %s: the push did not happen" % (branch, core.short(remote), core.short(head)))
    clone = request.get("clone")
    if not isinstance(clone, str) or not os.path.isdir(clone):
        raise core.Failure("git.push needs the clone of the ticket")
    on = core.git_text(["symbolic-ref", "--quiet", "--short", "HEAD"], cwd=clone, check=False)
    if on != branch:
        raise core.Refusal("git.push refuses: the clone is on %r, not on the branch of the call %s" % (core.clean(on, 80), branch))
    core.git(["cat-file", "-e", head + "^{commit}"], cwd=clone)
    state.acted = True
    proc = push_commit(clone, url, branch, head, auth)
    if proc.returncode != 0:
        lines = [line for line in proc.stderr.decode("utf-8", "replace").splitlines()
                 if line.strip() and not line.startswith("hint:")]
        err = core.clean(" ".join(lines[-4:])).replace(auth.split("=", 1)[1], "[header]")
        if any(word in " ".join(lines) for word in ("[rejected]", "non-fast-forward", "fetch first")):
            return failed("the remote %s has commits that the clone does not have, and Vision never forces a push: %s" % (branch, err))
        return uncertain("git push failed: %s" % err)
    if remote_head(url, branch, auth) != head:
        return uncertain("git push ended, but %s is not at %s on the remote" % (branch, core.short(head)))
    return pushed


def push_commit(clone, url, branch, head, auth):
    """Pushes `head` as `branch` from a new bare repo that borrows the objects of the clone, so
    that no hook, no configuration, and no remote of the clone takes part, and the token never
    reaches a file of the clone. The refspec has no `+`: a push never forces."""
    objects = core.git_text(["rev-parse", "--git-path", "objects"], cwd=clone)
    if not os.path.isabs(objects):
        objects = os.path.join(clone, objects)
    with tempfile.TemporaryDirectory(prefix="vision-push-") as tmp:
        repo = os.path.join(tmp, "push.git")
        core.git(["init", "--bare", "--quiet", repo])
        with open(os.path.join(repo, "objects", "info", "alternates"), "w") as handle:
            handle.write(os.path.realpath(objects) + "\n")
        core.git(["update-ref", "refs/heads/" + branch, head], cwd=repo)
        return core.git(
            ["push", "--quiet", "--no-follow-tags", url, "refs/heads/%s:refs/heads/%s" % (branch, branch)],
            cwd=repo, check=False, config=[NO_REDIRECT, auth], timeout=450,
        )


# pr.open --------------------------------------------------------------------------------------


def pr_text(text):
    if not isinstance(text, str) or not text.strip():
        raise core.Failure("pr.open needs args.text: the text of pr.md, {doc:pr.md}")
    lines = text.strip().splitlines()
    title = lines[0].strip().lstrip("#").strip()
    if not title:
        raise core.Failure("the first line of pr.md is empty: it is the title of the pull request")
    return title[:256], "\n".join(lines[1:]).strip()


def pr_answer(pull, branch):
    number = pull.get("number")
    sha = (pull.get("head") or {}).get("sha")
    values = {"pr": number}
    if isinstance(sha, str) and core.COMMIT.match(sha):
        values["head"] = sha
    return confirmed("pull request #%s is open for %s" % (number, branch), str(number), values)


def pr_open(config, request, reconcile, state):
    why = "pr.open"
    branch = request.get("branch") or ""
    if not branch or branch == config.base_branch():
        raise core.Refusal("pr.open refuses the branch %r" % core.clean(branch, 80))
    # The token of the person, when there is one, so that the person is the author.
    gh = forge.GitHub(config, why, token=os.environ.get("PR_TOKEN") or None)
    open_pulls = sorted(gh.pulls_of(branch, "open"), key=lambda p: p.get("number") or 0, reverse=True)
    if open_pulls:
        return pr_answer(open_pulls[0], branch)
    if reconcile:
        return failed("no pull request is open for %s" % branch)
    title, body = pr_text((request.get("args") or {}).get("text"))
    state.acted = True
    try:
        pull = gh.client.json(
            "POST", gh.path("pulls"), "open the pull request of %s" % branch,
            body={"title": title, "head": branch, "base": config.base_branch(), "body": body, "draft": True},
            ok=(201,),
        ) or {}
    except core.HttpError as e:
        if e.status != 422:
            raise
        open_pulls = gh.pulls_of(branch, "open")
        if open_pulls:
            return pr_answer(open_pulls[0], branch)
        return failed("GitHub refused the pull request of %s: %s" % (branch, e))
    return pr_answer(pull, branch)


# ci.run ---------------------------------------------------------------------------------------


def ci_run(config, request, reconcile, state):
    why = "ci.run"
    branch = request.get("branch") or ""
    head = request.get("head") or ""
    if not core.COMMIT.match(head):
        raise core.Failure("ci.run needs the last commit in head")
    workflow = config.text("ci_workflow", why)
    gh = forge.GitHub(config, why)
    run = gh.last_run(head, workflow)
    if run:
        return run_found(run, workflow, head)
    if reconcile:
        return uncertain("no run of %s at %s yet" % (workflow, core.short(head)))
    remote = gh.branch_head(branch)
    if remote != head:
        return failed("%s is at %s on GitHub, not at %s: push it first" % (branch, core.short(remote), core.short(head)))
    state.acted = True
    try:
        gh.client.call(
            "POST", gh.path("actions/workflows/%s/dispatches" % core.segment(workflow)),
            "start the workflow %s on %s" % (workflow, branch), body={"ref": branch}, ok=(204,),
        )
    except core.HttpError as e:
        if 400 <= e.status < 500:
            return failed(str(e))
        raise
    find = config.number("ci_find_seconds", 20)
    poll = config.number("ci_poll_seconds", 3)
    deadline = time.monotonic() + find
    while True:
        run = gh.last_run(head, workflow)
        if run:
            return run_found(run, workflow, head)
        left = deadline - time.monotonic()
        if left <= 0:
            break
        time.sleep(min(poll, left))
    return uncertain("the workflow %s started on %s, but no run at %s showed up in %d s. The tick looks again" % (
        workflow, branch, core.short(head), find))


def run_found(run, workflow, head):
    return confirmed(
        "the run %s of %s at %s is %s" % (run.get("id"), workflow, core.short(head), forge.run_state(run)),
        "run-%s" % run.get("id"),
    )


# review.reply ---------------------------------------------------------------------------------


def verdict_rows(text):
    """The rows of verdicts.json: a JSON list, or an object with the list under `verdicts`."""
    if isinstance(text, str):
        stripped = text.strip()
        if stripped.startswith("```"):
            stripped = stripped.split("\n", 1)[1] if "\n" in stripped else ""
            stripped = stripped.rstrip()
            if stripped.endswith("```"):
                stripped = stripped[:-3]
        try:
            data = json.loads(stripped)
        except ValueError:
            raise core.Failure("args.verdicts of review.reply is not JSON: it must be {doc:verdicts.json}")
    else:
        data = text
    if isinstance(data, dict):
        data = data.get("verdicts")
    if not isinstance(data, list):
        raise core.Failure("review.reply needs args.verdicts: the JSON list of verdicts.json")
    rows = []
    for n, row in enumerate(data):
        raw = row.get("comment_id") if isinstance(row, dict) else None
        if isinstance(raw, str) and raw.strip().lstrip("#").isdigit():
            raw = int(raw.strip().lstrip("#"))
        if not isinstance(raw, int) or isinstance(raw, bool):
            raise core.Failure("row %d of verdicts.json has no comment_id number" % (n + 1))
        reply = row.get("reply")
        rows.append((raw, reply if isinstance(reply, str) else ""))
    return rows


def review_reply(config, request, reconcile, state):
    why = "review.reply"
    rows = verdict_rows((request.get("args") or {}).get("verdicts"))
    own = config.logins("own_logins")
    if not own:
        raise core.Failure("review.reply needs the key `own_logins` in the [adapter] table, so that it never posts a reply twice")
    gh = forge.GitHub(config, why)
    pull = gh.find_pr(request.get("pr"), request.get("branch") or "")
    if not pull:
        return failed("no pull request of %s" % (request.get("branch") or "the ticket"))
    number = pull["number"]
    groups = forge.threads(gh.review_comments(number))
    root_of = {c.get("id"): root for root, items in groups.items() for c in items}
    posted, skipped, missing, unanswered = [], [], [], []
    for comment_id, reply in rows:
        if not reply.strip():
            continue
        root = root_of.get(comment_id)
        if root is None:
            missing.append(comment_id)
            continue
        if any(forge.login(c.get("user")).lower() in own and same_text(c.get("body"), reply) for c in groups[root]):
            skipped.append(comment_id)
            continue
        if reconcile:
            unanswered.append(comment_id)
            continue
        state.acted = True
        gh.client.call(
            "POST", gh.path("pulls/%d/comments/%d/replies" % (number, root)),
            "reply to the comment %d of #%d" % (comment_id, number), body={"body": reply}, ok=(201,),
        )
        groups[root].append({"user": {"login": own[0]}, "body": reply})
        posted.append(comment_id)
    external = "replies-%d" % number
    if reconcile:
        if missing or unanswered:
            return failed("pull request #%d has no reply for the comments %s%s" % (
                number, unanswered + missing, " (no such comment: %s)" % missing if missing else ""))
        return confirmed("each reply on #%d is posted" % number, external)
    detail = "on #%d: posted the replies to %s, and skipped %s, which have the same reply already" % (number, posted, skipped)
    if missing:
        return failed(detail + ". Pull request #%d has no comment %s" % (number, missing))
    return confirmed(detail, external)


# ci.rerun -------------------------------------------------------------------------------------


def ci_rerun(config, request, reconcile, state):
    why = "ci.rerun"
    head = request.get("head") or ""
    if not core.COMMIT.match(head):
        raise core.Failure("ci.rerun needs the last commit in head")
    workflow = config.text("ci_workflow", why)
    gh = forge.GitHub(config, why)
    run = gh.last_run(head, workflow)
    if not run:
        return failed("no run of %s at %s, so nothing runs again" % (workflow, core.short(head)))
    run_id = run.get("id")
    external = "run-%s" % run_id
    attempt = run.get("run_attempt") or 1
    status = forge.run_state(run)
    if status == "running":
        return confirmed("the run %s runs (attempt %s)" % (run_id, attempt), external)
    if status == "green":
        return confirmed("the run %s is green (attempt %s), so nothing runs again" % (run_id, attempt), external)
    if reconcile:
        if attempt > 1:
            return confirmed("the run %s ran again: attempt %s is %s" % (run_id, attempt, status), external)
        return failed("the run %s is still its first attempt and %s: the failed jobs did not run again" % (run_id, status))
    state.acted = True
    try:
        gh.client.call("POST", gh.path("actions/runs/%d/rerun-failed-jobs" % int(run_id)),
                       "run the failed jobs of the run %s again" % run_id, ok=(201, 204))
    except core.HttpError as e:
        if 400 <= e.status < 500:
            return failed(str(e))
        raise
    find = config.number("ci_find_seconds", 20)
    poll = config.number("ci_poll_seconds", 3)
    deadline = time.monotonic() + find
    while True:
        now = gh.client.json("GET", gh.path("actions/runs/%d" % int(run_id)), "read the run %s" % run_id) or {}
        if now.get("status") != "completed" or (now.get("run_attempt") or 1) > attempt:
            return confirmed("the failed jobs of the run %s run again (attempt %s)" % (
                run_id, now.get("run_attempt") or attempt + 1), external)
        left = deadline - time.monotonic()
        if left <= 0:
            return uncertain("GitHub took the rerun of the run %s, but it did not start in %d s. The tick looks again" % (run_id, find))
        time.sleep(min(poll, left))


# deploy ---------------------------------------------------------------------------------------


def deploy(config, request, reconcile, state):
    """Merges the head commit into the branch of an env, with no checkout."""
    why = "deploy"
    key = core.check_key(request.get("key"))
    head = request.get("head") or ""
    if not core.COMMIT.match(head):
        raise core.Failure("deploy needs the last commit in head")
    name = (request.get("args") or {}).get("env")
    if not isinstance(name, str) or not name:
        raise core.Failure("deploy needs args.env: the name of an env of `envs`")
    env = next((e for e in config.envs() if e["name"] == name), None)
    if env is None:
        raise core.Failure("deploy needs the env %s in `envs` of the [adapter] table, with its branch" % name)
    branch = env["branch"]
    if branch == config.base_branch() or BAD_REF.search(branch):
        raise core.Refusal("deploy refuses the branch %r of the env %s: it merges only into a staging branch" % (branch, name))
    gh = forge.GitHub(config, why)
    external = "deploy-%s-%s" % (name, core.short(head))
    holds = gh.holds(branch, head)
    if holds is None:
        return failed("the branch %s of the env %s does not exist on GitHub" % (branch, name))
    if holds:
        return confirmed("%s already holds %s" % (branch, core.short(head)), external)
    if reconcile:
        return failed("%s does not hold %s: the merge did not happen" % (branch, core.short(head)))
    state.acted = True
    response = gh.client.call(
        "POST", gh.path("merges"), "merge %s into %s" % (core.short(head), branch),
        body={"base": branch, "head": head, "commit_message": "Merge %s (%s) into %s" % (key, core.short(head), branch)},
        ok=(201, 204, 404, 409),
    )
    if response.status == 201:
        sha = (response.json() or {}).get("sha") or ""
        return confirmed("merged %s into %s as %s (201)" % (core.short(head), branch, core.short(sha)), external)
    if response.status == 204:
        return confirmed("%s already held %s (204)" % (branch, core.short(head)), external)
    message = core.clean(_reason(response), 300)
    if response.status == 409:
        return failed("conflict: %s does not merge into %s%s. Merge it by hand, or fix the branch" % (
            core.short(head), branch, ": " + message if message else ""))
    return failed("GitHub has no branch %s or no commit %s (404)" % (branch, core.short(head)))


def _reason(response):
    try:
        data = response.json() or {}
    except core.Failure:
        return ""
    return data.get("message") or "" if isinstance(data, dict) else ""


# review.rerequest -----------------------------------------------------------------------------


def review_rerequest(config, request, reconcile, state):
    """Asks each approver whose last review asked for changes for a new review."""
    why = "review.rerequest"
    gh = forge.GitHub(config, why)
    pull = gh.find_pr(request.get("pr"), request.get("branch") or "")
    if not pull:
        return failed("no pull request of %s" % (request.get("branch") or "the ticket"))
    number = pull["number"]
    facts, _state = forge.pr_facts(gh, pull, config)
    asked = facts["changes_by"]
    requested = set(facts["requested"])
    todo = [n for n in asked if n.lower() not in requested]
    external = "rerequest-%d" % number
    if not asked:
        return confirmed("no approver asked for changes on #%d" % number, external)
    if not todo:
        return confirmed("a new review of %s is requested on #%d" % (", ".join(asked), number), external)
    if reconcile:
        return failed("no new review of %s is requested on #%d" % (", ".join(todo), number))
    state.acted = True
    try:
        gh.client.call("POST", gh.path("pulls/%d/requested_reviewers" % number),
                       "ask %s for a new review of #%d" % (", ".join(todo), number),
                       body={"reviewers": todo}, ok=(200, 201))
    except core.HttpError as e:
        if e.status == 422:
            return failed(str(e))
        raise
    return confirmed("asked %s for a new review of #%d" % (", ".join(todo), number), external)


# Read verbs -----------------------------------------------------------------------------------

SEARCH_MAX = 50


def read_doc(name, data, detail):
    return {"detail": detail, "documents": [{"name": name, "content": json.dumps(data, indent=2) + "\n"}]}


def jira_issue(config, request):
    key = core.check_key((request.get("args") or {}).get("key") or request.get("key"))
    tracker = jira.Jira(config, "jira.issue")
    view = tracker.view(tracker.issue(key, jira.ISSUE_FIELDS))
    return read_doc("issue-%s.json" % key, {"key": key, "issue": {"untrusted": view}}, "the issue %s" % key)


def jira_search(config, request):
    args = request.get("args") or {}
    jql = args.get("jql")
    if not isinstance(jql, str) or not jql.strip():
        raise core.Failure("jira.search needs args.jql, such as \"project = DEMO AND text ~ export\"")
    limit = args.get("limit", 20)
    if isinstance(limit, bool) or not isinstance(limit, int) or limit < 1:
        limit = 20
    tracker = jira.Jira(config, "jira.search")
    issues = tracker.search(jql, ["summary", "status", "issuetype"], limit=min(limit, SEARCH_MAX))
    rows = [{"key": i.get("key"), "title": (i.get("fields") or {}).get("summary") or "",
             "status": jira.name_of((i.get("fields") or {}).get("status")),
             "type": jira.name_of((i.get("fields") or {}).get("issuetype"))} for i in issues]
    return read_doc("jira-search.json", {"jql": jql, "issues": {"untrusted": rows}}, "%d issue(s)" % len(rows))


def github_pr(config, request):
    args = request.get("args") or {}
    number = args.get("number") or request.get("pr")
    if number is not None and (isinstance(number, bool) or not isinstance(number, int)):
        raise core.Failure("github.pr needs args.number: a pull request number")
    gh = forge.GitHub(config, "github.pr")
    pull = gh.find_pr(number, request.get("branch") or "")
    if not pull:
        raise core.Failure("no pull request of %s" % (request.get("branch") or "the ticket"))
    n = pull["number"]
    facts, review = forge.pr_facts(gh, pull, config)
    reviews = [{"author": forge.login(r.get("user")), "state": r.get("state"), "text": r.get("body") or ""}
               for r in gh.reviews(n) if r.get("state") != "PENDING"]
    threads = [[{"id": c.get("id"), "author": forge.login(c.get("user")), "path": c.get("path"),
                 "line": c.get("line") or c.get("original_line"), "text": c.get("body") or ""} for c in items]
               for _root, items in sorted(forge.threads(gh.review_comments(n)).items())]
    data = {"pr": facts, "review_state": review, "untrusted": {
        "title": pull.get("title") or "", "body": pull.get("body") or "", "reviews": reviews, "threads": threads}}
    return read_doc("pr-%d.json" % n, data, "the pull request #%d" % n)


def ci_log_read(config, request):
    from . import jobs  # the log reader of the job ci-log
    args = request.get("args") or {}
    run_id = args.get("run")
    if run_id is not None and (isinstance(run_id, bool) or not isinstance(run_id, int)):
        raise core.Failure("ci.log needs args.run: a run id, or nothing for the last run at the head")
    head = request.get("head") or ""
    if run_id is None and not core.COMMIT.match(head):
        raise core.Failure("ci.log needs the head commit, or args.run")
    workflow = config.text("ci_workflow", "ci.log")
    gh = forge.GitHub(config, "ci.log")
    keep = int(config.number("log_tail_kb", 64) * 1024)
    content, detail = jobs.ci_log_text(gh, config, head, workflow, keep, run_id=run_id)
    name = "ci-log-%s.txt" % (run_id or core.short(head))
    return {"detail": detail, "documents": [{"name": name, "content": content}]}


VERBS = {
    "tracker.describe": tracker_describe,
    "tracker.move": tracker_move,
    "git.push": git_push,
    "pr.open": pr_open,
    "ci.run": ci_run,
    "ci.rerun": ci_rerun,
    "deploy": deploy,
    "review.reply": review_reply,
    "review.rerequest": review_rerequest,
}

READS = {
    "jira.issue": jira_issue,
    "jira.search": jira_search,
    "github.pr": github_pr,
    "ci.log": ci_log_read,
}
