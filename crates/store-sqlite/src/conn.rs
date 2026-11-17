//! Opening connections and applying PRAGMAs (SPEC §6.2).

use std::path::Path;
use std::time::Duration;

use rusqlite::{Connection, OpenFlags};

use crate::Error;

/// `PRAGMA synchronous` setting. `Full` is the default because data safety
/// outranks write throughput (SPEC §6.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Synchronous {
    /// Every commit is durable on power loss.
    #[default]
    Full,
    /// WAL + NORMAL: may lose the last commits on power loss, never corrupts.
    Normal,
}

impl Synchronous {
    fn as_sql(self) -> &'static str {
        match self {
            Self::Full => "FULL",
            Self::Normal => "NORMAL",
        }
    }
}

/// Options for one database file.
#[derive(Debug, Clone)]
pub struct DbOptions {
    /// `PRAGMA synchronous`.
    pub synchronous: Synchronous,
    /// Number of read-only connections.
    pub readers: usize,
    /// Capacity of the writer job queue (SPEC §4.4, default 1024).
    pub writer_queue: usize,
}

impl DbOptions {
    /// Defaults for `data.db`: `max(4, num_cpus)` readers (SPEC §6.3).
    pub fn data(synchronous: Synchronous) -> Self {
        let cpus = std::thread::available_parallelism().map_or(4, |n| n.get());
        Self {
            synchronous,
            readers: cpus.max(4),
            writer_queue: 1024,
        }
    }

    /// Defaults for `logs.db`: 2 readers (SPEC §6.3).
    pub fn logs(synchronous: Synchronous) -> Self {
        Self {
            synchronous,
            readers: 2,
            writer_queue: 1024,
        }
    }
}

/// Opens the read-write connection (creating the file) and applies PRAGMAs.
pub fn open_writer(path: &Path, opts: &DbOptions) -> Result<Connection, Error> {
    let mut conn = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_WRITE
            | OpenFlags::SQLITE_OPEN_CREATE
            | OpenFlags::SQLITE_OPEN_NO_MUTEX
            | OpenFlags::SQLITE_OPEN_URI,
    )?;
    let mode: String = conn.query_row("PRAGMA journal_mode = WAL", [], |r| r.get(0))?;
    if !mode.eq_ignore_ascii_case("wal") {
        return Err(Error::WalUnavailable(mode));
    }
    apply_common(&mut conn, opts)?;
    Ok(conn)
}

/// Opens a read-only connection and applies PRAGMAs.
pub fn open_reader(path: &Path, opts: &DbOptions) -> Result<Connection, Error> {
    let mut conn = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY
            | OpenFlags::SQLITE_OPEN_NO_MUTEX
            | OpenFlags::SQLITE_OPEN_URI,
    )?;
    apply_common(&mut conn, opts)?;
    Ok(conn)
}

fn apply_common(conn: &mut Connection, opts: &DbOptions) -> Result<(), Error> {
    crate::records::install_timeout_handler(conn)?;
    conn.busy_timeout(Duration::from_millis(5000))?;
    conn.execute_batch(&format!(
        "PRAGMA synchronous = {};
         PRAGMA foreign_keys = ON;
         PRAGMA temp_store = MEMORY;
         PRAGMA cache_size = -32000;",
        opts.synchronous.as_sql()
    ))?;
    Ok(())
}

/// Runs `PRAGMA integrity_check` and returns the problems found (empty = ok).
pub fn integrity_problems(conn: &Connection) -> Result<Vec<String>, Error> {
    let mut stmt = conn.prepare("PRAGMA integrity_check")?;
    let rows: Vec<String> = stmt
        .query_map([], |r| r.get::<_, String>(0))?
        .collect::<Result<_, _>>()?;
    Ok(if rows.len() == 1 && rows[0] == "ok" {
        Vec::new()
    } else {
        rows
    })
}

/// Runs `PRAGMA foreign_key_check` and returns the violations found (empty = ok).
pub fn foreign_key_problems(conn: &Connection) -> Result<Vec<String>, Error> {
    let mut stmt = conn.prepare("PRAGMA foreign_key_check")?;
    let rows: Vec<String> = stmt
        .query_map([], |r| {
            let table: String = r.get(0)?;
            let rowid: Option<i64> = r.get(1)?;
            let parent: String = r.get(2)?;
            Ok(format!("{table} row {rowid:?} references missing {parent}"))
        })?
        .collect::<Result<_, _>>()?;
    Ok(rows)
}
