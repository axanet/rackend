//! The `Store` port: transactions and repositories over the database (SPEC §4.4).
//!
//! The port itself is object safe (`Arc<dyn Store>`): it moves boxed jobs to
//! the adapter. [`StoreExt`] adds the typed `read`/`write` API that use cases
//! call.

use std::any::Any;
use std::future::Future;
use std::pin::Pin;

use crate::{Error, Result};

/// A boxed future returned by [`Store`] methods.
pub type StoreFuture<'a, T> = Pin<Box<dyn Future<Output = Result<T>> + Send + 'a>>;

/// A type-erased value returned from a job.
pub type AnyBox = Box<dyn Any + Send>;

/// A job run inside one read transaction.
pub type ReadJob = Box<dyn FnOnce(&mut dyn ReadTx) -> Result<AnyBox> + Send>;

/// A job run inside one write transaction. Returning `Err` rolls it back.
pub type WriteJob = Box<dyn FnOnce(&mut dyn WriteTx) -> Result<AnyBox> + Send>;

/// One row of `_settings` (SPEC §15).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SettingRow {
    /// Settings key, e.g. `app` or `smtp`.
    pub key: String,
    /// JSON text of the value. Secrets are stored encrypted.
    pub value: String,
    /// Whether the value is a secret.
    pub is_secret: bool,
    /// RFC 3339 timestamp of the last change.
    pub updated: String,
}

/// Repository methods available inside a read transaction.
pub trait ReadTx {
    /// Reads a value from `_meta`.
    fn meta_get(&mut self, key: &str) -> Result<Option<String>>;
    /// Lists all rows of `_settings`, ordered by key.
    fn settings_list(&mut self) -> Result<Vec<SettingRow>>;
}

/// Repository methods available inside a write transaction.
pub trait WriteTx: ReadTx {
    /// Inserts or replaces a value in `_meta`.
    fn meta_set(&mut self, key: &str, value: &str) -> Result<()>;
    /// Inserts or replaces a row in `_settings`.
    fn setting_put(&mut self, row: &SettingRow) -> Result<()>;
}

/// Transactional access to the database.
///
/// Implementations run read jobs on a pool of read-only connections and write
/// jobs on a single writer, each inside one SQLite transaction (`BEGIN
/// IMMEDIATE` for writes). When the writer queue is full, `write_job` fails
/// with [`Error::Unavailable`].
pub trait Store: Send + Sync + std::fmt::Debug + 'static {
    /// Runs `job` inside a read transaction.
    fn read_job(&self, job: ReadJob) -> StoreFuture<'_, AnyBox>;
    /// Runs `job` inside a write transaction.
    fn write_job(&self, job: WriteJob) -> StoreFuture<'_, AnyBox>;
}

/// Typed access to a [`Store`]. Implemented for every store.
pub trait StoreExt: Store {
    /// Runs `f` inside one read transaction.
    fn read<R, F>(&self, f: F) -> impl Future<Output = Result<R>> + Send + '_
    where
        R: Send + 'static,
        F: FnOnce(&mut dyn ReadTx) -> Result<R> + Send + 'static,
    {
        let fut = self.read_job(Box::new(move |tx| f(tx).map(|r| Box::new(r) as AnyBox)));
        async move { downcast(fut.await?) }
    }

    /// Runs `f` inside one write transaction. Returning `Err` rolls it back.
    fn write<R, F>(&self, f: F) -> impl Future<Output = Result<R>> + Send + '_
    where
        R: Send + 'static,
        F: FnOnce(&mut dyn WriteTx) -> Result<R> + Send + 'static,
    {
        let fut = self.write_job(Box::new(move |tx| f(tx).map(|r| Box::new(r) as AnyBox)));
        async move { downcast(fut.await?) }
    }
}

impl<S: Store + ?Sized> StoreExt for S {}

fn downcast<R: 'static>(value: AnyBox) -> Result<R> {
    value
        .downcast::<R>()
        .map(|b| *b)
        .map_err(|_| Error::internal("store returned a value of an unexpected type"))
}
