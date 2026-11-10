"""bin/app-token: prints an installation token of a GitHub App, and nothing else.

It makes a JWT signed with RS256 by the `openssl` program (the standard library cannot sign with
RSA), and asks GitHub for an installation token. The private key comes from a file or from the
standard output of a command. A key from a command goes to openssl through a pipe: it never
lands in a file, and it is never printed. Use it as the command of a secret:

    FORGE_TOKEN = { cmd = "/path/to/bin/app-token --app-id 123 --installation-id 456 --key-file ~/.keys/app.pem" }
"""

import argparse
import base64
import json
import os
import subprocess
import sys
import time

from . import core

KEY_MAX = 60000


def b64url(data):
    return base64.urlsafe_b64encode(data).rstrip(b"=").decode("ascii")


def parse(argv):
    parser = argparse.ArgumentParser(prog="app-token", description="Print an installation token of a GitHub App.")
    parser.add_argument("--app-id", required=True, help="the app id (or the client id) of the GitHub App")
    parser.add_argument("--installation-id", required=True, type=int, help="the id of the installation")
    source = parser.add_mutually_exclusive_group(required=True)
    source.add_argument("--key-file", help="the file of the private key (PEM)")
    source.add_argument("--key-command", help="a shell command that prints the private key")
    parser.add_argument("--api", default=core.DEFAULT_GITHUB_API, help="the REST address of GitHub")
    parser.add_argument("--timeout", type=float, default=30, help="seconds for each step")
    return parser.parse_args(argv)


def sign(data, key_file=None, key=None, timeout=30):
    """The RS256 signature of `data` by openssl. A key in memory goes through a pipe."""
    command = ["openssl", "dgst", "-sha256", "-sign"]
    pass_fds = ()
    reader = writer = None
    if key is not None:
        if len(key) > KEY_MAX:
            raise core.Failure("the private key is too large")
        reader, writer = os.pipe()
        os.write(writer, key)
        os.close(writer)
        command.append("/dev/fd/%d" % reader)
        pass_fds = (reader,)
    else:
        command.append(key_file)
    try:
        proc = subprocess.run(command, input=data, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                              pass_fds=pass_fds, timeout=timeout, env=core.plain_env())
    except (OSError, subprocess.TimeoutExpired) as e:
        raise core.Failure("could not run openssl: %s" % type(e).__name__)
    finally:
        if reader is not None:
            os.close(reader)
    if proc.returncode != 0 or not proc.stdout:
        # The text of openssl names the problem; it never holds the key.
        raise core.Failure("openssl could not sign with the private key: %s" % core.clean(
            proc.stderr.decode("utf-8", "replace"), 200))
    return proc.stdout


def read_key(args):
    if args.key_file:
        path = os.path.expanduser(args.key_file)
        if not os.path.isfile(path):
            raise core.Failure("the key file %s does not exist" % path)
        return None, path
    try:
        proc = subprocess.run(["sh", "-c", args.key_command], stdout=subprocess.PIPE, stderr=subprocess.DEVNULL,
                              stdin=subprocess.DEVNULL, timeout=args.timeout, env=core.plain_env())
    except subprocess.TimeoutExpired:
        raise core.Failure("the key command did not end in %d s" % args.timeout)
    if proc.returncode != 0 or not proc.stdout.strip():
        raise core.Failure("the key command failed with exit code %d or printed nothing" % proc.returncode)
    return proc.stdout, None


def token(args):
    api = core.check_base(args.api, "--api")
    key, key_file = read_key(args)
    now = int(time.time())
    header = b64url(json.dumps({"alg": "RS256", "typ": "JWT"}, separators=(",", ":")).encode())
    claims = {"iat": now - 60, "exp": now + 540, "iss": int(args.app_id) if args.app_id.isdigit() else args.app_id}
    payload = b64url(json.dumps(claims, separators=(",", ":")).encode())
    signing_input = ("%s.%s" % (header, payload)).encode("ascii")
    jwt = "%s.%s" % (signing_input.decode("ascii"), b64url(sign(signing_input, key_file, key, args.timeout)))
    client = core.Client("github", api, {
        "Authorization": "Bearer " + jwt,
        "Accept": "application/vnd.github+json",
        "X-GitHub-Api-Version": "2022-11-28",
        "User-Agent": "vision-jira-github-adapter",
    }, args.timeout)
    data = client.json("POST", "/app/installations/%d/access_tokens" % args.installation_id,
                       "make an installation token", ok=(201,)) or {}
    value = data.get("token")
    if not isinstance(value, str) or not value:
        raise core.Failure("GitHub gave no token")
    return value


def main(argv=None):
    args = parse(sys.argv[1:] if argv is None else argv)
    try:
        value = token(args)
    except core.Failure as e:
        print("app-token: %s" % e, file=sys.stderr)
        return 1
    sys.stdout.write(value + "\n")
    return 0
