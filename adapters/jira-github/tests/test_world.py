"""serve_world.py: the fake Jira and GitHub with state, as a process on a free localhost port.

The test of the whole path (tests/e2e_jira_github.rs of the repo) drives the core through this
world. Here the world itself is tested: its entry point, its checks of the tokens, its CI, and
its control calls.
"""

import base64
import json
import os
import subprocess
import sys
import time
import unittest
import urllib.error
import urllib.request

from support import AdapterTest, git

HERE = os.path.dirname(os.path.abspath(__file__))
JIRA, FORGE, USER = "jira-world-token-5a6b7c8d", "forge-world-token-1e2f3a4b", "bot@example.invalid"


def call(url, method="GET", body=None, headers=None):
    data = json.dumps(body).encode() if body is not None else None
    request = urllib.request.Request(url, data=data, method=method)
    for key, value in (headers or {}).items():
        request.add_header(key, value)
    if data is not None:
        request.add_header("Content-Type", "application/json")
    opener = urllib.request.build_opener(urllib.request.ProxyHandler({}))
    try:
        with opener.open(request, timeout=30) as response:
            raw = response.read()
            return response.status, json.loads(raw) if raw else None
    except urllib.error.HTTPError as e:
        with e:
            raw = e.read()
        return e.code, json.loads(raw) if raw else None


class World(AdapterTest):
    def setUp(self):
        super().setUp()
        self.remote = os.path.join(self.tmp, "remote.git")
        work = os.path.join(self.tmp, "work")
        os.makedirs(work)
        git(["init", "--bare", "--quiet", self.remote], self.tmp)
        git(["init", "--quiet"], work)
        with open(os.path.join(work, "ci.sh"), "w") as handle:
            handle.write("echo checking; test ! -f TODO.txt\n")
        git(["add", "-A"], work)
        git(["commit", "--quiet", "-m", "start"], work)
        git(["push", "--quiet", self.remote, "HEAD:refs/heads/main"], work)
        with open(os.path.join(work, "TODO.txt"), "w") as handle:
            handle.write("x\n")
        git(["add", "-A"], work)
        git(["commit", "--quiet", "-m", "todo"], work)
        git(["push", "--quiet", self.remote, "HEAD:refs/heads/vision/DEMO-12"], work)
        self.dir = os.path.join(self.tmp, "world")
        env = {"PATH": os.environ.get("PATH", ""), "HOME": self.home, "LANG": "C.UTF-8", "TMPDIR": self.scratch,
               "FAKE_JIRA_TOKEN": JIRA, "FAKE_FORGE_TOKEN": FORGE, "FAKE_JIRA_USER": USER}
        self.proc = subprocess.Popen(
            [sys.executable, "-B", os.path.join(HERE, "serve_world.py"), "--dir", self.dir, "--remote", self.remote,
             "--issue", "DEMO-12", "--title", "Add subtract", "--description", "The calculator can only add."],
            stdin=subprocess.PIPE, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, env=env,
        )
        self.addCleanup(self.stop)
        port_file = os.path.join(self.dir, "port")
        deadline = time.monotonic() + 60
        while not os.path.exists(port_file):
            self.assertIsNone(self.proc.poll(), "the world ended before it listened")
            self.assertLess(time.monotonic(), deadline, "the world never wrote its port")
            time.sleep(0.05)
        with open(port_file) as handle:
            self.url = "http://127.0.0.1:%d" % int(handle.read())

    def stop(self):
        if self.proc.poll() is None:
            self.proc.stdin.close()
            try:
                self.proc.wait(timeout=30)
            except subprocess.TimeoutExpired:
                self.proc.kill()
                self.proc.wait()

    def jira(self, method, path, body=None):
        auth = "Basic " + base64.b64encode(("%s:%s" % (USER, JIRA)).encode()).decode()
        return call(self.url + "/rest/api/3" + path, method, body, {"Authorization": auth})

    def gh(self, method, path, body=None):
        return call(self.url + "/repos/acme/widgets" + path, method, body, {"Authorization": "Bearer " + FORGE})

    def test_the_world_checks_tokens_and_keeps_state(self):
        self.assertEqual(call(self.url + "/rest/api/3/issue/DEMO-12")[0], 401)
        self.assertEqual(call(self.url + "/repos/acme/widgets/branches", headers={"Authorization": "Bearer x"})[0], 401)
        status, issue = self.jira("GET", "/issue/DEMO-12?fields=summary,status,description")
        self.assertEqual(status, 200)
        self.assertEqual(issue["fields"]["status"], {"name": "To Do"})
        self.assertEqual(issue["fields"]["description"]["content"][0]["content"][0]["text"], "The calculator can only add.")
        self.assertEqual(self.jira("POST", "/issue/DEMO-12/transitions", {"transition": {"id": "11"}})[0], 204)
        self.assertEqual(self.jira("GET", "/issue/DEMO-12?fields=status")[1]["fields"]["status"], {"name": "In Progress"})
        status, branches = self.gh("GET", "/branches")
        self.assertEqual(sorted(b["name"] for b in branches), ["main", "vision/DEMO-12"])
        status, pull = self.gh("POST", "/pulls", {"title": "T", "head": "vision/DEMO-12", "base": "main", "draft": True})
        self.assertEqual((status, pull["number"], pull["draft"]), (201, 7, True))
        # A dispatch runs ci.sh of the head: TODO.txt makes it red.
        self.assertEqual(self.gh("POST", "/actions/workflows/ci.yml/dispatches", {"ref": "vision/DEMO-12"})[0], 204)
        # A run takes its time: queued, in progress, and then ended.
        seen = []
        for _ in range(3):
            runs = self.gh("GET", "/actions/runs?head_sha=" + pull["head"]["sha"])[1]["workflow_runs"]
            seen.append((runs[0]["status"], runs[0]["conclusion"], runs[0]["run_attempt"]))
        self.assertEqual(seen, [("queued", None, 1), ("in_progress", None, 1), ("completed", "failure", 1)])
        self.assertEqual(self.gh("POST", "/actions/runs/%d/rerun-failed-jobs" % runs[0]["id"])[0], 201)
        self.assertEqual(self.gh("GET", "/actions/runs/%d" % runs[0]["id"])[1]["run_attempt"], 2)
        self.assertEqual(self.gh("POST", "/actions/runs/%d/rerun-failed-jobs" % runs[0]["id"])[0], 403,
                         "a run that runs cannot run again")
        self.gh("GET", "/actions/runs/%d" % runs[0]["id"])
        jobs = self.gh("GET", "/actions/runs/%d/jobs" % runs[0]["id"])[1]["jobs"]
        self.assertEqual([j["conclusion"] for j in jobs], ["failure"])
        # The team: ready, a review, and the merge.
        self.assertEqual(call(self.url + "/_control/ready", "POST", {"number": 7})[1]["draft"], False)
        call(self.url + "/_control/review", "POST", {"number": 7, "login": "lead", "state": "APPROVED", "id": 601})
        self.assertEqual(self.gh("GET", "/pulls/7/reviews")[1][0]["id"], 601)
        merged = call(self.url + "/_control/merge", "POST", {"number": 7})[1]
        self.assertTrue(merged["merged"])
        self.assertIn("Merge pull request #7", git(["log", "-1", "--format=%s", "main"], self.remote))
        # The log has each call of the adapter, with no header value, and no control call.
        with open(os.path.join(self.dir, "requests.jsonl")) as handle:
            text = handle.read()
        entries = [json.loads(line) for line in text.splitlines()]
        self.assertNotIn(JIRA, text)
        self.assertNotIn(FORGE, text)
        self.assertEqual([e["auth"] for e in entries[:2]], ["none", "bad"])
        self.assertFalse([e for e in entries if e["path"].startswith("/_control/")])
        # The world ends when its standard input closes.
        self.proc.stdin.close()
        self.assertEqual(self.proc.wait(timeout=30), 0)


if __name__ == "__main__":
    unittest.main()
