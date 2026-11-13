//! A small client of an MCP server over HTTP (section 16): JSON-RPC 2.0 in one POST for each
//! message, with a time limit for each call.
//!
//! The client sends `initialize` first and the notification `notifications/initialized` after
//! it. A server that gives a session id in the header `Mcp-Session-Id` gets it back on each
//! later request. An answer can be plain JSON or a stream of server events with `data:` lines.
//! The server of MemPalace answers with plain JSON and no session id.

use std::io::Read;
use std::time::Duration;

use anyhow::{anyhow, bail, Context, Result};
use serde_json::{json, Value};

/// The version of the protocol that the client asks for.
pub const PROTOCOL_VERSION: &str = "2025-06-18";
/// The largest answer that the client reads.
const MAX_ANSWER: u64 = 16 * 1024 * 1024;

/// A failure to reach the server, as opposed to an error that the server gives.
#[derive(Debug, thiserror::Error)]
#[error("{0}")]
pub struct Unreachable(pub String);

/// One connection to a server, after `initialize`.
#[derive(Debug)]
pub struct Client {
    url: String,
    timeout: Duration,
    session: Option<String>,
    next_id: u64,
    agent: ureq::Agent,
}

impl Client {
    /// Connects: `initialize`, then `notifications/initialized`.
    pub fn connect(url: &str, timeout: Duration) -> Result<Client> {
        let agent = ureq::Agent::config_builder()
            .http_status_as_error(false)
            .max_redirects(0)
            .proxy(None)
            .timeout_global(Some(timeout))
            .build()
            .new_agent();
        let mut client = Client {
            url: url.trim().to_string(),
            timeout,
            session: None,
            next_id: 1,
            agent,
        };
        client.request(
            "initialize",
            json!({
                "protocolVersion": PROTOCOL_VERSION,
                "capabilities": {},
                "clientInfo": {"name": "vision", "version": env!("CARGO_PKG_VERSION")},
            }),
        )?;
        client.notify("notifications/initialized")?;
        Ok(client)
    }

    /// The names of the tools of the server.
    pub fn tool_names(&mut self) -> Result<Vec<String>> {
        let result = self.request("tools/list", json!({}))?;
        Ok(result
            .get("tools")
            .and_then(Value::as_array)
            .map(|tools| {
                tools
                    .iter()
                    .filter_map(|t| t.get("name").and_then(Value::as_str))
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default())
    }

    /// Calls a tool. Gives the JSON of its first text block, or that text as a string when it
    /// is not JSON. A result with `isError`, or a JSON object with `error`, is an error.
    pub fn call_tool(&mut self, name: &str, arguments: Value) -> Result<Value> {
        let result = self.request("tools/call", json!({"name": name, "arguments": arguments}))?;
        let text = result
            .get("content")
            .and_then(Value::as_array)
            .and_then(|blocks| {
                blocks
                    .iter()
                    .find_map(|b| b.get("text").and_then(Value::as_str))
            })
            .unwrap_or("");
        let value = serde_json::from_str::<Value>(text).unwrap_or(Value::String(text.into()));
        if result.get("isError").and_then(Value::as_bool) == Some(true) {
            bail!("{name} failed: {}", short(&value));
        }
        if let Some(error) = value.get("error") {
            bail!("{name} failed: {}", short(error));
        }
        Ok(value)
    }

    fn notify(&mut self, method: &str) -> Result<()> {
        self.post(&json!({"jsonrpc": "2.0", "method": method}))
            .map(|_| ())
    }

    fn request(&mut self, method: &str, params: Value) -> Result<Value> {
        let id = self.next_id;
        self.next_id += 1;
        let message = json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params});
        let answer = self
            .post(&message)?
            .ok_or_else(|| anyhow!("{method}: the server gave no answer"))?;
        let reply = pick_reply(&answer, id)
            .ok_or_else(|| anyhow!("{method}: the answer has no reply with the id {id}"))?;
        if let Some(error) = reply.get("error") {
            bail!("{method} failed: {}", short(error));
        }
        Ok(reply.get("result").cloned().unwrap_or(Value::Null))
    }

    /// Sends one message. Gives the answer text, or `None` for an answer with no body.
    fn post(&mut self, message: &Value) -> Result<Option<String>> {
        let body = serde_json::to_vec(message)?;
        let mut request = self
            .agent
            .post(&self.url)
            .header("Content-Type", "application/json")
            .header("Accept", "application/json, text/event-stream")
            .header("MCP-Protocol-Version", PROTOCOL_VERSION);
        if let Some(session) = &self.session {
            request = request.header("Mcp-Session-Id", session.as_str());
        }
        let mut response = request.send(&body[..]).map_err(|e| {
            let why = match e {
                ureq::Error::Timeout(_) => format!("no answer in {} s", self.timeout.as_secs()),
                other => format!("no answer: {other}"),
            };
            Unreachable(format!("MemPalace at {}: {why}", self.url))
        })?;
        let status = response.status().as_u16();
        if let Some(session) = response
            .headers()
            .get("mcp-session-id")
            .and_then(|v| v.to_str().ok())
        {
            self.session = Some(session.to_string());
        }
        let mut text = String::new();
        response
            .body_mut()
            .as_reader()
            .take(MAX_ANSWER)
            .read_to_string(&mut text)
            .map_err(|e| Unreachable(format!("MemPalace at {}: {e}", self.url)))?;
        if !(200..300).contains(&status) {
            return Err(Unreachable(format!(
                "MemPalace at {} answered with the status {status}: {}",
                self.url,
                crate::journal::errors::cut(text.trim(), 200)
            ))
            .into());
        }
        Ok((!text.trim().is_empty()).then_some(text))
    }
}

/// The reply with the id: the JSON of the answer, or one `data:` event of a stream.
fn pick_reply(answer: &str, id: u64) -> Option<Value> {
    let fits = |v: &Value| v.get("id").and_then(Value::as_u64) == Some(id);
    if let Ok(value) = serde_json::from_str::<Value>(answer.trim()) {
        return match value {
            Value::Array(items) => items.into_iter().find(fits),
            value => fits(&value).then_some(value),
        };
    }
    // A stream of server events: each event has one or more `data:` lines.
    let mut data = String::new();
    for line in answer.lines().chain(std::iter::once("")) {
        if let Some(rest) = line.strip_prefix("data:") {
            data.push_str(rest.trim_start());
            continue;
        }
        if line.trim().is_empty() && !data.is_empty() {
            if let Ok(value) = serde_json::from_str::<Value>(&data) {
                if fits(&value) {
                    return Some(value);
                }
            }
            data.clear();
        }
    }
    None
}

fn short(value: &Value) -> String {
    let text = match value {
        Value::String(text) => text.clone(),
        Value::Object(map) => map
            .get("message")
            .and_then(Value::as_str)
            .map(str::to_string)
            .unwrap_or_else(|| value.to_string()),
        other => other.to_string(),
    };
    crate::journal::errors::cut(text.trim(), 300)
}

/// True when the error is a failure to reach the server.
pub fn is_unreachable(e: &anyhow::Error) -> bool {
    e.chain().any(|cause| cause.is::<Unreachable>())
}

/// Connects with the configuration of `[journal]`.
pub fn connect_config(journal: &crate::config::Journal) -> Result<Client> {
    Client::connect(
        &journal.mempalace_url,
        Duration::from_secs(journal.timeout.max(1)),
    )
    .context("the index of the journal")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_reply_comes_from_json_or_from_a_stream_of_events() {
        assert_eq!(
            pick_reply(r#"{"jsonrpc":"2.0","id":2,"result":{}}"#, 2).unwrap()["result"],
            json!({})
        );
        assert!(pick_reply(r#"{"jsonrpc":"2.0","id":3,"result":{}}"#, 2).is_none());
        let stream = "event: message\ndata: {\"jsonrpc\":\"2.0\",\"method\":\"x\"}\n\nevent: message\ndata: {\"jsonrpc\":\"2.0\",\"id\":7,\n";
        assert!(
            pick_reply(stream, 7).is_none(),
            "a broken event is no reply"
        );
        let stream =
            "event: message\ndata: {\"jsonrpc\":\"2.0\",\"id\":7,\"result\":{\"ok\":1}}\n\n";
        assert_eq!(pick_reply(stream, 7).unwrap()["result"]["ok"], 1);
    }
}
