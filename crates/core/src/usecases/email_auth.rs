//! Email flows of auth collections (SPEC §10.6, §11.4): verification,
//! password reset, email change, one-time codes and magic links.
//!
//! Request endpoints answer the same way whether or not the account exists
//! (no account enumeration): the lookup and the token write run in one
//! transaction either way, and email goes out in the background.

use std::sync::Arc;

use serde_json::{Map, Value, json};
use time::Duration;
use uuid::Uuid;

use super::auth::{Tokens, audit_auth, normalize_email};
use super::records::{WriteCtx, system_update};
use super::sessions::{Subject, start_session};
use super::{App, Principal, RequestCtx};
use crate::auth::broker::{OtpRow, TokenPurpose, TokenRow};
use crate::auth::token::{ct_eq, hash_token, random_bytes, random_token};
use crate::domain::{Collection, Record};
use crate::error::FieldError;
use crate::ports::{StoreExt as _, WriteTx};
use crate::time::format_timestamp;
use crate::{Error, Result};

/// Verification link lifetime.
pub const VERIFY_TTL: Duration = Duration::hours(72);
/// Password reset link lifetime.
pub const RESET_TTL: Duration = Duration::hours(1);
/// Email change link lifetime.
pub const EMAIL_CHANGE_TTL: Duration = Duration::hours(1);
/// Magic link lifetime.
pub const MAGIC_LINK_TTL: Duration = Duration::minutes(15);
/// One-time code lifetime.
pub const OTP_TTL: Duration = Duration::minutes(10);
/// Wrong codes before a one-time code is void.
pub const OTP_MAX_ATTEMPTS: u32 = 5;

/// A uniformly random six-digit code.
fn otp_code() -> String {
    loop {
        let n = u32::from_le_bytes(random_bytes::<4>());
        // Reject the tail so every code is equally likely.
        if n < u32::MAX - (u32::MAX % 1_000_000) {
            return format!("{:06}", n % 1_000_000);
        }
    }
}

fn invalid_token() -> Error {
    Error::field(
        "token",
        FieldError::new("invalid_token", "The link is invalid or has expired."),
    )
}

/// A link token to store for a user found by email.
struct NewToken {
    purpose: TokenPurpose,
    ttl: Duration,
    token: String,
    payload: Option<String>,
    /// Users for whom no token is made (already verified).
    skip: fn(&Record) -> bool,
}

/// What a hosted page needs to know about a token before using it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TokenInfo {
    /// The token is unused and unexpired.
    pub valid: bool,
    /// Confirming needs the current password (email change).
    pub needs_password: bool,
}

impl App {
    /// An auth collection by id or name.
    pub(crate) fn auth_collection(&self, key: &str) -> Result<Arc<Collection>> {
        let compiled = self.schema.get();
        let c = compiled.schema.by_id_or_name(key).ok_or(Error::NotFound)?;
        c.auth().ok_or(Error::NotFound)?;
        Ok(Arc::clone(c))
    }

    /// `app.public_url`, required to build links in emails.
    fn link_base(&self) -> Result<String> {
        self.settings()
            .public_url()
            .ok_or(Error::NotConfigured("public_url"))
    }

    /// Whether `uri` may receive tokens: an exact entry of
    /// `security.app_redirect_uris`, or `http://localhost:*` in dev mode.
    pub fn redirect_allowed(&self, uri: &str) -> bool {
        if self
            .settings()
            .strings("security", "app_redirect_uris")
            .iter()
            .any(|u| u == uri)
        {
            return true;
        }
        if !self.dev_mode {
            return false;
        }
        let Some(rest) = uri
            .strip_prefix("http://localhost")
            .or_else(|| uri.strip_prefix("http://127.0.0.1"))
        else {
            return false;
        };
        rest.is_empty() || rest.starts_with(':') || rest.starts_with('/')
    }

    /// Stores a new token for the user with `email` (voiding older ones of
    /// the same purpose) and returns the user, in one transaction whether or
    /// not the user exists.
    async fn issue_token(
        &self,
        c: &Arc<Collection>,
        email: String,
        t: NewToken,
    ) -> Result<Option<Record>> {
        let NewToken {
            purpose,
            ttl,
            token,
            payload,
            skip,
        } = t;
        let (c2, now) = (Arc::clone(c), self.clock.now());
        self.store
            .write(move |tx| {
                let Some((record, _)) = tx.auth_find_by_email(&c2, &email)? else {
                    return Ok(None);
                };
                if skip(&record) {
                    return Ok(None);
                }
                let user_id = record.id.to_string();
                tx.tokens_void(&user_id, purpose, &format_timestamp(now))?;
                tx.token_insert(&TokenRow {
                    hash: hash_token(&token),
                    purpose,
                    collection_id: c2.id.to_string(),
                    user_id,
                    payload,
                    expires: format_timestamp(now + ttl),
                    used_at: None,
                })?;
                Ok(Some(record))
            })
            .await
    }

    /// Uses a token of collection `c`; returns it with its user.
    fn take_token_in(
        tx: &mut dyn WriteTx,
        c: &Collection,
        token: &str,
        purpose: TokenPurpose,
        now: &str,
    ) -> Result<(TokenRow, Record)> {
        let row = tx
            .token_take(&hash_token(token), purpose, now)?
            .filter(|t| t.collection_id == c.id.to_string())
            .ok_or_else(invalid_token)?;
        let record = tx.record_get(c, &row.user_id)?.ok_or_else(invalid_token)?;
        Ok((row, record))
    }

    /// Whether a link token is still usable, for the hosted pages.
    pub async fn token_info(
        &self,
        key: &str,
        token: &str,
        purpose: TokenPurpose,
    ) -> Result<TokenInfo> {
        let c = self.auth_collection(key)?;
        let (hash, now) = (hash_token(token), format_timestamp(self.clock.now()));
        let c2 = Arc::clone(&c);
        self.store
            .read(move |tx| {
                let Some(row) = tx
                    .token_get(&hash, purpose, &now)?
                    .filter(|t| t.collection_id == c2.id.to_string())
                else {
                    return Ok(TokenInfo {
                        valid: false,
                        needs_password: false,
                    });
                };
                let needs_password = purpose == TokenPurpose::ChangeEmail
                    && tx.auth_password_hash(&c2, &row.user_id)?.is_some();
                Ok(TokenInfo {
                    valid: true,
                    needs_password,
                })
            })
            .await
    }

    /// Commits a write made by the server for a user and publishes its events.
    async fn user_write<T: Send + 'static>(
        &self,
        ctx: &RequestCtx,
        f: impl FnOnce(&mut dyn WriteTx, &WriteCtx) -> Result<T> + Send + 'static,
    ) -> Result<T> {
        let w = self.system_write_ctx(ctx, ctx.actor());
        let (out, events) = self
            .store
            .write(move |tx| {
                let out = f(tx, &w)?;
                Ok((out, w.take_events()))
            })
            .await?;
        self.hub.publish(events);
        Ok(out)
    }

    // Verification ---------------------------------------------------------

    /// `POST /api/collections/{c}/request-verification {email}`: always 204.
    pub async fn request_verification(
        &self,
        _ctx: &RequestCtx,
        key: &str,
        email: &str,
    ) -> Result<()> {
        let c = self.auth_collection(key)?;
        self.mailer()?;
        let base = self.link_base()?;
        let Ok(email) = normalize_email(email) else {
            return Ok(());
        };
        let token = random_token();
        let user = self
            .issue_token(
                &c,
                email.clone(),
                NewToken {
                    purpose: TokenPurpose::VerifyEmail,
                    ttl: VERIFY_TTL,
                    token: token.clone(),
                    payload: Some(email.clone()),
                    skip: |r| r.get("email_verified") == &Value::Bool(true),
                },
            )
            .await?;
        if user.is_some() {
            let link = format!("{base}/auth/verify?c={}&token={token}", c.name);
            self.send_later(self.render_email("verification", &email, &[("link", &link)]));
        }
        Ok(())
    }

    /// `POST /api/collections/{c}/confirm-verification {token}`.
    pub async fn confirm_verification(
        &self,
        ctx: &RequestCtx,
        key: &str,
        token: &str,
    ) -> Result<()> {
        let c = self.auth_collection(key)?;
        let (token, now) = (token.to_owned(), format_timestamp(ctx.now));
        let ctx2 = ctx.clone();
        self.user_write(ctx, move |tx, w| {
            let (row, record) =
                Self::take_token_in(tx, &c, &token, TokenPurpose::VerifyEmail, &now)?;
            // The address may have changed since the link was sent.
            if record.get("email").as_str() != row.payload.as_deref() {
                return Err(invalid_token());
            }
            if record.get("email_verified") != &Value::Bool(true) {
                let mut changes = Map::new();
                changes.insert("email_verified".into(), Value::Bool(true));
                system_update(tx, w, &c, &record, changes)?;
            }
            audit_auth(
                tx,
                &Subject::user(&c, &record).actor(&ctx2),
                "auth.email.verify",
                ctx2.now,
                json!({ "collection": c.name }),
            )
        })
        .await
    }

    // Password reset -------------------------------------------------------

    /// `POST /api/collections/{c}/request-password-reset {email}`: always 204.
    pub async fn request_password_reset(
        &self,
        _ctx: &RequestCtx,
        key: &str,
        email: &str,
    ) -> Result<()> {
        let c = self.auth_collection(key)?;
        if !c.auth().is_some_and(|o| o.methods.password) {
            return Err(Error::Forbidden(
                "Password login is disabled for this collection.".into(),
            ));
        }
        self.mailer()?;
        let base = self.link_base()?;
        let Ok(email) = normalize_email(email) else {
            return Ok(());
        };
        let token = random_token();
        let user = self
            .issue_token(
                &c,
                email.clone(),
                NewToken {
                    purpose: TokenPurpose::ResetPassword,
                    ttl: RESET_TTL,
                    token: token.clone(),
                    payload: None,
                    skip: |_| false,
                },
            )
            .await?;
        if user.is_some() {
            let link = format!("{base}/auth/reset?c={}&token={token}", c.name);
            self.send_later(self.render_email("password_reset", &email, &[("link", &link)]));
        }
        Ok(())
    }

    /// `POST /api/collections/{c}/confirm-password-reset {token, password}`:
    /// sets the password, verifies the email (the link proved access to it)
    /// and ends every session.
    pub async fn confirm_password_reset(
        &self,
        ctx: &RequestCtx,
        key: &str,
        token: &str,
        password: &str,
    ) -> Result<()> {
        let c = self.auth_collection(key)?;
        let min = c.auth().map_or(10, |o| o.password_min_length);
        // Check the password first so a weak one does not use up the link.
        let hash = self.hash_new_password(password, min).await?;
        let (token, now) = (token.to_owned(), format_timestamp(ctx.now));
        let ctx2 = ctx.clone();
        let user_id = self
            .user_write(ctx, move |tx, w| {
                let (_, record) =
                    Self::take_token_in(tx, &c, &token, TokenPurpose::ResetPassword, &now)?;
                let id = record.id.to_string();
                tx.auth_password_set(&c, &id, Some(&hash))?;
                tx.tokens_void(&id, TokenPurpose::ResetPassword, &now)?;
                if record.get("email_verified") != &Value::Bool(true) {
                    let mut changes = Map::new();
                    changes.insert("email_verified".into(), Value::Bool(true));
                    system_update(tx, w, &c, &record, changes)?;
                }
                audit_auth(
                    tx,
                    &Subject::user(&c, &record).actor(&ctx2),
                    "auth.password.reset",
                    ctx2.now,
                    json!({ "collection": c.name }),
                )?;
                Ok(id)
            })
            .await?;
        self.revoke_all_for(&user_id, None, "password_reset", ctx.now)
            .await
    }

    // Email change ---------------------------------------------------------

    /// `POST /api/collections/{c}/request-email-change {new_email}`: the
    /// signed-in user asks to move to a new address; the link goes there.
    /// Always 204, also when the address is taken.
    pub async fn request_email_change(
        &self,
        ctx: &RequestCtx,
        key: &str,
        new_email: &str,
    ) -> Result<()> {
        let c = self.auth_collection(key)?;
        let Principal::User {
            id, collection_id, ..
        } = &ctx.principal
        else {
            return Err(Error::Unauthorized);
        };
        if *collection_id != c.id.to_string() {
            return Err(Error::Forbidden(
                "Sign in as a user of this collection.".into(),
            ));
        }
        self.mailer()?;
        let base = self.link_base()?;
        let new_email = normalize_email(new_email).map_err(|_| {
            Error::field(
                "new_email",
                FieldError::new("invalid_email", "Must be a valid email address."),
            )
        })?;
        let (c2, id2, email2, now) = (Arc::clone(&c), id.clone(), new_email.clone(), ctx.now);
        let token = random_token();
        let token2 = token.clone();
        let send = self
            .store
            .write(move |tx| {
                let taken = tx.auth_find_by_email(&c2, &email2)?.is_some();
                tx.tokens_void(&id2, TokenPurpose::ChangeEmail, &format_timestamp(now))?;
                if taken {
                    return Ok(false);
                }
                tx.token_insert(&TokenRow {
                    hash: hash_token(&token2),
                    purpose: TokenPurpose::ChangeEmail,
                    collection_id: c2.id.to_string(),
                    user_id: id2,
                    payload: Some(email2),
                    expires: format_timestamp(now + EMAIL_CHANGE_TTL),
                    used_at: None,
                })?;
                Ok(true)
            })
            .await?;
        if send {
            let link = format!("{base}/auth/email-change?c={}&token={token}", c.name);
            self.send_later(self.render_email("email_change", &new_email, &[("link", &link)]));
        }
        Ok(())
    }

    /// `POST /api/collections/{c}/confirm-email-change {token, password?}`:
    /// the password is required when the account has one. Ends every session.
    pub async fn confirm_email_change(
        &self,
        ctx: &RequestCtx,
        key: &str,
        token: &str,
        password: Option<&str>,
    ) -> Result<()> {
        let c = self.auth_collection(key)?;
        let (hash, now) = (hash_token(token), format_timestamp(ctx.now));
        let (c2, h2, n2) = (Arc::clone(&c), hash.clone(), now.clone());
        let (row, pw_hash) = self
            .store
            .read(move |tx| {
                let Some(row) = tx
                    .token_get(&h2, TokenPurpose::ChangeEmail, &n2)?
                    .filter(|t| t.collection_id == c2.id.to_string())
                else {
                    return Ok((None, None));
                };
                let pw = tx.auth_password_hash(&c2, &row.user_id)?;
                Ok((Some(row), pw))
            })
            .await?;
        let row = row.ok_or_else(invalid_token)?;
        if pw_hash.is_some() {
            let ok = self
                .verify_password(pw_hash, password.unwrap_or_default())
                .await?;
            if !ok {
                return Err(Error::field(
                    "password",
                    FieldError::new("invalid_password", "The password is not correct."),
                ));
            }
        }
        let new_email = row.payload.clone().ok_or_else(invalid_token)?;
        let (token, ctx2) = (token.to_owned(), ctx.clone());
        let user_id = self
            .user_write(ctx, move |tx, w| {
                let (_, record) =
                    Self::take_token_in(tx, &c, &token, TokenPurpose::ChangeEmail, &now)?;
                let old = record.get("email").clone();
                let mut changes = Map::new();
                changes.insert("email".into(), Value::String(new_email.clone()));
                changes.insert("email_verified".into(), Value::Bool(true));
                system_update(tx, w, &c, &record, changes)?;
                let id = record.id.to_string();
                tx.tokens_void(&id, TokenPurpose::VerifyEmail, &now)?;
                tx.tokens_void(&id, TokenPurpose::ResetPassword, &now)?;
                tx.tokens_void(&id, TokenPurpose::MagicLink, &now)?;
                tx.otps_void(&id, &now)?;
                audit_auth(
                    tx,
                    &Subject::user(&c, &record).actor(&ctx2),
                    "auth.email.change",
                    ctx2.now,
                    json!({ "collection": c.name, "before": old, "after": new_email }),
                )?;
                Ok(id)
            })
            .await?;
        self.revoke_all_for(&user_id, None, "email_changed", ctx.now)
            .await
    }

    // One-time codes -------------------------------------------------------

    fn otp_hash(&self, id: &str, code: &str) -> String {
        self.secrets
            .mac("otp", format!("{id}:{code}").as_bytes())
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect()
    }

    /// `POST /api/collections/{c}/request-otp {email}` → `{otp_id}`, also for
    /// unknown emails (nothing is stored then).
    pub async fn request_otp(&self, _ctx: &RequestCtx, key: &str, email: &str) -> Result<String> {
        let c = self.auth_collection(key)?;
        if !c.auth().is_some_and(|o| o.methods.otp) {
            return Err(Error::Forbidden(
                "One-time codes are disabled for this collection.".into(),
            ));
        }
        self.mailer()?;
        let otp_id = Uuid::now_v7().to_string();
        let Ok(email) = normalize_email(email) else {
            return Ok(otp_id);
        };
        let code = otp_code();
        let row = OtpRow {
            id: otp_id.clone(),
            collection_id: c.id.to_string(),
            user_id: String::new(),
            code_hash: self.otp_hash(&otp_id, &code),
            purpose: "login".into(),
            expires: format_timestamp(self.clock.now() + OTP_TTL),
            attempts: 0,
            used_at: None,
        };
        let (c2, email2, now) = (Arc::clone(&c), email.clone(), self.clock.now());
        let found = self
            .store
            .write(move |tx| {
                let Some((record, _)) = tx.auth_find_by_email(&c2, &email2)? else {
                    return Ok(false);
                };
                let user_id = record.id.to_string();
                tx.otps_void(&user_id, &format_timestamp(now))?;
                tx.otp_insert(&OtpRow { user_id, ..row })?;
                Ok(true)
            })
            .await?;
        if found {
            self.send_later(self.render_email("otp", &email, &[("code", &code)]));
        }
        Ok(otp_id)
    }

    /// `POST /api/collections/{c}/auth-with-otp {otp_id, code}`. A correct
    /// code also verifies the email.
    pub async fn auth_with_otp(
        &self,
        ctx: &RequestCtx,
        key: &str,
        otp_id: &str,
        code: &str,
    ) -> Result<Tokens> {
        let c = self.auth_collection(key)?;
        if !c.auth().is_some_and(|o| o.methods.otp) {
            return Err(Error::Forbidden(
                "One-time codes are disabled for this collection.".into(),
            ));
        }
        let expected = self.otp_hash(otp_id, code.trim());
        let (id, now) = (otp_id.to_owned(), format_timestamp(ctx.now));
        let ctx2 = ctx.clone();
        let c2 = Arc::clone(&c);
        let out = self
            .user_write(ctx, move |tx, w| {
                let Some(mut otp) = tx.otp_get(&id)?.filter(|o| {
                    o.collection_id == c2.id.to_string() && o.used_at.is_none() && o.expires > now
                }) else {
                    return Ok(Err(Error::Unauthorized));
                };
                if !ct_eq(&otp.code_hash, &expected) {
                    otp.attempts += 1;
                    if otp.attempts >= OTP_MAX_ATTEMPTS {
                        otp.used_at = Some(now.clone());
                    }
                    tx.otp_update(&otp)?;
                    audit_auth(
                        tx,
                        &ctx2.actor(),
                        "auth.login.failure",
                        ctx2.now,
                        json!({ "method": "otp", "collection": c2.name }),
                    )?;
                    return Ok(Err(Error::Unauthorized));
                }
                otp.used_at = Some(now.clone());
                tx.otp_update(&otp)?;
                let Some(mut record) = tx.record_get(&c2, &otp.user_id)? else {
                    return Ok(Err(Error::Unauthorized));
                };
                if record.get("email_verified") != &Value::Bool(true) {
                    let mut changes = Map::new();
                    changes.insert("email_verified".into(), Value::Bool(true));
                    record = system_update(tx, w, &c2, &record, changes)?;
                }
                Ok(Ok(start_session(
                    tx,
                    &Subject::user(&c2, &record),
                    &ctx2,
                    "otp",
                )?))
            })
            .await?;
        self.observe.counters.login("otp", out.is_ok());
        out
    }

    // Magic links ----------------------------------------------------------

    /// `POST /api/collections/{c}/request-magic-link {email, redirect_uri}`:
    /// always 204. The link is `{redirect_uri}#token=…`.
    pub async fn request_magic_link(
        &self,
        _ctx: &RequestCtx,
        key: &str,
        email: &str,
        redirect_uri: &str,
    ) -> Result<()> {
        let c = self.auth_collection(key)?;
        if !c.auth().is_some_and(|o| o.methods.magic_link) {
            return Err(Error::Forbidden(
                "Magic links are disabled for this collection.".into(),
            ));
        }
        self.mailer()?;
        if !self.redirect_allowed(redirect_uri) {
            return Err(Error::field(
                "redirect_uri",
                FieldError::new(
                    "redirect_not_allowed",
                    "Add this URI to security.app_redirect_uris first.",
                ),
            ));
        }
        let Ok(email) = normalize_email(email) else {
            return Ok(());
        };
        let token = random_token();
        let user = self
            .issue_token(
                &c,
                email.clone(),
                NewToken {
                    purpose: TokenPurpose::MagicLink,
                    ttl: MAGIC_LINK_TTL,
                    token: token.clone(),
                    payload: None,
                    skip: |_| false,
                },
            )
            .await?;
        if user.is_some() {
            let link = format!("{redirect_uri}#token={token}");
            self.send_later(self.render_email("magic_link", &email, &[("link", &link)]));
        }
        Ok(())
    }

    /// `POST /api/collections/{c}/auth-with-magic-link {token}`. Also
    /// verifies the email.
    pub async fn auth_with_magic_link(
        &self,
        ctx: &RequestCtx,
        key: &str,
        token: &str,
    ) -> Result<Tokens> {
        let c = self.auth_collection(key)?;
        if !c.auth().is_some_and(|o| o.methods.magic_link) {
            return Err(Error::Forbidden(
                "Magic links are disabled for this collection.".into(),
            ));
        }
        let (token, now, ctx2) = (token.to_owned(), format_timestamp(ctx.now), ctx.clone());
        let out = self
            .user_write(ctx, move |tx, w| {
                let (_, mut record) =
                    Self::take_token_in(tx, &c, &token, TokenPurpose::MagicLink, &now)
                        .map_err(|_| Error::Unauthorized)?;
                if record.get("email_verified") != &Value::Bool(true) {
                    let mut changes = Map::new();
                    changes.insert("email_verified".into(), Value::Bool(true));
                    record = system_update(tx, w, &c, &record, changes)?;
                }
                start_session(tx, &Subject::user(&c, &record), &ctx2, "magic_link")
            })
            .await;
        self.observe.counters.login("magic_link", out.is_ok());
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn otp_codes_are_six_digits() {
        for _ in 0..1000 {
            let c = otp_code();
            assert_eq!(c.len(), 6);
            assert!(c.bytes().all(|b| b.is_ascii_digit()));
        }
    }
}
