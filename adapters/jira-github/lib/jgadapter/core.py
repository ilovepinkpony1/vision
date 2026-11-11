"""The call loop of a script, the configuration, the secrets, git, and HTTP.

Rules that each module follows:

- A string from Jira or GitHub is data. It never reaches a shell, and it reaches a command line
  only as one item of an argument list.
- A secret comes from the environment of the script, and it goes only into one HTTP header or
  into the environment of one git call. It never goes into a file, a command line, an answer,
  or an error text. Each user of a machine can read a command line in the process list.
- A child process that runs code of the project gets a plain environment with no secret.
"""

import base64
import datetime
import json
import os
import re
import socket
import subprocess
import sys
import urllib.error
import urllib.parse
import urllib.request

# The secrets that the scripts read, by name. Each value is masked in each text that leaves.
SECRET_NAMES = ("JIRA_TOKEN", "FORGE_TOKEN", "PR_TOKEN")

# The executor gives a script these names and the secrets. A child process that runs code of
# the project gets only these.
PLAIN_ENV = ("PATH", "HOME", "LANG", "TMPDIR")

# Each git call: no hook of the clone, no fsmonitor command, no credential helper, no prompt.
GIT_SAFE = [
    "-c", "core.hooksPath=/dev/null",
    "-c", "core.fsmonitor=false",
    "-c", "credential.helper=",
    "-c", "core.askPass=",
]

KEY = re.compile(r"^[A-Z][A-Z0-9_]{0,31}-[1-9][0-9]{0,9}$")
COMMIT = re.compile(r"^[0-9a-f]{40}([0-9a-f]{24})?$")

DEFAULT_GITHUB_API = "https://api.github.com"
DEFAULT_HTTP_TIMEOUT = 30


class Failure(Exception):
    """The call failed: exit code 1."""


class Refusal(Exception):
    """A gate of the script refused the call: exit code 2."""


class HttpError(Failure):
    """A server answered with a status that the call does not accept."""

    def __init__(self, text, status):
        Failure.__init__(self, text)
        self.status = status


class Unreachable(Failure):
    """No answer: the address, the network, or the time limit."""


def now():
    return datetime.datetime.now(datetime.timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")


def stamp():
    return datetime.datetime.now(datetime.timezone.utc).strftime("%Y%m%dT%H%M%SZ")


def short(commit):
    return (commit or "-")[:12]


def mask(text):
    """The text with each secret value of the environment replaced by its name."""
    text = str(text)
    for name in SECRET_NAMES:
        value = os.environ.get(name)
        if value and len(value) >= 4 and value in text:
            text = text.replace(value, "[secret %s]" % name)
    return text


def clean(text, limit=300):
    """Untrusted text for an error or a detail: one line, no control character, cut."""
    text = "".join(c if c.isprintable() else " " for c in str(text))
    text = " ".join(text.split())
    return text if len(text) <= limit else text[:limit] + " [cut]"


def mask_json(value):
    if isinstance(value, str):
        return mask(value)
    if isinstance(value, list):
        return [mask_json(v) for v in value]
    if isinstance(value, dict):
        return {mask(k): mask_json(v) for k, v in value.items()}
    return value


def main(handler):
    """Reads the call from standard input, runs the handler, and prints its answer."""
    try:
        data = sys.stdin.buffer.read().decode("utf-8")
        request = json.loads(data) if data.strip() else {}
        if not isinstance(request, dict):
            raise Failure("the call is not a JSON object")
        result = handler(request)
    except Refusal as e:
        text = mask(e)
        print(text, file=sys.stderr)
        sys.stdout.buffer.write(json.dumps({"detail": text}).encode("utf-8"))
        sys.exit(2)
    except Failure as e:
        print(mask(e), file=sys.stderr)
        sys.exit(1)
    except Exception as e:  # noqa: BLE001 - the last guard: a masked line, never a trace
        print("unexpected error: %s: %s" % (type(e).__name__, mask(e)), file=sys.stderr)
        sys.exit(1)
    sys.stdout.buffer.write(json.dumps(mask_json(result)).encode("utf-8"))
    sys.exit(0)


def not_built(what):
    """A part of a later stage: the core stops with a clear need and never acts wrongly."""
    raise Refusal("%s is not built yet in the adapter jira-github" % what)


# Configuration --------------------------------------------------------------------------------


class Config:
    """The [adapter] table of the configuration, with the defaults of adapter.toml, and
    `base_ref` of the call: the `base` of adapter.toml, such as origin/main."""

    def __init__(self, table, base_ref=None):
        self.table = table if isinstance(table, dict) else {}
        self.base_ref = base_ref if isinstance(base_ref, str) else None

    @classmethod
    def of(cls, request):
        return cls(request.get("config"), request.get("base_ref"))

    def get(self, key, default=None):
        value = self.table.get(key)
        if value is None or value == "" or value == [] or value == {}:
            return default
        return value

    def need(self, key, why):
        value = self.get(key)
        if value is None:
            raise Failure("%s needs the key `%s` in the [adapter] table of config.toml" % (why, key))
        return value

    def text(self, key, why=None, default=None):
        value = self.need(key, why) if why else self.get(key, default)
        if value is not None and not isinstance(value, str):
            raise Failure("`%s` in the [adapter] table must be a text" % key)
        return value

    def number(self, key, default):
        value = self.get(key, default)
        if isinstance(value, bool) or not isinstance(value, (int, float)) or value <= 0:
            raise Failure("`%s` in the [adapter] table must be a number above 0" % key)
        return value

    def flag(self, key, default):
        """A key that is true or false."""
        value = self.table.get(key)
        if value is None:
            return default
        if not isinstance(value, bool):
            raise Failure("`%s` in the [adapter] table must be true or false" % key)
        return value

    def logins(self, key):
        """A list of GitHub logins, lowercase."""
        value = self.get(key, [])
        if isinstance(value, str):
            value = [value]
        if not isinstance(value, list) or not all(isinstance(v, str) for v in value):
            raise Failure("`%s` in the [adapter] table must be a list of GitHub logins" % key)
        return [v.strip().lower() for v in value if v.strip()]

    def repo(self, why):
        repo = self.text("github_repo", why)
        if not re.match(r"^[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+$", repo):
            raise Failure("`github_repo` must be owner/name, such as acme/widgets")
        return repo

    def base_branch(self):
        """The branch that pull requests go into. `base_ref` of the call gives it: the text
        after its first `/`, such as main of origin/main. The key `base_branch` of the
        [adapter] table overrides it, for a project whose remote branch has a different name.
        With neither, it is main."""
        override = self.text("base_branch")
        if override:
            return override
        ref = (self.base_ref or "").strip()
        if ref:
            return ref.split("/", 1)[1] if "/" in ref else ref
        return "main"

    def http_timeout(self):
        return self.number("http_timeout", DEFAULT_HTTP_TIMEOUT)

    def envs(self):
        """The staging envs: a list of tables with `name` and `branch`, and the optional
        `deployment`: the name of a GitHub environment whose deployments tell which commit the
        env runs."""
        value = self.get("envs", [])
        if isinstance(value, dict):
            value = [dict(v, name=k) for k, v in value.items() if isinstance(v, dict)]
        if not isinstance(value, list):
            raise Failure("`envs` in the [adapter] table must be a list of tables with name and branch")
        out = []
        for item in value:
            if not isinstance(item, dict) or not isinstance(item.get("name"), str) or not isinstance(item.get("branch"), str):
                raise Failure("each item of `envs` needs `name` and `branch`, such as { name = \"staging\", branch = \"staging\" }")
            env = {"name": item["name"], "branch": item["branch"]}
            deployment = item.get("deployment")
            if deployment is not None:
                if not isinstance(deployment, str) or not deployment.strip():
                    raise Failure("`deployment` of the env %s must be the name of a GitHub environment, such as \"staging\""
                                  % clean(item["name"], 40))
                env["deployment"] = deployment.strip()
            out.append(env)
        return out


def secret(name, why):
    value = os.environ.get(name)
    if not value:
        raise Failure(
            "%s needs the secret %s. Add it to [secrets] of config.toml, such as %s = { env = \"...\" }" % (why, name, name)
        )
    return value


def plain_env():
    return {k: os.environ[k] for k in PLAIN_ENV if k in os.environ}


def check_key(key, what="the ticket key"):
    if not isinstance(key, str) or not KEY.match(key):
        raise Failure("%s must be a Jira key such as DEMO-12, not %r" % (what, clean(key, 40)))
    return key


# git ------------------------------------------------------------------------------------------


def git_env():
    env = plain_env()
    env["GIT_TERMINAL_PROMPT"] = "0"
    env["GIT_CONFIG_NOSYSTEM"] = "1"
    env["GIT_CONFIG_GLOBAL"] = os.devnull
    env["GIT_ASKPASS"] = ""
    env["SSH_ASKPASS"] = ""
    return env


def git(args, cwd=None, check=True, data=None, timeout=120, config=()):
    """Runs git with the safe options and a plain environment. Each item `name=value` of
    `config` is one more configuration value of this call. It goes through the environment
    (GIT_CONFIG_COUNT, git 2.31 or newer) and not through the command line, because such a
    value can hold a token. With `check`, a failure raises Failure. Each text of the failure
    is masked."""
    command = ["git"] + GIT_SAFE + list(args)
    env = git_env()
    for index, item in enumerate(config):
        name, _, value = item.partition("=")
        env["GIT_CONFIG_KEY_%d" % index] = name
        env["GIT_CONFIG_VALUE_%d" % index] = value
    if config:
        env["GIT_CONFIG_COUNT"] = str(len(config))
    try:
        proc = subprocess.run(
            command,
            cwd=cwd,
            input=data,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            env=env,
            timeout=timeout,
        )
    except subprocess.TimeoutExpired:
        raise Failure("git %s did not end in %d s" % (args[0] if args else "", timeout))
    except OSError as e:
        raise Failure("could not run git: %s" % e)
    if check and proc.returncode != 0:
        err = proc.stderr.decode("utf-8", "replace").strip().splitlines()[-3:]
        raise Failure("git %s failed: %s" % (args[0] if args else "", mask(clean(" ".join(err)))))
    return proc


def git_text(args, cwd=None, **kw):
    return git(args, cwd=cwd, **kw).stdout.decode("utf-8", "replace").strip()


# HTTP -----------------------------------------------------------------------------------------


class _NoRedirect(urllib.request.HTTPRedirectHandler):
    """A redirect is never followed by itself: it could carry the token to a different host."""

    def redirect_request(self, req, fp, code, msg, headers, newurl):
        return None


_OPENER = urllib.request.build_opener(urllib.request.ProxyHandler({}), _NoRedirect())


def check_base(url, key):
    """A base address of the configuration. A token goes only over https, or to this machine."""
    if not isinstance(url, str) or not url.strip():
        raise Failure("`%s` in the [adapter] table must be an address, such as https://example.atlassian.net" % key)
    parts = urllib.parse.urlsplit(url.strip())
    local = parts.hostname in ("localhost", "127.0.0.1", "::1")
    if parts.scheme == "https" or (parts.scheme == "http" and local):
        if parts.query or parts.fragment or parts.username:
            raise Failure("`%s` must be a plain address with no query, fragment, or user" % key)
        return url.strip().rstrip("/")
    raise Failure("`%s` must start with https:// (a token never travels in clear text)" % key)


def segment(text):
    """One path segment from untrusted text."""
    return urllib.parse.quote(str(text), safe="")


def ref_path(branch):
    """A branch name as a path, each part quoted."""
    return "/".join(segment(part) for part in str(branch).split("/"))


class Response:
    def __init__(self, status, headers, body):
        self.status = status
        self.headers = headers
        self.body = body

    def json(self):
        if not self.body:
            return None
        try:
            return json.loads(self.body.decode("utf-8"))
        except ValueError:
            raise Failure("the answer is not JSON")

    def header(self, name):
        for key, value in self.headers:
            if key.lower() == name.lower():
                return value
        return None


def _message(body):
    """The error message of a Jira or GitHub answer, cut, or nothing."""
    try:
        data = json.loads(body.decode("utf-8"))
    except (ValueError, UnicodeDecodeError, AttributeError):
        return ""
    parts = []
    if isinstance(data, dict):
        if isinstance(data.get("message"), str):
            parts.append(data["message"])
        for item in data.get("errorMessages") or []:
            if isinstance(item, str):
                parts.append(item)
        errors = data.get("errors")
        if isinstance(errors, dict):
            parts += ["%s: %s" % (k, v) for k, v in errors.items() if isinstance(v, str)]
        elif isinstance(errors, list):
            for item in errors:
                if isinstance(item, dict) and isinstance(item.get("message"), str):
                    parts.append(item["message"])
    return mask(clean("; ".join(parts), 300))


def send(method, url, headers, body=None, timeout=DEFAULT_HTTP_TIMEOUT, limit=None):
    """One HTTP call with a time limit. Gives a Response for any status, a redirect included.
    `limit` keeps only the last `limit` bytes of the body. Raises Unreachable."""
    request = urllib.request.Request(url, data=body, method=method)
    for key, value in headers.items():
        request.add_header(key, value)
    try:
        response = _OPENER.open(request, timeout=timeout)
    except urllib.error.HTTPError as e:
        data = e.read() if e.fp is not None else b""
        return Response(e.code, list(e.headers.items()) if e.headers else [], data)
    except (urllib.error.URLError, socket.timeout, ConnectionError, OSError) as e:
        reason = getattr(e, "reason", e)
        raise Unreachable("no answer: %s" % mask(clean(reason, 200)))
    with response:
        data = _read(response, limit)
        return Response(response.status, list(response.headers.items()), data)


def _read(response, limit):
    if limit is None:
        return response.read()
    kept = b""
    while True:
        chunk = response.read(65536)
        if not chunk:
            return kept
        kept = (kept + chunk)[-limit:]


class Client:
    """A JSON client of one service: the base address, the auth header, and the time limit."""

    def __init__(self, service, base, headers, timeout):
        self.service = service
        self.base = base
        self.headers = headers
        self.timeout = timeout

    def call(self, method, path, what, query=None, body=None, ok=(200,), limit=None):
        """Gives the Response of a call whose status is in `ok`. A different status raises
        HttpError, which names the call and the status, never a header."""
        url = self.base + path
        if query:
            url += "?" + urllib.parse.urlencode(query)
        data = None
        headers = dict(self.headers)
        if body is not None:
            data = json.dumps(body).encode("utf-8")
            headers["Content-Type"] = "application/json"
        label = "%s: %s (%s %s)" % (self.service, what, method, path)
        try:
            response = send(method, url, headers, data, self.timeout, limit)
        except Unreachable as e:
            raise Unreachable("%s: %s in %d s" % (label, e, self.timeout))
        if response.status not in ok:
            message = _message(response.body)
            raise HttpError(
                "%s answered HTTP %d%s" % (label, response.status, ": " + message if message else ""),
                response.status,
            )
        return response

    def json(self, method, path, what, **kw):
        return self.call(method, path, what, **kw).json()

    def pages(self, path, what, query=None, per_page=100, max_pages=20):
        """Each item of a list that GitHub pages with `page` and `per_page`."""
        items = []
        for page in range(1, max_pages + 1):
            q = dict(query or {})
            q["per_page"] = per_page
            q["page"] = page
            batch = self.json("GET", path, what, query=q)
            if isinstance(batch, dict):
                # The runs and the jobs come inside an object.
                batch = next((v for v in batch.values() if isinstance(v, list)), [])
            if not isinstance(batch, list):
                raise Failure("%s: %s gave no list" % (self.service, what))
            items += batch
            if len(batch) < per_page:
                break
        return items


def basic(user, token):
    return "Basic " + base64.b64encode(("%s:%s" % (user, token)).encode("utf-8")).decode("ascii")
