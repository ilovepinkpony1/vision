#!/usr/bin/env python3
"""Serves the fake Jira and GitHub of world.py on a free localhost port, for a test of the whole
path of a ticket. Standard library only; it never reaches the network.

    serve_world.py --dir DIR --remote BARE --issue KEY --title TITLE --description TEXT

- DIR gets `port` (written to a temporary name and renamed, once the server listens),
  `requests.jsonl` (one line for each call of the adapter, with no header value), and
  `world.log`.
- BARE is the bare repo that git.push pushes to: the branches, the merges, and the CI runs
  read it.
- The expected tokens come from the environment: FAKE_JIRA_TOKEN, FAKE_FORGE_TOKEN, and
  FAKE_JIRA_USER. A call with a different token gets 401.

The server stops when its standard input closes, so a test that ends, also with a panic,
never leaves it running.
"""

import argparse
import os
import sys
import threading

sys.dont_write_bytecode = True
HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)

from support import FakeServer  # noqa: E402
from world import World, install  # noqa: E402


def main():
    parser = argparse.ArgumentParser(description="Serve the fake Jira and GitHub of world.py.")
    parser.add_argument("--dir", required=True)
    parser.add_argument("--remote", required=True)
    parser.add_argument("--issue", required=True)
    parser.add_argument("--title", required=True)
    parser.add_argument("--description", default="")
    args = parser.parse_args()
    os.makedirs(args.dir, exist_ok=True)
    tokens = {"jira": os.environ.get("FAKE_JIRA_TOKEN"), "forge": os.environ.get("FAKE_FORGE_TOKEN")}
    world = World(os.path.abspath(args.remote), tokens=tokens, jira_user=os.environ.get("FAKE_JIRA_USER"))
    world.add_issue(args.issue, args.title, args.description)
    server = FakeServer()
    install(server, world, os.path.join(args.dir, "requests.jsonl"))
    temp = os.path.join(args.dir, ".port.tmp")
    with open(temp, "w") as handle:
        handle.write("%d\n" % server.port)
    os.replace(temp, os.path.join(args.dir, "port"))
    print("listening on 127.0.0.1:%d" % server.port, flush=True)

    done = threading.Event()

    def watch_stdin():
        # The end of standard input is the end of the test.
        while sys.stdin.buffer.read(1024):
            pass
        done.set()

    threading.Thread(target=watch_stdin, daemon=True).start()
    done.wait()
    server.stop()


if __name__ == "__main__":
    main()
