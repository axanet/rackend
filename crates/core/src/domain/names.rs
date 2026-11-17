//! Naming rules for collections and fields (SPEC §8.2).

use crate::error::FieldError;

/// Collection names that clash with API routes.
const RESERVED_COLLECTIONS: &[&str] = &[
    "collections",
    "settings",
    "files",
    "realtime",
    "auth",
    "batch",
    "health",
    "superusers",
];

/// Field names managed by the system.
pub const RESERVED_FIELDS: &[&str] = &[
    "id",
    "created",
    "updated",
    "expand",
    "collection_id",
    "collection_name",
];

/// SQLite keywords (https://sqlite.org/lang_keywords.html), lowercase.
const SQLITE_KEYWORDS: &[&str] = &[
    "abort",
    "action",
    "add",
    "after",
    "all",
    "alter",
    "always",
    "analyze",
    "and",
    "as",
    "asc",
    "attach",
    "autoincrement",
    "before",
    "begin",
    "between",
    "by",
    "cascade",
    "case",
    "cast",
    "check",
    "collate",
    "column",
    "commit",
    "conflict",
    "constraint",
    "create",
    "cross",
    "current",
    "current_date",
    "current_time",
    "current_timestamp",
    "database",
    "default",
    "deferrable",
    "deferred",
    "delete",
    "desc",
    "detach",
    "distinct",
    "do",
    "drop",
    "each",
    "else",
    "end",
    "escape",
    "except",
    "exclude",
    "exclusive",
    "exists",
    "explain",
    "fail",
    "filter",
    "first",
    "following",
    "for",
    "foreign",
    "from",
    "full",
    "generated",
    "glob",
    "group",
    "groups",
    "having",
    "if",
    "ignore",
    "immediate",
    "in",
    "index",
    "indexed",
    "initially",
    "inner",
    "insert",
    "instead",
    "intersect",
    "into",
    "is",
    "isnull",
    "join",
    "key",
    "last",
    "left",
    "like",
    "limit",
    "match",
    "materialized",
    "natural",
    "no",
    "not",
    "nothing",
    "notnull",
    "null",
    "nulls",
    "of",
    "offset",
    "on",
    "or",
    "order",
    "others",
    "outer",
    "over",
    "partition",
    "plan",
    "pragma",
    "preceding",
    "primary",
    "query",
    "raise",
    "range",
    "recursive",
    "references",
    "regexp",
    "reindex",
    "release",
    "rename",
    "replace",
    "restrict",
    "returning",
    "right",
    "rollback",
    "row",
    "rows",
    "savepoint",
    "select",
    "set",
    "table",
    "temp",
    "temporary",
    "then",
    "ties",
    "to",
    "transaction",
    "trigger",
    "unbounded",
    "union",
    "unique",
    "update",
    "using",
    "vacuum",
    "values",
    "view",
    "virtual",
    "when",
    "where",
    "window",
    "with",
    "without",
];

fn valid_identifier(name: &str) -> bool {
    let mut chars = name.chars();
    matches!(chars.next(), Some('a'..='z'))
        && name.len() <= 63
        && chars.all(|c| matches!(c, 'a'..='z' | '0'..='9' | '_'))
}

fn pattern_error() -> FieldError {
    FieldError::new(
        "invalid_name",
        "Must start with a lowercase letter and contain only a-z, 0-9 and _ (max 63 characters).",
    )
}

/// Validates a collection name: `^[a-z][a-z0-9_]{0,62}$`, not reserved.
pub fn check_collection_name(name: &str) -> Result<(), FieldError> {
    if !valid_identifier(name) {
        return Err(pattern_error());
    }
    if RESERVED_COLLECTIONS.contains(&name)
        || SQLITE_KEYWORDS.contains(&name)
        || name.starts_with("_")
    {
        return Err(FieldError::new("reserved_name", "This name is reserved.").param("name", name));
    }
    if name.contains("_via_") {
        return Err(FieldError::new(
            "reserved_name",
            "Names must not contain \"_via_\" (back-relation syntax).",
        ));
    }
    Ok(())
}

/// Validates a field name. `extra_reserved` lists auth system fields for auth collections.
pub fn check_field_name(name: &str, extra_reserved: &[&str]) -> Result<(), FieldError> {
    if !valid_identifier(name) {
        return Err(pattern_error());
    }
    if RESERVED_FIELDS.contains(&name)
        || extra_reserved.contains(&name)
        || SQLITE_KEYWORDS.contains(&name)
    {
        return Err(FieldError::new("reserved_name", "This name is reserved.").param("name", name));
    }
    if name.contains("_via_") {
        return Err(FieldError::new(
            "reserved_name",
            "Names must not contain \"_via_\" (back-relation syntax).",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn collection_names() {
        assert!(check_collection_name("posts").is_ok());
        assert!(check_collection_name("room_members2").is_ok());
        for bad in [
            "",
            "Posts",
            "1posts",
            "_x",
            "po-sts",
            "settings",
            "select",
            "a_via_b",
            &"x".repeat(64),
        ] {
            assert!(check_collection_name(bad).is_err(), "{bad}");
        }
        assert!(check_collection_name(&"x".repeat(63)).is_ok());
    }

    #[test]
    fn field_names() {
        assert!(check_field_name("title", &[]).is_ok());
        for bad in [
            "id",
            "created",
            "expand",
            "collection_name",
            "order",
            "Title",
        ] {
            assert!(check_field_name(bad, &[]).is_err(), "{bad}");
        }
        assert!(check_field_name("email", &["email"]).is_err());
    }
}
