//! The core `Store` port over `data.db` (SPEC §4.4).

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use rackend_core::auth::broker::{IdentityRow, OtpRow, ProviderRow, TokenPurpose, TokenRow};
use rackend_core::auth::keys::KeyRow;
use rackend_core::auth::{PasskeyRow, SessionRow, SubjectType, Superuser};
use rackend_core::domain::files::FileRow;
use rackend_core::domain::{
    ActorType, AuditEntry, Collection, DdlStep, HistoryEntry, Record, TrashEntry, TrashKind,
};
use rackend_core::ports::{
    AnyBox, AuditQuery, ReadJob, ReadTx, RecordPage, RecordQuery, SettingRow, Store, StoreFuture,
    WriteJob, WriteTx,
};
use rackend_core::rules::TypedRule;
use rackend_core::rules::eval::RequestValues;
use rackend_core::{Error as CoreError, Result as CoreResult};
use rusqlite::types::Value as SqlValue;
use rusqlite::{Connection, OptionalExtension, Row, TransactionBehavior, params, params_from_iter};
use serde_json::{Map, Value};

use crate::db::SqliteDb;
use crate::records::{self, map_err};

/// Default timeout of rule-produced queries (setting `limits.rule_timeout_ms`).
pub const DEFAULT_QUERY_TIMEOUT_MS: u64 = 2000;

/// `Store` implementation on SQLite.
#[derive(Debug, Clone)]
pub struct SqliteStore {
    db: SqliteDb,
    timeout_ms: Arc<AtomicU64>,
}

impl SqliteStore {
    /// Wraps a started `data.db`.
    pub fn new(db: SqliteDb) -> Self {
        Self {
            db,
            timeout_ms: Arc::new(AtomicU64::new(DEFAULT_QUERY_TIMEOUT_MS)),
        }
    }

    /// The underlying database (for shutdown and maintenance jobs).
    pub fn db(&self) -> &SqliteDb {
        &self.db
    }

    fn timeout(&self) -> Duration {
        Duration::from_millis(self.timeout_ms.load(Ordering::Relaxed))
    }
}

impl Store for SqliteStore {
    fn stats(&self) -> rackend_core::ports::StoreStats {
        let (db_size, wal_size) = self.db.file_sizes();
        rackend_core::ports::StoreStats {
            write_queue_depth: self.db.queue_depth(),
            db_size,
            wal_size,
        }
    }

    fn set_query_timeout(&self, ms: u64) {
        self.timeout_ms.store(ms.max(1), Ordering::Relaxed);
    }

    fn read_job(&self, job: ReadJob) -> StoreFuture<'_, AnyBox> {
        let timeout = self.timeout();
        Box::pin(async move {
            self.db
                .with_reader(move |conn| -> CoreResult<AnyBox> {
                    // A deferred transaction gives the job one consistent snapshot.
                    let tx = conn.transaction().map_err(sql)?;
                    let mut stx = SqliteTx { conn: &tx, timeout };
                    job(&mut stx)
                    // `tx` is dropped here, which rolls the read transaction back.
                })
                .await
                .map_err(CoreError::from)?
        })
    }

    fn write_job(&self, job: WriteJob) -> StoreFuture<'_, AnyBox> {
        let timeout = self.timeout();
        Box::pin(async move {
            self.db
                .with_writer(move |conn| -> CoreResult<AnyBox> {
                    let tx = conn
                        .transaction_with_behavior(TransactionBehavior::Immediate)
                        .map_err(sql)?;
                    let out = {
                        let mut stx = SqliteTx { conn: &tx, timeout };
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
    timeout: Duration,
}

fn sql(err: rusqlite::Error) -> CoreError {
    map_err(err)
}

fn json_text(v: &Option<Value>) -> Option<String> {
    v.as_ref().map(Value::to_string)
}

fn parse_json(s: Option<String>) -> Option<Value> {
    s.and_then(|s| serde_json::from_str(&s).ok())
}

fn collection_from_row(row: &Row<'_>) -> rusqlite::Result<Value> {
    let get_json = |i: usize| -> rusqlite::Result<Value> {
        let s: String = row.get(i)?;
        Ok(serde_json::from_str(&s).unwrap_or(Value::Null))
    };
    Ok(serde_json::json!({
        "id": row.get::<_, String>(0)?,
        "name": row.get::<_, String>(1)?,
        "type": row.get::<_, String>(2)?,
        "fields": get_json(3)?,
        "indexes": get_json(4)?,
        "rules": get_json(5)?,
        "options": get_json(6)?,
        "created": row.get::<_, String>(7)?,
        "updated": row.get::<_, String>(8)?,
    }))
}

fn history_from_row(row: &Row<'_>) -> rusqlite::Result<HistoryEntry> {
    Ok(HistoryEntry {
        id: row.get(0)?,
        collection_id: row.get(1)?,
        action: row.get(2)?,
        before: parse_json(row.get(3)?),
        after: parse_json(row.get(4)?),
        ddl: row.get(5)?,
        actor: row.get(6)?,
        ts: row.get(7)?,
    })
}

const TRASH_COLS: &str = "id, kind, collection_id, original_name, storage_name, definition, deleted_at, purge_after, actor";

fn trash_from_row(row: &Row<'_>) -> rusqlite::Result<TrashEntry> {
    let kind: String = row.get(1)?;
    Ok(TrashEntry {
        id: row.get(0)?,
        kind: if kind == "collection" {
            TrashKind::Collection
        } else {
            TrashKind::Field
        },
        collection_id: row.get(2)?,
        original_name: row.get(3)?,
        storage_name: row.get(4)?,
        definition: parse_json(row.get(5)?).unwrap_or(Value::Null),
        deleted_at: row.get(6)?,
        purge_after: row.get(7)?,
        actor: row.get(8)?,
    })
}

const AUDIT_COLS: &str = "id, ts, actor_type, actor_id, actor_label, action, collection_id, record_id, diff, ip, user_agent, request_id, meta";

fn audit_from_row(row: &Row<'_>) -> rusqlite::Result<AuditEntry> {
    let actor: String = row.get(2)?;
    Ok(AuditEntry {
        id: row.get(0)?,
        ts: row.get(1)?,
        actor_type: match actor.as_str() {
            "superuser" => ActorType::Superuser,
            "user" => ActorType::User,
            "client" => ActorType::Client,
            "system" => ActorType::System,
            _ => ActorType::Anonymous,
        },
        actor_id: row.get(3)?,
        actor_label: row.get(4)?,
        action: row.get(5)?,
        collection_id: row.get(6)?,
        record_id: row.get(7)?,
        diff: parse_json(row.get(8)?),
        ip: row.get(9)?,
        user_agent: row.get(10)?,
        request_id: row.get(11)?,
        meta: parse_json(row.get(12)?),
    })
}

fn superuser_from_row(row: &Row<'_>) -> rusqlite::Result<Superuser> {
    Ok(Superuser {
        id: row.get(0)?,
        email: row.get(1)?,
        password_hash: row.get(2)?,
        created: row.get(3)?,
        updated: row.get(4)?,
    })
}

const SESSION_COLS: &str = "id, family_id, subject_type, collection_id, user_id, client_id, auth_method, access_hash, access_expires, refresh_hash, prev_refresh_hash, refresh_expires, created, last_seen, revoked_at, revoked_reason, ip, user_agent";

fn session_from_row(row: &Row<'_>) -> rusqlite::Result<SessionRow> {
    let st: String = row.get(2)?;
    Ok(SessionRow {
        id: row.get(0)?,
        family_id: row.get(1)?,
        subject_type: if st == "superuser" {
            SubjectType::Superuser
        } else {
            SubjectType::User
        },
        collection_id: row.get(3)?,
        user_id: row.get(4)?,
        client_id: row.get(5)?,
        auth_method: row.get(6)?,
        access_hash: row.get(7)?,
        access_expires: row.get(8)?,
        refresh_hash: row.get(9)?,
        prev_refresh_hash: row.get(10)?,
        refresh_expires: row.get(11)?,
        created: row.get(12)?,
        last_seen: row.get(13)?,
        revoked_at: row.get(14)?,
        revoked_reason: row.get(15)?,
        ip: row.get(16)?,
        user_agent: row.get(17)?,
    })
}

const PASSKEY_COLS: &str =
    "id, owner_type, collection_id, user_id, credential_id, credential, name, created, last_used";

fn passkey_from_row(row: &Row<'_>) -> rusqlite::Result<PasskeyRow> {
    let owner: String = row.get(1)?;
    Ok(PasskeyRow {
        id: row.get(0)?,
        owner_type: if owner == "superuser" {
            SubjectType::Superuser
        } else {
            SubjectType::User
        },
        collection_id: row.get(2)?,
        user_id: row.get(3)?,
        credential_id: row.get(4)?,
        credential: row.get(5)?,
        name: row.get(6)?,
        created: row.get(7)?,
        last_used: row.get(8)?,
    })
}

fn session_params(s: &SessionRow) -> Vec<SqlValue> {
    let opt = |v: &Option<String>| v.clone().map_or(SqlValue::Null, SqlValue::Text);
    vec![
        SqlValue::Text(s.id.clone()),
        SqlValue::Text(s.family_id.clone()),
        SqlValue::Text(s.subject_type.as_str().into()),
        opt(&s.collection_id),
        SqlValue::Text(s.user_id.clone()),
        opt(&s.client_id),
        SqlValue::Text(s.auth_method.clone()),
        SqlValue::Text(s.access_hash.clone()),
        SqlValue::Text(s.access_expires.clone()),
        opt(&s.refresh_hash),
        opt(&s.prev_refresh_hash),
        opt(&s.refresh_expires),
        SqlValue::Text(s.created.clone()),
        SqlValue::Text(s.last_seen.clone()),
        opt(&s.revoked_at),
        opt(&s.revoked_reason),
        opt(&s.ip),
        opt(&s.user_agent),
    ]
}

impl SqliteTx<'_> {
    fn collect<T>(
        &self,
        query: &str,
        params: impl rusqlite::Params,
        f: impl FnMut(&Row<'_>) -> rusqlite::Result<T>,
    ) -> CoreResult<Vec<T>> {
        let mut stmt = self.conn.prepare_cached(query).map_err(sql)?;
        let rows = stmt.query_map(params, f).map_err(sql)?;
        rows.collect::<rusqlite::Result<Vec<_>>>().map_err(sql)
    }
}

const FILE_COLS: &str = "id, collection_id, record_id, field, storage_key, original_name, size, mime, width, height, sha256, created, deleted_at";

fn file_from_row(r: &Row<'_>) -> rusqlite::Result<FileRow> {
    Ok(FileRow {
        id: r.get(0)?,
        collection_id: r.get(1)?,
        record_id: r.get(2)?,
        field: r.get(3)?,
        storage_key: r.get(4)?,
        original_name: r.get(5)?,
        size: u64::try_from(r.get::<_, i64>(6)?).unwrap_or(0),
        mime: r.get(7)?,
        width: r
            .get::<_, Option<i64>>(8)?
            .and_then(|n| u32::try_from(n).ok()),
        height: r
            .get::<_, Option<i64>>(9)?
            .and_then(|n| u32::try_from(n).ok()),
        sha256: r.get(10)?,
        created: r.get(11)?,
        deleted_at: r.get(12)?,
    })
}

const IDENTITY_COLS: &str =
    "id, collection_id, user_id, provider_id, issuer, subject, email_at_link, created, last_login";

fn identity_from_row(r: &Row<'_>) -> rusqlite::Result<IdentityRow> {
    Ok(IdentityRow {
        id: r.get(0)?,
        collection_id: r.get(1)?,
        user_id: r.get(2)?,
        provider_id: r.get(3)?,
        issuer: r.get(4)?,
        subject: r.get(5)?,
        email_at_link: r.get(6)?,
        created: r.get(7)?,
        last_login: r.get(8)?,
    })
}

fn purpose_from(s: &str) -> TokenPurpose {
    match s {
        "verify_email" => TokenPurpose::VerifyEmail,
        "reset_password" => TokenPurpose::ResetPassword,
        "change_email" => TokenPurpose::ChangeEmail,
        "magic_link" => TokenPurpose::MagicLink,
        _ => TokenPurpose::Exchange,
    }
}

const OTP_COLS: &str = "id, collection_id, user_id, code_hash, purpose, expires, attempts, used_at";

fn otp_from_row(r: &Row<'_>) -> rusqlite::Result<OtpRow> {
    Ok(OtpRow {
        id: r.get(0)?,
        collection_id: r.get(1)?,
        user_id: r.get(2)?,
        code_hash: r.get(3)?,
        purpose: r.get(4)?,
        expires: r.get(5)?,
        attempts: u32::try_from(r.get::<_, i64>(6)?).unwrap_or(u32::MAX),
        used_at: r.get(7)?,
    })
}

const PROVIDER_COLS: &str = "id, name, kind, preset, issuer, client_id, client_secret_enc, scopes, collection_id, auto_link_verified_email, claim_mapping, enabled, created, updated";

fn provider_from_row(r: &Row<'_>) -> rusqlite::Result<ProviderRow> {
    Ok(ProviderRow {
        id: r.get(0)?,
        name: r.get(1)?,
        kind: r.get(2)?,
        preset: r.get(3)?,
        issuer: r.get(4)?,
        client_id: r.get(5)?,
        client_secret_enc: r.get(6)?,
        scopes: r.get(7)?,
        collection_id: r.get(8)?,
        auto_link_verified_email: r.get::<_, i64>(9)? != 0,
        claim_mapping: r.get(10)?,
        enabled: r.get::<_, i64>(11)? != 0,
        created: r.get(12)?,
        updated: r.get(13)?,
    })
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
        self.collect(
            "SELECT key, value, is_secret, updated FROM _settings ORDER BY key",
            [],
            |r| {
                Ok(SettingRow {
                    key: r.get(0)?,
                    value: r.get(1)?,
                    is_secret: r.get::<_, i64>(2)? != 0,
                    updated: r.get(3)?,
                })
            },
        )
    }

    fn collections_list(&mut self) -> CoreResult<Vec<Collection>> {
        let rows = self.collect(
            "SELECT id, name, type, fields, indexes, rules, options, created, updated FROM _collections ORDER BY created, id",
            [],
            collection_from_row,
        )?;
        rows.into_iter()
            .map(|v| {
                Collection::from_input(&v, None).map_err(|e| {
                    CoreError::internal(format!(
                        "stored collection {} is invalid: {e:?}",
                        v["name"]
                    ))
                })
            })
            .collect()
    }

    fn schema_history(
        &mut self,
        collection_id: Option<&str>,
        limit: u32,
    ) -> CoreResult<Vec<HistoryEntry>> {
        self.collect(
            "SELECT id, collection_id, action, before, after, ddl, actor, ts FROM _schema_history
             WHERE ?1 IS NULL OR collection_id = ?1 ORDER BY ts DESC, id DESC LIMIT ?2",
            params![collection_id, limit],
            history_from_row,
        )
    }

    fn trash_list(&mut self) -> CoreResult<Vec<TrashEntry>> {
        self.collect(
            &format!("SELECT {TRASH_COLS} FROM _trash ORDER BY deleted_at DESC, id DESC"),
            [],
            trash_from_row,
        )
    }

    fn trash_get(&mut self, id: &str) -> CoreResult<Option<TrashEntry>> {
        self.conn
            .query_row(
                &format!("SELECT {TRASH_COLS} FROM _trash WHERE id = ?1"),
                [id],
                trash_from_row,
            )
            .optional()
            .map_err(sql)
    }

    fn record_get(&mut self, c: &Collection, id: &str) -> CoreResult<Option<Record>> {
        records::get(self.conn, c, id)
    }

    fn records_query(&mut self, q: &RecordQuery<'_>) -> CoreResult<RecordPage> {
        records::query(self.conn, q, self.timeout)
    }

    fn record_matches(
        &mut self,
        c: &Collection,
        id: &str,
        rule: &TypedRule,
        req: &RequestValues,
    ) -> CoreResult<bool> {
        records::matches(self.conn, c, id, rule, req, self.timeout)
    }

    fn record_values_match(
        &mut self,
        c: &Collection,
        record: &Record,
        rule: &TypedRule,
        req: &RequestValues,
    ) -> CoreResult<bool> {
        records::matches_values(self.conn, c, record, rule, req, self.timeout)
    }

    fn sessions_live(&mut self, ids: &[String], now: &str) -> CoreResult<Vec<String>> {
        if ids.is_empty() {
            return Ok(Vec::new());
        }
        let json = serde_json::to_string(ids).unwrap_or_else(|_| "[]".into());
        self.collect(
            "SELECT s.id FROM _sessions s JOIN json_each(?1) j ON j.value = s.id
             WHERE s.revoked_at IS NULL AND coalesce(s.refresh_expires, s.access_expires) > ?2",
            params![json, now],
            |r| r.get(0),
        )
    }

    fn records_missing(&mut self, c: &Collection, ids: &[String]) -> CoreResult<Vec<String>> {
        records::missing(self.conn, c, ids)
    }

    fn count_multi_valued(&mut self, table: &str, column: &str) -> CoreResult<u64> {
        let c = crate::rules_sql::q(column);
        let n: i64 = self
            .conn
            .query_row(
                &format!(
                    "SELECT count(*) FROM {} WHERE json_valid({c}) AND json_array_length({c}) > 1",
                    crate::rules_sql::q(table)
                ),
                [],
                |r| r.get(0),
            )
            .map_err(sql)?;
        Ok(u64::try_from(n).unwrap_or(0))
    }

    fn audit_query(&mut self, q: &AuditQuery) -> CoreResult<(Vec<AuditEntry>, u64)> {
        let mut conds = Vec::new();
        let mut values: Vec<SqlValue> = Vec::new();
        let mut add = |cond: &str, v: Option<&String>| {
            if let Some(v) = v {
                values.push(SqlValue::Text(v.clone()));
                conds.push(cond.replace('?', &format!("?{}", values.len())));
            }
        };
        add("actor_type = ?", q.actor_type.as_ref());
        add("actor_id = ?", q.actor_id.as_ref());
        add("collection_id = ?", q.collection_id.as_ref());
        add("record_id = ?", q.record_id.as_ref());
        add("ts >= ?", q.from.as_ref());
        add("ts < ?", q.to.as_ref());
        if let Some(a) = &q.action {
            if let Some(prefix) = a.strip_suffix('.') {
                values.push(SqlValue::Text(format!("{prefix}.%")));
                conds.push(format!("action LIKE ?{}", values.len()));
            } else {
                values.push(SqlValue::Text(a.clone()));
                conds.push(format!("action = ?{}", values.len()));
            }
        }
        let clause = if conds.is_empty() {
            "1".to_owned()
        } else {
            conds.join(" AND ")
        };
        let total: i64 = self
            .conn
            .query_row(
                &format!("SELECT count(*) FROM _audit WHERE {clause}"),
                params_from_iter(values.iter()),
                |r| r.get(0),
            )
            .map_err(sql)?;
        let dir = if q.newest_first { "DESC" } else { "ASC" };
        values.push(SqlValue::Integer(i64::from(q.limit)));
        values.push(SqlValue::Integer(
            i64::try_from(q.offset).unwrap_or(i64::MAX),
        ));
        let n = values.len();
        let items = self.collect(
            &format!(
                "SELECT {AUDIT_COLS} FROM _audit WHERE {clause} ORDER BY ts {dir}, id {dir} LIMIT ?{} OFFSET ?{n}",
                n - 1
            ),
            params_from_iter(values.iter()),
            audit_from_row,
        )?;
        Ok((items, u64::try_from(total).unwrap_or(0)))
    }

    fn superusers_count(&mut self) -> CoreResult<u64> {
        let n: i64 = self
            .conn
            .query_row("SELECT count(*) FROM _superusers", [], |r| r.get(0))
            .map_err(sql)?;
        Ok(u64::try_from(n).unwrap_or(0))
    }

    fn superuser_by_email(&mut self, email: &str) -> CoreResult<Option<Superuser>> {
        self.conn
            .query_row(
                "SELECT id, email, password_hash, created, updated FROM _superusers WHERE email = ?1",
                [email],
                superuser_from_row,
            )
            .optional()
            .map_err(sql)
    }

    fn superuser_by_id(&mut self, id: &str) -> CoreResult<Option<Superuser>> {
        self.conn
            .query_row(
                "SELECT id, email, password_hash, created, updated FROM _superusers WHERE id = ?1",
                [id],
                superuser_from_row,
            )
            .optional()
            .map_err(sql)
    }

    fn superusers_list(&mut self) -> CoreResult<Vec<Superuser>> {
        self.collect(
            "SELECT id, email, password_hash, created, updated FROM _superusers ORDER BY created, id",
            [],
            superuser_from_row,
        )
    }

    fn session_get(&mut self, id: &str) -> CoreResult<Option<SessionRow>> {
        self.conn
            .query_row(
                &format!("SELECT {SESSION_COLS} FROM _sessions WHERE id = ?1"),
                [id],
                session_from_row,
            )
            .optional()
            .map_err(sql)
    }

    fn setup_token_valid(&mut self, hash: &str, now: &str) -> CoreResult<bool> {
        self.conn
            .query_row(
                "SELECT EXISTS (SELECT 1 FROM _setup_tokens WHERE hash = ?1 AND used_at IS NULL AND expires > ?2)",
                [hash, now],
                |r| r.get(0),
            )
            .map_err(sql)
    }

    fn auth_find_by_email(
        &mut self,
        c: &Collection,
        email: &str,
    ) -> CoreResult<Option<(Record, Option<String>)>> {
        let Some(id) = self
            .conn
            .query_row(
                &format!(
                    "SELECT \"id\" FROM {} WHERE \"email\" = ?1",
                    crate::rules_sql::q(&c.name)
                ),
                [email],
                |r| r.get::<_, String>(0),
            )
            .optional()
            .map_err(sql)?
        else {
            return Ok(None);
        };
        let rec = records::get(self.conn, c, &id)?;
        let hash = self.auth_password_hash(c, &id)?;
        Ok(rec.map(|r| (r, hash)))
    }

    fn auth_password_hash(&mut self, c: &Collection, id: &str) -> CoreResult<Option<String>> {
        self.conn
            .query_row(
                &format!(
                    "SELECT \"password_hash\" FROM {} WHERE \"id\" = ?1",
                    crate::rules_sql::q(&c.name)
                ),
                [id],
                |r| r.get::<_, Option<String>>(0),
            )
            .optional()
            .map(Option::flatten)
            .map_err(sql)
    }

    fn sessions_for(&mut self, user_id: &str) -> CoreResult<Vec<SessionRow>> {
        self.collect(
            &format!(
                "SELECT {SESSION_COLS} FROM _sessions WHERE user_id = ?1 ORDER BY created DESC"
            ),
            [user_id],
            session_from_row,
        )
    }

    fn passkeys_for(&mut self, user_id: &str) -> CoreResult<Vec<PasskeyRow>> {
        self.collect(
            &format!("SELECT {PASSKEY_COLS} FROM _passkeys WHERE user_id = ?1 ORDER BY created"),
            [user_id],
            passkey_from_row,
        )
    }

    fn passkey_by_credential(&mut self, credential_id: &str) -> CoreResult<Option<PasskeyRow>> {
        self.conn
            .query_row(
                &format!("SELECT {PASSKEY_COLS} FROM _passkeys WHERE credential_id = ?1"),
                [credential_id],
                passkey_from_row,
            )
            .optional()
            .map_err(sql)
    }

    fn keys_list(&mut self) -> CoreResult<Vec<KeyRow>> {
        self.collect(
            "SELECT kid, alg, private_enc, public_jwk, created, activated_at, retired_at, delete_after
             FROM _keys ORDER BY created",
            [],
            |r| {
                Ok(KeyRow {
                    kid: r.get(0)?,
                    alg: r.get(1)?,
                    private_enc: r.get(2)?,
                    public_jwk: r.get(3)?,
                    created: r.get(4)?,
                    activated_at: r.get(5)?,
                    retired_at: r.get(6)?,
                    delete_after: r.get(7)?,
                })
            },
        )
    }

    fn file_by_key(&mut self, storage_key: &str) -> CoreResult<Option<FileRow>> {
        self.conn
            .query_row(
                &format!(
                    "SELECT {FILE_COLS} FROM _files WHERE storage_key = ?1 AND deleted_at IS NULL"
                ),
                [storage_key],
                file_from_row,
            )
            .optional()
            .map_err(sql)
    }

    fn files_deleted(&mut self, limit: u32) -> CoreResult<Vec<FileRow>> {
        self.collect(
            &format!("SELECT {FILE_COLS} FROM _files WHERE deleted_at IS NOT NULL ORDER BY deleted_at LIMIT ?1"),
            [limit],
            file_from_row,
        )
    }

    fn files_known_keys(&mut self, keys: &[String]) -> CoreResult<Vec<String>> {
        let mut out = Vec::new();
        for chunk in keys.chunks(500) {
            let marks = vec!["?"; chunk.len()].join(", ");
            out.extend(self.collect(
                &format!("SELECT storage_key FROM _files WHERE storage_key IN ({marks})"),
                params_from_iter(chunk.iter()),
                |r| r.get(0),
            )?);
        }
        Ok(out)
    }

    fn identity_by_subject(
        &mut self,
        issuer: &str,
        subject: &str,
    ) -> CoreResult<Option<IdentityRow>> {
        self.conn
            .query_row(
                &format!(
                    "SELECT {IDENTITY_COLS} FROM _identities WHERE issuer = ?1 AND subject = ?2"
                ),
                [issuer, subject],
                identity_from_row,
            )
            .optional()
            .map_err(sql)
    }

    fn identities_for(&mut self, user_id: &str) -> CoreResult<Vec<IdentityRow>> {
        self.collect(
            &format!("SELECT {IDENTITY_COLS} FROM _identities WHERE user_id = ?1 ORDER BY created"),
            [user_id],
            identity_from_row,
        )
    }

    fn otp_get(&mut self, id: &str) -> CoreResult<Option<OtpRow>> {
        self.conn
            .query_row(
                &format!("SELECT {OTP_COLS} FROM _otps WHERE id = ?1"),
                [id],
                otp_from_row,
            )
            .optional()
            .map_err(sql)
    }

    fn token_get(
        &mut self,
        hash: &str,
        purpose: TokenPurpose,
        now: &str,
    ) -> CoreResult<Option<TokenRow>> {
        self.conn
            .query_row(
                "SELECT hash, purpose, collection_id, user_id, payload, expires, used_at FROM _verification_tokens
                 WHERE hash = ?1 AND purpose = ?2 AND used_at IS NULL AND expires > ?3",
                params![hash, purpose.as_str(), now],
                |r| {
                    Ok(TokenRow {
                        hash: r.get(0)?,
                        purpose: purpose_from(&r.get::<_, String>(1)?),
                        collection_id: r.get(2)?,
                        user_id: r.get(3)?,
                        payload: r.get(4)?,
                        expires: r.get(5)?,
                        used_at: r.get(6)?,
                    })
                },
            )
            .optional()
            .map_err(sql)
    }

    fn providers_list(&mut self) -> CoreResult<Vec<ProviderRow>> {
        self.collect(
            &format!("SELECT {PROVIDER_COLS} FROM _auth_providers ORDER BY name"),
            [],
            provider_from_row,
        )
    }

    fn files_usage(&mut self) -> CoreResult<(u64, u64)> {
        self.conn
            .query_row(
                "SELECT count(*), coalesce(sum(size), 0) FROM _files WHERE deleted_at IS NULL",
                [],
                |r| {
                    Ok((
                        u64::try_from(r.get::<_, i64>(0)?).unwrap_or(0),
                        u64::try_from(r.get::<_, i64>(1)?).unwrap_or(0),
                    ))
                },
            )
            .map_err(sql)
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

    fn schema_apply(&mut self, steps: &[DdlStep]) -> CoreResult<String> {
        crate::ddl::apply(self.conn, steps)
    }

    fn schema_render(&mut self, steps: &[DdlStep]) -> CoreResult<String> {
        Ok(crate::ddl::render_all(steps))
    }

    fn collection_put(&mut self, c: &Collection) -> CoreResult<()> {
        self.conn
            .execute(
                "INSERT INTO _collections (id, name, type, fields, indexes, rules, options, created, updated)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
                 ON CONFLICT (id) DO UPDATE SET name = excluded.name, type = excluded.type,
                   fields = excluded.fields, indexes = excluded.indexes, rules = excluded.rules,
                   options = excluded.options, updated = excluded.updated",
                params![
                    c.id.to_string(),
                    c.name,
                    c.kind.as_str(),
                    c.fields_json().to_string(),
                    c.indexes_json().to_string(),
                    c.rules.to_json().to_string(),
                    c.options_json().to_string(),
                    c.created,
                    c.updated
                ],
            )
            .map(drop)
            .map_err(sql)
    }

    fn collection_remove(&mut self, id: &str) -> CoreResult<()> {
        self.conn
            .execute("DELETE FROM _collections WHERE id = ?1", [id])
            .map(drop)
            .map_err(sql)
    }

    fn schema_history_insert(&mut self, h: &HistoryEntry) -> CoreResult<()> {
        self.conn
            .execute(
                "INSERT INTO _schema_history (id, collection_id, action, before, after, ddl, actor, ts)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                params![h.id, h.collection_id, h.action, json_text(&h.before), json_text(&h.after), h.ddl, h.actor, h.ts],
            )
            .map(drop)
            .map_err(sql)
    }

    fn trash_insert(&mut self, t: &TrashEntry) -> CoreResult<()> {
        let kind = match t.kind {
            TrashKind::Field => "field",
            TrashKind::Collection => "collection",
        };
        self.conn
            .execute(
                &format!(
                    "INSERT INTO _trash ({TRASH_COLS}) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)"
                ),
                params![
                    t.id,
                    kind,
                    t.collection_id,
                    t.original_name,
                    t.storage_name,
                    t.definition.to_string(),
                    t.deleted_at,
                    t.purge_after,
                    t.actor
                ],
            )
            .map(drop)
            .map_err(sql)
    }

    fn trash_remove(&mut self, id: &str) -> CoreResult<()> {
        self.conn
            .execute("DELETE FROM _trash WHERE id = ?1", [id])
            .map(drop)
            .map_err(sql)
    }

    fn trash_due(&mut self, now: &str, limit: u32) -> CoreResult<Vec<TrashEntry>> {
        self.collect(
            &format!("SELECT {TRASH_COLS} FROM _trash WHERE purge_after <= ?1 ORDER BY purge_after LIMIT ?2"),
            params![now, limit],
            trash_from_row,
        )
    }

    fn record_insert(&mut self, c: &Collection, r: &Record) -> CoreResult<()> {
        records::insert(self.conn, c, r)
    }

    fn record_update(
        &mut self,
        c: &Collection,
        id: &str,
        updated: &str,
        changes: &Map<String, Value>,
    ) -> CoreResult<()> {
        records::update(self.conn, c, id, updated, changes)
    }

    fn record_delete(&mut self, c: &Collection, id: &str) -> CoreResult<()> {
        records::delete(self.conn, c, id)
    }

    fn audit_insert(&mut self, e: &AuditEntry) -> CoreResult<()> {
        self.conn
            .execute(
                &format!("INSERT INTO _audit ({AUDIT_COLS}) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)"),
                params![
                    e.id,
                    e.ts,
                    e.actor_type.as_str(),
                    e.actor_id,
                    e.actor_label,
                    e.action,
                    e.collection_id,
                    e.record_id,
                    json_text(&e.diff),
                    e.ip,
                    e.user_agent,
                    e.request_id,
                    json_text(&e.meta)
                ],
            )
            .map(drop)
            .map_err(sql)
    }

    fn audit_delete_before(&mut self, before: &str, limit: u32) -> CoreResult<u64> {
        let n = self
            .conn
            .execute(
                "DELETE FROM _audit WHERE id IN (SELECT id FROM _audit WHERE ts < ?1 ORDER BY ts LIMIT ?2)",
                params![before, limit],
            )
            .map_err(sql)?;
        Ok(n as u64)
    }

    fn superuser_insert(&mut self, s: &Superuser) -> CoreResult<()> {
        self.conn
            .execute(
                "INSERT INTO _superusers (id, email, password_hash, created, updated) VALUES (?1, ?2, ?3, ?4, ?5)",
                params![s.id, s.email, s.password_hash, s.created, s.updated],
            )
            .map(drop)
            .map_err(sql)
    }

    fn superuser_update(&mut self, s: &Superuser) -> CoreResult<()> {
        self.conn
            .execute(
                "UPDATE _superusers SET email = ?2, password_hash = ?3, updated = ?4 WHERE id = ?1",
                params![s.id, s.email, s.password_hash, s.updated],
            )
            .map(drop)
            .map_err(sql)
    }

    fn superuser_delete(&mut self, id: &str) -> CoreResult<()> {
        self.conn
            .execute("DELETE FROM _superusers WHERE id = ?1", [id])
            .map(drop)
            .map_err(sql)
    }

    fn session_insert(&mut self, s: &SessionRow) -> CoreResult<()> {
        let placeholders: Vec<String> = (1..=18).map(|i| format!("?{i}")).collect();
        self.conn
            .execute(
                &format!(
                    "INSERT INTO _sessions ({SESSION_COLS}) VALUES ({})",
                    placeholders.join(", ")
                ),
                params_from_iter(session_params(s)),
            )
            .map(drop)
            .map_err(sql)
    }

    fn session_update(&mut self, s: &SessionRow) -> CoreResult<()> {
        let sets: Vec<String> = SESSION_COLS
            .split(", ")
            .enumerate()
            .skip(1)
            .map(|(i, c)| format!("{c} = ?{}", i + 1))
            .collect();
        self.conn
            .execute(
                &format!("UPDATE _sessions SET {} WHERE id = ?1", sets.join(", ")),
                params_from_iter(session_params(s)),
            )
            .map(drop)
            .map_err(sql)
    }

    fn sessions_revoke_for(
        &mut self,
        user_id: &str,
        keep: Option<&str>,
        reason: &str,
        now: &str,
    ) -> CoreResult<Vec<String>> {
        let ids = self.collect(
            "SELECT id FROM _sessions WHERE user_id = ?1 AND revoked_at IS NULL AND (?2 IS NULL OR id != ?2)",
            params![user_id, keep],
            |r| r.get::<_, String>(0),
        )?;
        self.conn
            .execute(
                "UPDATE _sessions SET revoked_at = ?3, revoked_reason = ?4
                 WHERE user_id = ?1 AND revoked_at IS NULL AND (?2 IS NULL OR id != ?2)",
                params![user_id, keep, now, reason],
            )
            .map_err(sql)?;
        Ok(ids)
    }

    fn sessions_delete_before(&mut self, before: &str) -> CoreResult<u64> {
        let n = self
            .conn
            .execute(
                "DELETE FROM _sessions WHERE (revoked_at IS NOT NULL AND revoked_at < ?1)
                 OR (coalesce(refresh_expires, access_expires) < ?1)",
                [before],
            )
            .map_err(sql)?;
        Ok(n as u64)
    }

    fn setup_token_replace(&mut self, hash: &str, expires: &str) -> CoreResult<()> {
        self.conn
            .execute("DELETE FROM _setup_tokens", [])
            .map_err(sql)?;
        self.conn
            .execute(
                "INSERT INTO _setup_tokens (hash, expires) VALUES (?1, ?2)",
                [hash, expires],
            )
            .map(drop)
            .map_err(sql)
    }

    fn setup_token_use(&mut self, hash: &str, now: &str) -> CoreResult<()> {
        self.conn
            .execute(
                "UPDATE _setup_tokens SET used_at = ?2 WHERE hash = ?1",
                [hash, now],
            )
            .map(drop)
            .map_err(sql)
    }

    fn auth_password_set(
        &mut self,
        c: &Collection,
        id: &str,
        hash: Option<&str>,
    ) -> CoreResult<()> {
        self.conn
            .execute(
                &format!(
                    "UPDATE {} SET \"password_hash\" = ?2 WHERE \"id\" = ?1",
                    crate::rules_sql::q(&c.name)
                ),
                params![id, hash],
            )
            .map(drop)
            .map_err(sql)
    }

    fn passkey_insert(&mut self, p: &PasskeyRow) -> CoreResult<()> {
        self.conn
            .execute(
                &format!(
                    "INSERT INTO _passkeys ({PASSKEY_COLS}) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)"
                ),
                params![
                    p.id,
                    p.owner_type.as_str(),
                    p.collection_id,
                    p.user_id,
                    p.credential_id,
                    p.credential,
                    p.name,
                    p.created,
                    p.last_used
                ],
            )
            .map(drop)
            .map_err(sql)
    }

    fn passkey_update(&mut self, p: &PasskeyRow) -> CoreResult<()> {
        self.conn
            .execute(
                "UPDATE _passkeys SET credential = ?2, last_used = ?3, name = ?4 WHERE id = ?1",
                params![p.id, p.credential, p.last_used, p.name],
            )
            .map(drop)
            .map_err(sql)
    }

    fn passkey_delete(&mut self, user_id: &str, id: &str) -> CoreResult<bool> {
        self.conn
            .execute(
                "DELETE FROM _passkeys WHERE id = ?1 AND user_id = ?2",
                [id, user_id],
            )
            .map(|n| n > 0)
            .map_err(sql)
    }

    fn passkeys_delete_for(&mut self, user_id: &str) -> CoreResult<u64> {
        self.conn
            .execute("DELETE FROM _passkeys WHERE user_id = ?1", [user_id])
            .map(|n| n as u64)
            .map_err(sql)
    }

    fn key_insert(&mut self, k: &KeyRow) -> CoreResult<()> {
        self.conn
            .execute(
                "INSERT INTO _keys (kid, alg, private_enc, public_jwk, created, activated_at, retired_at, delete_after)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                params![
                    k.kid,
                    k.alg,
                    k.private_enc,
                    k.public_jwk,
                    k.created,
                    k.activated_at,
                    k.retired_at,
                    k.delete_after
                ],
            )
            .map(drop)
            .map_err(sql)
    }

    fn key_set_private(&mut self, kid: &str, private_enc: &str) -> CoreResult<()> {
        self.conn
            .execute(
                "UPDATE _keys SET private_enc = ?2 WHERE kid = ?1",
                [kid, private_enc],
            )
            .map(drop)
            .map_err(sql)
    }

    fn keys_delete_all(&mut self) -> CoreResult<u64> {
        self.conn
            .execute("DELETE FROM _keys", [])
            .map(|n| n as u64)
            .map_err(sql)
    }

    fn file_insert(&mut self, f: &FileRow) -> CoreResult<()> {
        self.conn
            .execute(
                &format!("INSERT INTO _files ({FILE_COLS}) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)"),
                params![
                    f.id,
                    f.collection_id,
                    f.record_id,
                    f.field,
                    f.storage_key,
                    f.original_name,
                    i64::try_from(f.size).unwrap_or(i64::MAX),
                    f.mime,
                    f.width,
                    f.height,
                    f.sha256,
                    f.created,
                    f.deleted_at
                ],
            )
            .map(drop)
            .map_err(sql)
    }

    fn files_mark_deleted(&mut self, storage_keys: &[String], now: &str) -> CoreResult<u64> {
        let mut n = 0;
        for k in storage_keys {
            n += self
                .conn
                .execute(
                    "UPDATE _files SET deleted_at = ?2 WHERE storage_key = ?1 AND deleted_at IS NULL",
                    [k.as_str(), now],
                )
                .map_err(sql)? as u64;
        }
        Ok(n)
    }

    fn files_mark_deleted_where(
        &mut self,
        collection_id: &str,
        record_id: Option<&str>,
        field: Option<&str>,
        now: &str,
    ) -> CoreResult<u64> {
        self.conn
            .execute(
                "UPDATE _files SET deleted_at = ?4
                 WHERE collection_id = ?1 AND deleted_at IS NULL
                   AND (?2 IS NULL OR record_id = ?2) AND (?3 IS NULL OR field = ?3)",
                params![collection_id, record_id, field, now],
            )
            .map(|n| n as u64)
            .map_err(sql)
    }

    fn identity_insert(&mut self, i: &IdentityRow) -> CoreResult<()> {
        self.conn
            .execute(
                &format!("INSERT INTO _identities ({IDENTITY_COLS}) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)"),
                params![i.id, i.collection_id, i.user_id, i.provider_id, i.issuer, i.subject, i.email_at_link, i.created, i.last_login],
            )
            .map(drop)
            .map_err(sql)
    }

    fn identity_touch(&mut self, id: &str, now: &str) -> CoreResult<()> {
        self.conn
            .execute(
                "UPDATE _identities SET last_login = ?2 WHERE id = ?1",
                [id, now],
            )
            .map(drop)
            .map_err(sql)
    }

    fn identity_delete(&mut self, user_id: &str, id: &str) -> CoreResult<bool> {
        self.conn
            .execute(
                "DELETE FROM _identities WHERE id = ?1 AND user_id = ?2",
                [id, user_id],
            )
            .map(|n| n > 0)
            .map_err(sql)
    }

    fn identities_delete_for(&mut self, user_id: &str) -> CoreResult<u64> {
        self.conn
            .execute("DELETE FROM _identities WHERE user_id = ?1", [user_id])
            .map(|n| n as u64)
            .map_err(sql)
    }

    fn token_insert(&mut self, t: &TokenRow) -> CoreResult<()> {
        self.conn
            .execute(
                "INSERT INTO _verification_tokens (hash, purpose, collection_id, user_id, payload, expires, used_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                params![t.hash, t.purpose.as_str(), t.collection_id, t.user_id, t.payload, t.expires, t.used_at],
            )
            .map(drop)
            .map_err(sql)
    }

    fn token_take(
        &mut self,
        hash: &str,
        purpose: TokenPurpose,
        now: &str,
    ) -> CoreResult<Option<TokenRow>> {
        let row = self.token_get(hash, purpose, now)?;
        if row.is_some() {
            self.conn
                .execute(
                    "UPDATE _verification_tokens SET used_at = ?2 WHERE hash = ?1",
                    [hash, now],
                )
                .map_err(sql)?;
        }
        Ok(row)
    }

    fn tokens_void(&mut self, user_id: &str, purpose: TokenPurpose, now: &str) -> CoreResult<u64> {
        self.conn
            .execute(
                "UPDATE _verification_tokens SET used_at = ?3 WHERE user_id = ?1 AND purpose = ?2 AND used_at IS NULL",
                params![user_id, purpose.as_str(), now],
            )
            .map(|n| n as u64)
            .map_err(sql)
    }

    fn otp_insert(&mut self, o: &OtpRow) -> CoreResult<()> {
        self.conn
            .execute(
                &format!("INSERT INTO _otps ({OTP_COLS}) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)"),
                params![
                    o.id,
                    o.collection_id,
                    o.user_id,
                    o.code_hash,
                    o.purpose,
                    o.expires,
                    o.attempts,
                    o.used_at
                ],
            )
            .map(drop)
            .map_err(sql)
    }

    fn otp_update(&mut self, o: &OtpRow) -> CoreResult<()> {
        self.conn
            .execute(
                "UPDATE _otps SET attempts = ?2, used_at = ?3 WHERE id = ?1",
                params![o.id, o.attempts, o.used_at],
            )
            .map(drop)
            .map_err(sql)
    }

    fn otps_void(&mut self, user_id: &str, now: &str) -> CoreResult<u64> {
        self.conn
            .execute(
                "UPDATE _otps SET used_at = ?2 WHERE user_id = ?1 AND used_at IS NULL",
                [user_id, now],
            )
            .map(|n| n as u64)
            .map_err(sql)
    }

    fn provider_put(&mut self, p: &ProviderRow) -> CoreResult<()> {
        self.conn
            .execute(
                &format!("INSERT OR REPLACE INTO _auth_providers ({PROVIDER_COLS}) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)"),
                params![
                    p.id,
                    p.name,
                    p.kind,
                    p.preset,
                    p.issuer,
                    p.client_id,
                    p.client_secret_enc,
                    p.scopes,
                    p.collection_id,
                    p.auto_link_verified_email,
                    p.claim_mapping,
                    p.enabled,
                    p.created,
                    p.updated
                ],
            )
            .map(drop)
            .map_err(|e| match e {
                rusqlite::Error::SqliteFailure(f, _) if f.code == rusqlite::ErrorCode::ConstraintViolation => {
                    CoreError::Conflict("A provider with this name exists.".into())
                }
                other => sql(other),
            })
    }

    fn provider_delete(&mut self, id: &str) -> CoreResult<bool> {
        self.conn
            .execute("DELETE FROM _auth_providers WHERE id = ?1", [id])
            .map(|n| n > 0)
            .map_err(sql)
    }

    fn tokens_gc(&mut self, before: &str) -> CoreResult<u64> {
        let a = self
            .conn
            .execute(
                "DELETE FROM _verification_tokens WHERE expires < ?1 OR used_at < ?1",
                [before],
            )
            .map_err(sql)?;
        let b = self
            .conn
            .execute(
                "DELETE FROM _otps WHERE expires < ?1 OR used_at < ?1",
                [before],
            )
            .map_err(sql)?;
        Ok((a + b) as u64)
    }

    fn files_remove(&mut self, ids: &[String]) -> CoreResult<u64> {
        let mut n = 0;
        for id in ids {
            n += self
                .conn
                .execute("DELETE FROM _files WHERE id = ?1", [id])
                .map_err(sql)? as u64;
        }
        Ok(n)
    }
}
