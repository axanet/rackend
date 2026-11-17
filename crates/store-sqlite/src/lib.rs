//! `rackend-store-sqlite`: the `Store` port on SQLite (SPEC §6, §7).
//!
//! - [`conn`]: opening connections with the PRAGMAs of SPEC §6.2.
//! - [`db`]: one writer thread plus a pool of read-only connections (SPEC §6.3).
//! - [`migrate`]: the system migration runner and its safety procedure (SPEC §7.3).
//! - [`store`]: the core `Store` port implementation over `data.db`.
//! - [`open_data_db`] / [`open_logs_db`]: the startup entry points.

pub mod conn;
pub mod db;
pub mod ddl;
mod error;
pub mod migrate;
mod open;
pub mod records;
pub mod rules_sql;
pub mod store;

pub use conn::{DbOptions, Synchronous};
pub use db::SqliteDb;
pub use error::Error;
pub use open::{DataDbPaths, LogsDb, data_migration_status, open_data_db, open_logs_db};
pub use store::SqliteStore;

/// The SQLite library version compiled into the binary.
pub fn sqlite_version() -> &'static str {
    rusqlite::version()
}
