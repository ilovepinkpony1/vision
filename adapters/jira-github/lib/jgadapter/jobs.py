"""bin/job: one job, such as the proof (design section 11.1).

A job gets no secret, unless its table in adapter.toml sets `secrets = true`. Only `ci-log` and
`comments` do, because they only read GitHub and run no code of the project. Each command of the
project (the tests, the linter, the second reviewer, the app) runs with a plain environment, in
its own process group, with a time limit. A file name from the diff reaches it only as one item
of an argument list, never inside the text of a shell command.
"""

import contextlib
import hashlib
import json
import os
import re
import shutil
import signal
import stat
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
    return JOBS[name](core.Config.of(request), request)


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


def canaries_of(value):
    """`test_canary`: one test file, or a list of test files, that exist on the base commit and
    pass there. An empty list when the key is missing."""
    if value is None:
        return []
    if isinstance(value, str):
        value = [value]
    if not isinstance(value, list) or not all(isinstance(v, str) for v in value):
        raise core.Failure("`test_canary` in the [adapter] table must be a test file or a list of test files, "
                           "such as \"tests/test_smoke.py\"")
    out = []
    for item in value:
        path = item.strip()
        path = path[2:] if path.startswith("./") else path
        if path and path not in out:
            out.append(path)
    return out


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


# A line of the plan that declares the proof green: `Proof: green` for the whole proof, or
# `Proof: green <path>` for one test file. Any case; a list mark, a quote mark, and emphasis
# marks can come first, such as `- **Proof:** green tests/test_export.py`.
PROOF_LINE = re.compile(r"^\s*(?:>\s*)*(?:(?:[-*+]|\d+[.)])\s+)?[*_]*\s*proof\s*[*_]*\s*:\s*[*_]*\s*green(?![A-Za-z0-9]|_[A-Za-z0-9])(.*)$",
                        re.IGNORECASE)


def plan_green(text):
    """What the plan text declares green: (True, set()) for the whole proof, or (False, the
    test files). A path has a `/` or a file ending, or it stands in backticks. Each other text
    after `green`, such as `- no new behavior`, `(a refactor)`, or `because ...`, is a comment:
    the whole proof is green."""
    whole, paths = False, set()
    if not isinstance(text, str):
        return whole, paths
    for line in text.splitlines():
        match = PROOF_LINE.match(line)
        if not match:
            continue
        rest = match.group(1).strip().strip("*_").strip()
        if rest.startswith("`"):
            path = rest[1:].split("`", 1)[0].strip()
        elif rest and (rest[0].isalnum() or rest[0] in "./_"):
            # A path has a folder part or a file ending. A plain word, such as `because`,
            # starts a comment.
            word = rest.split()[0].rstrip(".,;:*_")
            path = word if ("/" in word or "." in word) else ""
        else:
            path = ""
        if path:
            paths.add(path[2:] if path.startswith("./") else path)
        else:
            whole = True
    return whole, paths


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
    canaries = canaries_of(config.get("test_canary"))
    args = request.get("args") or {}
    # The plan (args.plan, the text of approved-plan.md) declares the proof green, and the
    # older form args.green does too: true, or the test files.
    plan_all, plan_paths = plan_green(args.get("plan"))
    green_all = args.get("green") is True or plan_all
    green = paths_of(args.get("green")) | plan_paths
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
    canary_note = ""
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
            if on_base:
                # A canary proves that the base worktree can run tests at all, so that a broken
                # setup never passes the base half of the proof for the wrong reason.
                guards = []
                for canary in canaries:
                    if canary in tests:
                        log.append("== the canary %s is a changed test, so it is no canary: skipped ==" % canary)
                    else:
                        guards.append(canary)
                if not guards and canaries:
                    canary_note = "; no canary guards the run on the base, because each one is a changed test"
                elif not guards:
                    canary_note = "; no canary guards the run on the base (set test_canary)"
                for canary in guards:
                    failure = run_canary(command, canary, tree, base, timeout, checks, log)
                    if failure:
                        problems.append(failure)
                        on_base = []
                        break
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
        detail = "the proof does not hold: " + "; ".join(problems) + canary_note
    else:
        detail = "the proof holds: %d changed test file(s) pass at %s, %d of them fail on the base %s%s%s" % (
            len(tests), core.short(head), len(on_base), core.short(base), green_note, canary_note)
    return {
        "ok": not problems,
        "detail": detail,
        "documents": [{"name": "proof.txt", "content": "\n\n".join(log) + "\n"}],
        "checks": checks,
    }


def run_canary(command, canary, tree, base, timeout, checks, log):
    """Runs one canary of `test_canary` in the worktree of the base commit, with a check row of
    the kind proof-canary. Gives the problem when the worktree cannot run it, else None."""
    label = "%s %s" % (command, canary)
    target = os.path.join(tree, canary)
    inside = os.path.realpath(tree) + os.sep
    if not os.path.realpath(target).startswith(inside) or not os.path.isfile(target):
        checks.append({"kind": "proof-canary", "result": "fail", "command": label, "seconds": 0, "cause": "env"})
        log.append("== the canary %s is missing in the base worktree %s ==" % (canary, core.short(base)))
        return ("the worktree of the base commit %s cannot run tests: the canary %s is missing there. "
                "Fix setup_command (or the path in test_canary), then run vision retry" % (core.short(base), canary))
    ran = run(shell(command, [canary]), tree, timeout)
    checks.append({"kind": "proof-canary", "result": "pass" if ran.passed else "fail", "command": label,
                   "seconds": ran.seconds, "cause": "" if ran.passed else "env"})
    log.append("== the canary %s on the base %s: %s (%d s) ==\n%s" % (
        canary, core.short(base), "pass" if ran.passed else "fail", ran.seconds, ran.output[-OUTPUT_KEEP:]))
    if ran.passed:
        return None
    return ("the worktree of the base commit %s cannot run tests: the canary %s fails there, so no changed test "
            "ran on the base. Fix setup_command so that the tests run in that worktree, then run vision retry"
            % (core.short(base), canary))


# second-review --------------------------------------------------------------------------------


def second_review(config, request):
    command = config.text("second_review_command")
    if not command:
        # No document: the facts file of the review turn then gives null for second-review.md,
        # which the reviewer reads as "no second review ran".
        return {
            "ok": True,
            "detail": "no second reviewer is set: second_review_command is empty, so no second review ran",
            "documents": [],
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
    """Each comment that waits for an answer, as comments.json: the inline threads, the reviews
    that ask for changes or comment with a body, and the conversation comments, one row each
    with its kind. The text is untrusted data."""
    why = "the job comments"
    own = config.logins("own_logins")
    if not own:
        raise core.Failure("%s needs the key `own_logins` in the [adapter] table" % why)
    ignore = forge.Ignore(config.logins("ignore_logins"))
    gh = forge.GitHub(config, why)
    pull = gh.find_pr(request.get("pr"), request.get("branch") or "")
    if not pull:
        return {"ok": True, "detail": "no pull request, so no comment waits",
                "documents": [{"name": "comments.json", "content": "[]\n"}]}
    number = pull["number"]
    rows = forge.waiting_rows(gh.reviews(number), gh.review_comments(number), gh.issue_comments(number), own, ignore)
    kinds = ", ".join("%d %s" % (sum(1 for r in rows if r["kind"] == k), k) for k in forge.KINDS)
    return {
        "ok": True,
        "detail": "%d comment(s) of #%d wait for a reply (%s)" % (len(rows), number, kinds),
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


def answers(url, timeout=2.0):
    """True when the address answers with a status below 400."""
    try:
        return core.send("GET", url, {"User-Agent": "vision-jira-github-adapter"}, timeout=timeout, limit=1024).status < 400
    except core.Unreachable:
        return False


def app_start(config, request):
    """Starts the app of the clone for QA, and waits until its health address answers. The app
    runs in its own session with a plain environment, so it outlives the job and gets no secret.

    The job remembers the app that it started in a state file of the temporary folder (see
    `app_state_path`): the process id, the start time of that process as the system reports it,
    the head commit, and the log. At the next call for the same clone and env:

    - The app of the state file still runs, and the head commit changed: the job stops its
      process group and starts the app again at the new head (unless `app_restart` is false,
      for an app that loads new code by itself).
    - The address answers, and no app of the job runs: the person started it. The job leaves it
      as it is and says that it can run older code.

    The job never stops a process that it did not start: it compares the start time of the
    process with the start time of the state file first, so that a process id that the system
    gave to a different process is never taken for the app."""
    what = "app-start"
    command = config.text("app_start_command", what)
    health = config.text("app_health_url", what)
    parts = urllib.parse.urlsplit(health)
    if parts.scheme not in ("http", "https") or not parts.netloc:
        raise core.Failure("`app_health_url` must be an http or https address, such as http://localhost:3000/health")
    app_url = config.text("app_url") or urllib.parse.urlunsplit((parts.scheme, parts.netloc, "/", "", ""))
    timeout = config.number("app_start_timeout", 120)
    restart = config.flag("app_restart", True)
    env_name = str((request.get("args") or {}).get("env") or "local")
    if not re.match(r"^[A-Za-z0-9_-]{1,40}$", env_name):
        raise core.Failure("args.env of app-start must be a plain name, such as local")
    clone = request.get("clone")
    if not isinstance(clone, str) or not os.path.isdir(clone):
        raise core.Failure("app-start needs the clone of the ticket")
    head = clone_head(clone)
    info = {"env": env_name, "url": app_url, "health": health, "started": False, "pid": None, "log": None,
            "head": None, "restarted": False}
    name = "app-%s.json" % env_name

    def answer(ok, detail):
        return {"ok": ok, "detail": detail, "documents": [{"name": name, "content": json.dumps(info, indent=2) + "\n"}]}

    state_path = app_state_path(clone, env_name)
    state = read_app_state(state_path)
    ours = state is not None and running_since(state["pid"]) == state["started"]
    if state is not None and not ours:
        # The app of the state file ended, or its process id now names a different process.
        remove_file(state_path)
        state = None
    restarted_from = None
    if ours:
        info.update({"pid": state["pid"], "log": state["log"], "head": state["head"]})
        same = state["head"] == head
        if (same or not restart) and answers(health):
            if same:
                return answer(True, "the app that Vision started (process id %d) already answers at %s, at the head %s, so nothing was started" % (
                    state["pid"], health, core.short(head or "") or "-"))
            return answer(True, "the app that Vision started (process id %d) already answers at %s. The head is %s now, and app_restart is false, so it runs on: it loads the new code by itself" % (
                state["pid"], health, core.short(head or "") or "-"))
        # A new head, or an app of the job that no longer answers: stop it, then start again.
        if running_since(state["pid"]) != state["started"]:
            remove_file(state_path)
        else:
            stop_group(state["pid"], started=state["started"])
            remove_file(state_path)
            restarted_from = state["head"]
            if not wait_silent(health, 10):
                info.update({"pid": None, "log": None, "head": None})
                return answer(False, "Vision stopped its app (process id %d), but %s still answers, so a different process answers there. Stop that process, then run vision retry" % (
                    state["pid"], health))
        info.update({"pid": None, "log": None, "head": None})
    elif answers(health):
        return answer(True, "the app already answers at %s, but Vision did not start it, so it can run older code than the head %s. Stop it to let Vision start the app at the head, or restart it yourself" % (
            health, core.short(head or "") or "-"))
    key = core.check_key(request.get("key"))
    # The log is a new file that only this user can read. Its name has a random part, so that
    # no other user of a shared temporary folder can put a link at that name first.
    log_fd, log_path = tempfile.mkstemp(prefix="vision-app-%s-%s-" % (key, core.stamp()), suffix=".log")
    started = time.monotonic()
    deadline = started + timeout
    # A Python app writes its output at once, so that its log shows how far it came even when
    # it is stopped: a stopped process loses the output that waits in its buffer.
    proc_env = dict(core.plain_env(), PYTHONUNBUFFERED="1")
    with os.fdopen(log_fd, "ab") as log:
        proc = subprocess.Popen(["sh", "-c", command], cwd=clone, env=proc_env, stdin=subprocess.DEVNULL,
                                stdout=log, stderr=subprocess.STDOUT, start_new_session=True)
    info.update({"started": True, "pid": proc.pid, "log": log_path, "head": head,
                 "restarted": restarted_from is not None})
    since = running_since(proc.pid)
    if since:
        write_app_state(state_path, {"clone": clone, "env": env_name, "pid": proc.pid, "started": since,
                                     "head": head, "log": log_path, "at": core.now()})
    again = ""
    if restarted_from is not None:
        again = " It stopped the app of the head %s first, so QA tests the code of the head %s." % (
            core.short(restarted_from or "") or "-", core.short(head or "") or "-")
    while True:
        # One probe never runs past the time limit, and the last probe comes at the limit.
        left = deadline - time.monotonic()
        if answers(health, timeout=max(0.2, min(2.0, left))):
            return answer(True, "started the app (process id %d) in %d s; it answers at %s. Its log is %s.%s" % (
                proc.pid, int(time.monotonic() - started), health, log_path, again))
        code = proc.poll()
        if code is not None:
            remove_file(state_path)
            return answer(False, "the app ended with exit code %d before it answered at %s. The end of its log: %s" % (
                code, health, core.clean(tail_of(log_path), 1500)))
        if time.monotonic() >= deadline:
            stop_group(proc)
            remove_file(state_path)
            return answer(False, "the app did not answer at %s in %d s, so it was stopped. The end of its log: %s" % (
                health, timeout, core.clean(tail_of(log_path), 1500) or "(empty)"))
        time.sleep(min(0.5, max(0.0, deadline - time.monotonic())))


def clone_head(clone):
    """The head commit of the clone, or None when it has none."""
    proc = core.git(["rev-parse", "--verify", "--quiet", "HEAD"], cwd=clone, check=False)
    text = proc.stdout.decode("utf-8", "replace").strip()
    return text if proc.returncode == 0 and core.COMMIT.match(text) else None


def app_state_path(clone, env_name):
    """The state file of the app of one clone and env: `vision-app-<hash>.json` in the temporary
    folder, where the hash is of the real path of the clone and the name of the env."""
    text = "%s\0%s" % (os.path.realpath(clone), env_name)
    digest = hashlib.sha256(text.encode("utf-8")).hexdigest()[:24]
    return os.path.join(tempfile.gettempdir(), "vision-app-%s.json" % digest)


def read_app_state(path):
    """The state of an app that an earlier call started, or None for a missing or broken file.

    The job stops the process that this file names, and other users can write into a shared
    temporary folder. So the file counts only when it is a plain file of this user that no
    other user can write. The job never follows a link at that name."""
    try:
        fd = os.open(path, os.O_RDONLY | os.O_NONBLOCK | getattr(os, "O_NOFOLLOW", 0))
    except OSError:
        return None
    try:
        with os.fdopen(fd, "r", encoding="utf-8") as handle:
            meta = os.fstat(handle.fileno())
            if not stat.S_ISREG(meta.st_mode) or meta.st_uid != os.getuid() or meta.st_mode & 0o022:
                return None
            state = json.load(handle)
    except (OSError, ValueError):
        return None
    if not isinstance(state, dict) or not isinstance(state.get("pid"), int) or state["pid"] <= 1:
        return None
    if not isinstance(state.get("started"), str) or not state["started"]:
        return None
    head = state.get("head")
    return {"pid": state["pid"], "started": state["started"], "head": head if isinstance(head, str) else None,
            "log": state.get("log") if isinstance(state.get("log"), str) else None}


def write_app_state(path, state):
    """Writes the state file for this user only: to a new temporary name first, and then a
    rename. Gives False when the folder does not let it, such as a file of a different user at
    that name. The job then has no state, so it never stops that app at a later call."""
    temp = "%s.%d.tmp" % (path, os.getpid())
    try:
        remove_file(temp)
        fd = os.open(temp, os.O_WRONLY | os.O_CREAT | os.O_EXCL | getattr(os, "O_NOFOLLOW", 0), 0o600)
        with os.fdopen(fd, "w", encoding="utf-8") as handle:
            json.dump(state, handle, indent=2)
            handle.write("\n")
        os.replace(temp, path)
    except OSError:
        remove_file(temp)
        return False
    return True


def remove_file(path):
    with contextlib.suppress(OSError):
        os.remove(path)


def running_since(pid):
    """The start time of a running process as the system reports it (`ps -o lstart=`, on macOS
    and on Linux), or None when no such process runs. A zombie counts as gone."""
    try:
        proc = subprocess.run(["ps", "-o", "stat=", "-o", "lstart=", "-p", str(int(pid))],
                              stdin=subprocess.DEVNULL, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL,
                              env=dict(core.plain_env(), LC_ALL="C", LANG="C"), timeout=30)
    except (OSError, ValueError, subprocess.TimeoutExpired):
        return None
    parts = proc.stdout.decode("utf-8", "replace").split(None, 1)
    if proc.returncode != 0 or len(parts) != 2 or parts[0].startswith("Z"):
        return None
    return " ".join(parts[1].split())


def wait_silent(url, seconds):
    """Waits until the address no longer answers. Gives False when it still answers after
    `seconds`."""
    deadline = time.monotonic() + seconds
    while answers(url, timeout=1.0):
        if time.monotonic() >= deadline:
            return False
        time.sleep(0.2)
    return True


def stop_group(proc, grace=3, started=None):
    """Stops the process group of the app: SIGTERM, then SIGKILL after `grace` seconds. It waits
    for the end of the app, so that the log holds each line that the app wrote.

    `proc` is the process of this job, or the process id of an app that an earlier call
    started. For a process id, `started` is its start time from the state file: the group is
    signalled only while the process with that id still has that start time."""
    if isinstance(proc, subprocess.Popen):
        with contextlib.suppress(OSError):
            os.killpg(proc.pid, signal.SIGTERM)
        try:
            proc.wait(timeout=grace)
        except subprocess.TimeoutExpired:
            with contextlib.suppress(OSError):
                os.killpg(proc.pid, signal.SIGKILL)
            with contextlib.suppress(subprocess.TimeoutExpired):
                proc.wait(timeout=grace)
        return
    pid = int(proc)
    for sig in (signal.SIGTERM, signal.SIGKILL):
        if not started or running_since(pid) != started:
            return
        with contextlib.suppress(OSError):
            os.killpg(pid, sig)
        deadline = time.monotonic() + grace
        while running_since(pid) == started and time.monotonic() < deadline:
            time.sleep(0.1)


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
