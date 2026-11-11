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
        return self._holds(core.ref_path(branch), branch, head)

    def commit_holds(self, commit, head):
        """True when the commit `commit` holds the commit `head`: the same test as `holds`, with
        a commit in the place of the branch. None when GitHub does not know the commit."""
        if commit == head:
            return True
        return self._holds(core.segment(commit), core.short(commit), head)

    def _holds(self, ref, shown, head):
        try:
            data = self.client.json(
                "GET", self.path("compare/%s...%s" % (ref, core.segment(head))),
                "compare %s with %s" % (shown, core.short(head)),
            ) or {}
        except core.HttpError as e:
            if e.status == 404:
                return None
            raise
        return data.get("status") in ("behind", "identical")

    def latest_deployment(self, environment):
        """The newest deployment of a GitHub environment, or None. One call: the statuses of the
        deployment are not read."""
        data = self.client.json("GET", self.path("deployments"), "list the deployments of %s" % environment,
                                query={"environment": environment, "per_page": 1})
        items = [d for d in data if isinstance(d, dict)] if isinstance(data, list) else []
        return items[0] if items else None

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

    def issue_comments(self, number):
        """The conversation comments of the pull request."""
        return [c for c in self.client.pages(self.path("issues/%d/comments" % number),
                                             "list the conversation comments of #%d" % number)
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


# The last line of a reply of Vision to a review or to a conversation comment: it names the
# kind and the id of the comment that it answers.
MARK = re.compile(r"^<!-- vision-reply (review|conversation) (\d{1,20}) -->$")
KINDS = ("inline", "review", "conversation")


def first_line(text, limit=200):
    """The first line of a text that is not empty, cut, with no mark of an HTML comment, so
    that a quote of it can never hold the mark of a reply."""
    for line in str(text or "").splitlines():
        line = line.strip()
        if line:
            line = line.replace("<!--", "").replace("-->", "").strip()
            return line if len(line) <= limit else line[:limit] + "..."
    return ""


def quoted(text):
    """The quote of the first line of a review or a conversation comment."""
    return "> " + first_line(text)


MENTION = re.compile(r"@(?=[A-Za-z0-9])")


def no_mentions(text):
    """A text whose mentions call nobody: a zero-width space after each `@` that starts a name.
    A quote of a comment must not notify its people a second time."""
    return MENTION.sub("@\u200b", str(text or ""))


def reply_marks(issue_comments, own):
    """What the conversation comments of `own` (lowercase logins) answer: the (kind, id) of the
    mark of their last line, and the quoted first lines."""
    marks, quotes = set(), set()
    for c in issue_comments:
        if login(c.get("user")).lower() not in own:
            continue
        lines = [line.strip() for line in str(c.get("body") or "").splitlines() if line.strip()]
        if lines:
            match = MARK.match(lines[-1])
            if match:
                marks.add((match.group(1), int(match.group(2))))
        quotes.update(line for line in lines if line.startswith("> ") and len(line) > 2)
    return marks, quotes


def reply_body(kind, comment_id, original, reply):
    """A reply to a review or a conversation comment: a new conversation comment that quotes
    the first line of the comment, the reply, and the mark of the comment."""
    return "%s\n\n%s\n\n<!-- vision-reply %s %d -->" % (original, reply.strip(), kind, comment_id)


def last_states(reviews):
    """The last review state of each author (APPROVED, CHANGES_REQUESTED, or DISMISSED), by the
    lowercase login, and the login as GitHub shows it."""
    last, shown = {}, {}
    for review in sorted(reviews, key=lambda r: r.get("id") or 0):
        state = review.get("state")
        name = login(review.get("user"))
        if state in REVIEW_STATES and name:
            last[name.lower()] = state
            shown[name.lower()] = name
    return last, shown


def review_first_line(review):
    return first_line(review.get("body")) or "%s asked for changes." % login(review.get("user"))


class Ignore:
    """The comments that never wait, from the entries of `ignore_logins`: a login (each kind of
    its comments), or `<login>:<kind>` (only that kind), with the kind inline, review, or
    conversation, such as "review-bot[bot]:conversation". A login can hold `[`, `]`, and `-`, so
    an entry splits at its last `:`. Logins are lowercase."""

    def __init__(self, entries=()):
        self.every = set()
        self.kinds = {kind: set() for kind in KINDS}
        for entry in entries:
            text = str(entry).strip().lower()
            if not text:
                continue
            name, colon, kind = text.rpartition(":")
            if not colon:
                self.every.add(text)
                continue
            if kind not in KINDS or not name.strip():
                raise core.Failure(
                    "the entry %r of `ignore_logins` must be a login or <login>:<kind>, with the kind "
                    "inline, review, or conversation, such as \"review-bot[bot]:conversation\"" % core.clean(entry, 80))
            self.kinds[kind].add(name.strip())

    @classmethod
    def of(cls, value):
        return value if isinstance(value, Ignore) else cls(value or ())

    def skips(self, name, kind):
        name = str(name or "").lower()
        return name in self.every or name in self.kinds[kind]


def waiting_rows(reviews, review_comments, issue_comments, own, ignore=()):
    """Each comment of a pull request that waits for an answer, as one row with its kind:

    - `inline`: a thread of review comments whose last comment is not from `own`. Its id is
      the id of the first comment, and a reply goes into the thread.
    - `review`: a review with a body of a login that is not in `own`, that asks for changes
      (while that is the last state of its author) or that comments. A review with no body
      has nothing to answer: its inline comments are rows of their own.
    - `conversation`: a conversation comment of a login that is not in `own`.

    A review or a conversation comment waits until a conversation comment of `own` answers it:
    one with its mark, or one that quotes its first line. A comment that `ignore` (an `Ignore`,
    or the entries of `ignore_logins`) names never waits: the rows leave it out, as if nobody
    wrote it. The text of each row is untrusted."""
    ignore = Ignore.of(ignore)
    rows = []
    review_comments = [c for c in review_comments if not ignore.skips(login(c.get("user")), "inline")]
    for root, items in sorted(waiting_threads(review_comments, own).items()):
        first, last = items[0], items[-1]
        mine = [i for i, c in enumerate(items) if login(c.get("user")).lower() in own]
        new = items[mine[-1] + 1:] if mine else items
        if len(new) == 1:
            text = new[0].get("body") or ""
        else:
            text = "\n\n".join("%s: %s" % (login(c.get("user")), c.get("body") or "") for c in new)
        rows.append({
            "id": root,
            "kind": "inline",
            "author": login(last.get("user")),
            "path": first.get("path"),
            "line": first.get("line") or first.get("original_line"),
            "text": {"untrusted": text},
            "thread": [{"id": c.get("id"), "author": login(c.get("user")), "text": {"untrusted": c.get("body") or ""}}
                       for c in items],
        })
    marks, quotes = reply_marks(issue_comments, own)

    def answered(kind, comment_id, text):
        line = first_line(text)
        return (kind, comment_id) in marks or (bool(line) and ("> " + line) in quotes)

    last, _shown = last_states(reviews)
    for review in sorted(reviews, key=lambda r: r.get("id") or 0):
        name = login(review.get("user"))
        state = review.get("state")
        body = review.get("body") or ""
        if not name or name.lower() in own or ignore.skips(name, "review") or not isinstance(review.get("id"), int):
            continue
        if not body.strip():
            continue
        if state == "CHANGES_REQUESTED":
            if last.get(name.lower()) != "CHANGES_REQUESTED":
                continue
        elif state != "COMMENTED":
            continue
        if answered("review", review["id"], body):
            continue
        rows.append({
            "id": review["id"],
            "kind": "review",
            "state": state,
            "author": name,
            "path": None,
            "line": None,
            "text": {"untrusted": body},
            "thread": [{"id": review["id"], "author": name, "text": {"untrusted": body}}],
        })
    for c in sorted(issue_comments, key=lambda c: c.get("id") or 0):
        name = login(c.get("user"))
        if not name or name.lower() in own or ignore.skips(name, "conversation") or not isinstance(c.get("id"), int):
            continue
        body = c.get("body") or ""
        if answered("conversation", c["id"], body):
            continue
        rows.append({
            "id": c["id"],
            "kind": "conversation",
            "author": name,
            "path": None,
            "line": None,
            "text": {"untrusted": body},
            "thread": [{"id": c["id"], "author": name, "text": {"untrusted": body}}],
        })
    return rows


def pr_facts(gh, pr, config):
    """The pull request facts of design 11.1, with the review state of the in-review step.

    The review state is the first of these that holds: `merged`, `closed`, `comments` (a
    comment waits), `changes` (a reviewer asks for changes and is not asked again), `conflict`
    (the open pull request conflicts with its base), `approved`, and `waiting`."""
    number = pr["number"]
    approvers = config.logins("approvers")
    own = config.logins("own_logins")
    ignore = Ignore(config.logins("ignore_logins"))
    needed = int(config.number("approvals_needed", 1))

    def counts(name):
        return name in approvers if approvers else name not in own

    reviews = gh.reviews(number)
    last, shown = last_states(reviews)
    approved_by = sorted(shown[n] for n, s in last.items() if s == "APPROVED" and counts(n))
    asked = sorted(shown[n] for n, s in last.items() if s == "CHANGES_REQUESTED" and counts(n))
    requested = {login(u).lower() for u in pr.get("requested_reviewers") or []}
    # Each comment that waits, of each kind: the same rows as the job comments gives.
    waiting = waiting_rows(reviews, gh.review_comments(number), gh.issue_comments(number), own, ignore)
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
    elif facts["conflict"]:
        state = "conflict"
    elif len(approved_by) >= needed and not asked:
        state = "approved"
    else:
        state = "waiting"
    return facts, state


KEY_IN = r"(?<![A-Za-z0-9])%s(?![0-9])"


def names_key(text, key):
    return bool(re.search(KEY_IN % re.escape(key), str(text or ""), re.IGNORECASE))
