//! REST endpoints (SPEC §10).

pub mod admin;
pub mod auth;
pub mod collections;
pub mod files;
pub mod input;
pub mod observe;
pub mod realtime;
pub mod records;
pub mod users;

use axum::Json;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde_json::Value;

use crate::problem::{ApiError, ProblemCode};

/// Handler result.
pub type ApiResult<T = Response> = Result<T, ApiError>;

/// `200 OK` with a JSON body.
pub fn ok(v: Value) -> Response {
    Json(v).into_response()
}

/// `201 Created` with a JSON body.
pub fn created(v: Value) -> Response {
    (StatusCode::CREATED, Json(v)).into_response()
}

/// `204 No Content`.
pub fn no_content() -> Response {
    StatusCode::NO_CONTENT.into_response()
}

/// A required JSON string member of a body.
pub fn str_field<'a>(body: &'a Value, key: &str) -> ApiResult<&'a str> {
    body.get(key).and_then(Value::as_str).ok_or_else(|| {
        let mut e = ApiError::with_detail(
            ProblemCode::ValidationFailed,
            "One or more fields are invalid.",
        );
        e.problem_mut().errors.insert(
            key.to_owned(),
            vec![crate::problem::FieldError {
                code: "required".into(),
                message: "Required.".into(),
                params: serde_json::Map::new(),
            }],
        );
        e
    })
}

/// Whether a query flag (`?dry_run=true`) is set.
pub fn flag(q: &std::collections::BTreeMap<String, String>, key: &str) -> bool {
    q.get(key)
        .is_some_and(|v| matches!(v.as_str(), "1" | "true" | "yes"))
}
