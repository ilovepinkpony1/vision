"""A fake Jira and GitHub with state, for the whole path of a ticket.

The world serves the calls of the adapter on a FakeServer (see support.py) and keeps the state
that the path needs:

- Jira: issues with an ADF description, a status, and transitions; the search of the queue.
- GitHub: the branches of a local bare repo, pull requests, reviews, review comments with their
  replies, conversation comments, requested reviewers, and workflow runs with jobs and logs.
  A run runs `ci.sh` of the commit in a temporary folder: its exit code is the conclusion and
  its output is the log of its one job. Each attempt of a run shows `queued` at its first read
  and `in_progress` at its second, and it has ended from its third read on, as a real run
  takes its time.

A person or a test plays the team with the control calls (`POST /_control/<action>`, a JSON
body): ready, review, comment, issue_comment, merge, and `GET /_control/state`. They are not
calls of the adapter, so the request log leaves them out.

Each call of the adapter must carry the token of its service. The world checks it, and the
request log records only which token came (`jira`, `forge`, `none`, or `bad`), never a value.
"""

import copy
import datetime
import json
import os
import re
import subprocess
import tarfile
import io
import tempfile
import threading

REPO = "acme/widgets"
OWNER = "acme"
REPO_PATH = "/repos/acme/widgets"
WORKFLOW = "ci.yml"
LOG_KEEP = 20000

TRANSITIONS = [
    {"id": "11", "name": "Start", "to": {"name": "In Progress"}},
    {"id": "21", "name": "Ask for a review", "to": {"name": "In Review"}},
    {"id": "31", "name": "Finish", "to": {"name": "Done"}},
    {"id": "41", "name": "Stop", "to": {"name": "To Do"}},
]


def now():
    return datetime.datetime.now(datetime.timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")


def adf(text):
    """A plain text as an ADF document: one paragraph for each block of lines."""
    blocks = [b.strip() for b in (text or "").split("\n\n") if b.strip()]
    return {"type": "doc", "version": 1, "content": [
        {"type": "paragraph", "content": [{"type": "text", "text": b}]} for b in blocks
    ]}


def plain_env():
    env = {k: os.environ[k] for k in ("PATH", "HOME", "LANG", "TMPDIR") if k in os.environ}
    env.update({"GIT_CONFIG_NOSYSTEM": "1", "GIT_CONFIG_GLOBAL": os.devnull, "GIT_TERMINAL_PROMPT": "0"})
    return env


def git(args, cwd, check=True, data=None):
    proc = subprocess.run(
        ["git", "-c", "core.hooksPath=/dev/null", "-c", "commit.gpgsign=false", "-c", "user.name=team",
         "-c", "user.email=team@example.invalid"] + list(args),
        cwd=cwd, input=data, stdout=subprocess.PIPE, stderr=subprocess.PIPE, env=plain_env(), timeout=120,
    )
    if check and proc.returncode != 0:
        raise RuntimeError("git %s failed: %s" % (" ".join(args), proc.stderr.decode("utf-8", "replace")))
    return proc


class World:
    """The state of the fake services, and the handler of each call."""

    def __init__(self, remote, tokens=None, jira_user=None):
        self.remote = remote
        self.tokens = tokens or {}
        self.jira_user = jira_user
        self.lock = threading.RLock()
        self.issues = {}
        self.pulls = {}
        self.reviews = {}
        self.comments = {}
        self.issue_comments = {}
        self.runs = []
        self.jobs = {}
        self.logs = {}
        self.next_pull = 7
        self.next_id = 1000
        self.next_run = 40

    # Seeds.

    def add_issue(self, key, title, description, status="To Do"):
        with self.lock:
            self.issues[key] = {"key": key, "summary": title, "description": adf(description), "status": status}

    def new_id(self, wanted=None):
        if wanted is not None:
            return int(wanted)
        self.next_id += 1
        return self.next_id

    # The bare repo.

    def branch_head(self, branch):
        proc = git(["rev-parse", "--verify", "--quiet", "refs/heads/" + branch], self.remote, check=False)
        text = proc.stdout.decode().strip()
        return text if proc.returncode == 0 and text else None

    def branches(self):
        text = git(["for-each-ref", "--format=%(refname:strip=2)", "refs/heads"], self.remote).stdout.decode()
        return [line for line in text.splitlines() if line]

    def run_ci(self, sha):
        """Runs ci.sh of the commit. Gives (conclusion, log)."""
        with tempfile.TemporaryDirectory(prefix="fake-ci-") as tmp:
            data = git(["archive", "--format=tar", sha], self.remote).stdout
            with tarfile.open(fileobj=io.BytesIO(data)) as archive:
                if hasattr(tarfile, "data_filter"):
                    archive.extractall(tmp, filter="data")
                else:
                    archive.extractall(tmp)
            if not os.path.isfile(os.path.join(tmp, "ci.sh")):
                return "success", "There is no ci.sh at this commit.\n"
            env = plain_env()
            env["PYTHONDONTWRITEBYTECODE"] = "1"
            proc = subprocess.run(["sh", "ci.sh"], cwd=tmp, stdout=subprocess.PIPE, stderr=subprocess.STDOUT,
                                  env=env, timeout=300)
            output = proc.stdout.decode("utf-8", "replace")[-LOG_KEEP:]
            return ("success" if proc.returncode == 0 else "failure"), output

    # Views.

    def pull_view(self, pull):
        view = copy.deepcopy(pull)
        if not view["merged"]:
            view["head"]["sha"] = self.branch_head(view["head"]["ref"]) or view["head"]["sha"]
        view["requested_reviewers"] = [{"login": n} for n in pull["requested"]]
        del view["requested"]
        return view

    def issue_view(self, key, fields):
        issue = self.issues[key]
        all_fields = {
            "summary": issue["summary"],
            "status": {"name": issue["status"]},
            "description": issue["description"],
            "issuetype": {"name": "Task"},
            "priority": {"name": "Medium"},
            "labels": [],
            "assignee": None,
            "comment": {"comments": [], "total": 0},
        }
        wanted = [f for f in (fields or "").split(",") if f] or list(all_fields)
        return {"key": key, "fields": {f: all_fields[f] for f in wanted if f in all_fields}}

    def run_view(self, run, count=True):
        """The run as GitHub shows it. A read of the adapter counts toward the end of the
        attempt; a read of the control calls does not."""
        reads = run.get("_reads", 0)
        if count:
            run["_reads"] = reads + 1
        view = {k: v for k, v in run.items() if not k.startswith("_")}
        if reads < 2:
            view.update({"status": "queued" if reads == 0 else "in_progress", "conclusion": None})
        view["path"] = ".github/workflows/" + WORKFLOW
        view["html_url"] = "https://github.example/%s/actions/runs/%d" % (REPO, run["id"])
        return view

    # The handler.

    def auth_of(self, record):
        header = record["headers"].get("authorization")
        if not header:
            return "none"
        forge = self.tokens.get("forge")
        if forge and header == "Bearer " + forge:
            return "forge"
        jira = self.tokens.get("jira")
        if jira and header.startswith("Basic "):
            import base64
            try:
                user, _, token = base64.b64decode(header[6:]).decode().partition(":")
            except ValueError:
                return "bad"
            if token == jira and (self.jira_user is None or user == self.jira_user):
                return "jira"
        return "bad"

    def handle(self, record):
        path = record["path"]
        if path == "/health":
            return 200, {"ok": True}
        if path.startswith("/storage/logs/"):
            job_id = int(path.rsplit("/", 1)[1])
            with self.lock:
                return (200, self.logs.get(job_id, ""), {"Content-Type": "text/plain"})
        auth = record.get("auth")
        if path.startswith("/rest/api/3/"):
            if auth != "jira":
                return 401, {"errorMessages": ["The token of Jira is missing or wrong."]}
            with self.lock:
                return self.jira(record, path[len("/rest/api/3"):])
        if path.startswith(REPO_PATH):
            if auth != "forge":
                return 401, {"message": "Bad credentials"}
            with self.lock:
                return self.github(record, path[len(REPO_PATH):])
        return None

    def jira(self, record, path):
        method, body = record["method"], record["body"] or {}
        if method == "POST" and path == "/search/jql":
            todo = [k for k, i in self.issues.items() if i["status"] == "To Do"]
            fields = ",".join(body.get("fields") or [])
            return 200, {"issues": [self.issue_view(k, fields) for k in sorted(todo)], "isLast": True}
        match = re.match(r"^/issue/([A-Z][A-Z0-9_]*-\d+)(/transitions)?$", path)
        if not match or match.group(1) not in self.issues:
            return 404, {"errorMessages": ["Issue does not exist or you do not have permission to see it."]}
        key = match.group(1)
        issue = self.issues[key]
        if match.group(2):
            if method == "GET":
                return 200, {"transitions": [t for t in TRANSITIONS if t["to"]["name"] != issue["status"]]}
            if method == "POST":
                wanted = str((body.get("transition") or {}).get("id"))
                found = next((t for t in TRANSITIONS if t["id"] == wanted), None)
                if not found:
                    return 400, {"errorMessages": ["The transition %s is not valid." % wanted]}
                issue["status"] = found["to"]["name"]
                return 204, None
        elif method == "GET":
            return 200, self.issue_view(key, record["query"].get("fields"))
        elif method == "PUT":
            description = ((body.get("fields") or {}).get("description"))
            if not isinstance(description, dict) or description.get("type") != "doc":
                return 400, {"errors": {"description": "Operation value must be an ADF document"}}
            issue["description"] = description
            return 204, None
        return 405, {"errorMessages": ["Method not allowed"]}

    def page(self, items, record):
        per = int(record["query"].get("per_page", 30))
        page = int(record["query"].get("page", 1))
        return copy.deepcopy(items[(page - 1) * per:page * per])

    def github(self, record, path):
        method, body, query = record["method"], record["body"] or {}, record["query"]
        if method == "GET" and path == "/branches":
            return 200, self.page([{"name": b} for b in self.branches()], record)
        match = re.match(r"^/git/ref/heads/(.+)$", path)
        if method == "GET" and match:
            sha = self.branch_head(match.group(1))
            if not sha:
                return 404, {"message": "Not Found"}
            return 200, {"ref": "refs/heads/" + match.group(1), "object": {"sha": sha, "type": "commit"}}
        if path == "/pulls":
            if method == "POST":
                return self.create_pull(body)
            state = query.get("state", "open")
            head = query.get("head")
            found = []
            for number in sorted(self.pulls):
                pull = self.pulls[number]
                if head and "%s:%s" % (OWNER, pull["head"]["ref"]) != head:
                    continue
                if state != "all" and pull["state"] != state:
                    continue
                found.append(self.pull_view(pull))
            return 200, self.page(found, record)
        match = re.match(r"^/pulls/(\d+)(/.*)?$", path)
        if match:
            number = int(match.group(1))
            if number not in self.pulls:
                return 404, {"message": "Not Found"}
            return self.pull_call(record, number, match.group(2) or "")
        match = re.match(r"^/issues/(\d+)/comments$", path)
        if match:
            number = int(match.group(1))
            if number not in self.pulls:
                return 404, {"message": "Not Found"}
            if method == "POST":
                comment = {"id": self.new_id(), "user": {"login": "vision-bot"}, "body": body.get("body") or "",
                           "created_at": now()}
                self.issue_comments.setdefault(number, []).append(comment)
                return 201, comment
            return 200, self.page(self.issue_comments.get(number, []), record)
        return self.actions(record, path)

    def create_pull(self, body):
        branch = body.get("head")
        if not branch or not self.branch_head(branch):
            return 422, {"message": "Validation Failed", "errors": [{"message": "No commits between main and %s" % branch}]}
        if any(p["head"]["ref"] == branch and p["state"] == "open" for p in self.pulls.values()):
            return 422, {"message": "Validation Failed", "errors": [{"message": "A pull request already exists"}]}
        number = self.next_pull
        self.next_pull += 1
        self.pulls[number] = {
            "number": number, "state": "open", "draft": bool(body.get("draft")), "merged": False, "merged_at": None,
            "mergeable": True, "mergeable_state": "clean", "title": body.get("title") or "", "body": body.get("body") or "",
            "head": {"ref": branch, "sha": self.branch_head(branch)}, "base": {"ref": body.get("base") or "main"},
            "requested": [], "html_url": "https://github.example/%s/pull/%d" % (REPO, number),
        }
        return 201, self.pull_view(self.pulls[number])

    def pull_call(self, record, number, rest):
        method, body = record["method"], record["body"] or {}
        pull = self.pulls[number]
        if rest == "" and method == "GET":
            return 200, self.pull_view(pull)
        if rest == "/reviews" and method == "GET":
            return 200, self.page(self.reviews.get(number, []), record)
        if rest == "/comments" and method == "GET":
            return 200, self.page(self.comments.get(number, []), record)
        match = re.match(r"^/comments/(\d+)/replies$", rest)
        if match and method == "POST":
            root = int(match.group(1))
            items = self.comments.get(number, [])
            first = next((c for c in items if c["id"] == root), None)
            if first is None or first.get("in_reply_to_id"):
                return 404, {"message": "Not Found"}
            reply = {"id": self.new_id(), "user": {"login": "vision-bot"}, "body": body.get("body") or "",
                     "in_reply_to_id": root, "path": first["path"], "line": first["line"],
                     "original_line": first["line"], "created_at": now()}
            items.append(reply)
            return 201, reply
        if rest == "/requested_reviewers" and method == "POST":
            for name in body.get("reviewers") or []:
                if name not in pull["requested"]:
                    pull["requested"].append(name)
            return 201, self.pull_view(pull)
        return 404, {"message": "Not Found"}

    def actions(self, record, path):
        method, body = record["method"], record["body"] or {}
        if method == "GET" and path == "/actions/runs":
            sha = record["query"].get("head_sha")
            runs = [self.run_view(r) for r in reversed(self.runs) if not sha or r["head_sha"] == sha]
            return 200, {"total_count": len(runs), "workflow_runs": self.page(runs, record)}
        if method == "POST" and path == "/actions/workflows/%s/dispatches" % WORKFLOW:
            ref = body.get("ref")
            sha = self.branch_head(ref or "")
            if not sha:
                return 422, {"message": "No ref found for: %s" % ref}
            self.next_run += 1
            run = {"id": self.next_run, "head_sha": sha, "head_branch": ref, "event": "workflow_dispatch",
                   "created_at": now(), "run_attempt": 0}
            self.runs.append(run)
            self.attempt(run)
            return 204, None
        match = re.match(r"^/actions/runs/(\d+)(/rerun-failed-jobs|/jobs)?$", path)
        if match:
            run = next((r for r in self.runs if r["id"] == int(match.group(1))), None)
            if run is None:
                return 404, {"message": "Not Found"}
            if match.group(2) == "/rerun-failed-jobs" and method == "POST":
                if run.get("_reads", 0) < 2:
                    return 403, {"message": "This workflow run is not completed"}
                if run["conclusion"] == "success":
                    return 403, {"message": "This workflow run cannot be rerun"}
                self.attempt(run)
                return 201, {}
            if match.group(2) == "/jobs" and method == "GET":
                jobs = [self.jobs[j] for j in run["_jobs"][-1:]]
                if run.get("_reads", 0) < 2:
                    jobs = [dict(j, status="in_progress", conclusion=None) for j in jobs]
                return 200, {"total_count": len(jobs), "jobs": self.page(jobs, record)}
            if not match.group(2) and method == "GET":
                return 200, self.run_view(run)
        match = re.match(r"^/actions/jobs/(\d+)/logs$", path)
        if match and method == "GET":
            job_id = int(match.group(1))
            if job_id not in self.logs:
                return 404, {"message": "Not Found"}
            base = "http://%s" % record["headers"].get("host", "127.0.0.1")
            return 302, None, {"Location": "%s/storage/logs/%d?sig=fake" % (base, job_id)}
        return 404, {"message": "Not Found"}

    def attempt(self, run):
        """One attempt of a run: it runs ci.sh of the commit and ends at once."""
        conclusion, log = self.run_ci(run["head_sha"])
        run["run_attempt"] += 1
        run["_reads"] = 0
        run.update({"status": "completed", "conclusion": conclusion, "run_started_at": now(), "updated_at": now()})
        job_id = run["id"] * 100 + run["run_attempt"]
        step = {"name": "Run ci.sh", "conclusion": conclusion, "number": 1}
        self.jobs[job_id] = {"id": job_id, "run_id": run["id"], "name": "test", "status": "completed",
                             "conclusion": conclusion, "steps": [step], "run_attempt": run["run_attempt"]}
        self.logs[job_id] = log
        run.setdefault("_jobs", []).append(job_id)

    # Control: the team.

    def control(self, action, body):
        with self.lock:
            number = int(body.get("number") or 0)
            pull = self.pulls.get(number)
            if action == "state":
                return 200, self.state()
            if pull is None:
                return 404, {"message": "no pull request #%s" % number}
            if action == "ready":
                pull["draft"] = False
                return 200, self.pull_view(pull)
            if action == "review":
                login = body["login"]
                review = {"id": self.new_id(body.get("id")), "user": {"login": login}, "state": body["state"],
                          "body": body.get("body") or "", "submitted_at": now(),
                          "commit_id": self.pull_view(pull)["head"]["sha"]}
                self.reviews.setdefault(number, []).append(review)
                if login in pull["requested"]:
                    pull["requested"].remove(login)
                return 201, review
            if action == "comment":
                comment = {"id": self.new_id(body.get("id")), "user": {"login": body["login"]}, "body": body["body"],
                           "path": body.get("path"), "line": body.get("line"), "original_line": body.get("line"),
                           "created_at": now()}
                self.comments.setdefault(number, []).append(comment)
                return 201, comment
            if action == "issue_comment":
                comment = {"id": self.new_id(body.get("id")), "user": {"login": body["login"]}, "body": body["body"],
                           "created_at": now()}
                self.issue_comments.setdefault(number, []).append(comment)
                return 201, comment
            if action == "conflict":
                # The base moved on, and the pull request conflicts with it, or no longer does.
                conflict = bool(body.get("conflict", True))
                pull.update({"mergeable": not conflict, "mergeable_state": "dirty" if conflict else "clean"})
                return 200, self.pull_view(pull)
            if action == "merge":
                return self.merge(pull)
            return 404, {"message": "no control action %s" % action}

    def merge(self, pull):
        if pull["merged"]:
            return 200, self.pull_view(pull)
        branch = pull["head"]["ref"]
        head = self.branch_head(branch)
        with tempfile.TemporaryDirectory(prefix="fake-merge-") as work:
            git(["clone", "--quiet", self.remote, work], self.remote)
            git(["fetch", "--quiet", "origin", "refs/heads/%s:refs/remotes/origin/%s" % (branch, branch)], work)
            git(["merge", "--quiet", "--no-ff", "-m", "Merge pull request #%d from %s" % (pull["number"], branch),
                 "origin/" + branch], work)
            git(["push", "--quiet", "origin", "HEAD:refs/heads/main"], work)
        pull.update({"merged": True, "merged_at": now(), "state": "closed", "mergeable": None,
                     "mergeable_state": "unknown"})
        pull["head"]["sha"] = head
        return 200, self.pull_view(pull)

    def state(self):
        return {
            "issues": copy.deepcopy(self.issues),
            "pulls": {str(n): self.pull_view(p) for n, p in self.pulls.items()},
            "reviews": {str(n): copy.deepcopy(r) for n, r in self.reviews.items()},
            "comments": {str(n): copy.deepcopy(c) for n, c in self.comments.items()},
            "issue_comments": {str(n): copy.deepcopy(c) for n, c in self.issue_comments.items()},
            "runs": [self.run_view(r, count=False) for r in self.runs],
        }


def install(server, world, log_path=None):
    """Serves the world on a FakeServer: each call that no route answers goes to the world. Each
    call of the adapter is recorded in `log_path` as one JSON line, with no header value."""
    write = threading.Lock()

    def fallback(record):
        record["auth"] = world.auth_of(record)
        if record["path"].startswith("/_control/"):
            return world.control(record["path"][len("/_control/"):], record["body"] or {})
        result = world.handle(record)
        if log_path:
            entry = {"method": record["method"], "path": record["path"], "raw_path": record["raw_path"],
                     "query": record["query"], "auth": record["auth"], "body": record["body"],
                     "status": result[0] if result else 404}
            with write, open(log_path, "a", encoding="utf-8") as handle:
                handle.write(json.dumps(entry, sort_keys=True) + "\n")
        return result

    server.fallback = fallback
