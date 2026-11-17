//! Writers for the document: pretty JSON and YAML (block style) with the
//! key order of [`ordered`]. YAML strings and keys are double-quoted scalars
//! with JSON escapes, which YAML 1.2 reads identically, so no YAML
//! dependency is needed.

use std::fmt::Write as _;

use serde_json::{Map, Value};

/// Keys that come first, in this order; the rest follow alphabetically.
/// Generators use the first media type of a request body, so multipart
/// (which can carry files) comes before JSON.
const FIRST: &[&str] = &[
    "openapi",
    "info",
    "servers",
    "security",
    "tags",
    "paths",
    "components",
    "operationId",
    "summary",
    "parameters",
    "requestBody",
    "responses",
    "multipart/form-data",
];

/// The keys of `m` in document order.
pub fn ordered(m: &Map<String, Value>) -> Vec<(&String, &Value)> {
    let mut v: Vec<(&String, &Value)> = m.iter().collect();
    let rank = |k: &str| FIRST.iter().position(|f| *f == k).unwrap_or(FIRST.len());
    v.sort_by(|a, b| rank(a.0).cmp(&rank(b.0)).then_with(|| a.0.cmp(b.0)));
    v
}

fn json_into(v: &Value, indent: usize, out: &mut String) {
    let pad = "  ".repeat(indent + 1);
    let end = "  ".repeat(indent);
    match v {
        Value::Object(o) if !o.is_empty() => {
            out.push_str("{\n");
            let items = ordered(o);
            for (i, (k, child)) in items.iter().enumerate() {
                let _ = write!(out, "{pad}{}: ", Value::String((*k).clone()));
                json_into(child, indent + 1, out);
                out.push_str(if i + 1 < items.len() { ",\n" } else { "\n" });
            }
            let _ = write!(out, "{end}}}");
        }
        Value::Array(a) if !a.is_empty() => {
            out.push_str("[\n");
            for (i, child) in a.iter().enumerate() {
                out.push_str(&pad);
                json_into(child, indent + 1, out);
                out.push_str(if i + 1 < a.len() { ",\n" } else { "\n" });
            }
            let _ = write!(out, "{end}]");
        }
        other => out.push_str(&scalar(other)),
    }
}

/// `v` as pretty JSON in document key order.
pub fn to_json(v: &Value) -> String {
    let mut out = String::new();
    json_into(v, 0, &mut out);
    out.push('\n');
    out
}

fn scalar(v: &Value) -> String {
    match v {
        Value::Null => "null".into(),
        Value::Bool(b) => b.to_string(),
        Value::Number(n) => n.to_string(),
        Value::String(s) => Value::String(s.clone()).to_string(),
        Value::Array(a) if a.is_empty() => "[]".into(),
        Value::Object(o) if o.is_empty() => "{}".into(),
        _ => String::new(),
    }
}

fn is_block(v: &Value) -> bool {
    match v {
        Value::Array(a) => !a.is_empty(),
        Value::Object(o) => !o.is_empty(),
        _ => false,
    }
}

fn emit(v: &Value, indent: usize, out: &mut String) {
    let pad = " ".repeat(indent);
    match v {
        Value::Object(o) if !o.is_empty() => {
            for (k, child) in ordered(o) {
                let key = Value::String(k.clone()).to_string();
                if is_block(child) {
                    let _ = writeln!(out, "{pad}{key}:");
                    emit(child, indent + 2, out);
                } else {
                    let _ = writeln!(out, "{pad}{key}: {}", scalar(child));
                }
            }
        }
        Value::Array(a) if !a.is_empty() => {
            for child in a {
                if is_block(child) {
                    let _ = writeln!(out, "{pad}-");
                    emit(child, indent + 2, out);
                } else {
                    let _ = writeln!(out, "{pad}- {}", scalar(child));
                }
            }
        }
        other => {
            let _ = writeln!(out, "{pad}{}", scalar(other));
        }
    }
}

/// `v` as a YAML document.
pub fn to_yaml(v: &Value) -> String {
    let mut out = String::new();
    emit(v, 0, &mut out);
    out
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use serde_json::json;

    use super::*;

    #[test]
    fn json_order_and_round_trip() {
        let v = json!({ "z": 1, "paths": {}, "openapi": "3.1.0", "content": { "application/json": 1, "multipart/form-data": 2 } });
        let text = to_json(&v);
        assert!(
            text.starts_with("{\n  \"openapi\": \"3.1.0\",\n  \"paths\": {},"),
            "{text}"
        );
        assert!(text.find("multipart").unwrap() < text.find("application/json").unwrap());
        assert_eq!(serde_json::from_str::<Value>(&text).unwrap(), v);
    }

    #[test]
    fn emits_blocks_and_scalars() {
        let v = json!({ "a": [1, { "b": "x: \"y\"\n" }, []], "c": {}, "d": null });
        assert_eq!(
            to_yaml(&v),
            "\"a\":\n  - 1\n  -\n    \"b\": \"x: \\\"y\\\"\\n\"\n  - []\n\"c\": {}\n\"d\": null\n"
        );
    }
}
