//! Sessions for superusers and auth-collection users (SPEC §11.1, §11.5).
//! Every login method ends in [`new_session`]; every token resolves to one
//! `_sessions` row; revoking it invalidates all its tokens immediately.

use std::sync::Arc;
use std::time::Instant;

use serde_json::{Map, Value, json};
use time::{Duration, OffsetDateTime};
use uuid::Uuid;

use super::auth::{
    CachedSession, REFRESH_GRACE, SUPERUSER_ACCESS_TTL, SUPERUSER_REFRESH_TTL, Tokens, audit_auth,
    superuser_json,
};
use super::{App, Principal, RequestCtx};
use crate::auth::token::{TokenKind, ct_eq, hash_token, parse_session_token, session_token};
use crate::auth::{SessionRow, SubjectType, Superuser, password};
use crate::domain::{Actor, ActorType, Collection, Record};
use crate::ports::{StoreExt, WriteTx};
use crate::time::format_timestamp;
use crate::{Error, Result};

/// Who a session is for.
#[derive(Debug, Clone)]
pub struct Subject {
    /// Superuser or user.
    pub kind: SubjectType,
    /// Auth collection (users only).
    pub collection: Option<Arc<Collection>>,
    /// Superuser or record id.
    pub id: String,
    /// Email, for audit labels.
    pub label: Option<String>,
    /// JSON returned with the tokens.
    pub record: Value,
}

impl Subject {
    /// A superuser.
    pub fn superuser(s: &Superuser) -> Self {
        Self {
            kind: SubjectType::Superuser,
            collection: None,
            id: s.id.clone(),
            label: Some(s.email.clone()),
            record: superuser_json(s),
        }
    }

    /// A user of an auth collection.
    pub fn user(c: &Arc<Collection>, r: &Record) -> Self {
        Self {
            kind: SubjectType::User,
            collection: Some(Arc::clone(c)),
            id: r.id.to_string(),
            label: r.get("email").as_str().map(str::to_owned),
            record: Value::Object(r.to_json(c)),
        }
    }

    /// (access, refresh) lifetimes and the idle timeout.
    fn ttls(&self) -> (Duration, Duration, Option<Duration>) {
        match self.collection.as_ref().and_then(|c| c.auth()) {
            Some(o) => (
                Duration::seconds(o.access_ttl_s),
                Duration::seconds(o.refresh_ttl_s),
                Some(Duration::seconds(o.idle_timeout_s)),
            ),
            None => (SUPERUSER_ACCESS_TTL, SUPERUSER_REFRESH_TTL, None),
        }
    }

    /// The audit actor acting as this subject.
    pub fn actor(&self, ctx: &RequestCtx) -> Actor {
        Actor {
            kind: match self.kind {
                SubjectType::Superuser => ActorType::Superuser,
                SubjectType::User => ActorType::User,
            },
            id: Some(self.id.clone()),
            label: self.label.clone(),
            ..ctx.actor()
        }
    }
}

/// A new session row and its tokens.
pub fn new_session(subject: &Subject, ctx: &RequestCtx, method: &str) -> (SessionRow, Tokens) {
    let id = Uuid::now_v7().to_string();
    let access = session_token(TokenKind::Access, &id);
    let refresh = session_token(TokenKind::Refresh, &id);
    let (access_ttl, refresh_ttl, _) = subject.ttls();
    let row = SessionRow {
        id: id.clone(),
        family_id: id.clone(),
        subject_type: subject.kind,
        collection_id: subject.collection.as_ref().map(|c| c.id.to_string()),
        user_id: subject.id.clone(),
        client_id: None,
        auth_method: method.to_owned(),
        access_hash: hash_token(&access),
        access_expires: format_timestamp(ctx.now + access_ttl),
        refresh_hash: Some(hash_token(&refresh)),
        prev_refresh_hash: None,
        refresh_expires: Some(format_timestamp(ctx.now + refresh_ttl)),
        created: format_timestamp(ctx.now),
        last_seen: format_timestamp(ctx.now),
        revoked_at: None,
        revoked_reason: None,
        ip: ctx.ip.clone(),
        user_agent: ctx.user_agent.clone(),
    };
    let tokens = Tokens {
        access_token: access,
        refresh_token: refresh,
        expires_in: access_ttl.whole_seconds(),
        refresh_expires_in: refresh_ttl.whole_seconds(),
        session_id: id,
        record: subject.record.clone(),
    };
    (row, tokens)
}

/// Inserts a session and audits the login.
pub fn start_session(
    tx: &mut dyn WriteTx,
    subject: &Subject,
    ctx: &RequestCtx,
    method: &str,
) -> Result<Tokens> {
    let (row, tokens) = new_session(subject, ctx, method);
    tx.session_insert(&row)?;
    audit_auth(
        tx,
        &subject.actor(ctx),
        "auth.login.success",
        ctx.now,
        json!({ "method": method, "session": row.id, "collection": row.collection_id }),
    )?;
    Ok(tokens)
}

/// The principal of an auth record: `@request.auth.*` sees all its fields.
pub fn user_principal(c: &Collection, r: &Record, session_id: &str) -> Principal {
    let mut fields: Map<String, Value> = r.data.clone();
    fields.insert("created".into(), json!(r.created));
    fields.insert("updated".into(), json!(r.updated));
    Principal::User {
        id: r.id.to_string(),
        collection_id: c.id.to_string(),
        collection_name: c.name.clone(),
        fields,
        session_id: session_id.to_owned(),
    }
}

/// Which sessions a refresh endpoint accepts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RefreshScope<'a> {
    /// `/api/superusers/auth-refresh`.
    Superuser,
    /// `/api/collections/{c}/auth-refresh`, by collection id.
    Collection(&'a str),
}

fn idle_expired(row: &SessionRow, idle: Option<Duration>, now: OffsetDateTime) -> bool {
    let Some(idle) = idle else { return false };
    let Ok(last) = time::OffsetDateTime::parse(
        &row.last_seen,
        &time::format_description::well_known::Rfc3339,
    ) else {
        return false;
    };
    now - last > idle
}

impl App {
    /// Superuser password login. Unknown emails take as long as known ones.
    pub async fn superuser_login(&self, ctx: &RequestCtx, email: &str, pw: &str) -> Result<Tokens> {
        let email = email.trim().to_lowercase();
        let lookup = email.clone();
        let found = self
            .store
            .read(move |tx| tx.superuser_by_email(&lookup))
            .await?;
        let ok = self
            .verify_password(found.as_ref().map(|s| s.password_hash.clone()), pw)
            .await?;
        let Some(s) = found.filter(|_| ok) else {
            self.audit_login_failure(ctx, json!({ "subject": "superuser", "email": email }))
                .await?;
            return Err(Error::Unauthorized);
        };
        if self
            .settings()
            .section("security")
            .and_then(|s| s.get("superuser_require_passkey"))
            .and_then(Value::as_bool)
            == Some(true)
        {
            return Err(Error::Forbidden(
                "Superusers must sign in with a passkey on this instance.".into(),
            ));
        }
        let ctx2 = ctx.clone();
        let t = self
            .store
            .write(move |tx| start_session(tx, &Subject::superuser(&s), &ctx2, "password"))
            .await?;
        self.observe.counters.login("password", true);
        Ok(t)
    }

    /// Password login of an auth-collection user (SPEC §10.6).
    pub async fn user_login(
        &self,
        ctx: &RequestCtx,
        key: &str,
        email: &str,
        pw: &str,
    ) -> Result<Tokens> {
        let c = Arc::clone(
            self.schema
                .get()
                .schema
                .by_id_or_name(key)
                .ok_or(Error::NotFound)?,
        );
        let Some(opts) = c.auth().cloned() else {
            return Err(Error::NotFound);
        };
        if !opts.methods.password {
            return Err(Error::Forbidden(
                "Password login is disabled for this collection.".into(),
            ));
        }
        let email = email.trim().to_lowercase();
        let (c2, lookup) = (Arc::clone(&c), email.clone());
        let found = self
            .store
            .read(move |tx| tx.auth_find_by_email(&c2, &lookup))
            .await?;
        let hash = found.as_ref().and_then(|(_, h)| h.clone());
        let ok = self.verify_password(hash, pw).await?;
        let Some((record, _)) = found.filter(|_| ok) else {
            self.audit_login_failure(ctx, json!({ "collection": c.name, "email": email }))
                .await?;
            return Err(Error::Unauthorized);
        };
        if opts.require_verified_email_to_login
            && record.get("email_verified") != &Value::Bool(true)
        {
            return Err(Error::Forbidden(
                "Verify your email address before signing in.".into(),
            ));
        }
        let ctx2 = ctx.clone();
        let t = self
            .store
            .write(move |tx| start_session(tx, &Subject::user(&c, &record), &ctx2, "password"))
            .await?;
        self.observe.counters.login("password", true);
        Ok(t)
    }

    /// Verifies a password off the async threads; a missing hash runs a dummy
    /// verification so the timing does not reveal whether the account exists.
    pub(crate) async fn verify_password(&self, hash: Option<String>, pw: &str) -> Result<bool> {
        let pw = pw.to_owned();
        self.blocking(move || match hash {
            Some(h) => password::verify(&pw, &h),
            None => {
                password::dummy_verify(&pw);
                false
            }
        })
        .await
    }

    /// Failed logins are audited in their own small transaction (SPEC §14.1).
    async fn audit_login_failure(&self, ctx: &RequestCtx, meta: Value) -> Result<()> {
        self.observe.counters.login("password", false);
        let ctx = ctx.clone();
        self.store
            .write(move |tx| audit_auth(tx, &ctx.actor(), "auth.login.failure", ctx.now, meta))
            .await
    }

    /// Rotates a refresh token (SPEC §11.5). Reuse of the previous token
    /// revokes the session, except once within the 10 s grace window, which
    /// returns the same new pair.
    pub async fn refresh(
        &self,
        ctx: &RequestCtx,
        scope: RefreshScope<'_>,
        refresh_token: &str,
    ) -> Result<Tokens> {
        let session_id = parse_session_token(TokenKind::Refresh, refresh_token)
            .ok_or(Error::Unauthorized)?
            .to_owned();
        let presented = hash_token(refresh_token);
        if let Some(tokens) = self.take_grace(&presented) {
            return Ok(tokens);
        }
        let compiled = self.schema.get();
        let (ctx2, sid, p2) = (ctx.clone(), session_id.clone(), presented.clone());
        let scope_collection = match scope {
            RefreshScope::Superuser => None,
            RefreshScope::Collection(id) => Some(id.to_owned()),
        };
        let result = self
            .store
            .write(
                move |tx| -> Result<std::result::Result<(Tokens, String), bool>> {
                    let now_s = format_timestamp(ctx2.now);
                    let Some(mut row) = tx.session_get(&sid)? else {
                        return Ok(Err(false));
                    };
                    let in_scope = match &scope_collection {
                        None => row.subject_type == SubjectType::Superuser,
                        Some(c) => {
                            row.subject_type == SubjectType::User
                                && row.collection_id.as_deref() == Some(c)
                        }
                    };
                    if row.revoked_at.is_some() || !in_scope || row.client_id.is_some() {
                        return Ok(Err(false));
                    }
                    if row
                        .refresh_expires
                        .as_deref()
                        .is_none_or(|e| e <= now_s.as_str())
                    {
                        return Ok(Err(false));
                    }
                    // Resolve the subject (it may have been deleted).
                    let subject = match row.subject_type {
                        SubjectType::Superuser => tx
                            .superuser_by_id(&row.user_id)?
                            .map(|s| Subject::superuser(&s)),
                        SubjectType::User => {
                            let c = row
                                .collection_id
                                .as_deref()
                                .and_then(|id| compiled.schema.by_id_or_name(id))
                                .cloned();
                            match c {
                                Some(c) => tx
                                    .record_get(&c, &row.user_id)?
                                    .map(|r| Subject::user(&c, &r)),
                                None => None,
                            }
                        }
                    };
                    let Some(subject) = subject else {
                        return Ok(Err(false));
                    };
                    let (access_ttl, refresh_ttl, idle) = subject.ttls();
                    if idle_expired(&row, idle, ctx2.now) {
                        row.revoked_at = Some(now_s);
                        row.revoked_reason = Some("idle".into());
                        tx.session_update(&row)?;
                        return Ok(Err(true));
                    }
                    let current = row.refresh_hash.clone().unwrap_or_default();
                    if !ct_eq(&current, &p2) {
                        if row
                            .prev_refresh_hash
                            .as_deref()
                            .is_some_and(|prev| ct_eq(prev, &p2))
                        {
                            // Reuse of a rotated token: assume theft, revoke.
                            row.revoked_at = Some(now_s);
                            row.revoked_reason = Some("refresh_reuse".into());
                            tx.session_update(&row)?;
                            audit_auth(
                                tx,
                                &subject.actor(&ctx2),
                                "auth.refresh_reuse",
                                ctx2.now,
                                json!({ "session": row.id }),
                            )?;
                            return Ok(Err(true));
                        }
                        return Ok(Err(false));
                    }
                    let access = session_token(TokenKind::Access, &row.id);
                    let refresh = session_token(TokenKind::Refresh, &row.id);
                    row.prev_refresh_hash = Some(current);
                    row.refresh_hash = Some(hash_token(&refresh));
                    row.access_hash = hash_token(&access);
                    row.access_expires = format_timestamp(ctx2.now + access_ttl);
                    row.refresh_expires = Some(format_timestamp(ctx2.now + refresh_ttl));
                    row.last_seen = now_s;
                    tx.session_update(&row)?;
                    Ok(Ok((
                        Tokens {
                            access_token: access,
                            refresh_token: refresh,
                            expires_in: access_ttl.whole_seconds(),
                            refresh_expires_in: refresh_ttl.whole_seconds(),
                            session_id: row.id.clone(),
                            record: subject.record,
                        },
                        p2,
                    )))
                },
            )
            .await?;
        match result {
            Ok((tokens, prev_hash)) => {
                self.sessions.invalidate(&session_id);
                if let Ok(mut g) = self.sessions.grace.lock() {
                    g.retain(|_, (_, at)| at.elapsed() < REFRESH_GRACE);
                    g.insert(prev_hash, (tokens.clone(), Instant::now()));
                }
                Ok(tokens)
            }
            Err(revoked) => {
                if revoked {
                    self.sessions.invalidate(&session_id);
                }
                Err(Error::Unauthorized)
            }
        }
    }

    fn take_grace(&self, prev_hash: &str) -> Option<Tokens> {
        let mut g = self.sessions.grace.lock().ok()?;
        let (tokens, at) = g.remove(prev_hash)?;
        (at.elapsed() < REFRESH_GRACE).then_some(tokens)
    }

    /// Revokes the caller's session.
    pub async fn logout(&self, ctx: &RequestCtx) -> Result<()> {
        let session_id = match &ctx.principal {
            Principal::Superuser { session_id, .. } | Principal::User { session_id, .. } => {
                session_id.clone()
            }
            Principal::Anonymous => return Err(Error::Unauthorized),
        };
        self.revoke_sessions(ctx, &[session_id], "logout", "auth.logout")
            .await
    }

    /// Revokes sessions by id and drops them from the cache.
    async fn revoke_sessions(
        &self,
        ctx: &RequestCtx,
        ids: &[String],
        reason: &str,
        action: &str,
    ) -> Result<()> {
        let (ctx2, ids2, reason) = (ctx.clone(), ids.to_vec(), reason.to_owned());
        let action = action.to_owned();
        self.store
            .write(move |tx| {
                for sid in &ids2 {
                    if let Some(mut row) = tx.session_get(sid)?
                        && row.revoked_at.is_none()
                    {
                        row.revoked_at = Some(format_timestamp(ctx2.now));
                        row.revoked_reason = Some(reason.clone());
                        tx.session_update(&row)?;
                        audit_auth(
                            tx,
                            &ctx2.actor(),
                            &action,
                            ctx2.now,
                            json!({ "session": sid }),
                        )?;
                    }
                }
                Ok(())
            })
            .await?;
        for sid in ids {
            self.sessions.invalidate(sid);
        }
        Ok(())
    }

    /// Resolves an access token to a principal. Revoked sessions fail
    /// immediately (the cache is invalidated on revocation).
    pub async fn authenticate(&self, access_token: &str) -> Result<Principal> {
        let session_id = parse_session_token(TokenKind::Access, access_token)
            .ok_or(Error::Unauthorized)?
            .to_owned();
        let presented = hash_token(access_token);
        let now = format_timestamp(self.clock.now());
        if let Some(c) = self.sessions.get(&session_id) {
            if ct_eq(&c.access_hash, &presented) && c.access_expires.as_str() > now.as_str() {
                self.touch(&session_id);
                return Ok(c.principal);
            }
            return Err(Error::Unauthorized);
        }
        let compiled = self.schema.get();
        let clock_now = self.clock.now();
        let sid = session_id.clone();
        let found = self
            .store
            .read(move |tx| {
                let Some(row) = tx.session_get(&sid)? else {
                    return Ok(None);
                };
                if row.revoked_at.is_some() || row.client_id.is_some() {
                    return Ok(None);
                }
                let principal = match row.subject_type {
                    SubjectType::Superuser => {
                        tx.superuser_by_id(&row.user_id)?
                            .map(|s| Principal::Superuser {
                                id: s.id,
                                email: s.email,
                                session_id: row.id.clone(),
                            })
                    }
                    SubjectType::User => {
                        let c = row
                            .collection_id
                            .as_deref()
                            .and_then(|id| compiled.schema.by_id_or_name(id))
                            .cloned();
                        match c {
                            Some(c) => {
                                let idle = c.auth().map(|o| Duration::seconds(o.idle_timeout_s));
                                if idle_expired(&row, idle, clock_now) {
                                    return Ok(None);
                                }
                                tx.record_get(&c, &row.user_id)?
                                    .map(|r| user_principal(&c, &r, &row.id))
                            }
                            None => None,
                        }
                    }
                };
                Ok(principal.map(|p| (row, p)))
            })
            .await?;
        let Some((row, principal)) = found else {
            return Err(Error::Unauthorized);
        };
        self.sessions.put(
            row.id.clone(),
            CachedSession {
                principal: principal.clone(),
                access_hash: row.access_hash.clone(),
                access_expires: row.access_expires.clone(),
                at: Instant::now(),
            },
        );
        if !ct_eq(&row.access_hash, &presented) || row.access_expires.as_str() <= now.as_str() {
            return Err(Error::Unauthorized);
        }
        self.touch(&row.id);
        Ok(principal)
    }

    /// Updates `last_seen` at most once per minute per session, in the background.
    fn touch(&self, session_id: &str) {
        if !self.sessions.should_touch(session_id) {
            return;
        }
        let store = self.store.clone();
        let (sid, now) = (session_id.to_owned(), format_timestamp(self.clock.now()));
        self.spawner.spawn(
            "session.last_seen",
            Box::pin(async move {
                let _ = store
                    .write(move |tx| {
                        if let Some(mut row) = tx.session_get(&sid)? {
                            row.last_seen = now;
                            tx.session_update(&row)?;
                        }
                        Ok(())
                    })
                    .await;
            }),
        );
    }

    /// The caller's own sessions in an auth collection (device list).
    pub async fn user_sessions(&self, ctx: &RequestCtx, key: &str) -> Result<Vec<Value>> {
        let (user_id, current) = self.own_user(ctx, key)?;
        let rows = self.store.read(move |tx| tx.sessions_for(&user_id)).await?;
        Ok(rows
            .into_iter()
            .filter(|r| r.revoked_at.is_none() && r.client_id.is_none())
            .map(|r| {
                json!({
                    "id": r.id, "created": r.created, "last_seen": r.last_seen, "auth_method": r.auth_method,
                    "ip": r.ip, "user_agent": r.user_agent, "current": r.id == current,
                })
            })
            .collect())
    }

    /// Revokes one of the caller's sessions, or (`all`) every other one.
    pub async fn user_revoke_session(
        &self,
        ctx: &RequestCtx,
        key: &str,
        id: Option<&str>,
    ) -> Result<()> {
        let (user_id, current) = self.own_user(ctx, key)?;
        let rows = self.store.read(move |tx| tx.sessions_for(&user_id)).await?;
        let ids: Vec<String> = match id {
            Some(id) => {
                let row = rows.iter().find(|r| r.id == id).ok_or(Error::NotFound)?;
                vec![row.id.clone()]
            }
            None => rows
                .iter()
                .filter(|r| r.revoked_at.is_none() && r.id != current)
                .map(|r| r.id.clone())
                .collect(),
        };
        self.revoke_sessions(ctx, &ids, "revoked", "auth.session.revoke")
            .await
    }

    /// Revokes all sessions of a user (password change, deletion), except `keep`.
    pub(crate) async fn revoke_all_for(
        &self,
        user_id: &str,
        keep: Option<&str>,
        reason: &str,
        now: OffsetDateTime,
    ) -> Result<()> {
        let (uid, keep2, reason2) = (
            user_id.to_owned(),
            keep.map(str::to_owned),
            reason.to_owned(),
        );
        let revoked = self
            .store
            .write(move |tx| {
                tx.sessions_revoke_for(&uid, keep2.as_deref(), &reason2, &format_timestamp(now))
            })
            .await?;
        for sid in revoked {
            self.sessions.invalidate(&sid);
        }
        Ok(())
    }

    /// The caller as a user of collection `key`, with the current session id.
    fn own_user(&self, ctx: &RequestCtx, key: &str) -> Result<(String, String)> {
        let compiled = self.schema.get();
        let c = compiled.schema.by_id_or_name(key).ok_or(Error::NotFound)?;
        match &ctx.principal {
            Principal::User {
                id,
                collection_id,
                session_id,
                ..
            } if *collection_id == c.id.to_string() => Ok((id.clone(), session_id.clone())),
            Principal::Anonymous => Err(Error::Unauthorized),
            _ => Err(Error::Forbidden(
                "Sign in as a user of this collection.".into(),
            )),
        }
    }
}

impl App {
    /// `GET /api/collections/{c}/auth-methods` (SPEC §10.6). Methods that
    /// need SMTP or a public URL report `enabled: false` until configured.
    pub fn auth_methods(&self, key: &str) -> Result<Value> {
        let compiled = self.schema.get();
        let c = compiled.schema.by_id_or_name(key).ok_or(Error::NotFound)?;
        let o = c.auth().ok_or(Error::NotFound)?;
        let public_url = self.settings().public_url().is_some();
        Ok(json!({
            "password": { "enabled": o.methods.password, "identity_fields": ["email"] },
            "passkey": { "enabled": o.methods.passkey && public_url },
            "otp": { "enabled": false },
            "magic_link": { "enabled": false },
            "providers": [],
            "signup_enabled": o.signup_enabled,
        }))
    }

    /// The id of auth collection `key` (id or name).
    pub fn auth_collection_id(&self, key: &str) -> Result<String> {
        let compiled = self.schema.get();
        let c = compiled.schema.by_id_or_name(key).ok_or(Error::NotFound)?;
        c.auth().ok_or(Error::NotFound)?;
        Ok(c.id.to_string())
    }

    /// Auth collections and their methods, for `GET /api/info`.
    pub fn auth_collections_info(&self) -> Vec<Value> {
        let compiled = self.schema.get();
        compiled
            .schema
            .all()
            .iter()
            .filter(|c| c.auth().is_some())
            .filter_map(|c| {
                self.auth_methods(&c.name)
                    .ok()
                    .map(|m| json!({ "name": c.name, "methods": m }))
            })
            .collect()
    }
}
