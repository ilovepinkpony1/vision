//! The stream parser (section 7.4): the events of `stream.jsonl` that Vision reads. Each other
//! type, and each line that is not JSON, is ignored.

use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::Path;

use anyhow::{Context, Result};
use serde::Serialize;
use serde_json::Value;

/// The four token counts of a result.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
pub struct Usage {
    pub input: i64,
    pub cache_creation: i64,
    pub cache_read: i64,
    pub output: i64,
}

/// One tool call of the agent.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct ToolCall {
    pub name: String,
    pub input: Value,
}

/// The `result` event at the end of a session step.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct ResultEvent {
    pub subtype: String,
    pub is_error: bool,
    pub result: Option<String>,
    pub structured_output: Option<Value>,
    pub session_id: Option<String>,
    pub cost_usd: Option<f64>,
    pub num_turns: Option<i64>,
    /// The number of tool calls that the permissions refused.
    pub denials: i64,
    pub usage: Usage,
}

/// One event of the stream.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub enum Event {
    Init {
        session_id: Option<String>,
        tools: Vec<String>,
        permission_mode: Option<String>,
        mcp_servers: Vec<String>,
        model: Option<String>,
    },
    RateLimit {
        /// `five_hour`, `seven_day`, or another window.
        window: String,
        /// The used part from 0 to 1, when the session reports it.
        part: Option<f64>,
        /// The reset time in seconds since 1970.
        resets_at: Option<i64>,
        status: Option<String>,
    },
    Assistant {
        texts: Vec<String>,
        tools: Vec<ToolCall>,
    },
    User {
        /// Text that the session got: the first message and the messages of the person.
        texts: Vec<String>,
        tool_results: usize,
    },
    Result(ResultEvent),
}

/// Reads one line. `None` for a line that is not JSON or not a known event.
pub fn parse_line(line: &str) -> Option<Event> {
    let value: Value = serde_json::from_str(line.trim()).ok()?;
    parse_value(&value)
}

/// Reads one parsed event.
pub fn parse_value(value: &Value) -> Option<Event> {
    let text = |v: &Value| v.as_str().map(str::to_string);
    match value.get("type")?.as_str()? {
        "system" if value.get("subtype").and_then(Value::as_str) == Some("init") => {
            Some(Event::Init {
                session_id: value.get("session_id").and_then(text),
                tools: strings(value.get("tools")),
                permission_mode: value.get("permissionMode").and_then(text),
                mcp_servers: value
                    .get("mcp_servers")
                    .and_then(Value::as_array)
                    .map(|servers| {
                        servers
                            .iter()
                            .map(|s| match s.get("name").and_then(Value::as_str) {
                                Some(name) => name.to_string(),
                                None => s.as_str().map_or_else(|| s.to_string(), str::to_string),
                            })
                            .collect()
                    })
                    .unwrap_or_default(),
                model: value.get("model").and_then(text),
            })
        }
        "rate_limit_event" => {
            let info = value.get("rate_limit_info")?;
            Some(Event::RateLimit {
                window: info.get("rateLimitType").and_then(text)?,
                part: info.get("utilization").and_then(Value::as_f64),
                resets_at: info.get("resetsAt").and_then(Value::as_i64),
                status: info.get("status").and_then(text),
            })
        }
        "assistant" => {
            let mut texts = Vec::new();
            let mut tools = Vec::new();
            for block in content(value) {
                match block.get("type").and_then(Value::as_str) {
                    Some("text") => texts.extend(block.get("text").and_then(text)),
                    Some("tool_use") => tools.push(ToolCall {
                        name: block
                            .get("name")
                            .and_then(text)
                            .unwrap_or_else(|| "?".into()),
                        input: block.get("input").cloned().unwrap_or(Value::Null),
                    }),
                    _ => {}
                }
            }
            Some(Event::Assistant { texts, tools })
        }
        "user" => {
            let message = value.get("message")?;
            let mut texts = Vec::new();
            let mut tool_results = 0;
            match message.get("content") {
                Some(Value::String(s)) => texts.push(s.clone()),
                Some(Value::Array(blocks)) => {
                    for block in blocks {
                        match block.get("type").and_then(Value::as_str) {
                            Some("text") => texts.extend(block.get("text").and_then(text)),
                            Some("tool_result") => tool_results += 1,
                            _ => {}
                        }
                    }
                }
                _ => {}
            }
            Some(Event::User {
                texts,
                tool_results,
            })
        }
        "result" => {
            let usage = value.get("usage");
            let count = |key: &str| {
                usage
                    .and_then(|u| u.get(key))
                    .and_then(Value::as_i64)
                    .unwrap_or(0)
            };
            Some(Event::Result(ResultEvent {
                subtype: value
                    .get("subtype")
                    .and_then(text)
                    .unwrap_or_else(|| "unknown".into()),
                is_error: value
                    .get("is_error")
                    .and_then(Value::as_bool)
                    .unwrap_or(false),
                result: value.get("result").and_then(text),
                structured_output: value
                    .get("structured_output")
                    .filter(|v| !v.is_null())
                    .cloned(),
                session_id: value.get("session_id").and_then(text),
                cost_usd: value.get("total_cost_usd").and_then(Value::as_f64),
                num_turns: value.get("num_turns").and_then(Value::as_i64),
                denials: value
                    .get("permission_denials")
                    .and_then(Value::as_array)
                    .map_or(0, |d| d.len() as i64),
                usage: Usage {
                    input: count("input_tokens"),
                    cache_creation: count("cache_creation_input_tokens"),
                    cache_read: count("cache_read_input_tokens"),
                    output: count("output_tokens"),
                },
            }))
        }
        _ => None,
    }
}

fn strings(value: Option<&Value>) -> Vec<String> {
    value
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

fn content(value: &Value) -> &[Value] {
    value
        .get("message")
        .and_then(|m| m.get("content"))
        .and_then(Value::as_array)
        .map_or(&[], Vec::as_slice)
}

/// The last reading of one window.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Reading {
    pub part: f64,
    pub resets_at: Option<i64>,
}

/// What a whole stream says.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct Summary {
    pub session_id: Option<String>,
    pub init_seen: bool,
    /// The last `result` event.
    pub result: Option<ResultEvent>,
    pub five_hour: Option<Reading>,
    pub seven_day: Option<Reading>,
    pub tool_calls: usize,
    pub lines: usize,
}

impl Summary {
    /// Adds one event.
    pub fn add(&mut self, event: &Event) {
        match event {
            Event::Init { session_id, .. } => {
                self.init_seen = true;
                if session_id.is_some() {
                    self.session_id.clone_from(session_id);
                }
            }
            Event::RateLimit {
                window,
                part,
                resets_at,
                status,
            } => {
                // A refused window is full, even when the session gives no number.
                let part = part.or_else(|| (status.as_deref() == Some("rejected")).then_some(1.0));
                let Some(part) = part else { return };
                let reading = Some(Reading {
                    part,
                    resets_at: *resets_at,
                });
                match window.as_str() {
                    "five_hour" => self.five_hour = reading,
                    "seven_day" => self.seven_day = reading,
                    _ => {}
                }
            }
            Event::Assistant { tools, .. } => self.tool_calls += tools.len(),
            Event::User { .. } => {}
            Event::Result(result) => {
                if result.session_id.is_some() {
                    self.session_id.clone_from(&result.session_id);
                }
                self.result = Some(result.clone());
            }
        }
    }
}

/// Reads a whole `stream.jsonl`. A missing file is an empty stream. A last line with no
/// newline is read too.
pub fn summarize(path: &Path) -> Result<Summary> {
    let mut summary = Summary::default();
    let file = match File::open(path) {
        Ok(file) => file,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(summary),
        Err(e) => return Err(e).with_context(|| format!("could not read {}", path.display())),
    };
    let mut reader = BufReader::new(file);
    let mut line = Vec::new();
    loop {
        line.clear();
        let read = reader
            .read_until(b'\n', &mut line)
            .with_context(|| format!("could not read {}", path.display()))?;
        if read == 0 {
            break;
        }
        summary.lines += 1;
        if let Some(event) = parse_line(&String::from_utf8_lossy(&line)) {
            summary.add(&event);
        }
    }
    Ok(summary)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_events_of_section_7_4_parse() {
        let init = parse_line(r#"{"type":"system","subtype":"init","session_id":"s1","tools":["Read"],"permissionMode":"dontAsk","mcp_servers":[{"name":"x","status":"connected"}]}"#).unwrap();
        assert_eq!(
            init,
            Event::Init {
                session_id: Some("s1".into()),
                tools: vec!["Read".into()],
                permission_mode: Some("dontAsk".into()),
                mcp_servers: vec!["x".into()],
                model: None
            }
        );
        let limit = parse_line(r#"{"type":"rate_limit_event","rate_limit_info":{"rateLimitType":"seven_day","utilization":0.75,"resetsAt":1794412800,"status":"allowed_warning"}}"#).unwrap();
        let mut summary = Summary::default();
        summary.add(&limit);
        assert_eq!(
            summary.seven_day,
            Some(Reading {
                part: 0.75,
                resets_at: Some(1794412800)
            })
        );
        let result = parse_line(r#"{"type":"result","subtype":"success","is_error":false,"result":"ok","structured_output":{"kind":"result"},"session_id":"s1","total_cost_usd":0.0014,"num_turns":11,"permission_denials":[{}],"usage":{"input_tokens":1,"cache_creation_input_tokens":2,"cache_read_input_tokens":3,"output_tokens":4}}"#).unwrap();
        let Event::Result(result) = result else {
            panic!("not a result")
        };
        assert_eq!(result.denials, 1);
        assert_eq!(result.usage.cache_read, 3);
        assert_eq!(result.cost_usd, Some(0.0014));
        assert!(parse_line("not json").is_none());
        assert!(parse_line(r#"{"type":"stream_event"}"#).is_none());
        let user = parse_line(r#"{"type":"user","message":{"role":"user","content":"hello"}}"#);
        assert_eq!(
            user,
            Some(Event::User {
                texts: vec!["hello".into()],
                tool_results: 0
            })
        );
    }

    #[test]
    fn a_last_line_without_newline_counts() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("stream.jsonl");
        std::fs::write(
            &path,
            "{\"type\":\"system\",\"subtype\":\"init\",\"session_id\":\"s\"}\n{\"type\":\"result\",\"subtype\":\"success\",\"session_id\":\"s2\"}",
        )
        .unwrap();
        let summary = summarize(&path).unwrap();
        assert!(summary.init_seen);
        assert_eq!(summary.session_id.as_deref(), Some("s2"));
        assert!(summary.result.is_some());
        assert_eq!(
            summarize(&dir.path().join("none")).unwrap(),
            Summary::default()
        );
    }
}
