"""Canned objects of Jira and GitHub, and a small stateful world on the fake server."""

import copy

from support import BRANCH, KEY

HEAD = "a" * 40
OTHER = "b" * 40
REPO_PATH = "/repos/acme/widgets"


def adf_doc(*paragraphs):
    return {"type": "doc", "version": 1, "content": [
        {"type": "paragraph", "content": [{"type": "text", "text": p}]} for p in paragraphs
    ]}


def jira_issue(key=KEY, summary="Export the report", status="To Do", description=None, comments=(), parent=None):
    fields = {
        "summary": summary,
        "status": {"name": status},
        "description": description,
        "issuetype": {"name": "Task"},
        "priority": {"name": "Medium"},
        "labels": ["reports"],
        "assignee": None,
        "comment": {"comments": [
            {"id": str(10 + n), "author": {"displayName": author}, "created": "2026-11-01T10:00:00.000+0000",
             "body": adf_doc(text)} for n, (author, text) in enumerate(comments)
        ], "total": len(comments)},
    }
    if parent:
        fields["parent"] = {"key": parent, "fields": {"summary": "The parent"}}
    return {"key": key, "fields": fields}


def pull(number=7, draft=True, sha=HEAD, state="open", merged=False, mergeable_state="clean", requested=()):
    return {
        "number": number,
        "state": state,
        "draft": draft,
        "merged": merged,
        "merged_at": "2026-11-02T10:00:00Z" if merged else None,
        "mergeable": mergeable_state != "dirty",
        "mergeable_state": mergeable_state,
        "head": {"ref": BRANCH, "sha": sha},
        "base": {"ref": "main"},
        "requested_reviewers": [{"login": r} for r in requested],
        "html_url": "https://github.example/acme/widgets/pull/%d" % number,
        "title": "Export the report",
    }


def review(review_id, login, state):
    return {"id": review_id, "user": {"login": login}, "state": state}


def comment(comment_id, login, body, reply_to=None, path="app.py", line=3):
    item = {"id": comment_id, "user": {"login": login}, "body": body, "path": path, "line": line,
            "original_line": line, "created_at": "2026-11-02T10:00:%02dZ" % (comment_id % 60)}
    if reply_to:
        item["in_reply_to_id"] = reply_to
    return item


def run(run_id, sha=HEAD, status="completed", conclusion="success", path=".github/workflows/ci.yml", attempt=1):
    return {"id": run_id, "head_sha": sha, "status": status, "conclusion": conclusion, "path": path,
            "created_at": "2026-11-02T10:%02d:00Z" % (run_id % 60), "run_attempt": attempt,
            "html_url": "https://github.example/acme/widgets/actions/runs/%d" % run_id}


class GitHubWorld:
    """The pull request #7 of the branch, with its reviews, comments, and runs, as state that the
    fake server serves and that the POST routes change."""

    def __init__(self, server, pr=None):
        self.server = server
        self.pr = pr if pr is not None else pull()
        self.reviews = []
        self.comments = []
        self.runs = []
        self.branch_sha = HEAD
        self.next_id = 900
        self.install()

    def install(self):
        s = self.server
        s.route("GET", REPO_PATH + "/pulls", self.list_pulls)
        s.route("GET", REPO_PATH + "/pulls/7", lambda r: (200, self.pr) if self.pr else (404, {"message": "Not Found"}))
        s.route("GET", REPO_PATH + "/pulls/7/reviews", lambda r: (200, self.page(self.reviews, r)))
        s.route("GET", REPO_PATH + "/pulls/7/comments", lambda r: (200, self.page(self.comments, r)))
        s.route("GET", REPO_PATH + "/actions/runs", self.list_runs)
        s.route("GET", REPO_PATH + "/git/ref/heads/" + BRANCH, self.branch_ref)

    @staticmethod
    def page(items, req):
        per = int(req["query"].get("per_page", 30))
        page = int(req["query"].get("page", 1))
        return copy.deepcopy(items[(page - 1) * per:page * per])

    def list_pulls(self, req):
        if not self.pr or req["query"].get("head") != "acme:" + BRANCH:
            return 200, []
        state = req["query"].get("state", "open")
        if state != "all" and self.pr["state"] != state:
            return 200, []
        return 200, self.page([self.pr], req)

    def list_runs(self, req):
        sha = req["query"].get("head_sha")
        return 200, {"total_count": 0, "workflow_runs": self.page([r for r in self.runs if r["head_sha"] == sha], req)}

    def branch_ref(self, req):
        if not self.branch_sha:
            return 404, {"message": "Not Found"}
        return 200, {"ref": "refs/heads/" + BRANCH, "object": {"sha": self.branch_sha, "type": "commit"}}

    def add_reply_route(self, login="vision-bot"):
        def reply(req):
            root = int(req["path"].rsplit("/", 2)[-2])
            self.next_id += 1
            self.comments.append(comment(self.next_id, login, req["body"]["body"], reply_to=root))
            return 201, self.comments[-1]
        for c in list(self.comments):
            if not c.get("in_reply_to_id"):
                self.server.route("POST", REPO_PATH + "/pulls/7/comments/%d/replies" % c["id"], reply)
