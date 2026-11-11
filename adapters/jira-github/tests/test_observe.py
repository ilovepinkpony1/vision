"""bin/observe: the queue, and one ticket with each review state and each CI state."""

import unittest

from fixtures import (HEAD, OTHER, REPO_PATH, GitHubWorld, adf_doc, comment, issue_comment, jira_issue, pull, review,
                      review_with, run)
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
        facts = self.observe_ticket()
        self.assertTrue(facts["pr"]["conflict"])
        self.assertEqual(facts["review"]["state"], "changes", "the changes of lead come before the conflict")

    def test_a_conflict_with_the_base_is_its_own_state_in_its_order(self):
        w = self.world
        # merged, closed, comments, changes, conflict, approved, waiting
        w.pr, w.reviews, w.comments = pull(draft=False, mergeable_state="dirty"), [], []
        facts = self.observe_ticket()
        self.assertTrue(facts["pr"]["conflict"])
        self.assertEqual(facts["review"]["state"], "conflict")
        w.reviews = [review(1, "lead", "APPROVED")]
        self.assertEqual(self.observe_ticket()["review"]["state"], "conflict", "a conflict comes before approved")
        w.reviews.append(review(2, "lead", "CHANGES_REQUESTED"))
        self.assertEqual(self.observe_ticket()["review"]["state"], "changes", "changes come before a conflict")
        w.reviews = [review(1, "lead", "APPROVED")]
        w.comments = [comment(10, "lead", "Rename this")]
        self.assertEqual(self.observe_ticket()["review"]["state"], "comments", "a comment comes before a conflict")
        w.comments = []
        w.pr = pull(draft=False, mergeable=False)
        self.assertEqual(self.observe_ticket()["review"]["state"], "conflict", "mergeable false is a conflict")
        w.pr = pull(draft=False, mergeable_state="dirty", state="closed", merged=True)
        self.assertEqual(self.observe_ticket()["review"]["state"], "merged")
        w.pr = pull(draft=False, mergeable_state="dirty", state="closed")
        self.assertEqual(self.observe_ticket()["review"]["state"], "closed")
        # GitHub has not computed it yet: no conflict.
        w.pr = pull(draft=False, mergeable=None, mergeable_state="unknown")
        facts = self.observe_ticket()
        self.assertFalse(facts["pr"]["conflict"])
        self.assertEqual(facts["review"]["state"], "approved")

    def test_each_kind_of_comment_waits_and_the_state_agrees(self):
        w = self.world
        w.pr, w.reviews, w.comments = pull(draft=False), [review(1, "lead", "APPROVED")], []
        self.assertEqual(self.observe_ticket()["review"]["state"], "approved")
        w.issue_comments = [issue_comment(60, "lead", "One more thing: the README.")]
        facts = self.observe_ticket()
        self.assertEqual(facts["review"]["state"], "comments", "a conversation comment waits")
        self.assertEqual(facts["pr"]["comments_waiting"], 1)
        w.reviews.append(review_with(2, "other", "COMMENTED", "Why CSV?"))
        self.assertEqual(self.observe_ticket()["pr"]["comments_waiting"], 2, "a review with a body waits")
        w.issue_comments += [
            issue_comment(61, "vision-bot", "> One more thing: the README.\n\nDone.\n\n<!-- vision-reply conversation 60 -->"),
            issue_comment(62, "person", "> Why CSV?\n\nThe ticket asks for it."),
        ]
        facts = self.observe_ticket()
        self.assertEqual(facts["pr"]["comments_waiting"], 0, "a mark or a quote answers each one")
        self.assertEqual(facts["review"]["state"], "approved")

    def test_a_comment_of_an_ignored_login_does_not_change_the_state(self):
        w = self.world
        w.pr, w.reviews, w.comments = pull(draft=False), [review(1, "lead", "APPROVED")], []
        w.issue_comments = [issue_comment(60, "ci-bot", "Deployed the preview.")]
        self.assertEqual(self.observe_ticket()["review"]["state"], "comments")
        facts = self.observe_ticket(ignore_logins=["ci-bot"])
        self.assertEqual(facts["pr"]["comments_waiting"], 0)
        self.assertEqual(facts["review"]["state"], "approved")
        # An entry with a kind: the status comment never waits, and a review of the bot still does.
        facts = self.observe_ticket(ignore_logins=["ci-bot:conversation"])
        self.assertEqual(facts["review"]["state"], "approved")
        w.reviews.append(review_with(2, "ci-bot", "COMMENTED", "A null check is missing."))
        facts = self.observe_ticket(ignore_logins=["ci-bot:conversation"])
        self.assertEqual(facts["pr"]["comments_waiting"], 1)
        self.assertEqual(facts["review"]["state"], "comments")
        result = self.observe({"what": "ticket", "key": KEY, "branch": BRANCH, "pr": 7,
                               "config": self.config(ignore_logins=["ci-bot:other"])})
        self.assertEqual(result.code, 1)
        self.assertIn("inline, review, or conversation", result.stderr)

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

    def test_the_deployment_of_an_env_tells_whether_the_env_runs_the_head(self):
        deployments = REPO_PATH + "/deployments"
        for branch in ("staging", "demo"):
            self.server.json_route("GET", REPO_PATH + "/git/ref/heads/" + branch, {"object": {"sha": HEAD}})
            self.server.json_route("GET", REPO_PATH + "/compare/%s...%s" % (branch, HEAD), {"status": "identical"})
        envs = [{"name": "staging", "branch": "staging", "deployment": "staging-env"},
                {"name": "demo", "branch": "demo"}]

        # No deployment of the environment yet.
        self.server.json_route("GET", deployments, [])
        facts = self.observe_ticket(envs=envs)["envs"]
        self.assertEqual(facts["staging"], {"branch": "staging", "head": HEAD, "holds": True, "deployed": False,
                                            "deployment_sha": None, "deployed_at": None})
        self.assertEqual(facts["demo"], {"branch": "demo", "head": HEAD, "holds": True},
                         "an env with no deployment key gets no new fact")
        calls = self.server.calls("GET", deployments)
        self.assertEqual(len(calls), 1, "one call for the one env with a deployment")
        self.assertEqual(calls[0]["query"], {"environment": "staging-env", "per_page": "1"})

        # The newest deployment runs an older commit: the env does not run the head yet.
        self.server.json_route("GET", deployments, [
            {"id": 3, "sha": OTHER, "environment": "staging-env", "created_at": "2026-11-02T10:00:00Z"}])
        self.server.json_route("GET", REPO_PATH + "/compare/%s...%s" % (OTHER, HEAD), {"status": "ahead"})
        staging = self.observe_ticket(envs=envs)["envs"]["staging"]
        self.assertEqual((staging["deployed"], staging["deployment_sha"], staging["deployed_at"]),
                         (False, OTHER, "2026-11-02T10:00:00Z"))

        # A newer commit that holds the head, and then the head itself, with no compare.
        newer = "c" * 40
        self.server.json_route("GET", deployments, [
            {"id": 4, "sha": newer, "environment": "staging-env", "created_at": "2026-11-03T10:00:00Z"}])
        self.server.json_route("GET", REPO_PATH + "/compare/%s...%s" % (newer, HEAD), {"status": "behind"})
        staging = self.observe_ticket(envs=envs)["envs"]["staging"]
        self.assertEqual((staging["deployed"], staging["deployment_sha"]), (True, newer))
        self.server.json_route("GET", deployments, [
            {"id": 5, "sha": HEAD, "environment": "staging-env", "created_at": "2026-11-04T10:00:00Z"}])
        compares = len(self.server.calls("GET", REPO_PATH + "/compare/%s...%s" % (HEAD, HEAD)))
        staging = self.observe_ticket(envs=envs)["envs"]["staging"]
        self.assertEqual((staging["deployed"], staging["deployed_at"]), (True, "2026-11-04T10:00:00Z"))
        self.assertEqual(len(self.server.calls("GET", REPO_PATH + "/compare/%s...%s" % (HEAD, HEAD))), compares)
        self.assertFalse([c for c in self.server.calls() if "/statuses" in c["path"]],
                         "the statuses of a deployment are not read")
        self.assertEqual(len(self.server.calls("GET", deployments)), 4, "one call for each look")

        # A bad deployment key is named.
        result = self.observe({"what": "ticket", "key": KEY, "branch": BRANCH, "pr": 7,
                               "config": self.config(envs=[{"name": "staging", "branch": "staging", "deployment": 3}])})
        self.assertEqual(result.code, 1)
        self.assertIn("`deployment` of the env staging", result.stderr)

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
