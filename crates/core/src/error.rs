//! The error type shared by use cases and ports.

use std::collections::BTreeMap;
use std::fmt;

use serde::Serialize;

/// A boxed error used as the source of [`Error::Internal`].
pub type BoxError = Box<dyn std::error::Error + Send + Sync + 'static>;

/// Result alias for core operations.
pub type Result<T, E = Error> = std::result::Result<T, E>;

/// Why a request could not be served right now.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum UnavailableReason {
    /// The database writer queue is full (SPEC §4.4).
    WriterQueueFull,
    /// The instance is shutting down (SPEC §5.6).
    ShuttingDown,
    /// The instance is applying migrations (SPEC §5.4).
    Migrating,
    /// The file storage backend failed (SPEC §12).
    Storage,
}

impl fmt::Display for UnavailableReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::WriterQueueFull => "the server is busy",
            Self::ShuttingDown => "the server is shutting down",
            Self::Migrating => "the server is applying migrations",
            Self::Storage => "the file storage is unavailable",
        })
    }
}

/// One field-level validation problem.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct FieldError {
    /// Stable snake_case code, e.g. `too_long`.
    pub code: &'static str,
    /// Human-readable message.
    pub message: String,
    /// Parameters of the message, e.g. `{"max": 200}`.
    #[serde(skip_serializing_if = "serde_json::Map::is_empty")]
    pub params: serde_json::Map<String, serde_json::Value>,
}

impl FieldError {
    /// A field error without parameters.
    pub fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            params: serde_json::Map::new(),
        }
    }

    /// Adds a parameter.
    #[must_use]
    pub fn param(mut self, key: &str, value: impl Into<serde_json::Value>) -> Self {
        self.params.insert(key.to_owned(), value.into());
        self
    }
}

/// Field-level validation errors keyed by field path (dot notation).
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct ValidationErrors(pub BTreeMap<String, Vec<FieldError>>);

impl ValidationErrors {
    /// Records an error for `field`.
    pub fn add(&mut self, field: impl Into<String>, err: FieldError) {
        self.0.entry(field.into()).or_default().push(err);
    }

    /// Whether no error was recorded.
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// `Ok(())` if empty, else [`Error::Validation`].
    pub fn into_result(self) -> Result<()> {
        if self.is_empty() {
            Ok(())
        } else {
            Err(Error::Validation(self))
        }
    }

    /// A single error as a `ValidationErrors` value.
    pub fn single(field: impl Into<String>, err: FieldError) -> Self {
        let mut v = Self::default();
        v.add(field, err);
        v
    }
}

/// Position of a rule/filter compile error.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct Position {
    /// 1-based line.
    pub line: u32,
    /// 1-based column (in characters).
    pub column: u32,
    /// 0-based byte offset.
    pub offset: u32,
}

/// Errors returned by use cases and ports.
///
/// Adapters map their own errors into this type. The HTTP adapter maps it to
/// RFC 9457 problem details (SPEC §10.2).
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    /// The request is malformed (bad parameter, bad JSON shape).
    #[error("bad request: {0}")]
    BadRequest(String),
    /// Body validation failed.
    #[error("validation failed")]
    Validation(ValidationErrors),
    /// A rule or filter expression does not compile.
    #[error("rule compile error at {}:{}: {message}", position.line, position.column)]
    RuleCompile {
        /// What is wrong.
        message: String,
        /// Where.
        position: Position,
    },
    /// Missing or invalid credentials.
    #[error("unauthorized")]
    Unauthorized,
    /// Authenticated but not allowed.
    #[error("forbidden: {0}")]
    Forbidden(String),
    /// The requested entity does not exist or is not visible to the caller.
    #[error("not found")]
    NotFound,
    /// The operation conflicts with the current state.
    #[error("conflict: {0}")]
    Conflict(String),
    /// An `If-Match` precondition failed.
    #[error("precondition failed")]
    PreconditionFailed,
    /// An uploaded file's type is not allowed for its field (per field).
    #[error("unsupported media type")]
    UnsupportedMediaType(ValidationErrors),
    /// A body exceeds a size limit.
    #[error("payload too large (max {max_bytes} bytes)")]
    PayloadTooLarge {
        /// The limit.
        max_bytes: u64,
    },
    /// The action needs configuration that is missing: `public_url` or `smtp`.
    #[error("not configured: {0}")]
    NotConfigured(&'static str),
    /// A rule-produced query exceeded its time limit.
    #[error("query timeout")]
    QueryTimeout,
    /// The operation cannot be served right now and may be retried.
    #[error("unavailable: {reason}")]
    Unavailable {
        /// Why the operation is unavailable.
        reason: UnavailableReason,
        /// Suggested delay before retrying, in seconds.
        retry_after_s: u32,
    },
    /// An unexpected failure. The message is for logs only and never shown to clients.
    #[error("internal error: {0}")]
    Internal(#[source] BoxError),
}

impl Error {
    /// Builds an [`Error::Internal`] from any error.
    pub fn internal(err: impl Into<BoxError>) -> Self {
        Self::Internal(err.into())
    }

    /// Builds an [`Error::Unavailable`] with a one-second retry hint.
    pub fn unavailable(reason: UnavailableReason) -> Self {
        Self::Unavailable {
            reason,
            retry_after_s: 1,
        }
    }

    /// A validation error for a single field.
    pub fn field(field: impl Into<String>, err: FieldError) -> Self {
        Self::Validation(ValidationErrors::single(field, err))
    }
}
