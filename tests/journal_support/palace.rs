//! A fake MCP server of MemPalace for the tests of the journal and of the sessions: plain JSON
//! answers and no session id, as the real server gives them. It keeps its drawers in memory.

use std::sync::{Arc, Mutex};

use serde_json::{json, Value};

use crate::server::{FakeServer, Reply};

/// The drawers and the calls of a fake MemPalace.
#[derive(Default)]
pub struct Palace {
    pub drawers: Vec<Value>,
    pub calls: Vec<(String, Value)>,
}

impl Palace {
    pub fn calls_of(&self, tool: &str) -> Vec<Value> {
        self.calls
            .iter()
            .filter(|(name, _)| name == tool)
            .map(|(_, args)| args.clone())
            .collect()
    }
}

fn reply(id: Option<Value>, result: Value) -> Reply {
    Reply::json(200, json!({"jsonrpc": "2.0", "id": id, "result": result}))
}

/// A fake MCP server of MemPalace on `/mcp`, with plain JSON answers and no session id, as the
/// real server gives them.
pub fn fake_palace() -> (FakeServer, Arc<Mutex<Palace>>) {
    let server = FakeServer::start();
    let palace = Arc::new(Mutex::new(Palace::default()));
    let state = Arc::clone(&palace);
    server.route("POST", "/mcp", move |record| {
        let body = &record.body;
        let id = body.get("id").cloned();
        match body["method"].as_str().unwrap_or("") {
            "initialize" => reply(
                id,
                json!({"protocolVersion": "2025-06-18", "capabilities": {"tools": {}},
                       "serverInfo": {"name": "mempalace", "version": "test"}}),
            ),
            "notifications/initialized" => Reply::empty(202),
            "tools/list" => reply(
                id,
                json!({"tools": [
                    {"name": "mempalace_status"}, {"name": "mempalace_search"},
                    {"name": "mempalace_add_drawer"}, {"name": "mempalace_delete_by_source"},
                    {"name": "mempalace_get_drawer"}]}),
            ),
            "tools/call" => {
                let name = body["params"]["name"].as_str().unwrap_or("").to_string();
                let args = body["params"]["arguments"].clone();
                let mut palace = state.lock().unwrap();
                palace.calls.push((name.clone(), args.clone()));
                let result = match name.as_str() {
                    "mempalace_delete_by_source" => {
                        let source = args["source_file"].clone();
                        let before = palace.drawers.len();
                        if args["dry_run"] == json!(false) {
                            palace.drawers.retain(|d| d["source_file"] != source);
                        }
                        json!({"success": true, "dry_run": args["dry_run"] != json!(false),
                               "deleted": before - palace.drawers.len()})
                    }
                    "mempalace_add_drawer" => {
                        let drawer_id = format!("drawer_{}", palace.drawers.len() + 1);
                        palace.drawers.push(json!({
                            "drawer_id": drawer_id, "wing": args["wing"], "room": args["room"],
                            "content": args["content"], "source_file": args["source_file"],
                        }));
                        json!({"success": true, "drawer_id": drawer_id, "wing": args["wing"],
                               "room": args["room"], "chunks": 1})
                    }
                    "mempalace_search" => {
                        let words: Vec<String> = args["query"]
                            .as_str()
                            .unwrap_or("")
                            .to_lowercase()
                            .split_whitespace()
                            .map(str::to_string)
                            .collect();
                        let results: Vec<Value> = palace
                            .drawers
                            .iter()
                            .filter(|d| d["wing"] == args["wing"])
                            .filter(|d| {
                                let text = d["content"].as_str().unwrap_or("").to_lowercase();
                                words.iter().any(|w| text.contains(w.as_str()))
                            })
                            .map(|d| {
                                let source = d["source_file"].as_str().unwrap_or("");
                                json!({"drawer_id": d["drawer_id"], "text": d["content"],
                                       "wing": d["wing"], "room": d["room"],
                                       "source_file": source.rsplit('/').next().unwrap_or(""),
                                       "source_path": source, "similarity": 0.8})
                            })
                            .collect();
                        json!({"query": args["query"], "results": results})
                    }
                    other => json!({"error": format!("unknown tool {other}")}),
                };
                reply(
                    id,
                    json!({"content": [{"type": "text", "text": result.to_string()}]}),
                )
            }
            other => Reply::json(
                200,
                json!({"jsonrpc": "2.0", "id": id,
                       "error": {"code": -32601, "message": format!("no method {other}")}}),
            ),
        }
    });
    (server, palace)
}

/// An address where nothing answers.
pub fn dead_url() -> String {
    let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let port = listener.local_addr().unwrap().port();
    drop(listener);
    format!("http://127.0.0.1:{port}/mcp")
}
