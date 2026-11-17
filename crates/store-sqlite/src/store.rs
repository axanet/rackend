//! The core `Store` port over `data.db` (SPEC §4.4).

use rackend_core::ports::{
    AnyBox, ReadJob, ReadTx, SettingRow, Store, StoreFuture, WriteJob, WriteTx,
};
use rackend_core::{Error as CoreError, Result as CoreResult};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};

use crate::db::SqliteDb;

/// `Store` implementation on SQLite.
#[derive(Debug, Clone)]
pub struct SqliteStore {
    db: SqliteDb,
}

impl SqliteStore {
    /// Wraps a started `data.db`.
    pub fn new(db: SqliteDb) -> Self {
        Self { db }
    }

    /// The underlying database (for shutdown and maintenance jobs).
    pub fn db(&self) -> &SqliteDb {
        &self.db
    }
}

impl Store for SqliteStore {
    fn read_job(&self, job: ReadJob) -> StoreFuture<'_, AnyBox> {
        Box::pin(async move {
            self.db
                .with_reader(move |conn| -> CoreResult<AnyBox> {
                    // A deferred transaction gives the job one consistent snapshot.
                    let tx = conn.transaction().map_err(sql)?;
                    let mut stx = SqliteTx { conn: &tx };
                    job(&mut stx)
                    // `tx` is dropped here, which rolls the read transaction back.
                })
                .await
                .map_err(CoreError::from)?
        })
    }

    fn write_job(&self, job: WriteJob) -> StoreFuture<'_, AnyBox> {
        Box::pin(async move {
            self.db
                .with_writer(move |conn| -> CoreResult<AnyBox> {
                    let tx = conn
                        .transaction_with_behavior(TransactionBehavior::Immediate)
                        .map_err(sql)?;
                    let out = {
                        let mut stx = SqliteTx { conn: &tx };
                        job(&mut stx)
                    };
                    match out {
                        Ok(value) => {
                            tx.commit().map_err(sql)?;
                            Ok(value)
                        }
                        Err(e) => {
                            tx.rollback().map_err(sql)?;
                            Err(e)
                        }
                    }
                })
                .await
                .map_err(CoreError::from)?
        })
    }
}

/// Repository methods bound to one open transaction.
struct SqliteTx<'a> {
    conn: &'a Connection,
}

fn sql(err: rusqlite::Error) -> CoreError {
    CoreError::internal(err)
}

impl ReadTx for SqliteTx<'_> {
    fn meta_get(&mut self, key: &str) -> CoreResult<Option<String>> {
        self.conn
            .query_row("SELECT value FROM _meta WHERE key = ?1", [key], |r| {
                r.get(0)
            })
            .optional()
            .map_err(sql)
    }

    fn settings_list(&mut self) -> CoreResult<Vec<SettingRow>> {
        let mut stmt = self
            .conn
            .prepare_cached("SELECT key, value, is_secret, updated FROM _settings ORDER BY key")
            .map_err(sql)?;
        let rows = stmt
            .query_map([], |r| {
                Ok(SettingRow {
                    key: r.get(0)?,
                    value: r.get(1)?,
                    is_secret: r.get::<_, i64>(2)? != 0,
                    updated: r.get(3)?,
                })
            })
            .map_err(sql)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(sql)?;
        Ok(rows)
    }
}

impl WriteTx for SqliteTx<'_> {
    fn meta_set(&mut self, key: &str, value: &str) -> CoreResult<()> {
        self.conn
            .execute(
                "INSERT INTO _meta (key, value) VALUES (?1, ?2)
                 ON CONFLICT (key) DO UPDATE SET value = excluded.value",
                params![key, value],
            )
            .map(drop)
            .map_err(sql)
    }

    fn setting_put(&mut self, row: &SettingRow) -> CoreResult<()> {
        self.conn
            .execute(
                "INSERT INTO _settings (key, value, is_secret, updated) VALUES (?1, ?2, ?3, ?4)
                 ON CONFLICT (key) DO UPDATE SET
                   value = excluded.value, is_secret = excluded.is_secret, updated = excluded.updated",
                params![row.key, row.value, i64::from(row.is_secret), row.updated],
            )
            .map(drop)
            .map_err(sql)
    }
}
