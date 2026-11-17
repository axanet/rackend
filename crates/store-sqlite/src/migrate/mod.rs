//! System migrations and the startup migration procedure (SPEC §7).
//!
//! Migrations are numbered, append-only and forward-only. Each one runs in its
//! own transaction and is recorded in `_migrations` together with its
//! checksum. Before the first pending migration is applied, `data.db` is
//! snapshotted with `VACUUM INTO` and the snapshot is verified.

mod embedded;

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use rackend_core::time::{format_file_timestamp, format_timestamp};
use rusqlite::{
    Connection, OpenFlags, OptionalExtension, Transaction, TransactionBehavior, params,
};
use sha2::{Digest, Sha256};
use time::OffsetDateTime;

pub use embedded::{DATA_MIGRATIONS, LOGS_MIGRATIONS};

use crate::conn::{foreign_key_problems, integrity_problems};

/// Extra space required on top of twice the database size (SPEC §7.3 step 5a).
const BACKUP_SPACE_MARGIN: u64 = 100 * 1024 * 1024;
/// Number of pre-upgrade backups kept (SPEC §7.3 step 9).
pub const KEEP_PRE_UPGRADE_BACKUPS: usize = 5;

/// Optional Rust step of a migration, run after its SQL in the same transaction.
pub type MigrationFn = fn(&Transaction<'_>) -> rusqlite::Result<()>;

/// One system migration.
#[derive(Debug, Clone, Copy)]
pub struct Migration {
    /// Sequential id, starting at 1.
    pub id: u32,
    /// Short snake_case name.
    pub name: &'static str,
    /// SQL executed inside the migration transaction.
    pub sql: &'static str,
    /// Optional Rust step executed after the SQL.
    pub rust: Option<MigrationFn>,
    /// Run with `PRAGMA foreign_keys = OFF` (12-step table rebuilds). The
    /// runner checks `PRAGMA foreign_key_check` before commit.
    pub foreign_keys_off: bool,
}

impl Migration {
    /// SHA-256 over the name and SQL text, hex encoded.
    pub fn checksum(&self) -> String {
        let mut h = Sha256::new();
        h.update(self.name.as_bytes());
        h.update(b"\n");
        h.update(self.sql.as_bytes());
        hex(&h.finalize())
    }
}

fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write;
    bytes
        .iter()
        .fold(String::with_capacity(bytes.len() * 2), |mut s, b| {
            let _ = write!(s, "{b:02x}");
            s
        })
}

/// Options for one run of the migration procedure.
#[derive(Debug, Clone)]
pub struct MigrateOptions<'a> {
    /// Where pre-upgrade backups go. `None` disables them (used for `logs.db`).
    pub backups_dir: Option<&'a Path>,
    /// Version of the running binary, recorded in `_migrations` and `_meta`.
    pub binary_version: &'a str,
    /// Current time (injected for tests).
    pub now: OffsetDateTime,
}

/// Outcome of a successful run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MigrationReport {
    /// Schema version before the run.
    pub from: u32,
    /// Schema version after the run.
    pub to: u32,
    /// Ids of the migrations applied in this run.
    pub applied: Vec<u32>,
    /// The verified pre-upgrade backup, if one was taken.
    pub backup: Option<PathBuf>,
}

/// Failures of the migration procedure. Each maps to an exit code (SPEC §7.4).
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum MigrateError {
    /// The database could not be opened or read.
    #[error("could not open the database: {0}")]
    Open(#[source] crate::Error),
    /// The database was written by a newer binary (downgrade).
    #[error(
        "database schema v{db} is newer than this binary supports (v{binary}). Downgrading is not supported; restore a pre-upgrade backup from {backups} to go back."
    )]
    SchemaTooNew {
        /// Schema version found in the database.
        db: u32,
        /// Highest migration known to this binary.
        binary: u32,
        /// Backups directory, for the message.
        backups: String,
    },
    /// An applied migration differs from the one embedded in this binary.
    #[error(
        "applied migration {id} ({name}) does not match this binary (checksum {found} in the database, {expected} embedded). Released migrations must never change."
    )]
    ChecksumMismatch {
        /// Migration id.
        id: u32,
        /// Migration name.
        name: String,
        /// Checksum embedded in the binary.
        expected: String,
        /// Checksum recorded in the database.
        found: String,
    },
    /// The database records a migration this binary doesn't know.
    #[error("the database records migration {id} ({name}), which this binary does not know")]
    UnknownMigration {
        /// Migration id.
        id: u32,
        /// Migration name.
        name: String,
    },
    /// Not enough free space for a safe pre-upgrade backup.
    #[error(
        "not enough free disk space for a safe upgrade: {needed} bytes needed in {dir}, {available} available. Free up space and start again; no data was changed."
    )]
    InsufficientSpace {
        /// Bytes required.
        needed: u64,
        /// Bytes available.
        available: u64,
        /// Directory checked.
        dir: String,
    },
    /// The pre-upgrade backup did not pass `integrity_check`.
    #[error(
        "the pre-upgrade backup {path} failed verification: {problems:?}. No data was changed."
    )]
    BackupVerification {
        /// Backup path.
        path: String,
        /// Problems reported by SQLite.
        problems: Vec<String>,
    },
    /// A migration failed and was rolled back.
    #[error(
        "migration {id} ({name}) failed and was rolled back: {reason}. Earlier migrations of this run stay applied; fix the cause and start again."
    )]
    MigrationFailed {
        /// Migration id.
        id: u32,
        /// Migration name.
        name: String,
        /// Error message.
        reason: String,
    },
    /// The database failed integrity or foreign-key checks after migrating.
    #[error(
        "the database failed integrity checks after migrating: {problems:?}. Restore the pre-upgrade backup{}.",
        backup.as_ref().map(|p| format!(" {}", p.display())).unwrap_or_default()
    )]
    IntegrityCheck {
        /// Problems reported by SQLite.
        problems: Vec<String>,
        /// The pre-upgrade backup of this run, if any.
        backup: Option<PathBuf>,
    },
    /// `_meta.schema_version` is not a number.
    #[error("_meta.schema_version is corrupt: {0:?}")]
    CorruptMeta(String),
    /// An SQLite error outside a migration.
    #[error(transparent)]
    Sqlite(#[from] rusqlite::Error),
    /// A filesystem error.
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

impl MigrateError {
    /// The process exit code for this failure (SPEC §7.4).
    pub fn exit_code(&self) -> u8 {
        match self {
            Self::SchemaTooNew { .. }
            | Self::ChecksumMismatch { .. }
            | Self::UnknownMigration { .. } => 78,
            Self::InsufficientSpace { .. } => 73,
            Self::BackupVerification { .. } => 74,
            Self::Open(_)
            | Self::MigrationFailed { .. }
            | Self::IntegrityCheck { .. }
            | Self::CorruptMeta(_)
            | Self::Sqlite(_)
            | Self::Io(_) => 70,
        }
    }
}

/// One row of `_migrations`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppliedMigration {
    /// Migration id.
    pub id: u32,
    /// Migration name.
    pub name: String,
    /// Recorded checksum.
    pub checksum: String,
    /// When it was applied.
    pub applied_at: String,
    /// Binary version that applied it.
    pub binary_version: String,
}

/// Applied and pending migrations, without changing anything (`rackend migrate status`).
#[derive(Debug, Clone)]
pub struct MigrationStatus {
    /// `_meta.schema_version` (0 for a new database).
    pub schema_version: u32,
    /// Highest migration known to this binary.
    pub binary_max: u32,
    /// Rows of `_migrations`.
    pub applied: Vec<AppliedMigration>,
    /// Embedded migrations that are not applied yet.
    pub pending: Vec<Migration>,
}

/// Reads the migration state. Never writes.
pub fn status(
    conn: &Connection,
    migrations: &[Migration],
) -> Result<MigrationStatus, MigrateError> {
    let applied = applied_migrations(conn)?;
    let pending = migrations
        .iter()
        .filter(|m| !applied.iter().any(|a| a.id == m.id))
        .copied()
        .collect();
    Ok(MigrationStatus {
        schema_version: schema_version(conn)?,
        binary_max: migrations.last().map_or(0, |m| m.id),
        applied,
        pending,
    })
}

/// Runs the migration procedure of SPEC §7.3 on `conn` (the writer connection
/// of `db_path`). Must be called before the connection is shared.
pub fn run(
    conn: &mut Connection,
    db_path: &Path,
    migrations: &[Migration],
    opts: &MigrateOptions<'_>,
) -> Result<MigrationReport, MigrateError> {
    let binary_max = migrations.last().map_or(0, |m| m.id);
    let from = schema_version(conn)?;

    // Step 2: downgrade protection.
    if from > binary_max {
        return Err(MigrateError::SchemaTooNew {
            db: from,
            binary: binary_max,
            backups: opts.backups_dir.map_or_else(
                || "the backups directory".to_owned(),
                |p| p.display().to_string(),
            ),
        });
    }

    // Step 3: checksums of applied migrations.
    let applied = applied_migrations(conn)?;
    let embedded: BTreeMap<u32, &Migration> = migrations.iter().map(|m| (m.id, m)).collect();
    for a in &applied {
        let Some(m) = embedded.get(&a.id) else {
            return Err(MigrateError::UnknownMigration {
                id: a.id,
                name: a.name.clone(),
            });
        };
        let expected = m.checksum();
        if expected != a.checksum {
            return Err(MigrateError::ChecksumMismatch {
                id: a.id,
                name: a.name.clone(),
                expected,
                found: a.checksum.clone(),
            });
        }
    }

    // Step 4: nothing pending.
    let pending: Vec<&Migration> = migrations
        .iter()
        .filter(|m| !applied.iter().any(|a| a.id == m.id))
        .collect();
    if pending.is_empty() {
        record_binary_version(conn, opts.binary_version)?;
        return Ok(MigrationReport {
            from,
            to: from,
            applied: Vec::new(),
            backup: None,
        });
    }

    // Step 5: verified pre-upgrade backup (skipped for a database without tables:
    // there is nothing to protect yet, see ADR 0003).
    let backup = match opts.backups_dir {
        Some(dir) if has_tables(conn)? => {
            let from_version =
                meta_value(conn, "last_binary_version")?.unwrap_or_else(|| "unknown".into());
            Some(pre_upgrade_backup(conn, db_path, dir, &from_version, opts)?)
        }
        _ => None,
    };

    // Step 6: apply each pending migration in its own transaction.
    let mut applied_now = Vec::with_capacity(pending.len());
    for m in pending {
        apply(conn, m, opts).map_err(|reason| {
            tracing::error!(migration = m.id, name = m.name, %reason, "migration failed");
            MigrateError::MigrationFailed {
                id: m.id,
                name: m.name.to_owned(),
                reason,
            }
        })?;
        tracing::info!(migration = m.id, name = m.name, "migration applied");
        applied_now.push(m.id);
    }

    // Step 7: integrity after migrating.
    let mut problems = integrity_problems(conn).map_err(MigrateError::Open)?;
    problems.extend(foreign_key_problems(conn).map_err(MigrateError::Open)?);
    if !problems.is_empty() {
        return Err(MigrateError::IntegrityCheck { problems, backup });
    }

    // Step 8: schema_version is updated per migration; record the binary.
    record_binary_version(conn, opts.binary_version)?;

    // Step 9: retention of pre-upgrade backups.
    if let Some(dir) = opts.backups_dir {
        prune_pre_upgrade_backups(dir, KEEP_PRE_UPGRADE_BACKUPS)?;
    }

    Ok(MigrationReport {
        from,
        to: schema_version(conn)?,
        applied: applied_now,
        backup,
    })
}

fn table_exists(conn: &Connection, name: &str) -> rusqlite::Result<bool> {
    conn.query_row(
        "SELECT EXISTS (SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?1)",
        [name],
        |r| r.get(0),
    )
}

fn has_tables(conn: &Connection) -> rusqlite::Result<bool> {
    conn.query_row(
        "SELECT EXISTS (SELECT 1 FROM sqlite_master WHERE type = 'table')",
        [],
        |r| r.get(0),
    )
}

fn meta_value(conn: &Connection, key: &str) -> rusqlite::Result<Option<String>> {
    if !table_exists(conn, "_meta")? {
        return Ok(None);
    }
    conn.query_row("SELECT value FROM _meta WHERE key = ?1", [key], |r| {
        r.get(0)
    })
    .optional()
}

/// `_meta.schema_version`, 0 for a new database.
pub fn schema_version(conn: &Connection) -> Result<u32, MigrateError> {
    match meta_value(conn, "schema_version")? {
        None => Ok(0),
        Some(v) => v.parse().map_err(|_| MigrateError::CorruptMeta(v)),
    }
}

fn applied_migrations(conn: &Connection) -> Result<Vec<AppliedMigration>, MigrateError> {
    if !table_exists(conn, "_migrations")? {
        return Ok(Vec::new());
    }
    let mut stmt = conn.prepare(
        "SELECT id, name, checksum, applied_at, binary_version FROM _migrations ORDER BY id",
    )?;
    let rows = stmt
        .query_map([], |r| {
            Ok(AppliedMigration {
                id: r.get(0)?,
                name: r.get(1)?,
                checksum: r.get(2)?,
                applied_at: r.get(3)?,
                binary_version: r.get(4)?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

fn record_binary_version(conn: &Connection, version: &str) -> Result<(), MigrateError> {
    if !table_exists(conn, "_meta")? {
        return Ok(());
    }
    if meta_value(conn, "last_binary_version")?.as_deref() != Some(version) {
        conn.execute(
            "INSERT INTO _meta (key, value) VALUES ('last_binary_version', ?1)
             ON CONFLICT (key) DO UPDATE SET value = excluded.value",
            [version],
        )?;
    }
    Ok(())
}

fn file_len(path: &Path) -> u64 {
    fs::metadata(path).map_or(0, |m| m.len())
}

fn pre_upgrade_backup(
    conn: &Connection,
    db_path: &Path,
    dir: &Path,
    from_version: &str,
    opts: &MigrateOptions<'_>,
) -> Result<PathBuf, MigrateError> {
    fs::create_dir_all(dir)?;

    // 5a: free space >= 2 x size(data.db) + 100 MB.
    let mut wal = db_path.as_os_str().to_owned();
    wal.push("-wal");
    let size = file_len(db_path) + file_len(Path::new(&wal));
    let needed = size.saturating_mul(2).saturating_add(BACKUP_SPACE_MARGIN);
    let available = fs4::available_space(dir)?;
    if available < needed {
        return Err(MigrateError::InsufficientSpace {
            needed,
            available,
            dir: dir.display().to_string(),
        });
    }

    // 5b: VACUUM INTO a new file.
    let base = format!(
        "pre-upgrade-{}-to-{}-{}",
        sanitize(from_version),
        sanitize(opts.binary_version),
        format_file_timestamp(opts.now)
    );
    let mut path = dir.join(format!("{base}.db"));
    let mut n = 1;
    while path.exists() {
        path = dir.join(format!("{base}.{n}.db"));
        n += 1;
    }
    let target = path
        .to_str()
        .ok_or_else(|| std::io::Error::other("backup path is not valid UTF-8"))?;
    conn.execute("VACUUM INTO ?1", [target])?;

    // 5c: verify the backup read-only.
    let problems = {
        let backup = Connection::open_with_flags(&path, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
        integrity_problems(&backup).map_err(MigrateError::Open)?
    };
    if !problems.is_empty() {
        let mut failed = path.clone().into_os_string();
        failed.push(".failed-verification");
        let _ = fs::rename(&path, &failed);
        return Err(MigrateError::BackupVerification {
            path: path.display().to_string(),
            problems,
        });
    }
    tracing::info!(backup = %path.display(), "pre-upgrade backup verified");
    Ok(path)
}

fn sanitize(s: &str) -> String {
    s.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '.' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

fn apply(conn: &mut Connection, m: &Migration, opts: &MigrateOptions<'_>) -> Result<(), String> {
    if m.foreign_keys_off {
        conn.execute_batch("PRAGMA foreign_keys = OFF")
            .map_err(|e| e.to_string())?;
    }
    let result = (|| -> Result<(), String> {
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|e| e.to_string())?;
        tx.execute_batch(m.sql).map_err(|e| e.to_string())?;
        if let Some(step) = m.rust {
            step(&tx).map_err(|e| e.to_string())?;
        }
        if m.foreign_keys_off {
            let problems = foreign_key_problems(&tx).map_err(|e| e.to_string())?;
            if !problems.is_empty() {
                return Err(format!("foreign key violations: {problems:?}"));
            }
        }
        tx.execute(
            "INSERT INTO _migrations (id, name, checksum, applied_at, binary_version)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                m.id,
                m.name,
                m.checksum(),
                format_timestamp(opts.now),
                opts.binary_version
            ],
        )
        .map_err(|e| e.to_string())?;
        tx.execute(
            "INSERT INTO _meta (key, value) VALUES ('schema_version', ?1)
             ON CONFLICT (key) DO UPDATE SET value = excluded.value",
            [m.id.to_string()],
        )
        .map_err(|e| e.to_string())?;
        tx.commit().map_err(|e| e.to_string())
    })();
    if m.foreign_keys_off {
        conn.execute_batch("PRAGMA foreign_keys = ON")
            .map_err(|e| e.to_string())?;
    }
    result
}

/// Deletes all but the newest `keep` pre-upgrade backups in `dir`.
pub fn prune_pre_upgrade_backups(dir: &Path, keep: usize) -> std::io::Result<()> {
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e),
    };
    let mut backups: Vec<(String, PathBuf)> = Vec::new();
    for entry in entries {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().into_owned();
        if !(name.starts_with("pre-upgrade-") && name.ends_with(".db")) {
            continue;
        }
        // The timestamp is the last '-' separated segment: `...-20261117T143600Z[.n].db`.
        let ts = name.rsplit('-').next().unwrap_or_default().to_owned();
        backups.push((ts, entry.path()));
    }
    backups.sort_by(|a, b| b.0.cmp(&a.0));
    for (_, path) in backups.into_iter().skip(keep) {
        tracing::info!(backup = %path.display(), "deleting old pre-upgrade backup");
        fs::remove_file(path)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests;
