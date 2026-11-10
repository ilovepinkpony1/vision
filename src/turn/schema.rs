//! A test of a result object against the schemas of `agents/` (section 7.6).
//!
//! Claude Code already tests the output against `--json-schema`. Vision tests it again, because
//! the word of a session proves nothing. Only the keywords that the schemas of `agents/` use
//! are known: `type`, `required`, `properties`, `additionalProperties: false`, `enum`,
//! `items`, and `minItems`. Each other keyword is ignored.

use serde_json::Value;

/// The problems of `value` against `schema`. Empty when it fits.
pub fn problems(schema: &Value, value: &Value) -> Vec<String> {
    let mut out = Vec::new();
    check(schema, value, "", &mut out);
    out
}

fn check(schema: &Value, value: &Value, at: &str, out: &mut Vec<String>) {
    let here = if at.is_empty() { "the object" } else { at };
    if let Some(types) = schema.get("type") {
        let names: Vec<&str> = match types {
            Value::String(name) => vec![name.as_str()],
            Value::Array(list) => list.iter().filter_map(Value::as_str).collect(),
            _ => Vec::new(),
        };
        if !names.is_empty() && !names.iter().any(|name| is_type(value, name)) {
            out.push(format!("{here} must be {}", names.join(" or ")));
            return;
        }
    }
    if let Some(Value::Array(choices)) = schema.get("enum") {
        if !choices.contains(value) {
            let list: Vec<String> = choices.iter().map(Value::to_string).collect();
            out.push(format!("{here} must be one of {}", list.join(", ")));
        }
    }
    if let Value::Object(map) = value {
        let properties = schema.get("properties").and_then(Value::as_object);
        if let Some(Value::Array(required)) = schema.get("required") {
            for name in required.iter().filter_map(Value::as_str) {
                if !map.contains_key(name) {
                    out.push(format!("{here} lacks `{name}`"));
                }
            }
        }
        for (name, item) in map {
            let path = format!("{at}/{name}");
            match properties.and_then(|p| p.get(name)) {
                Some(sub) => check(sub, item, &path, out),
                None if schema.get("additionalProperties") == Some(&Value::Bool(false)) => {
                    out.push(format!(
                        "{here} has `{name}`, which the schema does not allow"
                    ));
                }
                None => {}
            }
        }
    }
    if let Value::Array(items) = value {
        if let Some(min) = schema.get("minItems").and_then(Value::as_u64) {
            if (items.len() as u64) < min {
                out.push(format!("{here} needs at least {min} items"));
            }
        }
        if let Some(sub) = schema.get("items") {
            for (index, item) in items.iter().enumerate() {
                check(sub, item, &format!("{at}/{index}"), out);
            }
        }
    }
}

fn is_type(value: &Value, name: &str) -> bool {
    match name {
        "object" => value.is_object(),
        "array" => value.is_array(),
        "string" => value.is_string(),
        "boolean" => value.is_boolean(),
        "null" => value.is_null(),
        "number" => value.is_number(),
        "integer" => value.is_i64() || value.is_u64(),
        _ => true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn turn_schema() -> Value {
        serde_json::from_str(include_str!("../../agents/turn.schema.json")).unwrap()
    }

    #[test]
    fn a_valid_result_fits() {
        let value = json!({"kind": "questions", "summary": "two cards",
            "questions": [{"text": "CSV or XLSX?", "context": "c", "evidence": "e", "kind": "product",
                "options": [{"label": "CSV", "means": "m"}, {"label": "XLSX", "means": "m"}], "recommended": null}]});
        assert_eq!(problems(&turn_schema(), &value), Vec::<String>::new());
    }

    #[test]
    fn each_kind_of_problem_is_named() {
        let schema = turn_schema();
        assert_eq!(
            problems(&schema, &json!("text")),
            ["the object must be object"]
        );
        let value = json!({"kind": "done", "extra": 1,
            "questions": [{"text": "t", "context": "c", "evidence": "e", "kind": "code",
                "options": [{"label": "a", "means": "m"}]}]});
        let found = problems(&schema, &value);
        assert!(
            found.iter().any(|p| p.contains("lacks `summary`")),
            "{found:?}"
        );
        assert!(
            found.iter().any(|p| p.starts_with("/kind must be one of")),
            "{found:?}"
        );
        assert!(found.iter().any(|p| p.contains("has `extra`")), "{found:?}");
        assert!(
            found
                .iter()
                .any(|p| p == "/questions/0/options needs at least 2 items"),
            "{found:?}"
        );
    }
}
