"""bin/act: tracker.describe, tracker.move, git.push, pr.open, ci.run, review.reply, and the
reconcile form of each one."""

import json
import os
import unittest

from fixtures import HEAD, OTHER, REPO_PATH, GitHubWorld, adf_doc, comment, jira_issue, pull, review, run
from support import BRANCH, KEY, SECRETS, AdapterTest, Repos, header_hash, read_calls, secret_forms

from jgadapter import adf

ISSUE = "/rest/api/3/issue/" + KEY
TRANSITIONS = ISSUE + "/transitions"


def issue_json(description):
    """issue.json as the pipeline writes it from the observe answer."""
    return json.dumps({"key": KEY, "tracker_status": "To Do",
                       "issue": {"untrusted": {"key": KEY, "description": description}}}, indent=2)


class TrackerDescribe(AdapterTest):
    NEW = "## Problem\n\nThe export is **slow**.\n\n- one\n- two"

    def setUp(self):
        super().setUp()
        self.description = adf_doc("The old text")
        self.server.route("GET", ISSUE, lambda r: (200, jira_issue(description=self.description)))

        def put(req):
            self.description = req["body"]["fields"]["description"]
            return 204, None
        self.server.route("PUT", ISSUE, put)

    def test_it_replaces_the_description_and_gives_the_backup(self):
        result = self.act("tracker.describe", {"text": self.NEW, "seen": issue_json("The old text")})
        self.assertEqual(result.code, 0, result)
        self.assertEqual(result.json["state"], "confirmed", result)
        puts = self.server.calls("PUT", ISSUE)
        self.assertEqual(len(puts), 1)
        self.assertEqual(puts[0]["body"], {"fields": {"description": adf.to_adf(self.NEW)}})
        docs = result.json["documents"]
        self.assertEqual(len(docs), 1)
        self.assertRegex(docs[0]["name"], r"^description-backup-DEMO-12-\d{8}T\d{6}Z\.json$")
        backup = json.loads(docs[0]["content"])
        self.assertEqual(backup["adf"], adf_doc("The old text"))
        self.assertEqual(backup["markdown"], "The old text")
        self.assertIn(docs[0]["name"], result.json["detail"])
        self.assertIn("The old text", result.json["detail"])
        # A second call finds the new text and changes nothing.
        again = self.act("tracker.describe", {"text": self.NEW, "seen": issue_json("The old text")})
        self.assertEqual(again.json["state"], "confirmed")
        self.assertEqual(len(self.server.calls("PUT", ISSUE)), 1)

    def test_it_refuses_after_a_person_edited_the_description(self):
        result = self.act("tracker.describe", {"text": self.NEW, "seen": issue_json("An older text")})
        self.assertEqual(result.code, 2, result)
        self.assertIn("a person edited the description of DEMO-12", result.json["detail"])
        self.assertEqual(self.server.calls("PUT", ISSUE), [])

    def test_the_reconcile_only_reads(self):
        result = self.act("tracker.describe", {"text": self.NEW, "seen": issue_json("The old text")}, reconcile=True)
        self.assertEqual(result.json["state"], "failed")
        self.description = adf.to_adf(self.NEW)
        result = self.act("tracker.describe", {"text": self.NEW, "seen": issue_json("The old text")}, reconcile=True)
        self.assertEqual(result.json["state"], "confirmed")
        self.assertEqual(self.server.calls("PUT", ISSUE), [])

    def test_missing_arguments_keys_and_secrets_are_named(self):
        result = self.act("tracker.describe", {"text": self.NEW})
        self.assertEqual(result.json["state"], "failed")
        self.assertIn("args.seen", result.json["detail"])
        result = self.act("tracker.describe", {"text": self.NEW, "seen": issue_json("x")},
                          config=self.config(jira_user=None))
        self.assertEqual(result.json["state"], "failed")
        self.assertIn("jira_user", result.json["detail"])
        secrets = dict(SECRETS)
        del secrets["JIRA_TOKEN"]
        result = self.act("tracker.describe", {"text": self.NEW, "seen": issue_json("x")}, secrets=secrets)
        self.assertEqual(result.json["state"], "failed")
        self.assertIn("JIRA_TOKEN", result.json["detail"])
        self.assertEqual(self.server.calls("PUT", ISSUE), [])

    def test_a_server_error_after_the_change_is_uncertain_and_a_refusal_is_failed(self):
        self.server.json_route("PUT", ISSUE, {"message": "boom"}, status=502)
        result = self.act("tracker.describe", {"text": self.NEW, "seen": issue_json("The old text")})
        self.assertEqual(result.json["state"], "uncertain", result)
        self.assertIn("HTTP 502", result.json["detail"])
        self.server.json_route("PUT", ISSUE, {"errors": {"description": "Operation value must be an ADF document"}},
                               status=400)
        result = self.act("tracker.describe", {"text": self.NEW, "seen": issue_json("The old text")})
        self.assertEqual(result.json["state"], "failed", result)
        self.assertIn("HTTP 400: description: Operation value must be an ADF document", result.json["detail"])


class TrackerMove(AdapterTest):
    def setUp(self):
        super().setUp()
        self.status = "To Do"
        self.server.route("GET", ISSUE, lambda r: (200, jira_issue(status=self.status)))
        self.server.json_route("GET", TRANSITIONS, {"transitions": [
            {"id": "11", "name": "Start", "to": {"name": "In Progress"}},
            {"id": "21", "name": "Block", "to": {"name": "Blocked"}},
        ]})

        def post(req):
            self.status = {"11": "In Progress", "21": "Blocked"}[req["body"]["transition"]["id"]]
            return 204, None
        self.server.route("POST", TRANSITIONS, post)

    def test_a_transition_leads_there(self):
        result = self.act("tracker.move", {"status": "in_progress"})
        self.assertEqual(result.json["state"], "confirmed", result)
        self.assertEqual(self.server.calls("POST", TRANSITIONS)[0]["body"], {"transition": {"id": "11"}})
        self.assertIn("moved from To Do to In Progress", result.json["detail"])

    def test_the_status_already_holds(self):
        self.status = "in progress"
        result = self.act("tracker.move", {"status": "in_progress"})
        self.assertEqual(result.json["state"], "confirmed")
        self.assertEqual(self.server.calls("POST", TRANSITIONS), [])

    def test_no_transition_leads_there(self):
        result = self.act("tracker.move", {"status": "in_review"})
        self.assertEqual(result.json["state"], "failed")
        self.assertIn("no transition of DEMO-12 leads from To Do to In Review", result.json["detail"])
        self.assertIn("Start -> In Progress; Block -> Blocked", result.json["detail"])
        self.assertEqual(self.server.calls("POST", TRANSITIONS), [])

    def test_the_reconcile_only_reads(self):
        self.assertEqual(self.act("tracker.move", {"status": "in_progress"}, reconcile=True).json["state"], "failed")
        self.status = "In Progress"
        self.assertEqual(self.act("tracker.move", {"status": "in_progress"}, reconcile=True).json["state"], "confirmed")
        self.assertEqual(self.server.calls("POST", TRANSITIONS), [])
        self.assertEqual(self.server.calls("GET", TRANSITIONS), [])

    def test_an_unknown_role_names_the_key(self):
        result = self.act("tracker.move", {"status": "shipped"})
        self.assertEqual(result.json["state"], "failed")
        self.assertIn("statuses.shipped", result.json["detail"])


class GitPush(AdapterTest):
    def setUp(self):
        super().setUp()
        self.repos = Repos(os.path.join(self.tmp, "repos"))
        self.log = self.git_spy()

    def push(self, reconcile=False, args=None, **fields):
        request = {"clone": self.repos.clone, "head": self.repos.head}
        request.update(fields)
        return self.act("git.push", args or {}, reconcile=reconcile,
                        config=self.config(git_remote_url=self.repos.remote), **request)

    def test_the_bare_repo_gets_the_branch_with_no_force_and_no_token_in_a_file(self):
        result = self.push()
        self.assertEqual(result.json["state"], "confirmed", result)
        self.assertEqual(result.json["set"], {"head": self.repos.head})
        self.assertEqual(self.repos.remote_head(BRANCH), self.repos.head)
        calls = read_calls(self.log)
        pushes = [c for c in calls if "push" in c["args"]]
        self.assertEqual(len(pushes), 1)
        push = pushes[0]["args"]
        for flag in ("--force", "-f", "--force-with-lease", "--mirror", "--delete", "--prune"):
            self.assertNotIn(flag, push)
        self.assertEqual(push[-1], "refs/heads/%s:refs/heads/%s" % (BRANCH, BRANCH), "no + in the refspec")
        self.assertIn("core.hooksPath=/dev/null", push)
        config = pushes[0]["config"]
        self.assertIn("http.followRedirects=false", config, "a redirect never takes the header elsewhere")
        header = "http.%s.extraheader=%s" % (self.repos.remote, header_hash(SECRETS["FORGE_TOKEN"]))
        self.assertIn(header, config, "the token goes as one header value in the environment of the call")
        for call in calls:
            self.assertIn("core.hooksPath=/dev/null", call["args"])
            self.assertFalse(set(call["env"]) & set(SECRETS), "git never gets a secret under its own name")
            # A command line is visible to each user of the machine, so the header is never on it.
            self.assertFalse([a for a in call["args"] if "extraheader" in a or "Authorization" in a], call["args"])
            for form in secret_forms():
                self.assertFalse([a for a in call["args"] if form in a], "a secret is on a command line")
        self.assert_no_secret_in_files(self.repos.clone, self.repos.remote, self.home, self.scratch)
        with open(os.path.join(self.repos.clone, ".git", "config")) as handle:
            self.assertNotIn("extraheader", handle.read())
        self.assertEqual(os.listdir(self.scratch), [], "the temporary push repo is gone")

    def test_a_second_push_and_the_reconcile_only_read(self):
        self.assertEqual(self.push(reconcile=True).json["state"], "failed")
        self.assertEqual(self.push().json["state"], "confirmed")
        again = self.push()
        self.assertEqual(again.json["state"], "confirmed")
        self.assertEqual(self.push(reconcile=True).json["state"], "confirmed")
        pushes = [c for c in read_calls(self.log) if "push" in c["args"]]
        self.assertEqual(len(pushes), 1)

    def test_it_refuses_the_base_branch_another_branch_and_force(self):
        cases = [
            ({"branch": "main"}, "the base branch"),
            ({"args": {"branch": "vision/DEMO-13"}}, "only the branch of the call"),
            ({"args": {"force": True}}, "never uses force"),
            ({"branch": "feature/other"}, "does not hold the key DEMO-12"),
            ({"branch": "-vision/DEMO-12"}, "not a branch name"),
        ]
        for fields, text in cases:
            with self.subTest(fields):
                result = self.push(**fields)
                self.assertEqual(result.code, 2, result)
                self.assertIn(text, result.json["detail"])
        self.assertEqual(read_calls(self.log), [], "a refusal runs no git")
        self.assertIsNone(self.repos.remote_head(BRANCH))

    def test_a_branch_that_moved_on_the_remote_is_never_forced(self):
        self.assertEqual(self.push().json["state"], "confirmed")
        other = os.path.join(self.tmp, "other")
        from support import git
        git(["clone", "--quiet", "--branch", BRANCH, self.repos.remote, other], self.tmp)
        with open(os.path.join(other, "x.txt"), "w") as handle:
            handle.write("x\n")
        git(["add", "-A"], other)
        git(["commit", "--quiet", "-m", "someone else"], other)
        git(["push", "--quiet", "origin", BRANCH], other)
        theirs = self.repos.remote_head(BRANCH)
        self.repos.write({"y.txt": "y\n"})
        mine = self.repos.commit("mine")
        result = self.push(head=mine)
        self.assertEqual(result.json["state"], "failed", result)
        self.assertIn("never forces a push", result.json["detail"])
        self.assertEqual(self.repos.remote_head(BRANCH), theirs)

    def test_the_clone_must_be_on_the_branch_of_the_call(self):
        from support import git
        git(["checkout", "--quiet", "main"], self.repos.clone)
        result = self.push()
        self.assertEqual(result.code, 2, result)
        self.assertIn("the clone is on 'main'", result.json["detail"])

    def test_the_remote_must_be_https_or_local(self):
        result = self.act("git.push", {}, clone=self.repos.clone, head=self.repos.head,
                          config=self.config(git_remote_url="http://example.com/acme/widgets.git"))
        self.assertEqual(result.json["state"], "failed")
        self.assertIn("git_remote_url", result.json["detail"])


class PrOpen(AdapterTest):
    TEXT = "# Export the report\n\nThe export now makes a CSV file.\n\nCloses DEMO-12."

    def setUp(self):
        super().setUp()
        self.world = GitHubWorld(self.server, pr={})
        self.world.pr = None

        def create(req):
            self.world.pr = pull(number=7, sha=HEAD)
            return 201, self.world.pr
        self.server.route("POST", REPO_PATH + "/pulls", create)

    def test_a_new_draft_pull_request_with_the_token_of_the_person(self):
        result = self.act("pr.open", {"text": self.TEXT}, head=HEAD)
        self.assertEqual(result.json["state"], "confirmed", result)
        self.assertEqual(result.json["set"], {"pr": 7, "head": HEAD})
        self.assertEqual(result.json["external_id"], "7")
        post = self.server.calls("POST", REPO_PATH + "/pulls")[0]
        self.assertEqual(post["body"], {"title": "Export the report", "head": BRANCH, "base": "main",
                                        "body": "The export now makes a CSV file.\n\nCloses DEMO-12.", "draft": True})
        self.assertEqual(post["headers"]["authorization"], "Bearer " + SECRETS["PR_TOKEN"])

    def test_without_the_token_of_the_person_the_forge_token_opens_it(self):
        secrets = dict(SECRETS)
        del secrets["PR_TOKEN"]
        result = self.act("pr.open", {"text": self.TEXT}, secrets=secrets)
        self.assertEqual(result.json["state"], "confirmed", result)
        post = self.server.calls("POST", REPO_PATH + "/pulls")[0]
        self.assertEqual(post["headers"]["authorization"], "Bearer " + SECRETS["FORGE_TOKEN"])

    def test_an_existing_pull_request_is_returned(self):
        self.world.pr = pull(number=7)
        result = self.act("pr.open", {"text": self.TEXT})
        self.assertEqual(result.json["state"], "confirmed")
        self.assertEqual(result.json["set"]["pr"], 7)
        self.assertEqual(self.server.calls("POST", REPO_PATH + "/pulls"), [])

    def test_the_reconcile_only_reads(self):
        self.assertEqual(self.act("pr.open", {"text": self.TEXT}, reconcile=True).json["state"], "failed")
        self.world.pr = pull(number=7)
        self.assertEqual(self.act("pr.open", {"text": self.TEXT}, reconcile=True).json["state"], "confirmed")
        self.assertEqual(self.server.calls("POST", REPO_PATH + "/pulls"), [])

    def test_a_refused_pull_request_is_failed(self):
        self.server.json_route("POST", REPO_PATH + "/pulls",
                               {"message": "Validation Failed", "errors": [{"message": "No commits between main and x"}]},
                               status=422)
        result = self.act("pr.open", {"text": self.TEXT})
        self.assertEqual(result.json["state"], "failed", result)
        self.assertIn("HTTP 422", result.json["detail"])
        self.assertIn("No commits between", result.json["detail"])


class CiRun(AdapterTest):
    DISPATCH = REPO_PATH + "/actions/workflows/ci.yml/dispatches"

    def setUp(self):
        super().setUp()
        self.world = GitHubWorld(self.server)
        self.server.route("POST", self.DISPATCH, self.dispatch)
        self.start_run = True

    def dispatch(self, req):
        if self.start_run:
            self.world.runs.append(run(41, status="queued", conclusion=None))
        return 204, None

    def test_the_new_run_shows_up(self):
        result = self.act("ci.run", head=HEAD)
        self.assertEqual(result.json["state"], "confirmed", result)
        self.assertEqual(result.json["external_id"], "run-41")
        self.assertEqual(self.server.calls("POST", self.DISPATCH)[0]["body"], {"ref": BRANCH})

    def test_no_run_shows_up(self):
        self.start_run = False
        result = self.act("ci.run", head=HEAD, config=self.config(ci_find_seconds=1))
        self.assertEqual(result.json["state"], "uncertain", result)
        self.assertIn("no run at", result.json["detail"])
        self.assertEqual(len(self.server.calls("POST", self.DISPATCH)), 1)

    def test_a_run_of_the_head_is_used_and_nothing_starts(self):
        self.world.runs = [run(40, conclusion="failure")]
        result = self.act("ci.run", head=HEAD)
        self.assertEqual(result.json["state"], "confirmed")
        self.assertEqual(result.json["external_id"], "run-40")
        self.assertEqual(self.server.calls("POST", self.DISPATCH), [])

    def test_a_branch_that_is_not_pushed_is_failed(self):
        self.world.branch_sha = OTHER
        result = self.act("ci.run", head=HEAD)
        self.assertEqual(result.json["state"], "failed")
        self.assertIn("push it first", result.json["detail"])
        self.assertEqual(self.server.calls("POST", self.DISPATCH), [])

    def test_the_reconcile_only_reads(self):
        self.assertEqual(self.act("ci.run", head=HEAD, reconcile=True).json["state"], "uncertain")
        self.world.runs = [run(41)]
        self.assertEqual(self.act("ci.run", head=HEAD, reconcile=True).json["state"], "confirmed")
        self.assertEqual(self.server.calls("POST", self.DISPATCH), [])

    def test_a_workflow_with_no_dispatch_trigger_is_failed(self):
        self.server.json_route("POST", self.DISPATCH, {"message": "Workflow does not have 'workflow_dispatch' trigger"},
                               status=422)
        result = self.act("ci.run", head=HEAD)
        self.assertEqual(result.json["state"], "failed")
        self.assertIn("workflow_dispatch", result.json["detail"])


class ReviewReply(AdapterTest):
    def setUp(self):
        super().setUp()
        self.world = GitHubWorld(self.server, pr=pull(draft=False))
        self.world.comments = [
            comment(10, "lead", "Rename this"),
            comment(20, "lead", "Why a loop?"),
            comment(21, "lead", "Still?", reply_to=20),
        ]
        self.world.add_reply_route()
        self.verdicts = json.dumps([
            {"comment_id": 10, "verdict": "valid", "fix_commit": "abc1234", "reply": "Renamed in abc1234."},
            {"comment_id": "21", "verdict": "incorrect", "fix_commit": None, "reply": "The loop reads one page."},
            {"comment_id": 30, "verdict": "question", "fix_commit": None, "reply": ""},
        ])

    def test_two_runs_post_each_reply_once(self):
        result = self.act("review.reply", {"verdicts": self.verdicts}, pr=7)
        self.assertEqual(result.json["state"], "confirmed", result)
        posts = [c for c in self.server.calls("POST") if c["path"].endswith("/replies")]
        self.assertEqual([(p["path"], p["body"]) for p in posts], [
            (REPO_PATH + "/pulls/7/comments/10/replies", {"body": "Renamed in abc1234."}),
            (REPO_PATH + "/pulls/7/comments/20/replies", {"body": "The loop reads one page."}),
        ], "a reply to a reply goes to the root of its thread")
        again = self.act("review.reply", {"verdicts": self.verdicts}, pr=7)
        self.assertEqual(again.json["state"], "confirmed", again)
        self.assertIn("skipped [10, 21]", again.json["detail"])
        self.assertEqual(len([c for c in self.server.calls("POST") if c["path"].endswith("/replies")]), 2)

    def test_the_reconcile_only_reads(self):
        self.assertEqual(self.act("review.reply", {"verdicts": self.verdicts}, pr=7, reconcile=True).json["state"], "failed")
        self.act("review.reply", {"verdicts": self.verdicts}, pr=7)
        result = self.act("review.reply", {"verdicts": self.verdicts}, pr=7, reconcile=True)
        self.assertEqual(result.json["state"], "confirmed")
        self.assertEqual(len([c for c in self.server.calls("POST") if c["path"].endswith("/replies")]), 2)

    def test_a_missing_comment_is_failed_after_the_others(self):
        verdicts = json.dumps([{"comment_id": 10, "verdict": "valid", "reply": "Done."},
                               {"comment_id": 99, "verdict": "valid", "reply": "Done too."}])
        result = self.act("review.reply", {"verdicts": verdicts}, pr=7)
        self.assertEqual(result.json["state"], "failed")
        self.assertIn("no comment [99]", result.json["detail"])

    def test_own_logins_is_needed(self):
        result = self.act("review.reply", {"verdicts": self.verdicts}, pr=7, config=self.config(own_logins=None))
        self.assertEqual(result.json["state"], "failed")
        self.assertIn("own_logins", result.json["detail"])

    def test_verdicts_in_a_code_fence_and_bad_rows(self):
        fenced = "```json\n" + self.verdicts + "\n```"
        self.assertEqual(self.act("review.reply", {"verdicts": fenced}, pr=7).json["state"], "confirmed")
        bad = json.dumps([{"verdict": "valid", "reply": "x"}])
        result = self.act("review.reply", {"verdicts": bad}, pr=7)
        self.assertEqual(result.json["state"], "failed")
        self.assertIn("comment_id", result.json["detail"])


class CiRerun(AdapterTest):
    RERUN = REPO_PATH + "/actions/runs/41/rerun-failed-jobs"

    def setUp(self):
        super().setUp()
        self.world = GitHubWorld(self.server)
        self.world.runs = [run(41, conclusion="failure")]
        self.starts = True
        self.server.route("GET", REPO_PATH + "/actions/runs/41", lambda r: (200, self.world.runs[0]))

        def rerun(req):
            if self.starts:
                self.world.runs[0] = run(41, status="queued", conclusion=None, attempt=2)
            return 201, {}
        self.server.route("POST", self.RERUN, rerun)

    def test_the_failed_jobs_run_again(self):
        result = self.act("ci.rerun", head=HEAD)
        self.assertEqual(result.json["state"], "confirmed", result)
        self.assertIn("attempt 2", result.json["detail"])
        self.assertEqual(result.json["external_id"], "run-41")
        self.assertEqual(len(self.server.calls("POST", self.RERUN)), 1)

    def test_a_green_or_running_run_needs_nothing(self):
        for state in (run(41), run(41, status="in_progress", conclusion=None)):
            self.world.runs = [state]
            self.assertEqual(self.act("ci.rerun", head=HEAD).json["state"], "confirmed")
        self.assertEqual(self.server.calls("POST", self.RERUN), [])

    def test_no_run_and_a_rerun_that_does_not_start(self):
        self.world.runs = []
        result = self.act("ci.rerun", head=HEAD)
        self.assertEqual(result.json["state"], "failed")
        self.assertIn("nothing runs again", result.json["detail"])
        self.world.runs = [run(41, conclusion="failure")]
        self.starts = False
        result = self.act("ci.rerun", head=HEAD, config=self.config(ci_find_seconds=1))
        self.assertEqual(result.json["state"], "uncertain", result)

    def test_the_reconcile_only_reads(self):
        self.assertEqual(self.act("ci.rerun", head=HEAD, reconcile=True).json["state"], "failed")
        self.world.runs = [run(41, conclusion="failure", attempt=2)]
        self.assertEqual(self.act("ci.rerun", head=HEAD, reconcile=True).json["state"], "confirmed")
        self.assertEqual(self.server.calls("POST", self.RERUN), [])


class Deploy(AdapterTest):
    MERGES = REPO_PATH + "/merges"
    COMPARE = REPO_PATH + "/compare/staging..." + HEAD

    def setUp(self):
        super().setUp()
        self.status = "diverged"
        self.server.route("GET", self.COMPARE, lambda r: (200, {"status": self.status}))
        self.merge = (201, {"sha": "c" * 40})
        self.server.route("POST", self.MERGES, lambda r: self.merge)

    def deploy(self, env="staging", reconcile=False, **config):
        settings = dict({"envs": [{"name": "staging", "branch": "staging"}, {"name": "prod", "branch": "main"}]}, **config)
        return self.act("deploy", {"env": env}, reconcile=reconcile, head=HEAD, config=self.config(**settings))

    def test_201_merges_the_head(self):
        result = self.deploy()
        self.assertEqual(result.json["state"], "confirmed", result)
        self.assertIn("(201)", result.json["detail"])
        post = self.server.calls("POST", self.MERGES)[0]
        self.assertEqual(post["body"], {"base": "staging", "head": HEAD,
                                        "commit_message": "Merge DEMO-12 (%s) into staging" % HEAD[:12]})

    def test_204_means_the_branch_holds_the_head(self):
        self.merge = (204, None)
        result = self.deploy()
        self.assertEqual(result.json["state"], "confirmed")
        self.assertIn("(204)", result.json["detail"])

    def test_409_is_a_conflict(self):
        self.merge = (409, {"message": "Merge conflict"})
        result = self.deploy()
        self.assertEqual(result.json["state"], "failed")
        self.assertIn("conflict", result.json["detail"])

    def test_a_branch_that_holds_the_head_and_the_reconcile(self):
        self.assertEqual(self.deploy(reconcile=True).json["state"], "failed")
        self.status = "behind"
        self.assertEqual(self.deploy().json["state"], "confirmed")
        self.assertEqual(self.deploy(reconcile=True).json["state"], "confirmed")
        self.assertEqual(self.server.calls("POST", self.MERGES), [])

    def test_an_unknown_env_and_the_base_branch(self):
        result = self.deploy(env="qa")
        self.assertEqual(result.json["state"], "failed")
        self.assertIn("the env qa in `envs`", result.json["detail"])
        result = self.deploy(env="prod")
        self.assertEqual(result.code, 2, result)
        self.assertIn("only into a staging branch", result.json["detail"])
        self.assertEqual(self.server.calls("POST", self.MERGES), [])


class ReviewRerequest(AdapterTest):
    REQUEST = REPO_PATH + "/pulls/7/requested_reviewers"

    def setUp(self):
        super().setUp()
        self.world = GitHubWorld(self.server, pr=pull(draft=False))
        self.world.reviews = [review(1, "lead", "APPROVED"), review(2, "lead", "CHANGES_REQUESTED"),
                              review(3, "outsider", "CHANGES_REQUESTED")]

        def post(req):
            self.world.pr = pull(draft=False, requested=req["body"]["reviewers"])
            return 201, self.world.pr
        self.server.route("POST", self.REQUEST, post)

    def test_it_asks_each_approver_who_asked_for_changes_once(self):
        result = self.act("review.rerequest", pr=7)
        self.assertEqual(result.json["state"], "confirmed", result)
        self.assertEqual(self.server.calls("POST", self.REQUEST)[0]["body"], {"reviewers": ["lead"]})
        again = self.act("review.rerequest", pr=7)
        self.assertEqual(again.json["state"], "confirmed")
        self.assertEqual(len(self.server.calls("POST", self.REQUEST)), 1)

    def test_no_approver_asked_for_changes(self):
        self.world.reviews = [review(1, "lead", "APPROVED")]
        result = self.act("review.rerequest", pr=7)
        self.assertEqual(result.json["state"], "confirmed")
        self.assertEqual(self.server.calls("POST", self.REQUEST), [])

    def test_the_reconcile_only_reads(self):
        self.assertEqual(self.act("review.rerequest", pr=7, reconcile=True).json["state"], "failed")
        self.world.pr = pull(draft=False, requested=["lead"])
        self.assertEqual(self.act("review.rerequest", pr=7, reconcile=True).json["state"], "confirmed")
        self.assertEqual(self.server.calls("POST", self.REQUEST), [])


class Reads(AdapterTest):
    def setUp(self):
        super().setUp()
        self.server.json_route("GET", ISSUE, jira_issue(description=adf_doc("Make it **work**"), comments=[("Sam", "ok")]))
        self.server.json_route("POST", "/rest/api/3/search/jql", {"issues": [jira_issue("DEMO-3", "Other")], "isLast": True})
        self.world = GitHubWorld(self.server, pr=pull(draft=False))
        self.world.reviews = [review(1, "lead", "COMMENTED")]
        self.world.comments = [comment(10, "lead", "Why?")]
        self.world.runs = [run(41, conclusion="failure")]
        self.server.json_route("GET", REPO_PATH + "/actions/runs/41", run(41, conclusion="failure"))
        self.server.json_route("GET", REPO_PATH + "/actions/runs/41/jobs", {"jobs": []})

    def doc(self, result):
        self.assertEqual(result.code, 0, result)
        docs = result.json["documents"]
        self.assertEqual(len(docs), 1)
        return docs[0]["name"], docs[0]["content"]

    def test_jira_issue(self):
        name, content = self.doc(self.act("jira.issue", read=True))
        self.assertEqual(name, "issue-DEMO-12.json")
        data = json.loads(content)
        self.assertEqual(data["issue"]["untrusted"]["description"], "Make it \\*\\*work\\*\\*")
        self.assertEqual(data["issue"]["untrusted"]["comments"][0]["text"], "ok")
        result = self.act("jira.issue", {"key": "DEMO-1/../x"}, read=True)
        self.assertEqual(result.code, 1)

    def test_jira_search(self):
        name, content = self.doc(self.act("jira.search", {"jql": "project = DEMO", "limit": 500}, read=True))
        self.assertEqual(name, "jira-search.json")
        self.assertEqual(json.loads(content)["issues"]["untrusted"][0]["key"], "DEMO-3")
        body = self.server.calls("POST", "/rest/api/3/search/jql")[0]["body"]
        self.assertEqual((body["jql"], body["maxResults"]), ("project = DEMO", 50))
        self.assertEqual(self.act("jira.search", {}, read=True).code, 1)

    def test_github_pr(self):
        name, content = self.doc(self.act("github.pr", read=True, pr=7))
        self.assertEqual(name, "pr-7.json")
        data = json.loads(content)
        self.assertEqual(data["pr"]["number"], 7)
        self.assertEqual(data["review_state"], "comments")
        self.assertEqual(data["untrusted"]["threads"][0][0]["text"], "Why?")

    def test_ci_log(self):
        name, content = self.doc(self.act("ci.log", read=True, head=HEAD))
        self.assertEqual(name, "ci-log-%s.txt" % HEAD[:12])
        self.assertIn("The run 41 of ci.yml", content)
        name, _content = self.doc(self.act("ci.log", {"run": 41}, read=True, head=HEAD))
        self.assertEqual(name, "ci-log-41.txt")

    def test_an_unknown_read_verb(self):
        result = self.act("jira.delete", read=True)
        self.assertEqual(result.code, 1)
        self.assertIn("has no read verb", result.stderr)


class UnknownVerb(AdapterTest):
    def test_an_unknown_verb_fails(self):
        result = self.act("pr.merge")
        self.assertEqual(result.code, 1)
        self.assertIn("has no verb 'pr.merge'", result.stderr)


if __name__ == "__main__":
    unittest.main()
