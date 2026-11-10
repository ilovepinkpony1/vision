"""The client of Jira Cloud REST v3: basic authentication with an account email and a token."""

from . import adf, core

ISSUE_FIELDS = ["summary", "status", "description", "issuetype", "parent", "comment", "labels", "assignee", "priority"]
API = "/rest/api/3"


class Jira:
    def __init__(self, config, why):
        self.base = core.check_base(config.text("jira_url", why), "jira_url")
        user = config.text("jira_user", why)
        token = core.secret("JIRA_TOKEN", why)
        headers = {"Authorization": core.basic(user, token), "Accept": "application/json"}
        self.client = core.Client("jira", self.base, headers, config.http_timeout())

    def search(self, jql, fields, limit=100):
        """The issues of a JQL query, page by page with nextPageToken, at most `limit`."""
        issues = []
        token = None
        while len(issues) < limit:
            body = {"jql": jql, "fields": list(fields), "maxResults": min(100, limit - len(issues))}
            if token:
                body["nextPageToken"] = token
            page = self.client.json("POST", API + "/search/jql", "search the issues", body=body) or {}
            batch = page.get("issues") or []
            issues += [i for i in batch if isinstance(i, dict)]
            token = page.get("nextPageToken")
            if not batch or not token or page.get("isLast"):
                break
        return issues[:limit]

    def issue(self, key, fields):
        core.check_key(key)
        return self.client.json(
            "GET", API + "/issue/" + core.segment(key), "read the issue %s" % key, query={"fields": ",".join(fields)}
        ) or {}

    def set_description(self, key, doc):
        core.check_key(key)
        self.client.call(
            "PUT", API + "/issue/" + core.segment(key), "replace the description of %s" % key,
            body={"fields": {"description": doc}}, ok=(200, 204),
        )

    def transitions(self, key):
        core.check_key(key)
        data = self.client.json(
            "GET", API + "/issue/" + core.segment(key) + "/transitions", "list the transitions of %s" % key
        ) or {}
        return [t for t in data.get("transitions") or [] if isinstance(t, dict)]

    def transition(self, key, transition_id):
        core.check_key(key)
        self.client.call(
            "POST", API + "/issue/" + core.segment(key) + "/transitions", "move %s" % key,
            body={"transition": {"id": str(transition_id)}}, ok=(200, 204),
        )

    def status(self, key):
        fields = self.issue(key, ["status"]).get("fields") or {}
        return name_of(fields.get("status"))

    def view(self, raw, with_parent=True):
        """The issue as observe gives it: the description and the comments as Markdown."""
        fields = raw.get("fields") or {}
        key = raw.get("key") or ""
        parent = None
        p = fields.get("parent")
        if isinstance(p, dict) and isinstance(p.get("key"), str):
            parent = {"key": p["key"], "title": (p.get("fields") or {}).get("summary") or "", "description": ""}
            if with_parent:
                try:
                    full = self.issue(p["key"], ["summary", "description"]).get("fields") or {}
                    parent["title"] = full.get("summary") or parent["title"]
                    parent["description"] = adf.to_markdown(full.get("description"))
                except core.HttpError as e:
                    if e.status not in (403, 404):
                        raise
        comments = []
        block = fields.get("comment")
        for c in (block.get("comments") if isinstance(block, dict) else None) or []:
            if isinstance(c, dict):
                comments.append({
                    "id": str(c.get("id") or ""),
                    "author": name_of(c.get("author"), "displayName"),
                    "created": c.get("created") or "",
                    "text": adf.to_markdown(c.get("body")),
                })
        return {
            "key": key,
            "title": fields.get("summary") or "",
            "description": adf.to_markdown(fields.get("description")),
            "status": name_of(fields.get("status")),
            "type": name_of(fields.get("issuetype")),
            "priority": name_of(fields.get("priority")),
            "labels": [x for x in fields.get("labels") or [] if isinstance(x, str)],
            "assignee": name_of(fields.get("assignee"), "displayName") or None,
            "parent": parent,
            "comments": comments,
            "url": "%s/browse/%s" % (self.base, key),
        }


def name_of(value, key="name"):
    if isinstance(value, dict) and isinstance(value.get(key), str):
        return value[key]
    return ""
