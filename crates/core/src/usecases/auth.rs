//! Superuser authentication (SPEC §5.5, §10.7, §11.5, §11.9; ADR 0008).

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration as StdDuration, Instant};

use serde_json::{Value, json};
use time::{Duration, OffsetDateTime};
use uuid::Uuid;

use super::audit::entry;
use super::{App, Principal, RequestCtx};
use crate::auth::token::{
    TokenKind, ct_eq, hash_token, parse_session_token, random_token, session_token,
};
use crate::auth::{SessionRow, SubjectType, Superuser, password};
use crate::domain::Actor;
use crate::error::{FieldError, ValidationErrors};
use crate::ports::{SettingRow, StoreExt, WriteTx};
use crate::settings::normalize_public_url;
use crate::time::format_timestamp;
use crate::{Error, Result};

/// Superuser access token lifetime.
pub const SUPERUSER_ACCESS_TTL: Duration = Duration::minutes(15);
/// Superuser refresh token lifetime.
pub const SUPERUSER_REFRESH_TTL: Duration = Duration::days(7);
/// Setup token lifetime.
pub const SETUP_TOKEN_TTL: Duration = Duration::hours(24);
/// Refresh reuse grace window.
pub const REFRESH_GRACE: StdDuration = StdDuration::from_secs(10);
/// Session cache entry lifetime.
const CACHE_TTL: StdDuration = StdDuration::from_secs(30);
/// Session cache size.
const CACHE_MAX: usize = 10_000;
/// Minimum superuser password length.
pub const SUPERUSER_MIN_PASSWORD: usize = 10;

/// Issued tokens.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Tokens {
    /// `rk_at_…`
    pub access_token: String,
    /// `rk_rt_…`
    pub refresh_token: String,
    /// Access token lifetime in seconds.
    pub expires_in: i64,
    /// Refresh token lifetime in seconds.
    pub refresh_expires_in: i64,
    /// The session.
    pub session_id: String,
    /// The superuser.
    pub superuser: Value,
}

/// A cached, resolved access token.
#[derive(Debug, Clone)]
struct CachedSession {
    principal: Principal,
    access_hash: String,
    access_expires: String,
    at: Instant,
}

/// In-memory session lookup cache (SPEC §11.5) and the refresh grace window.
#[derive(Debug, Default)]
pub struct SessionCache {
    sessions: Mutex<HashMap<String, CachedSession>>,
    /// prev refresh hash → (tokens issued for it, when).
    grace: Mutex<HashMap<String, (Tokens, Instant)>>,
    /// session id → last `last_seen` write.
    seen: Mutex<HashMap<String, Instant>>,
}

impl SessionCache {
    /// Drops a session from the cache (revocation, password change).
    pub fn invalidate(&self, session_id: &str) {
        if let Ok(mut m) = self.sessions.lock() {
            m.remove(session_id);
        }
    }

    /// Drops every cached session.
    pub fn invalidate_all(&self) {
        if let Ok(mut m) = self.sessions.lock() {
            m.clear();
        }
    }

    fn get(&self, id: &str) -> Option<CachedSession> {
        let m = self.sessions.lock().ok()?;
        m.get(id).filter(|c| c.at.elapsed() < CACHE_TTL).cloned()
    }

    fn put(&self, id: String, c: CachedSession) {
        if let Ok(mut m) = self.sessions.lock() {
            if m.len() >= CACHE_MAX {
                m.retain(|_, v| v.at.elapsed() < CACHE_TTL);
                if m.len() >= CACHE_MAX {
                    m.clear();
                }
            }
            m.insert(id, c);
        }
    }

    fn should_touch(&self, id: &str) -> bool {
        let Ok(mut m) = self.seen.lock() else {
            return false;
        };
        match m.get(id) {
            Some(t) if t.elapsed() < StdDuration::from_secs(60) => false,
            _ => {
                if m.len() > CACHE_MAX {
                    m.clear();
                }
                m.insert(id.to_owned(), Instant::now());
                true
            }
        }
    }
}

fn superuser_json(s: &Superuser) -> Value {
    json!({ "id": s.id, "email": s.email, "created": s.created, "updated": s.updated, "collection_name": "_superusers" })
}

fn normalize_email(email: &str) -> Result<String> {
    let e = email.trim().to_lowercase();
    let ok = e.len() <= 254
        && e.split_once('@')
            .is_some_and(|(l, d)| !l.is_empty() && d.contains('.') && !d.starts_with('.'));
    if !ok || e.chars().any(char::is_whitespace) {
        return Err(Error::field(
            "email",
            FieldError::new("invalid_email", "Must be a valid email address."),
        ));
    }
    Ok(e)
}

fn new_session(s: &Superuser, ctx: &RequestCtx, method: &str) -> (SessionRow, Tokens) {
    let id = Uuid::now_v7().to_string();
    let access = session_token(TokenKind::Access, &id);
    let refresh = session_token(TokenKind::Refresh, &id);
    let now = ctx.now;
    let row = SessionRow {
        id: id.clone(),
        family_id: id.clone(),
        subject_type: SubjectType::Superuser,
        collection_id: None,
        user_id: s.id.clone(),
        client_id: None,
        auth_method: method.to_owned(),
        access_hash: hash_token(&access),
        access_expires: format_timestamp(now + SUPERUSER_ACCESS_TTL),
        refresh_hash: Some(hash_token(&refresh)),
        prev_refresh_hash: None,
        refresh_expires: Some(format_timestamp(now + SUPERUSER_REFRESH_TTL)),
        created: format_timestamp(now),
        last_seen: format_timestamp(now),
        revoked_at: None,
        revoked_reason: None,
        ip: ctx.ip.clone(),
        user_agent: ctx.user_agent.clone(),
    };
    let tokens = Tokens {
        access_token: access,
        refresh_token: refresh,
        expires_in: SUPERUSER_ACCESS_TTL.whole_seconds(),
        refresh_expires_in: SUPERUSER_REFRESH_TTL.whole_seconds(),
        session_id: id,
        superuser: superuser_json(s),
    };
    (row, tokens)
}

fn audit_auth(
    tx: &mut dyn WriteTx,
    actor: &Actor,
    action: &str,
    now: OffsetDateTime,
    meta: Value,
) -> Result<()> {
    let mut e = entry(actor, action, now);
    e.meta = Some(meta);
    tx.audit_insert(&e)
}

impl App {
    /// While no superuser exists, creates a new one-time setup token (the
    /// previous one is replaced) and returns it for the startup log
    /// (SPEC §5.5 step 4).
    pub async fn setup_token_if_needed(&self) -> Result<Option<String>> {
        let now = self.clock.now();
        self.store
            .write(move |tx| {
                if tx.superusers_count()? > 0 {
                    return Ok(None);
                }
                let token = random_token();
                tx.setup_token_replace(
                    &hash_token(&token),
                    &format_timestamp(now + SETUP_TOKEN_TTL),
                )?;
                Ok(Some(token))
            })
            .await
    }

    /// Headless first superuser from `RACKEND_SUPERUSER_EMAIL`/`_PASSWORD`;
    /// does nothing if one exists. Returns whether one was created.
    pub async fn ensure_superuser(&self, email: &str, password: &str) -> Result<bool> {
        let email = normalize_email(email)?;
        password::check_policy(password, SUPERUSER_MIN_PASSWORD)
            .map_err(|e| Error::field("password", e))?;
        let pw = password.to_owned();
        let hash = self.blocking(move || password::hash(&pw)).await??;
        let now = self.clock.now();
        self.store
            .write(move |tx| {
                if tx.superusers_count()? > 0 {
                    return Ok(false);
                }
                let s = Superuser {
                    id: Uuid::now_v7().to_string(),
                    email,
                    password_hash: hash,
                    created: format_timestamp(now),
                    updated: format_timestamp(now),
                };
                tx.superuser_insert(&s)?;
                let mut e = entry(&Actor::system(), "superuser.create", now);
                e.meta = Some(json!({ "id": s.id, "email": s.email, "via": "environment" }));
                tx.audit_insert(&e)?;
                Ok(true)
            })
            .await
    }

    /// `POST /api/setup`: creates the first superuser with a valid setup
    /// token, stores the public URL and logs the superuser in.
    pub async fn setup(
        &self,
        ctx: &RequestCtx,
        token: &str,
        email: &str,
        pw: &str,
        public_url: &str,
    ) -> Result<Tokens> {
        let mut errs = ValidationErrors::default();
        let email = match normalize_email(email) {
            Ok(e) => e,
            Err(Error::Validation(v)) => {
                errs.0.extend(v.0);
                String::new()
            }
            Err(e) => return Err(e),
        };
        if let Err(e) = password::check_policy(pw, SUPERUSER_MIN_PASSWORD) {
            errs.add("password", e);
        }
        let url = match normalize_public_url(public_url) {
            Ok(u) => u,
            Err(e) => {
                errs.add("public_url", e);
                String::new()
            }
        };
        errs.into_result()?;
        let pw = pw.to_owned();
        let hash = self.blocking(move || password::hash(&pw)).await??;
        let token_hash = hash_token(token);
        let ctx2 = ctx.clone();
        let tokens = self
            .store
            .write(move |tx| {
                let now = ctx2.now;
                let now_s = format_timestamp(now);
                if tx.superusers_count()? > 0 {
                    return Err(Error::Conflict("Setup is already complete.".into()));
                }
                if !tx.setup_token_valid(&token_hash, &now_s)? {
                    return Err(Error::Forbidden(
                        "The setup token is invalid or expired. Restart rackend to get a new one."
                            .into(),
                    ));
                }
                tx.setup_token_use(&token_hash, &now_s)?;
                let s = Superuser {
                    id: Uuid::now_v7().to_string(),
                    email,
                    password_hash: hash,
                    created: now_s.clone(),
                    updated: now_s.clone(),
                };
                tx.superuser_insert(&s)?;
                let actor = Actor {
                    kind: crate::domain::ActorType::Superuser,
                    id: Some(s.id.clone()),
                    label: Some(s.email.clone()),
                    ..ctx2.actor()
                };
                audit_auth(
                    tx,
                    &actor,
                    "superuser.create",
                    now,
                    json!({ "id": s.id, "email": s.email, "via": "setup" }),
                )?;

                let rows = tx.settings_list()?;
                let mut app = rows
                    .iter()
                    .find(|r| r.key == "app")
                    .and_then(|r| serde_json::from_str::<Value>(&r.value).ok())
                    .and_then(|v| v.as_object().cloned())
                    .unwrap_or_default();
                let before = app.get("public_url").cloned();
                app.insert("public_url".into(), Value::String(url.clone()));
                tx.setting_put(&SettingRow {
                    key: "app".into(),
                    value: Value::Object(app).to_string(),
                    is_secret: false,
                    updated: now_s,
                })?;
                let mut e = entry(&actor, "settings.update", now);
                e.diff = Some(json!({ "app.public_url": { "before": before, "after": url } }));
                tx.audit_insert(&e)?;

                let (row, tokens) = new_session(&s, &ctx2, "password");
                tx.session_insert(&row)?;
                audit_auth(
                    tx,
                    &actor,
                    "auth.login.success",
                    now,
                    json!({ "method": "setup", "session": row.id }),
                )?;
                Ok(tokens)
            })
            .await?;
        self.reload_settings().await?;
        Ok(tokens)
    }

    /// Superuser password login. Unknown emails take as long as known ones.
    pub async fn superuser_login(&self, ctx: &RequestCtx, email: &str, pw: &str) -> Result<Tokens> {
        let email = email.trim().to_lowercase();
        let lookup = email.clone();
        let found = self
            .store
            .read(move |tx| tx.superuser_by_email(&lookup))
            .await?;
        let pw_owned = pw.to_owned();
        let ok = match &found {
            Some(s) => {
                let h = s.password_hash.clone();
                self.blocking(move || password::verify(&pw_owned, &h))
                    .await?
            }
            None => {
                self.blocking(move || password::dummy_verify(&pw_owned))
                    .await?;
                false
            }
        };
        let ctx2 = ctx.clone();
        let Some(s) = found.filter(|_| ok) else {
            // Failed logins are audited in their own small transaction.
            self.store
                .write(move |tx| {
                    audit_auth(
                        tx,
                        &ctx2.actor(),
                        "auth.login.failure",
                        ctx2.now,
                        json!({ "subject": "superuser", "email": email }),
                    )
                })
                .await?;
            return Err(Error::Unauthorized);
        };
        self.store
            .write(move |tx| {
                let (row, tokens) = new_session(&s, &ctx2, "password");
                tx.session_insert(&row)?;
                let actor = Actor {
                    kind: crate::domain::ActorType::Superuser,
                    id: Some(s.id.clone()),
                    label: Some(s.email.clone()),
                    ..ctx2.actor()
                };
                audit_auth(
                    tx,
                    &actor,
                    "auth.login.success",
                    ctx2.now,
                    json!({ "method": "password", "session": row.id }),
                )?;
                Ok(tokens)
            })
            .await
    }

    /// Rotates a refresh token (SPEC §11.5): reuse of the previous token
    /// revokes the session, except once within the 10 s grace window, which
    /// returns the same new pair.
    pub async fn superuser_refresh(&self, ctx: &RequestCtx, refresh_token: &str) -> Result<Tokens> {
        let session_id = parse_session_token(TokenKind::Refresh, refresh_token)
            .ok_or(Error::Unauthorized)?
            .to_owned();
        let presented = hash_token(refresh_token);
        if let Some(tokens) = self.take_grace(&presented) {
            return Ok(tokens);
        }
        let ctx2 = ctx.clone();
        let sid = session_id.clone();
        let p2 = presented.clone();
        let result = self
            .store
            .write(
                move |tx| -> Result<std::result::Result<(Tokens, String), bool>> {
                    let now_s = format_timestamp(ctx2.now);
                    let Some(mut row) = tx.session_get(&sid)? else {
                        return Ok(Err(false));
                    };
                    if row.revoked_at.is_some() || row.subject_type != SubjectType::Superuser {
                        return Ok(Err(false));
                    }
                    if row
                        .refresh_expires
                        .as_deref()
                        .is_none_or(|e| e <= now_s.as_str())
                    {
                        return Ok(Err(false));
                    }
                    let current = row.refresh_hash.clone().unwrap_or_default();
                    if !ct_eq(&current, &p2) {
                        if row
                            .prev_refresh_hash
                            .as_deref()
                            .is_some_and(|prev| ct_eq(prev, &p2))
                        {
                            // Reuse of a rotated token: assume theft, revoke.
                            row.revoked_at = Some(now_s.clone());
                            row.revoked_reason = Some("refresh_reuse".into());
                            tx.session_update(&row)?;
                            let actor = Actor {
                                kind: crate::domain::ActorType::Superuser,
                                id: Some(row.user_id.clone()),
                                ..ctx2.actor()
                            };
                            audit_auth(
                                tx,
                                &actor,
                                "auth.refresh_reuse",
                                ctx2.now,
                                json!({ "session": row.id }),
                            )?;
                            return Ok(Err(true));
                        }
                        return Ok(Err(false));
                    }
                    let s = tx
                        .superuser_by_id(&row.user_id)?
                        .ok_or(Error::Unauthorized)?;
                    let access = session_token(TokenKind::Access, &row.id);
                    let refresh = session_token(TokenKind::Refresh, &row.id);
                    row.prev_refresh_hash = Some(current);
                    row.refresh_hash = Some(hash_token(&refresh));
                    row.access_hash = hash_token(&access);
                    row.access_expires = format_timestamp(ctx2.now + SUPERUSER_ACCESS_TTL);
                    row.refresh_expires = Some(format_timestamp(ctx2.now + SUPERUSER_REFRESH_TTL));
                    row.last_seen = now_s;
                    tx.session_update(&row)?;
                    Ok(Ok((
                        Tokens {
                            access_token: access,
                            refresh_token: refresh,
                            expires_in: SUPERUSER_ACCESS_TTL.whole_seconds(),
                            refresh_expires_in: SUPERUSER_REFRESH_TTL.whole_seconds(),
                            session_id: row.id.clone(),
                            superuser: superuser_json(&s),
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
        let ctx2 = ctx.clone();
        let sid = session_id.clone();
        self.store
            .write(move |tx| {
                if let Some(mut row) = tx.session_get(&sid)? {
                    row.revoked_at = Some(format_timestamp(ctx2.now));
                    row.revoked_reason = Some("logout".into());
                    tx.session_update(&row)?;
                    audit_auth(
                        tx,
                        &ctx2.actor(),
                        "auth.logout",
                        ctx2.now,
                        json!({ "session": sid }),
                    )?;
                }
                Ok(())
            })
            .await?;
        self.sessions.invalidate(&session_id);
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
        let sid = session_id.clone();
        let found = self
            .store
            .read(move |tx| {
                let Some(row) = tx.session_get(&sid)? else {
                    return Ok(None);
                };
                if row.subject_type != SubjectType::Superuser {
                    return Ok(None);
                }
                let s = tx.superuser_by_id(&row.user_id)?;
                Ok(s.map(|s| (row, s)))
            })
            .await?;
        let Some((row, s)) = found else {
            return Err(Error::Unauthorized);
        };
        if row.revoked_at.is_some() {
            return Err(Error::Unauthorized);
        }
        let principal = Principal::Superuser {
            id: s.id,
            email: s.email,
            session_id: row.id.clone(),
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

    /// All superusers.
    pub async fn superusers_list(&self, ctx: &RequestCtx) -> Result<Vec<Value>> {
        ctx.require_superuser()?;
        let list = self.store.read(|tx| tx.superusers_list()).await?;
        Ok(list.iter().map(superuser_json).collect())
    }

    /// Creates a superuser.
    pub async fn superuser_create(&self, ctx: &RequestCtx, email: &str, pw: &str) -> Result<Value> {
        ctx.require_superuser()?;
        let email = normalize_email(email)?;
        password::check_policy(pw, SUPERUSER_MIN_PASSWORD)
            .map_err(|e| Error::field("password", e))?;
        let pw = pw.to_owned();
        let hash = self.blocking(move || password::hash(&pw)).await??;
        let ctx2 = ctx.clone();
        self.store
            .write(move |tx| {
                if tx.superuser_by_email(&email)?.is_some() {
                    return Err(Error::field(
                        "email",
                        FieldError::new("taken", "A superuser with this email exists."),
                    ));
                }
                let now = format_timestamp(ctx2.now);
                let s = Superuser {
                    id: Uuid::now_v7().to_string(),
                    email,
                    password_hash: hash,
                    created: now.clone(),
                    updated: now,
                };
                tx.superuser_insert(&s)?;
                audit_auth(
                    tx,
                    &ctx2.actor(),
                    "superuser.create",
                    ctx2.now,
                    json!({ "id": s.id, "email": s.email }),
                )?;
                Ok(superuser_json(&s))
            })
            .await
    }

    /// Changes email and/or password of a superuser. A password change
    /// revokes the superuser's other sessions.
    pub async fn superuser_update(
        &self,
        ctx: &RequestCtx,
        id: &str,
        email: Option<&str>,
        pw: Option<&str>,
    ) -> Result<Value> {
        ctx.require_superuser()?;
        let email = email.map(normalize_email).transpose()?;
        let hash = match pw {
            Some(p) => {
                password::check_policy(p, SUPERUSER_MIN_PASSWORD)
                    .map_err(|e| Error::field("password", e))?;
                let p = p.to_owned();
                Some(self.blocking(move || password::hash(&p)).await??)
            }
            None => None,
        };
        let ctx2 = ctx.clone();
        let id = id.to_owned();
        let keep = match &ctx.principal {
            Principal::Superuser { session_id, .. } => Some(session_id.clone()),
            _ => None,
        };
        let (json, revoked) = self
            .store
            .write(move |tx| {
                let mut s = tx.superuser_by_id(&id)?.ok_or(Error::NotFound)?;
                if let Some(e) = email {
                    if e != s.email && tx.superuser_by_email(&e)?.is_some() {
                        return Err(Error::field(
                            "email",
                            FieldError::new("taken", "A superuser with this email exists."),
                        ));
                    }
                    s.email = e;
                }
                let changed_password = hash.is_some();
                if let Some(h) = hash {
                    s.password_hash = h;
                }
                let now_s = format_timestamp(ctx2.now);
                s.updated = now_s.clone();
                tx.superuser_update(&s)?;
                let mut revoked = Vec::new();
                if changed_password {
                    revoked =
                        tx.sessions_revoke_for(&s.id, keep.as_deref(), "password_change", &now_s)?;
                    audit_auth(
                        tx,
                        &ctx2.actor(),
                        "auth.password.change",
                        ctx2.now,
                        json!({ "superuser": s.id }),
                    )?;
                }
                audit_auth(
                    tx,
                    &ctx2.actor(),
                    "superuser.update",
                    ctx2.now,
                    json!({ "id": s.id, "email": s.email }),
                )?;
                Ok((superuser_json(&s), revoked))
            })
            .await?;
        for sid in revoked {
            self.sessions.invalidate(&sid);
        }
        Ok(json)
    }

    /// Deletes a superuser (never the last one) and revokes its sessions.
    pub async fn superuser_delete(&self, ctx: &RequestCtx, id: &str) -> Result<()> {
        ctx.require_superuser()?;
        let ctx2 = ctx.clone();
        let id = id.to_owned();
        let revoked = self
            .store
            .write(move |tx| {
                let s = tx.superuser_by_id(&id)?.ok_or(Error::NotFound)?;
                if tx.superusers_count()? <= 1 {
                    return Err(Error::Conflict(
                        "The last superuser cannot be deleted.".into(),
                    ));
                }
                let revoked =
                    tx.sessions_revoke_for(&s.id, None, "deleted", &format_timestamp(ctx2.now))?;
                tx.superuser_delete(&s.id)?;
                audit_auth(
                    tx,
                    &ctx2.actor(),
                    "superuser.delete",
                    ctx2.now,
                    json!({ "id": s.id, "email": s.email }),
                )?;
                Ok(revoked)
            })
            .await?;
        for sid in revoked {
            self.sessions.invalidate(&sid);
        }
        Ok(())
    }
}
