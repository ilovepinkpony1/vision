//! The client of Jira Cloud REST v3: basic authentication with an account email and a token.

use serde_json::{json, Map, Value};

use super::adf;
use super::core::{check_key, secret, truthy, Config, Error, Res};
use super::http::{basic, check_base, segment, Call, Client};

pub const ISSUE_FIELDS: [&str; 9] = [
    "summary",
    "status",
    "description",
    "issuetype",
    "parent",
    "comment",
    "labels",
    "assignee",
    "priority",
];
const API: &str = "/rest/api/3";

pub struct Jira {
    pub base: String,
    pub client: Client,
}

impl Jira {
    pub fn new(config: &Config, why: &str) -> Res<Jira> {
        let base = check_base(&config.text_need("jira_url", why)?, "jira_url")?;
        let user = config.text_need("jira_user", why)?;
        let token = secret("JIRA_TOKEN", why)?;
        let headers = vec![
            ("Authorization".to_string(), basic(&user, &token)),
            ("Accept".to_string(), "application/json".to_string()),
        ];
        let client = Client {
            service: "jira".into(),
            base: base.clone(),
            headers,
            timeout: config.http_timeout()?,
        };
        Ok(Jira { base, client })
    }

    /// The issues of a JQL query, page by page with nextPageToken, at most `limit`.
    pub fn search(&self, jql: &str, fields: &[&str], limit: usize) -> Res<Vec<Value>> {
        let mut issues: Vec<Value> = Vec::new();
        let mut token: Option<Value> = None;
        while issues.len() < limit {
            let mut body = json!({
                "jql": jql,
                "fields": fields,
                "maxResults": (limit - issues.len()).min(100),
            });
            if let Some(token) = &token {
                body["nextPageToken"] = token.clone();
            }
            let page = self
                .client
                .call(
                    "POST",
                    &format!("{API}/search/jql"),
                    "search the issues",
                    Call {
                        body: Some(&body),
                        ..Call::default()
                    },
                )?
                .json()?;
            let page = truthy(Some(&page)).cloned().unwrap_or(json!({}));
            let batch: Vec<Value> = match truthy(page.get("issues")) {
                Some(Value::Array(items)) => items.clone(),
                _ => Vec::new(),
            };
            let empty = batch.is_empty();
            issues.extend(batch.into_iter().filter(Value::is_object));
            token = truthy(page.get("nextPageToken")).cloned();
            if empty || token.is_none() || truthy(page.get("isLast")).is_some() {
                break;
            }
        }
        issues.truncate(limit);
        Ok(issues)
    }

    pub fn issue(&self, key: &str, fields: &[&str]) -> Res<Value> {
        check_key(Some(&json!(key)), "the ticket key")?;
        let value = self.client.get_query(
            &format!("{API}/issue/{}", segment(key)),
            &format!("read the issue {key}"),
            vec![("fields".into(), fields.join(","))],
        )?;
        Ok(truthy(Some(&value)).cloned().unwrap_or(json!({})))
    }

    pub fn set_description(&self, key: &str, doc: &Value) -> Res<()> {
        check_key(Some(&json!(key)), "the ticket key")?;
        let body = json!({"fields": {"description": doc}});
        self.client.call(
            "PUT",
            &format!("{API}/issue/{}", segment(key)),
            &format!("replace the description of {key}"),
            Call {
                body: Some(&body),
                ok: &[200, 204],
                ..Call::default()
            },
        )?;
        Ok(())
    }

    pub fn transitions(&self, key: &str) -> Res<Vec<Value>> {
        check_key(Some(&json!(key)), "the ticket key")?;
        let data = self.client.get(
            &format!("{API}/issue/{}/transitions", segment(key)),
            &format!("list the transitions of {key}"),
        )?;
        Ok(match truthy(data.get("transitions")) {
            Some(Value::Array(items)) => items.iter().filter(|t| t.is_object()).cloned().collect(),
            _ => Vec::new(),
        })
    }

    pub fn transition(&self, key: &str, id: &str) -> Res<()> {
        check_key(Some(&json!(key)), "the ticket key")?;
        let body = json!({"transition": {"id": id}});
        self.client.call(
            "POST",
            &format!("{API}/issue/{}/transitions", segment(key)),
            &format!("move {key}"),
            Call {
                body: Some(&body),
                ok: &[200, 204],
                ..Call::default()
            },
        )?;
        Ok(())
    }

    pub fn status(&self, key: &str) -> Res<String> {
        let issue = self.issue(key, &["status"])?;
        Ok(name_of(fields_of(&issue).get("status"), "name"))
    }

    /// The issue as observe gives it: the description and the comments as Markdown.
    pub fn view(&self, raw: &Value, with_parent: bool) -> Res<Value> {
        let fields = fields_of(raw);
        let key = raw
            .get("key")
            .and_then(Value::as_str)
            .filter(|k| !k.is_empty())
            .unwrap_or("")
            .to_string();
        let mut parent = Value::Null;
        if let Some(Value::Object(p)) = fields.get("parent") {
            if let Some(parent_key) = p.get("key").and_then(Value::as_str) {
                let title = text_or_empty(fields_of(&Value::Object(p.clone())).get("summary"));
                let mut view = json!({"key": parent_key, "title": title, "description": ""});
                if with_parent {
                    match self.issue(parent_key, &["summary", "description"]) {
                        Ok(full) => {
                            let full = fields_of(&full);
                            if let Some(t) = truthy(full.get("summary")) {
                                view["title"] = t.clone();
                            }
                            view["description"] = json!(adf::to_markdown(
                                full.get("description").unwrap_or(&Value::Null)
                            ));
                        }
                        Err(Error::Http { status, .. }) if status == 403 || status == 404 => {}
                        Err(e) => return Err(e),
                    }
                }
                parent = view;
            }
        }
        let mut comments = Vec::new();
        if let Some(Value::Object(block)) = fields.get("comment") {
            if let Some(Value::Array(items)) = truthy(block.get("comments")) {
                for c in items.iter().filter_map(Value::as_object) {
                    comments.push(json!({
                        "id": super::core::py_str(truthy(c.get("id")).unwrap_or(&json!(""))),
                        "author": name_of(c.get("author"), "displayName"),
                        "created": truthy(c.get("created")).cloned().unwrap_or(json!("")),
                        "text": adf::to_markdown(c.get("body").unwrap_or(&Value::Null)),
                    }));
                }
            }
        }
        let labels: Vec<Value> = match truthy(fields.get("labels")) {
            Some(Value::Array(items)) => items.iter().filter(|x| x.is_string()).cloned().collect(),
            _ => Vec::new(),
        };
        let assignee = name_of(fields.get("assignee"), "displayName");
        Ok(json!({
            "key": key,
            "title": truthy(fields.get("summary")).cloned().unwrap_or(json!("")),
            "description": adf::to_markdown(fields.get("description").unwrap_or(&Value::Null)),
            "status": name_of(fields.get("status"), "name"),
            "type": name_of(fields.get("issuetype"), "name"),
            "priority": name_of(fields.get("priority"), "name"),
            "labels": labels,
            "assignee": if assignee.is_empty() { Value::Null } else { json!(assignee) },
            "parent": parent,
            "comments": comments,
            "url": format!("{}/browse/{key}", self.base),
        }))
    }
}

/// The `fields` object of an issue, or an empty one.
pub fn fields_of(raw: &Value) -> Map<String, Value> {
    match truthy(raw.get("fields")) {
        Some(Value::Object(map)) => map.clone(),
        _ => Map::new(),
    }
}

fn text_or_empty(value: Option<&Value>) -> Value {
    truthy(value).cloned().unwrap_or(json!(""))
}

/// The text of `key` of an object, such as the name of a status, or "".
pub fn name_of(value: Option<&Value>, key: &str) -> String {
    value
        .and_then(|v| v.get(key))
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string()
}
