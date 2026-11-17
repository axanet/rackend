//! Startup entry points: open, migrate and start `data.db` and `logs.db`.

use std::path::{Path, PathBuf};

use rackend_core::time::format_file_timestamp;
use time::OffsetDateTime;

use crate::conn::{DbOptions, open_writer};
use crate::db::SqliteDb;
use crate::migrate::{
    self, DATA_MIGRATIONS, LOGS_MIGRATIONS, MigrateError, MigrateOptions, MigrationReport,
};

/// Paths needed to open `data.db`.
#[derive(Debug, Clone)]
pub struct DataDbPaths {
    /// The database file.
    pub db: PathBuf,
    /// Directory for pre-upgrade backups.
    pub backups_dir: PathBuf,
}

/// Opens `data.db`, runs the migration procedure (SPEC §7.3) and starts the
/// writer thread and reader pool. On any error nothing has been started and
/// the data is untouched (beyond migrations that completed and were recorded).
pub fn open_data_db(
    paths: &DataDbPaths,
    opts: DbOptions,
    binary_version: &str,
    now: OffsetDateTime,
) -> Result<(SqliteDb, MigrationReport), MigrateError> {
    let mut conn = open_writer(&paths.db, &opts).map_err(MigrateError::Open)?;
    let report = migrate::run(
        &mut conn,
        &paths.db,
        DATA_MIGRATIONS,
        &MigrateOptions {
            backups_dir: Some(&paths.backups_dir),
            binary_version,
            now,
        },
    )?;
    let db = SqliteDb::start(conn, &paths.db, opts).map_err(MigrateError::Open)?;
    Ok((db, report))
}

/// `logs.db`: request logs and metric points. Loss is acceptable (SPEC §6.1).
#[derive(Debug, Clone)]
pub struct LogsDb {
    db: SqliteDb,
}

impl LogsDb {
    /// The underlying database.
    pub fn db(&self) -> &SqliteDb {
        &self.db
    }
}

/// Opens and migrates `logs.db`. If that fails, the file is moved aside to
/// `logs.db.broken-<ts>` and recreated (SPEC §7.3). `data.db` is never
/// treated this way.
pub fn open_logs_db(
    path: &Path,
    opts: DbOptions,
    binary_version: &str,
    now: OffsetDateTime,
) -> Result<LogsDb, MigrateError> {
    match try_open_logs(path, &opts, binary_version, now) {
        Ok(db) => Ok(LogsDb { db }),
        Err(err) => {
            let suffix = format!("broken-{}", format_file_timestamp(now));
            tracing::warn!(
                error = %err,
                path = %path.display(),
                "logs.db could not be opened or migrated; moving it aside and starting a new one"
            );
            for ext in ["", "-wal", "-shm"] {
                let mut from = path.as_os_str().to_owned();
                from.push(ext);
                let from = PathBuf::from(from);
                if from.exists() {
                    let mut to = path.as_os_str().to_owned();
                    to.push(format!(".{suffix}{ext}"));
                    std::fs::rename(&from, PathBuf::from(to))?;
                }
            }
            let db = try_open_logs(path, &opts, binary_version, now)?;
            Ok(LogsDb { db })
        }
    }
}

fn try_open_logs(
    path: &Path,
    opts: &DbOptions,
    binary_version: &str,
    now: OffsetDateTime,
) -> Result<SqliteDb, MigrateError> {
    let mut conn = open_writer(path, opts).map_err(MigrateError::Open)?;
    migrate::run(
        &mut conn,
        path,
        LOGS_MIGRATIONS,
        &MigrateOptions {
            backups_dir: None,
            binary_version,
            now,
        },
    )?;
    SqliteDb::start(conn, path, opts.clone()).map_err(MigrateError::Open)
}

/// Applied and pending `data.db` migrations, read-only (`rackend migrate status`).
/// Safe to run while the server is running (WAL readers don't block).
pub fn data_migration_status(path: &Path) -> Result<migrate::MigrationStatus, MigrateError> {
    let conn = rusqlite::Connection::open_with_flags(
        path,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(|e| MigrateError::Open(e.into()))?;
    migrate::status(&conn, DATA_MIGRATIONS)
}
