"""The world of the demo adapter (design section 11.4): a tracker, a forge, and a CI in one folder.

The `[adapter]` table of the configuration names the folder with `root`:

    root/
      tracker.json   {"tickets": [{"key", "title", "description", "status", "assignee"}]}
      remote.git/    a bare repo: the remote of the clones
      prs.json       {"next": 1, "prs": [{"number", "key", "branch", "head", "title", "body",
                      "draft", "merged", "closed", "approvals", "changes_requested",
                      "comments", "op"}]}
      ci.json        {"<commit>": {"state": "green" | "red", "log": "...", "at": "..."}}
      act.log        one JSON line for each call of bin/act that changed something

The scripts read one JSON object from standard input and print one JSON object. Exit code 0
means done, 2 means that a gate of the script refused the call, and 1 means a failure. They use
the standard library only, and they need no network.
"""

import contextlib
import datetime
import fcntl
import io
import json
import os
import re
import subprocess
import sys
import tarfile
import tempfile
import time

IDENTITY = ["-c", "user.name=demo", "-c", "user.email=demo@example.invalid"]
QUIET_GIT = ["-c", "commit.gpgsign=false", "-c", "core.hooksPath=/dev/null", "-c", "init.defaultBranch=main"]
KEY = re.compile(r"^[A-Za-z0-9][A-Za-z0-9._-]{0,63}$")
TEST_FOLDERS = {"test", "tests", "spec", "specs", "__tests__"}
LOG_KEEP = 20000

TOY_FILES = {
    "calc.py": '"""A tiny calculator, the toy project of the demo world."""\n\n\n'
    "def add(a, b):\n"
    '    """Gives a plus b."""\n'
    "    return a + b\n",
    "test_calc.py": "import unittest\n\n"
    "from calc import add\n\n\n"
    "class AddTest(unittest.TestCase):\n"
    "    def test_add(self):\n"
    "        self.assertEqual(add(2, 3), 5)\n\n\n"
    'if __name__ == "__main__":\n'
    "    unittest.main()\n",
    "ci.sh": "#!/bin/sh\n# The CI of the toy project: each test must pass.\nset -e\npython3 -B -m unittest discover -q\n",
    ".gitignore": "__pycache__/\n*.pyc\n",
}

TOY_TICKETS = [
    {
        "key": "DEMO-12",
        "title": "Add subtract to the calculator",
        "description": "The calculator can only add. Add a function subtract(a, b) that gives a minus b, with a test.",
        "status": "To Do",
        "assignee": None,
    },
    {
        "key": "DEMO-13",
        "title": "Add multiply to the calculator",
        "description": "Add a function multiply(a, b) that gives a times b, with a test.",
        "status": "To Do",
        "assignee": None,
    },
]

TEST_COMMAND = "python3 -B -m unittest discover -q"


class Failure(Exception):
    """The call failed: exit code 1."""


class Refusal(Exception):
    """A gate of the script refused the call: exit code 2."""


def now():
    return datetime.datetime.now(datetime.timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")


def short(commit):
    return (commit or "-")[:12]


def main(handler):
    """Reads the call, runs the handler, and prints its answer."""
    try:
        data = sys.stdin.buffer.read().decode("utf-8")
        request = json.loads(data) if data.strip() else {}
        if not isinstance(request, dict):
            raise Failure("the call is not a JSON object")
        result = handler(request)
    except Refusal as e:
        print(str(e), file=sys.stderr)
        sys.stdout.buffer.write(json.dumps({"detail": str(e)}).encode("utf-8"))
        sys.exit(2)
    except Failure as e:
        print(str(e), file=sys.stderr)
        sys.exit(1)
    sys.stdout.buffer.write(json.dumps(result).encode("utf-8"))
    sys.exit(0)


# The executor gives a script PATH, HOME, LANG, TMPDIR, and each secret. The commands of the
# project (its tests and its CI) run code that an agent wrote, so they get no secret.
PLAIN_ENV = ("PATH", "HOME", "LANG", "TMPDIR")


def plain_env():
    return {k: os.environ[k] for k in PLAIN_ENV if k in os.environ}


def git_env():
    env = plain_env()
    env["GIT_TERMINAL_PROMPT"] = "0"
    return env


def git(args, cwd=None, check=True, data=None):
    """Runs git and gives the finished process. With `check`, a failure raises Failure."""
    proc = subprocess.run(
        ["git"] + QUIET_GIT + IDENTITY + list(args),
        cwd=cwd,
        input=data,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        env=git_env(),
    )
    if check and proc.returncode != 0:
        err = proc.stderr.decode("utf-8", "replace").strip()
        raise Failure("git %s failed: %s" % (" ".join(args), err))
    return proc


def git_text(args, cwd=None):
    return git(args, cwd=cwd).stdout.decode("utf-8", "replace").strip()


def export_tree(repo, commit, dest):
    """Writes the files of `commit` of `repo` to the folder `dest`."""
    data = git(["archive", "--format=tar", commit], cwd=repo).stdout
    os.makedirs(dest, exist_ok=True)
    with tarfile.open(fileobj=io.BytesIO(data)) as archive:
        if hasattr(tarfile, "data_filter"):
            archive.extractall(dest, filter="data")
        else:
            archive.extractall(dest)


def run_command(command, cwd, timeout):
    """Runs a shell command. Gives (passed, the end of its output, seconds)."""
    started = time.monotonic()
    env = plain_env()
    env["PYTHONDONTWRITEBYTECODE"] = "1"
    try:
        proc = subprocess.run(
            command,
            shell=True,
            cwd=cwd,
            stdout=subprocess.PIPE,
            stderr=subprocess.STDOUT,
            timeout=timeout,
            env=env,
            start_new_session=True,
        )
        output = proc.stdout.decode("utf-8", "replace")
        passed = proc.returncode == 0
    except subprocess.TimeoutExpired as e:
        output = (e.output or b"").decode("utf-8", "replace") + "\n(killed after %d s)" % timeout
        passed = False
    seconds = int(time.monotonic() - started)
    return passed, output[-LOG_KEEP:], seconds


class World:
    """One demo world in the folder `root`."""

    def __init__(self, root):
        self.root = root
        self.remote = os.path.join(root, "remote.git")

    @classmethod
    def of(cls, config):
        root = (config or {}).get("root")
        if not isinstance(root, str) or not root.strip():
            raise Failure(
                "set root in the [adapter] table of config.toml: the folder that `bin/world new DIR` made"
            )
        root = os.path.abspath(os.path.expanduser(root))
        if not os.path.isfile(os.path.join(root, "tracker.json")):
            raise Failure("%s is not a demo world. Make one with `bin/world new %s`" % (root, root))
        return cls(root)

    @contextlib.contextmanager
    def lock(self):
        with open(os.path.join(self.root, ".lock"), "a") as handle:
            fcntl.flock(handle, fcntl.LOCK_EX)
            try:
                yield
            finally:
                fcntl.flock(handle, fcntl.LOCK_UN)

    def load(self, name, default):
        path = os.path.join(self.root, name)
        if not os.path.exists(path):
            return default
        with open(path, encoding="utf-8") as handle:
            return json.load(handle)

    def save(self, name, data):
        path = os.path.join(self.root, name)
        fd, temp = tempfile.mkstemp(dir=self.root, prefix="." + name, suffix=".tmp")
        with os.fdopen(fd, "w", encoding="utf-8") as handle:
            json.dump(data, handle, indent=2, sort_keys=True)
            handle.write("\n")
        os.replace(temp, path)

    def tracker(self):
        return self.load("tracker.json", {"tickets": []})

    def prs(self):
        return self.load("prs.json", {"next": 1, "prs": []})

    def ci(self):
        return self.load("ci.json", {})

    def ticket(self, tracker, key):
        for ticket in tracker["tickets"]:
            if ticket.get("key") == key:
                return ticket
        return None

    def branch_head(self, branch):
        if not branch:
            return None
        proc = git(["rev-parse", "--verify", "--quiet", "refs/heads/" + branch], cwd=self.remote, check=False)
        text = proc.stdout.decode("utf-8", "replace").strip()
        return text if proc.returncode == 0 and text else None

    def find_pr(self, prs, number=None, branch=None):
        """The pull request with this number, else the open one of the branch, else the newest
        merged one of the branch."""
        if number is not None:
            for pr in prs["prs"]:
                if pr.get("number") == number:
                    return pr
            return None
        mine = [pr for pr in prs["prs"] if branch and pr.get("branch") == branch]
        for pr in reversed(mine):
            if not pr.get("merged") and not pr.get("closed"):
                return pr
        for pr in reversed(mine):
            if pr.get("merged"):
                return pr
        return None

    def conflict(self, pr):
        proc = git(["merge-tree", "--write-tree", "refs/heads/main", "refs/heads/" + pr["branch"]], cwd=self.remote, check=False)
        return proc.returncode == 1

    def pr_head(self, pr):
        if pr.get("merged"):
            return pr.get("head")
        return self.branch_head(pr.get("branch")) or pr.get("head")

    def log_act(self, entry):
        with open(os.path.join(self.root, "act.log"), "a", encoding="utf-8") as handle:
            handle.write(json.dumps(entry, sort_keys=True) + "\n")


def issue_of(ticket):
    return {k: ticket.get(k) for k in ("key", "title", "description", "status", "assignee")}


# observe -------------------------------------------------------------------------------------


def observe(request):
    world = World.of(request.get("config"))
    what = request.get("what")
    if what == "queue":
        return queue(world, request.get("config") or {})
    if what == "ticket":
        return ticket_facts(world, request)
    raise Failure("observe: `what` must be queue or ticket, not %r" % (what,))


def queue(world, config):
    assignee = config.get("assignee")
    tickets = []
    for ticket in world.tracker()["tickets"]:
        status = ticket.get("status") or ""
        eligible, reason = True, ""
        if status != "To Do":
            eligible, reason = False, "the status is %s, not To Do" % status
        elif assignee and ticket.get("assignee") not in (None, assignee):
            eligible, reason = False, "it is assigned to %s" % ticket.get("assignee")
        tickets.append(
            {
                "key": ticket.get("key"),
                "title": ticket.get("title") or "",
                "status": status,
                "eligible": eligible,
                "reason": reason,
            }
        )
    return {"tickets": tickets}


def ticket_facts(world, request):
    key = request.get("key")
    ticket = world.ticket(world.tracker(), key)
    if ticket is None:
        raise Failure("the tracker has no ticket %r" % (key,))
    pr = world.find_pr(world.prs(), number=request.get("pr"), branch=request.get("branch"))
    pr_view = None
    if pr is not None:
        waiting = [c for c in pr.get("comments", []) if not c.get("replies") and not c.get("resolved")]
        open_pr = not pr.get("merged") and not pr.get("closed")
        pr_view = {
            "number": pr["number"],
            "draft": bool(pr.get("draft")),
            "head": world.pr_head(pr),
            "merged": bool(pr.get("merged")),
            "closed": bool(pr.get("closed")),
            "conflict": world.conflict(pr) if open_pr else False,
            "approved_by": list(pr.get("approvals", [])),
            "changes_requested": bool(pr.get("changes_requested")),
            "comments_waiting": len(waiting),
        }
    ci_head = pr_view["head"] if pr_view else world.branch_head(request.get("branch"))
    state = world.ci().get(ci_head, {}).get("state", "none") if ci_head else "none"
    return {
        "issue": issue_of(ticket),
        "tracker_status": ticket.get("status"),
        "pr": pr_view,
        "ci": {"head": ci_head, "state": state},
        "envs": {},
    }


# act -----------------------------------------------------------------------------------------


def act(request):
    world = World.of(request.get("config"))
    verb = request.get("verb")
    if request.get("read"):
        reads = {"tracker.issue": tracker_issue}
        if verb not in reads:
            raise Failure("the demo adapter has no read verb %r" % (verb,))
        return reads[verb](world, request)
    acts = {
        "tracker.describe": tracker_describe,
        "git.push": git_push,
        "pr.open": pr_open,
        "ci.run": ci_run,
        "review.reply": review_reply,
    }
    if verb not in acts:
        raise Failure("the demo adapter has no verb %r" % (verb,))
    reconcile = bool(request.get("reconcile"))
    with world.lock():
        result = acts[verb](world, request, reconcile)
        if not reconcile:
            world.log_act(
                {
                    "at": now(),
                    "op": request.get("op"),
                    "verb": verb,
                    "key": request.get("key"),
                    "head": request.get("head"),
                    "state": result.get("state"),
                }
            )
    return result


def confirmed(detail, external_id=None, values=None):
    answer = {"state": "confirmed", "detail": detail}
    if external_id is not None:
        answer["external_id"] = external_id
    if values:
        answer["set"] = values
    return answer


def failed(detail):
    return {"state": "failed", "detail": detail}


def tracker_describe(world, request, reconcile):
    args = request.get("args") or {}
    text = args.get("text")
    status = args.get("status")
    if not isinstance(text, str) or not text.strip():
        raise Failure("tracker.describe needs args.text, the new description of the ticket")
    key = request.get("key")
    tracker = world.tracker()
    ticket = world.ticket(tracker, key)
    if ticket is None:
        raise Failure("the tracker has no ticket %r" % (key,))
    done = ticket.get("description") == text and (status is None or ticket.get("status") == status)
    if reconcile:
        if done:
            return confirmed("the description of %s is the new one" % key, key)
        return failed("the description of %s is not the new one" % key)
    ticket["description"] = text
    if isinstance(status, str) and status:
        ticket["status"] = status
    world.save("tracker.json", tracker)
    return confirmed("the description of %s is replaced" % key, key)


def git_push(world, request, reconcile):
    branch = request.get("branch") or ""
    head = request.get("head") or ""
    if not branch or branch in ("main", "master") or branch.startswith("-"):
        raise Refusal("git.push refuses the branch %r: it pushes only the branch of a ticket" % branch)
    if not head:
        raise Failure("git.push needs the last commit in head")
    remote_head = world.branch_head(branch)
    pushed = confirmed("%s is at %s on the remote" % (branch, short(head)), "%s@%s" % (branch, short(head)), {"head": head})
    if reconcile:
        if remote_head == head:
            return pushed
        return failed("%s is at %s on the remote, not at %s: the push did not happen" % (branch, short(remote_head), short(head)))
    clone = request.get("clone")
    if not clone or not os.path.isdir(clone):
        raise Failure("git.push needs the clone of the ticket")
    args = ["push", "--quiet"]
    if (request.get("args") or {}).get("force"):
        args.append("--force")
    proc = git(args + [world.remote, "%s:refs/heads/%s" % (head, branch)], cwd=clone, check=False)
    if proc.returncode != 0:
        err = proc.stderr.decode("utf-8", "replace").strip().splitlines()[-3:]
        return failed("git push failed: %s" % " ".join(err))
    if world.branch_head(branch) != head:
        return {"state": "uncertain", "detail": "git push ended, but %s is not at %s on the remote" % (branch, short(head))}
    return pushed


def pr_open(world, request, reconcile):
    branch = request.get("branch") or ""
    key = request.get("key")
    args = request.get("args") or {}
    prs = world.prs()
    open_pr = None
    for pr in reversed(prs["prs"]):
        if pr.get("branch") == branch and not pr.get("merged") and not pr.get("closed"):
            open_pr = pr
            break
    if open_pr is not None:
        number = open_pr["number"]
        return confirmed("pull request #%d is open for %s" % (number, branch), str(number), {"pr": number})
    if reconcile:
        return failed("no pull request is open for %s" % branch)
    head = world.branch_head(branch)
    if head is None:
        return failed("the branch %s is not on the remote: push it first" % branch)
    ticket = world.ticket(world.tracker(), key) or {}
    # args.text is the whole pr.md: its first line is the title, and the rest is the body.
    text = args.get("text") if isinstance(args.get("text"), str) else ""
    first, _, rest = text.strip().partition("\n")
    number = prs.get("next", 1)
    prs["next"] = number + 1
    prs["prs"].append(
        {
            "number": number,
            "key": key,
            "branch": branch,
            "head": head,
            "title": args.get("title") or first.strip().lstrip("#").strip() or ticket.get("title") or branch,
            "body": args.get("body") or rest.strip(),
            "draft": args.get("draft", True) is not False,
            "merged": False,
            "closed": False,
            "approvals": [],
            "changes_requested": False,
            "comments": [],
            "op": request.get("op"),
            "at": now(),
        }
    )
    world.save("prs.json", prs)
    return confirmed("pull request #%d is open for %s" % (number, branch), str(number), {"pr": number})


def ci_run(world, request, reconcile):
    head = request.get("head") or ""
    clone = request.get("clone")
    if not head:
        raise Failure("ci.run needs the last commit in head")
    runs = world.ci()
    if reconcile:
        if head in runs:
            return confirmed("CI is %s at %s" % (runs[head]["state"], short(head)), "ci-" + short(head))
        return failed("CI did not run at %s" % short(head))
    if not clone or not os.path.isdir(clone):
        raise Failure("ci.run needs the clone of the ticket")
    timeout = int((request.get("config") or {}).get("ci_timeout") or 300)
    with tempfile.TemporaryDirectory() as temp:
        export_tree(clone, head, temp)
        if os.path.isfile(os.path.join(temp, "ci.sh")):
            passed, output, _ = run_command("sh ci.sh", temp, timeout)
            state = "green" if passed else "red"
        else:
            state, output = "green", "there is no ci.sh at this commit, so there is nothing to run"
    runs[head] = {"state": state, "log": output, "at": now()}
    world.save("ci.json", runs)
    return confirmed("CI is %s at %s" % (state, short(head)), "ci-" + short(head))


def review_reply(world, request, reconcile):
    args = request.get("args") or {}
    if "verdicts" in args:
        return review_replies(world, request, reconcile)
    comment_id = args.get("comment")
    body = args.get("body")
    if not isinstance(comment_id, int) or not isinstance(body, str) or not body.strip():
        raise Failure("review.reply needs args.comment (the id of the comment) and args.body (the reply)")
    prs = world.prs()
    pr = world.find_pr(prs, number=request.get("pr"), branch=request.get("branch"))
    if pr is None:
        return failed("no pull request for %s" % request.get("branch"))
    comment = next((c for c in pr.get("comments", []) if c.get("id") == comment_id), None)
    if comment is None:
        return failed("pull request #%d has no comment %d" % (pr["number"], comment_id))
    replies = comment.setdefault("replies", [])
    op = request.get("op")
    external_id = "comment-%d-reply" % comment_id
    if any(r.get("op") == op for r in replies):
        return confirmed("the reply to comment %d is posted" % comment_id, external_id)
    if reconcile:
        return failed("no reply of this operation to comment %d" % comment_id)
    replies.append({"op": op, "body": body, "at": now()})
    world.save("prs.json", prs)
    return confirmed("the reply to comment %d is posted" % comment_id, external_id)


def review_replies(world, request, reconcile):
    """args.verdicts is verdicts.json of the step: one reply for each row that has one."""
    args = request.get("args") or {}
    try:
        rows = json.loads(args.get("verdicts") or "")
    except (TypeError, ValueError):
        raise Failure("review.reply needs args.verdicts, the JSON list of verdicts.json")
    if isinstance(rows, dict):
        rows = rows.get("verdicts")
    if not isinstance(rows, list):
        raise Failure("args.verdicts of review.reply must be a JSON list of rows")
    prs = world.prs()
    pr = world.find_pr(prs, number=request.get("pr"), branch=request.get("branch"))
    if pr is None:
        return failed("no pull request for %s" % request.get("branch"))
    op = request.get("op")
    posted, missing = [], []
    for row in rows:
        try:
            comment_id = int(row.get("comment_id"))
        except (TypeError, ValueError, AttributeError):
            continue
        body = row.get("reply") if isinstance(row, dict) else None
        if not isinstance(body, str) or not body.strip():
            continue
        comment = next((c for c in pr.get("comments", []) if c.get("id") == comment_id), None)
        if comment is None:
            missing.append(comment_id)
            continue
        replies = comment.setdefault("replies", [])
        if not any(r.get("op") == op for r in replies):
            if reconcile:
                missing.append(comment_id)
                continue
            replies.append({"op": op, "body": body, "at": now()})
        posted.append(comment_id)
    if missing:
        if not reconcile:
            world.save("prs.json", prs)
        return failed("pull request #%d has no comment or no reply of this operation for %s" % (pr["number"], missing))
    if not reconcile:
        world.save("prs.json", prs)
    return confirmed("the replies to the comments %s are posted" % posted, "replies-%s" % (op or ""))


def tracker_issue(world, request):
    args = request.get("args") or {}
    key = args.get("key") or request.get("key")
    if not isinstance(key, str) or not KEY.match(key):
        raise Failure("tracker.issue needs a ticket key, such as DEMO-12")
    ticket = world.ticket(world.tracker(), key)
    if ticket is None:
        raise Failure("the tracker has no ticket %r" % key)
    content = json.dumps(issue_of(ticket), indent=2) + "\n"
    return {"detail": "the issue %s" % key, "documents": [{"name": "issue-%s.json" % key, "content": content}]}


# job -----------------------------------------------------------------------------------------


def job(request):
    world = World.of(request.get("config"))
    jobs = {"proof": proof, "second-review": second_review, "comments": comments, "ci-log": ci_log}
    name = request.get("job")
    if name not in jobs:
        raise Failure("the demo adapter has no job %r" % (name,))
    return jobs[name](world, request)


def need_commits(request, what):
    clone, base, head = request.get("clone"), request.get("base"), request.get("head")
    if not clone or not os.path.isdir(clone) or not base or not head:
        raise Failure("%s needs the clone, the base commit, and the last commit" % what)
    return clone, base, head


def is_test(path):
    parts = path.split("/")
    name = parts[-1]
    stem = name.rsplit(".", 1)[0]
    return (
        name.startswith("test_")
        or name.startswith("test-")
        or stem.endswith(("_test", ".test", "_spec", ".spec"))
        or any(part in TEST_FOLDERS for part in parts[:-1])
    )


def proof(world, request):
    """The changed tests pass at the last commit and fail on the base commit (design 11.3)."""
    config = request.get("config") or {}
    command = config.get("test_command")
    if not isinstance(command, str) or not command.strip():
        raise Failure('the proof needs test_command in the [adapter] table, such as "%s"' % TEST_COMMAND)
    clone, base, head = need_commits(request, "the proof")
    green = bool((request.get("args") or {}).get("green"))
    timeout = int(config.get("test_timeout") or 300)
    changed = git_text(["diff", "--name-only", "--no-renames", "--diff-filter=d", base, head], cwd=clone).splitlines()
    tests = [path for path in changed if is_test(path)]
    checks = []
    log = []
    with tempfile.TemporaryDirectory() as temp:
        at_head = os.path.join(temp, "head")
        export_tree(clone, head, at_head)
        passed, output, seconds = run_command(command, at_head, timeout)
        checks.append({"kind": "proof-head", "result": "pass" if passed else "fail", "command": command, "seconds": seconds, "cause": "" if passed else "code"})
        log += ["== the tests at the last commit %s: %s ==" % (short(head), "pass" if passed else "fail"), output]
        if green:
            ok = passed
            detail = "the plan declares the proof green; the tests %s at %s" % ("pass" if passed else "fail", short(head))
        elif not tests:
            ok = False
            detail = "no test changed between the base and the last commit, so nothing proves the change. Add a test that fails on the base, or declare the proof green in the plan"
        else:
            on_base = os.path.join(temp, "base")
            export_tree(clone, base, on_base)
            for path in tests:
                target = os.path.join(on_base, path)
                os.makedirs(os.path.dirname(target), exist_ok=True)
                with open(target, "wb") as handle:
                    handle.write(git(["show", "%s:%s" % (head, path)], cwd=clone).stdout)
            base_passed, base_output, base_seconds = run_command(command, on_base, timeout)
            checks.append({"kind": "proof-base", "result": "pass" if base_passed else "fail", "command": command, "seconds": base_seconds, "cause": "code" if base_passed else ""})
            log += ["== the changed tests on the base %s: %s ==" % (short(base), "pass" if base_passed else "fail"), base_output]
            ok = passed and not base_passed
            if ok:
                detail = "the changed tests fail on the base %s and pass at %s" % (short(base), short(head))
            elif not passed:
                detail = "the tests fail at the last commit %s" % short(head)
            else:
                detail = "the changed tests also pass on the base %s, so they do not prove the change" % short(base)
    return {"ok": ok, "detail": detail, "documents": [{"name": "proof.txt", "content": "\n".join(log) + "\n"}], "checks": checks}


MARKERS = ["TODO", "FIXME", "XXX", "breakpoint()", "pdb.set_trace"]


def second_review(world, request):
    """A second look at the diff: the changed files, and the markers that a change must not leave."""
    clone, base, head = need_commits(request, "the second review")
    stats = git_text(["diff", "--numstat", base, head], cwd=clone).splitlines()
    diff = git_text(["diff", "--unified=0", "--no-color", "--src-prefix=a/", "--dst-prefix=b/", base, head], cwd=clone)
    findings = []
    path, line_no = None, 0
    for line in diff.splitlines():
        if line.startswith("+++ "):
            path = line[6:] if line.startswith("+++ b/") else None
        elif line.startswith("@@"):
            match = re.search(r"\+(\d+)", line)
            line_no = int(match.group(1)) if match else 0
        elif line.startswith("+") and path:
            for marker in MARKERS:
                if marker in line:
                    findings.append("%s line %d: %s is left in the code" % (path, line_no, marker))
            line_no += 1
    lines = ["# Second review of %s" % short(head), "", "## Changed files", ""]
    for stat in stats:
        added, removed, name = (stat.split("\t") + ["", "", ""])[:3]
        lines.append("- %s (+%s -%s)" % (name, added, removed))
    lines += ["", "## Findings", ""]
    lines += ["- " + f for f in findings] or ["- none"]
    check = {"kind": "second-review", "result": "warn" if findings else "pass", "command": "second-review", "seconds": 0, "cause": ""}
    return {
        "ok": True,
        "detail": "%d finding(s) in %d changed file(s)" % (len(findings), len(stats)),
        "documents": [{"name": "second-review.md", "content": "\n".join(lines) + "\n"}],
        "checks": [check],
    }


def comments(world, request):
    """The review comments that wait for a reply, as comments.json."""
    pr = world.find_pr(world.prs(), number=request.get("pr"), branch=request.get("branch"))
    waiting = []
    if pr is not None:
        for c in pr.get("comments", []):
            if not c.get("replies") and not c.get("resolved"):
                waiting.append({k: c.get(k) for k in ("id", "author", "path", "line", "text")})
    return {
        "ok": True,
        "detail": "%d comment(s) wait for a reply" % len(waiting),
        "documents": [{"name": "comments.json", "content": json.dumps(waiting, indent=2) + "\n"}],
    }


def ci_log(world, request):
    """The log of the CI run at the last commit, as ci-log.txt."""
    head = request.get("head") or ""
    run = world.ci().get(head)
    if run is None:
        content = "CI did not run at %s.\n" % short(head)
    else:
        content = "CI is %s at %s (%s).\n\n%s\n" % (run.get("state"), short(head), run.get("at"), run.get("log", ""))
    return {"ok": True, "detail": "the CI log at %s" % short(head), "documents": [{"name": "ci-log.txt", "content": content}]}
