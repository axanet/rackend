use rackend_core::error::UnavailableReason;

/// Errors of the SQLite adapter.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    /// An SQLite error.
    #[error(transparent)]
    Sqlite(#[from] rusqlite::Error),
    /// A filesystem error.
    #[error(transparent)]
    Io(#[from] std::io::Error),
    /// The writer queue is full (SPEC §4.4).
    #[error("database writer queue is full")]
    QueueFull,
    /// The database is closed or shutting down.
    #[error("database is shutting down")]
    Closed,
    /// The database could not be switched to WAL journal mode.
    #[error("could not enable WAL journal mode (got {0:?})")]
    WalUnavailable(String),
    /// A blocking worker failed (panicked or was cancelled).
    #[error("database worker failed: {0}")]
    Worker(String),
}

impl From<Error> for rackend_core::Error {
    fn from(err: Error) -> Self {
        match err {
            Error::QueueFull => Self::unavailable(UnavailableReason::WriterQueueFull),
            Error::Closed => Self::unavailable(UnavailableReason::ShuttingDown),
            other => Self::internal(other),
        }
    }
}
