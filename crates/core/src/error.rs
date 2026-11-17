//! The error type shared by use cases and ports.

use std::fmt;

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
}

impl fmt::Display for UnavailableReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::WriterQueueFull => "the server is busy",
            Self::ShuttingDown => "the server is shutting down",
            Self::Migrating => "the server is applying migrations",
        })
    }
}

/// Errors returned by use cases and ports.
///
/// Adapters map their own errors into this type. The HTTP adapter maps it to
/// RFC 9457 problem details (SPEC §10.2).
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    /// The requested entity does not exist or is not visible to the caller.
    #[error("not found")]
    NotFound,
    /// The operation conflicts with the current state.
    #[error("conflict: {0}")]
    Conflict(String),
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
}
