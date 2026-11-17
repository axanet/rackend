//! Building audit entries (SPEC §14.1).

use serde_json::{Map, Value, json};
use time::OffsetDateTime;

use crate::domain::audit::JSON_DIFF_MAX;
use crate::domain::field::FieldKind;
use crate::domain::{Actor, AuditEntry, Collection};
use crate::time::format_timestamp;

/// A new entry for `action` by `actor`.
pub fn entry(actor: &Actor, action: &str, now: OffsetDateTime) -> AuditEntry {
    AuditEntry {
        id: uuid::Uuid::now_v7().to_string(),
        ts: format_timestamp(now),
        actor_type: actor.kind,
        actor_id: actor.id.clone(),
        actor_label: actor.label.clone(),
        action: action.to_owned(),
        collection_id: None,
        record_id: None,
        diff: None,
        ip: actor.ip.clone(),
        user_agent: actor.user_agent.clone(),
        request_id: actor.request_id.clone(),
        meta: None,
    }
}

/// A field value as it appears in a diff: `json` values over 8 KB are truncated.
fn diff_value(c: &Collection, name: &str, v: &Value) -> Value {
    let is_json = c
        .field(name)
        .is_some_and(|f| matches!(f.kind, FieldKind::Json { .. }));
    if is_json {
        let text = v.to_string();
        if text.len() > JSON_DIFF_MAX {
            let mut cut = JSON_DIFF_MAX;
            while !text.is_char_boundary(cut) {
                cut -= 1;
            }
            return json!({ "truncated": true, "size": text.len(), "prefix": &text[..cut] });
        }
    }
    v.clone()
}

/// Diff of changed fields: `{"field": {"before": …, "after": …}}`.
pub fn record_diff(
    c: &Collection,
    before: Option<&Map<String, Value>>,
    after: Option<&Map<String, Value>>,
) -> Value {
    let mut out = Map::new();
    for f in &c.fields {
        let b = before.and_then(|m| m.get(&f.name)).unwrap_or(&Value::Null);
        let a = after.and_then(|m| m.get(&f.name)).unwrap_or(&Value::Null);
        if before.is_some() && after.is_some() && a == b {
            continue;
        }
        let mut change = Map::new();
        if before.is_some() {
            change.insert("before".into(), diff_value(c, &f.name, b));
        }
        if after.is_some() {
            change.insert("after".into(), diff_value(c, &f.name, a));
        }
        out.insert(f.name.clone(), Value::Object(change));
    }
    Value::Object(out)
}
