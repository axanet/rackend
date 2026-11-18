//! RFC 9457 problem details (SPEC §10.2).
//!
//! Handlers return [`ApiError`]. Its `IntoResponse` impl does not render a
//! body: it attaches the [`Problem`] to the response extensions, and the
//! problem-mapping middleware renders it with the request id. The same
//! middleware converts every other error response (framework rejections,
//! 404/405, panics, limits) into a problem, so no error leaves the server in
//! another format.

use std::collections::BTreeMap;

use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use rackend_core::Error as CoreError;
use rackend_core::error::UnavailableReason;
use serde::Serialize;

/// Base of every problem `type` URI.
pub const TYPE_BASE: &str = "https://rackend.axanet.ch/docs/reference/errors/#";

/// Media type of problem responses.
pub const PROBLEM_JSON: &str = "application/problem+json";

macro_rules! problem_codes {
    ($( $variant:ident => ($code:literal, $status:literal, $title:literal) ),+ $(,)?) => {
        /// The problem type registry (SPEC §10.2.2). Every code is documented
        /// at https://rackend.axanet.ch/docs/reference/errors/ (source: rackend-homepage). A released code is permanent.
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
        #[non_exhaustive]
        pub enum ProblemCode { $( #[doc = $title] $variant ),+ }

        impl ProblemCode {
            /// Every registered code.
            pub const ALL: &'static [ProblemCode] = &[ $( ProblemCode::$variant ),+ ];

            /// Stable machine-readable identifier (the `code` member).
            pub fn as_str(self) -> &'static str {
                match self { $( Self::$variant => $code ),+ }
            }

            /// HTTP status of this problem type.
            pub fn status(self) -> StatusCode {
                match self {
                    $( Self::$variant => StatusCode::from_u16($status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR) ),+
                }
            }

            /// Constant human-readable title.
            pub fn title(self) -> &'static str {
                match self { $( Self::$variant => $title ),+ }
            }
        }
    };
}

problem_codes! {
    BadRequest => ("bad_request", 400, "Bad request"),
    ValidationFailed => ("validation_failed", 400, "Validation failed"),
    RuleCompileError => ("rule_compile_error", 400, "Invalid rule or filter expression"),
    Unauthorized => ("unauthorized", 401, "Unauthorized"),
    Forbidden => ("forbidden", 403, "Forbidden"),
    NotFound => ("not_found", 404, "Not found"),
    MethodNotAllowed => ("method_not_allowed", 405, "Method not allowed"),
    NotAcceptable => ("not_acceptable", 406, "Not acceptable"),
    RequestTimeout => ("request_timeout", 408, "Request timeout"),
    Conflict => ("conflict", 409, "Conflict"),
    PreconditionFailed => ("precondition_failed", 412, "Precondition failed"),
    PayloadTooLarge => ("payload_too_large", 413, "Payload too large"),
    UnsupportedMediaType => ("unsupported_media_type", 415, "Unsupported media type"),
    RangeNotSatisfiable => ("range_not_satisfiable", 416, "Range not satisfiable"),
    RateLimited => ("rate_limited", 429, "Too many requests"),
    SmtpNotConfigured => ("smtp_not_configured", 409, "SMTP is not configured"),
    PublicUrlNotConfigured => ("public_url_not_configured", 409, "Public URL is not configured"),
    Internal => ("internal", 500, "Internal server error"),
    QueryTimeout => ("query_timeout", 503, "Query timeout"),
    Unavailable => ("unavailable", 503, "Service unavailable"),
}

impl ProblemCode {
    /// The absolute `type` URI.
    pub fn type_uri(self) -> String {
        format!("{TYPE_BASE}{}", self.as_str())
    }

    /// The code used for a framework error response with this status.
    /// Statuses outside the registry fall back to `bad_request` (4xx) or
    /// `internal` (5xx), and the response status is rewritten to match.
    pub fn for_status(status: StatusCode) -> Self {
        match status.as_u16() {
            400 | 422 => Self::BadRequest,
            401 => Self::Unauthorized,
            403 => Self::Forbidden,
            404 => Self::NotFound,
            405 => Self::MethodNotAllowed,
            406 => Self::NotAcceptable,
            408 => Self::RequestTimeout,
            409 => Self::Conflict,
            412 => Self::PreconditionFailed,
            413 => Self::PayloadTooLarge,
            415 => Self::UnsupportedMediaType,
            416 => Self::RangeNotSatisfiable,
            429 => Self::RateLimited,
            503 => Self::Unavailable,
            s if (400..500).contains(&s) => Self::BadRequest,
            _ => Self::Internal,
        }
    }
}

/// Position of a rule/filter compile error.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct Position {
    /// 1-based line.
    pub line: u32,
    /// 1-based column.
    pub column: u32,
    /// 0-based byte offset.
    pub offset: u32,
}

/// One field-level validation error.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct FieldError {
    /// Stable snake_case code, e.g. `too_long`.
    pub code: String,
    /// Human-readable message.
    pub message: String,
    /// Parameters of the message, e.g. `{"max": 200}`.
    #[serde(skip_serializing_if = "serde_json::Map::is_empty")]
    pub params: serde_json::Map<String, serde_json::Value>,
}

/// A problem before rendering: everything except the per-request members.
#[derive(Debug, Clone, PartialEq)]
pub struct Problem {
    /// Problem type.
    pub code: ProblemCode,
    /// Occurrence-specific explanation. Never contains internal details.
    pub detail: Option<String>,
    /// Field-level validation errors.
    pub errors: BTreeMap<String, Vec<FieldError>>,
    /// Rule/filter compile error position.
    pub position: Option<Position>,
    /// Retry hint for 429/503.
    pub retry_after_s: Option<u32>,
    /// Size limit for 413.
    pub max_bytes: Option<u64>,
    /// Allowed methods for 405.
    pub allowed: Vec<String>,
    /// Further extension members (e.g. batch `results`).
    pub extra: serde_json::Map<String, serde_json::Value>,
}

impl Problem {
    /// A problem with only a code.
    pub fn new(code: ProblemCode) -> Self {
        Self {
            code,
            detail: None,
            errors: BTreeMap::new(),
            position: None,
            retry_after_s: None,
            max_bytes: None,
            allowed: Vec::new(),
            extra: serde_json::Map::new(),
        }
    }

    /// Sets the detail text.
    #[must_use]
    pub fn with_detail(mut self, detail: impl Into<String>) -> Self {
        self.detail = Some(detail.into());
        self
    }
}

/// The rendered RFC 9457 body.
#[derive(Debug, Serialize)]
pub struct ProblemBody<'a> {
    #[serde(rename = "type")]
    type_: String,
    title: &'static str,
    status: u16,
    #[serde(skip_serializing_if = "Option::is_none")]
    detail: Option<&'a str>,
    instance: String,
    code: &'static str,
    request_id: String,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    errors: &'a BTreeMap<String, Vec<FieldError>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    position: Option<Position>,
    #[serde(skip_serializing_if = "Option::is_none")]
    retry_after_s: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    max_bytes: Option<u64>,
    #[serde(skip_serializing_if = "<[String]>::is_empty")]
    allowed: &'a [String],
    #[serde(flatten)]
    extra: &'a serde_json::Map<String, serde_json::Value>,
}

impl Problem {
    /// Renders the body for one request.
    pub fn body(&self, request_id: &str) -> ProblemBody<'_> {
        ProblemBody {
            type_: self.code.type_uri(),
            title: self.code.title(),
            status: self.code.status().as_u16(),
            detail: self.detail.as_deref(),
            instance: format!("urn:uuid:{request_id}"),
            code: self.code.as_str(),
            request_id: request_id.to_owned(),
            errors: &self.errors,
            position: self.position,
            retry_after_s: self.retry_after_s,
            max_bytes: self.max_bytes,
            allowed: &self.allowed,
            extra: &self.extra,
        }
    }
}

/// Error returned by handlers. Converted to a problem response by the
/// problem-mapping middleware.
#[derive(Debug)]
pub struct ApiError {
    problem: Box<Problem>,
}

impl ApiError {
    /// An error with the given code and no detail.
    pub fn new(code: ProblemCode) -> Self {
        Self {
            problem: Box::new(Problem::new(code)),
        }
    }

    /// An error with the given code and detail text.
    pub fn with_detail(code: ProblemCode, detail: impl Into<String>) -> Self {
        Self {
            problem: Box::new(Problem::new(code).with_detail(detail)),
        }
    }

    /// An internal error. `source` is logged (with the request id, via the
    /// current span) and never sent to the client.
    pub fn internal(source: &(dyn std::error::Error + 'static)) -> Self {
        tracing::error!(error = %source, chain = ?source, "internal error");
        Self::new(ProblemCode::Internal)
    }

    /// The problem this error renders as.
    pub fn problem(&self) -> &Problem {
        &self.problem
    }

    /// Mutable access, to add extension members.
    pub fn problem_mut(&mut self) -> &mut Problem {
        &mut self.problem
    }
}

impl From<Problem> for ApiError {
    fn from(problem: Problem) -> Self {
        Self {
            problem: Box::new(problem),
        }
    }
}

impl From<CoreError> for ApiError {
    fn from(err: CoreError) -> Self {
        match err {
            CoreError::BadRequest(d) => Self::with_detail(ProblemCode::BadRequest, d),
            CoreError::Validation(v) => {
                let mut p = Problem::new(ProblemCode::ValidationFailed)
                    .with_detail("One or more fields are invalid.");
                for (field, errs) in v.0 {
                    p.errors.insert(
                        field,
                        errs.into_iter()
                            .map(|e| FieldError {
                                code: e.code.to_owned(),
                                message: e.message,
                                params: e.params,
                            })
                            .collect(),
                    );
                }
                p.into()
            }
            CoreError::RuleCompile { message, position } => {
                let mut p = Problem::new(ProblemCode::RuleCompileError).with_detail(message);
                p.position = Some(Position {
                    line: position.line,
                    column: position.column,
                    offset: position.offset,
                });
                p.into()
            }
            CoreError::Unauthorized => Self::with_detail(
                ProblemCode::Unauthorized,
                "Missing, invalid or expired credentials.",
            ),
            CoreError::Forbidden(d) => Self::with_detail(ProblemCode::Forbidden, d),
            CoreError::NotFound => Self::with_detail(
                ProblemCode::NotFound,
                "The requested resource does not exist.",
            ),
            CoreError::Conflict(detail) => Self::with_detail(ProblemCode::Conflict, detail),
            CoreError::PreconditionFailed => Self::with_detail(
                ProblemCode::PreconditionFailed,
                "The record was changed since you read it (If-Match).",
            ),
            CoreError::UnsupportedMediaType(errs) => {
                let mut p = Problem::new(ProblemCode::UnsupportedMediaType)
                    .with_detail("A file type is not allowed for its field.");
                for (k, v) in errs.0 {
                    p.errors.insert(
                        k,
                        v.into_iter()
                            .map(|e| FieldError {
                                code: e.code.to_owned(),
                                message: e.message,
                                params: e.params,
                            })
                            .collect(),
                    );
                }
                p.into()
            }
            CoreError::RateLimited { retry_after_s } => {
                let mut p = Problem::new(ProblemCode::RateLimited)
                    .with_detail("Too many requests or connections. Retry later.");
                p.retry_after_s = Some(retry_after_s);
                p.into()
            }
            CoreError::PayloadTooLarge { max_bytes } => {
                let mut p = Problem::new(ProblemCode::PayloadTooLarge);
                p.max_bytes = Some(max_bytes);
                p.into()
            }
            CoreError::NotConfigured("smtp") => Self::with_detail(
                ProblemCode::SmtpNotConfigured,
                "This action sends email, and SMTP is not configured.",
            ),
            CoreError::NotConfigured(_) => Self::with_detail(
                ProblemCode::PublicUrlNotConfigured,
                "Set the public URL (app.public_url) first.",
            ),
            CoreError::QueryTimeout => {
                let mut p = Problem::new(ProblemCode::QueryTimeout)
                    .with_detail("The query took too long. Simplify the filter or add an index.");
                p.retry_after_s = Some(1);
                p.into()
            }
            CoreError::Unavailable {
                reason,
                retry_after_s,
            } => {
                let mut p = Problem::new(ProblemCode::Unavailable).with_detail(match reason {
                    UnavailableReason::WriterQueueFull => "The server is busy. Retry shortly.",
                    UnavailableReason::ShuttingDown => {
                        "The server is shutting down. Retry shortly."
                    }
                    UnavailableReason::Migrating => {
                        "The server is applying migrations. Retry shortly."
                    }
                    UnavailableReason::Storage => {
                        "The file storage is unavailable. Retry shortly or check the storage settings."
                    }
                    UnavailableReason::IdentityProvider => {
                        "The identity provider cannot be reached. Retry shortly or check its settings."
                    }
                    _ => "The service is temporarily unavailable. Retry shortly.",
                });
                p.retry_after_s = Some(retry_after_s);
                p.into()
            }
            CoreError::Internal(source) => Self::internal(source.as_ref()),
            other => Self::internal(&other),
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let mut res = self.problem.code.status().into_response();
        res.extensions_mut().insert(*self.problem);
        res
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codes_are_unique_snake_case() {
        let mut seen = std::collections::HashSet::new();
        for c in ProblemCode::ALL {
            assert!(seen.insert(c.as_str()), "duplicate {}", c.as_str());
            assert!(
                c.as_str()
                    .chars()
                    .all(|ch| ch.is_ascii_lowercase() || ch == '_')
            );
            assert!(c.status().is_client_error() || c.status().is_server_error());
        }
    }

    #[test]
    fn framework_statuses_map_into_the_registry() {
        for s in [400u16, 404, 405, 413, 415, 422, 418, 500, 502, 503] {
            let status = StatusCode::from_u16(s).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
            let code = ProblemCode::for_status(status);
            assert!(ProblemCode::ALL.contains(&code));
        }
        assert_eq!(
            ProblemCode::for_status(StatusCode::UNPROCESSABLE_ENTITY),
            ProblemCode::BadRequest
        );
        assert_eq!(
            ProblemCode::for_status(StatusCode::BAD_GATEWAY),
            ProblemCode::Internal
        );
    }
}
