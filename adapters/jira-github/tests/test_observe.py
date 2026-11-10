"""bin/observe: the queue, and one ticket with each review state and each CI state."""

import unittest

from fixtures import HEAD, OTHER, REPO_PATH, GitHubWorld, adf_doc, comment, jira_issue, pull, review, run
from support import BRANCH, KEY, SECRETS, AdapterTest

SEARCH = "/rest/api/3/search/jql"


class Queue(AdapterTest):
    def setUp(self):
        super().setUp()
        pages = [
            (200, {"issues": [jira_issue("DEMO-1", "Free one"), jira_issue("DEMO-2", "Has a branch")],
                   "nextPageToken": "page-2"}),
            (200, {"issues": [jira_issue("DEMO-3", "Has a pull request", status="To Do")], "isLast": True}),
        ]
        self.server.route("POST", SEARCH, pages)
        self.server.json_route("GET", REPO_PATH + "/branches", [
            {"name": "main"}, {"name": "feature/demo-2-export"}, {"name": "vision/DEMO-12"},
        ])
        self.server.json_route("GET", REPO_PATH + "/pulls", [
            {"number": 4, "title": "DEMO-3: add the export", "head": {"ref": "export"}},
        ])

    def test_eligible_and_not_eligible_issues(self):
        result = self.observe({"what": "queue"})
        self.assertEqual(result.code, 0, result)
        tickets = {t["key"]: t for t in result.json["tickets"]}
        self.assertEqual(list(tickets), ["DEMO-1", "DEMO-2", "DEMO-3"])
        self.assertEqual(tickets["DEMO-1"], {"key": "DEMO-1", "title": "Free one", "status": "To Do",
                                             "eligible": True, "reason": ""})
        self.assertFalse(tickets["DEMO-2"]["eligible"])
        self.assertIn("the branch feature/demo-2-export already has DEMO-2", tickets["DEMO-2"]["reason"])
        self.assertFalse(tickets["DEMO-3"]["eligible"])
        self.assertIn("the pull request #4 already has DEMO-3", tickets["DEMO-3"]["reason"])
        # vision/DEMO-12 does not hold DEMO-1: a key matches only as a whole.
        searches = self.server.calls("POST", SEARCH)
        self.assertEqual(len(searches), 2)
        self.assertEqual(searches[0]["body"]["jql"], 'project = DEMO AND status = "To Do"')
        self.assertEqual(searches[0]["body"]["fields"], ["summary", "status"])
        self.assertNotIn("nextPageToken", searches[0]["body"])
        self.assertEqual(searches[1]["body"]["nextPageToken"], "page-2")
        auth = searches[0]["headers"]["authorization"]
        self.assertTrue(auth.startswith("Basic "), auth)
        gh = self.server.calls("GET", REPO_PATH + "/branches")[0]["headers"]
        self.assertEqual(gh["authorization"], "Bearer " + SECRETS["FORGE_TOKEN"])
        self.assertEqual(gh["accept"], "application/vnd.github+json")
        self.assertEqual(gh["x-github-api-version"], "2022-11-28")
        self.assertEqual(self.server.calls("GET", REPO_PATH + "/pulls")[0]["query"]["state"], "open")

    def test_a_missing_key_and_a_missing_secret_are_named(self):
        result = self.observe({"what": "queue", "config": self.config(queue_jql=None)})
        self.assertEqual(result.code, 1)
        self.assertIn("queue_jql", result.stderr)
        secrets = dict(SECRETS)
        del secrets["JIRA_TOKEN"]
        result = self.observe({"what": "queue"}, secrets=secrets)
        self.assertEqual(result.code, 1)
        self.assertIn("JIRA_TOKEN", result.stderr)
        self.assertEqual(self.server.calls(), [], "nothing is called without the secret")

    def test_a_token_never_travels_in_clear_text(self):
        result = self.observe({"what": "queue", "config": self.config(jira_url="http://example.atlassian.net")})
        self.assertEqual(result.code, 1)
        self.assertIn("jira_url", result.stderr)
        self.assertIn("https://", result.stderr)


class Ticket(AdapterTest):
    def setUp(self):
        super().setUp()
        description = {"type": "doc", "version": 1, "content": [
            {"type": "heading", "attrs": {"level": 2}, "content": [{"type": "text", "text": "Goal"}]},
            {"type": "paragraph", "content": [{"type": "text", "text": "Export "},
                                              {"type": "text", "text": "fast", "marks": [{"type": "strong"}]}]},
        ]}
        issue = jira_issue(description=description, status="In Progress",
                           comments=[("Sam", "Please keep CSV")], parent="DEMO-1")
        self.server.json_route("GET", "/rest/api/3/issue/" + KEY, issue)
        self.server.json_route("GET", "/rest/api/3/issue/DEMO-1",
                               {"key": "DEMO-1", "fields": {"summary": "Reports", "description": adf_doc("The epic")}})
        self.world = GitHubWorld(self.server)

    def observe_ticket(self, pr=7, **config):
        result = self.observe({"what": "ticket", "key": KEY, "branch": BRANCH, "pr": pr, "clone": None,
                               "config": self.config(**config)})
        self.assertEqual(result.code, 0, result)
        return result.json

    def test_the_issue_and_the_shape_of_the_answer(self):
        facts = self.observe_ticket()
        self.assertEqual(sorted(facts), ["ci", "envs", "issue", "pr", "review", "tracker_status"])
        issue = facts["issue"]
        self.assertEqual(issue["key"], KEY)
        self.assertEqual(issue["title"], "Export the report")
        self.assertEqual(issue["description"], "## Goal\n\nExport **fast**")
        self.assertEqual(issue["status"], "In Progress")
        self.assertEqual(issue["parent"], {"key": "DEMO-1", "title": "Reports", "description": "The epic"})
        self.assertEqual(issue["comments"][0]["author"], "Sam")
        self.assertEqual(issue["comments"][0]["text"], "Please keep CSV")
        self.assertEqual(facts["tracker_status"], "In Progress")
        pr = facts["pr"]
        for key in ("number", "draft", "head", "merged", "closed", "conflict", "approved_by",
                    "changes_requested", "comments_waiting"):
            self.assertIn(key, pr)
        self.assertEqual((pr["number"], pr["draft"], pr["head"]), (7, True, HEAD))
        self.assertEqual(facts["ci"], {"head": HEAD, "state": "none"})
        self.assertEqual(facts["envs"], {})
        fields = self.server.calls("GET", "/rest/api/3/issue/" + KEY)[0]["query"]["fields"]
        self.assertIn("description", fields.split(","))

    def test_each_review_state(self):
        w = self.world
        # Run each case in order, changing the world between them.
        w.pr, w.reviews, w.comments = pull(draft=False), [], []
        self.assertEqual(self.observe_ticket()["review"]["state"], "waiting")
        w.reviews = [review(1, "outsider", "APPROVED")]
        self.assertEqual(self.observe_ticket()["review"]["state"], "waiting", "only approvers count")
        w.reviews = [review(1, "lead", "APPROVED")]
        facts = self.observe_ticket()
        self.assertEqual(facts["review"]["state"], "approved")
        self.assertEqual(facts["pr"]["approved_by"], ["lead"])
        w.reviews.append(review(2, "lead", "CHANGES_REQUESTED"))
        facts = self.observe_ticket()
        self.assertEqual(facts["review"]["state"], "changes")
        self.assertTrue(facts["pr"]["changes_requested"])
        self.assertEqual(facts["pr"]["approved_by"], [])
        w.pr = pull(draft=False, requested=["lead"])
        self.assertEqual(self.observe_ticket()["review"]["state"], "waiting", "the review is asked again")
        w.comments = [comment(10, "lead", "Rename this"), comment(11, "vision-bot", "Done", reply_to=10),
                      comment(12, "lead", "And this?")]
        facts = self.observe_ticket()
        self.assertEqual(facts["review"]["state"], "comments")
        self.assertEqual(facts["pr"]["comments_waiting"], 1, "the answered thread does not wait")
        w.pr = pull(draft=False, state="closed", merged=True)
        self.assertEqual(self.observe_ticket()["review"]["state"], "merged")
        w.pr = pull(draft=False, state="closed")
        facts = self.observe_ticket()
        self.assertEqual(facts["review"]["state"], "closed")
        self.assertTrue(facts["pr"]["closed"])
        w.pr = pull(draft=False, mergeable_state="dirty")
        w.comments = []
        self.assertTrue(self.observe_ticket()["pr"]["conflict"])

    def test_any_reviewer_counts_when_approvers_is_empty(self):
        self.world.reviews = [review(1, "someone", "APPROVED")]
        facts = self.observe_ticket(approvers=[])
        self.assertEqual(facts["review"]["state"], "approved")
        self.world.reviews = [review(1, "person", "APPROVED")]
        self.assertEqual(self.observe_ticket(approvers=[])["review"]["state"], "waiting", "an own login never counts")

    def test_each_ci_state(self):
        w = self.world
        cases = [
            ([], "none"),
            ([run(1, status="in_progress", conclusion=None)], "running"),
            ([run(1, status="queued", conclusion=None)], "running"),
            ([run(1)], "green"),
            ([run(1, conclusion="failure")], "red"),
            ([run(1, conclusion="cancelled")], "red"),
            ([run(1, conclusion="failure"), run(2)], "green"),
            ([run(1), run(2, path=".github/workflows/lint.yml", conclusion="failure")], "green"),
            ([run(1, sha=OTHER, conclusion="failure")], "none"),
        ]
        for runs, state in cases:
            with self.subTest(runs=runs):
                w.runs = runs
                ci = self.observe_ticket()["ci"]
                self.assertEqual(ci["state"], state)
                self.assertEqual(ci["head"], HEAD)
        runs = self.server.calls("GET", REPO_PATH + "/actions/runs")
        self.assertEqual(runs[-1]["query"]["head_sha"], HEAD)

    def test_no_pull_request_uses_the_branch(self):
        self.world.pr = None
        self.world.runs = [run(1)]
        facts = self.observe_ticket(pr=None)
        self.assertIsNone(facts["pr"])
        self.assertEqual(facts["review"]["state"], "none")
        self.assertEqual(facts["ci"]["state"], "green")
        self.world.branch_sha = None
        facts = self.observe_ticket(pr=None)
        self.assertEqual(facts["ci"], {"head": None, "state": "none"})

    def test_each_env_tells_whether_it_holds_the_head(self):
        self.server.json_route("GET", REPO_PATH + "/git/ref/heads/staging", {"object": {"sha": OTHER}})
        self.server.json_route("GET", REPO_PATH + "/git/ref/heads/demo", {"object": {"sha": HEAD}})
        self.server.json_route("GET", REPO_PATH + "/compare/staging..." + HEAD, {"status": "ahead"})
        self.server.json_route("GET", REPO_PATH + "/compare/demo..." + HEAD, {"status": "identical"})
        envs = [{"name": "staging", "branch": "staging"}, {"name": "demo", "branch": "demo"},
                {"name": "gone", "branch": "gone"}]
        facts = self.observe_ticket(envs=envs)
        self.assertEqual(facts["envs"], {
            "staging": {"branch": "staging", "head": OTHER, "holds": False},
            "demo": {"branch": "demo", "head": HEAD, "holds": True},
            "gone": {"branch": "gone", "head": None, "holds": False},
        })

    def test_an_http_error_names_the_call_and_the_status_never_a_header(self):
        self.server.json_route("GET", "/rest/api/3/issue/" + KEY,
                               {"errorMessages": ["Issue does not exist or you do not have permission to see it."]},
                               status=404)
        result = self.observe({"what": "ticket", "key": KEY, "branch": BRANCH, "pr": 7})
        self.assertEqual(result.code, 1)
        self.assertIn("jira: read the issue DEMO-12 (GET /rest/api/3/issue/DEMO-12) answered HTTP 404", result.stderr)
        self.assertIn("Issue does not exist", result.stderr)
        self.assertNotIn("Authorization", result.stderr)
        self.assertNotIn("Basic", result.stderr)

    def test_each_http_call_has_a_time_limit(self):
        self.server.route("GET", "/rest/api/3/issue/" + KEY, lambda r: (200, {}, {"X-Fake-Delay": "3"}))
        result = self.observe({"what": "ticket", "key": KEY, "branch": BRANCH, "pr": 7,
                               "config": self.config(http_timeout=1)})
        self.assertEqual(result.code, 1)
        self.assertIn("read the issue DEMO-12", result.stderr)
        self.assertIn("in 1 s", result.stderr)

    def test_a_bad_key_never_reaches_a_request(self):
        result = self.observe({"what": "ticket", "key": "DEMO-1/../x", "branch": BRANCH})
        self.assertEqual(result.code, 1)
        self.assertEqual(self.server.calls(), [])


if __name__ == "__main__":
    unittest.main()
