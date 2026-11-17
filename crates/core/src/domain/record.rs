//! Records and request-body validation (SPEC §8.3, §8.4, §10.5).

use serde_json::{Map, Value, json};
use uuid::Uuid;

use super::collection::Collection;
use super::field::{Field, FieldKind};
use super::value::{is_empty, number_json};
use crate::error::{FieldError, ValidationErrors};

/// A stored record.
#[derive(Debug, Clone, PartialEq)]
pub struct Record {
    /// UUIDv7 id.
    pub id: Uuid,
    /// Creation time.
    pub created: String,
    /// Last change; also the ETag.
    pub updated: String,
    /// Field values by field name (all fields of the collection).
    pub data: Map<String, Value>,
}

impl Record {
    /// The API JSON form (without `expand`).
    pub fn to_json(&self, collection: &Collection) -> Map<String, Value> {
        let mut m = Map::new();
        m.insert("id".into(), json!(self.id));
        m.insert("collection_id".into(), json!(collection.id));
        m.insert("collection_name".into(), json!(collection.name));
        m.insert("created".into(), json!(self.created));
        m.insert("updated".into(), json!(self.updated));
        for f in &collection.fields {
            m.insert(
                f.name.clone(),
                self.data.get(&f.name).cloned().unwrap_or(Value::Null),
            );
        }
        m
    }

    /// Value of a field.
    pub fn get(&self, name: &str) -> &Value {
        self.data.get(name).unwrap_or(&Value::Null)
    }
}

/// Fields that may never be written by clients.
const READ_ONLY: &[&str] = &[
    "created",
    "updated",
    "collection_id",
    "collection_name",
    "expand",
];

/// Validated values for an insert.
#[derive(Debug, Clone, PartialEq)]
pub struct CreatePlan {
    /// Client-supplied id, if any.
    pub id: Option<Uuid>,
    /// Value for every field.
    pub values: Map<String, Value>,
}

/// Validated changes for an update: only the fields that change.
#[derive(Debug, Clone, PartialEq)]
pub struct UpdatePlan {
    /// New values of changed fields.
    pub changes: Map<String, Value>,
}

/// Validates a create body. `now` fills `autodate` fields with `on_create`.
pub fn prepare_create(
    c: &Collection,
    body: &Map<String, Value>,
    now: &str,
) -> Result<CreatePlan, ValidationErrors> {
    let mut errs = ValidationErrors::default();
    let mut id = None;
    for (k, v) in body {
        if k == "id" {
            match v.as_str().map(Uuid::try_parse) {
                Some(Ok(u)) => id = Some(u),
                _ => errs.add("id", FieldError::new("invalid_id", "Must be a UUID.")),
            }
        } else if READ_ONLY.contains(&k.as_str()) {
            errs.add(
                k.clone(),
                FieldError::new("read_only", "This field is set by the server."),
            );
        } else if c.field(k).is_none() {
            errs.add(
                k.clone(),
                FieldError::new("unknown_field", "Unknown field."),
            );
        }
    }
    let mut values = Map::new();
    for f in &c.fields {
        let v = match (&f.kind, body.get(&f.name)) {
            (FieldKind::Autodate { on_create, .. }, None) => {
                if *on_create {
                    Value::String(now.to_owned())
                } else {
                    Value::Null
                }
            }
            (_, None) => f.normalize(&Value::Null).unwrap_or(Value::Null),
            (_, Some(input)) => match f.normalize(input) {
                Ok(v) => v,
                Err(e) => {
                    errs.add(f.name.clone(), e);
                    continue;
                }
            },
        };
        if let Err(e) = f.check_required(&v) {
            errs.add(f.name.clone(), e);
        }
        values.insert(f.name.clone(), v);
    }
    if errs.is_empty() {
        Ok(CreatePlan { id, values })
    } else {
        Err(errs)
    }
}

/// Password input of an auth-collection write, taken out of the body before
/// field validation (`password` is not a field; SPEC §8.6).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AuthInput {
    /// New password.
    pub password: Option<String>,
    /// Current password, required for a non-superuser password change.
    pub old_password: Option<String>,
}

/// Removes `password` / `old_password` from the body of an auth collection.
/// Non-string values are validation errors.
pub fn take_auth_input(
    c: &Collection,
    body: &mut Map<String, Value>,
) -> Result<AuthInput, ValidationErrors> {
    let mut out = AuthInput::default();
    if c.auth().is_none() {
        return Ok(out);
    }
    let mut errs = ValidationErrors::default();
    for key in ["password", "old_password"] {
        match body.remove(key) {
            None | Some(Value::Null) => {}
            Some(Value::String(s)) if key == "password" => out.password = Some(s),
            Some(Value::String(s)) => out.old_password = Some(s),
            Some(_) => errs.add(key, FieldError::new("invalid_type", "Must be a string.")),
        }
    }
    if errs.is_empty() { Ok(out) } else { Err(errs) }
}

/// Stores emails of auth records lowercase, and empty as NULL (the unique
/// index allows many NULLs but only one empty string).
pub fn normalize_auth_values(c: &Collection, values: &mut Map<String, Value>) {
    if c.auth().is_none() {
        return;
    }
    if let Some(v) = values.get_mut("email") {
        *v = match v.as_str().map(str::trim) {
            Some("") | None => Value::Null,
            Some(e) => Value::String(e.to_lowercase()),
        };
    }
}

/// A body key with its modifier (SPEC §10.5).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Modifier {
    Set,
    Append,
    Prepend,
    Remove,
}

fn split_modifier(key: &str) -> (&str, Modifier) {
    if let Some(name) = key.strip_prefix('+') {
        (name, Modifier::Prepend)
    } else if let Some(name) = key.strip_suffix('+') {
        (name, Modifier::Append)
    } else if let Some(name) = key.strip_suffix('-') {
        (name, Modifier::Remove)
    } else {
        (key, Modifier::Set)
    }
}

/// Validates an update body against the current record, applying modifiers
/// (`tags+`, `tags-`, `+tags`, `views+`). Runs inside the write transaction,
/// so modifiers are atomic. `now` fills `autodate` fields with `on_update`.
pub fn prepare_update(
    c: &Collection,
    current: &Record,
    body: &Map<String, Value>,
    now: &str,
) -> Result<UpdatePlan, ValidationErrors> {
    let mut errs = ValidationErrors::default();
    let mut changes = Map::new();
    let mut touched = std::collections::HashSet::new();
    for (key, input) in body {
        let (name, modifier) = split_modifier(key);
        if name == "id" || READ_ONLY.contains(&name) {
            errs.add(
                key.clone(),
                FieldError::new("read_only", "This field cannot be changed."),
            );
            continue;
        }
        let Some(f) = c.field(name) else {
            errs.add(
                key.clone(),
                FieldError::new("unknown_field", "Unknown field."),
            );
            continue;
        };
        if !touched.insert(name) {
            errs.add(
                key.clone(),
                FieldError::new(
                    "conflicting_modifiers",
                    "Change a field only once per request.",
                ),
            );
            continue;
        }
        let base = changes
            .get(name)
            .unwrap_or_else(|| current.get(name))
            .clone();
        let raw = match apply_modifier(f, modifier, &base, input) {
            Ok(v) => v,
            Err(e) => {
                errs.add(key.clone(), e);
                continue;
            }
        };
        match f
            .normalize(&raw)
            .and_then(|v| f.check_required(&v).map(|()| v))
        {
            Ok(v) => {
                changes.insert(name.to_owned(), v);
            }
            Err(e) => errs.add(key.clone(), e),
        }
    }
    for f in &c.fields {
        if let FieldKind::Autodate {
            on_update: true, ..
        } = f.kind
        {
            changes.insert(f.name.clone(), Value::String(now.to_owned()));
        }
    }
    if errs.is_empty() {
        Ok(UpdatePlan { changes })
    } else {
        Err(errs)
    }
}

fn apply_modifier(
    f: &Field,
    m: Modifier,
    current: &Value,
    input: &Value,
) -> Result<Value, FieldError> {
    if m == Modifier::Set {
        return Ok(input.clone());
    }
    let unsupported = || {
        FieldError::new(
            "invalid_modifier",
            "This modifier is not supported for this field.",
        )
    };
    match (&f.kind, m) {
        (FieldKind::Number { .. }, Modifier::Append | Modifier::Remove) => {
            let delta = input
                .as_f64()
                .ok_or_else(|| FieldError::new("invalid_type", "Must be a number."))?;
            let base = current.as_f64().unwrap_or(0.0);
            let n = if m == Modifier::Append {
                base + delta
            } else {
                base - delta
            };
            Ok(number_json(n))
        }
        (FieldKind::Select { .. } | FieldKind::Relation { .. }, _) if f.is_multi() => {
            let items: Vec<Value> = match input {
                Value::Array(a) => a.clone(),
                Value::String(_) => vec![input.clone()],
                _ => {
                    return Err(FieldError::new(
                        "invalid_type",
                        "Must be a string or an array of strings.",
                    ));
                }
            };
            let mut cur: Vec<Value> = current.as_array().cloned().unwrap_or_default();
            match m {
                Modifier::Append => cur.extend(items),
                Modifier::Prepend => {
                    let mut v = items;
                    v.extend(cur);
                    cur = v;
                }
                Modifier::Remove => cur.retain(|v| !items.contains(v)),
                Modifier::Set => {}
            }
            Ok(Value::Array(cur))
        }
        _ => Err(unsupported()),
    }
}

/// Whether a value is empty (re-exported for use cases).
pub fn value_is_empty(v: &Value) -> bool {
    is_empty(v)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    fn coll() -> Collection {
        Collection::from_input(
            &json!({"name": "posts", "fields": [
                {"name": "title", "type": "text", "required": true},
                {"name": "views", "type": "number"},
                {"name": "tags", "type": "select", "values": ["a", "b", "c"], "max_select": 3},
                {"name": "done", "type": "bool"},
                {"name": "stamp", "type": "autodate", "on_create": true, "on_update": true}
            ]}),
            None,
        )
        .unwrap()
    }

    fn obj(v: Value) -> Map<String, Value> {
        v.as_object().unwrap().clone()
    }

    #[test]
    fn create_fills_defaults_and_rejects_unknown() {
        let c = coll();
        let p = prepare_create(&c, &obj(json!({"title": "hi"})), "NOW").unwrap();
        assert_eq!(p.values["done"], json!(false));
        assert_eq!(p.values["tags"], Value::Null);
        assert_eq!(p.values["stamp"], json!("NOW"));
        let e = prepare_create(
            &c,
            &obj(json!({"title": "", "nope": 1, "created": "x"})),
            "NOW",
        )
        .unwrap_err();
        assert_eq!(e.0["title"][0].code, "required");
        assert_eq!(e.0["nope"][0].code, "unknown_field");
        assert_eq!(e.0["created"][0].code, "read_only");
    }

    #[test]
    fn update_modifiers() {
        let c = coll();
        let cur = Record {
            id: Uuid::now_v7(),
            created: "c".into(),
            updated: "u".into(),
            data: obj(
                json!({"title": "t", "views": 2, "tags": ["a"], "done": false, "stamp": "c"}),
            ),
        };
        let p = prepare_update(
            &c,
            &cur,
            &obj(json!({"views+": 3, "tags+": ["b", "a"], "+tags": "c"})),
            "NOW",
        );
        let e = p.unwrap_err();
        assert!(
            e.0.values()
                .flatten()
                .any(|fe| fe.code == "conflicting_modifiers"),
            "{e:?}"
        );
        let p = prepare_update(
            &c,
            &cur,
            &obj(json!({"views+": 3, "tags+": ["b", "a"]})),
            "NOW",
        )
        .unwrap();
        assert_eq!(p.changes["views"], json!(5));
        assert_eq!(p.changes["tags"], json!(["a", "b"]), "append deduplicates");
        assert_eq!(p.changes["stamp"], json!("NOW"));
        let p = prepare_update(&c, &cur, &obj(json!({"tags-": "a", "views-": 1})), "NOW").unwrap();
        assert_eq!(p.changes["tags"], json!([]));
        assert_eq!(p.changes["views"], json!(1));
        let p = prepare_update(&c, &cur, &obj(json!({"+tags": ["c"]})), "NOW").unwrap();
        assert_eq!(p.changes["tags"], json!(["c", "a"]));
        let e = prepare_update(&c, &cur, &obj(json!({"title+": "x"})), "NOW").unwrap_err();
        assert_eq!(e.0["title+"][0].code, "invalid_modifier");
        let e = prepare_update(&c, &cur, &obj(json!({"id": "x"})), "NOW").unwrap_err();
        assert_eq!(e.0["id"][0].code, "read_only");
    }
}
