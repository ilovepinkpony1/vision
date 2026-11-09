//! The role files and the result schemas in `agents/` are inputs of each session.
//! These tests make sure that each schema parses and keeps the values of the design.

use std::fs;
use std::path::PathBuf;

use serde_json::Value;

fn agents_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("agents")
}

fn schema(name: &str) -> Value {
    let path = agents_dir().join(name);
    let text = fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
    serde_json::from_str(&text).unwrap_or_else(|e| panic!("parse {}: {e}", path.display()))
}

fn strings(value: &Value) -> Vec<&str> {
    value
        .as_array()
        .expect("an array")
        .iter()
        .map(|v| v.as_str().expect("a string"))
        .collect()
}

#[test]
fn the_turn_schema_has_the_three_kinds_and_the_card_fields() {
    let s = schema("turn.schema.json");
    assert_eq!(s["type"], "object");
    assert_eq!(s["additionalProperties"], false);
    assert_eq!(strings(&s["required"]), ["kind", "summary"]);
    assert_eq!(
        strings(&s["properties"]["kind"]["enum"]),
        ["result", "questions", "requests"]
    );
    let card = &s["properties"]["questions"]["items"];
    assert_eq!(
        strings(&card["required"]),
        ["text", "context", "options", "evidence", "kind"]
    );
    assert_eq!(
        strings(&card["properties"]["kind"]["enum"]),
        ["product", "architecture", "scope", "code"]
    );
    assert_eq!(card["properties"]["options"]["minItems"], 2);
    assert_eq!(
        strings(&s["properties"]["documents"]["items"]["required"]),
        ["name", "content"]
    );
}

#[test]
fn the_judge_schema_has_the_outcomes_and_the_bases() {
    let s = schema("judge.schema.json");
    let answer = &s["properties"]["answers"]["items"];
    assert_eq!(
        strings(&answer["required"]),
        ["card", "outcome", "basis", "sure", "reason"]
    );
    assert_eq!(
        strings(&answer["properties"]["outcome"]["enum"]),
        ["answer", "propose", "ask"]
    );
    assert_eq!(
        strings(&answer["properties"]["basis"]["enum"]),
        ["decision", "principle", "inference", "none"]
    );
    assert_eq!(answer["properties"]["sure"]["type"], "boolean");
}

#[test]
fn the_curator_schema_has_the_five_operations() {
    let s = schema("curator.schema.json");
    assert_eq!(strings(&s["required"]), ["summary", "changes"]);
    let change = &s["properties"]["changes"]["items"];
    assert_eq!(strings(&change["required"]), ["op", "reason"]);
    assert_eq!(
        strings(&change["properties"]["op"]["enum"]),
        ["add", "edit", "merge", "retire", "question"]
    );
}

#[test]
fn each_role_file_has_a_title_and_tells_that_outside_text_is_data() {
    let mut seen = 0;
    for entry in fs::read_dir(agents_dir()).expect("the agents folder") {
        let path = entry.expect("an entry").path();
        if path.extension().and_then(|e| e.to_str()) != Some("md") {
            continue;
        }
        let text = fs::read_to_string(&path).expect("a role file");
        assert!(text.starts_with("# "), "{} has no title", path.display());
        assert!(
            text.contains("{\"untrusted\": \"...\"}"),
            "{} does not tell that outside text is data",
            path.display()
        );
        seen += 1;
    }
    assert!(seen >= 3, "expected the role files, found {seen}");
}
