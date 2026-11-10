"""bin/job: one job, such as the proof (design section 11.1).

A job gets no secret, unless its table in adapter.toml sets `secrets = true`. Only `ci-log` and
`comments` do, because they only read GitHub and run no code of the project. Each command of the
project (the tests, the linter, the second reviewer, the app) runs with a plain environment, in
its own process group, with a time limit. A file name from the diff reaches it only as one item
of an argument list, never inside the text of a shell command.
"""

import contextlib
import json
import os
import re
import shutil
import signal
import subprocess
import tempfile
import time
import urllib.parse

from . import core, forge

OUTPUT_KEEP = 20000
RED_JOBS = {"failure", "timed_out", "cancelled", "startup_failure"}
ANSI = re.compile(r"\x1b\[[0-9;?]*[ -/]*[@-~]")


def job(request):
    name = request.get("job")
    if name not in JOBS:
        raise core.Failure("the adapter jira-github has no job %r. Its jobs: %s" % (
            core.clean(name, 40), ", ".join(sorted(JOBS))))
    return JOBS[name](core.Config(request.get("config")), request)


# Commands of the project ----------------------------------------------------------------------


class Ran:
    def __init__(self, code, output, seconds, timed_out, stderr=""):
        self.code = code
        self.output = output
        self.seconds = seconds
        self.timed_out = timed_out
        self.stderr = stderr

    @property
    def passed(self):
        return self.code == 0 and not self.timed_out

    def cause(self):
        """The cause of a failure (design 11.1): a missing command is the env, a time limit infra."""
        if self.timed_out:
            return "infra"
        if self.code in (126, 127):
            return "env"
        return "code"


def run(argv, cwd, timeout, extra_env=None, stdout_only=False):
    """Runs a command in its own process group with a plain environment. At the time limit the
    whole group is killed."""
    env = core.plain_env()
    env["PYTHONDONTWRITEBYTECODE"] = "1"
    env.update(extra_env or {})
    started = time.monotonic()
    proc = subprocess.Popen(
        argv, cwd=cwd, env=env, stdin=subprocess.DEVNULL, stdout=subprocess.PIPE,
        stderr=subprocess.PIPE if stdout_only else subprocess.STDOUT, start_new_session=True,
    )
    timed_out = False
    try:
        out, err = proc.communicate(timeout=timeout)
    except subprocess.TimeoutExpired:
        timed_out = True
        with contextlib.suppress(OSError):
            os.killpg(proc.pid, signal.SIGKILL)
        out, err = proc.communicate()
    seconds = int(time.monotonic() - started)
    text = out.decode("utf-8", "replace")
    if timed_out:
        text += "\n(killed after %d s)" % timeout
    return Ran(proc.returncode, text, seconds, timed_out, (err or b"").decode("utf-8", "replace"))


def shell(command, args):
    """`sh -c '<command> "$@"' sh args...`: the command of the configuration may use the shell,
    and each argument stays one argument."""
    return ["sh", "-c", command + ' "$@"', "sh"] + [("./" + a) if a.startswith("-") else a for a in args]


def need_commits(request, what):
    clone, base, head = request.get("clone"), request.get("base"), request.get("head")
    if not isinstance(clone, str) or not os.path.isdir(clone):
        raise core.Failure("%s needs the clone of the ticket" % what)
    for name, value in (("base", base), ("head", head)):
        if not isinstance(value, str) or not core.COMMIT.match(value):
            raise core.Failure("%s needs the %s commit" % (what, name))
    return clone, base, head


def glob_regex(pattern):
    """A glob as a regex: `**/` is any number of folders, `*` and `?` stay inside one folder."""
    out = []
    i = 0
    while i < len(pattern):
        if pattern.startswith("**/", i):
            out.append("(?:.*/)?")
            i += 3
        elif pattern.startswith("**", i):
            out.append(".*")
            i += 2
        elif pattern[i] == "*":
            out.append("[^/]*")
            i += 1
        elif pattern[i] == "?":
            out.append("[^/]")
            i += 1
        elif pattern[i] == "{" and "}" in pattern[i:]:
            end = pattern.index("}", i)
            out.append("(?:%s)" % "|".join(re.escape(p) for p in pattern[i + 1:end].split(",")))
            i = end + 1
        else:
            out.append(re.escape(pattern[i]))
            i += 1
    return re.compile("^" + "".join(out) + "$")


def globs_of(value):
    if isinstance(value, str):
        value = re.split(r"[\s,]+", value.strip())
    if not isinstance(value, list) or not all(isinstance(v, str) for v in value) or not any(value):
        raise core.Failure("the proof needs the key `test_glob` in the [adapter] table: a glob or a list of globs")
    return [glob_regex(v) for v in value if v]


def paths_of(value):
    """The files that the plan declares green: a list, or a text with one path on each line."""
    if isinstance(value, str):
        value = re.split(r"[\n,]+", value)
    if not isinstance(value, list):
        return set()
    out = set()
    for item in value:
        if isinstance(item, str) and item.strip():
            path = item.strip()
            out.add(path[2:] if path.startswith("./") else path)
    return out


@contextlib.contextmanager
def worktree(clone, commit):
    """A temporary worktree of the clone at the commit. It is always removed."""
    tmp = tempfile.mkdtemp(prefix="vision-proof-")
    path = os.path.join(tmp, "base")
    try:
        core.git(["worktree", "add", "--detach", "--quiet", path, commit], cwd=clone, timeout=600)
        yield path
    finally:
        core.git(["worktree", "remove", "--force", "--force", path], cwd=clone, check=False)
        shutil.rmtree(tmp, ignore_errors=True)
        core.git(["worktree", "prune"], cwd=clone, check=False)


# proof ----------------------------------------------------------------------------------------


def proof(config, request):
    """The changed tests pass at the head commit and fail on the base commit (design 11.3)."""
    what = "the proof"
    command = config.text("test_command", what)
    globs = globs_of(config.get("test_glob"))
    clone, base, head = need_commits(request, what)
    timeout = config.number("test_timeout", 600)
    args = request.get("args") or {}
    green_all = args.get("green") is True
    green = paths_of(args.get("green"))
    at = core.git_text(["rev-parse", "HEAD"], cwd=clone)
    if at != head:
        raise core.Failure("the clone is at %s, not at the head %s" % (core.short(at), core.short(head)))
    raw = core.git(["diff", "--name-only", "--no-renames", "--diff-filter=d", "-z", base, head], cwd=clone).stdout
    changed = [p for p in raw.decode("utf-8", "replace").split("\0") if p]
    tests = [p for p in changed if any(g.match(p) for g in globs)]
    checks, log, problems = [], [], []

    def check(kind, path, ran, holds, cause):
        checks.append({
            "kind": kind, "result": "pass" if holds else "fail", "command": "%s %s" % (command, path),
            "seconds": ran.seconds, "cause": "" if holds else cause,
        })

    if not tests and not green_all:
        return {
            "ok": False,
            "detail": "no test changed between the base %s and the head %s, so nothing proves the change. "
                      "Add a test that fails on the base, or declare the proof green in the plan" % (
                          core.short(base), core.short(head)),
            "documents": [{"name": "proof.txt", "content": "No changed file matches test_glob.\n"}],
            "checks": [],
        }
    for path in tests:
        ran = run(shell(command, [path]), clone, timeout)
        check("proof-head", path, ran, ran.passed, ran.cause())
        log.append("== %s at the head %s: %s (%d s) ==\n%s" % (
            path, core.short(head), "pass" if ran.passed else "fail", ran.seconds, ran.output[-OUTPUT_KEEP:]))
        if not ran.passed:
            problems.append("%s fails at the head" % path)
    on_base = [p for p in tests if not green_all and p not in green]
    if on_base:
        with worktree(clone, base) as tree:
            setup = config.text("setup_command")
            if setup:
                ran = run(["sh", "-c", setup], tree, timeout, {"VISION_CLONE": clone})
                log.append("== setup_command in the base worktree: %s ==\n%s" % (
                    "pass" if ran.passed else "fail", ran.output[-OUTPUT_KEEP:]))
                if not ran.passed:
                    checks.append({"kind": "proof-setup", "result": "fail", "command": setup,
                                   "seconds": ran.seconds, "cause": "env"})
                    problems.append("setup_command failed in the base worktree")
                    on_base = []
            for path in on_base:
                target = os.path.join(tree, path)
                inside = os.path.realpath(tree) + os.sep
                if not os.path.realpath(target).startswith(inside):
                    raise core.Failure("the test file %r is outside of the worktree" % core.clean(path, 100))
                os.makedirs(os.path.dirname(target), exist_ok=True)
                with open(target, "wb") as handle:
                    handle.write(core.git(["show", "%s:%s" % (head, path)], cwd=clone).stdout)
                ran = run(shell(command, [path]), tree, timeout)
                fails = not ran.passed
                check("proof-base", path, ran, fails, "code" if ran.passed else ran.cause())
                log.append("== %s on the base %s: %s (%d s) ==\n%s" % (
                    path, core.short(base), "fail, as it must" if fails else "pass", ran.seconds,
                    ran.output[-OUTPUT_KEEP:]))
                if not fails:
                    problems.append("%s also passes on the base, so it does not prove the change" % path)
    lint = config.text("lint_command")
    if lint and changed:
        ran = run(shell(lint, changed), clone, timeout)
        checks.append({"kind": "proof-lint", "result": "pass" if ran.passed else "fail", "command": lint,
                       "seconds": ran.seconds, "cause": "" if ran.passed else ran.cause()})
        log.append("== lint_command: %s ==\n%s" % ("pass" if ran.passed else "fail", ran.output[-OUTPUT_KEEP:]))
        if not ran.passed:
            problems.append("lint_command fails on the changed files")
    green_note = ""
    if green_all:
        green_note = "; the plan declares the whole proof green"
    elif green & set(tests):
        green_note = "; the plan declares %s green" % ", ".join(sorted(green & set(tests)))
    if problems:
        detail = "the proof does not hold: " + "; ".join(problems)
    else:
        detail = "the proof holds: %d changed test file(s) pass at %s, %d of them fail on the base %s%s" % (
            len(tests), core.short(head), len(on_base), core.short(base), green_note)
    return {
        "ok": not problems,
        "detail": detail,
        "documents": [{"name": "proof.txt", "content": "\n\n".join(log) + "\n"}],
        "checks": checks,
    }


# second-review --------------------------------------------------------------------------------


def second_review(config, request):
    command = config.text("second_review_command")
    if not command:
        return {
            "ok": True,
            "detail": "no second reviewer is set: second_review_command is empty",
            "documents": [{"name": "second-review.md", "content": ""}],
            "checks": [],
        }
    clone, base, head = need_commits(request, "the second review")
    timeout = config.number("second_review_timeout", 1100)
    span = "%s..%s" % (base, head)
    ran = run(shell(command, [span]), clone, timeout, {"VISION_BASE": base, "VISION_HEAD": head}, stdout_only=True)
    check = {"kind": "second-review", "result": "pass" if ran.passed else "fail", "command": command,
             "seconds": ran.seconds, "cause": "" if ran.passed else ran.cause()}
    if not ran.passed:
        return {
            "ok": False,
            "detail": "second_review_command failed with exit code %s: %s" % (
                ran.code, core.clean(ran.stderr[-1000:] or ran.output[-1000:], 600)),
            "documents": [{"name": "second-review.md", "content": ran.output}],
            "checks": [check],
        }
    return {
        "ok": True,
        "detail": "the second reviewer looked at %s..%s" % (core.short(base), core.short(head)),
        "documents": [{"name": "second-review.md", "content": ran.output}],
        "checks": [check],
    }


# comments -------------------------------------------------------------------------------------


def comments(config, request):
    """The review comment threads that wait, as comments.json. The text is untrusted data."""
    why = "the job comments"
    own = config.logins("own_logins")
    if not own:
        raise core.Failure("%s needs the key `own_logins` in the [adapter] table" % why)
    gh = forge.GitHub(config, why)
    pull = gh.find_pr(request.get("pr"), request.get("branch") or "")
    if not pull:
        return {"ok": True, "detail": "no pull request, so no comment waits",
                "documents": [{"name": "comments.json", "content": "[]\n"}]}
    number = pull["number"]
    rows = []
    for root, items in sorted(forge.waiting_threads(gh.review_comments(number), own).items()):
        first, last = items[0], items[-1]
        mine = [i for i, c in enumerate(items) if forge.login(c.get("user")).lower() in own]
        new = items[mine[-1] + 1:] if mine else items
        if len(new) == 1:
            text = new[0].get("body") or ""
        else:
            text = "\n\n".join("%s: %s" % (forge.login(c.get("user")), c.get("body") or "") for c in new)
        rows.append({
            "id": root,
            "author": forge.login(last.get("user")),
            "path": first.get("path"),
            "line": first.get("line") or first.get("original_line"),
            "text": {"untrusted": text},
            "thread": [{"id": c.get("id"), "author": forge.login(c.get("user")), "text": {"untrusted": c.get("body") or ""}}
                       for c in items],
        })
    return {
        "ok": True,
        "detail": "%d thread(s) of #%d wait for a reply" % (len(rows), number),
        "documents": [{"name": "comments.json", "content": json.dumps(rows, indent=2) + "\n"}],
    }


# ci-log ---------------------------------------------------------------------------------------


def ci_log(config, request):
    """The end of the log of each failed job of the last run at the head, as ci-log.txt."""
    why = "the job ci-log"
    head = request.get("head")
    if not isinstance(head, str) or not core.COMMIT.match(head):
        raise core.Failure("%s needs the head commit" % why)
    workflow = config.text("ci_workflow", why)
    keep = int(config.number("log_tail_kb", 64) * 1024)
    gh = forge.GitHub(config, why)
    content, detail = ci_log_text(gh, config, head, workflow, keep)
    return {"ok": True, "detail": detail, "documents": [{"name": "ci-log.txt", "content": content}]}


def ci_log_text(gh, config, head, workflow, keep, run_id=None):
    if run_id:
        run = gh.client.json("GET", gh.path("actions/runs/%d" % int(run_id)), "read the run %d" % int(run_id))
    else:
        run = gh.last_run(head, workflow)
    if not run:
        return "No run of %s at %s.\n" % (workflow, core.short(head)), "no run of %s at %s" % (workflow, core.short(head))
    jobs = [j for j in gh.client.pages(gh.path("actions/runs/%d/jobs" % run["id"]), "list the jobs of the run %d" % run["id"],
                                       query={"filter": "latest"}, max_pages=5) if isinstance(j, dict)]
    failed = [j for j in jobs if j.get("conclusion") in RED_JOBS]
    lines = [
        "The run %s of %s at %s: %s (attempt %s)" % (run.get("id"), workflow, core.short(run.get("head_sha") or head),
                                                     forge.run_state(run), run.get("run_attempt") or 1),
        "Failed jobs: %s" % (", ".join(str(j.get("name")) for j in failed) or "none"),
        "",
    ]
    for j in failed:
        steps = [s.get("name") for s in j.get("steps") or [] if isinstance(s, dict) and s.get("conclusion") in RED_JOBS]
        lines.append("== the job %s: %s; failed steps: %s ==" % (j.get("name"), j.get("conclusion"), ", ".join(map(str, steps)) or "-"))
        lines.append(job_log(gh, config, j.get("id"), keep))
        lines.append("")
    return "\n".join(lines), "the logs of %d failed job(s) of the run %s" % (len(failed), run.get("id"))


def job_log(gh, config, job_id, keep):
    """The last `keep` bytes of a job log. GitHub answers with a redirect to a storage address;
    the token never goes there."""
    try:
        response = gh.client.call("GET", gh.path("actions/jobs/%d/logs" % int(job_id)), "read the log of the job %s" % job_id,
                                  ok=(200, 301, 302, 303, 307), limit=keep)
    except core.HttpError as e:
        return "(the log could not be read: %s)" % e
    if response.status != 200:
        location = response.header("Location") or ""
        try:
            core.check_base(urllib.parse.urlunsplit(urllib.parse.urlsplit(location)[:2] + ("", "", "")), "the log address")
        except core.Failure:
            return "(the log is at an address that is not https, so it was not read)"
        try:
            response = core.send("GET", location, {"User-Agent": "vision-jira-github-adapter"},
                                 timeout=config.http_timeout(), limit=keep)
        except core.Unreachable as e:
            return "(the log could not be read: %s)" % e
        if response.status != 200:
            return "(the log could not be read: the storage answered HTTP %d)" % response.status
    text = ANSI.sub("", response.body.decode("utf-8", "replace"))
    if len(response.body) >= keep and "\n" in text:
        text = "(the start is cut)\n" + text.split("\n", 1)[1]
    return text


# app-start ------------------------------------------------------------------------------------


def answers(url, timeout=2):
    """True when the address answers with a status below 400."""
    try:
        return core.send("GET", url, {"User-Agent": "vision-jira-github-adapter"}, timeout=timeout, limit=1024).status < 400
    except core.Unreachable:
        return False


def app_start(config, request):
    """Starts the app of the clone for QA, and waits until its health address answers. The app
    runs in its own session with a plain environment, so it outlives the job and gets no secret.
    An app that already answers is left as it is."""
    what = "app-start"
    command = config.text("app_start_command", what)
    health = config.text("app_health_url", what)
    parts = urllib.parse.urlsplit(health)
    if parts.scheme not in ("http", "https") or not parts.netloc:
        raise core.Failure("`app_health_url` must be an http or https address, such as http://localhost:3000/health")
    app_url = config.text("app_url") or urllib.parse.urlunsplit((parts.scheme, parts.netloc, "/", "", ""))
    timeout = config.number("app_start_timeout", 120)
    env = str((request.get("args") or {}).get("env") or "local")
    if not re.match(r"^[A-Za-z0-9_-]{1,40}$", env):
        raise core.Failure("args.env of app-start must be a plain name, such as local")
    clone = request.get("clone")
    if not isinstance(clone, str) or not os.path.isdir(clone):
        raise core.Failure("app-start needs the clone of the ticket")
    info = {"env": env, "url": app_url, "health": health, "started": False, "pid": None, "log": None}
    name = "app-%s.json" % env

    def answer(ok, detail):
        return {"ok": ok, "detail": detail, "documents": [{"name": name, "content": json.dumps(info, indent=2) + "\n"}]}

    if answers(health):
        return answer(True, "the app already answers at %s, so nothing was started" % health)
    key = core.check_key(request.get("key"))
    log_path = os.path.join(tempfile.gettempdir(), "vision-app-%s-%s.log" % (key, core.stamp()))
    started = time.monotonic()
    with open(log_path, "ab") as log:
        proc = subprocess.Popen(["sh", "-c", command], cwd=clone, env=core.plain_env(), stdin=subprocess.DEVNULL,
                                stdout=log, stderr=subprocess.STDOUT, start_new_session=True)
    info.update({"started": True, "pid": proc.pid, "log": log_path})
    while True:
        if answers(health):
            return answer(True, "started the app (process id %d) in %d s; it answers at %s. Its log is %s" % (
                proc.pid, int(time.monotonic() - started), health, log_path))
        code = proc.poll()
        if code is not None:
            return answer(False, "the app ended with exit code %d before it answered at %s. The end of its log: %s" % (
                code, health, core.clean(tail_of(log_path), 1500)))
        if time.monotonic() - started > timeout:
            with contextlib.suppress(OSError):
                os.killpg(proc.pid, signal.SIGTERM)
            return answer(False, "the app did not answer at %s in %d s, so it was stopped. The end of its log: %s" % (
                health, timeout, core.clean(tail_of(log_path), 1500)))
        time.sleep(0.5)


def tail_of(path, size=4000):
    try:
        with open(path, "rb") as handle:
            handle.seek(0, os.SEEK_END)
            handle.seek(max(0, handle.tell() - size))
            return handle.read().decode("utf-8", "replace")
    except OSError:
        return ""


JOBS = {
    "proof": proof,
    "app-start": app_start,
    "second-review": second_review,
    "comments": comments,
    "ci-log": ci_log,
}
