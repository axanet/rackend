//! Process exit codes (SPEC §7.4) and fatal startup errors.

use std::fmt;

/// Normal exit.
pub const OK: u8 = 0;
/// Generic failure (bad configuration, cannot bind, …).
pub const FAILURE: u8 = 1;
/// Data directory locked by another instance.
pub const LOCKED: u8 = 75;

/// An error that ends the process with a specific exit code.
#[derive(Debug)]
pub struct Fatal {
    pub code: u8,
    pub message: String,
}

impl Fatal {
    /// Generic startup failure (exit 1).
    pub fn startup(message: impl Into<String>) -> Self {
        Self {
            code: FAILURE,
            message: message.into(),
        }
    }

    /// Data directory locked (exit 75).
    pub fn locked(message: impl Into<String>) -> Self {
        Self {
            code: LOCKED,
            message: message.into(),
        }
    }

    /// From a migration failure, using its exit code (70, 73, 74 or 78).
    pub fn migration(err: &rackend_store_sqlite::migrate::MigrateError) -> Self {
        Self {
            code: err.exit_code(),
            message: err.to_string(),
        }
    }
}

impl fmt::Display for Fatal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for Fatal {}
