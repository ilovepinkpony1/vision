"""Helpers of the tests: a fake HTTP server on a free localhost port, temporary git repos, and a
runner that calls a script the way the executor does (a plain environment and the secrets).

No test reaches the network: each base address of the configuration points at the fake server.
"""

import base64
import hashlib
import http.server
import json
import os
import shutil
import subprocess
import sys
import tempfile
import threading
import time
import unittest
import urllib.parse

ADAPTER = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
BIN = os.path.join(ADAPTER, "bin")
LIB = os.path.join(ADAPTER, "lib")
if LIB not in sys.path:
    sys.path.insert(0, LIB)

# Fake secrets. A test fails when one of them, or its base64 form, shows up where it must not.
SECRETS = {
    "JIRA_TOKEN": "jira-fake-token-7c1d2e3f4a5b",
    "FORGE_TOKEN": "forge-fake-token-9e8d7c6b5a41",
    "PR_TOKEN": "pr-fake-token-31a2b3c4d5e6",
}

REAL_GIT = shutil.which("git")
KEY = "DEMO-12"
BRANCH = "vision/DEMO-12"
REPO = "acme/widgets"


def secret_forms():
    """Each secret, and the forms in which a client could leak it."""
    forms = []
    for value in SECRETS.values():
        forms += [value, base64.b64encode(("x-access-token:" + value).encode()).decode()]
    return forms


class FakeServer:
    """Serves canned answers and records each request.

    `route(method, path, answer)`: `path` has no query. `answer` is a callable that takes the
    request record and gives (status, body) or (status, body, headers), or a list of such
    tuples that is served in order (the last one repeats). `fallback`, when set, answers each
    request that no route answers, or gives None. An unknown route answers 404.
    """

    def __init__(self):
        self.routes = {}
        self.fallback = None
        self.requests = []
        self.lock = threading.Lock()
        fake = self

        class Handler(http.server.BaseHTTPRequestHandler):
            protocol_version = "HTTP/1.1"

            def log_message(self, *args):
                pass

            def handle_any(self):
                fake.serve(self)

            do_GET = do_POST = do_PUT = do_PATCH = do_DELETE = handle_any

        self.httpd = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Handler)
        self.httpd.daemon_threads = True
        self.httpd.block_on_close = False
        self.port = self.httpd.server_address[1]
        self.url = "http://127.0.0.1:%d" % self.port
        self.thread = threading.Thread(target=self.httpd.serve_forever, kwargs={"poll_interval": 0.02}, daemon=True)
        self.thread.start()

    def stop(self):
        self.httpd.shutdown()
        self.httpd.server_close()

    def route(self, method, path, answer):
        with self.lock:
            self.routes[(method, path)] = answer

    def json_route(self, method, path, body, status=200):
        self.route(method, path, lambda req: (status, body))

    def calls(self, method=None, path=None):
        with self.lock:
            return [r for r in self.requests
                    if (method is None or r["method"] == method) and (path is None or r["path"] == path)]

    def serve(self, handler):
        parts = urllib.parse.urlsplit(handler.path)
        length = int(handler.headers.get("Content-Length") or 0)
        raw = handler.rfile.read(length) if length else b""
        try:
            body = json.loads(raw.decode()) if raw else None
        except ValueError:
            body = raw.decode("utf-8", "replace")
        record = {
            "method": handler.command,
            "path": urllib.parse.unquote(parts.path),
            "raw_path": parts.path,
            "query": {k: v[0] for k, v in urllib.parse.parse_qs(parts.query).items()},
            "headers": {k.lower(): v for k, v in handler.headers.items()},
            "body": body,
        }
        with self.lock:
            self.requests.append(record)
            answer = self.routes.get((handler.command, record["path"]))
            if isinstance(answer, list):
                result = answer.pop(0) if len(answer) > 1 else answer[0]
            else:
                result = None
        if answer is None:
            result = self.fallback(record) if self.fallback else None
            if result is None:
                result = (404, {"message": "Not Found (fake server)"})
        elif result is None:
            result = answer(record)
        status, data = result[0], result[1]
        headers = result[2] if len(result) > 2 else {}
        if callable(data):
            data = data()
        if isinstance(data, (dict, list)):
            payload = json.dumps(data).encode()
            headers = dict({"Content-Type": "application/json"}, **headers)
        elif isinstance(data, str):
            payload = data.encode()
        elif data is None:
            payload = b""
        else:
            payload = data
        delay = headers.pop("X-Fake-Delay", None)
        if delay:
            time.sleep(float(delay))
        handler.send_response(status)
        for key, value in headers.items():
            handler.send_header(key, value)
        handler.send_header("Content-Length", str(len(payload)))
        handler.end_headers()
        if handler.command != "HEAD":
            handler.wfile.write(payload)


class Result:
    def __init__(self, proc):
        self.code = proc.returncode
        self.stdout = proc.stdout.decode("utf-8", "replace")
        self.stderr = proc.stderr.decode("utf-8", "replace")
        try:
            self.json = json.loads(self.stdout) if self.stdout.strip() else None
        except ValueError:
            self.json = None

    def __repr__(self):
        return "Result(code=%d, stdout=%r, stderr=%r)" % (self.code, self.stdout[:2000], self.stderr[-2000:])


def git(args, cwd, check=True):
    env = {"PATH": os.environ.get("PATH", ""), "GIT_CONFIG_GLOBAL": os.devnull, "GIT_CONFIG_NOSYSTEM": "1",
           "HOME": cwd, "LANG": "C"}
    proc = subprocess.run(
        [REAL_GIT, "-c", "user.name=tester", "-c", "user.email=tester@example.invalid", "-c", "core.hooksPath=/dev/null",
         "-c", "init.defaultBranch=main", "-c", "commit.gpgsign=false"] + list(args),
        cwd=cwd, stdout=subprocess.PIPE, stderr=subprocess.PIPE, env=env,
    )
    if check and proc.returncode != 0:
        raise AssertionError("git %s failed: %s" % (args, proc.stderr.decode()))
    return proc.stdout.decode().strip()


class Repos:
    """A bare remote with `main`, and a clone on the branch of the ticket with one commit."""

    def __init__(self, root):
        self.root = root
        self.remote = os.path.join(root, "remote.git")
        self.clone = os.path.join(root, "clone")
        os.makedirs(self.clone)
        git(["init", "--bare", "--quiet", self.remote], root)
        git(["init", "--quiet"], self.clone)
        self.write({"app.py": "def add(a, b):\n    return a + b\n"})
        self.base = self.commit("the first commit")
        git(["push", "--quiet", self.remote, "main:refs/heads/main"], self.clone)
        git(["checkout", "--quiet", "-b", BRANCH], self.clone)
        self.write({"app.py": "def add(a, b):\n    return a + b\n\n\ndef sub(a, b):\n    return a - b\n"})
        self.head = self.commit("add sub")

    def write(self, files):
        for name, text in files.items():
            path = os.path.join(self.clone, name)
            os.makedirs(os.path.dirname(path), exist_ok=True)
            with open(path, "w") as handle:
                handle.write(text)

    def commit(self, message):
        git(["add", "-A"], self.clone)
        git(["commit", "--quiet", "-m", message], self.clone)
        return git(["rev-parse", "HEAD"], self.clone)

    def remote_head(self, branch):
        proc = subprocess.run([REAL_GIT, "rev-parse", "--verify", "--quiet", "refs/heads/" + branch],
                              cwd=self.remote, stdout=subprocess.PIPE)
        return proc.stdout.decode().strip() or None


class AdapterTest(unittest.TestCase):
    """Starts the fake server, makes a temporary folder, and runs the scripts."""

    def setUp(self):
        self.server = FakeServer()
        self.addCleanup(self.server.stop)
        self.tmp = tempfile.mkdtemp(prefix="jg-test-")
        self.addCleanup(shutil.rmtree, self.tmp, True)
        self.home = os.path.join(self.tmp, "home")
        self.scratch = os.path.join(self.tmp, "scratch")
        os.makedirs(self.home)
        os.makedirs(self.scratch)
        self.path_prefix = []

    def config(self, **more):
        config = {
            "jira_url": self.server.url,
            "jira_user": "bot@example.invalid",
            "queue_jql": "project = DEMO AND status = \"To Do\"",
            "github_repo": REPO,
            "github_api": self.server.url,
            "ci_workflow": "ci.yml",
            "approvers": ["lead"],
            "own_logins": ["vision-bot", "person"],
            "statuses": {"in_progress": "In Progress", "in_review": "In Review"},
            "ci_find_seconds": 2,
            "ci_poll_seconds": 0.2,
            "http_timeout": 5,
        }
        config.update(more)
        return {k: v for k, v in config.items() if v is not None}

    def env(self, secrets=None):
        env = {
            "PATH": os.pathsep.join(self.path_prefix + [os.environ.get("PATH", "")]),
            "HOME": self.home,
            "LANG": "C.UTF-8",
            "TMPDIR": self.scratch,
        }
        env.update(SECRETS if secrets is None else secrets)
        return env

    def run_script(self, name, request, secrets=None, timeout=120):
        proc = subprocess.run(
            [os.path.join(BIN, name)], input=json.dumps(request).encode(),
            stdout=subprocess.PIPE, stderr=subprocess.PIPE, env=self.env(secrets), timeout=timeout,
        )
        result = Result(proc)
        self.assert_no_secret(result.stdout + result.stderr, "the output of bin/%s" % name)
        return result

    # The core gives each call the `base` of adapter.toml as base_ref.
    BASE_REF = "origin/main"

    def observe(self, request, **kw):
        request = dict(request)
        request.setdefault("config", self.config())
        request.setdefault("base_ref", self.BASE_REF)
        return self.run_script("observe", request, **kw)

    def act(self, verb, args=None, reconcile=False, read=False, config=None, secrets=None, **fields):
        request = {"verb": verb, "op": "op-test", "key": KEY, "branch": BRANCH, "args": args or {},
                   "config": config or self.config(), "base_ref": self.BASE_REF}
        request.update(fields)
        if reconcile:
            request["reconcile"] = True
        if read:
            request["read"] = True
        return self.run_script("act", request, secrets=secrets)

    def job(self, name, args=None, config=None, secrets=None, timeout=120, **fields):
        request = {"job": name, "key": KEY, "branch": BRANCH, "args": args or {}, "config": config or self.config(),
                   "base_ref": self.BASE_REF}
        request.update(fields)
        return self.run_script("job", request, secrets={} if secrets is None else secrets, timeout=timeout)

    def assert_no_secret(self, text, where):
        for form in secret_forms():
            self.assertNotIn(form, text, "a secret is in %s" % where)

    def assert_no_secret_in_files(self, *roots):
        for root in roots:
            for folder, _dirs, files in os.walk(root):
                for name in files:
                    path = os.path.join(folder, name)
                    if os.path.islink(path):
                        continue
                    with open(path, "rb") as handle:
                        data = handle.read().decode("latin-1")
                    for form in secret_forms():
                        self.assertNotIn(form, data, "a secret is in the file %s" % path)

    def git_spy(self):
        """A `git` first in PATH that logs its arguments, the names of its environment, and
        the configuration values that came through the environment (the header value as a
        hash), and runs the real git. Gives the path of the log."""
        spy = os.path.join(self.tmp, "spy")
        os.makedirs(spy, exist_ok=True)
        log = os.path.join(self.tmp, "git-calls.jsonl")
        with open(os.path.join(spy, "git"), "w") as handle:
            handle.write(
                "#!%s\nimport hashlib, json, os, sys\n"
                "def shown(item):\n"
                "    if 'extraheader=' in item:\n"
                "        name, value = item.split('=', 1)\n"
                "        return name + '=<sha256:' + hashlib.sha256(value.encode()).hexdigest() + '>'\n"
                "    return item\n"
                "args = [shown(a) for a in sys.argv[1:]]\n"
                "count = int(os.environ.get('GIT_CONFIG_COUNT') or 0)\n"
                "config = [shown(os.environ['GIT_CONFIG_KEY_%%d' %% i] + '=' + os.environ['GIT_CONFIG_VALUE_%%d' %% i]) for i in range(count)]\n"
                "with open(%r, 'a') as f:\n"
                "    f.write(json.dumps({'args': args, 'config': config, 'env': sorted(os.environ)}) + '\\n')\n"
                "os.execv(%r, [%r] + sys.argv[1:])\n" % (sys.executable, log, REAL_GIT, REAL_GIT)
            )
        os.chmod(os.path.join(spy, "git"), 0o755)
        self.path_prefix = [spy]
        return log


def header_hash(token):
    value = "Authorization: Basic " + base64.b64encode(("x-access-token:" + token).encode()).decode()
    return "<sha256:" + hashlib.sha256(value.encode()).hexdigest() + ">"


def read_calls(log):
    if not os.path.exists(log):
        return []
    with open(log) as handle:
        return [json.loads(line) for line in handle if line.strip()]
