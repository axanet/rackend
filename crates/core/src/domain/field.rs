//! Field definitions, their JSON form, and value validation (SPEC §8.4).

use std::sync::Arc;

use serde_json::{Map, Value, json};
use uuid::Uuid;

use super::value::{is_empty, normalize_date, number_json};
use crate::error::{FieldError, ValidationErrors};

/// Default `max` of text fields, in characters.
pub const TEXT_MAX_DEFAULT: u32 = 10_000;
/// Default `max_size` of json fields, in bytes.
pub const JSON_MAX_DEFAULT: u64 = 1024 * 1024;

/// What a relation does when its target record is deleted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum OnDelete {
    /// Remove the reference (default).
    #[default]
    SetNull,
    /// Delete the referencing record too.
    Cascade,
    /// Refuse to delete the target.
    Restrict,
}

impl OnDelete {
    fn as_str(self) -> &'static str {
        match self {
            Self::SetNull => "set_null",
            Self::Cascade => "cascade",
            Self::Restrict => "restrict",
        }
    }
}

/// A compiled `pattern` option.
#[derive(Debug, Clone)]
pub struct Pattern {
    /// Source text.
    pub source: String,
    /// Compiled regex.
    pub regex: Arc<regex::Regex>,
}

impl PartialEq for Pattern {
    fn eq(&self, other: &Self) -> bool {
        self.source == other.source
    }
}

/// Type-specific options.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum FieldKind {
    /// Free text.
    Text {
        required: bool,
        min: Option<u32>,
        max: u32,
        pattern: Option<Pattern>,
    },
    /// An email address.
    Email {
        required: bool,
        only_domains: Vec<String>,
        except_domains: Vec<String>,
    },
    /// A URL.
    Url {
        required: bool,
        schemes: Vec<String>,
    },
    /// A number (stored as REAL).
    Number {
        required: bool,
        min: Option<f64>,
        max: Option<f64>,
        integer_only: bool,
    },
    /// A boolean.
    Bool,
    /// A date-time.
    Date {
        required: bool,
        min: Option<String>,
        max: Option<String>,
    },
    /// A server-managed timestamp.
    Autodate { on_create: bool, on_update: bool },
    /// One or more values from a fixed list.
    Select {
        required: bool,
        values: Vec<String>,
        max_select: u32,
    },
    /// Any JSON value.
    Json { max_size: u64 },
    /// Reference(s) to records of another collection.
    Relation {
        required: bool,
        collection_id: Uuid,
        max_select: u32,
        min_select: u32,
        on_delete: OnDelete,
    },
}

/// The scalar type of a field's values, for the rule type checker.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ValueType {
    /// Text (text, email, url, select).
    Text,
    /// Number.
    Number,
    /// Boolean.
    Bool,
    /// Date-time (stored as canonical RFC 3339 text).
    Date,
    /// Any JSON value.
    Any,
    /// Id of a record in the given collection.
    Relation(Uuid),
}

/// A field of a collection.
#[derive(Debug, Clone, PartialEq)]
pub struct Field {
    /// Immutable id; renames keep it.
    pub id: Uuid,
    /// Column name.
    pub name: String,
    /// Type and options.
    pub kind: FieldKind,
}

impl Field {
    /// The `type` string.
    pub fn type_name(&self) -> &'static str {
        match self.kind {
            FieldKind::Text { .. } => "text",
            FieldKind::Email { .. } => "email",
            FieldKind::Url { .. } => "url",
            FieldKind::Number { .. } => "number",
            FieldKind::Bool => "bool",
            FieldKind::Date { .. } => "date",
            FieldKind::Autodate { .. } => "autodate",
            FieldKind::Select { .. } => "select",
            FieldKind::Json { .. } => "json",
            FieldKind::Relation { .. } => "relation",
        }
    }

    /// Whether values are stored as a JSON array.
    pub fn is_multi(&self) -> bool {
        match self.kind {
            FieldKind::Select { max_select, .. } | FieldKind::Relation { max_select, .. } => {
                max_select > 1
            }
            _ => false,
        }
    }

    /// Whether the field must be non-empty.
    pub fn required(&self) -> bool {
        match self.kind {
            FieldKind::Text { required, .. }
            | FieldKind::Email { required, .. }
            | FieldKind::Url { required, .. }
            | FieldKind::Number { required, .. }
            | FieldKind::Date { required, .. }
            | FieldKind::Select { required, .. }
            | FieldKind::Relation { required, .. } => required,
            _ => false,
        }
    }

    /// Scalar type of the values.
    pub fn value_type(&self) -> ValueType {
        match &self.kind {
            FieldKind::Text { .. }
            | FieldKind::Email { .. }
            | FieldKind::Url { .. }
            | FieldKind::Select { .. } => ValueType::Text,
            FieldKind::Number { .. } => ValueType::Number,
            FieldKind::Bool => ValueType::Bool,
            FieldKind::Date { .. } | FieldKind::Autodate { .. } => ValueType::Date,
            FieldKind::Json { .. } => ValueType::Any,
            FieldKind::Relation { collection_id, .. } => ValueType::Relation(*collection_id),
        }
    }

    /// The relation target, if this is a relation field.
    pub fn relation_target(&self) -> Option<Uuid> {
        match self.kind {
            FieldKind::Relation { collection_id, .. } => Some(collection_id),
            _ => None,
        }
    }

    /// The SQL column type.
    pub fn sql_type(&self) -> &'static str {
        match self.kind {
            FieldKind::Number { .. } => "REAL",
            FieldKind::Bool => "INTEGER",
            _ => "TEXT",
        }
    }

    /// The JSON form used by the API and stored in `_collections.fields`.
    pub fn to_json(&self) -> Value {
        let mut m = Map::new();
        m.insert("id".into(), json!(self.id));
        m.insert("name".into(), json!(self.name));
        m.insert("type".into(), json!(self.type_name()));
        match &self.kind {
            FieldKind::Text {
                required,
                min,
                max,
                pattern,
            } => {
                m.insert("required".into(), json!(required));
                m.insert("min".into(), json!(min));
                m.insert("max".into(), json!(max));
                m.insert("pattern".into(), json!(pattern.as_ref().map(|p| &p.source)));
            }
            FieldKind::Email {
                required,
                only_domains,
                except_domains,
            } => {
                m.insert("required".into(), json!(required));
                m.insert("only_domains".into(), json!(only_domains));
                m.insert("except_domains".into(), json!(except_domains));
            }
            FieldKind::Url { required, schemes } => {
                m.insert("required".into(), json!(required));
                m.insert("schemes".into(), json!(schemes));
            }
            FieldKind::Number {
                required,
                min,
                max,
                integer_only,
            } => {
                m.insert("required".into(), json!(required));
                m.insert("min".into(), min.map_or(Value::Null, number_json));
                m.insert("max".into(), max.map_or(Value::Null, number_json));
                m.insert("integer_only".into(), json!(integer_only));
            }
            FieldKind::Bool => {}
            FieldKind::Date { required, min, max } => {
                m.insert("required".into(), json!(required));
                m.insert("min".into(), json!(min));
                m.insert("max".into(), json!(max));
            }
            FieldKind::Autodate {
                on_create,
                on_update,
            } => {
                m.insert("on_create".into(), json!(on_create));
                m.insert("on_update".into(), json!(on_update));
            }
            FieldKind::Select {
                required,
                values,
                max_select,
            } => {
                m.insert("required".into(), json!(required));
                m.insert("values".into(), json!(values));
                m.insert("max_select".into(), json!(max_select));
            }
            FieldKind::Json { max_size } => {
                m.insert("max_size".into(), json!(max_size));
            }
            FieldKind::Relation {
                required,
                collection_id,
                max_select,
                min_select,
                on_delete,
            } => {
                m.insert("required".into(), json!(required));
                m.insert("collection_id".into(), json!(collection_id));
                m.insert("max_select".into(), json!(max_select));
                m.insert("min_select".into(), json!(min_select));
                m.insert("on_delete".into(), json!(on_delete.as_str()));
            }
        }
        Value::Object(m)
    }

    /// Parses a field definition. Errors are recorded under `path`
    /// (e.g. `fields.2`) and `None` is returned.
    pub fn from_json(v: &Value, path: &str, errs: &mut ValidationErrors) -> Option<Self> {
        let Some(obj) = v.as_object() else {
            errs.add(
                path,
                FieldError::new("invalid_type", "A field must be an object."),
            );
            return None;
        };
        let mut p = Opts {
            obj,
            path,
            errs,
            used: vec!["id", "name", "type"],
        };
        let id = match obj.get("id") {
            None | Some(Value::Null) => Uuid::now_v7(),
            Some(Value::String(s)) => match Uuid::try_parse(s) {
                Ok(id) => id,
                Err(_) => {
                    p.err("id", FieldError::new("invalid_id", "Must be a UUID."));
                    return None;
                }
            },
            Some(_) => {
                p.err("id", FieldError::new("invalid_id", "Must be a UUID."));
                return None;
            }
        };
        let Some(name) = obj.get("name").and_then(Value::as_str) else {
            p.err("name", FieldError::new("required", "Required."));
            return None;
        };
        let Some(ty) = obj.get("type").and_then(Value::as_str) else {
            p.err("type", FieldError::new("required", "Required."));
            return None;
        };
        let kind = match ty {
            "text" => {
                let required = p.bool("required", false);
                let min = p.u32("min");
                let max = p.u32("max").unwrap_or(TEXT_MAX_DEFAULT);
                let pattern = p.str("pattern").filter(|s| !s.is_empty()).and_then(|src| {
                    match regex::Regex::new(&src) {
                        Ok(r) => Some(Pattern {
                            source: src,
                            regex: Arc::new(r),
                        }),
                        Err(e) => {
                            p.err(
                                "pattern",
                                FieldError::new(
                                    "invalid_pattern",
                                    format!("Invalid regular expression: {e}"),
                                ),
                            );
                            None
                        }
                    }
                });
                if max == 0 {
                    p.err(
                        "max",
                        FieldError::new("invalid_option", "Must be at least 1."),
                    );
                }
                if min.is_some_and(|m| m > max) {
                    p.err(
                        "min",
                        FieldError::new("invalid_option", "Must not be greater than max."),
                    );
                }
                FieldKind::Text {
                    required,
                    min,
                    max,
                    pattern,
                }
            }
            "email" => FieldKind::Email {
                required: p.bool("required", false),
                only_domains: p
                    .str_list("only_domains")
                    .into_iter()
                    .map(|d| d.to_ascii_lowercase())
                    .collect(),
                except_domains: p
                    .str_list("except_domains")
                    .into_iter()
                    .map(|d| d.to_ascii_lowercase())
                    .collect(),
            },
            "url" => {
                let required = p.bool("required", false);
                let mut schemes: Vec<String> = p
                    .str_list("schemes")
                    .into_iter()
                    .map(|s| s.to_ascii_lowercase())
                    .collect();
                if schemes.is_empty() {
                    schemes = vec!["https".into(), "http".into()];
                }
                FieldKind::Url { required, schemes }
            }
            "number" => {
                let required = p.bool("required", false);
                let min = p.f64("min");
                let max = p.f64("max");
                if let (Some(a), Some(b)) = (min, max)
                    && a > b
                {
                    p.err(
                        "min",
                        FieldError::new("invalid_option", "Must not be greater than max."),
                    );
                }
                FieldKind::Number {
                    required,
                    min,
                    max,
                    integer_only: p.bool("integer_only", false),
                }
            }
            "bool" => FieldKind::Bool,
            "date" => {
                let required = p.bool("required", false);
                let date = |key: &'static str, p: &mut Opts<'_, '_>| {
                    p.str(key)
                        .filter(|s| !s.is_empty())
                        .and_then(|s| match normalize_date(&s) {
                            Some(d) => Some(d),
                            None => {
                                p.err(key, FieldError::new("invalid_date", "Must be a date."));
                                None
                            }
                        })
                };
                let min = date("min", &mut p);
                let max = date("max", &mut p);
                FieldKind::Date { required, min, max }
            }
            "autodate" => {
                let on_create = p.bool("on_create", true);
                let on_update = p.bool("on_update", false);
                if !on_create && !on_update {
                    p.err(
                        "on_create",
                        FieldError::new("invalid_option", "Set on_create, on_update or both."),
                    );
                }
                FieldKind::Autodate {
                    on_create,
                    on_update,
                }
            }
            "select" => {
                let required = p.bool("required", false);
                let values = p.str_list("values");
                if values.is_empty() {
                    p.err(
                        "values",
                        FieldError::new("required", "List at least one value."),
                    );
                }
                let mut seen = std::collections::HashSet::new();
                if values.iter().any(|v| !seen.insert(v)) {
                    p.err(
                        "values",
                        FieldError::new("duplicate", "Values must be unique."),
                    );
                }
                let max_select = p.u32("max_select").unwrap_or(1).max(1);
                FieldKind::Select {
                    required,
                    values,
                    max_select,
                }
            }
            "json" => FieldKind::Json {
                max_size: p.u64("max_size").unwrap_or(JSON_MAX_DEFAULT).max(1),
            },
            "relation" => {
                let required = p.bool("required", false);
                let collection_id = match p.str("collection_id").map(|s| Uuid::try_parse(&s)) {
                    Some(Ok(id)) => id,
                    _ => {
                        p.err(
                            "collection_id",
                            FieldError::new("required", "The id of the target collection."),
                        );
                        Uuid::nil()
                    }
                };
                let max_select = p.u32("max_select").unwrap_or(1).max(1);
                let min_select = p.u32("min_select").unwrap_or(0);
                if min_select > max_select {
                    p.err(
                        "min_select",
                        FieldError::new("invalid_option", "Must not be greater than max_select."),
                    );
                }
                let on_delete = match p.str("on_delete").as_deref() {
                    None | Some("set_null") => OnDelete::SetNull,
                    Some("cascade") => OnDelete::Cascade,
                    Some("restrict") => OnDelete::Restrict,
                    Some(_) => {
                        p.err(
                            "on_delete",
                            FieldError::new(
                                "invalid_option",
                                "One of set_null, cascade, restrict.",
                            ),
                        );
                        OnDelete::SetNull
                    }
                };
                FieldKind::Relation {
                    required,
                    collection_id,
                    max_select,
                    min_select,
                    on_delete,
                }
            }
            "file" | "ydoc" => {
                p.err(
                    "type",
                    FieldError::new(
                        "unsupported_type",
                        format!("The {ty} type is not available yet."),
                    ),
                );
                return None;
            }
            _ => {
                p.err(
                    "type",
                    FieldError::new("invalid_type", "Unknown field type.").param("type", ty),
                );
                return None;
            }
        };
        p.reject_unknown();
        let failed = p.failed();
        if failed {
            return None;
        }
        Some(Self {
            id,
            name: name.to_owned(),
            kind,
        })
    }

    /// Validates and normalizes an input value. `None` input means absent.
    /// Returns the value to store (JSON form) or a field error.
    pub fn normalize(&self, input: &Value) -> Result<Value, FieldError> {
        let v = match (&self.kind, input) {
            (FieldKind::Autodate { .. }, _) => {
                return Err(FieldError::new(
                    "read_only",
                    "This field is set by the server.",
                ));
            }
            (FieldKind::Bool, Value::Null) => Value::Bool(false),
            (FieldKind::Bool, Value::Bool(b)) => Value::Bool(*b),
            (FieldKind::Bool, _) => return Err(type_err("a boolean")),
            (FieldKind::Json { max_size }, v) => {
                let size = v.to_string().len() as u64;
                if size > *max_size {
                    return Err(FieldError::new(
                        "too_large",
                        format!("Must be at most {max_size} bytes."),
                    )
                    .param("max", *max_size));
                }
                v.clone()
            }
            (_, Value::Null) => Value::Null,
            (
                FieldKind::Text {
                    min, max, pattern, ..
                },
                Value::String(s),
            ) => {
                let n = s.chars().count();
                if n > *max as usize {
                    return Err(FieldError::new(
                        "too_long",
                        format!("Must be at most {max} characters."),
                    )
                    .param("max", *max));
                }
                if !s.is_empty() {
                    if let Some(min) = min
                        && n < *min as usize
                    {
                        return Err(FieldError::new(
                            "too_short",
                            format!("Must be at least {min} characters."),
                        )
                        .param("min", *min));
                    }
                    if let Some(p) = pattern
                        && !p.regex.is_match(s)
                    {
                        return Err(FieldError::new(
                            "invalid_format",
                            "Does not match the required pattern.",
                        )
                        .param("pattern", p.source.clone()));
                    }
                }
                Value::String(s.clone())
            }
            (
                FieldKind::Email {
                    only_domains,
                    except_domains,
                    ..
                },
                Value::String(s),
            ) => {
                if s.is_empty() {
                    Value::String(String::new())
                } else {
                    let Some(domain) = email_domain(s) else {
                        return Err(FieldError::new(
                            "invalid_email",
                            "Must be a valid email address.",
                        ));
                    };
                    if !only_domains.is_empty() && !only_domains.iter().any(|d| d == &domain) {
                        return Err(FieldError::new(
                            "domain_not_allowed",
                            "This email domain is not allowed.",
                        ));
                    }
                    if except_domains.iter().any(|d| d == &domain) {
                        return Err(FieldError::new(
                            "domain_not_allowed",
                            "This email domain is not allowed.",
                        ));
                    }
                    Value::String(s.clone())
                }
            }
            (FieldKind::Url { schemes, .. }, Value::String(s)) => {
                if s.is_empty() {
                    Value::String(String::new())
                } else {
                    check_url(s, schemes)?;
                    Value::String(s.clone())
                }
            }
            (
                FieldKind::Number {
                    min,
                    max,
                    integer_only,
                    ..
                },
                Value::Number(n),
            ) => {
                let Some(f) = n.as_f64().filter(|f| f.is_finite()) else {
                    return Err(type_err("a number"));
                };
                if *integer_only && f.fract() != 0.0 {
                    return Err(FieldError::new("not_integer", "Must be a whole number."));
                }
                if let Some(m) = min
                    && f < *m
                {
                    return Err(
                        FieldError::new("too_small", format!("Must be at least {m}."))
                            .param("min", number_json(*m)),
                    );
                }
                if let Some(m) = max
                    && f > *m
                {
                    return Err(
                        FieldError::new("too_large", format!("Must be at most {m}."))
                            .param("max", number_json(*m)),
                    );
                }
                number_json(f)
            }
            (FieldKind::Date { min, max, .. }, Value::String(s)) => {
                if s.is_empty() {
                    Value::Null
                } else {
                    let Some(d) = normalize_date(s) else {
                        return Err(FieldError::new(
                            "invalid_date",
                            "Must be a date (RFC 3339).",
                        ));
                    };
                    if let Some(m) = min
                        && d < *m
                    {
                        return Err(FieldError::new(
                            "too_early",
                            format!("Must be at or after {m}."),
                        )
                        .param("min", m.clone()));
                    }
                    if let Some(m) = max
                        && d > *m
                    {
                        return Err(FieldError::new(
                            "too_late",
                            format!("Must be at or before {m}."),
                        )
                        .param("max", m.clone()));
                    }
                    Value::String(d)
                }
            }
            (
                FieldKind::Select {
                    values, max_select, ..
                },
                v,
            ) => {
                let picked = string_list(v, *max_select > 1).ok_or_else(|| {
                    type_err(if *max_select > 1 {
                        "an array of strings"
                    } else {
                        "a string"
                    })
                })?;
                for p in &picked {
                    if !values.contains(p) {
                        return Err(FieldError::new(
                            "invalid_value",
                            "Not one of the allowed values.",
                        )
                        .param("value", p.clone()));
                    }
                }
                multi_result(picked, *max_select)?
            }
            (
                FieldKind::Relation {
                    max_select,
                    min_select,
                    ..
                },
                v,
            ) => {
                let ids = string_list(v, *max_select > 1).ok_or_else(|| {
                    type_err(if *max_select > 1 {
                        "an array of record ids"
                    } else {
                        "a record id"
                    })
                })?;
                let mut out = Vec::with_capacity(ids.len());
                for id in ids {
                    match Uuid::try_parse(&id) {
                        Ok(u) => out.push(u.to_string()),
                        Err(_) => {
                            return Err(FieldError::new("invalid_id", "Must be a record id.")
                                .param("value", id));
                        }
                    }
                }
                if *max_select > 1 && !out.is_empty() && (out.len() as u32) < *min_select {
                    return Err(FieldError::new(
                        "too_few",
                        format!("Select at least {min_select}."),
                    )
                    .param("min", *min_select));
                }
                multi_result(out, *max_select)?
            }
            (
                FieldKind::Text { .. }
                | FieldKind::Email { .. }
                | FieldKind::Url { .. }
                | FieldKind::Date { .. },
                _,
            ) => {
                return Err(type_err("a string"));
            }
            (FieldKind::Number { .. }, _) => return Err(type_err("a number")),
        };
        Ok(v)
    }

    /// Checks `required` on a normalized value.
    pub fn check_required(&self, v: &Value) -> Result<(), FieldError> {
        if self.required() && is_empty(v) {
            return Err(FieldError::new("required", "Required."));
        }
        Ok(())
    }
}

fn type_err(what: &str) -> FieldError {
    FieldError::new("invalid_type", format!("Must be {what}."))
}

/// Strings of a select/relation value. Multi fields take an array (a single
/// string is accepted and wrapped); single fields take a string.
fn string_list(v: &Value, multi: bool) -> Option<Vec<String>> {
    match v {
        Value::String(s) if s.is_empty() => Some(Vec::new()),
        Value::String(s) => Some(vec![s.clone()]),
        Value::Array(items) if multi => items
            .iter()
            .map(|i| i.as_str().map(str::to_owned))
            .collect(),
        _ => None,
    }
}

fn multi_result(mut items: Vec<String>, max_select: u32) -> Result<Value, FieldError> {
    if max_select <= 1 {
        return Ok(items.pop().map_or(Value::Null, Value::String));
    }
    let mut seen = std::collections::HashSet::new();
    items.retain(|i| seen.insert(i.clone()));
    if items.len() > max_select as usize {
        return Err(
            FieldError::new("too_many", format!("Select at most {max_select}."))
                .param("max", max_select),
        );
    }
    Ok(Value::Array(items.into_iter().map(Value::String).collect()))
}

/// The lowercased domain of a plausible email address.
fn email_domain(s: &str) -> Option<String> {
    if s.len() > 254 || s.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return None;
    }
    let (local, domain) = s.rsplit_once('@')?;
    if local.is_empty() || local.contains('@') || domain.len() < 3 || !domain.contains('.') {
        return None;
    }
    if domain.starts_with('.') || domain.ends_with('.') || domain.contains("..") {
        return None;
    }
    Some(domain.to_ascii_lowercase())
}

fn check_url(s: &str, schemes: &[String]) -> Result<(), FieldError> {
    let bad = || FieldError::new("invalid_url", "Must be a valid URL.");
    if s.len() > 2048 || s.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return Err(bad());
    }
    let (scheme, rest) = s.split_once("://").ok_or_else(bad)?;
    let scheme = scheme.to_ascii_lowercase();
    if scheme.is_empty()
        || !scheme
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'))
    {
        return Err(bad());
    }
    if !schemes.contains(&scheme) {
        return Err(
            FieldError::new("scheme_not_allowed", "This URL scheme is not allowed.")
                .param("schemes", schemes.to_vec()),
        );
    }
    let host = rest.split(['/', '?', '#']).next().unwrap_or_default();
    let host = host.rsplit_once('@').map_or(host, |(_, h)| h);
    if host.is_empty() {
        return Err(bad());
    }
    Ok(())
}

/// Reads options from a field object and records errors.
struct Opts<'a, 'e> {
    obj: &'a Map<String, Value>,
    path: &'a str,
    errs: &'e mut ValidationErrors,
    used: Vec<&'static str>,
}

impl Opts<'_, '_> {
    fn err(&mut self, key: &str, e: FieldError) {
        self.errs.add(format!("{}.{key}", self.path), e);
    }

    fn failed(&self) -> bool {
        self.errs
            .0
            .keys()
            .any(|k| k.starts_with(&format!("{}.", self.path)) || k == self.path)
    }

    fn get(&mut self, key: &'static str) -> Option<&Value> {
        self.used.push(key);
        self.obj.get(key).filter(|v| !v.is_null())
    }

    fn bool(&mut self, key: &'static str, default: bool) -> bool {
        match self.get(key).cloned() {
            None => default,
            Some(Value::Bool(b)) => b,
            Some(_) => {
                self.err(key, type_err("a boolean"));
                default
            }
        }
    }

    fn u64(&mut self, key: &'static str) -> Option<u64> {
        match self.get(key).cloned() {
            None => None,
            Some(v) => match v.as_u64() {
                Some(n) => Some(n),
                None => {
                    self.err(key, type_err("a non-negative integer"));
                    None
                }
            },
        }
    }

    fn u32(&mut self, key: &'static str) -> Option<u32> {
        self.u64(key).and_then(|n| match u32::try_from(n) {
            Ok(n) => Some(n),
            Err(_) => {
                self.err(key, FieldError::new("invalid_option", "Too large."));
                None
            }
        })
    }

    fn f64(&mut self, key: &'static str) -> Option<f64> {
        match self.get(key).cloned() {
            None => None,
            Some(v) => match v.as_f64() {
                Some(n) => Some(n),
                None => {
                    self.err(key, type_err("a number"));
                    None
                }
            },
        }
    }

    fn str(&mut self, key: &'static str) -> Option<String> {
        match self.get(key).cloned() {
            None => None,
            Some(Value::String(s)) => Some(s),
            Some(_) => {
                self.err(key, type_err("a string"));
                None
            }
        }
    }

    fn str_list(&mut self, key: &'static str) -> Vec<String> {
        match self.get(key).cloned() {
            None => Vec::new(),
            Some(Value::Array(items)) => {
                let out: Option<Vec<String>> = items
                    .iter()
                    .map(|i| i.as_str().map(str::to_owned))
                    .collect();
                out.unwrap_or_else(|| {
                    self.err(key, type_err("an array of strings"));
                    Vec::new()
                })
            }
            Some(_) => {
                self.err(key, type_err("an array of strings"));
                Vec::new()
            }
        }
    }

    fn reject_unknown(&mut self) {
        let unknown: Vec<String> = self
            .obj
            .keys()
            .filter(|k| !self.used.contains(&k.as_str()))
            .cloned()
            .collect();
        for k in unknown {
            self.errs.add(
                format!("{}.{k}", self.path),
                FieldError::new("unknown_option", "Unknown option for this field type."),
            );
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    fn field(v: Value) -> Result<Field, ValidationErrors> {
        let mut errs = ValidationErrors::default();
        Field::from_json(&v, "fields.0", &mut errs).ok_or(errs)
    }

    #[test]
    fn parses_and_round_trips() {
        let f =
            field(json!({"name": "title", "type": "text", "required": true, "max": 200})).unwrap();
        let back = field(f.to_json()).unwrap();
        assert_eq!(f, back);
        assert_eq!(f.to_json()["max"], 200);
    }

    #[test]
    fn rejects_unknown_options_and_bad_values() {
        let e = field(json!({"name": "t", "type": "text", "colour": "red"})).unwrap_err();
        assert!(e.0.contains_key("fields.0.colour"));
        let e = field(json!({"name": "t", "type": "text", "pattern": "("})).unwrap_err();
        assert_eq!(e.0["fields.0.pattern"][0].code, "invalid_pattern");
        let e = field(json!({"name": "t", "type": "warp"})).unwrap_err();
        assert_eq!(e.0["fields.0.type"][0].code, "invalid_type");
        let e = field(json!({"name": "t", "type": "file"})).unwrap_err();
        assert_eq!(e.0["fields.0.type"][0].code, "unsupported_type");
    }

    #[test]
    fn text_validation() {
        let f =
            field(json!({"name": "t", "type": "text", "min": 2, "max": 4, "pattern": "^[a-z]+$"}))
                .unwrap();
        assert_eq!(f.normalize(&json!("abc")).unwrap(), json!("abc"));
        assert_eq!(
            f.normalize(&json!("")).unwrap(),
            json!(""),
            "empty skips min/pattern"
        );
        assert_eq!(f.normalize(&json!("a")).unwrap_err().code, "too_short");
        assert_eq!(f.normalize(&json!("abcde")).unwrap_err().code, "too_long");
        assert_eq!(
            f.normalize(&json!("AB")).unwrap_err().code,
            "invalid_format"
        );
        assert_eq!(f.normalize(&json!(3)).unwrap_err().code, "invalid_type");
    }

    #[test]
    fn number_bool_date() {
        let n = field(
            json!({"name": "n", "type": "number", "min": 0, "max": 10, "integer_only": true}),
        )
        .unwrap();
        assert_eq!(n.normalize(&json!(3.0)).unwrap(), json!(3));
        assert_eq!(n.normalize(&json!(2.5)).unwrap_err().code, "not_integer");
        assert_eq!(n.normalize(&json!(11)).unwrap_err().code, "too_large");
        let b = field(json!({"name": "b", "type": "bool"})).unwrap();
        assert_eq!(b.normalize(&Value::Null).unwrap(), json!(false));
        assert!(b.check_required(&json!(false)).is_ok());
        let d = field(json!({"name": "d", "type": "date", "min": "2026-01-01"})).unwrap();
        assert_eq!(
            d.normalize(&json!("2026-02-01")).unwrap(),
            json!("2026-02-01T00:00:00.000Z")
        );
        assert_eq!(
            d.normalize(&json!("2025-02-01")).unwrap_err().code,
            "too_early"
        );
    }

    #[test]
    fn select_and_relation_cardinality() {
        let s = field(
            json!({"name": "s", "type": "select", "values": ["a", "b", "c"], "max_select": 2}),
        )
        .unwrap();
        assert!(s.is_multi());
        assert_eq!(
            s.normalize(&json!(["a", "a", "b"])).unwrap(),
            json!(["a", "b"])
        );
        assert_eq!(
            s.normalize(&json!(["a", "b", "c"])).unwrap_err().code,
            "too_many"
        );
        assert_eq!(
            s.normalize(&json!(["x"])).unwrap_err().code,
            "invalid_value"
        );
        let single = field(json!({"name": "s", "type": "select", "values": ["a"]})).unwrap();
        assert_eq!(single.normalize(&json!("a")).unwrap(), json!("a"));
        assert_eq!(
            single.normalize(&json!(["a"])).unwrap_err().code,
            "invalid_type"
        );

        let target = Uuid::now_v7();
        let r = field(json!({"name": "r", "type": "relation", "collection_id": target, "max_select": 3, "min_select": 2}))
            .unwrap();
        let id = Uuid::now_v7().to_string();
        assert_eq!(r.normalize(&json!([id])).unwrap_err().code, "too_few");
        assert_eq!(
            r.normalize(&json!(["nope"])).unwrap_err().code,
            "invalid_id"
        );
    }

    #[test]
    fn email_and_url() {
        let e =
            field(json!({"name": "e", "type": "email", "only_domains": ["Example.com"]})).unwrap();
        assert!(e.normalize(&json!("a@example.com")).is_ok());
        assert_eq!(
            e.normalize(&json!("a@other.com")).unwrap_err().code,
            "domain_not_allowed"
        );
        assert_eq!(
            e.normalize(&json!("not an email")).unwrap_err().code,
            "invalid_email"
        );
        let u = field(json!({"name": "u", "type": "url"})).unwrap();
        assert!(u.normalize(&json!("https://example.com/x?y")).is_ok());
        assert_eq!(
            u.normalize(&json!("ftp://example.com")).unwrap_err().code,
            "scheme_not_allowed"
        );
        assert_eq!(
            u.normalize(&json!("https://")).unwrap_err().code,
            "invalid_url"
        );
    }

    #[test]
    fn json_size_and_autodate() {
        let j = field(json!({"name": "j", "type": "json", "max_size": 10})).unwrap();
        assert_eq!(
            j.normalize(&json!({"a": "long string"})).unwrap_err().code,
            "too_large"
        );
        let a = field(json!({"name": "a", "type": "autodate", "on_update": true})).unwrap();
        assert_eq!(
            a.normalize(&json!("2026-01-01")).unwrap_err().code,
            "read_only"
        );
    }
}
