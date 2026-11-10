"""The client of GitHub REST, and the facts of a pull request, its reviews, and its CI."""

import re

from . import core

GREEN = {"success"}
RED = {"failure", "timed_out", "startup_failure", "cancelled"}
REVIEW_STATES = {"APPROVED", "CHANGES_REQUESTED", "DISMISSED"}


class GitHub:
    def __init__(self, config, why, token=None):
        self.config = config
        self.repo = config.repo(why)
        self.owner, self.name = self.repo.split("/")
        base = core.check_base(config.text("github_api", default=core.DEFAULT_GITHUB_API), "github_api")
        token = token or core.secret("FORGE_TOKEN", why)
        headers = {
            "Authorization": "Bearer " + token,
            "Accept": "application/vnd.github+json",
            "X-GitHub-Api-Version": "2022-11-28",
            "User-Agent": "vision-jira-github-adapter",
        }
        self.client = core.Client("github", base, headers, config.http_timeout())

    def path(self, *parts):
        return "/repos/%s/%s" % (core.segment(self.owner), core.segment(self.name)) + "".join("/" + p for p in parts)

    # Branches and commits.

    def branch_head(self, branch):
        """The commit of a branch on GitHub, or None when it does not exist."""
        try:
            data = self.client.json("GET", self.path("git/ref/heads/" + core.ref_path(branch)), "read the branch %s" % branch)
        except core.HttpError as e:
            if e.status == 404:
                return None
            raise
        obj = (data or {}).get("object") or {}
        return obj.get("sha")

    def branches(self, max_pages=20):
        return [b.get("name") for b in self.client.pages(self.path("branches"), "list the branches", max_pages=max_pages)
                if isinstance(b, dict) and isinstance(b.get("name"), str)]

    def holds(self, branch, head):
        """True when `branch` holds the commit `head`; None when the branch does not exist."""
        try:
            data = self.client.json(
                "GET", self.path("compare/%s...%s" % (core.ref_path(branch), core.segment(head))),
                "compare %s with %s" % (branch, core.short(head)),
            ) or {}
        except core.HttpError as e:
            if e.status == 404:
                return None
            raise
        return data.get("status") in ("behind", "identical")

    # Pull requests.

    def pulls_of(self, branch, state="all"):
        query = {"head": "%s:%s" % (self.owner, branch), "state": state}
        return [p for p in self.client.pages(self.path("pulls"), "list the pull requests of %s" % branch, query=query, max_pages=3)
                if isinstance(p, dict)]

    def pull(self, number):
        return self.client.json("GET", self.path("pulls/%d" % int(number)), "read the pull request #%d" % int(number)) or {}

    def find_pr(self, number, branch):
        """The pull request of the call: the number of the ticket row, else the open one of the
        branch, else the newest merged one, else the newest one. Read in full, or None."""
        if number:
            try:
                return self.pull(number)
            except core.HttpError as e:
                if e.status != 404:
                    raise
        if not branch:
            return None
        pulls = sorted(self.pulls_of(branch), key=lambda p: p.get("number") or 0, reverse=True)
        chosen = next((p for p in pulls if p.get("state") == "open"), None)
        chosen = chosen or next((p for p in pulls if p.get("merged_at")), None)
        chosen = chosen or (pulls[0] if pulls else None)
        return self.pull(chosen["number"]) if chosen else None

    def reviews(self, number):
        return [r for r in self.client.pages(self.path("pulls/%d/reviews" % number), "list the reviews of #%d" % number)
                if isinstance(r, dict)]

    def review_comments(self, number):
        return [c for c in self.client.pages(self.path("pulls/%d/comments" % number), "list the review comments of #%d" % number)
                if isinstance(c, dict)]

    # Actions.

    def runs_at(self, head, workflow):
        """The runs of the workflow file at the commit, newest first."""
        runs = self.client.pages(self.path("actions/runs"), "list the runs of %s" % core.short(head),
                                 query={"head_sha": head}, max_pages=3)
        mine = [r for r in runs if isinstance(r, dict) and r.get("head_sha") == head and workflow_matches(r, workflow)]
        return sorted(mine, key=lambda r: (r.get("created_at") or "", r.get("id") or 0), reverse=True)

    def last_run(self, head, workflow):
        runs = self.runs_at(head, workflow) if head else []
        return runs[0] if runs else None


def workflow_matches(run, workflow):
    path = str(run.get("path") or "").split("@")[0]
    name = workflow.rsplit("/", 1)[-1]
    return path.rsplit("/", 1)[-1] == name or str(run.get("workflow_id")) == str(workflow)


def run_state(run):
    """green or red for a run that ended; running, none, or the conclusion for the others."""
    if not run:
        return "none"
    if run.get("status") != "completed":
        return "running"
    conclusion = run.get("conclusion") or "unknown"
    if conclusion in GREEN:
        return "green"
    if conclusion in RED:
        return "red"
    return conclusion


def login(user):
    return str((user or {}).get("login") or "") if isinstance(user, dict) else ""


def threads(comments):
    """The review comment threads: the root comment, then its replies, oldest first."""
    by_id = {c.get("id"): c for c in comments if isinstance(c.get("id"), int)}

    def root_of(c):
        seen = set()
        while c.get("in_reply_to_id") in by_id and c.get("id") not in seen:
            seen.add(c.get("id"))
            c = by_id[c["in_reply_to_id"]]
        return c.get("in_reply_to_id") or c.get("id")

    groups = {}
    for c in sorted(by_id.values(), key=lambda c: c["id"]):
        groups.setdefault(root_of(c), []).append(c)
    return groups


def waiting_threads(comments, own):
    """The threads whose last comment is not from one of `own` (lowercase logins)."""
    out = {}
    for root, items in threads(comments).items():
        if login(items[-1].get("user")).lower() not in own:
            out[root] = items
    return out


def pr_facts(gh, pr, config):
    """The pull request facts of design 11.1, with the review state of the in-review step."""
    number = pr["number"]
    approvers = config.logins("approvers")
    own = config.logins("own_logins")
    needed = int(config.number("approvals_needed", 1))

    def counts(name):
        return name in approvers if approvers else name not in own

    last = {}
    shown = {}
    for review in sorted(gh.reviews(number), key=lambda r: r.get("id") or 0):
        state = review.get("state")
        name = login(review.get("user"))
        if state in REVIEW_STATES and name:
            last[name.lower()] = state
            shown[name.lower()] = name
    approved_by = sorted(shown[n] for n, s in last.items() if s == "APPROVED" and counts(n))
    asked = sorted(shown[n] for n, s in last.items() if s == "CHANGES_REQUESTED" and counts(n))
    requested = {login(u).lower() for u in pr.get("requested_reviewers") or []}
    waiting = waiting_threads(gh.review_comments(number), own)
    merged = bool(pr.get("merged") or pr.get("merged_at"))
    closed = pr.get("state") == "closed" and not merged
    facts = {
        "number": number,
        "draft": bool(pr.get("draft")),
        "head": (pr.get("head") or {}).get("sha"),
        "branch": (pr.get("head") or {}).get("ref"),
        "base": (pr.get("base") or {}).get("ref"),
        "merged": merged,
        "closed": closed,
        "conflict": pr.get("mergeable_state") == "dirty" or pr.get("mergeable") is False,
        "approved_by": approved_by,
        "changes_requested": bool(asked),
        "changes_by": asked,
        "requested": sorted(requested),
        "comments_waiting": len(waiting),
        "url": pr.get("html_url") or "",
    }
    if merged:
        state = "merged"
    elif closed:
        state = "closed"
    elif waiting:
        state = "comments"
    elif any(n.lower() not in requested for n in asked):
        state = "changes"
    elif len(approved_by) >= needed and not asked:
        state = "approved"
    else:
        state = "waiting"
    return facts, state


KEY_IN = r"(?<![A-Za-z0-9])%s(?![0-9])"


def names_key(text, key):
    return bool(re.search(KEY_IN % re.escape(key), str(text or ""), re.IGNORECASE))
