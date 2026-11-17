//! The `Store` port: transactions and repositories over the database (SPEC §4.4).
//!
//! The port itself is object safe (`Arc<dyn Store>`): it moves boxed jobs to
//! the adapter. [`StoreExt`] adds the typed `read`/`write` API that use cases
//! call.

use std::any::Any;
use std::future::Future;
use std::pin::Pin;

use serde_json::{Map, Value};

use crate::auth::keys::KeyRow;
use crate::auth::{PasskeyRow, SessionRow, Superuser};
use crate::domain::{AuditEntry, Collection, DdlStep, Field, HistoryEntry, Record, TrashEntry};
use crate::rules::TypedRule;
use crate::rules::eval::RequestValues;
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

/// Sort key of a record query.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Sort {
    /// Ascending by column.
    Asc(String),
    /// Descending by column.
    Desc(String),
    /// `@random`.
    Random,
}

/// A record query: access rule and filter are ANDed (SPEC §9.4).
#[derive(Debug, Clone)]
pub struct RecordQuery<'a> {
    /// Collection to read.
    pub collection: &'a Collection,
    /// Conditions (compiled rules), all must hold.
    pub conditions: Vec<&'a TypedRule>,
    /// Request values for macros.
    pub req: &'a RequestValues,
    /// Only these ids.
    pub ids: Option<&'a [String]>,
    /// Only records whose relation `field` references one of these ids.
    pub referencing: Option<(&'a Field, &'a [String])>,
    /// Ordering.
    pub sort: Vec<Sort>,
    /// Page size (`None` = all).
    pub limit: Option<u32>,
    /// Rows to skip.
    pub offset: u64,
    /// Compute the total count.
    pub count_total: bool,
}

/// A page of records.
#[derive(Debug, Clone, PartialEq)]
pub struct RecordPage {
    /// Records.
    pub items: Vec<Record>,
    /// Total matching records, if requested.
    pub total: Option<u64>,
}

/// Filter of an audit query.
#[derive(Debug, Clone, Default)]
pub struct AuditQuery {
    /// Actor type.
    pub actor_type: Option<String>,
    /// Actor id.
    pub actor_id: Option<String>,
    /// Action, or prefix ending in `.` (e.g. `record.`).
    pub action: Option<String>,
    /// Collection id.
    pub collection_id: Option<String>,
    /// Record id.
    pub record_id: Option<String>,
    /// From (inclusive).
    pub from: Option<String>,
    /// To (exclusive).
    pub to: Option<String>,
    /// Newest first.
    pub newest_first: bool,
    /// Page size.
    pub limit: u32,
    /// Rows to skip.
    pub offset: u64,
}

/// Repository methods available inside a read transaction.
pub trait ReadTx {
    /// Reads a value from `_meta`.
    fn meta_get(&mut self, key: &str) -> Result<Option<String>>;
    /// Lists all rows of `_settings`, ordered by key.
    fn settings_list(&mut self) -> Result<Vec<SettingRow>>;

    /// All collections, in creation order.
    fn collections_list(&mut self) -> Result<Vec<Collection>>;
    /// Schema history of one collection (or all), newest first.
    fn schema_history(
        &mut self,
        collection_id: Option<&str>,
        limit: u32,
    ) -> Result<Vec<HistoryEntry>>;
    /// Trash rows, newest first.
    fn trash_list(&mut self) -> Result<Vec<TrashEntry>>;
    /// One trash row.
    fn trash_get(&mut self, id: &str) -> Result<Option<TrashEntry>>;

    /// One record, without rule checks.
    fn record_get(&mut self, c: &Collection, id: &str) -> Result<Option<Record>>;
    /// Records matching a query. Rule-produced queries run with the query timeout.
    fn records_query(&mut self, q: &RecordQuery<'_>) -> Result<RecordPage>;
    /// Whether record `id` satisfies `rule` for `req`.
    fn record_matches(
        &mut self,
        c: &Collection,
        id: &str,
        rule: &TypedRule,
        req: &RequestValues,
    ) -> Result<bool>;
    /// Which of `ids` do not exist in `c`.
    fn records_missing(&mut self, c: &Collection, ids: &[String]) -> Result<Vec<String>>;
    /// Number of rows whose multi-valued column has more than one value.
    fn count_multi_valued(&mut self, table: &str, column: &str) -> Result<u64>;

    /// Audit entries.
    fn audit_query(&mut self, q: &AuditQuery) -> Result<(Vec<AuditEntry>, u64)>;

    /// Number of superusers.
    fn superusers_count(&mut self) -> Result<u64>;
    /// Superuser by (lowercased) email.
    fn superuser_by_email(&mut self, email: &str) -> Result<Option<Superuser>>;
    /// Superuser by id.
    fn superuser_by_id(&mut self, id: &str) -> Result<Option<Superuser>>;
    /// All superusers.
    fn superusers_list(&mut self) -> Result<Vec<Superuser>>;
    /// Session by id.
    fn session_get(&mut self, id: &str) -> Result<Option<SessionRow>>;
    /// Whether an unused, unexpired setup token with this hash exists.
    fn setup_token_valid(&mut self, hash: &str, now: &str) -> Result<bool>;

    /// An auth record by (lowercase) email, with its password hash.
    fn auth_find_by_email(
        &mut self,
        c: &Collection,
        email: &str,
    ) -> Result<Option<(Record, Option<String>)>>;
    /// The password hash of an auth record.
    fn auth_password_hash(&mut self, c: &Collection, id: &str) -> Result<Option<String>>;
    /// Sessions of a user or superuser, newest first.
    fn sessions_for(&mut self, user_id: &str) -> Result<Vec<SessionRow>>;
    /// Passkeys of a user or superuser.
    fn passkeys_for(&mut self, user_id: &str) -> Result<Vec<PasskeyRow>>;
    /// A passkey by credential id.
    fn passkey_by_credential(&mut self, credential_id: &str) -> Result<Option<PasskeyRow>>;
    /// All signing keys, oldest first.
    fn keys_list(&mut self) -> Result<Vec<KeyRow>>;
}

/// Repository methods available inside a write transaction.
pub trait WriteTx: ReadTx {
    /// Inserts or replaces a value in `_meta`.
    fn meta_set(&mut self, key: &str, value: &str) -> Result<()>;
    /// Inserts or replaces a row in `_settings`.
    fn setting_put(&mut self, row: &SettingRow) -> Result<()>;

    /// Runs schema steps and returns the executed SQL (for history).
    fn schema_apply(&mut self, steps: &[DdlStep]) -> Result<String>;
    /// The SQL `schema_apply` would run, without running it (dry runs).
    fn schema_render(&mut self, steps: &[DdlStep]) -> Result<String>;
    /// Inserts or replaces a `_collections` row.
    fn collection_put(&mut self, c: &Collection) -> Result<()>;
    /// Deletes a `_collections` row.
    fn collection_remove(&mut self, id: &str) -> Result<()>;
    /// Appends a schema history row.
    fn schema_history_insert(&mut self, h: &HistoryEntry) -> Result<()>;
    /// Adds a trash row.
    fn trash_insert(&mut self, t: &TrashEntry) -> Result<()>;
    /// Removes a trash row.
    fn trash_remove(&mut self, id: &str) -> Result<()>;
    /// Trash rows whose `purge_after` is before `now`.
    fn trash_due(&mut self, now: &str, limit: u32) -> Result<Vec<TrashEntry>>;

    /// Inserts a record. Unique violations map to `Conflict`.
    fn record_insert(&mut self, c: &Collection, r: &Record) -> Result<()>;
    /// Updates fields of a record and its `updated` time.
    fn record_update(
        &mut self,
        c: &Collection,
        id: &str,
        updated: &str,
        changes: &Map<String, Value>,
    ) -> Result<()>;
    /// Deletes a record.
    fn record_delete(&mut self, c: &Collection, id: &str) -> Result<()>;

    /// Appends an audit entry.
    fn audit_insert(&mut self, e: &AuditEntry) -> Result<()>;
    /// Deletes up to `limit` audit entries older than `before`; returns how many.
    fn audit_delete_before(&mut self, before: &str, limit: u32) -> Result<u64>;

    /// Inserts a superuser.
    fn superuser_insert(&mut self, s: &Superuser) -> Result<()>;
    /// Updates email/password of a superuser.
    fn superuser_update(&mut self, s: &Superuser) -> Result<()>;
    /// Deletes a superuser.
    fn superuser_delete(&mut self, id: &str) -> Result<()>;
    /// Inserts a session.
    fn session_insert(&mut self, s: &SessionRow) -> Result<()>;
    /// Replaces a session row.
    fn session_update(&mut self, s: &SessionRow) -> Result<()>;
    /// Revokes all active sessions of a subject, except `keep`; returns their ids.
    fn sessions_revoke_for(
        &mut self,
        user_id: &str,
        keep: Option<&str>,
        reason: &str,
        now: &str,
    ) -> Result<Vec<String>>;
    /// Deletes sessions expired or revoked before `before`.
    fn sessions_delete_before(&mut self, before: &str) -> Result<u64>;
    /// Replaces all setup tokens with one.
    fn setup_token_replace(&mut self, hash: &str, expires: &str) -> Result<()>;
    /// Marks a setup token used.
    fn setup_token_use(&mut self, hash: &str, now: &str) -> Result<()>;

    /// Sets (or clears) the password hash of an auth record.
    fn auth_password_set(&mut self, c: &Collection, id: &str, hash: Option<&str>) -> Result<()>;
    /// Inserts a passkey.
    fn passkey_insert(&mut self, p: &PasskeyRow) -> Result<()>;
    /// Updates the credential (sign counter) and last use of a passkey.
    fn passkey_update(&mut self, p: &PasskeyRow) -> Result<()>;
    /// Deletes a passkey of `user_id`; returns whether it existed.
    fn passkey_delete(&mut self, user_id: &str, id: &str) -> Result<bool>;
    /// Deletes all passkeys of a user or superuser.
    fn passkeys_delete_for(&mut self, user_id: &str) -> Result<u64>;
    /// Inserts a signing key.
    fn key_insert(&mut self, k: &KeyRow) -> Result<()>;
    /// Replaces the encrypted private key of a signing key (re-encryption).
    fn key_set_private(&mut self, kid: &str, private_enc: &str) -> Result<()>;
    /// Deletes all signing keys (secrets reset).
    fn keys_delete_all(&mut self) -> Result<u64>;
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

    /// Sets the time limit of rule-produced queries (`limits.rule_timeout_ms`).
    fn set_query_timeout(&self, _ms: u64) {}
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
