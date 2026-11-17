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
    /// Options of auth collections (SPEC §8.6); `None` for base collections.
    pub auth: Option<AuthOptions>,
}

impl Default for CollectionOptions {
    fn default() -> Self {
        Self {
            audit_records: true,
            auth: None,
        }
    }
}

/// Login methods of an auth collection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AuthMethods {
    /// Email + password.
    pub password: bool,
    /// Passkeys (needs `app.public_url`).
    pub passkey: bool,
    /// Email one-time codes (needs SMTP).
    pub otp: bool,
    /// Magic links (needs SMTP).
    pub magic_link: bool,
}

/// Options of an auth collection (SPEC §8.6).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthOptions {
    /// Public sign-up through the create rule.
    pub signup_enabled: bool,
    /// Others may see `email` (the owner and superusers always can).
    pub email_visibility: bool,
    /// Enabled login methods.
    pub methods: AuthMethods,
    /// Minimum password length (the maximum is 256).
    pub password_min_length: u32,
    /// Refuse password login until the email is verified.
    pub require_verified_email_to_login: bool,
    /// Access token lifetime in seconds.
    pub access_ttl_s: i64,
    /// Refresh token lifetime in seconds.
    pub refresh_ttl_s: i64,
    /// Session idle timeout in seconds.
    pub idle_timeout_s: i64,
    /// OIDC claim to field mapping (`name`, `picture`).
    pub oidc_claims: std::collections::BTreeMap<String, String>,
}

impl Default for AuthOptions {
    fn default() -> Self {
        Self {
            signup_enabled: true,
            email_visibility: false,
            methods: AuthMethods {
                password: true,
                passkey: true,
                otp: true,
                magic_link: true,
            },
            password_min_length: 10,
            require_verified_email_to_login: false,
            access_ttl_s: 900,
            refresh_ttl_s: 2_592_000,
            idle_timeout_s: 1_209_600,
            oidc_claims: [("name", "name"), ("picture", "avatar")]
                .into_iter()
                .map(|(k, v)| (k.to_owned(), v.to_owned()))
                .collect(),
        }
    }
}

impl AuthOptions {
    fn to_json(&self) -> Value {
        json!({
            "signup_enabled": self.signup_enabled,
            "email_visibility": self.email_visibility,
            "methods": {
                "password": self.methods.password, "passkey": self.methods.passkey,
                "otp": self.methods.otp, "magic_link": self.methods.magic_link,
            },
            "password": {
                "min_length": self.password_min_length,
                "require_verified_email_to_login": self.require_verified_email_to_login,
            },
            "session": {
                "access_ttl_s": self.access_ttl_s, "refresh_ttl_s": self.refresh_ttl_s,
                "idle_timeout_s": self.idle_timeout_s,
            },
            "oidc_claims": self.oidc_claims,
        })
    }

    /// Applies one option from JSON; errors go under `options.<key>`.
    fn apply(&mut self, key: &str, v: &Value, errs: &mut ValidationErrors) {
        fn bad(errs: &mut ValidationErrors, path: &str, msg: &str) {
            errs.add(
                format!("options.{path}"),
                FieldError::new("invalid_option", msg),
            );
        }
        let obj = |v: &Value| v.as_object().cloned().unwrap_or_default();
        match key {
            "signup_enabled" | "email_visibility" => match v.as_bool() {
                Some(b) if key == "signup_enabled" => self.signup_enabled = b,
                Some(b) => self.email_visibility = b,
                None => bad(errs, key, "Must be a boolean."),
            },
            "methods" => {
                for (k, x) in obj(v) {
                    let Some(b) = x.as_bool() else {
                        bad(errs, &format!("methods.{k}"), "Must be a boolean.");
                        continue;
                    };
                    match k.as_str() {
                        "password" => self.methods.password = b,
                        "passkey" => self.methods.passkey = b,
                        "otp" => self.methods.otp = b,
                        "magic_link" => self.methods.magic_link = b,
                        _ => bad(errs, &format!("methods.{k}"), "Unknown method."),
                    }
                }
            }
            "password" => {
                for (k, x) in obj(v) {
                    match (k.as_str(), &x) {
                        ("min_length", x) => match x.as_u64().filter(|n| (8..=256).contains(n)) {
                            Some(n) => self.password_min_length = u32::try_from(n).unwrap_or(10),
                            None => bad(errs, "password.min_length", "Between 8 and 256."),
                        },
                        ("require_verified_email_to_login", Value::Bool(b)) => {
                            self.require_verified_email_to_login = *b;
                        }
                        _ => bad(errs, &format!("password.{k}"), "Unknown or invalid option."),
                    }
                }
            }
            "session" => {
                for (k, x) in obj(v) {
                    let n = x.as_i64().filter(|n| *n >= 60);
                    match (k.as_str(), n) {
                        ("access_ttl_s", Some(n)) => self.access_ttl_s = n,
                        ("refresh_ttl_s", Some(n)) => self.refresh_ttl_s = n,
                        ("idle_timeout_s", Some(n)) => self.idle_timeout_s = n,
                        _ => bad(errs, &format!("session.{k}"), "Seconds, at least 60."),
                    }
                }
            }
            "oidc_claims" => {
                for (k, x) in obj(v) {
                    match x.as_str() {
                        Some(f) => {
                            self.oidc_claims.insert(k, f.to_owned());
                        }
                        None => bad(errs, &format!("oidc_claims.{k}"), "Must be a field name."),
                    }
                }
            }
            _ => errs.add(
                format!("options.{key}"),
                FieldError::new("unknown_field", "Unknown option."),
            ),
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
        let mut v = json!({ "audit_records": self.options.audit_records });
        if let (Some(auth), Value::Object(m)) = (&self.options.auth, &mut v)
            && let Value::Object(a) = auth.to_json()
        {
            m.extend(a);
        }
        v
    }

    /// Auth options, if this is an auth collection.
    pub fn auth(&self) -> Option<&AuthOptions> {
        self.options.auth.as_ref()
    }

    /// Name of the unique index on `email` of an auth collection. It carries
    /// the collection id so it survives renames and the trash.
    pub fn email_index_name(&self) -> String {
        format!("sys_email_{}", self.id.simple())
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
            Some("auth") => CollectionType::Auth,
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

        let mut fields: Vec<Field> = match obj.get("fields") {
            None => existing.map(|e| e.fields.clone()).unwrap_or_default(),
            Some(Value::Array(items)) => items
                .iter()
                .enumerate()
                // System fields are canonical: echoes of them are ignored.
                .filter(|(_, f)| f.get("system").and_then(Value::as_bool) != Some(true))
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

        // Auth collections always start with their system fields.
        fields.retain(|f| !f.system);
        if kind == CollectionType::Auth {
            let mut all = crate::domain::field::auth_system_fields();
            all.append(&mut fields);
            fields = all;
        }

        let mut options = existing.map(|e| e.options.clone()).unwrap_or_default();
        if kind == CollectionType::Auth && options.auth.is_none() {
            options.auth = Some(AuthOptions::default());
        }
        if kind == CollectionType::Base {
            options.auth = None;
        }
        match obj.get("options") {
            None => {}
            Some(Value::Object(o)) => {
                for (k, v) in o {
                    match (k.as_str(), v, options.auth.as_mut()) {
                        ("audit_records", Value::Bool(b), _) => options.audit_records = *b,
                        ("audit_records", _, _) => errs.add(
                            "options.audit_records",
                            FieldError::new("invalid_type", "Must be a boolean."),
                        ),
                        (_, v, Some(auth)) => auth.apply(k, v, &mut errs),
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
        let reserved: &[&str] = if self.kind == CollectionType::Auth {
            crate::domain::field::AUTH_RESERVED
        } else {
            &[]
        };
        for (i, f) in self.fields.iter().enumerate() {
            if f.system {
                continue;
            }
            if let Err(e) = check_field_name(&f.name, reserved) {
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
