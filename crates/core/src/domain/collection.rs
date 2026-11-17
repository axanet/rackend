//! Collections (SPEC §8.1–§8.3, §8.5, §9.1).

use std::collections::HashSet;

use serde_json::{Value, json};
use uuid::Uuid;

use super::field::{Field, FieldKind};
use super::names::{check_collection_name, check_field_name};
use crate::error::{FieldError, ValidationErrors};

/// Collection type.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CollectionType {
    /// Plain data.
    Base,
    /// Data + authentication (M2).
    Auth,
}

impl CollectionType {
    /// The `type` string.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Base => "base",
            Self::Auth => "auth",
        }
    }
}

/// The five rule-controlled operations.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RuleOp {
    /// List / search.
    List,
    /// View one record.
    View,
    /// Create.
    Create,
    /// Update.
    Update,
    /// Delete.
    Delete,
}

impl RuleOp {
    /// All operations.
    pub const ALL: [RuleOp; 5] = [
        Self::List,
        Self::View,
        Self::Create,
        Self::Update,
        Self::Delete,
    ];

    /// Lowercase name.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::List => "list",
            Self::View => "view",
            Self::Create => "create",
            Self::Update => "update",
            Self::Delete => "delete",
        }
    }
}

/// Rule values (SPEC §9.1): `None` = locked (superusers only), `Some("")` =
/// public, otherwise an expression.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Rules {
    /// List rule.
    pub list: Option<String>,
    /// View rule.
    pub view: Option<String>,
    /// Create rule.
    pub create: Option<String>,
    /// Update rule.
    pub update: Option<String>,
    /// Delete rule.
    pub delete: Option<String>,
}

impl Rules {
    /// The rule of one operation.
    pub fn get(&self, op: RuleOp) -> Option<&str> {
        match op {
            RuleOp::List => self.list.as_deref(),
            RuleOp::View => self.view.as_deref(),
            RuleOp::Create => self.create.as_deref(),
            RuleOp::Update => self.update.as_deref(),
            RuleOp::Delete => self.delete.as_deref(),
        }
    }

    fn set(&mut self, op: RuleOp, v: Option<String>) {
        match op {
            RuleOp::List => self.list = v,
            RuleOp::View => self.view = v,
            RuleOp::Create => self.create = v,
            RuleOp::Update => self.update = v,
            RuleOp::Delete => self.delete = v,
        }
    }

    /// JSON form.
    pub fn to_json(&self) -> Value {
        json!({ "list": self.list, "view": self.view, "create": self.create, "update": self.update, "delete": self.delete })
    }
}

/// A declared index: `CREATE [UNIQUE] INDEX "idx_<collection>_<name>"`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IndexDef {
    /// Index name (unique within the collection).
    pub name: String,
    /// Field names.
    pub fields: Vec<String>,
    /// Unique index.
    pub unique: bool,
}

/// Collection options.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CollectionOptions {
    /// Record-level audit (SPEC §14.1), default true.
    pub audit_records: bool,
}

impl Default for CollectionOptions {
    fn default() -> Self {
        Self {
            audit_records: true,
        }
    }
}

/// A collection definition.
#[derive(Debug, Clone, PartialEq)]
pub struct Collection {
    /// Immutable id.
    pub id: Uuid,
    /// Table name.
    pub name: String,
    /// Type.
    pub kind: CollectionType,
    /// User fields, in order.
    pub fields: Vec<Field>,
    /// Declared indexes.
    pub indexes: Vec<IndexDef>,
    /// Access rules.
    pub rules: Rules,
    /// Options.
    pub options: CollectionOptions,
    /// Creation time.
    pub created: String,
    /// Last change.
    pub updated: String,
}

impl Collection {
    /// Field by name.
    pub fn field(&self, name: &str) -> Option<&Field> {
        self.fields.iter().find(|f| f.name == name)
    }

    /// Field by id.
    pub fn field_by_id(&self, id: Uuid) -> Option<&Field> {
        self.fields.iter().find(|f| f.id == id)
    }

    /// The API/DB JSON form.
    pub fn to_json(&self) -> Value {
        json!({
            "id": self.id,
            "name": self.name,
            "type": self.kind.as_str(),
            "fields": self.fields_json(),
            "indexes": self.indexes_json(),
            "rules": self.rules.to_json(),
            "options": self.options_json(),
            "created": self.created,
            "updated": self.updated,
        })
    }

    /// `fields` column JSON.
    pub fn fields_json(&self) -> Value {
        Value::Array(self.fields.iter().map(Field::to_json).collect())
    }

    /// `indexes` column JSON.
    pub fn indexes_json(&self) -> Value {
        Value::Array(
            self.indexes
                .iter()
                .map(|i| json!({"name": i.name, "fields": i.fields, "unique": i.unique}))
                .collect(),
        )
    }

    /// `options` column JSON.
    pub fn options_json(&self) -> Value {
        json!({ "audit_records": self.options.audit_records })
    }

    /// Parses a collection definition (create, update, import or a stored
    /// row). `existing` provides defaults for an update (partial input).
    pub fn from_input(
        input: &Value,
        existing: Option<&Collection>,
    ) -> Result<Collection, ValidationErrors> {
        let mut errs = ValidationErrors::default();
        let Some(obj) = input.as_object() else {
            return Err(ValidationErrors::single(
                "",
                FieldError::new("invalid_type", "Must be an object."),
            ));
        };
        const KNOWN: &[&str] = &[
            "id", "name", "type", "fields", "indexes", "rules", "options", "created", "updated",
        ];
        for k in obj.keys().filter(|k| !KNOWN.contains(&k.as_str())) {
            errs.add(
                k.clone(),
                FieldError::new("unknown_field", "Unknown property."),
            );
        }

        let id = match (obj.get("id"), existing) {
            (_, Some(e)) => e.id,
            (None | Some(Value::Null), None) => Uuid::now_v7(),
            (Some(Value::String(s)), None) => Uuid::try_parse(s).unwrap_or_else(|_| {
                errs.add("id", FieldError::new("invalid_id", "Must be a UUID."));
                Uuid::nil()
            }),
            (Some(_), None) => {
                errs.add("id", FieldError::new("invalid_id", "Must be a UUID."));
                Uuid::nil()
            }
        };

        let name = match obj.get("name") {
            Some(Value::String(s)) => s.clone(),
            None if existing.is_some() => existing.map(|e| e.name.clone()).unwrap_or_default(),
            _ => {
                errs.add("name", FieldError::new("required", "Required."));
                String::new()
            }
        };

        let kind = match obj.get("type").and_then(Value::as_str) {
            None => existing.map_or(CollectionType::Base, |e| e.kind),
            Some("base") => CollectionType::Base,
            Some("auth") => {
                errs.add(
                    "type",
                    FieldError::new(
                        "unsupported_type",
                        "Auth collections are not available yet.",
                    ),
                );
                CollectionType::Auth
            }
            Some(_) => {
                errs.add(
                    "type",
                    FieldError::new("invalid_type", "One of base, auth."),
                );
                CollectionType::Base
            }
        };
        if let Some(e) = existing
            && e.kind != kind
        {
            errs.add(
                "type",
                FieldError::new("immutable", "The type of a collection cannot be changed."),
            );
        }

        let fields = match obj.get("fields") {
            None => existing.map(|e| e.fields.clone()).unwrap_or_default(),
            Some(Value::Array(items)) => items
                .iter()
                .enumerate()
                .filter_map(|(i, f)| Field::from_json(f, &format!("fields.{i}"), &mut errs))
                .collect(),
            Some(_) => {
                errs.add(
                    "fields",
                    FieldError::new("invalid_type", "Must be an array."),
                );
                Vec::new()
            }
        };

        let indexes = match obj.get("indexes") {
            None => existing.map(|e| e.indexes.clone()).unwrap_or_default(),
            Some(Value::Array(items)) => items
                .iter()
                .enumerate()
                .filter_map(|(i, v)| parse_index(v, &format!("indexes.{i}"), &mut errs))
                .collect(),
            Some(_) => {
                errs.add(
                    "indexes",
                    FieldError::new("invalid_type", "Must be an array."),
                );
                Vec::new()
            }
        };

        let mut rules = existing.map(|e| e.rules.clone()).unwrap_or_default();
        match obj.get("rules") {
            None => {}
            Some(Value::Object(r)) => {
                for k in r.keys() {
                    if !RuleOp::ALL.iter().any(|op| op.as_str() == k) {
                        errs.add(
                            format!("rules.{k}"),
                            FieldError::new("unknown_field", "Unknown rule."),
                        );
                    }
                }
                for op in RuleOp::ALL {
                    match r.get(op.as_str()) {
                        None => {}
                        Some(Value::Null) => rules.set(op, None),
                        Some(Value::String(s)) => rules.set(op, Some(s.clone())),
                        Some(_) => errs.add(
                            format!("rules.{}", op.as_str()),
                            FieldError::new("invalid_type", "Must be a string or null."),
                        ),
                    }
                }
            }
            Some(_) => errs.add(
                "rules",
                FieldError::new("invalid_type", "Must be an object."),
            ),
        }

        let mut options = existing.map(|e| e.options.clone()).unwrap_or_default();
        match obj.get("options") {
            None => {}
            Some(Value::Object(o)) => {
                for (k, v) in o {
                    match (k.as_str(), v) {
                        ("audit_records", Value::Bool(b)) => options.audit_records = *b,
                        ("audit_records", _) => {
                            errs.add(
                                "options.audit_records",
                                FieldError::new("invalid_type", "Must be a boolean."),
                            );
                        }
                        _ => errs.add(
                            format!("options.{k}"),
                            FieldError::new("unknown_field", "Unknown option."),
                        ),
                    }
                }
            }
            Some(_) => errs.add(
                "options",
                FieldError::new("invalid_type", "Must be an object."),
            ),
        }

        let text = |k: &str| obj.get(k).and_then(Value::as_str).map(str::to_owned);
        let collection = Collection {
            id,
            name,
            kind,
            fields,
            indexes,
            rules,
            options,
            created: existing
                .map(|e| e.created.clone())
                .or_else(|| text("created"))
                .unwrap_or_default(),
            updated: existing
                .map(|e| e.updated.clone())
                .or_else(|| text("updated"))
                .unwrap_or_default(),
        };
        if errs.is_empty() {
            Ok(collection)
        } else {
            Err(errs)
        }
    }

    /// Structural validation that needs no other collection: names,
    /// uniqueness, index references. Relation targets are checked by the
    /// caller against the schema.
    pub fn validate_shape(&self) -> ValidationErrors {
        let mut errs = ValidationErrors::default();
        if let Err(e) = check_collection_name(&self.name) {
            errs.add("name", e);
        }
        let mut names = HashSet::new();
        let mut ids = HashSet::new();
        for (i, f) in self.fields.iter().enumerate() {
            if let Err(e) = check_field_name(&f.name, &[]) {
                errs.add(format!("fields.{i}.name"), e);
            }
            if !names.insert(f.name.as_str()) {
                errs.add(
                    format!("fields.{i}.name"),
                    FieldError::new("duplicate", "Field names must be unique."),
                );
            }
            if !ids.insert(f.id) {
                errs.add(
                    format!("fields.{i}.id"),
                    FieldError::new("duplicate", "Field ids must be unique."),
                );
            }
        }
        let mut index_names = HashSet::new();
        for (i, idx) in self.indexes.iter().enumerate() {
            if !index_names.insert(idx.name.as_str()) {
                errs.add(
                    format!("indexes.{i}.name"),
                    FieldError::new("duplicate", "Index names must be unique."),
                );
            }
            for f in &idx.fields {
                let known = matches!(f.as_str(), "id" | "created" | "updated")
                    || self
                        .field(f)
                        .is_some_and(|fd| !matches!(fd.kind, FieldKind::Json { .. }));
                if !known {
                    errs.add(
                        format!("indexes.{i}.fields"),
                        FieldError::new("unknown_field", "Unknown or non-indexable field.")
                            .param("field", f.clone()),
                    );
                }
            }
        }
        errs
    }

    /// Fields that reference another collection.
    pub fn relation_fields(&self) -> impl Iterator<Item = &Field> {
        self.fields.iter().filter(|f| f.relation_target().is_some())
    }

    /// The SQL name of an index.
    pub fn index_sql_name(&self, index: &IndexDef) -> String {
        format!("idx_{}_{}", self.name, index.name)
    }
}

fn parse_index(v: &Value, path: &str, errs: &mut ValidationErrors) -> Option<IndexDef> {
    let Some(o) = v.as_object() else {
        errs.add(path, FieldError::new("invalid_type", "Must be an object."));
        return None;
    };
    let name = o
        .get("name")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned();
    let valid_name = name.chars().next().is_some_and(|c| c.is_ascii_lowercase())
        && name.len() <= 63
        && name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_');
    if !valid_name {
        errs.add(
            format!("{path}.name"),
            FieldError::new("invalid_name", "a-z, 0-9 and _, starting with a letter."),
        );
    }
    let fields: Vec<String> = o
        .get("fields")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|f| f.as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default();
    if fields.is_empty() {
        errs.add(
            format!("{path}.fields"),
            FieldError::new("required", "List at least one field."),
        );
    }
    let unique = o.get("unique").and_then(Value::as_bool).unwrap_or(false);
    for k in o
        .keys()
        .filter(|k| !["name", "fields", "unique"].contains(&k.as_str()))
    {
        errs.add(
            format!("{path}.{k}"),
            FieldError::new("unknown_field", "Unknown property."),
        );
    }
    valid_name.then_some(IndexDef {
        name,
        fields,
        unique,
    })
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    #[test]
    fn parses_defaults_and_round_trips() {
        let c = Collection::from_input(
            &json!({"name": "posts", "fields": [{"name": "title", "type": "text"}], "rules": {"view": ""}}),
            None,
        )
        .unwrap();
        assert_eq!(c.kind, CollectionType::Base);
        assert_eq!(c.rules.list, None, "locked by default");
        assert_eq!(c.rules.view.as_deref(), Some(""));
        assert!(c.options.audit_records);
        let again = Collection::from_input(&c.to_json(), None).unwrap();
        assert_eq!(again, c);
        assert!(c.validate_shape().is_empty());
    }

    #[test]
    fn update_keeps_unspecified_parts() {
        let c =
            Collection::from_input(&json!({"name": "posts", "rules": {"list": ""}}), None).unwrap();
        let u = Collection::from_input(&json!({"rules": {"view": "id != ''"}}), Some(&c)).unwrap();
        assert_eq!(u.id, c.id);
        assert_eq!(u.name, "posts");
        assert_eq!(u.rules.list.as_deref(), Some(""));
        assert_eq!(u.rules.view.as_deref(), Some("id != ''"));
    }

    #[test]
    fn shape_errors() {
        let c = Collection::from_input(
            &json!({"name": "select", "fields": [
                {"name": "a", "type": "text"}, {"name": "a", "type": "bool"}, {"name": "id", "type": "text"}
            ], "indexes": [{"name": "i", "fields": ["nope"]}]}),
            None,
        )
        .unwrap();
        let e = c.validate_shape();
        assert!(e.0.contains_key("name"));
        assert!(e.0.contains_key("fields.1.name"));
        assert!(e.0.contains_key("fields.2.name"));
        assert!(e.0.contains_key("indexes.0.fields"));
    }

    #[test]
    fn rejects_unknown_properties() {
        let e = Collection::from_input(
            &json!({"name": "x", "colour": 1, "rules": {"read": ""}}),
            None,
        )
        .unwrap_err();
        assert!(e.0.contains_key("colour"));
        assert!(e.0.contains_key("rules.read"));
    }
}
