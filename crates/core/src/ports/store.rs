//! The `Store` port: transactions and repositories over the database (SPEC §4.4).
//!
//! The port itself is object safe (`Arc<dyn Store>`): it moves boxed jobs to
//! the adapter. [`StoreExt`] adds the typed `read`/`write` API that use cases
//! call.

use std::any::Any;
use std::future::Future;
use std::pin::Pin;

use serde_json::{Map, Value};

use crate::auth::broker::{IdentityRow, OtpRow, ProviderRow, TokenPurpose, TokenRow};
use crate::auth::keys::KeyRow;
use crate::auth::oauth::{ClientRow, CodeRow, ConsentRow};
use crate::auth::{PasskeyRow, SessionRow, Superuser};
use crate::domain::channel::ChannelDef;
use crate::domain::files::FileRow;
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

/// Writer queue depth and file sizes of the data database.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize)]
pub struct StoreStats {
    /// Write jobs waiting for the writer thread.
    pub write_queue_depth: u64,
    /// `data.db` bytes.
    pub db_size: u64,
    /// `data.db-wal` bytes.
    pub wal_size: u64,
}

/// A `ydoc` field as stored.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct YdocStored {
    /// The compacted state (`None` for a new document).
    pub state: Option<Vec<u8>>,
    /// Updates since the last compaction: `(id, update)`.
    pub updates: Vec<(i64, Vec<u8>)>,
}

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
    /// Whether `rule` holds for a record given by its values (not read from
    /// the table): realtime checks deletes against the record as it was.
    fn record_values_match(
        &mut self,
        c: &Collection,
        record: &Record,
        rule: &TypedRule,
        req: &RequestValues,
    ) -> Result<bool>;
    /// Which of `ids` are live sessions at `now` (not revoked, not expired).
    fn sessions_live(&mut self, ids: &[String], now: &str) -> Result<Vec<String>>;
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

    /// A live (not deleted) file by storage key.
    fn file_by_key(&mut self, storage_key: &str) -> Result<Option<FileRow>>;
    /// Files marked deleted, oldest first.
    fn files_deleted(&mut self, limit: u32) -> Result<Vec<FileRow>>;
    /// Which of `keys` have a `_files` row (live or deleted).
    fn files_known_keys(&mut self, keys: &[String]) -> Result<Vec<String>>;
    /// Live files and their total size in bytes.
    fn files_usage(&mut self) -> Result<(u64, u64)>;

    /// The identity with this issuer and subject.
    fn identity_by_subject(&mut self, issuer: &str, subject: &str) -> Result<Option<IdentityRow>>;
    /// Identities of a user.
    fn identities_for(&mut self, user_id: &str) -> Result<Vec<IdentityRow>>;
    /// An OTP by id.
    fn otp_get(&mut self, id: &str) -> Result<Option<OtpRow>>;
    /// All OAuth clients.
    fn oauth_clients_list(&mut self) -> Result<Vec<ClientRow>>;
    /// All channel definitions.
    fn channels_list(&mut self) -> Result<Vec<ChannelDef>>;
    /// Whether a rule without a current record (a channel rule) holds.
    fn rule_holds(&mut self, rule: &TypedRule, req: &RequestValues) -> Result<bool>;
    /// The stored state of a `ydoc` field and its pending updates, `None`
    /// when the record does not exist.
    fn ydoc_load(
        &mut self,
        c: &Collection,
        record_id: &str,
        field: &str,
    ) -> Result<Option<YdocStored>>;
    /// The consent a user gave a client.
    fn oauth_consent_get(
        &mut self,
        collection_id: &str,
        user_id: &str,
        client_id: &str,
    ) -> Result<Option<ConsentRow>>;
    /// An unused, unexpired token, without using it.
    fn token_get(
        &mut self,
        hash: &str,
        purpose: TokenPurpose,
        now: &str,
    ) -> Result<Option<TokenRow>>;
    /// All providers.
    fn providers_list(&mut self) -> Result<Vec<ProviderRow>>;
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

    /// Inserts a file row.
    fn file_insert(&mut self, f: &FileRow) -> Result<()>;
    /// Marks files deleted by storage key.
    fn files_mark_deleted(&mut self, storage_keys: &[String], now: &str) -> Result<u64>;
    /// Marks the files of a collection deleted, optionally only of one
    /// record or one field (field id).
    fn files_mark_deleted_where(
        &mut self,
        collection_id: &str,
        record_id: Option<&str>,
        field: Option<&str>,
        now: &str,
    ) -> Result<u64>;
    /// Removes rows (after their blobs are gone).
    fn files_remove(&mut self, ids: &[String]) -> Result<u64>;

    /// Links an identity.
    fn identity_insert(&mut self, i: &IdentityRow) -> Result<()>;
    /// Records a login through an identity.
    fn identity_touch(&mut self, id: &str, now: &str) -> Result<()>;
    /// Unlinks an identity of `user_id`; returns whether it existed.
    fn identity_delete(&mut self, user_id: &str, id: &str) -> Result<bool>;
    /// Inserts or replaces a channel; a taken name or pattern is a conflict.
    fn channel_put(&mut self, c: &ChannelDef) -> Result<()>;
    /// Deletes a channel.
    fn channel_delete(&mut self, id: &str) -> Result<bool>;
    /// Appends Yjs updates of a document; returns the id of the last one.
    fn ydoc_append(
        &mut self,
        collection_id: &str,
        record_id: &str,
        field: &str,
        updates: &[Vec<u8>],
        now: &str,
    ) -> Result<i64>;
    /// Writes the merged state to the record and deletes the updates up to
    /// `up_to` (inclusive) that it contains.
    fn ydoc_compact(
        &mut self,
        c: &Collection,
        record_id: &str,
        field: &str,
        state: &[u8],
        up_to: i64,
    ) -> Result<()>;
    /// Deletes stored document updates of a collection, optionally only of
    /// one record or one field (record deleted, trash purged).
    fn ydoc_updates_delete_where(
        &mut self,
        collection_id: &str,
        record_id: Option<&str>,
        field: Option<&str>,
    ) -> Result<u64>;
    /// Inserts or replaces an OAuth client.
    fn oauth_client_put(&mut self, c: &ClientRow) -> Result<()>;
    /// Deletes a client, its codes and consents; returns whether it existed.
    fn oauth_client_delete(&mut self, id: &str) -> Result<bool>;
    /// Stores an authorization code.
    fn oauth_code_insert(&mut self, c: &CodeRow) -> Result<()>;
    /// Marks a code used and returns it as it was (also when already used
    /// or expired, so callers can detect reuse).
    fn oauth_code_take(&mut self, hash: &str, now: &str) -> Result<Option<CodeRow>>;
    /// Records the session an exchange created.
    fn oauth_code_set_issued(&mut self, hash: &str, session_id: &str) -> Result<()>;
    /// Stores a consent.
    fn oauth_consent_put(&mut self, c: &ConsentRow) -> Result<()>;
    /// Deletes the consents of a user (user deleted).
    fn oauth_consents_delete_for(&mut self, user_id: &str) -> Result<u64>;
    /// Deletes codes that expired before `before`.
    fn oauth_codes_gc(&mut self, before: &str) -> Result<u64>;
    /// Revokes every active session of a family (the browser session and the
    /// client sessions created from it); returns their ids.
    fn sessions_revoke_family(
        &mut self,
        family_id: &str,
        reason: &str,
        now: &str,
    ) -> Result<Vec<String>>;
    /// Revokes every active session of an OIDC client; returns their ids.
    fn sessions_revoke_client(
        &mut self,
        client_id: &str,
        reason: &str,
        now: &str,
    ) -> Result<Vec<String>>;
    /// Replaces the lifecycle times of a signing key.
    fn key_update(&mut self, k: &KeyRow) -> Result<()>;
    /// Deletes a signing key.
    fn key_delete(&mut self, kid: &str) -> Result<()>;
    /// Unlinks all identities of a user (user deleted).
    fn identities_delete_for(&mut self, user_id: &str) -> Result<u64>;
    /// Stores a single-use token.
    fn token_insert(&mut self, t: &TokenRow) -> Result<()>;
    /// Uses a token: returns it when it exists, has `purpose`, is unused and
    /// not expired at `now`, and marks it used.
    fn token_take(
        &mut self,
        hash: &str,
        purpose: TokenPurpose,
        now: &str,
    ) -> Result<Option<TokenRow>>;
    /// Voids unused tokens of a user for a purpose.
    fn tokens_void(&mut self, user_id: &str, purpose: TokenPurpose, now: &str) -> Result<u64>;
    /// Stores an OTP.
    fn otp_insert(&mut self, o: &OtpRow) -> Result<()>;
    /// Updates attempts / used_at of an OTP.
    fn otp_update(&mut self, o: &OtpRow) -> Result<()>;
    /// Voids unused OTPs of a user.
    fn otps_void(&mut self, user_id: &str, now: &str) -> Result<u64>;
    /// Inserts or replaces a provider.
    fn provider_put(&mut self, p: &ProviderRow) -> Result<()>;
    /// Deletes a provider.
    fn provider_delete(&mut self, id: &str) -> Result<bool>;
    /// Deletes expired or used tokens and OTPs older than `before`.
    fn tokens_gc(&mut self, before: &str) -> Result<u64>;
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
    /// Writer queue depth and database file sizes (dashboard, metrics).
    fn stats(&self) -> StoreStats {
        StoreStats::default()
    }

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
